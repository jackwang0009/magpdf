import type { Rect } from '../geometry';

/**
 * 瓦片来源接口。画布只通过它取位图；V06 接入引擎时用共享内存实现同一接口替换 MockTileProvider。
 *
 * 约定：
 * - `pxPerPt` 是渲染分辨率（设备像素 / pt），等价于 dpi = pxPerPt × 72。
 * - 返回的位图尺寸必须等于 `pixelRect` 的宽高；边缘瓦片比 `tileSize` 小。
 * - 请求可能被 `signal` 取消（视图已经移走），实现应尽早放弃并以 AbortError 拒绝。
 * - 像素为不透明 RGBA；Uint8Array 形式按行主序、无行填充。只有对象层（`layer.mode` 为 `only`）带透明度，
 *   颜色分量要预乘好。
 */

/**
 * 分层渲染里的一层（拖动预览，方案 3.2）：拖动开始时把页面分成“去掉被拖对象的底图”和“只有被拖对象的
 * 透明图层”，拖动过程中界面只挪动后者。
 */
export interface TileLayer {
  /** `without`：去掉这些对象的底图；`only`：只有这些对象，别处全透明 */
  readonly mode: 'without' | 'only';
  /** 对象 id（文档快照里的），都在这一页上 */
  readonly objects: readonly string[];
}

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
  /** 只画页面的一层；省略时画整页。只有 `layers` 为 true 的来源才会收到带它的请求。 */
  readonly layer?: TileLayer;
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
  /** 能不能按层渲染（`TileRequest.layer`）。不能的话画布不做分层的拖动预览，拖动时只画虚线框。 */
  readonly layers?: boolean;
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
