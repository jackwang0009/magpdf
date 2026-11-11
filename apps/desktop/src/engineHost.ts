import { invoke, isTauri } from '@tauri-apps/api/core';
import { EngineSession, editorStore } from '@magpdf/editor';
import type { DocInfo } from '@magpdf/protocol';
import { TauriTransport } from './tauriTransport';

/**
 * 在桌面外壳里运行时，建立到引擎的会话并把已经开着的文档接进来；
 * 在普通浏览器里（`pnpm dev` 直接看界面）返回 null，界面用模拟文档。
 *
 * 整个页面只调一次（`main.tsx`）：会话订阅了主进程的事件和编辑器的 store，建两个会重复处理。
 */
export function createEngineHost(): EngineSession | null {
  if (!isTauri()) return null;
  const session = new EngineSession({
    transport: new TauriTransport(),
    store: editorStore,
    // 路径由主进程经手：对话框是它弹的，文件是它交给引擎打开的
    pickDocument: () => invoke<DocInfo | null>('open_document_dialog'),
    listDocuments: () => invoke<DocInfo[]>('open_documents'),
  });
  session.attach().catch((error: unknown) => console.error('接上已打开的文档失败', error));
  return session;
}
