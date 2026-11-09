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
export function PanelSearch({ placeholder }: { readonly placeholder: string }) {
  return (
    <span className="mg-panel-search">
      <Icon name="search" size={13} className="mg-muted" />
      <span>{placeholder}</span>
    </span>
  );
}

export function PanelMenuButton() {
  return <IconButton icon="menu" small label={t.panelActions.menu} disabled />;
}
