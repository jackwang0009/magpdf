<#
.SYNOPSIS
    V17 的全部实验：两批样本 × 两份 PDFium × 不改动 / 随机修改。

.DESCRIPTION
    同一个 magpdf-cli.exe 分别配原版预编译库和修补版的 pdfium.dll：把可执行文件和 DLL 各复制到
    一个目录里，从那里运行（Windows 先从可执行文件所在目录装载 DLL）。

    每个组合跑四次：
      roundtrip            不改动；对得上的页也经改写层写一遍再读回核对（--rewrite-all）；带生成器对照组
      modify               随机修改 5%，修改种类轮流取（首轮的做法）；带生成器对照组
      modify-random        随机修改 5%，修改种类随机取；带生成器对照组
      modify-regenerated   同 modify，但预期画面按首轮的做法得到（只跑自制样本，用来复查首轮的数字）

    结果写到 tests/out/v17/<Tag>/：每次运行一个 JSON、一个差异图目录，外加 manifest.json
    （两份 DLL 和可执行文件的 SHA-256、提交号、各次运行的退出码）。

.EXAMPLE
    . D:\workspace\magpdf-wt\setup-dev-env.ps1
    pwsh engine/magpdf-content/tools/run-experiments.ps1 -Tag r2
#>
[CmdletBinding()]
param(
    # 输出目录名：tests/out/v17/<Tag>
    [string]$Tag = 'r2',
    # 原版预编译库（含 bin/pdfium.dll）
    [string]$StockDir = $(if ($env:PDFIUM_PREBUILT_DIR) { $env:PDFIUM_PREBUILT_DIR } else { 'third_party/pdfium-prebuilt' }),
    # 修补版的固定副本（含 bin/pdfium.dll）
    [string]$PatchedDir = 'D:\workspace\magpdf-wt\pdfium-dist-8477545cb9',
    # PDFium 公开测试集
    [string]$Public = 'D:\tmp\v03-public\testing\resources',
    # 放两份“可执行文件 + DLL”的目录
    [string]$WorkDir = $(Join-Path $(if ($env:TEMP) { $env:TEMP } else { '.' }) 'v17-run'),
    [int]$Jobs = 4,
    [int]$Timeout = 120,
    # 不重新构建 magpdf-cli
    [switch]$SkipBuild,
    # 只跑自制样本
    [switch]$NoPublic
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
Push-Location $repo
try {
    if (-not $SkipBuild) {
        cargo build --locked --offline --release -p magpdf-cli
        if ($LASTEXITCODE -ne 0) { throw "构建 magpdf-cli 失败（退出码 $LASTEXITCODE）" }
    }
    $exe = Join-Path $repo 'target\release\magpdf-cli.exe'
    if (-not (Test-Path $exe)) { throw "找不到 $exe" }

    $outDir = Join-Path $repo "tests\out\v17\$Tag"
    New-Item -ItemType Directory -Force $outDir | Out-Null

    $pdfiums = [ordered]@{
        stock   = (Join-Path $StockDir 'bin\pdfium.dll')
        patched = (Join-Path $PatchedDir 'bin\pdfium.dll')
    }
    $corpora = [ordered]@{ synthetic = (Join-Path $repo 'tests\corpus\synthetic') }
    if (-not $NoPublic) { $corpora['public'] = $Public }

    $manifest = [ordered]@{
        tag        = $Tag
        started    = (Get-Date).ToString('o')
        commit     = (git rev-parse HEAD)
        dirty      = [bool](git status --porcelain)
        exe_sha256 = (Get-FileHash $exe -Algorithm SHA256).Hash
        pdfium     = [ordered]@{}
        corpora    = [ordered]@{}
        runs       = @()
    }
    foreach ($name in $corpora.Keys) {
        $manifest.corpora[$name] = [ordered]@{
            path  = $corpora[$name]
            files = @(Get-ChildItem $corpora[$name] -Recurse -Filter *.pdf).Count
        }
    }

    foreach ($kind in $pdfiums.Keys) {
        $dll = $pdfiums[$kind]
        if (-not (Test-Path $dll)) { throw "找不到 $dll" }
        $runDir = Join-Path $WorkDir $kind
        New-Item -ItemType Directory -Force $runDir | Out-Null
        Copy-Item $exe (Join-Path $runDir 'magpdf-cli.exe') -Force
        Copy-Item $dll (Join-Path $runDir 'pdfium.dll') -Force
        $manifest.pdfium[$kind] = [ordered]@{
            source = (Resolve-Path $dll).Path
            sha256 = (Get-FileHash $dll -Algorithm SHA256).Hash
            bytes  = (Get-Item $dll).Length
        }
        $cli = Join-Path $runDir 'magpdf-cli.exe'

        foreach ($corpus in $corpora.Keys) {
            $experiments = [ordered]@{
                'roundtrip'     = @('--mode', 'roundtrip', '--rewrite-all', '--control')
                'modify'        = @('--mode', 'modify', '--control')
                'modify-random' = @('--mode', 'modify', '--edit-mix', 'random', '--control')
            }
            if ($corpus -eq 'synthetic') {
                $experiments['modify-regenerated'] = @('--mode', 'modify', '--expected', 'regenerated', '--control')
            }
            foreach ($exp in $experiments.Keys) {
                $stem = "$corpus-$kind-$exp"
                $json = Join-Path $outDir "$stem.json"
                $diff = Join-Path $outDir "diff\$stem"
                if (Test-Path $diff) { Remove-Item $diff -Recurse -Force }
                $cliArgs = @('content-roundtrip', $corpora[$corpus], '--source', $corpus) + $experiments[$exp] + @(
                    '--isolate', '--jobs', $Jobs, '--timeout', $Timeout, '--quiet',
                    '--label', "$kind pdfium.dll（$($manifest.pdfium[$kind].sha256.Substring(0, 16))）",
                    '--json', $json, '--diff-dir', $diff)
                $watch = [System.Diagnostics.Stopwatch]::StartNew()
                & $cli @cliArgs | Out-Null
                $code = $LASTEXITCODE
                $watch.Stop()
                Write-Host ("{0,-40} 退出码 {1}  {2:N1} s" -f $stem, $code, $watch.Elapsed.TotalSeconds)
                $manifest.runs += [ordered]@{ name = $stem; exit_code = $code; seconds = [math]::Round($watch.Elapsed.TotalSeconds, 1) }
            }
        }
    }
    $manifest['finished'] = (Get-Date).ToString('o')
    $manifest | ConvertTo-Json -Depth 6 | Set-Content -Encoding utf8 (Join-Path $outDir 'manifest.json')
    Write-Host "结果在 $outDir"
}
finally {
    Pop-Location
}
