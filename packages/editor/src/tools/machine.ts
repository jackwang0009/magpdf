import type { HitTester } from '../document/hitTest';
import type { PageLayout } from '../document/pageLayout';
import type { PageObject, PageSnapshot } from '../document/types';
import type { Point, Rect, Size } from '../geometry';
import type { ViewState } from '../view/viewState';
import type { CommandId } from './shortcuts';
import type { ToolId } from './tools';

/**
 * 工具状态机的公共协议：纯函数 `transition(state, event, ctx) → { state, effects }`。
 * 状态机不碰 DOM、不碰 store，副作用由 CanvasController 执行，便于单测。
 */
export interface ToolPointerEvent {
  readonly type: 'pointerdown' | 'pointermove' | 'pointerup' | 'pointercancel';
  /** 视口内 CSS px 坐标 */
  readonly screen: Point;
  /** 文档坐标（pt，布局空间） */
  readonly doc: Point;
  /** 0 左键，1 中键，2 右键 */
  readonly button: number;
  readonly shift: boolean;
  readonly alt: boolean;
  readonly ctrl: boolean;
}

export interface ToolCommandEvent {
  readonly type: 'command';
  readonly command: CommandId;
}

export interface ToolCancelEvent {
  /** 失焦、切换工具等外部打断 */
  readonly type: 'cancel';
}

export type ToolEvent = ToolPointerEvent | ToolCommandEvent | ToolCancelEvent;

export interface ObjectRef {
  readonly page: number;
  readonly object: PageObject;
  /** 对象包围盒在文档（布局）坐标中的位置 */
  readonly docRect: Rect;
}

export interface ToolContext {
  readonly view: ViewState;
  readonly viewport: Size;
  readonly layout: PageLayout;
  readonly pages: readonly PageSnapshot[];
  readonly hitTester: HitTester;
  readonly selection: readonly string[];
  readonly objectById: (id: string) => ObjectRef | undefined;
  /** 文档坐标 → 屏幕坐标 */
  readonly docToScreen: (p: Point) => Point;
  readonly screenToDoc: (p: Point) => Point;
  readonly options: ToolOptions;
}

export interface ToolOptions {
  /** 框选判定：相交（默认）或完全包含 */
  readonly marqueeMode: 'intersect' | 'contain';
  /** 微移步长（pt），默认 0.1 mm */
  readonly nudgeStep: number;
  /** 拖动判定阈值（px） */
  readonly dragThreshold: number;
}

/** 交互过程中的临时图形，由叠加层绘制（不进 store） */
export interface InteractionOverlay {
  /** 框选矩形（屏幕坐标） */
  readonly marquee?: Rect;
  /** 拖动/缩放预览：对象 id → 新的文档矩形 */
  readonly preview?: ReadonlyMap<string, Rect>;
  /** 旋转预览角度（度） */
  readonly rotation?: number;
  /** 缩放工具的框选矩形 */
  readonly zoomRect?: Rect;
}

export type ToolEffect =
  | { readonly type: 'setSelection'; readonly ids: readonly string[] }
  | {
      readonly type: 'moveObjects';
      readonly ids: readonly string[];
      readonly dx: number;
      readonly dy: number;
    }
  | {
      readonly type: 'resizeObjects';
      /** 每个对象缩放之后的文档矩形 */
      readonly rects: ReadonlyMap<string, Rect>;
      /**
       * 选区的包围盒，缩放之前和之后（文档坐标）。所有对象做的是同一个变换：把 `from` 映到 `to` 的
       * 缩放加平移。交给引擎的是这个变换，不是一个个矩形。
       */
      readonly from: Rect;
      readonly to: Rect;
    }
  | { readonly type: 'deleteObjects'; readonly ids: readonly string[] }
  | { readonly type: 'setView'; readonly view: ViewState }
  | { readonly type: 'setTool'; readonly tool: ToolId }
  | { readonly type: 'overlay'; readonly overlay: InteractionOverlay | null }
  | { readonly type: 'cursor'; readonly cursor: string };

export interface Transition<S> {
  readonly state: S;
  readonly effects: readonly ToolEffect[];
}

export interface ToolMachine<S> {
  readonly id: ToolId;
  initial(): S;
  transition(state: S, event: ToolEvent, ctx: ToolContext): Transition<S>;
  /** 当前光标；hover 为鼠标所在屏幕点（可能为空） */
  cursor(state: S, ctx: ToolContext, hover: Point | null): string;
}

export function transition<S>(state: S, ...effects: ToolEffect[]): Transition<S> {
  return { state, effects };
}

/**
 * 这些对象里有没有不能修改的：它所在的页没有通过保真校验，只能在上面添加内容（方案 2.3）。
 * 这样的对象可以选中、查看，不能移动、缩放、删除；只要选区里有一个，整个选区都不动。
 */
export function hasFrozenObject(
  ctx: Pick<ToolContext, 'pages' | 'objectById'>,
  ids: readonly string[],
): boolean {
  return ids.some((id) => {
    const ref = ctx.objectById(id);
    return ref !== undefined && ctx.pages[ref.page]?.overlayOnly !== undefined;
  });
}
