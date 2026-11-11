import { describe, expect, it } from 'vitest';
import { linearHitTester } from '../document/hitTest';
import { PageLayout } from '../document/pageLayout';
import type { PageObject, PageSnapshot } from '../document/types';
import { A4 } from '../document/types';
import { mmToPt } from '../geometry';
import type { Point, Rect } from '../geometry';
import { cssPxPerPt } from '../view/viewState';
import type { ViewState } from '../view/viewState';
import { ROTATE_CURSOR } from './cursors';
import type { ToolContext, ToolEffect, ToolPointerEvent } from './machine';
import { NUDGE_STEP_PT, selectTool } from './selectTool';
import type { SelectState } from './selectTool';

function obj(id: string, x: number, y: number, w = 40, h = 20): PageObject {
  return {
    id,
    type: 'text',
    name: id,
    bbox: { x, y, width: w, height: h },
    appearance: { kind: 'rect', fill: '#000' },
  };
}

const page: PageSnapshot = {
  index: 0,
  width: A4.width,
  height: A4.height,
  objects: [obj('a', 20, 20), obj('b', 200, 20)],
};
const layout = new PageLayout([A4]);
const view: ViewState = { zoom: 1, x: 0, y: 0 };
const pageOrigin = layout.pageRect(0);

function context(selection: readonly string[] = []): ToolContext {
  const objects = new Map<string, Rect>();
  const byId = new Map<string, PageObject>();
  for (const o of page.objects) {
    objects.set(o.id, {
      x: o.bbox.x + pageOrigin.x,
      y: o.bbox.y + pageOrigin.y,
      width: o.bbox.width,
      height: o.bbox.height,
    });
    byId.set(o.id, o);
  }
  const s = cssPxPerPt(view.zoom);
  return {
    view,
    viewport: { width: 1200, height: 900 },
    layout,
    pages: [page],
    hitTester: linearHitTester,
    selection,
    objectById: (id) => {
      const object = byId.get(id);
      const rect = objects.get(id);
      return object && rect ? { page: 0, object, docRect: rect } : undefined;
    },
    docToScreen: (p) => ({ x: (p.x - view.x) * s, y: (p.y - view.y) * s }),
    screenToDoc: (p) => ({ x: view.x + p.x / s, y: view.y + p.y / s }),
    options: { marqueeMode: 'intersect', nudgeStep: NUDGE_STEP_PT, dragThreshold: 4 },
  };
}

function pointer(
  type: ToolPointerEvent['type'],
  doc: Point,
  extra: Partial<ToolPointerEvent> = {},
): ToolPointerEvent {
  const ctx = context();
  return {
    type,
    doc,
    screen: ctx.docToScreen(doc),
    button: 0,
    shift: false,
    alt: false,
    ctrl: false,
    ...extra,
  };
}

/** 依次执行事件，返回最终状态与全部副作用 */
function run(
  start: SelectState,
  events: Parameters<typeof selectTool.transition>[1][],
  ctx = context(),
) {
  let state = start;
  const effects: ToolEffect[] = [];
  for (const event of events) {
    const result = selectTool.transition(state, event, ctx);
    state = result.state;
    effects.push(...result.effects);
  }
  return { state, effects };
}

const center = (id: string) => {
  const o = page.objects.find((p) => p.id === id)!;
  return { x: pageOrigin.x + o.bbox.x + 5, y: pageOrigin.y + o.bbox.y + 5 };
};

