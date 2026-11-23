// @vitest-environment jsdom
import { act } from 'react';
import { createRoot } from 'react-dom/client';
import type { Root } from 'react-dom/client';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import { format, t } from '../i18n';
import { UnsavedDialog } from './Dialogs';
import type { UnsavedDialogProps } from './Dialogs';

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

function render(props: Partial<UnsavedDialogProps> & { names: readonly string[] }) {
  const calls: string[] = [];
  container = document.createElement('div');
  document.body.append(container);
  root = createRoot(container);
  act(() =>
    root!.render(
      <UnsavedDialog
        busy={false}
        onSave={() => calls.push('save')}
        onDiscard={() => calls.push('discard')}
        onCancel={() => calls.push('cancel')}
        {...props}
      />,
    ),
  );
  return { view: container, calls };
}

function button(within: HTMLElement, label: string): HTMLButtonElement {
  const found = [...within.querySelectorAll('button')].find((b) => b.textContent === label);
  if (!found) throw new Error(`没有按钮 ${label}`);
  return found;
}

describe('关闭前保存的对话框', () => {
  it('一份文档：标题写文件名，按钮是“保存”“不保存”“取消”，默认焦点在“保存”上', () => {
    const { view, calls } = render({ names: ['提单.pdf'] });
    const dialog = view.querySelector('[role="alertdialog"]')!;
    expect(dialog.getAttribute('aria-label')).toBe(
      format(t.unsaved.titleOne, { name: '提单.pdf' }),
    );
    expect(view.querySelector('ul')).toBeNull();
    expect(document.activeElement).toBe(button(view, t.unsaved.save));
    act(() => button(view, t.unsaved.save).click());
    act(() => button(view, t.unsaved.discard).click());
    act(() => button(view, t.unsaved.cancel).click());
    expect(calls).toEqual(['save', 'discard', 'cancel']);
  });

  it('几份文档：标题写份数，列出文件名；Esc 等于取消，按键不往外传', () => {
    const { view, calls } = render({ names: ['提单.pdf', '发票.pdf'] });
    expect(view.querySelector('[role="alertdialog"]')!.getAttribute('aria-label')).toBe(
      format(t.unsaved.titleMany, { n: 2 }),
    );
    expect([...view.querySelectorAll('li')].map((li) => li.textContent)).toEqual([
      '提单.pdf',
      '发票.pdf',
    ]);
    let reachedWindow = 0;
    const onWindowKey = () => reachedWindow++;
    window.addEventListener('keydown', onWindowKey);
    try {
      act(() =>
        button(view, t.unsaved.save).dispatchEvent(
          new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }),
        ),
      );
      act(() =>
        button(view, t.unsaved.save).dispatchEvent(
          new KeyboardEvent('keydown', { key: 'Delete', bubbles: true }),
        ),
      );
    } finally {
      window.removeEventListener('keydown', onWindowKey);
    }
    expect(calls).toEqual(['cancel']);
    expect(reachedWindow).toBe(0);
  });

  it('正在保存：按钮都不能点，Esc 也不取消', () => {
    const { view, calls } = render({ names: ['提单.pdf'], busy: true });
    expect(view.textContent).toContain(t.unsaved.saving);
    for (const label of [t.unsaved.save, t.unsaved.discard, t.unsaved.cancel]) {
      expect(button(view, label).disabled).toBe(true);
    }
    act(() =>
      view
        .querySelector('[role="alertdialog"]')!
        .dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })),
    );
    expect(calls).toEqual([]);
  });
});
