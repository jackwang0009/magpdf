import { EVENT, METHOD } from '@magpdf/protocol';
import type { DocInfo, DocSnapshot } from '@magpdf/protocol';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { HostNotice } from '../shell/editorHost';
import { createEditorStore } from '../store/editorStore';
import { EngineSession } from './engineSession';
import { FakeTransport } from './fakeTransport';

function docInfo(docId: number, name: string): DocInfo {
  return {
    docId,
    path: `C:\\docs\\${name}`,
    title: name,
    version: 0,
    pageCount: 2,
    pageWidth: 300,
    pageHeight: 440,
  };
}

function snapshot(version = 0): DocSnapshot {
  return {
    version,
    pages: [
      // 故意倒着给，界面要按 index 排
      { pageId: 21, index: 1, width: 440, height: 300 },
      { pageId: 20, index: 0, width: 300, height: 440 },
    ],
  };
}

/** 一个应答 doc.snapshot 和 doc.close 的假引擎，外加会话和 store。 */
function setup(options: { open?: DocInfo[]; pick?: () => Promise<DocInfo | null> } = {}) {
  const transport = new FakeTransport();
  const snapshots = new Map<number, DocSnapshot>();
  transport.handle = (method, params) => {
    const { docId } = params as { docId: number };
    if (method === METHOD.DOC_SNAPSHOT) {
      const found = snapshots.get(docId) ?? snapshot();
      return found;
    }
    if (method === METHOD.DOC_CLOSE) return null;
    throw { code: 1001, message: `测试没有处理 ${method}` };
  };
  const store = createEditorStore();
  const session = new EngineSession({
    transport,
    store,
    pickDocument: options.pick ?? (() => Promise.resolve(null)),
    listDocuments: () => Promise.resolve(options.open ?? []),
  });
  const notices: HostNotice[] = [];
  session.onNotice((notice) => notices.push(notice));
  return { transport, store, session, snapshots, notices };
}

