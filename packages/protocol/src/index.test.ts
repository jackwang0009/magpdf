import { describe, expect, it } from 'vitest';

import {
  ERROR_CODE,
  EVENT,
  METHOD,
  PROTOCOL_VERSION,
  asDocumentVersion,
  isCompatibleProtocol,
} from './index';
import type { Request, RpcError, TileReady } from './index';

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

describe('从 Rust 生成的常量', () => {
  it('方法名和事件名是《接口协议》里的写法', () => {
    expect(METHOD.SESSION_HELLO).toBe('session.hello');
    expect(METHOD.RENDER_TILES).toBe('render.tiles');
    expect(EVENT.RENDER_TILE_READY).toBe('render.tileReady');
    expect(EVENT.ENGINE_RESTORED).toBe('engine.restored');
  });

  it('名字和错误码都不重复', () => {
    for (const table of [METHOD, EVENT, ERROR_CODE]) {
      const values = Object.values(table);
      expect(new Set(values).size).toBe(values.length);
    }
    expect(ERROR_CODE.E_PROTOCOL).toBe(1001);
    expect(ERROR_CODE.E_INTERNAL).toBe(9001);
  });
});

describe('从 Rust 生成的类型', () => {
  it('请求的 id 和事件里的整数都是 number，JSON 往返后不变', () => {
    const request: Request = {
      jsonrpc: '2.0',
      id: 17,
      method: METHOD.SESSION_PING,
      params: { t: 42 },
    };
    expect(JSON.parse(JSON.stringify(request))).toEqual(request);

    const ready: TileReady = {
      requestId: 1,
      docId: 1,
      pageId: 0,
      scale: 1.5,
      x: 512,
      y: 0,
      version: 0,
      slot: 3,
      generation: 2,
    };
    expect(JSON.parse(JSON.stringify(ready))).toEqual(ready);

    const error: RpcError = { code: ERROR_CODE.E_INVALID_ARGUMENT, message: '参数不合法' };
    expect(error.code).toBe(3005);
  });
});
