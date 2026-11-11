import { metrics, palette } from '@magpdf/ui';
import type { DocumentEditor, PageTransform } from '../document/documentEditor';
import { linearHitTester } from '../document/hitTest';
import { PageLayout } from '../document/pageLayout';
import {
  documentTransformToPage,
  rectToRectMatrix,
  translationMatrix,
} from '../document/pageSpace';
import type { Matrix } from '../document/pageSpace';
import type { DocumentSnapshot, PageSnapshot } from '../document/types';
import { MM_PER_INCH, PT_PER_INCH, intersectRect, translateRect, unionRects } from '../geometry';
import type { Point, Rect, Size } from '../geometry';
import { format, t } from '../i18n';
import { findObject } from '../store/editorStore';
import type { EditorStore } from '../store/editorStore';
import type {
  InteractionOverlay,
  ObjectRef,
  ToolContext,
  ToolEffect,
  ToolPointerEvent,
} from '../tools/machine';
import { mapRectBetween } from '../tools/handles';
import { hasFrozenObject } from '../tools/machine';
import type { CommandId } from '../tools/shortcuts';
import { isTextInputTarget } from '../tools/shortcuts';
import { ToolManager } from '../tools/toolManager';
import type { ToolId } from '../tools/tools';
import {
  clampView,
  cssPxPerPt,
  docRectToScreen,
  docToScreen,
  fitPageZoom,
  centerOnPage,
  nextZoomStep,
  panBy,
  screenToDoc,
  scrollToPageTop,
  viewEquals,
  visibleDocRect,
  zoomAt,
} from '../view/viewState';
import type { ViewState } from '../view/viewState';
import { DragLayers, layerTileSpecs } from './dragLayers';
import { FrameStats, formatMs } from './frameStats';
import { GlTileRenderer, parseCssColor } from './glRenderer';
import { MockTileProvider } from './mockTileProvider';
import { drawOverlay } from './overlay';
import type { OverlayScene } from './overlay';
import { computeRenderPlan } from './renderPlan';
import type { RenderPlan, TileRequestSpec } from './renderPlan';
import { TileCache } from './tileCache';
import type { TileEntry } from './tileCache';
import type { TileProvider } from './tileProvider';
import { releasePixels } from './tileProvider';
import { TileScheduler } from './tileScheduler';
import { levelPxPerPt } from './tiles';

/**
 * 画布控制器：把 store、瓦片调度、WebGL 合成、叠加层和工具状态机接在一起。
 * 一个文档一个实例；在 React 之外运行，React 组件只负责挂载/卸载。
 */
export interface CanvasControllerOptions {
  readonly store: EditorStore;
  readonly documentId: string;
  /** 已定位（position: relative/absolute）的容器，控制器往里面放两个 canvas */
  readonly container: HTMLElement;
  readonly provider?: TileProvider;
  /**
   * 这份文档的修改入口。有的话，工具产生的修改（移动、缩放、删除、撤销、重做）交给它；
   * 没有的话（模拟文档）直接改 store 里的快照。
   */
  readonly editor?: DocumentEditor;
  /** 瓦片缓存上限（字节），默认 96 MB */
  readonly cacheBytes?: number;
}

const DEFAULT_CACHE_BYTES = 96 * 1024 * 1024;
const WHEEL_ZOOM_SENSITIVITY = 1 / 400;
const STATS_PUSH_INTERVAL = 500;
const FRAME_GAP_RESET = 100;
const SCROLLBAR_THICKNESS = 6;

// ---- 拖动预览（方案 3.2）----
/** 对象层四周多渲染的一圈（设备像素）：描边和抗锯齿会略微超出包围盒 */
const LAYER_MARGIN_PX = 4;
/** 对象层最多用几块瓦片。对象比视口大很多时只取视口周围的部分；还是超过就不做分层预览 */
const MAX_OBJECT_TILES = 128;
/** 一次最多拖几个对象还做分层预览：对象的编号要随每一批瓦片请求发给引擎 */
const MAX_LAYER_OBJECTS = 500;
/** 松手之后预览最多留多久（ms）。正常情况下这一页的新瓦片几十毫秒就到齐 */
const HOLD_LIMIT_MS = 1500;
/** 松手之后对象应该在的位置和快照里的差多少算对不上（pt） */
const HOLD_TOLERANCE_PT = 0.5;

/** 一次拖动预览的情况，测量和端到端测试用。 */
export interface DragPreviewInfo {
  /** 两层的瓦片到齐了，画布正在用分层的画法 */
  readonly ready: boolean;
  /** 从开始拖到两层到齐用了多久（ms）；还没到齐是 null */
  readonly readyMs: number | null;
  readonly baseTiles: number;
  readonly objectTiles: number;
  /** 已经松手，预览还留着等新瓦片 */
  readonly held: boolean;
}

/** 文档 id → 控制器，供快捷键分发和基准脚本查找 */
export const canvasRegistry = new Map<string, CanvasController>();

export class CanvasController {
  readonly documentId: string;
  readonly stats = new FrameStats();
  readonly cache: TileCache;
  readonly scheduler: TileScheduler;
  readonly tools = new ToolManager();

  private readonly store: EditorStore;
  private readonly container: HTMLElement;
  private readonly glCanvas: HTMLCanvasElement;
  private readonly overlayCanvas: HTMLCanvasElement;
  private readonly overlayCtx: CanvasRenderingContext2D | null;
  /** “WebGL2 不可用”的提示层；WebGL2 可用时没有这一层 */
  private readonly message: HTMLDivElement | null;
  private readonly renderer: GlTileRenderer | null;
  private readonly provider: TileProvider;
  private readonly ownsProvider: boolean;
  private readonly editor: DocumentEditor | null;
  private readonly resizeObserver: ResizeObserver | null;
  private readonly unsubscribe: () => void;

