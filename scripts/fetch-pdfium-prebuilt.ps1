# 拉取 pdfium-binaries 发布的原版 PDFium 预编译库到 third_party/pdfium-prebuilt/。
# P0 用原版 PDFium 做往返基线（方案 2.1、P0 计划 V02）；非引擎组成员也靠它编译运行（方案 12）。
# 版本和校验和固定在这里，升级时一起改。
#
# 用法：pwsh scripts/fetch-pdfium-prebuilt.ps1 [-Force]
# 代理：读取环境变量 HTTPS_PROXY（国内直连 GitHub Release 常超时）。
param(
    [switch]$Force
)
$ErrorActionPreference = 'Stop'

$Release = 'chromium/8138'
$Asset = 'pdfium-win-x64.tgz'
$Sha256 = '8D48973E3EB472DDA0F5F2435E595AC043F9F2219DD71883392F549445AFA8A5'
$ExpectedVersion = 'BUILD=8138'

$root = Resolve-Path (Join-Path $PSScriptRoot '..')
$dest = Join-Path $root 'third_party\pdfium-prebuilt'
$versionFile = Join-Path $dest 'VERSION'

if (-not $Force -and (Test-Path $versionFile) -and (Select-String -Path $versionFile -Pattern $ExpectedVersion -Quiet)) {
    Write-Host "pdfium-prebuilt 已是 $Release，跳过（加 -Force 重新下载）"
    exit 0
}

$url = "https://github.com/bblanchon/pdfium-binaries/releases/download/$([uri]::EscapeDataString($Release))/$Asset"
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) "magpdf-$Asset"
Write-Host "下载 $url"
$curlArgs = @('-L', '--fail', '--retry', '3', '-o', $tmp, $url)
if ($env:HTTPS_PROXY) { $curlArgs = @('-x', $env:HTTPS_PROXY) + $curlArgs }
& curl.exe @curlArgs
if ($LASTEXITCODE -ne 0) { throw "下载失败（exit $LASTEXITCODE）" }

$actual = (Get-FileHash $tmp -Algorithm SHA256).Hash
if ($actual -ne $Sha256) { throw "SHA-256 不匹配：期望 $Sha256，实际 $actual" }

if (Test-Path $dest) { Remove-Item $dest -Recurse -Force }
New-Item -ItemType Directory -Path $dest | Out-Null
& tar -xzf $tmp -C $dest
if ($LASTEXITCODE -ne 0) { throw "解压失败（exit $LASTEXITCODE）" }
Remove-Item $tmp -Force

Write-Host "已安装到 $dest"
Get-Content $versionFile | Write-Host
