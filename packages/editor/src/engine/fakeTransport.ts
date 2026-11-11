import { EVENT, METHOD } from '@magpdf/protocol';
import type { RenderTilesParams, TileReady } from '@magpdf/protocol';
import { TileGoneError } from './transport';
import type { EngineEvent, EngineTransport } from './transport';

/**
 * 测试用的通道：记下每条请求，按 `handle` 给的结果应答，事件由测试自己发。
 * 不在包的出口里，只给本目录的测试用。
 */
export class FakeTransport implements EngineTransport {
  readonly requests: { readonly method: string; readonly params: unknown }[] = [];
  /** 取过哪些瓦片，`槽位/代数` */
  readonly fetched: string[] = [];
  /** 共享内存里现有的瓦片，`槽位/代数` → 像素 */
  readonly tiles = new Map<string, Uint8Array>();
  /** 怎么应答一条请求；抛出协议的错误对象表示失败 */
  handle: (method: string, params: unknown) => unknown = (method) => {
    throw { code: 1001, message: `测试没有处理 ${method}` };
  };

  private readonly listeners = new Set<(event: EngineEvent) => void>();
  private nextRequestId = 1;

  request<T = unknown>(method: string, params?: unknown): Promise<T> {
    this.requests.push({ method, params });
    return Promise.resolve().then(() => this.handle(method, params) as T);
  }

  onEvent(listener: (event: EngineEvent) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  fetchTile(slot: number, generation: number): Promise<Uint8Array> {
    const key = `${slot}/${generation}`;
    this.fetched.push(key);
    const pixels = this.tiles.get(key);
    return pixels ? Promise.resolve(pixels) : Promise.reject(new TileGoneError(slot, generation));
  }

  emit(method: string, params: unknown): void {
    for (const listener of [...this.listeners]) listener({ method, params });
  }

  get listenerCount(): number {
    return this.listeners.size;
  }

  /** 发出去的某个方法的请求参数 */
  sent<T>(method: string): T[] {
    return this.requests.filter((r) => r.method === method).map((r) => r.params as T);
  }

  /**
   * 像引擎那样应答一条 `render.tiles`：分配 requestId，把 `pixels` 放进一个新槽位。
   * 返回应答和对应的 `render.tileReady` 参数，什么时候发事件由测试决定。
   */
  renderTile(params: RenderTilesParams, pixels: Uint8Array) {
    const requestId = this.nextRequestId++;
    const tile = params.tiles[0]!;
    const ready: TileReady = {
      requestId,
      docId: params.docId,
      pageId: params.pageId,
      scale: params.scale,
      x: tile.x,
      y: tile.y,
      version: 0,
      slot: requestId % 16,
      generation: 2 * requestId,
    };
    this.tiles.set(`${ready.slot}/${ready.generation}`, pixels);
    return { result: { requestIds: [requestId] }, ready };
  }

  /** 最常见的引擎行为：`render.tiles` 立即应答，随后（下一个微任务）发 `render.tileReady`。 */
  renderWith(pixels: (params: RenderTilesParams) => Uint8Array): void {
    this.handle = (method, params) => {
      if (method !== METHOD.RENDER_TILES) throw { code: 1001, message: `测试没有处理 ${method}` };
      const request = params as RenderTilesParams;
      const { result, ready } = this.renderTile(request, pixels(request));
      queueMicrotask(() => this.emit(EVENT.RENDER_TILE_READY, ready));
      return result;
    };
  }
}
