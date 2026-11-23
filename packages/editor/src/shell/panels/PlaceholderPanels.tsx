import { Icon } from '@magpdf/ui';
import type { IconName } from '@magpdf/ui';
import { format, t } from '../../i18n';
import { activeDocument, useEditorStore } from '../../store/editorStore';
import type { PanelId } from '../../workspace/workspaceTypes';
import { useEditorHost } from '../editorHost';
import { PanelFrame } from './PanelFrame';

/**
 * 历史记录（方案 4.2）：最新的在上面。第一条是打开时的样子；撤销了的几步划掉显示，还能重做。
 * 引擎打开的文档点哪一条就撤销或重做到哪一条；模拟文档只列出来。
 */
export function HistoryPanel() {
  const doc = useEditorStore(activeDocument);
  const host = useEditorHost();
  if (!doc || doc.history.length === 0) {
    return (
      <PanelFrame>
        <p className="mg-empty">{t.history.empty}</p>
      </PanelFrame>
    );
  }
  const editor = host?.documentEditor(doc.snapshot.id);
  const at = doc.historyAt ?? doc.history.length - 1;
  const steps = doc.history.map((label, index) => ({ label, index })).reverse();
  return (
    <PanelFrame>
      <ol className="mg-history">
        {steps.map(({ label, index }) => {
          const className = `mg-history-step${index === at ? ' mg-on' : ''}${index > at ? ' mg-undone' : ''}`;
          const content = (
            <>
              <span className="mg-history-dot" aria-hidden />
              <span className="mg-history-label">{label}</span>
            </>
          );
          return (
            <li key={index}>
              {editor ? (
                <button
                  type="button"
                  className={className}
                  disabled={index === at}
                  title={index === at ? undefined : format(t.history.jumpTo, { label })}
                  onClick={() => editor.jumpHistory(index)}
                >
                  {content}
                </button>
              ) : (
                <div className={className}>{content}</div>
              )}
            </li>
          );
        })}
      </ol>
    </PanelFrame>
  );
}

/** 其余面板（字符、段落、导航器、色板、信息）的占位。 */
export function PlaceholderPanel({ name }: { readonly name: string }) {
  return (
    <PanelFrame>
      <p className="mg-empty">{t.placeholder.panel.replace('{name}', name)}</p>
    </PanelFrame>
  );
}

/**
 * 右侧 32px 图标条：显示折叠到条里的面板，点击把面板浮到右侧。
 * 作为 dockview 面板挂在固定宽度的组里。
 */
/** 图标条上直接显示图标（与工具栏一致的 16px 线框图标） */
const STRIP_ICON: Partial<Record<PanelId, IconName>> = {
  components: 'components',
  properties: 'properties',
  character: 'character',
  paragraph: 'paragraph',
  layers: 'layers',
  pages: 'pages',
  history: 'history',
  navigator: 'navigator',
  swatches: 'swatches',
  info: 'info',
};

export function CollapsedStrip({ onOpen }: { readonly onOpen?: (id: PanelId) => void }) {
  const panels = useEditorStore((s) => s.collapsedPanels);
  return (
    <div className="mg-strip mg-strip-right">
      {panels.map((id) => (
        <button
          key={id}
          type="button"
          className="mg-strip-btn"
          title={t.panelActions.openPanel.replace('{name}', t.panels[id])}
          aria-label={t.panelActions.openPanel.replace('{name}', t.panels[id])}
          onClick={() => onOpen?.(id)}
        >
          <Icon name={STRIP_ICON[id] ?? 'info'} size={16} />
        </button>
      ))}
    </div>
  );
}
