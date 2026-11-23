import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { TileGoneError } from '@magpdf/editor';
import type { EngineEvent, EngineTransport } from '@magpdf/editor';

/** 主进程转发引擎事件用的 Tauri 事件名（`src-tauri/src/app.rs` 的 `ENGINE_EVENT`）。 */
const ENGINE_EVENT = 'engine-event';

/**
 * 取瓦片的地址。《接口协议》3.3 写的是 `magpdf://tile/<槽位>/<代数>`；WebView2 不认自定义的协议名，
 * Tauri 把它映射成 `http://magpdf.localhost/…`。MagPDF 只有 Windows 版，这里直接写映射后的地址。
 */
const TILE_BASE = 'http://magpdf.localhost/tile';

/** 一块瓦片的字节数（512×512 BGRA）。 */
const TILE_BYTES = 512 * 512 * 4;
/** 共享缓冲区开头两个标记占的字节（`src-tauri/src/shared_tiles.rs` 的 `HEADER_BYTES`）。 */
const SHARED_HEADER_BYTES = 16;
/** 读到一半被下一块瓦片盖掉时重取几次。 */
const SHARED_RETRIES = 3;

/** WebView2 页面里的 `chrome.webview`，只写用到的部分。 */
interface WebView2Host {
  addEventListener(
    type: 'sharedbufferreceived',
    listener: (event: { getBuffer(): ArrayBuffer; additionalData: unknown }) => void,
  ): void;
}

interface SharedTile {
  index: number;
  tag: number;
}

/**
 * 取瓦片走 WebView2 共享缓冲区（`src-tauri/src/shared_tiles.rs`）。主进程没打开这条路、或者 WebView2 太旧
 * 没有这个接口时 {@link SharedTileBuffers.attach} 返回 `null`，调用方走自定义协议。
 */
class SharedTileBuffers {
  private constructor(private readonly buffers: Map<number, ArrayBuffer>) {}

  static async attach(): Promise<SharedTileBuffers | null> {
    if ((await invoke<string>('tile_transport')) !== 'sharedBuffer') return null;
    const host = (window as unknown as { chrome?: { webview?: WebView2Host } }).chrome?.webview;
    if (!host) return null;
    const buffers = new Map<number, ArrayBuffer>();
    let expected = Infinity;
    let done = (): void => undefined;
    const allReceived = new Promise<void>((resolve) => {
      done = resolve;
    });
    host.addEventListener('sharedbufferreceived', (event) => {
      const index = (event.additionalData as { magpdfTileBuffer?: unknown } | null)
        ?.magpdfTileBuffer;
      if (typeof index !== 'number') return;
      buffers.set(index, event.getBuffer());
      if (buffers.size >= expected) done();
    });
    try {
      expected = await invoke<number>('attach_tile_buffers');
    } catch (error) {
      console.warn('共享缓冲区没交到页面，改走自定义协议', error);
      return null;
    }
    if (buffers.size < expected) await allReceived;
    return new SharedTileBuffers(buffers);
  }

  async fetch(slot: number, generation: number): Promise<Uint8Array> {
    for (let attempt = 0; attempt < SHARED_RETRIES; attempt++) {
      let tile: SharedTile;
      try {
        tile = await invoke<SharedTile>('fetch_tile_shared', { slot, generation });
      } catch (error) {
        if (error === 'gone') throw new TileGoneError(slot, generation);
        throw new Error(`取瓦片失败：${String(error)}`, { cause: error });
      }
      const pixels = this.read(tile);
      if (pixels) return pixels;
    }
    throw new Error('取瓦片失败：共享缓冲区一直被别的瓦片占着');
  }

  /**
   * 从缓冲区里拷出一块瓦片。先看写完的标记，拷出像素，再看写入前的标记：中间被下一次取瓦片盖掉的话
   * 两个对不上，返回 `null`。
   */
  read(tile: SharedTile): Uint8Array | null {
    const buffer = this.buffers.get(tile.index);
    if (!buffer) return null;
    const marks = new Uint32Array(buffer, 0, 2);
    if (marks[1] !== tile.tag) return null;
    const pixels = new Uint8Array(buffer, SHARED_HEADER_BYTES, TILE_BYTES).slice();
    return marks[0] === tile.tag ? pixels : null;
  }
}

