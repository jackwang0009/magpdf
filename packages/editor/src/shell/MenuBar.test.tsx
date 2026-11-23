// @vitest-environment jsdom
import { act } from 'react';
import { createRoot } from 'react-dom/client';
import type { Root } from 'react-dom/client';
import { afterEach, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import type { DocumentEditor } from '../document/documentEditor';
import { t } from '../i18n';
import { editorStore } from '../store/editorStore';
import { EditorHostContext } from './editorHost';
import type { EditorHost } from './editorHost';
import { MenuBar } from './MenuBar';
import { ShellContext } from './shellContext';
import type { ShellCommands } from './shellContext';

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
    id: 'menu-doc',
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
  editorStore.getState().closeDocument('menu-doc');
});

/** 引擎打开的文档的宿主：只要有修改入口就行。 */
const engineHost: EditorHost = {
  openDocument: () => Promise.resolve(),
  tileProvider: () => undefined,
  documentEditor: () => ({}) as DocumentEditor,
  onNotice: () => () => undefined,
};

function openEditMenu(host: EditorHost | null): HTMLDivElement {
  container = document.createElement('div');
  document.body.append(container);
  root = createRoot(container);
  act(() =>
    root!.render(
      <ShellContext.Provider value={{} as ShellCommands}>
        <EditorHostContext.Provider value={host}>
          <MenuBar workspace="freight" showRulers showFrameStats={false} />
        </EditorHostContext.Provider>
      </ShellContext.Provider>,
    ),
  );
  const edit = [...container.querySelectorAll<HTMLButtonElement>('.mg-menu')].find(
    (button) => button.textContent === t.menu.edit,
  )!;
  act(() => edit.click());
  return container;
}

const item = (view: HTMLElement, label: string) =>
  [...view.querySelectorAll<HTMLButtonElement>('.mg-menu-item')].find(
    (button) => button.querySelector('.mg-menu-label')?.textContent === label,
  )!;

describe('菜单栏：撤销、重做随历史记录变灰', () => {
  it('引擎的文档：有可撤销的才能撤销，有撤销了的才能重做', () => {
    const history = [t.history.opened, '移动 1 个对象', '删除 1 个对象'];
    act(() => editorStore.getState().setHistory('menu-doc', history, 1));
    const view = openEditMenu(engineHost);
    expect(item(view, t.menuItems.undo).disabled).toBe(false);
    expect(item(view, t.menuItems.redo).disabled).toBe(false);

    act(() => editorStore.getState().setHistory('menu-doc', history, 0));
    expect(item(view, t.menuItems.undo).disabled).toBe(true);
    expect(item(view, t.menuItems.redo).disabled).toBe(false);

    act(() => editorStore.getState().setHistory('menu-doc', history, 2));
    expect(item(view, t.menuItems.undo).disabled).toBe(false);
    expect(item(view, t.menuItems.redo).disabled).toBe(true);
  });

  it('模拟文档没有撤销：两项都是灰的', () => {
    const view = openEditMenu(null);
    expect(item(view, t.menuItems.undo).disabled).toBe(true);
    expect(item(view, t.menuItems.redo).disabled).toBe(true);
  });
});
