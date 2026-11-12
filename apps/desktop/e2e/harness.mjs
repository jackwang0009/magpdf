/**
 * 端到端测试和性能测量共用的一套：起桌面程序、经 CDP 连进 WebView2、等画面到齐、挑对象、量修改的延迟。
 * 用的人：`smoke.mjs`（冒烟测试）、`perf.mjs`（V05 性能测量）。
 */
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

export const repo = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');

/**
 * 向系统要一个空闲的端口。系统的套接字缓冲区一时用光（ENOBUFS，机器上别的程序占着大量端口时会碰到）
 * 就等一会儿再要，不让它算成测试失败。
 */
export async function freePort() {
  for (let attempt = 0; ; attempt++) {
    try {
      return await new Promise((done, fail) => {
        const server = createServer();
        server.once('error', fail);
        server.listen(0, '127.0.0.1', () => {
          const { port } = server.address();
          server.close(() => done(port));
        });
      });
    } catch (error) {
      if (error.code !== 'ENOBUFS' || attempt >= 5) throw error;
      await sleep(500);
    }
  }
}

export function sleep(ms) {
  return new Promise((done) => setTimeout(done, ms));
}

/** 毫秒数（或别的正整数）；没有给时用 `fallback`，写错了直接报错。 */
export function parseDuration(text, fallback) {
  if (text === undefined || text === '') return fallback;
  assert.match(text, /^[1-9]\d*$/, `要写成正整数，现在是 ${text}`);
  return Number(text);
}

/** “宽x高” → `{ width, height }`；没有给时返回 null，写错了直接报错，不悄悄用默认值。 */
export function parseWindowSize(text, name = 'MAGPDF_E2E_WINDOW') {
  if (text === undefined || text === '') return null;
  const match = /^(\d+)x(\d+)$/.exec(text);
  assert.ok(match, `${name} 要写成 宽x高（比如 1920x1040），现在是 ${text}`);
  return { width: Number(match[1]), height: Number(match[2]) };
}

export function isRunning(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

export async function waitFor(what, check, timeoutMs, intervalMs = 50) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await check();
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`等了 ${timeoutMs} ms：${what}`);
    await sleep(intervalMs);
  }
}

/** 主进程名下的引擎进程。 */
export function enginePid(appPid) {
  const out = execFileSync(
    'powershell.exe',
    [
      '-NoProfile',
      '-Command',
      `(Get-CimInstance Win32_Process -Filter "ParentProcessId=${appPid} AND Name='magpdf-engine.exe'").ProcessId`,
    ],
    { encoding: 'utf8' },
  ).trim();
  const pid = Number.parseInt(out, 10);
  return Number.isInteger(pid) ? pid : null;
}

/** 把主进程的窗口挪到屏幕左上角、改成指定的大小（屏幕像素）。不激活窗口，不改它和别的窗口的前后次序。 */
export function resizeWindow(appPid, { width, height }) {
  const SWP_NOZORDER_NOACTIVATE = 0x0014;
  execFileSync('powershell.exe', [
    '-NoProfile',
    '-Command',
    `$api = Add-Type -PassThru -Namespace MagpdfE2e -Name Window -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y, int width, int height, uint flags);';` +
      ` $window = (Get-Process -Id ${appPid}).MainWindowHandle;` +
      ` if ($window -eq [IntPtr]::Zero) { exit 2 };` +
      ` if (-not $api::SetWindowPos($window, [IntPtr]::Zero, 0, 0, ${width}, ${height}, ${SWP_NOZORDER_NOACTIVATE})) { exit 3 }`,
  ]);
}

/**
 * 起桌面程序，命令行打开 `files`（最后一份是活动文档）。数据目录和 WebView2 的用户目录放在一个新建的临时目录里，
 * 用完由 `cleanUp` 删掉。返回子进程、调试端口、临时目录和启动的时刻（`Date.now()`）。
 */
export async function launchApp(appBin, files) {
  const scratch = mkdtempSync(join(tmpdir(), 'magpdf-e2e-'));
  const port = await freePort();
  const launched = Date.now();
  const app = spawn(appBin, files, {
    env: {
      ...process.env,
      MAGPDF_WEBVIEW_DEBUG_PORT: String(port),
      MAGPDF_DATA_DIR: join(scratch, 'data'),
      WEBVIEW2_USER_DATA_FOLDER: join(scratch, 'webview'),
      MAGPDF_LOG: 'warn',
    },
    stdio: ['ignore', 'ignore', 'inherit'],
  });
  return { app, port, scratch, launched };
}

