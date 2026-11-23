/** 调整层级的方式（方案 4.1，《接口协议》6.5 `edit.zOrder`）。 */
export type ZOrderOp = 'forward' | 'backward' | 'front' | 'back';

/**
 * 一页从下到上的对象 `order` 里，把 `selected` 这几个按 `op` 挪过之后的顺序。规则和引擎一样：
 * 没选中的对象之间先后不变；上移、下移一层时挨着的几个选中对象一起动，已经到头的不动。
 */
export function arrangeZOrder<T>(order: readonly T[], selected: ReadonlySet<T>, op: ZOrderOp): T[] {
  const chosen = order.filter((item) => selected.has(item));
  const others = order.filter((item) => !selected.has(item));
  switch (op) {
    case 'front':
      return [...others, ...chosen];
    case 'back':
      return [...chosen, ...others];
    case 'forward': {
      const arranged = [...order];
      for (let i = arranged.length - 2; i >= 0; i--) {
        if (selected.has(arranged[i]!) && !selected.has(arranged[i + 1]!)) {
          [arranged[i], arranged[i + 1]] = [arranged[i + 1]!, arranged[i]!];
        }
      }
      return arranged;
    }
    case 'backward': {
      const arranged = [...order];
      for (let i = 1; i < arranged.length; i++) {
        if (selected.has(arranged[i]!) && !selected.has(arranged[i - 1]!)) {
          [arranged[i], arranged[i - 1]] = [arranged[i - 1]!, arranged[i]!];
        }
      }
      return arranged;
    }
  }
}
