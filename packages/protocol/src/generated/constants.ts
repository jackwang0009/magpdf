// 本文件由 magpdf-ipc 的测试 export_protocol_constants 生成（cargo test -p magpdf-ipc），不要手工修改。

/** 协议版本：不兼容修改时加 1（《接口协议》第 4 节）。 */
export const PROTOCOL_VERSION = 1 as const;

/** 方法名（《接口协议》第 4、6 节，以及 V06 原型的测量方法）。 */
export const METHOD = {
  SESSION_HELLO: 'session.hello',
  SESSION_PING: 'session.ping',
  SESSION_SHUTDOWN: 'session.shutdown',
  DOC_OPEN: 'doc.open',
  DOC_CLOSE: 'doc.close',
  DOC_SNAPSHOT: 'doc.snapshot',
  OBJ_LIST: 'obj.list',
  OBJ_HIT_TEST: 'obj.hitTest',
  EDIT_TRANSFORM: 'edit.transform',
  EDIT_SET_STYLE: 'edit.setStyle',
  EDIT_DELETE: 'edit.delete',
  HISTORY_UNDO: 'history.undo',
  HISTORY_REDO: 'history.redo',
  RENDER_TILES: 'render.tiles',
  RENDER_CANCEL: 'render.cancel',
  TILE_FETCH: 'tile.fetch',
  BENCH_ECHO: 'bench.echo',
  BENCH_ROUNDTRIP: 'bench.roundtrip',
  BENCH_ENGINE: 'bench.engine',
} as const;

/** 事件名（《接口协议》第 7 节）。 */
export const EVENT = {
  DOC_CHANGED: 'doc.changed',
  RENDER_TILE_READY: 'render.tileReady',
  ENGINE_BUSY: 'engine.busy',
  ENGINE_RESTARTING: 'engine.restarting',
  ENGINE_RESTORED: 'engine.restored',
} as const;

/** 错误码（《接口协议》第 8 节）。 */
export const ERROR_CODE = {
  E_PROTOCOL: 1001,
  E_ENGINE_RESTARTED: 1002,
  E_CANCELLED: 1003,
  E_FILE_NOT_FOUND: 2001,
  E_PASSWORD_REQUIRED: 2002,
  E_PASSWORD_WRONG: 2003,
  E_FILE_DAMAGED: 2004,
  E_UNSUPPORTED: 2005,
  E_VERSION_CONFLICT: 3001,
  E_OBJECT_NOT_FOUND: 3002,
  E_PAGE_OVERLAY_ONLY: 3004,
  E_INVALID_ARGUMENT: 3005,
  E_TILE_GENERATION_STALE: 3006,
  E_INTERNAL: 9001,
} as const;
