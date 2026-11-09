import type { Rect } from '../geometry';

/**
 * 瓦片来源接口。画布只通过它取位图；V06 接入引擎时用共享内存实现同一接口替换 MockTileProvider。
 *
 * 约定：
 * - `pxPerPt` 是渲染分辨率（设备像素 / pt），等价于 dpi = pxPerPt × 72。
 * - 返回的位图尺寸必须等于 `pixelRect` 的宽高；边缘瓦片比 `tileSize` 小。
 * - 请求可能被 `signal` 取消（视图已经移走），实现应尽早放弃并以 AbortError 拒绝。
 * - 像素为不透明 RGBA；Uint8Array 形式按行主序、无行填充。
 */
export interface TileRequest {
  readonly page: number;
  /** 缩放档索引（见 tiles.ts 的 TILE_LEVELS） */
  readonly level: number;
  readonly pxPerPt: number;
  readonly tx: number;
  readonly ty: number;
  readonly tileSize: number;
  /** 瓦片在整页位图中的像素矩形 */
  readonly pixelRect: Rect;
  /** 瓦片覆盖的页面区域（页面坐标，pt） */
  readonly pageRect: Rect;
  /** 发出请求时的文档版本；结果回来时版本已变则丢弃 */
  readonly documentVersion: number;
  readonly signal: AbortSignal;
}

export interface RawPixels {
  readonly data: Uint8Array | Uint8ClampedArray;
  readonly width: number;
  readonly height: number;
}

export type TilePixels = ImageBitmap | RawPixels;

export interface TileResult {
  readonly width: number;
  readonly height: number;
  readonly pixels: TilePixels;
}

export interface TileProvider {
  renderTile(request: TileRequest): Promise<TileResult>;
  /**
   * 同时在途的请求上限。PDFium 在引擎里串行执行（方案 3.3），真实实现一般给 1–2，
   * 让排队发生在界面侧，这样视图移动后还能按新优先级重排；未提供时调度器用默认值。
   */
  readonly concurrency?: number;
  dispose?(): void;
}

export function isImageBitmap(pixels: TilePixels): pixels is ImageBitmap {
  return typeof ImageBitmap !== 'undefined' && pixels instanceof ImageBitmap;
}

export function releasePixels(pixels: TilePixels | null): void {
  if (pixels && isImageBitmap(pixels)) pixels.close();
}

export class AbortError extends Error {
  override readonly name = 'AbortError';

  constructor(message = '瓦片请求已取消') {
    super(message);
  }
}

export function isAbortError(error: unknown): boolean {
  return error instanceof Error && error.name === 'AbortError';
}
