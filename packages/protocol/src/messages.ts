/**
 * 再导出 `generated/` 里由 Rust 生成的消息类型。
 *
 * 新增类型后在这里补一行；漏了的话 `cargo test -p magpdf-ipc` 会报错
 * （`protocol_package_reexports_every_generated_type`）。
 */
export type { BenchEngineParams } from './generated/BenchEngineParams';
export type { BenchEngineResult } from './generated/BenchEngineResult';
export type { DocCloseParams } from './generated/DocCloseParams';
export type { DocInfo } from './generated/DocInfo';
export type { DocOpenParams } from './generated/DocOpenParams';
export type { HelloParams } from './generated/HelloParams';
export type { HelloResult } from './generated/HelloResult';
export type { JsonRpcTag } from './generated/JsonRpcTag';
export type { JsonValue } from './generated/JsonValue';
export type { Notification } from './generated/Notification';
export type { PingParams } from './generated/PingParams';
export type { RenderPriority } from './generated/RenderPriority';
export type { RenderTilesParams } from './generated/RenderTilesParams';
export type { RenderTilesResult } from './generated/RenderTilesResult';
export type { Request } from './generated/Request';
export type { Response } from './generated/Response';
export type { RoundtripParams } from './generated/RoundtripParams';
export type { RoundtripResult } from './generated/RoundtripResult';
export type { RpcError } from './generated/RpcError';
export type { Stats } from './generated/Stats';
export type { TileCoord } from './generated/TileCoord';
export type { TileFetchParams } from './generated/TileFetchParams';
export type { TileFetchResult } from './generated/TileFetchResult';
export type { TileReady } from './generated/TileReady';
