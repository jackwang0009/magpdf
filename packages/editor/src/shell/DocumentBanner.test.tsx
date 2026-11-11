// @vitest-environment jsdom
import { act } from 'react';
import { createRoot } from 'react-dom/client';
import { afterEach, beforeAll, describe, expect, it } from 'vitest';
import { createMockDocument } from '../document/mockDocument';
import type { DocumentSnapshot } from '../document/types';
import { editorStore } from '../store/editorStore';
import { DocumentBanner, overlayOnlyDetail, overlayOnlyMessage } from './DocumentBanner';

declare global {
  // React 要求测试环境显式声明支持 act()
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

beforeAll(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
});

describe('只能叠加内容的页：给用户的说明', () => {
  it('一页：和《交互设计》9.3 的文案一字不差', () => {
    expect(overlayOnlyMessage([2])).toBe(
      '第 3 页无法安全修改原有内容：页面使用了软件尚不能完整保留的效果。可以在这一页上添加内容。',
    );
  });

  it('两三页：页码都列出来', () => {
    expect(overlayOnlyMessage([0, 2, 4])).toBe(
      '第 1、3、5 页无法安全修改原有内容：页面使用了软件尚不能完整保留的效果。可以在这些页上添加内容。',
    );
  });

  it('再多：列前三个，写明一共几页', () => {
    expect(overlayOnlyMessage([0, 1, 2, 7, 9])).toBe(
      '第 1、2、3 等 5 页无法安全修改原有内容：页面使用了软件尚不能完整保留的效果。可以在这些页上添加内容。',
    );
  });

  it('详细信息：引擎给的原因，相同的只写一次；没有原因就不显示', () => {
    const page = (index: number, reason: string) => ({
      index,
      width: 100,
      height: 100,
      objects: [],
      overlayOnly: { reason },
    });
    expect(
      overlayOnlyDetail([page(0, '对象的个数变了'), page(1, '画面变了'), page(2, '画面变了')]),
    ).toBe('详细信息：对象的个数变了；画面变了');
    expect(overlayOnlyDetail([page(0, '')])).toBeUndefined();
  });
});

describe('文档面板顶部的提示条', () => {
  const id = 'banner-test';
  let container: HTMLDivElement;

  afterEach(() => {
    container.remove();
    editorStore.getState().closeDocument(id);
  });

  it('没有这样的页时不出现；有了就出现，页面恢复后消失', async () => {
    const snapshot: DocumentSnapshot = createMockDocument({
      id,
      name: '测试.pdf',
      pageCount: 3,
      template: 'report',
    });
    editorStore.getState().openDocument(snapshot);
    container = document.createElement('div');
    document.body.appendChild(container);
    const root = createRoot(container);
    await act(async () => {
      root.render(<DocumentBanner documentId={id} />);
    });
    expect(container.querySelector('.mg-doc-banner')).toBeNull();

    await act(async () => {
      editorStore.getState().updateSnapshot(id, {
        ...snapshot,
        pages: snapshot.pages.map((page) =>
          page.index === 1 ? { ...page, overlayOnly: { reason: '画面变了' } } : page,
        ),
      });
    });
    const banner = container.querySelector('.mg-doc-banner');
    expect(banner?.textContent).toBe(overlayOnlyMessage([1]));
    expect(banner?.getAttribute('role')).toBe('status');
    expect(banner?.getAttribute('title')).toBe('详细信息：画面变了');

    // 引擎重启之后页面记录全部换新，旧的结论不留着
    await act(async () => {
      editorStore.getState().updateSnapshot(id, snapshot);
    });
    expect(container.querySelector('.mg-doc-banner')).toBeNull();
    await act(async () => {
      root.unmount();
    });
  });
});
