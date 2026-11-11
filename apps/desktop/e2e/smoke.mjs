/**
 * 界面端到端冒烟测试（方案 13.2 第 8 项；《测试计划》“界面端到端”）。
 *
 * 起真实的桌面程序（主进程 + 引擎进程 + WebView2），Playwright 经 CDP 连进 WebView2，走一遍：
 * 命令行打开 PDF → 页面由引擎渲染出来 → 滚轮缩放 → 强制结束引擎后自动恢复显示 → 主进程退出后引擎跟着退出。
 *
 * 先构建发布版（界面打进可执行文件里）：
 *   pnpm --filter @magpdf/desktop build
 *   cargo build --release -p magpdf-engine-proc
 *   cargo build --release -p magpdf-desktop --features tauri/custom-protocol
 * 再运行：
 *   pnpm --filter @magpdf/desktop e2e
 *
 * 环境变量：
 *   MAGPDF_DESKTOP_BIN  被测的可执行文件，默认 target/release/magpdf-desktop.exe
 *   MAGPDF_E2E_PDF      打开哪份 PDF，默认 tests/corpus/synthetic/freight-bl-composite.pdf
 *
 * 结果打印成一行 JSON（各步耗时），截图存到 tests/out/e2e/。测试窗口不抢焦点，不动鼠标键盘。
 */
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const appBin = resolve(
  process.env.MAGPDF_DESKTOP_BIN ?? join(repo, 'target/release/magpdf-desktop.exe'),
);
const samplePdf = resolve(
  process.env.MAGPDF_E2E_PDF ?? join(repo, 'tests/corpus/synthetic/freight-bl-composite.pdf'),
);
const outDir = join(repo, 'tests/out/e2e');

/** 《P0 技术验证计划》V06 的通过线：强制结束引擎后 5 秒内恢复显示。 */
const RESTORE_LIMIT_MS = 5000;

function freePort() {
  return new Promise((done, fail) => {
    const server = createServer();
    server.once('error', fail);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close(() => done(port));
    });
  });
}

function sleep(ms) {
  return new Promise((done) => setTimeout(done, ms));
}

function isRunning(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function waitFor(what, check, timeoutMs, intervalMs = 50) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await check();
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`等了 ${timeoutMs} ms：${what}`);
    await sleep(intervalMs);
  }
}

