import type { Orientation, SerializedDockview } from 'dockview-react';
import { t } from '../i18n';
import {
  COMPONENTS_GROUP_ID,
  COMPONENT_PANEL_WIDTH,
  DOCUMENTS_GROUP_ID,
  DOC_PANEL_PREFIX,
  ICON_STRIP_WIDTH,
  PANEL_IDS,
  RIGHT_DOCK_WIDTH,
  STRIP_GROUP_ID,
  STRIP_PANEL_ID,
  isPanelId,
} from './workspaceTypes';
import type { PanelId, SideWidths, WorkspaceId, WorkspaceLayout } from './workspaceTypes';

// dockview-core 没有从入口导出网格叶子节点的序列化类型（GroupPanelViewState / SerializedGridObject），
// 这里按它的实际结构写最窄的本地类型；fromJSON 处只做一次断言，见 canvas 之外的 Workbench。
export interface WorkspaceGroupState {
  readonly id: string;
  views: string[];
  activeView?: string;
  hideHeader?: boolean;
  /** 与 dockview 的 DockviewGroupPanelLocked 一致（它没有从入口导出） */
  locked?: boolean | 'no-drop-target';
  constraints?: { minimumWidth?: number; maximumWidth?: number };
}

export interface WorkspaceGridNode {
  readonly type: 'leaf' | 'branch';
  readonly data: WorkspaceGroupState | WorkspaceGridNode[];
  readonly size?: number;
}

/**
 * 内置工作区的 dockview 布局 JSON，以及工作区的序列化/反序列化。
 * 纯数据，不依赖 DOM，便于单测往返。
 */
export interface WorkspaceSize {
  readonly width: number;
  readonly height: number;
}

export const WORKSPACE_IDS: readonly WorkspaceId[] = ['basic', 'freight'];
export const DEFAULT_WORKSPACE: WorkspaceId = 'freight';

export function workspaceLabel(id: WorkspaceId): string {
  return t.workspaces[id];
}

export function panelTitle(id: PanelId): string {
  return t.panels[id];
}

function leaf(
  id: string,
  views: readonly PanelId[],
  size: number,
  extra: Partial<WorkspaceGroupState> = {},
): WorkspaceGridNode {
  const data: WorkspaceGroupState = { id, views: [...views], ...extra };
  if (views.length > 0) data.activeView = views[0]!;
  return { type: 'leaf', data, size };
}

function branch(children: WorkspaceGridNode[], size: number): WorkspaceGridNode {
  return { type: 'branch', data: children, size };
}

function panelStates(ids: readonly PanelId[]): SerializedDockview['panels'] {
  const panels: SerializedDockview['panels'] = {};
  for (const id of ids) {
    panels[id] = { id, contentComponent: id, title: panelTitle(id) };
  }
  return panels;
}

/** 组件面板组：展开 240px；折叠成 32px 图标条（隐藏标签头、不接受拖放） */
export function componentsLeaf(mode: 'expanded' | 'collapsed', width: number): WorkspaceGridNode {
  if (mode === 'collapsed') {
    return leaf(COMPONENTS_GROUP_ID, ['components'], ICON_STRIP_WIDTH, {
      hideHeader: true,
      locked: 'no-drop-target',
      constraints: { minimumWidth: ICON_STRIP_WIDTH, maximumWidth: ICON_STRIP_WIDTH },
    });
  }
  return leaf(COMPONENTS_GROUP_ID, ['components'], width, {
    constraints: { minimumWidth: 200, maximumWidth: 400 },
  });
}

/** 右侧图标条组（折叠的右侧面板） */
export function stripLeaf(): WorkspaceGridNode {
  return {
    type: 'leaf',
    size: ICON_STRIP_WIDTH,
    data: {
      id: STRIP_GROUP_ID,
      views: [STRIP_PANEL_ID],
      activeView: STRIP_PANEL_ID,
      hideHeader: true,
      locked: 'no-drop-target',
      constraints: { minimumWidth: ICON_STRIP_WIDTH, maximumWidth: ICON_STRIP_WIDTH },
    },
  };
}

