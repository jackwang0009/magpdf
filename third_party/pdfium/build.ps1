# 构建 MagPDF 的 PDFium 分支（mag/main）：gn gen + ninja，产出可动态链接的 pdfium.dll，
# 并把产物整理成与 pdfium-binaries 完全一致的布局到 <Out>/dist/，供 pdfium-sys 通过 PDFIUM_PREBUILT_DIR 直接使用：
#   dist/bin/pdfium.dll (+ pdfium.dll.pdb)   dist/lib/pdfium.dll.lib   dist/include/*.h  dist/include/cpp/*.h
#   dist/VERSION   dist/args.gn   dist/LICENSE   dist/licenses/*   dist/BUILD-INFO.txt
#
# 前提：源码已用 sync.ps1（或手工 gclient sync）同步好，并切到 mag/main（含 [mag-local] pdf_is_shared_lib 提交）。
# 不依赖 vcvars 环境：GN 自己通过 build/vs_toolchain.py 调 vcvarsall.bat；本脚本只设好它需要的环境变量。
#
# 用法（在主仓库根目录）：
#   pwsh third_party/pdfium/build.ps1                                   # Release，out/Release，全部核心数
#   pwsh third_party/pdfium/build.ps1 -Config Debug -Jobs 12
#   pwsh third_party/pdfium/build.ps1 -Source D:\pdfium-src\pdfium -Out D:\pdfium-out\rel -Targets pdfium
#   pwsh third_party/pdfium/build.ps1 -Version 159.0.8138.0 -ExtraArgs 'symbol_level = 2'
# 完成后：$env:PDFIUM_PREBUILT_DIR = "<Out>\dist"; cargo test --workspace
#
# 环境变量默认值：PDFIUM_SRC（源码目录）、HTTPS_PROXY（只用于在线查询 Chromium 主版本号，可不设）。
# 细节、耗时与常见错误见同目录 README.md。
#Requires -Version 5.1
[CmdletBinding()]
param(
    # PDFium 源码目录（gclient 检出里的 pdfium/，含 .gn 与 BUILD.gn）
    [string]$Source = $(if ($env:PDFIUM_SRC) { $env:PDFIUM_SRC } else { 'D:\pdfium-src\pdfium' }),
    # 输出目录；相对路径相对于 Source（gn 惯例 out/Release）
    [string]$Out = '',
    [ValidateSet('Release', 'Debug')] [string]$Config = 'Release',
    # ninja 并行度；多个人/agent 共用一台机器时请调低（如 12）
    [int]$Jobs = [Environment]::ProcessorCount,
    # 要构建的 ninja 目标；pdfium 总会包含
    [string[]]$Targets = @('pdfium', 'pdfium_test', 'pdfium_unittests'),
    # 追加到 args.gn 末尾的行，如 'symbol_level = 2'
    [string[]]$ExtraArgs = @(),
    # 写进 dist/VERSION 的版本（MAJOR.MINOR.BUILD.PATCH）；默认从 chromium/NNNN 分支推导
    [string]$Version = '',
    # Visual Studio / Build Tools 安装目录；默认用 vswhere 找
    [string]$VsPath = '',
    # Windows SDK 版本；默认取已安装的最新 10.0.x
    [string]$WinSdkVersion = '',
    # 先 gn clean 再构建（保留 args.gn）
    [switch]$Clean,
    # 只构建不整理 dist/
    [switch]$NoDist
)
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$sw = [Diagnostics.Stopwatch]::StartNew()
function Step([string]$msg) { Write-Host ("[{0,8:0.0}s] {1}" -f $sw.Elapsed.TotalSeconds, $msg) -ForegroundColor Cyan }
# 写文本文件：UTF-8 无 BOM、LF 换行（dist 内容与 pdfium-binaries 的产物逐字节一致，也避开 autocrlf）
function WriteLf([string]$path, [string[]]$lines, [bool]$finalNewline = $true) {
    $text = ($lines -join "`n") -replace "`r`n", "`n"
    if ($finalNewline) { $text += "`n" }
    [IO.File]::WriteAllText($path, $text, (New-Object Text.UTF8Encoding $false))
}
function Run([string]$exe, [string[]]$argv, [string]$log = '') {
    if ($log) { & $exe @argv *> $log } else { & $exe @argv }
    if ($LASTEXITCODE -ne 0) {
        if ($log) { Get-Content $log | Select-Object -Last 40 | Write-Host }
        throw "命令失败（exit $LASTEXITCODE）：$exe $($argv -join ' ')"
    }
}

