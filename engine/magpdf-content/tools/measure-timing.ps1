<#
.SYNOPSIS
    V17 计时：自研读写层读入、对应、保存各花多久。

.DESCRIPTION
    对每批样本、每种做法把整批样本跑 Runs 遍（至少 5 遍），每遍取 magpdf-cli 自己记下的三段耗时
    （整批样本各文件之和）：

      读入  读文件，自研对象层打开文档（交叉引用表、对象）
      对应  找页、解内容流、照 PDFium 的规则解释，再和 PDFium 的对象逐个核对
            （含一次“不改动时改写层的输出就是原文”的核对）
      保存  改写内容流，写增量更新（只算自研的部分，不含回退给 PDFium 生成器的页）

    两种做法：

      modify       随机修改 5% 的对象；只有改了的页要改写、保存
      rewrite-all  不改动，但每一页都经改写层写回文件

    每个文件在单独的子进程里处理（--isolate --jobs 1）。PDFium 那一侧的加载、渲染和像素比对照常进行，
    不计入这三段。每遍前后各记一次整机 CPU 占用。

    结果写到 tests/out/v17/timing/timing.json（-Out 可改），并打印中位数和最小、最大值的表。

.EXAMPLE
    . D:\workspace\magpdf-wt\setup-dev-env.ps1
    pwsh -NoProfile -File engine/magpdf-content/tools/measure-timing.ps1
#>
[CmdletBinding()]
param(
    # 每种情形跑几遍（至少 5）
    [ValidateRange(5, 1000)]
    [int]$Runs = 5,
    # 用哪一份 PDFium（含 bin/pdfium.dll）。这三段是自研代码，换一份 PDFium 不影响。
    [string]$PdfiumDir = $(if ($env:PDFIUM_PREBUILT_DIR) { $env:PDFIUM_PREBUILT_DIR } else { 'third_party/pdfium-prebuilt' }),
    # PDFium 公开测试集
    [string]$Public = 'D:\tmp\v03-public\testing\resources',
    # 放“可执行文件 + DLL”和每一遍的 JSON 的目录
    [string]$WorkDir = $(Join-Path $(if ($env:TEMP) { $env:TEMP } else { '.' }) 'v17-timing'),
    # 结果文件；默认 tests/out/v17/timing/timing.json
    [string]$Out = '',
    [int]$Timeout = 120,
    # 不重新构建 magpdf-cli
    [switch]$SkipBuild,
    # 只量自制样本
    [switch]$NoPublic
)

$ErrorActionPreference = 'Stop'

# 整机 CPU 占用（%）。量不到时返回 $null（结果里写“没有量”）。
function Get-CpuLoad {
    try {
        $sample = Get-Counter '\Processor(_Total)\% Processor Time' -ErrorAction Stop
        return [math]::Round($sample.CounterSamples[0].CookedValue, 1)
    }
    catch {
        return $null
    }
}

# 中位数、最小值、最大值。
function Get-Stats([double[]]$Values) {
    $sorted = @($Values | Sort-Object)
    $n = $sorted.Count
    $median = if ($n % 2 -eq 1) { $sorted[($n - 1) / 2] } else { ($sorted[$n / 2 - 1] + $sorted[$n / 2]) / 2 }
    return [ordered]@{
        median = [math]::Round($median, 2)
        min    = [math]::Round($sorted[0], 2)
        max    = [math]::Round($sorted[$n - 1], 2)
    }
}

