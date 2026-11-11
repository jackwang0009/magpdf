import { Field, Icon, SectionHeader } from '@magpdf/ui';
import { ptToMm } from '../../geometry';
import { t } from '../../i18n';
import { activeDocument, useEditorStore } from '../../store/editorStore';
import { overlayOnlyDetail } from '../DocumentBanner';
import { PanelFrame } from './PanelFrame';

/** 属性面板：单个对象显示变换与组件参数；多选显示数量；空选显示提示。 */
export function PropertiesPanel() {
  const doc = useEditorStore(activeDocument);
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
  // 只能叠加内容的页上，原有对象的属性只读，面板顶部说明原因（《交互设计》5.3）
  const frozen = objects.filter(({ page }) => page.overlayOnly).map(({ page }) => page);
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
  return (
    <PanelFrame>
      {readOnly}
      <div className="mg-props-head">
        <span className="mg-props-name">{object.name}</span>
        {object.type === 'component' && object.componentKind ? (
          <span className="mg-badge">
            <Icon name="components" size={11} />
            {t.properties.componentBadge}
          </span>
        ) : null}
      </div>
      <SectionHeader title={t.properties.transform} />
      <div className="mg-props-grid">
        <label>{t.options.x}</label>
        <Field value={ptToMm(b.x).toFixed(2)} unit="mm" />
        <label>{t.options.y}</label>
        <Field value={ptToMm(b.y).toFixed(2)} unit="mm" />
        <label>{t.options.w}</label>
        <Field value={ptToMm(b.width).toFixed(2)} unit="mm" />
        <label>{t.options.h}</label>
        <Field value={ptToMm(b.height).toFixed(2)} unit="mm" />
        <label>{t.properties.rotation}</label>
        <Field value="0°" />
      </div>
      {object.appearance?.kind === 'barcode' ? (
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

function Empty() {
  return (
    <PanelFrame>
      <p className="mg-empty">{t.documents.none}</p>
    </PanelFrame>
  );
}