# ---------- 源码与工具 ----------
$Source = (Resolve-Path $Source).Path
foreach ($f in 'BUILD.gn', 'pdfium.gni', 'buildtools\win\gn.exe', 'third_party\ninja\ninja.exe',
    'third_party\llvm-build\Release+Asserts\bin\clang-cl.exe', 'third_party\cpython3\host\bin\python3.exe') {
    if (-not (Test-Path (Join-Path $Source $f))) { throw "源码目录缺少 $f：$Source（先运行 sync.ps1）" }
}
if (-not (Select-String -Path (Join-Path $Source 'pdfium.gni') -Pattern 'pdf_is_shared_lib' -Quiet)) {
    throw "pdfium.gni 里没有 pdf_is_shared_lib：源码不在 mag/main 上（缺少 [mag-local] 共享库提交）"
}
$gn = Join-Path $Source 'buildtools\win\gn.exe'
$ninja = Join-Path $Source 'third_party\ninja\ninja.exe'
if (-not $Out) { $Out = "out\$Config" }
if (-not [IO.Path]::IsPathRooted($Out)) { $Out = Join-Path $Source $Out }
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path
if ($Targets -notcontains 'pdfium') { $Targets = @('pdfium') + $Targets }

# ---------- MSVC / Windows SDK（Build Tools 即可，不需要完整 VS） ----------
if (-not $VsPath) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path $vswhere) {
        $VsPath = & $vswhere -products * -latest -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    }
    if (-not $VsPath) { throw '找不到 Visual Studio / Build Tools（含 C++ x64 工具集），请用 -VsPath 指定' }
}
$vsVersion = & (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe') -path $VsPath -property catalog_productLineVersion 2>$null
if (-not $vsVersion) { $vsVersion = '2022' }
$sdkRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10'
if (-not $WinSdkVersion) {
    $WinSdkVersion = Get-ChildItem (Join-Path $sdkRoot 'Include') -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^10\.0\.\d+\.\d+$' -and (Test-Path (Join-Path $_.FullName 'um\windows.h')) } |
        Sort-Object { [version]$_.Name } -Descending | Select-Object -First 1 -ExpandProperty Name
    if (-not $WinSdkVersion) { throw "在 $sdkRoot 下找不到 Windows 10/11 SDK，请安装或用 -WinSdkVersion 指定" }
}
$msvcVersion = Get-ChildItem (Join-Path $VsPath 'VC\Tools\MSVC') -Directory | Sort-Object { [version]$_.Name } -Descending |
    Select-Object -First 1 -ExpandProperty Name

# depot_tools 的 vs_toolchain.py 默认去拉 Google 内部工具链，必须关掉并告诉它 VS 在哪（vs2022_install 这个名字是它查版本用的）
$env:DEPOT_TOOLS_WIN_TOOLCHAIN = '0'
$env:GYP_MSVS_OVERRIDE_PATH = $VsPath
Set-Item -Path "Env:vs${vsVersion}_install" -Value $VsPath
$env:WINDOWSSDKDIR = $sdkRoot
# 由 patches/build/*.patch 引入：build/ 里写死的 SDK 版本改成可覆盖
$env:WINDOWS_SDK_VERSION = $WinSdkVersion
# gn gen 期间不走代理（googlesource 不需要）；版本查询单独处理
$proxy = $env:HTTPS_PROXY
Remove-Item Env:HTTPS_PROXY, Env:HTTP_PROXY -ErrorAction SilentlyContinue

Step "源码 $Source"
Step "工具链 VS $vsVersion @ $VsPath  MSVC $msvcVersion  Windows SDK $WinSdkVersion  clang $(Get-Content (Join-Path $Source 'third_party\llvm-build\Release+Asserts\cr_build_revision') -ErrorAction SilentlyContinue)"

# ---------- DEPS 仓库补丁（build/：SDK 版本可覆盖、不要求 Debugging Tools、NTDDI 与 SDK 26100 兼容） ----------
& (Join-Path $here 'apply-patches.ps1') -Source $Source

# ---------- args.gn + gn gen ----------
$template = Join-Path $here "args\$($Config.ToLower()).gn"
$argsText = @("# 由 third_party/pdfium/build.ps1 从 $(Split-Path $template -Leaf) 生成于 $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')；改参数请改模板或用 -ExtraArgs。") +
    (Get-Content $template) + $ExtraArgs
$argsFile = Join-Path $Out 'args.gn'
$oldArgs = if (Test-Path $argsFile) { (Get-Content $argsFile | Where-Object { $_ -notmatch '^# 由 third_party' }) -join "`n" } else { $null }
$newArgs = ($argsText | Where-Object { $_ -notmatch '^# 由 third_party' }) -join "`n"
if ($oldArgs -ne $newArgs) { WriteLf $argsFile $argsText; Step "写入 $argsFile" }
if ($Clean) { Step 'gn clean'; Run $gn @('clean', "--root=$Source", $Out) }
Step 'gn gen'
Run $gn @('gen', "--root=$Source", $Out) (Join-Path $Out 'gn-gen.log')
Get-Content (Join-Path $Out 'gn-gen.log') | Select-Object -Last 1 | Write-Host

# ---------- ninja ----------
Step "ninja -j $Jobs $($Targets -join ' ')"
$buildLog = Join-Path $Out 'ninja-build.log'
$t0 = $sw.Elapsed
Run $ninja (@('-C', $Out, '-j', "$Jobs") + $Targets) $buildLog
$buildTime = $sw.Elapsed - $t0
$lastStep = Select-String -Path $buildLog -Pattern '^\[(\d+)/(\d+)\]' | Select-Object -Last 1
$steps = if ($lastStep) { $lastStep.Matches[0].Groups[2].Value } else { 0 }
$warnings = (Select-String -Path $buildLog -Pattern 'warning:' | Measure-Object).Count
Step ("ninja 完成：{0} 步，{1:mm\:ss} 墙上时间，{2} 条警告" -f $steps, $buildTime, $warnings)
if ($NoDist) { exit 0 }

# ---------- 版本号 ----------
# 格式与 pdfium-binaries 相同：MAJOR=Chromium 里程碑，BUILD=chromium/NNNN 分支号，MINOR/PATCH 固定 0。
# BUILD 取 HEAD 可达的最大 origin/chromium/NNNN（需要完整历史和上游分支引用）；拿不到就用 upstream-branch.txt。
# MAJOR 需要 Chromium 仓库的 chrome/VERSION（在线，可走 HTTPS_PROXY）；离线时用已知对照表。
function Get-PdfiumVersion {
    if ($Version) {
        if ($Version -notmatch '^\d+\.\d+\.\d+\.\d+$') { throw "-Version 必须是 MAJOR.MINOR.BUILD.PATCH：$Version" }
        return $Version
    }
    # 分支号：HEAD 能回溯到的 origin/chromium/NNNN 里，优先取 upstream-branch.txt 钉住的那个
    # （相邻的若干个 chromium/NNNN 分支头常常指向同一个提交，例如 8138..8141），否则取最大的。
    $pinned = (Get-Content (Join-Path $here 'upstream-branch.txt') -ErrorAction SilentlyContinue | Select-Object -First 1)
    $pinnedBuild = if ($pinned -match 'chromium/(\d+)') { [int]$matches[1] } else { $null }
    $reachable = @()
    $refs = & git -C $Source for-each-ref --format='%(refname:short) %(objectname)' 'refs/remotes/origin/chromium/*' 2>$null
    if ($refs) {
        $hist = New-Object 'System.Collections.Generic.HashSet[string]'
        & git -C $Source rev-list HEAD 2>$null | ForEach-Object { [void]$hist.Add($_) }
        foreach ($line in $refs) {
            $name, $sha = $line -split ' '
            if ($name -match '/chromium/(\d+)$' -and $hist.Contains($sha)) { $reachable += [int]$matches[1] }
        }
    }
    $build = $null
    if ($reachable.Count -gt 0) {
        $build = if ($null -ne $pinnedBuild -and $reachable -contains $pinnedBuild) { $pinnedBuild } else { ($reachable | Measure-Object -Maximum).Maximum }
        if ($null -ne $pinnedBuild -and $build -ne $pinnedBuild) { Write-Warning "源码基于 chromium/$build，但 upstream-branch.txt 写的是 $pinned" }
    }
    elseif ($null -ne $pinnedBuild) { $build = $pinnedBuild; Write-Warning "找不到 origin/chromium/* 引用，分支号按 upstream-branch.txt 取 $pinned" }
    if ($null -eq $build) { throw '无法确定 chromium/NNNN 分支号，请用 -Version 指定' }
    $major = $null
    $url = "https://chromium.googlesource.com/chromium/src/+/refs/branch-heads/$build/chrome/VERSION?format=TEXT"
    $curlArgs = @('-sS', '--fail', '-m', '10', $url); if ($proxy) { $curlArgs = @('-x', $proxy) + $curlArgs }
    $b64 = & curl.exe @curlArgs 2>$null
    if ($LASTEXITCODE -eq 0 -and $b64) {
        $text = [Text.Encoding]::ASCII.GetString([Convert]::FromBase64String(($b64 -join '')))
        if ($text -match 'MAJOR=(\d+)') { $major = [int]$matches[1] }
    }
    if ($null -eq $major) {
        $known = @{ 8138 = 159 }  # 离线对照表：分支号 → 里程碑；新分支请补充或用 -Version
        if ($known.ContainsKey($build)) { $major = $known[$build]; Write-Warning "离线：MAJOR 取自内置对照表（$build → $major）" }
        else { $major = 0; Write-Warning "无法确定 Chromium 里程碑，MAJOR 写 0；请用 -Version 指定" }
    }
    return "$major.0.$build.0"
}
$ver = Get-PdfiumVersion
$major, $minor, $build, $patch = $ver -split '\.'

# ---------- dist/ ----------
Step "整理 dist/（版本 $ver）"
$dist = Join-Path $Out 'dist'
if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }
foreach ($d in 'bin', 'lib', 'include\cpp', 'licenses') { New-Item -ItemType Directory -Force (Join-Path $dist $d) | Out-Null }
Copy-Item (Join-Path $Out 'pdfium.dll') (Join-Path $dist 'bin')
if (Test-Path (Join-Path $Out 'pdfium.dll.pdb')) { Copy-Item (Join-Path $Out 'pdfium.dll.pdb') (Join-Path $dist 'bin') }
else { Write-Warning 'symbol_level = 0：没有 pdfium.dll.pdb，崩溃转储将无法符号化' }
Copy-Item (Join-Path $Out 'pdfium.dll.lib') (Join-Path $dist 'lib')
Get-ChildItem (Join-Path $Source 'public') -Filter '*.h' -File | Copy-Item -Destination (Join-Path $dist 'include')
Get-ChildItem (Join-Path $Source 'public\cpp') -Filter '*.h' -File | Copy-Item -Destination (Join-Path $dist 'include\cpp')

