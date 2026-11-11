import { ERROR_CODE, EVENT, METHOD, OBJ_LIST_MAX } from '@magpdf/protocol';
import type {
  DocChanged,
  DocCloseParams,
  DocInfo,
  DocSnapshot,
  DocSnapshotParams,
  EditDeleteParams,
  EditResult,
  EditTransformParams,
  EngineFailed,
  EngineRestored,
  FidelityCheckParams,
  FidelityResult,
  HistoryParams,
  ObjListParams,
  ObjectKind,
  ObjectSummary,
  PageInfo,
  RpcError,
} from '@magpdf/protocol';
import type { TileProvider } from '../canvas/tileProvider';
import type { DocumentEditor, PageTransform } from '../document/documentEditor';
import {
  displayMatrix,
  pageTransformToUser,
  transformRect,
  userBoxToPageRect,
} from '../document/pageSpace';
import type { Matrix } from '../document/pageSpace';
import type { DocumentSnapshot, ObjectType, PageObject } from '../document/types';
import { format, t } from '../i18n';
import type { EditorHost, HostNotice } from '../shell/editorHost';
import type { EditorStore } from '../store/editorStore';
import { EngineTileProvider } from './engineTileProvider';
import { engineIds, objectId, pageOfObject } from './objectIds';
import { describeError, isEngineRestarted, isRpcError } from './transport';
import type { EngineEvent, EngineTransport } from './transport';

export interface EngineSessionOptions {
  readonly transport: EngineTransport;
  readonly store: EditorStore;
  /**
   * 请主进程弹“打开”对话框并让引擎打开选中的文件；用户取消时返回 null。
   * 路径由主进程经手，界面不传路径（《接口协议》第 2 节：文件对话框由主进程处理）。
   */
  readonly pickDocument: () => Promise<DocInfo | null>;
  /** 主进程知道的、已经开着的文档：启动时命令行给的，或者界面重新加载之前打开的。 */
  readonly listDocuments: () => Promise<readonly DocInfo[]>;
}

/** 一页在界面这边的记录。 */
interface EnginePage {
  readonly pageId: number;
  /** PDF 用户空间到页面坐标的矩阵。要等这一页的旋转和可见区域取到之后才有。 */
  display: Matrix | null;
  /** 对象列表取到过没有 */
  loaded: boolean;
  /** 正在取对象列表的那一次的序号；0 表示没有在取。又发起一次时，前一次的结果作废。 */
  loading: number;
  /** 界面上已经先改了、引擎还没有回话的修改命令里，有几条动了这一页 */
  pendingEdits: number;
  /** 有修改在途时到过一份对象列表，没有用（用了会把界面上先改的冲掉）；等修改都有了结果再取一次 */
  reloadAfterEdits: boolean;
}

/** 一份由引擎打开的文档在界面这边的记录。 */
interface EngineDocument {
  /** 界面里的文档 id。引擎重启后 `docId` 会变，它不变 */
  readonly id: string;
  readonly path: string;
  readonly title: string;
  /** 引擎里的文档编号；引擎重启期间是 null */
  docId: number | null;
  /** 引擎里这份文档的版本号（界面知道的最新值），修改命令的 `baseVersion` 用它 */
  version: number;
  /** 按页序号 */
  pages: EnginePage[];
  /** 修改命令排着队一条一条发：后一条要带上前一条之后的版本号 */
  queue: Promise<void>;
  readonly provider: EngineTileProvider;
  readonly editor: DocumentEditor;
}

