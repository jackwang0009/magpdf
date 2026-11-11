import { Icon } from '@magpdf/ui';
import type { PageSnapshot } from '../document/types';
import { format, t } from '../i18n';
import { useEditorStore } from '../store/editorStore';

/** 提示条里最多列出几个页码，再多就写“等 N 页” */
const LISTED_PAGES = 3;

/**
 * 文档面板顶部的提示条（《交互设计》9.1）：说的是整份文档的状态，不是某一次操作的结果。
 * 现在只有一种：有的页没有通过保真校验，只能在上面添加内容（方案 2.3）。
 */
export function DocumentBanner({ documentId }: { readonly documentId: string }) {
  const pages = useEditorStore((state) => state.documents[documentId]?.snapshot.pages);
  const frozen = (pages ?? []).filter((page) => page.overlayOnly);
  if (frozen.length === 0) return null;
  return (
    <div className="mg-doc-banner" role="status" title={overlayOnlyDetail(frozen)}>
      <Icon name="info" size={14} />
      <span>{overlayOnlyMessage(frozen.map((page) => page.index))}</span>
    </div>
  );
}

/** 给用户看的说明：哪几页不能修改原有内容（页序号从 0 开始，显示时从 1 开始）。 */
export function overlayOnlyMessage(pages: readonly number[]): string {
  const listed = pages
    .slice(0, LISTED_PAGES)
    .map((index) => index + 1)
    .join(t.overlayOnly.pageSeparator);
  if (pages.length === 1) return format(t.overlayOnly.onePage, { pages: listed });
  if (pages.length <= LISTED_PAGES) return format(t.overlayOnly.somePages, { pages: listed });
  return format(t.overlayOnly.manyPages, { pages: listed, n: pages.length });
}

/** 悬停时显示的详细信息：引擎给的具体原因，相同的只写一次。没有原因时不显示。 */
export function overlayOnlyDetail(pages: readonly PageSnapshot[]): string | undefined {
  const reasons = [...new Set(pages.map((page) => page.overlayOnly?.reason ?? ''))].filter(Boolean);
  if (reasons.length === 0) return undefined;
  return format(t.overlayOnly.detail, { reason: reasons.join('；') });
}