# 头文件后处理，与 pdfium-binaries 发布的 include/ 保持一致：
#  1. fpdfview.h：拿掉 FPDF_EXPORT 外层的 COMPONENT_BUILD/FPDF_SHARED_LIBRARY 条件，
#     这份头文件只配 pdfium.dll 使用，使用方无需再定义宏就能拿到 dllimport；
#  2. include/cpp/*.h：#include "public/..." 改成相对路径，脱离源码树也能编译。
$utf8 = New-Object Text.UTF8Encoding $false
$fv = Join-Path $dist 'include\fpdfview.h'
$text = [IO.File]::ReadAllText($fv)
$guard = '(?m)^#if defined\(COMPONENT_BUILD\) \|\| defined\(FPDF_SHARED_LIBRARY\)\r?\n(?://.*\r?\n)*'
$tail = '(?m)^#else\r?\n#define FPDF_EXPORT\r?\n#endif  // defined\(COMPONENT_BUILD\) \|\| defined\(FPDF_SHARED_LIBRARY\)\r?\n'
if ($text -match $guard -and $text -match $tail) {
    $text = [regex]::Replace($text, $guard, ''); $text = [regex]::Replace($text, $tail, '')
    [IO.File]::WriteAllText($fv, $text, $utf8)
}
else { Write-Warning 'fpdfview.h 里没找到预期的 FPDF_EXPORT 条件块，dist/include/fpdfview.h 保持原样（使用方需自行定义 FPDF_SHARED_LIBRARY）' }
foreach ($h in Get-ChildItem (Join-Path $dist 'include\cpp') -Filter '*.h' -File) {
    $t = [IO.File]::ReadAllText($h.FullName)
    $t2 = $t -replace '#include "public/cpp/', '#include "' -replace '#include "public/', '#include "../'
    if ($t2 -ne $t) { [IO.File]::WriteAllText($h.FullName, $t2, $utf8) }
}
Copy-Item $argsFile (Join-Path $dist 'args.gn')
Copy-Item (Join-Path $Source 'LICENSE') (Join-Path $dist 'LICENSE')
WriteLf (Join-Path $dist 'VERSION') @("MAJOR=$major", "MINOR=$minor", "BUILD=$build", "PATCH=$patch")

