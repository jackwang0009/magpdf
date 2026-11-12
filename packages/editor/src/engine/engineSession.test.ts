import { EVENT, METHOD, OBJ_LIST_MAX } from '@magpdf/protocol';
import type { DocInfo, EditTransformParams } from '@magpdf/protocol';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { rectToRectMatrix, translationMatrix } from '../document/pageSpace';
import { t } from '../i18n';
import type { HostNotice } from '../shell/editorHost';
import { createEditorStore } from '../store/editorStore';
import { EngineSession } from './engineSession';
import { FAKE_OVERLAY_REASON, FakeEngine } from './fakeEngine';
import type { FakeObject, FakePage } from './fakeEngine';

/** 第 1 页：300×440，不旋转。整页白底（点不中）、一行文字、一个方框。 */
function frontPage(): FakePage {
  return {
    pageId: 20,
    width: 300,
    height: 440,
    rotate: 0,
    cropBox: [0, 0, 300, 440],
    objects: [
      { id: 1, kind: 'path', bbox: [0, 0, 300, 440], pickable: false },
      { id: 2, kind: 'text', bbox: [20, 400, 120, 420] },
      { id: 3, kind: 'path', bbox: [50, 100, 250, 300] },
    ],
  };
}

/** 第 2 页：MediaBox 300×200，旋转 90°，显示出来是 200×300。 */
function rotatedPage(): FakePage {
  return {
    pageId: 21,
    width: 200,
    height: 300,
    rotate: 90,
    cropBox: [0, 0, 300, 200],
    objects: [{ id: 4, kind: 'image', bbox: [130, 120, 170, 190] }],
  };
}

function plainPage(pageId: number, objects: FakeObject[] = []): FakePage {
  return { pageId, width: 300, height: 440, rotate: 0, cropBox: [0, 0, 300, 440], objects };
}

function fourPages(): FakePage[] {
  return [
    frontPage(),
    rotatedPage(),
    plainPage(22, [{ id: 5, kind: 'text', bbox: [10, 10, 60, 30] }]),
    plainPage(23, [{ id: 6, kind: 'shading', bbox: [0, 0, 10, 10] }]),
  ];
}

function setup() {
  const engine = new FakeEngine();
  const store = createEditorStore();
  /** 主进程知道的、已经开着的文档 */
  const open: DocInfo[] = [];
  /** 下一次“打开”对话框的结果 */
  const dialog: { result: DocInfo | null | Error | object } = { result: null };
  const session = new EngineSession({
    transport: engine.transport,
    store,
    pickDocument: () =>
      dialog.result !== null && !('docId' in dialog.result)
        ? Promise.reject(dialog.result)
        : Promise.resolve(dialog.result as DocInfo | null),
    listDocuments: () => Promise.resolve(open),
  });
  const notices: HostNotice[] = [];
  session.onNotice((notice) => notices.push(notice));
  const snapshot = (id = 'engine-1') => store.getState().documents[id]!.snapshot;
  const objects = (page: number, id = 'engine-1') => snapshot(id).pages[page]!.objects;
  const object = (objectId: string, id = 'engine-1') => {
    const found = snapshot(id)
      .pages.flatMap((p) => p.objects)
      .find((o) => o.id === objectId);
    if (!found) throw new Error(`没有对象 ${objectId}`);
    return found;
  };
  return { engine, store, session, open, dialog, notices, snapshot, objects, object };
}

/** 等在途的请求、应答和事件都走完。 */
async function flush(): Promise<void> {
  for (let i = 0; i < 6; i++) await new Promise((resolve) => setTimeout(resolve, 0));
}