/**
 * 界面这一侧的引擎会话：把引擎打开的文档放进编辑器的 store，给每份文档配一个瓦片来源和一个修改入口，
 * 并跟着引擎的事件走（《接口协议》5.1、5.2、第 7 节）。
 *
 * - **页面和对象**：打开时只取页面列表；一页的旋转、可见区域和对象列表等看到这一页（和它前后各一页）
 *   时才取，几百页的文档不用在打开时全部加载。对象的包围盒从 PDF 用户空间换算成页面坐标。
 * - **修改**：移动、缩放、删除、撤销、重做都发给引擎执行。界面先按预期把对象挪过去（不等引擎），
 *   引擎推送 `doc.changed` 之后重新取这一页的对象，以引擎的为准，画面随之重画；
 *   引擎不接受时界面恢复原状，并告诉用户原因。
 * - **只能叠加内容的页**（方案 2.3）：页面显示之后引擎会在空闲时做保真校验，结论经 `fidelity.result`
 *   推送过来，记在页面快照的 `overlayOnly` 上；工具据此不让修改这一页原有的对象，外壳据此显示提示条。
 *   校验还没做完用户就动手了也没关系：引擎在执行修改命令前自己会校验，不通过就拒绝，界面恢复原状。
 * - **引擎重启**：按路径把文档和新的 `docId` 对上，重新载入；对不上的关掉并告诉用户。
 */
export class EngineSession implements EditorHost {
  private readonly transport: EngineTransport;
  private readonly store: EditorStore;
  private readonly options: EngineSessionOptions;
  private readonly documents = new Map<string, EngineDocument>();
  /** 正在接进来的文档（按 docId），防止同一份文档被接两次 */
  private readonly adopting = new Map<number, Promise<void>>();
  private readonly noticeListeners = new Set<(notice: HostNotice) => void>();
  private readonly unsubscribeEvents: () => void;
  private readonly unsubscribeStore: () => void;
  private sequence = 0;
  /** 页面修订号的来源：每次画面变化取一个新值，整个会话里不重复 */
  private revision = 0;
  private loadSequence = 0;
  private disposed = false;

  constructor(options: EngineSessionOptions) {
    this.options = options;
    this.transport = options.transport;
    this.store = options.store;
    this.unsubscribeEvents = this.transport.onEvent((event) => this.onEvent(event));
    this.unsubscribeStore = this.store.subscribe((state, previous) => {
      if (state.documents === previous.documents) return;
      for (const document of [...this.documents.values()]) {
        const now = state.documents[document.id];
        if (!now) {
          // 标签页关掉之后，把引擎里的文档也关掉
          this.release(document);
        } else if (now.currentPage !== previous.documents[document.id]?.currentPage) {
          // 翻到了别的页：把那一页和前后各一页的对象取过来
          this.ensureAround(document, now.currentPage);
        }
      }
    });
  }

  /** 把已经开着的文档接进来。界面启动时调一次。 */
  async attach(): Promise<void> {
    const open = await this.options.listDocuments();
    for (const info of open) await this.adopt(info);
  }

  async openDocument(): Promise<void> {
    const info = await this.options.pickDocument();
    if (info) await this.adopt(info);
  }

  tileProvider(documentId: string): TileProvider | undefined {
    return this.documents.get(documentId)?.provider;
  }

  documentEditor(documentId: string): DocumentEditor | undefined {
    return this.documents.get(documentId)?.editor;
  }

  onNotice(listener: (notice: HostNotice) => void): () => void {
    this.noticeListeners.add(listener);
    return () => this.noticeListeners.delete(listener);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.unsubscribeEvents();
    this.unsubscribeStore();
    for (const document of this.documents.values()) document.provider.dispose();
    this.documents.clear();
    this.noticeListeners.clear();
  }

  // ---------- 打开与关闭 ----------

  /** 把引擎里的一份文档放进 store；已经在了就切过去。 */
  private adopt(info: DocInfo): Promise<void> {
    const known = this.find(info.docId);
    if (known) {
      this.store.getState().activateDocument(known.id);
      return Promise.resolve();
    }
    const pending = this.adopting.get(info.docId);
    if (pending) return pending;
    const adopting = this.load(info).finally(() => this.adopting.delete(info.docId));
    this.adopting.set(info.docId, adopting);
    return adopting;
  }

  private async load(info: DocInfo): Promise<void> {
    const snapshot = await this.snapshotOf(info.docId);
    if (this.disposed) return;
    const id = `engine-${++this.sequence}`;
    const document: EngineDocument = {
      id,
      path: info.path,
      title: info.title,
      docId: info.docId,
      version: snapshot.version,
      pages: pagesOf(snapshot),
      queue: Promise.resolve(),
      provider: new EngineTileProvider({
        transport: this.transport,
        target: (page) => {
          const pageId = document.pages[page]?.pageId;
          return document.docId === null || pageId === undefined
            ? null
            : { docId: document.docId, pageId };
        },
      }),
      editor: this.editorOf(id),
    };
    // 先登记再放进 store：画布一挂载就会来要瓦片来源和修改入口
    this.documents.set(id, document);
    this.store.getState().openDocument(this.blankSnapshot(id, info.title, snapshot));
    this.ensureAround(document, 0);
  }

