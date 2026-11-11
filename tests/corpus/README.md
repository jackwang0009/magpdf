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
| `perf/` | 性能测量用的页面（V04、V05）：`generate.py` 生成 5000 个对象的 A4 页、300 页的文档、300 dpi 的整页“扫描件”等，写到 `tests/out/perf/`，文件不入库。都是画出来的、最容易渲染的一类页面，量出来的数字只能当下限 |

规则：
- 真实单证必须先按脱敏规范处理，未脱敏的文件不得入库，也不得上传到任何外部服务（《工程规范》11）。
- 大文件走 Git LFS；`tests/baselines/` 存像素比对基线。
- 每个目录放一份 `manifest.json`，记录文件来源、生成工具、页数和已知特性，供报告按来源分类。字段见《测试样本收集与脱敏规范》第 6 节；
  用 `tools/make_manifest.py <目录>` 生成或更新（`synthetic/` 由其 `generate.py` 自行生成）。
- 往返测试按相对路径的第一级目录（`pdfium`、`pdfjs`）区分来源；PDFium 内部的四个子目录在报告里单独列一行。
