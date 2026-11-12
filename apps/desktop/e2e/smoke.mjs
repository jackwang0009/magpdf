/**
 * 界面端到端冒烟测试（方案 13.2 第 8 项；《测试计划》“界面端到端”）。
 *
 * 起真实的桌面程序（主进程 + 引擎进程 + WebView2），Playwright 经 CDP 连进 WebView2，用真实的鼠标键盘
 * 事件走一遍：
 *   命令行打开两份 PDF → 页面由引擎渲染出来 → 滚轮缩放 → 强制结束引擎后自动恢复显示
 *   → 只能叠加内容的页：出提示条，原有对象拖不动
 *   → 能改的页：点选、拖动（拖到一半对象本身已经跟着走）、微移、删除、撤销，看画面；再拖两秒量帧间隔
 *   → 关标签页 → 主进程退出后引擎跟着退出。
 *
 * 先构建发布版（界面打进可执行文件里）：
 *   pnpm --filter @magpdf/desktop build
 *   cargo build --release -p magpdf-engine-proc
 *   cargo build --release -p magpdf-desktop --features tauri/custom-protocol
 * 再运行：
 *   pnpm --filter @magpdf/desktop e2e
 *
 * 环境变量：
 *   MAGPDF_DESKTOP_BIN   被测的可执行文件，默认 target/release/magpdf-desktop.exe
 *   MAGPDF_E2E_PDF       看渲染、缩放、恢复用的 PDF，默认 tests/corpus/synthetic/freight-bl-composite.pdf。
 *                        它的第 1 页在原版 PDFium 下只能叠加内容，在修过生成器的构建下可以修改，两种都认
 *   MAGPDF_E2E_EDIT_PDF  做修改用的 PDF，第 1 页必须能修改，默认 tests/corpus/synthetic/color-devicegray.pdf
 *   MAGPDF_E2E_WINDOW    被测程序的窗口大小，写成 宽x高（屏幕像素，比如 1920x1040）；不设就用程序默认的大小。
 *                        量帧间隔时用它换一个大一些的画布
 *   MAGPDF_E2E_DRAG_MS   量帧间隔时拖多久（毫秒），默认 2000。超过 2000 时结果里多一项 overBySecond：
 *                        每一秒里有几帧超过 16.7 ms
 *
 * 结果打印成一行 JSON（各步耗时，修改的延迟，拖动预览就绪的延迟和拖动时的帧间隔），截图存到 tests/out/e2e/。
 * 测试窗口不抢焦点，不动真实的鼠标键盘。
 */
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import {
  cleanUp,
  connect,
  documentState,
  editSettled,
  enginePid,
  installEditTimer,
  isRunning,
  launchApp,
  pageRectOnScreen,
  parkMouse,
  parseDuration,
  parseWindowSize,
  pickTarget,
  repo,
  resizeWindow,
  sleep,
  summarize,
  tilesSettled,
  waitFor,
} from './harness.mjs';
const appBin = resolve(
  process.env.MAGPDF_DESKTOP_BIN ?? join(repo, 'target/release/magpdf-desktop.exe'),
);
const samplePdf = resolve(
  process.env.MAGPDF_E2E_PDF ?? join(repo, 'tests/corpus/synthetic/freight-bl-composite.pdf'),
);
const editPdf = resolve(
  process.env.MAGPDF_E2E_EDIT_PDF ?? join(repo, 'tests/corpus/synthetic/color-devicegray.pdf'),
);
const outDir = join(repo, 'tests/out/e2e');

