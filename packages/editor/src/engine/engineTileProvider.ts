import { EVENT, METHOD } from '@magpdf/protocol';
import type { DocChanged, RenderTilesParams, RenderTilesResult, TileReady } from '@magpdf/protocol';
import { AbortError } from '../canvas/tileProvider';
import type { TileProvider, TileRequest, TileResult } from '../canvas/tileProvider';
import type { Rect } from '../geometry';
import { TileGoneError, describeError, isEngineRestarted } from './transport';
import type { EngineEvent, EngineTransport } from './transport';

/** 引擎瓦片的边长，设备像素（《接口协议》3.3）。 */
export const ENGINE_TILE_SIZE = 512;
const ENGINE_TILE_BYTES = ENGINE_TILE_SIZE * ENGINE_TILE_SIZE * 4;

/** 槽位被别的瓦片占了之后最多重新请求几次。 */
const MAX_RETRIES = 2;

export interface EngineTileTarget {
  readonly docId: number;
  readonly pageId: number;
}

export interface EngineTileProviderOptions {
  readonly transport: EngineTransport;
  /**
   * 页序号对应引擎里的哪份文档、哪一页。引擎重启后 `docId` 会变，所以每次现问；
   * 文档现在不可用（引擎正在重启、页面不存在）时返回 null。
   */
  readonly target: (page: number) => EngineTileTarget | null;
  /** 等 `render.tileReady` 的上限（ms），默认 30 秒。渲染失败时引擎不会发任何消息，只能靠它。 */
  readonly timeoutMs?: number;
  /** 最多留几块引擎瓦片的像素（每块 1 MB），默认 12。 */
  readonly cacheTiles?: number;
}

interface Waiter {
  readonly resolve: (ready: TileReady) => void;
  readonly reject: (error: Error) => void;
  readonly timer: ReturnType<typeof setTimeout>;
}

/**
 * 瓦片来源：向引擎要位图（《接口协议》3.3、6.3）。
 *
 * 画布的瓦片是 256 像素见方，引擎的是 512（见 `tiles.ts` 开头的说明）。这里把画布要的每一块对到盖住它的
 * 那块引擎瓦片上：`render.tiles` 请求渲染，等 `render.tileReady`，经 `magpdf://tile/…` 取回像素，再裁出
 * 画布要的那一块，同时把 BGRA 换成 RGBA。一块引擎瓦片最多被四块画布瓦片用到，所以取回的像素先留一会儿。
 */
export class EngineTileProvider implements TileProvider {
  /**
   * 引擎里 PDFium 是串行的，在途请求多了并不会更快；但四块画布瓦片共用一块引擎瓦片，
   * 给 4 才能让它们一起等同一次渲染。
   */
  readonly concurrency = 4;
  /** 统计：取回的引擎瓦片数、因槽位被占而重试的次数、用上已取回像素的次数 */
  readonly stats = { engineTiles: 0, retries: 0, reused: 0 };

  private readonly transport: EngineTransport;
  private readonly target: (page: number) => EngineTileTarget | null;
  private readonly timeoutMs: number;
  private readonly capacity: number;
  /** 引擎瓦片的像素，按最近使用排序（Map 的插入顺序） */
  private readonly tiles = new Map<string, Promise<Uint8Array>>();
  private readonly waiting = new Map<number, Waiter>();
  /** 先于 `render.tiles` 的应答到达的事件：命令的应答和事件走两条路，先后没有保证 */
  private readonly early = new Map<number, TileReady>();
  private readonly unsubscribe: () => void;
  private disposed = false;

  constructor(options: EngineTileProviderOptions) {
    this.transport = options.transport;
    this.target = options.target;
    this.timeoutMs = options.timeoutMs ?? 30_000;
    this.capacity = Math.max(1, options.cacheTiles ?? 12);
    this.unsubscribe = this.transport.onEvent((event) => this.onEvent(event));
  }

  async renderTile(request: TileRequest): Promise<TileResult> {
    if (this.disposed || request.signal.aborted) throw new AbortError();
    const origin = engineTileOrigin(request.pixelRect);
    const source = await abortable(this.engineTile(request, origin), request.signal);
    const { width, height } = request.pixelRect;
    return {
      width,
      height,
      pixels: { data: cropBgraToRgba(source, origin, request.pixelRect), width, height },
    };
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.unsubscribe();
    this.abandon('瓦片来源已释放');
  }

  private engineTile(request: TileRequest, origin: PixelPoint): Promise<Uint8Array> {
    const key = `${request.page}@${request.pxPerPt}:${origin.x},${origin.y}#${request.documentVersion}`;
    const known = this.tiles.get(key);
    if (known) {
      this.tiles.delete(key);
      this.tiles.set(key, known);
      this.stats.reused++;
      return known;
    }
    const loading = this.load(request.page, request.pxPerPt, origin);
    this.tiles.set(key, loading);
    // 失败的不留着，下次重新要；这里接住拒绝，免得没人等的时候变成未处理的拒绝
    loading.catch(() => {
      if (this.tiles.get(key) === loading) this.tiles.delete(key);
    });
    while (this.tiles.size > this.capacity) {
      const oldest = this.tiles.keys().next().value;
      if (oldest === undefined) break;
      this.tiles.delete(oldest);
    }
    return loading;
  }

