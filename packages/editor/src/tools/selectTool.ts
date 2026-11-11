import { distance, mmToPt, rectFromPoints, translateRect, unionRects } from '../geometry';
import type { Point, Rect } from '../geometry';
import { cssPxPerPt } from '../view/viewState';
import {
  handleCursor,
  hitHandle,
  inRotateZone,
  mapRectBetween,
  resizeRectByHandle,
} from './handles';
import type { HandleId } from './handles';
import { hasFrozenObject, transition } from './machine';
import type { ToolContext, ToolEffect, ToolMachine, ToolPointerEvent, Transition } from './machine';
import { ROTATE_CURSOR } from './cursors';

/**
 * 选择工具（V）状态机：点选、Shift 加选、框选、拖动移动、控制点缩放、旋转热区、方向键微移。
 * 《交互设计》2.1–2.2。
 *
 * 只能叠加内容的页（方案 2.3）上，原有的对象照常可以点选、框选，但不能修改：选区里只要有一个这样的
 * 对象，就没有控制点和旋转热区，拖动、微移、删除都不起作用。
 */
export type SelectState =
  | { readonly kind: 'idle' }
  | {
      readonly kind: 'pressed';
      readonly startDoc: Point;
      readonly startScreen: Point;
      readonly hitId: string | null;
      readonly shift: boolean;
      /** 按下时该对象是否已经在选区里（决定抬起时是否取消选中） */
      readonly wasSelected: boolean;
    }
  | {
      readonly kind: 'marquee';
      readonly startDoc: Point;
      readonly currentDoc: Point;
      readonly shift: boolean;
      readonly baseSelection: readonly string[];
    }
  | {
      readonly kind: 'moving';
      readonly startDoc: Point;
      readonly currentDoc: Point;
      readonly ids: readonly string[];
      readonly startRects: ReadonlyMap<string, Rect>;
    }
  | {
      readonly kind: 'pressedHandle';
      readonly handle: HandleId;
      readonly startDoc: Point;
      readonly startScreen: Point;
      readonly bbox: Rect;
      readonly ids: readonly string[];
      readonly startRects: ReadonlyMap<string, Rect>;
    }
  | {
      readonly kind: 'scaling';
      readonly handle: HandleId;
      readonly startDoc: Point;
      readonly currentDoc: Point;
      readonly bbox: Rect;
      readonly ids: readonly string[];
      readonly startRects: ReadonlyMap<string, Rect>;
      readonly keepRatio: boolean;
    }
  | {
      readonly kind: 'rotating';
      readonly center: Point;
      readonly startAngle: number;
      readonly currentAngle: number;
      readonly ids: readonly string[];
    }
  /** 按住不能修改的对象在拖：什么都不动，光标提示“不行”，等鼠标抬起 */
  | { readonly kind: 'refused' };

/** 点选容差（px） */
const HIT_TOLERANCE_PX = 3;
/** 微移默认步长 0.1 mm；Shift 为 10 倍 */
export const NUDGE_STEP_PT = mmToPt(0.1);

const idle: SelectState = { kind: 'idle' };

export const selectTool: ToolMachine<SelectState> = {
  id: 'select',
  initial: () => idle,
  transition(state, event, ctx) {
    switch (event.type) {
      case 'pointerdown':
        return onPointerDown(state, event, ctx);
      case 'pointermove':
        return onPointerMove(state, event, ctx);
      case 'pointerup':
      case 'pointercancel':
        return onPointerUp(state, event, ctx);
      case 'command':
        return onCommand(state, event.command, ctx);
      case 'cancel':
        return cancel(state);
    }
  },
  cursor(state, ctx, hover) {
    switch (state.kind) {
      case 'moving':
        return 'move';
      case 'marquee':
        return 'crosshair';
      case 'scaling':
      case 'pressedHandle':
        return handleCursor(state.handle);
      case 'rotating':
        return ROTATE_CURSOR;
      case 'refused':
        return 'not-allowed';
      case 'pressed':
        return state.hitId && !hasFrozenObject(ctx, dragTargets(ctx, state.hitId))
          ? 'move'
          : 'default';
      case 'idle': {
        if (!hover) return 'default';
        const frozen = hasFrozenObject(ctx, ctx.selection);
        const bbox = frozen ? null : selectionScreenRect(ctx);
        if (bbox) {
          const handle = hitHandle(bbox, hover);
          if (handle) return handleCursor(handle);
          if (inRotateZone(bbox, hover)) return ROTATE_CURSOR;
        }
        const hit = hitObjectAtScreen(ctx, hover);
        return hit && ctx.selection.includes(hit) && !frozen ? 'move' : 'default';
      }
    }
  },
};

