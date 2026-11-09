import { describe, expect, it } from 'vitest';
import { A4 } from './types';
import { PageLayout } from './pageLayout';

const pages = (n: number) => Array.from({ length: n }, () => A4);

describe('页面布局', () => {
  it('连续纵向排列，页面水平居中，留白计入总量', () => {
    const layout = new PageLayout([A4, { width: 400, height: 600 }], { gap: 10, margin: 20 });
    const first = layout.pageRect(0);
    const second = layout.pageRect(1);
    expect(first.y).toBe(20);
    expect(second.y).toBe(20 + A4.height + 10);
    // 窄页在宽页下面居中
    expect(second.x).toBe(20 + (A4.width - 400) / 2);
    expect(layout.totalHeight).toBe(20 + A4.height + 10 + 600 + 20);
    expect(layout.totalWidth).toBe(A4.width + 40);
  });

  it('可见页用二分查找，只返回相交的页（300 页）', () => {
    const layout = new PageLayout(pages(300));
    const y = layout.pageRect(150).y;
    const [first, last] = layout.visiblePages(y - 5, y + 5);
    expect(first).toBe(150);
    expect(last).toBe(151);
    // 跨两页的视口
    const [a, b] = layout.visiblePages(y - 5, layout.pageRect(151).y + 5);
    expect(a).toBe(150);
    expect(b).toBe(152);
  });

  it('完全在留白或间距里时没有可见页', () => {
    const layout = new PageLayout(pages(3));
    expect(layout.visiblePages(0, 10)).toEqual([0, 0]);
    expect(layout.visiblePages(layout.totalHeight, layout.totalHeight + 10)).toEqual([3, 3]);
  });

  it('点命中：页内返回页号与页内坐标，页外返回 null', () => {
    const layout = new PageLayout(pages(2));
    const rect = layout.pageRect(1);
    const hit = layout.pageAtPoint({ x: rect.x + 10, y: rect.y + 20 });
    expect(hit).toEqual({ index: 1, local: { x: 10, y: 20 } });
    expect(layout.pageAtPoint({ x: -100, y: rect.y + 20 })).toBeNull();
  });

  it('当前页取相交面积最大的页', () => {
    const layout = new PageLayout(pages(3));
    const y = layout.pageRect(1).y;
    // 大部分在第二页
    expect(layout.dominantPage(y - 10, y + A4.height - 10)).toBe(1);
    expect(layout.dominantPage(layout.pageRect(2).y, layout.pageRect(2).y + 100)).toBe(2);
  });

  it('空文档不崩', () => {
    const layout = new PageLayout([]);
    expect(layout.pageCount).toBe(0);
    expect(layout.visiblePages(0, 100)).toEqual([0, 0]);
    expect(layout.dominantPage(0, 100)).toBe(-1);
    expect(() => layout.pageRect(0)).toThrow(RangeError);
  });
});
