import { Icon, VerticalSeparator } from '@magpdf/ui';
import { t } from '../i18n';
import { useEditorStore } from '../store/editorStore';
import { toolTooltip, v1Tools } from '../tools/tools';
import type { ToolId } from '../tools/tools';

/**
 * 左侧单列工具栏 40px：v1.0 的 7 个工具 + 填充/描边色块（方案 8.2）。
 * 作为 dockview 的浮动面板挂在网格左侧，不参与网格尺寸分配。
 */
export function Toolbar({ onPick }: { readonly onPick?: (tool: ToolId) => void }) {
  const activeTool = useEditorStore((s) => s.activeTool);
  const setTool = useEditorStore((s) => s.setTool);
  const pick = (tool: ToolId) => {
    setTool(tool);
    onPick?.(tool);
  };
  return (
    <div className="mg-toolbar" role="toolbar" aria-label={t.toolbar.label}>
      {v1Tools.map((spec) => (
        <div key={spec.id} className="mg-tool-slot">
          <button
            type="button"
            className={`mg-tool${activeTool === spec.id ? ' mg-on' : ''}`}
            title={toolTooltip(spec.id)}
            aria-label={toolTooltip(spec.id)}
            aria-pressed={activeTool === spec.id}
            onClick={() => pick(spec.id)}
          >
            <Icon name={spec.icon} size={18} />
          </button>
          {spec.hasVariants ? <span className="mg-tool-variant" aria-hidden /> : null}
          {spec.separatorAfter ? <VerticalSeparator /> : null}
        </div>
      ))}
      <div className="mg-swatches" title={t.toolbar.fillStroke}>
        <span className="mg-swatch mg-swatch-fill" aria-hidden />
        <span className="mg-swatch mg-swatch-stroke" aria-hidden />
      </div>
    </div>
  );
}