/** 《P0 技术验证计划》V06 的通过线：强制结束引擎后 5 秒内恢复显示。 */
const RESTORE_LIMIT_MS = 5000;
/** 量修改延迟时微移多少次。 */
const NUDGES = 30;
/** 量拖动的帧间隔时拖多久（ms）。MAGPDF_E2E_DRAG_MS 可以改长：别的程序偶尔占一下显卡，两秒不一定碰得上。 */
const DRAG_MS = parseDuration(process.env.MAGPDF_E2E_DRAG_MS, 2000);
/** 被测程序的窗口要改成多大；没有设 MAGPDF_E2E_WINDOW 时是 null，用程序默认的大小。 */
const windowSize = parseWindowSize(process.env.MAGPDF_E2E_WINDOW);
/** 《交互设计》9.3 的文案。 */
const OVERLAY_ONLY_BANNER =
  '第 1 页无法安全修改原有内容：页面使用了软件尚不能完整保留的效果。可以在这一页上添加内容。';

/** 切到某份文档的标签页，等它的画面到齐。 */
async function activate(page, pdf) {
  const name = basename(pdf);
  await page.locator('.mg-doc-tab-title', { hasText: name }).click();
  await page.waitForFunction((title) => {
    const state = window.__magpdf.store.getState();
    return state.documents[state.activeDocumentId]?.snapshot.name === title;
  }, name);
  await tilesSettled(page, 1);
}

/**
 * 等引擎给出某份文档第 1 页保真校验的结论（方案 2.3）：页面显示之后引擎空闲时自己会做。
 * 返回 `{ status: 'ok' | 'overlayOnly', reason? }`。直接问引擎，因为界面上只有“只能叠加内容”才有表现。
 */
async function fidelityOfFirstPage(page, pdf) {
  const handle = await page.waitForFunction(
    async (title) => {
      const { invoke } = window.__TAURI_INTERNALS__;
      const doc = (await invoke('open_documents')).find((info) => info.title === title);
      if (!doc) return false;
      const snapshot = await invoke('engine_request', {
        method: 'doc.snapshot',
        params: { docId: doc.docId },
      });
      const { fidelity, fidelityReason } = snapshot.pages[0];
      return fidelity === 'pending' ? false : { status: fidelity, reason: fidelityReason ?? null };
    },
    basename(pdf),
    { timeout: 20_000, polling: 50 },
  );
  return handle.jsonValue();
}

/**
 * 最近截的几张图。测试失败时另存一份（`failureEvidence`）：比对两张截图的断言失败之后，要看的就是
 * 这两张图，而平常存下来的那些文件下一次运行就被盖掉了。
 */
const recentShots = [];

function remember(shot) {
  recentShots.push(shot);
  if (recentShots.length > 4) recentShots.shift();
  return shot;
}

/** 截一张图存下来，连同第 1 页所在的区域一起返回。先把鼠标挪开，等画面是最新的。 */
async function capture(page, name) {
  await parkMouse(page);
  await page.waitForFunction(() => window.__magpdf.controller().upToDate);
  const area = await pageRectOnScreen(page);
  assert.ok(area.width > 50 && area.height > 50, `页面没有显示在画布上：${JSON.stringify(area)}`);
  const png = await page.screenshot({ scale: 'css' });
  writeFileSync(join(outDir, `${name}.png`), png);
  return remember({ name, area, base64: png.toString('base64') });
}

/**
 * 数页面区域里的像素：白纸、有内容、既不白也不深的（没渲染出来时是画布的底色）。
 * 截图在页面里解码（不给 Node 这边加图像库）。
 */
async function inspectPage(page, name) {
  const shot = await capture(page, name);
  const counts = await page.evaluate(async ({ base64, area }) => {
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
  }, shot);
  return { ...counts, zoom: shot.area.zoom };
}

function assertPageIsDrawn(counts, when) {
  const white = counts.white / counts.total;
  const ink = counts.ink / counts.total;
  assert.ok(
    white > 0.3 && ink > 0.02 && ink < 0.6,
    `${when}：页面区域里白 ${(white * 100).toFixed(1)}%、有内容 ${(ink * 100).toFixed(1)}%，不像一页渲染好的单证`,
  );
}

/**
 * 不挪鼠标截一张图（拖到一半的时候用）。等画面是最新的；`area` 沿用 `like` 那张截图的页面区域。
 */
