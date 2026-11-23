import type { Point } from '../geometry';
import type { ComponentValues, PlaceableComponent } from './components';
import type { Matrix } from './pageSpace';
import type { ZOrderOp } from './zOrder';

/** 对一页上的若干对象做同一个变换。 */
export interface PageTransform {
  /** 页序号 */
  readonly page: number;
  readonly ids: readonly string[];
  /** 页面坐标（pt，左上角为原点，y 向下）里的仿射变换 */
  readonly matrix: Matrix;
}

/** 文字对象现在的文字（属性面板显示、修改）。 */
export interface TextInfo {
  readonly content: string;
  /** 字体名，去掉了子集前缀；读不出时为空 */
  readonly font: string;
  /** 字号（pt） */
  readonly size: number;
  readonly editable: boolean;
  /** 不能改时的原因，可以直接显示 */
  readonly reason?: string;
}

/** 对象画出来用的颜色，`#rrggbb`；没有填充（描边）时为 null。 */
export interface StyleInfo {
  readonly fill: string | null;
  readonly stroke: string | null;
}

/**
 * 一份文档的修改入口。
 *
 * 引擎打开的文档，修改由引擎执行（方案 3.1：界面只持有只读快照，所有修改都以命令发给引擎）：
 * 画布把工具产生的修改交给它，结果经快照的更新回到界面上。没有它的文档（模拟文档）直接改 store 里的快照。
 *
 * 这些方法都立即返回，不等引擎。做不成的时候（比如这一页只能叠加内容）由宿主告诉用户，界面恢复原状。
 */
export interface DocumentEditor {
  /** 变换对象。几页上都有选中的对象时每页一条，各页的矩阵都是那一页的页面坐标里的。 */
  transformObjects(changes: readonly PageTransform[]): void;
  deleteObjects(ids: readonly string[]): void;
  /** 置入组件，参数用默认值，左上角放在 `at`（页面坐标，pt）。放好之后选中它。 */
  placeComponent(page: number, kind: PlaceableComponent, at: Point): void;
  /** 改组件参数，只给要改的项。按新参数重新生成，左上角不动。 */
  updateComponent(id: string, values: Partial<ComponentValues>): void;
  /** 读文字对象的文字。不是文字对象（或者是文本组件）时为 null。这一个要等引擎。 */
  readText(id: string): Promise<TextInfo | null>;
  /** 把文字对象的文字换掉（方案 5.2 三级回退）。换了字体时由宿主在状态栏上说一声。 */
  setText(id: string, text: string): void;
  /** 读对象的颜色。这一个要等引擎。 */
  readStyle(id: string): Promise<StyleInfo>;
  /** 改填充色（`#rrggbb`），不透明度不动。 */
  setFill(ids: readonly string[], color: string): void;
  /**
   * 调整层级。页面不先改：引擎可能拒绝（对象分在几段内容流里时跨段挪不了），由宿主告诉用户原因；
   * 新的顺序随对象列表回来。
   */
  reorder(ids: readonly string[], op: ZOrderOp): void;
  undo(): void;
  redo(): void;
  /** 撤销或重做到历史记录的第 `index` 条之后（0 是打开时的样子），历史记录面板点了一条时用。 */
  jumpHistory(index: number): void;
}