  private snapshotOf(docId: number): Promise<DocSnapshot> {
    const params: DocSnapshotParams = { docId };
    return this.transport.request<DocSnapshot>(METHOD.DOC_SNAPSHOT, params);
  }

  /** 只有页面、还没有对象的快照。每一页都取一个新的修订号，画布据此把旧瓦片全部换掉。 */
  private blankSnapshot(id: string, name: string, snapshot: DocSnapshot): DocumentSnapshot {
    return {
      id,
      name,
      dirty: false,
      version: snapshot.version,
      pages: [...snapshot.pages]
        .sort((a, b) => a.index - b.index)
        .map((page, index) => ({
          index,
          width: page.width,
          height: page.height,
          objects: [],
          revision: ++this.revision,
          // 界面重新加载之后接回来的文档，引擎那边可能早就校验过了
          ...overlayOnlyOf(page),
        })),
    };
  }

  private find(docId: number): EngineDocument | undefined {
    for (const document of this.documents.values()) {
      if (document.docId === docId) return document;
    }
    return undefined;
  }

  /** 界面这边不再用这份文档了：释放瓦片来源，让引擎关掉它。 */
  private release(document: EngineDocument): void {
    this.documents.delete(document.id);
    document.provider.dispose();
    if (document.docId === null) return;
    const params: DocCloseParams = { docId: document.docId, discard: true };
    this.transport.request(METHOD.DOC_CLOSE, params).catch((error: unknown) => {
      // 引擎正好在重启的话，文档本来就没了
      if (!isEngineRestarted(error)) console.error('doc.close 失败', error);
    });
  }

  // ---------- 快照的更新 ----------

  /** 改 store 里这份文档的快照；文档已经不在 store 里时什么也不做。 */
  private mutate(
    document: EngineDocument,
    update: (snapshot: DocumentSnapshot) => DocumentSnapshot,
  ): void {
    const current = this.store.getState().documents[document.id]?.snapshot;
    if (!current) return;
    const next = update(current);
    if (next !== current) this.store.getState().updateSnapshot(document.id, next);
  }

  /** 记下一页保真校验的结论：`reason` 有值表示只能叠加内容，null 表示可以修改原有对象。 */
  private setOverlayOnly(document: EngineDocument, index: number, reason: string | null): void {
    this.mutate(document, (snapshot) => {
      const page = snapshot.pages[index];
      if (!page || (page.overlayOnly?.reason ?? null) === reason) return snapshot;
      const { overlayOnly: _before, ...rest } = page;
      const next = reason === null ? rest : { ...rest, overlayOnly: { reason } };
      return { ...snapshot, pages: snapshot.pages.map((p, i) => (i === index ? next : p)) };
    });
  }

  /** 这一页是不是只能叠加内容（按界面现在知道的）。 */
  private isOverlayOnly(document: EngineDocument, index: number): boolean {
    const snapshot = this.store.getState().documents[document.id]?.snapshot;
    return snapshot?.pages[index]?.overlayOnly !== undefined;
  }

  /** 换掉几页上的对象列表（页序号 → 新列表的算法），画面的修订号不动。 */
  private patchObjects(
    document: EngineDocument,
    patches: ReadonlyMap<number, (objects: readonly PageObject[]) => readonly PageObject[]>,
  ): void {
    this.mutate(document, (snapshot) => ({
      ...snapshot,
      pages: snapshot.pages.map((page) => {
        const patch = patches.get(page.index);
        return patch ? { ...page, objects: patch(page.objects) } : page;
      }),
    }));
  }

  // ---------- 页面的对象 ----------

