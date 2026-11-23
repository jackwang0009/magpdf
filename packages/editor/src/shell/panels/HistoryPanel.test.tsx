// @vitest-environment jsdom
import { act } from 'react';
import { createRoot } from 'react-dom/client';
import type { Root } from 'react-dom/client';
import { afterEach, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import type { DocumentEditor } from '../../document/documentEditor';
import { t } from '../../i18n';
import { editorStore } from '../../store/editorStore';
import { EditorHostContext } from '../editorHost';
import type { EditorHost } from '../editorHost';
import { HistoryPanel } from './PlaceholderPanels';

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
    id: 'history-doc',
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
  editorStore.getState().closeDocument('history-doc');
});

function render(host: EditorHost | null): HTMLDivElement {
  container = document.createElement('div');
  document.body.append(container);
  root = createRoot(container);
  act(() =>
    root!.render(
      <EditorHostContext.Provider value={host}>
        <HistoryPanel />
      </EditorHostContext.Provider>,
    ),
  );
  return container;
}

function hostWith(jumps: number[]): EditorHost {
  const editor: DocumentEditor = {
    transformObjects: () => undefined,
    deleteObjects: () => undefined,
    placeComponent: () => undefined,
    updateComponent: () => undefined,
    readText: () => Promise.resolve(null),
    setText: () => undefined,
    undo: () => undefined,
    redo: () => undefined,
    jumpHistory: (index) => jumps.push(index),
  };
  return {
    openDocument: () => Promise.resolve(),
    tileProvider: () => undefined,
    documentEditor: () => editor,
    onNotice: () => () => undefined,
  };
}

const steps = (view: HTMLElement) =>
  [...view.querySelectorAll<HTMLElement>('.mg-history-step')].map((step) => ({
    label: step.textContent,
    on: step.classList.contains('mg-on'),
    undone: step.classList.contains('mg-undone'),
  }));

describe('历史记录面板', () => {
  it('最新的在上面；现在的位置高亮，撤销了的几步划掉；点一条跳过去，点现在这条不发', () => {
    act(() =>
      editorStore
        .getState()
        .setHistory('history-doc', [t.history.opened, '移动 1 个对象', '删除 1 个对象'], 1),
    );
    const jumps: number[] = [];
    const view = render(hostWith(jumps));
    expect(steps(view)).toEqual([
      { label: '删除 1 个对象', on: false, undone: true },
      { label: '移动 1 个对象', on: true, undone: false },
      { label: t.history.opened, on: false, undone: false },
    ]);
    const buttons = [...view.querySelectorAll<HTMLButtonElement>('button.mg-history-step')];
    act(() => buttons[2]!.click());
    act(() => buttons[0]!.click());
    expect(jumps).toEqual([0, 2]);
    expect(buttons[1]!.disabled).toBe(true);
  });

  it('模拟文档：只列出来，最后一条是现在的位置，点不了', () => {
    const view = render(null);
    expect(view.querySelector('button')).toBeNull();
    expect(steps(view)).toEqual([{ label: t.history.opened, on: true, undone: false }]);
  });
});