export function stripPanelState(): SerializedDockview['panels'][string] {
  return { id: STRIP_PANEL_ID, contentComponent: 'strip', title: '' };
}

/** 「货代单证」：组件面板展开在左，属性在右上，图层/页面/历史记录/导航器在右下 */
function freightWorkspace(size: WorkspaceSize): WorkspaceLayout {
  const widths: SideWidths = { components: COMPONENT_PANEL_WIDTH, rightDock: RIGHT_DOCK_WIDTH };
  const center = Math.max(200, size.width - widths.components - widths.rightDock);
  const topHeight = Math.round(size.height * 0.46);
  const rightTop: PanelId[] = ['properties', 'character', 'paragraph'];
  const rightBottom: PanelId[] = ['layers', 'pages', 'history', 'navigator'];
  const dockview: SerializedDockview = {
    grid: {
      root: branch(
        [
          componentsLeaf('expanded', widths.components),
          leaf(DOCUMENTS_GROUP_ID, [], center),
          branch(
            [
              leaf('right-top', rightTop, topHeight),
              leaf('right-bottom', rightBottom, size.height - topHeight),
            ],
            widths.rightDock,
          ),
        ],
        size.width,
      ),
      width: size.width,
      height: size.height,
      orientation: 'HORIZONTAL' as Orientation,
    },
    panels: panelStates(['components', ...rightTop, ...rightBottom]),
    activeGroup: DOCUMENTS_GROUP_ID,
  };
  return {
    version: 1,
    id: 'freight',
    dockview,
    componentPanelMode: 'expanded',
    sideWidths: widths,
    collapsedPanels: ['character', 'paragraph', 'swatches', 'info'],
  };
}

/** 「基础」：组件面板折叠成图标条，右侧一个面板组（属性、图层、页面、历史记录） */
function basicWorkspace(size: WorkspaceSize): WorkspaceLayout {
  const widths: SideWidths = { components: COMPONENT_PANEL_WIDTH, rightDock: RIGHT_DOCK_WIDTH };
  const center = Math.max(200, size.width - ICON_STRIP_WIDTH - widths.rightDock);
  const right: PanelId[] = ['properties', 'layers', 'pages', 'history'];
  const dockview: SerializedDockview = {
    grid: {
      root: branch(
        [
          componentsLeaf('collapsed', widths.components),
          leaf(DOCUMENTS_GROUP_ID, [], center),
          leaf('right', right, widths.rightDock),
        ],
        size.width,
      ),
      width: size.width,
      height: size.height,
      orientation: 'HORIZONTAL' as Orientation,
    },
    panels: panelStates(['components', ...right]),
    activeGroup: DOCUMENTS_GROUP_ID,
  };
  return {
    version: 1,
    id: 'basic',
    dockview,
    componentPanelMode: 'collapsed',
    sideWidths: widths,
    collapsedPanels: ['character', 'paragraph', 'navigator', 'swatches', 'info'],
  };
}

export function builtinWorkspace(id: WorkspaceId, size: WorkspaceSize): WorkspaceLayout {
  return id === 'basic' ? basicWorkspace(size) : freightWorkspace(size);
}

/** 从 dockview 当前布局中去掉文档面板（文档随会话走，不属于工作区） */
export function stripDocumentPanels(layout: SerializedDockview): SerializedDockview {
  const panels: SerializedDockview['panels'] = {};
  for (const [id, state] of Object.entries(layout.panels)) {
    if (!id.startsWith(DOC_PANEL_PREFIX)) panels[id] = state;
  }
  // 网格节点类型在 dockview 入口里没有导出，取实际布局后按本地结构处理，出口时还原
  const visit = (node: WorkspaceGridNode): WorkspaceGridNode => {
    if (node.type === 'branch') {
      return { ...node, data: (node.data as WorkspaceGridNode[]).map(visit) };
    }
    const data = node.data as WorkspaceGroupState;
    const views = data.views.filter((v) => !v.startsWith(DOC_PANEL_PREFIX));
    const next: WorkspaceGroupState = { ...data, views };
    if (data.activeView && data.activeView.startsWith(DOC_PANEL_PREFIX)) {
      if (views.length > 0) next.activeView = views[0]!;
      else delete next.activeView;
    }
    return { ...node, data: next };
  };
  const root = visit(layout.grid.root as unknown as WorkspaceGridNode);
  const result: SerializedDockview = {
    ...layout,
    grid: { ...layout.grid, root: root as unknown as SerializedDockview['grid']['root'] },
    panels,
  };
  if (layout.floatingGroups) {
    result.floatingGroups = layout.floatingGroups.map((group) => {
      if (!group.data) return group;
      return {
        ...group,
        data: {
          ...group.data,
          views: group.data.views.filter((v) => !v.startsWith(DOC_PANEL_PREFIX)),
        },
      };
    });
  }
  return result;
}

