// @vitest-environment jsdom
import { act } from 'react';
import { createRoot } from 'react-dom/client';
import type { Root } from 'react-dom/client';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import { t } from '../i18n';
import { RecoveryDialog } from './Dialogs';

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
});

function render(onResolve: (ids: readonly string[]) => void): HTMLDivElement {
  container = document.createElement('div');
  document.body.append(container);
  root = createRoot(container);
  act(() =>
    root!.render(
      <RecoveryDialog
        documents={[
          { id: '0', name: '提单.pdf', modifiedMs: Date.UTC(2026, 10, 19, 1, 2), unsaved: true },
          { id: '1', name: '合同.pdf', modifiedMs: 0, unsaved: false },
        ]}
        onResolve={onResolve}
      />,
    ),
  );
  return container;
}

function button(within: HTMLElement, label: string): HTMLButtonElement {
  const found = [...within.querySelectorAll('button')].find((b) => b.textContent === label);
  if (!found) throw new Error(`没有按钮 ${label}`);
  return found;
}

describe('崩溃恢复对话框', () => {
  it('列出文件名，默认都选上；取消一份之后只重新打开另一份', () => {
    const answers: (readonly string[])[] = [];
    const view = render((ids) => answers.push(ids));
    expect(view.textContent).toContain(t.recovery.message);
    // 有自动备份的标出来：没保存的修改能一起恢复
    const marks = [...view.querySelectorAll('tbody tr')].map(
      (row) => row.lastElementChild?.textContent,
    );
    expect(marks).toEqual([t.recovery.backup, t.recovery.noBackup]);
    const boxes = [...view.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')];
    expect(boxes.map((b) => [b.getAttribute('aria-label'), b.checked])).toEqual([
      ['提单.pdf', true],
      ['合同.pdf', true],
    ]);
    act(() => boxes[1]!.click());
    act(() => button(view, t.recovery.reopen).click());
    expect(answers).toEqual([['0']]);
  });

  it('“都不要”给空的；一份都没选时不能点“重新打开”', () => {
    const answers: (readonly string[])[] = [];
    const view = render((ids) => answers.push(ids));
    for (const box of view.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')) {
      act(() => box.click());
    }
    expect(button(view, t.recovery.reopen).disabled).toBe(true);
    act(() => button(view, t.recovery.discard).click());
    expect(answers).toEqual([[]]);
  });
});
