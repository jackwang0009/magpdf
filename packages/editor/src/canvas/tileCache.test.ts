import { describe, expect, it, vi } from 'vitest';
import { TileCache } from './tileCache';
import type { TileEntry } from './tileCache';

function entry(id: string, bytes: number, page = 0): TileEntry {
  return {
    key: { page, level: 6, tx: 0, ty: 0 },
    id,
    width: 4,
    height: 4,
    bytes,
    version: 1,
    pixels: null,
    texture: null,
  };
}

describe('瓦片 LRU 缓存', () => {
  it('超出上限时淘汰最久未用的条目', () => {
    const cache = new TileCache(300);
    cache.set(entry('a', 100));
    cache.set(entry('b', 100));
    cache.set(entry('c', 100));
    cache.get('a'); // a 变成最近使用
    cache.set(entry('d', 100));
    expect(cache.has('b')).toBe(false);
    expect(cache.has('a')).toBe(true);
    expect(cache.has('c')).toBe(true);
    expect(cache.has('d')).toBe(true);
    expect(cache.bytes).toBe(300);
  });

  it('刚放入的条目即使超限也保留', () => {
    const cache = new TileCache(150);
    cache.set(entry('a', 100));
    cache.set(entry('big', 200));
    expect(cache.has('big')).toBe(true);
    expect(cache.has('a')).toBe(false);
  });

  it('只有一块地方变了：各个缩放档里碰到它的瓦片标成旧的，别的不动', () => {
    const cache = new TileCache(1000);
    const at = (id: string, page: number, level: number, tx: number): TileEntry => ({
      ...entry(id, 10, page),
      key: { page, level, tx, ty: 0 },
    });
    cache.set(at('100%-0', 0, 6, 0)); // x 0–192 pt
    cache.set(at('100%-1', 0, 6, 1)); // x 192–384 pt
    cache.set(at('200%-0', 0, 8, 0)); // x 0–96 pt
    cache.set(at('200%-1', 0, 8, 1)); // x 96–192 pt
    cache.set(at('第 2 页', 1, 6, 0));
    cache.markAreaStale(0, { x: 10, y: 10, width: 20, height: 20 });
    const fresh = ['100%-0', '100%-1', '200%-0', '200%-1', '第 2 页'].filter((id) =>
      cache.isFresh(id),
    );
    expect(fresh).toEqual(['100%-1', '200%-1', '第 2 页']);
    // 旧的先留着顶上
    expect(cache.has('100%-0')).toBe(true);
  });

  it('按页失效只清掉该页', () => {
    const cache = new TileCache(1000);
    cache.set(entry('p0', 100, 0));
    cache.set(entry('p1', 100, 1));
    cache.invalidatePage(0);
    expect(cache.has('p0')).toBe(false);
    expect(cache.has('p1')).toBe(true);
    expect(cache.bytes).toBe(100);
  });

  it('页面画面变了：这一页的瓦片标成旧的，留着先顶上，放进新的就换掉', () => {
    const onEvict = vi.fn();
    const cache = new TileCache(1000, onEvict);
    cache.set(entry('p0', 100, 0));
    cache.set(entry('p1', 100, 1));
    cache.markPageStale(0);
    // 旧的还在（画得出来），但不算最新；别的页不受影响
    expect(cache.has('p0')).toBe(true);
    expect(cache.isFresh('p0')).toBe(false);
    expect(cache.isFresh('p1')).toBe(true);
    expect(cache.isFresh('没有这块')).toBe(false);
    expect(onEvict).not.toHaveBeenCalled();

    const replacement = entry('p0', 120, 0);
    cache.set(replacement);
    expect(cache.isFresh('p0')).toBe(true);
    expect(cache.peek('p0')).toBe(replacement);
    expect(cache.bytes).toBe(220);
    // 旧的那块交给渲染器释放纹理
    expect(onEvict).toHaveBeenCalledTimes(1);
    expect(onEvict.mock.calls[0]![0].stale).toBe(true);
  });

  it('淘汰时通知渲染器释放纹理', () => {
    const onEvict = vi.fn();
    const cache = new TileCache(100, onEvict);
    cache.set(entry('a', 100));
    cache.set(entry('b', 100));
    expect(onEvict).toHaveBeenCalledTimes(1);
    expect(onEvict.mock.calls[0]![0].id).toBe('a');
  });

  it('调小上限立即淘汰', () => {
    const cache = new TileCache(1000);
    cache.set(entry('a', 100));
    cache.set(entry('b', 100));
    cache.maxBytes = 150;
    expect(cache.count).toBe(1);
    expect(cache.has('b')).toBe(true);
  });
});