describe('选择工具状态机', () => {
  it('点击选中，点空白取消选中', () => {
    const click = run({ kind: 'idle' }, [
      pointer('pointerdown', center('a')),
      pointer('pointerup', center('a')),
    ]);
    expect(click.state.kind).toBe('idle');
    expect(click.effects).toContainEqual({ type: 'setSelection', ids: ['a'] });

    const blank = run(
      { kind: 'idle' },
      [pointer('pointerdown', { x: pageOrigin.x + 300, y: pageOrigin.y + 300 })],
      context(['a']),
    );
    expect(blank.effects[0]).toEqual({ type: 'setSelection', ids: [] });
  });

  it('Shift 点击加选，再 Shift 点击取消', () => {
    const add = run(
      { kind: 'idle' },
      [pointer('pointerdown', center('b'), { shift: true })],
      context(['a']),
    );
    expect(add.effects).toContainEqual({ type: 'setSelection', ids: ['a', 'b'] });

    const remove = run(
      { kind: 'idle' },
      [
        pointer('pointerdown', center('b'), { shift: true }),
        pointer('pointerup', center('b'), { shift: true }),
      ],
      context(['a', 'b']),
    );
    expect(remove.effects).toContainEqual({ type: 'setSelection', ids: ['a'] });
  });

  it('框选：相交选中的对象，叠加 Shift 保留原选区', () => {
    const start = pointer('pointerdown', { x: pageOrigin.x + 10, y: pageOrigin.y + 10 });
    const move = pointer('pointermove', { x: pageOrigin.x + 100, y: pageOrigin.y + 80 });
    const up = pointer('pointerup', { x: pageOrigin.x + 100, y: pageOrigin.y + 80 });
    const result = run({ kind: 'idle' }, [start, move, up]);
    expect(result.state.kind).toBe('idle');
    expect(result.effects).toContainEqual({ type: 'setSelection', ids: ['a'] });
    expect(result.effects.some((e) => e.type === 'overlay' && e.overlay?.marquee)).toBe(true);
  });

  it('拖动移动：提交位移副作用，预览不进状态', () => {
    const from = center('a');
    const to = { x: from.x + 30, y: from.y + 40 };
    const result = run(
      { kind: 'idle' },
      [pointer('pointerdown', from), pointer('pointermove', to), pointer('pointerup', to)],
      context(['a']),
    );
    expect(result.effects).toContainEqual({ type: 'moveObjects', ids: ['a'], dx: 30, dy: 40 });
    expect(result.state).toEqual({ kind: 'idle' });
  });

  it('移动中的预览矩形跟着指针走', () => {
    const from = center('a');
    const result = run(
      { kind: 'idle' },
      [pointer('pointerdown', from), pointer('pointermove', { x: from.x + 10, y: from.y + 10 })],
      context(['a']),
    );
    const overlay = result.effects.find((e) => e.type === 'overlay');
    expect(
      overlay && overlay.type === 'overlay' ? overlay.overlay?.preview?.get('a') : null,
    ).toMatchObject({
      x: pageOrigin.x + 30,
      y: pageOrigin.y + 30,
    });
  });

  it('方向键微移 0.1 mm，Shift 十倍，没有选中时不产生副作用', () => {
    expect(NUDGE_STEP_PT).toBeCloseTo(mmToPt(0.1), 6);
    const one = run(
      { kind: 'idle' },
      [{ type: 'command', command: 'edit.nudgeRight' }],
      context(['a']),
    );
    expect(one.effects[0]).toEqual({ type: 'moveObjects', ids: ['a'], dx: NUDGE_STEP_PT, dy: 0 });
    const large = run(
      { kind: 'idle' },
      [{ type: 'command', command: 'edit.nudgeUpLarge' }],
      context(['a']),
    );
    expect(large.effects[0]).toEqual({
      type: 'moveObjects',
      ids: ['a'],
      dx: 0,
      dy: -NUDGE_STEP_PT * 10,
    });
    const none = run({ kind: 'idle' }, [{ type: 'command', command: 'edit.nudgeLeft' }]);
    expect(none.effects).toHaveLength(0);
  });

  it('Delete 删除选中、Esc 取消除选中', () => {
    const del = run(
      { kind: 'idle' },
      [{ type: 'command', command: 'edit.delete' }],
      context(['a', 'b']),
    );
    expect(del.effects[0]).toEqual({ type: 'deleteObjects', ids: ['a', 'b'] });
    const esc = run(
      { kind: 'idle' },
      [{ type: 'command', command: 'edit.deselect' }],
      context(['a']),
    );
    expect(esc.effects[0]).toEqual({ type: 'setSelection', ids: [] });
  });

  it('拖控制点缩放：提交每页内的新矩形', () => {
    const ctx = context(['a']);
    const rect = ctx.objectById('a')!.docRect;
    const se = { x: rect.x + rect.width, y: rect.y + rect.height };
    const result = run(
      { kind: 'idle' },
      [
        pointer('pointerdown', se),
        pointer('pointermove', { x: se.x + 10, y: se.y + 10 }),
        pointer('pointerup', { x: se.x + 10, y: se.y + 10 }),
      ],
      ctx,
    );
    const resize = result.effects.find((e) => e.type === 'resizeObjects');
    expect(resize && resize.type === 'resizeObjects' ? resize.rects.get('a') : null).toMatchObject({
      x: rect.x,
      y: rect.y,
      width: rect.width + 10,
      height: rect.height + 10,
    });
    // 同时给出选区包围盒缩放前后的样子：交给引擎的是“把前一个映到后一个”的变换
    expect(resize).toMatchObject({
      from: rect,
      to: { x: rect.x, y: rect.y, width: rect.width + 10, height: rect.height + 10 },
    });
  });

  it('多选缩放：前后两个包围盒是整个选区的，每个对象按同一个变换缩放', () => {
    const ctx = context(['a', 'b']);
    const a = ctx.objectById('a')!.docRect;
    const b = ctx.objectById('b')!.docRect;
    // 选区从 a 的左上角到 b 的右下角；拖右边中点把宽度加一倍
    const box = { x: a.x, y: a.y, width: b.x + b.width - a.x, height: a.height };
    const east = { x: box.x + box.width, y: box.y + box.height / 2 };
    const result = run(
      { kind: 'idle' },
      [
        pointer('pointerdown', east),
        pointer('pointermove', { x: east.x + box.width, y: east.y }),
        pointer('pointerup', { x: east.x + box.width, y: east.y }),
      ],
      ctx,
    );
    const resize = result.effects.find((e) => e.type === 'resizeObjects');
    expect(resize).toMatchObject({ from: box, to: { ...box, width: box.width * 2 } });
    const rects = resize && resize.type === 'resizeObjects' ? resize.rects : new Map();
    expect(rects.get('a')).toMatchObject({ x: a.x, width: a.width * 2 });
    expect(rects.get('b')).toMatchObject({ x: a.x + (b.x - a.x) * 2, width: b.width * 2 });
  });

  it('旋转热区按下进入旋转状态（光标随之改变）', () => {
    const ctx = context(['a']);
    const rect = ctx.objectById('a')!.docRect;
    // 角外侧 12 pt：在控制点命中半径之外、旋转热区之内
    const outside = { x: rect.x - 12, y: rect.y - 12 };
    const result = run({ kind: 'idle' }, [pointer('pointerdown', outside)], ctx);
    expect(result.state.kind).toBe('rotating');
    expect(result.effects.some((e) => e.type === 'overlay' && e.overlay?.rotation === 0)).toBe(
      true,
    );
    expect(selectTool.cursor(result.state, ctx, outside)).toBe(ROTATE_CURSOR);
    // 有选中对象时，悬停到其角外侧显示旋转光标；没有选中时是普通光标
    // （光标判定用屏幕坐标，这里取选择框左上角像素再往外 12px）
    const screenBox = ctx;
    const corner = screenBox.docToScreen({ x: rect.x, y: rect.y });
    const screenOutside = { x: corner.x - 12, y: corner.y - 12 };
    expect(selectTool.cursor({ kind: 'idle' }, screenBox, screenOutside)).toBe(ROTATE_CURSOR);
    expect(selectTool.cursor({ kind: 'idle' }, context([]), screenOutside)).toBe('default');
  });

  it('取消事件回到空闲并清掉叠加层', () => {
    const result = run(
      { kind: 'idle' },
      [pointer('pointerdown', center('a')), { type: 'cancel' }],
      context(['a']),
    );
    expect(result.state).toEqual({ kind: 'idle' });
    expect(result.effects).toContainEqual({ type: 'overlay', overlay: null });
  });
});
