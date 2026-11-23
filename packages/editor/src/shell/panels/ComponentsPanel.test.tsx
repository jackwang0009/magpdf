// @vitest-environment jsdom
import { act } from 'react';
import { createRoot } from 'react-dom/client';
import type { Root } from 'react-dom/client';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import { COMPONENT_DRAG_TYPE } from '../../document/components';
import { format, t } from '../../i18n';
import { editorStore } from '../../store/editorStore';
import { ComponentsPanel } from './ComponentsPanel';

declare global {
  // React 要求测试环境显式声明支持 act()
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

beforeAll(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
});

let root: Root | null = null;
let container: HTMLDivElement | null = null;

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  root = null;
  container = null;
  editorStore.getState().setPlacing(null);
});

function render(): HTMLDivElement {
  container = document.createElement('div');
  document.body.append(container);
  root = createRoot(container);
  act(() => root!.render(<ComponentsPanel />));
  return container;
}

function items(view: HTMLElement, name: string): HTMLButtonElement[] {
  return [...view.querySelectorAll<HTMLButtonElement>('.mg-comp-item')].filter(
    (b) => b.querySelector('.mg-comp-name')?.textContent === name,
  );
}

describe('组件面板', () => {
  it('文本和 Code 128 能点：点一下进入放置状态，再点一下取消；别的组件是灰的', () => {
    const view = render();
    const [code128] = items(view, t.components.items.code128);
    expect(code128!.disabled).toBe(false);
    expect(code128!.title).toBe(format(t.components.placeHint, { name: 'Code 128' }));
    act(() => code128!.click());
    expect(editorStore.getState().placing).toBe('code128');
    // 同一个组件出现在几个分类里，都亮起来
    for (const button of items(view, t.components.items.code128)) {
      expect(button.getAttribute('aria-pressed')).toBe('true');
    }
    act(() => items(view, t.components.items.text)[0]!.click());
    expect(editorStore.getState().placing).toBe('text');
    act(() => items(view, t.components.items.text)[0]!.click());
    expect(editorStore.getState().placing).toBeNull();

    const [qr] = items(view, t.components.items.qr);
    expect(qr!.disabled).toBe(true);
    expect(qr!.title).toBe(format(t.components.notYet, { name: t.components.items.qr }));
  });

  it('拖出去的时候带上组件种类', () => {
    const view = render();
    const [text] = items(view, t.components.items.text);
    expect(text!.draggable).toBe(true);
    const data = new Map<string, string>();
    const event = new Event('dragstart', { bubbles: true });
    Object.defineProperty(event, 'dataTransfer', {
      value: { setData: (k: string, v: string) => data.set(k, v), effectAllowed: '' },
    });
    act(() => text!.dispatchEvent(event));
    expect(data.get(COMPONENT_DRAG_TYPE)).toBe('text');
  });
});
