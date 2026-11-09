import { useEffect, useRef, useState } from 'react';
import { Icon } from '@magpdf/ui';
import { t } from '../i18n';
import type { CommandId } from '../tools/shortcuts';
import { defaultShortcutMap } from '../tools/shortcuts';
import type { WorkspaceId } from '../workspace/workspaceTypes';
import { useShellCommands } from './shellContext';

interface MenuItem {
  readonly id: string;
  readonly label: string;
  readonly command?: CommandId;
  readonly action?: 'new' | 'newContract' | 'newBlob' | 'newLarge' | 'closeDocument';
  readonly workspace?: WorkspaceId;
  readonly checked?: boolean;
  readonly disabled?: boolean;
  readonly separator?: boolean;
}

interface Menu {
  readonly id: string;
  readonly label: string;
  readonly items: readonly MenuItem[];
}

export interface MenuBarProps {
  readonly workspace: WorkspaceId;
  readonly showRulers: boolean;
  readonly showFrameStats: boolean;
}

/** 菜单栏 28px：MagPDF 标志、8 个菜单、窗口按钮。P0 只有部分项接了命令，其余置灰。 */
export function MenuBar({ workspace, showRulers, showFrameStats }: MenuBarProps) {
  const commands = useShellCommands();
  const [open, setOpen] = useState<string | null>(null);
  const barRef = useRef<HTMLElement>(null);

  const menus: Menu[] = [
    {
      id: 'file',
      label: t.menu.file,
      items: [
        { id: 'new', label: t.menuItems.new, action: 'new' },
        { id: 'newContract', label: t.mock.contractName, action: 'newContract' },
        { id: 'newBlob', label: t.mock.billOfLadingName, action: 'newBlob' },
        { id: 'newLarge', label: t.status.largeDocument, action: 'newLarge' },
        { id: 'sep1', label: '', separator: true },
        { id: 'open', label: t.menuItems.open, command: 'file.open' },
        { id: 'close', label: t.menuItems.close, action: 'closeDocument', command: 'file.close' },
        { id: 'sep2', label: '', separator: true },
        { id: 'save', label: t.menuItems.save, disabled: true },
        { id: 'saveAs', label: t.menuItems.saveAs, disabled: true },
        { id: 'print', label: t.menuItems.print, disabled: true },
        { id: 'sep3', label: '', separator: true },
        { id: 'exit', label: t.menuItems.exit, disabled: true },
      ],
    },
    {
      id: 'edit',
      label: t.menu.edit,
      items: [
        { id: 'undo', label: t.menuItems.undo, disabled: true },
        { id: 'redo', label: t.menuItems.redo, disabled: true },
        { id: 'sep1', label: '', separator: true },
        { id: 'cut', label: t.menuItems.cut, disabled: true },
        { id: 'copy', label: t.menuItems.copy, disabled: true },
        { id: 'paste', label: t.menuItems.paste, disabled: true },
        { id: 'delete', label: t.menuItems.delete, command: 'edit.delete' },
        { id: 'sep2', label: '', separator: true },
        { id: 'selectAll', label: t.menuItems.selectAll, command: 'edit.selectAll' },
        { id: 'deselect', label: t.menuItems.deselect, command: 'edit.deselect' },
        { id: 'sep3', label: '', separator: true },
        { id: 'preferences', label: t.menuItems.preferences, disabled: true },
      ],
    },
    {
      id: 'object',
      label: t.menu.object,
      items: [
        { id: 'freeTransform', label: t.menuItems.freeTransform, disabled: true },
        { id: 'duplicate', label: t.menuItems.duplicate, disabled: true },
        { id: 'sep1', label: '', separator: true },
        { id: 'group', label: t.menuItems.group, disabled: true },
        { id: 'ungroup', label: t.menuItems.ungroup, disabled: true },
        { id: 'sep2', label: '', separator: true },
        { id: 'bringForward', label: t.menuItems.bringForward, disabled: true },
        { id: 'sendBackward', label: t.menuItems.sendBackward, disabled: true },
        { id: 'bringToFront', label: t.menuItems.bringToFront, disabled: true },
        { id: 'sendToBack', label: t.menuItems.sendToBack, disabled: true },
      ],
    },
    {
      id: 'text',
      label: t.menu.text,
      items: [
        { id: 'character', label: t.menuItems.character, disabled: true },
        { id: 'paragraph', label: t.menuItems.paragraph, disabled: true },
      ],
    },
    {
      id: 'page',
      label: t.menu.page,
      items: [
        { id: 'insertPage', label: t.menuItems.insertPage, disabled: true },
        { id: 'deletePage', label: t.menuItems.deletePage, disabled: true },
        { id: 'rotatePage', label: t.menuItems.rotatePage, disabled: true },
      ],
    },
    {
      id: 'view',
      label: t.menu.view,
      items: [
        { id: 'zoomIn', label: t.menuItems.zoomIn, command: 'view.zoomIn' },
        { id: 'zoomOut', label: t.menuItems.zoomOut, command: 'view.zoomOut' },
        { id: 'fitPage', label: t.menuItems.fitPage, command: 'view.fitPage' },
        { id: 'actualSize', label: t.menuItems.actualSize, command: 'view.actualSize' },
        { id: 'sep1', label: '', separator: true },
        { id: 'rulers', label: t.menuItems.rulers, command: 'view.toggleRulers' },
        { id: 'guides', label: t.menuItems.guides, disabled: true },
        { id: 'grid', label: t.menuItems.grid, disabled: true },
        { id: 'sep2', label: '', separator: true },
        { id: 'frameStats', label: t.menuItems.frameStats, command: 'window.toggleFrameStats' },
        { id: 'benchmark', label: t.menuItems.benchmark, command: 'window.benchmark' },
      ],
    },
    {
      id: 'window',
      label: t.menu.window,
      items: [
        { id: 'wsBasic', label: t.workspaces.basic, workspace: 'basic' },
        { id: 'wsFreight', label: t.workspaces.freight, workspace: 'freight' },
        { id: 'sep1', label: '', separator: true },
        {
          id: 'resetWorkspace',
          label: t.menuItems.resetWorkspace,
          command: 'window.resetWorkspace',
        },
        { id: 'togglePanels', label: t.menuItems.componentsPanel, command: 'window.togglePanels' },
      ],
    },
    {
      id: 'help',
      label: t.menu.help,
      items: [
        { id: 'manual', label: t.menuItems.manual, disabled: true },
        { id: 'about', label: t.menuItems.about, disabled: true },
      ],
    },
  ];

  useEffect(() => {
    if (!open) return;
    const onDown = (event: PointerEvent) => {
      if (barRef.current && !barRef.current.contains(event.target as Node)) setOpen(null);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setOpen(null);
    };
    window.addEventListener('pointerdown', onDown);
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('pointerdown', onDown);
      window.removeEventListener('keydown', onKey);
    };
  }, [open]);

  const run = (item: MenuItem) => {
    setOpen(null);
    if (item.disabled) return;
    if (item.workspace) {
      commands.applyWorkspace(item.workspace);
      return;
    }
    switch (item.action) {
      case 'new':
        commands.openDocument('blank');
        return;
      case 'newContract':
        commands.openDocument('contract');
        return;
      case 'newBlob':
        commands.openDocument('billOfLading');
        return;
      case 'newLarge':
        commands.openDocument('large');
        return;
      case 'closeDocument':
        commands.closeActiveDocument();
        return;
      default:
        break;
    }
    if (item.command) commands.run(item.command);
  };

  const shortcut = (item: MenuItem): string | null => {
    if (!item.command) return null;
    const combo = defaultShortcutMap.comboFor(item.command);
    if (!combo) return null;
    return combo
      .replace('Ctrl+', 'Ctrl+')
      .replace('Arrow', '↑')
      .replace('Left', '←')
      .replace('Right', '→')
      .replace('Up', '↑')
      .replace('Down', '↓');
  };

  const checked = (item: MenuItem): boolean => {
    if (item.workspace) return item.workspace === workspace;
    if (item.command === 'view.toggleRulers') return showRulers;
    if (item.command === 'window.toggleFrameStats') return showFrameStats;
    return false;
  };

  return (
    <header className="mg-menubar" ref={barRef}>
      <span className="mg-logo">{t.app.logo}</span>
      <nav className="mg-menus" aria-label={t.app.name}>
        {menus.map((menu) => (
          <div key={menu.id} className="mg-menu-wrap">
            <button
              type="button"
              className={`mg-menu${open === menu.id ? ' mg-open' : ''}`}
              aria-haspopup="menu"
              aria-expanded={open === menu.id}
              onClick={() => setOpen(open === menu.id ? null : menu.id)}
              onPointerEnter={() => {
                if (open) setOpen(menu.id);
              }}
            >
              {menu.label}
            </button>
            {open === menu.id ? (
              <div className="mg-menu-popup" role="menu">
                {menu.items.map((item) =>
                  item.separator ? (
                    <div key={item.id} className="mg-menu-sep" role="separator" />
                  ) : (
                    <button
                      key={item.id}
                      type="button"
                      role="menuitem"
                      className="mg-menu-item"
                      disabled={item.disabled}
                      onClick={() => run(item)}
                    >
                      <span className="mg-menu-check">
                        {checked(item) ? <Icon name="check" size={12} /> : null}
                      </span>
                      <span className="mg-menu-label">{item.label}</span>
                      <span className="mg-menu-key">{shortcut(item)}</span>
                    </button>
                  ),
                )}
              </div>
            ) : null}
          </div>
        ))}
      </nav>
      <div className="mg-title">{t.app.name}</div>
      <div className="mg-window-buttons">
        <button type="button" title={t.window.minimize} aria-label={t.window.minimize}>
          <Icon name="windowMinimize" size={12} />
        </button>
        <button type="button" title={t.window.maximize} aria-label={t.window.maximize}>
          <Icon name="windowMaximize" size={12} />
        </button>
        <button
          type="button"
          className="mg-window-close"
          title={t.window.close}
          aria-label={t.window.close}
        >
          <Icon name="windowClose" size={12} />
        </button>
      </div>
    </header>
  );
}
