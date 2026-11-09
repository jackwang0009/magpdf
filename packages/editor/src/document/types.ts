import type { Rect } from '../geometry';

/**
 * 界面持有的文档只读快照（方案 3.1）：对象列表、包围盒、少量属性。
 * 正式类型由 V06 从 Rust 生成到 @magpdf/protocol，这里先用模拟数据的形状；字段命名尽量与《接口协议》一致。
 */

export type ObjectType = 'text' | 'path' | 'image' | 'component';

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
  readonly appearance: MockAppearance;
}

export interface PageSnapshot {
  readonly index: number;
  /** pt */
  readonly width: number;
  /** pt */
  readonly height: number;
  /** 从底到顶的绘制顺序 */
  readonly objects: readonly PageObject[];
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
