import { afterEach, describe, expect, it, vi } from 'vitest';
import type { TileRequestSpec } from './renderPlan';
import { TileCache } from './tileCache';
import type { TileEntry } from './tileCache';
import { AbortError } from './tileProvider';
import type { TileProvider, TileRequest, TileResult } from './tileProvider';
import { TileScheduler } from './tileScheduler';
import { tileKeyString } from './tiles';

function spec(page: number, tx: number): TileRequestSpec {
  const key = { page, level: 6, tx, ty: 0 };
  return {
    id: tileKeyString(key),
    key,
    pxPerPt: 4 / 3,
    pixelRect: { x: tx * 256, y: 0, width: 256, height: 256 },
    pageRect: { x: tx * 192, y: 0, width: 192, height: 192 },
    priority: tx,
  };
}

interface ManualRequest {
  readonly request: TileRequest;
  /** 是不是已经有结果了（完成或失败）。被取消的请求看 `request.signal.aborted`。 */
  settled: boolean;
  readonly finish: (label?: string) => void;
  readonly fail: (error: Error) => void;
}

/** 瓦片来源：请求先挂着，测试说完成才完成。 */
class ManualProvider implements TileProvider {
  readonly concurrency = 8;
  readonly requests: ManualRequest[] = [];

  renderTile(request: TileRequest): Promise<TileResult> {
    return new Promise((resolve, reject) => {
      request.signal.addEventListener('abort', () => reject(new AbortError()), { once: true });
      const record: ManualRequest = {
        request,
        settled: false,
        // 像素数据里放一个标记，好认出缓存里放的是哪一次的结果
        finish: (label = '') => {
          record.settled = true;
          resolve({
            width: 256,
            height: 256,
            pixels: { data: new TextEncoder().encode(label), width: 256, height: 256 },
          });
        },
        fail: (error) => {
          record.settled = true;
          reject(error);
        },
      };
      this.requests.push(record);
    });
  }

  /** 还在途的请求：没有结果，也没被取消 */
  get open(): ManualRequest[] {
    return this.requests.filter((r) => !r.settled && !r.request.signal.aborted);
  }
}

function setup() {
  const provider = new ManualProvider();
  const cache = new TileCache(64 * 1024 * 1024);
  const ready: TileEntry[] = [];
  const scheduler = new TileScheduler(provider, cache, (entry) => ready.push(entry));
  const label = (id: string) => {
    const pixels = cache.peek(id)?.pixels;
    return pixels && 'data' in pixels ? new TextDecoder().decode(pixels.data) : null;
  };
  return { provider, cache, scheduler, ready, label };
}

