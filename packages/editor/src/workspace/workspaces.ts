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
  SIDE_PANEL_MAX_WIDTH,
  SIDE_PANEL_MIN_WIDTH,
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

/** 两侧组的宽度约束：组件面板展开时和右侧停靠组共用（默认左 240px、右 280px，都在 200–320px 内） */
const SIDE_PANEL_CONSTRAINTS: NonNullable<WorkspaceGroupState['constraints']> = {
  minimumWidth: SIDE_PANEL_MIN_WIDTH,
  maximumWidth: SIDE_PANEL_MAX_WIDTH,
};

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
    constraints: SIDE_PANEL_CONSTRAINTS,
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
              leaf('right-top', rightTop, topHeight, { constraints: SIDE_PANEL_CONSTRAINTS }),
              leaf('right-bottom', rightBottom, size.height - topHeight, {
                constraints: SIDE_PANEL_CONSTRAINTS,
              }),
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
    version: 2,
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
          leaf('right', right, widths.rightDock, { constraints: SIDE_PANEL_CONSTRAINTS }),
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
    version: 2,
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

/**
 * 即使视图为空也保留的结构组 id：文档组在内置布局里本来就是空的（文档面板由
 * Workbench 的同步逻辑随后补上）；组件组和右侧组是外壳结构。图标条组（'strip'）
 * 目前没有内置叶节点，列在这里是防止以后内置布局里出现它时被误删。
 */
const STRUCTURAL_GROUP_IDS: readonly string[] = [
  COMPONENTS_GROUP_ID,
  DOCUMENTS_GROUP_ID,
  STRIP_GROUP_ID,
  'right-top',
  'right-bottom',
  'right',
];

function collectLeafIds(root: WorkspaceGridNode, into: Set<string>): void {
  if (root.type === 'branch') {
    for (const child of root.data as WorkspaceGridNode[]) collectLeafIds(child, into);
    return;
  }
  into.add((root.data as WorkspaceGroupState).id);
}

/**
 * 清理序列化的 dockview 布局：去掉文档面板，并丢弃变空的非结构组。
 *
 * 背景：旧的 captureWorkspace() 只从面板表和浮动组里去掉文档面板，网格叶节
 * 点的视图里还留着 `doc:*`；dockview 8.8.3 在 fromJSON 时会跳过面板表里没
 * 有的视图、把组留空，而 closeEmptyGroups={false} 时空组不自动消失，只显示
 * “没有打开的文档”水印——旧保存值里的空组会在每次刷新后复活。右侧图标条
 * 也是同类问题：ensureStrip() 每次加载都在 components 组右侧新建一组放图
 * 标条，上一轮的组在保存值里只剩 `['strip']` 视图，面板表里又没有它，下次
 * 加载就多一个空组。
 *
 * 清理分三步：先用 stripDocumentPanels() 去掉文档面板；再把每个叶节点的
 * 视图过滤到面板表里还存在的（面板表里没有的视图在 fromJSON 时同样会被
 * 跳过）；最后删掉视图已空的非结构叶节点（变空的分支一并删掉，根分支除外）。
 * 结构组即使视图为空也保留。activeGroup 指向被删的组时改指文档组，文档
 * 组也不存在时删掉。输入不改动，返回新对象。
 */
export function cleanDockviewLayout(layout: SerializedDockview): SerializedDockview {
  const stripped = stripDocumentPanels(layout);
  const panels = stripped.panels;
  const cleanLeaf = (data: WorkspaceGroupState): WorkspaceGroupState => {
    const views = data.views.filter((v) => panels[v]);
    const next: WorkspaceGroupState = { ...data, views };
    if (next.activeView && !views.includes(next.activeView)) {
      if (views.length > 0) next.activeView = views[0]!;
      else delete next.activeView;
    }
    return next;
  };
  const prune = (node: WorkspaceGridNode): WorkspaceGridNode | null => {
    if (node.type === 'branch') {
      const children = (node.data as WorkspaceGridNode[])
        .map(prune)
        .filter((child): child is WorkspaceGridNode => child !== null);
      return children.length > 0 ? { ...node, data: children } : null;
    }
    const data = cleanLeaf(node.data as WorkspaceGroupState);
    if (data.views.length === 0 && !STRUCTURAL_GROUP_IDS.includes(data.id)) return null;
    return { ...node, data };
  };
  const root = stripped.grid.root as unknown as WorkspaceGridNode;
  const pruned = prune(root);
  // 极端情况：所有叶节点都被删掉时保留原网格，不造出一个空根分支
  const nextRoot = pruned ?? root;
  const result: SerializedDockview = {
    ...stripped,
    grid: { ...stripped.grid, root: nextRoot as unknown as SerializedDockview['grid']['root'] },
  };
  if (stripped.floatingGroups) {
    result.floatingGroups = stripped.floatingGroups.filter(
      (group) => (group.data?.views ?? []).length > 0,
    );
  }
  const remaining = new Set<string>();
  collectLeafIds(nextRoot, remaining);
  if (result.activeGroup && !remaining.has(result.activeGroup)) {
    if (remaining.has(DOCUMENTS_GROUP_ID)) result.activeGroup = DOCUMENTS_GROUP_ID;
    else delete result.activeGroup;
  }
  return result;
}

