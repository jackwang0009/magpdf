/**
 * 标尺刻度计算（纯函数）：单位毫米，原点在当前页左上角。
 * 返回屏幕位置（CSS px）与标签；绘制在 overlay.ts。
 */
export interface RulerTicks {
  /** 主刻度（带数字） */
  readonly major: readonly { readonly pos: number; readonly label: string }[];
  /** 次刻度 */
  readonly minor: readonly number[];
  /** 主刻度间隔（mm） */
  readonly stepMm: number;
}

const STEPS_MM: readonly number[] = [1, 2, 5, 10, 20, 50, 100, 200, 500];

/** 选择主刻度间隔：两条主刻度至少隔 minMajorPx 像素 */
export function pickRulerStep(pxPerMm: number, minMajorPx = 48): number {
  for (const step of STEPS_MM) {
    if (step * pxPerMm >= minMajorPx) return step;
  }
  return STEPS_MM[STEPS_MM.length - 1]!;
}

/**
 * @param originPx 原点（页左上角）在屏幕上的位置（沿标尺方向）
 * @param lengthPx 标尺可见长度
 * @param pxPerMm 当前缩放下 1 mm 的像素数
 */
export function rulerTicks(originPx: number, lengthPx: number, pxPerMm: number): RulerTicks {
  const stepMm = pickRulerStep(pxPerMm);
  const minorPerMajor = stepMm === 1 || stepMm === 2 ? 2 : stepMm === 5 ? 5 : 10;
  const minorMm = stepMm / minorPerMajor;
  const major: { pos: number; label: string }[] = [];
  const minor: number[] = [];
  const firstMm = Math.floor(-originPx / pxPerMm / minorMm) * minorMm;
  const lastMm = (lengthPx - originPx) / pxPerMm;
  for (let mm = firstMm; mm <= lastMm; mm += minorMm) {
    const pos = originPx + mm * pxPerMm;
    if (pos < -1 || pos > lengthPx + 1) continue;
    // 浮点累加误差：用四舍五入后的值判断是否主刻度
    const rounded = Math.round(mm / minorMm) * minorMm;
    if (Math.abs(rounded / stepMm - Math.round(rounded / stepMm)) < 1e-6) {
      major.push({ pos, label: String(Math.round(rounded)) });
    } else {
      minor.push(pos);
    }
  }
  return { major, minor, stepMm };
}
