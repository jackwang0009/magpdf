# third_party/pdfium：MagPDF 的 PDFium 分支与构建脚本

PDFium 分支本身（`magpdf-pdfium` 仓库，分支 `mag/main`）**不在**这个目录里，这里只放构建它所需的脚本、参数、补丁与 CI 草案。
`pdfium-sys` 默认链接 `third_party/pdfium-prebuilt/`（pdfium-binaries 原版），引擎组用这里的脚本构建分支产物后，
把 `PDFIUM_PREBUILT_DIR` 指向 `<out>/dist` 即可切换，Rust 侧零改动（V01 报告：`docs/p0/V01-原生构建与预编译库.md`）。

```
third_party/pdfium/
├── sync.ps1               从零同步：depot_tools、.gclient、PDFium、DEPS；整理 upstream / mag/main 分支
├── build.ps1              gn gen + ninja，产出与 pdfium-binaries 布局一致的 dist/
├── apply-patches.ps1      把 patches/<子仓库>/ 下的补丁打到 DEPS 子仓库（幂等）
├── upstream-branch.txt    当前跟踪的上游稳定分支（chromium/8138）
├── args/release.gn        Release 构建参数模板（带 PDB）
├── args/debug.gn          Debug 构建参数模板
├── patches/pdfium/        mag/main 相对 upstream 的提交（git format-patch），远程仓库建好前的唯一载体
├── patches/build/         对 DEPS 子仓库 build/（Chromium 构建系统）的本地补丁
└── ci/build-pdfium.yml    magpdf-pdfium 仓库的 GitHub Actions 草案
```

## 1. 分支约定（《工程规范》§6）

| 分支 | 内容 | 规则 |
|---|---|---|
| `upstream` | 严格等于上游 `origin/chromium/NNNN`（`upstream-branch.txt`，现为 `chromium/8138`，即 PDFium 159.0.8138.0） | 只读镜像，不在上面提交 |
| `mag/main` | `upstream` + 我们的提交 | 每个修复/扩展一个独立提交，前缀 `[upstream-candidate]` / `[mag-ext]` / `[mag-local]`；上游 clang-format 风格 |

`chromium/8138`～`chromium/8141` 四个上游分支头指向同一个提交（`5f092b5299`），`build.ps1` 推导版本号时优先采用 `upstream-branch.txt` 里钉住的分支号。

当前 `upstream..mag/main` 只有一个提交：

```
9503125169 [mag-local] build: 以共享库形式构建 pdfium.dll
```

它新增 gn 参数 `pdf_is_shared_lib`（`pdfium.gni`），为真时 `pdfium` 目标用 `shared_library` 而不是 `component`，
并通过 `pdfium_public_config` 定义 `FPDF_SHARED_LIBRARY`，让 `public/fpdfview.h` 里的 `FPDF_EXPORT` 在非 component 构建下
也展开成 `dllexport` / `dllimport`。只改了 3 个文件、28 行，上游逻辑在默认值下完全不变。导出表与 pdfium-binaries 完全一致（471 个符号）。

### 远程仓库建好之后

`magpdf-pdfium` 仓库目前还没有创建，`mag/main` 靠 `patches/pdfium/*.patch` 重放。建好后：

1. 在本地检出里 `git remote add magpdf <url>`，推送 `upstream` 与 `mag/main`（`git push magpdf upstream mag/main`）。
2. 之后同步用 `sync.ps1 -MagRemote <url>`；`patches/pdfium/` 退役（保留到首个 tag 发布为止，作为可追溯记录）。
3. 把 `ci/build-pdfium.yml` 复制到该仓库的 `.github/workflows/`，改好 `MAGPDF_REPO`。
4. 在主仓库把它挂成子模块：`git submodule add -b mag/main <url> third_party/pdfium/src`，并在 `sync.ps1` 把默认 `-Root` 指向子模块所在目录
   （子模块只含 PDFium 本体，DEPS 子仓库依然由 gclient 管理，因此 `.gclient` 要放在子模块的父目录）。
   在此之前**不要**提交子模块，`.gitmodules` 保持为空。

## 2. 环境要求（Windows x64）

