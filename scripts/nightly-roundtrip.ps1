<#
.SYNOPSIS
  每晚的往返测试（V14）：全量往返（隔离模式）、校准轮、操作符级比对、报告页面、和基线比较。本机和流水线用同一个脚本。

.DESCRIPTION
  对每个语料集依次做：
    1. magpdf-cli roundtrip --isolate：写 JSON、没通过的页的三张图、保存结果
    2. magpdf-cli roundtrip --calibrate：校准轮，不重写内容流，应当全部无差异（-SkipCalibrate 跳过）
    3. tests/corpus/tools/ops_diff.py：操作符级比对（-SkipOpsDiff 跳过）
  然后用 tests/corpus/tools/roundtrip_report.py 生成报告页面，并和 tests/baselines/roundtrip/<语料集>.json 比较。

  语料集：
    synthetic       tests/corpus/synthetic，来源 synthetic
    public-pdfium   PDFium 的 testing/resources（chromium/8138），来源 public/pdfium/<子目录>。
                    位置用 -PublicPdfium 或环境变量 MAGPDF_PUBLIC_PDFIUM 给；没给就跳过。
                    流水线上用 scripts/fetch-public-corpus.ps1 取。
    corpus          真实样本的测试文件库，来源是第一级目录名（office-word、design……）。
                    位置用 -Corpus 或环境变量 MAGPDF_CORPUS 给；没给就跳过。

  输出（-OutDir，默认 tests/out/nightly，每次运行先清掉里面上一次的结果）：
    report.html                         报告页面（要连同各语料集的 diff 目录一起拿走，原图才点得开）
    summary.json、summary.md            机器可读的摘要、Markdown 摘要
    run.json、timings.json              这一次的输入和环境、各步耗时
    baseline-candidate/<语料集>.json    这一次的结果，按基线的格式写出
    <语料集>/roundtrip.json、calibrate.json、ops.json、diff/、saved/

  退出码：0 没有退步（或者没有基线）；2 到了报警线；3 有退步但没到报警线；1 脚本或工具出错。
  规则见 tests/baselines/README.md。

.PARAMETER Cli
  magpdf-cli.exe 的位置。默认依次找环境变量 MAGPDF_CLI、target\release\magpdf-cli.exe。pdfium.dll 用它旁边的那一份。

.PARAMETER FailOn
  any（默认）：有退步退出码就非 0；alarm：只有到报警线才非 0；never：只出报告。

.PARAMETER UpdateBaseline
  跑完之后把这一次的结果写成新的基线（tests/baselines/roundtrip/）。报告页面照常是和旧基线比的，退出码 0。

.PARAMETER RequireBaseline
  有语料集没有基线时退出码 1（流水线上用，免得基线被删了还一路绿）。

.EXAMPLE
  pwsh scripts/nightly-roundtrip.ps1 -Cli D:\workspace\magpdf-wt\bin\magpdf-cli.exe -PublicPdfium D:\tmp\v03-public\testing\resources

.EXAMPLE
  pwsh scripts/nightly-roundtrip.ps1 -PublicPdfium <目录> -UpdateBaseline
#>
param(
    [string]$Cli = '',
    [string]$PublicPdfium = '',
    [string]$Corpus = '',
    [string]$OutDir = '',
    [string]$BaselineDir = '',
    [int]$Jobs = 0,
    [int]$Timeout = 60,
    [ValidateSet('any', 'alarm', 'never')] [string]$FailOn = 'any',
    [switch]$UpdateBaseline,
    [switch]$RequireBaseline,
    [switch]$SkipCalibrate,
    [switch]$SkipOpsDiff,
    [switch]$SkipSynthetic,
    [string]$Python = 'python'
)
$ErrorActionPreference = 'Stop'

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$tools = Join-Path $root 'tests\corpus\tools'

function Get-FullPath([string]$Path) {
    $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path)
}

function Get-Seconds([Diagnostics.Stopwatch]$Watch) {
    [Math]::Round($Watch.Elapsed.TotalSeconds, 2)
}

