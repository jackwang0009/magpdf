/**
 * 旋转：选中红方块，在选框角外的旋转热区按住，按着 Shift 转 45°（吸附到 15° 的倍数），松手后引擎绕方块中心
 * 转过去；量画面：原来的角上变白，右边多出来的一块变红。Ctrl+Z 撤销回到原样。再在属性面板的“旋转”里输入
 * -45 回车，一样转过去。
 *
 * 用 color-devicergb.pdf（原版 PDFium 下能修改）。
 *
 * 先构建发布版（步骤见 smoke.mjs 开头），再运行：
 *   node apps/desktop/e2e/rotate.mjs
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

/** 面积最大的几个路径里最靠左上的那个（红方块）。 */
function firstSquare(page) {
  return page.evaluate(() => {
    const state = window.__magpdf.store.getState();
    const objects = state.documents[state.activeDocumentId].snapshot.pages[0].objects;
    const squares = objects.filter(
      (o) => o.type === 'path' && o.pickable !== false && o.bbox.width > 40 && o.bbox.height > 40,
    );
    squares.sort((a, b) => a.bbox.y - b.bbox.y || a.bbox.x - b.bbox.x);
    return squares[0] ?? null;
  });
}

/** 截图里这一小块的平均颜色（0–255）。PNG 交给页面自己解码，不另装依赖。 */
async function averageColor(page, at) {
  const png = await page.screenshot({ clip: { x: at.x - 2, y: at.y - 2, width: 4, height: 4 } });
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

const isRed = ([r, g, b]) => r > 150 && g < 100 && b < 100;
const isWhite = (color) => color.every((c) => c > 230);

const { app, port, scratch } = await launchApp(appBin, [pdf]);
let browser;
try {
  let page;
  ({ browser, page } = await connect(port));
  await tilesSettled(page, 1);
  await parkMouse(page);

  const square = await firstSquare(page);
  assert.ok(square, '第 1 页上没有找到方块');
  const { x, y, width, height } = square.bbox;
  const cx = x + width / 2;
  const cy = y + height / 2;
  // 量颜色的两点：原来的角里面一点；方块右边外面一点（转 45° 之后落进菱形里）
  const cornerPt = await onPage(page, x + 3, y + 3);
  const besidePt = await onPage(page, cx + width * 0.62, cy);
  assert.ok(isRed(await averageColor(page, cornerPt)), '角上原来是红的');
  assert.ok(isWhite(await averageColor(page, besidePt)), '右边原来是白的');

  // 选中，再在左上角外 12 个像素（旋转热区）按住，按着 Shift 转 45°
  const center = await onPage(page, cx, cy);
  await page.mouse.click(center.x, center.y);
  await waitFor(
    '方块被选中',
    async () => (await documentState(page)).selection.join() === square.id,
    3_000,
  );
  const corner = await onPage(page, x, y);
  const from = { x: corner.x - 12, y: corner.y - 12 };
  const angle = Math.PI / 4 + 0.05;
  const dx = from.x - center.x;
  const dy = from.y - center.y;
  const to = {
    x: center.x + dx * Math.cos(angle) - dy * Math.sin(angle),
    y: center.y + dx * Math.sin(angle) + dy * Math.cos(angle),
  };
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.keyboard.down('Shift');
  for (let i = 1; i <= 8; i++) {
    const t = (angle * i) / 8;
    await page.mouse.move(
      center.x + dx * Math.cos(t) - dy * Math.sin(t),
      center.y + dx * Math.sin(t) + dy * Math.cos(t),
    );
  }
  await page.mouse.move(to.x, to.y);
  await page.screenshot({ path: join(shots, 'rotate-dragging.png') });
  await page.mouse.up();
  await page.keyboard.up('Shift');
  await editSettled(page, 1, '旋转 45°');
  await parkMouse(page);
  await tilesSettled(page, 1);

  const rotated = await page.evaluate((id) => {
    const state = window.__magpdf.store.getState();
    return state.documents[state.activeDocumentId].snapshot.pages[0].objects.find(
      (o) => o.id === id,
    ).bbox;
  }, square.id);
  // 正方形转 45°：包围盒的边长是原来的 √2 倍，中心不动
  assert.ok(
    Math.abs(rotated.width / width - Math.SQRT2) < 0.02,
    `包围盒 ${JSON.stringify(rotated)}`,
  );
  assert.ok(Math.abs(rotated.x + rotated.width / 2 - cx) < 0.5, '中心不动');
  const cornerAfter = await averageColor(page, cornerPt);
  const besideAfter = await averageColor(page, besidePt);
  assert.ok(isWhite(cornerAfter), `原来的角上变白：${cornerAfter}`);
  assert.ok(isRed(besideAfter), `右边多出来的一块变红：${besideAfter}`);
  await page.screenshot({ path: join(shots, 'rotate-done.png') });

  // 撤销（选中状态下按 Ctrl+Z）
  await page.keyboard.press('Control+z');
  await editSettled(page, 2, '撤销旋转');
  await parkMouse(page);
  await tilesSettled(page, 1);
  assert.ok(isRed(await averageColor(page, cornerPt)), '撤销之后角上又是红的');
  assert.ok(isWhite(await averageColor(page, besidePt)), '撤销之后右边又是白的');

  // 属性面板：输入 -45 回车，同样绕中心转过去（逆时针 45°，正方形看起来一样），输入框清空
  const field = page.locator('input[aria-label="旋转"]');
  await field.fill('-45');
  await field.press('Enter');
  await editSettled(page, 3, '属性面板里转 -45°');
  assert.equal(await field.inputValue(), '');
  await parkMouse(page);
  await tilesSettled(page, 1);
  assert.ok(isWhite(await averageColor(page, cornerPt)), '属性面板转过之后角上变白');
  assert.ok(isRed(await averageColor(page, besidePt)), '属性面板转过之后右边变红');

  console.log(
    JSON.stringify({
      object: square.id,
      bbox: {
        before: [width, height],
        after: [+rotated.width.toFixed(1), +rotated.height.toFixed(1)],
      },
      corner: cornerAfter,
      beside: besideAfter,
      undone: true,
    }),
  );
} finally {
  await browser?.close().catch(() => undefined);
  app.kill();
  await sleep(200);
  await cleanUp(scratch);
}
