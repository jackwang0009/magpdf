/**
 * 视觉规范常量，照抄方案 8.3；改规范时先改方案再改这里。
 * 其他包里的样式只能引用这里的值（直接 import，或通过 theme.ts 生成的 CSS 变量）。
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

  // 以下是界面图源文件（docs/images/ui-main.html）里用到、但方案 8.3 的表还没有列出的派生色。
  // V12 报告建议把它们补进 8.3；在那之前这里是唯一出处，CSS 里不得再写字面量。
  /** 区块之间的分隔线（与输入框同色） */
  separator: '#1e1e1e',
  /** 控件边框、面板内的细线 */
  line: '#3e3e3e',
  /** 工具栏内的分隔线 */
  toolbarSeparator: '#474747',
  /** 悬停或按下的图标按钮背景 */
  controlActive: '#454545',
  /** 当前工具的背景 */
  toolActive: '#4a4a4a',
  /** 当前文档标签页的背景 */
  tabActive: '#3a3a3a',
  /** 组件面板网格单元的背景 */
  cellBackground: '#2a2a2a',
  /** 组件面板网格单元的边框 */
  cellBorder: '#3a3a3a',
  /** 标尺刻度 */
  rulerTick: '#686868',
  /** 校验通过 */
  success: '#6cc070',
  /** 选框控制点的底色 */
  handleFill: '#ffffff',
  /** 页面周围的描边（画布上） */
  pageOutline: 'rgba(0, 0, 0, 0.4)',
} as const;

export type PaletteKey = keyof typeof palette;

/** 密度与布局尺寸，单位 px（方案 8.3）。 */
export const metrics = {
  fontSize: 12,
  /** 状态栏、角标等次要文字 */
  fontSizeSmall: 11,
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
  /** 组件面板可拖动的宽度范围（《交互设计》5.1） */
  componentPanelMinWidth: 200,
  componentPanelMaxWidth: 400,
  componentGridCell: 72,
  /** 选框控制点：8 个白底方形 */
  handleCount: 8,
  /** 控制点边长 */
  handleSize: 7,
  /** 选框线宽 */
  selectionStroke: 1,
} as const;

export type MetricKey = keyof typeof metrics;

/** 界面字体栈：系统 UI 字体 + 微软雅黑。 */
export const fontFamily = 'system-ui, "Segoe UI", "Microsoft YaHei", sans-serif';

/** 等宽字体：条码内容等需要对齐的文字。 */
export const monoFontFamily = 'Consolas, "Segoe UI", monospace';
