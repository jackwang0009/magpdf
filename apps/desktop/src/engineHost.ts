import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { EngineSession, editorStore } from '@magpdf/editor';
import type { RecoverableDocument } from '@magpdf/editor';
import type { DocInfo, DocSaveResult } from '@magpdf/protocol';
import { TauriTransport } from './tauriTransport';

/**
 * 在桌面外壳里运行时，建立到引擎的会话并把已经开着的文档接进来；
 * 在普通浏览器里（`pnpm dev` 直接看界面）返回 null，界面用模拟文档。
 *
 * 整个页面只调一次（`main.tsx`）：会话订阅了主进程的事件和编辑器的 store，建两个会重复处理。
 */
export function createEngineHost(): EngineSession | null {
  if (!isTauri()) return null;
  const transport = new TauriTransport();
  // 端到端测试和性能测量用（`e2e/`）：在页面里直接比两条取瓦片的路。
  (window as unknown as { __magpdfTransport?: TauriTransport }).__magpdfTransport = transport;
  const session = new EngineSession({
    transport,
    store: editorStore,
    // 路径由主进程经手：对话框是它弹的，文件是它交给引擎打开的
    pickDocument: () => invoke<DocInfo | null>('open_document_dialog'),
    listDocuments: () => invoke<DocInfo[]>('open_documents'),
    recoverableDocuments: () => invoke<RecoverableDocument[]>('recoverable_documents'),
    recoverDocuments: (ids) => invoke<DocInfo[]>('recover_documents', { ids }),
    saveDocumentAs: (docId) => invoke<DocSaveResult | null>('save_document_as', { docId }),
    // 用户点了窗口的关闭按钮：主进程先不关，等外壳问过要不要保存。先回一声“收到了”，不然主进程过几秒就直接关
    exitRequests: (listener) => {
      const unlisten = listen('close-requested', () => {
        void invoke('acknowledge_close_request');
        listener();
      });
      return () => void unlisten.then((stop) => stop());
    },
    exitApp: () => void invoke('confirm_exit'),
  });
  session.attach().catch((error: unknown) => console.error('接上已打开的文档失败', error));
  return session;
}
