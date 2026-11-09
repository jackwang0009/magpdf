# 按 manifest.json 下载 OCR 模型与 ONNX Runtime，并校验 SHA-256（方案 6.5、13.3；P0 计划 V09）。
#
# 用法：
#   pwsh models/fetch-models.ps1                 # 默认内置模型（-Set default）+ ONNX Runtime
#   pwsh models/fetch-models.ps1 -Set server     # 另拉服务器版 det/rec 作对照
#   pwsh models/fetch-models.ps1 -Set all        # 清单里的全部模型
#   pwsh models/fetch-models.ps1 -Verify         # 只校验已下载文件
#   pwsh models/fetch-models.ps1 -SkipRuntime    # 不下载 ONNX Runtime
# 代理：读取环境变量 HTTPS_PROXY（GitHub Release 国内直连常超时；ModelScope 一般直连可达）。
param(
    [string[]]$Set = @('default'),
    [switch]$SkipRuntime,
    [switch]$Force,
    [switch]$Verify
)
$ErrorActionPreference = 'Stop'

$root = $PSScriptRoot
$manifest = Get-Content (Join-Path $root 'manifest.json') -Raw -Encoding UTF8 | ConvertFrom-Json

function Get-Sha256([string]$path) {
    (Get-FileHash -Path $path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Test-FileOk([string]$path, [string]$sha256, [long]$size) {
    if (-not (Test-Path $path)) { return $false }
    $item = Get-Item $path
    if ($size -gt 0 -and $item.Length -ne $size) { return $false }
    return (Get-Sha256 $path) -eq $sha256.ToLowerInvariant()
}

function Invoke-Download([string]$url, [string]$dest) {
    $dir = Split-Path $dest -Parent
    if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }
    $tmp = "$dest.part"
    $curlArgs = @('-sS', '-L', '--fail', '--retry', '3', '--retry-delay', '2', '-o', $tmp, $url)
    if ($env:HTTPS_PROXY) { $curlArgs = @('-x', $env:HTTPS_PROXY) + $curlArgs }
    Write-Host "下载 $url"
    & curl.exe @curlArgs
    if ($LASTEXITCODE -ne 0) { throw "下载失败（exit $LASTEXITCODE）：$url" }
    Move-Item -Force $tmp $dest
}

function Sync-File($entry, [string]$dest) {
    if (-not $Force -and (Test-FileOk $dest $entry.sha256 $entry.size)) {
        Write-Host "已就绪 $($entry.id)（$dest）"
        return $true
    }
    if ($Verify) {
        Write-Warning "缺失或校验失败：$($entry.id)（$dest）"
        return $false
    }
    Invoke-Download $entry.url $dest
    $actual = Get-Sha256 $dest
    if ($actual -ne $entry.sha256.ToLowerInvariant()) {
        Remove-Item $dest -Force
        throw "SHA-256 不匹配：$($entry.id) 期望 $($entry.sha256)，实际 $actual"
    }
    if ($entry.size -gt 0 -and (Get-Item $dest).Length -ne $entry.size) {
        Remove-Item $dest -Force
        throw "大小不匹配：$($entry.id) 期望 $($entry.size) 字节"
    }
    Write-Host "完成 $($entry.id)（$([math]::Round((Get-Item $dest).Length / 1MB, 2)) MB）"
    return $true
}

$ok = $true

# 模型
$wanted = $manifest.models | Where-Object {
    ($Set -contains 'all') -or (@($_.sets) | Where-Object { $Set -contains $_ })
}
if (-not $wanted) { throw "没有匹配 -Set $($Set -join ',') 的模型；可用：default、server、legacy、all" }
foreach ($m in $wanted) {
    $dest = Join-Path $root ($m.file -replace '/', '\')
    if (-not (Sync-File $m $dest)) { $ok = $false }
}

# ONNX Runtime：下载 zip、校验、只解出需要的文件
if (-not $SkipRuntime) {
    $rt = $manifest.runtime
    $missing = @($rt.extract | Where-Object { -not (Test-Path (Join-Path $root ($_.to -replace '/', '\'))) })
    $versionFile = Join-Path $root 'onnxruntime\VERSION_NUMBER'
    $versionOk = (Test-Path $versionFile) -and ((Get-Content $versionFile -Raw).Trim() -eq $rt.version)
    if (-not $Force -and $missing.Count -eq 0 -and $versionOk) {
        Write-Host "已就绪 onnxruntime $($rt.version)"
    } elseif ($Verify) {
        Write-Warning "onnxruntime $($rt.version) 缺失或版本不符"
        $ok = $false
    } else {
        $zip = Join-Path ([System.IO.Path]::GetTempPath()) "magpdf-onnxruntime-$($rt.version).zip"
        if (-not (Test-FileOk $zip $rt.sha256 $rt.size)) {
            Invoke-Download $rt.url $zip
            $actual = Get-Sha256 $zip
            if ($actual -ne $rt.sha256.ToLowerInvariant()) {
                Remove-Item $zip -Force
                throw "SHA-256 不匹配：onnxruntime 期望 $($rt.sha256)，实际 $actual"
            }
        }
        $stage = Join-Path ([System.IO.Path]::GetTempPath()) "magpdf-onnxruntime-$($rt.version)"
        if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
        New-Item -ItemType Directory -Path $stage | Out-Null
        & tar -xf $zip -C $stage
        if ($LASTEXITCODE -ne 0) { throw "解压失败（exit $LASTEXITCODE）" }
        foreach ($e in $rt.extract) {
            $src = Join-Path $stage ($e.from -replace '/', '\')
            $dst = Join-Path $root ($e.to -replace '/', '\')
            $dir = Split-Path $dst -Parent
            if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }
            Copy-Item -Force $src $dst
        }
        Remove-Item $stage -Recurse -Force
        Write-Host "完成 onnxruntime $($rt.version) → $(Join-Path $root 'onnxruntime')"
    }
}

if (-not $ok) { exit 1 }
Write-Host "全部就绪。模型目录：$root"
