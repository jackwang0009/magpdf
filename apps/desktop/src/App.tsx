import { useEffect } from 'react';
import { EditorShell } from '@magpdf/editor';
import { applyTheme } from '@magpdf/ui';

/**
 * 桌面应用入口页：写入主题变量（方案 8.3）后挂载编辑器外壳（方案 8.2）。
 * 外壳、瓦片画布、面板都在 @magpdf/editor 里，这里只做组合。
 */
export function App() {
  useEffect(() => {
    const root = document.documentElement;
    return applyTheme(root);
  }, []);
  return <EditorShell />;
}