async function captureInPlace(page, name, like) {
  await page.waitForFunction(() => window.__magpdf.controller().upToDate);
  const png = await page.screenshot({ scale: 'css' });
  writeFileSync(join(outDir, `${name}.png`), png);
  return remember({ name, area: like.area, base64: png.toString('base64') });
}

/**
 * 两张截图有多大比例的像素不一样（任一通道差 8 以上才算）。
 * 默认比第 1 页所在的区域；给了 `region`（屏幕上的矩形，CSS 像素）就只比那一块；
 * 再给 `exclude` 的话，落在它里面的像素不算。要比的范围里一个像素也没有时返回 0。
 */
function pageDifference(page, before, after, region = before.area, exclude = null) {
  return page.evaluate(
    async ({ a, b, area, skip }) => {
      const [x0, y0] = [Math.ceil(area.x), Math.ceil(area.y)];
      const [width, height] = [Math.floor(area.width), Math.floor(area.height)];
      if (width <= 0 || height <= 0) return 0;
      const pixels = async (base64) => {
        const bytes = Uint8Array.from(atob(base64), (ch) => ch.charCodeAt(0));
        const bitmap = await createImageBitmap(new Blob([bytes], { type: 'image/png' }));
        const canvas = new OffscreenCanvas(bitmap.width, bitmap.height);
        const context = canvas.getContext('2d');
        context.drawImage(bitmap, 0, 0);
        return context.getImageData(x0, y0, width, height).data;
      };
      const [left, right] = await Promise.all([pixels(a), pixels(b)]);
      const skipped = (x, y) =>
        skip !== null &&
        x >= skip.x &&
        x < skip.x + skip.width &&
        y >= skip.y &&
        y < skip.y + skip.height;
      let compared = 0;
      let different = 0;
      for (let y = 0; y < height; y++) {
        for (let x = 0; x < width; x++) {
          if (skipped(x0 + x, y0 + y)) continue;
          compared++;
          const i = (y * width + x) * 4;
          if (
            Math.abs(left[i] - right[i]) > 8 ||
            Math.abs(left[i + 1] - right[i + 1]) > 8 ||
            Math.abs(left[i + 2] - right[i + 2]) > 8
          ) {
            different++;
          }
        }
      }
      return compared === 0 ? 0 : different / compared;
    },
    { a: before.base64, b: after.base64, area: region, skip: exclude },
  );
}

function boxOf(page, id) {
  return page.evaluate((objectId) => {
    const state = window.__magpdf.store.getState();
    const doc = state.documents[state.activeDocumentId];
    return doc.snapshot.pages[0].objects.find((object) => object.id === objectId)?.bbox ?? null;
  }, id);
}

/** 按住对象的中心拖 (dx, dy) 个 CSS 像素，中途停一下让调用方看一眼，再松开。 */
async function drag(page, target, dx, dy, midway = async () => undefined) {
  await page.mouse.move(target.x, target.y);
  await page.mouse.down();
  await page.mouse.move(target.x + dx, target.y + dy, { steps: 6 });
  await midway();
  await page.mouse.up();
}

/**
 * 对象拖动 (dx, dy) 前后在屏幕上占的地方（CSS 像素）：
 * `home` 是原来的地方，`landed` 是挪过去之后的地方，`landedInner` 是后者往里收一点。
 * `home` 和 `landedInner` 四周往里收了几个像素，躲开选框的线。
 */
function dragRegions(target, dx, dy) {
  const width = target.bbox.width * target.scale;
  const height = target.bbox.height * target.scale;
  const inset = (size) => Math.min(3, Math.max(0, (size - 4) / 2));
  const inner = (x, y) => ({
    x: x + inset(width),
    y: y + inset(height),
    width: width - 2 * inset(width),
    height: height - 2 * inset(height),
  });
  const [left, top] = [target.x - width / 2, target.y - height / 2];
  return {
    home: inner(left, top),
    landed: { x: left + dx, y: top + dy, width, height },
    landedInner: inner(left + dx, top + dy),
  };
}

