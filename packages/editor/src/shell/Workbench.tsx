import { useCallback, useEffect, useRef, useState } from 'react';
import { DockviewReact } from 'dockview-react';
import type { DockviewApi, DockviewReadyEvent, IDockviewPanelProps } from 'dockview-react';
import 'dockview-react/dist/styles/dockview.css';
import './shell.css';
import { t } from '../i18n';
import { editorStore } from '../store/editorStore';
import type { EditorStore } from '../store/editorStore';
import { TileCanvas } from '../canvas/TileCanvas';
import { DocumentTab, DocumentsWatermark } from './DocumentTab';
import { ComponentsPanel, ComponentStrip } from './panels/ComponentsPanel';
import { LayersPanel } from './panels/LayersPanel';
import { PagesPanel } from './panels/PagesPanel';
import { CollapsedStrip, HistoryPanel, PlaceholderPanel } from './panels/PlaceholderPanels';
import { useShellCommands } from './shellContext';
import { PropertiesPanel } from './panels/PropertiesPanel';
import { Toolbar } from './Toolbar';
import {
  applyWorkspace,
  captureWorkspace,
  COMPONENTS_GROUP,
  DOC_PANEL_PREFIX,
  MAGPDF_THEME,
  TOOLBAR_PANEL,
} from './workbenchState';
import type { PanelId, WorkspaceLayout } from '../workspace/workspaceTypes';

export interface WorkbenchProps {
  readonly store: EditorStore;
  readonly workspace: WorkspaceLayout;
  readonly onLayoutChange?: (layout: WorkspaceLayout) => void;
  readonly onReady?: (api: DockviewApi) => void;
}

/** 文档面板：每个文档一个瓦片画布 */
function CanvasPanel(props: IDockviewPanelProps<{ documentId: string }>) {
  return <TileCanvas documentId={props.params.documentId} store={editorStore} />;
}

const Placeholder = ({ name }: { readonly name: string }) => <PlaceholderPanel name={name} />;

const COMPONENTS: Record<string, React.FunctionComponent<IDockviewPanelProps>> = {
  canvas: CanvasPanel as React.FunctionComponent<IDockviewPanelProps>,
  components: ComponentsPanel,
  properties: PropertiesPanel,
  character: () => <Placeholder name={t.panels.character} />,
  paragraph: () => <Placeholder name={t.panels.paragraph} />,
  layers: LayersPanel,
  pages: PagesPanel,
  history: HistoryPanel,
  navigator: () => <Placeholder name={t.panels.navigator} />,
  swatches: () => <Placeholder name={t.panels.swatches} />,
  info: () => <Placeholder name={t.panels.info} />,
  strip: ComponentStrip,
  'right-strip': RightStrip,
  toolbar: () => <Toolbar />,
};

/** 右侧图标条：点图标把面板浮到右侧（命令由 EditorShell 提供） */
function RightStrip() {
  const commands = useShellCommands();
  return <CollapsedStrip onOpen={(id) => commands.openPanel(id)} />;
}

const TAB_COMPONENTS = { 'doc-tab': DocumentTab };

/**
 * 工作台：dockview 外壳 + 文档面板同步。
 * 关闭标签时先同步 store；文档增删由 store 订阅驱动 dockview。
 */
