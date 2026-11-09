import type { Point, Rect, Size } from '../geometry';

/**
 * 连续页面的纵向排列（文档坐标，pt）。
 * 页面顶边用前缀和保存，可见页用二分查找，300 页以上也不必每帧遍历全部页。
 */
export interface PageLayoutOptions {
  /** 页间距（pt） */
  readonly gap?: number;
  /** 文档四周留白（pt），用于“适合页面”时页面不贴边 */
  readonly margin?: number;
}

export const DEFAULT_PAGE_GAP = 12;
export const DEFAULT_PAGE_MARGIN = 24;

export class PageLayout {
  readonly pageCount: number;
  readonly gap: number;
  readonly margin: number;
  /** 最宽页面的宽度（pt） */
  readonly maxWidth: number;
  /** 全部页面加间距与留白的总高度（pt） */
  readonly totalHeight: number;
  /** 文档总宽（含留白） */
  readonly totalWidth: number;

  private readonly tops: Float64Array;
  private readonly widths: Float64Array;
  private readonly heights: Float64Array;

  constructor(pages: readonly Size[], options: PageLayoutOptions = {}) {
    this.gap = options.gap ?? DEFAULT_PAGE_GAP;
    this.margin = options.margin ?? DEFAULT_PAGE_MARGIN;
    this.pageCount = pages.length;
    this.tops = new Float64Array(pages.length);
    this.widths = new Float64Array(pages.length);
    this.heights = new Float64Array(pages.length);
    let y = this.margin;
    let maxWidth = 0;
    for (let i = 0; i < pages.length; i++) {
      const page = pages[i]!;
      this.tops[i] = y;
      this.widths[i] = page.width;
      this.heights[i] = page.height;
      maxWidth = Math.max(maxWidth, page.width);
      y += page.height + (i < pages.length - 1 ? this.gap : 0);
    }
    this.maxWidth = maxWidth;
    this.totalWidth = maxWidth + this.margin * 2;
    this.totalHeight = (pages.length > 0 ? y : this.margin) + this.margin;
  }

  /** 页面在文档坐标中的矩形；页面水平居中。 */
  pageRect(index: number): Rect {
    const width = this.widths[index];
    const height = this.heights[index];
    const top = this.tops[index];
    if (width === undefined || height === undefined || top === undefined) {
      throw new RangeError(`页码越界：${index}`);
    }
    return { x: this.margin + (this.maxWidth - width) / 2, y: top, width, height };
  }

  pageSize(index: number): Size {
    const width = this.widths[index];
    const height = this.heights[index];
    if (width === undefined || height === undefined) throw new RangeError(`页码越界：${index}`);
    return { width, height };
  }

  /**
   * 与 [yMin, yMax) 相交的页面范围 [first, lastExclusive)。O(log n)。
   * 没有相交页面时返回 first === lastExclusive。
   */
  visiblePages(yMin: number, yMax: number): readonly [number, number] {
    if (this.pageCount === 0 || yMax <= yMin) return [0, 0];
    // 第一个底边 > yMin 的页
    let lo = 0;
    let hi = this.pageCount;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (this.tops[mid]! + this.heights[mid]! > yMin) hi = mid;
      else lo = mid + 1;
    }
    const first = lo;
    // 第一个顶边 >= yMax 的页
    hi = this.pageCount;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (this.tops[mid]! >= yMax) hi = mid;
      else lo = mid + 1;
    }
    return [first, Math.max(first, lo)];
  }

  /** 包含 y 的页（落在间距里时取最近的下一页；超出范围时夹到首尾页）。 */
  pageAtY(y: number): number {
    if (this.pageCount === 0) return -1;
    let lo = 0;
    let hi = this.pageCount - 1;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (this.tops[mid]! + this.heights[mid]! + this.gap <= y) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  }

  /** 文档坐标点落在哪一页上（页面矩形内），不在任何页上返回 null。 */
  pageAtPoint(point: Point): { readonly index: number; readonly local: Point } | null {
    const index = this.pageAtY(point.y);
    if (index < 0) return null;
    const r = this.pageRect(index);
    if (point.x < r.x || point.x > r.x + r.width || point.y < r.y || point.y > r.y + r.height) {
      return null;
    }
    return { index, local: { x: point.x - r.x, y: point.y - r.y } };
  }

  /** 在可见范围 [yMin, yMax) 内占据面积最大的页，用作“当前页”。 */
  dominantPage(yMin: number, yMax: number): number {
    const [first, last] = this.visiblePages(yMin, yMax);
    if (first >= last) return this.pageCount === 0 ? -1 : this.pageAtY((yMin + yMax) / 2);
    let best = first;
    let bestOverlap = -1;
    for (let i = first; i < last; i++) {
      const top = this.tops[i]!;
      const bottom = top + this.heights[i]!;
      const overlap = Math.min(bottom, yMax) - Math.max(top, yMin);
      if (overlap > bestOverlap) {
        bestOverlap = overlap;
        best = i;
      }
    }
    return best;
  }
}
