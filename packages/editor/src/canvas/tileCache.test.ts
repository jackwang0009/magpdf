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

  it('按页失效只清掉该页', () => {
    const cache = new TileCache(1000);
    cache.set(entry('p0', 100, 0));
    cache.set(entry('p1', 100, 1));
    cache.invalidatePage(0);
    expect(cache.has('p0')).toBe(false);
    expect(cache.has('p1')).toBe(true);
    expect(cache.bytes).toBe(100);
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
