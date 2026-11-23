// @vitest-environment jsdom
import { act } from 'react';
import { createRoot } from 'react-dom/client';
import type { Root } from 'react-dom/client';
import { afterEach, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { t } from '../../i18n';
import { editorStore } from '../../store/editorStore';
import { PropertiesPanel } from './PropertiesPanel';

declare global {
  // React 要求测试环境显式声明支持 act()
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

beforeAll(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
});

let root: Root | null = null;
let container: HTMLDivElement | null = null;

beforeEach(() => {
  editorStore.getState().openDocument({
    id: 'props-doc',
    name: '测试.pdf',
    dirty: false,
    version: 0,
    pages: [{ index: 0, width: 600, height: 800, objects: [] }],
  });
});

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  root = null;
  container = null;
  editorStore.getState().closeDocument('props-doc');
});

function render(): HTMLDivElement {
  container = document.createElement('div');
  document.body.append(container);
  root = createRoot(container);
  act(() => root!.render(<PropertiesPanel />));
  return container;
}

/** 像用户那样改输入框：React 认的是原生 setter 加 input 事件。 */
function type(input: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!;
  act(() => {
    setter.call(input, value);
    input.dispatchEvent(new Event('input', { bubbles: true }));
  });
}

function placed() {
  return editorStore.getState().documents['props-doc']!.snapshot.pages[0]!.objects[0]!;
}

describe('属性面板：组件参数', () => {
  it('选中置入的条码：显示内容和条高；不合法的不改，标出原因；合法的回车生效', () => {
    act(() => editorStore.getState().placeComponent('props-doc', 0, 'code128', { x: 30, y: 40 }));
    const view = render();
    const content = view.querySelector<HTMLInputElement>('#mg-comp-content')!;
    const size = view.querySelector<HTMLInputElement>('#mg-comp-size')!;
    expect(content.value).toBe('SAMPLE-0001');
    expect(size.value).toBe('36');
    expect(view.textContent).toContain(t.properties.barHeight);

    type(content, '提单号');
    act(() => content.dispatchEvent(new FocusEvent('focusout', { bubbles: true })));
    expect(view.querySelector('[role="alert"]')?.textContent).toContain(
      t.componentChecks.code128AsciiOnly,
    );
    expect(content.className).toContain('mg-bad');
    expect(placed().component?.values.content).toBe('SAMPLE-0001');

    type(content, 'BL-42');
    type(size, '20');
    act(() => content.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })));
    expect(placed().component?.values).toEqual({ content: 'BL-42', size: 20 });
    expect(view.querySelector('[role="alert"]')).toBeNull();
    // 面板换成了新参数
    expect(view.querySelector<HTMLInputElement>('#mg-comp-content')!.value).toBe('BL-42');
  });

  it('只能叠加内容的页上：新加的组件照样能改，不显示只读提示', () => {
    act(() => {
      const state = editorStore.getState();
      const snapshot = state.documents['props-doc']!.snapshot;
      state.updateSnapshot('props-doc', {
        ...snapshot,
        pages: [{ ...snapshot.pages[0]!, overlayOnly: { reason: '测试' } }],
      });
      state.placeComponent('props-doc', 0, 'text', { x: 30, y: 40 });
    });
    const view = render();
    expect(view.querySelector('.mg-props-readonly')).toBeNull();
    expect(view.querySelector<HTMLInputElement>('#mg-comp-content')!.disabled).toBe(false);
    expect(view.textContent).toContain(t.properties.fontSize);
  });
});
