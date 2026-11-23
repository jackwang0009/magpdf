import { Field, Icon, SectionHeader } from '@magpdf/ui';
import { useEffect, useState } from 'react';
import type { KeyboardEvent } from 'react';
import { checkComponent, componentName, sizeLabel } from '../../document/components';
import type { ComponentProblem, ComponentValues } from '../../document/components';
import type { StyleInfo, TextInfo } from '../../document/documentEditor';
import { rectToRectMatrix, rotationMatrix } from '../../document/pageSpace';
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
        <RotateField
          disabled={frozenHere || !host?.documentEditor(documentId)}
          onCommit={(degrees) =>
            host?.documentEditor(documentId)?.transformObjects([
              {
                page: entry.page.index,
                ids: [object.id],
                matrix: rotationMatrix({ x: b.x + b.width / 2, y: b.y + b.height / 2 }, degrees),
              },
            ])
          }
        />
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
      {object.component || object.type === 'component' ? null : (
        <ColorFields
          // 文档版本变了（改了颜色、撤销了）重新读一遍
          key={`color|${object.id}|${doc.snapshot.version}`}
          object={object}
          readOnly={frozenHere}
          documentId={documentId}
        />
      )}
      {object.type === 'text' ? (
        <TextFields
          // 文档版本变了（改了字、撤销了）重新读一遍
          key={`${object.id}|${doc.snapshot.version}`}
          object={object}
          readOnly={frozenHere}
          documentId={documentId}
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

/**
 * 文字对象的文字（方案 5.2）：引擎打开的文档向引擎读，模拟文档用外观里的文字。回车或离开输入框时
 * 文字变了才交给引擎；字体由引擎按三级回退挑，换了字体时状态栏会说。
 */
function TextFields({
  object,
  readOnly,
  documentId,
}: {
  readonly object: PageObject;
  readonly readOnly: boolean;
  readonly documentId: string;
}) {
  const host = useEditorHost();
  const editor = host?.documentEditor(documentId) ?? null;
  const mock = object.appearance?.kind === 'text' ? object.appearance : null;
  const [info, setInfo] = useState<TextInfo | null | 'loading' | 'failed'>(() =>
    editor
      ? 'loading'
      : mock
        ? { content: mock.text, font: '', size: mock.fontSize, editable: true }
        : null,
  );
  const [content, setContent] = useState(typeof info === 'object' ? (info?.content ?? '') : '');
  const [empty, setEmpty] = useState(false);
  /** 已经交出去的文字：回车之后再离开输入框不再发一遍 */
  const [sent, setSent] = useState<string | null>(null);
  useEffect(() => {
    if (!editor) return;
    let current = true;
    editor.readText(object.id).then(
      (read) => {
        if (!current) return;
        setInfo(read);
        setContent(read?.content ?? '');
      },
      (error: unknown) => {
        console.error('读文字失败', error);
        if (current) setInfo('failed');
      },
    );
    return () => {
      current = false;
    };
  }, [editor, object.id]);
  if (info === null) return null;
  if (info === 'loading' || info === 'failed') {
    return (
      <>
        <SectionHeader title={t.properties.text} />
        <p className="mg-props-hint">
          {info === 'loading' ? t.properties.textLoading : t.properties.textUnreadable}
        </p>
      </>
    );
  }
  const locked = readOnly || !info.editable;
  const commit = () => {
    if (locked || content === info.content || content === sent) return;
    if (content.length === 0) {
      setEmpty(true);
      return;
    }
    setEmpty(false);
    setSent(content);
    if (editor) editor.setText(object.id, content);
    else editorStore.getState().setText(documentId, object.id, content);
  };
  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'Enter') commit();
    if (event.key === 'Escape') {
      setContent(info.content);
      setEmpty(false);
    }
  };
  return (
    <>
      <SectionHeader title={t.properties.text} />
      <div className="mg-props-form">
        <label htmlFor="mg-text-content">{t.properties.content}</label>
        <input
          id="mg-text-content"
          className={`mg-input${empty ? ' mg-bad' : ''}`}
          value={content}
          disabled={locked}
          spellCheck={false}
          onChange={(event) => setContent(event.target.value)}
          onBlur={commit}
          onKeyDown={onKeyDown}
        />
        <label>{t.properties.font}</label>
        <span className="mg-props-note">{info.font || '—'}</span>
        <label>{t.properties.fontSize}</label>
        <span className="mg-props-note">{`${Number(info.size.toFixed(2))} pt`}</span>
      </div>
      {empty ? (
        <p className="mg-check-line mg-bad" role="alert">
          <Icon name="close" size={12} /> {t.properties.textEmpty}
        </p>
      ) : info.reason ? (
        <p className="mg-props-hint">{info.reason}</p>
      ) : locked ? null : (
        <>
          <p className="mg-props-hint">{t.properties.applyHint}</p>
          <p className="mg-props-hint">{t.properties.textEditNote}</p>
        </>
      )}
    </>
  );
}