  /** 保证这一页和前后各一页的对象在取，或者已经取到。 */
  private ensureAround(document: EngineDocument, index: number): void {
    for (const i of [index, index + 1, index - 1]) {
      const page = document.pages[i];
      if (page && !page.loaded && page.loading === 0) void this.loadPage(document, i);
    }
  }

  /**
   * 取一页的对象列表放进 store（第一次还要先取这一页的旋转和可见区域）。
   * 已经取过的页再调就是重新取：文档变了之后以引擎的对象为准。
   */
  private async loadPage(document: EngineDocument, index: number): Promise<void> {
    const page = document.pages[index];
    const docId = document.docId;
    if (!page || docId === null) return;
    const turn = ++this.loadSequence;
    page.loading = turn;
    /** 这一次还作数吗：没有更新的一次加载，引擎也没有换 */
    const current = () =>
      page.loading === turn && document.docId === docId && document.pages[index] === page;
    try {
      let display = page.display;
      if (!display) {
        const params: DocSnapshotParams = { docId, pageIds: [page.pageId] };
        const detail = await this.transport.request<DocSnapshot>(METHOD.DOC_SNAPSHOT, params);
        const info = detail.pages[0];
        if (!info?.cropBox || info.rotate === undefined || info.rotate === null) {
          throw new Error('doc.snapshot 没有给出页面的旋转和可见区域');
        }
        if (!current()) return;
        display = displayMatrix({ cropBox: info.cropBox, rotate: info.rotate });
        page.display = display;
        if (info.fidelity !== 'pending') {
          this.setOverlayOnly(document, index, overlayOnlyOf(info).overlayOnly?.reason ?? null);
        }
      }
      const summaries = await this.listObjects(docId, page.pageId);
      if (!current()) return;
      page.loading = 0;
      if (page.pendingEdits > 0) {
        // 这份列表不含界面上已经先改的那几步，现在换上去对象会跳回原处；等那几步有了结果再取
        page.reloadAfterEdits = true;
        return;
      }
      page.loaded = true;
      const matrix = display;
      const objects = summaries.map((summary) => toPageObject(index, summary, matrix));
      this.patchObjects(document, new Map([[index, () => objects]]));
    } catch (error) {
      if (page.loading === turn) page.loading = 0;
      // 引擎重启时请求会失败；文档恢复之后会重新取
      if (!isEngineRestarted(error)) console.error('取页面对象失败', error);
    }
  }

  /** 一页的全部对象，按层级从下到上。对象多的页分几次取（《接口协议》5.3）。 */
  private async listObjects(docId: number, pageId: number): Promise<ObjectSummary[]> {
    const all: ObjectSummary[] = [];
    for (;;) {
      const params: ObjListParams = { docId, pageId, offset: all.length, limit: OBJ_LIST_MAX };
      const batch = await this.transport.request<ObjectSummary[]>(METHOD.OBJ_LIST, params);
      all.push(...batch);
      if (batch.length < OBJ_LIST_MAX) return all;
    }
  }

  // ---------- 修改 ----------

  /** 文档的修改入口。按 id 现找文档：文档关掉之后这些调用什么也不做。 */
  private editorOf(id: string): DocumentEditor {
    const on = (run: (document: EngineDocument) => void) => {
      const document = this.documents.get(id);
      if (document) run(document);
    };
    return {
      transformObjects: (changes) => on((document) => this.transformObjects(document, changes)),
      deleteObjects: (ids) => on((document) => this.deleteObjects(document, ids)),
      undo: () => on((document) => this.stepHistory(document, METHOD.HISTORY_UNDO)),
      redo: () => on((document) => this.stepHistory(document, METHOD.HISTORY_REDO)),
    };
  }

