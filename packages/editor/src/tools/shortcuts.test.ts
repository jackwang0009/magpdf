import { describe, expect, it } from 'vitest';
import { ShortcutMap, comboFromEvent, defaultShortcutMap, defaultShortcuts } from './shortcuts';
import type { KeyLike } from './shortcuts';
import { toolForKey, v1Tools } from './tools';

function key(value: string, mods: Partial<KeyLike> = {}): KeyLike {
  return { key: value, ctrlKey: false, shiftKey: false, altKey: false, ...mods };
}

describe('快捷键映射', () => {
  it('默认表本身没有冲突（构造时会检查）', () => {
    expect(() => new ShortcutMap(defaultShortcuts)).not.toThrow();
    expect(new ShortcutMap(defaultShortcuts).resolve(key('V'))).toBe('tool.select');
  });

  it('组合键规范化：+ / = 归并、字母大写、meta 视作 Ctrl', () => {
    expect(comboFromEvent(key('+', { ctrlKey: true, shiftKey: true }))).toBe('Ctrl+=');
    expect(comboFromEvent(key('=', { ctrlKey: true }))).toBe('Ctrl+=');
    expect(comboFromEvent(key('_', { ctrlKey: true, shiftKey: true }))).toBe('Ctrl+-');
    expect(comboFromEvent(key('z', { metaKey: true }))).toBe('Ctrl+Z');
    expect(comboFromEvent(key('ArrowLeft', { shiftKey: true }))).toBe('Shift+ArrowLeft');
    expect(comboFromEvent(key(' '))).toBe('Space');
  });

  it('方案 8.4 的视图命令与微移命令', () => {
    expect(defaultShortcutMap.resolve(key('0', { ctrlKey: true }))).toBe('view.fitPage');
    expect(defaultShortcutMap.resolve(key('1', { ctrlKey: true }))).toBe('view.actualSize');
    expect(defaultShortcutMap.resolve(key('+', { ctrlKey: true, shiftKey: true }))).toBe(
      'view.zoomIn',
    );
    expect(defaultShortcutMap.resolve(key('-', { ctrlKey: true }))).toBe('view.zoomOut');
    expect(defaultShortcutMap.resolve(key('r', { ctrlKey: true }))).toBe('view.toggleRulers');
    expect(defaultShortcutMap.resolve(key('ArrowRight'))).toBe('edit.nudgeRight');
    expect(defaultShortcutMap.resolve(key('ArrowDown', { shiftKey: true }))).toBe(
      'edit.nudgeDownLarge',
    );
    expect(defaultShortcutMap.resolve(key('Escape'))).toBe('edit.deselect');
    expect(defaultShortcutMap.resolve(key('Delete'))).toBe('edit.delete');
    expect(defaultShortcutMap.resolve(key('Tab'))).toBe('window.togglePanels');
  });

  it('v1.0 的 7 个工具键都能映射到工具命令', () => {
    for (const tool of v1Tools) {
      expect(defaultShortcutMap.resolve(key(tool.key.toLowerCase()))).toBe(`tool.${tool.id}`);
      expect(toolForKey(tool.key.toLowerCase())?.id).toBe(tool.id);
    }
  });

  it('未登记的按键返回 undefined；重复登记在构造时报错', () => {
    expect(defaultShortcutMap.resolve(key('F9'))).toBeUndefined();
    expect(
      () =>
        new ShortcutMap([
          { combo: 'Ctrl+K', command: 'view.zoomIn' },
          { combo: 'Ctrl+K', command: 'view.zoomOut' },
        ]),
    ).toThrow(/冲突/);
  });

  it('能反查命令的组合键（菜单右侧提示用）', () => {
    expect(defaultShortcutMap.comboFor('view.fitPage')).toBe('Ctrl+0');
    expect(defaultShortcutMap.comboFor('edit.nudgeLeftLarge')).toBe('Shift+ArrowLeft');
  });
});
