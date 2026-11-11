import { describe, expect, it } from 'vitest';
import type { Rect } from '../geometry';
import {
  IDENTITY_MATRIX,
  applyMatrix,
  composeMatrix,
  displayMatrix,
  documentTransformToPage,
  invertMatrix,
  pageTransformToUser,
  rectToRectMatrix,
  transformRect,
  translationMatrix,
  userBoxToPageRect,
} from './pageSpace';
import type { Matrix } from './pageSpace';

function expectRect(actual: Rect, expected: readonly [number, number, number, number]): void {
  const [x0, y0, x1, y1] = expected;
  expect(actual.x).toBeCloseTo(x0, 2);
  expect(actual.y).toBeCloseTo(y0, 2);
  expect(actual.x + actual.width).toBeCloseTo(x1, 2);
  expect(actual.y + actual.height).toBeCloseTo(y1, 2);
}

function expectMatrix(actual: Matrix | null, expected: Matrix): void {
  expect(actual).not.toBeNull();
  actual!.forEach((value, i) => expect(value).toBeCloseTo(expected[i]!, 6));
}

describe('用户空间到页面坐标', () => {
  /**
   * 对照数据来自引擎那边的测试 `engine/pdfium-sys/tests/page_space.rs`：它把页面上的对象逐个渲染出来，
   * 核对过墨迹确实落在这些范围里。左列是对象在 PDF 用户空间里的包围盒，右列是页面坐标里的 [左, 上, 右, 下]。
   */
  const cases: readonly {
    readonly name: string;
    readonly rotate: number;
    readonly cropBox: readonly [number, number, number, number];
    readonly size: readonly [number, number];
    readonly user: readonly [number, number, number, number];
    readonly page: readonly [number, number, number, number];
  }[] = [
    {
      name: '旋转 90°',
      rotate: 90,
      cropBox: [0, 0, 300, 200],
      size: [200, 300],
      user: [130, 120, 170, 190],
      page: [120, 130, 190, 170],
    },
    {
      name: '旋转 90°，文字',
      rotate: 90,
      cropBox: [0, 0, 300, 200],
      size: [200, 300],
      user: [20, 17.624, 220.508, 28.736],
      page: [17.62, 20, 28.74, 220.51],
    },
    {
      name: '旋转 180°',
      rotate: 180,
      cropBox: [0, 0, 300, 200],
      size: [300, 200],
      user: [20, 17.624, 227.096, 28.748],
      page: [72.9, 17.62, 280, 28.75],
    },
    {
      name: '旋转 270°',
      rotate: 270,
      cropBox: [0, 0, 300, 200],
      size: [200, 300],
      user: [130, 120, 170, 190],
      page: [10, 130, 80, 170],
    },
    {
      name: 'MediaBox 不从原点开始',
      rotate: 0,
      cropBox: [100, 100, 400, 300],
      size: [300, 200],
      user: [112.666, 110.218, 226.489, 118.471],
      page: [12.67, 181.53, 126.49, 189.78],
    },
    {
      name: 'CropBox 比 MediaBox 小',
      rotate: 0,
      cropBox: [50, 50, 250, 150],
      size: [200, 100],
      user: [62.45, 60.218, 161.477, 68.552],
      page: [12.45, 81.45, 111.48, 89.78],
    },
    {
      name: '负坐标',
      rotate: 0,
      cropBox: [-150, -100, 150, 100],
      size: [300, 200],
      user: [-137.334, -89.782, -17.517, -81.529],
      page: [12.67, 181.53, 132.48, 189.78],
    },
  ];

  it.each(cases)('$name：对象落在渲染出来的位置上', ({ rotate, cropBox, user, page }) => {
    expectRect(userBoxToPageRect(displayMatrix({ rotate, cropBox }), user), page);
  });

  it.each(cases)('$name：可见区域正好铺满页面', ({ rotate, cropBox, size }) => {
    expectRect(userBoxToPageRect(displayMatrix({ rotate, cropBox }), cropBox), [0, 0, ...size]);
  });

  it('旋转写成 360、-90 这样的值也认', () => {
    const cropBox = [0, 0, 300, 200] as const;
    expect(displayMatrix({ rotate: 360, cropBox })).toEqual(displayMatrix({ rotate: 0, cropBox }));
    expect(displayMatrix({ rotate: -90, cropBox })).toEqual(
      displayMatrix({ rotate: 270, cropBox }),
    );
  });
});