/**
 * 旋转：不是“现在转了多少度”（路径的坐标直接写在内容里，说不清），而是“再转多少度”。输入角度回车，
 * 绕对象中心转过去（顺时针为正，和画布上旋转时标的一样），然后清空。写得不对的、0 度的不发。
 */
function RotateField({
  disabled,
  onCommit,
}: {
  readonly disabled: boolean;
  readonly onCommit: (degrees: number) => void;
}) {
  const [text, setText] = useState('');
  const commit = () => {
    const degrees = Number(text.replace(/[°\s]/g, ''));
    setText('');
    if (text.trim() === '' || !Number.isFinite(degrees) || degrees % 360 === 0) return;
    onCommit(degrees);
  };
  return (
    <span className="mg-mm-field" title={t.properties.rotateHint}>
      <input
        className="mg-input"
        aria-label={t.properties.rotation}
        value={text}
        placeholder="0"
        inputMode="decimal"
        disabled={disabled}
        onChange={(event) => setText(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === 'Enter') commit();
          if (event.key === 'Escape') setText('');
        }}
        onBlur={() => setText('')}
      />
      <span className="mg-field-unit">°</span>
    </span>
  );
}

/** 模拟文档的颜色：方块的填充和描边，文字、文字条的颜色。 */
function mockStyle(object: PageObject): StyleInfo {
  const look = object.appearance;
  if (look?.kind === 'rect') return { fill: look.fill ?? null, stroke: look.stroke ?? null };
  if (look?.kind === 'text' || look?.kind === 'textBars') return { fill: look.color, stroke: null };
  return { fill: null, stroke: null };
}

/** `#rrggbb` 或 `rrggbb`（大小写都行）规整成 `#rrggbb`；写得不对时为 null。 */
function normalizeHex(text: string): string | null {
  const match = /^#?([0-9a-f]{6})$/i.exec(text.trim());
  return match ? `#${match[1]!.toLowerCase()}` : null;
}

/**
 * 颜色：填充色能改（回车或离开输入框生效），描边色只显示。不填充也不描边的对象（图像等）不显示这一节。
 * 引擎打开的文档向引擎读，模拟文档用外观里的颜色。
 */
function ColorFields({
  object,
  readOnly,
  documentId,
}: {
  readonly object: PageObject;
  readonly readOnly: boolean;
  readonly documentId: string;
}) {
  const host = useEditorHost();
  const editor = host?.documentEditor(documentId) ?? null;
  const [style, setStyle] = useState<StyleInfo | null>(() => (editor ? null : mockStyle(object)));
  const [fill, setFill] = useState(style?.fill ?? '');
  const [bad, setBad] = useState(false);
  const [sent, setSent] = useState<string | null>(null);
  useEffect(() => {
    if (!editor) return;
    let current = true;
    editor.readStyle(object.id).then(
      (read) => {
        if (!current) return;
        setStyle(read);
        setFill(read.fill ?? '');
      },
      (error: unknown) => console.error('读颜色失败', error),
    );
    return () => {
      current = false;
    };
  }, [editor, object.id]);
  if (!style || (!style.fill && !style.stroke)) return null;
  const commit = () => {
    if (readOnly || !style.fill) return;
    const color = normalizeHex(fill);
    if (!color) {
      setBad(true);
      return;
    }
    setBad(false);
    setFill(color);
    if (color === style.fill.toLowerCase() || color === sent) return;
    setSent(color);
    if (editor) editor.setFill([object.id], color);
    else editorStore.getState().setFill(documentId, [object.id], color);
  };
  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'Enter') commit();
    if (event.key === 'Escape') {
      setFill(style.fill ?? '');
      setBad(false);
    }
  };
  return (
    <>
      <SectionHeader title={t.properties.colors} />
      <div className="mg-props-form">
        {style.fill ? (
          <>
            <label htmlFor="mg-fill">{t.properties.fill}</label>
            <span className="mg-color-field">
              <span
                className="mg-swatch"
                style={{ background: normalizeHex(fill) ?? style.fill }}
                aria-hidden
              />
              <input
                id="mg-fill"
                className={`mg-input mg-mono${bad ? ' mg-bad' : ''}`}
                value={fill}
                disabled={readOnly}
                spellCheck={false}
                onChange={(event) => setFill(event.target.value)}
                onBlur={commit}
                onKeyDown={onKeyDown}
              />
            </span>
          </>
        ) : null}
        {style.stroke ? (
          <>
            <label>{t.properties.stroke}</label>
            <span className="mg-color-field">
              <span className="mg-swatch" style={{ background: style.stroke }} aria-hidden />
              <span className="mg-props-note mg-mono">{style.stroke}</span>
            </span>
          </>
        ) : null}
      </div>
      {bad ? (
        <p className="mg-check-line mg-bad" role="alert">
          <Icon name="close" size={12} /> {t.properties.colorInvalid}
        </p>
      ) : style.fill && !readOnly ? (
        <p className="mg-props-hint">{t.properties.colorNote}</p>
      ) : null}
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
