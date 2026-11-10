import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { DockviewApi } from 'dockview-react';
import { cssVar } from '@magpdf/ui';
import { runBenchmark } from '../canvas/benchmark';
import type { BenchmarkResult } from '../canvas/benchmark';
import { canvasRegistry } from '../canvas/canvasController';
import { createMockDocument } from '../document/mockDocument';
import { format, t } from '../i18n';
import { editorStore } from '../store/editorStore';
import type { EditorStore } from '../store/editorStore';
import type { CommandId } from '../tools/shortcuts';
import { defaultShortcutMap, isTextInputTarget } from '../tools/shortcuts';
import { toolForKey } from '../tools/tools';
import type { ToolId } from '../tools/tools';
import type { PanelId, WorkspaceId, WorkspaceLayout } from '../workspace/workspaceTypes';
import {
  builtinWorkspace,
  loadCurrentWorkspaceId,
  loadWorkspace,
  panelTitle,
  resetWorkspace as clearSavedWorkspace,
  saveCurrentWorkspaceId,
  saveWorkspace,
} from '../workspace/workspaces';
import type { WorkspaceStorage } from '../workspace/workspaces';
import { BenchmarkDialog } from './Dialogs';
import { MenuBar } from './MenuBar';
import { OptionsBar } from './OptionsBar';
import { ShellContext } from './shellContext';
import type { ShellCommands } from './shellContext';
import { StatusBar } from './StatusBar';
import { Toolbar, ToolbarSwatches } from './Toolbar';
import { Workbench } from './Workbench';
import { builtinCounts, documentPreset } from './presets';

export interface EditorShellProps {
  readonly store?: EditorStore;
  /** 工作区持久化；不传时不保存（测试、嵌入用） */
  readonly storage?: WorkspaceStorage | null;
  readonly initialWorkspace?: WorkspaceId;
  /** 是否在挂载时打开示例文档（默认打开「合同.pdf」与「提单.pdf」） */
  readonly demoDocuments?: boolean;
}

function browserStorage(): WorkspaceStorage | null {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
}

function fallbackSize(): { width: number; height: number } {
  return { width: 1440, height: 900 };
}

/**
 * 编辑器外壳（方案 8.2）：
 * 菜单栏 28 / 顶部工具栏与选项栏 36（同一行：7 个工具横排 + 工具选项，填充/描边色块在行尾）/ 工作台（组件面板 240 + 文档标签 28 + 画布 + 右侧停靠 280）/ 状态栏 22。
 * 工具栏是外壳的普通横条，不再是 dockview 浮动面板。
 */
