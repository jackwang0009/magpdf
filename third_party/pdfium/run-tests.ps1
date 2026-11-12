# 跑 PDFium 自己的测试（pdfium_unittests、pdfium_embeddertests），和 known-test-failures.txt 里登记的
# “这台机器上原版就不过”的用例对比：有新的失败就退出码 1。
#
# pdfium_unittests 用共享库构建的输出目录（build.ps1 默认的 out\Release）；
# pdfium_embeddertests 要静态链接，用另一个输出目录（args/embeddertests.gn，默认 out\Tests）。
#
# 用法（在主仓库根目录）：
#   pwsh third_party/pdfium/run-tests.ps1 -Source D:\pdfium-src\pdfium -Jobs 8            # 先增量构建两个目录，再跑全部
#   pwsh third_party/pdfium/run-tests.ps1 -NoBuild -Filter 'CPDFPageContentGeneratorTest.*:FPDFEdit*'
#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$Source = $(if ($env:PDFIUM_SRC) { $env:PDFIUM_SRC } else { 'D:\pdfium-src\pdfium' }),
    [int]$Jobs = 8,
    # gtest 过滤器，两个测试程序都用它；默认跑全部
    [string]$Filter = '*',
    # 不构建，直接跑已有的可执行文件
    [switch]$NoBuild,
    # 写进 dist/VERSION 的版本；给了就不联网查（见 build.ps1）
    [string]$Version = '159.0.8138.0',
    # 日志目录；默认 <Source>\out\test-logs
    [string]$LogDir = ''
)
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$Source = (Resolve-Path $Source).Path
if (-not $LogDir) { $LogDir = Join-Path $Source 'out\test-logs' }
New-Item -ItemType Directory -Force $LogDir | Out-Null

if (-not $NoBuild) {
    & (Join-Path $here 'build.ps1') -Source $Source -Jobs $Jobs -Version $Version | Select-Object -Last 3
    if ($LASTEXITCODE -ne 0) { throw 'out\Release 构建失败' }
    & (Join-Path $here 'build.ps1') -Source $Source -Jobs $Jobs -Out 'out\Tests' `
        -ArgsTemplate (Join-Path $here 'args\embeddertests.gn') -Targets pdfium_embeddertests -NoDist | Select-Object -Last 1
    if ($LASTEXITCODE -ne 0) { throw 'out\Tests 构建失败' }
}

# 登记的失败：每行“<测试程序> <用例名>  # 原因”
$known = @{}
foreach ($line in Get-Content (Join-Path $here 'known-test-failures.txt')) {
    $text = ($line -replace '#.*$', '').Trim()
    if (-not $text) { continue }
    $exe, $name = $text -split '\s+', 2
    $known["$exe $name"] = $true
}

$newFailures = @()
foreach ($t in @(@{ exe = 'pdfium_unittests'; dir = 'out\Release' }, @{ exe = 'pdfium_embeddertests'; dir = 'out\Tests' })) {
    $path = Join-Path $Source (Join-Path $t.dir "$($t.exe).exe")
    if (-not (Test-Path $path)) { throw "找不到 $path（去掉 -NoBuild 先构建）" }
    $log = Join-Path $LogDir "$($t.exe).txt"
    Push-Location (Split-Path $path)
    try { & $path "--gtest_filter=$Filter" '--gtest_brief=1' *> $log } finally { Pop-Location }
    $exit = $LASTEXITCODE
    $summary = (Select-String -Path $log -Pattern '^\[==========\] (\d+) tests? from' | Select-Object -Last 1)
    $ran = if ($summary) { [int]$summary.Matches[0].Groups[1].Value } else { 0 }
    $failed = @(Select-String -Path $log -Pattern '^\[  FAILED  \] (\S+) \(' | ForEach-Object { $_.Matches[0].Groups[1].Value } | Sort-Object -Unique)
    $unexpected = @($failed | Where-Object { -not $known["$($t.exe) $_"] })
    Write-Host ("{0}: 跑了 {1} 个，失败 {2} 个（其中登记过的 {3} 个）  日志 {4}" -f $t.exe, $ran, $failed.Count, ($failed.Count - $unexpected.Count), $log)
    if ($ran -eq 0 -or ($exit -ne 0 -and $failed.Count -eq 0)) {
        # 没跑起来或者中途崩了：gtest 来不及打印失败的用例
        Get-Content $log | Select-Object -Last 15 | Write-Host
        $newFailures += "$($t.exe)：没有正常跑完（exit $exit）"
    }
    foreach ($name in $unexpected) { $newFailures += "$($t.exe) $name" }
}

if ($newFailures.Count -gt 0) {
    Write-Host "新的失败（$($newFailures.Count)）：" -ForegroundColor Red
    $newFailures | ForEach-Object { Write-Host "  $_" }
    exit 1
}
Write-Host '没有新的失败。' -ForegroundColor Green
