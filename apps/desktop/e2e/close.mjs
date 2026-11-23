/**
 * 关闭前保存（《交互设计》9.1、9.2）：关标签页、关窗口时有没保存的修改，先问“保存”“不保存”“取消”。
 *
 * 在提单样本的一份副本上做，起三次程序：
 *   1. 放一个条码 → 点标签页的关闭按钮：问；“取消”文档还开着 → Ctrl+W：问；“不保存”文档关了，磁盘上的文件没动
 *   2. 放一个条码 → 关窗口（发 WM_CLOSE，和点右上角的关闭按钮一样）：问；“保存”之后程序退出，文件里有条码
 *   3. 什么都不改 → 关窗口：不问，程序直接退出
 *
 * 先构建发布版（步骤见 smoke.mjs 开头），再运行：
 *   node apps/desktop/e2e/close.mjs
 *
 * 环境变量：
 *   MAGPDF_DESKTOP_BIN   被测的可执行文件，默认 target/release/magpdf-desktop.exe；引擎要在同一个目录
 *
 * 结果打印成一行 JSON。测试窗口不抢焦点，不动真实的鼠标键盘。
 */
import assert from 'node:assert/strict';
import { copyFileSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { inflateSync } from 'node:zlib';
import {
  cleanUp,
  connect,
  editSettled,
  launchApp,
  repo,
  requestWindowClose,
  sleep,
  tilesSettled,
  waitFor,
} from './harness.mjs';

const appBin = resolve(
  process.env.MAGPDF_DESKTOP_BIN ?? join(repo, 'target/release/magpdf-desktop.exe'),
);
const work = mkdtempSync(join(tmpdir(), 'magpdf-close-e2e-'));
const pdf = join(work, '提单.pdf');
copyFileSync(join(repo, 'tests/corpus/synthetic/freight-bl-composite.pdf'), pdf);
const original = readFileSync(pdf);
const QUESTION = '要保存对“提单.pdf”的修改吗？';

/** 文件里所有流的内容（压缩过的解开），拼成一个 latin1 字符串。 */
function streamText(bytes) {
  const text = bytes.toString('latin1');
  const parts = [];
  const pattern = /stream\r?\n/g;
  let match;
  while ((match = pattern.exec(text))) {
    const start = match.index + match[0].length;
    const end = text.indexOf('endstream', start);
    if (end < 0) break;
    const raw = bytes.subarray(start, end);
    try {
      parts.push(inflateSync(raw).toString('latin1'));
    } catch {
      parts.push(raw.toString('latin1'));
    }
    pattern.lastIndex = end + 'endstream'.length;
  }
  return parts.join('\n');
}

/** 起程序打开副本，等画面出来。 */
async function start() {
  const launched = await launchApp(appBin, [pdf]);
  const { browser, page } = await connect(launched.port);
  await tilesSettled(page, 1);
  return { ...launched, browser, page };
}

/** 在第 1 页上放一个 Code 128（文档变成“已修改”）。 */
async function placeBarcode(page) {
  const at = await page.evaluate(() => {
    const controller = window.__magpdf.controller();
    const view = controller.view;
    const rect = controller.pageLayout.pageRect(0);
    const scale = (view.zoom * 96) / 72;
    const canvas = controller.container.querySelector('.mg-canvas-gl').getBoundingClientRect();
    return {
      x: canvas.left + (rect.x + 60 - view.x) * scale,
      y: canvas.top + (rect.y + 80 - view.y) * scale,
    };
  });
  await page.locator('.mg-comp-item:not([disabled])', { hasText: 'Code 128' }).first().click();
  await page.mouse.click(at.x, at.y);
  await editSettled(page, 1, '置入条码');
}

const documentCount = (page) => page.evaluate(() => window.__magpdf.documents().length);

async function exited(app) {
  await waitFor('程序退出', () => app.exitCode !== null || app.signalCode !== null, 10_000);
}

const result = {};
const scratches = [];
try {
  // 1. 关标签页
  {
    const run = await start();
    scratches.push(run.scratch);
    try {
      await placeBarcode(run.page);
      await run.page.locator('.mg-doc-tab-close').first().click();
      const ask = run.page.getByRole('alertdialog', { name: QUESTION });
      await ask.waitFor({ timeout: 5_000 });
      await ask.getByRole('button', { name: '取消' }).click();
      assert.equal(await documentCount(run.page), 1, '取消之后文档还开着');

      // 焦点在标签页的按钮上时 Ctrl+W 也算：外壳统一处理
      await run.page.keyboard.press('Control+w');
      await ask.waitFor({ timeout: 5_000 });
      await ask.getByRole('button', { name: '不保存' }).click();
      await run.page.waitForFunction(() => window.__magpdf.documents().length === 0);
      assert.ok(readFileSync(pdf).equals(original), '不保存：磁盘上的文件没动');
      result.closeTab = 'cancel, then discard';
    } finally {
      await run.browser.close().catch(() => undefined);
      run.app.kill();
    }
  }

  // 2. 关窗口，选“保存”
  {
    const run = await start();
    scratches.push(run.scratch);
    try {
      await placeBarcode(run.page);
      requestWindowClose(run.app.pid);
      const ask = run.page.getByRole('alertdialog', { name: QUESTION });
      await ask.waitFor({ timeout: 5_000 });
      await ask.getByRole('button', { name: '保存', exact: true }).click();
      await exited(run.app);
      assert.ok(streamText(readFileSync(pdf)).includes('SAMPLE-0001'), '存了：文件里有条码');
      result.exitSave = 'saved, then exited';
    } finally {
      await run.browser.close().catch(() => undefined);
      run.app.kill();
    }
  }

  // 3. 没改过：关窗口不问，直接退出
  {
    const run = await start();
    scratches.push(run.scratch);
    try {
      const closed = Date.now();
      requestWindowClose(run.app.pid);
      await exited(run.app);
      result.exitClean = { exitedMs: Date.now() - closed };
      // 主进程等界面回应最多 3 秒才会自己关；这里应当是界面答复之后马上关的
      assert.ok(result.exitClean.exitedMs < 3_000, `退出用了 ${result.exitClean.exitedMs} ms`);
    } finally {
      await run.browser.close().catch(() => undefined);
      run.app.kill();
    }
  }
  console.log(JSON.stringify(result));
} finally {
  await sleep(200);
  for (const scratch of scratches) await cleanUp(scratch);
  rmSync(work, { recursive: true, force: true });
}
