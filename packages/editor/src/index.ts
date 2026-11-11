/**
 * 编辑器核心：瓦片画布（WebGL2）、工具状态机（方案 8.4）、选择与变换、快捷键、工作区布局。
 * P0 的 V12 在这里实现外壳、瓦片画布和选择框原型。
 */

export { PROTOCOL_VERSION } from '@magpdf/protocol';

// 几何与单位
export * from './geometry';

// 界面文字
export { t, format } from './i18n';
export type { Strings } from './i18n/zh-CN';

// 文档模型（P0 模拟数据）
export * from './document/types';
export { createMockDocument, seededRandom } from './document/mockDocument';
export type { MockDocumentOptions } from './document/mockDocument';
export { code128Bars, code128Values } from './document/code128';
export { PageLayout } from './document/pageLayout';
export type { PageLayoutOptions } from './document/pageLayout';
export { linearHitTester } from './document/hitTest';
export type { HitTester, MarqueeMode } from './document/hitTest';
export * from './document/pageSpace';
export type { DocumentEditor, PageTransform } from './document/documentEditor';

// 视图
export * from './view/viewState';

// 瓦片画布
export * from './canvas/tiles';
export * from './canvas/tileProvider';
export { TileCache } from './canvas/tileCache';
export type { TileEntry } from './canvas/tileCache';
export { computeRenderPlan } from './canvas/renderPlan';
export type { RenderPlan, PagePlacement, TileQuad, TileRequestSpec } from './canvas/renderPlan';
export { TileScheduler } from './canvas/tileScheduler';
export { MockTileProvider, drawTile } from './canvas/mockTileProvider';
export type { MockTileProviderOptions } from './canvas/mockTileProvider';
export { FrameStats, percentile } from './canvas/frameStats';
export type { FrameStatsSummary } from './canvas/frameStats';
export { GlTileRenderer, parseCssColor } from './canvas/glRenderer';
export { CanvasController } from './canvas/canvasController';
export type { CanvasControllerOptions } from './canvas/canvasController';
export { TileCanvas } from './canvas/TileCanvas';
export { runBenchmark } from './canvas/benchmark';
export type { BenchmarkResult } from './canvas/benchmark';

// 工具
export * from './tools/tools';
export * from './tools/shortcuts';
export * from './tools/machine';
export * from './tools/handles';
export { selectTool, NUDGE_STEP_PT } from './tools/selectTool';
export type { SelectState } from './tools/selectTool';
export { handTool } from './tools/handTool';
export type { HandState } from './tools/handTool';
export { zoomTool, zoomToScreenRect } from './tools/zoomTool';
export type { ZoomState } from './tools/zoomTool';
export { ToolManager } from './tools/toolManager';

// 状态
export * from './store/editorStore';

// 工作区
export * from './workspace/workspaceTypes';
export * from './workspace/workspaces';

// 接引擎（《接口协议》第 2、3 节）
export * from './engine/transport';
export {
  EngineTileProvider,
  ENGINE_TILE_SIZE,
  engineTileOrigin,
  cropBgraToRgba,
} from './engine/engineTileProvider';
export type { EngineTileProviderOptions, EngineTileTarget } from './engine/engineTileProvider';
export { EngineSession } from './engine/engineSession';
export type { EngineSessionOptions } from './engine/engineSession';

// 界面外壳
export { EditorShell } from './shell/EditorShell';
export type { EditorShellProps } from './shell/EditorShell';
export type { EditorHost, HostNotice } from './shell/editorHost';