/** 布局里出现的全部面板 id（含浮动组） */
export function panelsInLayout(layout: SerializedDockview): string[] {
  return Object.keys(layout.panels);
}

/** 停靠在网格/浮动组里的工具面板（不含图标条里的） */
export function dockedPanels(layout: SerializedDockview): PanelId[] {
  return panelsInLayout(layout).filter(isPanelId);
}

export function serializeWorkspace(layout: WorkspaceLayout): string {
  return JSON.stringify(layout);
}

export function parseWorkspace(json: string): WorkspaceLayout | null {
  try {
    const value: unknown = JSON.parse(json);
    if (!isWorkspaceLayout(value)) return null;
    return value;
  } catch {
    return null;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function isWorkspaceLayout(value: unknown): value is WorkspaceLayout {
  if (!isRecord(value)) return false;
  if (value.version !== 1) return false;
  if (!WORKSPACE_IDS.includes(value.id as WorkspaceId)) return false;
  if (!['expanded', 'collapsed', 'hidden'].includes(value.componentPanelMode as string))
    return false;
  const widths = value.sideWidths;
  if (
    !isRecord(widths) ||
    typeof widths.components !== 'number' ||
    typeof widths.rightDock !== 'number'
  ) {
    return false;
  }
  if (
    !Array.isArray(value.collapsedPanels) ||
    !value.collapsedPanels.every((p) => isPanelId(String(p)))
  ) {
    return false;
  }
  const dockview = value.dockview;
  if (!isRecord(dockview) || !isRecord(dockview.grid) || !isRecord(dockview.panels)) return false;
  const grid = dockview.grid;
  if (!isRecord(grid.root) || typeof grid.width !== 'number' || typeof grid.height !== 'number')
    return false;
  // 只接受我们认识的面板组件，防止旧版本/损坏数据让 dockview 抛错
  for (const [id, panel] of Object.entries(dockview.panels)) {
    if (!isRecord(panel)) return false;
    const component = panel.contentComponent;
    if (id === STRIP_PANEL_ID) continue;
    if (!PANEL_IDS.includes(component as PanelId)) return false;
  }
  return true;
}

const STORAGE_PREFIX = 'magpdf.workspace.';

export interface WorkspaceStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export function saveWorkspace(storage: WorkspaceStorage, layout: WorkspaceLayout): void {
  storage.setItem(STORAGE_PREFIX + layout.id, serializeWorkspace(layout));
}

export function loadWorkspace(storage: WorkspaceStorage, id: WorkspaceId): WorkspaceLayout | null {
  const json = storage.getItem(STORAGE_PREFIX + id);
  return json ? parseWorkspace(json) : null;
}

export function resetWorkspace(storage: WorkspaceStorage, id: WorkspaceId): void {
  storage.removeItem(STORAGE_PREFIX + id);
}

const CURRENT_KEY = 'magpdf.workspace.current';

export function loadCurrentWorkspaceId(storage: WorkspaceStorage): WorkspaceId {
  const value = storage.getItem(CURRENT_KEY);
  return WORKSPACE_IDS.includes(value as WorkspaceId) ? (value as WorkspaceId) : DEFAULT_WORKSPACE;
}

export function saveCurrentWorkspaceId(storage: WorkspaceStorage, id: WorkspaceId): void {
  storage.setItem(CURRENT_KEY, id);
}
