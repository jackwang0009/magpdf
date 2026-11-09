import { describe, expect, it } from 'vitest';
import { linearHitTester } from '../document/hitTest';
import { PageLayout } from '../document/pageLayout';
import { A4 } from '../document/types';
import type { PageSnapshot } from '../document/types';
import type { ToolContext, ToolPointerEvent } from './machine';
import { ToolManager } from './toolManager';

const page: PageSnapshot = { index: 0, width: A4.width, height: A4.height, objects: [] };
const layout = new PageLayout([A4]);
const ctx: ToolContext = {
  view: { zoom: 1, x: 0, y: 0 },
  viewport: { width: 1000, height: 800 },
  layout,
  pages: [page],
  hitTester: linearHitTester,
  selection: [],
  objectById: () => undefined,
  docToScreen: (p) => p,
  screenToDoc: (p) => p,
  options: { marqueeMode: 'intersect', nudgeStep: 0.35, dragThreshold: 4 },
};

function pointer(type: ToolPointerEvent['type'], x: number, button = 0): ToolPointerEvent {
  return {
    type,
    screen: { x, y: 100 },
    doc: { x, y: 100 },
    button,
    shift: false,
    alt: false,
    ctrl: false,
  };
}

describe('工具管理器', () => {
  it('默认选择工具；切换工具时先取消进行中的交互', () => {
    const manager = new ToolManager();
    expect(manager.active).toBe('select');
    manager.dispatch(pointer('pointerdown', 100), ctx);
    const effects = manager.setActiveTool('hand', ctx);
    expect(effects.some((e) => e.type === 'overlay')).toBe(true);
    expect(manager.active).toBe('hand');
    expect(manager.stateOf('select')).toEqual({ kind: 'idle' });
  });

  it('Space 临时切抓手，松开恢复；拖动中松开等抬起再恢复', () => {
    const manager = new ToolManager();
    manager.beginSpace();
    expect(manager.effective).toBe('hand');
    const effects = manager.dispatch(pointer('pointerdown', 100), ctx);
    manager.endSpace();
    // 还在拖动：继续由抓手处理
    expect(manager.effective).toBe('hand');
    effects.push(...manager.dispatch(pointer('pointermove', 120), ctx));
    expect(effects.some((e) => e.type === 'setView')).toBe(true);
    manager.dispatch(pointer('pointerup', 120), ctx);
    expect(manager.effective).toBe('select');
  });

  it('中键拖动平移，不改当前工具', () => {
    const manager = new ToolManager();
    const down = manager.dispatch(pointer('pointerdown', 100, 1), ctx);
    const move = manager.dispatch(pointer('pointermove', 80), ctx);
    manager.dispatch(pointer('pointerup', 80, 1), ctx);
    expect(down).toHaveLength(0);
    // 向左拖 20 px 时内容跟着指针走：视图原点右移 20 CSS px（100% 下 1 px = 0.75 pt）
    const setView = move.find((e) => e.type === 'setView');
    expect(Math.abs(setView && setView.type === 'setView' ? setView.view.x : 0)).toBeCloseTo(
      20 / (96 / 72),
      6,
    );
    expect(manager.active).toBe('select');
    expect(manager.effective).toBe('select');
  });

  it('抓手工具本身按 Space 不额外覆盖', () => {
    const manager = new ToolManager();
    manager.setActiveTool('hand', ctx);
    manager.beginSpace();
    expect(manager.effective).toBe('hand');
    manager.endSpace();
    expect(manager.effective).toBe('hand');
  });

  it('同一次手势里选区变更立刻对后续事件生效', () => {
    const manager = new ToolManager();
    // 按在空白处会先清空选区；抬高时不应再基于旧选区做任何事
    manager.dispatch(pointer('pointerdown', 500), { ...ctx, selection: ['x'] });
    const effects = manager.dispatch(pointer('pointerup', 500), { ...ctx, selection: ['x'] });
    expect(effects.filter((e) => e.type === 'moveObjects')).toHaveLength(0);
    manager.forgetSelectionOverride();
  });

  it('光标由生效工具提供', () => {
    const manager = new ToolManager();
    expect(manager.cursor(ctx, null)).toBe('default');
    manager.beginSpace();
    expect(manager.cursor(ctx, null)).toBe('grab');
    manager.endSpace();
    manager.setActiveTool('zoom', ctx);
    expect(manager.cursor(ctx, null)).toContain('zoom-in');
  });
});
