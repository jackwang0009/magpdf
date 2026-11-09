import { describe, expect, it } from 'vitest';
import { serializeWorkspace } from './workspaces';
import {
  builtinWorkspace,
  loadCurrentWorkspaceId,
  loadWorkspace,
  parseWorkspace,
  resetWorkspace,
  saveCurrentWorkspaceId,
  saveWorkspace,
  stripDocumentPanels,
} from './workspaces';
import type { WorkspaceStorage } from './workspaces';
import { PANEL_IDS, isPanelId } from './workspaceTypes';

const size = { width: 1440, height: 900 };

function memoryStorage(): WorkspaceStorage & { map: Map<string, string> } {
  const map = new Map<string, string>();
  return {
    map,
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => void map.set(k, v),
    removeItem: (k) => void map.delete(k),
  };
}

describe('工作区', () => {
  it('序列化后解析得到同样的布局（往返）', () => {
    for (const id of ['basic', 'freight'] as const) {
      const layout = builtinWorkspace(id, size);
      const parsed = parseWorkspace(serializeWorkspace(layout));
      expect(parsed).toEqual(layout);
    }
  });

  it('两个内置工作区的面板齐全；货代单证默认组件面板展开', () => {
    const freight = builtinWorkspace('freight', size);
    expect(freight.componentPanelMode).toBe('expanded');
    expect(freight.sideWidths).toEqual({ components: 240, rightDock: 280 });
    expect(Object.keys(freight.dockview.panels)).toEqual(
      expect.arrayContaining(['components', 'properties', 'layers', 'pages', 'history']),
    );
    const basic = builtinWorkspace('basic', size);
    expect(basic.componentPanelMode).toBe('collapsed');
    // 面板组件名都认识
    for (const panel of Object.values(freight.dockview.panels)) {
      expect(isPanelId(panel.contentComponent ?? '')).toBe(true);
    }
  });

  it('树里只出现已知面板，文档组为空（文档不随工作区保存）', () => {
    const layout = builtinWorkspace('freight', size);
    const root = layout.dockview.grid.root;
    const leaves: string[] = [];
    const walk = (node: typeof root) => {
      if (node.type === 'branch') (node.data as (typeof root)[]).forEach(walk);
      else leaves.push((node.data as { views: string[] }).views.join(','));
    };
    walk(root);
    expect(leaves).toContain('');
    for (const view of leaves.join(',').split(',').filter(Boolean)) {
      expect(PANEL_IDS).toContain(view);
    }
  });

  it('去掉文档面板：网格、面板表、浮动组都要清掉', () => {
    const layout = builtinWorkspace('freight', size);
    const json = JSON.parse(JSON.stringify(layout.dockview)) as typeof layout.dockview;
    json.panels['doc:a'] = { id: 'doc:a', contentComponent: 'canvas', title: 'a.pdf' };
    const root = json.grid.root as { type: 'branch'; data: unknown[] };
    (
      root.data as { type: string; data: { id?: string; views: string[]; activeView?: string } }[]
    ).push({
      type: 'leaf',
      data: { id: 'documents', views: ['doc:a'], activeView: 'doc:a' },
    });
    const stripped = stripDocumentPanels(json);
    expect(stripped.panels['doc:a']).toBeUndefined();
    const text = JSON.stringify(stripped.grid.root);
    expect(text).not.toContain('doc:a');
  });

  it('损坏或旧版本的数据解析为 null，不抛错', () => {
    expect(parseWorkspace('not json')).toBeNull();
    expect(parseWorkspace('{"version":2}')).toBeNull();
    const layout = builtinWorkspace('basic', size);
    const bad = JSON.parse(serializeWorkspace(layout)) as {
      dockview: { panels: Record<string, { contentComponent?: string }> };
    };
    bad.dockview.panels['properties'] = { contentComponent: 'unknown-panel' };
    expect(parseWorkspace(JSON.stringify(bad))).toBeNull();
  });

  it('按工作区 id 存取，复位清除保存值', () => {
    const storage = memoryStorage();
    expect(loadWorkspace(storage, 'freight')).toBeNull();
    const layout = builtinWorkspace('freight', size);
    saveWorkspace(storage, layout);
    expect(loadWorkspace(storage, 'freight')).toEqual(layout);
    saveCurrentWorkspaceId(storage, 'basic');
    expect(loadCurrentWorkspaceId(storage)).toBe('basic');
    resetWorkspace(storage, 'freight');
    expect(loadWorkspace(storage, 'freight')).toBeNull();
  });

  it('没有保存过时默认「货代单证」', () => {
    expect(loadCurrentWorkspaceId(memoryStorage())).toBe('freight');
  });
});
