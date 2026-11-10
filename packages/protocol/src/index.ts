/**
 * 界面与引擎之间的协议类型。
 *
 * 消息、事件和错误码在 Rust 侧定义（`engine/magpdf-ipc`），运行 `cargo test -p magpdf-ipc` 时生成到
 * `src/generated/`（方案 3.1：`packages/protocol`，带协议版本号）。这里再导出生成的内容，
 * 手写部分只保留生成器给不出的基础类型和判断函数。
 */
import { PROTOCOL_VERSION } from './generated/constants';

export { ERROR_CODE, EVENT, METHOD, PROTOCOL_VERSION } from './generated/constants';
export type * from './messages';

/** 文档版本号：每条命令携带，引擎按版本顺序推送变更事件（方案 3.1 快照同步）。 */
export type DocumentVersion = number & { readonly __brand: 'DocumentVersion' };

export function asDocumentVersion(n: number): DocumentVersion {
  if (!Number.isInteger(n) || n < 0) {
    throw new RangeError(`文档版本号必须是非负整数，收到 ${n}`);
  }
  return n as DocumentVersion;
}

/** 判断引擎上报的协议版本能否与本界面协作（方案 3.1：主进程启动引擎时检查版本一致）。 */
export function isCompatibleProtocol(engineVersion: number): boolean {
  return engineVersion === PROTOCOL_VERSION;
}
