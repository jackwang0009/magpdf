import { fontFamily, metrics, monoFontFamily, palette } from './tokens';
import type { MetricKey, PaletteKey } from './tokens';

/**
 * 把 palette/metrics 变成 CSS 自定义属性，样式表只引用这些变量，不另写颜色和尺寸字面量。
 * 变量名：颜色 `--mg-<paletteKey>`，尺寸 `--mg-<metricKey>`（带 px 单位），均为 kebab-case。
 */
export function themeCssVariables(): Record<string, string> {
  const vars: Record<string, string> = {};
  for (const [key, value] of Object.entries(palette)) {
    vars[`--mg-${kebab(key)}`] = value;
  }
  for (const [key, value] of Object.entries(metrics)) {
    vars[`--mg-${kebab(key)}`] = `${value}px`;
  }
  vars['--mg-font-family'] = fontFamily;
  vars['--mg-mono-font-family'] = monoFontFamily;
  return vars;
}

/** 在元素上写入主题变量；通常用于应用根节点。返回清理函数。 */
export function applyTheme(element: HTMLElement): () => void {
  const vars = themeCssVariables();
  for (const [name, value] of Object.entries(vars)) {
    element.style.setProperty(name, value);
  }
  return () => {
    for (const name of Object.keys(vars)) {
      element.style.removeProperty(name);
    }
  };
}

/** 在行内样式里引用主题变量：`cssVar('accent')` → `var(--mg-accent)`。 */
export function cssVar(key: PaletteKey | MetricKey): string {
  return `var(--mg-${kebab(key)})`;
}

function kebab(name: string): string {
  return name.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`);
}