  private layout: PageLayout;
  private snapshot: DocumentSnapshot | undefined;
  private containerSize: Size = { width: 0, height: 0 };
  private dpr = 1;
  private frameHandle: number | null = null;
  private overlay: InteractionOverlay | null = null;
  private hoverScreen: Point | null = null;
  private lastFrameAt = 0;
  private lastStatsPush = 0;
  private lastPointerPush = 0;
  private cursor = '';
  /** 上一次从 store 同步过来的选区，用来判断是否要丢掉工具内部的选区覆盖值 */
  private knownSelection: readonly string[] | null = null;
  private pendingVisibleTiles = 0;
  /** 拖动预览的两层瓦片。没有在拖，或者这一次拖动用不了分层预览时是 null */
  private dragLayers: DragLayers | null = null;
  /** 对象层渲染时被拖对象的外框（文档坐标），和现在该把它画到哪里 */
  private dragFrom: Rect | null = null;
  private dragTo: Rect | null = null;
  /** 已经松手：预览留着，等这一页的新瓦片到齐再撤 */
  private dragHeld: {
    /** 松手那一刻这一页的修订号 */
    readonly revision: number | undefined;
    readonly deadline: number;
    /** 修改生效的话，各个对象应该在的位置（文档坐标） */
    readonly expected: ReadonlyMap<string, Rect>;
  } | null = null;
  /** 这一次拖动试过分层预览而用不了（对象跨页、瓦片渲染失败），拖动结束前不再试 */
  private dragGaveUp: readonly string[] | null = null;
  /** 最近结束的一次拖动预览 */
  private lastDrag: DragPreviewInfo | null = null;
  private disposed = false;

  constructor(options: CanvasControllerOptions) {
    this.store = options.store;
    this.documentId = options.documentId;
    this.container = options.container;
    this.editor = options.editor ?? null;
    this.snapshot = this.store.getState().documents[this.documentId]?.snapshot;
    this.layout = buildLayout(this.snapshot);

    this.glCanvas = document.createElement('canvas');
    this.glCanvas.className = 'mg-canvas-gl';
    this.overlayCanvas = document.createElement('canvas');
    this.overlayCanvas.className = 'mg-canvas-overlay';
    this.overlayCanvas.setAttribute('role', 'img');
    this.overlayCanvas.setAttribute('aria-label', t.canvas.label);
    this.container.append(this.glCanvas, this.overlayCanvas);

    this.renderer = GlTileRenderer.create(this.glCanvas);
    // 提示层盖在整个画布上面，所以只在 WebGL2 真的不可用时才放进去：
    // 平时留着它（哪怕标了 hidden）会挡住叠加层，鼠标事件到不了画布。
    if (this.renderer) {
      this.message = null;
    } else {
      this.message = document.createElement('div');
      this.message.className = 'mg-canvas-message';
      this.message.textContent = t.canvas.webglUnavailable;
      this.container.append(this.message);
    }
    this.overlayCtx = this.overlayCanvas.getContext('2d');

    this.cache = new TileCache(options.cacheBytes ?? DEFAULT_CACHE_BYTES, (entry) =>
      this.onEvict(entry),
    );
    if (options.provider) {
      this.provider = options.provider;
      this.ownsProvider = false;
    } else {
      this.provider = new MockTileProvider({
        getDocument: () => this.store.getState().documents[this.documentId]?.snapshot,
      });
      this.ownsProvider = true;
    }
    this.scheduler = new TileScheduler(this.provider, this.cache, (entry) =>
      this.onTileReady(entry),
    );

    this.unsubscribe = this.store.subscribe((state, previous) =>
      this.onStoreChange(state, previous),
    );

    if (typeof ResizeObserver !== 'undefined') {
      this.resizeObserver = new ResizeObserver(() => this.measure());
      this.resizeObserver.observe(this.container);
    } else {
      this.resizeObserver = null;
    }
    this.measure();
    this.attachEvents();
    canvasRegistry.set(this.documentId, this);
    this.invalidate();
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    canvasRegistry.delete(this.documentId);
    if (this.frameHandle !== null) cancelAnimationFrame(this.frameHandle);
    this.detachEvents();
    this.resizeObserver?.disconnect();
    this.unsubscribe();
    this.dragLayers?.dispose();
    this.dragLayers = null;
    this.scheduler.dispose();
    this.cache.clear();
    if (this.ownsProvider) this.provider.dispose?.();
    this.renderer?.dispose();
    this.glCanvas.remove();
    this.overlayCanvas.remove();
    this.message?.remove();
  }

  // ---------- 对外 API（菜单、选项栏、基准脚本） ----------

  get view(): ViewState {
    return this.document?.view ?? { zoom: 1, x: 0, y: 0 };
  }

  /** 画布（不含标尺）的 CSS 像素尺寸 */
  get viewport(): Size {
    const offset = this.rulerOffset;
    return {
      width: Math.max(0, this.containerSize.width - offset),
      height: Math.max(0, this.containerSize.height - offset),
    };
  }

  /**
   * 画面是不是最新的：没有等着画的帧，上一帧里视口内的瓦片也都是最新内容（没有缺的，也没有还在显示
   * 旧图、等着换的）。视口外的预取不算。端到端测试靠它量“修改之后画面什么时候重画完”。
   */
  get upToDate(): boolean {
    return this.frameHandle === null && this.pendingVisibleTiles === 0;
  }

  /** 正在进行的拖动预览（方案 3.2）；没有时是 null。 */
  get dragPreview(): DragPreviewInfo | null {
    return this.dragLayers ? this.describeDrag(this.dragLayers) : null;
  }

  /** 最近结束的一次拖动预览；还没有过是 null。 */
  get lastDragPreview(): DragPreviewInfo | null {
    return this.lastDrag;
  }

  get hasWebGL(): boolean {
    return this.renderer !== null;
  }

  get pageLayout(): PageLayout {
    return this.layout;
  }

  get tileProvider(): TileProvider {
    return this.provider;
  }

  setView(view: ViewState): void {
    const next = clampView(view, this.viewport, this.layout);
    if (viewEquals(next, this.view)) return;
    this.store.getState().setView(this.documentId, next);
  }

  panBy(dxPx: number, dyPx: number): void {
    this.setView(panBy(this.view, dxPx, dyPx));
  }

