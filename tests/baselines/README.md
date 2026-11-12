# 比对基线

## 往返测试的基线：`roundtrip/<语料集>.json`

每晚的往返测试（`scripts/nightly-roundtrip.ps1`）跑完之后和这里的基线比，判断有没有退步。基线记的是主干上**认可的现状**：
在仓库默认的配置下，每个文件、每一页现在是什么判定。它不是目标值——原版 PDFium 写不好的页，在基线里就是“有差异”。

| 文件 | 语料 | 来源 |
|---|---|---|
| `roundtrip/synthetic.json` | `tests/corpus/synthetic/`，60 份自制样本 | `synthetic` |
| `roundtrip/public-pdfium.json` | PDFium 的 `testing/resources`（`chromium/8138`，346 个 PDF，不入库，`scripts/fetch-public-corpus.ps1` 取） | `public/pdfium/resources`、`pixel`、`javascript`、`xfa` |
| `roundtrip/corpus.json`（还没有） | 真实样本的测试文件库 | 第一级目录名（`office-word`、`design`……） |

### 存什么

只存判定和几个数，不存图、不存耗时、不存归因线索：

- 每个文件：相对路径、来源、状态（`ok`、`load_failed`、`save_failed`、`reopen_failed`、`crashed`、`timed_out`）。
- 每一页（一页一行）：页码；像素判定 `pixel`（`pass` 无差异、`diff` 有差异、`error` 原件这一页就加载不了）；
  页上的对象数 `objects`（0 就是无对象页）；差异像素数 `diff_pixels` 和最大连通区域 `largest_region`（是 0 就不写）；
  操作符级五类差异的处数 `ops`（`{}` 是比过、没有差异；没有这一项是没比成）。
- `meta`：生成时间、当时的提交、命令行工具和 PDFium 的版本、`pdfium.dll` 的 SHA-256、机器名、判定阈值和渲染倍率、
  语料指纹（`tests/corpus/tools/corpus_fingerprint.py`）。和基线比的时候，这几项对不上会在报告页面上提示。
  `commit` 是跑测试时工作树的提交；命令行工具用 `-Cli` 另外给的时候，它是哪一份看 `cli_sha256`（只留底，比较时不看）。
  流水线上机器名记的是镜像（`github-actions/<镜像>`），不是每次都变的主机名。

文件按来源和路径排序、一页一行，`git diff` 能直接看出是哪几页变了。

### 退步怎么判

规则来自《测试计划》第 7 节；比例都按“扣除无对象页”算（《P0 技术验证计划》V02）。实现在 `tests/corpus/tools/roundtrip_model.py`。

| 情况 | 结果 | 入口脚本的退出码 |
|---|---|---|
| 任一来源的无差异比例比基线低 0.5 个百分点以上；“像素和操作符都无差异”的比例同样算一次 | 到了报警线 | 2 |
| 出现基线里没有的崩溃或超时（崩溃文件的页不算比对页，比例看不出来，所以单算） | 到了报警线 | 2 |
| 有新变坏的页（基线里无差异，这次是有差异、页错误，或者文件崩溃了没有比对到），但各来源的比例没掉过线 | 有退步，没到报警线 | 3 |
| 像素仍无差异、操作符级从没有差异变成有 | 有退步，没到报警线 | 3 |
| 基线里的文件这次没跑到；文件从正常变成打不开或保存失败；基线里有操作符级结果而这次没有；校准轮有差异 | 有退步，没到报警线 | 3 |
| 有新变好的页；有基线里没有的新文件；PDFium、机器、语料指纹和基线不一样 | 只提示 | 0 |
| 没有基线 | 不判定，只出报告 | 0（加 `-RequireBaseline` 时是 1） |

`-FailOn alarm` 时退出码 3 改成 0，`-FailOn never` 时 2、3 都改成 0。

和基线比的比例，两边都只拿**基线里有、这次也有的文件**算：

- 语料加了文件（真实样本是陆续收进来的）：新文件不算进比例，不会因为它们通过得少就报警，也盖不住老文件的退步。
  新文件在报告里照常列出，更新基线之后才受保护。
