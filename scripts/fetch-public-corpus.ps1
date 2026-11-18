# 取公开测试文件（分类 G，tests/corpus/public/README.md）到本机，给往返测试和内容流读写层实验用。
# 文件不进主仓库：
#   - pdfium（默认）：PDFium 的 testing/resources（固定在 chromium/8138，BSD-3-Clause，346 个 PDF）
#   - pdfjs         ：mozilla/pdf.js 的 test/pdfs（固定提交，Apache-2.0，1092 个随仓 PDF；不含 .link 外链）
#   - all           ：两批都取
#
# PDFium 没有把 testing/resources/*.pdf 标成 binary：core.autocrlf=true 的检出会把约 270 个 PDF 的换行改成 CRLF，
# 往返结果因此不可比（third_party/pdfium/README.md 第 2 节、《工程规范》第 1 节）。这里克隆时就把 core.autocrlf 设成 false，
# 两批取完都核对一遍内容指纹；指纹对不上就失败，不让坏掉的文件进到测试里。
#
# 用法：
#   pwsh scripts/fetch-public-corpus.ps1 [-Dest <目录>] [-Force]                         # 默认只取 pdfium（兼容流水线）
#   pwsh scripts/fetch-public-corpus.ps1 -Corpus pdfjs [-PdfjsDest <目录>] [-Force]      # 只取 pdf.js
#   pwsh scripts/fetch-public-corpus.ps1 -Corpus all [-Force]                            # 两批都取
#   pwsh scripts/fetch-public-corpus.ps1 -VerifyOnly -Corpus pdfjs -Dest <已有的检出>    # 只核对，不下载
#
# 在 GitHub Actions 里会把取到的目录写进 GITHUB_ENV（MAGPDF_PUBLIC_PDFIUM / MAGPDF_PUBLIC_PDFJS），
# 后面的 scripts/nightly-roundtrip.ps1 直接读它们。
#
# 代理：读取环境变量 HTTPS_PROXY。
param(
    [ValidateSet('pdfium', 'pdfjs', 'all')] [string]$Corpus = 'pdfium',
    [string]$Dest = '',
    [string]$PdfjsDest = '',
    [switch]$Force,
    [switch]$VerifyOnly,
    # 下面几项只在测试或替换镜像时改。
    [string]$Url = 'https://pdfium.googlesource.com/pdfium',
    [string]$Branch = 'chromium/8138',
    [string]$PdfjsArchiveUrl = '',
    [string]$PdfjsCommit = 'cf5e89757794c44e3961cc9133b260b6cb0ca796',
    [string]$Python = 'python'
)
$ErrorActionPreference = 'Stop'

# 版本和指纹固定在这里。
# PDFium 升级时和 scripts/fetch-pdfium-prebuilt.ps1、tests/baselines/roundtrip/public-pdfium.json 一起改；
# pdf.js 固定在 V17 实验核对过的那一次提交（docs/p0/V17-自研无损内容流读写层原型.md 9.15）。
# 指纹的算法见 tests/corpus/tools/corpus_fingerprint.py。
$CommitPrefix = '5f092b5299'
$ExpectedFiles = 346
$ExpectedSha256 = '0cfbd442a229d5805df4739b77d0faad60f8c27e349fd96c8ad7e82a041b9657'

$PdfjsExpectedFiles = 1092
$PdfjsExpectedSha256 = '49b6bf620978239670c388b42372bbc6b34624301cbbce4cd2c1e3624becfbdb'

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$fingerprintTool = Join-Path $root 'tests\corpus\tools\corpus_fingerprint.py'

function Resolve-Target([string]$Path) {
    $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Path)
}

function Test-CorpusFingerprint([string]$Dir, [string]$Sha256, [int]$Files) {
    if (-not (Test-Path $Dir -PathType Container)) { return $false }
    & $Python $fingerprintTool $Dir --expect $Sha256 --expect-files $Files | Out-Host
    return ($LASTEXITCODE -eq 0)
}

function Invoke-Git {
    & git @args
    if ($LASTEXITCODE -ne 0) { throw "git $($args -join ' ') 失败（退出码 $LASTEXITCODE）" }
}

