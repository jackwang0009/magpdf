# V11 背景修补原型

P0 技术验证 V11 的原型：扫描件上把一行原来的字抹掉、把背景补回来（方案 6.4），看传统算法够不够用、LaMa 模型在 CPU 上要多久。
只用 Python，只在开发机上评估用，不进安装包。结论和数字在 `docs/p0/V11-背景修补原型.md`。

## 环境

要两个虚拟环境（不要装进全局 Python），外加一个模型文件：

```powershell
# 主环境：OpenCV、NumPy、Pillow、onnxruntime、psutil
python -m venv D:\workspace\magpdf-wt\venvs\v11
D:\workspace\magpdf-wt\venvs\v11\Scripts\python -m pip install -r tests/prototypes/v11-inpaint/requirements.txt

# 第二个环境：只为了 OpenCV contrib 里 xphoto 模块的两种修复算法（移位图、FSR）。
# opencv-python-headless 和 opencv-contrib-python-headless 都提供 cv2，装在一起会互相覆盖，所以分开。
python -m venv D:\workspace\magpdf-wt\venvs\v11-contrib
D:\workspace\magpdf-wt\venvs\v11-contrib\Scripts\python -m pip install -r tests/prototypes/v11-inpaint/requirements-contrib.txt

# LaMa 模型（208 MB，不入库）：从 hf-mirror.com 下到 D:\workspace\magpdf-wt\models-extra\，核对大小和 SHA-256
D:\workspace\magpdf-wt\venvs\v11\Scripts\python tests/prototypes/v11-inpaint/fetch_lama.py
```

第二个环境和模型都可以没有：没有的时候对应的方法自动跳过，别的照常跑。

OCR 用现成的 `D:\workspace\magpdf-wt\bin\magpdf-cli.exe` 和 `D:\workspace\magpdf\models`（只读）。
这几个位置可以用环境变量改：`MAGPDF_CLI`、`MAGPDF_MODELS_DIR`、`MAGPDF_LAMA_MODEL`、`MAGPDF_V11_CONTRIB_PYTHON`。

## 一条命令重跑

```powershell
D:\workspace\magpdf-wt\venvs\v11\Scripts\python -X dev tests/prototypes/v11-inpaint/run_all.py
```

按顺序跑下面几步，输出都在 `tests/out/v11/`（不入库）。开发机上从空目录全部跑完，机器比较空的时候 23 分钟，
被别的任务占满的时候 96 分钟；时间主要花在 evaluate 对每一组结果跑 OCR、掩膜对比和 LaMa 上。加 `--quick` 不跑带 * 的三步，
要评价的结果少得多，机器比较空的时候 6 分 14 秒。`--stages` 只跑指定的几步；`--clean` 先清空输出目录。
同一个输出目录同时只能有一个 `run_all.py` 在跑（有锁文件）。重跑出来的样本、结果图和评价的数是逐字节相同的（耗时除外）。

| 步骤 | 做什么 | 脚本 | 主要输出 |
|---|---|---|---|
| samples | 造 50 个样本（固定种子，重跑逐字节相同） | `gen_samples.py` | `samples/<编号>/`、`samples/index.json` |
| ocr-input | 对带字原图、无字对照图跑 OCR，取这一行的行框 | `ocr.py` | `ocr-input.json` |
| masks | 从行框里分出字的像素，和真值掩膜比 | `masks.py` | `masks/`、`masks.json` |
| inpaint | 底色填充、`cv2.inpaint` | `inpaint.py --families fill opencv` | `results/<掩膜>/<方法>/` |
| xphoto * | 移位图、FSR 快档（用第二个环境的 Python 跑） | `inpaint.py --families xphoto` | 同上 |
| lama * | LaMa | `inpaint.py --families lama` | 同上 |
| sweep * | 掩膜往外扩 1、5、7 像素的对比；不按颜色分的掩膜的对比 | `inpaint.py --masks ...` | 同上 |
| evaluate | 客观评价（含对每一组结果跑 OCR） | `evaluate.py` | `evaluation.json`、`evaluation.md` |
| sheets | 给人打分用的对照图和打分表 | `sheets.py`、`sheets.py --blind` | `sheets/`、`sheets-blind/` |

