import { CSS_PX_PER_PT } from '../geometry';
import type { Rect, Size } from '../geometry';

/**
 * 瓦片几何。
 * 瓦片是固定边长（设备像素）的正方形；页面在某个“缩放档”下渲染成位图后按网格切开。
 * 缩放档按半个八度递进（√2 倍），渲染分辨率最多比屏幕需要的高 √2 倍，像素开销不超过 2 倍。
 * 方案第 7 节写的是 512 px 和整八度（1/4、1/2、1、2、4）；这里用更小的瓦片和更密的档位，理由见 V12 报告。
 */
export const TILE_SIZE = 256;

/** 缩放档：设备像素 / 100% 缩放下的 CSS 像素。 */
export const TILE_LEVELS: readonly number[] = Array.from({ length: 13 }, (_, k) =>
  Number(Math.pow(2, (k - 6) / 2).toFixed(6)),
);

export interface TileKey {
  readonly page: number;
  readonly level: number;
  readonly tx: number;
  readonly ty: number;
}

export function tileKeyString(key: TileKey): string {
  return `${key.page}/${key.level}/${key.tx}/${key.ty}`;
}

/** 某档位下设备像素与 pt 的比例 */
export function levelPxPerPt(level: number): number {
  const scale = TILE_LEVELS[level];
  if (scale === undefined) throw new RangeError(`缩放档越界：${level}`);
  return scale * CSS_PX_PER_PT;
}

/**
 * 为当前显示比例选择缩放档：取不小于 deviceScale 的最小档（宁可多渲染、再缩小，也不放大模糊）。
 * 允许 0.5% 的误差，避免 1.0000001 被推到上一档。
 */
export function pickLevel(deviceScale: number): number {
  const target = deviceScale * (1 - 0.005);
  for (let i = 0; i < TILE_LEVELS.length; i++) {
    if (TILE_LEVELS[i]! >= target) return i;
  }
  return TILE_LEVELS.length - 1;
}

export interface TileGrid {
  readonly cols: number;
  readonly rows: number;
  /** 页面在该档位下的位图尺寸（设备像素） */
  readonly pxWidth: number;
  readonly pxHeight: number;
  readonly pxPerPt: number;
}

export function pageTileGrid(page: Size, level: number): TileGrid {
  const pxPerPt = levelPxPerPt(level);
  const pxWidth = Math.ceil(page.width * pxPerPt);
  const pxHeight = Math.ceil(page.height * pxPerPt);
  return {
    cols: Math.max(1, Math.ceil(pxWidth / TILE_SIZE)),
    rows: Math.max(1, Math.ceil(pxHeight / TILE_SIZE)),
    pxWidth,
    pxHeight,
    pxPerPt,
  };
}

/** 瓦片在页面位图中的像素矩形（边缘瓦片裁到页面边界） */
export function tilePixelRect(grid: TileGrid, tx: number, ty: number): Rect {
  const x = tx * TILE_SIZE;
  const y = ty * TILE_SIZE;
  return {
    x,
    y,
    width: Math.min(TILE_SIZE, grid.pxWidth - x),
    height: Math.min(TILE_SIZE, grid.pxHeight - y),
  };
}

/** 瓦片覆盖的页面区域（页面坐标，pt） */
export function tilePageRect(grid: TileGrid, tx: number, ty: number): Rect {
  const px = tilePixelRect(grid, tx, ty);
  return {
    x: px.x / grid.pxPerPt,
    y: px.y / grid.pxPerPt,
    width: px.width / grid.pxPerPt,
    height: px.height / grid.pxPerPt,
  };
}

/** 页面上一块地方变了以后，往外再多重画几个设备像素：抗锯齿会画到对象的包围盒外面一点。 */
export const DAMAGE_MARGIN_PX = 2;

/**
 * 这块瓦片碰不碰得到页面上的 `rect`（页面坐标，pt）。`rect` 先往外放宽 `marginPx` 个设备像素
 * （按这块瓦片的缩放档换算），擦边的也算碰到。
 */
export function tileTouches(key: TileKey, rect: Rect, marginPx = DAMAGE_MARGIN_PX): boolean {
  const pxPerPt = levelPxPerPt(key.level);
  const left = rect.x * pxPerPt - marginPx;
  const top = rect.y * pxPerPt - marginPx;
  const right = (rect.x + rect.width) * pxPerPt + marginPx;
  const bottom = (rect.y + rect.height) * pxPerPt + marginPx;
  const x0 = key.tx * TILE_SIZE;
  const y0 = key.ty * TILE_SIZE;
  return right > x0 && left < x0 + TILE_SIZE && bottom > y0 && top < y0 + TILE_SIZE;
}

/** 与页面局部矩形（pt）相交的瓦片索引范围，按行优先返回。 */
export function tilesInPageRect(
  grid: TileGrid,
  rect: Rect,
  page: number,
  level: number,
): TileKey[] {
  const x0 = Math.max(0, Math.floor((rect.x * grid.pxPerPt) / TILE_SIZE));
  const y0 = Math.max(0, Math.floor((rect.y * grid.pxPerPt) / TILE_SIZE));
  const x1 = Math.min(
    grid.cols - 1,
    Math.ceil(((rect.x + rect.width) * grid.pxPerPt) / TILE_SIZE) - 1,
  );
  const y1 = Math.min(
    grid.rows - 1,
    Math.ceil(((rect.y + rect.height) * grid.pxPerPt) / TILE_SIZE) - 1,
  );
  const keys: TileKey[] = [];
  for (let ty = y0; ty <= y1; ty++) {
    for (let tx = x0; tx <= x1; tx++) {
      keys.push({ page, level, tx, ty });
    }
  }
  return keys;
}

/** 瓦片位图的字节数（RGBA） */
export function tileBytes(width: number, height: number): number {
  return width * height * 4;
}
