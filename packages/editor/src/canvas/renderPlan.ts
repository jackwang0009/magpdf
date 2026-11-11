import type { PageLayout } from '../document/pageLayout';
import { intersectRect, rectCenter } from '../geometry';
import type { Rect, Size } from '../geometry';
import { visibleDocRect } from '../view/viewState';
import type { ViewState } from '../view/viewState';
import {
  TILE_LEVELS,
  pageTileGrid,
  pickLevel,
  tileKeyString,
  tilePageRect,
  tilePixelRect,
  tilesInPageRect,
} from './tiles';
import type { TileGrid, TileKey } from './tiles';

/**
 * 每帧的“计算”部分：根据视图算出要画哪些瓦片、缺哪些、先预取哪些。
 * 不碰 WebGL，可以在 jsdom 里测试；GL 调用在 glRenderer.ts。
 */
export interface TileQuad {
  readonly id: string;
  readonly key: TileKey;
  /** 瓦片在文档坐标中的矩形（pt） */
  readonly docRect: Rect;
}

export interface TileRequestSpec {
  readonly id: string;
  readonly key: TileKey;
  readonly pxPerPt: number;
  readonly pixelRect: Rect;
  readonly pageRect: Rect;
  /** 越小越先渲染 */
  readonly priority: number;
}

export interface PagePlacement {
  readonly page: number;
  readonly docRect: Rect;
}

export interface RenderPlan {
  readonly targetLevel: number;
  /** 可见页（画底色和描边用） */
  readonly pages: readonly PagePlacement[];
  /** 其他档位里能临时顶上的瓦片，先画；粗的在前 */
  readonly fallback: readonly TileQuad[];
  /** 目标档位里已有的瓦片（包括已经变旧、正等着换的），后画 */
  readonly tiles: readonly TileQuad[];
  /** 可见、要去请求的瓦片：缺的，以及画着但已经变旧的；按优先级排序 */
  readonly missing: readonly TileRequestSpec[];
  /** 视口周边的预取，优先级低于 missing */
  readonly prefetch: readonly TileRequestSpec[];
}

export interface PlanInput {
  readonly layout: PageLayout;
  readonly view: ViewState;
  /** 视口尺寸，CSS 像素 */
  readonly viewport: Size;
  readonly devicePixelRatio: number;
  /** 这块瓦片现在画得出来吗（旧的也算） */
  readonly has: (id: string) => boolean;
  /**
   * 画得出来的瓦片是不是最新的。不是的话照常画（先顶着），同时列进要请求的瓦片里。
   * 不给时凡是画得出来的都当作最新的。
   */
  readonly isFresh?: (id: string) => boolean;
  /** 预取范围：视口向四周扩大的比例，默认 0.5（各方向半个视口） */
  readonly prefetchRatio?: number;
}