/** 接进一份四页的文档，等前两页的对象到齐。 */
async function attached() {
  const context = setup();
  context.open.push(context.engine.open('提单.pdf', fourPages()));
  await context.session.attach();
  await flush();
  const editor = context.session.documentEditor('engine-1')!;
  return { ...context, editor };
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe('EngineSession：打开、翻页、关闭', () => {
  it('页面列表先到；看得到的页（和下一页）才取对象，包围盒换算成页面坐标', async () => {
    const { engine, store, session, open, snapshot, objects } = setup();
    open.push(engine.open('提单.pdf', fourPages()), engine.open('发票.pdf', [plainPage(30)]));
    await session.attach();

    expect(store.getState().documentOrder).toEqual(['engine-1', 'engine-2']);
    expect(store.getState().activeDocumentId).toBe('engine-2');
    expect(snapshot()).toMatchObject({ name: '提单.pdf', dirty: false, version: 0 });
    expect(snapshot().pages.map((p) => [p.index, p.width, p.height])).toEqual([
      [0, 300, 440],
      [1, 200, 300],
      [2, 300, 440],
      [3, 300, 440],
    ]);
    const revisions = snapshot().pages.map((p) => p.revision);
    expect(new Set(revisions).size).toBe(4);

    await flush();
    // 不旋转的页：y 轴翻过来。整页白底点不中，但仍然列出来。
    expect(objects(0)).toEqual([
      {
        id: 'p0-1',
        type: 'path',
        name: `${t.objectTypes.path} 1`,
        bbox: { x: 0, y: 0, width: 300, height: 440 },
        pickable: false,
      },
      {
        id: 'p0-2',
        type: 'text',
        name: `${t.objectTypes.text} 2`,
        bbox: { x: 20, y: 20, width: 100, height: 20 },
      },
      {
        id: 'p0-3',
        type: 'path',
        name: `${t.objectTypes.path} 3`,
        bbox: { x: 50, y: 140, width: 200, height: 200 },
      },
    ]);
    // 旋转 90° 的页：位置和宽高都按显示方向换算（对照数据见 pageSpace.test.ts）
    expect(objects(1)).toEqual([
      {
        id: 'p1-4',
        type: 'image',
        name: `${t.objectTypes.image} 4`,
        bbox: { x: 120, y: 130, width: 70, height: 40 },
      },
    ]);
    // 后面的页还没有看到，不取
    expect(objects(2)).toEqual([]);
    expect(objects(3)).toEqual([]);
    expect(engine.transport.sent(METHOD.OBJ_LIST)).toEqual([
      { docId: 1, pageId: 20, offset: 0, limit: OBJ_LIST_MAX },
      { docId: 1, pageId: 21, offset: 0, limit: OBJ_LIST_MAX },
      { docId: 2, pageId: 30, offset: 0, limit: OBJ_LIST_MAX },
    ]);
    // 对象列表到了，画面并没有变：修订号不动，瓦片不用重画
    expect(snapshot().pages.map((p) => p.revision)).toEqual(revisions);

    expect(session.tileProvider('engine-1')).toBeDefined();
    expect(session.tileProvider('engine-2')).not.toBe(session.tileProvider('engine-1'));
    expect(session.documentEditor('engine-1')).toBeDefined();
    expect(session.tileProvider('mock-1')).toBeUndefined();
    expect(session.documentEditor('mock-1')).toBeUndefined();
  });

  it('翻到后面的页时取那一页和前后各一页，取过的不再取', async () => {
    const { engine, store, objects } = await attached();
    store.getState().setCurrentPage('engine-1', 3);
    await flush();
    expect(objects(3).map((o) => [o.id, o.type])).toEqual([['p3-6', 'shading']]);
    expect(objects(2).map((o) => o.id)).toEqual(['p2-5']);
    store.getState().setCurrentPage('engine-1', 1);
    await flush();
    const listed = engine.transport.sent<{ pageId: number }>(METHOD.OBJ_LIST).map((p) => p.pageId);
    expect(listed.sort()).toEqual([20, 21, 22, 23]);
  });

  it('对象多的页分几次取完', async () => {
    const { engine, session, open, objects } = setup();
    const many: FakeObject[] = Array.from({ length: OBJ_LIST_MAX + 3 }, (_, i) => ({
      id: i + 1,
      kind: 'path',
      bbox: [i, 0, i + 1, 1],
    }));
    open.push(engine.open('图纸.pdf', [plainPage(40, many)]));
    await session.attach();
    await flush();
    expect(objects(0)).toHaveLength(OBJ_LIST_MAX + 3);
    expect(objects(0).at(-1)!.id).toBe(`p0-${OBJ_LIST_MAX + 3}`);
    expect(engine.transport.sent<{ offset: number }>(METHOD.OBJ_LIST).map((p) => p.offset)).toEqual(
      [0, OBJ_LIST_MAX],
    );
  });

  it('瓦片请求带的是这份文档的 docId 和这一页的 pageId', async () => {
    const { engine, session } = await attached();
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
    ).rejects.toThrow('假引擎没有实现 render.tiles');
    expect(engine.transport.sent(METHOD.RENDER_TILES)).toEqual([
      { docId: 1, pageId: 21, scale: 2, tiles: [{ x: 0, y: 0 }], priority: 'visible' },
    ]);
  });

  it('「打开」：主进程选好文件交回来就接进来；取消什么也不做；同一份文档不接两次', async () => {
    const { engine, store, session, dialog } = setup();

    await session.openDocument();
    expect(store.getState().documentOrder).toEqual([]);

    dialog.result = engine.open('提单.pdf', [plainPage(20)]);
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
    const { session, dialog } = setup();
    dialog.result = { code: 2002, message: '需要打开密码' };
    await expect(session.openDocument()).rejects.toEqual({ code: 2002, message: '需要打开密码' });
  });

  it('标签页关掉之后让引擎关闭文档，并释放它的瓦片来源', async () => {
    const { engine, store, session, open } = setup();
    open.push(engine.open('提单.pdf', [plainPage(20)]), engine.open('发票.pdf', [plainPage(30)]));
    await session.attach();
    await flush();
    const listeners = engine.transport.listenerCount;

    store.getState().closeDocument('engine-1');
    expect(engine.transport.sent(METHOD.DOC_CLOSE)).toEqual([{ docId: 1, discard: true }]);
    expect(session.tileProvider('engine-1')).toBeUndefined();
    expect(session.documentEditor('engine-1')).toBeUndefined();
    expect(session.tileProvider('engine-2')).toBeDefined();
    expect(engine.transport.listenerCount).toBe(listeners - 1);

    // 模拟文档的开关和引擎无关
    store
      .getState()
      .openDocument({ id: 'mock-1', name: '模拟', dirty: false, version: 0, pages: [] });
    store.getState().closeDocument('mock-1');
    expect(engine.transport.sent(METHOD.DOC_CLOSE)).toHaveLength(1);
  });

  it('释放后不再理会事件和 store', async () => {
    const { engine, store, session } = await attached();
    session.dispose();
    expect(engine.transport.listenerCount).toBe(0);
    store.getState().closeDocument('engine-1');
    expect(engine.transport.sent(METHOD.DOC_CLOSE)).toEqual([]);
  });
});

