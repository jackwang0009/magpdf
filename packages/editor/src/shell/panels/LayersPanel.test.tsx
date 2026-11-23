// @vitest-environment jsdom
import { act } from 'react';
import { createRoot } from 'react-dom/client';
import type { Root } from 'react-dom/client';
import { afterEach, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import type { DocumentEditor } from '../../document/documentEditor';
import type { PageObject } from '../../document/types';
import type { ZOrderTarget } from '../../document/zOrder';
import { t } from '../../i18n';
import { editorStore } from '../../store/editorStore';
import { EditorHostContext } from '../editorHost';
import type { EditorHost } from '../editorHost';
import { LayersPanel } from './LayersPanel';

declare global {
  // React 要求测试环境显式声明支持 act()
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

beforeAll(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
});

let root: Root | null = null;
let container: HTMLDivElement | null = null;

const box = { x: 0, y: 0, width: 10, height: 10 };
/** 从下到上：框 a、文字 b、框 c */
const objects: PageObject[] = [
  { id: 'a', type: 'path', name: '框 a', bbox: box },
  { id: 'b', type: 'text', name: '标题 b', bbox: box },
  { id: 'c', type: 'path', name: '框 c', bbox: box },
];

function openDoc(overlayOnly = false) {
  editorStore.getState().openDocument({
    id: 'layers-doc',
    name: '测试.pdf',
    dirty: false,
    version: 0,
    pages: [
      {
        index: 0,
        width: 600,
        height: 800,
        objects,
        ...(overlayOnly ? { overlayOnly: { reason: '测试' } } : {}),
      },
    ],
  });
}

beforeEach(() => openDoc());

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  root = null;
  container = null;
  editorStore.getState().closeDocument('layers-doc');
});

function render(host: EditorHost | null = null): HTMLDivElement {
  container = document.createElement('div');
  document.body.append(container);
  root = createRoot(container);
  act(() =>
    root!.render(
      <EditorHostContext.Provider value={host}>
        <LayersPanel />
      </EditorHostContext.Provider>,
    ),
  );
  return container;
}

const rows = (view: HTMLElement) =>
  [...view.querySelectorAll<HTMLElement>('.mg-layer-name')].map((name) => name.textContent);
const row = (view: HTMLElement, name: string) =>
  [...view.querySelectorAll<HTMLLIElement>('.mg-layers li')].find(
    (li) => li.querySelector('.mg-layer-name')?.textContent === name,
  )!;
const order = () =>
  editorStore.getState().documents['layers-doc']!.snapshot.pages[0]!.objects.map((o) => o.id);

/** 拖放事件：jsdom 没有 DragEvent，用普通事件带上 clientY；行的位置按 top 100、高 20 算。 */
function fire(target: Element, type: string, clientY = 0) {
  const event = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperty(event, 'clientY', { value: clientY });
  act(() => {
    target.dispatchEvent(event);
  });
}
function drag(view: HTMLElement, from: string, to: string, above: boolean) {
  const target = row(view, to);
  target.getBoundingClientRect = () => ({ top: 100, height: 20 }) as DOMRect;
  const handle = row(view, from).querySelector('button.mg-layer')!;
  fire(handle, 'dragstart');
  const y = above ? 104 : 116;
  fire(target, 'dragover', y);
  fire(target, 'drop', y);
  fire(handle, 'dragend');
}