# CMake 包配置（与 pdfium-binaries 的 PDFiumConfig.cmake 相同，C/C++ 使用方 find_package(PDFium) 即可）
$cmakeText = @"
# PDFium Package Configuration for CMake
#
# To use PDFium in your CMake project:
#
#   1. set the environment variable PDFium_DIR to the folder containing this file.
#   2. in your CMakeLists.txt, add
#       find_package(PDFium)
#   3. then link your executable with PDFium
#       target_link_libraries(my_exe pdfium)

include(FindPackageHandleStandardArgs)

find_path(PDFium_INCLUDE_DIR
    NAMES "fpdfview.h"
    PATHS "`${CMAKE_CURRENT_LIST_DIR}"
    PATH_SUFFIXES "include"
)

set(PDFium_VERSION "$ver")

if(WIN32)
  find_file(PDFium_LIBRARY
        NAMES "pdfium.dll"
        PATHS "`${CMAKE_CURRENT_LIST_DIR}"
        PATH_SUFFIXES "bin")

  find_file(PDFium_IMPLIB
        NAMES "pdfium.dll.lib"
        PATHS "`${CMAKE_CURRENT_LIST_DIR}"
        PATH_SUFFIXES "lib")

  add_library(pdfium SHARED IMPORTED)
  set_target_properties(pdfium
    PROPERTIES
    IMPORTED_LOCATION             "`${PDFium_LIBRARY}"
    IMPORTED_IMPLIB               "`${PDFium_IMPLIB}"
    INTERFACE_INCLUDE_DIRECTORIES "`${PDFium_INCLUDE_DIR};`${PDFium_INCLUDE_DIR}/cpp"
  )

  find_package_handle_standard_args(PDFium
    REQUIRED_VARS PDFium_LIBRARY PDFium_IMPLIB PDFium_INCLUDE_DIR
    VERSION_VAR PDFium_VERSION
  )