describe('EngineSession：修改', () => {
  // 修改命令的应答和 doc.changed 走两条路，谁先到没有保证
  it.each([
    ['doc.changed 先到', true],
    ['应答先到', false],
  ])('移动：界面先挪过去，命令发给引擎，最后以引擎的对象为准（%s）', async (_name, eventFirst) => {
    const { engine, store, editor, snapshot, object } = await attached();
    engine.eventFirst = eventFirst;
    store.getState().setSelection('engine-1', ['p0-2']);
    const before = snapshot().pages.map((p) => p.revision);

    editor.transformObjects([{ page: 0, ids: ['p0-2'], matrix: translationMatrix(10, 4) }]);
    // 不等引擎：包围盒已经挪过去了，但画面还没变（修订号、版本号都不动）
    expect(object('p0-2').bbox).toEqual({ x: 30, y: 24, width: 100, height: 20 });
    expect(snapshot().pages.map((p) => p.revision)).toEqual(before);
    expect(snapshot()).toMatchObject({ version: 0, dirty: false });

    await flush();
    // 页面坐标里向右下挪 (10, 4)，在用户空间里是 x 加 10、y 减 4；剪切路径跟着走
    expect(engine.transport.sent<EditTransformParams>(METHOD.EDIT_TRANSFORM)).toEqual([
      { docId: 1, baseVersion: 0, ids: [2], matrix: [1, 0, 0, 1, 10, -4], clipFollows: true },
    ]);
    expect(engine.page(1, 20).objects[1]!.bbox).toEqual([30, 396, 130, 416]);
    expect(snapshot()).toMatchObject({ version: 1, dirty: true });
    expect(object('p0-2').bbox).toEqual({ x: 30, y: 24, width: 100, height: 20 });
    // 只有改了的那一页要重画
    const after = snapshot().pages.map((p) => p.revision);
    expect(after[0]).not.toBe(before[0]);
    expect(after.slice(1)).toEqual(before.slice(1));
    // 对象编号没变，选区还在
    expect(store.getState().documents['engine-1']!.selection).toEqual(['p0-2']);
  });

  it('旋转过的页上，移动的方向换到用户空间里跟着转', async () => {
    const { engine, editor, object } = await attached();
    editor.transformObjects([{ page: 1, ids: ['p1-4'], matrix: translationMatrix(10, 4) }]);
    await flush();
    expect(engine.transport.sent<EditTransformParams>(METHOD.EDIT_TRANSFORM)[0]!.matrix).toEqual([
      1, 0, 0, 1, 4, 10,
    ]);
    // 引擎那边按用户空间挪了，换算回来正好是页面上挪了 (10, 4)
    expect(engine.page(1, 21).objects[0]!.bbox).toEqual([134, 130, 174, 200]);
    expect(object('p1-4').bbox).toEqual({ x: 130, y: 134, width: 70, height: 40 });
  });

  it('缩放：把选区的包围盒映到新的包围盒，是一个带缩放的矩阵', async () => {
    const { engine, editor, object } = await attached();
    // 方框在页面上是 (50, 140) 200×200，拖右下角缩成 100×50，左上角不动
    const matrix = rectToRectMatrix(
      { x: 50, y: 140, width: 200, height: 200 },
      { x: 50, y: 140, width: 100, height: 50 },
    );
    editor.transformObjects([{ page: 0, ids: ['p0-3'], matrix }]);
    expect(object('p0-3').bbox).toEqual({ x: 50, y: 140, width: 100, height: 50 });
    await flush();
    const sent = engine.transport.sent<EditTransformParams>(METHOD.EDIT_TRANSFORM)[0]!;
    expect(sent.matrix.map((v) => Math.round(v * 1000) / 1000)).toEqual([0.5, 0, 0, 0.25, 25, 225]);
    // 用户空间里左上角 (50, 300) 不动，宽 100、高 50
    expect(engine.page(1, 20).objects[2]!.bbox).toEqual([50, 250, 150, 300]);
    expect(object('p0-3').bbox).toEqual({ x: 50, y: 140, width: 100, height: 50 });
  });

  it('几页上的对象一起变换：每页一条命令，各用各的矩阵，一条接一条', async () => {
    const { engine, editor } = await attached();
    editor.transformObjects([
      { page: 0, ids: ['p0-2', 'p0-3'], matrix: translationMatrix(5, 0) },
      { page: 1, ids: ['p1-4'], matrix: translationMatrix(5, 0) },
    ]);
    await flush();
    expect(engine.transport.sent<EditTransformParams>(METHOD.EDIT_TRANSFORM)).toEqual([
      { docId: 1, baseVersion: 0, ids: [2, 3], matrix: [1, 0, 0, 1, 5, 0], clipFollows: true },
      { docId: 1, baseVersion: 1, ids: [4], matrix: [1, 0, 0, 1, 0, 5], clipFollows: true },
    ]);
  });

  it('连着两次修改：第二条命令带的是第一条之后的版本号', async () => {
    const { engine, editor, object, snapshot } = await attached();
    editor.transformObjects([{ page: 0, ids: ['p0-2'], matrix: translationMatrix(10, 0) }]);
    editor.transformObjects([{ page: 0, ids: ['p0-2'], matrix: translationMatrix(10, 0) }]);
    expect(object('p0-2').bbox.x).toBe(40);
    await flush();
    expect(
      engine.transport.sent<EditTransformParams>(METHOD.EDIT_TRANSFORM).map((p) => p.baseVersion),
    ).toEqual([0, 1]);
    expect(object('p0-2').bbox.x).toBe(40);
    expect(snapshot().version).toBe(2);
  });

  it('前一步的对象列表晚到，不会把界面上后一步先改的结果冲掉', async () => {
    const { engine, store, editor, object } = await attached();
    const seen: number[] = [];
    store.subscribe(() => {
      const x = object('p0-2').bbox.x;
      if (seen.at(-1) !== x) seen.push(x);
    });
    // 第一步：引擎已经做了，doc.changed 也到了，但它的应答和随后取的对象列表都还在路上
    const releaseCommand = engine.transport.hold(METHOD.EDIT_TRANSFORM);
    const releaseList = engine.transport.hold(METHOD.OBJ_LIST);
    editor.transformObjects([{ page: 0, ids: ['p0-2'], matrix: translationMatrix(10, 0) }]);
    await flush();
    // 第二步：界面上先挪过去了，命令还排在第一步后面
    editor.transformObjects([{ page: 0, ids: ['p0-2'], matrix: translationMatrix(10, 0) }]);
    expect(object('p0-2').bbox.x).toBe(40);
    // 这时第一步之后的对象列表到了：它只反映第一步，不能拿来用
    releaseList();
    await flush();
    expect(object('p0-2').bbox.x).toBe(40);
    releaseCommand();
    await flush();
    // 一路只往前走：20 → 30 → 40，中间没有跳回 30
    expect(seen).toEqual([30, 40]);
    expect(object('p0-2').bbox.x).toBe(40);
    expect(engine.page(1, 20).objects[1]!.bbox![0]).toBe(40);
    // 最后界面上的对象是引擎给的那一份，不是界面自己算的
    expect(engine.transport.sent(METHOD.OBJ_LIST).length).toBeGreaterThanOrEqual(4);
  });

  it('删除、撤销、重做', async () => {
    const { engine, store, editor, objects, snapshot } = await attached();
    store.getState().setSelection('engine-1', ['p0-2', 'p0-3']);
    const selection = () => store.getState().documents['engine-1']!.selection;

    editor.deleteObjects(['p0-2']);
    // 界面上马上就没了，选区里也去掉
    expect(objects(0).map((o) => o.id)).toEqual(['p0-1', 'p0-3']);
    expect(selection()).toEqual(['p0-3']);
    await flush();
    expect(engine.transport.sent(METHOD.EDIT_DELETE)).toEqual([
      { docId: 1, baseVersion: 0, ids: [2] },
    ]);
    expect(objects(0).map((o) => o.id)).toEqual(['p0-1', 'p0-3']);
    expect(snapshot().version).toBe(1);

    editor.undo();
    await flush();
    // 对象回来了，编号和层级都是原来的
    expect(engine.transport.sent(METHOD.HISTORY_UNDO)).toEqual([{ docId: 1 }]);
    expect(objects(0).map((o) => o.id)).toEqual(['p0-1', 'p0-2', 'p0-3']);
    expect(snapshot().version).toBe(2);

    editor.redo();
    await flush();
    expect(objects(0).map((o) => o.id)).toEqual(['p0-1', 'p0-3']);
    expect(snapshot().version).toBe(3);
  });

  it('没有可撤销的操作时按撤销：什么也不发生，也不报错', async () => {
    const { editor, notices, snapshot } = await attached();
    const logged = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    editor.undo();
    editor.redo();
    await flush();
    expect(notices).toEqual([]);
    expect(logged).not.toHaveBeenCalled();
    expect(snapshot().version).toBe(0);
  });

  it('引擎报别的错：界面恢复原状，把原因告诉用户', async () => {
    const { engine, editor, object, notices } = await attached();
    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    // 对象在引擎那边已经不在了
    engine.page(1, 20).objects = engine.page(1, 20).objects.filter((o) => o.id !== 2);
    editor.transformObjects([{ page: 0, ids: ['p0-2'], matrix: translationMatrix(10, 4) }]);
    await flush();
    expect(notices).toEqual([
      { kind: 'error', message: t.host.editFailed.replace('{reason}', '对象不存在') },
    ]);
    // 以引擎的对象列表为准：这个对象没有了
    expect(() => object('p0-2')).toThrow();
  });

  it('界面记的版本号落后了：问一下现在的版本，再发一次', async () => {
    const { engine, editor, object, notices, snapshot } = await attached();
    // 引擎那边的版本号往前走了一步，界面不知道
    engine.advanceVersionSilently(1);

    editor.transformObjects([{ page: 0, ids: ['p0-2'], matrix: translationMatrix(10, 0) }]);
    await flush();
    // 第一次被拒绝（版本冲突），问到现在是第 1 版之后再发，这次成了
    expect(
      engine.transport.sent<EditTransformParams>(METHOD.EDIT_TRANSFORM).map((p) => p.baseVersion),
    ).toEqual([0, 1]);
    expect(notices).toEqual([]);
    expect(object('p0-2').bbox.x).toBe(30);
    expect(engine.version(1)).toBe(2);
    expect(snapshot().version).toBe(2);
  });

  it('文档关掉之后，修改入口不再发命令', async () => {
    const { engine, store, editor } = await attached();
    store.getState().closeDocument('engine-1');
    editor.transformObjects([{ page: 0, ids: ['p0-2'], matrix: translationMatrix(10, 0) }]);
    editor.deleteObjects(['p0-2']);
    editor.undo();
    await flush();
    expect(engine.transport.sent(METHOD.EDIT_TRANSFORM)).toEqual([]);
    expect(engine.transport.sent(METHOD.EDIT_DELETE)).toEqual([]);
    expect(engine.transport.sent(METHOD.HISTORY_UNDO)).toEqual([]);
  });
});

