import { EVENT, METHOD } from '@magpdf/protocol';
import type { RenderTilesParams } from '@magpdf/protocol';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { isAbortError } from '../canvas/tileProvider';
import type { RawPixels, TileRequest } from '../canvas/tileProvider';
import type { Rect } from '../geometry';
import {
  ENGINE_TILE_SIZE,
  EngineTileProvider,
  cropBgraToRgba,
  engineTileOrigin,
} from './engineTileProvider';
import { FakeTransport } from './fakeTransport';

/**
 * 一块引擎瓦片（512×512、BGRA），每个像素都认得出自己在哪：
 * B = x 的低 8 位，G = y 的低 8 位，R = 所在象限（右半 +1，下半 +2），A = 255。
 */
function engineTile(tag = 0): Uint8Array {
  const pixels = new Uint8Array(ENGINE_TILE_SIZE * ENGINE_TILE_SIZE * 4);
  for (let y = 0; y < ENGINE_TILE_SIZE; y++) {
    for (let x = 0; x < ENGINE_TILE_SIZE; x++) {
      const i = (y * ENGINE_TILE_SIZE + x) * 4;
      pixels[i] = x & 255;
      pixels[i + 1] = y & 255;
      pixels[i + 2] = (x >= 256 ? 1 : 0) + (y >= 256 ? 2 : 0) + tag;
      pixels[i + 3] = 255;
    }
  }
  return pixels;
}

function tileRequest(pixelRect: Rect, overrides: Partial<TileRequest> = {}): TileRequest {
  return {
    page: 0,
    level: 6,
    pxPerPt: 4 / 3,
    tx: pixelRect.x / 256,
    ty: pixelRect.y / 256,
    tileSize: 256,
    pixelRect,
    pageRect: { x: 0, y: 0, width: 1, height: 1 },
    documentVersion: 0,
    signal: new AbortController().signal,
    ...overrides,
  };
}

function provider(transport: FakeTransport, options: { timeoutMs?: number } = {}) {
  return new EngineTileProvider({
    transport,
    target: (page) => (page < 3 ? { docId: 7, pageId: 100 + page } : null),
    ...options,
  });
}

