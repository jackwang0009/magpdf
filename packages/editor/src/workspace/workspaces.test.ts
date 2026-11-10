import { describe, expect, it } from 'vitest';
import { serializeWorkspace } from './workspaces';
import {
  builtinWorkspace,
  clampSideWidths,
  cleanDockviewLayout,
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

function leafById(
  root: unknown,
): Map<string, { type: string; data: Record<string, unknown>; size?: number }> {
  const map = new Map<string, { type: string; data: Record<string, unknown>; size?: number }>();
  const walk = (node: unknown): void => {
    const n = node as { type: string; data: unknown; size?: number };
    if (n.type === 'branch') (n.data as unknown[]).forEach(walk);
    else
      map.set(
        (n.data as { id: string }).id,
        n as { type: string; data: Record<string, unknown>; size?: number },
      );
  };
  walk(root);
  return map;
}

describe('工作区', () => {
  it('序列化后解析得到同样的布局（往返）', () => {
    for (const id of ['basic', 'freight'] as const) {
      const layout = builtinWorkspace(id, size);
      const parsed = parseWorkspace(serializeWorkspace(layout));
      expect(parsed).toEqual(layout);
    }
  });

  it('内置布局是版本 2，不含工具栏面板（工具栏已移到外壳顶部）', () => {
    for (const id of ['basic', 'freight'] as const) {
      const layout = builtinWorkspace(id, size);
      expect(layout.version).toBe(2);
      expect(Object.keys(layout.dockview.panels)).not.toContain('toolbar');
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

  it('清掉旧保存值里的文档视图和变空的非结构组，文档组保留，activeGroup 指回文档组', () => {
    const layout = builtinWorkspace('freight', size);
    // 模拟旧保存值：面板表里没有文档面板，但网格里还留着文档视图；文档区被拆出多余的组，
    // 另有一组只剩图标条视图；浮动组里也只剩文档视图；activeGroup 指向一个多余的组
    const json = JSON.parse(
      JSON.stringify({
        ...layout.dockview,
        floatingGroups: [{ data: { id: 'float-1', views: ['doc:a'] } }],
      }),
    ) as typeof layout.dockview;
    json.panels['doc:a'] = { id: 'doc:a', contentComponent: 'canvas', title: 'a.pdf' };
    const root = json.grid.root as { type: 'branch'; data: unknown[] };
    (
      root.data as { type: string; data: { id?: string; views: string[]; activeView?: string } }[]
    ).push(
      { type: 'leaf', data: { id: 'split-1', views: ['doc:a'], activeView: 'doc:a' } },
      { type: 'leaf', data: { id: 'split-2', views: ['strip'], activeView: 'strip' } },
    );
    json.activeGroup = 'split-1';
    const cleaned = cleanDockviewLayout(json);
    expect(JSON.stringify(cleaned)).not.toContain('doc:');
    const text = JSON.stringify(cleaned.grid.root);
    expect(text).not.toContain('split-1');
    expect(text).not.toContain('split-2');
    expect(text).toContain('"documents"');
    expect(cleaned.activeGroup).toBe('documents');
    expect(cleaned.panels['doc:a']).toBeUndefined();
    expect(cleaned.floatingGroups).toEqual([]);
  });

  it('内置布局本身清理后不变，清理两次结果相同', () => {
    const layout = builtinWorkspace('freight', size);
    const once = cleanDockviewLayout(layout.dockview);
    expect(once).toEqual(layout.dockview);
    expect(cleanDockviewLayout(once)).toEqual(once);
  });

  it('两侧宽度限制：内置布局里组件面板和右侧组都是 200–320，折叠的图标条保持 32', () => {
    const freight = builtinWorkspace('freight', size);
    const leaves = leafById(freight.dockview.grid.root);
    expect(leaves.get('components')?.data.constraints).toEqual({
      minimumWidth: 200,
      maximumWidth: 320,
    });
    expect(leaves.get('components')?.size).toBe(240);
    expect(leaves.get('right-top')?.data.constraints).toEqual({
      minimumWidth: 200,
      maximumWidth: 320,
    });
    expect(leaves.get('right-bottom')?.data.constraints).toEqual({
      minimumWidth: 200,
      maximumWidth: 320,
    });
    const basic = builtinWorkspace('basic', size);
    const basicLeaves = leafById(basic.dockview.grid.root);
    // 基础工作区里组件面板折叠成图标条，约束保持 32px 不动
    expect(basicLeaves.get('components')?.data.constraints).toEqual({
      minimumWidth: 32,
      maximumWidth: 32,
    });
    expect(basicLeaves.get('components')?.size).toBe(32);
    expect(basicLeaves.get('right')?.data.constraints).toEqual({
      minimumWidth: 200,
      maximumWidth: 320,
    });
    expect(basicLeaves.get('right')?.size).toBe(280);
    // 内置布局本身已经在范围内，夹取后不变
    for (const id of ['basic', 'freight'] as const) {
      const layout = builtinWorkspace(id, size);
      expect(clampSideWidths(layout.dockview)).toEqual(layout.dockview);
    }
  });

  it('旧保存值里过宽的两侧收进 200–320，高度不动，输入不改动', () => {
    const layout = builtinWorkspace('freight', size);
    const json = JSON.parse(JSON.stringify(layout.dockview)) as typeof layout.dockview;
    const root = json.grid.root as { type: string; data: unknown[] };
    const leaves = leafById(root);
    // 模拟旧保存值：组件面板被拖到 400，右侧被拖到 368
    leaves.get('components')!.size = 400;
    const rightBranch = (root.data as unknown[]).find(
      (c) => (c as { type: string }).type === 'branch' && leafById(c).has('right-top'),
    ) as { size?: number };
    rightBranch.size = 368;
    const rightTopHeight = leaves.get('right-top')?.size;
    const clamped = clampSideWidths(json);
    const after = leafById(clamped.grid.root);
    expect(after.get('components')?.size).toBe(320);
    expect(after.get('components')?.data.constraints).toEqual({
      minimumWidth: 200,
      maximumWidth: 320,
    });
    const afterBranch = (clamped.grid.root as { data: unknown[] }).data.find(
      (c) => (c as { type: string }).type === 'branch' && leafById(c).has('right-top'),
    ) as { size?: number };
    expect(afterBranch.size).toBe(320);
    // 右上/右下两组的高度不动，只补上宽度约束
    expect(after.get('right-top')?.size).toBe(rightTopHeight);
    expect(after.get('right-top')?.data.constraints).toEqual({
      minimumWidth: 200,
      maximumWidth: 320,
    });
    expect(after.get('right-bottom')?.data.constraints).toEqual({
      minimumWidth: 200,
      maximumWidth: 320,
    });
    // 输入不改动，夹取幂等
    expect(leafById(json.grid.root).get('components')?.size).toBe(400);
    expect(clampSideWidths(clamped)).toEqual(clamped);
  });

  it('损坏或旧版本的数据解析为 null，不抛错', () => {
    expect(parseWorkspace('not json')).toBeNull();
    expect(parseWorkspace('{"version":3}')).toBeNull();
    // 布局版本 2 之前（含浮动工具栏的 v1）也不再接受，自动回落到内置布局
    expect(parseWorkspace('{"version":1}')).toBeNull();
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
