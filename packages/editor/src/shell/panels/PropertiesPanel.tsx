import { Field, Icon, SectionHeader } from '@magpdf/ui';
import { useState } from 'react';
import type { KeyboardEvent } from 'react';
import { checkComponent, componentName, sizeLabel } from '../../document/components';
import type { ComponentProblem, ComponentValues } from '../../document/components';
import { rectToRectMatrix } from '../../document/pageSpace';
import type { PageObject } from '../../document/types';
import { ptToMm } from '../../geometry';
import type { Rect } from '../../geometry';
import { t } from '../../i18n';
import { activeDocument, editorStore, useEditorStore } from '../../store/editorStore';
import { overlayOnlyDetail } from '../DocumentBanner';
import { useEditorHost } from '../editorHost';
import { MmField } from '../MmField';
import { PanelFrame } from './PanelFrame';

/** 属性面板：单个对象显示变换与组件参数；多选显示数量；空选显示提示。 */
export function PropertiesPanel() {
  const doc = useEditorStore(activeDocument);
  const host = useEditorHost();
  if (!doc) return <Empty />;
  const objects = doc.snapshot.pages.flatMap((page) =>
    page.objects.filter((o) => doc.selection.includes(o.id)).map((o) => ({ page, o })),
  );
  if (objects.length === 0) {
    return (
      <PanelFrame>
        <p className="mg-empty">{t.properties.noSelection}</p>
      </PanelFrame>
    );
  }
  // 只能叠加内容的页上，原有对象的属性只读，面板顶部说明原因（《交互设计》5.3）；新加的组件照样能改
  const frozen = objects
    .filter(({ page, o }) => page.overlayOnly && !o.added)
    .map(({ page }) => page);
  const readOnly =
    frozen.length > 0 ? (
      <p className="mg-props-readonly" title={overlayOnlyDetail(frozen)}>
        <Icon name="lock" size={12} /> {t.overlayOnly.properties}
      </p>
    ) : null;
  if (objects.length > 1) {
    return (
      <PanelFrame>
        {readOnly}
        <p className="mg-empty">
          {t.properties.multiSelection.replace('{n}', String(objects.length))}
        </p>
      </PanelFrame>
    );
  }
  const entry = objects[0]!;
  const object = entry.o;
  const b = object.bbox;
  const documentId = doc.snapshot.id;
  const applyComponent = (values: Partial<ComponentValues>) => {
    const editor = host?.documentEditor(documentId);
    if (editor) editor.updateComponent(object.id, values);
    else editorStore.getState().updateComponent(documentId, object.id, values);
  };
  // 只能叠加内容的页上原有的对象不能动；组件的大小由参数（条高、字号）决定，拉伸会把条码、字形拉变形
  const frozenHere = Boolean(entry.page.overlayOnly && !object.added);
  const fixedSize = object.type === 'component';
  /** 把对象的外框改成 `next`（页面坐标，pt）：交给修改入口的是把原外框映到新外框的变换，撤销照常。 */
  const transformTo = (next: Rect) => {
    const editor = host?.documentEditor(documentId);
    if (editor) {
      editor.transformObjects([
        { page: entry.page.index, ids: [object.id], matrix: rectToRectMatrix(b, next) },
      ]);
    } else if (next.width === b.width && next.height === b.height) {
      editorStore.getState().moveObjects(documentId, [object.id], next.x - b.x, next.y - b.y);
    } else {
      editorStore.getState().resizeObjects(documentId, new Map([[object.id, next]]));
    }
  };
  return (
    <PanelFrame>
      {readOnly}
      <div className="mg-props-head">
        <span className="mg-props-name">{object.name}</span>
        {object.type === 'component' && (object.componentKind || object.component) ? (
          <span className="mg-badge">
            <Icon name="components" size={11} />
            {t.properties.componentBadge}
          </span>
        ) : null}
      </div>
      <SectionHeader title={t.properties.transform} />
      <div className="mg-props-grid">
        <label>{t.options.x}</label>
        <MmField
          key={`x${b.x}`}
          label={t.options.x}
          value={b.x}
          disabled={frozenHere}
          onCommit={(x) => transformTo({ ...b, x })}
        />
        <label>{t.options.y}</label>
        <MmField
          key={`y${b.y}`}
          label={t.options.y}
          value={b.y}
          disabled={frozenHere}
          onCommit={(y) => transformTo({ ...b, y })}
        />
        <label>{t.options.w}</label>
        <MmField
          key={`w${b.width}`}
          label={t.options.w}
          value={b.width}
          positive
          disabled={frozenHere || fixedSize}
          onCommit={(width) => transformTo({ ...b, width })}
        />
        <label>{t.options.h}</label>
        <MmField
          key={`h${b.height}`}
          label={t.options.h}
          value={b.height}
          positive
          disabled={frozenHere || fixedSize}
          onCommit={(height) => transformTo({ ...b, height })}
        />
        <label>{t.properties.rotation}</label>
        <Field value="0°" />
      </div>
      {fixedSize && !frozenHere ? (
        <p className="mg-props-hint">{t.properties.componentSizeHint}</p>
      ) : null}
      {object.component ? (
        <ComponentFields
          // 参数变了（引擎重新生成之后、撤销之后）输入框跟着换成新的值
          key={`${object.id}|${object.component.values.content}|${object.component.values.size}`}
          component={object.component}
          readOnly={frozenHere}
          apply={applyComponent}
        />
      ) : null}
      {object.appearance?.kind === 'barcode' && !object.component ? (
        <>
          <SectionHeader title={t.properties.componentParams} />
          <div className="mg-props-grid">
            <label>{t.properties.codeType}</label>
            <Field value="Code 128" />
            <label>{t.properties.content}</label>
            <Field value={object.appearance.content} mono />
            <label>{t.properties.moduleWidth}</label>
            <Field value="0.375" unit="mm" />
            <label>{t.properties.barHeight}</label>
            <Field value={ptToMm(object.appearance.barHeight).toFixed(2)} unit="mm" />
            <label>{t.properties.quietZone}</label>
            <span className="mg-props-note">{t.properties.quietZoneAuto}</span>
          </div>
          <p className="mg-check-line">
            <Icon name="check" size={12} /> {t.properties.readbackPassed}
          </p>
          <p className="mg-check-line">
            <Icon name="check" size={12} /> {t.properties.sizeChecksPassed}
          </p>
        </>
      ) : null}
      {object.appearance?.kind === 'stamp' ? (
        <>
          <SectionHeader title={t.properties.componentParams} />
          <div className="mg-props-grid">
            <label>{t.properties.content}</label>
            <Field value={object.appearance.text} />
            <label>{t.properties.rotation}</label>
            <Field value={`${object.appearance.angle}°`} />
          </div>
        </>
      ) : null}
      <SectionHeader title={t.properties.name} />
      <div className="mg-props-grid">
        <label>{t.properties.type}</label>
        <span className="mg-props-note">{t.objectTypes[object.type]}</span>
        <label>{t.properties.page}</label>
        <span className="mg-props-note">
          {entry.page.index + 1} / {doc.snapshot.pages.length}
        </span>
      </div>
    </PanelFrame>
  );
}