function Format-Stats($Stats) {
    return ('{0:N1}（{1:N1}–{2:N1}）' -f $Stats.median, $Stats.min, $Stats.max)
}

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
Push-Location $repo
try {
    if (-not $SkipBuild) {
        cargo build --locked --offline --release -p magpdf-cli
        if ($LASTEXITCODE -ne 0) { throw "构建 magpdf-cli 失败（退出码 $LASTEXITCODE）" }
    }
    $exe = Join-Path $repo 'target\release\magpdf-cli.exe'
    if (-not (Test-Path $exe)) { throw "找不到 $exe" }
    $dll = Join-Path $PdfiumDir 'bin\pdfium.dll'
    if (-not (Test-Path $dll)) { throw "找不到 $dll" }

    $binDir = Join-Path $WorkDir 'bin'
    New-Item -ItemType Directory -Force $binDir | Out-Null
    Copy-Item $exe (Join-Path $binDir 'magpdf-cli.exe') -Force
    Copy-Item $dll (Join-Path $binDir 'pdfium.dll') -Force
    $cli = Join-Path $binDir 'magpdf-cli.exe'

    if (-not $Out) { $Out = Join-Path $repo 'tests\out\v17\timing\timing.json' }
    New-Item -ItemType Directory -Force (Split-Path -Parent $Out) | Out-Null

    $corpora = [ordered]@{ synthetic = (Join-Path $repo 'tests\corpus\synthetic') }
    if (-not $NoPublic) { $corpora['public'] = $Public }
    $modes = [ordered]@{
        'modify'      = @('--mode', 'modify')
        'rewrite-all' = @('--mode', 'roundtrip', '--rewrite-all')
    }

    $processor = Get-CimInstance Win32_Processor | Select-Object -First 1
    $result = [ordered]@{
        tool          = 'engine/magpdf-content/tools/measure-timing.ps1'
        note          = '开发机实测，量的时候有别的任务在跑'
        started       = (Get-Date).ToString('o')
        commit        = (git rev-parse HEAD)
        dirty         = [bool](git status --porcelain)
        exe_sha256    = (Get-FileHash $exe -Algorithm SHA256).Hash
        pdfium        = [ordered]@{
            source = (Resolve-Path $dll).Path
            sha256 = (Get-FileHash $dll -Algorithm SHA256).Hash
        }
        machine       = [ordered]@{
            cpu                = $processor.Name.Trim()
            logical_processors = [Environment]::ProcessorCount
            os                 = [Environment]::OSVersion.VersionString
        }
        runs_per_case = $Runs
        cases         = @()
    }

    $rows = @()
    foreach ($corpus in $corpora.Keys) {
        foreach ($mode in $modes.Keys) {
            $samples = @()
            $last = $null
            for ($i = 1; $i -le $Runs; $i++) {
                $json = Join-Path $WorkDir "$corpus-$mode-$i.json"
                if (Test-Path $json) { Remove-Item $json -Force }
                $cpuBefore = Get-CpuLoad
                $watch = [System.Diagnostics.Stopwatch]::StartNew()
                $cliArgs = @('content-roundtrip', $corpora[$corpus], '--source', $corpus) + $modes[$mode] + @(
                    '--isolate', '--jobs', '1', '--timeout', $Timeout, '--quiet', '--json', $json)
                & $cli @cliArgs | Out-Null
                $code = $LASTEXITCODE
                $watch.Stop()
                $cpuAfter = Get-CpuLoad
                if (-not (Test-Path $json)) { throw "$corpus $mode 第 $i 遍没有写出结果（退出码 $code）" }
                $last = Get-Content $json -Raw -Encoding utf8 | ConvertFrom-Json
                $t = $last.total.timings
                $samples += [ordered]@{
                    run             = $i
                    exit_code       = $code
                    wall_s          = [math]::Round($watch.Elapsed.TotalSeconds, 1)
                    read_ms         = [math]::Round($t.read_us / 1000, 2)
                    map_ms          = [math]::Round($t.map_us / 1000, 2)
                    save_ms         = [math]::Round($t.save_us / 1000, 2)
                    cpu_load_before = $cpuBefore
                    cpu_load_after  = $cpuAfter
                }
                Write-Host ("{0,-10} {1,-12} 第 {2} 遍  读入 {3,8:N1} ms  对应 {4,8:N1} ms  保存 {5,8:N1} ms  CPU {6}% → {7}%" -f `
                        $corpus, $mode, $i, ($t.read_us / 1000), ($t.map_us / 1000), ($t.save_us / 1000), $cpuBefore, $cpuAfter)
            }

            $total = $last.total
            $pages = $total.pages - $total.pages_error
            $read = Get-Stats ([double[]]@($samples | ForEach-Object { $_.read_ms }))
            $map = Get-Stats ([double[]]@($samples | ForEach-Object { $_.map_ms }))
            $save = Get-Stats ([double[]]@($samples | ForEach-Object { $_.save_ms }))
            $loads = @($samples | ForEach-Object { $_.cpu_load_before; $_.cpu_load_after } | Where-Object { $null -ne $_ })
            $cpu = if ($loads.Count) {
                [ordered]@{
                    min = ($loads | Measure-Object -Minimum).Minimum
                    max = ($loads | Measure-Object -Maximum).Maximum
                }
            }
            else { $null }
            $result.cases += [ordered]@{
                corpus          = $corpus
                mode            = $mode
                files           = $total.files
                files_not_ok    = $total.load_failed + $total.crashed + $total.timed_out
                pages           = $pages
                pages_matched   = $total.pages_matched
                pages_edited    = $total.pages_edited
                bytes           = $total.bytes
                streams         = $total.streams
                operators       = $total.operators
                read_ms         = $read
                map_ms          = $map
                save_ms         = $save
                # 每页平均（微秒），按中位数算
                per_page_us     = [ordered]@{
                    read = if ($pages) { [math]::Round($read.median * 1000 / $pages, 1) } else { $null }
                    map  = if ($pages) { [math]::Round($map.median * 1000 / $pages, 1) } else { $null }
                    save = if ($pages) { [math]::Round($save.median * 1000 / $pages, 1) } else { $null }
                }
                cpu_load        = $cpu
                samples         = $samples
            }
            $cpuText = if ($cpu) { '{0:N0}%–{1:N0}%' -f $cpu.min, $cpu.max } else { '没有量' }
            $rows += ('| {0} | {1} | {2} | {3} | {4:N0} | {5} | {6} | {7} | {8} |' -f `
                    $corpus, $mode, $total.files, $pages, $total.bytes, (Format-Stats $read), (Format-Stats $map), (Format-Stats $save), $cpuText)
        }
    }

    $result['finished'] = (Get-Date).ToString('o')
    $result | ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8 $Out

    Write-Host ''
    Write-Host "开发机实测，量的时候有别的任务在跑。每种情形 $Runs 遍，整批样本各文件耗时之和，中位数（最小–最大），单位毫秒。"
    Write-Host ''
    Write-Host '| 样本 | 做法 | 文件 | 页 | 字节 | 读入 | 对应 | 保存 | 量的时候整机 CPU 占用 |'
    Write-Host '|---|---|---|---|---|---|---|---|---|'
    $rows | ForEach-Object { Write-Host $_ }
    Write-Host ''
    Write-Host "结果在 $Out"
}
finally {
    Pop-Location
}
