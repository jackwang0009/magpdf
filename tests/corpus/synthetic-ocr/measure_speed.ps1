<#
.SYNOPSIS
  量 OCR 的速度和内存（V09）。一条命令把报告里的速度数据重量一遍，结果写成 JSON。

.DESCRIPTION
  样本是 out/speed/ 里的三页（gen_synthetic.py 生成）：中文合同、英文提单、30 行货物的英文装箱单，
  每页有 300 dpi 的 PNG 和 PDF、200 dpi 的 PNG、仿手机拍照的 JPEG。

  一共跑 -Runs 轮（默认 5）。每一轮里各个测量项各起一个进程，进程加载一次模型，然后每页连着识别
  -Repeat 次（默认 3）。各项轮流跑，不是一项连着跑完再跑下一项：机器上别的任务时多时少，轮流跑
  每一项的数都摊在整段时间里，各项之间才好比。于是每一项有：
    首次加载     每个进程一个数（加载 ONNX Runtime、建三个会话），共 Runs 个
    第一页       每个进程第一次识别的耗时（会话刚建好，比后面的慢），共 Runs 个
    每页         其余各次识别的耗时，每页 Runs × Repeat 个（进程里排第一的那一页少 Runs 个）
    取图         图片的读文件加解码，或 PDF 的打开、加载页面、渲染；每页 Runs 个
    峰值工作集   每个进程一个数
    CPU 时间     每个进程各线程用掉的 CPU 时间之和，和进程的墙上时间放在一起看机器忙不忙
  每个数给中位数、最小、最大和个数。每个进程开始前记一次系统 CPU 占用（别的任务占了多少）。

  PDF 一次只能给一个文件，所以 PDF 那两项每份文件各起一个进程，进程数是 Runs × 3。

  全部跑完要半个小时左右。每一轮结束都把到这一轮为止的结果写一次；中途被打断，结果文件里是已经
  跑完的那几轮，runs_completed 是轮数（跑完时等于 runs）。

.EXAMPLE
  pwsh tests/corpus/synthetic-ocr/measure_speed.ps1
  pwsh tests/corpus/synthetic-ocr/measure_speed.ps1 -Only png300,pdf300 -Out out/speed-quick.json