  zoomAtPoint(screen: Point, zoom: number): void {
    this.setView(zoomAt(this.view, screen, zoom));
  }

  zoomStep(direction: 1 | -1): void {
    const vp = this.viewport;
    this.zoomAtPoint(
      { x: vp.width / 2, y: vp.height / 2 },
      nextZoomStep(this.view.zoom, direction),
    );
  }

  fitPage(): void {
    const page = this.currentPage;
    if (page === null) return;
    const zoom = fitPageZoom(this.layout.pageSize(page), this.viewport);
    this.setView(centerOnPage(this.layout, page, zoom, this.viewport));
  }

  actualSize(): void {
    const page = this.currentPage;
    if (page === null) return;
    this.setView(centerOnPage(this.layout, page, 1, this.viewport));
  }

  scrollToPage(index: number): void {
    if (index < 0 || index >= this.layout.pageCount) return;
    this.setView(scrollToPageTop(this.layout, index, this.view, this.viewport));
  }

  /** 执行快捷键命令；返回是否处理 */
  runCommand(command: CommandId): boolean {
    switch (command) {
      case 'view.zoomIn':
        this.zoomStep(1);
        return true;
      case 'view.zoomOut':
        this.zoomStep(-1);
        return true;
      case 'view.fitPage':
        this.fitPage();
        return true;
      case 'view.actualSize':
        this.actualSize();
        return true;
      case 'edit.undo':
      case 'edit.redo':
        // 撤销记录在引擎里（方案 4.2）。模拟文档没有撤销，按键照旧算处理过了，免得落到浏览器的默认行为上。
        if (this.editor) {
          if (command === 'edit.undo') this.editor.undo();
          else this.editor.redo();
        }
        return true;
      default: {
        if (command.startsWith('edit.')) {
          this.applyEffects(this.tools.command(command, this.toolContext()));
          this.invalidate();
          return true;
        }
        return false;
      }
    }
  }

  /** 键盘：Space 临时抓手。返回是否处理 */
  handleKeyDown(event: KeyboardEvent): boolean {
    if (event.key === ' ' && !isTextInputTarget(event.target)) {
      if (!event.repeat) {
        this.tools.beginSpace();
        this.updateCursor();
      }
      return true;
    }
    return false;
  }

  handleKeyUp(event: KeyboardEvent): boolean {
    if (event.key === ' ') {
      this.tools.endSpace();
      this.updateCursor();
      return true;
    }
    return false;
  }

  /** 工具切换由 store 驱动；这里同步状态机并取消进行中的交互 */
  syncTool(tool: ToolId): void {
    this.applyEffects(this.tools.setActiveTool(tool, this.toolContext()));
    this.updateCursor();
    this.invalidate();
  }

  invalidate(): void {
    if (this.disposed || this.frameHandle !== null) return;
    this.frameHandle = requestAnimationFrame(this.frame);
  }

  /** 把当前帧时间统计写进 store（状态栏显示） */
  pushStats(): void {
    this.store.getState().setFrameStats(this.stats.summary());
  }

  // ---------- 内部 ----------

  private get document() {
    return this.store.getState().documents[this.documentId];
  }

  private get rulerOffset(): number {
    return this.store.getState().showRulers ? metrics.rulerSize : 0;
  }

  private get currentPage(): number | null {
    if (this.layout.pageCount === 0) return null;
    return this.document?.currentPage ?? 0;
  }

  private measure(): void {
    const rect = this.container.getBoundingClientRect();
    const size = { width: Math.round(rect.width), height: Math.round(rect.height) };
    const dpr = window.devicePixelRatio || 1;
    if (
      size.width === this.containerSize.width &&
      size.height === this.containerSize.height &&
      dpr === this.dpr
    ) {
      return;
    }
    const first = this.containerSize.width === 0 && this.containerSize.height === 0;
    this.containerSize = size;
    this.dpr = dpr;
    this.overlayCanvas.width = Math.round(size.width * dpr);
    this.overlayCanvas.height = Math.round(size.height * dpr);
    this.overlayCanvas.style.width = `${size.width}px`;
    this.overlayCanvas.style.height = `${size.height}px`;
    this.layoutGlCanvas();
    if (first && size.width > 0 && size.height > 0) {
      // 首次有尺寸：适合页面
      const page = this.currentPage;
      if (page !== null && this.document) {
        const zoom = fitPageZoom(this.layout.pageSize(page), this.viewport);
        const view = clampView(
          centerOnPage(this.layout, page, zoom, this.viewport),
          this.viewport,
          this.layout,
        );
        this.store.getState().setView(this.documentId, view);
      }
    } else {
      this.setView(this.view);
    }
    this.invalidate();
  }

  private layoutGlCanvas(): void {
    const offset = this.rulerOffset;
    const vp = this.viewport;
    this.glCanvas.style.left = `${offset}px`;
    this.glCanvas.style.top = `${offset}px`;
    this.glCanvas.style.width = `${vp.width}px`;
    this.glCanvas.style.height = `${vp.height}px`;
    this.renderer?.resize(
      Math.max(1, Math.round(vp.width * this.dpr)),
      Math.max(1, Math.round(vp.height * this.dpr)),
    );
  }

  private onStoreChange(
    state: ReturnType<EditorStore['getState']>,
    previous: ReturnType<EditorStore['getState']>,
  ): void {
    const doc = state.documents[this.documentId];
    const prevDoc = previous.documents[this.documentId];
    if (!doc) return;
    if (doc.snapshot !== this.snapshot) {
      this.onSnapshotChange(doc.snapshot);
    }
    if (state.showRulers !== previous.showRulers) {
      this.layoutGlCanvas();
      this.setView(this.view);
    }
    if (state.activeTool !== previous.activeTool) {
      this.syncTool(state.activeTool);
    }
    if (state.showFrameStats !== previous.showFrameStats) {
      this.stats.reset();
      this.lastStatsPush = 0;
    }
    if (doc.selection !== this.knownSelection) {
      // 选区被工具之外的地方改了（图层面板、删除对象）：工具内部的临时选区作废
      this.knownSelection = doc.selection;
      this.tools.forgetSelectionOverride();
      this.updateCursor();
    }
    if (doc !== prevDoc || state.showRulers !== previous.showRulers) {
      this.invalidate();
    }
  }

