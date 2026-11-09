# 公开测试集（分类 G）

公开仓库自带的 PDF 测试文件（方案 13.1、《测试样本收集与脱敏规范》2、3.2 节）。本目录只放 `manifest.json` 与本说明；
文件本身不进主仓库——按规范第 5 节放测试文件库（`magpdf-corpus`，Git LFS 或企业网盘），由测试组按下面的规则决定入库清单。

## 来源

| 来源 | 位置 | 许可 | 规模（2026-11 本机统计） |
|---|---|---|---|
| PDFium 测试资源 | 本机 `D:\pdfium-src\pdfium\testing\resources`（上游 `pdfium/testing/resources`） | BSD-3-Clause（`pdfium/LICENSE`） | 347 个 `.pdf`，共 2.9 MB：根目录 301、`pixel/` 33、`javascript/` 6、`xfa/` 6；另有 569 个 `.in` 模板 |
| pdf.js 测试文件 | `pdf.js/test/pdfs`（GitHub `mozilla/pdf.js`） | Apache-2.0（仓库 `LICENSE`）；个别文件在 `test/pdfs/` 内另有说明 | 数百个文件，其中大量是 `.link` 文件（只记录外部网址，仓库里没有文件本体） |

PDFium 的 `.in` 文件是模板，经 `testing/tools/fixup_pdf_template.py` 补上 xref 后才是合法 PDF；`pixel/`、`javascript/` 下的 `.in`
对应的 `.evt` 是事件脚本。需要时用该脚本批量生成 `.pdf` 再入库，并在 manifest 的 `note` 里记录"由 .in 生成"。

## 入库规则

**要收**（对往返测试、第二渲染器比对、编辑测试有价值）：
- 覆盖颜色空间、渐变、透明、软蒙版、Type3、CID 字体、内联图像、标记内容、多内容流、Form XObject 的文件；
- 结构不规范但常见阅读器能打开的文件（PDFium 以 `bug_*.pdf` 命名的回归样本有 151 个，逐个看内容再决定）；
- 带批注外观流、AcroForm 的文件（P2 打印"文档和批注"要用）。

**不要收**：
- 模糊测试产生的崩溃样本和 `testing/corpus`（独立仓库）的海量文件——与往返测试无关，CI 跑不完；
- 单个超过 1 MB 的文件（公开集里只有 `resolve_nodes_*.pdf` 三个 200 KB 的，可收）；总量控制见规范第 8 节；
- `xfa/` 下的 XFA 表单（预编译 PDFium 未开 XFA，渲染为空）；
- `javascript/` 下依赖脚本执行的文件（引擎不执行 JavaScript，方案 13.3）；
- pdf.js 的 `.link` 文件指向的外部 PDF：下载后按规范 3.2 记录网址和日期，只作内部测试，不对外分发。

## 操作

1. 把选中的文件复制到测试文件库的 `public/pdfium/`、`public/pdfjs/` 子目录（保留原文件名，便于对照上游）。
2. 在该目录运行 `python tests/corpus/tools/make_manifest.py <dir> --category G --origin public`，得到 `manifest.json`
   （文件名、SHA-256、大小、页数、每页对象统计）。
3. 手工补 `license`（`BSD-3-Clause` / `Apache-2.0`）、`tags`、`expected`；来自 `.in` 的写明生成方式。
4. manifest 提交到本目录；文件本体提交到测试文件库。
