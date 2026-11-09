import { rectContainsPoint, inflateRect } from '../geometry';
import type { Point, Rect } from '../geometry';

/**
 * 选择框上的 8 个控制点与旋转热区（《交互设计》2.2 / 《方案》8.3）。
 * 全部在屏幕坐标（CSS px）里计算，不随缩放改变大小。
 */
export type HandleId = 'nw' | 'n' | 'ne' | 'e' | 'se' | 's' | 'sw' | 'w';

export const HANDLE_IDS: readonly HandleId[] = ['nw', 'n', 'ne', 'e', 'se', 's', 'sw', 'w'];

/** 控制点命中半径（px）：比绘制尺寸略大，方便点中 */
export const HANDLE_HIT_RADIUS = 5;
/** 旋转热区：角控制点外侧 6–20 px 的环形区域 */
export const ROTATE_ZONE_INNER = 6;
export const ROTATE_ZONE_OUTER = 20;

export interface Handle {
  readonly id: HandleId;
  readonly center: Point;
}

export function handleCenters(rect: Rect): readonly Handle[] {
  const { x, y, width, height } = rect;
  const cx = x + width / 2;
  const cy = y + height / 2;
  const right = x + width;
  const bottom = y + height;
  return [
    { id: 'nw', center: { x, y } },
    { id: 'n', center: { x: cx, y } },
    { id: 'ne', center: { x: right, y } },
    { id: 'e', center: { x: right, y: cy } },
    { id: 'se', center: { x: right, y: bottom } },
    { id: 's', center: { x: cx, y: bottom } },
    { id: 'sw', center: { x, y: bottom } },
    { id: 'w', center: { x, y: cy } },
  ];
}

export function hitHandle(rect: Rect, point: Point, radius = HANDLE_HIT_RADIUS): HandleId | null {
  for (const handle of handleCenters(rect)) {
    if (
      Math.abs(point.x - handle.center.x) <= radius &&
      Math.abs(point.y - handle.center.y) <= radius
    ) {
      return handle.id;
    }
  }
  return null;
}

/** 点是否落在旋转热区（选择框外侧、角控制点附近的环形区域） */
export function inRotateZone(rect: Rect, point: Point): boolean {
  if (rectContainsPoint(inflateRect(rect, ROTATE_ZONE_INNER), point)) return false;
  if (!rectContainsPoint(inflateRect(rect, ROTATE_ZONE_OUTER), point)) return false;
  // 只有靠近四角的区域算旋转热区，边中点外侧留给普通点击
  const nearX =
    point.x < rect.x + ROTATE_ZONE_OUTER || point.x > rect.x + rect.width - ROTATE_ZONE_OUTER;
  const nearY =
    point.y < rect.y + ROTATE_ZONE_OUTER || point.y > rect.y + rect.height - ROTATE_ZONE_OUTER;
  return nearX && nearY;
}

/** 控制点对应的 CSS 光标 */
export function handleCursor(id: HandleId): string {
  switch (id) {
    case 'n':
    case 's':
      return 'ns-resize';
    case 'e':
    case 'w':
      return 'ew-resize';
    case 'nw':
    case 'se':
      return 'nwse-resize';
    case 'ne':
    case 'sw':
      return 'nesw-resize';
  }
}

/**
 * 按控制点拖动得到新矩形（文档坐标）。dx/dy 为拖动位移；
 * keepRatio 为按住 Shift 时等比缩放（角控制点有效）。
 */
export function resizeRectByHandle(
  rect: Rect,
  id: HandleId,
  dx: number,
  dy: number,
  keepRatio = false,
  minSize = 0.5,
): Rect {
  let left = rect.x;
  let top = rect.y;
  let right = rect.x + rect.width;
  let bottom = rect.y + rect.height;
  if (id.includes('w')) left += dx;
  if (id.includes('e')) right += dx;
  if (id.includes('n')) top += dy;
  if (id.includes('s')) bottom += dy;

  if (keepRatio && rect.width > 0 && rect.height > 0 && id.length === 2) {
    const ratio = rect.width / rect.height;
    const w = right - left;
    const h = bottom - top;
    if (Math.abs(w) / rect.width > Math.abs(h) / rect.height) {
      const newH = Math.abs(w) / ratio;
      if (id.includes('n')) top = bottom - newH;
      else bottom = top + newH;
    } else {
      const newW = Math.abs(h) * ratio;
      if (id.includes('w')) left = right - newW;
      else right = left + newW;
    }
  }

  // 不允许翻转到负尺寸，留一个最小尺寸
  if (right - left < minSize) {
    if (id.includes('w')) left = right - minSize;
    else right = left + minSize;
  }
  if (bottom - top < minSize) {
    if (id.includes('n')) top = bottom - minSize;
    else bottom = top + minSize;
  }
  return { x: left, y: top, width: right - left, height: bottom - top };
}

/** 把 from 矩形到 to 矩形的仿射变换应用到子矩形（多选整体缩放用） */
export function mapRectBetween(child: Rect, from: Rect, to: Rect): Rect {
  const sx = from.width === 0 ? 1 : to.width / from.width;
  const sy = from.height === 0 ? 1 : to.height / from.height;
  return {
    x: to.x + (child.x - from.x) * sx,
    y: to.y + (child.y - from.y) * sy,
    width: child.width * sx,
    height: child.height * sy,
  };
}
