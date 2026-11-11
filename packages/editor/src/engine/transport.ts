import { ERROR_CODE } from '@magpdf/protocol';
import type { RpcError } from '@magpdf/protocol';

/**
 * 界面到主进程的通道（《接口协议》第 2、3 节）。桌面端用 Tauri 的命令、事件和 `magpdf://` 协议实现，
 * 测试里用假的。编辑器只认这个接口，不直接依赖 Tauri。
 */
export interface EngineTransport {
  /**
   * 文档类请求，主进程原样转给引擎。失败时以协议的错误对象（`{ code, message, data? }`）拒绝，
   * 用 {@link isRpcError} 判断。
   */
  request<T = unknown>(method: string, params?: unknown): Promise<T>;
  /** 订阅引擎和主进程推送的事件（《接口协议》第 7 节）；返回取消订阅的函数。 */
  onEvent(listener: (event: EngineEvent) => void): () => void;
  /**
   * 取一块瓦片的像素：512×512、BGRA、自顶向下（《接口协议》3.3）。
   * 槽位已经被别的瓦片占了（HTTP 410）时以 {@link TileGoneError} 拒绝，调用方重新请求渲染。
   */
  fetchTile(slot: number, generation: number): Promise<Uint8Array>;
}

export interface EngineEvent {
  readonly method: string;
  readonly params: unknown;
}

/** 槽位里已经不是要的那块瓦片了。 */
export class TileGoneError extends Error {
  override readonly name = 'TileGoneError';

  constructor(slot: number, generation: number) {
    super(`瓦片已被替换：槽位 ${slot}，代数 ${generation}`);
  }
}

export function isRpcError(value: unknown): value is RpcError {
  if (typeof value !== 'object' || value === null) return false;
  const candidate = value as { code?: unknown; message?: unknown };
  return typeof candidate.code === 'number' && typeof candidate.message === 'string';
}

/** 引擎正在重启，这条请求没有结果（《接口协议》第 2 节）：等 `engine.restored` 之后重试。 */
export function isEngineRestarted(error: unknown): boolean {
  return isRpcError(error) && error.code === ERROR_CODE.E_ENGINE_RESTARTED;
}

/** 把请求失败的原因变成一句能给用户看的话。 */
export function describeError(error: unknown): string {
  if (isRpcError(error)) return error.message;
  if (error instanceof Error) return error.message;
  return String(error);
}
