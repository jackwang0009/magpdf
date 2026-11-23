/**
 * 保存（方案 9.1；原型只有完整重写）：改了文档按 Ctrl+S，文件存回去，“已修改”标记去掉，状态栏写“已保存”；
 * 存出来的文件里只有组件现在的参数，改之前的那一版不留（没有引用的对象在保存时去掉了）。
 *
 * 在提单样本的一份副本上做：点 Code 128 放到页面上 → 属性面板把内容改成 SAVED-0042 → Ctrl+S
 * → 读磁盘上的文件，把所有压缩过的流解开：有 SAVED-0042，没有改之前的 SAMPLE-0001；不留临时文件。
 * 另存为要点系统的对话框，这里不测（引擎那一侧的另存为见 engine/magpdf-engine-proc/tests/e2e.rs）。
 *
 * 先构建发布版（步骤见 smoke.mjs 开头），再运行：
 *   node apps/desktop/e2e/save.mjs
 *
 * 环境变量：
 *   MAGPDF_DESKTOP_BIN   被测的可执行文件，默认 target/release/magpdf-desktop.exe；引擎要在同一个目录
 *
 * 结果打印成一行 JSON。测试窗口不抢焦点，不动真实的鼠标键盘。
 */
import assert from 'node:assert/strict';
import { copyFileSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { inflateSync } from 'node:zlib';
import {
  cleanUp,
  connect,
  documentState,
  editSettled,
  launchApp,
  repo,
  sleep,
  tilesSettled,
  waitFor,
} from './harness.mjs';

const appBin = resolve(
  process.env.MAGPDF_DESKTOP_BIN ?? join(repo, 'target/release/magpdf-desktop.exe'),
);
const work = mkdtempSync(join(tmpdir(), 'magpdf-save-e2e-'));
const pdf = join(work, '提单.pdf');
copyFileSync(join(repo, 'tests/corpus/synthetic/freight-bl-composite.pdf'), pdf);
const originalSize = statSync(pdf).size;

/** 文件里所有流的内容（压缩过的解开，解不开的原样），拼成一个 latin1 字符串，找组件的参数用。 */
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
    // 跳过整个 endstream，免得下一次在它里面匹配到 stream
    pattern.lastIndex = end + 'endstream'.length;
  }
  return parts.join('\n');
}

function dirty(page) {
  return page.evaluate(() => {
    const state = window.__magpdf.store.getState();
    return state.documents[state.activeDocumentId].snapshot.dirty;
  });
}

const { app, port, scratch } = await launchApp(appBin, [pdf]);
let browser;
try {
  let page;
  ({ browser, page } = await connect(port));
  await tilesSettled(page, 1);
  assert.equal(await dirty(page), false);

  // 放一个条码，改内容（改之前那一版在 PDFium 的文档里留成没有引用的对象）
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
  const content = page.locator('#mg-comp-content');
  await content.waitFor({ timeout: 5_000 });
  await content.fill('SAVED-0042');
  await content.press('Enter');
  await editSettled(page, 2, '改条码内容');
  assert.equal(await dirty(page), true, '改过之后标成已修改');

  // Ctrl+S：先让输入框交出焦点（点一下画布的标尺交汇处，那里没有对象）
  const corner = await page.evaluate(() => {
    const rect = window.__magpdf.controller().container.getBoundingClientRect();
    return { x: rect.left + 6, y: rect.top + 6 };
  });
  await page.mouse.click(corner.x, corner.y);
  await page.keyboard.press('Control+s');
  const message = await waitFor(
    '状态栏写“已保存”',
    () =>
      page.evaluate(() => {
        const state = window.__magpdf.store.getState();
        return state.statusMessage?.text ?? null;
      }),
    10_000,
  );
  assert.equal(message, '已保存“提单.pdf”');
  assert.equal(await dirty(page), false, '存完去掉已修改标记');
  assert.equal((await documentState(page)).version, 2, '保存不改文档版本');

  const saved = readFileSync(pdf);
  assert.notEqual(saved.length, originalSize, '文件存回去了');
  const streams = streamText(saved);
  assert.ok(streams.includes('SAVED-0042'), '存出来的文件里有组件现在的内容');
  assert.ok(!streams.includes('SAMPLE-0001'), '改之前的那一版不能留在文件里');
  assert.deepEqual(readdirSync(work), ['提单.pdf'], '不留临时文件');

  console.log(
    JSON.stringify({
      saved: true,
      bytes: { before: originalSize, after: saved.length },
      oldVersionGone: true,
      message,
    }),
  );
} finally {
  await browser?.close().catch(() => undefined);
  app.kill();
  await sleep(200);
  await cleanUp(scratch);
  rmSync(work, { recursive: true, force: true });
}
