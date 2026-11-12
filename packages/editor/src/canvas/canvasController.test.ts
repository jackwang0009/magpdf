// @vitest-environment jsdom
// 控制器要往容器里放 canvas，需要 DOM；编辑器包的其他测试都是纯逻辑，跑在 node 环境里。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DocumentEditor, PageTransform } from '../document/documentEditor';
import { createMockDocument } from '../document/mockDocument';
import { transformRect } from '../document/pageSpace';
import type { Rect } from '../geometry';
import { t } from '../i18n';
import { createEditorStore } from '../store/editorStore';
import { NUDGE_STEP_PT } from '../tools/selectTool';
import { cssPxPerPt } from '../view/viewState';
import { CanvasController, repaintedArea } from './canvasController';
import { GlTileRenderer } from './glRenderer';
import { AbortError } from './tileProvider';
import type { RawPixels, TileProvider, TileRequest, TileResult } from './tileProvider';

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

describe('页面的画面变了哪里', () => {
  const page = { index: 0, width: 200, height: 100, objects: [] };
  const rect: Rect = { x: 10, y: 20, width: 30, height: 40 };

  it('修订号没变就没变；变了而且说了变的是哪一块，只重画那一块', () => {
    expect(repaintedArea({ ...page, revision: 1 }, { ...page, revision: 1 })).toBeNull();
    expect(
      repaintedArea({ ...page, revision: 1 }, { ...page, revision: 2, damage: { since: 1, rect } }),
    ).toEqual(rect);
    expect(repaintedArea({ ...page, revision: 1 }, { ...page, revision: 2 })).toBe('page');
  });

  it('中间漏看了一次修订号：说的那一块不全，整页重画', () => {
    expect(
      repaintedArea({ ...page, revision: 1 }, { ...page, revision: 3, damage: { since: 2, rect } }),
    ).toBe('page');
  });

  it('模拟文档没有修订号：页面对象换了就整页重画', () => {
    expect(repaintedArea(page, { ...page, objects: [] })).toBe('page');
    expect(repaintedArea(page, page)).toBeNull();
  });
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

describe('拖动预览（方案 3.2）', () => {
  /** 瓦片请求先攒着，测试决定什么时候给结果。给出来的“像素”上带着是哪一层、哪一块。 */
  class ManualProvider implements TileProvider {
    readonly concurrency = 64;
    readonly requests: {
      readonly request: TileRequest;
      done: boolean;
      readonly resolve: () => void;
      readonly fail: () => void;
    }[] = [];

    constructor(readonly layers: boolean) {}

    renderTile(request: TileRequest): Promise<TileResult> {
      return new Promise((resolve, reject) => {
        const { width, height } = request.pixelRect;
        const pixels = {
          data: new Uint8Array(4),
          width,
          height,
          layer: request.layer?.mode ?? 'page',
          tile: `${request.tx}/${request.ty}`,
        } as RawPixels;
        const entry = {
          request,
          done: false,
          resolve: () => {
            entry.done = true;
            resolve({ width, height, pixels });
          },
          fail: () => {
            entry.done = true;
            reject(new Error('渲染失败'));
          },
        };
        request.signal.addEventListener('abort', () => {
          entry.done = true;
          reject(new AbortError());
        });
        this.requests.push(entry);
      });
    }

    /** 还没有结果的请求里，属于某一层的（`page` 是平常的整页瓦片） */
    pending(layer: 'page' | 'without' | 'only') {
      return this.requests.filter(
        (entry) => !entry.done && (entry.request.layer?.mode ?? 'page') === layer,
      );
    }
  }

  interface Drawn {
    readonly rect: Rect;
    readonly texture: { readonly layer: string; readonly tile: string } | null;
  }

  /** 等瓦片结果的回调跑完。 */
  const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

  /**
   * 一页 600×800 pt 的文档，上面两个对象，像引擎打开的文档那样带修订号，修改交给 `editor`。
   * 修改入口照 `EngineSession` 的做法先把包围盒挪过去；`accept` 为 false 时什么也不做（修改被拒绝）。
   */
  function mount(options: { readonly layers?: boolean; readonly accept?: boolean } = {}) {
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
      frames.push(callback);
      return frames.length;
    });
    vi.stubGlobal('cancelAnimationFrame', () => undefined);
    // jsdom 没有指针捕获
    Object.assign(HTMLElement.prototype, {
      setPointerCapture: () => undefined,
      releasePointerCapture: () => undefined,
      hasPointerCapture: () => false,
    });

    const draws: Drawn[] = [];
    const deleted: unknown[] = [];
    vi.spyOn(GlTileRenderer, 'create').mockReturnValue({
      isLost: false,
      frameStats: { draws: 0, uploads: 0 },
      resize: () => undefined,
      beginFrame: () => {
        draws.length = 0;
      },
      drawQuad: (rect: Rect, texture: Drawn['texture']) => {
        draws.push({ rect, texture });
      },
      endFrame: () => undefined,
      // “纹理”就是像素对象本身，上面带着层和位置的标记
      uploadTexture: (pixels: unknown) => pixels,
      deleteTexture: (texture: unknown) => {
        deleted.push(texture);
      },
      onContextLost: () => () => undefined,
      dispose: () => undefined,
    } as unknown as GlTileRenderer);

    const store = createEditorStore();
    store.getState().openDocument({
      id: 'doc',
      name: '测试.pdf',
      dirty: false,
      version: 0,
      pages: [
        {
          index: 0,
          width: 600,
          height: 800,
          revision: 1,
          objects: [
            {
              id: 'p0-1',
              type: 'path',
              name: 'a',
              bbox: { x: 100, y: 100, width: 80, height: 60 },
            },
            {
              id: 'p0-2',
              type: 'path',
              name: 'b',
              bbox: { x: 300, y: 400, width: 50, height: 50 },
            },
          ],
        },
      ],
    });
    const snapshot = () => store.getState().documents.doc!.snapshot;

    const edits: PageTransform[][] = [];
    const editor: DocumentEditor = {
      transformObjects: (changes) => {
        edits.push([...changes]);
        if (options.accept === false) return;
        const current = snapshot();
        store.getState().updateSnapshot('doc', {
          ...current,
          pages: current.pages.map((page) => {
            const change = changes.find((c) => c.page === page.index);
            if (!change) return page;
            return {
              ...page,
              objects: page.objects.map((object) =>
                change.ids.includes(object.id)
                  ? { ...object, bbox: transformRect(change.matrix, object.bbox) }
                  : object,
              ),
            };
          }),
        });
      },
      deleteObjects: () => undefined,
      undo: () => undefined,
      redo: () => undefined,
    };

    const container = document.createElement('div');
    container.getBoundingClientRect = () => new DOMRect(0, 0, 800, 600);
    document.body.append(container);
    const provider = new ManualProvider(options.layers ?? true);
    const controller = new CanvasController({
      store,
      documentId: 'doc',
      container,
      provider,
      editor,
    });
    const overlay = container.querySelector('.mg-canvas-overlay')!;

    /** 跑完等着的帧。`at` 是传给帧回调的时间，默认现在。 */
    const frame = (at = performance.now()) => {
      for (const callback of frames.splice(0)) callback(at);
    };
    /** 页面坐标里的一点在窗口里的位置（标尺占了左、上各一条） */
    const client = (x: number, y: number) => {
      const view = controller.view;
      const pageRect = controller.pageLayout.pageRect(0);
      const scale = cssPxPerPt(view.zoom);
      const ruler = 20;
      return {
        x: (pageRect.x + x - view.x) * scale + ruler,
        y: (pageRect.y + y - view.y) * scale + ruler,
      };
    };
    const pointer = (type: string, at: { x: number; y: number }) => {
      overlay.dispatchEvent(
        new MouseEvent(type, { clientX: at.x, clientY: at.y, button: 0, bubbles: true }),
      );
    };
    /** 页面的修订号加一，像引擎推送 doc.changed 之后那样 */
    const repaint = () => {
      const current = snapshot();
      store.getState().updateSnapshot('doc', {
        ...current,
        pages: current.pages.map((page) => ({ ...page, revision: (page.revision ?? 0) + 1 })),
      });
    };
    /** 给某一层等着的请求结果，等回调跑完 */
    const deliver = async (layer: 'page' | 'without' | 'only') => {
      for (const entry of provider.pending(layer)) entry.resolve();
      await flush();
    };
    const drawn = (layer: string) => draws.filter((draw) => draw.texture?.layer === layer);

    return {
      controller,
      provider,
      edits,
      deleted,
      draws,
      drawn,
      frame,
      client,
      pointer,
      repaint,
      deliver,
      snapshot,
    };
  }

  /** 页面先画出来，然后按住第一个对象拖出去 (40, 30) 个像素。 */
  async function startDrag(context: ReturnType<typeof mount>) {
    const { frame, deliver, client, pointer } = context;
    frame();
    await deliver('page');
    frame();
    const from = client(140, 130);
    pointer('pointerdown', from);
    pointer('pointermove', { x: from.x + 40, y: from.y + 30 });
    frame();
    return from;
  }

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('拖动一开始要两层瓦片；到齐之前照旧，到齐之后底图盖在上面、对象层跟着鼠标走', async () => {
    const context = mount();
    const { controller, provider, drawn, frame, deliver, pointer } = context;
    try {
      const from = await startDrag(context);
      // 底图：这一页在视口里的每一块；对象层：盖住对象的那一块
      const level = provider.pending('without')[0]!.request.level;
      const visible = provider.requests.filter(
        (entry) => !entry.request.layer && entry.request.level === level,
      );
      expect(provider.pending('without')).toHaveLength(visible.length);
      expect(provider.pending('only').map((entry) => entry.request.tx)).toEqual([0]);
      for (const entry of [...provider.pending('without'), ...provider.pending('only')]) {
        expect(entry.request.layer?.objects).toEqual(['p0-1']);
      }
      expect(controller.dragPreview).toMatchObject({ ready: false, held: false });
      expect(drawn('without')).toEqual([]);

      // 只到了一层：还不能用
      await deliver('without');
      frame();
      expect(controller.dragPreview?.ready).toBe(false);
      expect(drawn('without')).toEqual([]);

      await deliver('only');
      frame();
      expect(controller.dragPreview).toMatchObject({
        ready: true,
        baseTiles: visible.length,
        objectTiles: 1,
      });
      expect(controller.dragPreview?.readyMs).toBeGreaterThanOrEqual(0);
      // 画的顺序：平常的瓦片、底图、对象层
      const order = context.draws.map((draw) => draw.texture?.layer ?? 'paper');
      expect(order.lastIndexOf('page')).toBeLessThan(order.indexOf('without'));
      expect(order.lastIndexOf('without')).toBeLessThan(order.indexOf('only'));
      expect(drawn('without')).toHaveLength(visible.length);
      // 对象层那一块比它原来的位置（底图里同一块瓦片）挪了正好 (40, 30) 个像素
      const home = drawn('without').find((draw) => draw.texture?.tile === '0/0')!.rect;
      const moved = drawn('only')[0]!.rect;
      expect([moved.x - home.x, moved.y - home.y]).toEqual([40, 30]);
      expect([moved.width, moved.height]).toEqual([home.width, home.height]);

      // 接着拖：只是换个位置画，不再发任何请求
      const sent = provider.requests.length;
      pointer('pointermove', { x: from.x + 75, y: from.y - 12 });
      frame();
      const later = drawn('only')[0]!.rect;
      expect([later.x - home.x, later.y - home.y]).toEqual([75, -12]);
      expect(provider.requests).toHaveLength(sent);
    } finally {
      controller.dispose();
    }
  });

  it('拖控制点缩放：对象层按“原来的外框 → 现在的外框”拉伸着画，松手后同样留着等新瓦片', async () => {
    const context = mount();
    const { controller, edits, drawn, frame, deliver, client, pointer } = context;
    try {
      frame();
      await deliver('page');
      frame();
      // 先点选第一个对象（页面坐标 100,100，80×60），再按住它右下角的控制点往右下拖 (40, 30) 个像素
      const center = client(140, 130);
      pointer('pointerdown', center);
      pointer('pointerup', center);
      const corner = client(180, 160);
      pointer('pointerdown', corner);
      pointer('pointermove', { x: corner.x + 40, y: corner.y + 30 });
      frame();
      await deliver('without');
      await deliver('only');
      frame();
      expect(controller.dragPreview).toMatchObject({ ready: true, objectTiles: 1 });

      // 外框的左上角不动，宽高各放大这么多倍
      const scale = cssPxPerPt(controller.view.zoom);
      const [kx, ky] = [(80 * scale + 40) / (80 * scale), (60 * scale + 30) / (60 * scale)];
      const home = drawn('without').find((draw) => draw.texture?.tile === '0/0')!.rect;
      const stretched = drawn('only')[0]!.rect;
      expect(stretched.width / home.width).toBeCloseTo(kx, 6);
      expect(stretched.height / home.height).toBeCloseTo(ky, 6);
      // 对象左上角在屏幕上的位置（去掉标尺占的 20 个像素）不变，瓦片上别的点离它的距离按比例放大。
      // 底图的瓦片对到了整数像素上，所以允许差半个像素再乘上放大倍数
      const corner0 = client(100, 100);
      const [objectX, objectY] = [corner0.x - 20, corner0.y - 20];
      expect(Math.abs(stretched.x - (objectX + (home.x - objectX) * kx))).toBeLessThan(1);
      expect(Math.abs(stretched.y - (objectY + (home.y - objectY) * ky))).toBeLessThan(1);

      pointer('pointerup', { x: corner.x + 40, y: corner.y + 30 });
      expect(edits).toHaveLength(1);
      expect(controller.dragPreview?.held).toBe(true);
      frame();
      const kept = drawn('only')[0]!.rect;
      expect(kept.width / home.width).toBeCloseTo(kx, 6);
    } finally {
      controller.dispose();
    }
  });

  it('松手之后预览留着，等这一页重画好的瓦片到齐才撤', async () => {
    const context = mount();
    const { controller, provider, edits, deleted, drawn, frame, deliver, pointer, repaint } =
      context;
    try {
      const from = await startDrag(context);
      await deliver('without');
      await deliver('only');
      frame();
      const home = drawn('without').find((draw) => draw.texture?.tile === '0/0')!.rect;

      pointer('pointerup', { x: from.x + 40, y: from.y + 30 });
      // 修改交给了修改入口，对象的包围盒已经先挪过去了
      expect(edits).toHaveLength(1);
      expect(context.snapshot().pages[0]!.objects[0]!.bbox.x).toBeGreaterThan(100);
      expect(controller.dragPreview).toMatchObject({ ready: true, held: true });

      // 引擎还没重画：预览照旧画着，对象在松手的位置上
      frame();
      const shown = drawn('only')[0]!.rect;
      expect([shown.x - home.x, shown.y - home.y]).toEqual([40, 30]);
      expect(controller.upToDate).toBe(false);

      // 引擎改完了，这一页要重画；新瓦片还在路上，预览继续盖着
      repaint();
      frame();
      expect(provider.pending('page').length).toBeGreaterThan(0);
      expect(drawn('only')).toHaveLength(1);
      expect(controller.dragPreview?.held).toBe(true);

      // 新瓦片到齐：预览撤掉，两层的纹理交还
      await deliver('page');
      frame();
      expect(controller.dragPreview).toBeNull();
      expect(controller.lastDragPreview).toMatchObject({ ready: true, objectTiles: 1 });
      expect(
        deleted.filter((texture) => (texture as { layer: string }).layer !== 'page'),
      ).toHaveLength(controller.lastDragPreview!.baseTiles + 1);
      frame();
      expect(drawn('without')).toEqual([]);
      expect(drawn('only')).toEqual([]);
      expect(controller.upToDate).toBe(true);
    } finally {
      controller.dispose();
    }
  });

  it('修改没有生效（被拒绝）：对象不在松手的位置上，预览马上撤', async () => {
    const context = mount({ accept: false });
    const { controller, edits, drawn, frame, deliver, pointer } = context;
    try {
      const from = await startDrag(context);
      await deliver('without');
      await deliver('only');
      frame();
      pointer('pointerup', { x: from.x + 40, y: from.y + 30 });
      expect(edits).toHaveLength(1);
      frame();
      expect(controller.dragPreview).toBeNull();
      frame();
      expect(drawn('only')).toEqual([]);
    } finally {
      controller.dispose();
    }
  });

  it('重画好的瓦片一直不来：过了时限也撤，不会一直盖着', async () => {
    const context = mount();
    const { controller, frame, deliver, pointer } = context;
    try {
      const from = await startDrag(context);
      await deliver('without');
      await deliver('only');
      frame();
      pointer('pointerup', { x: from.x + 40, y: from.y + 30 });
      frame();
      expect(controller.dragPreview?.held).toBe(true);
      frame(performance.now() + 60_000);
      expect(controller.dragPreview).toBeNull();
    } finally {
      controller.dispose();
    }
  });

  it('两层还没到齐就松手了：不留预览', async () => {
    const context = mount();
    const { controller, provider, edits, frame, deliver, pointer } = context;
    try {
      const from = await startDrag(context);
      await deliver('without');
      const waiting = provider.pending('only');
      pointer('pointerup', { x: from.x + 40, y: from.y + 30 });
      expect(edits).toHaveLength(1);
      expect(controller.dragPreview).toBeNull();
      // 在途的请求取消了
      expect(waiting.every((entry) => entry.request.signal.aborted)).toBe(true);
      frame();
      expect(controller.lastDragPreview).toMatchObject({ ready: false });
    } finally {
      controller.dispose();
    }
  });

  it('拖到一半取消：预览撤掉，在途的请求取消', async () => {
    const context = mount();
    const { controller, provider, edits, frame, pointer } = context;
    try {
      const from = await startDrag(context);
      const waiting = [...provider.pending('without'), ...provider.pending('only')];
      expect(waiting.length).toBeGreaterThan(1);
      pointer('pointercancel', { x: from.x + 40, y: from.y + 30 });
      frame();
      expect(controller.dragPreview).toBeNull();
      expect(waiting.every((entry) => entry.request.signal.aborted)).toBe(true);
      expect(edits).toEqual([]);
    } finally {
      controller.dispose();
    }
  });

  it('有瓦片渲染不出来：这一次拖动退回虚线框，不反复重试', async () => {
    const context = mount();
    const { controller, provider, drawn, frame, deliver, pointer } = context;
    vi.spyOn(console, 'error').mockImplementation(() => undefined);
    try {
      const from = await startDrag(context);
      await deliver('without');
      provider.pending('only')[0]!.fail();
      await flush();
      frame();
      expect(controller.dragPreview).toBeNull();
      const sent = provider.requests.length;
      pointer('pointermove', { x: from.x + 60, y: from.y + 30 });
      frame();
      pointer('pointermove', { x: from.x + 80, y: from.y + 30 });
      frame();
      expect(provider.requests).toHaveLength(sent);
      expect(drawn('only')).toEqual([]);
    } finally {
      controller.dispose();
    }
  });

  it('瓦片来源不会按层渲染：不要两层瓦片，拖动时只有虚线框', async () => {
    const context = mount({ layers: false });
    const { controller, provider, edits, pointer } = context;
    try {
      const from = await startDrag(context);
      expect(provider.requests.some((entry) => entry.request.layer)).toBe(false);
      expect(controller.dragPreview).toBeNull();
      pointer('pointerup', { x: from.x + 40, y: from.y + 30 });
      expect(edits).toHaveLength(1);
    } finally {
      controller.dispose();
    }
  });
});