describe('页面坐标里的变换换成用户空间里的', () => {
  const cropBox = [100, 100, 400, 300] as const;

  it('向右下拖动：不旋转的页上是 x 加、y 减', () => {
    const display = displayMatrix({ rotate: 0, cropBox });
    expectMatrix(pageTransformToUser(display, translationMatrix(10, 4)), [1, 0, 0, 1, 10, -4]);
  });

  it('向右下拖动：旋转过的页上方向跟着转', () => {
    // 页面坐标 (x, y) 对应用户空间：90° 是 (y + l, x + b)，180° 是 (r − x, y + b)，270° 是 (r − y, t − x)
    const at = (rotate: number) =>
      pageTransformToUser(displayMatrix({ rotate, cropBox }), translationMatrix(10, 4));
    expectMatrix(at(90), [1, 0, 0, 1, 4, 10]);
    expectMatrix(at(180), [1, 0, 0, 1, -10, 4]);
    expectMatrix(at(270), [1, 0, 0, 1, -4, -10]);
  });

  it.each([0, 90, 180, 270])('旋转 %i°：换过去的变换，效果和在页面上直接变换一样', (rotate) => {
    const display = displayMatrix({ rotate, cropBox });
    // 以页面上的 (20, 30) 为锚点，横向放大 2 倍、纵向缩到一半，再平移
    const onPage = rectToRectMatrix(
      { x: 20, y: 30, width: 40, height: 20 },
      { x: 25, y: 28, width: 80, height: 10 },
    );
    const inUser = pageTransformToUser(display, onPage)!;
    for (const [x, y] of [
      [120, 130],
      [333, 250],
      [100, 300],
    ] as const) {
      const moved = applyMatrix(inUser, x, y);
      const viaUser = applyMatrix(display, moved.x, moved.y);
      const before = applyMatrix(display, x, y);
      const viaPage = applyMatrix(onPage, before.x, before.y);
      expect(viaUser.x).toBeCloseTo(viaPage.x, 6);
      expect(viaUser.y).toBeCloseTo(viaPage.y, 6);
    }
  });
});

describe('文档坐标里的变换换成页面坐标里的', () => {
  it('平移在哪一页上都一样', () => {
    expect(documentTransformToPage(translationMatrix(7, -3), { x: 24, y: 500 })).toEqual([
      1, 0, 0, 1, 7, -3,
    ]);
  });

  it('缩放的锚点跟着页面的位置换算', () => {
    // 文档里以 (74, 640) 为锚点缩到一半；这一页的左上角在 (24, 500)，锚点在页面上是 (50, 140)
    const inDocument = rectToRectMatrix(
      { x: 74, y: 640, width: 200, height: 200 },
      { x: 74, y: 640, width: 100, height: 100 },
    );
    const onPage = documentTransformToPage(inDocument, { x: 24, y: 500 });
    expect(applyMatrix(onPage, 50, 140)).toEqual({ x: 50, y: 140 });
    expect(applyMatrix(onPage, 250, 340)).toEqual({ x: 150, y: 240 });
  });
});

describe('矩阵运算', () => {
  it('复合是先做第一个再做第二个', () => {
    const scaleThenMove = composeMatrix([2, 0, 0, 3, 0, 0], translationMatrix(10, 20));
    expect(applyMatrix(scaleThenMove, 1, 1)).toEqual({ x: 12, y: 23 });
    const moveThenScale = composeMatrix(translationMatrix(10, 20), [2, 0, 0, 3, 0, 0]);
    expect(applyMatrix(moveThenScale, 1, 1)).toEqual({ x: 22, y: 63 });
    expect(composeMatrix(IDENTITY_MATRIX, scaleThenMove)).toEqual(scaleThenMove);
  });

  it('逆矩阵把点变回去；压扁了的矩阵没有逆', () => {
    const m: Matrix = [0, 2, -3, 0, 7, -5];
    const p = applyMatrix(m, 4, 9);
    const back = applyMatrix(invertMatrix(m)!, p.x, p.y);
    expect(back.x).toBeCloseTo(4, 9);
    expect(back.y).toBeCloseTo(9, 9);
    expect(invertMatrix([1, 2, 2, 4, 0, 0])).toBeNull();
    expect(invertMatrix([0, 0, 0, 0, 5, 5])).toBeNull();
  });

  it('矩形变换后取四个角的包围盒', () => {
    // 逆时针转 90°：(x, y) → (−y, x)
    expectRect(
      transformRect([0, 1, -1, 0, 0, 0], { x: 10, y: 20, width: 30, height: 5 }),
      [-25, 10, -20, 40],
    );
  });

  it('矩形到矩形：没有宽度的线只平移，不缩放', () => {
    const from = { x: 10, y: 10, width: 0, height: 40 };
    const to = { x: 30, y: 20, width: 0, height: 80 };
    expect(rectToRectMatrix(from, to)).toEqual([1, 0, 0, 2, 20, 0]);
    expectRect(transformRect(rectToRectMatrix(from, to), from), [30, 20, 30, 100]);
  });
});
