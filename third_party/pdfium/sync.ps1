# 从零（或在已有检出上）同步 MagPDF 的 PDFium 源码：depot_tools、.gclient、PDFium 本体、DEPS 依赖，
# 并按《工程规范》§6 摆好分支：upstream → origin/<chromium/NNNN>，mag/main 在其上叠加 [mag-local] 提交。
#
# 用法（默认值都对应本机约定，可按需覆盖）：
#   pwsh third_party/pdfium/sync.ps1                         # Root=D:\pdfium-src, DepotTools=D:\depot_tools
#   pwsh third_party/pdfium/sync.ps1 -Root E:\src -Proxy http://127.0.0.1:7897 -NoHistory -Jobs 8
#   pwsh third_party/pdfium/sync.ps1 -MagRemote https://github.com/<org>/magpdf-pdfium.git   # 仓库建好之后
#   pwsh third_party/pdfium/sync.ps1 -NoSync                 # 离线：只整理分支/重放补丁，不跑 gclient sync
#   pwsh third_party/pdfium/sync.ps1 -Root $PWD -DepsOnly -NoHistory   # CI：检出已由 actions/checkout 放好，只拉 DEPS + 打补丁
#
# 结果目录结构：<Root>\.gclient、<Root>\pdfium\（检出，HEAD 在 mag/main）、<Root>\pdfium\build\ 等 DEPS 子仓库。
# 之后用 build.ps1 -Source <Root>\pdfium 构建。
#
# mag/main 的来源：
#   * 给了 -MagRemote：从那里拉 mag/main（远程名固定为 magpdf，避免与分支名 mag/main 混淆）；
#   * 没给且本地还没有 mag/main：在 upstream 上用 git am 重放 patches/pdfium/*.patch 生成（当前就是这种方式）；
#   * 本地已有 mag/main：原样保留，只 checkout 过去。
[CmdletBinding()]
param(
    [string]$Root = $(if ($env:PDFIUM_ROOT) { $env:PDFIUM_ROOT } else { 'D:\pdfium-src' }),
    [string]$DepotTools = $(if ($env:DEPOT_TOOLS) { $env:DEPOT_TOOLS } else { 'D:\depot_tools' }),
    [string]$Remote = 'https://pdfium.googlesource.com/pdfium.git',
    [string]$MagRemote = '',
    [string]$UpstreamBranch = '',
    [switch]$NoHistory,
    [int]$Jobs = 8,
    [string]$Proxy = $env:HTTPS_PROXY,
    [switch]$NoHooks,
    [switch]$NoSync,
    [switch]$DepsOnly
)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$sw = [Diagnostics.Stopwatch]::StartNew()
function Step($msg) { Write-Host ('[{0,8:N1}s] {1}' -f $sw.Elapsed.TotalSeconds, $msg) -ForegroundColor Cyan }
function Run([string]$exe, [string[]]$argv) {
    Write-Host "  > $exe $($argv -join ' ')" -ForegroundColor DarkGray
    & $exe @argv
    if ($LASTEXITCODE -ne 0) { throw "命令失败（exit $LASTEXITCODE）：$exe $($argv -join ' ')" }
}

if (-not $UpstreamBranch) {
    $UpstreamBranch = (Get-Content (Join-Path $here 'upstream-branch.txt') | Where-Object { $_ -and $_ -notmatch '^\s*#' } | Select-Object -First 1).Trim()
}
if ($UpstreamBranch -notmatch '^chromium/\d+$') { throw "上游分支必须形如 chromium/NNNN，现在是 '$UpstreamBranch'" }

# ---------- 环境 ----------
$env:DEPOT_TOOLS_WIN_TOOLCHAIN = '0'   # 不要 Google 内部的 VS 工具链包，用本机 Build Tools
if ($Proxy) {
    $env:HTTP_PROXY = $Proxy; $env:HTTPS_PROXY = $Proxy
    $env:NO_PROXY = 'localhost,127.0.0.1'
    Step "代理 $Proxy"
}
else { Remove-Item Env:HTTP_PROXY, Env:HTTPS_PROXY -ErrorAction SilentlyContinue }