export function EditorShell({
  store = editorStore,
  storage,
  initialWorkspace,
  demoDocuments = true,
}: EditorShellProps) {
  const persist = storage === undefined ? browserStorage() : storage;
  const rootRef = useRef<HTMLDivElement>(null);
  const [workspaceId, setWorkspaceId] = useState<WorkspaceId>(
    () => initialWorkspace ?? (persist ? loadCurrentWorkspaceId(persist) : 'freight'),
  );
  const [layout, setLayout] = useState<WorkspaceLayout>(() => {
    const saved = persist
      ? loadWorkspace(persist, initialWorkspace ?? loadCurrentWorkspaceId(persist))
      : null;
    return saved ?? builtinWorkspace(initialWorkspace ?? 'freight', fallbackSize());
  });
  const apiRef = useRef<DockviewApi | null>(null);
  const [benchmark, setBenchmark] = useState<{ result: BenchmarkResult | null; running: boolean }>({
    result: null,
    running: false,
  });

  const doc = useStoreDoc(store);
  const showRulers = useStoreValue(store, (s) => s.showRulers);
  const showFrameStats = useStoreValue(store, (s) => s.showFrameStats);
  const componentMode = useStoreValue(store, (s) => s.componentPanelMode);
  const workspace = useStoreValue(store, (s) => s.workspace);

  // 首次挂载：打开示例文档、按 store 的工作区初始化
  const initialized = useRef(false);
  useEffect(() => {
    if (initialized.current) return;
    initialized.current = true;
    rootRef.current?.style.setProperty('--mg-canvas-backdrop-fallback', cssVar('canvasBackdrop'));
    store.getState().setWorkspace(workspaceId);
    if (demoDocuments && store.getState().documentOrder.length === 0) {
      const contract = createMockDocument({
        id: 'contract',
        name: t.mock.contractName,
        pageCount: 6,
        template: 'report',
      });
      store.getState().openDocument(contract);
      store.getState().openDocument(
        createMockDocument({
          id: 'bl',
          name: t.mock.billOfLadingName,
          pageCount: builtinCounts.billOfLading,
          template: 'billOfLading',
          dirty: false,
        }),
      );
    }
    // 只在挂载时执行一次
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 开发模式的调试入口（控制台 __magpdf）：放在单独 effect 里，StrictMode 双挂载也安全
  useEffect(() => {
    if (!isDevBuild()) return undefined;
    const hook = {
      benchmark: async (frames?: number) => {
        const id = store.getState().activeDocumentId;
        const controller = id ? canvasRegistry.get(id) : undefined;
        if (!controller) throw new Error('没有打开的文档');
        return runBenchmark(controller, store, frames ? { framesPerPhase: frames } : {});
      },
      store,
      documents: () => store.getState().documentOrder,
      selection: () => store.getState().activeDocumentId,
      open: (preset: 'blank' | 'contract' | 'billOfLading' | 'large') => {
        const count = store.getState().documentOrder.filter((d) => d.startsWith('mock')).length + 1;
        store.getState().openDocument(documentPreset(preset, count));
      },
      activate: (id: string) => store.getState().activateDocument(id),
    };
    (window as unknown as { __magpdf?: typeof hook }).__magpdf = hook;
    return () => {
      delete (window as unknown as { __magpdf?: typeof hook }).__magpdf;
    };
  }, [store]);

  // 文档标题变化（新建、脏标记）同步到标签页
  useEffect(() => {
    if (!apiRef.current) return undefined;
    return store.subscribe((state, prev) => {
      const api = apiRef.current;
      if (!api) return;
      for (const id of state.documentOrder) {
        if (state.documents[id] === prev.documents[id]) continue;
        const panel = api.getPanel(`doc:${id}`);
        const doc = state.documents[id];
        if (panel && doc) panel.api.setTitle(doc.snapshot.name);
      }
    });
  }, [store]);

  const activeController = useCallback(() => {
    const id = store.getState().activeDocumentId;
    return id ? (canvasRegistry.get(id) ?? null) : null;
  }, [store]);

  const commands = useMemo<ShellCommands>(() => {
    const run = (command: CommandId): boolean => {
      switch (command) {
        case 'view.toggleRulers':
          store.getState().toggleRulers();
          return true;
        case 'window.toggleFrameStats':
          store.getState().toggleFrameStats();
          return true;
        case 'window.togglePanels':
          store.getState().togglePanelsHidden();
          return true;
        case 'window.resetWorkspace':
          if (persist) clearSavedWorkspace(persist, workspaceId);
          applyLayout(builtinWorkspace(workspaceId, sizeOf(rootRef.current)));
          return true;
        case 'window.benchmark':
          void startBenchmark();
          return true;
        case 'file.open':
          openDocument('billOfLading');
          return true;
        case 'file.close':
          closeActiveDocument();
          return true;
        default:
          break;
      }
      const controller = activeController();
      if (controller) return controller.runCommand(command);
      return false;
    };

    /** 换布局时同步图标条里的面板（每个工作区记住自己那组） */
    const applyLayout = (next: WorkspaceLayout) => {
      setLayout(next);
      store.getState().setCollapsedPanels(next.collapsedPanels);
    };

    const startBenchmark = async () => {
      const controller = activeController();
      if (!controller || benchmark.running) return;
      setBenchmark({ result: null, running: true });
      try {
        const result = await runBenchmark(controller, store);
        setBenchmark({ result, running: false });
      } catch (error) {
        console.error(error);
        setBenchmark({ result: null, running: false });
      }
    };

    const openDocument = (preset: Parameters<typeof documentPreset>[0]) => {
      const count = store.getState().documentOrder.filter((id) => id.startsWith('mock')).length + 1;
      store.getState().openDocument(documentPreset(preset, count));
    };

    const closeActiveDocument = () => {
      const id = store.getState().activeDocumentId;
      if (!id) return;
      const panel = apiRef.current?.getPanel(`doc:${id}`);
      if (panel) panel.api.close();
      else store.getState().closeDocument(id);
    };

    const applyWorkspace = (id: WorkspaceId) => {
      const saved = persist ? loadWorkspace(persist, id) : null;
      setWorkspaceId(id);
      store.getState().setWorkspace(id);
      if (persist) saveCurrentWorkspaceId(persist, id);
      applyLayout(saved ?? builtinWorkspace(id, sizeOf(rootRef.current)));
    };

    const resetWorkspace = () => {
      if (persist) clearSavedWorkspace(persist, workspaceId);
      applyLayout(builtinWorkspace(workspaceId, sizeOf(rootRef.current)));
    };

    const setComponentsPanel = (mode: 'expanded' | 'collapsed' | 'hidden') => {
      store.getState().setComponentPanelMode(mode);
    };

    const openPanel = (id: PanelId) => {
      const api = apiRef.current;
      if (!api || api.getPanel(id)) {
        api?.getPanel(id)?.api.setActive();
        return;
      }
      const reference = api.groups.find((group) => group.id.startsWith('right')) ?? api.groups[0];
      api.addPanel({
        id,
        component: id,
        title: panelTitle(id),
        ...(reference
          ? { position: { referenceGroup: reference.id, direction: 'right' as const } }
          : {}),
      });
    };

    return {
      controller: activeController,
      run,
      openDocument,
      closeActiveDocument,
      applyWorkspace,
      resetWorkspace,
      runBenchmark: () => void startBenchmark(),
      toggleFrameStats: () => store.getState().toggleFrameStats(),
      togglePanels: () => store.getState().togglePanelsHidden(),
      setComponentsPanel,
      setCollapsedPanels: (panels) => store.getState().setCollapsedPanels(panels),
      openPanel,
      toggleRulers: () => store.getState().toggleRulers(),
    };
  }, [store, activeController, persist, workspaceId, benchmark.running]);

  // 全局键盘：工具快捷键、视图命令、Space 临时抓手
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (isTextInputTarget(event.target)) return;
      if (event.key === ' ' && !event.repeat) {
        const controller = activeController();
        if (controller?.handleKeyDown(event)) event.preventDefault();
        return;
      }
      const toolSpec =
        !event.ctrlKey && !event.metaKey && !event.altKey ? toolForKey(event.key) : undefined;
      if (toolSpec) {
        store.getState().setTool(toolSpec.id);
        event.preventDefault();
        return;
      }
      if (event.key === 'Escape') {
        const controller = activeController();
        controller?.runCommand('edit.deselect');
        return;
      }
      if (event.ctrlKey && (event.key === '=' || event.key === '+')) event.preventDefault();
      const command = defaultShortcutMap.resolve(event);
      if (!command) return;
      if (command.startsWith('tool.')) {
        store.getState().setTool(command.slice(5) as ToolId);
        event.preventDefault();
        return;
      }
      if (commands.run(command)) event.preventDefault();
    };
    const onKeyUp = (event: KeyboardEvent) => {
      if (event.key === ' ') activeController()?.handleKeyUp(event);
    };
    window.addEventListener('keydown', onKeyDown);
    window.addEventListener('keyup', onKeyUp);
    return () => {
      window.removeEventListener('keydown', onKeyDown);
      window.removeEventListener('keyup', onKeyUp);
    };
  }, [activeController, commands, store]);

  useEffect(() => {
    if (!doc) return;
    document.title = `${doc.snapshot.name}${doc.snapshot.dirty ? t.documents.dirtyMark : ''} - ${t.app.name}`;
  }, [doc]);

  const onLayoutChange = useCallback(
    (next: WorkspaceLayout) => {
      if (persist) saveWorkspace(persist, next);
    },
    [persist],
  );

  return (
    <ShellContext.Provider value={commands}>
      <div
        className={`mg-shell mg-root${workspace === 'freight' ? ' mg-ws-freight' : ' mg-ws-basic'}`}
        ref={rootRef}
      >
        <MenuBar workspace={workspace} showRulers={showRulers} showFrameStats={showFrameStats} />
        <div className="mg-topbar">
          <Toolbar />
          <OptionsBar workspace={workspace} />
          <ToolbarSwatches />
        </div>
        <div className={`mg-body${componentMode === 'hidden' ? ' mg-hide-components' : ''}`}>
          <Workbench
            store={store}
            workspace={layout}
            onLayoutChange={onLayoutChange}
            onReady={(api) => {
              apiRef.current = api;
            }}
          />
        </div>
        <StatusBar />
        <BenchmarkDialog
          result={benchmark.result}
          running={benchmark.running}
          onClose={() => setBenchmark({ result: null, running: false })}
        />
      </div>
    </ShellContext.Provider>
  );
}

/**
 * Vite 在开发构建里把 import.meta.env.DEV 直接替换成 true；编辑器包不依赖 vite/client 类型，
 * 这里读得宽松些。构建产物里没有调试入口以外的副作用，保留不影响用户。
 */
function isDevBuild(): boolean {
  const meta = import.meta as ImportMeta & { env?: { DEV?: boolean } };
  return meta.env?.DEV !== false;
}

function sizeOf(element: HTMLElement | null): { width: number; height: number } {
  return element ? { width: element.clientWidth, height: element.clientHeight } : fallbackSize();
}

function useStoreValue<T>(
  store: EditorStore,
  selector: (state: ReturnType<EditorStore['getState']>) => T,
): T {
  const [value, setValue] = useState(() => selector(store.getState()));
  useEffect(() => store.subscribe((state) => setValue(selector(state))), [store, selector]);
  return value;
}

function useStoreDoc(store: EditorStore) {
  return useStoreValue(store, (s) =>
    s.activeDocumentId ? s.documents[s.activeDocumentId] : undefined,
  );
}

export { format };
