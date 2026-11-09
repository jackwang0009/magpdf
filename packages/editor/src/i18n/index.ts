import { zhCN } from './zh-CN';
import type { Strings } from './zh-CN';

export type { Strings } from './zh-CN';

/** 当前语言的全部界面文字。v1.0 只有简体中文；以后按用户设置在这里切换。 */
export const t: Strings = zhCN;

/** 代入 `{name}` 形式的参数：`format('第 {n} 页', { n: 3 })` → `第 3 页`。 */
export function format(
  template: string,
  params: Record<string, string | number | undefined>,
): string {
  return template.replace(/\{(\w+)\}/g, (match, key: string) => {
    const value = params[key];
    return value === undefined ? match : String(value);
  });
}
