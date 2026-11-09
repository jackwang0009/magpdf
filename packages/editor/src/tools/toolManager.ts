import type { Point } from '../geometry';
import { handTool } from './handTool';
import { transition } from './machine';
import type { ToolContext, ToolEffect, ToolEvent, ToolMachine } from './machine';
import { selectTool } from './selectTool';
import type { CommandId } from './shortcuts';
import type { ToolId } from './tools';
import { zoomTool } from './zoomTool';

/**
 * 工具管理器：持有每个工具的状态机与当前状态，处理 Space 临时抓手、中键拖动平移等覆盖规则，
 * 把事件分发给“生效工具”，返回副作用给 CanvasController 执行。
 */
interface Slot {
  readonly machine: ToolMachine<unknown>;
  state: unknown;
}

type OverrideReason = 'space' | 'middle';

/** 尚未实现交互的工具：只提供光标，事件不产生副作用 */
function placeholderTool(id: ToolId, cursor: string): ToolMachine<null> {
  return {
    id,
    initial: () => null,
    transition: (state) => transition(state),
    cursor: () => cursor,
  };
}

export class ToolManager {
  private readonly slots = new Map<ToolId, Slot>();
  private activeTool: ToolId = 'select';
  private override: { tool: ToolId; reason: OverrideReason } | null = null;
  /** 收到 pointerdown 的工具；抬起前即使覆盖结束也继续把事件发给它 */
  private pointerOwner: ToolId | null = null;
  /** 本工具会话内由工具自己改过的选区（对象 id）；为 null 时以 store 的选区为准 */
  private selectionOverride: readonly string[] | null = null;
  private spaceHeld = false;
  private releaseSpaceAfterPointer = false;

  constructor() {
    const machines: ToolMachine<unknown>[] = [
      selectTool as ToolMachine<unknown>,
      handTool as ToolMachine<unknown>,
      zoomTool as ToolMachine<unknown>,
      placeholderTool('direct-select', 'default'),
      placeholderTool('text', 'text'),
      placeholderTool('pen', 'crosshair'),
      placeholderTool('shape', 'crosshair'),
      placeholderTool('heal', 'crosshair'),
      placeholderTool('eyedropper', 'crosshair'),
      placeholderTool('crop', 'crosshair'),
    ];
    for (const machine of machines)
      this.slots.set(machine.id, { machine, state: machine.initial() });
  }

  get active(): ToolId {
    return this.activeTool;
  }

  /** 当前真正处理事件的工具（含临时覆盖） */
  get effective(): ToolId {
    return this.pointerOwner ?? this.override?.tool ?? this.activeTool;
  }

  get isOverridden(): boolean {
    return this.override !== null;
  }

  setActiveTool(tool: ToolId, ctx: ToolContext): ToolEffect[] {
    if (tool === this.activeTool) return [];
    const effects = this.cancelAll(ctx);
    this.activeTool = tool;
    return effects;
  }

  dispatch(event: ToolEvent, ctx: ToolContext): ToolEffect[] {
    if (event.type === 'pointerdown' && !this.pointerOwner) this.selectionOverride = null;
    if (event.type === 'pointerdown') {
      if (event.button === 1 && !this.pointerOwner) {
        this.override = { tool: 'hand', reason: 'middle' };
      }
      const owner = this.effective;
      this.pointerOwner = owner;
      // 抓手对中键按下也要响应，所以把按钮统一成左键
      const normalized = this.override?.reason === 'middle' ? { ...event, button: 0 } : event;
      return this.run(owner, normalized, ctx);
    }
    if (event.type === 'pointerup' || event.type === 'pointercancel') {
      const owner = this.pointerOwner ?? this.effective;
      const effects = this.run(owner, event, ctx);
      this.pointerOwner = null;
      if (this.override?.reason === 'middle') this.override = null;
      if (this.releaseSpaceAfterPointer) {
        this.releaseSpaceAfterPointer = false;
        this.endSpace();
      }
      return effects;
    }
    return this.run(this.effective, event, ctx);
  }

  /** Space 按下：临时切到抓手（抓手/缩放工具本身不需要） */
  beginSpace(): void {
    if (this.spaceHeld) return;
    this.spaceHeld = true;
    if (this.pointerOwner) return;
    if (this.activeTool !== 'hand' && !this.override) {
      this.override = { tool: 'hand', reason: 'space' };
    }
  }

  endSpace(): void {
    if (!this.spaceHeld) return;
    if (this.pointerOwner) {
      // 拖动中松开 Space：等抬起后再恢复
      this.releaseSpaceAfterPointer = true;
      return;
    }
    this.spaceHeld = false;
    if (this.override?.reason === 'space') this.override = null;
  }

  /** 外部改了选区（层面板点选、store 变更后调用），之后不再用工具内部的覆盖值 */
  forgetSelectionOverride(): void {
    this.selectionOverride = null;
  }

  /** 把快捷键命令发给生效工具；返回 null 表示工具没有处理（交给应用层） */
  command(command: CommandId, ctx: ToolContext): ToolEffect[] {
    return this.run(this.effective, { type: 'command', command }, ctx);
  }

  cursor(ctx: ToolContext, hover: Point | null): string {
    const slot = this.slots.get(this.effective);
    const live: ToolContext = this.selectionOverride
      ? { ...ctx, selection: this.selectionOverride }
      : ctx;
    return slot ? slot.machine.cursor(slot.state, live, hover) : 'default';
  }

  cancelAll(ctx: ToolContext): ToolEffect[] {
    const effects: ToolEffect[] = [];
    for (const [id] of this.slots) effects.push(...this.run(id, { type: 'cancel' }, ctx));
    this.pointerOwner = null;
    if (this.override?.reason === 'middle') this.override = null;
    return effects;
  }

  /** 测试/调试用：读取某个工具的状态 */
  stateOf<S>(tool: ToolId): S {
    return this.slots.get(tool)!.state as S;
  }

  private run(tool: ToolId, event: ToolEvent, ctx: ToolContext): ToolEffect[] {
    const slot = this.slots.get(tool);
    if (!slot) return [];
    // 选择类状态机依赖“按下的那一刻”的选区：应用副作用之前的状态要回填给上下文，
    // 否则同一次手势里的后续事件读到的还是旧选区（store 在下一帧才更新）。
    const live: ToolContext = this.selectionOverride
      ? { ...ctx, selection: this.selectionOverride }
      : ctx;
    const result = slot.machine.transition(slot.state, event, live);
    slot.state = result.state;
    for (const effect of result.effects) {
      if (effect.type === 'setSelection') this.selectionOverride = effect.ids;
      else if (effect.type === 'deleteObjects') {
        this.selectionOverride = (this.selectionOverride ?? ctx.selection).filter(
          (id) => !effect.ids.includes(id),
        );
      }
    }
    return [...result.effects];
  }
}
