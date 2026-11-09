import { describe, expect, it } from 'vitest';

import { toolForKey, tools, v1Tools } from './index';

describe('tools', () => {
  it('快捷键互不重复', () => {
    const keys = tools.map((t) => t.key);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it('v1.0 工具栏是方案 8.2 列出的 7 个', () => {
    expect(v1Tools.map((t) => t.key)).toEqual(['V', 'T', 'U', 'I', 'C', 'H', 'Z']);
  });

  it('按键查找不区分大小写', () => {
    expect(toolForKey('v')?.id).toBe('select');
    expect(toolForKey('X')).toBeUndefined();
  });
});
