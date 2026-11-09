import { useEffect, useState } from 'react';
import { Icon } from '@magpdf/ui';
import type { IDockviewPanelHeaderProps } from 'dockview-react';
import { t } from '../i18n';
import { useEditorStore } from '../store/editorStore';

export interface DocumentTabParams {
  readonly documentId: string;
}

/**
 * 文档标签页 28px：标题、未保存标记 `*`、关闭按钮（方案 8.2）。
 * dockview 默认标签太宽，这里自己画；高度由主题变量控制。
 */
export function DocumentTab(props: IDockviewPanelHeaderProps<DocumentTabParams>) {
  const documentId = props.params?.documentId;
  const doc = useEditorStore((s) => (documentId ? s.documents[documentId] : undefined));
  const [active, setActive] = useState(() => props.api.isActive);
  useEffect(() => {
    const sub = props.api.onDidActiveChange((event) => setActive(event.isActive));
    return () => sub.dispose();
  }, [props.api]);
  const title = doc?.snapshot.name ?? t.documents.none;
  return (
    <div className={`mg-doc-tab${active ? ' mg-on' : ''}`} data-testid="doc-tab">
      <span className="mg-doc-tab-icon">
        <Icon name="pages" size={12} />
      </span>
      <span className="mg-doc-tab-title">
        {title}
        {doc?.snapshot.dirty ? (
          <span className="mg-doc-tab-dirty">{t.documents.dirtyMark}</span>
        ) : null}
      </span>
      <button
        type="button"
        className="mg-doc-tab-close"
        title={t.documents.closeTab}
        aria-label={`${t.documents.closeTab} ${title}`}
        onClick={(event) => {
          event.stopPropagation();
          props.api.close();
        }}
      >
        <Icon name="close" size={11} />
      </button>
    </div>
  );
}

/** 文档区没有打开文档时的水印 */
export function DocumentsWatermark() {
  return <div className="mg-watermark">{t.documents.none}</div>;
}