  private onSnapshotChange(snapshot: DocumentSnapshot): void {
    const previous = this.snapshot;
    this.snapshot = snapshot;
    if (
      !previous ||
      previous.pages.length !== snapshot.pages.length ||
      pagesResized(previous, snapshot)
    ) {
      // 页面的排布变了：旧瓦片的位置和大小都对不上了，在途的请求也一样
      this.layout = buildLayout(snapshot);
      this.scheduler.reset();
      this.cache.clear();
    } else {
      for (let i = 0; i < snapshot.pages.length; i++) {
        if (pageRepainted(previous.pages[i]!, snapshot.pages[i]!)) this.scheduler.invalidatePage(i);
      }
    }
    this.invalidate();
  }

  private onTileReady(entry: TileEntry): void {
    if (this.renderer && entry.pixels) {
      entry.texture = this.renderer.uploadTexture(entry.pixels, entry.width, entry.height);
      releasePixels(entry.pixels);
      entry.pixels = null;
    }
    this.invalidate();
  }

  private onEvict(entry: TileEntry): void {
    if (entry.texture) {
      this.renderer?.deleteTexture(entry.texture);
      entry.texture = null;
    }
  }

  // ---------- 渲染 ----------

  private readonly frame = (now: number): void => {
    this.frameHandle = null;
    if (this.disposed) return;
    const doc = this.document;
    if (!doc) return;
    const statsOn = this.store.getState().showFrameStats || this.store.getState().benchmarkRunning;
    if (statsOn) {
      if (now - this.lastFrameAt > FRAME_GAP_RESET) this.stats.breakSequence();
      this.stats.beginFrame(now);
    }
    this.lastFrameAt = now;

    const view = doc.view;
    const vp = this.viewport;
    const dpr = this.dpr;
    const plan = computeRenderPlan({
      layout: this.layout,
      view,
      viewport: vp,
      devicePixelRatio: dpr,
      has: (id) => {
        const entry = this.cache.peek(id);
        return entry !== undefined && (entry.texture !== null || !this.renderer);
      },
      isFresh: (id) => this.cache.isFresh(id),
    });
    this.syncDragLayers(plan, now);

    if (this.renderer && !this.renderer.isLost) {
      const r = this.renderer;
      r.beginFrame(parseCssColor(palette.canvasBackdrop));
      const scale = cssPxPerPt(view.zoom) * dpr;
      for (const page of plan.pages) {
        r.drawQuad(deviceRect(page.docRect, view, scale), null, [1, 1, 1, 1]);
      }
      for (const quad of [...plan.fallback, ...plan.tiles]) {
        const entry = this.cache.get(quad.id);
        if (!entry?.texture) continue;
        const pageRect = this.layout.pageRect(quad.key.page);
        r.drawQuad(
          tileDeviceRect(quad.docRect, pageRect, view, scale),
          entry.texture as WebGLTexture,
        );
      }
      this.drawDragLayers(r, view, scale);
      r.endFrame();
    }

    this.scheduler.setWanted([...plan.missing, ...plan.prefetch], doc.snapshot.version);
    this.pendingVisibleTiles = plan.missing.length;
    // 松手之后预览还留着：每一帧都要看新瓦片到齐了没有
    if (this.dragHeld) this.invalidate();

    this.drawOverlayLayer(
      doc.selection,
      plan.pages.map((p) => p.docRect),
    );
    this.pushPointer(now);
    this.updateCurrentPage();

    if (statsOn) {
      this.stats.endFrame(performance.now());
      if (now - this.lastStatsPush > STATS_PUSH_INTERVAL && this.store.getState().showFrameStats) {
        this.lastStatsPush = now;
        this.pushStats();
      }
    }
  };

  private drawOverlayLayer(selection: readonly string[], pageRects: readonly Rect[]): void {
    const ctx = this.overlayCtx;
    if (!ctx) return;
    const view = this.view;
    const state = this.store.getState();
    const dragged = this.overlay?.preview
      ? Array.from(this.overlay.preview.values(), (r) => docRectToScreen(view, r))
      : [];
    // 分层预览在用的时候，对象本身已经跟着鼠标走了：选框也跟过去（不带控制点），不再画虚线框
    const layered = dragged.length > 0 && this.dragLayers?.ready === true && !this.dragHeld;
    const selectedRects: Rect[] = [];
    if (layered) {
      selectedRects.push(...dragged);
    } else {
      for (const id of selection) {
        const ref = this.objectById(id);
        if (ref) selectedRects.push(docRectToScreen(view, ref.docRect));
      }
    }
    const preview = layered ? [] : dragged;
    const selectionBox = selectedRects.length ? unionRects(selectedRects) : null;
    const hoverRect = this.hoverRect(view, selection);
    const current = this.currentPage ?? 0;
    const origin = this.layout.pageCount
      ? docToScreen(view, this.layout.pageRect(current))
      : { x: 0, y: 0 };
    const vp = this.viewport;
    const summary = state.showFrameStats ? state.frameStats : null;
    const scene: OverlayScene = {
      viewport: this.containerSize,
      devicePixelRatio: this.dpr,
      rulers: state.showRulers,
      rulerOrigin: origin,
      pxPerMm: (cssPxPerPt(view.zoom) * PT_PER_INCH) / MM_PER_INCH,
      pointer: this.hoverScreen,
      pages: pageRects.map((r) => docRectToScreen(view, r)),
      hover: hoverRect,
      selected: selectedRects,
      selectionBox,
      handles:
        !layered &&
        !hasFrozenObject(
          {
            pages: this.document?.snapshot.pages ?? [],
            objectById: (id) => this.objectById(id),
          },
          selection,
        ),
      marquee: this.overlay?.marquee ?? null,
      zoomRect: this.overlay?.zoomRect ?? null,
      preview,
      rotation: this.overlay?.rotation ?? null,
      scrollbars: this.scrollbars(view, vp),
      statsText: summary
        ? format(t.status.frameStats, {
            p50: formatMs(summary.intervalP50),
            p95: formatMs(summary.intervalP95),
            work95: formatMs(summary.workP95),
            n: summary.samples,
          })
        : null,
    };
    drawOverlay(ctx, scene);
  }

