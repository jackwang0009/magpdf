import { tools } from '@magpdf/editor';
import { PROTOCOL_VERSION } from '@magpdf/protocol';
import { metrics } from '@magpdf/ui';

/**
 * 占位外壳：只摆出方案 8.2 的区块与尺寸，证明工作区各包能互相引用。
 * dockview 外壳、WebGL2 瓦片画布、选择框由 V12 在 packages/editor 中实现后替换这里。
 */
export function App() {
  return (
    <div className="shell">
      <header className="menu-bar" style={{ height: metrics.menuBarHeight }}>
        MagPDF
      </header>
      <div className="options-bar" style={{ height: metrics.optionsBarHeight }}>
        选项栏
      </div>
      <main className="main">
        <nav className="toolbar" style={{ width: metrics.toolbarWidth }} aria-label="工具栏">
          {tools
            .filter((t) => t.v1_0)
            .map((t) => (
              <button key={t.id} type="button" title={`${t.label}（${t.key}）`}>
                {t.key}
              </button>
            ))}
        </nav>
        <section className="canvas" aria-label="画布">
          画布（V12：WebGL2 瓦片）
        </section>
        <aside className="dock" style={{ width: metrics.dockPanelWidth }} aria-label="面板">
          属性
        </aside>
      </main>
      <footer className="status-bar" style={{ height: metrics.statusBarHeight }}>
        协议 v{PROTOCOL_VERSION}
      </footer>
    </div>
  );
}
