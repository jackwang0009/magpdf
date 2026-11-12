/**
 * 界面一侧的性能测量（《P0 技术验证计划》V05，指标见方案第 7 节）。
 *
 * 起真实的桌面程序（主进程 + 引擎进程 + WebView2），Playwright 经 CDP 连进 WebView2，用真实的鼠标键盘事件量：
 *   first   打开多页文档到首屏画完。从启动程序算起，拆成三段：WebView2 起来、界面加载到开始接入文档、
 *           开始接入文档到首屏瓦片画齐（最后一段相当于在开着的程序里打开文档）。每次都重新启动程序
 *   scroll  同一份文档上平移、缩放、跳页时的帧间隔（界面自带的基准，瓦片由引擎渲染）
 *   hover   对象很多的页上鼠标来回移动时的帧间隔，以及点选到画出选框用了多久
 *   edit    单对象修改到重画完：选中一个对象按方向键微移，每次记收到 doc.changed、画面重画完各用了多久，
 *           向引擎取了几块瓦片、画布收到了几块瓦片
 *
 * 先构建发布版（步骤见 smoke.mjs 开头）、生成样本（python tests/corpus/perf/generate.py），再运行：
 *   node apps/desktop/e2e/perf.mjs
 *
 * 环境变量：
 *   MAGPDF_DESKTOP_BIN    被测的可执行文件，默认 target/release/magpdf-desktop.exe；引擎要在同一个目录
 *   MAGPDF_PERF_PDF       多页文档（first、scroll），默认 tests/out/perf/report-300-pages.pdf
 *   MAGPDF_PERF_EDIT_PDF  对象很多的一页（hover、edit），第 1 页要能修改，默认 tests/out/perf/drawing-5000-objects.pdf
 *   MAGPDF_PERF_WINDOW    hover、edit、scroll 时的窗口大小，宽x高（屏幕像素）；不设就用程序默认的大小
 *   MAGPDF_PERF_ONLY      只量其中几项，逗号分开，比如 edit,hover；不设就全量
 *   MAGPDF_PERF_LAUNCHES  first 启动几次，默认 3
 *   MAGPDF_PERF_EDITS     edit 微移几次，默认 40
 *   MAGPDF_PERF_OUT       完整结果（含每一次的原始数据）另存成这个 JSON 文件
 *
 * 结果打印成一行 JSON（只有分位数）。测试窗口不抢焦点，不动真实的鼠标键盘。
 */
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import {
  cleanUp,
  connect,
  documentState,
  editSettled,
  installEditTimer,
  launchApp,
  pageRectOnScreen,
  parkMouse,
  parseDuration,
  parseWindowSize,
  pickTarget,
  repo,
  resizeWindow,
  summarize,
  tilesSettled,
} from './harness.mjs';

const appBin = resolve(
  process.env.MAGPDF_DESKTOP_BIN ?? join(repo, 'target/release/magpdf-desktop.exe'),
);
const pagesPdf = resolve(
  process.env.MAGPDF_PERF_PDF ?? join(repo, 'tests/out/perf/report-300-pages.pdf'),
);
const editPdf = resolve(
  process.env.MAGPDF_PERF_EDIT_PDF ?? join(repo, 'tests/out/perf/drawing-5000-objects.pdf'),
);
const windowSize = parseWindowSize(process.env.MAGPDF_PERF_WINDOW, 'MAGPDF_PERF_WINDOW');
const SCENARIOS = ['first', 'scroll', 'hover', 'edit'];
const only = (process.env.MAGPDF_PERF_ONLY ?? '')
  .split(',')
  .map((name) => name.trim())
  .filter(Boolean);
for (const name of only) {
  assert.ok(SCENARIOS.includes(name), `MAGPDF_PERF_ONLY 里的 ${name} 不认识，可选：${SCENARIOS}`);
}
const wanted = (name) => only.length === 0 || only.includes(name);
const LAUNCHES = parseDuration(process.env.MAGPDF_PERF_LAUNCHES, 3);
const EDITS = parseDuration(process.env.MAGPDF_PERF_EDITS, 40);
/** 鼠标在页面上来回移动多久（ms） */
const HOVER_MS = 3000;
/** 点选几次 */
const CLICKS = 20;
/** 60 fps 的一帧 */
const FRAME_MS = 16.7;