/** 等在途的 Promise 都走完（请求发出去、应答回来、等事件的登记好）。 */
function settle(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function rgbaAt(pixels: RawPixels, x: number, y: number): number[] {
  const i = (y * pixels.width + x) * 4;
  return Array.from(pixels.data.slice(i, i + 4));
}

afterEach(() => {
  vi.useRealTimers();
});

describe('引擎瓦片的几何', () => {
  it('画布的 256 像素瓦片落在盖住它的那块 512 像素引擎瓦片里', () => {
    expect(engineTileOrigin({ x: 0, y: 0, width: 256, height: 256 })).toEqual({ x: 0, y: 0 });
    expect(engineTileOrigin({ x: 256, y: 256, width: 256, height: 256 })).toEqual({ x: 0, y: 0 });
    expect(engineTileOrigin({ x: 512, y: 768, width: 88, height: 256 })).toEqual({
      x: 512,
      y: 512,
    });
    expect(engineTileOrigin({ x: 1280, y: 0, width: 256, height: 40 })).toEqual({ x: 1024, y: 0 });
  });

  it('裁出指定的一块，并把 BGRA 换成 RGBA', () => {
    const out = cropBgraToRgba(
      engineTile(),
      { x: 512, y: 0 },
      { x: 768, y: 256, width: 3, height: 2 },
    );
    // 整页 (768, 256) 是这块引擎瓦片里的 (256, 256)：右下象限 R = 3，G = y 低位，B = x 低位
    expect(Array.from(out)).toEqual([
      ...[3, 0, 0, 255],
      ...[3, 0, 1, 255],
      ...[3, 0, 2, 255],
      ...[3, 1, 0, 255],
      ...[3, 1, 1, 255],
      ...[3, 1, 2, 255],
    ]);
  });

  it('要裁的区域不在这块引擎瓦片里时报错，而不是读到别处去', () => {
    const origin = { x: 0, y: 0 };
    expect(() =>
      cropBgraToRgba(engineTile(), origin, { x: 384, y: 0, width: 256, height: 16 }),
    ).toThrow(RangeError);
    expect(() =>
      cropBgraToRgba(engineTile(), origin, { x: 512, y: 0, width: 16, height: 16 }),
    ).toThrow(RangeError);
  });
});

describe('EngineTileProvider', () => {
  it('向引擎要盖住它的那块瓦片，取回后裁出自己的一块', async () => {
    const transport = new FakeTransport();
    transport.renderWith(() => engineTile());
    const tiles = provider(transport);

    const result = await tiles.renderTile(
      tileRequest({ x: 256, y: 512, width: 256, height: 120 }, { page: 2, pxPerPt: 1.885618 }),
    );

    expect(transport.sent<RenderTilesParams>(METHOD.RENDER_TILES)).toEqual([
      { docId: 7, pageId: 102, scale: 1.885618, tiles: [{ x: 0, y: 512 }], priority: 'visible' },
    ]);
    expect(result.width).toBe(256);
    expect(result.height).toBe(120);
    const pixels = result.pixels as RawPixels;
    expect(pixels.data).toHaveLength(256 * 120 * 4);
    // 整页 (256, 512) 是引擎瓦片里的 (256, 0)：右上象限
    expect(rgbaAt(pixels, 0, 0)).toEqual([1, 0, 0, 255]);
    expect(rgbaAt(pixels, 255, 119)).toEqual([1, 119, 255, 255]);
  });

  it('同一块引擎瓦片里的四块画布瓦片只渲染一次', async () => {
    const transport = new FakeTransport();
    transport.renderWith(() => engineTile());
    const tiles = provider(transport);

    const quadrants = await Promise.all(
      [
        { x: 0, y: 0 },
        { x: 256, y: 0 },
        { x: 0, y: 256 },
        { x: 256, y: 256 },
      ].map(({ x, y }) => tiles.renderTile(tileRequest({ x, y, width: 256, height: 256 }))),
    );

    expect(transport.sent(METHOD.RENDER_TILES)).toHaveLength(1);
    expect(transport.fetched).toHaveLength(1);
    expect(quadrants.map((q) => rgbaAt(q.pixels as RawPixels, 0, 0)[0])).toEqual([0, 1, 2, 3]);
    expect(tiles.stats).toMatchObject({ engineTiles: 1, reused: 3 });

    // 别的缩放、别的版本、别的引擎瓦片各算各的
    await tiles.renderTile(tileRequest({ x: 0, y: 0, width: 256, height: 256 }, { pxPerPt: 2 }));
    await tiles.renderTile(
      tileRequest({ x: 0, y: 0, width: 256, height: 256 }, { documentVersion: 1 }),
    );
    await tiles.renderTile(tileRequest({ x: 512, y: 0, width: 256, height: 256 }));
    expect(transport.sent(METHOD.RENDER_TILES)).toHaveLength(4);
  });

  it('render.tileReady 比 render.tiles 的应答先到也认得出来', async () => {
    const transport = new FakeTransport();
    transport.handle = (_method, params) => {
      const { result, ready } = transport.renderTile(params as RenderTilesParams, engineTile());
      // 事件和应答走两条路：这里让事件在应答交到调用方之前就发出去
      transport.emit(EVENT.RENDER_TILE_READY, ready);
      return result;
    };
    const tiles = provider(transport);
    const result = await tiles.renderTile(tileRequest({ x: 0, y: 0, width: 256, height: 256 }));
    expect(result.width).toBe(256);
  });

  it('别的文档的 render.tileReady 不会被攒下来', async () => {
    const transport = new FakeTransport();
    transport.renderWith(() => engineTile());
    const tiles = provider(transport);
    for (let requestId = 1000; requestId < 1200; requestId++) {
      transport.emit(EVENT.RENDER_TILE_READY, { requestId, docId: 8, slot: 0, generation: 2 });
    }
    // 自己文档的、没人认领的事件最多留 64 条；这里只确认照常工作
    const result = await tiles.renderTile(tileRequest({ x: 0, y: 0, width: 256, height: 256 }));
    expect(result.height).toBe(256);
  });

  it('槽位在取之前被别的瓦片占了，就重新请求渲染', async () => {
    const transport = new FakeTransport();
    let renders = 0;
    transport.handle = (_method, params) => {
      renders++;
      const { result, ready } = transport.renderTile(params as RenderTilesParams, engineTile());
      // 第一次：事件发出去之后槽位立刻被覆盖
      if (renders === 1) transport.tiles.clear();
      queueMicrotask(() => transport.emit(EVENT.RENDER_TILE_READY, ready));
      return result;
    };
    const tiles = provider(transport);
    const result = await tiles.renderTile(tileRequest({ x: 0, y: 0, width: 256, height: 256 }));
    expect(result.width).toBe(256);
    expect(renders).toBe(2);
    expect(tiles.stats.retries).toBe(1);
  });

  it('一直取不到就报错，下次再要时重新来', async () => {
    const transport = new FakeTransport();
    let lose = true;
    transport.handle = (_method, params) => {
      const { result, ready } = transport.renderTile(params as RenderTilesParams, engineTile());
      if (lose) transport.tiles.clear();
      queueMicrotask(() => transport.emit(EVENT.RENDER_TILE_READY, ready));
      return result;
    };
    const tiles = provider(transport);
    const rect = { x: 0, y: 0, width: 256, height: 256 };
    await expect(tiles.renderTile(tileRequest(rect))).rejects.toThrow('瓦片已被替换');
    expect(transport.sent(METHOD.RENDER_TILES)).toHaveLength(3);

    lose = false;
    await expect(tiles.renderTile(tileRequest(rect))).resolves.toMatchObject({ width: 256 });
  });

  it('请求取消后以 AbortError 结束；同一块引擎瓦片的其他请求不受影响', async () => {
    const transport = new FakeTransport();
    let release = () => {};
    transport.handle = (_method, params) => {
      const { result, ready } = transport.renderTile(params as RenderTilesParams, engineTile());
      release = () => transport.emit(EVENT.RENDER_TILE_READY, ready);
      return result;
    };
    const tiles = provider(transport);
    const abort = new AbortController();
    const cancelled = tiles.renderTile(
      tileRequest({ x: 0, y: 0, width: 256, height: 256 }, { signal: abort.signal }),
    );
    const kept = tiles.renderTile(tileRequest({ x: 256, y: 0, width: 256, height: 256 }));
    await settle();

    abort.abort();
    await expect(cancelled).rejects.toSatisfy(isAbortError);
    release();
    await expect(kept).resolves.toMatchObject({ width: 256 });

    const already = new AbortController();
    already.abort();
    await expect(
      tiles.renderTile(
        tileRequest({ x: 0, y: 0, width: 256, height: 256 }, { signal: already.signal }),
      ),
    ).rejects.toSatisfy(isAbortError);
  });

  it('引擎重启时在等的请求算取消，不算失败', async () => {
    const transport = new FakeTransport();
    transport.handle = (_method, params) =>
      transport.renderTile(params as RenderTilesParams, engineTile()).result;
    const tiles = provider(transport);
    const waiting = tiles.renderTile(tileRequest({ x: 0, y: 0, width: 256, height: 256 }));
    await settle();
    transport.emit(EVENT.ENGINE_RESTARTING, {});
    await expect(waiting).rejects.toSatisfy(isAbortError);

    // 请求发出去时引擎已经没了
    transport.handle = () => {
      throw { code: 1002, message: '处理过程中引擎重启' };
    };
    await expect(
      tiles.renderTile(tileRequest({ x: 0, y: 0, width: 256, height: 256 })),
    ).rejects.toSatisfy(isAbortError);
  });

  it('文档不可用（引擎还没恢复、页面不存在）时不发请求', async () => {
    const transport = new FakeTransport();
    const tiles = provider(transport);
    await expect(
      tiles.renderTile(tileRequest({ x: 0, y: 0, width: 256, height: 256 }, { page: 9 })),
    ).rejects.toSatisfy(isAbortError);
    expect(transport.requests).toHaveLength(0);
  });

  it('引擎的其他错误原样报出来', async () => {
    const transport = new FakeTransport();
    transport.handle = () => {
      throw { code: 3005, message: '参数不合法' };
    };
    const tiles = provider(transport);
    const failed = tiles.renderTile(tileRequest({ x: 0, y: 0, width: 256, height: 256 }));
    await expect(failed).rejects.toThrow('参数不合法');
    await expect(failed).rejects.not.toSatisfy(isAbortError);
  });

  it('等不到 render.tileReady 就超时', async () => {
    vi.useFakeTimers();
    const transport = new FakeTransport();
    transport.handle = (_method, params) =>
      transport.renderTile(params as RenderTilesParams, engineTile()).result;
    const tiles = provider(transport, { timeoutMs: 1000 });
    const waiting = tiles.renderTile(tileRequest({ x: 0, y: 0, width: 256, height: 256 }));
    const outcome = expect(waiting).rejects.toThrow('瓦片还没有渲染出来');
    await vi.advanceTimersByTimeAsync(1001);
    await outcome;
  });

  it('文档变了之后，手里的像素不再用', async () => {
    const transport = new FakeTransport();
    let tag = 0;
    transport.renderWith(() => engineTile(tag));
    const tiles = provider(transport);
    const rect = { x: 0, y: 0, width: 256, height: 256 };
    await tiles.renderTile(tileRequest(rect));

    // 别的文档变了，不相干
    transport.emit(EVENT.DOC_CHANGED, { docId: 8, version: 1, changes: [], pagesChanged: false });
    await tiles.renderTile(tileRequest(rect));
    expect(transport.sent(METHOD.RENDER_TILES)).toHaveLength(1);

    tag = 10;
    transport.emit(EVENT.DOC_CHANGED, { docId: 7, version: 1, changes: [], pagesChanged: false });
    const fresh = await tiles.renderTile(tileRequest(rect));
    expect(transport.sent(METHOD.RENDER_TILES)).toHaveLength(2);
    expect(rgbaAt(fresh.pixels as RawPixels, 0, 0)[0]).toBe(10);
  });

  it('释放后不再订阅事件，也不再接请求', async () => {
    const transport = new FakeTransport();
    transport.renderWith(() => engineTile());
    const tiles = provider(transport);
    expect(transport.listenerCount).toBe(1);
    tiles.dispose();
    expect(transport.listenerCount).toBe(0);
    await expect(
      tiles.renderTile(tileRequest({ x: 0, y: 0, width: 256, height: 256 })),
    ).rejects.toSatisfy(isAbortError);
  });
});
