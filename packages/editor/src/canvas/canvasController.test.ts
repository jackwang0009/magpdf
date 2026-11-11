// @vitest-environment jsdom
// 控制器要往容器里放 canvas，需要 DOM；编辑器包的其他测试都是纯逻辑，跑在 node 环境里。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DocumentEditor, PageTransform } from '../document/documentEditor';
import { createMockDocument } from '../document/mockDocument';
import { t } from '../i18n';
import { createEditorStore } from '../store/editorStore';
import { NUDGE_STEP_PT } from '../tools/selectTool';
import { CanvasController } from './canvasController';
import { GlTileRenderer } from './glRenderer';

/** 控制器用到的那部分合成器接口；测试里不真的画。 */
function stubRenderer(): GlTileRenderer {
  return {
    isLost: false,
    frameStats: { draws: 0, uploads: 0 },
    resize: () => undefined,
    beginFrame: () => undefined,
    drawQuad: () => undefined,
    endFrame: () => undefined,
    uploadTexture: () => null,
    deleteTexture: () => undefined,
    onContextLost: () => () => undefined,
    dispose: () => undefined,
  } as unknown as GlTileRenderer;
}

function mount(options: { readonly pages?: number; readonly editor?: DocumentEditor } = {}) {
  const store = createEditorStore();
  store.getState().openDocument(
    createMockDocument({
      id: 'doc',
      name: '测试.pdf',
      pageCount: options.pages ?? 1,
      template: 'report',
    }),
  );
  const container = document.createElement('div');
  document.body.append(container);
  const controller = new CanvasController({
    store,
    documentId: 'doc',
    container,
    ...(options.editor ? { editor: options.editor } : {}),
  });
  return { store, container, controller };
}

beforeEach(() => {
  // jsdom 没有画布上下文；叠加层拿不到 2D 上下文时控制器本来就会跳过绘制
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
});

afterEach(() => {
  vi.restoreAllMocks();
  document.body.replaceChildren();
});

describe('画布上的“WebGL2 不可用”提示层', () => {
  it('WebGL2 可用时没有这一层，最上面是接收鼠标事件的叠加层', () => {
    vi.spyOn(GlTileRenderer, 'create').mockReturnValue(stubRenderer());
    const { container, controller } = mount();
    try {
      // 这一层盖住整个画布。哪怕只是标成 hidden 留在那里，样式表里的 display 也会让它照常显示，
      // 鼠标事件就到不了叠加层：滚轮、拖动、选择全部失灵。
      expect(container.querySelector('.mg-canvas-message')).toBeNull();
      expect(container.lastElementChild?.className).toBe('mg-canvas-overlay');
      expect(controller.hasWebGL).toBe(true);
    } finally {
      controller.dispose();
    }
    expect(container.childElementCount).toBe(0);
  });

  it('WebGL2 不可用时显示提示，释放时一并移除', () => {
    vi.spyOn(GlTileRenderer, 'create').mockReturnValue(null);
    const { container, controller } = mount();
    try {
      const message = container.querySelector('.mg-canvas-message');
      expect(message?.textContent).toBe(t.canvas.webglUnavailable);
      expect(controller.hasWebGL).toBe(false);
    } finally {
      controller.dispose();
    }
    expect(container.childElementCount).toBe(0);
  });
});

