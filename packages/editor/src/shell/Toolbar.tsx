import { Icon, VerticalSeparator } from '@magpdf/ui';
import { t } from '../i18n';
import { useEditorStore } from '../store/editorStore';
import { toolTooltip, v1Tools } from '../tools/tools';
import type { ToolId } from '../tools/tools';

/**
 * 顶部横排工具栏（方案 8.2）：v1.0 的 7 个工具横排在菜单栏下方、
 * 与选项栏同一行（36px）里，工具之间用竖分隔线分组。
 * 是外壳的普通 flex 行，不再是 dockview 的浮动面板，不会盖住组件面板或画布。
 * 填充/描边色块不在这里，见下面的 ToolbarSwatches，放在那一行的行尾。
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
    </div>
  );
}

/** 填充/描边色块：放在顶部那一行的行尾（纯展示，P0 无取色面板）。 */
export function ToolbarSwatches() {
  return (
    <div className="mg-swatches" title={t.toolbar.fillStroke}>
      <span className="mg-swatch mg-swatch-fill" aria-hidden />
      <span className="mg-swatch mg-swatch-stroke" aria-hidden />
    </div>
  );
}
