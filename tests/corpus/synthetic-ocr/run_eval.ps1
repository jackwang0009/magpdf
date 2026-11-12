<#
.SYNOPSIS
  在合成样本上跑一遍 OCR 并评测（V09）。输出到 out/<Name>/：pred/、Label.txt（只含选中的图）、
  run.<k>.log、eval.txt、eval.json、details.json、summary.json。

.DESCRIPTION
  -Filter 按图像主干名匹配（通配符，可以给多个），主干名的样子是 <语言>_<序号>_<变体>_<分辨率>，
  例如 en_007_noise_300、zh_012_photo_12mp。按变体、分辨率分开的数字由 analyze.py 从 details.json 里算。

  -Shards 把选中的图分给几个进程同时跑，只为省时间：每个进程各自加载一份模型，识别结果和一个进程跑
  的一样。这样跑出来的耗时互相挤占，不能当速度数据；量速度用 measure_speed.ps1。

.EXAMPLE
  在 PowerShell 7 里直接调用。-Filter、-OcrArgs 是数组，写成 `pwsh run_eval.ps1 -OcrArgs ...` 另起一个
  进程时传不进去（多个值会报错，或者被当成一个值）。
  ./tests/corpus/synthetic-ocr/run_eval.ps1 -Name default
  ./tests/corpus/synthetic-ocr/run_eval.ps1 -Name en-rec -Filter 'en_*' -OcrArgs '--lang','en'
  ./tests/corpus/synthetic-ocr/run_eval.ps1 -Name server -Filter '*_300' -OcrArgs '--server' -Shards 3
#>
param(
    [Parameter(Mandatory)] [string]$Name,
    [string[]]$Filter = @('*'),
    [string[]]$OcrArgs = @(),
    [string[]]$EvalArgs = @(),
    [string]$Cli = '',
    # 样本子目录：images（评测用的全套）、speed（量速度用的三页）、images-red（make_red_channel.py 出的红色通道图）、
    # images-bin（make_binary.py 出的二值图）
    [string]$Set = 'images',
    [ValidateRange(1, 16)] [int]$Shards = 1
)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..\..\..')
$out = Join-Path $PSScriptRoot 'out'
$labelFile = if ($Set -eq 'images') { Join-Path $out 'Label.txt' } else { Join-Path $out "$Set\Label.txt" }
if (-not (Test-Path $labelFile)) {
    throw "没有合成样本，先运行 python $PSScriptRoot\gen_synthetic.py"
}
if (-not $Cli) { $Cli = Join-Path $root 'target\release\magpdf-cli.exe' }
if (-not (Test-Path $Cli)) { throw "找不到 $Cli，先 cargo build -p magpdf-cli --release" }

$runDir = Join-Path $out $Name
if ($Name -eq $Set) { throw "输出目录不能和样本目录同名（$Name）：开头要清空输出目录，会把样本删掉" }
$predDir = Join-Path $runDir 'pred'
Remove-Item -Recurse -Force $runDir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $predDir | Out-Null
# 按过滤条件挑图（硬链接，省空间），轮流分给各个进程。
$picked = @(Get-ChildItem (Join-Path $out $Set) -File | Where-Object {
    $item = $_
    $item.Extension -in '.png', '.jpg', '.jpeg' -and ($Filter | Where-Object { $item.BaseName -like $_ })
} | Sort-Object Name)
$count = $picked.Count
if ($count -eq 0) { throw "过滤条件 $($Filter -join ', ') 没有匹配到图片" }
$Shards = [Math]::Min($Shards, $count)
$imgDirs = 0..($Shards - 1) | ForEach-Object { Join-Path $runDir "images\$_" }
New-Item -ItemType Directory -Force $imgDirs | Out-Null
for ($i = 0; $i -lt $count; $i++) {
    $f = $picked[$i]
    New-Item -ItemType HardLink -Path (Join-Path $imgDirs[$i % $Shards] $f.Name) -Target $f.FullName | Out-Null
}
# 标注只留选中的图，评测时不会把没跑的图报成“缺少预测”。
$names = [Collections.Generic.HashSet[string]]::new([string[]]@($picked | ForEach-Object { $_.Name }))
$runLabels = Join-Path $runDir 'Label.txt'
$utf8 = [Text.UTF8Encoding]::new($false)
$lines = [IO.File]::ReadAllLines($labelFile, $utf8) | Where-Object {
    $rel = $_.Split("`t")[0]
    $names.Contains($rel.Substring($rel.LastIndexOf('/') + 1))
}
[IO.File]::WriteAllLines($runLabels, [string[]]@($lines), $utf8)