  private hoverRect(view: ViewState, selection: readonly string[]): Rect | null {
    if (!this.hoverScreen || this.overlay || this.tools.effective !== 'select') return null;
    const id = this.hitAt(this.hoverScreen);
    if (!id || selection.includes(id)) return null;
    const ref = this.objectById(id);
    return ref ? docRectToScreen(view, ref.docRect) : null;
  }

  private scrollbars(view: ViewState, vp: Size): OverlayScene['scrollbars'] {
    const s = cssPxPerPt(view.zoom);
    const docW = this.layout.totalWidth * s;
    const docH = this.layout.totalHeight * s;
    let vertical: Rect | null = null;
    let horizontal: Rect | null = null;
    if (docH > vp.height + 1) {
      const track = vp.height - 4;
      const length = Math.max(24, (vp.height / docH) * track);
      const pos = 2 + ((view.y * s) / (docH - vp.height)) * (track - length);
      vertical = {
        x: vp.width - SCROLLBAR_THICKNESS - 2,
        y: pos,
        width: SCROLLBAR_THICKNESS,
        height: length,
      };
    }
    if (docW > vp.width + 1) {
      const track = vp.width - 4;
      const length = Math.max(24, (vp.width / docW) * track);
      const pos = 2 + ((view.x * s) / (docW - vp.width)) * (track - length);
      horizontal = {
        x: pos,
        y: vp.height - SCROLLBAR_THICKNESS - 2,
        width: length,
        height: SCROLLBAR_THICKNESS,
      };
    }
    return { vertical, horizontal };
  }

  private pushPointer(now: number): void {
    if (now - this.lastPointerPush < 33) return;
    this.lastPointerPush = now;
    const state = this.store.getState();
    if (state.activeDocumentId !== this.documentId) return;
    if (!this.hoverScreen) {
      state.setPointer(null);
      return;
    }
    const doc = screenToDoc(this.view, this.hoverScreen);
    const hit = this.layout.pageAtPoint(doc);
    if (hit) {
      state.setPointer({ page: hit.index, x: hit.local.x, y: hit.local.y });
    } else {
      // 页外：相对当前页左上角
      const page = this.currentPage ?? 0;
      const rect = this.layout.pageRect(page);
      state.setPointer({ page, x: doc.x - rect.x, y: doc.y - rect.y });
    }
  }

  private updateCurrentPage(): void {
    if (this.layout.pageCount === 0) return;
    const view = this.view;
    const yMax = view.y + this.viewport.height / cssPxPerPt(view.zoom);
    const page = this.layout.dominantPage(view.y, yMax);
    const doc = this.document;
    if (doc && doc.currentPage !== page)
      this.store.getState().setCurrentPage(this.documentId, page);
  }

  // ---------- 事件 ----------

  private attachEvents(): void {
    const el = this.overlayCanvas;
    el.addEventListener('pointerdown', this.onPointerDown);
    el.addEventListener('pointermove', this.onPointerMove);
    el.addEventListener('pointerup', this.onPointerUp);
    el.addEventListener('pointercancel', this.onPointerUp);
    el.addEventListener('pointerleave', this.onPointerLeave);
    el.addEventListener('wheel', this.onWheel, { passive: false });
    el.addEventListener('contextmenu', this.onContextMenu);
    el.addEventListener('dblclick', this.onDoubleClick);
  }

  private detachEvents(): void {
    const el = this.overlayCanvas;
    el.removeEventListener('pointerdown', this.onPointerDown);
    el.removeEventListener('pointermove', this.onPointerMove);
    el.removeEventListener('pointerup', this.onPointerUp);
    el.removeEventListener('pointercancel', this.onPointerUp);
    el.removeEventListener('pointerleave', this.onPointerLeave);
    el.removeEventListener('wheel', this.onWheel);
    el.removeEventListener('contextmenu', this.onContextMenu);
    el.removeEventListener('dblclick', this.onDoubleClick);
  }

  private screenPoint(event: MouseEvent): Point {
    const rect = this.overlayCanvas.getBoundingClientRect();
    const offset = this.rulerOffset;
    return { x: event.clientX - rect.left - offset, y: event.clientY - rect.top - offset };
  }

  private toolEvent(type: ToolPointerEvent['type'], event: PointerEvent): ToolPointerEvent {
    const screen = this.screenPoint(event);
    return {
      type,
      screen,
      doc: screenToDoc(this.view, screen),
      button: event.button,
      shift: event.shiftKey,
      alt: event.altKey,
      ctrl: event.ctrlKey || event.metaKey,
    };
  }

  private readonly onPointerDown = (event: PointerEvent): void => {
    if (!this.document) return;
    this.store.getState().activateDocument(this.documentId);
    const screen = this.screenPoint(event);
    if (screen.x < 0 || screen.y < 0) return; // 标尺区域
    this.overlayCanvas.setPointerCapture(event.pointerId);
    this.hoverScreen = screen;
    this.applyEffects(
      this.tools.dispatch(this.toolEvent('pointerdown', event), this.toolContext()),
    );
    this.updateCursor();
    this.invalidate();
    event.preventDefault();
  };

  private readonly onPointerMove = (event: PointerEvent): void => {
    if (!this.document) return;
    this.hoverScreen = this.screenPoint(event);
    this.applyEffects(
      this.tools.dispatch(this.toolEvent('pointermove', event), this.toolContext()),
    );
    this.updateCursor();
    this.invalidate();
  };

  private readonly onPointerUp = (event: PointerEvent): void => {
    if (!this.document) return;
    if (this.overlayCanvas.hasPointerCapture(event.pointerId)) {
      this.overlayCanvas.releasePointerCapture(event.pointerId);
    }
    const type = event.type === 'pointercancel' ? 'pointercancel' : 'pointerup';
    this.applyEffects(this.tools.dispatch(this.toolEvent(type, event), this.toolContext()));
    this.stats.breakSequence();
    this.updateCursor();
    this.invalidate();
  };

