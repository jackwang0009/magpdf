# V10 GPU 加速评估：测量脚本

同样的 ONNX 模型、同样的输入，比 CPU 和 DirectML（每块显卡各一遍）的模型推理耗时、初始化时间、内存、连续 1000 页的稳定性、输出差多少，另外量安装包要多带多少。只量模型推理这一段（`InferenceSession.run`），前后处理不算；CPU 上整条 OCR 流水线每页多久见 V09。

评估用 Python 做，不动 Rust 代码。结论和数字在 `docs/p0/V10-GPU加速评估.md`。

## 重量一遍

在工作树根目录下一条命令：

```powershell
D:\workspace\magpdf-wt\venvs\v10\Scripts\python.exe tests/perf/gpu/run_all.py
```

结果写到 `tests/out/v10/`（不入库）：

- `summary.md`、`summary.json`：汇总，报告里的表从这里抄。
- `runs/*.json`：每个测量进程一份原始结果。
- `env.json`：显卡、驱动、DirectML、ONNX Runtime 的版本；`winml.json`：Windows ML 的目录在这台机器上列出了什么；`pkg-sizes.json`：安装包大小。

已经量过的项默认跳过，所以中途断了再跑同一条命令会接着量；`--force` 全部重来。常用参数：

| 参数 | 作用 |
|---|---|
| `--quick` | 只看脚本跑不跑得通：1 次、2 页、连续处理 20 页，结果另放在 `tests/out/v10-quick/`，几分钟 |
| `--dev` | 子进程带 `-X dev` 运行并打印它们的标准错误输出，检查有没有警告（不用来量时间） |
| `--only timing,summarize`、`--skip soak` | 只做、跳过某几步；步骤名见 `run_all.py` 开头 |
| `--targets cpu-1.31,dml0` | 只量标签里含这些字样的配置 |
| `--runs 5`、`--soak-pages 1000` | 每种配置起几个进程、连续处理多少页 |
| `--no-priority-pass` | 不量“测量进程优先级调高一档”的那一份（见下面） |
| `--all-adapters` | 同名的 DXGI 适配器也各量一遍（默认只量第一块） |

这台开发机上全部跑一遍约 4 小时，其中连续处理 1000 页（CPU、DirectML 各一遍）约 1 小时。

## 环境

三个虚拟环境，都在 D 盘，不往全局 Python 里装东西：

```powershell
$env:TEMP = $env:TMP = 'D:\workspace\magpdf-wt\tmp\v10'
$env:PIP_CACHE_DIR = 'D:\workspace\magpdf-wt\pip-cache'
$py = 'D:\Programs\Python\Python312\python.exe'

& $py -m venv D:\workspace\magpdf-wt\venvs\v10          # DirectML 版 ONNX Runtime，run_all.py 用它启动
D:\workspace\magpdf-wt\venvs\v10\Scripts\python.exe -m pip install -r tests/perf/gpu/requirements.txt

& $py -m venv D:\workspace\magpdf-wt\venvs\v10-cpu      # 和产品一致的 CPU 版 1.31.1
D:\workspace\magpdf-wt\venvs\v10-cpu\Scripts\python.exe -m pip install -r tests/perf/gpu/requirements-cpu.txt

& $py -m venv D:\workspace\magpdf-wt\venvs\v10-winml    # Windows ML 那份运行时，可选
D:\workspace\magpdf-wt\venvs\v10-winml\Scripts\python.exe -m pip install -r tests/perf/gpu/requirements-winml.txt
```

- `onnxruntime` 和 `onnxruntime-directml` 装出来是同一个 Python 模块，不能放在一个环境里，所以分开。`run_all.py` 按名字找另外两个环境（启动它的环境名后面加 `-cpu`、`-winml`），也可以用 `--python-cpu`、`--python-winml` 指定；没有就不量那一项，并在汇总里写明。
- 带 DirectML 的 ONNX Runtime 官方只发到 1.24.4，和产品用的 1.31.1 对不上。所以 CPU 量两份：产品用的 1.31.1，和 DirectML 包里自带的 CPU 执行提供程序（同一个构建，用来分清“版本不同”和“换了执行提供程序”）。
- 模型、固定的命令行工具只读使用：`D:\workspace\magpdf\models`（或环境变量 `MAGPDF_MODELS_DIR`、参数 `--models-dir`）、`D:\workspace\magpdf-wt\bin\magpdf-cli.exe`（`MAGPDF_CLI`、`--cli`）。
- `sizes` 一步第一次要从 NuGet 下载约 530 MB 的包，放在 `D:\workspace\magpdf-wt\tmp\v10\nuget`，之后不重下。

## 文件

