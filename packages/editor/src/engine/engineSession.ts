import { EVENT, METHOD } from '@magpdf/protocol';
import type {
  DocChanged,
  DocCloseParams,
  DocInfo,
  DocSnapshot,
  DocSnapshotParams,
  EngineFailed,
  EngineRestored,
} from '@magpdf/protocol';
import type { TileProvider } from '../canvas/tileProvider';
import type { DocumentSnapshot } from '../document/types';
import { format, t } from '../i18n';
import type { EditorHost, HostNotice } from '../shell/editorHost';
import type { EditorStore } from '../store/editorStore';
import { EngineTileProvider } from './engineTileProvider';
import { isEngineRestarted } from './transport';
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

/** 一份由引擎打开的文档在界面这边的记录。 */
interface EngineDocument {
  /** 界面里的文档 id。引擎重启后 `docId` 会变，它不变 */
  readonly id: string;
  readonly path: string;
  readonly title: string;
  /** 引擎里的文档编号；引擎重启期间是 null */
  docId: number | null;
  /** 页序号 → pageId */
  pageIds: readonly number[];
  readonly provider: EngineTileProvider;
}

/**
 * 界面这一侧的引擎会话：把引擎打开的文档放进编辑器的 store，给每份文档配一个瓦片来源，
 * 并跟着引擎的事件走——文档变了更新快照，引擎重启后把文档重新对上（《接口协议》5.1、5.2、第 7 节）。
 *
 * 现在只接了“看”：页面列表和瓦片。对象列表、选择和修改命令还没有接，真实文档的 `objects` 是空的。
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
  private disposed = false;

  constructor(options: EngineSessionOptions) {
    this.options = options;
    this.transport = options.transport;
    this.store = options.store;
    this.unsubscribeEvents = this.transport.onEvent((event) => this.onEvent(event));
    // 标签页关掉之后，把引擎里的文档也关掉
    this.unsubscribeStore = this.store.subscribe((state, previous) => {
      if (state.documents === previous.documents) return;
      for (const document of [...this.documents.values()]) {
        if (!state.documents[document.id]) this.release(document);
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
      pageIds: pageIdsOf(snapshot),
      provider: new EngineTileProvider({
        transport: this.transport,
        target: (page) => {
          const pageId = document.pageIds[page];
          return document.docId === null || pageId === undefined
            ? null
            : { docId: document.docId, pageId };
        },
      }),
    };
    // 先登记再放进 store：画布一挂载就会来要瓦片来源
    this.documents.set(id, document);
    this.store.getState().openDocument(toDocumentSnapshot(id, info.title, snapshot, false));
  }

  private snapshotOf(docId: number): Promise<DocSnapshot> {
    const params: DocSnapshotParams = { docId };
    return this.transport.request<DocSnapshot>(METHOD.DOC_SNAPSHOT, params);
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

  private onEvent(event: EngineEvent): void {
    switch (event.method) {
      case EVENT.DOC_CHANGED:
        this.onDocumentChanged(event.params as DocChanged);
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
          format(t.host.engineFailed, {
            reason: (event.params as EngineFailed).message,
          }),
        );
        break;
      default:
        break;
    }
  }

  /** 文档变了：记下新版本号，变了的页换一个新的对象，画布据此重画那几页（《接口协议》5.2）。 */
  private onDocumentChanged(change: DocChanged): void {
    const document = this.find(change.docId);
    const current = document && this.store.getState().documents[document.id]?.snapshot;
    if (!document || !current) return;
    const changed = new Set(change.changes.map((page) => document.pageIds.indexOf(page.pageId)));
    this.store.getState().openDocument(
      {
        ...current,
        version: change.version,
        dirty: true,
        pages: current.pages.map((page) => (changed.has(page.index) ? { ...page } : page)),
      },
      { activate: false },
    );
  }

  /** 新引擎就绪了：按路径把手里的文档和新的 `docId` 对上，重新拉快照；对不上的关掉。 */
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
        document.docId = match.docId;
        document.pageIds = pageIdsOf(snapshot);
        // 每一页都是新对象，画布会把旧瓦片全部丢掉重新要
        this.store
          .getState()
          .openDocument(toDocumentSnapshot(document.id, document.title, snapshot, false), {
            activate: false,
          });
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

function pageIdsOf(snapshot: DocSnapshot): number[] {
  return [...snapshot.pages].sort((a, b) => a.index - b.index).map((page) => page.pageId);
}

function toDocumentSnapshot(
  id: string,
  name: string,
  snapshot: DocSnapshot,
  dirty: boolean,
): DocumentSnapshot {
  return {
    id,
    name,
    dirty,
    version: snapshot.version,
    pages: [...snapshot.pages]
      .sort((a, b) => a.index - b.index)
      .map((page, index) => ({ index, width: page.width, height: page.height, objects: [] })),
  };
}

/** Windows 的路径不分大小写，斜杠方向也可能不同。 */
function samePath(a: string, b: string): boolean {
  const normalize = (path: string) => path.replace(/\//g, '\\').toLowerCase();
  return normalize(a) === normalize(b);
}
