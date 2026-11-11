import { Icon } from '@magpdf/ui';
import { ptToMm } from '../../geometry';
import { t } from '../../i18n';
import { activeDocument, useEditorStore } from '../../store/editorStore';
import { overlayOnlyDetail } from '../DocumentBanner';
import { useShellCommands } from '../shellContext';
import { PanelFrame, PanelMenuButton } from './PanelFrame';

/** 页面面板：当前文档的页列表，点击滚动到该页；当前页高亮。 */
export function PagesPanel() {
  const commands = useShellCommands();
  const doc = useEditorStore(activeDocument);
  const setCurrentPage = useEditorStore((s) => s.setCurrentPage);
  return (
    <PanelFrame
      toolbar={
        <>
          <span className="mg-panel-count">
            {doc ? `${doc.snapshot.pages.length}` : 0}
            {t.panels.pages}
          </span>
          <PanelMenuButton />
        </>
      }
    >
      {doc ? (
        <ol className="mg-pages">
          {doc.snapshot.pages.map((page) => (
            <li key={page.index}>
              <button
                type="button"
                className={`mg-page-row${page.index === doc.currentPage ? ' mg-on' : ''}`}
                aria-current={page.index === doc.currentPage}
                onClick={() => {
                  setCurrentPage(doc.snapshot.id, page.index);
                  commands.controller()?.scrollToPage(page.index);
                }}
              >
                <span className="mg-page-thumb" aria-hidden />
                <span className="mg-page-label">
                  {t.pages.row.replace('{n}', String(page.index + 1))}
                </span>
                {page.overlayOnly ? (
                  // 保真校验角标，悬停说明原因（方案 8.6）
                  <span
                    className="mg-page-flag"
                    role="img"
                    aria-label={t.overlayOnly.badge}
                    title={[t.overlayOnly.badge, overlayOnlyDetail([page])]
                      .filter(Boolean)
                      .join('\n')}
                  >
                    <Icon name="lock" size={11} />
                  </span>
                ) : null}
                <span className="mg-page-size">
                  {t.pages.size
                    .replace('{w}', ptToMm(page.width).toFixed(0))
                    .replace('{h}', ptToMm(page.height).toFixed(0))}
                </span>
              </button>
            </li>
          ))}
        </ol>
      ) : (
        <p className="mg-empty">{t.documents.none}</p>
      )}
    </PanelFrame>
  );
}
