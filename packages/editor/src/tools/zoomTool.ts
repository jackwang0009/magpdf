import { distance, rectFromPoints } from '../geometry';
import type { Point } from '../geometry';
import { CSS_PX_PER_PT, clamp } from '../geometry';
import { MAX_ZOOM, MIN_ZOOM, nextZoomStep, zoomAt } from '../view/viewState';
import type { ViewState } from '../view/viewState';
import { ZOOM_IN_CURSOR, ZOOM_OUT_CURSOR } from './cursors';
import { transition } from './machine';
import type { ToolMachine } from './machine';

/** 缩放工具（Z）：点击放大一档、Alt+点击缩小一档、拖出矩形放大到该区域。 */
export type ZoomState =
  | { readonly kind: 'idle'; readonly alt: boolean }
  | { readonly kind: 'pressed'; readonly startScreen: Point; readonly alt: boolean }
  | { readonly kind: 'dragging'; readonly startScreen: Point; readonly currentScreen: Point };

const idle: ZoomState = { kind: 'idle', alt: false };

export const zoomTool: ToolMachine<ZoomState> = {
  id: 'zoom',
  initial: () => idle,
  transition(state, event, ctx) {
    switch (event.type) {
      case 'pointerdown':
        if (state.kind !== 'idle' || event.button !== 0) return transition(state);
        return transition<ZoomState>({
          kind: 'pressed',
          startScreen: event.screen,
          alt: event.alt,
        });
      case 'pointermove': {
        if (state.kind === 'idle') {
          return state.alt === event.alt
            ? transition(state)
            : transition<ZoomState>({ kind: 'idle', alt: event.alt });
        }
        if (state.kind === 'pressed') {
          if (distance(state.startScreen, event.screen) < ctx.options.dragThreshold)
            return transition(state);
          const next: ZoomState = {
            kind: 'dragging',
            startScreen: state.startScreen,
            currentScreen: event.screen,
          };
          return transition(next, {
            type: 'overlay',
            overlay: { zoomRect: rectFromPoints(next.startScreen, next.currentScreen) },
          });
        }
        const next: ZoomState = { ...state, currentScreen: event.screen };
        return transition(next, {
          type: 'overlay',
          overlay: { zoomRect: rectFromPoints(next.startScreen, next.currentScreen) },
        });
      }
      case 'pointerup': {
        if (state.kind === 'pressed') {
          const direction = state.alt ? -1 : 1;
          const view = zoomAt(ctx.view, event.screen, nextZoomStep(ctx.view.zoom, direction));
          return transition<ZoomState>({ kind: 'idle', alt: event.alt }, { type: 'setView', view });
        }
        if (state.kind === 'dragging') {
          const rect = rectFromPoints(state.startScreen, event.screen);
          const view = zoomToScreenRect(ctx.view, rect, ctx.viewport);
          return transition<ZoomState>(
            { kind: 'idle', alt: event.alt },
            { type: 'overlay', overlay: null },
            { type: 'setView', view },
          );
        }
        return transition(state);
      }
      case 'pointercancel':
      case 'cancel':
        return state.kind === 'dragging'
          ? transition(idle, { type: 'overlay', overlay: null })
          : transition(idle);
      case 'command':
        return transition(state);
    }
  },
  cursor(state) {
    const alt = state.kind === 'dragging' ? false : state.alt;
    return alt ? ZOOM_OUT_CURSOR : ZOOM_IN_CURSOR;
  },
};

/** 把屏幕矩形放大到充满视口 */
export function zoomToScreenRect(
  view: ViewState,
  rect: { x: number; y: number; width: number; height: number },
  viewport: { width: number; height: number },
): ViewState {
  if (rect.width < 2 || rect.height < 2) return view;
  const s = view.zoom * CSS_PX_PER_PT;
  const docX = view.x + rect.x / s;
  const docY = view.y + rect.y / s;
  const docW = rect.width / s;
  const docH = rect.height / s;
  const zoom = clamp(
    Math.min(viewport.width / (docW * CSS_PX_PER_PT), viewport.height / (docH * CSS_PX_PER_PT)),
    MIN_ZOOM,
    MAX_ZOOM,
  );
  const ns = zoom * CSS_PX_PER_PT;
  return {
    zoom,
    x: docX + docW / 2 - viewport.width / (2 * ns),
    y: docY + docH / 2 - viewport.height / (2 * ns),
  };
}
