/**
 * 崩溃恢复（方案 9.3，《交互设计》6.9）：程序被强制结束之后再启动，列出当时开着的文档，选了就重新打开；
 * 当时有没保存的修改、引擎写过自动备份的，从备份打开，修改还在。
 *
 * 起桌面程序打开一份 PDF（样本的副本），放一个条码（有了没保存的修改），等引擎写好自动备份（间隔调成 0.3 秒），
 * 强制结束（恢复文件和备份留下）；用同一份数据目录再起一次，不带文档。
 * 期望：弹出“上次没有正常退出”，列着那份文档、标着“有自动备份”；点“重新打开选中的”之后文档打开，
 * 标着“已修改”，条码还在，磁盘上的文件一直没动；上次留下的恢复文件删掉了，备份记进了这一次的恢复文件。
 *
 * 先构建发布版（步骤见 smoke.mjs 开头），再运行：
 *   node apps/desktop/e2e/recovery.mjs
 *
 * 环境变量：
 *   MAGPDF_DESKTOP_BIN   被测的可执行文件，默认 target/release/magpdf-desktop.exe；引擎要在同一个目录
 *   MAGPDF_E2E_PDF       打开的 PDF，默认 tests/corpus/synthetic/freight-bl-composite.pdf（复制一份来用）
 *
 * 测试窗口不抢焦点，不动真实的鼠标键盘。
 */
import assert from 'node:assert/strict';
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync } from 'node:fs';
import { basename, join, resolve } from 'node:path';
import {
  cleanUp,
  connect,
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
const source = resolve(
  process.env.MAGPDF_E2E_PDF ?? join(repo, 'tests/corpus/synthetic/freight-bl-composite.pdf'),
);
const env = { MAGPDF_RECOVERY_INTERVAL_MS: '300' };

const first = await launchApp(appBin, [], { env });
const { scratch } = first;
const work = join(scratch, 'docs');
mkdirSync(work, { recursive: true });
const pdf = join(work, basename(source));
copyFileSync(source, pdf);
const original = readFileSync(pdf);
first.app.kill();
await waitFor(
  '先起的那一次退出',
  () => first.app.exitCode !== null || first.app.signalCode !== null,
  10_000,
);

const crashed = await launchApp(appBin, [pdf], { scratch, env });
const recoveryDir = join(scratch, 'data', 'local', 'recovery');
const leftover = join(recoveryDir, `session-${crashed.app.pid}.json`);
let second;
let browser;
try {
  const connected = await connect(crashed.port);
  await tilesSettled(connected.page, 1);
  // 放一个条码：有了没保存的修改
  const at = await connected.page.evaluate(() => {
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
  await connected.page
    .locator('.mg-comp-item:not([disabled])', { hasText: 'Code 128' })
    .first()
    .click();
  await connected.page.mouse.click(at.x, at.y);
  await editSettled(connected.page, 1, '置入条码');
  await connected.browser.close();
  const backup = await waitFor(
    '恢复文件里记下这份文档的自动备份',
    () => {
      if (!existsSync(leftover)) return false;
      const docs = JSON.parse(readFileSync(leftover, 'utf8')).docs ?? [];
      const file = docs[0]?.snapshot;
      return file && existsSync(file) ? file : false;
    },
    10_000,
  );

  // 强制结束：恢复文件和备份留下（正常退出时会删掉）
  crashed.app.kill();
  await waitFor(
    '程序退出',
    () => crashed.app.exitCode !== null || crashed.app.signalCode !== null,
    10_000,
  );
  assert.ok(existsSync(leftover) && existsSync(backup), '强制结束之后恢复文件和备份应当还在');
  assert.ok(readFileSync(pdf).equals(original), '没保存：磁盘上的文件没动');

  second = await launchApp(appBin, [], { scratch, env });
  ({ browser } = await connect(second.port));
  const page = (await browser.contexts()[0].pages()).find((p) => /^https?:\/\//.test(p.url()));
  const dialog = page.getByRole('alertdialog', { name: '上次没有正常退出' });
  await dialog.waitFor({ timeout: 20_000 });
  const name = basename(pdf);
  const rows = await dialog
    .locator('tbody tr')
    .evaluateAll((trs) =>
      trs.map((tr) => [...tr.querySelectorAll('td')].slice(1).map((td) => td.textContent)),
    );
  assert.equal(rows.length, 1, `对话框里应当只列出 ${name}`);
  assert.equal(rows[0][0], name);
  assert.equal(rows[0][2], '有自动备份');

  await dialog.getByRole('button', { name: '重新打开选中的' }).click();
  await page.waitForFunction(
    (title) => {
      const state = window.__magpdf?.store.getState();
      return state && Object.values(state.documents).some((d) => d.snapshot.name === title);
    },
    name,
    { timeout: 20_000 },
  );
  await tilesSettled(page, 1);
  const reopened = await waitFor(
    '重新打开的文档里有条码',
    () =>
      page.evaluate(() => {
        const state = window.__magpdf.store.getState();
        const doc = state.documents[state.activeDocumentId];
        const barcode = doc.snapshot.pages[0].objects.find((o) => o.component?.kind === 'code128');
        return barcode
          ? { dirty: doc.snapshot.dirty, content: barcode.component.values.content }
          : null;
      }),
    10_000,
  );
  assert.deepEqual(reopened, { dirty: true, content: 'SAMPLE-0001' });
  assert.ok(readFileSync(pdf).equals(original), '重新打开也没动磁盘上的文件');

  await waitFor('上次留下的恢复文件删掉', () => !existsSync(leftover), 5_000);
  const remaining = readdirSync(recoveryDir).filter((f) => f.endsWith('.json'));
  assert.deepEqual(remaining, [`session-${second.app.pid}.json`], '只剩这一次运行自己的恢复文件');
  const current = JSON.parse(readFileSync(join(recoveryDir, remaining[0]), 'utf8'));
  assert.equal(current.docs[0].snapshot, backup, '备份记进了这一次的恢复文件，再崩溃也还能恢复');
  assert.ok(existsSync(backup));

  console.log(JSON.stringify({ reopened: name, fromBackup: true, leftoverDeleted: true }));
} finally {
  await browser?.close().catch(() => undefined);
  second?.app.kill();
  crashed.app.kill();
  await sleep(200);
  await cleanUp(scratch);
}