# ---------- depot_tools ----------
if (-not (Test-Path (Join-Path $DepotTools 'gclient.bat'))) {
    Step "克隆 depot_tools 到 $DepotTools"
    Run git @('clone', 'https://chromium.googlesource.com/chromium/tools/depot_tools.git', $DepotTools)
}
else { Step "depot_tools 已存在：$DepotTools" }
$env:PATH = "$DepotTools;$env:PATH"
# 首次运行 gclient 会自举 depot_tools 自带的 python3/cipd（需要网络），之后走缓存。

# ---------- .gclient ----------
New-Item -ItemType Directory -Force $Root | Out-Null
$gclientFile = Join-Path $Root '.gclient'
# custom_deps 里置 None 的是 Windows 上无条件下载、但 enable_rust=false / use_siso=false 的 PDFium 构建用不到的包：
#   rust-toolchain 下载 427 MB、解压后 2.8 GB；siso 64 MB。去掉 rust-toolchain 后 gn gen 已验证可用（V01 报告第 4 节）。
$gclientWanted = @"
solutions = [
  { "name"        : 'pdfium',
    "url"         : '$Remote',
    "deps_file"   : 'DEPS',
    "managed"     : False,
    "custom_deps" : {
      'pdfium/third_party/rust-toolchain': None,
      'pdfium/third_party/siso/cipd': None,
    },
    "custom_vars": {'checkout_configuration': 'minimal', 'checkout_v8': False},
  },
]
"@
if (-not (Test-Path $gclientFile)) {
    Step "写入 $gclientFile（unmanaged、minimal、无 V8）"
    Set-Content -Path $gclientFile -Value $gclientWanted -Encoding ascii
}
elseif ((Get-Content $gclientFile -Raw).Trim() -ne $gclientWanted.Trim()) {
    Write-Warning "$gclientFile 与脚本模板不同，保留现有内容（如需重置请手工删除后重跑）"
}

# ---------- PDFium 本体 ----------
$src = Join-Path $Root 'pdfium'
if ($DepsOnly) {
    # CI 场景：<Root>\pdfium 已经是 magpdf-pdfium 的检出（origin 不是上游），不动分支，不 fetch
    if (-not (Test-Path (Join-Path $src '.git'))) { throw "-DepsOnly 要求 $src 已经检出" }
    Step "-DepsOnly：使用现有检出 $src（$(& git -C $src rev-parse --short HEAD)），跳过分支整理"
}
elseif (-not (Test-Path (Join-Path $src '.git'))) {
    Step "克隆 PDFium（$UpstreamBranch$(if ($NoHistory) { '，浅克隆' })）"
    $cloneArgs = @('clone')
    if ($NoHistory) { $cloneArgs += @('--depth', '1', '--branch', $UpstreamBranch) }
    $cloneArgs += @($Remote, $src)
    Run git $cloneArgs
}
else {
    Step "PDFium 已检出：$src，fetch origin"
    $fetchArgs = @('-C', $src, 'fetch', 'origin')
    if ($NoHistory) { $fetchArgs += @('--depth', '1', $UpstreamBranch) }
    Run git $fetchArgs
}

