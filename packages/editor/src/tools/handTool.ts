import type { Point } from '../geometry';
import { panBy } from '../view/viewState';
import type { ViewState } from '../view/viewState';
import { transition } from './machine';
import type { ToolMachine } from './machine';

/** 抓手工具（H）：按下拖动平移视图。Space 临时抓手与中键拖动复用同一状态机。 */
export type HandState =
  | { readonly kind: 'idle' }
  | { readonly kind: 'panning'; readonly startScreen: Point; readonly startView: ViewState };

const idle: HandState = { kind: 'idle' };

export const handTool: ToolMachine<HandState> = {
  id: 'hand',
  initial: () => idle,
  transition(state, event, ctx) {
    switch (event.type) {
      case 'pointerdown':
        if (state.kind !== 'idle') return transition(state);
        return transition<HandState>({
          kind: 'panning',
          startScreen: event.screen,
          startView: ctx.view,
        });
      case 'pointermove': {
        if (state.kind !== 'panning') return transition(state);
        const dx = event.screen.x - state.startScreen.x;
        const dy = event.screen.y - state.startScreen.y;
        return transition(state, { type: 'setView', view: panBy(state.startView, -dx, -dy) });
      }
      case 'pointerup':
      case 'pointercancel':
      case 'cancel':
        return transition(idle);
      case 'command':
        return transition(state);
    }
  },
  cursor(state) {
    return state.kind === 'panning' ? 'grabbing' : 'grab';
  },
};
