import type { ToolId } from './tools';
import { tools } from './tools';

/**
 * 快捷键 → 命令（方案 8.4、《交互设计》第 8 节）。可自定义在 P1 做，这里先把默认表写成数据。
 * 组合键写法：修饰键按 Ctrl、Shift、Alt 的顺序，用 `+` 连接，最后是 KeyboardEvent.key 的规范形式
 * （字母大写，如 `Ctrl+Shift+Z`、`Shift+ArrowLeft`、`Ctrl+0`）。
 */
export type CommandId =
  | `tool.${ToolId}`
  | 'view.zoomIn'
  | 'view.zoomOut'
  | 'view.fitPage'
  | 'view.actualSize'
  | 'view.toggleRulers'
  | 'edit.undo'
  | 'edit.redo'
  | 'edit.selectAll'
  | 'edit.deselect'
  | 'edit.delete'
  | 'edit.nudgeLeft'
  | 'edit.nudgeRight'
  | 'edit.nudgeUp'
  | 'edit.nudgeDown'
  | 'edit.nudgeLeftLarge'
  | 'edit.nudgeRightLarge'
  | 'edit.nudgeUpLarge'
  | 'edit.nudgeDownLarge'
  | 'file.new'
  | 'file.open'
  | 'file.save'
  | 'file.print'
  | 'file.close'
  | 'window.togglePanels'
  | 'window.toggleFrameStats'
  | 'window.benchmark'
  | 'window.resetWorkspace';

export interface Shortcut {
  readonly combo: string;
  readonly command: CommandId;
}

export const defaultShortcuts: readonly Shortcut[] = [
  ...tools.map((tool): Shortcut => ({ combo: tool.key, command: `tool.${tool.id}` })),
  { combo: 'Ctrl+0', command: 'view.fitPage' },
  { combo: 'Ctrl+1', command: 'view.actualSize' },
  { combo: 'Ctrl+=', command: 'view.zoomIn' },
  { combo: 'Ctrl+-', command: 'view.zoomOut' },
  { combo: 'Ctrl+R', command: 'view.toggleRulers' },
  { combo: 'Ctrl+Z', command: 'edit.undo' },
  { combo: 'Ctrl+Shift+Z', command: 'edit.redo' },
  { combo: 'Ctrl+A', command: 'edit.selectAll' },
  { combo: 'Escape', command: 'edit.deselect' },
  { combo: 'Delete', command: 'edit.delete' },
  { combo: 'Backspace', command: 'edit.delete' },
  { combo: 'ArrowLeft', command: 'edit.nudgeLeft' },
  { combo: 'ArrowRight', command: 'edit.nudgeRight' },
  { combo: 'ArrowUp', command: 'edit.nudgeUp' },
  { combo: 'ArrowDown', command: 'edit.nudgeDown' },
  { combo: 'Shift+ArrowLeft', command: 'edit.nudgeLeftLarge' },
  { combo: 'Shift+ArrowRight', command: 'edit.nudgeRightLarge' },
  { combo: 'Shift+ArrowUp', command: 'edit.nudgeUpLarge' },
  { combo: 'Shift+ArrowDown', command: 'edit.nudgeDownLarge' },
  { combo: 'Ctrl+N', command: 'file.new' },
  { combo: 'Ctrl+O', command: 'file.open' },
  { combo: 'Ctrl+S', command: 'file.save' },
  { combo: 'Ctrl+P', command: 'file.print' },
  { combo: 'Ctrl+W', command: 'file.close' },
  { combo: 'Tab', command: 'window.togglePanels' },
];

/** KeyboardEvent 里本模块用到的字段，便于测试时构造。 */
export interface KeyLike {
  readonly key: string;
  readonly ctrlKey: boolean;
  readonly shiftKey: boolean;
  readonly altKey: boolean;
  readonly metaKey?: boolean;
}

/** 把按键事件规范成组合键字符串；`+`/`=` 和 `-`/`_` 归并，这样 Ctrl+Shift+= 也算放大。 */
export function comboFromEvent(event: KeyLike): string {
  let key = event.key;
  let shift = event.shiftKey;
  if (key === '+' || key === '=') {
    key = '=';
    shift = false;
  } else if (key === '-' || key === '_') {
    key = '-';
    shift = false;
  } else if (key === ' ') {
    key = 'Space';
  } else if (key.length === 1) {
    // 单个字母或数字的大小写由 Shift 决定，组合键里仍把 Shift 单列
    key = key.toUpperCase();
  }
  const parts: string[] = [];
  if (event.ctrlKey || event.metaKey) parts.push('Ctrl');
  if (shift) parts.push('Shift');
  if (event.altKey) parts.push('Alt');
  parts.push(key);
  return parts.join('+');
}

export class ShortcutMap {
  private readonly byCombo = new Map<string, CommandId>();

  constructor(shortcuts: readonly Shortcut[] = defaultShortcuts) {
    for (const shortcut of shortcuts) {
      if (this.byCombo.has(shortcut.combo)) {
        throw new Error(`快捷键冲突：${shortcut.combo}`);
      }
      this.byCombo.set(shortcut.combo, shortcut.command);
    }
  }

  resolve(event: KeyLike): CommandId | undefined {
    return this.byCombo.get(comboFromEvent(event));
  }

  /** 某命令的快捷键（用于菜单右侧的提示），没有时返回 undefined。 */
  comboFor(command: CommandId): string | undefined {
    for (const [combo, cmd] of this.byCombo) {
      if (cmd === command) return combo;
    }
    return undefined;
  }
}

export const defaultShortcutMap = new ShortcutMap();

/** 事件目标是否正在输入文字（输入框、可编辑区域），此时不处理单键快捷键。 */
export function isTextInputTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  if (tag === 'TEXTAREA' || tag === 'SELECT') return true;
  if (tag === 'INPUT') {
    const type = (target as HTMLInputElement).type;
    return type !== 'checkbox' && type !== 'radio' && type !== 'button' && type !== 'range';
  }
  return false;
}