# 分支：upstream 严格等于 origin/<UpstreamBranch>（只读镜像，不在上面提交）
$upstreamRef = "origin/$UpstreamBranch"
if ($DepsOnly) { Step '-DepsOnly：跳过 upstream / mag/main 分支整理' }
else {
    if ($NoHistory) {
        # --depth 1 --branch 只会留下 FETCH_HEAD/那一个分支，补一个远程跟踪引用方便后面统一处理
        & git -C $src rev-parse --verify --quiet "refs/remotes/$upstreamRef" *> $null
        if ($LASTEXITCODE -ne 0) { Run git @('-C', $src, 'update-ref', "refs/remotes/$upstreamRef", 'FETCH_HEAD') }
    }
    if ((& git -C $src branch --show-current) -eq 'upstream') {
        # 正停在 upstream 上时 branch --force 会拒绝，改用 checkout -B 把它重置到上游（upstream 本来就不该有本地提交）
        Run git @('-C', $src, 'checkout', '-q', '-B', 'upstream', $upstreamRef)
    }
    else { Run git @('-C', $src, 'branch', '--force', '--no-track', 'upstream', $upstreamRef) }
    Run git @('-C', $src, 'branch', '--set-upstream-to', $upstreamRef, 'upstream')

    # 分支：mag/main
    & git -C $src rev-parse --verify --quiet 'refs/heads/mag/main' *> $null
    $hasMagMain = ($LASTEXITCODE -eq 0)
    if ($MagRemote) {
        & git -C $src remote get-url magpdf *> $null
        if ($LASTEXITCODE -ne 0) { Run git @('-C', $src, 'remote', 'add', 'magpdf', $MagRemote) }
        else { Run git @('-C', $src, 'remote', 'set-url', 'magpdf', $MagRemote) }
        Step '从 magpdf 远程拉取 mag/main'
        Run git @('-C', $src, 'fetch', 'magpdf', 'mag/main')
        if ($hasMagMain) { Run git @('-C', $src, 'checkout', 'mag/main'); Run git @('-C', $src, 'merge', '--ff-only', 'magpdf/mag/main') }
        else { Run git @('-C', $src, 'checkout', '-b', 'mag/main', '--track', 'magpdf/mag/main') }
    }
    elseif ($hasMagMain) {
        Step 'mag/main 已存在，直接切换'
        Run git @('-C', $src, 'checkout', 'mag/main')
    }
    else {
        $patches = Get-ChildItem (Join-Path $here 'patches\pdfium') -Filter '*.patch' -File -ErrorAction SilentlyContinue | Sort-Object Name
        Step "在 upstream 上创建 mag/main 并重放 $($patches.Count) 个补丁（patches/pdfium/）"
        Run git @('-C', $src, 'checkout', '-b', 'mag/main', 'upstream')
        if ($patches) {
            # git am 需要提交者身份；仓库里没配就用一个占位身份，避免在无全局配置的 CI 上失败
            & git -C $src config user.email *> $null
            $amEnv = @()
            if ($LASTEXITCODE -ne 0) { $amEnv = @('-c', 'user.name=magpdf-sync', '-c', 'user.email=magpdf-sync@localhost') }
            # --keep-non-patch：去掉 [PATCH] 但保留 [mag-local]/[mag-ext] 这类前缀；
            # 主仓库检出可能把 .patch 变成 CRLF，git am 默认会去掉行尾 \r，直接喂原文件即可
            Run git (@('-C', $src) + $amEnv + @('am', '--3way', '--keep-non-patch') + ($patches | ForEach-Object { $_.FullName }))
        }
    }

    # 浅克隆 --branch 会顺手建一个同名本地分支 chromium/NNNN，与 upstream 重复；现在已经切到 mag/main，删掉保持 §6 的布局
    & git -C $src rev-parse --verify --quiet "refs/heads/$UpstreamBranch" *> $null
    if ($LASTEXITCODE -eq 0) { Run git @('-C', $src, 'branch', '-D', $UpstreamBranch) }
}  # -not $DepsOnly

# ---------- gclient sync（DEPS 子仓库 + CIPD 包 + hooks） ----------
if ($NoSync) { Step '-NoSync：跳过 gclient sync 与 DEPS 子仓库补丁' }
else {
    Step "gclient sync -j$Jobs$(if ($NoHistory) { ' --no-history' })$(if ($NoHooks) { ' --nohooks' })"
    # 不加 --reset / -D：DEPS 子仓库（如 build/）里可能有正在调试的本地改动，由 apply-patches.ps1 的幂等检查兜底
    $syncArgs = @('sync', "-j$Jobs")
    if ($NoHistory) { $syncArgs += '--no-history' }
    if ($NoHooks) { $syncArgs += '--nohooks' }
    Push-Location $Root
    try { Run (Join-Path $DepotTools 'gclient.bat') $syncArgs }
    finally { Pop-Location }

    # ---------- 本地补丁（build/ 等 DEPS 仓库） ----------
    & (Join-Path $here 'apply-patches.ps1') -Source $src
}

# ---------- 汇总 ----------
Step '完成'
Write-Host "  源码:      $src"
if ($DepsOnly) { Write-Host "  HEAD:      $(& git -C $src rev-parse --short HEAD)" }
else {
    Write-Host "  upstream:  $(& git -C $src rev-parse --short upstream)  ($upstreamRef)"
    Write-Host "  mag/main:  $(& git -C $src rev-parse --short mag/main)"
    Write-Host '  upstream..mag/main:'
    & git -C $src log --oneline upstream..mag/main | ForEach-Object { Write-Host "    $_" }
}
Write-Host "  下一步: pwsh $(Join-Path $here 'build.ps1') -Source $src"