/** 两侧组的 id：组件面板组和右侧停靠组（货代单证的右上/右下、基础的右侧组） */
const SIDE_GROUP_IDS: readonly string[] = [
  COMPONENTS_GROUP_ID,
  'right-top',
  'right-bottom',
  'right',
];

function clampSideWidth(width: number): number {
  return Math.min(SIDE_PANEL_MAX_WIDTH, Math.max(SIDE_PANEL_MIN_WIDTH, width));
}

function subtreeHasSideLeaf(node: WorkspaceGridNode): boolean {
  if (node.type === 'branch') {
    return (node.data as WorkspaceGridNode[]).some(subtreeHasSideLeaf);
  }
  return SIDE_GROUP_IDS.includes((node.data as WorkspaceGroupState).id);
}

/**
 * 把两侧宽度收进 200–320px：给组件面板组和右侧停靠组写上宽度约束，横向尺寸超出范围的夹到范围内。
 *
 * 背景：右侧组之前没有宽度约束，拖分隔条能把它拉到 368px 甚至更宽，挤占画布；旧保存值里的
 * 宽度下次加载也会原样恢复。内置布局的约束只管新布局，保存值要靠这里在 fromJSON 之前夹一次，
 * 运行时再由 workbenchState.enforceSideWidths() 兜底。竖向尺寸（右上/右下两组的高度）不动；
 * 折叠成 32px 图标条的组（locked: 'no-drop-target'）也不动。输入不改动，返回新对象。
 */
export function clampSideWidths(layout: SerializedDockview): SerializedDockview {
  const visit = (
    node: WorkspaceGridNode,
    parentHorizontal: boolean,
    isRoot: boolean,
  ): WorkspaceGridNode => {
    if (node.type === 'branch') {
      // 子节点的 size 沿本分支的朝向度量：根分支的朝向就是网格的 orientation；
      // 子分支的朝向与父分支相反，也就是与本节点 size 的度量轴相反
      const childHorizontal = isRoot ? parentHorizontal : !parentHorizontal;
      const children = (node.data as WorkspaceGridNode[]).map((child) =>
        visit(child, childHorizontal, false),
      );
      // 横向分支里装着右侧组的分支（如货代单证的右上/右下容器），它的 size 就是右侧宽度
      if (!isRoot && parentHorizontal && node.size !== undefined && subtreeHasSideLeaf(node)) {
        return { ...node, data: children, size: clampSideWidth(node.size) };
      }
      return { ...node, data: children };
    }
    const data = node.data as WorkspaceGroupState;
    if (!SIDE_GROUP_IDS.includes(data.id) || data.locked === 'no-drop-target') return node;
    const next: WorkspaceGroupState = {
      ...data,
      constraints: { ...SIDE_PANEL_CONSTRAINTS },
    };
    if (!parentHorizontal || node.size === undefined) return { ...node, data: next };
    return { ...node, data: next, size: clampSideWidth(node.size) };
  };
  const root = layout.grid.root as unknown as WorkspaceGridNode;
  // 根网格的方向：没写时按横向算（两个内置工作区都是横向根网格）
  const horizontal = layout.grid.orientation !== 'VERTICAL';
  const nextRoot = visit(root, horizontal, true);
  return {
    ...layout,
    grid: { ...layout.grid, root: nextRoot as unknown as SerializedDockview['grid']['root'] },
  };
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
  // 版本 2：工具栏已移到外壳顶部，不再是浮动面板；旧版 v1 保存值解析为无效，自动回落到内置布局
  if (value.version !== 2) return false;
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