# 命令行工具和 Python 的输出都是 UTF-8；管道里按控制台编码解码，中文系统默认是 GBK，先改过来，结束时恢复。
$oldConsoleEncoding = $null
try {
    $oldConsoleEncoding = [Console]::OutputEncoding
    [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
} catch {
    $oldConsoleEncoding = $null
}
$oldPythonIo = $env:PYTHONIOENCODING
$env:PYTHONIOENCODING = 'utf-8'

$code = 1
try {
    $totalWatch = [Diagnostics.Stopwatch]::StartNew()
    $started = Get-Date

    # ---- 工具 ----
    if (-not $Cli) { $Cli = $env:MAGPDF_CLI }
    if (-not $Cli) { $Cli = Join-Path $root 'target\release\magpdf-cli.exe' }
    if (-not (Test-Path $Cli -PathType Leaf)) {
        throw "找不到 magpdf-cli：$Cli。先 cargo build --release --locked -p magpdf-cli，或者用 -Cli 指定。"
    }
    $Cli = (Resolve-Path $Cli).Path
    $cliVersion = @(& $Cli --version)[0]
    if ($LASTEXITCODE -ne 0) { throw "magpdf-cli --version 退出码 $LASTEXITCODE" }
    $cliHash = (Get-FileHash $Cli -Algorithm SHA256).Hash.ToLower()
    $dll = Join-Path (Split-Path $Cli -Parent) 'pdfium.dll'
    $dllHash = ''
    if (Test-Path $dll -PathType Leaf) { $dllHash = (Get-FileHash $dll -Algorithm SHA256).Hash.ToLower() }
    $pythonVersion = @(& $Python --version)[0]
    if ($LASTEXITCODE -ne 0) { throw "找不到 Python（$Python）" }

    if (-not $OutDir) { $OutDir = Join-Path $root 'tests\out\nightly' }
    $OutDir = Get-FullPath $OutDir
    if (-not $BaselineDir) { $BaselineDir = Join-Path $root 'tests\baselines\roundtrip' }
    $BaselineDir = Get-FullPath $BaselineDir
    if ($Jobs -le 0) { $Jobs = [Math]::Max(1, [Math]::Min(8, [int][Math]::Floor([Environment]::ProcessorCount / 2))) }

    # ---- 语料集 ----
    $sets = @()
    if (-not $SkipSynthetic) {
        $sets += [ordered]@{
            name = 'synthetic'; title = '自制样本'; dir = (Join-Path $root 'tests\corpus\synthetic')
            prefix = ''; rootSource = 'synthetic'; source = 'synthetic'
        }
    }
    $publicDir = if ($PublicPdfium) { $PublicPdfium } else { $env:MAGPDF_PUBLIC_PDFIUM }
    if ($publicDir) {
        $sets += [ordered]@{
            name = 'public-pdfium'; title = 'PDFium 公开测试集'; dir = $publicDir
            prefix = 'public/pdfium/'; rootSource = 'resources'; source = ''
        }
    } else {
        Write-Host '没有给 PDFium 公开测试集的位置（-PublicPdfium 或环境变量 MAGPDF_PUBLIC_PDFIUM），这一批跳过。'
    }
    $corpusDir = if ($Corpus) { $Corpus } else { $env:MAGPDF_CORPUS }
    if ($corpusDir) {
        $sets += [ordered]@{
            name = 'corpus'; title = '测试文件库'; dir = $corpusDir
            prefix = ''; rootSource = ''; source = ''
        }
    }
    if ($sets.Count -eq 0) { throw '没有可跑的语料集。' }
    foreach ($set in $sets) {
        if (-not (Test-Path $set.dir -PathType Container)) { throw "找不到语料目录：$($set.dir)（$($set.name)）" }
        $set.dir = (Resolve-Path $set.dir).Path
    }

    # ---- 环境 ----
    $commit = ''; $branch = ''; $dirty = $false
    try {
        $commit = (& git -C $root rev-parse HEAD 2>$null)
        $branch = (& git -C $root rev-parse --abbrev-ref HEAD 2>$null)
        $dirty = [bool](& git -C $root status --porcelain --untracked-files=no 2>$null)
    } catch { }
    $os = [Environment]::OSVersion.VersionString; $cpu = ''
    try {
        $osInfo = Get-CimInstance Win32_OperatingSystem
        $os = "$($osInfo.Caption) $($osInfo.Version)"
        $cpu = ((Get-CimInstance Win32_Processor | Select-Object -First 1).Name).Trim()
    } catch { }
    # GitHub 托管的运行器每次换一台机器、名字都不一样。“和基线是不是同一种环境”要看的是镜像，所以流水线上记镜像名。
    $machine = "$env:COMPUTERNAME"
    if ($env:GITHUB_ACTIONS) {
        $image = if ($env:ImageOS) { $env:ImageOS } else { "$env:RUNNER_OS" }
        $machine = "github-actions/$image"
    }

    New-Item -ItemType Directory -Force $OutDir | Out-Null
    foreach ($name in @('report.html', 'summary.json', 'summary.md', 'run.json', 'timings.json')) {
        $stale = Join-Path $OutDir $name
        if (Test-Path $stale) { Remove-Item $stale -Force }
    }
    $candidateDir = Join-Path $OutDir 'baseline-candidate'
    if (Test-Path $candidateDir) { Remove-Item $candidateDir -Recurse -Force }

    Write-Host "每晚往返测试：$cliVersion，$pythonVersion，同时 $Jobs 个子进程，单个文件超时 $Timeout 秒"
    Write-Host "输出目录：$OutDir"

    $steps = [ordered]@{}
    $setRecords = @()
    foreach ($set in $sets) {
        $name = $set.name
        $setDir = Join-Path $OutDir $name
        if (Test-Path $setDir) { Remove-Item $setDir -Recurse -Force }
        New-Item -ItemType Directory -Force $setDir | Out-Null
        $seconds = [ordered]@{}
        Write-Host ''
        Write-Host "== $($set.title)（$name）：$($set.dir)"

        # 1. 往返
        $roundtripJson = Join-Path $setDir 'roundtrip.json'
        $cliArgs = @('roundtrip', $set.dir, '--isolate', '--jobs', "$Jobs", '--timeout', "$Timeout", '--quiet',
            '--json', $roundtripJson, '--diff-dir', (Join-Path $setDir 'diff'), '--save-dir', (Join-Path $setDir 'saved'))
        if ($set.source) { $cliArgs += @('--source', $set.source) }
        $watch = [Diagnostics.Stopwatch]::StartNew()
        & $Cli @cliArgs | Tee-Object -FilePath (Join-Path $setDir 'roundtrip.txt') | ForEach-Object { Write-Host "  $_" }
        if ($LASTEXITCODE -ne 0) { throw "magpdf-cli roundtrip 退出码 $LASTEXITCODE（$name）" }
        $seconds.roundtrip = Get-Seconds $watch
        $steps["$name.roundtrip"] = $seconds.roundtrip

        # 2. 校准轮：不重写内容流，只保存再重开。这一轮有差异，说明渲染或比对本身有噪声。
        $calibrateJson = $null
        if (-not $SkipCalibrate) {
            $calibrateJson = Join-Path $setDir 'calibrate.json'
            $calArgs = @('roundtrip', $set.dir, '--calibrate', '--isolate', '--jobs', "$Jobs", '--timeout', "$Timeout", '--quiet',
                '--json', $calibrateJson)
            if ($set.source) { $calArgs += @('--source', $set.source) }
            $watch = [Diagnostics.Stopwatch]::StartNew()
            & $Cli @calArgs | Set-Content -Path (Join-Path $setDir 'calibrate.txt') -Encoding utf8
            if ($LASTEXITCODE -ne 0) { throw "magpdf-cli roundtrip --calibrate 退出码 $LASTEXITCODE（$name）" }
            $seconds.calibrate = Get-Seconds $watch
            $steps["$name.calibrate"] = $seconds.calibrate
        }

        # 3. 操作符级比对。做不成不让整轮失败，报告页面上会写明；基线里有操作符级结果而这次没有，算退步。
        $opsJson = $null; $opsError = ''
        if (-not $SkipOpsDiff) {
            $opsJson = Join-Path $setDir 'ops.json'
            $watch = [Diagnostics.Stopwatch]::StartNew()
            & $Python -X dev (Join-Path $tools 'ops_diff.py') --roundtrip $roundtripJson --save-dir (Join-Path $setDir 'saved') `
                --json $opsJson --max-list 0 | Set-Content -Path (Join-Path $setDir 'ops.txt') -Encoding utf8
            if ($LASTEXITCODE -ne 0 -or -not (Test-Path $opsJson)) {
                $opsError = "ops_diff.py 退出码 $LASTEXITCODE"
                Write-Warning "操作符级比对没做成（$name）：$opsError"
                $opsJson = $null
            }
            $seconds.ops_diff = Get-Seconds $watch
            $steps["$name.ops_diff"] = $seconds.ops_diff
        }

        $record = [ordered]@{
            name = $name; title = $set.title; source_prefix = $set.prefix; root_source = $set.rootSource
            corpus_dir = $set.dir
            roundtrip = "$name/roundtrip.json"
            diff_dir = "$name/diff"
            baseline = (Join-Path $BaselineDir "$name.json")
            seconds = $seconds
        }
        if ($calibrateJson) { $record.calibrate = "$name/calibrate.json" }
        if ($opsJson) { $record.ops = "$name/ops.json" }
        if ($opsError) { $record.ops_error = $opsError }
        $setRecords += $record
    }

    # ---- 报告页面、和基线比较 ----
    $runJson = Join-Path $OutDir 'run.json'
    $run = [ordered]@{
        kind = 'magpdf-roundtrip-nightly-run'; format = 1
        started = $started.ToString('yyyy-MM-ddTHH:mm:sszzz')
        git = [ordered]@{ commit = "$commit"; branch = "$branch"; dirty = $dirty }
        machine = $machine; os = $os; cpu = $cpu; ci = [bool]$env:GITHUB_ACTIONS
        cli = [ordered]@{ path = $Cli; version = "$cliVersion"; sha256 = $cliHash }
        pdfium_dll_sha256 = $dllHash
        options = [ordered]@{ jobs = $Jobs; timeout_s = $Timeout; fail_on = $FailOn }
        sets = @($setRecords)
    }
    $run | ConvertTo-Json -Depth 8 | Set-Content -Path $runJson -Encoding utf8

    $reportHtml = Join-Path $OutDir 'report.html'
    $summaryMd = Join-Path $OutDir 'summary.md'
    $reportArgs = @('-X', 'dev', (Join-Path $tools 'roundtrip_report.py'), '--run', $runJson, '--out', $reportHtml,
        '--summary', (Join-Path $OutDir 'summary.json'), '--markdown', $summaryMd, '--baseline-out', $candidateDir, '--fail-on', $FailOn)
    if ($RequireBaseline -and -not $UpdateBaseline) { $reportArgs += '--require-baseline' }
    Write-Host ''
    $watch = [Diagnostics.Stopwatch]::StartNew()
    & $Python @reportArgs
    $code = $LASTEXITCODE
    $steps['report'] = Get-Seconds $watch
    if (-not (Test-Path $reportHtml)) { throw "报告页面没有生成（roundtrip_report.py 退出码 $code）" }

    $timings = [ordered]@{
        kind = 'magpdf-roundtrip-nightly-timings'; format = 1
        started = $started.ToString('yyyy-MM-ddTHH:mm:sszzz')
        machine = $machine; jobs = $Jobs
        total_s = Get-Seconds $totalWatch
        steps = $steps
    }
    $timings | ConvertTo-Json -Depth 4 | Set-Content -Path (Join-Path $OutDir 'timings.json') -Encoding utf8

    if (Test-Path $summaryMd) {
        Write-Host ''
        Get-Content $summaryMd -Encoding utf8 | ForEach-Object { Write-Host $_ }
        # GitHub Actions：把摘要贴到这一次运行的页面上，不用下载产物也能看到结论。
        if ($env:GITHUB_STEP_SUMMARY) {
            Get-Content $summaryMd -Raw -Encoding utf8 | Add-Content -Path $env:GITHUB_STEP_SUMMARY -Encoding utf8
        }
    }

    if ($UpdateBaseline) {
        if ($code -eq 1) { throw '报告生成出错，基线没有更新。' }
        New-Item -ItemType Directory -Force $BaselineDir | Out-Null
        foreach ($set in $sets) {
            $target = Join-Path $BaselineDir "$($set.name).json"
            Copy-Item (Join-Path $candidateDir "$($set.name).json") $target -Force
            Write-Host "基线已更新：$target"
        }
        Write-Host '提交之前用 git diff 看一遍哪些页的判定变了；报告页面第 2 节是和旧基线比的结果。'
        $code = 0
    }

    Write-Host ''
    Write-Host "总耗时 $($timings.total_s) 秒。报告页面：$reportHtml"
    switch ($code) {
        0 { Write-Host '退出码 0：没有退步（或者没有基线、只出报告）。' }
        2 { Write-Host '退出码 2：有退步，到了报警线。' }
        3 { Write-Host '退出码 3：有退步，没到报警线。' }
        default { Write-Host "退出码 ${code}：报告工具出错。" }
    }
} catch {
    Write-Host "每晚往返测试没有跑完：$($_.Exception.Message)" -ForegroundColor Red
    $code = 1
} finally {
    $env:PYTHONIOENCODING = $oldPythonIo
    if ($null -ne $oldConsoleEncoding) {
        try { [Console]::OutputEncoding = $oldConsoleEncoding } catch { }
    }
}
exit $code
