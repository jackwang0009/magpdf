import { useCallback, useEffect, useRef, useState } from 'react';
import { DockviewReact } from 'dockview-react';
import type { DockviewApi, DockviewReadyEvent, IDockviewPanelProps } from 'dockview-react';
import 'dockview-react/dist/styles/dockview.css';
import './shell.css';
import { t } from '../i18n';
import { editorStore } from '../store/editorStore';
import type { EditorStore } from '../store/editorStore';
import { TileCanvas } from '../canvas/TileCanvas';
import { DocumentBanner } from './DocumentBanner';
import { DocumentTab, DocumentsWatermark } from './DocumentTab';
import { useEditorHost } from './editorHost';
import { ComponentsPanel, ComponentStrip } from './panels/ComponentsPanel';
import { LayersPanel } from './panels/LayersPanel';
import { PagesPanel } from './panels/PagesPanel';
import { CollapsedStrip, HistoryPanel, PlaceholderPanel } from './panels/PlaceholderPanels';
import { useShellCommands } from './shellContext';
import { PropertiesPanel } from './panels/PropertiesPanel';
import {
  applyWorkspace,
  captureWorkspace,
  COMPONENTS_GROUP,
  DOC_PANEL_PREFIX,
  MAGPDF_THEME,
} from './workbenchState';
import type { PanelId, WorkspaceLayout } from '../workspace/workspaceTypes';

export interface WorkbenchProps {
  readonly store: EditorStore;
  readonly workspace: WorkspaceLayout;
  readonly onLayoutChange?: (layout: WorkspaceLayout) => void;
  readonly onReady?: (api: DockviewApi) => void;
}

/**
 * 文档面板：顶上是这份文档的提示条（有要说的才出现），下面是瓦片画布。宿主打开的文档，瓦片找宿主要、
 * 修改交给宿主；其余的（模拟文档）用模拟来源，修改直接落在 store 上。
 */
function CanvasPanel(props: IDockviewPanelProps<{ documentId: string }>) {
  const host = useEditorHost();
  const documentId = props.params.documentId;
  const provider = host?.tileProvider(documentId);
  const editor = host?.documentEditor(documentId);
  return (
    <div className="mg-doc-panel">
      <DocumentBanner documentId={documentId} />
      <TileCanvas
        documentId={documentId}
        store={editorStore}
        {...(provider ? { provider } : {})}
        {...(editor ? { editor } : {})}
      />
    </div>
  );
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
  /** 正在重排布局。这期间文档面板被拿掉只是为了重建，不是用户关了文档。 */
  const rebuilding = useRef(false);

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
      // 重排时文档面板先全部拿掉，再按 store 里的文档重新建。拿掉面板会触发下面的 onDidRemovePanel，
      // 那是给“用户关了标签页”用的；不拦住的话，切换一次工作区，打开的文档就全被关掉了。
      rebuilding.current = true;
      try {
        applyWorkspace(api, workspaceRef.current, width, height);
        const componentsWidth = workspaceRef.current.sideWidths.components;
        if (previously && Math.abs(previously - componentsWidth) > 1) {
          api.getGroup(COMPONENTS_GROUP)?.api.setSize({ width: previously });
        }
        for (const panel of [...api.panels]) {
          if (panel.id.startsWith(DOC_PANEL_PREFIX)) api.removePanel(panel);
        }
      } finally {
        rebuilding.current = false;
      }
      syncDocuments(api);
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
        if (rebuilding.current || !panel.id.startsWith(DOC_PANEL_PREFIX)) return;
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