/** 按住 `hitId` 拖动时会一起动的对象：它在选区里就是整个选区，否则只有它自己。 */
function dragTargets(ctx: ToolContext, hitId: string): readonly string[] {
  return ctx.selection.includes(hitId) ? ctx.selection : [hitId];
}

function onPointerDown(
  state: SelectState,
  event: ToolPointerEvent,
  ctx: ToolContext,
): Transition<SelectState> {
  if (event.button !== 0 || state.kind !== 'idle') return transition(state);

  // 选区里有不能修改的对象时没有控制点和旋转热区，按在那些位置上和按在别处一样
  const bbox = hasFrozenObject(ctx, ctx.selection) ? null : selectionScreenRect(ctx);
  if (bbox && ctx.selection.length > 0) {
    const handle = hitHandle(bbox, event.screen);
    if (handle) {
      return transition<SelectState>({
        kind: 'pressedHandle',
        handle,
        startDoc: event.doc,
        startScreen: event.screen,
        bbox: selectionDocRect(ctx)!,
        ids: ctx.selection,
        startRects: selectionRects(ctx),
      });
    }
    if (inRotateZone(bbox, event.screen)) {
      const docBox = selectionDocRect(ctx)!;
      const center = { x: docBox.x + docBox.width / 2, y: docBox.y + docBox.height / 2 };
      const angle = angleTo(center, event.doc);
      return transition<SelectState>(
        { kind: 'rotating', center, startAngle: angle, currentAngle: angle, ids: ctx.selection },
        { type: 'overlay', overlay: { rotation: 0 } },
      );
    }
  }

  const hitId = hitObjectAtScreen(ctx, event.screen);
  const wasSelected = hitId !== null && ctx.selection.includes(hitId);
  const effects: ToolEffect[] = [];
  if (hitId) {
    if (!wasSelected) {
      effects.push({
        type: 'setSelection',
        ids: event.shift ? [...ctx.selection, hitId] : [hitId],
      });
    }
  } else if (!event.shift && ctx.selection.length > 0) {
    effects.push({ type: 'setSelection', ids: [] });
  }
  return transition<SelectState>(
    {
      kind: 'pressed',
      startDoc: event.doc,
      startScreen: event.screen,
      hitId,
      shift: event.shift,
      wasSelected,
    },
    ...effects,
  );
}

function onPointerMove(
  state: SelectState,
  event: ToolPointerEvent,
  ctx: ToolContext,
): Transition<SelectState> {
  switch (state.kind) {
    case 'pressed': {
      if (distance(state.startScreen, event.screen) < ctx.options.dragThreshold) {
        return transition(state);
      }
      if (state.hitId) {
        const ids = dragTargets(ctx, state.hitId);
        if (hasFrozenObject(ctx, ids)) return transition<SelectState>({ kind: 'refused' });
        const startRects = rectsFor(ctx, ids);
        const next: SelectState = {
          kind: 'moving',
          startDoc: state.startDoc,
          currentDoc: event.doc,
          ids,
          startRects,
        };
        return transition(next, { type: 'overlay', overlay: { preview: movePreview(next) } });
      }
      const base = state.shift ? ctx.selection : [];
      return transition<SelectState>(
        {
          kind: 'marquee',
          startDoc: state.startDoc,
          currentDoc: event.doc,
          shift: state.shift,
          baseSelection: base,
        },
        {
          type: 'overlay',
          overlay: { marquee: marqueeScreenRect(ctx, state.startDoc, event.doc) },
        },
      );
    }
    case 'marquee': {
      const next = { ...state, currentDoc: event.doc };
      return transition<SelectState>(next, {
        type: 'overlay',
        overlay: { marquee: marqueeScreenRect(ctx, state.startDoc, event.doc) },
      });
    }
    case 'moving': {
      const next = { ...state, currentDoc: event.doc };
      return transition<SelectState>(next, {
        type: 'overlay',
        overlay: { preview: movePreview(next) },
      });
    }
    case 'pressedHandle': {
      if (distance(state.startScreen, event.screen) < ctx.options.dragThreshold) {
        return transition(state);
      }
      const next: SelectState = {
        kind: 'scaling',
        handle: state.handle,
        startDoc: state.startDoc,
        currentDoc: event.doc,
        bbox: state.bbox,
        ids: state.ids,
        startRects: state.startRects,
        keepRatio: event.shift,
      };
      return transition(next, { type: 'overlay', overlay: { preview: scalePreview(next) } });
    }
    case 'scaling': {
      const next = { ...state, currentDoc: event.doc, keepRatio: event.shift };
      return transition<SelectState>(next, {
        type: 'overlay',
        overlay: { preview: scalePreview(next) },
      });
    }
    case 'rotating': {
      const next = { ...state, currentAngle: angleTo(state.center, event.doc) };
      return transition<SelectState>(next, {
        type: 'overlay',
        overlay: { rotation: next.currentAngle - next.startAngle },
      });
    }
    case 'refused':
    case 'idle':
      return transition(state);
  }
}

