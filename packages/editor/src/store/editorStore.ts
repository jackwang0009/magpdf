import { createStore } from 'zustand/vanilla';
import type { StoreApi } from 'zustand/vanilla';
import { useStore } from 'zustand';
import type { FrameStatsSummary } from '../canvas/frameStats';
import type { DocumentSnapshot, PageObject } from '../document/types';
import { translateRect } from '../geometry';
import type { Rect } from '../geometry';
import { format, t } from '../i18n';
import { NUDGE_STEP_PT } from '../tools/selectTool';
import type { ToolOptions } from '../tools/machine';
import type { ToolId } from '../tools/tools';
import { DEFAULT_VIEW } from '../view/viewState';
import type { ViewState } from '../view/viewState';
import type { ComponentPanelMode, PanelId, WorkspaceId } from '../workspace/workspaceTypes';
import { STARTER_PANELS } from '../workspace/workspaceTypes';

/**
 * 编辑器状态（Zustand）：文档快照、每个文档的视图/选区、工具、工作区开关。
 * 画布控制器在 React 之外订阅它；面板用 `useEditorStore(selector)`。
 */
export interface DocumentState {
  readonly snapshot: DocumentSnapshot;
  readonly view: ViewState;
  readonly selection: readonly string[];
  /** 视口中占比最大的页（状态栏、页面面板高亮） */
  readonly currentPage: number;
  /** 历史记录面板的占位数据：最近的操作描述 */
  readonly history: readonly string[];
}

export type ComponentView = 'grid' | 'list';

export interface PointerInfo {
  readonly page: number;
  /** 页内坐标（pt） */
  readonly x: number;
  readonly y: number;
}

export interface EditorState {
  readonly documents: Readonly<Record<string, DocumentState>>;
  readonly documentOrder: readonly string[];
  readonly activeDocumentId: string | null;
  readonly activeTool: ToolId;
  readonly toolOptions: ToolOptions;
  readonly pointer: PointerInfo | null;
  readonly showRulers: boolean;
  readonly showFrameStats: boolean;
  readonly frameStats: FrameStatsSummary | null;
  readonly benchmarkRunning: boolean;
  readonly workspace: WorkspaceId;
  readonly componentPanelMode: ComponentPanelMode;
  readonly componentView: ComponentView;
  readonly panelsHidden: boolean;
  /** 折叠到右侧图标条里的面板 */
  readonly collapsedPanels: readonly PanelId[];

  openDocument(snapshot: DocumentSnapshot, options?: { activate?: boolean }): void;
  /**
   * 换掉一份已经打开的文档的快照（引擎推送了变化、对象列表到了）。视图不动；
   * 选区里已经不存在的对象去掉。文档没有打开时什么也不做。
   */
  updateSnapshot(id: string, snapshot: DocumentSnapshot): void;
  closeDocument(id: string): void;
  activateDocument(id: string): void;
  setView(id: string, view: ViewState): void;
  setCurrentPage(id: string, page: number): void;
  setSelection(id: string, ids: readonly string[]): void;
  moveObjects(id: string, ids: readonly string[], dx: number, dy: number): void;
  resizeObjects(id: string, rects: ReadonlyMap<string, Rect>): void;
  deleteObjects(id: string, ids: readonly string[]): void;
  setTool(tool: ToolId): void;
  setToolOptions(options: Partial<ToolOptions>): void;
  setPointer(pointer: PointerInfo | null): void;
  toggleRulers(): void;
  toggleFrameStats(): void;
  setFrameStats(summary: FrameStatsSummary | null): void;
  setBenchmarkRunning(running: boolean): void;
  setWorkspace(id: WorkspaceId): void;
  setComponentPanelMode(mode: ComponentPanelMode): void;
  setComponentView(view: ComponentView): void;
  togglePanelsHidden(): void;
  setCollapsedPanels(panels: readonly PanelId[]): void;
}

export const DEFAULT_TOOL_OPTIONS: ToolOptions = {
  marqueeMode: 'intersect',
  nudgeStep: NUDGE_STEP_PT,
  dragThreshold: 4,
};

const MAX_HISTORY = 50;

export type EditorStore = StoreApi<EditorState>;