export function Workbench({ store, workspace, onLayoutChange, onReady }: WorkbenchProps) {
  const apiRef = useRef<DockviewApi | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const [ready, setReady] = useState(false);
  const [panelSink] = useState(() => ({
    open: (_id: PanelId) => undefined,
  }));
  const workspaceRef = useRef(workspace);
  workspaceRef.current = workspace;

  const syncDocuments = useCallback(
    (api: DockviewApi) => {
      const state = store.getState();
      const wanted = new Set(state.documentOrder.map((id) => DOC_PANEL_PREFIX + id));
      for (const panel of [...api.panels]) {
        if (panel.id.startsWith(DOC_PANEL_PREFIX) && !wanted.has(panel.id)) api.removePanel(panel);
      }
      for (const id of state.documentOrder) {
        const doc = state.documents[id];
        if (!doc) continue;
        const panelId = DOC_PANEL_PREFIX + id;
        if (api.getPanel(panelId)) continue;
        api.addPanel({
          id: panelId,
          component: 'canvas',
          tabComponent: 'doc-tab',
          title: doc.snapshot.name,
          params: { documentId: id },
          position: { referenceGroup: 'documents', index: state.documentOrder.indexOf(id) },
        });
      }
      if (state.activeDocumentId)
        api.getPanel(DOC_PANEL_PREFIX + state.activeDocumentId)?.api.setActive();
    },
    [store],
  );

  const layoutAll = useCallback(
    (api: DockviewApi) => {
      const container = containerRef.current;
      const width = container?.clientWidth ?? 1440;
      const height = container?.clientHeight ?? 900;
      // 先记下上一轮用户拖出来的组件面板宽度，重新应用布局后按它还原
      const group = api.getGroup(COMPONENTS_GROUP) as
        { api?: { boundingBox?: { width: number } } } | undefined;
      const previously = group?.api?.boundingBox?.width;
      applyWorkspace(api, workspaceRef.current, width, height);
      const componentsWidth = workspaceRef.current.sideWidths.components;
      if (previously && Math.abs(previously - componentsWidth) > 1) {
        api.getGroup(COMPONENTS_GROUP)?.api.setSize({ width: previously });
      }
      for (const panel of [...api.panels]) {
        if (panel.id.startsWith(DOC_PANEL_PREFIX)) api.removePanel(panel);
      }
      syncDocuments(api);
      const toolbar = api.getPanel(TOOLBAR_PANEL);
      if (toolbar) {
        api.addFloatingGroup(toolbar, {
          x: 0,
          y: 40,
          width: 40,
          height: Math.min(560, height - 120),
        });
      }
    },
    [syncDocuments],
  );

  const handleReady = useCallback(
    (event: DockviewReadyEvent) => {
      const api = event.api;
      apiRef.current = api;
      layoutAll(api);
      void panelSink;
      setReady(true);
      onReady?.(api);
      api.onDidActivePanelChange((e) => {
        const id = e.panel?.id;
        if (id?.startsWith(DOC_PANEL_PREFIX))
          store.getState().activateDocument(id.slice(DOC_PANEL_PREFIX.length));
      });
      api.onDidRemovePanel((panel) => {
        if (!panel.id.startsWith(DOC_PANEL_PREFIX)) return;
        const documentId = panel.id.slice(DOC_PANEL_PREFIX.length);
        if (store.getState().documents[documentId]) store.getState().closeDocument(documentId);
      });
    },
    [layoutAll, onReady, store, panelSink],
  );

  // store：文档增删/切换 → dockview
  useEffect(() => {
    if (!apiRef.current) return undefined;
    let scheduled = false;
    const unsubscribe = store.subscribe((state, prev) => {
      if (
        state.documentOrder === prev.documentOrder &&
        state.activeDocumentId === prev.activeDocumentId
      )
        return;
      if (scheduled) return;
      scheduled = true;
      queueMicrotask(() => {
        scheduled = false;
        const api = apiRef.current;
        if (!api) return;
        syncDocuments(api);
        const active = store.getState().activeDocumentId;
        if (active) api.getPanel(DOC_PANEL_PREFIX + active)?.api.setActive();
      });
    });
    return unsubscribe;
  }, [ready, store, syncDocuments]);

  // 切换工作区：整体重排
  useEffect(() => {
    const api = apiRef.current;
    if (!api || !ready) return;
    layoutAll(api);
  }, [ready, workspace, layoutAll]);

  // 容器尺寸变化 → api.layout（关掉自动布局，文档区吸收变化）
  useEffect(() => {
    const container = containerRef.current;
    if (!container || typeof ResizeObserver === 'undefined') return undefined;
    let frame = 0;
    const observer = new ResizeObserver(() => {
      if (frame) return;
      frame = requestAnimationFrame(() => {
        frame = 0;
        apiRef.current?.layout(container.clientWidth, container.clientHeight);
      });
    });
    observer.observe(container);
    return () => {
      observer.disconnect();
      if (frame) cancelAnimationFrame(frame);
    };
  }, []);

  // 布局变化（拖分隔条、拖面板）后保存工作区
  const save = useCallback(() => {
    const api = apiRef.current;
    if (!api) return;
    const state = store.getState();
    onLayoutChange?.(
      captureWorkspace(
        api,
        workspaceRef.current.id,
        state.componentPanelMode,
        state.collapsedPanels,
      ),
    );
  }, [store, onLayoutChange]);

  useEffect(() => {
    const api = apiRef.current;
    if (!api || !ready) return undefined;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const sub = api.onDidLayoutChange(() => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(save, 500);
    });
    return () => {
      sub.dispose();
      if (timer) clearTimeout(timer);
    };
  }, [ready, save]);

  return (
    <div className="mg-workbench" ref={containerRef}>
      <DockviewReact
        className="mg-dockview"
        components={COMPONENTS}
        tabComponents={TAB_COMPONENTS}
        watermarkComponent={DocumentsWatermark}
        onReady={handleReady}
        theme={MAGPDF_THEME}
        disableAutoResizing
        proportionalLayout={false}
        closeEmptyGroups={false}
        singleTabMode="fullwidth"
      />
    </div>
  );
}