每一步的命令、耗时、跳过了什么记在 `run.json` 里（用 `--stages` 只跑几步时记在 `run-stages.json` 里，不盖掉整趟的记录）。
每个脚本也可以单独跑，用法写在各自文件开头。

## 重量时间

```powershell
D:\workspace\magpdf-wt\venvs\v11\Scripts\python -X dev tests/prototypes/v11-inpaint/bench.py
```

结果写 `tests/out/v11/bench.json`（`--json` 可以改位置）：

- LaMa 处理一个 512×512 区域：默认线程数、4、2、1 线程四种设置，每种起 5 个新进程，每个进程建一次会话、预热一次、再推理 7 次；
  记建会话的时间、第一次推理、之后 35 次推理的中位数和最小最大、峰值工作集和峰值提交内存；
- 传统方法和算掩膜处理一行字：10 行，每行 5 次取中位数（要先跑过 `run_all.py`；xphoto 的两种用第二个环境的 Python 量）；
- 量之前、量之后整机的 CPU 占用。

只量 LaMa：加 `--skip-traditional`。只量任务书要的两种线程设置：加 `--threads 0 4`。
机器上有别的任务在跑时，加 `--priority high` 把量的进程调成高优先级再量一遍（另存一份：`--json tests/out/v11/bench-high.json`），
两份对着看。这台开发机不是参考机 R1。

LaMa 建一次会话要十秒上下，这段时间省不省得掉，另有一个脚本量（默认设置、关掉图优化、读事先优化好的模型三种办法各建 5 次，
结果写 `tests/out/v11/bench-init.json`；要 LaMa 的模型，量的时候在临时目录里存一份约 200 MB 的优化模型，量完就删）：

```powershell
D:\workspace\magpdf-wt\venvs\v11\Scripts\python -X dev tests/prototypes/v11-inpaint/bench_init.py
```

## 样本

50 个，是画出来的，不是真实的扫描件，文字内容全部虚构。每个是一张 1280×560 像素的图（300 dpi，约 108×47 mm），
中间一行是要抹掉的“目标行”，上下排着不动的邻行：

| 类别 | 编号 | 个数 | 内容 |
|---|---|---|---|
| 纯色背景 | p01–p17 | 17 | 白纸、发黄、发灰、彩色联单、底色不匀、装订阴影、压着表格线 |
| 底纹背景 | t01–t17 | 17 | 网纹、点阵、渐变、浅色水印字 |
| 压着红章 | s01–s16 | 16 | 红色圆章半透明压在字上：压着环形文字、五角星、横排文字、外圈 |

中文 16 行、英文 17 行、中英混排 17 行；字体有宋体、黑体、楷体、仿宋、微软雅黑、Arial、Times New Roman、Courier New、Calibri；
字号 9、10.5、12、14、16、18、22 磅。都做了扫描的样子：轻微歪斜、模糊、噪声、JPEG 压缩。每个样本目录里有：

| 文件 | 内容 |
|---|---|
| `input.png` | 带字的图 |
| `ref.png` | 同样条件（同一份噪声、同样的模糊和压缩）下没有目标行的图，评价时当标准答案 |
| `ref_nonoise.png` | 同上但不加噪声；它和 `ref.png` 的差就是噪声本身，评价表里“噪声底”那一行用 |
| `gt_mask.png`、`gt_alpha.png` | 目标行字形的真值掩膜（模糊之前的字形）、模糊之后每个像素上字的浓度 |
| `struct.png`、`nostruct.png` | 有章、底纹的样本：章或底纹线条的像素；没有章或底纹时的图（算“线条还在不在”用） |

字体从 `C:\Windows\Fonts` 读，只在本机生成样本用，不随仓库分发。

