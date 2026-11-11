import { ERROR_CODE, EVENT, METHOD } from '@magpdf/protocol';
import type {
  DocChanged,
  DocInfo,
  DocSnapshot,
  DocSnapshotParams,
  EditDeleteParams,
  EditResult,
  EditTransformParams,
  FidelityCheckParams,
  FidelityResult,
  HistoryParams,
  ObjListParams,
  ObjectKind,
  ObjectSummary,
  PageInfo,
  RpcError,
} from '@magpdf/protocol';
import { FakeTransport } from './fakeTransport';

/**
 * 测试用的假引擎：照着《接口协议》应答文档、对象和修改命令，状态都在内存里。
 * 只模拟界面会话用到的那部分行为；几何量和真引擎一样用 PDF 用户空间。不在包的出口里。
 */
export type UserBox = [number, number, number, number];

export interface FakeObject {
  readonly id: number;
  readonly kind: ObjectKind;
  /** `[left, bottom, right, top]`；null 表示引擎给不出包围盒 */
  bbox: UserBox | null;
  readonly pickable?: boolean;
}

export interface FakePage {
  readonly pageId: number;
  /** 显示尺寸 */
  readonly width: number;
  readonly height: number;
  readonly rotate: number;
  readonly cropBox: UserBox;
  objects: FakeObject[];
  /** 为真时这一页通不过保真校验：修改命令被拒绝，校验的结论是“只能叠加内容” */
  overlayOnly?: boolean;
  /**
   * 保真校验做过没有。和真引擎一样，校验是晚一步的事：页面显示之后引擎空闲时做
   * （测试里调 `finishBackgroundCheck`），或者修改命令、`fidelity.check` 顺带做。
   */
  checked?: boolean;
}

/** 假引擎给“只能叠加内容”的页报的原因。 */
export const FAKE_OVERLAY_REASON = '重新生成内容流后页面的画面变了';

interface FakeDocument {
  readonly docId: number;
  readonly path: string;
  version: number;
  readonly pages: FakePage[];
  /** 撤销栈：每一步记下受影响的页，以及怎么撤、怎么重做 */
  readonly undo: Step[];
  readonly redo: Step[];
}

interface Step {
  readonly pages: readonly number[];
  readonly forward: () => void;
  readonly backward: () => void;
}

export class FakeEngine {
  readonly transport = new FakeTransport();
  /** 修改命令的 `doc.changed` 和应答哪个先到界面：两条路没有先后保证，测试里两种都要走 */
  eventFirst = true;
  private readonly documents = new Map<number, FakeDocument>();
  private nextDocId = 1;

  constructor() {
    this.transport.handle = (method, params) => this.handle(method, params);
  }

  /** 打开一份文档，返回主进程会交给界面的 `DocInfo`。 */
  open(name: string, pages: readonly FakePage[]): DocInfo {
    const docId = this.nextDocId++;
    const path = `C:\\docs\\${name}`;
    this.documents.set(docId, {
      docId,
      path,
      version: 0,
      pages: pages.map((page) => ({ ...page, objects: page.objects.map((o) => ({ ...o })) })),
      undo: [],
      redo: [],
    });
    return this.info(docId);
  }

  info(docId: number): DocInfo {
    const document = this.document(docId);
    const first = document.pages[0];
    return {
      docId,
      path: document.path,
      title: document.path.split('\\').pop() ?? '',
      version: document.version,
      pageCount: document.pages.length,
      pageWidth: first?.width ?? 0,
      pageHeight: first?.height ?? 0,
    };
  }

  /** 引擎被换掉了：旧文档全部作废，按原样重新打开 `keep` 里的那些（`docId` 是新的，修改和撤销记录都没了）。 */
  restart(
    keep: readonly { readonly name: string; readonly pages: readonly FakePage[] }[],
  ): DocInfo[] {
    this.documents.clear();
    this.nextDocId += 10;
    return keep.map(({ name, pages }) => this.open(name, pages));
  }

  page(docId: number, pageId: number): FakePage {
    const page = this.document(docId).pages.find((p) => p.pageId === pageId);
    if (!page) throw invalid(`页面 ${pageId} 不存在（文档 ${docId}）`);
    return page;
  }

  version(docId: number): number {
    return this.document(docId).version;
  }

  /** 文档的版本号往前走了一步，而界面没有收到 `doc.changed`。用来摆出“界面记的版本号落后”的情形。 */
  advanceVersionSilently(docId: number): void {
    this.document(docId).version += 1;
  }

