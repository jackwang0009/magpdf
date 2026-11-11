import { releasePixels } from './tileProvider';
import type { TilePixels } from './tileProvider';
import type { TileKey } from './tiles';

/**
 * 瓦片缓存：按字节上限的 LRU。
 * 条目里同时记录 CPU 位图（上传到 GPU 后释放）和 GPU 纹理（由渲染器填写、淘汰时通过 onEvict 释放）。
 */
export interface TileEntry {
  readonly key: TileKey;
  readonly id: string;
  readonly width: number;
  readonly height: number;
  readonly bytes: number;
  /** 发出请求时的文档版本 */
  readonly version: number;
  pixels: TilePixels | null;
  /** 渲染器持有的纹理；类型故意宽松，缓存本身不依赖 WebGL */
  texture: unknown;
  /**
   * 这一页的画面已经变了，这块是旧的。旧瓦片先留着顶上（否则每改一次页面就闪一下白），
   * 同时重新去要；新的到了由 `set` 换掉。
   */
  stale?: boolean;
}

export class TileCache {
  private readonly entries = new Map<string, TileEntry>();
  private readonly byPage = new Map<number, Set<string>>();
  private _bytes = 0;
  private _maxBytes: number;

  constructor(
    maxBytes: number,
    private readonly onEvict?: (entry: TileEntry) => void,
  ) {
    this._maxBytes = maxBytes;
  }

  get bytes(): number {
    return this._bytes;
  }

  get count(): number {
    return this.entries.size;
  }

  get maxBytes(): number {
    return this._maxBytes;
  }

  set maxBytes(value: number) {
    this._maxBytes = value;
    this.evictToLimit();
  }

  /** 有没有这块瓦片（旧的也算：可以先画着）。 */
  has(id: string): boolean {
    return this.entries.has(id);
  }

  /** 有这块瓦片，而且不是旧的：不用再去要。 */
  isFresh(id: string): boolean {
    const entry = this.entries.get(id);
    return entry !== undefined && !entry.stale;
  }

  /** 取出并标记为最近使用 */
  get(id: string): TileEntry | undefined {
    const entry = this.entries.get(id);
    if (!entry) return undefined;
    this.entries.delete(id);
    this.entries.set(id, entry);
    return entry;
  }

  /** 只查看，不改变 LRU 顺序 */
  peek(id: string): TileEntry | undefined {
    return this.entries.get(id);
  }

  set(entry: TileEntry): void {
    const existing = this.entries.get(entry.id);
    if (existing) this.remove(existing);
    this.entries.set(entry.id, entry);
    this._bytes += entry.bytes;
    let pageSet = this.byPage.get(entry.key.page);
    if (!pageSet) {
      pageSet = new Set();
      this.byPage.set(entry.key.page, pageSet);
    }
    pageSet.add(entry.id);
    this.evictToLimit(entry.id);
  }

  /** 某页画面变了（对象移动、引擎推送变更事件）：该页的瓦片标成旧的，留着先顶上。 */
  markPageStale(page: number): void {
    const ids = this.byPage.get(page);
    if (!ids) return;
    for (const id of ids) {
      const entry = this.entries.get(id);
      if (entry) entry.stale = true;
    }
  }

  /** 丢掉某页的全部瓦片（页面没了，或者旧瓦片不能再用） */
  invalidatePage(page: number): void {
    const ids = this.byPage.get(page);
    if (!ids) return;
    for (const id of Array.from(ids)) {
      const entry = this.entries.get(id);
      if (entry) this.remove(entry);
    }
  }

  clear(): void {
    for (const entry of Array.from(this.entries.values())) this.remove(entry);
  }

  /** 按 LRU 顺序（最久未用在前）列出条目，测试和调试用 */
  ids(): string[] {
    return Array.from(this.entries.keys());
  }

  private remove(entry: TileEntry): void {
    if (!this.entries.delete(entry.id)) return;
    this._bytes -= entry.bytes;
    const pageSet = this.byPage.get(entry.key.page);
    if (pageSet) {
      pageSet.delete(entry.id);
      if (pageSet.size === 0) this.byPage.delete(entry.key.page);
    }
    releasePixels(entry.pixels);
    entry.pixels = null;
    this.onEvict?.(entry);
  }

  /** 淘汰最久未用的条目直到不超限；`keep` 是刚放进来的条目，哪怕超限也不动它 */
  private evictToLimit(keep?: string): void {
    for (const [id, entry] of this.entries) {
      if (this._bytes <= this._maxBytes) break;
      if (id === keep) continue;
      this.remove(entry);
    }
  }
}
