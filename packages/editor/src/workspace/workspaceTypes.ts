import type { SerializedDockview } from 'dockview-react';

/** 内置工作区（《方案》8.2：默认「货代单证」） */
export type WorkspaceId = 'basic' | 'freight';

/** 停靠面板 id；文档面板另用 `doc:<文档 id>` */
export type PanelId =
  | 'components'
  | 'properties'
  | 'character'
  | 'paragraph'
  | 'layers'
  | 'pages'
  | 'history'
  | 'navigator'
  | 'swatches'
  | 'info';

export const PANEL_IDS: readonly PanelId[] = [
  'components',
  'properties',
  'character',
  'paragraph',
  'layers',
  'pages',
  'history',
  'navigator',
  'swatches',
  'info',
];

/** 内置工作区里没放、需要时打开的面板（图标条里显示的默认集合） */
export const STARTER_PANELS: readonly PanelId[] = ['character', 'paragraph', 'swatches', 'info'];

export const DOCUMENTS_GROUP_ID = 'documents';
export const COMPONENTS_GROUP_ID = 'components';
export const STRIP_GROUP_ID = 'strip';
export const STRIP_PANEL_ID = 'strip';
export const DOC_PANEL_PREFIX = 'doc:';

export const COMPONENT_PANEL_WIDTH = 240;
export const RIGHT_DOCK_WIDTH = 280;
export const ICON_STRIP_WIDTH = 32;

export type ComponentPanelMode = 'expanded' | 'collapsed' | 'hidden';

export interface SideWidths {
  readonly components: number;
  readonly rightDock: number;
}

/** 可序列化的工作区：dockview 布局（不含文档面板）+ 我们自己的几项状态 */
export interface WorkspaceLayout {
  readonly version: 1;
  readonly id: WorkspaceId;
  readonly dockview: SerializedDockview;
  readonly componentPanelMode: ComponentPanelMode;
  readonly sideWidths: SideWidths;
  /** 折叠到右侧图标条里的面板 */
  readonly collapsedPanels: readonly PanelId[];
}

export function docPanelId(documentId: string): string {
  return `${DOC_PANEL_PREFIX}${documentId}`;
}

export function documentIdOfPanel(panelId: string): string | null {
  return panelId.startsWith(DOC_PANEL_PREFIX) ? panelId.slice(DOC_PANEL_PREFIX.length) : null;
}

export function isPanelId(id: string): id is PanelId {
  return (PANEL_IDS as readonly string[]).includes(id);
}