## 掩膜和方法的名字

掩膜：来源加往外扩的像素数。`auto-d3` 是产品里会用的那种（只有 OCR 的行框，自己分出字的像素，往外扩 3 像素）；
`gt-d3` 是真值掩膜（对照）；`gray-d3` 是不按颜色分、直接对灰度图做大津二值化得到的（对照，看红章会不会被当成字）。

方法（`python inpaint.py --list`）：

| 名字 | 说明 |
|---|---|
| `fill-flat` | 用掩膜周围一圈像素的中位数颜色填，整行一个颜色 |
| `fill-local` | 每个像素取它附近的底色（两遍大窗口中值） |
| `fill-local-grain` | 同上，再按周围纸面的颗粒大小加一点噪声 |
| `telea-r1/3/5/9`、`ns-r1/3/5/9` | `cv2.inpaint` 的 Telea 和 Navier-Stokes，半径 1、3、5、9 |
| `shiftmap`、`fsr-fast`、`fsr-best` | xphoto 模块的移位图、频率选择重建快档和慢档（慢档一行十几到几十秒，不指名不跑） |
| `lama` | LaMa（ONNX，CPU），沿着这一行排 512×512 的窗口 |

修补的结果只存“补丁”：掩膜的外接矩形往外 8 像素那一块，和产品里“补丁图像叠在原图上”的做法一样；掩膜外面的像素不动。

## 客观指标怎么读

`evaluation.md` 里每类背景一张表，一行一种方法。比较的范围是“掩膜 ∪ 看得见的字”：既算补出来的像素，也算漏在掩膜外面的残字。

- 平均绝对差：这个范围里结果和 `ref.png` 每个像素每个通道差多少（0–255）。
- 超过阈值的像素：三个通道里差得最多的那个超过 24 的像素占多少。噪声本身几乎不会超过 24，所以这一项基本只算看得出来的差别。
- 最差一行：上一项在这类样本里最大的那个。
- PSNR、SSIM：同一个范围上算的；SSIM 用灰度图、11×11 高斯窗。
- OCR 还读出原文的行：对修补后的整张图跑 OCR，目标行的位置上读出来的字和原文的相似度（1 − 编辑距离 ÷ 较长的长度）在 0.5 以上的行数、大于 0 的行数。
- 章、底纹的像素还在的比例：掩膜里原来有章或底纹线条的像素，修补之后还剩一半以上“线条的颜色”的占多少。
- 两行基准：“不修补”是带字原图本身；“噪声底”是 `ref_nonoise.png`，修得再好也不会比它更接近 `ref.png`。

“超过阈值的像素”数不到一种痕迹：补上去的颜色和周围只差几级到十几级，但连成一片、形状就是原来那行字，人看得出来
（报告第 13.3 节）。`faint_traces.py` 另算一个数来补它：亮度差用 σ=2 像素的高斯抹平之后，超过 6 级的像素占多少。
它不在 `run_all.py` 的步骤里，跑完之后单独跑：

```powershell
D:\workspace\magpdf-wt\venvs\v11\Scripts\python -X dev tests/prototypes/v11-inpaint/faint_traces.py --each
```

这个数适合纯色、渐变、水印字这类没有细密图案的背景；网纹、点阵上只能在方法之间比着看（说明写在脚本开头）。
两种指标都代替不了主观分。

## 主观打分怎么做

主观分由三位评分人打，脚本和写脚本的人都不打。材料在 `tests/out/v11/sheets/`：

- `sheet-1-plain.png`、`sheet-2-texture.png`、`sheet-3-seal.png`：三类背景各一张大图。一行一个样本，从左到右是：
  带字的原图｜掩膜（蓝色是要抹掉的像素）｜各方法修补的结果｜对照（同样条件、本来就没有这行字的图）。
  每一格是原图上同一个位置裁出来的一块，300 dpi 原尺寸，没有缩放。