| 文件 | 做什么 |
|---|---|
| `run_all.py` | 一条命令的驱动：找出这台机器上能量的配置，按步骤起测量进程，最后汇总 |
| `bench.py` | 一个进程量一种配置。五种量法：`timing`、`soak`、`consistency`、`nodes`、`shapes` |
| `workload.py` | 把页面图和文字行框变成模型输入，预处理照 `engine/magpdf-scan/src/ocr/` 逐步移植到 numpy |
| `sysinfo.py` | DXGI 适配器（顺序就是 DirectML 的 `device_id`）、显卡驱动和设备状态、DLL 版本、进程内存、显存、CPU 和显卡占用。只用标准库 |
| `winml_probe.py` | 看 Windows ML 的目录对这台机器给出哪些执行提供程序。只看不下载；`--ensure-ready` 会往系统里装东西，默认不做 |
| `pkg_sizes.py` | 从 NuGet 取几个包，列出 64 位 DLL 的大小、版本、LZMA 压过之后的大小、许可证文件 |
| `requirements*.txt` | 三个虚拟环境的依赖，带版本号和许可证 |

## 量法

- 样本：`tests/corpus/synthetic-ocr/gen_synthetic.py` 生成的 18 页合成样本（V09 的那一套：300 dpi 的 A4 打印体单证，内容全部虚构；英文、中文、中英混排各 6 页，每种有干净的、加噪声的、转 ±1.5° 的、又加噪声又转的）。文字行的框来自固定的命令行工具跑出来的结果，Python 这边不做检测的后处理。这样每一页的“工作量”是真的：检测一次，全部文字行各做一次方向分类和识别，分批和补齐的办法和产品一样。
- 输入事先算好存成缓存，测量进程只做推理。会话的设置和 `runtime.rs` 一样（图优化开到最高一级，线程数交给 ONNX Runtime）；DirectML 另按它的要求关掉内存模式优化、用顺序执行。
- 每种配置每次都起一个新进程，配置之间轮流跑，默认各 5 次。进程里整套页面过 3 遍：第 1 遍单独记（各种输入形状都是头一次出现），第 2 遍起算稳定值。表里的写法是“中位数（最小–最大）”：先在进程里取各页的中位数，再对各次运行取中位数、最小、最大。
- 初始化分三段记：装载 ONNX Runtime、建三个会话、第一页的推理。
- 内存是进程的峰值工作集；显存读性能计数器 `GPU Process Memory`（任务管理器里那两列的来源），集成显卡主要看“共享”那一列。
- 每次都记下量的那一段时间里别的进程占了多少 CPU、同一块显卡上别的进程占了多少。
- 标签带 `-prio` 的是把测量进程的优先级调到“高于正常”量的。开发机上同时有别的任务抢 CPU 时，正常优先级量出来的 CPU 推理时间会被拉长好几倍；调高一档之后推理线程能排到别的任务前面，更接近机器空闲时的情况（不完全等于：同一个物理核上的另一个线程、内存带宽仍然被别的任务占着）。每页耗时两份都量；换宽度的代价、宽度分档、连续处理只量调高的那一份。
- 要的执行提供程序没有起来就算失败（Python 接口默认会悄悄回退到 CPU）。
- `consistency`：同一个进程里各建一套 CPU 和 DirectML 的会话，同一个输入逐页比输出，并把识别结果解码成文字，和命令行工具给出的文字比。
- `soak`：18 页轮着用，连续 1000 页，一个进程从头跑到尾。看有没有报错、内存和显存涨不涨（对页序号做直线拟合）、耗时漂不漂（按整数轮分窗口，第 2 个窗口对最后一个）。每 50 页另存一份到目前为止的结果（`*.partial.json`），进程中途被结束时不至于什么都没留下。
- 日志和文件名里只有页的编号，不出现文档内容。

## 已知的限制

- 只在这台开发机上量过。这台机器上独立显卡（GTX 1070）在设备管理器里是错误状态，DXGI 里看不到它，所以 DirectML 只量了集成显卡；脚本会自己找 DXGI 里能看到的每一块硬件显卡，显卡好了之后重跑同一条命令就会多出一种配置。
- Windows ML 只量了它自带的那份 ONNX Runtime 加 DirectML；它的目录里按硬件下发的执行提供程序（这台机器上一个也没有列出来）没有量。
- 评估用的是 Python 接口，产品里是 Rust 的 `ort`。两边调的是同一个 `onnxruntime.dll`，推理本身一样，但产品里怎么接（`ort` 的特性、随包带哪些 DLL、什么时候回退到 CPU）只做了一个不入库的小程序核对，写在报告里，没有在产品代码里实现。
