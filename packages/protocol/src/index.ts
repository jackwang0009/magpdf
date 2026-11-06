/**
 * 界面与引擎之间的协议类型。
 *
 * 正式的消息、事件和错误码在 Rust 侧定义，再生成到本包（方案 3.1：`packages/protocol`，带协议版本号），
 * 生成流程由 P0 的 V06（进程架构原型）建立。手写部分只保留版本号和生成器需要的基础类型。
 */

/** 协议版本：不兼容修改时加 1（《接口协议》第 4 节、《工程规范》第 8 节）。 */
export const PROTOCOL_VERSION = 1 as const;

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