  private readonly onPointerLeave = (): void => {
    this.hoverScreen = null;
    this.invalidate();
  };

  private readonly onWheel = (event: WheelEvent): void => {
    if (!this.document) return;
    event.preventDefault();
    const screen = this.screenPoint(event);
    const delta = event.deltaMode === WheelEvent.DOM_DELTA_LINE ? event.deltaY * 16 : event.deltaY;
    if (event.ctrlKey || event.metaKey || event.altKey) {
      const factor = Math.exp(-delta * WHEEL_ZOOM_SENSITIVITY);
      this.zoomAtPoint(screen, this.view.zoom * factor);
    } else if (event.shiftKey) {
      this.panBy(-delta, 0);
    } else {
      const dx = event.deltaMode === WheelEvent.DOM_DELTA_LINE ? event.deltaX * 16 : event.deltaX;
      this.panBy(-dx, -delta);
    }
    this.hoverScreen = screen;
    this.invalidate();
  };

  private readonly onContextMenu = (event: MouseEvent): void => {
    event.preventDefault();
  };

  private readonly onDoubleClick = (): void => {
    // 抓手工具双击：适合页面（Photoshop 习惯）
    if (this.tools.effective === 'hand') this.fitPage();
  };

  // ---------- 拖动预览（方案 3.2） ----------
  //
  // 拖动（或拖控制点缩放）一开始，向瓦片来源要两层：去掉被拖对象的底图，和只有被拖对象的透明图层。
  // 两层到齐之前照旧画虚线框；到齐之后每一帧只是把对象层挪到（或缩放到）新的位置叠在底图上，
  // 不再向引擎要任何东西。松手后预览留着，等这一页重画好的瓦片到齐再撤，中间不会闪回原来的位置。

  /** 每一帧开头：按工具的拖动状态起、停两层瓦片。 */
  private syncDragLayers(plan: RenderPlan, now: number): void {
    const preview = this.overlay?.preview;
    const ids = preview ? [...preview.keys()] : [];
    let layers = this.dragLayers;

    // 松手之后留着的预览：新瓦片到齐了（或者等不到了）就撤；这时又拖起别的来也撤
    if (layers && this.dragHeld) {
      if (!preview && !this.dragSettled(layers, plan, now)) return;
      this.endDragLayers();
      layers = null;
    }
    // 拖动中：取消了、换了对象、缩放档变了、有瓦片没渲染出来
    if (
      layers &&
      (!preview ||
        !sameIds(layers.objects, ids) ||
        layers.level !== plan.targetLevel ||
        layers.failed ||
        this.renderer?.isLost)
    ) {
      // 渲染不出来的话这一次拖动不再试；缩放档变了则按新的档位重新来
      if (preview && layers.failed) this.dragGaveUp = ids;
      this.endDragLayers();
      layers = null;
    }
    if (!preview) {
      this.dragGaveUp = null;
      return;
    }
    if (!layers) {
      if (this.dragGaveUp && sameIds(this.dragGaveUp, ids)) return;
      layers = this.startDragLayers(preview, plan);
      if (!layers) {
        this.dragGaveUp = ids;
        return;
      }
    }
    this.dragTo = unionRects([...preview.values()]);
    // 拖动途中视口挪了（滚轮）：这一页新露出来的部分补要底图
    layers.requestBase(this.visibleLayerSpecs(layers.page, layers.level));
  }

  /** 起两层瓦片。这一次拖动做不了分层预览时返回 null，画布照旧画虚线框。 */
  private startDragLayers(preview: ReadonlyMap<string, Rect>, plan: RenderPlan): DragLayers | null {
    const doc = this.document;
    const renderer = this.renderer;
    // 只对交给修改入口的文档（引擎打开的）做；瓦片来源要能按层渲染
    if (!doc || !this.editor || !this.provider.layers || !renderer || renderer.isLost) return null;
    const ids = [...preview.keys()];
    if (ids.length === 0 || ids.length > MAX_LAYER_OBJECTS) return null;
    const refs: ObjectRef[] = [];
    for (const id of ids) {
      const ref = this.objectById(id);
      if (!ref) return null;
      refs.push(ref);
    }
    // 两层是按页渲染的：选区跨页时不做
    const page = refs[0]!.page;
    if (refs.some((ref) => ref.page !== page)) return null;
    const from = unionRects(refs.map((ref) => ref.docRect));
    if (!from) return null;
    const level = plan.targetLevel;
    const baseSpecs = this.visibleLayerSpecs(page, level);
    // 对象层盖住对象现在所在的地方，四周多留一圈。对象比视口大很多时只取视口周围各一个视口的范围：
    // 拖动中够得着的基本就是这些
    const margin = LAYER_MARGIN_PX / levelPxPerPt(level);
    const visible = visibleDocRect(this.view, this.viewport);
    const reach: Rect = {
      x: visible.x - visible.width,
      y: visible.y - visible.height,
      width: visible.width * 3,
      height: visible.height * 3,
    };
    const region = intersectRect(
      {
        x: from.x - margin,
        y: from.y - margin,
        width: from.width + 2 * margin,
        height: from.height + 2 * margin,
      },
      reach,
    );
    if (!region) return null;
    const pageRect = this.layout.pageRect(page);
    const objectSpecs = layerTileSpecs(
      this.layout.pageSize(page),
      page,
      level,
      translateRect(region, -pageRect.x, -pageRect.y),
    );
    if (baseSpecs.length === 0 || objectSpecs.length === 0) return null;
    if (objectSpecs.length > MAX_OBJECT_TILES) return null;

    const layers = new DragLayers({
      provider: this.provider,
      page,
      level,
      objects: ids,
      documentVersion: doc.snapshot.version,
      upload: (pixels, width, height) => renderer.uploadTexture(pixels, width, height),
      release: (texture) => renderer.deleteTexture(texture),
      onChange: () => this.invalidate(),
    });
    layers.requestBase(baseSpecs);
    layers.requestObjects(objectSpecs);
    this.dragLayers = layers;
    this.dragFrom = from;
    this.dragTo = from;
    this.dragHeld = null;
    return layers;
  }

