/**
 * 通用界面控件：数值输入、色块、面板、图标。控件由 V12 起逐步加入。
 * 下面的常量照抄方案 8.3 视觉规范，改规范时先改方案再改这里。
 */

/** 默认深色主题的颜色（方案 8.3）。 */
export const palette = {
  /** 外壳 */
  shell: '#2b2b2b',
  /** 面板 */
  panel: '#323232',
  /** 输入框 */
  input: '#1e1e1e',
  /** 画布外区域 */
  canvasBackdrop: '#1f1f1f',
  /** 主文字 */
  text: '#d6d6d6',
  /** 次文字 */
  textMuted: '#9a9a9a',
  /** 强调色：选中、焦点、选框 */
  accent: '#3d8ee6',
} as const;

export type PaletteKey = keyof typeof palette;

/** 密度与布局尺寸，单位 px（方案 8.3）。 */
export const metrics = {
  fontSize: 12,
  rowHeight: 24,
  panelPadding: 8,
  iconSize: 16,
  menuBarHeight: 28,
  optionsBarHeight: 36,
  documentTabsHeight: 28,
  rulerSize: 20,
  statusBarHeight: 22,
  toolbarWidth: 40,
  dockPanelWidth: 280,
  collapsedDockWidth: 32,
  componentPanelWidth: 240,
  componentGridCell: 72,
  /** 选框控制点：8 个白底方形 */
  handleCount: 8,
} as const;

/** 界面字体栈：系统 UI 字体 + 微软雅黑。 */
export const fontFamily = 'system-ui, "Segoe UI", "Microsoft YaHei", sans-serif';
