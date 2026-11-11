import { createContext, useContext } from 'react';
import type { TileProvider } from '../canvas/tileProvider';

/**
 * 外壳向“宿主”要的东西：真实文档怎么打开、它们的瓦片找谁要。
 * 桌面应用里宿主是接着引擎的 `EngineSession`；没有宿主时（浏览器里看界面、测试）外壳用模拟文档。
 */
export interface EditorHost {
  /**
   * 文件 > 打开。用户取消时正常结束；打不开时拒绝，原因由外壳告诉用户
   * （协议的错误对象或 Error，用 `describeError` 取文字）。
   */
  openDocument(): Promise<void>;
  /** 某份文档的瓦片来源。不是宿主打开的文档返回 undefined，画布退回模拟来源。 */
  tileProvider(documentId: string): TileProvider | undefined;
  /** 宿主有事要告诉用户（引擎重启了、文档没能恢复）。返回取消订阅的函数。 */
  onNotice(listener: (notice: HostNotice) => void): () => void;
}

export interface HostNotice {
  readonly kind: 'info' | 'error';
  /** 可以直接显示的中文 */
  readonly message: string;
}

export const EditorHostContext = createContext<EditorHost | null>(null);

/** 当前的宿主；没有时为 null。 */
export function useEditorHost(): EditorHost | null {
  return useContext(EditorHostContext);
}