  private transformObjects(document: EngineDocument, changes: readonly PageTransform[]): void {
    const commands: { readonly ids: number[]; readonly matrix: Matrix }[] = [];
    const patches = new Map<number, (objects: readonly PageObject[]) => readonly PageObject[]>();
    // 工具不会让只能叠加内容的页上的对象动起来；这里再拦一道，这样的页一个对象也不发给引擎
    if (changes.some((change) => this.isOverlayOnly(document, change.page))) return;
    for (const change of changes) {
      const display = document.pages[change.page]?.display;
      const matrix = display && pageTransformToUser(display, change.matrix);
      const ids = engineIds(change.ids);
      if (!matrix || ids.length === 0) continue;
      commands.push({ ids, matrix });
      const moved = new Set(change.ids);
      patches.set(change.page, (objects) =>
        objects.map((object) =>
          moved.has(object.id)
            ? { ...object, bbox: transformRect(change.matrix, object.bbox) }
            : object,
        ),
      );
    }
    if (commands.length === 0) return;
    // 先按预期把包围盒挪过去，不等引擎；引擎的结果到了以它为准
    this.patchObjects(document, patches);
    // 一页一条命令：各页的旋转和可见区域可以不同，换到用户空间之后矩阵也就不同
    this.enqueue(document, [...patches.keys()], async () => {
      for (const { ids, matrix } of commands) {
        await this.command(document, METHOD.EDIT_TRANSFORM, (docId, baseVersion) => {
          // 剪切路径默认跟着对象一起动（方案 4.1）
          const params: EditTransformParams = {
            docId,
            baseVersion,
            ids,
            matrix: [...matrix],
            clipFollows: true,
          };
          return params;
        });
      }
    });
  }

  private deleteObjects(document: EngineDocument, ids: readonly string[]): void {
    const targets = engineIds(ids);
    if (targets.length === 0) return;
    const gone = new Set(ids);
    const patches = new Map<number, (objects: readonly PageObject[]) => readonly PageObject[]>();
    for (const id of ids) {
      const page = pageOfObject(id);
      if (page !== null) patches.set(page, (objects) => objects.filter((o) => !gone.has(o.id)));
    }
    if ([...patches.keys()].some((page) => this.isOverlayOnly(document, page))) return;
    this.patchObjects(document, patches);
    // 几页上的对象一起删是一条命令，撤销时一起回来
    this.enqueue(document, [...patches.keys()], () =>
      this.command(document, METHOD.EDIT_DELETE, (docId, baseVersion) => {
        const params: EditDeleteParams = { docId, baseVersion, ids: targets };
        return params;
      }),
    );
  }

  private stepHistory(document: EngineDocument, method: string): void {
    this.enqueue(document, [], async () => {
      try {
        await this.command(document, method, (docId) => {
          const params: HistoryParams = { docId };
          return params;
        });
      } catch (error) {
        // 没有可撤销（或可重做）的操作：引擎借用“参数不合法”来报，这不算出错
        if (isRpcError(error) && error.code === ERROR_CODE.E_INVALID_ARGUMENT) return;
        throw error;
      }
    });
  }

  /**
   * 把一条修改排进这份文档的队列。`touched` 是界面上已经先改了的页：
   * 命令有结果之前，这几页晚到的对象列表先不用。
   */
  private enqueue(
    document: EngineDocument,
    touched: readonly number[],
    task: () => Promise<void>,
  ): void {
    const pages = touched.flatMap((index) => document.pages[index] ?? []);
    for (const page of pages) page.pendingEdits++;
    document.queue = document.queue.then(async () => {
      try {
        await task();
      } catch (error) {
        // 界面上先改的要撤回来：等会儿按引擎的对象列表重新取
        for (const page of pages) page.reloadAfterEdits = true;
        this.reportEditFailure(document, pages, error);
      } finally {
        for (const page of pages) {
          page.pendingEdits--;
          if (page.pendingEdits > 0 || !page.reloadAfterEdits) continue;
          page.reloadAfterEdits = false;
          // 引擎重启过的话，这一页的记录已经换了新的，不用管旧的
          const index = document.pages.indexOf(page);
          if (index >= 0) void this.loadPage(document, index);
        }
      }
    });
  }

