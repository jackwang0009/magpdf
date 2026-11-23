import { describe, expect, it } from 'vitest';
import { t } from '../i18n';
import { hasFrozenObject } from '../tools/machine';
import type { ObjectRef } from '../tools/machine';
import { COMPONENT_DEFAULTS, checkComponent, mockComponentObject } from './components';
import type { PageObject, PageSnapshot } from './types';

describe('组件参数的检查（和引擎 component.rs 一致）', () => {
  it('默认参数能生成', () => {
    expect(checkComponent('text', COMPONENT_DEFAULTS.text)).toBeNull();
    expect(checkComponent('code128', COMPONENT_DEFAULTS.code128)).toBeNull();
  });

  it('内容：不能空，只认 ASCII 32–126，条码最多 48 个字符', () => {
    expect(checkComponent('text', { content: '', size: 12 })).toEqual({
      key: 'content',
      message: t.componentChecks.empty,
    });
    expect(checkComponent('text', { content: '提单', size: 12 })?.message).toBe(
      t.componentChecks.textAsciiOnly,
    );
    expect(checkComponent('code128', { content: 'BL\t1', size: 36 })?.message).toBe(
      t.componentChecks.code128AsciiOnly,
    );
    expect(checkComponent('code128', { content: '9'.repeat(48), size: 36 })).toBeNull();
    expect(checkComponent('code128', { content: '9'.repeat(49), size: 36 })?.key).toBe('content');
  });

  it('尺寸：文本字号 4–200 pt，条高 10–300 pt', () => {
    expect(checkComponent('text', { content: 'a', size: 3 })).toEqual({
      key: 'size',
      message: '字号要在 4–200 pt 之间',
    });
    expect(checkComponent('code128', { content: 'a', size: 5 })?.message).toBe(
      '条高要在 10–300 pt 之间',
    );
    expect(checkComponent('code128', { content: 'a', size: Number.NaN })?.key).toBe('size');
    expect(checkComponent('code128', { content: 'a', size: 300 })).toBeNull();
  });
});

describe('只能叠加内容的页上，新加的组件可以改', () => {
  const original: PageObject = {
    id: 'p0-1',
    type: 'path',
    name: 'a',
    bbox: { x: 0, y: 0, width: 10, height: 10 },
  };
  const placed = mockComponentObject('p0-2', 'code128', COMPONENT_DEFAULTS.code128, {
    x: 20,
    y: 20,
  });
  const page: PageSnapshot = {
    index: 0,
    width: 100,
    height: 100,
    objects: [original, placed],
    overlayOnly: { reason: '测试' },
  };
  const ctx = {
    pages: [page],
    objectById: (id: string): ObjectRef | undefined => {
      const object = page.objects.find((o) => o.id === id);
      return object ? { page: 0, object, docRect: object.bbox } : undefined;
    },
  };

  it('原有的对象冻结；新加的不冻结；两个一起选还是冻结', () => {
    expect(hasFrozenObject(ctx, ['p0-1'])).toBe(true);
    expect(hasFrozenObject(ctx, ['p0-2'])).toBe(false);
    expect(hasFrozenObject(ctx, ['p0-2', 'p0-1'])).toBe(true);
  });
});