# 运行 OCR，同时轮询各进程的峰值工作集。
$sw = [Diagnostics.Stopwatch]::StartNew()
$procs = 0..($Shards - 1) | ForEach-Object {
    $argList = @('ocr', $imgDirs[$_], '--json', $predDir) + $OcrArgs
    Start-Process -FilePath $Cli -ArgumentList $argList -NoNewWindow -PassThru `
        -RedirectStandardOutput (Join-Path $runDir "run.$_.log") -RedirectStandardError (Join-Path $runDir "run.$_.err")
}
$peak = 0
do {
    Start-Sleep -Milliseconds 100
    foreach ($p in $procs) {
        try { $p.Refresh(); $peak = [Math]::Max($peak, $p.PeakWorkingSet64) } catch {}
    }
} while ($procs | Where-Object { -not $_.HasExited })
$sw.Stop()
foreach ($k in 0..($Shards - 1)) {
    $procs[$k].WaitForExit()
    if ($procs[$k].ExitCode -ne 0) {
        Get-Content (Join-Path $runDir "run.$k.err") | Select-Object -Last 20
        throw "magpdf-cli ocr 退出码 $($procs[$k].ExitCode)（第 $k 个进程）"
    }
}
$peakMb = [Math]::Round($peak / 1MB, 1)

# 评测。
$evalArgList = @('ocr-eval', $predDir, $runLabels, '--json', (Join-Path $runDir 'eval.json'),
    '--details', (Join-Path $runDir 'details.json')) + $EvalArgs
& $Cli @evalArgList | Tee-Object -FilePath (Join-Path $runDir 'eval.txt')
if ($LASTEXITCODE -ne 0) { throw "magpdf-cli ocr-eval 退出码 $LASTEXITCODE" }

# 汇总每页耗时（中位数 / 平均）。这里的图一张接一张在同一个进程里跑，每张只跑一次；
# 正式的速度测量用 measure_speed.ps1。
$timings = Get-ChildItem $predDir -Filter '*.json' | ForEach-Object {
    $j = Get-Content $_.FullName -Raw -Encoding UTF8 | ConvertFrom-Json
    [pscustomobject]@{
        det     = $j.timings.det.resize + $j.timings.det.infer + $j.timings.det.postprocess
        cls     = $j.timings.cls
        rec     = $j.timings.rec
        total   = $j.timings.total
        lines   = $j.lines.Count
        checked = $j.orientation_checked
        changed = $j.orientation_overruled
    }
}
function Median($v) { $s = @($v | Sort-Object); $n = $s.Count; if ($n % 2) { $s[[int][Math]::Floor($n / 2)] } else { ($s[$n / 2 - 1] + $s[$n / 2]) / 2 } }
$summary = [ordered]@{
    name = $Name; images = $count; args = ($OcrArgs -join ' '); cli = "$Cli"; shards = $Shards
    models = (Get-Content (Join-Path $runDir 'run.0.log') -Encoding UTF8 | Select-Object -First 1)
    peak_working_set_mb = $peakMb
    wall_seconds = [Math]::Round($sw.Elapsed.TotalSeconds, 2)
    median_det_ms = [Math]::Round((Median $timings.det), 1)
    median_cls_ms = [Math]::Round((Median $timings.cls), 1)
    median_rec_ms = [Math]::Round((Median $timings.rec), 1)
    median_total_ms = [Math]::Round((Median $timings.total), 1)
    mean_total_ms = [Math]::Round(($timings.total | Measure-Object -Average).Average, 1)
    mean_lines = [Math]::Round(($timings.lines | Measure-Object -Average).Average, 1)
    orientation_checked = ($timings.checked | Measure-Object -Sum).Sum
    orientation_overruled = ($timings.changed | Measure-Object -Sum).Sum
}
$summary | ConvertTo-Json | Set-Content (Join-Path $runDir 'summary.json') -Encoding utf8
Get-Content (Join-Path $runDir 'run.0.log') -Encoding UTF8 | Select-Object -First 3
"$count 张图，$Shards 个进程，用时 $($summary.wall_seconds) s；单个进程的峰值工作集 $peakMb MB；每页中位耗时 det $($summary.median_det_ms) / cls $($summary.median_cls_ms) / rec $($summary.median_rec_ms) / 合计 $($summary.median_total_ms) ms（平均 $($summary.mean_lines) 行）"
