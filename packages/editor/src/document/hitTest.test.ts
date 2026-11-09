import { describe, expect, it } from 'vitest';
import { linearHitTester } from './hitTest';
import type { PageObject, PageSnapshot } from './types';
import { A4 } from './types';

function rectObject(
  id: string,
  x: number,
  y: number,
  w: number,
  h: number,
  locked = false,
): PageObject {
  return {
    id,
    type: 'component',
    name: id,
    bbox: { x, y, width: w, height: h },
    appearance: { kind: 'rect', fill: '#000' },
    ...(locked ? { locked: true } : {}),
  };
}

const page: PageSnapshot = {
  index: 0,
  width: A4.width,
  height: A4.height,
  objects: [
    rectObject('bottom', 10, 10, 100, 100),
    rectObject('middle', 50, 50, 100, 100),
    rectObject('top', 80, 80, 100, 100),
    rectObject('locked', 300, 300, 50, 50, true),
  ],
};

describe('命中测试', () => {
  it('点命中取最上层（数组末尾）的可选对象', () => {
    // (90, 90) 落在三个对象里，最上层是 top
    expect(linearHitTester.hitPoint(page, { x: 90, y: 90 }, 0)?.id).toBe('top');
    // 只有 middle 与 bottom 覆盖的位置
    expect(linearHitTester.hitPoint(page, { x: 60, y: 60 }, 0)?.id).toBe('middle');
    expect(linearHitTester.hitPoint(page, { x: 15, y: 15 }, 0)?.id).toBe('bottom');
  });

  it('点不中时返回 null，容差能扩大命中范围', () => {
    expect(linearHitTester.hitPoint(page, { x: 250, y: 20 }, 0)).toBeNull();
    expect(linearHitTester.hitPoint(page, { x: 250, y: 20 }, 150)?.id).toBe('top');
  });

  it('锁定对象不参与选中', () => {
    expect(linearHitTester.hitPoint(page, { x: 320, y: 320 }, 0)).toBeNull();
    expect(
      linearHitTester.hitRect(page, { x: 0, y: 0, width: 1000, height: 1000 }, 'intersect'),
    ).not.toContainEqual(expect.objectContaining({ id: 'locked' }));
  });

  it('框选：相交模式与完全包含模式', () => {
    const partial = { x: 0, y: 0, width: 60, height: 60 };
    expect(linearHitTester.hitRect(page, partial, 'intersect').map((o) => o.id)).toEqual([
      'bottom',
      'middle',
    ]);
    expect(linearHitTester.hitRect(page, partial, 'contain').map((o) => o.id)).toEqual([]);
    const big = { x: 0, y: 0, width: 200, height: 200 };
    expect(linearHitTester.hitRect(page, big, 'contain').map((o) => o.id)).toEqual([
      'bottom',
      'middle',
      'top',
    ]);
  });

  it('对象堆叠查询从上到下', () => {
    const stack = linearHitTester.objectsAtPoint(page, { x: 90, y: 90 }, 0);
    expect(stack.map((o) => o.id)).toEqual(['top', 'middle', 'bottom']);
  });
});
