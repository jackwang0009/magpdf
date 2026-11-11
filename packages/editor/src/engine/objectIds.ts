/**
 * 界面里的对象 id 和引擎的对象编号之间的换算。
 *
 * 引擎打开的文档，界面里的对象 id 是 `p{页序号}-{引擎编号}`：带页序号是为了按 id 找对象时不用逐页找。
 * 模拟文档的 id 是别的样子，这里的函数对它们返回空。
 */

/** 界面里的对象 id：页序号加引擎的对象编号。 */
export function objectId(page: number, engineId: number): string {
  return `p${page}-${engineId}`;
}

/** 对象 id 里的页序号；不是引擎对象的 id 时返回 null。 */
export function pageOfObject(id: string): number | null {
  const match = /^p(\d+)-\d+$/.exec(id);
  return match ? Number(match[1]) : null;
}

/** 这些对象在引擎里的编号；不是引擎对象的 id 略过。 */
export function engineIds(ids: readonly string[]): number[] {
  const out: number[] = [];
  for (const id of ids) {
    const match = /^p\d+-(\d+)$/.exec(id);
    if (match) out.push(Number(match[1]));
  }
  return out;
}
