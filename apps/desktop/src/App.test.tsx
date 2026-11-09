import { act } from 'react';
import { createRoot } from 'react-dom/client';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';

import { App } from './App';

declare global {
  // React 要求测试环境显式声明支持 act()
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

/**
 * jsdom 没有 ResizeObserver 和 WebGL/2D 上下文；这里给最小桩，
 * 让外壳挂载路径（含画布控制器）走到“WebGL2 不可用”的分支而不出错。
 */
beforeAll(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  class StubResizeObserver {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  }
  globalThis.ResizeObserver ??= StubResizeObserver as unknown as typeof ResizeObserver;
  // 只替身 WebGL2（返回 null 让画布显示“不可用”），2D 上下文交给 jsdom，dockview 的尺寸测量要用
  const original = HTMLCanvasElement.prototype.getContext;
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(function (
    this: HTMLCanvasElement,
    contextId: string,
    ...rest: unknown[]
  ) {
    if (contextId === 'webgl2') return null;
    return (original as (...args: unknown[]) => unknown).call(this, contextId, ...rest);
  } as unknown as HTMLCanvasElement['getContext']);
});

describe('App', () => {
  let container: HTMLDivElement;

  afterEach(() => {
    container.remove();
  });

  it('渲染出外壳：工具栏 7 个 v1.0 工具、菜单栏和状态栏', async () => {
    container = document.createElement('div');
    document.body.appendChild(container);
    const root = createRoot(container);
    await act(async () => {
      root.render(<App />);
    });
    const toolbar = container.querySelector('.mg-toolbar');
    expect(toolbar).not.toBeNull();
    expect(toolbar!.querySelectorAll('.mg-tool')).toHaveLength(7);
    expect(container.querySelector('.mg-menubar')?.textContent).toContain('MagPDF');
    expect(container.querySelectorAll('.mg-menu')).toHaveLength(8);
    expect(container.querySelector('.mg-options')).not.toBeNull();
    expect(container.querySelector('.mg-status')).not.toBeNull();
    await act(async () => {
      root.unmount();
    });
  });
});