if ($Corpus -in 'pdfium', 'all') {
    $pdfiumDest = if ($Dest) {
        if ($Corpus -eq 'all') { Join-Path $Dest 'pdfium' } else { $Dest }
    } else {
        Join-Path $root 'tests\out\corpus-cache\pdfium'
    }
    $pdfiumDest = Resolve-Target $pdfiumDest
    $resources = Join-Path $pdfiumDest 'testing\resources'

    if ($VerifyOnly) {
        if (-not (Test-CorpusFingerprint $resources $ExpectedSha256 $ExpectedFiles)) {
            throw "PDFium 公开测试集核对没有通过：$resources"
        }
    } elseif (-not $Force -and (Test-Path $resources -PathType Container) -and (Test-CorpusFingerprint $resources $ExpectedSha256 $ExpectedFiles)) {
        Write-Host "PDFium 公开测试集已经在 $resources，指纹对得上，跳过（加 -Force 重新取）"
    } else {
        if (Test-Path $pdfiumDest) { Remove-Item $pdfiumDest -Recurse -Force }
        New-Item -ItemType Directory -Force (Split-Path $pdfiumDest -Parent) | Out-Null
        Write-Host "从 $Url 取 $Branch 的 testing/resources 到 $pdfiumDest"
        # 只取一个提交、只检出 testing/resources；--filter 让服务器只在检出时才给用到的文件（服务器不支持时 git 会照常全取）。
        Invoke-Git clone --depth 1 --branch $Branch --filter=blob:none --no-checkout -c core.autocrlf=false $Url $pdfiumDest
        Invoke-Git -C $pdfiumDest sparse-checkout set testing/resources
        Invoke-Git -C $pdfiumDest checkout --quiet
        $head = (& git -C $pdfiumDest rev-parse HEAD)
        if ($head -notlike "$CommitPrefix*") {
            Write-Warning "$Branch 现在指向 $head，不是固定的 $CommitPrefix；以内容指纹为准。"
        }
        if (-not (Test-CorpusFingerprint $resources $ExpectedSha256 $ExpectedFiles)) {
            throw "取下来的 PDFium 公开测试集指纹对不上（$resources）。先查 core.autocrlf：git -C `"$pdfiumDest`" config core.autocrlf 应当是 false。"
        }
    }

    if ($env:GITHUB_ENV) { "MAGPDF_PUBLIC_PDFIUM=$resources" | Add-Content -Path $env:GITHUB_ENV -Encoding utf8 }
    Write-Host "PDFium 公开测试集：$resources"
}

if ($Corpus -in 'pdfjs', 'all') {
    $pdfjsRoot = if ($PdfjsDest) {
        $PdfjsDest
    } elseif ($Dest) {
        if ($Corpus -eq 'all') { Join-Path $Dest 'pdfjs' } else { $Dest }
    } else {
        Join-Path $root 'tests\out\corpus-cache\pdfjs'
    }
    $pdfjsRoot = Resolve-Target $pdfjsRoot
    $pdfsDir = if ((Test-Path $pdfjsRoot -PathType Container) -and -not (Test-Path (Join-Path $pdfjsRoot 'test\pdfs') -PathType Container) -and @(Get-ChildItem $pdfjsRoot -Filter '*.pdf' -File -ErrorAction SilentlyContinue).Count -gt 0) {
        $pdfjsRoot
    } else {
        Join-Path $pdfjsRoot 'test\pdfs'
    }

    if ($VerifyOnly) {
        if (-not (Test-CorpusFingerprint $pdfsDir $PdfjsExpectedSha256 $PdfjsExpectedFiles)) {
            throw "pdf.js 公开测试集核对没有通过：$pdfsDir"
        }
    } elseif (-not $Force -and (Test-Path $pdfsDir -PathType Container) -and (Test-CorpusFingerprint $pdfsDir $PdfjsExpectedSha256 $PdfjsExpectedFiles)) {
        Write-Host "pdf.js 公开测试集已经在 $pdfsDir，指纹对得上，跳过（加 -Force 重新取）"
    } else {
        $archiveUrl = if ($PdfjsArchiveUrl) { $PdfjsArchiveUrl } else { "https://codeload.github.com/mozilla/pdf.js/tar.gz/$PdfjsCommit" }
        $parent = Split-Path $pdfjsRoot -Parent
        New-Item -ItemType Directory -Force $parent | Out-Null
        # 压缩包约 128 MB，放在目标父目录（通常在数据盘），避免占系统盘临时目录。
        $tgz = Join-Path $parent "pdfjs-$($PdfjsCommit.Substring(0, 12)).tar.gz"
        if ($Force -and (Test-Path $tgz)) { Remove-Item $tgz -Force }
        Write-Host "从 $archiveUrl 下载 pdf.js 归档，只解出 test/pdfs 到 $pdfjsRoot"
        $curlArgs = @('-L', '--fail', '--retry', '5', '--retry-delay', '3', '-C', '-', '-o', $tgz, $archiveUrl)
        if ($env:HTTPS_PROXY) { $curlArgs = @('-x', $env:HTTPS_PROXY) + $curlArgs }
        & curl.exe @curlArgs
        if ($LASTEXITCODE -ne 0) { throw "下载 pdf.js 归档失败（退出码 $LASTEXITCODE）：$archiveUrl" }

        if (Test-Path $pdfjsRoot) { Remove-Item $pdfjsRoot -Recurse -Force }
        New-Item -ItemType Directory -Force $pdfjsRoot | Out-Null
        $entryPrefix = "pdf.js-$PdfjsCommit/test/pdfs"
        & tar.exe -xzf $tgz -C $pdfjsRoot --strip-components=1 $entryPrefix
        $tarCode = $LASTEXITCODE
        Remove-Item $tgz -Force -ErrorAction SilentlyContinue
        if ($tarCode -ne 0) { throw "解压 $entryPrefix 失败（tar 退出码 $tarCode）" }
        Set-Content -Path (Join-Path $pdfjsRoot 'COMMIT.txt') -Value $PdfjsCommit -Encoding ascii
        $pdfsDir = Join-Path $pdfjsRoot 'test\pdfs'
        if (-not (Test-CorpusFingerprint $pdfsDir $PdfjsExpectedSha256 $PdfjsExpectedFiles)) {
            throw "取下来的 pdf.js 公开测试集指纹对不上（$pdfsDir）"
        }
    }

    if ($env:GITHUB_ENV) { "MAGPDF_PUBLIC_PDFJS=$pdfsDir" | Add-Content -Path $env:GITHUB_ENV -Encoding utf8 }
    Write-Host "pdf.js 公开测试集：$pdfsDir"
}
