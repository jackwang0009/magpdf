import { describe, expect, it } from 'vitest';
import { createMockDocument } from '../document/mockDocument';
import { createEditorStore } from './editorStore';

function setup() {
  const store = createEditorStore();
  const doc = createMockDocument({ id: 'doc', name: 'test.pdf', pageCount: 3, template: 'report' });
  store.getState().openDocument(doc);
  return { store, doc };
}

describe('编辑器状态', () => {
  it('打开文档后成为活动文档，重复打开不重复加入', () => {
    const { store, doc } = setup();
    expect(store.getState().documentOrder).toEqual(['doc']);
    expect(store.getState().activeDocumentId).toBe('doc');
    store.getState().openDocument(doc);
    expect(store.getState().documentOrder).toEqual(['doc']);
  });

  it('关闭活动文档后切到相邻文档', () => {
    const { store } = setup();
    store
      .getState()
      .openDocument(
        createMockDocument({ id: 'b', name: 'b.pdf', pageCount: 1, template: 'report' }),
      );
    store.getState().closeDocument('b');
    expect(store.getState().activeDocumentId).toBe('doc');
    store.getState().closeDocument('doc');
    expect(store.getState().activeDocumentId).toBeNull();
    expect(store.getState().documentOrder).toEqual([]);
  });

  it('移动对象：包围盒平移、版本号加一、标记未保存、记一条历史', () => {
    const { store } = setup();
    const object = store.getState().documents.doc!.snapshot.pages[0]!.objects[0]!;
    store.getState().setSelection('doc', [object.id]);
    store.getState().moveObjects('doc', [object.id], 10, 5);
    const next = store.getState().documents.doc!;
    expect(next.snapshot.pages[0]!.objects[0]!.bbox.x).toBeCloseTo(object.bbox.x + 10, 6);
    expect(next.snapshot.version).toBe(1);
    expect(next.snapshot.dirty).toBe(true);
    expect(next.history.length).toBeGreaterThan(1);
  });

  it('删除对象后从选区和页面里移除', () => {
    const { store } = setup();
    const object = store.getState().documents.doc!.snapshot.pages[0]!.objects[0]!;
    store.getState().setSelection('doc', [object.id, 'not-exist']);
    store.getState().deleteObjects('doc', [object.id]);
    const next = store.getState().documents.doc!;
    expect(next.snapshot.pages[0]!.objects.some((o) => o.id === object.id)).toBe(false);
    expect(next.selection).toEqual([]);
  });

  it('换快照：视图和历史不动，选区里已经不存在的对象去掉', () => {
    const { store, doc } = setup();
    const [first, second] = doc.pages[0]!.objects;
    store.getState().setView('doc', { zoom: 2, x: 5, y: 6 });
    store.getState().setSelection('doc', [first!.id, second!.id]);
    const before = store.getState().documents.doc!;

    // 第 1 页少了一个对象（比如引擎那边删掉了）
    const pages = doc.pages.map((page, i) =>
      i === 0 ? { ...page, objects: page.objects.filter((o) => o.id !== first!.id) } : page,
    );
    store.getState().updateSnapshot('doc', { ...doc, version: 7, pages });
    const next = store.getState().documents.doc!;
    expect(next.snapshot.version).toBe(7);
    expect(next.selection).toEqual([second!.id]);
    expect(next.view).toEqual({ zoom: 2, x: 5, y: 6 });
    expect(next.history).toBe(before.history);
    expect(store.getState().activeDocumentId).toBe('doc');

    // 选区没受影响时，选区数组还是原来那个（订阅它的地方不用白忙）
    const kept = next.selection;
    store.getState().updateSnapshot('doc', { ...next.snapshot, version: 8 });
    expect(store.getState().documents.doc!.selection).toBe(kept);

    // 没打开的文档：什么也不做
    store.getState().updateSnapshot('nope', { ...doc, id: 'nope' });
    expect(store.getState().documentOrder).toEqual(['doc']);
  });

  it('指针与帧统计只在变化时更新', () => {
    const { store } = setup();
    store.getState().setPointer({ page: 0, x: 1, y: 2 });
    expect(store.getState().pointer).toEqual({ page: 0, x: 1, y: 2 });
    store.getState().setPointer(null);
    expect(store.getState().pointer).toBeNull();
    store.getState().setFrameStats({
      samples: 1,
      intervalP50: 16,
      intervalP95: 17,
      intervalMax: 18,
      workP50: 1,
      workP95: 2,
      workMax: 3,
      longFrames: 1,
    });
    expect(store.getState().frameStats?.intervalP95).toBe(17);
  });
});