  /**
   * 发一条修改命令并记下新的版本号。界面记的版本号落后时（`E_VERSION_CONFLICT`），
   * 问一下现在的版本再试一次（《接口协议》5.2）。
   */
  private async command(
    document: EngineDocument,
    method: string,
    params: (docId: number, baseVersion: number) => unknown,
  ): Promise<void> {
    for (let attempt = 0; ; attempt++) {
      const docId = document.docId;
      if (docId === null) {
        const restarting: RpcError = {
          code: ERROR_CODE.E_ENGINE_RESTARTED,
          message: '处理过程中引擎重启',
        };
        throw restarting;
      }
      try {
        const result = await this.transport.request<EditResult>(
          method,
          params(docId, document.version),
        );
        document.version = Math.max(document.version, result.version);
        return;
      } catch (error) {
        const conflict = isRpcError(error) && error.code === ERROR_CODE.E_VERSION_CONFLICT;
        if (!conflict || attempt > 0 || document.docId !== docId) throw error;
        document.version = Math.max(document.version, (await this.snapshotOf(docId)).version);
      }
    }
  }

  private reportEditFailure(
    document: EngineDocument,
    touched: readonly EnginePage[],
    error: unknown,
  ): void {
    // 引擎重启另有提示，文档恢复后整份重新载入
    if (isEngineRestarted(error)) return;
    if (isRpcError(error) && error.code === ERROR_CODE.E_PAGE_OVERLAY_ONLY) {
      // 用户赶在后台的保真校验做完之前动了手。不弹框：对象回到原处，这一页的提示条随校验结论出现。
      // 引擎拒绝时通常已经把结论推送过来了；万一界面还不知道，点名问一次。
      const unknown = touched.filter((page) => {
        const index = document.pages.indexOf(page);
        return index >= 0 && !this.isOverlayOnly(document, index);
      });
      if (unknown.length > 0) this.askFidelity(document, unknown);
      return;
    }
    console.error('修改命令失败', error);
    this.notify('error', format(t.host.editFailed, { reason: describeError(error) }));
  }

  /** 请引擎马上校验这几页；结论走 `fidelity.result`。 */
  private askFidelity(document: EngineDocument, pages: readonly EnginePage[]): void {
    if (document.docId === null) return;
    const params: FidelityCheckParams = {
      docId: document.docId,
      pageIds: pages.map((page) => page.pageId),
    };
    this.transport.request(METHOD.FIDELITY_CHECK, params).catch((error: unknown) => {
      if (!isEngineRestarted(error)) console.error('fidelity.check 失败', error);
    });
  }

  // ---------- 事件 ----------

  private onEvent(event: EngineEvent): void {
    switch (event.method) {
      case EVENT.DOC_CHANGED:
        this.onDocumentChanged(event.params as DocChanged);
        break;
      case EVENT.FIDELITY_RESULT:
        this.onFidelityResult(event.params as FidelityResult);
        break;
      case EVENT.ENGINE_RESTARTING:
        // 旧的 docId 全部作废，等 engine.restored 给新的（《接口协议》5.1）
        for (const document of this.documents.values()) document.docId = null;
        break;
      case EVENT.ENGINE_RESTORED:
        void this.onRestored(event.params as EngineRestored);
        break;
      case EVENT.ENGINE_FAILED:
        this.notify(
          'error',
          format(t.host.engineFailed, { reason: (event.params as EngineFailed).message }),
        );
        break;
      default:
        break;
    }
  }

  /**
   * 文档变了（《接口协议》5.2）：记下新版本号；变了的页取一个新的修订号，画布据此重画；
   * 这些页的对象列表界面上在用的话，重新取一份。
   */
  private onDocumentChanged(change: DocChanged): void {
    const document = this.find(change.docId);
    if (!document) return;
    document.version = Math.max(document.version, change.version);
    const repainted = new Map<number, number>();
    for (const { pageId } of change.changes) {
      const index = document.pages.findIndex((page) => page.pageId === pageId);
      if (index >= 0) repainted.set(index, ++this.revision);
    }
    this.mutate(document, (snapshot) => ({
      ...snapshot,
      version: document.version,
      dirty: true,
      pages: snapshot.pages.map((page) => {
        const revision = repainted.get(page.index);
        return revision === undefined ? page : { ...page, revision };
      }),
    }));
    for (const index of repainted.keys()) {
      const page = document.pages[index];
      if (page && (page.loaded || page.loading !== 0)) void this.loadPage(document, index);
    }
  }

