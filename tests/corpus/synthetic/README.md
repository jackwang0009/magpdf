# 自制样本（分类 S）

针对 PDFium 内容流生成器的已知缺陷（方案 2.1）和生成规则（方案 2.3）逐项构造的小样本，由 `generate.py` 用 Python 3.12 标准库
确定性生成（无时间戳、无随机数；重新生成逐字节相同），PDF 全部手写：对象、xref 偏移、xref 流、对象流、增量更新都由 `pdfgen.py` 组装。
样本清单、每份要观察的现象、分组说明见 [`docs/p0/V14-自制样本.md`](../../../docs/p0/V14-自制样本.md)。

```powershell
python tests/corpus/synthetic/generate.py                       # 重建全部 PDF 与 manifest.json
python tests/corpus/synthetic/generate.py --check               # 再运行 check.py
python tests/corpus/synthetic/check.py --render tests/out/synthetic --dpi 96   # 渲染第 1 页到 tests/out/ 肉眼检查（不入库）
python tests/corpus/synthetic/generate.py --list
```

`check.py` 做三件事：文件 SHA-256/大小与 manifest 一致；`magpdf-cli info --json` 的页数、尺寸、旋转、对象类型统计与 manifest 的
`expect` 一致（先 `cargo build -p magpdf-cli`，或设 `MAGPDF_CLI`）；pikepdf/qpdf 结构检查——`check: pass` 的样本不得有警告，
`check: expect-warn` 的三个故意损坏样本必须有警告且仍能打开。pikepdf 没装时跳过并提示（`pip install pikepdf`，MPL-2.0，仅测试用）。

文件组织：

| 文件 | 作用 |
|---|---|
| `pdfgen.py` | PDF 写出器（经典 xref / xref 流 + 对象流 / 增量更新 / 故意损坏），以及最小 ICC 配置、极小 TrueType 字体、Code 39、Adobe-GB1 CID、ISO 6346 校验位 |
| `common.py` | 文档骨架 `Doc`、内容流构造器 `Content`（每次绘制同时登记对象数预期）、样本登记 `@sample` |
| `samples_color.py` | 颜色空间、图案、渐变 |
| `samples_gs_text.py` | ExtGState、文字、字体 |
| `samples_struct.py` | 多内容流、XObject、图像、标记内容、页面几何、文件结构、OCG、批注 |
| `samples_composite.py` | 货代单证风格综合样本（虚构数据，编号校验位正确） |

加样本：在对应 `samples_*.py` 里写一个 `@sample(...)` 函数，返回 `(bytes, [每页 expect])`；用 `Content` 画内容就自动得到对象数预期。
样本内容必须虚构，不得含任何真实单据信息。