/** 主进程随 `render.tileReady` 放进缓冲区、还没取走的瓦片最多记几块。 */
const PUSHED_LIMIT = 64;

/**
 * 界面到主进程的通道，Tauri 实现：
 * 请求走命令 `engine_request`，事件走 Tauri 事件，瓦片走自定义协议（方案 3.1）；
 * 主进程打开了共享缓冲区时，瓦片改走共享缓冲区：`render.tileReady` 带着 `sharedBuffer` 的直接从缓冲区读，
 * 不带的（或者读的时候已经被别的瓦片盖掉了）调 `fetch_tile_shared`。
 */
export class TauriTransport implements EngineTransport {
  private readonly listeners = new Set<(event: EngineEvent) => void>();
  private readonly unlisten: Promise<() => void>;
  private readonly shared: Promise<SharedTileBuffers | null>;
  /** 主进程随 `render.tileReady` 放进缓冲区的瓦片，键是 `槽位/代数`；取走就删 */
  private readonly pushed = new Map<string, SharedTile>();
  /** 测试用：强制走自定义协议，好在同一个窗口里比两条路。 */
  forceProtocol = false;
  /** 测试用：不用主进程随事件放好的瓦片，每块都调命令取。 */
  ignorePushed = false;

  constructor() {
    this.unlisten = listen<EngineEvent>(ENGINE_EVENT, (event) => {
      this.remember(event.payload);
      for (const listener of this.listeners) listener(event.payload);
    });
    this.unlisten.catch((error: unknown) => console.error('订阅引擎事件失败', error));
    this.shared = SharedTileBuffers.attach().catch((error: unknown) => {
      console.warn('共享缓冲区不可用，改走自定义协议', error);
      return null;
    });
  }

  request<T = unknown>(method: string, params?: unknown): Promise<T> {
    return invoke<T>('engine_request', { method, params: params ?? null });
  }

  onEvent(listener: (event: EngineEvent) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** 取瓦片走的是哪条路。 */
  async tileTransport(): Promise<'sharedBuffer' | 'protocol'> {
    return !this.forceProtocol && (await this.shared) ? 'sharedBuffer' : 'protocol';
  }

  async fetchTile(slot: number, generation: number): Promise<Uint8Array> {
    const shared = this.forceProtocol ? null : await this.shared;
    if (shared) {
      const key = `${slot}/${generation}`;
      const tile = this.pushed.get(key);
      this.pushed.delete(key);
      const pixels = tile && !this.ignorePushed ? shared.read(tile) : null;
      return pixels ?? shared.fetch(slot, generation);
    }
    const response = await fetch(`${TILE_BASE}/${slot}/${generation}`, { cache: 'no-store' });
    if (response.status === 410) throw new TileGoneError(slot, generation);
    if (!response.ok) throw new Error(`取瓦片失败：HTTP ${response.status}`);
    return new Uint8Array(await response.arrayBuffer());
  }

  /** 记下主进程随 `render.tileReady` 放进缓冲区的瓦片。 */
  private remember(event: EngineEvent): void {
    if (event.method !== 'render.tileReady') return;
    const params = event.params as
      { slot?: number; generation?: number; sharedBuffer?: SharedTile } | undefined;
    if (!params?.sharedBuffer) return;
    this.pushed.set(`${params.slot}/${params.generation}`, params.sharedBuffer);
    while (this.pushed.size > PUSHED_LIMIT) {
      const oldest = this.pushed.keys().next().value;
      if (oldest === undefined) break;
      this.pushed.delete(oldest);
    }
  }

  dispose(): void {
    this.listeners.clear();
    void this.unlisten.then((stop) => stop()).catch(() => undefined);
  }
}