/** WebView2 的子进程在主进程结束后还要过一会儿才退出，在那之前临时目录删不掉。删不掉不算失败。 */
export async function cleanUp(scratch) {
  for (let attempt = 0; ; attempt++) {
    try {
      rmSync(scratch, { recursive: true, force: true });
      return;
    } catch (error) {
      if (attempt >= 40) {
        console.warn(`临时目录没删掉，留在 ${scratch}：${error.code ?? error}`);
        return;
      }
      await sleep(250);
    }
  }
}

export async function connect(port) {
  const browser = await waitFor(
    '连上 WebView2 的调试端口',
    () => chromium.connectOverCDP(`http://127.0.0.1:${port}`).catch(() => null),
    30_000,
    250,
  );
  const page = await waitFor(
    '找到界面的页面',
    () =>
      browser
        .contexts()
        .flatMap((context) => context.pages())
        .find((candidate) => /^https?:\/\//.test(candidate.url())),
    10_000,
  );
  return { browser, page };
}

/** 等画布要的瓦片都到齐：完成数至少 `min`，没有在途和排队的，画面是最新的。返回完成数。 */
export async function tilesSettled(page, min) {
  const handle = await page.waitForFunction(
    (atLeast) => {
      const controller = window.__magpdf?.controller();
      if (!controller) return false;
      const { stats, inFlight, queued } = controller.scheduler;
      const settled = stats.completed >= atLeast && inFlight === 0 && queued === 0;
      return settled && controller.upToDate ? stats.completed : false;
    },
    min,
    { timeout: 20_000, polling: 10 },
  );
  return handle.jsonValue();
}

/** 活动文档现在的样子：版本号、选区、第 1 页的对象数。 */
export function documentState(page) {
  return page.evaluate(() => {
    const state = window.__magpdf.store.getState();
    const doc = state.documents[state.activeDocumentId];
    return {
      name: doc.snapshot.name,
      version: doc.snapshot.version,
      selection: doc.selection,
      objects: doc.snapshot.pages[0].objects.length,
    };
  });
}

/** 第 1 页在屏幕上的矩形（CSS 像素，裁到画布之内）。 */
export function pageRectOnScreen(page) {
  return page.evaluate(() => {
    const controller = window.__magpdf.controller();
    const view = controller.view;
    const rect = controller.pageLayout.pageRect(0);
    const scale = (view.zoom * 96) / 72;
    // 画页面的那块画布让开了标尺，它的左上角就是视口的原点
    const canvas = controller.container.querySelector('.mg-canvas-gl').getBoundingClientRect();
    const left = Math.max(canvas.left, canvas.left + (rect.x - view.x) * scale);
    const top = Math.max(canvas.top, canvas.top + (rect.y - view.y) * scale);
    const right = Math.min(canvas.right, canvas.left + (rect.x + rect.width - view.x) * scale);
    const bottom = Math.min(canvas.bottom, canvas.top + (rect.y + rect.height - view.y) * scale);
    return { x: left, y: top, width: right - left, height: bottom - top, zoom: view.zoom };
  });
}

/**
 * 把鼠标挪到画布左上角的标尺交汇处：那里底下没有对象，不会留下悬停的高亮。
 * 等页面真的收到这一下移动再返回：`mouse.move` 返回时页面不一定处理过它，紧接着截图的话，上一个位置的
 * 悬停高亮还在图上（冒烟测试偶尔报“只能叠加内容的页画面变了”就是这样来的）。鼠标本来就停在那里时
 * 页面收不到移动事件，等一会儿就不等了。
 */
export async function parkMouse(page) {
  const corner = await page.evaluate(() => {
    const rect = window.__magpdf.controller().container.getBoundingClientRect();
    const corner = { x: rect.left + 6, y: rect.top + 6 };
    window.__parked = false;
    const onMove = (event) => {
      if (Math.abs(event.clientX - corner.x) < 1 && Math.abs(event.clientY - corner.y) < 1) {
        window.__parked = true;
        window.removeEventListener('pointermove', onMove, true);
      }
    };
    window.addEventListener('pointermove', onMove, true);
    return corner;
  });
  await page.mouse.move(corner.x, corner.y);
  await page
    .waitForFunction(() => window.__parked === true, null, { timeout: 1000, polling: 5 })
    .catch(() => undefined);
}

/**
 * 在活动文档的第 1 页上挑一个对象来操作：从上往下找第一个够大、而且点它的中心点中的就是它自己的。
 * 返回对象的编号、包围盒（页面坐标，pt）、中心在屏幕上的位置、每 pt 多少 CSS 像素。
 */
export async function pickTarget(page) {
  const target = await page.evaluate(() => {
    const state = window.__magpdf.store.getState();
    const doc = state.documents[state.activeDocumentId];
    const controller = window.__magpdf.controller();
    const scale = (controller.view.zoom * 96) / 72;
    const objects = doc.snapshot.pages[0].objects.filter((object) => object.pickable !== false);
    const contains = (box, x, y) =>
      x >= box.x && x <= box.x + box.width && y >= box.y && y <= box.y + box.height;
    for (let i = objects.length - 1; i >= 0; i--) {
      const { id, bbox } = objects[i];
      if (bbox.width * scale < 5 || bbox.height * scale < 5) continue;
      const [cx, cy] = [bbox.x + bbox.width / 2, bbox.y + bbox.height / 2];
      if (objects.slice(i + 1).some((above) => contains(above.bbox, cx, cy))) continue;
      const pageRect = controller.pageLayout.pageRect(0);
      const canvas = controller.container.querySelector('.mg-canvas-gl').getBoundingClientRect();
      return {
        id,
        bbox,
        scale,
        x: canvas.left + (pageRect.x + cx - controller.view.x) * scale,
        y: canvas.top + (pageRect.y + cy - controller.view.y) * scale,
      };
    }
    return null;
  });
  assert.ok(target, '第 1 页上没有找到可以拿来操作的对象');
  return target;
}

/** 等修改生效：文档到了这一版，受影响的瓦片重画完了。等不到时把当时的状态写进错误里。 */
export async function editSettled(page, version, what) {
  try {
    await page.waitForFunction(
      (expected) => {
        const state = window.__magpdf.store.getState();
        const doc = state.documents[state.activeDocumentId];
        const controller = window.__magpdf.controller();
        const { inFlight, queued } = controller.scheduler;
        return (
          doc.snapshot.version === expected && controller.upToDate && inFlight === 0 && queued === 0
        );
      },
      version,
      { timeout: 10_000, polling: 10 },
    );
  } catch (error) {
    const seen = await page.evaluate(() => {
      const controller = window.__magpdf.controller();
      const { inFlight, queued } = controller.scheduler;
      return {
        upToDate: controller.upToDate,
        inFlight,
        queued,
        notice: document.querySelector('.mg-notice-message')?.textContent ?? null,
      };
    });
    throw new Error(
      `${what}之后等不到文档的第 ${version} 版：${JSON.stringify({ ...(await documentState(page)), ...seen })}`,
      { cause: error },
    );
  }
}

/**
 * 在页面里装一个计时器：方向键按下的那一刻开始，记下“文档版本号变了”（收到 doc.changed）和
 * “画面重画完了”各用了多久，以及这期间向引擎取了几块瓦片、画布收到了几块瓦片。
 * 画面在每一帧画完之后看一眼，所以重画完的时间含等下一帧的时间，和用户看到的一致。
 */
export function installEditTimer(page) {
  return page.evaluate(() => {
    const { store } = window.__magpdf;
    const timer = { samples: [], current: null };
    const version = () => {
      const state = store.getState();
      return state.documents[state.activeDocumentId]?.snapshot.version ?? -1;
    };
    const tiles = () => {
      const controller = window.__magpdf.controller();
      return {
        engine: controller.tileProvider.stats?.engineTiles ?? 0,
        canvas: controller.scheduler.stats.completed,
      };
    };
    window.addEventListener(
      'keydown',
      (event) => {
        if (event.key.startsWith('Arrow')) {
          timer.current = { started: performance.now(), version: version(), tiles: tiles() };
        }
      },
      true,
    );
    store.subscribe(() => {
      const current = timer.current;
      if (current && current.changed === undefined && version() > current.version) {
        current.changed = performance.now() - current.started;
      }
    });
    const channel = new MessageChannel();
    channel.port1.onmessage = () => {
      const current = timer.current;
      if (current?.changed !== undefined && window.__magpdf.controller()?.upToDate) {
        const now = tiles();
        timer.samples.push({
          started: current.started,
          changed: current.changed,
          repainted: performance.now() - current.started,
          engineTiles: now.engine - current.tiles.engine,
          canvasTiles: now.canvas - current.tiles.canvas,
        });
        timer.current = null;
      }
    };
    // 每一帧的回调都跑完之后再看，这时画布控制器已经画完这一帧
    const tick = () => {
      channel.port2.postMessage(0);
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    window.__editTimer = timer;
  });
}

/** 一组数的中位数、P95（最近秩）和最大值，保留一位小数。 */
export function summarize(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const at = (q) => sorted[Math.min(sorted.length, Math.max(1, Math.ceil(q * sorted.length))) - 1];
  const round = (value) => Math.round(value * 10) / 10;
  return { median: round(at(0.5)), p95: round(at(0.95)), max: round(sorted[sorted.length - 1]) };
}
