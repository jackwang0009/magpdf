import { Icon } from '@magpdf/ui';
import type { IconName } from '@magpdf/ui';
import { t } from '../../i18n';
import { activeDocument, useEditorStore } from '../../store/editorStore';
import type { PanelId } from '../../workspace/workspaceTypes';
import { PanelFrame } from './PanelFrame';

/**
 * 历史记录：P0 只记录对象移动/缩放/删除的描述，撤销栈在 P1。
 * 文档快照里带 history 数组，这里倒序列出。
 */
export function HistoryPanel() {
  const doc = useEditorStore(activeDocument);
  return (
    <PanelFrame>
      {doc && doc.history.length > 0 ? (
        <ol className="mg-history">
          {[...doc.history].reverse().map((entry, index) => (
            <li key={index} className={index === 0 ? 'mg-on' : ''}>
              <span className="mg-history-dot" aria-hidden />
              {entry}
            </li>
          ))}
        </ol>
      ) : (
        <p className="mg-empty">{t.history.empty}</p>
      )}
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
