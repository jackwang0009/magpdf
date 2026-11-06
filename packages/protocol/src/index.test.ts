import { describe, expect, it } from 'vitest';

import { PROTOCOL_VERSION, asDocumentVersion, isCompatibleProtocol } from './index';

describe('protocol', () => {
  it('只接受相同的协议版本', () => {
    expect(isCompatibleProtocol(PROTOCOL_VERSION)).toBe(true);
    expect(isCompatibleProtocol(PROTOCOL_VERSION + 1)).toBe(false);
  });

  it('文档版本号必须是非负整数', () => {
    expect(asDocumentVersion(3)).toBe(3);
    expect(() => asDocumentVersion(-1)).toThrow(RangeError);
    expect(() => asDocumentVersion(1.5)).toThrow(RangeError);
  });
});
