import { useEffect, useRef } from 'react';
import { editorStore } from '../store/editorStore';
import type { EditorStore } from '../store/editorStore';
import { CanvasController } from './canvasController';
import type { TileProvider } from './tileProvider';

export interface TileCanvasProps {
  readonly documentId: string;
  readonly store?: EditorStore;
  readonly provider?: TileProvider;
}

/** React 壳：挂载时创建 CanvasController，卸载时销毁。渲染循环与事件都在控制器里。 */
export function TileCanvas({ documentId, store = editorStore, provider }: TileCanvasProps) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const container = ref.current;
    if (!container) return;
    const controller = new CanvasController(
      provider ? { store, documentId, container, provider } : { store, documentId, container },
    );
    return () => controller.dispose();
  }, [documentId, store, provider]);
  return <div ref={ref} className="mg-canvas" />;
}
