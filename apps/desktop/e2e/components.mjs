/**
 * 组件原型（方案第 11 节；P0 只有“文本”和“Code 128”两种）：在组件面板里点一下再点页面，或者直接拖到页面上，
 * 组件就放在那里并被选中；属性面板显示内容和尺寸，改了之后引擎按新参数重新生成。
 *
 * 用提单样本：原版 PDFium 下它的第 1 页只能叠加内容，新放的组件照样能放、能改、能移动（修过生成器的构建下
 * 这一页可以修改，流程一样）。走一遍：
 *   点 Code 128 → 点页面：条码出现在点的位置、被选中，画面上那一块变黑
 *   → 属性面板改内容：引擎重新生成，左上角不动；写中文：提示原因，不发命令
 *   → 方向键微移条码 → 把“文本”拖到页面上，内容改成中文（嵌系统字体的子集）→ 点了组件又按 Esc：不放了
 *   → 撤销五步，对象回到原样。
 *
 * 先构建发布版（步骤见 smoke.mjs 开头），再运行：
 *   node apps/desktop/e2e/components.mjs
 *
 * 环境变量：
 *   MAGPDF_DESKTOP_BIN   被测的可执行文件，默认 target/release/magpdf-desktop.exe；引擎要在同一个目录
 *   MAGPDF_E2E_PDF       打开的 PDF，默认 tests/corpus/synthetic/freight-bl-composite.pdf
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
const pdf = resolve(
  process.env.MAGPDF_E2E_PDF ?? join(repo, 'tests/corpus/synthetic/freight-bl-composite.pdf'),
);
const shots = join(repo, 'tests/out/e2e');
mkdirSync(shots, { recursive: true });

/** 第 1 页上一点（页面坐标，pt）在窗口里的位置，和每 pt 多少 CSS 像素。 */
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
        scale,
      };
    },
    [x, y],
  );
}

/** 选中的那一个对象，是置入的组件时返回它（编号、包围盒、参数、是否新加的）。 */
function selectedComponent(page) {
  return page.evaluate(() => {
    const state = window.__magpdf.store.getState();
    const doc = state.documents[state.activeDocumentId];
    if (doc.selection.length !== 1) return null;
    const object = doc.snapshot.pages[0].objects.find((o) => o.id === doc.selection[0]);
    return object?.component
      ? { id: object.id, bbox: object.bbox, component: object.component, added: object.added }
      : null;
  });
}

const placing = (page) => page.evaluate(() => window.__magpdf.store.getState().placing);

