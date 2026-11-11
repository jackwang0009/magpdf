import { useEffect } from 'react';
import { EditorShell } from '@magpdf/editor';
import type { EditorHost } from '@magpdf/editor';
import { applyTheme } from '@magpdf/ui';

export interface AppProps {
  /** 接着引擎的宿主（桌面外壳里才有）。没有的话外壳打开两份模拟文档，方便在浏览器里看界面。 */
  readonly host?: EditorHost;
}

/**
 * 桌面应用入口页：写入主题变量（方案 8.3）后挂载编辑器外壳（方案 8.2）。
 * 外壳、瓦片画布、面板都在 @magpdf/editor 里，这里只做组合。
 */
export function App({ host }: AppProps) {
  useEffect(() => {
    const root = document.documentElement;
    return applyTheme(root);
  }, []);
  return host ? <EditorShell host={host} demoDocuments={false} /> : <EditorShell />;
}