function settle(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe('EngineSession', () => {
  it('启动时把已经开着的文档接进来：页面按序号排，每份文档有自己的瓦片来源', async () => {
    const { store, session, transport } = setup({
      open: [docInfo(1, '提单.pdf'), docInfo(2, '发票.pdf')],
    });
    await session.attach();

    const state = store.getState();
    expect(state.documentOrder).toEqual(['engine-1', 'engine-2']);
    expect(state.activeDocumentId).toBe('engine-2');
    const first = state.documents['engine-1']!.snapshot;
    expect(first).toMatchObject({ name: '提单.pdf', dirty: false, version: 0 });
    expect(first.pages).toEqual([
      { index: 0, width: 300, height: 440, objects: [] },
      { index: 1, width: 440, height: 300, objects: [] },
    ]);
    expect(transport.sent(METHOD.DOC_SNAPSHOT)).toEqual([{ docId: 1 }, { docId: 2 }]);

    const provider = session.tileProvider('engine-1');
    expect(provider).toBeDefined();
    expect(session.tileProvider('engine-2')).not.toBe(provider);
    expect(session.tileProvider('mock-1')).toBeUndefined();
  });

  it('瓦片请求带的是这份文档的 docId 和这一页的 pageId', async () => {
    const { session, transport } = setup({ open: [docInfo(5, '提单.pdf')] });
    await session.attach();
    const baseline = transport.handle;
    transport.handle = (method, params) => {
      if (method === METHOD.RENDER_TILES) throw { code: 3005, message: '到此为止' };
      return baseline(method, params);
    };
    await expect(
      session.tileProvider('engine-1')!.renderTile({
        page: 1,
        level: 6,
        pxPerPt: 2,
        tx: 0,
        ty: 0,
        tileSize: 256,
        pixelRect: { x: 0, y: 0, width: 256, height: 256 },
        pageRect: { x: 0, y: 0, width: 128, height: 128 },
        documentVersion: 0,
        signal: new AbortController().signal,
      }),
    ).rejects.toThrow('到此为止');
    expect(transport.sent(METHOD.RENDER_TILES)).toEqual([
      { docId: 5, pageId: 21, scale: 2, tiles: [{ x: 0, y: 0 }], priority: 'visible' },
    ]);
  });

  it('「打开」：主进程选好文件交回来就接进来；取消什么也不做；同一份文档不接两次', async () => {
    let picked: DocInfo | null = null;
    const { store, session } = setup({ pick: () => Promise.resolve(picked) });

    await session.openDocument();
    expect(store.getState().documentOrder).toEqual([]);

    picked = docInfo(3, '提单.pdf');
    // 连点两下，第二次在第一次还没接完的时候到
    await Promise.all([session.openDocument(), session.openDocument()]);
    expect(store.getState().documentOrder).toEqual(['engine-1']);

    // 已经开着：切过去，不再开一份
    store
      .getState()
      .openDocument({ id: 'mock-1', name: '模拟', dirty: false, version: 0, pages: [] });
    expect(store.getState().activeDocumentId).toBe('mock-1');
    await session.openDocument();
    expect(store.getState().documentOrder).toEqual(['engine-1', 'mock-1']);
    expect(store.getState().activeDocumentId).toBe('engine-1');
  });

  it('打不开时把引擎的错误原样抛给外壳', async () => {
    const { session } = setup({
      pick: () => Promise.reject({ code: 2002, message: '需要打开密码' }),
    });
    await expect(session.openDocument()).rejects.toEqual({ code: 2002, message: '需要打开密码' });
  });

  it('标签页关掉之后让引擎关闭文档，并释放它的瓦片来源', async () => {
    const { store, session, transport } = setup({
      open: [docInfo(1, '提单.pdf'), docInfo(2, '发票.pdf')],
    });
    await session.attach();
    const listeners = transport.listenerCount;

    store.getState().closeDocument('engine-1');
    expect(transport.sent(METHOD.DOC_CLOSE)).toEqual([{ docId: 1, discard: true }]);
    expect(session.tileProvider('engine-1')).toBeUndefined();
    expect(session.tileProvider('engine-2')).toBeDefined();
    expect(transport.listenerCount).toBe(listeners - 1);

    // 模拟文档的开关和引擎无关
    store
      .getState()
      .openDocument({ id: 'mock-1', name: '模拟', dirty: false, version: 0, pages: [] });
    store.getState().closeDocument('mock-1');
    expect(transport.sent(METHOD.DOC_CLOSE)).toHaveLength(1);
  });

  it('doc.changed：记下新版本号，只有变了的页换成新对象', async () => {
    const { store, session, transport } = setup({ open: [docInfo(1, '提单.pdf')] });
    await session.attach();
    const before = store.getState().documents['engine-1']!.snapshot;

    transport.emit(EVENT.DOC_CHANGED, {
      docId: 1,
      version: 4,
      changes: [{ pageId: 21, added: [], removed: [9], modified: [] }],
      pagesChanged: false,
    });
    const after = store.getState().documents['engine-1']!.snapshot;
    expect(after).toMatchObject({ version: 4, dirty: true });
    expect(after.pages[0]).toBe(before.pages[0]);
    expect(after.pages[1]).not.toBe(before.pages[1]);
    expect(after.pages[1]).toEqual(before.pages[1]);

    // 别的文档的事件不相干
    transport.emit(EVENT.DOC_CHANGED, { docId: 9, version: 7, changes: [], pagesChanged: false });
    expect(store.getState().documents['engine-1']!.snapshot).toBe(after);
  });

  it('引擎重启：按路径对上新的 docId，重新拉快照，视图留着；对不上的关掉并告诉用户', async () => {
    const { store, session, transport, snapshots, notices } = setup({
      open: [docInfo(1, '提单.pdf'), docInfo(2, '发票.pdf')],
    });
    await session.attach();
    store.getState().activateDocument('engine-1');
    store.getState().setView('engine-1', { zoom: 2, x: 10, y: 20 });
    const before = store.getState().documents['engine-1']!.snapshot;
    transport.emit(EVENT.DOC_CHANGED, { docId: 1, version: 3, changes: [], pagesChanged: false });

    transport.emit(EVENT.ENGINE_RESTARTING, {});
    // 重启期间文档不可用：要瓦片会被当作取消，不发请求
    const requests = transport.requests.length;
    await expect(
      session.tileProvider('engine-1')!.renderTile({
        page: 0,
        level: 6,
        pxPerPt: 2,
        tx: 0,
        ty: 0,
        tileSize: 256,
        pixelRect: { x: 0, y: 0, width: 256, height: 256 },
        pageRect: { x: 0, y: 0, width: 128, height: 128 },
        documentVersion: 3,
        signal: new AbortController().signal,
      }),
    ).rejects.toMatchObject({ name: 'AbortError' });
    expect(transport.requests).toHaveLength(requests);

    // 新引擎只恢复了提单，docId 换成了 11；路径的大小写和斜杠不一样也要认得
    snapshots.set(11, snapshot(0));
    transport.emit(EVENT.ENGINE_RESTORED, {
      docIds: [11],
      docs: [{ ...docInfo(11, '提单.pdf'), path: 'c:/DOCS/提单.pdf' }],
    });
    await settle();

    const state = store.getState();
    expect(state.documentOrder).toEqual(['engine-1']);
    expect(state.activeDocumentId).toBe('engine-1');
    const restored = state.documents['engine-1']!;
    expect(restored.view).toEqual({ zoom: 2, x: 10, y: 20 });
    expect(restored.snapshot).toMatchObject({ version: 0, dirty: false, name: '提单.pdf' });
    // 每一页都是新对象：画布据此丢掉旧瓦片
    expect(restored.snapshot.pages[0]).not.toBe(before.pages[0]);
    expect(transport.sent(METHOD.DOC_SNAPSHOT).at(-1)).toEqual({ docId: 11 });
    // 没恢复的那份已经不在引擎里了，不用再发 doc.close
    expect(transport.sent(METHOD.DOC_CLOSE)).toEqual([]);
    expect(notices.map((n) => n.kind)).toEqual(['info', 'error']);
    expect(notices[1]!.message).toContain('发票.pdf');

    // 之后关掉它，用的是新的 docId
    store.getState().closeDocument('engine-1');
    expect(transport.sent(METHOD.DOC_CLOSE)).toEqual([{ docId: 11, discard: true }]);
  });

  it('引擎起不来时告诉用户', async () => {
    const { transport, notices } = setup();
    transport.emit(EVENT.ENGINE_FAILED, { message: '启动引擎进程失败' });
    expect(notices).toHaveLength(1);
    expect(notices[0]).toMatchObject({ kind: 'error' });
    expect(notices[0]!.message).toContain('启动引擎进程失败');
  });

  it('释放后不再理会事件和 store', async () => {
    const { store, session, transport } = setup({ open: [docInfo(1, '提单.pdf')] });
    await session.attach();
    session.dispose();
    expect(transport.listenerCount).toBe(0);
    store.getState().closeDocument('engine-1');
    expect(transport.sent(METHOD.DOC_CLOSE)).toEqual([]);
  });
});