/** 主进程名下的引擎进程。 */
function enginePid(appPid) {
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

async function connect(port) {
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

/** 等画布要的瓦片都到齐：完成数至少 `min`，没有在途和排队的。返回完成数。 */
async function tilesSettled(page, min) {
  const handle = await page.waitForFunction(
    (atLeast) => {
      const controller = window.__magpdf?.controller();
      if (!controller) return false;
      const { stats, inFlight, queued } = controller.scheduler;
      return stats.completed >= atLeast && inFlight === 0 && queued === 0 ? stats.completed : false;
    },
    min,
    { timeout: 20_000, polling: 10 },
  );
  return handle.jsonValue();
}

/** 第 1 页在屏幕上的矩形（CSS 像素，裁到画布之内）。 */
function pageRectOnScreen(page) {
  return page.evaluate(() => {
    const controller = window.__magpdf.controller();
    const view = controller.view;
    const rect = controller.pageLayout.pageRect(0);
    const scale = (view.zoom * 96) / 72;
    const canvas = document.querySelector('.mg-canvas-gl').getBoundingClientRect();
    const left = Math.max(canvas.left, canvas.left + (rect.x - view.x) * scale);
    const top = Math.max(canvas.top, canvas.top + (rect.y - view.y) * scale);
    const right = Math.min(canvas.right, canvas.left + (rect.x + rect.width - view.x) * scale);
    const bottom = Math.min(canvas.bottom, canvas.top + (rect.y + rect.height - view.y) * scale);
    return { x: left, y: top, width: right - left, height: bottom - top, zoom: view.zoom };
  });
}

/**
 * 截一张图，数页面区域里的像素：白纸、有内容、既不白也不深的（没渲染出来时是画布的底色）。
 * 截图在页面里解码（不给 Node 这边加图像库）。
 */
async function inspectPage(page, name) {
  const rect = await pageRectOnScreen(page);
  assert.ok(rect.width > 50 && rect.height > 50, `页面没有显示在画布上：${JSON.stringify(rect)}`);
  const png = await page.screenshot({ scale: 'css' });
  writeFileSync(join(outDir, `${name}.png`), png);
  const counts = await page.evaluate(
    async ({ base64, area }) => {
      const bytes = Uint8Array.from(atob(base64), (ch) => ch.charCodeAt(0));
      const bitmap = await createImageBitmap(new Blob([bytes], { type: 'image/png' }));
      const canvas = new OffscreenCanvas(bitmap.width, bitmap.height);
      const context = canvas.getContext('2d');
      context.drawImage(bitmap, 0, 0);
      const inset = 3;
      const data = context.getImageData(
        Math.ceil(area.x) + inset,
        Math.ceil(area.y) + inset,
        Math.floor(area.width) - 2 * inset,
        Math.floor(area.height) - 2 * inset,
      ).data;
      let white = 0;
      let ink = 0;
      for (let i = 0; i < data.length; i += 4) {
        const [r, g, b] = [data[i], data[i + 1], data[i + 2]];
        if (r > 245 && g > 245 && b > 245) white++;
        else if (r < 200 || g < 200 || b < 200) ink++;
      }
      return { total: data.length / 4, white, ink };
    },
    { base64: png.toString('base64'), area: rect },
  );
  return { ...counts, zoom: rect.zoom };
}

function assertPageIsDrawn(counts, when) {
  const white = counts.white / counts.total;
  const ink = counts.ink / counts.total;
  assert.ok(
    white > 0.3 && ink > 0.02 && ink < 0.6,
    `${when}：页面区域里白 ${(white * 100).toFixed(1)}%、有内容 ${(ink * 100).toFixed(1)}%，不像一页渲染好的单证`,
  );
}

async function main() {
  for (const [what, path] of [
    ['桌面程序', appBin],
    ['引擎', join(dirname(appBin), 'magpdf-engine.exe')],
    ['样本 PDF', samplePdf],
  ]) {
    assert.ok(existsSync(path), `找不到${what}：${path}（构建步骤见本文件开头）`);
  }
  mkdirSync(outDir, { recursive: true });
  const scratch = mkdtempSync(join(tmpdir(), 'magpdf-e2e-'));
  const port = await freePort();
  const result = { app: basename(appBin), pdf: basename(samplePdf) };

  const launched = performance.now();
  const app = spawn(appBin, [samplePdf], {
    env: {
      ...process.env,
      MAGPDF_WEBVIEW_DEBUG_PORT: String(port),
      MAGPDF_DATA_DIR: join(scratch, 'data'),
      WEBVIEW2_USER_DATA_FOLDER: join(scratch, 'webview'),
      MAGPDF_LOG: 'warn',
    },
    stdio: ['ignore', 'ignore', 'inherit'],
  });
  let browser;
  let engine = null;
  try {
    const session = await connect(port);
    browser = session.browser;
    const page = session.page;
    const problems = [];
    page.on('pageerror', (error) => problems.push(`页面里抛了异常：${error.message}`));
    page.on('console', (message) => {
      if (message.type() !== 'error') return;
      // 引擎被结束的那一刻，正在取的瓦片会得到 410（槽位作废），界面随后重新请求，这是协议规定的走法；
      // 浏览器会把每个非 2xx 的响应记成一条控制台错误，不算问题。
      if (/status of 410/.test(message.text())) return;
      problems.push(`控制台报错：${message.text()}`);
    });

    // 1. 命令行给的文档已经打开，页面由引擎渲染出来
    await page.waitForSelector('.mg-doc-tab-title', { timeout: 20_000 });
    assert.deepEqual(await page.locator('.mg-doc-tab-title').allTextContents(), [
      basename(samplePdf),
    ]);
    let completed = await tilesSettled(page, 1);
    result.firstPaintMs = Math.round(performance.now() - launched);
    assertPageIsDrawn(await inspectPage(page, 'smoke-1-opened'), '打开之后');
    assert.equal(await page.locator('.mg-canvas-message').count(), 0, '画布上不该有提示层');

    // 2. 真实的鼠标滚轮：Ctrl + 滚轮放大，画布按新的缩放重新要瓦片
    const before = await pageRectOnScreen(page);
    await page.mouse.move(before.x + before.width / 2, before.y + before.height / 2);
    await page.keyboard.down('Control');
    await page.mouse.wheel(0, -360);
    await page.keyboard.up('Control');
    await page.waitForFunction(
      (zoom) => window.__magpdf.controller().view.zoom > zoom * 1.2,
      before.zoom,
    );
    completed = await tilesSettled(page, completed + 1);
    const zoomed = await inspectPage(page, 'smoke-2-zoomed');
    assert.ok(zoomed.zoom > before.zoom * 1.2, `缩放没有变：${before.zoom} → ${zoomed.zoom}`);
    assertPageIsDrawn(zoomed, '放大之后');

    // 3. 强制结束引擎：界面不动，主进程重启引擎，页面重新渲染出来（V06 的第三条通过线）
    engine = enginePid(app.pid);
    assert.ok(engine, '没有找到引擎进程');
    const killed = performance.now();
    process.kill(engine);
    await page.waitForSelector('.mg-notice', { timeout: RESTORE_LIMIT_MS * 2 });
    await tilesSettled(page, completed + 1);
    result.restoreMs = Math.round(performance.now() - killed);
    assert.equal(await page.locator('.mg-notice').getAttribute('data-kind'), 'info');
    await page.locator('.mg-notice button').click();
    assert.equal(await page.locator('.mg-notice').count(), 0);
    assertPageIsDrawn(await inspectPage(page, 'smoke-3-restored'), '引擎重启之后');
    assert.deepEqual(await page.locator('.mg-doc-tab-title').allTextContents(), [
      basename(samplePdf),
    ]);
    const replacement = enginePid(app.pid);
    assert.ok(replacement && replacement !== engine, '引擎没有换成新的进程');
    engine = replacement;
    assert.ok(
      result.restoreMs < RESTORE_LIMIT_MS,
      `结束引擎后 ${result.restoreMs} ms 才恢复显示，超过 ${RESTORE_LIMIT_MS} ms`,
    );

    // 4. 关掉标签页，文档从界面上消失
    await page.locator('.mg-doc-tab-close').click();
    await page.waitForFunction(() => window.__magpdf.documents().length === 0);

    assert.deepEqual(problems, [], '运行过程中页面报了错');
  } finally {
    await browser?.close().catch(() => undefined);
    app.kill();
  }

  // 5. 主进程没了，引擎自己退出，不留下没人管的进程
  if (engine) {
    const gone = performance.now();
    await waitFor('引擎随主进程退出', () => !isRunning(engine), 5000);
    result.engineExitMs = Math.round(performance.now() - gone);
  }
  console.log(JSON.stringify(result));
  // WebView2 的子进程在主进程结束后还要过一会儿才退出，在那之前数据目录删不掉。删不掉不算测试失败。
  for (let attempt = 0; ; attempt++) {
    try {
      rmSync(scratch, { recursive: true, force: true });
      break;
    } catch (error) {
      if (attempt >= 40) {
        console.warn(`临时目录没删掉，留在 ${scratch}：${error.code ?? error}`);
        break;
      }
      await sleep(250);
    }
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