const round = (value, digits = 1) => Math.round(value * 10 ** digits) / 10 ** digits;

/** 起程序、连上界面，跑 `body`，最后关掉程序、删临时目录。页面里抛了异常算失败。 */
async function withApp(files, body) {
  const { app, port, scratch, launched } = await launchApp(appBin, files);
  let browser;
  try {
    const session = await connect(port);
    browser = session.browser;
    const errors = [];
    session.page.on('pageerror', (error) => errors.push(error.message));
    const result = await body(session.page, { app, launched });
    assert.deepEqual(errors, [], '运行过程中页面报了错');
    return result;
  } finally {
    await browser?.close().catch(() => undefined);
    app.kill();
    await cleanUp(scratch);
  }
}

/** 窗口改成要的大小（给了的话），页面按画布重新“适合页面”，等画面到齐。 */
async function fitWindow(page, app) {
  if (windowSize) {
    resizeWindow(app.pid, windowSize);
    await page.waitForFunction(
      (width) => Math.abs(window.innerWidth * window.devicePixelRatio - width) <= 48,
      windowSize.width,
      { timeout: 10_000 },
    );
  }
  await page.keyboard.press('Control+0');
  await tilesSettled(page, 1);
}

/** 窗口、画布、缩放、显卡：帧间隔和重画时间要对着这些看。 */
function display(page) {
  return page.evaluate(() => {
    const controller = window.__magpdf.controller();
    const canvas = controller.container.querySelector('.mg-canvas-gl');
    const gl = document
      .createElement('canvas')
      .getContext('webgl2', { powerPreference: 'high-performance' });
    const info = gl?.getExtension('WEBGL_debug_renderer_info');
    return {
      window: [window.innerWidth, window.innerHeight],
      canvas: [canvas.width, canvas.height],
      devicePixelRatio: window.devicePixelRatio,
      zoom: Math.round(controller.view.zoom * 1000) / 1000,
      gpu: gl && info ? gl.getParameter(info.UNMASKED_RENDERER_WEBGL) : null,
    };
  });
}

/**
 * 首屏：等界面打的两个性能标记（`magpdf:open:` 开始接入文档，`magpdf:first-screen:` 视口里的瓦片第一次
 * 画齐，见 engineSession.ts、canvasController.ts），和程序启动的时刻、WebView2 页面的起点对起来。
 */
async function firstScreen(page, launched) {
  const handle = await page.waitForFunction(
    () => {
      const marks = performance
        .getEntriesByType('mark')
        .filter((mark) => mark.name.startsWith('magpdf:'))
        .map((mark) => ({ name: mark.name, at: mark.startTime }));
      if (!marks.some((mark) => mark.name.startsWith('magpdf:first-screen:'))) return false;
      return { origin: performance.timeOrigin, marks };
    },
    null,
    { timeout: 30_000, polling: 20 },
  );
  const { origin, marks } = await handle.jsonValue();
  const at = (prefix) => marks.find((mark) => mark.name.startsWith(prefix))?.at;
  const opened = at('magpdf:open:');
  const shown = at('magpdf:first-screen:');
  assert.ok(opened !== undefined && shown !== undefined, `性能标记不全：${JSON.stringify(marks)}`);
  return {
    totalMs: Math.round(origin + shown - launched),
    webviewMs: Math.round(origin - launched),
    uiMs: Math.round(opened),
    openToFirstScreenMs: round(shown - opened),
  };
}

/** 平移、缩放、跳页：界面自带的基准（`canvas/benchmark.ts`），每段 240 帧。 */
async function measureScroll(page) {
  const result = await page.evaluate(() => window.__magpdf.benchmark(240));
  return {
    pages: result.pages,
    viewport: [result.viewport.width, result.viewport.height],
    phases: result.phases.map((phase) => ({
      name: phase.name,
      frames: phase.samples,
      intervalMs: {
        median: round(phase.intervalP50),
        p95: round(phase.intervalP95),
        max: round(phase.intervalMax),
      },
      scriptMs: {
        median: round(phase.workP50),
        p95: round(phase.workP95),
        max: round(phase.workMax),
      },
      longFrames: phase.longFrames,
    })),
    tiles: {
      completed: result.tiles.completed,
      aborted: result.tiles.aborted,
      failed: result.tiles.failed,
    },
  };
}

