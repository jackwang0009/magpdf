import type { Point, Rect } from '../geometry';

/**
 * 坐标换算：PDF 用户空间 ↔ 界面的页面坐标。
 *
 * 引擎给的几何量都在 PDF 用户空间里（单位 pt，y 向上，未经页面旋转，《接口协议》3.1）；界面的页面坐标
 * 是页面显示出来的样子（左上角为原点，y 向下，单位 pt）。页面的 `/Rotate` 和可见区域不从原点开始，
 * 都由界面在这里换算。
 */

/**
 * 2×3 仿射矩阵 `[a, b, c, d, e, f]`，和 PDF 的 `cm` 操作数、《接口协议》里的矩阵同一个顺序：
 * 点 `(x, y)` 变成 `(a·x + c·y + e, b·x + d·y + f)`。
 */
export type Matrix = readonly [number, number, number, number, number, number];

export const IDENTITY_MATRIX: Matrix = [1, 0, 0, 1, 0, 0];

export function translationMatrix(dx: number, dy: number): Matrix {
  return [1, 0, 0, 1, dx, dy];
}

export function applyMatrix(m: Matrix, x: number, y: number): Point {
  return { x: m[0] * x + m[2] * y + m[4], y: m[1] * x + m[3] * y + m[5] };
}

/** 先做 `first`，再做 `then`。 */
export function composeMatrix(first: Matrix, then: Matrix): Matrix {
  const [a1, b1, c1, d1, e1, f1] = first;
  const [a2, b2, c2, d2, e2, f2] = then;
  return [
    a2 * a1 + c2 * b1,
    b2 * a1 + d2 * b1,
    a2 * c1 + c2 * d1,
    b2 * c1 + d2 * d1,
    a2 * e1 + c2 * f1 + e2,
    b2 * e1 + d2 * f1 + f2,
  ];
}

/** 逆矩阵；不可逆（把平面压成一条线或一个点）时返回 null。 */
export function invertMatrix(m: Matrix): Matrix | null {
  const [a, b, c, d, e, f] = m;
  const det = a * d - b * c;
  if (!Number.isFinite(det) || Math.abs(det) < 1e-12) return null;
  return [d / det, -b / det, -c / det, a / det, (c * f - d * e) / det, (b * e - a * f) / det];
}

/** 矩形四个角变换之后的包围盒。 */
export function transformRect(m: Matrix, r: Rect): Rect {
  const corners = [
    applyMatrix(m, r.x, r.y),
    applyMatrix(m, r.x + r.width, r.y),
    applyMatrix(m, r.x, r.y + r.height),
    applyMatrix(m, r.x + r.width, r.y + r.height),
  ];
  const xs = corners.map((p) => p.x);
  const ys = corners.map((p) => p.y);
  const x = Math.min(...xs);
  const y = Math.min(...ys);
  return { x, y, width: Math.max(...xs) - x, height: Math.max(...ys) - y };
}

/**
 * 把矩形 `from` 映到矩形 `to` 的缩放加平移（拖控制点缩放选区时用）。
 * `from` 某个方向的尺寸是 0 时（一条横线或竖线），那个方向不缩放，只平移。
 */
export function rectToRectMatrix(from: Rect, to: Rect): Matrix {
  const sx = from.width === 0 ? 1 : to.width / from.width;
  const sy = from.height === 0 ? 1 : to.height / from.height;
  return [sx, 0, 0, sy, to.x - from.x * sx, to.y - from.y * sy];
}

/**
 * 文档坐标（各页从上到下排开的那个坐标系）里的一个变换，换成某一页的页面坐标里效果相同的变换。
 * 两个坐标系只差一个平移：`origin` 是这一页的左上角在文档坐标里的位置。
 */
export function documentTransformToPage(transform: Matrix, origin: Point): Matrix {
  // 页面坐标的点先挪到文档坐标，做变换，再挪回来
  return composeMatrix(
    composeMatrix(translationMatrix(origin.x, origin.y), transform),
    translationMatrix(-origin.x, -origin.y),
  );
}

/** 页面在 PDF 用户空间里的摆法（`@magpdf/protocol` 的 `PageInfo` 里的两项）。 */
export interface PageSpace {
  /** 可见区域 `[left, bottom, right, top]`：CropBox 与 MediaBox 的交集 */
  readonly cropBox: readonly [number, number, number, number];
  /** 页面的 `/Rotate`：0、90、180、270 */
  readonly rotate: number;
}

/**
 * PDF 用户空间 → 页面坐标的矩阵。
 *
 * 和 PDFium 渲染时的换算一致；引擎那边是 `pdfium-sys` 的 `PageGeometry::display_matrix`，
 * 那里有拿渲染结果核对这张表的测试（`tests/page_space.rs`），两边要一起改。
 * 记可见区域为 `[l, b, r, t]`：
 *
 * | `/Rotate` | 页面坐标 |
 * |---|---|
 * | 0 | `(x − l, t − y)` |
 * | 90 | `(y − b, x − l)` |
 * | 180 | `(r − x, y − b)` |
 * | 270 | `(t − y, r − x)` |
 */
export function displayMatrix(space: PageSpace): Matrix {
  const [l, b, r, t] = space.cropBox;
  switch ((((space.rotate / 90) % 4) + 4) % 4) {
    case 0:
      return [1, 0, 0, -1, -l, t];
    case 1:
      return [0, 1, 1, 0, -b, -l];
    case 2:
      return [-1, 0, 0, 1, r, -b];
    default:
      return [0, -1, -1, 0, t, r];
  }
}

/** PDF 用户空间里的包围盒 `[left, bottom, right, top]` 换算成页面坐标里的矩形。 */
export function userBoxToPageRect(
  display: Matrix,
  box: readonly [number, number, number, number],
): Rect {
  const p = applyMatrix(display, box[0], box[1]);
  const q = applyMatrix(display, box[2], box[3]);
  const x = Math.min(p.x, q.x);
  const y = Math.min(p.y, q.y);
  return { x, y, width: Math.abs(p.x - q.x), height: Math.abs(p.y - q.y) };
}

/**
 * 页面坐标里的一个变换，换成 PDF 用户空间里效果相同的变换（发给 `edit.transform` 的矩阵）。
 * `display` 是这一页的 {@link displayMatrix}；它不可逆时返回 null（正常的页面不会）。
 */
export function pageTransformToUser(display: Matrix, transform: Matrix): Matrix | null {
  const back = invertMatrix(display);
  if (!back) return null;
  // 用户空间的点先换到页面坐标，做变换，再换回用户空间
  return composeMatrix(composeMatrix(display, transform), back);
}