  /** 一页在视口里的那部分，在 `level` 档位下的瓦片。 */
  private visibleLayerSpecs(page: number, level: number): TileRequestSpec[] {
    const pageRect = this.layout.pageRect(page);
    const visible = intersectRect(visibleDocRect(this.view, this.viewport), pageRect);
    if (!visible) return [];
    return layerTileSpecs(
      this.layout.pageSize(page),
      page,
      level,
      translateRect(visible, -pageRect.x, -pageRect.y),
    );
  }

  /** 两层到齐之后：底图盖在这一页平常的瓦片上面，对象层画在被拖到的位置。 */
  private drawDragLayers(r: GlTileRenderer, view: ViewState, scale: number): void {
    const layers = this.dragLayers;
    const from = this.dragFrom;
    const to = this.dragTo;
    if (!layers?.ready || !from || !to) return;
    const pageRect = this.layout.pageRect(layers.page);
    const toDoc = (tile: Rect) => translateRect(tile, pageRect.x, pageRect.y);
    for (const tile of layers.base()) {
      r.drawQuad(
        tileDeviceRect(toDoc(tile.pageRect), pageRect, view, scale),
        tile.texture as WebGLTexture,
      );
    }
    const resized =
      Math.abs(to.width - from.width) > 1e-6 || Math.abs(to.height - from.height) > 1e-6;
    // 只是挪动：偏移取整到设备像素，对象层和渲染出来时一样清楚
    const dx = Math.round((to.x - from.x) * scale);
    const dy = Math.round((to.y - from.y) * scale);
    for (const tile of layers.objectLayer()) {
      const docRect = toDoc(tile.pageRect);
      if (resized) {
        // 缩放：把这块瓦片按“原来的外框 → 现在的外框”拉伸。相邻瓦片共用一条边，不各自取整，免得出缝
        const mapped = mapRectBetween(docRect, from, to);
        r.drawQuad(
          {
            x: (mapped.x - view.x) * scale,
            y: (mapped.y - view.y) * scale,
            width: mapped.width * scale,
            height: mapped.height * scale,
          },
          tile.texture as WebGLTexture,
        );
      } else {
        const rect = tileDeviceRect(docRect, pageRect, view, scale);
        r.drawQuad({ ...rect, x: rect.x + dx, y: rect.y + dy }, tile.texture as WebGLTexture);
      }
    }
  }

  /**
   * 松手了，修改正要交给修改入口。预览已经在用的话留着它，`to` 是被拖对象最后的外框；
   * 还没准备好就不留了（之后到的瓦片可能是修改之后渲染的，位置对不上）。
   */
  private holdDragLayers(ids: readonly string[], to: Rect): void {
    const layers = this.dragLayers;
    const from = this.dragFrom;
    if (!layers) return;
    // 上一次松手留下的预览还在，这又来了一次修改（比如紧接着按方向键）：那份预览已经对不上了
    if (this.dragHeld || !layers.ready || !from || !sameIds(layers.objects, ids)) {
      this.endDragLayers();
      return;
    }
    const expected = new Map<string, Rect>();
    for (const id of ids) {
      const ref = this.objectById(id);
      if (ref) expected.set(id, mapRectBetween(ref.docRect, from, to));
    }
    this.dragTo = to;
    this.dragHeld = {
      revision: this.document?.snapshot.pages[layers.page]?.revision,
      deadline: performance.now() + HOLD_LIMIT_MS,
      expected,
    };
  }

  /** 松手之后留着的预览可以撤了吗。 */
  private dragSettled(layers: DragLayers, plan: RenderPlan, now: number): boolean {
    const held = this.dragHeld;
    if (!held || layers.failed || now > held.deadline) return true;
    // 对象不在该在的位置上：修改被拒绝了、撤回了，或者引擎给的结果不一样
    for (const [id, expected] of held.expected) {
      const ref = this.objectById(id);
      if (!ref || !rectsClose(ref.docRect, expected, HOLD_TOLERANCE_PT)) return true;
    }
    // 这一页重画过了，而且视口里它的瓦片都换成了新的
    const page = this.document?.snapshot.pages[layers.page];
    return (
      page?.revision !== held.revision &&
      !plan.missing.some((spec) => spec.key.page === layers.page)
    );
  }

  private endDragLayers(): void {
    const layers = this.dragLayers;
    if (!layers) return;
    this.lastDrag = this.describeDrag(layers);
    layers.dispose();
    this.dragLayers = null;
    this.dragFrom = null;
    this.dragTo = null;
    this.dragHeld = null;
    this.invalidate();
  }

  private describeDrag(layers: DragLayers): DragPreviewInfo {
    const counts = layers.counts;
    return {
      ready: layers.ready,
      readyMs: layers.readyMs,
      baseTiles: counts.base,
      objectTiles: counts.objects,
      held: this.dragHeld !== null,
    };
  }

  // ---------- 工具上下文与副作用 ----------

  private toolContext(): ToolContext {
    const doc = this.document;
    const view = this.view;
    return {
      view,
      viewport: this.viewport,
      layout: this.layout,
      pages: doc?.snapshot.pages ?? [],
      hitTester: linearHitTester,
      selection: doc?.selection ?? [],
      objectById: (id) => this.objectById(id),
      docToScreen: (p) => docToScreen(view, p),
      screenToDoc: (p) => screenToDoc(view, p),
      options: this.store.getState().toolOptions,
    };
  }

  private objectById(id: string): ObjectRef | undefined {
    const doc = this.document;
    const found = doc && findObject(doc, id);
    if (!found || found.page >= this.layout.pageCount) return undefined;
    const pageRect = this.layout.pageRect(found.page);
    return {
      page: found.page,
      object: found.object,
      docRect: translateRect(found.object.bbox, pageRect.x, pageRect.y),
    };
  }

