<#
.SYNOPSIS
  在合成样本上跑一遍 OCR 并评测（V09）。输出到 out/<Name>/：pred/、run.log、eval.txt、eval.json、details.json、peak-memory。

.EXAMPLE
  pwsh tests/corpus/synthetic-ocr/run_eval.ps1 -Name default
  pwsh tests/corpus/synthetic-ocr/run_eval.ps1 -Name en-rec -Filter 'en_*' -OcrArgs @('--lang','en')
  pwsh tests/corpus/synthetic-ocr/run_eval.ps1 -Name server -OcrArgs @('--server')
#>
param(
    [Parameter(Mandatory)] [string]$Name,
    [string]$Filter = '*',
    [string[]]$OcrArgs = @(),
    [string[]]$EvalArgs = @(),
    [string]$Cli = ''
)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..\..\..')
$out = Join-Path $PSScriptRoot 'out'
if (-not (Test-Path (Join-Path $out 'Label.txt'))) {
    throw "没有合成样本，先运行 python $PSScriptRoot\gen_synthetic.py"
}
if (-not $Cli) { $Cli = Join-Path $root 'target\release\magpdf-cli.exe' }
if (-not (Test-Path $Cli)) { throw "找不到 $Cli，先 cargo build -p magpdf-cli --release" }

$runDir = Join-Path $out $Name
$imgDir = Join-Path $runDir 'images'
$predDir = Join-Path $runDir 'pred'
Remove-Item -Recurse -Force $runDir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $imgDir, $predDir | Out-Null
# 按过滤条件挑图（硬链接，省空间）。
Get-ChildItem (Join-Path $out 'images') -Filter "$Filter.png" | ForEach-Object {
    New-Item -ItemType HardLink -Path (Join-Path $imgDir $_.Name) -Target $_.FullName | Out-Null
}
$count = (Get-ChildItem $imgDir).Count
if ($count -eq 0) { throw "过滤条件 $Filter 没有匹配到图片" }

# 运行 OCR，同时轮询峰值工作集。
$log = Join-Path $runDir 'run.log'
$argList = @('ocr', $imgDir, '--json', $predDir) + $OcrArgs
$sw = [Diagnostics.Stopwatch]::StartNew()
$proc = Start-Process -FilePath $Cli -ArgumentList $argList -NoNewWindow -PassThru `
    -RedirectStandardOutput $log -RedirectStandardError (Join-Path $runDir 'run.err')
$peak = 0
while (-not $proc.HasExited) {
    try { $proc.Refresh(); $peak = [Math]::Max($peak, $proc.PeakWorkingSet64) } catch {}
    Start-Sleep -Milliseconds 50
}
$sw.Stop()
try { $proc.Refresh(); $peak = [Math]::Max($peak, $proc.PeakWorkingSet64) } catch {}
if ($proc.ExitCode -ne 0) {
    Get-Content (Join-Path $runDir 'run.err') | Select-Object -Last 20
    throw "magpdf-cli ocr 退出码 $($proc.ExitCode)"
}
$peakMb = [Math]::Round($peak / 1MB, 1)
"peak_working_set_mb=$peakMb`nwall_seconds=$([Math]::Round($sw.Elapsed.TotalSeconds, 2))`nimages=$count`nargs=$($OcrArgs -join ' ')" |
    Set-Content (Join-Path $runDir 'peak-memory.txt')

# 评测。
$evalArgList = @('ocr-eval', $predDir, (Join-Path $out 'Label.txt'), '--json', (Join-Path $runDir 'eval.json'),
    '--details', (Join-Path $runDir 'details.json')) + $EvalArgs
& $Cli @evalArgList | Tee-Object -FilePath (Join-Path $runDir 'eval.txt')

# 汇总每页耗时（中位数 / 平均）。
$timings = Get-ChildItem $predDir -Filter '*.json' | ForEach-Object {
    $j = Get-Content $_.FullName -Raw -Encoding UTF8 | ConvertFrom-Json
    [pscustomobject]@{
        det   = $j.timings.det.resize + $j.timings.det.infer + $j.timings.det.postprocess
        cls   = $j.timings.cls
        rec   = $j.timings.rec
        total = $j.timings.total
        lines = $j.lines.Count
    }
}
function Median($v) { $s = $v | Sort-Object; $n = $s.Count; if ($n % 2) { $s[[int][Math]::Floor($n / 2)] } else { ($s[$n / 2 - 1] + $s[$n / 2]) / 2 } }
$summary = [ordered]@{
    name = $Name; images = $count; peak_working_set_mb = $peakMb
    wall_seconds = [Math]::Round($sw.Elapsed.TotalSeconds, 2)
    median_det_ms = [Math]::Round((Median $timings.det), 1)
    median_cls_ms = [Math]::Round((Median $timings.cls), 1)
    median_rec_ms = [Math]::Round((Median $timings.rec), 1)
    median_total_ms = [Math]::Round((Median $timings.total), 1)
    mean_total_ms = [Math]::Round(($timings.total | Measure-Object -Average).Average, 1)
    mean_lines = [Math]::Round(($timings.lines | Measure-Object -Average).Average, 1)
}
$summary | ConvertTo-Json | Set-Content (Join-Path $runDir 'summary.json')
Get-Content (Join-Path $runDir 'run.log') | Select-Object -First 3
"峰值工作集 $peakMb MB；每页中位耗时 det $($summary.median_det_ms) / cls $($summary.median_cls_ms) / rec $($summary.median_rec_ms) / 合计 $($summary.median_total_ms) ms（平均 $($summary.mean_total_ms) ms，平均 $($summary.mean_lines) 行）"
