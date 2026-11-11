import { inflateRect, rectContains, rectContainsPoint, rectsIntersect } from '../geometry';
import type { Point, Rect } from '../geometry';
import type { PageObject, PageSnapshot } from './types';

/**
 * 命中测试接口。P0 在界面这边按包围盒线性扫描：悬停时每帧都要查，状态机又是同步的，不能每次去问引擎。
 * 引擎那边的规则（哪些对象点选时跳过）随对象列表一起给过来（`PageObject.pickable`）。
 * 上万个对象的页面、按路径和字形精确判断要靠引擎的 R-tree（方案 3.4、第 7 节），那时这个接口要改成异步的。
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

/** 画布上点选、框选能不能选到：锁定的对象不能（《交互设计》2.1），看不见的对象也不能（方案 4.3）。 */
function selectable(object: PageObject): boolean {
  return !object.locked && object.pickable !== false;
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