- `scores-template.csv`：打分表。一行一个（样本，方法），后面三列 `评分人1`、`评分人2`、`评分人3`。UTF-8 带 BOM，Excel 直接能开。

怕知道方法名会影响打分，用 `tests/out/v11/sheets-blind/` 那一套：图上的列名是“方法 A、B、C……”，每个样本各列的左右顺序是打乱的，
打分表按列名填；列名和方法的对应关系在 `blind-key.csv` 里，打完再对回去，打分之前不要看。

做法：

1. 每人拿一份打分表（或者在同一份里各填一列），各打各的，打完之前不互相商量。
2. 图放大到 100% 看（一格 520 像素宽，对应纸上 44 mm），不要缩小了看，也不用放大到 200% 以上找毛病。
3. 只看被抹掉的那一行所在的位置（对着“掩膜”那一格找），拿每个方法的那一格和最右边的对照比，打一个 1–5 的整数。
   上下别的行是没动过的，不用管。
4. 打的是“把原来的字抹掉之后，这块背景能不能用”。产品里抹掉之后会在上面排新的字，新字会盖住一部分痕迹，
   但这里不放新字，按最不利的情况打。

分数的含义：

| 分 | 含义 |
|---|---|
| 5 | 看不出改过。不看掩膜那一格，找不出哪里修补过 |
| 4 | 仔细看能找到轻微的痕迹（一小块颜色略有不同、底纹略微发糊），不指出来一般注意不到 |
| 3 | 一眼看得出这里修补过（发糊、有色块、底纹断了），但范围不大，在上面排上新的文字之后可以接受 |
| 2 | 痕迹明显：大片色块，底纹或印章被明显破坏，要再用修复画笔手工修才能用 |
| 1 | 不能用：原来的字还认得出来，或者背景、印章被大面积破坏，还不如不修 |

压着红章的样本另外留意两件事，都算在分数里：章的线条和字被抹掉了多少；有没有把旁边的黑字或章的碎片搬到这块来。

打完之后汇总（每类背景、每种方法的平均分，4 分以上、2 分以下的比例，评分人之间差多少）：

```powershell
D:\workspace\magpdf-wt\venvs\v11\Scripts\python tests/prototypes/v11-inpaint/score_summary.py <填好的表.csv> [更多的表 ...]
# 用的是不写方法名的那一套：
D:\workspace\magpdf-wt\venvs\v11\Scripts\python tests/prototypes/v11-inpaint/score_summary.py <填好的表.csv> --key tests/out/v11/sheets-blind/blind-key.csv
```

对照图上默认放六列：`fill-local`、`fill-local-grain`、`telea-r3`、`fsr-fast`、`shiftmap`、`lama`，都用 `auto-d3` 掩膜。
50 个样本 × 6 列，每人 300 个分。要换列用 `sheets.py --methods`；写成“方法@掩膜”可以让某一列换一套掩膜
（比如 `lama@auto-d7` 是掩膜往外扩 7 像素之后的 LaMa）。

## 文件

| 文件 | 内容 |
|---|---|
| `common.py` | 路径、读写图和 JSON、四边形和掩膜的小工具 |
| `gen_samples.py` | 造样本 |
| `ocr.py` | 调 `magpdf-cli ocr`，取目标行的行框，算读出来的字和原文的相似度 |
| `masks.py` | 从行框里分出字的像素 |
| `inpaint.py` | 各种修补方法 |
| `lama.py`、`fetch_lama.py` | LaMa 的推理和窗口排法；下载模型 |
| `evaluate.py`、`faint_traces.py` | 客观评价；另算一种更严的指标（差得不多但连成字形的浅痕） |
| `sheets.py`、`score_summary.py` | 对照图和打分表；汇总填好的打分表 |
| `bench.py`、`bench_init.py` | 量时间；量 LaMa 建会话的时间省不省得掉 |
| `run_all.py` | 一条命令跑全部 |
| `requirements.txt`、`requirements-contrib.txt` | 两个虚拟环境的依赖（写死版本，注了许可证） |