/** 页面里记每一帧的时刻；打开画布自己的帧统计（脚本时间）。 */
function startFrameProbe(page) {
  return page.evaluate(() => {
    const { store } = window.__magpdf;
    if (!store.getState().showFrameStats) store.getState().toggleFrameStats();
    window.__magpdf.controller().stats.reset();
    const probe = { times: [], running: true };
    const tick = (time) => {
      if (!probe.running) return;
      probe.times.push(time);
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    window.__frameProbe = probe;
  });
}

async function stopFrameProbe(page) {
  const { intervals, script } = await page.evaluate(() => {
    const probe = window.__frameProbe;
    probe.running = false;
    const intervals = [];
    for (let i = 1; i < probe.times.length; i++) {
      intervals.push(probe.times[i] - probe.times[i - 1]);
    }
    const stats = window.__magpdf.controller().stats.summary();
    const state = window.__magpdf.store.getState();
    if (state.showFrameStats) state.toggleFrameStats();
    return { intervals, script: { median: stats.workP50, p95: stats.workP95, max: stats.workMax } };
  });
  return {
    frames: intervals.length,
    intervalMs: summarize(intervals),
    over16ms: intervals.filter((interval) => interval > FRAME_MS).length,
    scriptMs: { median: round(script.median), p95: round(script.p95), max: round(script.max) },
  };
}

/** 鼠标在页面上来回移动（悬停的对象一直在换），量帧间隔。 */
async function measureHover(page) {
  const area = await pageRectOnScreen(page);
  await parkMouse(page);
  await startFrameProbe(page);
  const started = performance.now();
  let moves = 0;
  while (performance.now() - started < HOVER_MS) {
    const t = moves++ / 60;
    await page.mouse.move(
      area.x + area.width * (0.5 + 0.45 * Math.sin(t * 2.3)),
      area.y + area.height * (0.5 + 0.45 * Math.sin(t * 3.1)),
    );
  }
  return { moves, ...(await stopFrameProbe(page)) };
}

/**
 * 点选：在对象和它上方一块地方之间来回点，每次选区都会变。从按下鼠标算到画完选区变化之后的那一帧
 * （每一帧的回调都跑完之后看一眼）。
 */
async function measureSelect(page, target) {
  await page.evaluate(() => {
    const { store } = window.__magpdf;
    const selection = () => {
      const state = store.getState();
      return state.documents[state.activeDocumentId]?.selection;
    };
    const probe = { samples: [], current: null };
    window.addEventListener(
      'pointerdown',
      () => {
        probe.current = { started: performance.now(), before: selection() };
      },
      true,
    );
    const channel = new MessageChannel();
    channel.port1.onmessage = () => {
      const current = probe.current;
      if (current && selection() !== current.before && window.__magpdf.controller().upToDate) {
        probe.samples.push(performance.now() - current.started);
        probe.current = null;
      }
    };
    const tick = () => {
      channel.port2.postMessage(0);
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    window.__selectProbe = probe;
  });
  const away = { x: target.x, y: target.y - Math.max(60, target.bbox.height * target.scale) };
  for (let i = 0; i < CLICKS; i++) {
    const at = i % 2 === 0 ? target : away;
    await page.mouse.click(at.x, at.y);
    await page.waitForFunction((done) => window.__selectProbe.samples.length >= done, i + 1, {
      timeout: 5000,
      polling: 5,
    });
  }
  await page.keyboard.press('Escape');
  const samples = await page.evaluate(() => window.__selectProbe.samples);
  return { clicks: samples.length, selectedMs: summarize(samples) };
}

/** 等引擎给出第 1 页保真校验的结论：第一次修改不该把校验的时间算进去。 */
async function fidelityKnown(page) {
  const handle = await page.waitForFunction(
    async () => {
      const { invoke } = window.__TAURI_INTERNALS__;
      const [doc] = await invoke('open_documents');
      if (!doc) return false;
      const snapshot = await invoke('engine_request', {
        method: 'doc.snapshot',
        params: { docId: doc.docId },
      });
      const { fidelity, fidelityReason } = snapshot.pages[0];
      return fidelity === 'pending' ? false : { status: fidelity, reason: fidelityReason ?? null };
    },
    null,
    { timeout: 20_000, polling: 50 },
  );
  return handle.jsonValue();
}

/**
 * 在页面里记下每一次向主进程的请求（按方法名）和每一次取瓦片的起止时刻，以及主线程上超过 50 ms 的长任务，
 * 用来拆开“修改到重画完”的时间花在哪。Tauri 2 在 Windows 上的命令走 `fetch`（`http://ipc.localhost/<命令>`，
 * 参数是请求体里的 JSON），瓦片也走 `fetch`，所以包 `fetch` 一处就够了，界面的代码不知道。
 * 响应体读完才算结束。
 */
function installRequestLog(page) {
  return page.evaluate(() => {
    const log = { requests: [], longTasks: [] };
    const kindOf = (url, init) => {
      if (url.includes('magpdf.localhost/tile')) return 'tile';
      const command = /ipc\.localhost\/([^?]+)/.exec(url)?.[1];
      if (command === undefined) return 'other';
      if (command !== 'engine_request') return command;
      try {
        return JSON.parse(init.body).method;
      } catch {
        return command;
      }
    };
    for (const read of ['arrayBuffer', 'json', 'text']) {
      const original = Response.prototype[read];
      Response.prototype[read] = function () {
        const entry = this.__magpdfEntry;
        const body = original.call(this);
        if (entry) {
          body.then(() => {
            entry.end = performance.now();
          });
        }
        return body;
      };
    }
    const fetch = window.fetch.bind(window);
    window.fetch = (input, init) => {
      const url = typeof input === 'string' ? input : input.url;
      const entry = { kind: kindOf(url, init), start: performance.now(), end: null };
      log.requests.push(entry);
      return fetch(input, init).then((response) => {
        response.__magpdfEntry = entry;
        return response;
      });
    };
    new PerformanceObserver((list) => {
      for (const task of list.getEntries()) {
        log.longTasks.push({ start: task.startTime, end: task.startTime + task.duration });
      }
    }).observe({ type: 'longtask' });
    window.__requestLog = log;
  });
}

/**
 * 一次修改里的各段：从按键算起，修改命令什么时候回来，重新取对象列表花了多久，第一块瓦片什么时候开始取、
 * 最后一块什么时候取完，主线程长任务一共多久。
 */
function breakdown(sample, log) {
  const from = sample.started;
  const to = from + sample.repainted;
  const inside = log.requests.filter((r) => r.start >= from && r.start <= to && r.end !== null);
  const of = (kind) => inside.filter((r) => r.kind === kind);
  const offset = (time) => round(time - from);
  const edit = inside.find((r) => r.kind?.startsWith('edit.') || r.kind?.startsWith('history.'));
  const lists = of('obj.list');
  const tiles = of('tile');
  const busy = log.longTasks
    .filter((task) => task.end > from && task.start < to)
    .reduce((sum, task) => sum + Math.min(task.end, to) - Math.max(task.start, from), 0);
  return {
    editDone: edit ? offset(edit.end) : null,
    objList: lists.length
      ? {
          requests: lists.length,
          start: offset(lists[0].start),
          end: offset(Math.max(...lists.map((r) => r.end))),
        }
      : null,
    renderRequests: of('render.tiles').length,
    tiles: tiles.length
      ? {
          fetched: tiles.length,
          firstStart: offset(tiles[0].start),
          lastEnd: offset(Math.max(...tiles.map((r) => r.end))),
          eachMs: summarize(tiles.map((r) => r.end - r.start)),
        }
      : null,
    longTaskMs: round(busy),
  };
}

/** 单对象修改到重画完：选中一个对象，左右来回微移。 */
async function measureEdit(page) {
  const fidelity = await fidelityKnown(page);
  assert.equal(
    fidelity.status,
    'ok',
    `第 1 页不能修改（${fidelity.reason}），换一份 MAGPDF_PERF_EDIT_PDF`,
  );
  const initial = await documentState(page);
  const target = await pickTarget(page);
  await page.mouse.click(target.x, target.y);
  assert.deepEqual((await documentState(page)).selection, [target.id]);
  await parkMouse(page);
  await installEditTimer(page);
  await installRequestLog(page);
  let version = initial.version;
  for (let i = 0; i < EDITS; i++) {
    await page.keyboard.press(i % 2 === 0 ? 'ArrowRight' : 'ArrowLeft');
    await page.waitForFunction((done) => window.__editTimer.samples.length === done, i + 1, {
      timeout: 20_000,
      polling: 5,
    });
    await editSettled(page, ++version, `第 ${i + 1} 次微移`);
  }
  // 最后一次修改的请求可能还没记下结束的时刻；等一会儿，等不到的不算
  await page
    .waitForFunction(() => window.__requestLog.requests.every((r) => r.end !== null), null, {
      timeout: 2000,
    })
    .catch(() => undefined);
  const { samples: timed, log } = await page.evaluate(() => ({
    samples: window.__editTimer.samples,
    log: window.__requestLog,
  }));
  const unfinished = log.requests.filter((r) => r.end === null).map((r) => r.kind);
  const samples = timed.map((sample) => ({ ...sample, ...breakdown(sample, log) }));
  const pick = (get) => summarize(samples.map(get).filter((value) => value !== null));
  return {
    summary: {
      objects: initial.objects,
      edits: samples.length,
      changedMs: summarize(samples.map((sample) => sample.changed)),
      repaintedMs: summarize(samples.map((sample) => sample.repainted)),
      over150ms: samples.filter((sample) => sample.repainted > 150).length,
      engineTiles: summarize(samples.map((sample) => sample.engineTiles)),
      canvasTiles: summarize(samples.map((sample) => sample.canvasTiles)),
      // 各段的时刻都从按键算起
      stages: {
        editDone: pick((s) => s.editDone),
        objListEnd: pick((s) => s.objList?.end ?? null),
        objListRequests: pick((s) => s.objList?.requests ?? null),
        firstTileStart: pick((s) => s.tiles?.firstStart ?? null),
        lastTileEnd: pick((s) => s.tiles?.lastEnd ?? null),
        tileFetchMs: pick((s) => s.tiles?.eachMs.median ?? null),
        longTaskMs: pick((s) => s.longTaskMs),
      },
      ...(unfinished.length ? { unfinished } : {}),
    },
    samples,
  };
}

async function main() {
  for (const [what, path] of [
    ['桌面程序', appBin],
    ['引擎', join(dirname(appBin), 'magpdf-engine.exe')],
    ['多页文档', pagesPdf],
    ['对象很多的那一页', editPdf],
  ]) {
    assert.ok(existsSync(path), `找不到${what}：${path}（构建和生成样本的步骤见本文件开头）`);
  }
  const summary = { app: appBin, window: windowSize };
  const raw = {};

  if (wanted('first') || wanted('scroll')) {
    const launches = wanted('first') ? LAUNCHES : 1;
    const firsts = [];
    for (let i = 0; i < launches; i++) {
      const last = i === launches - 1;
      const first = await withApp([pagesPdf], async (page, { app, launched }) => {
        const measured = await firstScreen(page, launched);
        if (last && wanted('scroll')) {
          await fitWindow(page, app);
          summary.scrollDisplay = await display(page);
          summary.scroll = await measureScroll(page);
        }
        return measured;
      });
      firsts.push(first);
    }
    if (wanted('first')) {
      raw.first = firsts;
      const pick = (key) => summarize(firsts.map((first) => first[key]));
      summary.first = {
        launches: firsts.length,
        totalMs: pick('totalMs'),
        webviewMs: pick('webviewMs'),
        uiMs: pick('uiMs'),
        openToFirstScreenMs: pick('openToFirstScreenMs'),
      };
    }
  }

  if (wanted('hover') || wanted('edit')) {
    await withApp([editPdf], async (page, { app }) => {
      await tilesSettled(page, 1);
      // 对象列表到了才能挑对象
      await page.waitForFunction(() => {
        const state = window.__magpdf.store.getState();
        return state.documents[state.activeDocumentId]?.snapshot.pages[0].objects.length > 0;
      });
      await fitWindow(page, app);
      summary.display = await display(page);
      if (wanted('hover')) {
        summary.hover = await measureHover(page);
        summary.select = await measureSelect(page, await pickTarget(page));
      }
      if (wanted('edit')) {
        const edit = await measureEdit(page);
        summary.edit = edit.summary;
        raw.edit = edit.samples;
      }
    });
  }

  console.log(JSON.stringify(summary));
  const out = process.env.MAGPDF_PERF_OUT;
  if (out) {
    mkdirSync(dirname(resolve(out)), { recursive: true });
    writeFileSync(out, JSON.stringify({ ...summary, raw }, null, 2));
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
