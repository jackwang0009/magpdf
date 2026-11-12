import type { Rect } from '../geometry';
import type { TileRequestSpec } from './renderPlan';
import type { TileCache, TileEntry } from './tileCache';
import { isAbortError, releasePixels } from './tileProvider';
import type { TileProvider, TileRequest } from './tileProvider';
import { TILE_SIZE, tileBytes, tileTouches } from './tiles';

/**
 * 瓦片调度：维护“想要的瓦片”队列，按优先级向 TileProvider 发请求，限制并发，视图移走后取消不再需要的请求。
 * 优先级来自 renderPlan：可见 > 预取（方案 3.3 的排队思路在界面侧的对应物；拖动分层、缩略图由引擎侧排）。
 *
 * 页面画面变了的时候（`invalidatePage`），这一页在途的请求取消，缓存里的瓦片标成旧的：旧瓦片继续显示，
 * 同时重新请求，新的到了再换掉。一个请求只要没被取消，它的结果就是有效的，所以这里不再比较文档版本号——
 * 别的页改了并不影响这一页的瓦片。
 */
export interface TileSchedulerOptions {
  /** 同时在途的请求数；未提供时用 provider.concurrency，再没有则 3 */
  readonly concurrency?: number;
}

interface Pending {
  readonly spec: TileRequestSpec;
  readonly controller: AbortController;
}

export class TileScheduler {
  private readonly pending = new Map<string, Pending>();
  private queue: TileRequestSpec[] = [];
  private version = 0;
  private disposed = false;
  private readonly concurrency: number;
  /** 统计：完成、取消、失败的请求数。每个请求只落在其中一项里。 */
  readonly stats = { completed: 0, aborted: 0, failed: 0 };

  constructor(
    private readonly provider: TileProvider,
    private readonly cache: TileCache,
    private readonly onTileReady: (entry: TileEntry) => void,
    options: TileSchedulerOptions = {},
  ) {
    this.concurrency = Math.max(1, options.concurrency ?? provider.concurrency ?? 3);
  }

  get inFlight(): number {
    return this.pending.size;
  }

  get queued(): number {
    return this.queue.length;
  }

  /**
   * 替换想要的瓦片列表（已按优先级排序：先 missing 后 prefetch）。
   * 在途但不在新列表里的请求取消；缓存里有而且不旧的跳过。
   * `documentVersion` 随请求交给瓦片来源。
   */
  setWanted(specs: readonly TileRequestSpec[], documentVersion: number): void {
    if (this.disposed) return;
    this.version = documentVersion;
    const wanted = new Set<string>();
    const queue: TileRequestSpec[] = [];
    for (const spec of specs) {
      if (wanted.has(spec.id)) continue;
      wanted.add(spec.id);
      if (this.cache.isFresh(spec.id) || this.pending.has(spec.id)) continue;
      queue.push(spec);
    }
    for (const [id, pending] of this.pending) {
      if (!wanted.has(id)) {
        pending.controller.abort();
        this.pending.delete(id);
        this.stats.aborted++;
      }
    }
    this.queue = queue;
    this.pump();
  }

  /** 某页画面变了：取消该页在途的请求，缓存里该页的瓦片标成旧的（下一次 setWanted 会重新请求） */
  invalidatePage(page: number): void {
    for (const [id, pending] of this.pending) {
      if (pending.spec.key.page === page) {
        pending.controller.abort();
        this.pending.delete(id);
        this.stats.aborted++;
      }
    }
    this.queue = this.queue.filter((spec) => spec.key.page !== page);
    this.cache.markPageStale(page);
  }

  /**
   * 某页上只有 `rect`（页面坐标，pt）这一块画面变了：和它相交的瓦片在途的请求取消、排队的拿掉，缓存里的
   * 标成旧的。别处的瓦片还是对的，留着不动，不用再向引擎要。
   */
  invalidateArea(page: number, rect: Rect): void {
    const touched = (spec: TileRequestSpec) =>
      spec.key.page === page && tileTouches(spec.key, rect);
    for (const [id, pending] of this.pending) {
      if (touched(pending.spec)) {
        pending.controller.abort();
        this.pending.delete(id);
        this.stats.aborted++;
      }
    }
    this.queue = this.queue.filter((spec) => !touched(spec));
    this.cache.markAreaStale(page, rect);
  }

  /** 页面的排布变了（页数、尺寸）：在途和排队的请求全部作废。缓存由调用方清空。 */
  reset(): void {
    for (const pending of this.pending.values()) {
      pending.controller.abort();
      this.stats.aborted++;
    }
    this.pending.clear();
    this.queue = [];
  }

  dispose(): void {
    this.disposed = true;
    for (const pending of this.pending.values()) pending.controller.abort();
    this.pending.clear();
    this.queue = [];
  }

  private pump(): void {
    while (!this.disposed && this.pending.size < this.concurrency && this.queue.length > 0) {
      const spec = this.queue.shift()!;
      if (this.cache.isFresh(spec.id) || this.pending.has(spec.id)) continue;
      this.start(spec);
    }
  }

  private start(spec: TileRequestSpec): void {
    const controller = new AbortController();
    const version = this.version;
    const request: TileRequest = {
      page: spec.key.page,
      level: spec.key.level,
      pxPerPt: spec.pxPerPt,
      tx: spec.key.tx,
      ty: spec.key.ty,
      tileSize: TILE_SIZE,
      pixelRect: spec.pixelRect,
      pageRect: spec.pageRect,
      documentVersion: version,
      signal: controller.signal,
    };
    this.pending.set(spec.id, { spec, controller });
    this.provider.renderTile(request).then(
      (result) => {
        const stillPending = this.pending.get(spec.id)?.controller === controller;
        if (!stillPending || this.disposed) {
          // 已经取消了（取消的时候记过数），晚到的结果不要
          releasePixels(result.pixels);
          this.pump();
          return;
        }
        this.pending.delete(spec.id);
        const entry: TileEntry = {
          key: spec.key,
          id: spec.id,
          width: result.width,
          height: result.height,
          bytes: tileBytes(result.width, result.height),
          version,
          pixels: result.pixels,
          texture: null,
        };
        // 缓存里要是有这块的旧瓦片，这里把它换掉
        this.cache.set(entry);
        this.stats.completed++;
        this.onTileReady(entry);
        this.pump();
      },
      (error: unknown) => {
        // 不在途了说明是这边取消的，取消的时候已经记过数
        if (this.pending.get(spec.id)?.controller === controller) {
          this.pending.delete(spec.id);
          // 瓦片来源自己放弃的（比如引擎正在重启）算取消，别的算失败
          if (isAbortError(error)) this.stats.aborted++;
          else {
            this.stats.failed++;
            console.error('瓦片渲染失败', spec.id, error);
          }
        }
        this.pump();
      },
    );
  }
}
