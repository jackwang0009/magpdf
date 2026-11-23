/** 调整层级的方式（方案 4.1，《接口协议》6.5 `edit.zOrder`）。 */
export type ZOrderOp = 'forward' | 'backward' | 'front' | 'back';

/**
 * 菜单里的四种挪法，或者放到第 `index` 层（图层面板里拖动）：没选中的对象从下往上数，选中的几个
 * 放在第 `index` 个之前，按原来的先后挨着放；超过个数就放在最上面。
 */
export type ZOrderTarget = ZOrderOp | { readonly index: number };

/**
 * 一页从下到上的对象 `order` 里，把 `selected` 这几个按 `op` 挪过之后的顺序。规则和引擎一样：
 * 没选中的对象之间先后不变；上移、下移一层时挨着的几个选中对象一起动，已经到头的不动。
 */
export function arrangeZOrder<T>(
  order: readonly T[],
  selected: ReadonlySet<T>,
  op: ZOrderTarget,
): T[] {
  const chosen = order.filter((item) => selected.has(item));
  const others = order.filter((item) => !selected.has(item));
  if (typeof op === 'object') {
    const at = Math.max(0, Math.min(op.index, others.length));
    return [...others.slice(0, at), ...chosen, ...others.slice(at)];
  }
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
