/**
 * 调整层级：点选红方块，Ctrl+Shift+] 置于顶层，“对象 > 置底”放到最下面，图层面板跟着变；撤销两步回到原样；
 * 在图层面板里把它那一行拖到最上面；按名字筛选图层；隐藏、锁定它。
 * 新放的条码在 PDFium 追加的另一段内容流里，挪到原有对象下面写不下来：提示原因，文档不变。
 *
 * 用 color-devicergb.pdf（原有内容在一段内容流里）。
 *
 * 先构建发布版（步骤见 smoke.mjs 开头），再运行：
 *   node apps/desktop/e2e/layers.mjs
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

/** 第 1 页从下到上的对象编号。 */
const order = (page) =>
  page.evaluate(() => {
    const state = window.__magpdf.store.getState();
    return state.documents[state.activeDocumentId].snapshot.pages[0].objects.map((o) => o.id);
  });

/** 第 1 页上一点（页面坐标，pt）在窗口里的位置。 */
function onPage(page, x, y) {
  return page.evaluate(
    ([x, y]) => {
      const controller = window.__magpdf.controller();
      const view = controller.view;
      const rect = controller.pageLayout.pageRect(0);
      const scale = (view.zoom * 96) / 72;
      const canvas = controller.container.querySelector('.mg-canvas-gl').getBoundingClientRect();
      return {
        x: canvas.left + (rect.x + x - view.x) * scale,
        y: canvas.top + (rect.y + y - view.y) * scale,
      };
    },
    [x, y],
  );
}

