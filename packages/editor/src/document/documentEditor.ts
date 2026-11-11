import type { Matrix } from './pageSpace';

/** 对一页上的若干对象做同一个变换。 */
export interface PageTransform {
  /** 页序号 */
  readonly page: number;
  readonly ids: readonly string[];
  /** 页面坐标（pt，左上角为原点，y 向下）里的仿射变换 */
  readonly matrix: Matrix;
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
  undo(): void;
  redo(): void;
}
