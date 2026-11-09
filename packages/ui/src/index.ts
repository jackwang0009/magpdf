/**
 * 通用界面控件：数值输入、色块、面板、图标，以及方案 8.3 的视觉规范常量。
 * 控件由 V12 起逐步加入；颜色与尺寸的唯一出处是 tokens.ts。
 */

export { palette, metrics, fontFamily, monoFontFamily } from './tokens';
export type { PaletteKey, MetricKey } from './tokens';
export { themeCssVariables, applyTheme, cssVar } from './theme';
export { Icon, iconNames } from './icons';
export type { IconName, IconProps } from './icons';
export {
  Checkbox,
  Dropdown,
  Field,
  IconButton,
  PanelTabs,
  SectionHeader,
  VerticalSeparator,
} from './controls';
export type {
  CheckboxProps,
  DropdownProps,
  FieldProps,
  IconButtonProps,
  PanelTabsProps,
  SectionHeaderProps,
} from './controls';