/** 面积最大的几个路径里最靠左上的那个（红方块）。 */
function firstSquare(page) {
  return page.evaluate(() => {
    const state = window.__magpdf.store.getState();
    const objects = state.documents[state.activeDocumentId].snapshot.pages[0].objects;
    const squares = objects.filter(
      (o) => o.type === 'path' && o.pickable !== false && o.bbox.width > 40 && o.bbox.height > 40,
    );
    squares.sort((a, b) => a.bbox.y - b.bbox.y || a.bbox.x - b.bbox.x);
    const square = squares[0];
    return square
      ? {
          id: square.id,
          name: square.name,
          x: square.bbox.x + square.bbox.width / 2,
          y: square.bbox.y + square.bbox.height / 2,
        }
      : null;
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

async function menuItem(page, menu, label) {
  await page.locator('.mg-menu', { hasText: menu }).click();
  await page.locator('.mg-menu-item', { hasText: label }).click();
}

const { app, port, scratch } = await launchApp(appBin, [pdf]);
let browser;
try {
  let page;
  ({ browser, page } = await connect(port));
  await tilesSettled(page, 1);
  await parkMouse(page);
  const original = await order(page);

  // 1. 点红方块，Ctrl+Shift+]：置于顶层，图层面板最上面一行是它
  const square = await firstSquare(page);
  assert.ok(square, '第 1 页上没有找到方块');
  const at = await onPage(page, square.x, square.y);
  await page.mouse.click(at.x, at.y);
  await waitFor(
    '方块被选中',
    async () => (await documentState(page)).selection.join() === square.id,
    3_000,
  );
  await page.keyboard.press('Control+Shift+BracketRight');
  await editSettled(page, 1, '置于顶层');
  await waitFor('方块到了最上面', async () => (await order(page)).at(-1) === square.id, 5_000);
  await page.locator('.dv-tab', { hasText: '图层' }).click();
  const topLayer = page.locator('.mg-layer').first();
  await waitFor(
    '图层面板最上面一行是方块',
    async () => (await topLayer.textContent()).includes(square.name),
    3_000,
  );
  await parkMouse(page);
  await page.screenshot({ path: join(shots, 'layers-front.png') });

  // 2. 对象 > 置底
  await menuItem(page, '对象', '置底');
  await editSettled(page, 2, '置底');
  await waitFor('方块到了最下面', async () => (await order(page))[0] === square.id, 5_000);

  // 3. 撤销两步：回到原来的顺序（点画布空白处之前先让菜单收起）
  for (let version = 3; version <= 4; version++) {
    await page.keyboard.press('Control+z');
    await editSettled(page, version, `第 ${version - 2} 次撤销`);
  }
  await waitFor('顺序回到原样', async () => (await order(page)).join() === original.join(), 5_000);

  // 4. 图层面板：把方块那一行拖到最上面一行的上面；按名字筛选
  const layerRow = (name) =>
    page
      .locator('.mg-layers li')
      .filter({ has: page.locator('.mg-layer-name', { hasText: name }) });
  const topName = await page.locator('.mg-layer-name').first().textContent();
  await layerRow(square.name)
    .locator('button.mg-layer')
    .dragTo(layerRow(topName), { targetPosition: { x: 20, y: 3 } });
  await editSettled(page, 5, '图层面板里拖动');
  await waitFor('拖到了最上面', async () => (await order(page)).at(-1) === square.id, 5_000);
  await page.locator('.mg-layers').screenshot({ path: join(shots, 'layers-dragged.png') });
  const search = page.locator('.mg-panel-search-input');
  await search.fill('文字');
  const textRows = await page.locator('.mg-layer-name').allTextContents();
  assert.ok(
    textRows.length > 0 && textRows.every((name) => name.startsWith('文字')),
    `只剩文字：${textRows}`,
  );
  await search.fill('');

  // 5. 显示、锁定（只在本次会话有效，文档版本号不变）：隐藏之后那一块画的是白底、点不中；锁定之后照画、
  //    点不中、全选不选它
  const toggles = layerRow(square.name).locator('.mg-layer-toggle');
  const at2 = await onPage(page, square.x, square.y);
  const clip = { x: at2.x - 4, y: at2.y - 4, width: 8, height: 8 };
  const shown = await averageColor(page, clip);
  await toggles.nth(0).click();
  await sleep(100);
  await tilesSettled(page, 1);
  await parkMouse(page);
  const hiddenColor = await averageColor(page, clip);
  assert.ok(
    hiddenColor.every((c) => c > 240),
    `隐藏之后是白底：${hiddenColor}（原来 ${shown}）`,
  );
  await page.mouse.click(at2.x, at2.y);
  assert.ok(!(await documentState(page)).selection.includes(square.id), '隐藏的点不中');
  await page.screenshot({ path: join(shots, 'layers-hidden.png') });
  await toggles.nth(0).click();
  await sleep(100);
  await tilesSettled(page, 1);
  await parkMouse(page);
  assert.deepEqual(await averageColor(page, clip), shown, '显示回来，颜色和原来一样');

  await toggles.nth(1).click();
  await page.mouse.click(at2.x, at2.y);
  assert.ok(!(await documentState(page)).selection.includes(square.id), '锁定的点不中');
  await page.keyboard.press('Control+a');
  const all = (await documentState(page)).selection;
  assert.ok(all.length > 0 && !all.includes(square.id), `全选跳过锁定的：${all}`);
  await toggles.nth(1).click();
  assert.equal((await documentState(page)).version, 5, '隐藏、锁定不改文档');

  // 6. 新放的条码在另一段内容流里：挪到最下面写不下来，提示原因，文档不变
  await page.locator('.mg-comp-item:not([disabled])', { hasText: 'Code 128' }).first().click();
  // 页面只有 300×200 pt 左右，放在右下角的空处（点在页面外面不算放置）
  const spot = await onPage(page, 230, 150);
  await page.mouse.click(spot.x, spot.y);
  await editSettled(page, 6, '放条码');
  await waitFor(
    '条码被选中',
    async () => (await documentState(page)).selection.length === 1,
    5_000,
  );
  const placedOrder = await order(page);
  await page.keyboard.press('Control+Shift+BracketLeft');
  const notice = page.locator('.mg-notice[data-kind="error"] .mg-notice-message');
  await notice.waitFor({ timeout: 5_000 });
  const message = await notice.textContent();
  assert.match(message, /调不了/);
  await page.screenshot({ path: join(shots, 'layers-refused.png') });
  await page.locator('.mg-notice[data-kind="error"] button').click();
  await sleep(300);
  assert.equal((await documentState(page)).version, 6, '拒绝了的命令不改文档');
  assert.deepEqual(await order(page), placedOrder);

  console.log(
    JSON.stringify({ object: square.id, objects: original.length, refused: message.trim() }),
  );
} finally {
  await browser?.close().catch(() => undefined);
  app.kill();
  await sleep(200);
  await cleanUp(scratch);
}
