import { act } from 'react';
import { createRoot } from 'react-dom/client';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';

import { App } from './App';

declare global {
  // React 要求测试环境显式声明支持 act()
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

describe('App', () => {
  let container: HTMLDivElement;

  beforeAll(() => {
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  });

  afterEach(() => {
    container.remove();
  });

  it('渲染出 v1.0 的 7 个工具按钮', async () => {
    container = document.createElement('div');
    document.body.appendChild(container);
    const root = createRoot(container);
    await act(async () => {
      root.render(<App />);
    });
    const buttons = container.querySelectorAll('nav button');
    expect(buttons.length).toBe(7);
    expect(container.textContent).toContain('MagPDF');
    await act(async () => {
      root.unmount();
    });
  });
});
