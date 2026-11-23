import { Icon } from '@magpdf/ui';
import type { IconName } from '@magpdf/ui';
import { useState } from 'react';
import type { DragEvent } from 'react';
import { t } from '../../i18n';
import { activeDocument, editorStore, useEditorStore } from '../../store/editorStore';
import type { PageObject } from '../../document/types';
import { useEditorHost } from '../editorHost';
import { PanelFrame, PanelMenuButton, PanelSearch } from './PanelFrame';

const TYPE_ICON: Record<PageObject['type'], IconName> = {
  text: 'text',
  path: 'shape',
  image: 'scan',
  shading: 'palette',
  form: 'group',
  component: 'components',
  other: 'more',
};

/** 拖动图层时 dataTransfer 里的类型：只认自己拖出来的，别的东西拖进来不理。 */
const LAYER_DRAG_TYPE = 'application/x-magpdf-layers';

/** 拖到哪一行的上面（下面）。 */
interface DropTarget {
  readonly id: string;
  readonly above: boolean;
}

/**
 * 图层面板：按页列出对象（顶层在上），点选与画布选区同步。按名字或种类筛选；拖动一行（或选中的几行）
 * 放到别的行的上面或下面，调整层级。只能叠加内容的页、筛选的时候不能拖。每一行前面是“显示”“锁定”
 * 两个开关，只在本次会话有效（方案 4.3）：隐藏的不画、锁定的照画，两种都点不中、不能拖。
 */
export function LayersPanel() {
  const doc = useEditorStore(activeDocument);
  const setSelection = useEditorStore((s) => s.setSelection);
  const setHidden = useEditorStore((s) => s.setHidden);
  const setLocked = useEditorStore((s) => s.setLocked);
  const host = useEditorHost();
  const [filter, setFilter] = useState('');
  const [dragging, setDragging] = useState<readonly string[] | null>(null);
  const [drop, setDrop] = useState<DropTarget | null>(null);
  const page = doc?.snapshot.pages[doc.currentPage];
  const query = filter.trim().toLowerCase();
  const matches = (object: PageObject) =>
    !query ||
    object.name.toLowerCase().includes(query) ||
    t.objectTypes[object.type].includes(query);
  const draggable = Boolean(page && !page.overlayOnly && !query);
  const hidden = new Set(doc?.hidden ?? []);
  const locked = new Set(doc?.locked ?? []);

  const finishDrag = () => {
    setDragging(null);
    setDrop(null);
  };

  /** 拖的这几个对象放到 `target` 那一行的上面或下面。 */
  const dropOn = (target: DropTarget) => {
    if (!doc || !page || !dragging || dragging.includes(target.id)) return;
    const moving = new Set(dragging);
    // 没被拖的对象从下往上数，target 在第几个；放在它上面就是它后面那个位置
    const others = page.objects.filter((object) => !moving.has(object.id));
    const at = others.findIndex((object) => object.id === target.id);
    if (at < 0) return;
    const index = target.above ? at + 1 : at;
    const editor = host?.documentEditor(doc.snapshot.id);
    if (editor) editor.reorder(dragging, { index });
    else editorStore.getState().reorderObjects(doc.snapshot.id, dragging, { index });
  };

  const rowDragProps = (object: PageObject, selected: boolean) =>
    draggable && doc && page
      ? {
          draggable: true,
          onDragStart: (event: DragEvent) => {
            // 拖的是选中的那一行：带上这一页上所有选中的；拖的是没选中的：只拖它，并选中它
            const onPage = new Set(page.objects.map((o) => o.id));
            const ids = selected ? doc.selection.filter((id) => onPage.has(id)) : [object.id];
            if (!selected) setSelection(doc.snapshot.id, ids);
            event.dataTransfer?.setData(LAYER_DRAG_TYPE, ids.join(' '));
            if (event.dataTransfer) event.dataTransfer.effectAllowed = 'move';
            setDragging(ids);
          },
          onDragEnd: finishDrag,
        }
      : {};

  const rowDropProps = (object: PageObject) =>
    dragging
      ? {
          onDragOver: (event: DragEvent<HTMLLIElement>) => {
            if (dragging.includes(object.id)) return;
            event.preventDefault();
            if (event.dataTransfer) event.dataTransfer.dropEffect = 'move';
            const rect = event.currentTarget.getBoundingClientRect();
            const above = event.clientY < rect.top + rect.height / 2;
            if (drop?.id !== object.id || drop.above !== above) setDrop({ id: object.id, above });
          },
          onDrop: (event: DragEvent<HTMLLIElement>) => {
            event.preventDefault();
            const rect = event.currentTarget.getBoundingClientRect();
            dropOn({ id: object.id, above: event.clientY < rect.top + rect.height / 2 });
            finishDrag();
          },
        }
      : {};

  return (
    <PanelFrame
      toolbar={
        <>
          <PanelSearch
            placeholder={t.layers.filterPlaceholder}
            value={filter}
            onChange={setFilter}
          />
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
            {[...page.objects]
              .reverse()
              .filter(matches)
              .map((object) => {
                const selected = doc.selection.includes(object.id);
                const marker =
                  drop?.id === object.id ? (drop.above ? 'mg-drop-above' : 'mg-drop-below') : '';
                const isHidden = hidden.has(object.id);
                const isLocked = Boolean(object.locked) || locked.has(object.id);
                const toggles = (
                  <>
                    <button
                      type="button"
                      className={`mg-layer-toggle${isHidden ? ' mg-off' : ''}`}
                      aria-pressed={!isHidden}
                      aria-label={`${isHidden ? t.layers.show : t.layers.hide} ${object.name}`}
                      title={`${isHidden ? t.layers.show : t.layers.hide}（${t.layers.sessionOnly}）`}
                      onClick={() => setHidden(doc.snapshot.id, [object.id], !isHidden)}
                    >
                      <Icon name="eye" size={12} />
                    </button>
                    <button
                      type="button"
                      className={`mg-layer-toggle${isLocked ? '' : ' mg-off'}`}
                      aria-pressed={isLocked}
                      aria-label={`${isLocked ? t.layers.unlock : t.layers.lockLayer} ${object.name}`}
                      title={`${isLocked ? t.layers.unlock : t.layers.lockLayer}（${t.layers.sessionOnly}）`}
                      // 文件里本来就锁着的（模拟文档的数据）这里解不开
                      disabled={Boolean(object.locked)}
                      onClick={() => setLocked(doc.snapshot.id, [object.id], !isLocked)}
                    >
                      <Icon name="lock" size={12} />
                    </button>
                  </>
                );
                return (
                  <li
                    key={object.id}
                    className={`mg-layer-row${marker ? ` ${marker}` : ''}`}
                    {...rowDropProps(object)}
                  >
                    {toggles}
                    {isLocked || isHidden ? (
                      <span
                        className={`mg-layer ${isHidden ? 'mg-layer-hidden' : 'mg-layer-locked'}`}
                        title={isHidden ? t.layers.hidden : t.layers.locked}
                      >
                        <Icon name={TYPE_ICON[object.type]} size={13} className="mg-muted" />
                        <span className="mg-layer-name">{object.name}</span>
                      </span>
                    ) : (
                      <button
                        type="button"
                        className={`mg-layer${selected ? ' mg-on' : ''}`}
                        aria-pressed={selected}
                        {...rowDragProps(object, selected)}
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
          {query && !page.objects.some(matches) ? (
            <p className="mg-empty">{t.layers.noMatch}</p>
          ) : null}
        </>
      ) : (
        <p className="mg-empty">{t.documents.none}</p>
      )}
    </PanelFrame>
  );
}
