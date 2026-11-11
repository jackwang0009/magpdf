import { useEffect, useRef } from 'react';
import type { DocumentEditor } from '../document/documentEditor';
import { editorStore } from '../store/editorStore';
import type { EditorStore } from '../store/editorStore';
import { CanvasController } from './canvasController';
import type { TileProvider } from './tileProvider';

export interface TileCanvasProps {
  readonly documentId: string;
  readonly store?: EditorStore;
  /** 瓦片来源；不给时用模拟来源 */
  readonly provider?: TileProvider;
  /** 修改入口；不给时修改直接落在 store 的快照上（模拟文档） */
  readonly editor?: DocumentEditor;
}

/** React 壳：挂载时创建 CanvasController，卸载时销毁。渲染循环与事件都在控制器里。 */
export function TileCanvas({ documentId, store = editorStore, provider, editor }: TileCanvasProps) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const container = ref.current;
    if (!container) return;
    const controller = new CanvasController({
      store,
      documentId,
      container,
      ...(provider ? { provider } : {}),
      ...(editor ? { editor } : {}),
    });
    return () => controller.dispose();
  }, [documentId, store, provider, editor]);
  return <div ref={ref} className="mg-canvas" />;
}
