# 用 bindgen 从预编译库自带的头文件重新生成 src/bindings.rs。
# 前提：cargo install bindgen-cli；libclang 可用（VS Build Tools 的 "C++ Clang Compiler" 组件，或设置 LIBCLANG_PATH）；
#       fpdfview.h 在 Windows 上包含 windows.h，所以要在 VS 开发者环境（vcvars64）里运行，让 clang 找到 Windows SDK 头文件。
# 用法：pwsh engine/pdfium-sys/regen-bindings.ps1 [-IncludeDir <目录>]
param(
    [string]$IncludeDir = (Join-Path $PSScriptRoot '..\..\third_party\pdfium-prebuilt\include')
)
$ErrorActionPreference = 'Stop'
$IncludeDir = (Resolve-Path $IncludeDir).Path
$out = Join-Path $PSScriptRoot 'src\bindings.rs'

bindgen (Join-Path $PSScriptRoot 'wrapper.h') `
    --output $out `
    --rust-target 1.85 `
    --rust-edition 2024 `
    --allowlist-file '.*[\\/]fpdf[^\\/]*\.h' `
    --no-layout-tests `
    --no-prepend-enum-name `
    --default-enum-style consts `
    --use-core `
    --merge-extern-blocks `
    --sort-semantically `
    --raw-line '// 由 regen-bindings.ps1 生成，不要手工修改。' `
    -- -I $IncludeDir --target=x86_64-pc-windows-msvc

Write-Host "written: $out"