function onPointerUp(
  state: SelectState,
  event: ToolPointerEvent,
  ctx: ToolContext,
): Transition<SelectState> {
  if (event.type === 'pointercancel') return cancel(state);
  switch (state.kind) {
    case 'pressed': {
      // Shift 点击已选对象：抬起时取消选中（没有拖动才算点击）
      if (state.hitId && state.shift && state.wasSelected) {
        return transition(idle, {
          type: 'setSelection',
          ids: ctx.selection.filter((id) => id !== state.hitId),
        });
      }
      return transition(idle);
    }
    case 'marquee': {
      const rect = rectFromPoints(state.startDoc, event.doc);
      const hits = objectsInDocRect(ctx, rect);
      const ids = state.shift ? unique([...state.baseSelection, ...hits]) : hits;
      return transition(idle, { type: 'overlay', overlay: null }, { type: 'setSelection', ids });
    }
    case 'moving': {
      const dx = event.doc.x - state.startDoc.x;
      const dy = event.doc.y - state.startDoc.y;
      const effects: ToolEffect[] = [{ type: 'overlay', overlay: null }];
      if (dx !== 0 || dy !== 0) effects.push({ type: 'moveObjects', ids: state.ids, dx, dy });
      return transition(idle, ...effects);
    }
    case 'pressedHandle':
      return transition(idle);
    case 'scaling': {
      const final = { ...state, currentDoc: event.doc };
      return transition(
        idle,
        { type: 'overlay', overlay: null },
        {
          type: 'resizeObjects',
          rects: scalePreview(final),
          from: state.bbox,
          to: scaleTarget(final),
        },
      );
    }
    case 'rotating':
      // 旋转提交需要引擎侧的变换矩阵支持（P1）；P0 只保留状态与光标
      return transition(idle, { type: 'overlay', overlay: null });
    case 'refused':
      return transition(idle);
    case 'idle':
      return transition(state);
  }
}

function onCommand(state: SelectState, command: string, ctx: ToolContext): Transition<SelectState> {
  if (state.kind !== 'idle') {
    if (command === 'edit.deselect') return cancel(state);
    return transition(state);
  }
  const step = ctx.options.nudgeStep;
  switch (command) {
    case 'edit.nudgeLeft':
      return nudge(ctx, -step, 0);
    case 'edit.nudgeRight':
      return nudge(ctx, step, 0);
    case 'edit.nudgeUp':
      return nudge(ctx, 0, -step);
    case 'edit.nudgeDown':
      return nudge(ctx, 0, step);
    case 'edit.nudgeLeftLarge':
      return nudge(ctx, -step * 10, 0);
    case 'edit.nudgeRightLarge':
      return nudge(ctx, step * 10, 0);
    case 'edit.nudgeUpLarge':
      return nudge(ctx, 0, -step * 10);
    case 'edit.nudgeDownLarge':
      return nudge(ctx, 0, step * 10);
    case 'edit.delete':
      return ctx.selection.length && !hasFrozenObject(ctx, ctx.selection)
        ? transition(idle, { type: 'deleteObjects', ids: ctx.selection })
        : transition(idle);
    case 'edit.deselect':
      return ctx.selection.length
        ? transition(idle, { type: 'setSelection', ids: [] })
        : transition(idle);
    case 'edit.selectAll': {
      const pageIndex = currentPage(ctx);
      const page = ctx.pages[pageIndex];
      const ids = page ? page.objects.filter((o) => !o.locked).map((o) => o.id) : [];
      return transition(idle, { type: 'setSelection', ids });
    }
    default:
      return transition(state);
  }
}

