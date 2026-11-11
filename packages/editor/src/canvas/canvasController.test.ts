// @vitest-environment jsdom
// 控制器要往容器里放 canvas，需要 DOM；编辑器包的其他测试都是纯逻辑，跑在 node 环境里。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createMockDocument } from '../document/mockDocument';
import { t } from '../i18n';
import { createEditorStore } from '../store/editorStore';
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

function mount() {
  const store = createEditorStore();
  store
    .getState()
    .openDocument(
      createMockDocument({ id: 'doc', name: '测试.pdf', pageCount: 1, template: 'report' }),
    );
  const container = document.createElement('div');
  document.body.append(container);
  const controller = new CanvasController({ store, documentId: 'doc', container });
  return { container, controller };
}

describe('画布上的“WebGL2 不可用”提示层', () => {
  beforeEach(() => {
    // jsdom 没有画布上下文；叠加层拿不到 2D 上下文时控制器本来就会跳过绘制
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
  });

  afterEach(() => {
    vi.restoreAllMocks();
    document.body.replaceChildren();
  });

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
