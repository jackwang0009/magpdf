// @vitest-environment jsdom
import { act } from 'react';
import { createRoot } from 'react-dom/client';
import type { Root } from 'react-dom/client';
import { afterEach, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import type { DocumentEditor, StyleInfo, TextInfo } from '../../document/documentEditor';
import type { PageObject } from '../../document/types';
import { mmToPt } from '../../geometry';
import { format, t } from '../../i18n';
import { editorStore } from '../../store/editorStore';
import { EditorHostContext } from '../editorHost';
import type { EditorHost } from '../editorHost';
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

/** 宿主打开的文档：文字、颜色向修改入口读，改了交给它。 */
function withEditor(info: TextInfo | null, style: StyleInfo = { fill: null, stroke: null }) {
  const calls: [string, string][] = [];
  const fills: [readonly string[], string][] = [];
  const editor: DocumentEditor = {
    transformObjects: () => undefined,
    deleteObjects: () => undefined,
    placeComponent: () => undefined,
    updateComponent: () => undefined,
    readText: () => Promise.resolve(info),
    setText: (id, text) => calls.push([id, text]),
    readStyle: () => Promise.resolve(style),
    setFill: (ids, color) => fills.push([ids, color]),
    reorder: () => undefined,
    undo: () => undefined,
    redo: () => undefined,
    jumpHistory: () => undefined,
  };
  const host: EditorHost = {
    openDocument: () => Promise.resolve(),
    tileProvider: () => undefined,
    documentEditor: () => editor,
    onNotice: () => () => undefined,
  };
  return { host, calls, fills };
}

async function renderWith(host: EditorHost): Promise<HTMLDivElement> {
  container = document.createElement('div');
  document.body.append(container);
  root = createRoot(container);
  await act(async () =>
    root!.render(
      <EditorHostContext.Provider value={host}>
        <PropertiesPanel />
      </EditorHostContext.Provider>,
    ),
  );
  return container;
}

/** 第 1 页上放一个对象并选中它。 */
function addObject(object: PageObject) {
  act(() => {
    const state = editorStore.getState();
    const snapshot = state.documents['props-doc']!.snapshot;
    state.updateSnapshot('props-doc', {
      ...snapshot,
      pages: [{ ...snapshot.pages[0]!, objects: [object] }],
    });
    state.setSelection('props-doc', [object.id]);
  });
}

function placed() {
  return editorStore.getState().documents['props-doc']!.snapshot.pages[0]!.objects[0]!;
}

describe('属性面板：位置和大小', () => {
  function addRect() {
    act(() => {
      const state = editorStore.getState();
      const snapshot = state.documents['props-doc']!.snapshot;
      state.updateSnapshot('props-doc', {
        ...snapshot,
        pages: [
          {
            ...snapshot.pages[0]!,
            objects: [
              {
                id: 'r1',
                type: 'path',
                name: '框',
                bbox: { x: 72, y: 72, width: 144, height: 72 },
              },
            ],
          },
        ],
      });
      state.setSelection('props-doc', ['r1']);
    });
  }
  const field = (view: HTMLElement, label: string) =>
    view.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`)!;
  const bbox = () =>
    editorStore.getState().documents['props-doc']!.snapshot.pages[0]!.objects[0]!.bbox;

  it('X、Y、宽、高按 mm 显示；改了回车生效，写错了恢复原值', () => {
    addRect();
    const view = render();
    expect(field(view, t.options.x).value).toBe('25.40');
    expect(field(view, t.options.w).value).toBe('50.80');

    type(field(view, t.options.x), '50');
    act(() =>
      field(view, t.options.x).dispatchEvent(
        new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }),
      ),
    );
    expect(bbox().x).toBeCloseTo(mmToPt(50), 6);
    expect(field(view, t.options.x).value).toBe('50.00');

    type(field(view, t.options.w), '25.4');
    act(() =>
      field(view, t.options.w).dispatchEvent(new FocusEvent('focusout', { bubbles: true })),
    );
    expect(bbox().width).toBeCloseTo(72, 6);
    expect(bbox().x).toBeCloseTo(mmToPt(50), 6);

    type(field(view, t.options.h), '-3');
    act(() =>
      field(view, t.options.h).dispatchEvent(new FocusEvent('focusout', { bubbles: true })),
    );
    expect(bbox().height).toBe(72);
    expect(field(view, t.options.h).value).toBe('25.40');
  });

  it('组件的宽、高不能直接改；只能叠加内容的页上原有的对象什么都不能改', () => {
    act(() => editorStore.getState().placeComponent('props-doc', 0, 'code128', { x: 30, y: 40 }));
    let view = render();
    expect(field(view, t.options.x).disabled).toBe(false);
    expect(field(view, t.options.w).disabled).toBe(true);
    expect(view.textContent).toContain(t.properties.componentSizeHint);
    act(() => root?.unmount());
    container?.remove();

    addRect();
    act(() => {
      const state = editorStore.getState();
      const snapshot = state.documents['props-doc']!.snapshot;
      state.updateSnapshot('props-doc', {
        ...snapshot,
        pages: [{ ...snapshot.pages[0]!, overlayOnly: { reason: '测试' } }],
      });
    });
    view = render();
    for (const label of [t.options.x, t.options.y, t.options.w, t.options.h]) {
      expect(field(view, label).disabled).toBe(true);
    }
  });
});

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

describe('属性面板：改原有文字', () => {
  function addText(object: Partial<PageObject> = {}) {
    act(() => {
      const state = editorStore.getState();
      const snapshot = state.documents['props-doc']!.snapshot;
      state.updateSnapshot('props-doc', {
        ...snapshot,
        pages: [
          {
            ...snapshot.pages[0]!,
            objects: [
              {
                id: 't1',
                type: 'text',
                name: '文字',
                bbox: { x: 72, y: 72, width: 144, height: 12 },
                ...object,
              },
            ],
          },
        ],
      });
      state.setSelection('props-doc', ['t1']);
    });
  }
  const textInput = (view: HTMLElement) =>
    view.querySelector<HTMLInputElement>('#mg-text-content')!;

  it('引擎的文档：读出文字、字体、字号；空的不发，改了回车交给引擎', async () => {
    addText();
    const { host, calls } = withEditor({
      content: 'BL NO. 0042',
      font: 'Helvetica',
      size: 10,
      editable: true,
    });
    const view = await renderWith(host);
    expect(textInput(view).value).toBe('BL NO. 0042');
    expect(view.textContent).toContain('Helvetica');
    expect(view.textContent).toContain('10 pt');

    type(textInput(view), '');
    act(() => textInput(view).dispatchEvent(new FocusEvent('focusout', { bubbles: true })));
    expect(view.querySelector('[role="alert"]')?.textContent).toContain(t.properties.textEmpty);
    expect(calls).toEqual([]);

    type(textInput(view), 'BL NO. 0043');
    act(() =>
      textInput(view).dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })),
    );
    expect(calls).toEqual([['t1', 'BL NO. 0043']]);
    // 回车之后离开输入框不再发一遍
    act(() => textInput(view).dispatchEvent(new FocusEvent('focusout', { bubbles: true })));
    expect(calls).toHaveLength(1);
  });

  it('不能改的文字：输入框禁用，显示原因；不是文字（引擎说的）就不显示这一节', async () => {
    addText();
    const blocked = withEditor({
      content: 'ABC',
      font: 'T3',
      size: 9,
      editable: false,
      reason: '这段文字用的是 Type 3 字体',
    });
    let view = await renderWith(blocked.host);
    expect(textInput(view).disabled).toBe(true);
    expect(view.textContent).toContain('Type 3');
    act(() => root?.unmount());
    container?.remove();

    view = await renderWith(withEditor(null).host);
    expect(textInput(view)).toBeNull();
  });

  it('模拟文档：外观里的文字，改了落在 store 上；只能叠加内容的页上不能改', () => {
    addText({
      appearance: { kind: 'text', text: 'SHIPPER', fontSize: 9, color: '#000' },
    });
    const view = render();
    expect(textInput(view).value).toBe('SHIPPER');
    type(textInput(view), 'CONSIGNEE');
    act(() =>
      textInput(view).dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })),
    );
    const appearance = placed().appearance;
    expect(appearance?.kind === 'text' && appearance.text).toBe('CONSIGNEE');
    expect(editorStore.getState().documents['props-doc']!.history.at(-1)).toBe(t.history.setText);

    act(() => {
      const state = editorStore.getState();
      const snapshot = state.documents['props-doc']!.snapshot;
      state.updateSnapshot('props-doc', {
        ...snapshot,
        pages: [{ ...snapshot.pages[0]!, overlayOnly: { reason: '测试' } }],
      });
    });
    expect(textInput(view).disabled).toBe(true);
  });
});

describe('属性面板：颜色', () => {
  const box = { x: 72, y: 72, width: 144, height: 72 };
  const fillInput = (view: HTMLElement) => view.querySelector<HTMLInputElement>('#mg-fill');
  const press = (input: HTMLInputElement, key: string) =>
    act(() => input.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true })));

  it('引擎的文档：读出填充色和描边色；写错了不发、标出原因，写对了回车交给引擎，不重复发', async () => {
    addObject({ id: 'r1', type: 'path', name: '框', bbox: box });
    const { host, fills } = withEditor(null, { fill: '#cc3333', stroke: '#000000' });
    const view = await renderWith(host);
    const input = fillInput(view)!;
    expect(input.value).toBe('#cc3333');
    expect(view.textContent).toContain(t.properties.stroke);
    expect(view.textContent).toContain('#000000');

    type(input, '红色');
    press(input, 'Enter');
    expect(view.querySelector('[role="alert"]')?.textContent).toContain(t.properties.colorInvalid);
    expect(fills).toEqual([]);

    type(input, '1A6FD0');
    press(input, 'Enter');
    expect(fills).toEqual([[['r1'], '#1a6fd0']]);
    expect(input.value).toBe('#1a6fd0');
    act(() => input.dispatchEvent(new FocusEvent('focusout', { bubbles: true })));
    expect(fills).toHaveLength(1);
  });

  it('不填充也不描边的对象（图像）、组件不显示颜色', async () => {
    addObject({ id: 'i1', type: 'image', name: '图', bbox: box });
    let view = await renderWith(withEditor(null).host);
    expect(view.textContent).not.toContain(t.properties.colors);
    act(() => root?.unmount());
    container?.remove();

    act(() => editorStore.getState().placeComponent('props-doc', 0, 'code128', { x: 30, y: 40 }));
    view = await renderWith(withEditor(null, { fill: '#000000', stroke: null }).host);
    expect(fillInput(view)).toBeNull();
  });

  it('模拟文档：方块的填充色改了落在 store 上；只能叠加内容的页上不能改', () => {
    addObject({
      id: 'r1',
      type: 'path',
      name: '框',
      bbox: box,
      appearance: { kind: 'rect', fill: '#ff0000' },
    });
    const view = render();
    const input = fillInput(view)!;
    expect(input.value).toBe('#ff0000');
    type(input, '#00FF00');
    press(input, 'Enter');
    const appearance = placed().appearance;
    expect(appearance?.kind === 'rect' && appearance.fill).toBe('#00ff00');
    expect(editorStore.getState().documents['props-doc']!.history.at(-1)).toBe(
      format(t.history.setFill, { n: 1 }),
    );

    act(() => {
      const state = editorStore.getState();
      const snapshot = state.documents['props-doc']!.snapshot;
      state.updateSnapshot('props-doc', {
        ...snapshot,
        pages: [{ ...snapshot.pages[0]!, overlayOnly: { reason: '测试' } }],
      });
    });
    expect(fillInput(view)!.disabled).toBe(true);
  });
});
