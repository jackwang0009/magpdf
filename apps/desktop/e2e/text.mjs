/**
 * 改原有文字（方案 5.2 三级回退的原型）：点选页面上的一段文字，属性面板读出文字、字体、字号；改了回车，
 * 引擎生成新的文字对象换掉旧的，编号不变，画面跟着变。
 *
 * 用 color-devicergb.pdf：文字是不嵌入的 Helvetica。走一遍：
 *   点文字 → 属性面板显示原文和 Helvetica → 改成英文：原字体写得出，不提示换字体，画面变了
 *   → 改成中文：原字体写不出，换成系统字体，状态栏提示“字体已替换为 …” → 撤销两步，回到原文。
 *
 * 先构建发布版（步骤见 smoke.mjs 开头），再运行：
 *   node apps/desktop/e2e/text.mjs
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

/** 第 1 页上第一段点得中的文字：编号、包围盒（页面坐标，pt）、中心在窗口里的位置、截图用的区域。 */
function firstText(page) {
  return page.evaluate(() => {
    const state = window.__magpdf.store.getState();
    const doc = state.documents[state.activeDocumentId];
    const controller = window.__magpdf.controller();
    const view = controller.view;
    const scale = (view.zoom * 96) / 72;
    const pageRect = controller.pageLayout.pageRect(0);
    const canvas = controller.container.querySelector('.mg-canvas-gl').getBoundingClientRect();
    const objects = doc.snapshot.pages[0].objects;
    const contains = (box, x, y) =>
      x >= box.x && x <= box.x + box.width && y >= box.y && y <= box.y + box.height;
    for (let i = 0; i < objects.length; i++) {
      const { id, type, bbox, pickable } = objects[i];
      if (type !== 'text' || pickable === false || bbox.width * scale < 5) continue;
      const [cx, cy] = [bbox.x + bbox.width / 2, bbox.y + bbox.height / 2];
      if (objects.slice(i + 1).some((above) => contains(above.bbox, cx, cy))) continue;
      const toScreen = (x, y) => ({
        x: canvas.left + (pageRect.x + x - view.x) * scale,
        y: canvas.top + (pageRect.y + y - view.y) * scale,
      });
      const corner = toScreen(bbox.x - 4, bbox.y - 4);
      return {
        id,
        bbox,
        center: toScreen(cx, cy),
        // 新文字可能比原来的宽：往右多截一些
        clip: {
          x: corner.x,
          y: corner.y,
          width: (bbox.width * 2 + 8) * scale,
          height: (bbox.height + 8) * scale,
        },
      };
    }
    return null;
  });
}

const statusMessage = (page) =>
  page.evaluate(() => window.__magpdf.store.getState().statusMessage?.text ?? null);

/** 属性面板里的文字输入框读到了 `content`（文档改了之后面板重新读一遍）。 */
async function panelShows(page, content, what) {
  const input = page.locator('#mg-text-content');
  await waitFor(what, async () => (await input.inputValue().catch(() => null)) === content, 5_000);
}

const { app, port, scratch } = await launchApp(appBin, [pdf]);
let browser;
try {
  let page;
  ({ browser, page } = await connect(port));
  await tilesSettled(page, 1);
  await parkMouse(page);
  const before = await documentState(page);
  assert.equal(before.version, 0);

  // 1. 点文字：属性面板读出原文、字体
  const target = await firstText(page);
  assert.ok(target, '第 1 页上没有点得中的文字');
  await page.mouse.click(target.center.x, target.center.y);
  await waitFor(
    '文字被选中',
    async () => (await documentState(page)).selection.join() === target.id,
    3_000,
  );
  const input = page.locator('#mg-text-content');
  await input.waitFor({ timeout: 5_000 });
  await waitFor('读出原文', async () => (await input.inputValue()).length > 0, 5_000);
  const original = await input.inputValue();
  const panel = page.locator('.mg-panel', { has: input });
  assert.match(await panel.textContent(), /Helvetica/);
  await parkMouse(page);
  const shotBefore = await page.screenshot({ clip: target.clip });

  // 2. 改成英文：原字体写得出，不提示换字体；编号不变，画面变了
  await input.fill('MagPDF 123');
  await input.press('Enter');
  await editSettled(page, 1, '改成英文');
  await panelShows(page, 'MagPDF 123', '面板读到新文字');
  assert.equal(await statusMessage(page), null, '原字体写得出时不提示');
  assert.deepEqual((await documentState(page)).selection, [target.id], '改字不换编号');
  await parkMouse(page);
  await tilesSettled(page, 1);
  const shotEnglish = await page.screenshot({ clip: target.clip });
  assert.ok(!shotEnglish.equals(shotBefore), '画面上的文字变了');
  await page.screenshot({ path: join(shots, 'text-english.png') });

  // 3. 改成中文：换成系统字体，状态栏提示
  await page.locator('#mg-text-content').fill('提单号 0042');
  await page.locator('#mg-text-content').press('Enter');
  await editSettled(page, 2, '改成中文');
  await panelShows(page, '提单号 0042', '面板读到中文');
  const notice = await waitFor('状态栏提示换了字体', () => statusMessage(page), 3_000);
  assert.match(notice, /^字体已替换为 \S+/);
  await parkMouse(page);
  await tilesSettled(page, 1);
  await page.screenshot({ path: join(shots, 'text-chinese.png') });

  // 4. 撤销两步：回到原文（点画布让输入框交出焦点再按 Ctrl+Z）
  const corner = await page.evaluate(() => {
    const rect = window.__magpdf.controller().container.getBoundingClientRect();
    return { x: rect.left + 6, y: rect.top + 6 };
  });
  await page.mouse.click(corner.x, corner.y);
  for (let version = 3; version <= 4; version++) {
    await page.keyboard.press('Control+z');
    await editSettled(page, version, `第 ${version - 2} 次撤销`);
  }
  await page.mouse.click(target.center.x, target.center.y);
  await panelShows(page, original, '撤销之后面板读到原文');
  assert.equal((await documentState(page)).objects, before.objects);
  await sleep(100);

  console.log(
    JSON.stringify({
      object: target.id,
      originalLength: original.length,
      english: 'MagPDF 123',
      chineseNotice: notice,
      undone: true,
    }),
  );
} finally {
  await browser?.close().catch(() => undefined);
  app.kill();
  await sleep(200);
  await cleanUp(scratch);
}