describe('EngineSession：只能叠加内容的页（方案 2.3）', () => {
  const overlayOnly = { reason: FAKE_OVERLAY_REASON };

  it('保真校验的结论到了：通不过的页记下来，通过的页不受影响', async () => {
    const { engine, snapshot } = await attached();
    engine.page(1, 20).overlayOnly = true;
    const before = snapshot();
    expect(before.pages.map((p) => p.overlayOnly)).toEqual([
      undefined,
      undefined,
      undefined,
      undefined,
    ]);

    // 引擎空闲下来，把显示着的两页校验了
    engine.finishBackgroundCheck(1, 20);
    engine.finishBackgroundCheck(1, 21);
    expect(snapshot().pages.map((p) => p.overlayOnly)).toEqual([
      overlayOnly,
      undefined,
      undefined,
      undefined,
    ]);
    // 只是多了一个标记：对象还在，画面不用重画，文档也没有变
    expect(snapshot().pages[0]!.objects).toBe(before.pages[0]!.objects);
    expect(snapshot().pages[0]!.revision).toBe(before.pages[0]!.revision);
    expect(snapshot().pages[1]).toBe(before.pages[1]);
    expect(snapshot()).toMatchObject({ version: 0, dirty: false });
  });

  it('知道了这一页只能叠加内容之后：修改不发给引擎，界面上也不先动', async () => {
    const { engine, editor, object, objects, notices } = await attached();
    engine.page(1, 20).overlayOnly = true;
    engine.finishBackgroundCheck(1, 20);

    editor.transformObjects([{ page: 0, ids: ['p0-2'], matrix: translationMatrix(10, 4) }]);
    editor.deleteObjects(['p0-3']);
    expect(object('p0-2').bbox).toEqual({ x: 20, y: 20, width: 100, height: 20 });
    expect(objects(0).map((o) => o.id)).toEqual(['p0-1', 'p0-2', 'p0-3']);
    await flush();
    expect(engine.transport.sent(METHOD.EDIT_TRANSFORM)).toEqual([]);
    expect(engine.transport.sent(METHOD.EDIT_DELETE)).toEqual([]);
    expect(notices).toEqual([]);

    // 一次动两页上的对象，其中一页不能改：整个不做，另一页的对象也不动
    editor.transformObjects([
      { page: 0, ids: ['p0-2'], matrix: translationMatrix(10, 0) },
      { page: 1, ids: ['p1-4'], matrix: translationMatrix(10, 0) },
    ]);
    editor.deleteObjects(['p1-4', 'p0-2']);
    await flush();
    expect(object('p1-4').bbox).toEqual({ x: 120, y: 130, width: 70, height: 40 });
    expect(engine.transport.sent(METHOD.EDIT_TRANSFORM)).toEqual([]);
    expect(engine.transport.sent(METHOD.EDIT_DELETE)).toEqual([]);

    // 别的页照常能改
    editor.transformObjects([{ page: 1, ids: ['p1-4'], matrix: translationMatrix(10, 0) }]);
    await flush();
    expect(object('p1-4').bbox.x).toBe(130);
  });

  it.each([
    ['结论先到', true],
    ['应答先到', false],
  ])(
    '用户赶在校验做完之前动了手（%s）：引擎拒绝，界面恢复原状，不弹框',
    async (_name, eventFirst) => {
      const { engine, editor, object, objects, notices, snapshot } = await attached();
      engine.eventFirst = eventFirst;
      engine.page(1, 20).overlayOnly = true;
      const revision = snapshot().pages[0]!.revision;

      editor.transformObjects([{ page: 0, ids: ['p0-2'], matrix: translationMatrix(10, 4) }]);
      // 界面还不知道这一页不能改，先按预期挪过去
      expect(object('p0-2').bbox.x).toBe(30);
      await flush();
      expect(object('p0-2').bbox).toEqual({ x: 20, y: 20, width: 100, height: 20 });
      // 引擎执行命令之前自己做了校验，结论随后到了
      expect(snapshot().pages[0]!.overlayOnly).toEqual(overlayOnly);
      expect(notices).toEqual([]);
      // 文档没有变：版本号、修订号都不动
      expect(snapshot()).toMatchObject({ version: 0, dirty: false });
      expect(snapshot().pages[0]!.revision).toBe(revision);
      // 结论先到的话界面不用再问；拒绝的应答先到，界面那一刻还不知道原因，就问一次
      expect(engine.transport.sent(METHOD.FIDELITY_CHECK)).toEqual(
        eventFirst ? [] : [{ docId: 1, pageIds: [20] }],
      );

      // 之后的修改不再发给引擎
      editor.deleteObjects(['p0-3']);
      await flush();
      expect(objects(0).map((o) => o.id)).toEqual(['p0-1', 'p0-2', 'p0-3']);
      expect(engine.transport.sent(METHOD.EDIT_DELETE)).toEqual([]);
    },
  );

  it('引擎拒绝了，结论却一直没到：点名问一次', async () => {
    const { engine, editor, object, notices, snapshot } = await attached();
    // 引擎早就校验过这一页，结论不会再推送，而界面不知道
    Object.assign(engine.page(1, 20), { overlayOnly: true, checked: true });

    editor.deleteObjects(['p0-2']);
    await flush();
    expect(engine.transport.sent(METHOD.FIDELITY_CHECK)).toEqual([{ docId: 1, pageIds: [20] }]);
    expect(snapshot().pages[0]!.overlayOnly).toEqual(overlayOnly);
    expect(object('p0-2').bbox).toEqual({ x: 20, y: 20, width: 100, height: 20 });
    expect(notices).toEqual([]);
  });

  it('界面重新加载后接回来的文档：引擎早先的结论跟着页面列表一起到', async () => {
    const { engine, session, open, snapshot } = setup();
    open.push(engine.open('提单.pdf', fourPages()));
    engine.page(1, 22).overlayOnly = true;
    engine.finishBackgroundCheck(1, 22);
    engine.finishBackgroundCheck(1, 23);

    await session.attach();
    // 不用等这一页的对象：第 3 页还没有显示过，状态已经有了
    expect(snapshot().pages.map((p) => p.overlayOnly)).toEqual([
      undefined,
      undefined,
      overlayOnly,
      undefined,
    ]);
  });

  it('引擎重启之后重新校验：旧的结论不留着', async () => {
    const { engine, snapshot } = await attached();
    engine.page(1, 20).overlayOnly = true;
    engine.finishBackgroundCheck(1, 20);
    expect(snapshot().pages[0]!.overlayOnly).toEqual(overlayOnly);

    engine.transport.emit(EVENT.ENGINE_RESTARTING, {});
    const frozen = fourPages();
    frozen[0]!.overlayOnly = true;
    const restored = engine.restart([{ name: '提单.pdf', pages: frozen }]);
    engine.transport.emit(EVENT.ENGINE_RESTORED, {
      docIds: restored.map((d) => d.docId),
      docs: restored,
    });
    await flush();
    // 新引擎还没有校验
    expect(snapshot().pages[0]!.overlayOnly).toBeUndefined();
    // 旧引擎的结论晚到了：docId 对不上，不理
    engine.transport.emit(EVENT.FIDELITY_RESULT, {
      docId: 1,
      pageId: 20,
      status: 'overlayOnly',
      reason: FAKE_OVERLAY_REASON,
    });
    expect(snapshot().pages[0]!.overlayOnly).toBeUndefined();
    engine.finishBackgroundCheck(restored[0]!.docId, 20);
    expect(snapshot().pages[0]!.overlayOnly).toEqual(overlayOnly);
  });
});

