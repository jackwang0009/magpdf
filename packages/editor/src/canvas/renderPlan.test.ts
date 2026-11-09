import { describe, expect, it } from 'vitest';
import { PageLayout } from '../document/pageLayout';
import { A4 } from '../document/types';
import { computeRenderPlan } from './renderPlan';
import { pickLevel, tileKeyString } from './tiles';

const layout = new PageLayout(Array.from({ length: 300 }, () => A4));
const viewport = { width: 1200, height: 900 };

function planAt(zoom: number, y: number, has: (id: string) => boolean = () => false) {
  return computeRenderPlan({ layout, view: { zoom, x: 0, y }, viewport, devicePixelRatio: 1, has });
}

describe('每帧渲染计划', () => {
  it('只规划可见与预取范围内的页（300 页里每次只涉及少数几页）', () => {
    const y = layout.pageRect(120).y;
    const plan = planAt(1, y);
    const pageNumbers = new Set(plan.missing.map((m) => m.key.page));
    expect(pageNumbers.size).toBeLessThanOrEqual(4);
    expect(pageNumbers.has(120)).toBe(true);
    expect(pageNumbers.has(0)).toBe(false);
  });

  it('缩放档跟显示比例走，缺的瓦片按离视口中心由近到远排序', () => {
    const plan = planAt(1, layout.pageRect(0).y);
    expect(plan.targetLevel).toBe(pickLevel(1));
    expect(plan.missing.length).toBeGreaterThan(0);
    for (let i = 1; i < plan.missing.length; i++) {
      expect(plan.missing[i]!.priority).toBeGreaterThanOrEqual(plan.missing[i - 1]!.priority);
    }
    // 预取优先级不低于缺失
    if (plan.prefetch.length > 0) {
      expect(plan.prefetch[0]!.priority).toBeGreaterThanOrEqual(plan.missing.at(-1)!.priority);
    }
  });

  it('已缓存的瓦片进入绘制列表，不再请求', () => {
    const first = planAt(1, layout.pageRect(0).y);
    const cache = new Set(first.missing.map((m) => m.id));
    const second = planAt(1, layout.pageRect(0).y, (id) => cache.has(id));
    expect(second.tiles.length).toBeGreaterThan(0);
    expect(second.missing.filter((m) => cache.has(m.id))).toHaveLength(0);
  });

  it('目标档位缺失时用更粗一档已缓存的瓦片顶上（先粗后细）', () => {
    const y = layout.pageRect(0).y;
    const plan = planAt(1, y, (id) => id.startsWith('0/0/'));
    expect(plan.targetLevel).toBe(pickLevel(1));
    expect(plan.missing.length).toBeGreaterThan(0);
    expect(plan.fallback.length).toBeGreaterThan(0);
    for (const quad of plan.fallback) {
      expect(tileKeyString(quad.key).startsWith('0/0/')).toBe(true);
    }
  });

  it('页面底色只含与视口相交的页', () => {
    const y = layout.pageRect(10).y;
    const plan = planAt(0.5, y);
    expect(plan.pages.every((p) => p.page >= 10 && p.page <= 12)).toBe(true);
  });
});
