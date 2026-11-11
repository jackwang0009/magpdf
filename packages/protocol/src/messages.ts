/**
 * 再导出 `generated/` 里由 Rust 生成的消息类型。
 *
 * 新增类型后在这里补一行；漏了的话 `cargo test -p magpdf-ipc` 会报错
 * （`protocol_package_reexports_every_generated_type`）。
 */
export type { BenchEngineParams } from './generated/BenchEngineParams';
export type { BenchEngineResult } from './generated/BenchEngineResult';
export type { Color } from './generated/Color';
export type { ColorSpace } from './generated/ColorSpace';
export type { DocChanged } from './generated/DocChanged';
export type { DocCloseParams } from './generated/DocCloseParams';
export type { DocInfo } from './generated/DocInfo';
export type { DocOpenParams } from './generated/DocOpenParams';
export type { DocSnapshot } from './generated/DocSnapshot';
export type { DocSnapshotParams } from './generated/DocSnapshotParams';
export type { EditDeleteParams } from './generated/EditDeleteParams';
export type { EditResult } from './generated/EditResult';
export type { EditSetStyleParams } from './generated/EditSetStyleParams';
export type { EditTransformParams } from './generated/EditTransformParams';
export type { EngineFailed } from './generated/EngineFailed';
export type { EngineRestored } from './generated/EngineRestored';
export type { Fidelity } from './generated/Fidelity';
export type { FidelityCheckParams } from './generated/FidelityCheckParams';
export type { FidelityResult } from './generated/FidelityResult';
export type { HelloParams } from './generated/HelloParams';
export type { HelloResult } from './generated/HelloResult';
export type { HistoryParams } from './generated/HistoryParams';
export type { HitMode } from './generated/HitMode';
export type { JsonRpcTag } from './generated/JsonRpcTag';
export type { JsonValue } from './generated/JsonValue';
export type { LayerMode } from './generated/LayerMode';
export type { Notification } from './generated/Notification';
export type { ObjHitTestParams } from './generated/ObjHitTestParams';
export type { ObjListParams } from './generated/ObjListParams';
export type { ObjectKind } from './generated/ObjectKind';
export type { ObjectSummary } from './generated/ObjectSummary';
export type { PageChange } from './generated/PageChange';
export type { PageInfo } from './generated/PageInfo';
export type { PingParams } from './generated/PingParams';
export type { RenderPriority } from './generated/RenderPriority';
export type { RenderTilesParams } from './generated/RenderTilesParams';
export type { RenderTilesResult } from './generated/RenderTilesResult';
export type { Request } from './generated/Request';
export type { Response } from './generated/Response';
export type { RoundtripParams } from './generated/RoundtripParams';
export type { RoundtripResult } from './generated/RoundtripResult';
export type { RpcError } from './generated/RpcError';
export type { StallParams } from './generated/StallParams';
export type { Stats } from './generated/Stats';
export type { TileCoord } from './generated/TileCoord';
export type { TileFetchParams } from './generated/TileFetchParams';
export type { TileFetchResult } from './generated/TileFetchResult';
export type { TileLayer } from './generated/TileLayer';
export type { TileReady } from './generated/TileReady';