describe('工具产生的修改交给谁', () => {
  function recordingEditor() {
    const calls: unknown[][] = [];
    const editor: DocumentEditor = {
      transformObjects: (changes) => calls.push(['transform', changes]),
      deleteObjects: (ids) => calls.push(['delete', ids]),
      undo: () => calls.push(['undo']),
      redo: () => calls.push(['redo']),
    };
    return { editor, calls };
  }

  it('文档有修改入口（引擎打开的）：微移、删除、撤销、重做都交给它，store 里的快照不动', () => {
    vi.spyOn(GlTileRenderer, 'create').mockReturnValue(stubRenderer());
    const { editor, calls } = recordingEditor();
    const { store, controller } = mount({ pages: 2, editor });
    try {
      const snapshot = store.getState().documents.doc!.snapshot;
      const onFirst = snapshot.pages[0]!.objects[0]!.id;
      const onSecond = snapshot.pages[1]!.objects[0]!.id;
      store.getState().setSelection('doc', [onFirst, onSecond]);

      expect(controller.runCommand('edit.nudgeRight')).toBe(true);
      // 两页上各有一个选中的对象：按页分开，平移在哪一页上都是同一个矩阵
      expect(calls).toHaveLength(1);
      const changes = calls[0]![1] as PageTransform[];
      expect(changes.map((change) => [change.page, change.ids])).toEqual([
        [0, [onFirst]],
        [1, [onSecond]],
      ]);
      for (const change of changes) {
        const expected = [1, 0, 0, 1, NUDGE_STEP_PT, 0];
        change.matrix.forEach((value, i) => expect(value).toBeCloseTo(expected[i]!, 9));
      }

      expect(controller.runCommand('edit.delete')).toBe(true);
      expect(controller.runCommand('edit.undo')).toBe(true);
      expect(controller.runCommand('edit.redo')).toBe(true);
      expect(calls.slice(1)).toEqual([['delete', [onFirst, onSecond]], ['undo'], ['redo']]);
      expect(store.getState().documents.doc!.snapshot).toBe(snapshot);
    } finally {
      controller.dispose();
    }
  });

  it('只能叠加内容的页上的对象：微移、删除都不交给修改入口，别的页照常', () => {
    vi.spyOn(GlTileRenderer, 'create').mockReturnValue(stubRenderer());
    const { editor, calls } = recordingEditor();
    const { store, controller } = mount({ pages: 2, editor });
    try {
      const snapshot = store.getState().documents.doc!.snapshot;
      const [first, second] = snapshot.pages;
      // 第 1 页没有通过保真校验
      store.getState().updateSnapshot('doc', {
        ...snapshot,
        pages: [{ ...first!, overlayOnly: { reason: '测试' } }, second!],
      });
      const onFirst = first!.objects[0]!.id;
      const onSecond = second!.objects[0]!.id;

      store.getState().setSelection('doc', [onFirst]);
      expect(controller.runCommand('edit.nudgeRight')).toBe(true);
      expect(controller.runCommand('edit.delete')).toBe(true);
      // 选区里混着能改的对象也一样：整个选区都不动
      store.getState().setSelection('doc', [onSecond, onFirst]);
      controller.runCommand('edit.nudgeRight');
      controller.runCommand('edit.delete');
      expect(calls).toEqual([]);

      store.getState().setSelection('doc', [onSecond]);
      controller.runCommand('edit.delete');
      expect(calls).toEqual([['delete', [onSecond]]]);
    } finally {
      controller.dispose();
    }
  });

  it('文档没有修改入口（模拟文档）：照旧直接改 store 里的快照；撤销没有可做的', () => {
    vi.spyOn(GlTileRenderer, 'create').mockReturnValue(stubRenderer());
    const { store, controller } = mount();
    try {
      const before = store.getState().documents.doc!.snapshot;
      const object = before.pages[0]!.objects[0]!;
      store.getState().setSelection('doc', [object.id]);

      controller.runCommand('edit.nudgeRight');
      const moved = store.getState().documents.doc!.snapshot;
      expect(moved.pages[0]!.objects[0]!.bbox.x).toBeCloseTo(object.bbox.x + NUDGE_STEP_PT, 9);
      expect(moved.version).toBe(before.version + 1);

      // 按键算处理过了（不落到浏览器的默认行为上），但文档不变
      expect(controller.runCommand('edit.undo')).toBe(true);
      expect(store.getState().documents.doc!.snapshot).toBe(moved);

      controller.runCommand('edit.delete');
      expect(store.getState().documents.doc!.snapshot.pages[0]!.objects).not.toContainEqual(
        expect.objectContaining({ id: object.id }),
      );
    } finally {
      controller.dispose();
    }
  });
});
