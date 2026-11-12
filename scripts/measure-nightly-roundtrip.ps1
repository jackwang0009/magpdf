<#
.SYNOPSIS
  量每晚的往返测试跑一遍全量要多久：把 scripts/nightly-roundtrip.ps1 连跑几遍，各步耗时取中位数、最小、最大。

.DESCRIPTION
  每一遍都是完整的一遍：往返（隔离模式）、校准轮、操作符级比对、报告页面、和基线比较。不改基线；
  有退步也照常量（-FailOn never），只有脚本或工具出错才算这一遍失败。

  每一遍开始前记一次整机 CPU 占用：机器上同时有没有别的活，直接影响量出来的时间，要和结果写在一起。

  结果写成 JSON（默认 <OutDir>\timing.json），并打印一张表。每一遍的输出在 <OutDir>\run 里，后一遍盖掉前一遍；
  每一遍入口脚本打印的东西留在 <OutDir>\run-<序号>.txt。

.PARAMETER Runs
  跑几遍，默认 5（《P0 技术验证计划》第 2 节：每项测 5 次取中位数）。

.EXAMPLE
  pwsh scripts/measure-nightly-roundtrip.ps1 -Cli D:\workspace\magpdf-wt\bin\magpdf-cli.exe -PublicPdfium D:\tmp\v03-public\testing\resources
#>
param(
    [string]$Cli = '',
    [string]$PublicPdfium = '',
    [string]$Corpus = '',
    [int]$Runs = 5,
    [int]$Jobs = 0,
    [string]$OutDir = '',
    [string]$Json = ''
)
$ErrorActionPreference = 'Stop'

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$nightly = Join-Path $PSScriptRoot 'nightly-roundtrip.ps1'
if (-not $OutDir) { $OutDir = Join-Path $root 'tests\out\nightly-timing' }
$OutDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutDir)
if (-not $Json) { $Json = Join-Path $OutDir 'timing.json' }
$Json = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Json)
if ($Runs -lt 1) { throw '-Runs 至少是 1' }
New-Item -ItemType Directory -Force $OutDir | Out-Null
$runDir = Join-Path $OutDir 'run'

# 整机 CPU 占用（百分比）。性能计数器的名字跟着系统语言走，英文名取不到时改用 WMI 的 LoadPercentage。
function Get-CpuPercent {
    try {
        $samples = (Get-Counter '\Processor(_Total)\% Processor Time' -SampleInterval 1 -MaxSamples 3 -ErrorAction Stop).CounterSamples
        $value = ($samples | Measure-Object CookedValue -Average).Average
        return [ordered]@{ percent = [Math]::Round($value, 1); method = 'Get-Counter \Processor(_Total)\% Processor Time，3 秒平均' }
    } catch {
        $value = (Get-CimInstance Win32_Processor | Measure-Object LoadPercentage -Average).Average
        return [ordered]@{ percent = [Math]::Round([double]$value, 1); method = 'Win32_Processor.LoadPercentage' }
    }
}

function Get-Stats([double[]]$Values) {
    $sorted = @($Values | Sort-Object)
    $n = $sorted.Count
    $half = [int][Math]::Floor($n / 2)
    if ($n % 2 -eq 1) { $median = $sorted[$half] } else { $median = ($sorted[$half - 1] + $sorted[$half]) / 2 }
    [ordered]@{
        median = [Math]::Round($median, 2); min = [Math]::Round($sorted[0], 2); max = [Math]::Round($sorted[$n - 1], 2)
        values = @($Values | ForEach-Object { [Math]::Round($_, 2) })
    }
}