else()
  find_library(PDFium_LIBRARY
        NAMES "pdfium"
        PATHS "`${CMAKE_CURRENT_LIST_DIR}"
        PATH_SUFFIXES "lib")

  add_library(pdfium SHARED IMPORTED)
  set_target_properties(pdfium
    PROPERTIES
    IMPORTED_LOCATION             "`${PDFium_LIBRARY}"
    INTERFACE_INCLUDE_DIRECTORIES "`${PDFium_INCLUDE_DIR};`${PDFium_INCLUDE_DIR}/cpp"
  )

  find_package_handle_standard_args(PDFium
    REQUIRED_VARS PDFium_LIBRARY PDFium_INCLUDE_DIR
    VERSION_VAR PDFium_VERSION
  )
endif()
"@
WriteLf (Join-Path $dist 'PDFiumConfig.cmake') @($cmakeText)

# 第三方许可：从 ninja 的依赖图取 pdfium.dll 实际链进去的 third_party 组件（含只用头文件的），按 pdfium-binaries 的文件名放进 licenses/
$objs = & $ninja -C $Out -t inputs pdfium 2>$null | Where-Object { $_ -match '\.obj$' }
$used = New-Object 'System.Collections.Generic.HashSet[string]'
for ($i = 0; $i -lt $objs.Count; $i += 200) {
    $chunk = $objs[$i..([Math]::Min($i + 199, $objs.Count - 1))]
    & $ninja -C $Out -t deps @chunk 2>$null | ForEach-Object {
        if ($_ -match '(?:^|[\\/])third_party[\\/]([^\\/]+)[\\/]') { [void]$used.Add($matches[1]) }
    }
}
# 组件目录 → @{ 目标文件名 = 相对该目录的许可文件 }
$licenseMap = @{
    'abseil-cpp'    = @{ 'abseil.txt' = 'LICENSE' }
    'agg23'         = @{ 'agg23.txt' = 'copying' }
    'brotli'        = @{ 'brotli.txt' = 'LICENSE' }
    'dragonbox'     = @{ 'dragonbox-Apache2-LLVM.txt' = 'src\LICENSE-Apache2-LLVM'; 'dragonbox-Boost.txt' = 'src\LICENSE-Boost' }
    'fast_float'    = @{ 'fast_float.txt' = 'src\LICENSE-MIT' }
    'freetype'      = @{ 'freetype.txt' = 'src\docs\FTL.TXT' }
    'harfbuzz'      = @{ 'harfbuzz.txt' = 'src\COPYING' }
    'highway'       = @{ 'highway.txt' = 'src\LICENSE' }
    'icu'           = @{ 'icu.txt' = 'LICENSE' }
    'lcms'          = @{ 'lcms.txt' = 'src\LICENSE' }
    'libc++'        = @{ 'libcxx.txt' = 'src\LICENSE.TXT' }
    'libc++abi'     = @{ 'libcxxabi.txt' = 'src\LICENSE.TXT' }
    'libjpeg_turbo' = @{ 'libjpeg_turbo.md' = 'LICENSE.md'; 'libjpeg_turbo.ijg' = 'README.ijg' }
    'libopenjpeg'   = @{ 'libopenjpeg.txt' = 'src\LICENSE' }
    'libpng'        = @{ 'libpng.txt' = 'LICENSE' }
    'libtiff'       = @{ 'libtiff.md' = 'src\LICENSE.md' }
    'libyuv'        = @{ 'libyuv.txt' = 'LICENSE' }
    'llvm-libc'     = @{ 'llvm-libc.txt' = 'src\LICENSE.TXT' }
    'simdutf'       = @{ 'simdutf.txt' = 'LICENSE' }
    'zlib'          = @{ 'zlib.txt' = 'LICENSE' }
}
# 构建工具、测试资源、PDFium 自己的包装目录：不是链进 DLL 的代码
$notCode = '^(fx_.*|llvm-build|nasm|cpython3|ninja|depot_tools|clang-format|siso|rust-toolchain|test_fonts|NotoSans.*|googletest|instrumented_libs)$'
Copy-Item (Join-Path $Source 'LICENSE') (Join-Path $dist 'licenses\pdfium.txt')
foreach ($comp in ($used | Sort-Object)) {
    if ($comp -match $notCode) { continue }
    $dir = Join-Path $Source "third_party\$comp"
    if ($licenseMap.ContainsKey($comp)) {
        foreach ($kv in $licenseMap[$comp].GetEnumerator()) {
            $src = Join-Path $dir $kv.Value
            if (Test-Path $src) { Copy-Item $src (Join-Path $dist "licenses\$($kv.Key)") }
            else { Write-Warning "许可文件不存在：$src" }
        }
    } else {
        $found = Get-ChildItem $dir, (Join-Path $dir 'src') -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '^(LICENSE|COPYING)' } | Select-Object -First 1
        if ($found) { Copy-Item $found.FullName (Join-Path $dist "licenses\$comp.txt"); Write-Warning "组件 $comp 不在许可对照表里，已按通用规则复制 $($found.Name)，请核对并补进 build.ps1" }
        else { Write-Warning "组件 $comp 链进了 pdfium.dll 但找不到许可文件，请人工处理" }
    }
}