  /** 把对象按所在的页分组（页序号 → 对象 id），找不到的对象略过。 */
  private idsByPage(ids: readonly string[]): Map<number, string[]> {
    const pages = new Map<number, string[]>();
    for (const id of ids) {
      const ref = this.objectById(id);
      if (!ref) continue;
      const list = pages.get(ref.page);
      if (list) list.push(id);
      else pages.set(ref.page, [id]);
    }
    return pages;
  }

  /** 把文档坐标里的一个变换交给修改入口：按页分开，各页换成自己的页面坐标里的矩阵。 */
  private transformViaEditor(
    editor: DocumentEditor,
    ids: readonly string[],
    inDocument: Matrix,
  ): void {
    const changes: PageTransform[] = [];
    for (const [page, pageIds] of this.idsByPage(ids)) {
      changes.push({
        page,
        ids: pageIds,
        matrix: documentTransformToPage(inDocument, this.layout.pageRect(page)),
      });
    }
    if (changes.length > 0) editor.transformObjects(changes);
  }

  private hitAt(screen: Point): string | null {
    const doc = this.document;
    if (!doc) return null;
    const point = screenToDoc(this.view, screen);
    const hit = this.layout.pageAtPoint(point);
    if (!hit) return null;
    const page = doc.snapshot.pages[hit.index];
    if (!page) return null;
    return linearHitTester.hitPoint(page, hit.local, 3 / cssPxPerPt(this.view.zoom))?.id ?? null;
  }

  private applyEffects(effects: readonly ToolEffect[]): void {
    const state = this.store.getState();
    for (const effect of effects) {
      switch (effect.type) {
        case 'setSelection':
          state.setSelection(this.documentId, effect.ids);
          break;
        case 'moveObjects':
          if (this.editor) {
            // 拖动预览在用的话留着它，等重画好的瓦片到齐（要赶在快照被改之前记下预期的位置）
            if (this.dragFrom) {
              this.holdDragLayers(effect.ids, translateRect(this.dragFrom, effect.dx, effect.dy));
            }
            this.transformViaEditor(
              this.editor,
              effect.ids,
              translationMatrix(effect.dx, effect.dy),
            );
          } else {
            state.moveObjects(this.documentId, effect.ids, effect.dx, effect.dy);
          }
          break;
        case 'resizeObjects': {
          if (this.editor) {
            const ids = [...effect.rects.keys()];
            this.holdDragLayers(ids, effect.to);
            this.transformViaEditor(this.editor, ids, rectToRectMatrix(effect.from, effect.to));
            break;
          }
          const local = new Map<string, Rect>();
          for (const [id, docRect] of effect.rects) {
            const ref = this.objectById(id);
            if (!ref) continue;
            const pageRect = this.layout.pageRect(ref.page);
            local.set(id, translateRect(docRect, -pageRect.x, -pageRect.y));
          }
          state.resizeObjects(this.documentId, local);
          break;
        }
        case 'deleteObjects':
          if (this.editor) this.editor.deleteObjects(effect.ids);
          else state.deleteObjects(this.documentId, effect.ids);
          break;
        case 'setView':
          this.setView(effect.view);
          break;
        case 'setTool':
          state.setTool(effect.tool);
          break;
        case 'overlay':
          this.overlay = effect.overlay;
          break;
        case 'cursor':
          this.setCursor(effect.cursor);
          break;
      }
    }
  }

  private updateCursor(): void {
    this.setCursor(this.tools.cursor(this.toolContext(), this.hoverScreen));
  }

  private setCursor(cursor: string): void {
    if (cursor === this.cursor) return;
    this.cursor = cursor;
    this.overlayCanvas.style.cursor = cursor;
  }
}

function buildLayout(snapshot: DocumentSnapshot | undefined): PageLayout {
  return new PageLayout(
    snapshot ? snapshot.pages.map((p) => ({ width: p.width, height: p.height })) : [],
  );
}

/**
 * 这一页的画面是不是变了（瓦片要重画）。给了修订号的页只看修订号；
 * 没给的（模拟文档）页面对象换了就算变了。
 */
function pageRepainted(before: PageSnapshot, after: PageSnapshot): boolean {
  if (before === after) return false;
  if (before.revision !== undefined || after.revision !== undefined) {
    return before.revision !== after.revision;
  }
  return true;
}

function pagesResized(a: DocumentSnapshot, b: DocumentSnapshot): boolean {
  for (let i = 0; i < a.pages.length; i++) {
    const pa = a.pages[i]!;
    const pb = b.pages[i]!;
    if (pa.width !== pb.width || pa.height !== pb.height) return true;
  }
  return false;
}

/** 两组对象 id 是不是同一组（顺序也一样）。 */
function sameIds(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((id, i) => id === b[i]);
}

/** 两个矩形的四条边都相差不超过 `tolerance`。 */
function rectsClose(a: Rect, b: Rect, tolerance: number): boolean {
  return (
    Math.abs(a.x - b.x) <= tolerance &&
    Math.abs(a.y - b.y) <= tolerance &&
    Math.abs(a.x + a.width - (b.x + b.width)) <= tolerance &&
    Math.abs(a.y + a.height - (b.y + b.height)) <= tolerance
  );
}

/** 文档矩形 → 设备像素矩形 */
function deviceRect(r: Rect, view: ViewState, scale: number): Rect {
  const x = Math.round((r.x - view.x) * scale);
  const y = Math.round((r.y - view.y) * scale);
  return {
    x,
    y,
    width: Math.round((r.x + r.width - view.x) * scale) - x,
    height: Math.round((r.y + r.height - view.y) * scale) - y,
  };
}

/** 瓦片矩形：以页面左上角的整数像素为基准，档位与缩放一致时正好落在像素格上 */
function tileDeviceRect(tile: Rect, pageRect: Rect, view: ViewState, scale: number): Rect {
  const originX = Math.round((pageRect.x - view.x) * scale);
  const originY = Math.round((pageRect.y - view.y) * scale);
  return {
    x: originX + (tile.x - pageRect.x) * scale,
    y: originY + (tile.y - pageRect.y) * scale,
    width: tile.width * scale,
    height: tile.height * scale,
  };
}