/**
 * 置入的组件的参数：内容和尺寸（字号或条高）。回车或离开输入框时检查，通过了才交给引擎重新生成；
 * 不通过就在下面说明哪里不对，不发命令。
 */
function ComponentFields({
  component,
  readOnly,
  apply,
}: {
  readonly component: NonNullable<PageObject['component']>;
  readonly readOnly: boolean;
  readonly apply: (values: Partial<ComponentValues>) => void;
}) {
  const { kind, values } = component;
  const [content, setContent] = useState(values.content);
  const [size, setSize] = useState(String(values.size));
  const [problem, setProblem] = useState<ComponentProblem | null>(null);
  const commit = () => {
    const next = { content, size: Number(size) };
    const found = checkComponent(kind, next);
    setProblem(found);
    if (found) return;
    const changed: { content?: string; size?: number } = {};
    if (next.content !== values.content) changed.content = next.content;
    if (next.size !== values.size) changed.size = next.size;
    if (Object.keys(changed).length > 0) apply(changed);
  };
  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'Enter') commit();
    if (event.key === 'Escape') {
      setContent(values.content);
      setSize(String(values.size));
      setProblem(null);
    }
  };
  const bad = (key: ComponentProblem['key']) => (problem?.key === key ? ' mg-bad' : '');
  return (
    <>
      <SectionHeader title={t.properties.componentParams} />
      <div className="mg-props-form">
        <label>{t.properties.type}</label>
        <span className="mg-props-note">{componentName(kind)}</span>
        <label htmlFor="mg-comp-content">{t.properties.content}</label>
        <input
          id="mg-comp-content"
          className={`mg-input${kind === 'code128' ? ' mg-mono' : ''}${bad('content')}`}
          value={content}
          disabled={readOnly}
          spellCheck={false}
          onChange={(event) => setContent(event.target.value)}
          onBlur={commit}
          onKeyDown={onKeyDown}
        />
        <label htmlFor="mg-comp-size">{sizeLabel(kind)}</label>
        <input
          id="mg-comp-size"
          className={`mg-input${bad('size')}`}
          type="number"
          step="any"
          value={size}
          disabled={readOnly}
          onChange={(event) => setSize(event.target.value)}
          onBlur={commit}
          onKeyDown={onKeyDown}
        />
      </div>
      {problem ? (
        <p className="mg-check-line mg-bad" role="alert">
          <Icon name="close" size={12} /> {problem.message}
        </p>
      ) : (
        <p className="mg-props-hint">{t.properties.applyHint}</p>
      )}
      <p className="mg-props-hint">
        {kind === 'text' ? t.properties.textNote : t.properties.barcodeNote}
      </p>
    </>
  );
}

function Empty() {
  return (
    <PanelFrame>
      <p className="mg-empty">{t.documents.none}</p>
    </PanelFrame>
  );
}