function nudge(ctx: ToolContext, dx: number, dy: number): Transition<SelectState> {
  if (ctx.selection.length === 0 || hasFrozenObject(ctx, ctx.selection)) return transition(idle);
  return transition(idle, { type: 'moveObjects', ids: ctx.selection, dx, dy });
}

function cancel(state: SelectState): Transition<SelectState> {
  if (state.kind === 'idle') return transition(state);
  return transition(idle, { type: 'overlay', overlay: null });
}

// ---- 几何辅助 ----

export function hitObjectAtScreen(ctx: ToolContext, screen: Point): string | null {
  const doc = ctx.screenToDoc(screen);
  const hit = ctx.layout.pageAtPoint(doc);
  if (!hit) return null;
  const page = ctx.pages[hit.index];
  if (!page) return null;
  const tolerance = HIT_TOLERANCE_PX / cssPxPerPt(ctx.view.zoom);
  return ctx.hitTester.hitPoint(page, hit.local, tolerance)?.id ?? null;
}

function objectsInDocRect(ctx: ToolContext, rect: Rect): string[] {
  const [first, last] = ctx.layout.visiblePages(rect.y, rect.y + rect.height);
  const ids: string[] = [];
  for (let i = first; i < last; i++) {
    const page = ctx.pages[i];
    if (!page) continue;
    const pageRect = ctx.layout.pageRect(i);
    const local = translateRect(rect, -pageRect.x, -pageRect.y);
    for (const object of ctx.hitTester.hitRect(page, local, ctx.options.marqueeMode))
      ids.push(object.id);
  }
  return ids;
}

function rectsFor(ctx: ToolContext, ids: readonly string[]): Map<string, Rect> {
  const rects = new Map<string, Rect>();
  for (const id of ids) {
    const ref = ctx.objectById(id);
    if (ref) rects.set(id, ref.docRect);
  }
  return rects;
}

function selectionRects(ctx: ToolContext): Map<string, Rect> {
  return rectsFor(ctx, ctx.selection);
}

export function selectionDocRect(ctx: ToolContext): Rect | null {
  const rects = [...selectionRects(ctx).values()];
  return rects.length ? unionRects(rects) : null;
}

export function selectionScreenRect(ctx: ToolContext): Rect | null {
  const doc = selectionDocRect(ctx);
  if (!doc) return null;
  const a = ctx.docToScreen({ x: doc.x, y: doc.y });
  const b = ctx.docToScreen({ x: doc.x + doc.width, y: doc.y + doc.height });
  return rectFromPoints(a, b);
}

function marqueeScreenRect(ctx: ToolContext, a: Point, b: Point): Rect {
  return rectFromPoints(ctx.docToScreen(a), ctx.docToScreen(b));
}

function movePreview(state: Extract<SelectState, { kind: 'moving' }>): Map<string, Rect> {
  const dx = state.currentDoc.x - state.startDoc.x;
  const dy = state.currentDoc.y - state.startDoc.y;
  const out = new Map<string, Rect>();
  for (const [id, rect] of state.startRects) out.set(id, translateRect(rect, dx, dy));
  return out;
}

/** 拖到当前位置时，选区的包围盒变成什么样 */
function scaleTarget(state: Extract<SelectState, { kind: 'scaling' }>): Rect {
  const dx = state.currentDoc.x - state.startDoc.x;
  const dy = state.currentDoc.y - state.startDoc.y;
  return resizeRectByHandle(state.bbox, state.handle, dx, dy, state.keepRatio);
}

function scalePreview(state: Extract<SelectState, { kind: 'scaling' }>): Map<string, Rect> {
  const target = scaleTarget(state);
  const out = new Map<string, Rect>();
  for (const [id, rect] of state.startRects) out.set(id, mapRectBetween(rect, state.bbox, target));
  return out;
}

function angleTo(center: Point, p: Point): number {
  return (Math.atan2(p.y - center.y, p.x - center.x) * 180) / Math.PI;
}

function currentPage(ctx: ToolContext): number {
  const yMin = ctx.view.y;
  const yMax = ctx.view.y + ctx.viewport.height / cssPxPerPt(ctx.view.zoom);
  return ctx.layout.dominantPage(yMin, yMax);
}

function unique(ids: readonly string[]): string[] {
  return [...new Set(ids)];
}
