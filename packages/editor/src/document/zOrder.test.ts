import { describe, expect, it } from 'vitest';
import { arrangeZOrder } from './zOrder';

describe('arrangeZOrder：和引擎的规则一样', () => {
  const order = [1, 2, 3, 4, 5];
  const arrange = (op: Parameters<typeof arrangeZOrder>[2], picked: number[]) =>
    arrangeZOrder(order, new Set(picked), op);

  it('置于顶层、置于底层：选中的按原来的先后挨着放', () => {
    expect(arrange('front', [1, 3])).toEqual([2, 4, 5, 1, 3]);
    expect(arrange('back', [2, 5])).toEqual([2, 5, 1, 3, 4]);
  });

  it('上移、下移一层：挨着的几个一起动，已经到头的不动', () => {
    expect(arrange('forward', [1, 2])).toEqual([3, 1, 2, 4, 5]);
    expect(arrange('forward', [3, 5])).toEqual([1, 2, 4, 3, 5]);
    expect(arrange('backward', [4, 5])).toEqual([1, 2, 4, 5, 3]);
    expect(arrange('backward', [1, 3])).toEqual([1, 3, 2, 4, 5]);
  });

  it('放到第 index 层：数的是没选中的对象，超过个数放在最上面', () => {
    expect(arrange({ index: 1 }, [4, 5])).toEqual([1, 4, 5, 2, 3]);
    expect(arrange({ index: 0 }, [3])).toEqual([3, 1, 2, 4, 5]);
    expect(arrange({ index: 99 }, [1])).toEqual([2, 3, 4, 5, 1]);
  });
});