describe('EngineSession：引擎推送的变化', () => {
  it('没看过的页变了：只换修订号，不去取它的对象', async () => {
    const { engine, snapshot } = await attached();
    const before = snapshot().pages.map((p) => p.revision);
    const listed = engine.transport.sent(METHOD.OBJ_LIST).length;

    engine.transport.emit(EVENT.DOC_CHANGED, {
      docId: 1,
      version: 4,
      changes: [{ pageId: 23, added: [], removed: [9], modified: [] }],
      pagesChanged: false,
    });
    await flush();
    const after = snapshot().pages.map((p) => p.revision);
    expect(after[3]).not.toBe(before[3]);
    expect(after.slice(0, 3)).toEqual(before.slice(0, 3));
    expect(snapshot()).toMatchObject({ version: 4, dirty: true });
    expect(engine.transport.sent(METHOD.OBJ_LIST)).toHaveLength(listed);

    // 别的文档的事件不相干
    const settled = snapshot();
    engine.transport.emit(EVENT.DOC_CHANGED, {
      docId: 9,
      version: 7,
      changes: [],
      pagesChanged: false,
    });
    expect(snapshot()).toBe(settled);
  });

  it('引擎说了画面变在哪里：记成这一页修订号的变化范围（页面坐标）；下次没说就整页重画', async () => {
    const { engine, snapshot } = await attached();
    const before = snapshot().pages[0]!;
    engine.transport.emit(EVENT.DOC_CHANGED, {
      docId: 1,
      version: 1,
      changes: [{ pageId: 20, added: [], removed: [], modified: [2], area: [30, 396, 130, 416] }],
      pagesChanged: false,
    });
    await flush();
    const after = snapshot().pages[0]!;
    expect(after.revision).not.toBe(before.revision);
    expect(after.damage).toEqual({
      since: before.revision,
      rect: { x: 30, y: 24, width: 100, height: 20 },
    });

    engine.transport.emit(EVENT.DOC_CHANGED, {
      docId: 1,
      version: 2,
      changes: [{ pageId: 20, added: [], removed: [], modified: [2] }],
      pagesChanged: false,
    });
    await flush();
    expect(snapshot().pages[0]!.revision).not.toBe(after.revision);
    expect(snapshot().pages[0]!.damage).toBeUndefined();
  });

  it('引擎重启：按路径对上新的 docId，页面和对象重新取，视图留着；对不上的关掉并告诉用户', async () => {
    const { engine, store, session, open, notices, snapshot, objects } = setup();
    open.push(engine.open('提单.pdf', fourPages()), engine.open('发票.pdf', [plainPage(30)]));
    await session.attach();
    await flush();
    store.getState().activateDocument('engine-1');
    store.getState().setView('engine-1', { zoom: 2, x: 10, y: 20 });
    store.getState().setSelection('engine-1', ['p0-2']);
    const editor = session.documentEditor('engine-1')!;
    editor.deleteObjects(['p0-3']);
    await flush();
    expect(snapshot()).toMatchObject({ version: 1, dirty: true });
    const before = snapshot().pages.map((p) => p.revision);

    engine.transport.emit(EVENT.ENGINE_RESTARTING, {});
    // 重启期间文档不可用：要瓦片会被当作取消，修改命令也发不出去
    const requests = engine.transport.requests.length;
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
        documentVersion: 1,
        signal: new AbortController().signal,
      }),
    ).rejects.toMatchObject({ name: 'AbortError' });
    editor.transformObjects([{ page: 0, ids: ['p0-2'], matrix: translationMatrix(10, 0) }]);
    await flush();
    expect(engine.transport.requests).toHaveLength(requests);
    expect(notices).toEqual([]);

    // 新引擎只恢复了提单，docId 换了；没保存的修改没有了；路径的大小写和斜杠不一样也要认得
    const [restored] = engine.restart([{ name: '提单.pdf', pages: fourPages() }]);
    engine.transport.emit(EVENT.ENGINE_RESTORED, {
      docIds: [restored!.docId],
      docs: [{ ...restored!, path: 'c:/DOCS/提单.pdf' }],
    });
    await flush();

    const state = store.getState();
    expect(state.documentOrder).toEqual(['engine-1']);
    expect(state.activeDocumentId).toBe('engine-1');
    expect(state.documents['engine-1']!.view).toEqual({ zoom: 2, x: 10, y: 20 });
    expect(snapshot()).toMatchObject({ version: 0, dirty: false, name: '提单.pdf' });
    // 每一页都要重画；对象是从新引擎重新取的（删掉的那个回来了，重启期间先挪的那一下也不算数）
    const after = snapshot().pages.map((p) => p.revision);
    after.forEach((revision, i) => expect(revision).not.toBe(before[i]));
    expect(objects(0).map((o) => [o.id, o.bbox.x])).toEqual([
      ['p0-1', 0],
      ['p0-2', 20],
      ['p0-3', 50],
    ]);
    // 对象编号是新引擎重新分配的，旧的选区不能留
    expect(state.documents['engine-1']!.selection).toEqual([]);
    expect(notices.map((n) => n.kind)).toEqual(['info', 'error']);
    expect(notices[1]!.message).toContain('发票.pdf');

    // 之后的修改和关闭用的都是新的 docId
    editor.deleteObjects(['p0-2']);
    await flush();
    expect(engine.transport.sent(METHOD.EDIT_DELETE).at(-1)).toEqual({
      docId: restored!.docId,
      baseVersion: 0,
      ids: [2],
    });
    store.getState().closeDocument('engine-1');
    expect(engine.transport.sent(METHOD.DOC_CLOSE)).toEqual([
      { docId: restored!.docId, discard: true },
    ]);
  });

  it('引擎起不来时告诉用户', () => {
    const { engine, notices } = setup();
    engine.transport.emit(EVENT.ENGINE_FAILED, { message: '启动引擎进程失败' });
    expect(notices).toHaveLength(1);
    expect(notices[0]).toMatchObject({ kind: 'error' });
    expect(notices[0]!.message).toContain('启动引擎进程失败');
  });
});