# 构建信息：以后分析崩溃转储、核对 DLL 来源用
$commit = & git -C $Source rev-parse HEAD 2>$null
$branch = & git -C $Source rev-parse --abbrev-ref HEAD 2>$null
$patchNames = (Get-ChildItem (Join-Path $here 'patches') -Recurse -Filter '*.patch' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.Directory.Name)/$($_.Name)" }) -join ', '
$buildInfo = @(
    "version=$ver"
    "pdfium_commit=$commit"
    "pdfium_branch=$branch"
    "upstream_branch=chromium/$build"
    "config=$Config"
    "built_at=$(Get-Date -Format 'yyyy-MM-ddTHH:mm:ssK')"
    "host=$env:COMPUTERNAME"
    "clang=$(Get-Content (Join-Path $Source 'third_party\llvm-build\Release+Asserts\cr_build_revision') -ErrorAction SilentlyContinue)"
    "msvc=$msvcVersion"
    "windows_sdk=$WinSdkVersion"
    "gn=$((& $gn --version) -join ' ')"
    "ninja=$(& $ninja --version)"
    "patches=$patchNames"
    "build_wall_time=$($buildTime.ToString('hh\:mm\:ss'))"
    "jobs=$Jobs"
)
WriteLf (Join-Path $dist 'BUILD-INFO.txt') $buildInfo

# ---------- 汇总 ----------
function MB($p) { if (Test-Path $p) { '{0:N2} MB' -f ((Get-Item $p).Length / 1MB) } else { '-' } }
$distSize = '{0:N1} MB' -f ((Get-ChildItem $dist -Recurse -File | Measure-Object Length -Sum).Sum / 1MB)
Step "完成。dist = $dist"
Write-Host ("  pdfium.dll {0}  pdfium.dll.pdb {1}  pdfium.dll.lib {2}  dist 合计 {3}" -f (MB "$dist\bin\pdfium.dll"), (MB "$dist\bin\pdfium.dll.pdb"), (MB "$dist\lib\pdfium.dll.lib"), $distSize)
Write-Host ("  licenses/: {0}" -f ((Get-ChildItem "$dist\licenses" | Select-Object -ExpandProperty Name) -join ', '))
Write-Host "  使用：`$env:PDFIUM_PREBUILT_DIR = '$dist'; cargo test --workspace"
