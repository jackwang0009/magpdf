# 取 PDFium 的公开测试文件（testing/resources，固定在 chromium/8138）到本机，给每晚的往返测试用。
# 文件不进仓库（tests/corpus/public/README.md）；许可是 BSD-3-Clause（PDFium 的 LICENSE）。
#
# PDFium 没有把 testing/resources/*.pdf 标成 binary：core.autocrlf=true 的检出会把约 270 个 PDF 的换行改成 CRLF，
# 往返结果因此不可比（third_party/pdfium/README.md 第 2 节、《工程规范》第 1 节）。这里克隆时就把 core.autocrlf 设成 false，
# 取完再核对一遍内容指纹；指纹对不上就失败，不让坏掉的文件进到测试里。
#
# 用法：
#   pwsh scripts/fetch-public-corpus.ps1 [-Dest <目录>] [-Force]
#   pwsh scripts/fetch-public-corpus.ps1 -VerifyOnly -Dest <已有的检出>     只核对，不下载
# 结果在 <Dest>\testing\resources（346 个 PDF）。在 GitHub Actions 里还会把这个目录写进环境变量
# MAGPDF_PUBLIC_PDFIUM，后面的 scripts/nightly-roundtrip.ps1 直接读它。
#
# 代理：git 自己读环境变量 HTTPS_PROXY（国内直连 googlesource.com 不通）。
param(
    [string]$Dest = '',
    [switch]$Force,
    [switch]$VerifyOnly,
    # 下面两项只在测试这个脚本时改（指到本机的一个仓库）。
    [string]$Url = 'https://pdfium.googlesource.com/pdfium',
    [string]$Branch = 'chromium/8138',
    [string]$Python = 'python'
)
$ErrorActionPreference = 'Stop'

# 版本和指纹固定在这里，升级 PDFium 时和 scripts/fetch-pdfium-prebuilt.ps1、tests/baselines/roundtrip/public-pdfium.json 一起改。
# 指纹的算法见 tests/corpus/tools/corpus_fingerprint.py。
$CommitPrefix = '5f092b5299'
$ExpectedFiles = 346
$ExpectedSha256 = '0cfbd442a229d5805df4739b77d0faad60f8c27e349fd96c8ad7e82a041b9657'

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$fingerprintTool = Join-Path $root 'tests\corpus\tools\corpus_fingerprint.py'
if (-not $Dest) { $Dest = Join-Path $root 'tests\out\corpus-cache\pdfium' }
$Dest = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Dest)
$resources = Join-Path $Dest 'testing\resources'

function Test-Fingerprint {
    if (-not (Test-Path $resources -PathType Container)) { return $false }
    & $Python $fingerprintTool $resources --expect $ExpectedSha256 --expect-files $ExpectedFiles | Out-Host
    return ($LASTEXITCODE -eq 0)
}

function Invoke-Git {
    & git @args
    if ($LASTEXITCODE -ne 0) { throw "git $($args -join ' ') 失败（退出码 $LASTEXITCODE）" }
}

if ($VerifyOnly) {
    if (-not (Test-Fingerprint)) { throw "公开测试集核对没有通过：$resources" }
} elseif (-not $Force -and (Test-Path $resources -PathType Container) -and (Test-Fingerprint)) {
    Write-Host "公开测试集已经在 $resources，指纹对得上，跳过（加 -Force 重新取）"
} else {
    if (Test-Path $Dest) { Remove-Item $Dest -Recurse -Force }
    New-Item -ItemType Directory -Force (Split-Path $Dest -Parent) | Out-Null
    Write-Host "从 $Url 取 $Branch 的 testing/resources 到 $Dest"
    # 只取一个提交、只检出 testing/resources；--filter 让服务器只在检出时才给用到的文件（服务器不支持时 git 会照常全取）。
    Invoke-Git clone --depth 1 --branch $Branch --filter=blob:none --no-checkout -c core.autocrlf=false $Url $Dest
    Invoke-Git -C $Dest sparse-checkout set testing/resources
    Invoke-Git -C $Dest checkout --quiet
    $head = (& git -C $Dest rev-parse HEAD)
    if ($head -notlike "$CommitPrefix*") {
        Write-Warning "$Branch 现在指向 $head，不是固定的 $CommitPrefix；以内容指纹为准。"
    }
    if (-not (Test-Fingerprint)) {
        throw "取下来的公开测试集指纹对不上（$resources）。先查 core.autocrlf：git -C `"$Dest`" config core.autocrlf 应当是 false。"
    }
}

if ($env:GITHUB_ENV) { "MAGPDF_PUBLIC_PDFIUM=$resources" | Add-Content -Path $env:GITHUB_ENV -Encoding utf8 }
Write-Host "公开测试集：$resources"
