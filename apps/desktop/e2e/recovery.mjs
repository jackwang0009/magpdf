/**
 * 崩溃恢复（方案 9.3 第 3 条，《交互设计》6.9）：程序被强制结束之后再启动，列出当时开着的文档，选了就重新打开。
 *
 * 起桌面程序打开一份 PDF，等画面出来，强制结束（恢复文件留下）；用同一份数据目录再起一次，不带文档。
 * 期望：弹出“上次没有正常退出”，列着那份文档；点“重新打开选中的”之后文档打开，上次留下的恢复文件删掉了。
 *
 * 先构建发布版（步骤见 smoke.mjs 开头），再运行：
 *   node apps/desktop/e2e/recovery.mjs
 *
 * 环境变量：
 *   MAGPDF_DESKTOP_BIN   被测的可执行文件，默认 target/release/magpdf-desktop.exe；引擎要在同一个目录
 *   MAGPDF_E2E_PDF       打开的 PDF，默认 tests/corpus/synthetic/freight-bl-composite.pdf
 *
 * 测试窗口不抢焦点，不动真实的鼠标键盘。
 */
import assert from 'node:assert/strict';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { basename, join, resolve } from 'node:path';
import { cleanUp, connect, launchApp, repo, sleep, tilesSettled, waitFor } from './harness.mjs';

const appBin = resolve(
  process.env.MAGPDF_DESKTOP_BIN ?? join(repo, 'target/release/magpdf-desktop.exe'),
);
const pdf = resolve(
  process.env.MAGPDF_E2E_PDF ?? join(repo, 'tests/corpus/synthetic/freight-bl-composite.pdf'),
);

const first = await launchApp(appBin, [pdf]);
const { scratch } = first;
const recoveryDir = join(scratch, 'data', 'local', 'recovery');
const leftover = join(recoveryDir, `session-${first.app.pid}.json`);
let second;
let browser;
try {
  const connected = await connect(first.port);
  await tilesSettled(connected.page, 1);
  await connected.browser.close();
  await waitFor(
    '恢复文件里记下这份文档',
    () => {
      if (!existsSync(leftover)) return false;
      const docs = JSON.parse(readFileSync(leftover, 'utf8')).docs ?? [];
      return docs.length > 0;
    },
    10_000,
  );

  // 强制结束：恢复文件留下（正常退出时会删掉）
  first.app.kill();
  await waitFor(
    '第一次启动的程序退出',
    () => first.app.exitCode !== null || first.app.signalCode !== null,
    10_000,
  );
  assert.ok(existsSync(leftover), '强制结束之后恢复文件应当还在');

  second = await launchApp(appBin, [], { scratch });
  ({ browser } = await connect(second.port));
  const page = (await browser.contexts()[0].pages()).find((p) => /^https?:\/\//.test(p.url()));
  const dialog = page.getByRole('alertdialog', { name: '上次没有正常退出' });
  await dialog.waitFor({ timeout: 20_000 });
  const name = basename(pdf);
  const listed = await dialog.locator('tbody td:nth-child(2)').allTextContents();
  assert.deepEqual(listed, [name], `对话框里应当只列出 ${name}`);

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
  await waitFor('上次留下的恢复文件删掉', () => !existsSync(leftover), 5_000);
  const remaining = readdirSync(recoveryDir).filter((f) => f.startsWith('session-'));
  assert.deepEqual(remaining, [`session-${second.app.pid}.json`], '只剩这一次运行自己的恢复文件');

  console.log(JSON.stringify({ reopened: name, leftoverDeleted: true }));
} finally {
  await browser?.close().catch(() => undefined);
  second?.app.kill();
  first.app.kill();
  await sleep(200);
  await cleanUp(scratch);
}
