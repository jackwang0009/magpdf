# MagPDF

面向货代单证的对象级 PDF 编辑器。技术方案与配套文档见 [`docs/`](docs/)，入口是 [`docs/方案.md`](docs/方案.md)。
当前阶段：P0 技术验证（[`docs/P0技术验证计划.md`](docs/P0技术验证计划.md)）。

## 目录

按方案第 12 节组织：

```
apps/desktop/          Tauri 2 主进程（Rust，src-tauri）+ 界面入口（React 19 + Vite）+ 界面端到端测试（e2e）
packages/ui            通用界面控件
packages/editor        画布、工具状态机、选择与变换、快捷键、工作区
packages/protocol      界面与引擎之间的命令和数据类型（由 Rust 生成，cargo test -p magpdf-ipc 时更新）
engine/pdfium-sys      PDFium 与 MAG_* 扩展的 Rust 绑定（unsafe 只允许出现在这里，以及 magpdf-ipc 和主进程里调用 Windows 接口的模块）
engine/magpdf-engine   引擎服务：命令与撤销、保真校验、命中测试、渲染调度、保存与恢复
engine/magpdf-engine-proc  引擎进程 magpdf-engine：管道通信、PDFium 专用线程、共享内存瓦片
engine/magpdf-ipc      主进程与引擎之间的帧格式、消息和共享内存
engine/magpdf-content  自研内容流读写层原型（P0 的 V17）
engine/magpdf-text     文字块识别、排版、字体匹配、子集化
engine/magpdf-components  组件、条码、校验位、批量生成、打印
engine/magpdf-scan     OCR、版面分析、样式估计、背景修补
engine/magpdf-cli      命令行：往返测试、保真校验、批量比对（CI 用）
third_party/           PDFium 分支、Zint、ZXing-C++（子模块）与预编译库
tests/corpus           测试文件库（按来源分类）；tests/baselines 像素比对基线
models/  assets/       模型清单与下载脚本；内置素材与模板
```

## 开发环境（Windows x64）

| 工具 | 版本 | 说明 |
|---|---|---|
| Visual Studio Build Tools 2022 | C++ 桌面工作负载、Windows 11 SDK、Clang | Rust msvc 目标与 PDFium 都需要 |
| Rust | `rust-toolchain.toml` 固定版本 | `rustup` 安装 |
| Node.js / pnpm | Node ≥ 22，pnpm 见 `package.json` 的 `packageManager` | |
| PDFium 预编译库 | `scripts/fetch-pdfium-prebuilt.ps1` | 只有引擎组需要从源码构建 PDFium（方案 12） |

```powershell
pwsh scripts/fetch-pdfium-prebuilt.ps1   # 拉取原版 PDFium 预编译库到 third_party/pdfium-prebuilt
cargo test --workspace                   # 引擎
cargo run -p magpdf-cli -- info 某文件.pdf
cargo run -p magpdf-cli -- fidelity 某文件.pdf   # 逐页报告能不能修改原有对象；不能的页只能叠加内容（方案 2.3）
pnpm install && pnpm test && pnpm dev    # 界面，浏览器打开 http://localhost:1420（没有引擎，显示的是模拟文档）
```

桌面程序（界面接着真实的引擎）：

```powershell
pnpm dev                                    # 界面的开发服务器，留着别关
cargo build -p magpdf-engine-proc           # 另开一个终端。引擎要和主进程在同一个目录（target\debug）
cargo run -p magpdf-desktop -- 某文件.pdf   # 不给文件就用「文件 → 打开」

# 发布版（界面打进可执行文件）和界面端到端冒烟测试
pnpm --filter @magpdf/desktop build
cargo build --release -p magpdf-engine-proc
cargo build --release -p magpdf-desktop --features tauri/custom-protocol
pnpm --filter @magpdf/desktop e2e

# 性能测量用的页面（5000 个对象的 A4 页和图纸、300 页的报表、300 dpi 整页“扫描件”等）生成到 tests/out/perf。
# 在它们上面量拖动的帧间隔：见 docs/p0/V04-分层渲染与拖动预览原型.md 第 3.1 节
python -m pip install -r tests/requirements-test.txt
python tests/corpus/perf/generate.py
# 引擎一侧各步各花多久（加载页面、渲染瓦片、列对象、保真校验、挪一个对象），要用发布版
cargo run --release -p magpdf-cli -- perf tests/out/perf/drawing-5000-objects.pdf
```

代码检查：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo deny check`、
`pnpm lint`、`pnpm format:check`、`pnpm typecheck`。CI 对 PR 跑同样的检查（`.github/workflows/ci.yml`）。

## 规范

分支、提交、评审、编码规范、依赖白名单、完成的定义见 [`docs/工程规范.md`](docs/工程规范.md)。
修改协议、文件格式、组件参数时，先改对应文档并评审，再改代码。
