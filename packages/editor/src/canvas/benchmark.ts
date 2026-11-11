import type { Size } from '../geometry';
import type { EditorStore } from '../store/editorStore';
import { fitPageZoom } from '../view/viewState';
import type { CanvasController } from './canvasController';
import type { FrameStatsSummary } from './frameStats';
import { MockTileProvider } from './mockTileProvider';

/**
 * 脚本化性能基准：在当前文档上按固定脚本平移、缩放，记录每个阶段的帧时间分位数。
 * 走的是与用户输入相同的 setView 路径（省去事件换算），结果写进报告。
 * 用法：开发模式控制台 `await __magpdf.benchmark()`，或 Playwright 调用。
 */
export interface BenchmarkPhaseResult extends FrameStatsSummary {
  readonly name: string;
}

export interface BenchmarkResult {
  readonly pages: number;
  readonly devicePixelRatio: number;
  readonly viewport: Size;
  readonly phases: readonly BenchmarkPhaseResult[];
  readonly tiles: {
    readonly completed: number;
    readonly aborted: number;
    readonly failed: number;
    readonly cacheCount: number;
    readonly cacheMB: number;
    readonly mockRendered: number | null;
    readonly mockAvgDrawMs: number | null;
  };
  readonly userAgent: string;
}

export interface BenchmarkOptions {
  /** 每个阶段的帧数，默认 240（约 2.4–4 秒，取决于显示器刷新率） */
  readonly framesPerPhase?: number;
  readonly log?: boolean;
  /** 跳页阶段每次跳多少页；默认一屏 5 页（肉眼可读的最大变化速度） */
  readonly jumpPages?: number;
  /**
   * 读屏缩放：view.zoom × CSS_PX_PER_PT（鼠标滚轮一格的步长），默认 1.6。
   * 不叫 CSS 倍率，是因为这里的 zoom 是视图倍数，100% = 1.333 CSS px/pt。
   */
  readonly readZoom?: number;
}

function nextFrame(): Promise<number> {
  return new Promise((resolve) => requestAnimationFrame(resolve));
}

export async function runBenchmark(
  controller: CanvasController,
  store: EditorStore,
  options: BenchmarkOptions = {},
): Promise<BenchmarkResult> {
  const frames = options.framesPerPhase ?? 240;
  const layout = controller.pageLayout;
  const vp = controller.viewport;
  store.getState().setBenchmarkRunning(true);
  const phases: BenchmarkPhaseResult[] = [];

  const runPhase = async (name: string, step: (i: number) => void): Promise<void> => {
    await nextFrame();
    controller.stats.reset();
    for (let i = 0; i < frames; i++) {
      step(i);
      controller.invalidate();
      await nextFrame();
    }
    controller.stats.breakSequence();
    phases.push({ name, ...controller.stats.summary() });
  };

  try {
    // 阶段 1：适合页宽附近放大到读屏比例，匀速向下滚动（鼠标滚轮/触控板的节奏）
    const page0 = layout.pageSize(0);
    const fitZoom = fitPageZoom(page0, vp);
    const readZoom = options.readZoom ?? Math.min(2, fitZoom * 1.6);
    controller.setView({ zoom: readZoom, x: 0, y: 0 });
    controller.scrollToPage(0);
    await runPhase('pan-slow', (i) => controller.panBy(0, i % 60 < 30 ? -12 : 12));

    // 阶段 2：快速滚动（每帧 80 px，瓦片持续缺失，考验渐进显示与调度）
    controller.scrollToPage(0);
    await runPhase('pan-fast', () => controller.panBy(0, -80));

    // 阶段 3：围绕视口中心连续缩放（1/4 倍 ↔ 4 倍，正弦往返两次）
    controller.scrollToPage(Math.min(10, layout.pageCount - 1));
    const base = controller.view.zoom;
    const center = { x: vp.width / 2, y: vp.height / 2 };
    await runPhase('zoom', (i) => {
      const phase = (i / frames) * Math.PI * 4;
      controller.zoomAtPoint(center, base * Math.pow(2, Math.sin(phase) * 2));
    });

    // 阶段 4：跨页跳转（页面面板点击、拖动滚动条；每帧跳 1–10 页，整段停在同一缩放）
    const jumpPages = options.jumpPages ?? Math.min(10, Math.max(1, Math.round(frames / 40)));
    controller.scrollToPage(Math.min(10, layout.pageCount - 1));
    await runPhase('jump', (i) => {
      controller.scrollToPage(Math.min(layout.pageCount - 1, 10 + i * jumpPages));
    });
  } finally {
    store.getState().setBenchmarkRunning(false);
  }

  const schedulerStats = controller.scheduler.stats;
  const provider = controller.tileProvider;
  const mock = provider instanceof MockTileProvider ? provider : null;
  const result: BenchmarkResult = {
    pages: layout.pageCount,
    devicePixelRatio: window.devicePixelRatio,
    viewport: vp,
    phases,
    tiles: {
      completed: schedulerStats.completed,
      aborted: schedulerStats.aborted,
      failed: schedulerStats.failed,
      cacheCount: controller.cache.count,
      cacheMB: Math.round((controller.cache.bytes / 1024 / 1024) * 10) / 10,
      mockRendered: mock?.stats.rendered ?? null,
      mockAvgDrawMs:
        mock && mock.stats.rendered
          ? Math.round((mock.stats.drawMs / mock.stats.rendered) * 100) / 100
          : null,
    },
    userAgent: navigator.userAgent,
  };
  if (options.log ?? true) {
    console.table(
      phases.map((p) => ({
        阶段: p.name,
        帧数: p.samples,
        'P50 ms': p.intervalP50.toFixed(2),
        'P95 ms': p.intervalP95.toFixed(2),
        'Max ms': p.intervalMax.toFixed(2),
        '脚本 P50': p.workP50.toFixed(2),
        '脚本 P95': p.workP95.toFixed(2),
        长帧: p.longFrames,
      })),
    );
    console.info('瓦片统计', result.tiles);
  }
  return result;
}