export function createEditorStore(): EditorStore {
  return createStore<EditorState>()((set, get) => {
    const updateDocument = (id: string, update: (doc: DocumentState) => DocumentState) => {
      const doc = get().documents[id];
      if (!doc) return;
      set({ documents: { ...get().documents, [id]: update(doc) } });
    };

    const editObjects = (
      id: string,
      ids: readonly string[],
      label: string,
      edit: (object: PageObject) => PageObject | null,
    ) => {
      updateDocument(id, (doc) => {
        const wanted = new Set(ids);
        let changed = false;
        const pages = doc.snapshot.pages.map((page) => {
          if (!page.objects.some((o) => wanted.has(o.id))) return page;
          changed = true;
          const objects: PageObject[] = [];
          for (const object of page.objects) {
            if (!wanted.has(object.id)) {
              objects.push(object);
              continue;
            }
            const next = edit(object);
            if (next) objects.push(next);
          }
          return { ...page, objects };
        });
        if (!changed) return doc;
        const remaining = new Set(pages.flatMap((p) => p.objects.map((o) => o.id)));
        return {
          ...doc,
          snapshot: { ...doc.snapshot, pages, version: doc.snapshot.version + 1, dirty: true },
          selection: doc.selection.filter((sid) => remaining.has(sid)),
          history: [...doc.history, label].slice(-MAX_HISTORY),
        };
      });
    };

    return {
      documents: {},
      documentOrder: [],
      activeDocumentId: null,
      activeTool: 'select',
      toolOptions: DEFAULT_TOOL_OPTIONS,
      pointer: null,
      showRulers: true,
      showFrameStats: false,
      frameStats: null,
      benchmarkRunning: false,
      workspace: 'freight',
      componentPanelMode: 'expanded',
      componentView: 'grid',
      panelsHidden: false,
      collapsedPanels: STARTER_PANELS,

      openDocument(snapshot, options) {
        const state = get();
        const existing = state.documents[snapshot.id];
        const doc: DocumentState = existing
          ? { ...existing, snapshot }
          : {
              snapshot,
              view: DEFAULT_VIEW,
              selection: [],
              currentPage: 0,
              history: [t.history.opened],
            };
        set({
          documents: { ...state.documents, [snapshot.id]: doc },
          documentOrder: existing ? state.documentOrder : [...state.documentOrder, snapshot.id],
          activeDocumentId:
            (options?.activate ?? true) || !state.activeDocumentId
              ? snapshot.id
              : state.activeDocumentId,
        });
      },

      updateSnapshot(id, snapshot) {
        updateDocument(id, (doc) => {
          if (doc.snapshot === snapshot) return doc;
          const next = { ...doc, snapshot };
          const selection = doc.selection.filter((sid) => findObject(next, sid) !== undefined);
          return selection.length === doc.selection.length ? next : { ...next, selection };
        });
      },

      closeDocument(id) {
        const state = get();
        if (!state.documents[id]) return;
        const documents = { ...state.documents };
        delete documents[id];
        const order = state.documentOrder.filter((d) => d !== id);
        let active = state.activeDocumentId;
        if (active === id) {
          const index = state.documentOrder.indexOf(id);
          active = order[Math.min(index, order.length - 1)] ?? null;
        }
        set({ documents, documentOrder: order, activeDocumentId: active, pointer: null });
      },

      activateDocument(id) {
        if (get().documents[id] && get().activeDocumentId !== id) set({ activeDocumentId: id });
      },

      setView(id, view) {
        updateDocument(id, (doc) =>
          doc.view.zoom === view.zoom && doc.view.x === view.x && doc.view.y === view.y
            ? doc
            : { ...doc, view },
        );
      },

      setCurrentPage(id, page) {
        updateDocument(id, (doc) =>
          doc.currentPage === page ? doc : { ...doc, currentPage: page },
        );
      },

      setSelection(id, ids) {
        updateDocument(id, (doc) =>
          sameIds(doc.selection, ids) ? doc : { ...doc, selection: [...ids] },
        );
      },

      moveObjects(id, ids, dx, dy) {
        if (ids.length === 0 || (dx === 0 && dy === 0)) return;
        editObjects(id, ids, format(t.history.move, { n: ids.length }), (object) => ({
          ...object,
          bbox: translateRect(object.bbox, dx, dy),
        }));
      },

      resizeObjects(id, rects) {
        if (rects.size === 0) return;
        // rects 为页内坐标（调用方负责从布局坐标换算）
        editObjects(
          id,
          [...rects.keys()],
          format(t.history.resize, { n: rects.size }),
          (object) => {
            const target = rects.get(object.id);
            return target ? { ...object, bbox: target } : object;
          },
        );
      },

      deleteObjects(id, ids) {
        if (ids.length === 0) return;
        editObjects(id, ids, format(t.history.delete, { n: ids.length }), () => null);
      },

      setTool(tool) {
        if (get().activeTool !== tool) set({ activeTool: tool });
      },

      setToolOptions(options) {
        set({ toolOptions: { ...get().toolOptions, ...options } });
      },

      setPointer(pointer) {
        const current = get().pointer;
        if (
          current === pointer ||
          (current &&
            pointer &&
            current.page === pointer.page &&
            current.x === pointer.x &&
            current.y === pointer.y)
        ) {
          return;
        }
        set({ pointer });
      },

      toggleRulers() {
        set({ showRulers: !get().showRulers });
      },

      toggleFrameStats() {
        const next = !get().showFrameStats;
        set({ showFrameStats: next, frameStats: next ? get().frameStats : null });
      },

      setFrameStats(summary) {
        set({ frameStats: summary });
      },

      setBenchmarkRunning(running) {
        set({ benchmarkRunning: running });
      },

      setWorkspace(id) {
        set({ workspace: id });
      },

      setComponentPanelMode(mode) {
        set({ componentPanelMode: mode });
      },

      setComponentView(view) {
        set({ componentView: view });
      },

      togglePanelsHidden() {
        set({ panelsHidden: !get().panelsHidden });
      },

      setCollapsedPanels(panels) {
        if (!sameIds(get().collapsedPanels, panels)) set({ collapsedPanels: [...panels] });
      },
    };
  });
}

function sameIds(a: readonly string[], b: readonly string[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

/** 应用级单例；测试可用 createEditorStore() 建独立实例 */
export const editorStore: EditorStore = createEditorStore();

export function useEditorStore<T>(selector: (state: EditorState) => T): T {
  return useStore(editorStore, selector);
}

export function activeDocument(state: EditorState): DocumentState | undefined {
  return state.activeDocumentId ? state.documents[state.activeDocumentId] : undefined;
}

/**
 * 按 id 找对象。对象 id 带页序号前缀（`p{n}-…`）时直接去那一页找，否则逐页找。
 */
export function findObject(
  doc: Pick<DocumentState, 'snapshot'>,
  id: string,
): { page: number; object: PageObject } | undefined {
  const prefix = /^p(\d+)-/.exec(id);
  const pages = prefix ? [doc.snapshot.pages[Number(prefix[1])]] : doc.snapshot.pages;
  for (const page of pages) {
    if (!page) continue;
    const object = page.objects.find((o) => o.id === id);
    if (object) return { page: page.index, object };
  }
  return undefined;
}
