import { intersectRect } from '../geometry';
import type { Rect, Size } from '../geometry';
import type { TileRequestSpec } from './renderPlan';
import { isAbortError, releasePixels } from './tileProvider';
import type { TileLayer, TilePixels, TileProvider, TileRequest } from './tileProvider';
import {
  TILE_SIZE,
  pageTileGrid,
  tileKeyString,
  tilePageRect,
  tilePixelRect,
  tilesInPageRect,
} from './tiles';

/**
 * 拖动预览的两层瓦片（方案 3.2）。
 *
 * 拖动开始时把页面分两层各渲染一次：去掉被拖对象的底图，和只有被拖对象的透明图层。两层的第一批瓦片
 * 都到了（`ready`）之后，画布每一帧只是把对象层挪到新的位置叠在底图上，不再向引擎发任何请求，帧时间
 * 和页面有多复杂没有关系。到齐之前画布照旧画虚线框。
 *
 * 这个类只管“要瓦片、记住瓦片”，不碰 WebGL：像素怎么变成纹理由 `upload` 决定，画在哪里由画布控制器算。
 */
export interface LayerTile {
  readonly id: string;
  /** 瓦片覆盖的页面区域（页面坐标，pt） */
  readonly pageRect: Rect;
  /** `upload` 给出的纹理；像素还没到时是 null */
  texture: unknown;
}

export interface DragLayersOptions {
  readonly provider: TileProvider;
  /** 被拖的对象所在的页 */
  readonly page: number;
  /** 瓦片的缩放档，和拖动开始时画布用的一致 */
  readonly level: number;
  /** 被拖的对象 */
  readonly objects: readonly string[];
  readonly documentVersion: number;
  /** 把像素变成能画的东西（纹理）。画不了时返回 null */
  readonly upload: (pixels: TilePixels, width: number, height: number) => unknown;
  /** 释放 `upload` 给出的东西 */
  readonly release: (texture: unknown) => void;
  /** 有瓦片到了，或者状态变了：请重画一帧 */
  readonly onChange: () => void;
  /** 计时用的时钟，测试里换掉。默认 `performance.now` */
  readonly now?: () => number;
}

export class DragLayers {
  readonly page: number;
  readonly level: number;
  readonly objects: readonly string[];

  private readonly options: DragLayersOptions;
  private readonly baseTiles = new Map<string, LayerTile>();
  private readonly objectTiles = new Map<string, LayerTile>();
  private readonly abort = new AbortController();
  private readonly startedAt: number;
  private pending = 0;
  private elapsed: number | null = null;
  private broken = false;
  private disposed = false;

  constructor(options: DragLayersOptions) {
    this.options = options;
    this.page = options.page;
    this.level = options.level;
    this.objects = options.objects;
    this.startedAt = this.now();
  }

  /** 两层的第一批瓦片都到了，可以拿来画。之后补要的瓦片（视口挪动后新露出来的部分）到一块画一块。 */
  get ready(): boolean {
    return this.elapsed !== null && !this.broken && !this.disposed;
  }

  /** 有瓦片没能渲染出来：这一次拖动不能用分层预览了，画布退回虚线框。 */
  get failed(): boolean {
    return this.broken;
  }

  /** 从开始到第一批瓦片到齐用了多久（ms）；还没到齐时是 null。 */
  get readyMs(): number | null {
    return this.elapsed;
  }

  /** 两层各要了几块瓦片 */
  get counts(): { readonly base: number; readonly objects: number } {
    return { base: this.baseTiles.size, objects: this.objectTiles.size };
  }

  /** 要这些位置上的底图瓦片。已经要过的不重复要。 */
  requestBase(specs: readonly TileRequestSpec[]): void {
    this.request(this.baseTiles, 'without', specs);
  }

  /** 要这些位置上的对象层瓦片。已经要过的不重复要。 */
  requestObjects(specs: readonly TileRequestSpec[]): void {
    this.request(this.objectTiles, 'only', specs);
  }

  /** 已经到了的底图瓦片 */
  base(): LayerTile[] {
    return arrived(this.baseTiles);
  }

  /** 已经到了的对象层瓦片 */
  objectLayer(): LayerTile[] {
    return arrived(this.objectTiles);
  }

  /** 不再需要：在途的请求取消，纹理释放。 */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.abort.abort();
    for (const tile of [...this.baseTiles.values(), ...this.objectTiles.values()]) {
      if (tile.texture !== null) this.options.release(tile.texture);
      tile.texture = null;
    }
    this.baseTiles.clear();
    this.objectTiles.clear();
  }

  private request(
    into: Map<string, LayerTile>,
    mode: TileLayer['mode'],
    specs: readonly TileRequestSpec[],
  ): void {
    if (this.disposed || this.broken) return;
    for (const spec of specs) {
      if (into.has(spec.id)) continue;
      const tile: LayerTile = { id: spec.id, pageRect: spec.pageRect, texture: null };
      into.set(spec.id, tile);
      this.pending++;
      const request: TileRequest = {
        page: spec.key.page,
        level: spec.key.level,
        pxPerPt: spec.pxPerPt,
        tx: spec.key.tx,
        ty: spec.key.ty,
        tileSize: TILE_SIZE,
        pixelRect: spec.pixelRect,
        pageRect: spec.pageRect,
        documentVersion: this.options.documentVersion,
        layer: { mode, objects: this.objects },
        signal: this.abort.signal,
      };
      this.options.provider.renderTile(request).then(
        (result) => {
          if (this.disposed) {
            releasePixels(result.pixels);
            return;
          }
          tile.texture = this.options.upload(result.pixels, result.width, result.height);
          releasePixels(result.pixels);
          this.settled();
        },
        (error: unknown) => {
          if (this.disposed) return;
          if (!isAbortError(error)) console.error('拖动预览的瓦片渲染失败', spec.id, error);
          // 缺一块，画出来的页面就是破的：整个不用
          this.broken = true;
          this.settled();
        },
      );
    }
  }

  private settled(): void {
    this.pending--;
    if (this.pending === 0 && this.elapsed === null && !this.broken) {
      this.elapsed = this.now() - this.startedAt;
    }
    this.options.onChange();
  }

  private now(): number {
    return (this.options.now ?? (() => performance.now()))();
  }
}

function arrived(tiles: ReadonlyMap<string, LayerTile>): LayerTile[] {
  const out: LayerTile[] = [];
  for (const tile of tiles.values()) {
    if (tile.texture !== null) out.push(tile);
  }
  return out;
}

/**
 * 一页在 `level` 档位下和 `localRect`（页面坐标，pt）相交的瓦片。
 * 矩形伸到页面外面的部分不算；完全在页面外面时返回空。
 */
export function layerTileSpecs(
  pageSize: Size,
  page: number,
  level: number,
  localRect: Rect,
): TileRequestSpec[] {
  const onPage = intersectRect(localRect, {
    x: 0,
    y: 0,
    width: pageSize.width,
    height: pageSize.height,
  });
  if (!onPage) return [];
  const grid = pageTileGrid(pageSize, level);
  return tilesInPageRect(grid, onPage, page, level).map((key) => ({
    id: tileKeyString(key),
    key,
    pxPerPt: grid.pxPerPt,
    pixelRect: tilePixelRect(grid, key.tx, key.ty),
    pageRect: tilePageRect(grid, key.tx, key.ty),
    priority: 0,
  }));
}
