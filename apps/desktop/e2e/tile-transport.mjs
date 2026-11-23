/**
 * 两条取瓦片的路比一比（V06 报告 11.7）：自定义协议 `magpdf://tile/…`（《接口协议》3.3）和 WebView2 的共享缓冲区
 * （`src-tauri/src/shared_tiles.rs`）。
 *
 * 起真实的桌面程序，打开共享缓冲区（`MAGPDF_TILE_TRANSPORT=shared-buffer`），在 WebView2 里用 `performance.now()`
 * 计时：先渲染几块瓦片，再对同一块瓦片两条路各取 `MAGPDF_TILE_FETCHES` 次（取一次的耗时），以及 8 块同时取。
 * 两条路取到的像素逐字节比一遍。另外量“收到 render.tileReady 到像素到手”：自定义协议、共享缓冲区调命令取、
 * 共享缓冲区直接读主进程随事件放好的，各 40 次，每次新渲染一块。
 *
 * 先构建发布版（步骤见 smoke.mjs 开头），再运行：
 *   node apps/desktop/e2e/tile-transport.mjs
 *
 * 环境变量：
 *   MAGPDF_DESKTOP_BIN    被测的可执行文件，默认 target/release/magpdf-desktop.exe；引擎要在同一个目录
 *   MAGPDF_E2E_PDF        打开的 PDF，默认 tests/corpus/synthetic/freight-bl-composite.pdf
 *   MAGPDF_TILE_FETCHES   每条路取几次，默认 200
 *   MAGPDF_TILE_ROUNDS    量几轮，默认 3
 *
 * 结果打印成一行 JSON（中位数、P95，单位 ms）。测试窗口不抢焦点，不动真实的鼠标键盘。
 */
import assert from 'node:assert/strict';
import { join, resolve } from 'node:path';
import { cleanUp, connect, launchApp, parseDuration, repo, tilesSettled } from './harness.mjs';

const appBin = resolve(
  process.env.MAGPDF_DESKTOP_BIN ?? join(repo, 'target/release/magpdf-desktop.exe'),
);
const pdf = resolve(
  process.env.MAGPDF_E2E_PDF ?? join(repo, 'tests/corpus/synthetic/freight-bl-composite.pdf'),
);
const FETCHES = parseDuration(process.env.MAGPDF_TILE_FETCHES, 200);
const ROUNDS = parseDuration(process.env.MAGPDF_TILE_ROUNDS, 3);

process.env.MAGPDF_TILE_TRANSPORT = 'shared-buffer';
const { app, port, scratch } = await launchApp(appBin, [pdf]);
let browser;
try {
  const connected = await connect(port);
  browser = connected.browser;
  const { page } = connected;
  await tilesSettled(page, 1);
  const transport = await page.evaluate(() => window.__magpdfTransport.tileTransport());
  assert.equal(transport, 'sharedBuffer', '共享缓冲区没有打开：WebView2 运行时可能太旧');

  const rounds = [];
  for (let round = 0; round < ROUNDS; round++) {
    rounds.push(await page.evaluate(measure, FETCHES));
  }
  console.log(JSON.stringify({ pdf: pdf.slice(repo.length + 1), fetches: FETCHES, rounds }));
} finally {
  await browser?.close().catch(() => undefined);
  app.kill();
  await cleanUp(scratch);
}

/** 在页面里跑：渲染 8 块瓦片，两条路各取若干次。 */
async function measure(fetches) {
  const transport = window.__magpdfTransport;
  const { invoke } = window.__TAURI_INTERNALS__;
  const [doc] = await invoke('open_documents');
  const tiles = Array.from({ length: 8 }, (_, i) => ({
    x: (i % 4) * 512,
    y: Math.floor(i / 4) * 512,
  }));
  const ready = new Map();
  const arrived = new Promise((resolve) => {
    const stop = transport.onEvent(({ method, params }) => {
      if (method !== 'render.tileReady') return;
      ready.set(params.requestId, params);
      if (ready.size === tiles.length) {
        stop();
        resolve();
      }
    });
  });
  const { requestIds } = await transport.request('render.tiles', {
    docId: doc.docId,
    pageId: 0,
    scale: 4,
    tiles,
    priority: 'visible',
  });
  await arrived;
  const slots = requestIds.map((id) => ready.get(id));

  const stats = (values) => {
    const sorted = [...values].sort((a, b) => a - b);
    const at = (q) => sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))];
    return { median: +at(0.5).toFixed(2), p95: +at(0.95).toFixed(2) };
  };
  const one = async (protocol) => {
    transport.forceProtocol = protocol;
    const { slot, generation } = slots[0];
    const times = [];
    for (let i = 0; i < fetches; i++) {
      const start = performance.now();
      await transport.fetchTile(slot, generation);
      times.push(performance.now() - start);
    }
    return stats(times);
  };
  const eight = async (protocol) => {
    transport.forceProtocol = protocol;
    const times = [];
    for (let i = 0; i < 20; i++) {
      const start = performance.now();
      await Promise.all(slots.map(({ slot, generation }) => transport.fetchTile(slot, generation)));
      times.push(performance.now() - start);
    }
    return stats(times);
  };

  // 两条路取到的像素要一样。
  const same = [];
  for (const { slot, generation } of slots) {
    transport.forceProtocol = true;
    const a = await transport.fetchTile(slot, generation);
    transport.forceProtocol = false;
    const b = await transport.fetchTile(slot, generation);
    same.push(a.length === b.length && a.every((v, i) => v === b[i]));
  }

  // 收到 render.tileReady 到像素到手：每次新渲染一块，事件一到就取。共享缓冲区分两种：主进程随事件放好的
  // 直接读（默认），和不用放好的、调命令取（ignorePushed）。
  const afterEvent = async (protocol, ignorePushed) => {
    transport.forceProtocol = protocol;
    transport.ignorePushed = ignorePushed;
    const times = [];
    let pushedHits = 0;
    for (let i = 0; i < 40; i++) {
      const got = new Promise((resolve, reject) => {
        const stop = transport.onEvent(({ method, params }) => {
          if (method !== 'render.tileReady') return;
          stop();
          if (params.sharedBuffer) pushedHits++;
          const start = performance.now();
          transport
            .fetchTile(params.slot, params.generation)
            .then(() => resolve(performance.now() - start), reject);
        });
      });
      await transport.request('render.tiles', {
        docId: doc.docId,
        pageId: 0,
        scale: 2 + (i % 4),
        tiles: [{ x: (i % 2) * 512, y: 0 }],
        priority: 'visible',
      });
      times.push(await got);
    }
    return { ...stats(times), pushedHits };
  };

  const result = {
    identical: same.every(Boolean),
    protocol: await one(true),
    sharedBuffer: await one(false),
    eightProtocol: await eight(true),
    eightSharedBuffer: await eight(false),
    afterEventProtocol: await afterEvent(true, false),
    afterEventSharedCommand: await afterEvent(false, true),
    afterEventSharedPushed: await afterEvent(false, false),
  };
  transport.forceProtocol = false;
  transport.ignorePushed = false;
  return result;
}