function recordingHost() {
  const calls: [readonly string[], ZOrderTarget][] = [];
  const editor: DocumentEditor = {
    transformObjects: () => undefined,
    deleteObjects: () => undefined,
    placeComponent: () => undefined,
    updateComponent: () => undefined,
    readText: () => Promise.resolve(null),
    setText: () => undefined,
    readStyle: () => Promise.resolve({ fill: null, stroke: null }),
    setFill: () => undefined,
    reorder: (ids, op) => calls.push([ids, op]),
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
  return { host, calls };
}

describe('图层面板：筛选', () => {
  it('按名字或种类筛，大小写不分；对不上时说一声；Esc 清掉', () => {
    const view = render();
    expect(rows(view)).toEqual(['框 c', '标题 b', '框 a']);
    const input = view.querySelector<HTMLInputElement>('.mg-panel-search-input')!;
    const type = (value: string) => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!;
      act(() => {
        setter.call(input, value);
        input.dispatchEvent(new Event('input', { bubbles: true }));
      });
    };
    type('框');
    expect(rows(view)).toEqual(['框 c', '框 a']);
    type(t.objectTypes.text);
    expect(rows(view)).toEqual(['标题 b']);
    type('没有这个');
    expect(rows(view)).toEqual([]);
    expect(view.textContent).toContain(t.layers.noMatch);
    act(() => {
      input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    });
    expect(rows(view)).toHaveLength(3);
  });
});

describe('图层面板：拖动改层级', () => {
  it('模拟文档：框 a 拖到框 c 上面，到了最上面；拖到标题 b 下面，回到最下面', () => {
    const view = render();
    drag(view, '框 a', '框 c', true);
    expect(order()).toEqual(['b', 'c', 'a']);
    expect(editorStore.getState().documents['layers-doc']!.selection).toEqual(['a']);
    drag(view, '框 a', '标题 b', false);
    expect(order()).toEqual(['a', 'b', 'c']);
  });

  it('引擎的文档：交给修改入口，带的是没被拖的对象里数的位置；选中的几行一起拖', () => {
    const { host, calls } = recordingHost();
    const view = render(host);
    drag(view, '框 a', '框 c', true);
    act(() => editorStore.getState().setSelection('layers-doc', ['a', 'b']));
    drag(view, '标题 b', '框 c', false);
    expect(calls).toEqual([
      [['a'], { index: 2 }],
      [['a', 'b'], { index: 0 }],
    ]);
    // 界面上不先改，新顺序随对象列表回来
    expect(order()).toEqual(['a', 'b', 'c']);
  });

  it('隐藏、锁定的行变灰，点不了、拖不动；开关再点一下恢复；隐藏、锁定的从选区里去掉', () => {
    act(() => editorStore.getState().setSelection('layers-doc', ['a', 'b']));
    const view = render();
    const toggle = (name: string, which: 0 | 1) =>
      row(view, name).querySelectorAll<HTMLButtonElement>('.mg-layer-toggle')[which]!;
    act(() => toggle('框 a', 0).click());
    act(() => toggle('标题 b', 1).click());
    const state = () => editorStore.getState().documents['layers-doc']!;
    expect(state().hidden).toEqual(['a']);
    expect(state().locked).toEqual(['b']);
    expect(state().selection).toEqual([]);
    expect(toggle('框 a', 0).getAttribute('aria-pressed')).toBe('false');
    expect(row(view, '框 a').querySelector('button.mg-layer')).toBeNull();
    expect(row(view, '标题 b').querySelector('.mg-layer-locked')).not.toBeNull();
    expect(view.querySelectorAll('button[draggable="true"]')).toHaveLength(1);

    act(() => toggle('框 a', 0).click());
    act(() => toggle('标题 b', 1).click());
    expect(state().hidden).toEqual([]);
    expect(state().locked).toEqual([]);
    expect(view.querySelectorAll('button[draggable="true"]')).toHaveLength(3);
  });

  it('只能叠加内容的页、筛选的时候不能拖', () => {
    editorStore.getState().closeDocument('layers-doc');
    openDoc(true);
    let view = render();
    expect(view.querySelector('button[draggable="true"]')).toBeNull();
    act(() => root?.unmount());
    container?.remove();

    editorStore.getState().closeDocument('layers-doc');
    openDoc();
    view = render();
    expect(view.querySelectorAll('button[draggable="true"]')).toHaveLength(3);
    const input = view.querySelector<HTMLInputElement>('.mg-panel-search-input')!;
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!;
    act(() => {
      setter.call(input, '框');
      input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    expect(view.querySelector('button[draggable="true"]')).toBeNull();
  });
});
