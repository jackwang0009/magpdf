import { Icon } from '@magpdf/ui';
import type { IconName } from '@magpdf/ui';
import { t } from '../../i18n';
import { activeDocument, useEditorStore } from '../../store/editorStore';
import type { PageObject } from '../../document/types';
import { PanelFrame, PanelMenuButton, PanelSearch } from './PanelFrame';

const TYPE_ICON: Record<PageObject['type'], IconName> = {
  text: 'text',
  path: 'shape',
  image: 'scan',
  component: 'components',
};

/** 图层面板：按页列出对象（顶层在上），点选与画布选区同步。 */
export function LayersPanel() {
  const doc = useEditorStore(activeDocument);
  const setSelection = useEditorStore((s) => s.setSelection);
  const page = doc?.snapshot.pages[doc.currentPage];
  return (
    <PanelFrame
      toolbar={
        <>
          <PanelSearch placeholder={t.layers.filterPlaceholder} />
          <PanelMenuButton />
        </>
      }
    >
      {doc && page ? (
        <>
          <div className="mg-layer-page">
            {t.layers.pageRow.replace('{n}', String(page.index + 1))}
          </div>
          <ul className="mg-layers">
            {[...page.objects].reverse().map((object) => {
              const selected = doc.selection.includes(object.id);
              return (
                <li key={object.id}>
                  {object.locked ? (
                    <span className="mg-layer mg-layer-locked" title={t.layers.locked}>
                      <Icon name={TYPE_ICON[object.type]} size={13} className="mg-muted" />
                      <span className="mg-layer-name">{object.name}</span>
                      <Icon name="lock" size={12} className="mg-muted" />
                    </span>
                  ) : (
                    <button
                      type="button"
                      className={`mg-layer${selected ? ' mg-on' : ''}`}
                      aria-pressed={selected}
                      onClick={(event) => {
                        const next = event.shiftKey
                          ? selected
                            ? doc.selection.filter((id) => id !== object.id)
                            : [...doc.selection, object.id]
                          : [object.id];
                        setSelection(doc.snapshot.id, next);
                      }}
                    >
                      <Icon name={TYPE_ICON[object.type]} size={13} className="mg-muted" />
                      <span className="mg-layer-name">{object.name}</span>
                    </button>
                  )}
                </li>
              );
            })}
          </ul>
        </>
      ) : (
        <p className="mg-empty">{t.documents.none}</p>
      )}
    </PanelFrame>
  );
}