- 基线里的文件这次没跑到：那些文件两边都不算，比例不会因此变好看；这件事本身算“有退步，没到报警线”。
- 文件还在、里面可比对的页变了（崩溃修好之后多出来几页，其中有的有差异）：比例照常变，可能报警——这时该做的是在修复的
  那个 PR 里更新基线。

所以报告第 1 节里一个来源的比例（算全部文件）和第 2 节里“这次”的比例，在语料变了的时候可以不一样。

### 什么时候更新

1. **改动让结果变了**：修了生成器、换了默认链接的 PDFium、升级 PDFium、改了判定阈值或渲染倍率。在同一个 PR 里更新基线，
   PR 描述里写清哪些页变好、哪些页变坏、为什么接受。
2. **语料变了**：加了样本、删了样本、重新生成了自制样本。和语料的改动放在一起更新。
3. **每晚报了退步时不要为了变绿去更新**。先定位；确认是预期中的变化（比如一处修复让另几页的差异像素数变了）再按第 1 条更新。

### 怎么更新

```powershell
pwsh scripts/nightly-roundtrip.ps1 -PublicPdfium <PDFium 的 testing\resources> -UpdateBaseline
git diff --stat tests/baselines/roundtrip
git add tests/baselines/roundtrip/synthetic.json tests/baselines/roundtrip/public-pdfium.json
```

`-UpdateBaseline` 照常出报告页面（和旧基线比），然后用这次的结果覆盖基线，退出码 0。不加这个开关时，这次的结果也会按基线的格式
写在输出目录的 `baseline-candidate/` 里；流水线的产物里也有这个目录，可以直接拿来换。

### 怎么审

- 看 `git diff`。`"pixel": "pass"` 变成 `"diff"` 或 `"error"` 的每一行、`"status"` 从 `ok` 变成别的每一行、`"ops"` 从 `{}` 变成有数的每一行，
  都要在 PR 里有说法。反过来的是变好，确认是这次改动带来的。只有 `diff_pixels`、`largest_region` 变了的行，是原来就有差异的页差异大小变了。
- 看更新那一次的报告页面第 2 节（它是和旧基线比的），或者把 `summary.md` 贴进 PR。
- 看 `meta`：`pdfium_dll_sha256`、`machine`、`corpus` 有没有变。换了机器或者换了 PDFium 库生成的基线，和原来的不是一回事。
- `/tests/` 的评审人见 `CODEOWNERS`（测试与工程组）。

### 和机器有关的地方

没有嵌入的字体换成什么字体，各台机器不一样（V14 自制样本报告 4.4 第 9 条），所以基线要在跑每晚测试的那种环境里生成。
现在入库的两份基线是在开发机上用 `main` @ `ff6b0ac` 的命令行工具、配原版预编译库生成的。流水线第一次在 GitHub 上跑出来之后，
如果它的结果和这两份不一样，用它产物里的 `baseline-candidate/` 换掉这两份（见 `docs/p0/V14-每晚往返与分类报告.md`）。

要同时盯两种 PDFium（原版和 `mag/main`）时，各用一个目录：`-BaselineDir tests/baselines/roundtrip-<名字>`。

### 验证退步检测还管用

```powershell
python tests/corpus/tools/tamper_baseline.py tests/baselines/roundtrip/synthetic.json tests/out/tampered/synthetic.json --regress 3
Copy-Item tests/baselines/roundtrip/public-pdfium.json tests/out/tampered/
pwsh scripts/nightly-roundtrip.ps1 -PublicPdfium <目录> -BaselineDir tests/out/tampered -OutDir tests/out/nightly-tampered
# 期望：退出码 2，报告页面顶部是红色的“有退步，到了报警线”，第 2 节列出被改的那 3 页
```

## 其他

方案第 12 节把这个目录定为“像素比对基线”。目前只有往返测试用它；编辑测试、第二渲染器比对接进每晚流水线时，各自在这里建子目录。
