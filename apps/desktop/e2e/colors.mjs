/**
 * 颜色：点选页面上的方块，属性面板读出填充色；改成别的颜色回车，引擎改填充色，画面跟着变；撤销回到原色。
 *
 * 用 color-devicergb.pdf（红、绿、蓝三个只填充的方块）。走一遍：
 *   点红方块 → 面板显示填充色 → 写错（“红色”）提示原因、不发命令 → 改成 #1a6fd0 回车：方块变蓝
 *   → Ctrl+Z：又是红的，面板读回原来的颜色。
 *
 * 先构建发布版（步骤见 smoke.mjs 开头），再运行：
 *   node apps/desktop/e2e/colors.mjs
 *
 * 环境变量：
 *   MAGPDF_DESKTOP_BIN   被测的可执行文件，默认 target/release/magpdf-desktop.exe；引擎要在同一个目录
 *
 * 结果打印成一行 JSON，截图存到 tests/out/e2e/。测试窗口不抢焦点，不动真实的鼠标键盘。
 */
import assert from 'node:assert/strict';
import { mkdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import {
  cleanUp,
  connect,
  documentState,
  editSettled,
  launchApp,
  parkMouse,
  repo,
  sleep,
  tilesSettled,
  waitFor,
} from './harness.mjs';

const appBin = resolve(
  process.env.MAGPDF_DESKTOP_BIN ?? join(repo, 'target/release/magpdf-desktop.exe'),
);
const pdf = join(repo, 'tests/corpus/synthetic/color-devicergb.pdf');
const shots = join(repo, 'tests/out/e2e');
mkdirSync(shots, { recursive: true });

/** 第 1 页上面积最大的几个路径里最靠左上的那个（红方块）：编号、中心在窗口里的位置、中心附近的小块。 */
function firstSquare(page) {
  return page.evaluate(() => {
    const state = window.__magpdf.store.getState();
    const doc = state.documents[state.activeDocumentId];
    const controller = window.__magpdf.controller();
    const view = controller.view;
    const scale = (view.zoom * 96) / 72;
    const pageRect = controller.pageLayout.pageRect(0);
    const canvas = controller.container.querySelector('.mg-canvas-gl').getBoundingClientRect();
    const squares = doc.snapshot.pages[0].objects.filter(
      (o) => o.type === 'path' && o.pickable !== false && o.bbox.width > 40 && o.bbox.height > 40,
    );
    squares.sort((a, b) => a.bbox.y - b.bbox.y || a.bbox.x - b.bbox.x);
    const square = squares[0];
    if (!square) return null;
    const cx = canvas.left + (pageRect.x + square.bbox.x + square.bbox.width / 2 - view.x) * scale;
    const cy = canvas.top + (pageRect.y + square.bbox.y + square.bbox.height / 2 - view.y) * scale;
    return {
      id: square.id,
      center: { x: cx, y: cy },
      clip: { x: cx - 4, y: cy - 4, width: 8, height: 8 },
    };
  });
}

/** 截图里这一小块的平均颜色（0–255）。PNG 交给页面自己解码，不另装依赖。 */
async function averageColor(page, clip) {
  const png = await page.screenshot({ clip });
  return page.evaluate(async (base64) => {
    const bytes = Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
    const bitmap = await createImageBitmap(new Blob([bytes], { type: 'image/png' }));
    const canvas = new OffscreenCanvas(bitmap.width, bitmap.height);
    const context = canvas.getContext('2d');
    context.drawImage(bitmap, 0, 0);
    const { data } = context.getImageData(0, 0, bitmap.width, bitmap.height);
    const sum = [0, 0, 0];
    for (let i = 0; i < data.length; i += 4) for (let c = 0; c < 3; c++) sum[c] += data[i + c];
    const n = data.length / 4;
    return sum.map((s) => Math.round(s / n));
  }, png.toString('base64'));
}

const { app, port, scratch } = await launchApp(appBin, [pdf]);
let browser;
try {
  let page;
  ({ browser, page } = await connect(port));
  await tilesSettled(page, 1);
  await parkMouse(page);
  assert.equal((await documentState(page)).version, 0);

  // 1. 点红方块：面板显示填充色
  const square = await firstSquare(page);
  assert.ok(square, '第 1 页上没有找到方块');
  const before = await averageColor(page, square.clip);
  assert.ok(before[0] > 150 && before[2] < 100, `应当是红方块：${before}`);
  await page.mouse.click(square.center.x, square.center.y);
  const fill = page.locator('#mg-fill');
  await fill.waitFor({ timeout: 5_000 });
  await waitFor('读出填充色', async () => /^#[0-9a-f]{6}$/.test(await fill.inputValue()), 5_000);
  const original = await fill.inputValue();

  // 2. 写错：提示原因，不发命令
  await fill.fill('红色');
  await fill.press('Enter');
  const alert = page.locator('.mg-panel [role="alert"]');
  await alert.waitFor({ timeout: 2_000 });
  await sleep(300);
  assert.equal((await documentState(page)).version, 0, '写错的颜色不发给引擎');

  // 3. 改成蓝色：方块变蓝
  await fill.fill('#1a6fd0');
  await fill.press('Enter');
  await editSettled(page, 1, '改填充色');
  await waitFor('面板读回新颜色', async () => (await fill.inputValue()) === '#1a6fd0', 5_000);
  await parkMouse(page);
  await tilesSettled(page, 1);
  const blue = await averageColor(page, square.clip);
  assert.ok(
    Math.abs(blue[0] - 0x1a) < 12 && Math.abs(blue[1] - 0x6f) < 12 && Math.abs(blue[2] - 0xd0) < 12,
    `方块变成了 #1a6fd0：${blue}`,
  );
  await page.screenshot({ path: join(shots, 'colors-changed.png') });

  // 4. 撤销：又是红的（点画布让输入框交出焦点再按 Ctrl+Z）
  const corner = await page.evaluate(() => {
    const rect = window.__magpdf.controller().container.getBoundingClientRect();
    return { x: rect.left + 6, y: rect.top + 6 };
  });
  await page.mouse.click(corner.x, corner.y);
  await page.keyboard.press('Control+z');
  await editSettled(page, 2, '撤销改颜色');
  await parkMouse(page);
  await tilesSettled(page, 1);
  const undone = await averageColor(page, square.clip);
  assert.deepEqual(undone, before, '撤销之后颜色和原来一样');
  await page.mouse.click(square.center.x, square.center.y);
  await waitFor('面板读回原来的颜色', async () => (await fill.inputValue()) === original, 5_000);

  console.log(JSON.stringify({ object: square.id, original, before, blue, undone: true }));
} finally {
  await browser?.close().catch(() => undefined);
  app.kill();
  await sleep(200);
  await cleanUp(scratch);
}
