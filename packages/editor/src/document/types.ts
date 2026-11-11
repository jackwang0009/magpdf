import type { Rect } from '../geometry';

/**
 * 界面持有的文档只读快照（方案 3.1）：对象列表、包围盒、少量属性。
 * 模拟文档和引擎打开的文档共用这一套形状：引擎的对象摘要（`@magpdf/protocol` 的 `ObjectSummary`，
 * PDF 用户空间）由 `engine/engineSession.ts` 换算成这里的页面坐标。
 */

/** 对象种类（方案 4.1）。`other` 是引擎认不出种类的对象。 */
export type ObjectType = 'text' | 'path' | 'image' | 'shading' | 'form' | 'component' | 'other';

export type ComponentKind = 'barcode' | 'textStamp' | 'amountWords';

/** 模拟渲染用的外观，引擎接入后由位图代替，界面不再需要。 */
export type MockAppearance =
  | {
      readonly kind: 'textBars';
      readonly lines: number;
      readonly lineHeight: number;
      readonly barHeight: number;
      readonly color: string;
      /** 每行相对宽度 0–1，缺省为 1 */
      readonly widths?: readonly number[];
    }
  | {
      readonly kind: 'text';
      readonly text: string;
      readonly fontSize: number;
      readonly color: string;
      readonly bold?: boolean;
      readonly align?: 'left' | 'center' | 'right';
      readonly lineHeight?: number;
    }
  | {
      readonly kind: 'rect';
      readonly fill?: string;
      readonly stroke?: string;
      readonly strokeWidth?: number;
    }
  | {
      readonly kind: 'image';
      readonly color: string;
      readonly label?: string;
    }
  | {
      readonly kind: 'barcode';
      readonly content: string;
      /** 模块宽度（pt） */
      readonly moduleWidth: number;
      /** 条高（pt） */
      readonly barHeight: number;
      readonly showText: boolean;
    }
  | {
      readonly kind: 'stamp';
      readonly text: string;
      readonly color: string;
      /** 角度，度 */
      readonly angle: number;
    };

export interface PageObject {
  readonly id: string;
  readonly type: ObjectType;
  readonly componentKind?: ComponentKind;
  readonly name: string;
  /** 页面坐标，pt，左上角为原点，y 向下 */
  readonly bbox: Rect;
  readonly locked?: boolean;
  /**
   * 为 false 时在画布上点选、框选都跳过它：不可见文字、完全透明的对象、铺满页面的白色底块（方案 4.3）。
   * 这些对象仍然列在图层面板里，可以从那里选中。缺省为 true。
   */
  readonly pickable?: boolean;
  /** 模拟文档的外观。引擎打开的文档由引擎渲染的位图显示，没有这一项。 */
  readonly appearance?: MockAppearance;
}

export interface PageSnapshot {
  readonly index: number;
  /** pt */
  readonly width: number;
  /** pt */
  readonly height: number;
  /** 从底到顶的绘制顺序 */
  readonly objects: readonly PageObject[];
  /**
   * 页面画面的修订号：它变了，这一页的瓦片才需要重画。
   * 引擎打开的文档用它，因为换一个页面对象不等于画面变了（对象列表是后到的，修改先在界面上乐观更新）。
   * 模拟文档不给，页面对象换了就算画面变了。
   */
  readonly revision?: number;
}

export interface DocumentSnapshot {
  readonly id: string;
  readonly name: string;
  readonly dirty: boolean;
  /** 每次本地修改加 1；引擎接入后换成引擎推送的文档版本号 */
  readonly version: number;
  readonly pages: readonly PageSnapshot[];
}

/** A4 页面尺寸（pt） */
export const A4 = { width: 595.276, height: 841.89 } as const;