| 项 | 要求 | 说明 |
|---|---|---|
| **git `core.autocrlf`** | **必须为 `false`** | PDFium 没有把 `testing/resources/*.pdf` 标成 binary，autocrlf 会把约 270 个测试 PDF 改成 CRLF（本机 346 个里有 276 个），`pdfium_unittests` 和往返基线因此不可比。克隆前 `git config --global core.autocrlf false`；已坏掉的检出用 `git -C pdfium -c core.autocrlf=false checkout -- testing/resources` 恢复。取样一律用 `git cat-file blob` 或 `git archive` 取原始字节（《工程规范》第 1 节、V01 报告 5、V02 报告 4.2） |
| Visual Studio 2022 Build Tools | MSVC v143（已验证 14.44.35207）、Windows 11 SDK（已验证 10.0.26100） | 不需要完整 VS，也不需要 SDK 的 "Debugging Tools"（见 `patches/build/`） |
| depot_tools | `D:\depot_tools`（`sync.ps1` 没有就克隆） | 只用 `gclient`；gn / ninja / clang / python 都由 DEPS 带来，不用系统里的 |
| 磁盘 | 源码 + 依赖约 3.4 GB（全历史检出，含 .git 460 MB；已排除用不到的 rust-toolchain 2.8 GB），`out/Release` 约 330–370 MB | |
| 网络 | 首次同步要能访问 `*.googlesource.com`、`chrome-infra-packages.appspot.com`、`storage.googleapis.com` | `sync.ps1 -Proxy http://127.0.0.1:7897` 或先设 `HTTPS_PROXY`；构建本身离线即可 |
| 主仓库 `.gitattributes` | 需要 `*.patch text eol=lf` | 否则 `patches/` 下的补丁在 Windows 上被检出成 CRLF，`git apply` 失败；`apply-patches.ps1` 会先规范成 LF 兜底（《工程规范》第 1 节） |

必须的环境变量由脚本自行设置：`DEPOT_TOOLS_WIN_TOOLCHAIN=0`（不拉 Google 内部工具链包）、`GYP_MSVS_OVERRIDE_PATH` / `vs2022_install`
（Build Tools 目录）、`WINDOWSSDKDIR`、`WINDOWS_SDK_VERSION`（见第 5 节）。脚本不依赖 vcvars，可在任意 PowerShell 里运行。

## 3. 构建步骤

```powershell
# 1. 同步（首次约 10–20 分钟视网络而定；之后增量很快）
pwsh third_party/pdfium/sync.ps1 -Proxy http://127.0.0.1:7897 -NoHistory -Jobs 8
#    结果：D:\pdfium-src\.gclient，D:\pdfium-src\pdfium 在 mag/main

# 2. 构建 Release（pdfium.dll + pdfium_test + pdfium_unittests），整理 dist/
pwsh third_party/pdfium/build.ps1 -Source D:\pdfium-src\pdfium -Jobs 12
#    结果：D:\pdfium-src\pdfium\out\Release\dist

# 3. 让 Rust 工作区用它
$env:PDFIUM_PREBUILT_DIR = 'D:\pdfium-src\pdfium\out\Release\dist'
cargo test --workspace
```

`build.ps1` 常用参数：`-Config Debug`、`-Out <目录>`、`-Targets pdfium`（只编库）、`-Clean`（先 `gn clean`）、
`-ExtraArgs 'symbol_level = 2'`、`-Version 159.0.8138.0`（离线或分支号推不出来时手工指定）、`-NoDist`。
多个人或多个 agent 共用一台机器时请用 `-Jobs` 限制并行度。

`dist/` 布局与 pdfium-binaries 完全相同（`include/` 逐字节一致，另多出 `bin/pdfium.dll.pdb` 与 `BUILD-INFO.txt`）：

```
dist/bin/pdfium.dll  bin/pdfium.dll.pdb  lib/pdfium.dll.lib  include/*.h  include/cpp/*.h
dist/VERSION  args.gn  LICENSE  licenses/*  PDFiumConfig.cmake  BUILD-INFO.txt
```

`VERSION` 的格式沿用 pdfium-binaries：`MAJOR`=Chromium 里程碑（在线查 `chromium/src` 的 `refs/branch-heads/NNNN/chrome/VERSION`，
离线时用脚本内置对照表，查不到就写 0 并警告）、`BUILD`=分支号、`MINOR`/`PATCH`=0。
`licenses/` 按 ninja 依赖图收集实际链进 `pdfium.dll` 的第三方组件许可（pdfium-binaries 是写死的列表，
它多了没链进去的 libpng、simdutf，少了实际链进去的 brotli、libc++、libyuv）。

发布给其他组的预编译包命名为 `pdfium-win-x64-<MAJOR.MINOR.BUILD.PATCH>.zip`，在 `magpdf-pdfium` 仓库打 `v*` 标签发 Release；
`scripts/fetch-pdfium-prebuilt.ps1` 在 Release 可用后增加"从我们的 Release 拉取"的开关，让其他组在原版与分支预编译库之间切换（《工程规范》第 7 节、V01 报告 8.5）。

## 4. 耗时与体积（Ryzen 7 9800X3D 16 线程 / NVMe，`-Jobs 12`，另有 5 个 agent 在同时编译）