/** 等 Promise 的回调走完。 */
async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe('瓦片调度', () => {
  it('按顺序请求想要的瓦片，到了放进缓存；已经有的不再请求', async () => {
    const { provider, cache, scheduler, ready } = setup();
    const wanted = [spec(0, 0), spec(0, 1)];
    scheduler.setWanted(wanted, 3);
    expect(provider.requests.map(({ request }) => [request.tx, request.documentVersion])).toEqual([
      [0, 3],
      [1, 3],
    ]);
    provider.requests[0]!.finish();
    await settle();
    expect(ready.map((entry) => entry.id)).toEqual([wanted[0]!.id]);
    expect(cache.isFresh(wanted[0]!.id)).toBe(true);

    scheduler.setWanted(wanted, 3);
    expect(provider.requests).toHaveLength(2);
    expect(scheduler.stats).toMatchObject({ completed: 1, aborted: 0, failed: 0 });
  });

  it('视图移走了：不再想要的在途请求取消', async () => {
    const { provider, scheduler, ready } = setup();
    scheduler.setWanted([spec(0, 0), spec(0, 1)], 0);
    scheduler.setWanted([spec(0, 1)], 0);
    expect(provider.requests[0]!.request.signal.aborted).toBe(true);
    expect(provider.requests[1]!.request.signal.aborted).toBe(false);
    await settle();
    expect(ready).toEqual([]);
    expect(scheduler.stats.aborted).toBe(1);
  });

  it('页面画面变了：在途的请求取消，旧瓦片留着，重新请求，新的到了换掉旧的', async () => {
    const { provider, cache, scheduler, ready, label } = setup();
    const tile = spec(0, 0);
    const inFlight = spec(0, 1);
    scheduler.setWanted([tile], 0);
    provider.requests[0]!.finish('改之前');
    await settle();
    scheduler.setWanted([tile, inFlight], 0);
    expect(provider.open).toHaveLength(1);

    scheduler.invalidatePage(0);
    // 在途的那一块是按改之前的画面渲染的，不能要；已经有的那一块标成旧的，先留着
    expect(provider.open).toHaveLength(0);
    expect(cache.has(tile.id)).toBe(true);
    expect(cache.isFresh(tile.id)).toBe(false);
    expect(label(tile.id)).toBe('改之前');

    scheduler.setWanted([tile, inFlight], 1);
    expect(provider.open.map(({ request }) => [request.tx, request.documentVersion])).toEqual([
      [0, 1],
      [1, 1],
    ]);
    provider.open[0]!.finish('改之后');
    await settle();
    expect(cache.isFresh(tile.id)).toBe(true);
    expect(label(tile.id)).toBe('改之后');
    expect(ready.at(-1)!.id).toBe(tile.id);
    // 被取消的那个请求晚到的结果不会放进缓存
    provider.requests[1]!.finish('改之前，晚到');
    await settle();
    expect(cache.has(inFlight.id)).toBe(false);
  });

  it('只有一块地方变了：碰到它的瓦片重新要，别的瓦片留着、在途的照样有效', async () => {
    const { provider, cache, scheduler } = setup();
    const [left, right, far] = [spec(0, 0), spec(0, 1), spec(0, 3)];
    scheduler.setWanted([left, right, far], 0);
    provider.requests[0]!.finish();
    await settle();
    // 变的地方在第 1 块里（x 200–260 pt），离第 0 块 8 pt，往外放宽两个像素也碰不到
    scheduler.invalidateArea(0, { x: 200, y: 10, width: 60, height: 20 });
    expect(cache.isFresh(left.id)).toBe(true);
    expect(provider.open.map(({ request }) => request.tx)).toEqual([3]);

    scheduler.setWanted([left, right, far], 1);
    expect(provider.open.map(({ request }) => request.tx)).toEqual([3, 1]);
  });

  it('一页改了，别的页在途的瓦片照样有效', async () => {
    const { provider, cache, scheduler } = setup();
    const other = spec(1, 0);
    scheduler.setWanted([other], 0);
    // 第 0 页改了，文档版本号跟着变，但第 1 页的画面没有变
    scheduler.invalidatePage(0);
    scheduler.setWanted([other], 1);
    expect(provider.requests).toHaveLength(1);
    provider.requests[0]!.finish();
    await settle();
    expect(cache.isFresh(other.id)).toBe(true);
  });

  it('页面排布变了：在途和排队的请求全部作废', async () => {
    const { provider, cache, scheduler } = setup();
    scheduler.setWanted([spec(0, 0), spec(1, 0)], 0);
    scheduler.reset();
    expect(provider.open).toHaveLength(0);
    expect(scheduler.inFlight).toBe(0);
    expect(scheduler.queued).toBe(0);
    provider.requests[0]!.finish();
    await settle();
    expect(cache.count).toBe(0);
  });

  it('渲染失败的瓦片记一笔，下次还想要时重新请求', async () => {
    const { provider, scheduler } = setup();
    const logged = vi.spyOn(console, 'error').mockImplementation(() => undefined);
    const tile = spec(0, 0);
    scheduler.setWanted([tile], 0);
    provider.requests[0]!.fail(new Error('渲染失败'));
    await settle();
    expect(scheduler.stats.failed).toBe(1);
    expect(logged).toHaveBeenCalledTimes(1);
    scheduler.setWanted([tile], 0);
    expect(provider.requests).toHaveLength(2);
  });
});
