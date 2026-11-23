import { Icon, IconButton } from '@magpdf/ui';
import type { IconName } from '@magpdf/ui';
import { COMPONENT_DRAG_TYPE, isPlaceable } from '../../document/components';
import type { PlaceableComponent } from '../../document/components';
import { format, t } from '../../i18n';
import { useEditorStore } from '../../store/editorStore';
import { useShellCommands } from '../shellContext';
import { PanelFrame, PanelMenuButton, PanelSearch } from './PanelFrame';

type Category = keyof typeof t.components.categories;

interface ComponentItem {
  readonly id: keyof typeof t.components.items;
  readonly icon: IconName;
}

const CATEGORIES: readonly { id: Category; items: readonly ComponentItem[] }[] = [
  {
    id: 'recent',
    items: [
      { id: 'code128', icon: 'barcode' },
      { id: 'text', icon: 'text' },
      { id: 'date', icon: 'text' },
      { id: 'seal', icon: 'shape' },
    ],
  },
  {
    id: 'basic',
    items: [
      { id: 'text', icon: 'text' },
      { id: 'date', icon: 'text' },
      { id: 'serial', icon: 'listView' },
      { id: 'checkbox', icon: 'check' },
      { id: 'image', icon: 'scan' },
    ],
  },
  {
    id: 'barcode',
    items: [
      { id: 'code128', icon: 'barcode' },
      { id: 'qr', icon: 'gridView' },
      { id: 'gs1', icon: 'barcode' },
      { id: 'ean', icon: 'barcode' },
      { id: 'itf', icon: 'barcode' },
      { id: 'dm', icon: 'gridView' },
    ],
  },
  {
    id: 'stamp',
    items: [
      { id: 'seal', icon: 'shape' },
      { id: 'signature', icon: 'font' },
      { id: 'textStamp', icon: 'font' },
    ],
  },
  {
    id: 'freight',
    items: [
      { id: 'container', icon: 'barcode' },
      { id: 'bl', icon: 'barcode' },
      { id: 'awb', icon: 'barcode' },
      { id: 'amount', icon: 'font' },
    ],
  },
  {
    id: 'marks',
    items: [
      { id: 'seal', icon: 'shape' },
      { id: 'signature', icon: 'font' },
    ],
  },
];

/**
 * 组件面板：搜索框、分类页签、组件网格。展开宽 240px（可拖到 200–400），
 * 折叠成 32px 图标条时只显示图标（见 ComponentStrip）。
 */
export function ComponentsPanel() {
  const view = useEditorStore((s) => s.componentView);
  const setView = useEditorStore((s) => s.setComponentView);
  const mode = useEditorStore((s) => s.componentPanelMode);
  const setMode = useEditorStore((s) => s.setComponentPanelMode);
  return (
    <PanelFrame
      toolbar={
        <>
          <PanelSearch placeholder={t.components.searchPlaceholder} />
          <IconButton
            icon="gridView"
            small
            label={t.panelActions.gridView}
            active={view === 'grid'}
            onClick={() => setView('grid')}
          />
          <IconButton
            icon="listView"
            small
            label={t.panelActions.listView}
            active={view === 'list'}
            onClick={() => setView('list')}
          />
          <IconButton
            icon="collapse"
            small
            label={t.panelActions.collapseComponents}
            onClick={() => setMode('collapsed')}
          />
          <PanelMenuButton />
        </>
      }
    >
      {mode !== 'expanded' ? <CollapsedHint /> : null}
      {CATEGORIES.map((category) => (
        <section key={category.id} className="mg-comp-group">
          <h3 className="mg-comp-title">{t.components.categories[category.id]}</h3>
          <ul className={`mg-comp-list mg-comp-${view}`}>
            {category.items.map((item, index) => (
              <li key={`${item.id}-${index}`}>
                <ComponentButton item={item} />
              </li>
            ))}
          </ul>
        </section>
      ))}
    </PanelFrame>
  );
}

/**
 * 一个组件。能用的（文本、Code 128）点一下进入放置状态，再点页面放下去；也可以直接拖到页面上。
 * 别的组件 P1 才有，按钮是灰的。
 */
function ComponentButton({ item }: { readonly item: ComponentItem }) {
  const placing = useEditorStore((s) => s.placing);
  const setPlacing = useEditorStore((s) => s.setPlacing);
  const name = t.components.items[item.id];
  const kind: PlaceableComponent | null = isPlaceable(item.id) ? item.id : null;
  if (!kind) {
    return (
      <button
        type="button"
        className="mg-comp-item"
        disabled
        title={format(t.components.notYet, { name })}
      >
        <span className="mg-comp-preview">
          <Icon name={item.icon} size={20} />
        </span>
        <span className="mg-comp-name">{name}</span>
      </button>
    );
  }
  const active = placing === kind;
  return (
    <button
      type="button"
      className={`mg-comp-item${active ? ' mg-on' : ''}`}
      title={format(t.components.placeHint, { name })}
      aria-pressed={active}
      draggable
      onClick={() => setPlacing(active ? null : kind)}
      onDragStart={(event) => {
        event.dataTransfer.setData(COMPONENT_DRAG_TYPE, kind);
        event.dataTransfer.effectAllowed = 'copy';
      }}
    >
      <span className="mg-comp-preview">
        <Icon name={item.icon} size={20} />
      </span>
      <span className="mg-comp-name">{name}</span>
    </button>
  );
}

function CollapsedHint() {
  const setMode = useEditorStore((s) => s.setComponentPanelMode);
  return (
    <button type="button" className="mg-comp-expand" onClick={() => setMode('expanded')}>
      {t.panelActions.expandComponents}
    </button>
  );
}

/**
 * 组件面板折叠成左上方的 32px 图标条：点图标把组件面板浮回左侧。
 * 作为 dockview 面板挂在组件组里（组宽锁定 32px、隐藏标签头）。
 */
export function ComponentStrip() {
  const commands = useShellCommands();
  const mode = useEditorStore((s) => s.componentPanelMode);
  const setMode = useEditorStore((s) => s.setComponentPanelMode);
  return (
    <div className="mg-strip">
      <button
        type="button"
        className={`mg-strip-btn${mode === 'expanded' ? ' mg-on' : ''}`}
        title={t.panelActions.expandComponents}
        aria-label={t.panelActions.expandComponents}
        onClick={() => {
          commands.setComponentsPanel('expanded');
          setMode('expanded');
        }}
      >
        <Icon name="components" size={18} />
      </button>
      {(['barcode', 'stamp', 'freight'] as const).map((category) => (
        <button
          key={category}
          type="button"
          className="mg-strip-btn"
          title={t.components.categories[category]}
          aria-label={t.components.categories[category]}
          onClick={() => {
            commands.setComponentsPanel('expanded');
            setMode('expanded');
          }}
        >
          <Icon
            name={category === 'barcode' ? 'barcode' : category === 'stamp' ? 'shape' : 'listView'}
            size={18}
          />
        </button>
      ))}
    </div>
  );
}