/** 等拖动预览的两层瓦片到齐（方案 3.2），返回它的情况：用了多久、两层各几块瓦片。 */
async function dragPreviewReady(page) {
  const handle = await page.waitForFunction(
    () => {
      const preview = window.__magpdf.controller().dragPreview;
      return preview?.ready ? preview : false;
    },
    null,
    { timeout: 10_000, polling: 5 },
  );
  return handle.jsonValue();
}

/**
 * 量拖动时的帧间隔（《P0 技术验证计划》V04）：按住对象不停地挪 DRAG_MS 毫秒，页面里每一帧记一个时间。
 * 最后回到原处松手，没有位移，文档不变。
 */
async function measureDrag(page) {
  const target = await pickTarget(page);
  await page.mouse.click(target.x, target.y);
  await page.mouse.move(target.x, target.y);
  await page.mouse.down();
  await page.mouse.move(target.x + 12, target.y + 8, { steps: 2 });
  const preview = await dragPreviewReady(page);
  const before = await page.evaluate(() => {
    const { store } = window.__magpdf;
    const controller = window.__magpdf.controller();
    // 画布自己的帧统计（脚本时间）只在打开“帧时间统计”时才记
    if (!store.getState().showFrameStats) store.getState().toggleFrameStats();
    controller.stats.reset();
    const probe = { times: [], running: true };
    const tick = (time) => {
      if (!probe.running) return;
      probe.times.push(time);
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    window.__frameProbe = probe;
    const canvas = controller.container.querySelector('.mg-canvas-gl');
    return {
      engineTiles: controller.tileProvider.stats.engineTiles,
      // 每一帧要画多大一块、页面放大了多少：帧时间要对着这两样看
      canvas: [canvas.width, canvas.height],
      zoom: controller.view.zoom,
    };
  });

  const started = performance.now();
  let moves = 0;
  while (performance.now() - started < DRAG_MS) {
    const angle = (moves++ / 40) * 2 * Math.PI;
    await page.mouse.move(
      target.x + 70 + 60 * Math.cos(angle),
      target.y + 40 + 35 * Math.sin(angle),
    );
  }

  const measured = await page.evaluate(() => {
    const controller = window.__magpdf.controller();
    const probe = window.__frameProbe;
    probe.running = false;
    const intervals = [];
    for (let i = 1; i < probe.times.length; i++)
      intervals.push(probe.times[i] - probe.times[i - 1]);
    const stats = controller.stats.summary();
    return {
      intervals,
      script: { median: stats.workP50, p95: stats.workP95, max: stats.workMax },
      engineTiles: controller.tileProvider.stats.engineTiles,
      stillLayered: controller.dragPreview?.ready === true,
    };
  });
  await page.mouse.move(target.x, target.y);
  await page.mouse.up();
  await page.evaluate(() => {
    const state = window.__magpdf.store.getState();
    if (state.showFrameStats) state.toggleFrameStats();
  });
  await page.keyboard.press('Escape');

  assert.ok(measured.stillLayered, '拖动过程中分层预览没有一直在用');
  assert.equal(
    measured.engineTiles,
    before.engineTiles,
    '拖动过程中不该再向引擎要瓦片（方案 3.2）',
  );
  const round = (value) => Math.round(value * 10) / 10;
  // 超过 16.7 ms 的帧落在第几秒：别的程序隔一阵占一下显卡造成的掉帧是一簇一簇的
  const overBySecond = Array.from({ length: Math.ceil(DRAG_MS / 1000) }, () => 0);
  let elapsed = 0;
  for (const interval of measured.intervals) {
    elapsed += interval;
    const second = Math.min(overBySecond.length - 1, Math.floor(elapsed / 1000));
    if (interval > 16.7) overBySecond[second]++;
  }
  return {
    readyMs: round(preview.readyMs),
    baseTiles: preview.baseTiles,
    objectTiles: preview.objectTiles,
    canvas: before.canvas,
    zoom: Math.round(before.zoom * 1000) / 1000,
    frames: measured.intervals.length,
    intervalMs: summarize(measured.intervals),
    over16ms: measured.intervals.filter((interval) => interval > 16.7).length,
    ...(DRAG_MS > 2000 ? { overBySecond } : {}),
    scriptMs: {
      median: round(measured.script.median),
      p95: round(measured.script.p95),
      max: round(measured.script.max),
    },
  };
}

/** 只能叠加内容的页（方案 2.3）：提示条在，原有对象能选中但拖不动、删不掉，画面不变。 */
async function checkOverlayOnlyPage(page) {
  const banner = page.locator('.mg-doc-banner:visible');
  await banner.waitFor({ timeout: 5000 });
  assert.equal(await banner.textContent(), OVERLAY_ONLY_BANNER);

  const before = await capture(page, 'smoke-4-overlay-only');
  const target = await pickTarget(page);
  await page.mouse.click(target.x, target.y);
  assert.deepEqual((await documentState(page)).selection, [target.id]);
  // 属性面板顶部说明属性只能查看
  assert.equal(await page.locator('.mg-props-readonly').count(), 1);

  let cursor = null;
  await drag(page, target, 40, 24, async () => {
    cursor = await page.evaluate(
      () => window.__magpdf.controller().container.lastElementChild.style.cursor,
    );
    // 拖的过程中对象也不跟着走
    assert.deepEqual(await boxOf(page, target.id), target.bbox);
  });
  assert.equal(cursor, 'not-allowed', '拖不能修改的对象时，光标应当提示“不行”');
  await page.keyboard.press('Delete');
  await page.keyboard.press('ArrowRight');
  await sleep(300);

  const state = await documentState(page);
  assert.equal(state.version, 0, '只能叠加内容的页不该被改动');
  assert.deepEqual(await boxOf(page, target.id), target.bbox);
  assert.equal(await page.locator('.mg-notice').count(), 0, '不该弹提示框');
  await page.keyboard.press('Escape');
  const after = await capture(page, 'smoke-4-overlay-only-after');
  const changed = await pageDifference(page, before, after);
  assert.ok(changed < 0.001, `页面的画面变了 ${(changed * 100).toFixed(2)}%`);
}

/**
 * 能改的页：点选 → 拖动 → 微移若干次（量延迟）→ 删除 → 全部撤销，每一步看文档和画面。
 * 返回修改的延迟（毫秒）。
 */
async function checkEditing(page) {
  assert.equal(await page.locator('.mg-doc-banner:visible').count(), 0, '能改的页不该有提示条');
  const initial = await documentState(page);
  const pristine = await capture(page, 'smoke-5-before-edit');
  const target = await pickTarget(page);
  let version = initial.version;

  // 点选
  await page.mouse.click(target.x, target.y);
  assert.deepEqual((await documentState(page)).selection, [target.id]);

  // 拖动：按住拖出去，中途停住看一眼，再松手。对象跟到新位置，页面上受影响的地方重画
  const [dx, dy] = [40, 24];
  let preview = null;
  let midDrag = null;
  await drag(page, target, dx, dy, async () => {
    // 两层瓦片到齐之后，对象本身已经跟着鼠标走了（方案 3.2）
    preview = await dragPreviewReady(page);
    midDrag = await captureInPlace(page, 'smoke-5-dragging', pristine);
  });
  await editSettled(page, ++version, '拖动');
  const moved = await boxOf(page, target.id);
  assert.ok(
    Math.abs(moved.x - (target.bbox.x + dx / target.scale)) < 0.5 &&
      Math.abs(moved.y - (target.bbox.y + dy / target.scale)) < 0.5,
    `对象没有挪到该去的地方：${JSON.stringify(target.bbox)} → ${JSON.stringify(moved)}`,
  );
  await page.keyboard.press('Escape');
  const afterMove = await capture(page, 'smoke-5-moved');
  const moveChange = await pageDifference(page, pristine, afterMove);
  assert.ok(moveChange > 0.0005, `拖动之后页面的画面没有变（${(moveChange * 100).toFixed(3)}%）`);
  // 拖到一半的时候，对象腾出来的地方已经空了，和松手之后一样——而不是对象还留在原处、只多一个虚线框。
  // 只比腾出来的那部分（原来的地方减去对象现在占的地方）。
  const { home, landed, landedInner } = dragRegions(target, dx, dy);
  const vacated = await pageDifference(page, pristine, midDrag, home, landed);
  const settledGap = await pageDifference(page, midDrag, afterMove, home, landed);
  const percent = (value) => `${(value * 100).toFixed(1)}%`;
  assert.ok(
    vacated > 0.03,
    `拖动中途对象还留在原处：腾出来的地方只有 ${percent(vacated)} 的像素变了`,
  );
  assert.ok(
    settledGap < vacated / 4,
    `拖动中途的画面和松手之后对不上：对象腾出来的地方有 ${percent(settledGap)} 的像素不同（对象离开造成的变化是 ${percent(vacated)}）`,
  );
  // 对象在新位置上的像素，预览和最终结果有多少不一样。预览是把渲染好的位图按整像素挪过去，最终结果是
  // 引擎在新位置重新渲染的：视图的缩放不在瓦片的档位上时，文字和图像边缘的深浅会不一样；对象被别的对象
  // 压着、或者带混合模式时差得更多（V04 报告第 4 节）。只记下来，不作判定。
  const previewMismatch = await pageDifference(page, midDrag, afterMove, landedInner);

  // 微移：来回各一步，量从按键到 doc.changed、到画面重画完的时间
  await page.mouse.click(target.x + dx, target.y + dy);
  assert.deepEqual((await documentState(page)).selection, [target.id]);
  await parkMouse(page);
  await installEditTimer(page);
  for (let i = 0; i < NUDGES; i++) {
    await page.keyboard.press(i % 2 === 0 ? 'ArrowRight' : 'ArrowLeft');
    await page.waitForFunction((done) => window.__editTimer.samples.length === done, i + 1, {
      timeout: 20_000,
      polling: 5,
    });
    await editSettled(page, ++version, `第 ${i + 1} 次微移`);
  }
  const samples = await page.evaluate(() => window.__editTimer.samples);
  // 原始样本留一份，汇总里只有分位数
  writeFileSync(join(outDir, 'edit-latency.json'), JSON.stringify(samples, null, 2));
  const latency = {
    objects: initial.objects,
    edits: samples.length,
    changedMs: summarize(samples.map((sample) => sample.changed)),
    repaintedMs: summarize(samples.map((sample) => sample.repainted)),
    // 第一次拖动：从开始拖到两层瓦片到齐
    previewReadyMs: Math.round(preview.readyMs * 10) / 10,
    // 对象在新位置上，预览和松手后重新渲染的结果有多大比例的像素不一样
    previewMismatch: Math.round(previewMismatch * 1000) / 1000,
  };

  // 删除
  await page.keyboard.press('Delete');
  await editSettled(page, ++version, '删除');
  assert.equal((await documentState(page)).objects, initial.objects - 1);
  assert.equal(await boxOf(page, target.id), null);

  // 撤销到头：对象回到原处，画面和修改之前一样。再多按一次什么也不发生
  const steps = version - initial.version;
  for (let i = 0; i < steps; i++) {
    await page.keyboard.press('Control+z');
    await editSettled(page, ++version, `第 ${i + 1} 次撤销`);
  }
  await page.keyboard.press('Control+z');
  await sleep(300);
  const state = await documentState(page);
  assert.equal(state.version, version, '没有可撤销的操作时，文档不该变');
  assert.equal(state.objects, initial.objects);
  const restored = await boxOf(page, target.id);
  assert.ok(
    Math.abs(restored.x - target.bbox.x) < 0.01 && Math.abs(restored.y - target.bbox.y) < 0.01,
    `撤销之后对象没有回到原处：${JSON.stringify(target.bbox)} → ${JSON.stringify(restored)}`,
  );
  assert.equal(await page.locator('.mg-notice').count(), 0, '不该弹提示框');
  await page.keyboard.press('Escape');
  const afterUndo = await capture(page, 'smoke-5-undone');
  const undoChange = await pageDifference(page, pristine, afterUndo);
  assert.ok(
    undoChange < moveChange / 10,
    `全部撤销之后画面和修改之前差 ${(undoChange * 100).toFixed(3)}%（拖动造成的差别是 ${(moveChange * 100).toFixed(3)}%）`,
  );
  return latency;
}

/**
 * 测试失败时界面是什么样子：存一张截图，把失败之前截的最后几张图也另存一份，
 * 返回打开的文档、活动文档的状态和正在显示的提示。
 */
async function failureEvidence(page) {
  // 失败之前截的最后几张图另存一份，文件名前面加 failure-
  const earlier = recentShots.map((shot) => {
    const path = join(outDir, `failure-${shot.name}.png`);
    writeFileSync(path, Buffer.from(shot.base64, 'base64'));
    return basename(path);
  });
  if (!page) return { unavailable: '还没有连上界面', earlier };
  const screenshot = join(outDir, 'smoke-failure.png');
  writeFileSync(screenshot, await page.screenshot({ scale: 'css' }));
  const seen = await page.evaluate(() => {
    const state = window.__magpdf?.store.getState();
    const doc = state?.documents[state.activeDocumentId];
    return {
      documents: state?.documentOrder.map((id) => state.documents[id].snapshot.name) ?? null,
      active: doc
        ? {
            name: doc.snapshot.name,
            version: doc.snapshot.version,
            selection: doc.selection,
            objects: doc.snapshot.pages[0]?.objects.length ?? null,
            overlayOnly: doc.snapshot.pages[0]?.overlayOnly ?? null,
          }
        : null,
      notice: document.querySelector('.mg-notice-message')?.textContent ?? null,
      banner: document.querySelector('.mg-doc-banner')?.textContent ?? null,
    };
  });
  return { screenshot, earlier, ...seen };
}

async function main() {
  for (const [what, path] of [
    ['桌面程序', appBin],
    ['引擎', join(dirname(appBin), 'magpdf-engine.exe')],
    ['样本 PDF', samplePdf],
    ['做修改用的 PDF', editPdf],
  ]) {
    assert.ok(existsSync(path), `找不到${what}：${path}（构建步骤见本文件开头）`);
  }
  mkdirSync(outDir, { recursive: true });
  const names = [basename(editPdf), basename(samplePdf)];
  const result = { app: basename(appBin), pdf: names[1], editPdf: names[0] };

  const launched = performance.now();
  // 后给的那一份是活动文档：先在样本上看渲染、缩放和恢复，再切到另一份做修改
  const { app, port, scratch } = await launchApp(appBin, [editPdf, samplePdf]);
  let browser;
  let page;
  let engine = null;
  const problems = [];
  try {
    const session = await connect(port);
    browser = session.browser;
    page = session.page;
    page.on('pageerror', (error) => problems.push(`页面里抛了异常：${error.message}`));
    page.on('console', (message) => {
      if (message.type() !== 'error') return;
      // 引擎被结束的那一刻，正在取的瓦片会得到 410（槽位作废），界面随后重新请求，这是协议规定的走法；
      // 浏览器会把每个非 2xx 的响应记成一条控制台错误，不算问题。
      if (/status of 410/.test(message.text())) return;
      problems.push(`控制台报错：${message.text()}`);
    });

    // 1. 命令行给的文档已经打开，页面由引擎渲染出来
    await page.waitForFunction(
      () => document.querySelectorAll('.mg-doc-tab-title').length === 2,
      null,
      {
        timeout: 20_000,
      },
    );
    assert.deepEqual(await page.locator('.mg-doc-tab-title').allTextContents(), names);
    let completed = await tilesSettled(page, 1);
    result.firstPaintMs = Math.round(performance.now() - launched);
    if (windowSize) {
      // 首屏量完之后再改窗口大小，免得把起 PowerShell 的时间算进去
      resizeWindow(app.pid, windowSize);
      await page.waitForFunction(
        (width) => Math.abs(window.innerWidth * window.devicePixelRatio - width) <= 48,
        windowSize.width,
        { timeout: 10_000 },
      );
      result.window = await page.evaluate(() => [window.innerWidth, window.innerHeight]);
      completed = await tilesSettled(page, completed);
    }
    // 画布的 WebGL 跑在哪块显卡上（也可能是软件渲染）、屏幕的缩放：帧时间要对着这些看。
    // 画布有多大记在 drag 一项里（量帧间隔的那一刻）。
    result.display = await page.evaluate(() => {
      const gl = document
        .createElement('canvas')
        .getContext('webgl2', { powerPreference: 'high-performance' });
      const info = gl?.getExtension('WEBGL_debug_renderer_info');
      return {
        gpu: gl && info ? gl.getParameter(info.UNMASKED_RENDERER_WEBGL) : null,
        devicePixelRatio: window.devicePixelRatio,
      };
    });
    assert.equal((await documentState(page)).name, names[1]);
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
    assert.deepEqual(await page.locator('.mg-doc-tab-title').allTextContents(), names);
    const replacement = enginePid(app.pid);
    assert.ok(replacement && replacement !== engine, '引擎没有换成新的进程');
    engine = replacement;
    assert.ok(
      result.restoreMs < RESTORE_LIMIT_MS,
      `结束引擎后 ${result.restoreMs} ms 才恢复显示，超过 ${RESTORE_LIMIT_MS} ms`,
    );

    // 4. 保真校验：没有人要求，引擎在页面显示之后自己做。样本的第 1 页通不过的话只能叠加内容
    await page.keyboard.press('Control+0');
    await tilesSettled(page, 1);
    result.sampleFidelity = (await fidelityOfFirstPage(page, samplePdf)).status;
    if (result.sampleFidelity === 'overlayOnly') await checkOverlayOnlyPage(page);
    else assert.equal(await page.locator('.mg-doc-banner:visible').count(), 0);

    // 5. 修改：切到能改的那一份
    await activate(page, editPdf);
    if (windowSize) {
      // 窗口改过大小：这一份也按新的画布重新“适合页面”，页面有多大就画多大
      await page.keyboard.press('Control+0');
      await tilesSettled(page, 1);
    }
    const editable = await fidelityOfFirstPage(page, editPdf);
    assert.equal(
      editable.status,
      'ok',
      `${names[0]} 的第 1 页在这个 PDFium 构建下不能修改（引擎给的原因：${editable.reason}），用 MAGPDF_E2E_EDIT_PDF 换一份`,
    );
    result.edit = await checkEditing(page);
    // 文档已经撤销回原样：再按住同一个对象拖一阵（默认两秒），量帧间隔
    result.drag = await measureDrag(page);

    // 6. 关掉标签页，文档从界面上消失
    for (let left = 2; left > 0; left--) {
      await page.locator('.mg-doc-tab-close').first().click();
      await page.waitForFunction((n) => window.__magpdf.documents().length === n, left - 1);
    }

    assert.deepEqual(problems, [], '运行过程中页面报了错');
  } catch (error) {
    // 留下现场：失败那一刻的画面、界面上的文档和提示、页面报过的错。取不到也不要盖住原来的错误
    const evidence = await failureEvidence(page).catch((reason) => ({
      unavailable: String(reason),
    }));
    console.error(`失败时的现场：${JSON.stringify({ ...evidence, problems })}`);
    throw error;
  } finally {
    await browser?.close().catch(() => undefined);
    app.kill();
  }

  // 7. 主进程没了，引擎自己退出，不留下没人管的进程
  if (engine) {
    const gone = performance.now();
    await waitFor('引擎随主进程退出', () => !isRunning(engine), 5000);
    result.engineExitMs = Math.round(performance.now() - gone);
  }
  console.log(JSON.stringify(result));
  await cleanUp(scratch);
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
