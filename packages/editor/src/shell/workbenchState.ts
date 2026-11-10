import type {
  Box,
  Direction,
  DockviewApi,
  DockviewTheme,
  SerializedDockview,
} from 'dockview-react';
import type { ComponentPanelMode, PanelId, WorkspaceLayout } from '../workspace/workspaceTypes';
import {
  COMPONENT_PANEL_WIDTH,
  ICON_STRIP_WIDTH,
  RIGHT_DOCK_WIDTH,
  SIDE_PANEL_MAX_WIDTH,
  SIDE_PANEL_MIN_WIDTH,
  STARTER_PANELS,
  isPanelId,
} from '../workspace/workspaceTypes';
import { clampSideWidths, cleanDockviewLayout } from '../workspace/workspaces';

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

/** 旧版浮动工具栏的面板 id（布局版本 2 之前）：只用来在应用旧布局时丢弃残留面板，不再新建。 */
export const LEGACY_TOOLBAR_PANEL = 'toolbar';
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
 * 应用工作区：fromJSON，然后补上图标条、丢弃旧版浮动工具栏残留、重新按容器尺寸布局。
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
  // 先清掉保存值里残留的文档视图和空组再 fromJSON：dockview 会跳过面板表里没有的视图、把组留空，
  // 空组在 closeEmptyGroups={false} 时不自动消失，只显示“没有打开的文档”水印
  api.fromJSON(clampSideWidths(cleanDockviewLayout(layout.dockview)));
  api.layout(width, height, true);
  ensureStrip(api);
  // 旧布局里可能还残留浮动工具栏面板（布局版本 2 之前）：丢弃它，工具栏现在在外壳顶部
  const legacyToolbar = api.getPanel(LEGACY_TOOLBAR_PANEL);
  if (legacyToolbar) api.removePanel(legacyToolbar);
  // 两侧宽度运行时再兜底一次（见 enforceSideWidths）
  enforceSideWidths(api, layout.componentPanelMode);
}

/**
 * 运行时再兜底一次两侧宽度限制：给已有的两侧组设 200–320px 约束，超出范围的实测宽度收进范围。
 *
 * 序列化层面的夹取在 fromJSON 之前由 clampSideWidths() 做；这里防的是加载后又被其它逻辑
 * （如 Workbench 恢复上次拖出的组件面板宽度）拉出范围的情况——setSize 会被刚设好的约束夹住。
 * 组件面板折叠成图标条时（mode 不是 expanded）不动它，32px 条要保持。
 */
export function enforceSideWidths(api: DockviewApi, mode: ComponentPanelMode): void {
  const ids = [RIGHT_TOP_GROUP, RIGHT_BOTTOM_GROUP, RIGHT_GROUP];
  if (mode === 'expanded') ids.unshift(COMPONENTS_GROUP);
  for (const id of ids) {
    const group = api.getGroup(id);
    if (!group) continue;
    group.api.setConstraints({
      minimumWidth: SIDE_PANEL_MIN_WIDTH,
      maximumWidth: SIDE_PANEL_MAX_WIDTH,
    });
    const box = (group as unknown as { api?: { boundingBox?: Box } }).api?.boundingBox;
    if (!box) continue;
    const width = Math.round(box.width);
    if (width < SIDE_PANEL_MIN_WIDTH || width > SIDE_PANEL_MAX_WIDTH) {
      group.api.setSize({
        width: Math.min(SIDE_PANEL_MAX_WIDTH, Math.max(SIDE_PANEL_MIN_WIDTH, width)),
      });
    }
  }
}

/** 保存时记下的侧边宽度也收进范围；折叠成图标条的 32px 不动（<=32 的值原样保留） */
function clampRecordedWidth(width: number | undefined, fallback: number): number {
  if (width === undefined) return fallback;
  if (width <= ICON_STRIP_WIDTH) return width;
  return Math.min(SIDE_PANEL_MAX_WIDTH, Math.max(SIDE_PANEL_MIN_WIDTH, width));
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

/** 当前布局 → 工作区快照：去掉文档、图标条和旧版工具栏残留，丢弃网格里残留的文档视图和变空的非结构组，记录侧边宽度与折叠面板 */
export function captureWorkspace(
  api: DockviewApi,
  id: WorkspaceLayout['id'],
  mode: ComponentPanelMode,
  collapsed: readonly PanelId[],
): WorkspaceLayout {
  const json = api.toJSON();
  const panels: SerializedDockview['panels'] = {};
  for (const [key, value] of Object.entries(json.panels)) {
    if (key.startsWith(DOC_PANEL_PREFIX) || key === LEGACY_TOOLBAR_PANEL || key === STRIP_PANEL)
      continue;
    panels[key] = value;
  }
  // 组的实际宽度：优先取实测包围盒；虚拟/测试环境里没有 DOM 时用工作区记录值兜底
  const widthOf = (group: string): number | undefined => {
    const panel = api.getGroup(group) as { api?: { boundingBox?: Box } } | undefined;
    const box = panel?.api?.boundingBox;
    return box ? Math.round(box.width) : undefined;
  };
  const raw: SerializedDockview = { ...json, panels };
  if (json.floatingGroups) {
    raw.floatingGroups = json.floatingGroups.filter((group) => {
      const views = group.data?.views ?? [];
      return !views.includes(LEGACY_TOOLBAR_PANEL) && !views.includes(STRIP_PANEL);
    });
  }
  // 保存前清掉网格里残留的文档视图和变空的非结构组，否则下次 fromJSON 会把组留空；
  // 两侧宽度也收进 200–320px（约束写进叶子节点，下次加载直接生效）
  const dockview = clampSideWidths(cleanDockviewLayout(raw));
  return {
    version: 2,
    id,
    dockview,
    componentPanelMode: mode,
    sideWidths: {
      components: clampRecordedWidth(widthOf(COMPONENTS_GROUP), COMPONENT_PANEL_WIDTH),
      rightDock: clampRecordedWidth(
        widthOf(RIGHT_BOTTOM_GROUP) ?? widthOf(RIGHT_TOP_GROUP) ?? widthOf(RIGHT_GROUP),
        RIGHT_DOCK_WIDTH,
      ),
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
