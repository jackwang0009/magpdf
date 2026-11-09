import { inflateRect, rectContains, rectContainsPoint, rectsIntersect } from '../geometry';
import type { Point, Rect } from '../geometry';
import type { PageObject, PageSnapshot } from './types';

/**
 * 命中测试接口。P0 用线性扫描（每页对象不多）；上万个对象的页面由引擎侧的 R-tree 负责（方案 3.4、第 7 节），
 * 届时实现同一接口、走进程间查询即可。
 */
export type MarqueeMode = 'intersect' | 'contain';

export interface HitTester {
  /** 光标下最上层的可选对象（跳过锁定的对象）；tolerance 为容差（pt），用于细线 */
  hitPoint(page: PageSnapshot, point: Point, tolerance: number): PageObject | null;
  /** 与矩形相交（或被完全包含）的可选对象，按绘制顺序 */
  hitRect(page: PageSnapshot, rect: Rect, mode: MarqueeMode): PageObject[];
  /** 光标下的全部对象，从上到下（右键菜单“选择下层对象”） */
  objectsAtPoint(page: PageSnapshot, point: Point, tolerance: number): PageObject[];
}

function selectable(object: PageObject): boolean {
  return !object.locked;
}

export const linearHitTester: HitTester = {
  hitPoint(page, point, tolerance) {
    for (let i = page.objects.length - 1; i >= 0; i--) {
      const object = page.objects[i]!;
      if (!selectable(object)) continue;
      if (rectContainsPoint(inflateRect(object.bbox, tolerance), point)) return object;
    }
    return null;
  },
  hitRect(page, rect, mode) {
    const test = mode === 'contain' ? rectContains : rectsIntersect;
    return page.objects.filter((object) => selectable(object) && test(rect, object.bbox));
  },
  objectsAtPoint(page, point, tolerance) {
    const hits: PageObject[] = [];
    for (let i = page.objects.length - 1; i >= 0; i--) {
      const object = page.objects[i]!;
      if (rectContainsPoint(inflateRect(object.bbox, tolerance), point)) hits.push(object);
    }
    return hits;
  },
};
