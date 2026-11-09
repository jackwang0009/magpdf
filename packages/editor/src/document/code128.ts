/**
 * Code 128 编码（模拟渲染用）。正式的条码由引擎的 Zint 生成（方案 11.5），
 * 这里只为了让模拟文档上的条码看起来真实，并与界面图的宽度一致。
 * 码表取自 docs/images/ui-main.html：值 0–105 各 11 个模块，106 为终止符（13 个模块）。
 */
const PATTERNS = [
  '212222',
  '222122',
  '222221',
  '121223',
  '121322',
  '131222',
  '122213',
  '122312',
  '132212',
  '221213',
  '221312',
  '231212',
  '112232',
  '122132',
  '122231',
  '113222',
  '123122',
  '123221',
  '223211',
  '221132',
  '221231',
  '213212',
  '223112',
  '312131',
  '311222',
  '321122',
  '321221',
  '312212',
  '322112',
  '322211',
  '212123',
  '212321',
  '232121',
  '111323',
  '131123',
  '131321',
  '112313',
  '132113',
  '132311',
  '211313',
  '231113',
  '231311',
  '112133',
  '112331',
  '132131',
  '113123',
  '113321',
  '133121',
  '313121',
  '211331',
  '231131',
  '213113',
  '213311',
  '213131',
  '311123',
  '311321',
  '331121',
  '312113',
  '312311',
  '332111',
  '314111',
  '221411',
  '431111',
  '111224',
  '111422',
  '121124',
  '121421',
  '141122',
  '141221',
  '112214',
  '112412',
  '122114',
  '122411',
  '142112',
  '142211',
  '241211',
  '221114',
  '413111',
  '241112',
  '134111',
  '111242',
  '121142',
  '121241',
  '114212',
  '124112',
  '124211',
  '411212',
  '421112',
  '421211',
  '212141',
  '214121',
  '412121',
  '111143',
  '111341',
  '131141',
  '114113',
  '114311',
  '411113',
  '411311',
  '113141',
  '114131',
  '311141',
  '411131',
  '211412',
  '211214',
  '211232',
  '2331112',
] as const;

const START_B = 104;
const START_C = 105;
const CODE_B = 100;
const CODE_C = 99;
const STOP = 106;

function isDigit(c: string | undefined): boolean {
  return c !== undefined && c >= '0' && c <= '9';
}

/** 把文本编成码值序列（含起始符、校验位、终止符），数字段自动切换到子集 C。 */
export function code128Values(text: string): number[] {
  const digitRun = (k: number): number => {
    let n = 0;
    while (isDigit(text[k + n])) n++;
    return n;
  };
  const values: number[] = [];
  let set: 'B' | 'C' = digitRun(0) >= 4 ? 'C' : 'B';
  values.push(set === 'C' ? START_C : START_B);
  let i = 0;
  while (i < text.length) {
    const run = digitRun(i);
    if (set === 'C') {
      if (run >= 2) {
        values.push(Number(text.slice(i, i + 2)));
        i += 2;
        continue;
      }
      values.push(CODE_B);
      set = 'B';
    }
    if (run >= 4 && run % 2 === 0) {
      values.push(CODE_C);
      set = 'C';
      continue;
    }
    values.push(text.charCodeAt(i) - 32);
    i++;
    if (run >= 5 && run % 2 === 1) {
      values.push(CODE_C);
      set = 'C';
    }
  }
  let sum = values[0] ?? 0;
  for (let k = 1; k < values.length; k++) sum += (values[k] ?? 0) * k;
  values.push(sum % 103, STOP);
  return values;
}

export interface BarcodeBars {
  /** 含两侧静区的总模块数 */
  readonly modules: number;
  /** 每个条的 [起始模块, 宽度（模块）] */
  readonly bars: readonly (readonly [number, number])[];
}

/** 条的位置（单位：模块），两侧各留 `quiet` 个模块的静区。 */
export function code128Bars(text: string, quiet = 10): BarcodeBars {
  const bars: (readonly [number, number])[] = [];
  let x = quiet;
  for (const value of code128Values(text)) {
    const pattern = PATTERNS[value];
    if (!pattern) throw new Error(`Code 128 码值越界：${value}`);
    for (let k = 0; k < pattern.length; k++) {
      const w = Number(pattern[k]);
      if (k % 2 === 0) bars.push([x, w]);
      x += w;
    }
  }
  return { modules: x + quiet, bars };
}