  /** 一页的保真校验有了结论（方案 2.3）。 */
  private onFidelityResult(result: FidelityResult): void {
    const document = this.find(result.docId);
    if (!document || result.status === 'pending') return;
    const index = document.pages.findIndex((page) => page.pageId === result.pageId);
    if (index < 0) return;
    this.setOverlayOnly(document, index, overlayOnlyOf(result).overlayOnly?.reason ?? null);
  }

  /** 新引擎就绪了：按路径把手里的文档和新的 `docId` 对上，重新载入；对不上的关掉。 */
  private async onRestored(restored: EngineRestored): Promise<void> {
    const lost: string[] = [];
    for (const document of [...this.documents.values()]) {
      const match = restored.docs.find((info) => samePath(info.path, document.path));
      if (!match) {
        lost.push(document.title);
        this.store.getState().closeDocument(document.id);
        continue;
      }
      try {
        const snapshot = await this.snapshotOf(match.docId);
        if (this.disposed || !this.documents.has(document.id)) continue;
        // 对象编号是新引擎重新分配的（《接口协议》5.1）：页面的记录全部换新，对象重新取，选区随之清空
        document.docId = match.docId;
        document.version = snapshot.version;
        document.pages = pagesOf(snapshot);
        this.store
          .getState()
          .updateSnapshot(document.id, this.blankSnapshot(document.id, document.title, snapshot));
        const currentPage = this.store.getState().documents[document.id]?.currentPage ?? 0;
        this.ensureAround(document, currentPage);
      } catch (error) {
        console.error('引擎重启后重新载入文档失败', error);
        lost.push(document.title);
        this.store.getState().closeDocument(document.id);
      }
    }
    if (this.disposed) return;
    this.notify('info', t.host.engineRestored);
    for (const name of lost) {
      this.notify('error', format(t.host.documentNotRestored, { name }));
    }
  }

  private notify(kind: HostNotice['kind'], message: string): void {
    for (const listener of this.noticeListeners) listener({ kind, message });
  }
}

function pagesOf(snapshot: DocSnapshot): EnginePage[] {
  return [...snapshot.pages]
    .sort((a, b) => a.index - b.index)
    .map((page) => ({
      pageId: page.pageId,
      display: null,
      loaded: false,
      loading: 0,
      pendingEdits: 0,
      reloadAfterEdits: false,
    }));
}

/**
 * 引擎说的保真校验状态换成页面快照里的 `overlayOnly`：只有“只能叠加内容”时才有这一项。
 * 快照里的页面信息和 `fidelity.result` 事件字段名不同，都认。
 */
function overlayOnlyOf(
  page: Pick<PageInfo, 'fidelity' | 'fidelityReason'> | Pick<FidelityResult, 'status' | 'reason'>,
): { overlayOnly?: { reason: string } } {
  const [status, reason] =
    'status' in page ? [page.status, page.reason] : [page.fidelity, page.fidelityReason];
  return status === 'overlayOnly' ? { overlayOnly: { reason: reason ?? '' } } : {};
}

const OBJECT_TYPES: Record<ObjectKind, ObjectType> = {
  text: 'text',
  path: 'path',
  image: 'image',
  shading: 'shading',
  form: 'form',
  unknown: 'other',
};

/** 引擎的对象摘要换成界面的对象：包围盒从 PDF 用户空间换到页面坐标。 */
function toPageObject(page: number, summary: ObjectSummary, display: Matrix): PageObject {
  const type = OBJECT_TYPES[summary.kind];
  // 引擎给不出包围盒的对象在画布上没有位置，只能从图层面板里选
  const pickable = summary.pickable && Boolean(summary.bbox);
  return {
    id: objectId(page, summary.id),
    type,
    name: `${t.objectTypes[type]} ${summary.id}`,
    bbox: summary.bbox
      ? userBoxToPageRect(display, summary.bbox)
      : { x: 0, y: 0, width: 0, height: 0 },
    ...(summary.locked ? { locked: true } : {}),
    ...(pickable ? {} : { pickable: false }),
  };
}

/** Windows 的路径不分大小写，斜杠方向也可能不同。 */
function samePath(a: string, b: string): boolean {
  const normalize = (path: string) => path.replace(/\//g, '\\').toLowerCase();
  return normalize(a) === normalize(b);
}