export function computeRenderPlan(input: PlanInput): RenderPlan {
  const { layout, view, viewport, has } = input;
  /** 有，而且不用重新要 */
  const settled = (id: string) => has(id) && (input.isFresh?.(id) ?? true);
  const prefetchRatio = input.prefetchRatio ?? 0.5;
  const targetLevel = pickLevel(view.zoom * input.devicePixelRatio);
  const visRect = visibleDocRect(view, viewport);
  const center = rectCenter(visRect);
  const pages: PagePlacement[] = [];
  const tiles: TileQuad[] = [];
  const missing: TileRequestSpec[] = [];
  const prefetch: TileRequestSpec[] = [];
  const fallback = new Map<string, TileQuad>();
  const seen = new Set<string>();

  const expanded: Rect = {
    x: visRect.x - visRect.width * prefetchRatio,
    y: visRect.y - visRect.height * prefetchRatio,
    width: visRect.width * (1 + 2 * prefetchRatio),
    height: visRect.height * (1 + 2 * prefetchRatio),
  };

  const [first, last] = layout.visiblePages(expanded.y, expanded.y + expanded.height);
  for (let page = first; page < last; page++) {
    const pageRect = layout.pageRect(page);
    const pageSize = layout.pageSize(page);
    const grid = pageTileGrid(pageSize, targetLevel);
    const visibleInter = intersectRect(visRect, pageRect);
    const isVisible = visibleInter !== null;
    if (isVisible) pages.push({ page, docRect: pageRect });

    if (visibleInter) {
      const local = toLocal(visibleInter, pageRect);
      const missingLocal: Rect[] = [];
      for (const key of tilesInPageRect(grid, local, page, targetLevel)) {
        const id = tileKeyString(key);
        seen.add(id);
        const tileLocal = tilePageRect(grid, key.tx, key.ty);
        const docRect = toDoc(tileLocal, pageRect);
        if (has(id)) {
          tiles.push({ id, key, docRect });
          // 旧瓦片先画着，同时重新要
          if (!settled(id)) {
            missing.push(spec(id, key, grid, tileLocal, distanceSq(rectCenter(docRect), center)));
          }
        } else {
          missing.push(spec(id, key, grid, tileLocal, distanceSq(rectCenter(docRect), center)));
          missingLocal.push(tileLocal);
        }
      }
      if (missingLocal.length > 0) {
        collectFallback(missingLocal, page, pageRect, pageSize, targetLevel, has, fallback);
      }
    }

    // 预取：扩大后的视口范围内、目标档位里还没有的瓦片
    const prefetchInter = intersectRect(expanded, pageRect);
    if (prefetchInter) {
      const local = toLocal(prefetchInter, pageRect);
      for (const key of tilesInPageRect(grid, local, page, targetLevel)) {
        const id = tileKeyString(key);
        if (seen.has(id) || settled(id)) continue;
        seen.add(id);
        const tileLocal = tilePageRect(grid, key.tx, key.ty);
        const docRect = toDoc(tileLocal, pageRect);
        prefetch.push(spec(id, key, grid, tileLocal, distanceSq(rectCenter(docRect), center)));
      }
    }

    // 最粗一档的整页缩略图始终保留一份，缩放时总有东西可以先顶上
    if (isVisible && targetLevel !== 0) {
      const thumbGrid = pageTileGrid(pageSize, 0);
      for (const key of tilesInPageRect(
        thumbGrid,
        { x: 0, y: 0, width: pageSize.width, height: pageSize.height },
        page,
        0,
      )) {
        const id = tileKeyString(key);
        if (seen.has(id) || settled(id)) continue;
        seen.add(id);
        const tileLocal = tilePageRect(thumbGrid, key.tx, key.ty);
        prefetch.push(spec(id, key, thumbGrid, tileLocal, Number.MAX_SAFE_INTEGER / 2));
      }
    }
  }

  missing.sort((a, b) => a.priority - b.priority);
  prefetch.sort((a, b) => a.priority - b.priority);
  const fallbackList = Array.from(fallback.values()).sort(
    (a, b) => a.key.level - b.key.level || a.key.page - b.key.page,
  );
  return { targetLevel, pages, fallback: fallbackList, tiles, missing, prefetch };
}

function spec(
  id: string,
  key: TileKey,
  grid: TileGrid,
  pageRect: Rect,
  priority: number,
): TileRequestSpec {
  return {
    id,
    key,
    pxPerPt: grid.pxPerPt,
    pixelRect: tilePixelRect(grid, key.tx, key.ty),
    pageRect,
    priority,
  };
}

/**
 * 为缺失区域找其他档位里已缓存的瓦片。先找相邻档位（清晰度最接近），再逐步远离；
 * 某一档位能完全盖住缺失矩形就停，否则把找到的加进去继续找更粗的。
 */
function collectFallback(
  missingLocal: readonly Rect[],
  page: number,
  pageRect: Rect,
  pageSize: Size,
  targetLevel: number,
  has: (id: string) => boolean,
  out: Map<string, TileQuad>,
): void {
  const candidates: number[] = [];
  for (let d = 1; d < TILE_LEVELS.length; d++) {
    if (targetLevel - d >= 0) candidates.push(targetLevel - d);
    if (targetLevel + d < TILE_LEVELS.length) candidates.push(targetLevel + d);
  }
  for (const rect of missingLocal) {
    for (const level of candidates) {
      const grid = pageTileGrid(pageSize, level);
      const keys = tilesInPageRect(grid, rect, page, level);
      let present = 0;
      for (const key of keys) {
        const id = tileKeyString(key);
        if (!has(id)) continue;
        present++;
        if (!out.has(id)) {
          out.set(id, { id, key, docRect: toDoc(tilePageRect(grid, key.tx, key.ty), pageRect) });
        }
      }
      if (present === keys.length && present > 0) break;
    }
  }
}

function toLocal(r: Rect, pageRect: Rect): Rect {
  return { x: r.x - pageRect.x, y: r.y - pageRect.y, width: r.width, height: r.height };
}

function toDoc(r: Rect, pageRect: Rect): Rect {
  return { x: r.x + pageRect.x, y: r.y + pageRect.y, width: r.width, height: r.height };
}

function distanceSq(a: { x: number; y: number }, b: { x: number; y: number }): number {
  const dx = a.x - b.x;
  const dy = a.y - b.y;
  return dx * dx + dy * dy;
}