# 入口脚本的输出是 UTF-8；这里按控制台编码解码之后写进 run-<序号>.txt，中文系统默认是 GBK，先改过来，结束时恢复。
$oldConsoleEncoding = $null
try {
    $oldConsoleEncoding = [Console]::OutputEncoding
    [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
} catch {
    $oldConsoleEncoding = $null
}

try {
    $shell = (Get-Process -Id $PID).Path
    $nightlyArgs = @('-NoProfile', '-File', $nightly, '-OutDir', $runDir, '-FailOn', 'never')
    if ($Cli) { $nightlyArgs += @('-Cli', $Cli) }
    if ($PublicPdfium) { $nightlyArgs += @('-PublicPdfium', $PublicPdfium) }
    if ($Corpus) { $nightlyArgs += @('-Corpus', $Corpus) }
    if ($Jobs -gt 0) { $nightlyArgs += @('-Jobs', "$Jobs") }

    $totals = @()
    $cpuBefore = @()
    $cpuMethod = ''
    $stepValues = [ordered]@{}
    $runRecords = @()
    for ($i = 1; $i -le $Runs; $i++) {
        $cpu = Get-CpuPercent
        $cpuBefore += $cpu.percent
        $cpuMethod = $cpu.method
        $started = Get-Date
        $runLog = Join-Path $OutDir "run-$i.txt"
        # Windows PowerShell 5.1 里，子进程往标准错误写的每一行在 Stop 下都会被当成错误抛出来，这一句临时放开。
        $ErrorActionPreference = 'Continue'
        & $shell @nightlyArgs 2>&1 | ForEach-Object { "$_" } | Set-Content -Path $runLog -Encoding utf8
        $exit = $LASTEXITCODE
        $ErrorActionPreference = 'Stop'
        $timingFile = Join-Path $runDir 'timings.json'
        if ($exit -ne 0 -or -not (Test-Path $timingFile)) {
            throw "第 $i 遍没有跑完（退出码 $exit），看 $runLog"
        }
        $timing = Get-Content $timingFile -Raw -Encoding utf8 | ConvertFrom-Json
        $totals += [double]$timing.total_s
        foreach ($step in $timing.steps.PSObject.Properties) {
            if (-not $stepValues.Contains($step.Name)) { $stepValues[$step.Name] = @() }
            $stepValues[$step.Name] += [double]$step.Value
        }
        $runRecords += [ordered]@{
            started = $started.ToString('yyyy-MM-ddTHH:mm:sszzz'); cpu_before_percent = $cpu.percent; total_s = [double]$timing.total_s
        }
        Write-Host ("第 {0}/{1} 遍：{2:N1} 秒（开始前整机 CPU {3}%）" -f $i, $Runs, [double]$timing.total_s, $cpu.percent)
    }

    # 样本数、机器、工具从最后一遍的输出里取。
    $run = Get-Content (Join-Path $runDir 'run.json') -Raw -Encoding utf8 | ConvertFrom-Json
    $summary = Get-Content (Join-Path $runDir 'summary.json') -Raw -Encoding utf8 | ConvertFrom-Json
    $sets = @($summary.sets | ForEach-Object { [ordered]@{ name = $_.name; files = $_.files; pages = $_.pages } })
    $steps = [ordered]@{}
    foreach ($name in @($stepValues.Keys)) { $steps[$name] = Get-Stats $stepValues[$name] }

    $result = [ordered]@{
        kind = 'magpdf-roundtrip-nightly-timing-summary'; format = 1
        generated = (Get-Date).ToString('yyyy-MM-ddTHH:mm:sszzz')
        machine = $run.machine; os = $run.os; cpu = $run.cpu; logical_processors = [Environment]::ProcessorCount
        cli = $run.cli; pdfium_dll_sha256 = $run.pdfium_dll_sha256; commit = $run.git.commit
        jobs = $run.options.jobs; timeout_s = $run.options.timeout_s
        sets = $sets
        runs = $Runs
        cpu_before_percent = $cpuBefore
        cpu_method = $cpuMethod
        total_s = Get-Stats $totals
        steps = $steps
        per_run = $runRecords
    }
    $result | ConvertTo-Json -Depth 6 | Set-Content -Path $Json -Encoding utf8

    Write-Host ''
    Write-Host ("{0,-30} {1,9} {2,9} {3,9}   （秒，{4} 遍）" -f '步骤', '中位数', '最小', '最大', $Runs)
    foreach ($name in @($steps.Keys)) {
        Write-Host ("{0,-32} {1,10:N2} {2,10:N2} {3,10:N2}" -f $name, $steps[$name].median, $steps[$name].min, $steps[$name].max)
    }
    Write-Host ("{0,-28} {1,10:N2} {2,10:N2} {3,10:N2}" -f '一遍合计', $result.total_s.median, $result.total_s.min, $result.total_s.max)
    Write-Host ("每遍开始前整机 CPU 占用（%）：{0}" -f ($cpuBefore -join '、'))
    Write-Host "结果：$Json"
} finally {
    if ($null -ne $oldConsoleEncoding) {
        try { [Console]::OutputEncoding = $oldConsoleEncoding } catch { }
    }
}
