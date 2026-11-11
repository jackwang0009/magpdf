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

/**
 * 界面到主进程的通道，Tauri 实现：
 * 请求走命令 `engine_request`，事件走 Tauri 事件，瓦片走自定义协议（方案 3.1）。
 */
export class TauriTransport implements EngineTransport {
  private readonly listeners = new Set<(event: EngineEvent) => void>();
  private readonly unlisten: Promise<() => void>;

  constructor() {
    this.unlisten = listen<EngineEvent>(ENGINE_EVENT, (event) => {
      for (const listener of this.listeners) listener(event.payload);
    });
    this.unlisten.catch((error: unknown) => console.error('订阅引擎事件失败', error));
  }

  request<T = unknown>(method: string, params?: unknown): Promise<T> {
    return invoke<T>('engine_request', { method, params: params ?? null });
  }

  onEvent(listener: (event: EngineEvent) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  async fetchTile(slot: number, generation: number): Promise<Uint8Array> {
    const response = await fetch(`${TILE_BASE}/${slot}/${generation}`, { cache: 'no-store' });
    if (response.status === 410) throw new TileGoneError(slot, generation);
    if (!response.ok) throw new Error(`取瓦片失败：HTTP ${response.status}`);
    return new Uint8Array(await response.arrayBuffer());
  }

  dispose(): void {
    this.listeners.clear();
    void this.unlisten.then((stop) => stop()).catch(() => undefined);
  }
}