  private async load(page: number, scale: number, origin: PixelPoint): Promise<Uint8Array> {
    for (let attempt = 0; ; attempt++) {
      const target = this.disposed ? null : this.target(page);
      if (!target) throw new AbortError('文档现在不可用');
      let ready: TileReady;
      try {
        const params: RenderTilesParams = {
          docId: target.docId,
          pageId: target.pageId,
          scale,
          tiles: [origin],
          priority: 'visible',
        };
        const result = await this.transport.request<RenderTilesResult>(METHOD.RENDER_TILES, params);
        const requestId = result.requestIds[0];
        if (requestId === undefined) throw new Error('render.tiles 没有返回 requestId');
        ready = await this.waitReady(requestId);
      } catch (error) {
        // 引擎重启不算失败：恢复之后画布会重新要这块瓦片
        if (isEngineRestarted(error)) throw new AbortError('引擎正在重启');
        throw toError(error);
      }
      try {
        const pixels = await this.transport.fetchTile(ready.slot, ready.generation);
        if (pixels.byteLength !== ENGINE_TILE_BYTES) {
          throw new Error(`瓦片大小不对：${pixels.byteLength} 字节`);
        }
        this.stats.engineTiles++;
        return pixels;
      } catch (error) {
        // 槽位在取之前被别的瓦片占了：重新请求渲染（《接口协议》3.3）
        if (error instanceof TileGoneError && attempt < MAX_RETRIES) {
          this.stats.retries++;
          continue;
        }
        throw toError(error);
      }
    }
  }

  private waitReady(requestId: number): Promise<TileReady> {
    const early = this.early.get(requestId);
    if (early) {
      this.early.delete(requestId);
      return Promise.resolve(early);
    }
    return new Promise<TileReady>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiting.delete(requestId);
        reject(new Error(`等了 ${this.timeoutMs} ms 瓦片还没有渲染出来`));
      }, this.timeoutMs);
      this.waiting.set(requestId, { resolve, reject, timer });
    });
  }

  private onEvent(event: EngineEvent): void {
    switch (event.method) {
      case EVENT.RENDER_TILE_READY: {
        const ready = event.params as TileReady;
        const waiter = this.waiting.get(ready.requestId);
        if (waiter) {
          clearTimeout(waiter.timer);
          this.waiting.delete(ready.requestId);
          waiter.resolve(ready);
        } else if (this.owns(ready.docId)) {
          this.early.set(ready.requestId, ready);
          // 没人来认领的（请求已经取消）不能一直攒着
          while (this.early.size > 64) {
            const oldest = this.early.keys().next().value;
            if (oldest === undefined) break;
            this.early.delete(oldest);
          }
        }
        break;
      }
      case EVENT.DOC_CHANGED:
        // 文档变了，手里的像素都是旧版本的
        if (this.owns((event.params as DocChanged).docId)) this.tiles.clear();
        break;
      case EVENT.ENGINE_RESTARTING:
        // 旧引擎的请求编号和槽位都作废了
        this.abandon('引擎正在重启');
        break;
      default:
        break;
    }
  }

  /** 这个事件说的是不是本文档。页面编号不参与判断，问第 0 页就够了。 */
  private owns(docId: number): boolean {
    return this.target(0)?.docId === docId;
  }

  private abandon(reason: string): void {
    for (const waiter of this.waiting.values()) {
      clearTimeout(waiter.timer);
      waiter.reject(new AbortError(reason));
    }
    this.waiting.clear();
    this.early.clear();
    this.tiles.clear();
  }
}

interface PixelPoint {
  readonly x: number;
  readonly y: number;
}

/** 盖住 `rect` 的那块引擎瓦片的左上角（整页像素坐标，512 的倍数）。 */
export function engineTileOrigin(rect: Rect): PixelPoint {
  return {
    x: Math.floor(rect.x / ENGINE_TILE_SIZE) * ENGINE_TILE_SIZE,
    y: Math.floor(rect.y / ENGINE_TILE_SIZE) * ENGINE_TILE_SIZE,
  };
}

/**
 * 从一块引擎瓦片（512×512、BGRA）里裁出 `rect`（整页像素坐标）这一块，换成 RGBA。
 * `origin` 是这块引擎瓦片的左上角。
 */
export function cropBgraToRgba(source: Uint8Array, origin: PixelPoint, rect: Rect): Uint8Array {
  const left = rect.x - origin.x;
  const top = rect.y - origin.y;
  if (
    left < 0 ||
    top < 0 ||
    left + rect.width > ENGINE_TILE_SIZE ||
    top + rect.height > ENGINE_TILE_SIZE
  ) {
    throw new RangeError('要裁的区域超出了这块引擎瓦片');
  }
  const out = new Uint8Array(rect.width * rect.height * 4);
  let o = 0;
  for (let y = 0; y < rect.height; y++) {
    let s = ((top + y) * ENGINE_TILE_SIZE + left) * 4;
    for (let x = 0; x < rect.width; x++) {
      out[o] = source[s + 2]!;
      out[o + 1] = source[s + 1]!;
      out[o + 2] = source[s]!;
      out[o + 3] = source[s + 3]!;
      o += 4;
      s += 4;
    }
  }
  return out;
}

/** 等 `promise`，但 `signal` 一取消就立刻以 AbortError 结束（`promise` 本身继续跑，别人可能还在等）。 */
function abortable<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => reject(new AbortError());
    signal.addEventListener('abort', onAbort, { once: true });
    promise.then(
      (value) => {
        signal.removeEventListener('abort', onAbort);
        resolve(value);
      },
      (error: unknown) => {
        signal.removeEventListener('abort', onAbort);
        reject(toError(error));
      },
    );
  });
}

function toError(error: unknown): Error {
  return error instanceof Error ? error : new Error(describeError(error));
}
