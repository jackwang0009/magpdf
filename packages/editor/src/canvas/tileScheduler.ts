import type { TileRequestSpec } from './renderPlan';
import type { TileCache, TileEntry } from './tileCache';
import { isAbortError, releasePixels } from './tileProvider';
import type { TileProvider, TileRequest } from './tileProvider';
import { TILE_SIZE, tileBytes } from './tiles';

/**
 * 瓦片调度：维护“想要的瓦片”队列，按优先级向 TileProvider 发请求，限制并发，视图移走后取消不再需要的请求。
 * 优先级来自 renderPlan：可见 > 预取（方案 3.3 的排队思路在界面侧的对应物；拖动分层、缩略图由引擎侧排）。
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
  /** 统计：完成、取消、丢弃（版本过期）的请求数 */
  readonly stats = { completed: 0, aborted: 0, stale: 0, failed: 0 };

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
   * 在途但不在新列表里的请求取消；已缓存的跳过。
   */
  setWanted(specs: readonly TileRequestSpec[], documentVersion: number): void {
    if (this.disposed) return;
    this.version = documentVersion;
    const wanted = new Set<string>();
    const queue: TileRequestSpec[] = [];
    for (const spec of specs) {
      if (wanted.has(spec.id)) continue;
      wanted.add(spec.id);
      if (this.cache.has(spec.id) || this.pending.has(spec.id)) continue;
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

  /** 某页内容变了：取消该页在途的请求并清掉缓存 */
  invalidatePage(page: number): void {
    for (const [id, pending] of this.pending) {
      if (pending.spec.key.page === page) {
        pending.controller.abort();
        this.pending.delete(id);
        this.stats.aborted++;
      }
    }
    this.queue = this.queue.filter((spec) => spec.key.page !== page);
    this.cache.invalidatePage(page);
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
      if (this.cache.has(spec.id) || this.pending.has(spec.id)) continue;
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
        if (stillPending) this.pending.delete(spec.id);
        if (!stillPending || this.disposed || controller.signal.aborted) {
          releasePixels(result.pixels);
          this.stats.aborted++;
          this.pump();
          return;
        }
        if (version !== this.version) {
          releasePixels(result.pixels);
          this.stats.stale++;
          this.pump();
          return;
        }
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
        this.cache.set(entry);
        this.stats.completed++;
        this.onTileReady(entry);
        this.pump();
      },
      (error: unknown) => {
        if (this.pending.get(spec.id)?.controller === controller) this.pending.delete(spec.id);
        if (isAbortError(error)) this.stats.aborted++;
        else {
          this.stats.failed++;
          console.error('瓦片渲染失败', spec.id, error);
        }
        this.pump();
      },
    );
  }
}