  /** 引擎空闲下来，把这一页的保真校验做了（真引擎在页面显示之后自己会做），推送 `fidelity.result`。 */
  finishBackgroundCheck(docId: number, pageId: number): void {
    const page = this.page(docId, pageId);
    if (!page.checked) this.check(docId, page);
  }

  /** 校验一页并推送结论。 */
  private check(docId: number, page: FakePage): void {
    page.checked = true;
    const result: FidelityResult = {
      docId,
      pageId: page.pageId,
      status: page.overlayOnly ? 'overlayOnly' : 'ok',
      ...(page.overlayOnly ? { reason: FAKE_OVERLAY_REASON } : {}),
    };
    // 结论和请求的应答走的是两条路，没有先后保证
    if (this.eventFirst) this.transport.emit(EVENT.FIDELITY_RESULT, result);
    else setTimeout(() => this.transport.emit(EVENT.FIDELITY_RESULT, result), 0);
  }

  private document(docId: number): FakeDocument {
    const document = this.documents.get(docId);
    if (!document) throw invalid(`文档 ${docId} 不存在`);
    return document;
  }

  private handle(method: string, params: unknown): unknown {
    switch (method) {
      case METHOD.DOC_SNAPSHOT:
        return this.snapshot(params as DocSnapshotParams);
      case METHOD.OBJ_LIST:
        return this.list(params as ObjListParams);
      case METHOD.DOC_CLOSE:
        this.documents.delete((params as { docId: number }).docId);
        return null;
      case METHOD.EDIT_TRANSFORM:
        return this.transform(params as EditTransformParams);
      case METHOD.EDIT_DELETE:
        return this.remove(params as EditDeleteParams);
      case METHOD.HISTORY_UNDO:
        return this.history(params as HistoryParams, 'undo');
      case METHOD.HISTORY_REDO:
        return this.history(params as HistoryParams, 'redo');
      case METHOD.FIDELITY_CHECK: {
        const { docId, pageIds } = params as FidelityCheckParams;
        // 先确认页都在；已经校验过的页也回一条结论
        const pages = pageIds.map((pageId) => this.page(docId, pageId));
        for (const page of pages) this.check(docId, page);
        return null;
      }
      default:
        throw { code: ERROR_CODE.E_PROTOCOL, message: `假引擎没有实现 ${method}` };
    }
  }

  private snapshot(params: DocSnapshotParams): DocSnapshot {
    const document = this.document(params.docId);
    const brief = (page: FakePage, index: number): PageInfo => ({
      pageId: page.pageId,
      index,
      width: page.width,
      height: page.height,
      fidelity: !page.checked ? 'pending' : page.overlayOnly ? 'overlayOnly' : 'ok',
      ...(page.checked && page.overlayOnly ? { fidelityReason: FAKE_OVERLAY_REASON } : {}),
    });
    if (!params.pageIds) {
      return { version: document.version, pages: document.pages.map(brief) };
    }
    return {
      version: document.version,
      pages: params.pageIds.map((pageId) => {
        const page = this.page(params.docId, pageId);
        return {
          ...brief(page, document.pages.indexOf(page)),
          rotate: page.rotate,
          cropBox: [...page.cropBox],
        };
      }),
    };
  }

  private list(params: ObjListParams): ObjectSummary[] {
    const page = this.page(params.docId, params.pageId);
    const limit = Math.min(params.limit ?? 2000, 2000);
    return page.objects.slice(params.offset, params.offset + limit).map((object, i) => ({
      id: object.id,
      kind: object.kind,
      ...(object.bbox ? { bbox: [...object.bbox] as UserBox } : {}),
      z: params.offset + i,
      locked: false,
      pickable: object.pickable ?? true,
    }));
  }