#>
param(
    [ValidateRange(1, 50)] [int]$Runs = 5,
    [ValidateRange(2, 20)] [int]$Repeat = 3,
    [string]$Out = '',
    [string]$Cli = '',
    # 只跑这几项（名字见下面的 $cases）
    [string[]]$Only = @()
)
$ErrorActionPreference = 'Stop'
# 写成 `pwsh measure_speed.ps1 -Only a,b` 另起一个进程时，a,b 进来是一个带逗号的字符串，这里拆开。
$Only = @($Only | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$root = Resolve-Path (Join-Path $PSScriptRoot '..\..\..')
$speed = Join-Path $PSScriptRoot 'out\speed'
if (-not (Test-Path (Join-Path $speed 'Label.txt'))) {
    throw "没有样本，先运行 python $PSScriptRoot\gen_synthetic.py"
}
if (-not $Cli) { $Cli = Join-Path $root 'target\release\magpdf-cli.exe' }
if (-not (Test-Path $Cli)) { throw "找不到 $Cli，先 cargo build -p magpdf-cli --release" }
if (-not $Out) { $Out = Join-Path $PSScriptRoot 'out\speed-result.json' }
$work = Join-Path $PSScriptRoot 'out\speed-run'
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $work | Out-Null

$pages = 'speed_zh_contract', 'speed_en_bl', 'speed_en_packing_dense'
$enPages = 'speed_en_bl', 'speed_en_packing_dense'
$native = '--det-limit-type', 'min', '--det-limit-side', '64'
# 每一项：名字、说明、用哪几页、文件名后缀、识别参数。
$cases = @(
    @{ Name = 'png300';           Note = '300 dpi 图片，默认配置，线程数交给 ONNX Runtime'; Pages = $pages;   Suffix = '_noise_300.png';  Args = @() }
    @{ Name = 'png300-t4';        Note = '同上，每个会话 4 个线程';                         Pages = $pages;   Suffix = '_noise_300.png';  Args = @('--threads', '4') }
    @{ Name = 'png300-t2';        Note = '同上，每个会话 2 个线程';                         Pages = $pages;   Suffix = '_noise_300.png';  Args = @('--threads', '2') }
    @{ Name = 'png300-t1';        Note = '同上，每个会话 1 个线程';                         Pages = $pages;   Suffix = '_noise_300.png';  Args = @('--threads', '1') }
    @{ Name = 'pdf300';           Note = '扫描件 PDF，按 300 dpi 渲染后识别';               Pages = $pages;   Suffix = '_noise_300.pdf';  Args = @('--dpi', '300') }
    @{ Name = 'pdf300-t4';        Note = '同上，每个会话 4 个线程';                         Pages = $pages;   Suffix = '_noise_300.pdf';  Args = @('--dpi', '300', '--threads', '4') }
    @{ Name = 'png200';           Note = '200 dpi 图片';                                    Pages = $pages;   Suffix = '_noise_200.png';  Args = @() }
    @{ Name = 'photo12mp';        Note = '仿手机拍照，3024×4032 的 JPEG';                   Pages = $pages;   Suffix = '_photo_12mp.jpg'; Args = @() }
    @{ Name = 'png300-en';        Note = '英文页用英文专用识别模型';                        Pages = $enPages; Suffix = '_noise_300.png';  Args = @('--lang', 'en') }
    @{ Name = 'png300-zh-on-en';  Note = '同样两页英文用多语言识别模型（和上一项对比）';    Pages = $enPages; Suffix = '_noise_300.png';  Args = @() }
    @{ Name = 'png300-noverify';  Note = '不做方向复核';                                    Pages = $pages;   Suffix = '_noise_300.png';  Args = @('--no-cls-verify') }
    @{ Name = 'png300-nocls';     Note = '不做方向分类';                                    Pages = $pages;   Suffix = '_noise_300.png';  Args = @('--no-cls') }
    @{ Name = 'png300-det960';    Note = '检测输入长边 960';                                Pages = $pages;   Suffix = '_noise_300.png';  Args = @('--det-limit-side', '960') }
    @{ Name = 'png300-detnative'; Note = '检测不缩小（PaddleX 流水线的默认缩放规则）';      Pages = $pages;   Suffix = '_noise_300.png';  Args = $native }
    @{ Name = 'png300-server';    Note = '服务器版检测、识别模型';                          Pages = $pages;   Suffix = '_noise_300.png';  Args = @('--server') }
)
$unknown = @($Only | Where-Object { $_ -notin $cases.Name })
if ($unknown) { throw "没有这些测量项：$($unknown -join '、')；可选：$($cases.Name -join '、')" }

function Stat($values) {
    $s = @($values | Where-Object { $null -ne $_ } | Sort-Object)
    $n = $s.Count
    if ($n -eq 0) { return $null }
    $median = if ($n % 2) { $s[($n - 1) / 2] } else { ($s[$n / 2 - 1] + $s[$n / 2]) / 2 }
    [ordered]@{ median = [Math]::Round($median, 1); min = [Math]::Round($s[0], 1); max = [Math]::Round($s[$n - 1], 1); n = $n }
}
function CpuLoad {
    try { [Math]::Round((Get-Counter '\Processor(_Total)\% Processor Time').CounterSamples.CookedValue, 1) } catch { $null }
}
# 跑一个进程，返回用时、峰值工作集、CPU 时间。
function Invoke-Cli([string[]]$ArgList, [string]$Log) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process -FilePath $Cli -ArgumentList $ArgList -NoNewWindow -PassThru `
        -RedirectStandardOutput $Log -RedirectStandardError "$Log.err"
    $peak = 0
    while (-not $p.HasExited) {
        try { $p.Refresh(); $peak = [Math]::Max($peak, $p.PeakWorkingSet64) } catch {}
        Start-Sleep -Milliseconds 20
    }
    $p.WaitForExit()
    $sw.Stop()
    if ($p.ExitCode -ne 0) {
        Get-Content "$Log.err" | Select-Object -Last 10
        throw "magpdf-cli 退出码 $($p.ExitCode)：$($ArgList -join ' ')"
    }
    # 进程用掉的 CPU 时间（各线程相加）。
    $cpuMs = try { $p.TotalProcessorTime.TotalMilliseconds } catch { $null }
    [pscustomobject]@{ WallMs = $sw.Elapsed.TotalMilliseconds; PeakMb = $peak / 1MB; CpuMs = $cpuMs }
}
function StageTotal($t) { $t.det.resize + $t.det.infer + $t.det.postprocess }

# 准备：每一项一个目录，放样本的硬链接；state 里攒各个进程量到的数。
$selected = @($cases | Where-Object { -not $Only.Count -or $_.Name -in $Only })
$state = @{}
foreach ($case in $selected) {
    $caseDir = Join-Path $work $case.Name
    $inDir = Join-Path $caseDir 'in'
    New-Item -ItemType Directory -Force $inDir | Out-Null
    $files = @($case.Pages | ForEach-Object { Join-Path $speed "$_$($case.Suffix)" })
    foreach ($f in $files) {
        if (-not (Test-Path $f)) { throw "缺少样本 $f，重新运行 gen_synthetic.py" }
        New-Item -ItemType HardLink -Path (Join-Path $inDir (Split-Path $f -Leaf)) -Target $f | Out-Null
    }
    $isPdf = $case.Suffix.EndsWith('.pdf')
    $state[$case.Name] = @{
        Dir      = $caseDir
        IsPdf    = $isPdf
        # 图片：一个进程把目录里的几页按文件名顺序挨个识别。PDF：每份文件各起一个进程。
        Targets  = if ($isPdf) { @($files | ForEach-Object { Join-Path $inDir (Split-Path $_ -Leaf) }) } else { @($inDir) }
        Procs    = @()   # 每个进程：用时、峰值工作集、CPU 时间
        Loads    = @()   # 每个进程的加载耗时
        Firsts   = @()   # 每个进程的第一次识别
        Samples  = @{}   # 页 → 各次识别（除每个进程的第一次）
        Acquires = @{}   # 页 → 取图耗时
        Info     = @{}   # 页 → 尺寸、行数
        Cpu      = @()   # 每个进程开始前的系统 CPU 占用
        Models   = ''
    }
}

# 跑一轮：每个测量项起一个进程（PDF 是每份文件一个），量到的数攒进 $state。
function Measure-Round([int]$run) {
    foreach ($case in $selected) {
        $st = $state[$case.Name]
        $outDir = Join-Path $st.Dir "run$run"
        New-Item -ItemType Directory -Force $outDir | Out-Null
        foreach ($target in $st.Targets) {
            $json = if ($st.IsPdf) { Join-Path $outDir ((Split-Path $target -LeafBase) + '.json') } else { $outDir }
            $log = Join-Path $outDir ((Split-Path $target -LeafBase) + '.log')
            $st.Cpu += CpuLoad
            $st.Procs += Invoke-Cli (@('ocr', $target, '--json', $json, '--repeat', "$Repeat") + $case.Args) $log
            if (-not $st.Models) { $st.Models = (Get-Content $log -Encoding UTF8 | Select-Object -First 1) }
            # 结果文件按文件名排序，和进程里识别的顺序一样，第一个文件的第一次识别就是这个进程的第一次。
            $first = $true
            foreach ($jf in Get-ChildItem $outDir -Filter '*.json' | Sort-Object Name) {
                if ($st.IsPdf -and $jf.FullName -ne $json) { continue }
                $j = Get-Content $jf.FullName -Raw -Encoding UTF8 | ConvertFrom-Json
                $page = $jf.BaseName
                if (-not $st.Samples.ContainsKey($page)) { $st.Samples[$page] = @(); $st.Acquires[$page] = @() }
                $st.Info[$page] = [ordered]@{
                    width = $j.width; height = $j.height; lines = $j.lines.Count
                    det_input = $j.timings.det.input_size
                    orientation_checked = $j.orientation_checked; orientation_overruled = $j.orientation_overruled
                }
                $st.Acquires[$page] += $j.acquire
                $runsOfPage = @($j.runs)
                if ($first) {
                    $st.Loads += $j.load_timings
                    $st.Firsts += $runsOfPage[0]
                    $runsOfPage = @($runsOfPage | Select-Object -Skip 1)
                    $first = $false
                }
                $st.Samples[$page] += $runsOfPage
            }
        }
    }
}

$cpuInfo = Get-CimInstance Win32_Processor | Select-Object -First 1
$os = Get-CimInstance Win32_OperatingSystem
$machine = [ordered]@{
    cpu = $cpuInfo.Name.Trim()
    cores = $cpuInfo.NumberOfCores
    logical_processors = $cpuInfo.NumberOfLogicalProcessors
    memory_gb = [Math]::Round($os.TotalVisibleMemorySize / 1MB, 1)
    os = "$($os.Caption) $($os.Version)"
}

# 把到现在为止量到的数汇总，写成 JSON。每一轮结束都写一次：全部跑完要半个小时左右，中途被打断时
# 文件里留着已经跑完的那几轮（runs_completed 小于 runs），不会全丢。-Print 时把各项的结果打出来。
function Save-Report([int]$Done, [switch]$Print) {
    $results = @()
    foreach ($case in $selected) {
        $st = $state[$case.Name]
        $pageRows = foreach ($page in ($st.Samples.Keys | Sort-Object)) {
            $s = $st.Samples[$page]
            $total = Stat ($s | ForEach-Object { $_.total })
            $acq = Stat $st.Acquires[$page]
            [ordered]@{
                page = $page
                info = $st.Info[$page]
                acquire_ms = $acq
                total_ms = $total
                det_ms = Stat ($s | ForEach-Object { StageTotal $_ })
                det_infer_ms = Stat ($s | ForEach-Object { $_.det.infer })
                cls_ms = Stat ($s | ForEach-Object { $_.cls })
                rec_ms = Stat ($s | ForEach-Object { $_.rec })
                convert_crop_ms = Stat ($s | ForEach-Object { $_.convert + $_.crop })
                # 取图加识别：两边的中位数相加，不是逐次相加。
                acquire_plus_total_median_ms = [Math]::Round($acq.median + $total.median, 1)
            }
        }
        $row = [ordered]@{
            name = $case.Name
            note = $case.Note
            args = ($case.Args -join ' ')
            models = $st.Models
            processes = $st.Procs.Count
            cpu_load_before_percent = Stat $st.Cpu
            load_ms = [ordered]@{
                total = Stat ($st.Loads | ForEach-Object { $_.total })
                runtime = Stat ($st.Loads | ForEach-Object { $_.runtime })
                det = Stat ($st.Loads | ForEach-Object { $_.det })
                cls = Stat ($st.Loads | ForEach-Object { $_.cls })
                rec = Stat ($st.Loads | ForEach-Object { $_.rec })
            }
            first_page_ms = Stat ($st.Firsts | ForEach-Object { $_.total })
            peak_working_set_mb = Stat ($st.Procs | ForEach-Object { $_.PeakMb })
            process_wall_ms = Stat ($st.Procs | ForEach-Object { $_.WallMs })
            process_cpu_ms = Stat ($st.Procs | ForEach-Object { $_.CpuMs })
            pages = @($pageRows)
        }
        $results += $row
        if (-not $Print) { continue }
        Write-Host ("{0,-18} {1}（系统 CPU 占用中位数 {2}%，{3}–{4}%）" -f $case.Name, $case.Note,
            $row.cpu_load_before_percent.median, $row.cpu_load_before_percent.min, $row.cpu_load_before_percent.max)
        foreach ($pr in $pageRows) {
            Write-Host ("    {0,-36} {1,3} 行  取图 {2,7} ms  识别 {3,8} ms（{4}–{5}，{6} 次）  det {7} / cls {8} / rec {9}" -f `
                $pr.page, $pr.info.lines, $pr.acquire_ms.median, $pr.total_ms.median, $pr.total_ms.min, $pr.total_ms.max,
                $pr.total_ms.n, $pr.det_ms.median, $pr.cls_ms.median, $pr.rec_ms.median)
        }
        Write-Host ("    首次加载 {0} ms（{1}–{2}），第一页 {3} ms，峰值工作集 {4} MB" -f `
            $row.load_ms.total.median, $row.load_ms.total.min, $row.load_ms.total.max, $row.first_page_ms.median, $row.peak_working_set_mb.median)
    }
    $report = [ordered]@{
        generated = (Get-Date).ToString('yyyy-MM-ddTHH:mm:sszzz')
        machine = $machine
        cli = "$Cli"
        runs = $Runs
        runs_completed = $Done
        repeat = $Repeat
        cases = $results
    }
    # 先写到临时文件再改名：写到一半被打断，也不会留下半截的 JSON。
    $report | ConvertTo-Json -Depth 12 | Set-Content "$Out.tmp" -Encoding utf8
    Move-Item -Force "$Out.tmp" $Out
}

for ($run = 0; $run -lt $Runs; $run++) {
    Write-Host ("第 {0} 轮 / {1}（{2}）" -f ($run + 1), $Runs, (Get-Date).ToString('HH:mm:ss'))
    Measure-Round $run
    Save-Report ($run + 1) -Print:($run + 1 -eq $Runs)
}
Write-Host "结果写入 $Out"
