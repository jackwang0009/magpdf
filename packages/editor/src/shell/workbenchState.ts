import type {
  Box,
  Direction,
  DockviewApi,
  DockviewTheme,
  SerializedDockview,
} from 'dockview-react';
import type { ComponentPanelMode, PanelId, WorkspaceLayout } from '../workspace/workspaceTypes';
import { STARTER_PANELS, isPanelId } from '../workspace/workspaceTypes';

/**
 * 工作区布局在 dockview 上的操作：应用内置布局、取回当前布局。
 * 只依赖 dockview API 和纯数据，不涉及 React，便于测试。
 */
export const MAGPDF_THEME: DockviewTheme = {
  name: 'magpdf',
  className: 'dockview-theme-magpdf',
  colorScheme: 'dark',
  gap: 0,
  dndOverlayMounting: 'absolute',
  dndPanelOverlay: 'group',
};

export const COMPONENTS_GROUP = 'components';
export const COMPONENTS_PANEL = 'components';
export const RIGHT_TOP_GROUP = 'right-top';
export const RIGHT_BOTTOM_GROUP = 'right-bottom';
export const RIGHT_GROUP = 'right';

export const TOOLBAR_PANEL = 'toolbar';
export const TOOLBAR_GROUP = 'toolbar-group';
export const STRIP_PANEL = 'strip';
export const DOC_PANEL_PREFIX = 'doc:';

function leafIds(root: SerializedDockview['grid']['root']): string[] {
  const ids: string[] = [];
  const walk = (nodes: { type: string; data: unknown }[]) => {
    for (const node of nodes) {
      if (node.type === 'branch') walk(node.data as { type: string; data: unknown }[]);
      else ids.push((node.data as { id: string }).id);
    }
  };
  if (root.type === 'branch') walk(root.data as { type: string; data: unknown }[]);
  return ids;
}

/** 布局里右侧停靠组（从上到下） */
export function rightGroupIds(layout: SerializedDockview): string[] {
  const all = leafIds(layout.grid.root);
  const right = all.filter((id) => id.startsWith('right'));
  return right.length ? right : [RIGHT_GROUP];
}

/**
 * 应用工作区：fromJSON，然后补上图标条、浮动工具栏、重新按容器尺寸布局。
 * 不在这里补文档面板——文档随会话走，由 Workbench 的同步逻辑负责。
 */
export function applyWorkspace(
  api: DockviewApi,
  layout: WorkspaceLayout,
  width: number,
  height: number,
): void {
  // fromJSON 会跟着触发异步布局；显式按容器尺寸再排一次，避免在容器尺寸为 0 时套用保存的像素尺寸
  api.clear();
  api.fromJSON(layout.dockview);
  api.layout(width, height, true);
  ensureStrip(api);
  ensureToolbar(api);
}

function ensureStrip(api: DockviewApi): void {
  // 内置布局自带组件面板组：没有的话说明是用户存过又改坏了的布局，补一个左侧停靠组
  if (!api.getPanel(COMPONENTS_PANEL)) {
    const reference = api.getGroup(COMPONENTS_GROUP) ?? api.groups[0];
    if (reference) {
      api.addPanel({
        id: COMPONENTS_PANEL,
        component: 'components',
        title: '',
        position: { referenceGroup: reference.id, direction: 'left' as Direction },
      });
    }
  }
  if (api.getPanel(STRIP_PANEL)) return;
  // 图标条停在工作区最右侧（方案 8.2 主区域的 32px 列）
  const root = api.groups[0];
  api.addPanel({
    id: STRIP_PANEL,
    component: 'right-strip',
    title: '',
    ...(root ? { position: { referenceGroup: root.id, direction: 'right' as Direction } } : {}),
  });
  const panel = api.getPanel(STRIP_PANEL);
  if (!panel) return;
  panel.group.api.setConstraints({ maximumWidth: 32, minimumWidth: 32 });
  panel.group.header.hidden = true;
}

function ensureToolbar(api: DockviewApi): void {
  if (api.getPanel(TOOLBAR_PANEL)) return;
  const panel = api.addPanel({
    id: TOOLBAR_PANEL,
    component: 'toolbar',
    title: '',
    floating: { x: 0, y: 0, width: 40, height: 520 },
  });
  panel.group.api.setConstraints({ maximumWidth: 40, minimumWidth: 40 });
  panel.group.header.hidden = true;
}

/** 当前布局 → 工作区快照：去掉文档、工具栏、图标条，记录侧边宽度与折叠面板 */
export function captureWorkspace(
  api: DockviewApi,
  id: WorkspaceLayout['id'],
  mode: ComponentPanelMode,
  collapsed: readonly PanelId[],
): WorkspaceLayout {
  const json = api.toJSON();
  const panels: SerializedDockview['panels'] = {};
  for (const [key, value] of Object.entries(json.panels)) {
    if (key.startsWith(DOC_PANEL_PREFIX) || key === TOOLBAR_PANEL || key === STRIP_PANEL) continue;
    panels[key] = value;
  }
  // 组的实际宽度：优先取实测包围盒；虚拟/测试环境里没有 DOM 时用工作区记录值兜底
  const widthOf = (group: string): number | undefined => {
    const panel = api.getGroup(group) as { api?: { boundingBox?: Box } } | undefined;
    const box = panel?.api?.boundingBox;
    return box ? Math.round(box.width) : undefined;
  };
  const dockview: SerializedDockview = { ...json, panels };
  if (json.floatingGroups) {
    dockview.floatingGroups = json.floatingGroups.filter((group) => {
      const views = group.data?.views ?? [];
      return !views.includes(TOOLBAR_PANEL) && !views.includes(STRIP_PANEL);
    });
  }
  return {
    version: 1,
    id,
    dockview,
    componentPanelMode: mode,
    sideWidths: {
      components: widthOf(COMPONENTS_GROUP) ?? 240,
      rightDock:
        widthOf(RIGHT_BOTTOM_GROUP) ?? widthOf(RIGHT_TOP_GROUP) ?? widthOf(RIGHT_GROUP) ?? 280,
    },
    collapsedPanels: [...collapsed],
  };
}

export function panelIdsOf(layout: SerializedDockview): PanelId[] {
  return Object.keys(layout.panels).filter(isPanelId);
}

/** 工作区里没出现的面板默认收进图标条 */
export function defaultCollapsedPanels(layout: SerializedDockview): PanelId[] {
  const present = new Set(panelIdsOf(layout));
  return STARTER_PANELS.filter((id) => !present.has(id));
}