/** 截图里这一块（窗口坐标）有多少比例的像素是深色的。PNG 交给页面自己解码，不另装依赖。 */
async function darkShare(page, clip) {
  const png = await page.screenshot({ clip });
  return page.evaluate(async (base64) => {
    const bytes = Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
    const bitmap = await createImageBitmap(new Blob([bytes], { type: 'image/png' }));
    const canvas = new OffscreenCanvas(bitmap.width, bitmap.height);
    const context = canvas.getContext('2d');
    context.drawImage(bitmap, 0, 0);
    const { data } = context.getImageData(0, 0, bitmap.width, bitmap.height);
    let dark = 0;
    for (let i = 0; i < data.length; i += 4) {
      if (data[i] + data[i + 1] + data[i + 2] < 3 * 80) dark++;
    }
    return dark / (data.length / 4);
  }, png.toString('base64'));
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
  const pageFidelity = await page.evaluate(() => {
    const state = window.__magpdf.store.getState();
    return state.documents[state.activeDocumentId].snapshot.pages[0].overlayOnly
      ? 'overlayOnly'
      : 'ok';
  });

  // 1. 点 Code 128，再点页面：放在点的位置，选中，画面上那一块变黑
  const at = await onPage(page, 60, 80);
  // 条码默认 36 pt 高，SAMPLE-0001 连静区一共 167 个模块（1 pt 一个）
  const area = { x: at.x, y: at.y, width: 150 * at.scale, height: 36 * at.scale };
  const darkBefore = await darkShare(page, area);
  await page.locator('.mg-comp-item:not([disabled])', { hasText: 'Code 128' }).first().click();
  assert.equal(await placing(page), 'code128');
  await page.mouse.click(at.x, at.y);
  await editSettled(page, 1, '置入条码');
  const barcode = await waitFor('新放的条码被选中', () => selectedComponent(page), 5_000);
  assert.deepEqual(barcode.component, {
    kind: 'code128',
    values: { content: 'SAMPLE-0001', size: 36 },
  });
  assert.equal(barcode.added, true);
  assert.ok(
    Math.abs(barcode.bbox.x - 60) < 0.5 && Math.abs(barcode.bbox.y - 80) < 0.5,
    `条码的左上角应当在点下的位置：${JSON.stringify(barcode.bbox)}`,
  );
  assert.equal(await placing(page), null);
  await parkMouse(page);
  await tilesSettled(page, 1);
  const darkAfter = await darkShare(page, area);
  assert.ok(darkAfter - darkBefore > 0.2, `条码画出来了：深色像素 ${darkBefore} → ${darkAfter}`);
  await page.screenshot({ path: join(shots, 'components-placed.png') });

  // 2. 属性面板改内容：重新生成，左上角不动，变窄
  const content = page.locator('#mg-comp-content');
  assert.equal(await content.inputValue(), 'SAMPLE-0001');
  await content.fill('BL-0042');
  await content.press('Enter');
  await editSettled(page, 2, '改条码内容');
  const updated = await waitFor(
    '属性面板改过的内容到了对象上',
    async () => {
      const found = await selectedComponent(page);
      return found?.component.values.content === 'BL-0042' ? found : null;
    },
    5_000,
  );
  assert.equal(updated.id, barcode.id, '改参数不换编号');
  assert.ok(Math.abs(updated.bbox.x - barcode.bbox.x) < 0.01);
  assert.ok(Math.abs(updated.bbox.y - barcode.bbox.y) < 0.01);
  assert.ok(updated.bbox.width < barcode.bbox.width);

  // 写中文：提示原因，不发命令
  await content.fill('提单号');
  await content.press('Enter');
  const alert = page.locator('.mg-panel [role="alert"]');
  await alert.waitFor({ timeout: 2_000 });
  assert.match(await alert.textContent(), /ASCII 32–126/);
  await sleep(300);
  assert.equal((await documentState(page)).version, 2, '不合法的参数不发给引擎');
  await content.press('Escape');

  // 3. 方向键微移：只能叠加内容的页上，新放的组件也能动
  const center = await onPage(
    page,
    updated.bbox.x + updated.bbox.width / 2,
    updated.bbox.y + updated.bbox.height / 2,
  );
  await page.mouse.click(center.x, center.y);
  await page.keyboard.press('ArrowRight');
  await editSettled(page, 3, '微移条码');
  const nudged = await selectedComponent(page);
  assert.ok(nudged.bbox.x > updated.bbox.x, '条码往右挪了');

  // 4. 把“文本”拖到页面上
  const dropAt = await onPage(page, 60, 200);
  const overlay = page.locator('.mg-canvas-overlay');
  const box = await overlay.boundingBox();
  await page
    .locator('.mg-comp-item:not([disabled])', { hasText: '文本' })
    .first()
    .dragTo(overlay, { targetPosition: { x: dropAt.x - box.x, y: dropAt.y - box.y } });
  await editSettled(page, 4, '拖进文本');
  const text = await waitFor(
    '拖进来的文本被选中',
    async () => {
      const found = await selectedComponent(page);
      return found?.component.kind === 'text' ? found : null;
    },
    5_000,
  );
  assert.deepEqual(text.component.values, { content: 'Text', size: 12 });
  assert.ok(Math.abs(text.bbox.x - 60) < 2, `文本放在松手的地方：${JSON.stringify(text.bbox)}`);

  // 文本写中文：引擎嵌一份系统字体的子集重新生成
  await page.locator('#mg-comp-content').fill('收货人：上海');
  await page.locator('#mg-comp-content').press('Enter');
  await editSettled(page, 5, '文本改成中文');
  const chinese = await waitFor(
    '文本组件换成了中文',
    async () => {
      const found = await selectedComponent(page);
      return found?.component.values.content === '收货人：上海' ? found : null;
    },
    5_000,
  );
  assert.ok(chinese.bbox.width > 50, `中文画出来了：${JSON.stringify(chinese.bbox)}`);
  await parkMouse(page);
  await page.screenshot({ path: join(shots, 'components-chinese.png') });

  // 5. 点了组件又按 Esc：不放了
  await page.locator('.mg-comp-item:not([disabled])', { hasText: '文本' }).first().click();
  assert.equal(await placing(page), 'text');
  await page.keyboard.press('Escape');
  assert.equal(await placing(page), null);

  // 6. 撤销五步：文本改成中文、拖进文本、微移、改内容、置入条码（点画布让输入框交出焦点再按 Ctrl+Z）
  const corner = await page.evaluate(() => {
    const rect = window.__magpdf.controller().container.getBoundingClientRect();
    return { x: rect.left + 6, y: rect.top + 6 };
  });
  await page.mouse.click(corner.x, corner.y);
  for (let version = 6; version <= 10; version++) {
    await page.keyboard.press('Control+z');
    await editSettled(page, version, `第 ${version - 5} 次撤销`);
  }
  const after = await documentState(page);
  assert.equal(after.objects, before.objects, '撤销之后对象数回到原样');
  await parkMouse(page);
  await page.screenshot({ path: join(shots, 'components-undone.png') });

  console.log(
    JSON.stringify({
      page: pageFidelity,
      objects: before.objects,
      darkShare: { before: +darkBefore.toFixed(3), after: +darkAfter.toFixed(3) },
      barcodeWidthPt: {
        placed: +barcode.bbox.width.toFixed(1),
        updated: +updated.bbox.width.toFixed(1),
      },
      dropped: text.component.kind,
      undone: true,
    }),
  );
} finally {
  await browser?.close().catch(() => undefined);
  app.kill();
  await sleep(200);
  await cleanUp(scratch);
}
