import { act } from 'react';
import { createRoot } from 'react-dom/client';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import type { EditorHost } from '@magpdf/editor';

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
    // 工具栏横排在顶部那一行里，不在工作台（dockview）里；填充/描边色块在行尾
    const topbar = container.querySelector('.mg-topbar');
    expect(topbar).not.toBeNull();
    expect(topbar!.contains(toolbar)).toBe(true);
    expect(topbar!.querySelector('.mg-swatches')).not.toBeNull();
    expect(container.querySelector('.mg-workbench .mg-toolbar')).toBeNull();
    expect(container.querySelector('.mg-menubar')?.textContent).toContain('MagPDF');
    expect(container.querySelectorAll('.mg-menu')).toHaveLength(8);
    expect(container.querySelector('.mg-options')).not.toBeNull();
    expect(container.querySelector('.mg-status')).not.toBeNull();
    await act(async () => {
      root.unmount();
    });
  });

  it('接了宿主之后，「打开」交给宿主；打不开时把原因告诉用户', async () => {
    const openDocument = vi
      .fn<EditorHost['openDocument']>()
      .mockRejectedValueOnce({ code: 2002, message: '需要打开密码' })
      .mockResolvedValue(undefined);
    const host: EditorHost = {
      openDocument,
      tileProvider: () => undefined,
      onNotice: () => () => undefined,
    };
    container = document.createElement('div');
    document.body.appendChild(container);
    const root = createRoot(container);
    await act(async () => {
      root.render(<App host={host} />);
    });

    const pressOpen = () =>
      act(async () => {
        window.dispatchEvent(new KeyboardEvent('keydown', { key: 'o', ctrlKey: true }));
      });
    await pressOpen();
    expect(openDocument).toHaveBeenCalledTimes(1);
    const notice = container.querySelector('.mg-notice');
    expect(notice?.getAttribute('data-kind')).toBe('error');
    expect(notice?.querySelector('.mg-notice-message')?.textContent).toBe(
      '这个文件打不开：需要打开密码',
    );

    await act(async () => {
      notice?.querySelector('button')?.click();
    });
    expect(container.querySelector('.mg-notice')).toBeNull();

    // 第二次打开成功（或者用户取消了）：不弹提示
    await pressOpen();
    expect(openDocument).toHaveBeenCalledTimes(2);
    expect(container.querySelector('.mg-notice')).toBeNull();
    await act(async () => {
      root.unmount();
    });
  });
});