  /** 找到这些对象所在的页（页在文档里的序号 → 对象）。有不存在的对象，或者所在的页不让改，就报错。 */
  private locate(document: FakeDocument, baseVersion: number, ids: readonly number[]) {
    if (baseVersion !== document.version) {
      const conflict: RpcError = {
        code: ERROR_CODE.E_VERSION_CONFLICT,
        message: '文档已经被别的操作修改',
      };
      throw conflict;
    }
    const found = new Map<number, FakeObject[]>();
    for (const id of ids) {
      const index = document.pages.findIndex((page) => page.objects.some((o) => o.id === id));
      if (index < 0) throw { code: ERROR_CODE.E_OBJECT_NOT_FOUND, message: '对象不存在' };
      const page = document.pages[index]!;
      const list = found.get(index) ?? [];
      list.push(page.objects.find((o) => o.id === id)!);
      found.set(index, list);
    }
    // 动手之前先校验碰到的页（还没校验过的现在做，结论照样推送）；有一页通不过，整条命令都不做
    for (const index of found.keys()) {
      const page = document.pages[index]!;
      if (!page.checked) this.check(document.docId, page);
      if (page.overlayOnly) {
        throw {
          code: ERROR_CODE.E_PAGE_OVERLAY_ONLY,
          message: '页面保真校验未通过，只能叠加内容',
        };
      }
    }
    return found;
  }

  private transform(params: EditTransformParams): EditResult {
    const document = this.document(params.docId);
    const found = this.locate(document, params.baseVersion, params.ids);
    const objects = [...found.values()].flat();
    const before = objects.map((o) => o.bbox);
    const after = before.map((box) => (box ? transformBox(params.matrix, box) : null));
    const set = (boxes: readonly (UserBox | null)[]) =>
      objects.forEach((object, i) => {
        object.bbox = boxes[i] ?? null;
      });
    return this.commit(document, {
      pages: [...found.keys()],
      forward: () => set(after),
      backward: () => set(before),
    });
  }

  private remove(params: EditDeleteParams): EditResult {
    const document = this.document(params.docId);
    const found = this.locate(document, params.baseVersion, params.ids);
    const before = new Map(
      [...found.keys()].map((index) => [index, [...document.pages[index]!.objects]]),
    );
    const gone = new Set(params.ids);
    return this.commit(document, {
      pages: [...found.keys()],
      forward: () => {
        for (const index of found.keys()) {
          const page = document.pages[index]!;
          page.objects = page.objects.filter((o) => !gone.has(o.id));
        }
      },
      backward: () => {
        for (const [index, objects] of before) document.pages[index]!.objects = [...objects];
      },
    });
  }

  private commit(document: FakeDocument, step: Step): EditResult {
    step.forward();
    document.undo.push(step);
    document.redo.length = 0;
    return this.changed(document, step.pages);
  }

  private history(params: HistoryParams, direction: 'undo' | 'redo'): EditResult {
    const document = this.document(params.docId);
    const [from, to] =
      direction === 'undo' ? [document.undo, document.redo] : [document.redo, document.undo];
    const step = from.pop();
    // 真引擎也是借用“参数不合法”来报“没有可撤销的操作”
    if (!step) throw invalid(direction === 'undo' ? '没有可撤销的操作' : '没有可重做的操作');
    if (direction === 'undo') step.backward();
    else step.forward();
    to.push(step);
    return this.changed(document, step.pages);
  }

  /** 版本号加 1，发 `doc.changed`，给出应答。 */
  private changed(document: FakeDocument, pages: readonly number[]): EditResult {
    document.version += 1;
    const event: DocChanged = {
      docId: document.docId,
      version: document.version,
      changes: pages.map((index) => ({
        pageId: document.pages[index]!.pageId,
        added: [],
        removed: [],
        modified: [],
      })),
      pagesChanged: false,
    };
    if (this.eventFirst) this.transport.emit(EVENT.DOC_CHANGED, event);
    // 应答交到界面要过两个微任务，再晚一点发事件就是“应答先到”
    else setTimeout(() => this.transport.emit(EVENT.DOC_CHANGED, event), 0);
    return { version: document.version };
  }
}

function invalid(detail: string): RpcError {
  return { code: ERROR_CODE.E_INVALID_ARGUMENT, message: '参数不合法', data: { detail } };
}

/** 包围盒的四个角按矩阵变换之后的包围盒。 */
function transformBox(matrix: readonly number[], box: UserBox): UserBox {
  const [a, b, c, d, e, f] = matrix as [number, number, number, number, number, number];
  const corners = [
    [box[0], box[1]],
    [box[2], box[1]],
    [box[0], box[3]],
    [box[2], box[3]],
  ].map(([x, y]) => [a * x! + c * y! + e, b * x! + d * y! + f] as const);
  const xs = corners.map((p) => p[0]);
  const ys = corners.map((p) => p[1]);
  return [Math.min(...xs), Math.min(...ys), Math.max(...xs), Math.max(...ys)];
}
