import { afterEach, describe, expect, it, vi } from 'vitest';
import { DragLayers, layerTileSpecs } from './dragLayers';
import { AbortError } from './tileProvider';
import type { TileProvider, TileRequest, TileResult } from './tileProvider';

/** 600×800 pt 的一页。第 6 档是 100% 缩放：每 pt 4/3 个像素，页面 800×1067 像素，4 列 5 行瓦片。 */
const PAGE = { width: 600, height: 800 };
const LEVEL = 6;

/** 请求先攒着，测试决定什么时候给结果。 */
class ManualProvider implements TileProvider {
  readonly layers = true;
  readonly requests: {
    readonly request: TileRequest;
    readonly resolve: (result: TileResult) => void;
    readonly reject: (error: unknown) => void;
  }[] = [];

  renderTile(request: TileRequest): Promise<TileResult> {
    return new Promise((resolve, reject) => this.requests.push({ request, resolve, reject }));
  }
}

function tile(): TileResult {
  return { width: 256, height: 256, pixels: { data: new Uint8Array(4), width: 256, height: 256 } };
}

/** 等结果的回调跑完。 */
function flush(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function setup() {
  const provider = new ManualProvider();
  const uploaded: { readonly n: number }[] = [];
  const released: unknown[] = [];
  let clock = 1000;
  let changes = 0;
  const layers = new DragLayers({
    provider,
    page: 3,
    level: LEVEL,
    objects: ['p3-7', 'p3-9'],
    documentVersion: 5,
    upload: () => {
      const texture = { n: uploaded.length };
      uploaded.push(texture);
      return texture;
    },
    release: (texture) => released.push(texture),
    onChange: () => {
      changes++;
    },
    now: () => clock,
  });
  /** 页面左上角的两块瓦片，和它们下面的一块 */
  const topLeft = layerTileSpecs(PAGE, 3, LEVEL, { x: 150, y: 10, width: 100, height: 50 });
  const below = layerTileSpecs(PAGE, 3, LEVEL, { x: 10, y: 250, width: 50, height: 50 });
  return {
    provider,
    layers,
    uploaded,
    released,
    topLeft,
    below,
    tick: (ms: number) => {
      clock += ms;
    },
    changes: () => changes,
  };
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe('拖动预览要哪些瓦片', () => {
  const ids = (rect: { x: number; y: number; width: number; height: number }) =>
    layerTileSpecs(PAGE, 3, LEVEL, rect).map((spec) => spec.id);

  it('和矩形相交的瓦片，按行列出', () => {
    expect(ids({ x: 10, y: 10, width: 50, height: 50 })).toEqual(['3/6/0/0']);
    // 横跨第 0、1 列，竖跨第 1、2 行
    expect(ids({ x: 150, y: 350, width: 100, height: 60 })).toEqual([
      '3/6/0/1',
      '3/6/1/1',
      '3/6/0/2',
      '3/6/1/2',
    ]);
  });

  it('每块瓦片带着请求渲染要用的几何量', () => {
    const spec = layerTileSpecs(PAGE, 3, LEVEL, { x: 200, y: 200, width: 10, height: 10 })[0]!;
    expect(spec).toMatchObject({
      id: '3/6/1/1',
      key: { page: 3, level: LEVEL, tx: 1, ty: 1 },
      pixelRect: { x: 256, y: 256, width: 256, height: 256 },
    });
    expect(spec.pxPerPt).toBeCloseTo(4 / 3, 9);
    // 256 个像素是 192 pt
    expect(spec.pageRect.x).toBeCloseTo(192, 6);
    expect(spec.pageRect.width).toBeCloseTo(192, 6);
  });

  it('矩形伸到页面外面的部分不算；完全在外面、或者没有面积时一块也没有', () => {
    // 右上角：只有页面里的那一截（最右边两列的第一行）
    expect(ids({ x: 560, y: -20, width: 100, height: 60 })).toEqual(['3/6/2/0', '3/6/3/0']);
    expect(ids({ x: 700, y: 0, width: 50, height: 50 })).toEqual([]);
    expect(ids({ x: 0, y: -100, width: 50, height: 50 })).toEqual([]);
    expect(ids({ x: 100, y: 100, width: 0, height: 50 })).toEqual([]);
  });
});

describe('DragLayers', () => {
  it('两层各发一批请求，带上是哪一层、哪些对象', () => {
    const { provider, layers, topLeft, below } = setup();
    layers.requestBase([...topLeft, ...below]);
    layers.requestObjects(below);

    expect(
      provider.requests.map(({ request }) => [request.layer?.mode, request.tx, request.ty]),
    ).toEqual([
      ['without', 0, 0],
      ['without', 1, 0],
      ['without', 0, 1],
      ['only', 0, 1],
    ]);
    for (const { request } of provider.requests) {
      expect(request.layer?.objects).toEqual(['p3-7', 'p3-9']);
      expect(request).toMatchObject({ page: 3, level: LEVEL, tileSize: 256, documentVersion: 5 });
      expect(request.signal.aborted).toBe(false);
    }
    expect(provider.requests[3]!.request.pixelRect).toEqual({
      x: 0,
      y: 256,
      width: 256,
      height: 256,
    });
    expect(layers.counts).toEqual({ base: 3, objects: 1 });
  });

  it('两层的瓦片都到了才算就绪，并记下用了多久', async () => {
    const { provider, layers, topLeft, below, uploaded, tick, changes } = setup();
    layers.requestBase(topLeft);
    layers.requestObjects(below);
    expect(layers.ready).toBe(false);
    expect(layers.readyMs).toBeNull();

    // 底图到齐了，对象层还没有
    provider.requests[0]!.resolve(tile());
    provider.requests[1]!.resolve(tile());
    await flush();
    expect(layers.ready).toBe(false);
    expect(layers.base().map((t) => t.id)).toEqual(['3/6/0/0', '3/6/1/0']);
    expect(layers.objectLayer()).toEqual([]);

    tick(42);
    provider.requests[2]!.resolve(tile());
    await flush();
    expect(layers.ready).toBe(true);
    expect(layers.readyMs).toBe(42);
    expect(layers.objectLayer().map((t) => t.id)).toEqual(['3/6/0/1']);
    // 每块瓦片的像素都交给 upload 变成了纹理；每到一块请画布重画一次
    expect(uploaded).toHaveLength(3);
    expect(layers.objectLayer()[0]!.texture).toBe(uploaded[2]);
    expect(changes()).toBe(3);
  });

  it('要过的瓦片不重复要；就绪之后补要的瓦片到一块用一块，不影响就绪', async () => {
    const { provider, layers, topLeft, below, tick } = setup();
    layers.requestBase(topLeft);
    layers.requestObjects(below);
    for (const pending of provider.requests) pending.resolve(tile());
    await flush();
    expect(layers.ready).toBe(true);
    const readyMs = layers.readyMs;

    // 画布每一帧都会把视口里的瓦片再报一遍
    layers.requestBase(topLeft);
    layers.requestObjects(below);
    expect(provider.requests).toHaveLength(3);

    // 视口挪了，这一页新露出来一块
    tick(500);
    layers.requestBase([...topLeft, ...below]);
    expect(provider.requests).toHaveLength(4);
    expect(provider.requests[3]!.request.layer?.mode).toBe('without');
    expect(layers.ready).toBe(true);
    expect(layers.base()).toHaveLength(2);
    provider.requests[3]!.resolve(tile());
    await flush();
    expect(layers.base()).toHaveLength(3);
    expect(layers.readyMs).toBe(readyMs);
  });

  it('有一块渲染不出来：整个不能用了，也不再要别的', async () => {
    const { provider, layers, topLeft, below } = setup();
    const logged = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    layers.requestBase(topLeft);
    layers.requestObjects(below);
    provider.requests[0]!.resolve(tile());
    provider.requests[1]!.reject(new Error('渲染失败'));
    provider.requests[2]!.resolve(tile());
    await flush();
    expect(layers.failed).toBe(true);
    expect(layers.ready).toBe(false);
    expect(layers.readyMs).toBeNull();
    expect(logged).toHaveBeenCalledTimes(1);

    layers.requestBase(below);
    expect(provider.requests).toHaveLength(3);
  });

  it('瓦片来源自己放弃了（比如引擎正在重启）也算不能用，但不当成错误记下来', async () => {
    const { provider, layers, topLeft, below } = setup();
    const logged = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    layers.requestBase(topLeft);
    layers.requestObjects(below);
    provider.requests[2]!.reject(new AbortError('引擎正在重启'));
    await flush();
    expect(layers.failed).toBe(true);
    expect(logged).not.toHaveBeenCalled();
  });

  it('释放：在途的请求取消，纹理交还，之后到的结果不要', async () => {
    const { provider, layers, topLeft, below, uploaded, released, changes } = setup();
    layers.requestBase(topLeft);
    layers.requestObjects(below);
    provider.requests[0]!.resolve(tile());
    await flush();
    expect(uploaded).toHaveLength(1);
    const notified = changes();

    layers.dispose();
    expect(provider.requests.every(({ request }) => request.signal.aborted)).toBe(true);
    expect(released).toEqual([uploaded[0]]);
    expect(layers.ready).toBe(false);
    expect(layers.base()).toEqual([]);

    // 取消之前已经在路上的结果、取消引起的拒绝，都不再理会
    provider.requests[1]!.resolve(tile());
    provider.requests[2]!.reject(new AbortError());
    await flush();
    expect(uploaded).toHaveLength(1);
    expect(changes()).toBe(notified);
    expect(layers.failed).toBe(false);

    // 释放两次没有关系
    layers.dispose();
    expect(released).toHaveLength(1);
  });
});
