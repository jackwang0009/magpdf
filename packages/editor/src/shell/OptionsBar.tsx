import { Checkbox, Dropdown, Field, Icon, IconButton, VerticalSeparator } from '@magpdf/ui';
import { ptToMm } from '../geometry';
import { t } from '../i18n';
import { activeDocument, useEditorStore } from '../store/editorStore';
import type { DocumentState } from '../store/editorStore';
import { toolLabel } from '../tools/tools';
import type { WorkspaceId } from '../workspace/workspaceTypes';
import { workspaceLabel, WORKSPACE_IDS } from '../workspace/workspaces';
import { useShellCommands } from './shellContext';

function mm(value: number): string {
  return ptToMm(value).toFixed(2);
}

/**
 * 选项栏：与工具栏同一行（36px，方案 8.2），内容随工具变化。
 * 选择工具显示对齐、X/Y/W/H（mm）；抓手/缩放显示缩放命令；其余工具给占位控件。
 */
export function OptionsBar({ workspace }: { readonly workspace: WorkspaceId }) {
  const commands = useShellCommands();
  const tool = useEditorStore((s) => s.activeTool);
  const doc = useEditorStore(activeDocument);
  const selection = doc?.selection ?? [];
  const options = useEditorStore((s) => s.toolOptions);
  const setToolOptions = useEditorStore((s) => s.setToolOptions);

  return (
    <div className="mg-options" role="toolbar" aria-label={t.options.toolPreset}>
      <Dropdown
        value={toolLabel(tool)}
        icon="caret"
        label={t.options.toolPreset}
        onClick={() => undefined}
      />
      <VerticalSeparator />
      {tool === 'select' || tool === 'direct-select' ? (
        <>
          <Checkbox
            checked={options.marqueeMode === 'contain'}
            label={t.options.showTransformControls}
            onChange={(checked) =>
              setToolOptions({ marqueeMode: checked ? 'contain' : 'intersect' })
            }
          />
          <IconButton
            icon="alignLeft"
            label={t.options.alignLeft}
            disabled={selection.length < 2}
          />
          <IconButton
            icon="alignCenter"
            label={t.options.alignCenter}
            disabled={selection.length < 2}
          />
          <IconButton
            icon="alignRight"
            label={t.options.alignRight}
            disabled={selection.length < 2}
          />
          <IconButton icon="alignTop" label={t.options.alignTop} disabled={selection.length < 2} />
          <IconButton
            icon="alignMiddle"
            label={t.options.alignMiddle}
            disabled={selection.length < 2}
          />
          <IconButton
            icon="alignBottom"
            label={t.options.alignBottom}
            disabled={selection.length < 2}
          />
          <VerticalSeparator />
          <TransformFields doc={doc} />
        </>
      ) : null}
      {tool === 'hand' || tool === 'zoom' ? (
        <>
          <Checkbox checked={false} label={t.options.scrollAllWindows} onChange={() => undefined} />
          <VerticalSeparator />
          <IconButton
            icon="zoom"
            label={`${t.options.zoomIn}（Ctrl+=）`}
            onClick={() => commands.run('view.zoomIn')}
          />
          <IconButton
            icon="navigator"
            label={`${t.options.zoomOut}（Ctrl+-）`}
            onClick={() => commands.run('view.zoomOut')}
          />
          <Field value={doc ? `${Math.round(doc.view.zoom * 100)}%` : '100%'} unit="" />
          <IconButton
            icon="pages"
            label={t.options.fitPage}
            onClick={() => commands.run('view.fitPage')}
          />
          <span className="mg-hint">{t.options.zoomHint}</span>
        </>
      ) : null}
      {tool !== 'select' && tool !== 'direct-select' && tool !== 'hand' && tool !== 'zoom' ? (
        <>
          <Field value={toolLabel(tool)} />
          <span className="mg-hint">{t.options.pendingToolHint}</span>
          <TransformFields doc={doc} />
        </>
      ) : null}
      <div className="mg-options-tail">
        <Dropdown
          value={
            <>
              <Icon name="workspace" size={14} />
              {workspaceLabel(workspace)}
            </>
          }
          label={t.options.workspace}
          onClick={() => commands.applyWorkspace(workspace === 'freight' ? 'basic' : 'freight')}
        />
        <Dropdown
          value={WORKSPACE_IDS.map(workspaceLabel).join(' / ')}
          label={t.options.workspace}
          onClick={() => commands.resetWorkspace()}
        />
      </div>
    </div>
  );
}

/** X/Y/W/H 数值框：跟着选区变化，坐标相对当前页左上角，单位 mm（只读，P0） */
function TransformFields({ doc }: { readonly doc: DocumentState | undefined }) {
  const box = doc ? selectionBounds(doc) : null;
  const values = box ?? { x: 0, y: 0, width: 0, height: 0 };
  return (
    <div className="mg-fields">
      <Field value={mm(values.x)} unit="mm" label={`${t.options.x}`} />
      <Field value={mm(values.y)} unit="mm" label={`${t.options.y}`} />
      <Field value={mm(values.width)} unit="mm" label={`${t.options.w}`} />
      <Field value={mm(values.height)} unit="mm" label={`${t.options.h}`} />
    </div>
  );
}

function selectionBounds(doc: DocumentState) {
  if (doc.selection.length === 0) return null;
  const page = doc.snapshot.pages[doc.currentPage];
  if (!page) return null;
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const object of page.objects) {
    if (!doc.selection.includes(object.id)) continue;
    minX = Math.min(minX, object.bbox.x);
    minY = Math.min(minY, object.bbox.y);
    maxX = Math.max(maxX, object.bbox.x + object.bbox.width);
    maxY = Math.max(maxY, object.bbox.y + object.bbox.height);
  }
  if (minX === Infinity) return null;
  return { x: minX, y: minY, width: maxX - minX, height: maxY - minY };
}