| 项 | 数值 |
|---|---|
| gn gen | 2.7–2.9 s（684 targets） |
| 全新构建 pdfium + pdfium_test + pdfium_unittests | 1494 步，**1 分 41–45 秒** 墙上时间，累计 CPU 约 20 分钟 |
| `build.ps1 -Clean` 端到端（含 dist 整理） | 2 分 11 秒 |
| 增量：改一个 .cpp 重链 pdfium.dll | 1.8 s；无改动 0.08 s |
| pdfium_unittests | 1158 个用例 1.5 s（本机 4 个环境相关失败，见 V01 报告） |
| pdfium.dll / .pdb / .lib | 7.46 MB / 32.80 MB（symbol_level=1）/ 0.11 MB；dist 合计 40.9 MB |
| out/Release | 约 330–370 MB |
| Debug（只建 pdfium 目标） | 1251 步，1 分 09 秒；pdfium.dll 23.8 MB、PDB 114 MB |

## 5. 常见错误

| 现象 | 原因 | 处理 |
|---|---|---|
| `gn gen`：`No supported Visual Studio can be found` | `build/vs_toolchain.py` 只认 `vs2022_install` 环境变量，不认 `GYP_MSVS_VERSION` | `build.ps1` 自动设置；手工构建请 `$env:vs2022_install = 'D:\BuildTools'` |
| `gn gen`：`...\Windows Kits\10\Include\10.0.28000.0 ... does not exist` | Chromium `build/` 把 SDK 版本写死为 10.0.28000.0（Win11 预览 SDK） | `patches/build/0001-...patch` 改为可用 `WINDOWS_SDK_VERSION` 覆盖，脚本默认取本机最新 SDK |
| `gn gen`：找不到 `dbghelp.dll` / "Debugging Tools for Windows" | Chromium 要复制 SDK Debuggers 里的 dbghelp.dll，Build Tools 默认不装这个 SDK 组件 | 同一补丁：未安装时跳过（PDFium 不用它） |
| 编译 `fileapi.h`：`FILE_INFO_BY_HANDLE_CLASS` 未定义 | `NTDDI_VERSION=NTDDI_WIN11_BR` 只在 SDK 28000+ 存在 | 同一补丁：改为 `NTDDI_WIN11_GE`（26100 SDK） |
| ninja 报 siso / reclient / `use_remoteexec` 相关错误 | PDFium `.gn` 默认 `use_siso = true` | `args/*.gn` 已设 `use_siso = false`、`use_remoteexec = false`，不要用 autoninja |
| `gclient sync`：`cipd ensure` 失败、`Failed to fetch ... gn` | 代理只建立了 CONNECT 隧道但 TLS 握手失败 | 换可用代理后重跑 `sync.ps1`；git 依赖和 CIPD/hooks 是分开的，前者成功后 gn/clang 若已存在仍可构建 |
| `testing/resources/*.pdf` 被 PDFium 自己的测试判为损坏 | 全局 `core.autocrlf=true` 把没有 `.gitattributes` 保护、被判为文本的 PDF 改成 CRLF（本机 347 个里有 292 个） | 克隆前 `git config --global core.autocrlf false`，或 `git -C pdfium -c core.autocrlf=false checkout -- testing/resources`；取样请用 `git cat-file blob` |
| `pdfium_unittests` 的 `WideString.ToDefANSI/FromDefANSI` 失败 | 系统代码页 936（GBK）而非 1252 | 环境相关，CI（en-US）不会出现 |
| `apply-patches: ... 既不能应用也不是已应用状态` | DEPS 滚动后 `build/` 变了 | 手工在 `build/` 里改好，`git -C build diff > patches/build/0001-*.patch` 更新补丁 |

## 6. 同步上游（每季度，《工程规范》§6）

1. 改 `upstream-branch.txt` 为新的 `chromium/NNNN`；`sync.ps1` 会把 `upstream` 指过去。
2. `git -C D:\pdfium-src\pdfium rebase upstream mag/main`，逐个解决冲突，保持每个提交独立。
3. `gclient sync` 后检查 `patches/build/` 是否还能应用（`apply-patches.ps1 -Check`）。
4. `build.ps1 -Clean`，跑 `pdfium_unittests`、`cargo test --workspace`（指向新 dist）、V02 全量往返测试一周无退步，再更新主仓库子模块与 `scripts/fetch-pdfium-prebuilt.ps1` 的版本。
5. 重新导出 `patches/pdfium/`（`git format-patch upstream..mag/main -o third_party/pdfium/patches/pdfium/`，先清空旧文件）——远程仓库建立后此步省略。
