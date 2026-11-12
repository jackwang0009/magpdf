# 测试文件库

按来源分类（方案 13.1），所有统计按来源分别报告。数量、收集渠道和脱敏规则见《测试样本收集与脱敏规范》。

| 目录 | 来源 |
|---|---|
| `office-word/` | Word、WPS 导出 |
| `office-pptx-xlsx/` | PPT、Excel 导出（渐变、透明、大量小对象） |
| `design/` | Illustrator、InDesign、CorelDRAW 导出（CMYK、专色、软蒙版、叠印） |
| `browser-report/` | 浏览器打印、报表系统生成 |
| `scan/` | 扫描件（200/300 dpi 黑白与彩色、手机拍照） |
| `freight/` | 货代单证（提单、托书、装箱单、发票、面单、平台标签） |
| `public/` | 公开测试集（PDF.js、PDFium 仓库自带的测试文件），按 `public/<仓库名>/<子目录>` 组织：`public/pdfium/resources`、`public/pdfium/pixel`、`public/pdfjs/`… |
| `synthetic/` | 自制样本：针对生成器缺陷逐项构造（CMYK、专色、渐变、`TJ` 字距、`Tc/Tw`、Type3、文字裁剪、ExtGState、多内容流、内联图像等），分类 S |
| `synthetic-text/` | 自制文字样本（V08"改一行字"）：`generate.py` 用本机系统字体生成中文、英文短句页面，覆盖子集嵌入、完整嵌入、未嵌入三种情况；`samples/manifest.json` 记录每份的字体和要改的文字，`eval_v08.py` 跑评测。超过 1 MB 的完整嵌入样本不入库 |
| `synthetic-ocr/` | 合成的“打印体单证”扫描图（V09 的 OCR 评测）：`gen_synthetic.py` 画出中文、英文、中英对照的单证页，每个底页出 200/300 dpi 扫描（干净、噪声、歪斜）、整页旋转、仿手机拍照等变体，连同 PPOCRLabel 格式的标注写到 `out/`，文件不入库；`run_eval.ps1` 跑评测，`measure_speed.ps1` 量速度。内容全部虚构；是画出来的图，不能代替真实扫描件 |
| `perf/` | 性能测量用的页面（V04、V05）：`generate.py` 生成 5000 个对象的 A4 页、300 页的文档、300 dpi 的整页“扫描件”等，写到 `tests/out/perf/`，文件不入库。都是画出来的、最容易渲染的一类页面，量出来的数字只能当下限 |

规则：
- 真实单证必须先按脱敏规范处理，未脱敏的文件不得入库，也不得上传到任何外部服务（《工程规范》11）。
- 大文件走 Git LFS；`tests/baselines/` 存比对基线（往返测试存的是每页的判定和几个数，不存图，见 `tests/baselines/README.md`）。
- 每个目录放一份 `manifest.json`，记录文件来源、生成工具、页数和已知特性，供报告按来源分类。字段见《测试样本收集与脱敏规范》第 6 节；
  用 `tools/make_manifest.py <目录>` 生成或更新（`synthetic/` 由其 `generate.py` 自行生成）。
- 往返测试按相对路径的第一级目录（`pdfium`、`pdfjs`）区分来源；PDFium 内部的四个子目录在报告里单独列一行。

## 每晚的往返测试

`scripts/nightly-roundtrip.ps1` 对各语料集跑全量往返、校准轮、操作符级比对，生成按来源分类的报告页面，再和
`tests/baselines/roundtrip/` 里的基线比，有退步时退出码非 0。本机和每晚的流水线（`.github/workflows/nightly.yml`）用同一个脚本。

```powershell
pwsh scripts/nightly-roundtrip.ps1 -PublicPdfium <PDFium 的 testing\resources> [-Corpus <真实样本的目录>]
# 报告页面：tests/out/nightly/report.html
```

- `tools/roundtrip_report.py`：报告页面和退步判定（`roundtrip_model.py` 是口径和规则，`test_roundtrip_report.py` 是它们的单元测试）。
- `tools/corpus_fingerprint.py`：语料指纹，用来确认两次跑的是同一批文件。
- `tools/tamper_baseline.py`：故意改一份基线，验证退步检测还管用。
- 基线存什么、什么时候更新、怎么审：`tests/baselines/README.md`。做法和本机结果：`docs/p0/V14-每晚往返与分类报告.md`。
