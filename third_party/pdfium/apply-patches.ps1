# 把 third_party/pdfium/patches/ 下的补丁打到 PDFium 检出目录里的 DEPS 仓库（目前只有 build/）。
# 这些仓库不在 magpdf-pdfium 分支里，改动只能以补丁形式保存在主仓库，由 sync.ps1 / build.ps1 调用本脚本应用。
# 幂等：已应用的补丁跳过；既不能正向也不能反向应用的补丁报错（说明 DEPS 滚动后补丁需要更新）。
#
# 用法：pwsh third_party/pdfium/apply-patches.ps1 -Source D:\pdfium-src\pdfium [-Check]
#   -Check 只检查不修改，全部已应用时退出码 0，否则 1。
#
# 目录约定：patches/<子仓库相对路径，'/' 换成 '-'>/NNNN-*.patch，例如 patches/build/0001-*.patch 作用于 <Source>/build。
#Requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$Source,
    [switch]$Check
)
$ErrorActionPreference = 'Stop'

$patchRoot = Join-Path $PSScriptRoot 'patches'
if (-not (Test-Path $patchRoot)) { Write-Host 'apply-patches: 没有补丁目录，跳过'; exit 0 }

$pending = 0
# 主仓库 core.autocrlf=true 的检出会把 *.patch 变成 CRLF，git apply 会把 \r 当作内容而匹配失败；
# 这里统一规范成 LF 的临时副本再应用（补丁内容本身都是 LF 文本）。
$tmpDir = Join-Path ([IO.Path]::GetTempPath()) ("magpdf-patches-" + [IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Force $tmpDir | Out-Null
try {
    foreach ($repoDir in Get-ChildItem $patchRoot -Directory) {
        if ($repoDir.Name -eq 'pdfium') { continue }  # pdfium 自身的提交由 sync.ps1 用 git am 处理
        $target = Join-Path $Source ($repoDir.Name -replace '-', '/')
        if (-not (Test-Path (Join-Path $target '.git'))) { throw "apply-patches: $target 不是 git 仓库（gclient sync 还没跑？）" }
        foreach ($patch in Get-ChildItem $repoDir.FullName -Filter '*.patch' | Sort-Object Name) {
            $lf = Join-Path $tmpDir "$($repoDir.Name)-$($patch.Name)"
            [IO.File]::WriteAllText($lf, ([IO.File]::ReadAllText($patch.FullName) -replace "`r`n", "`n"), (New-Object Text.UTF8Encoding $false))
            & git -C $target apply --reverse --check $lf 2>$null
            if ($LASTEXITCODE -eq 0) { Write-Host "apply-patches: 已应用 $($repoDir.Name)/$($patch.Name)"; continue }
            & git -C $target apply --check $lf 2>$null
            if ($LASTEXITCODE -ne 0) {
                throw "apply-patches: $($repoDir.Name)/$($patch.Name) 既不能应用也不是已应用状态，请对照 $target 当前版本更新补丁"
            }
            if ($Check) { Write-Host "apply-patches: 未应用 $($repoDir.Name)/$($patch.Name)"; $pending++; continue }
            & git -C $target apply $lf
            if ($LASTEXITCODE -ne 0) { throw "apply-patches: 应用 $($patch.Name) 失败" }
            Write-Host "apply-patches: 已打上 $($repoDir.Name)/$($patch.Name)"
        }
    }
}
finally { Remove-Item $tmpDir -Recurse -Force -ErrorAction SilentlyContinue }
if ($Check -and $pending -gt 0) { exit 1 }
