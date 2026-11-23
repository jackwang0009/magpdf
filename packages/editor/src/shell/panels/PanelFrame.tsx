import type { ReactNode } from 'react';
import { Icon, IconButton } from '@magpdf/ui';
import type { IconName } from '@magpdf/ui';
import { t } from '../../i18n';

/**
 * 面板内容的外框：顶部一行小工具（搜索/筛选/视图切换，按面板传），下面是内容。
 * dockview 自己画标签头；这个外框只负责面板内部的一致性。
 */
export interface PanelFrameProps {
  readonly children: ReactNode;
  readonly toolbar?: ReactNode;
  readonly icon?: IconName;
}

export function PanelFrame({ children, toolbar, icon }: PanelFrameProps) {
  return (
    <div className="mg-panel">
      {toolbar ? (
        <div className="mg-panel-toolbar">
          {icon ? <Icon name={icon} size={14} className="mg-muted" /> : null}
          {toolbar}
        </div>
      ) : null}
      <div className="mg-panel-body">{children}</div>
    </div>
  );
}

/** 只读的搜索框外观（P0 不接搜索） */
/** 面板顶上的搜索框。给了 `onChange` 才能输入，没给只显示提示文字（还没接上的面板）。 */
export function PanelSearch({
  placeholder,
  value,
  onChange,
}: {
  readonly placeholder: string;
  readonly value?: string;
  readonly onChange?: (value: string) => void;
}) {
  return (
    <span className="mg-panel-search">
      <Icon name="search" size={13} className="mg-muted" />
      {onChange ? (
        <input
          className="mg-panel-search-input"
          type="search"
          value={value ?? ''}
          placeholder={placeholder}
          aria-label={placeholder}
          spellCheck={false}
          onChange={(event) => onChange(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === 'Escape' && value) {
              event.stopPropagation();
              onChange('');
            }
          }}
        />
      ) : (
        <span>{placeholder}</span>
      )}
    </span>
  );
}

export function PanelMenuButton() {
  return <IconButton icon="menu" small label={t.panelActions.menu} disabled />;
}
