# MagPDF

面向货代单证的对象级 PDF 编辑器。技术方案与配套文档见 [`docs/`](docs/)，入口是 [`docs/方案.md`](docs/方案.md)。
当前阶段：P0 技术验证（[`docs/P0技术验证计划.md`](docs/P0技术验证计划.md)）。

## 目录

按方案第 12 节组织：

```
apps/desktop/          Tauri 2 主进程（Rust）+ 界面入口（React 19 + Vite）
packages/ui            通用界面控件
packages/editor        画布、工具状态机、选择与变换、快捷键、工作区
packages/protocol      界面与引擎之间的命令和数据类型（由 Rust 生成）
engine/pdfium-sys      PDFium 与 MAG_* 扩展的 Rust 绑定（工作区里唯一允许 unsafe 的 crate）
engine/magpdf-engine   引擎进程：命令与撤销、保真校验、命中测试、渲染调度、保存与恢复
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
pnpm install && pnpm test && pnpm dev    # 界面，浏览器打开 http://localhost:1420
```

代码检查：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo deny check`、
`pnpm lint`、`pnpm format:check`、`pnpm typecheck`。CI 对 PR 跑同样的检查（`.github/workflows/ci.yml`）。

## 规范

分支、提交、评审、编码规范、依赖白名单、完成的定义见 [`docs/工程规范.md`](docs/工程规范.md)。
修改协议、文件格式、组件参数时，先改对应文档并评审，再改代码。
