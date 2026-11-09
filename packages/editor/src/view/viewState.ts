import type { PageLayout } from '../document/pageLayout';
import { CSS_PX_PER_PT, clamp } from '../geometry';
import type { Point, Rect, Size } from '../geometry';

/**
 * 视图状态：缩放倍数和视口左上角对应的文档坐标（pt）。
 * 100% 表示 1 pt = 96/72 CSS 像素（与界面图的 CSS mm 一致）。
 */
export interface ViewState {
  readonly zoom: number;
  readonly x: number;
  readonly y: number;
}

export const MIN_ZOOM = 0.05;
export const MAX_ZOOM = 32;

/** Ctrl+加号/减号的档位 */
export const ZOOM_STEPS: readonly number[] = [
  0.05, 0.0625, 0.0833, 0.125, 0.1667, 0.25, 0.3333, 0.5, 0.6667, 1, 1.5, 2, 3, 4, 6, 8, 12, 16, 24,
  32,
];

export const DEFAULT_VIEW: ViewState = { zoom: 1, x: 0, y: 0 };

/** 当前缩放下 1 pt 对应的 CSS 像素 */
export function cssPxPerPt(zoom: number): number {
  return zoom * CSS_PX_PER_PT;
}

export function docToScreen(view: ViewState, point: Point): Point {
  const s = cssPxPerPt(view.zoom);
  return { x: (point.x - view.x) * s, y: (point.y - view.y) * s };
}

export function screenToDoc(view: ViewState, point: Point): Point {
  const s = cssPxPerPt(view.zoom);
  return { x: view.x + point.x / s, y: view.y + point.y / s };
}

export function docRectToScreen(view: ViewState, r: Rect): Rect {
  const s = cssPxPerPt(view.zoom);
  return { x: (r.x - view.x) * s, y: (r.y - view.y) * s, width: r.width * s, height: r.height * s };
}

/** 视口（CSS 像素尺寸）对应的文档矩形 */
export function visibleDocRect(view: ViewState, viewport: Size): Rect {
  const s = cssPxPerPt(view.zoom);
  return { x: view.x, y: view.y, width: viewport.width / s, height: viewport.height / s };
}

/** 平移：屏幕像素位移 → 新视图 */
export function panBy(view: ViewState, dxPx: number, dyPx: number): ViewState {
  const s = cssPxPerPt(view.zoom);
  return { zoom: view.zoom, x: view.x - dxPx / s, y: view.y - dyPx / s };
}

/** 以屏幕上某点为中心缩放：该点下的文档位置保持不动 */
export function zoomAt(view: ViewState, screenPoint: Point, newZoom: number): ViewState {
  const zoom = clamp(newZoom, MIN_ZOOM, MAX_ZOOM);
  const anchor = screenToDoc(view, screenPoint);
  const s = cssPxPerPt(zoom);
  return { zoom, x: anchor.x - screenPoint.x / s, y: anchor.y - screenPoint.y / s };
}

/** 下一个/上一个档位 */
export function nextZoomStep(zoom: number, direction: 1 | -1): number {
  const eps = 1e-6;
  if (direction > 0) {
    const next = ZOOM_STEPS.find((step) => step > zoom + eps);
    return next ?? MAX_ZOOM;
  }
  for (let i = ZOOM_STEPS.length - 1; i >= 0; i--) {
    const step = ZOOM_STEPS[i]!;
    if (step < zoom - eps) return step;
  }
  return MIN_ZOOM;
}

/** 适合页面：整页放进视口（四周留 marginPx） */
export function fitPageZoom(page: Size, viewport: Size, marginPx = 24): number {
  const availW = Math.max(1, viewport.width - marginPx * 2);
  const availH = Math.max(1, viewport.height - marginPx * 2);
  const zoom = Math.min(
    availW / (page.width * CSS_PX_PER_PT),
    availH / (page.height * CSS_PX_PER_PT),
  );
  return clamp(zoom, MIN_ZOOM, MAX_ZOOM);
}

/** 让某页在视口中居中显示 */
export function centerOnPage(
  layout: PageLayout,
  pageIndex: number,
  zoom: number,
  viewport: Size,
): ViewState {
  const r = layout.pageRect(pageIndex);
  const s = cssPxPerPt(zoom);
  return {
    zoom,
    x: r.x + r.width / 2 - viewport.width / (2 * s),
    y: r.y + r.height / 2 - viewport.height / (2 * s),
  };
}

/** 滚动到某页顶部（页面面板点击时） */
export function scrollToPageTop(
  layout: PageLayout,
  pageIndex: number,
  view: ViewState,
  viewport: Size,
): ViewState {
  const r = layout.pageRect(pageIndex);
  const s = cssPxPerPt(view.zoom);
  return {
    zoom: view.zoom,
    x: r.x + r.width / 2 - viewport.width / (2 * s),
    y: r.y - layout.gap,
  };
}

/**
 * 把视图夹在文档范围内：文档比视口小的方向居中，否则不能滚出文档边界。
 */
export function clampView(view: ViewState, viewport: Size, layout: PageLayout): ViewState {
  const s = cssPxPerPt(view.zoom);
  const vw = viewport.width / s;
  const vh = viewport.height / s;
  const docW = layout.totalWidth;
  const docH = layout.totalHeight;
  const x = docW <= vw ? (docW - vw) / 2 : clamp(view.x, 0, docW - vw);
  const y = docH <= vh ? (docH - vh) / 2 : clamp(view.y, 0, docH - vh);
  return { zoom: view.zoom, x, y };
}

export function viewEquals(a: ViewState, b: ViewState): boolean {
  return a.zoom === b.zoom && a.x === b.x && a.y === b.y;
}
