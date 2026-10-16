# 构建便携版：测试 → 编译 → 校验依赖 → 拷贝到 dist-portable → 打包 zip
#
# 用法：
#   pwsh -File scripts\build.ps1                # 完整流程（含测试）
#   pwsh -File scripts\build.ps1 -SkipTests     # 只编译打包
#   pwsh -File scripts\build.ps1 -NoZip         # 不打包 zip

[CmdletBinding()]
param(
    [switch]$SkipTests,
    [switch]$NoZip
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$triple = 'x86_64-pc-windows-msvc'
$releaseDir = Join-Path $root "target\$triple\release"
$exe = Join-Path $releaseDir 'ClipHist.exe'
$dist = Join-Path $root 'dist-portable'

function Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }

# cargo 不在 PATH 里时（非交互式 shell）补上
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "找不到 cargo，请确认 Rust 工具链已安装"
}

Push-Location $root
try {
    if (-not $SkipTests) {
        Step '运行测试'
        cargo test --quiet
        if ($LASTEXITCODE -ne 0) { throw '测试失败，已中止打包' }
    }

    Step '编译 release'
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw '编译失败' }
    if (-not (Test-Path $exe)) { throw "找不到产物 $exe" }

    Step '校验依赖（便携版不应依赖 vcruntime140.dll 等 VC++ 运行库）'
    $dumpbin = Get-ChildItem "${env:ProgramFiles}\Microsoft Visual Studio\2022\*\VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe" -ErrorAction SilentlyContinue |
        Select-Object -First 1
    $bad = @()
    if ($dumpbin) {
        $deps = & $dumpbin.FullName /dependents $exe 2>$null |
            Select-String -Pattern '^\s{4}(\S+\.dll)$' |
            ForEach-Object { $_.Matches[0].Groups[1].Value.ToLower() }
        $bad = $deps | Where-Object { $_ -match '^(vcruntime|msvcp|ucrtbased)' }
        Write-Host ("    依赖：{0}" -f (($deps | Sort-Object -Unique) -join ', '))
    } else {
        Write-Warning '没找到 dumpbin，跳过依赖校验（不影响产物）'
    }
    if ($bad.Count -gt 0) { throw "产物依赖了 VC++ 运行库：$($bad -join ', ')" }

    Step '自检（存储与图片编解码）'
    & $exe --selftest
    if ($LASTEXITCODE -ne 0) { throw '自检失败' }

    Step '拷贝到 dist-portable'
    New-Item -ItemType Directory -Force -Path $dist | Out-Null
    Copy-Item $exe (Join-Path $dist 'ClipHist.exe') -Force
    $usage = Join-Path $root 'packaging\使用说明.txt'
    if (Test-Path $usage) { Copy-Item $usage (Join-Path $dist '使用说明.txt') -Force }

    $sizeKb = [math]::Round((Get-Item (Join-Path $dist 'ClipHist.exe')).Length / 1KB, 1)
    Write-Host "    ClipHist.exe  $sizeKb KB"

    if (-not $NoZip) {
        Step '打包 zip'
        $version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"(.+)"' |
            Select-Object -First 1).Matches[0].Groups[1].Value
        $zip = Join-Path $dist "ClipHist-$version-portable.zip"
        if (Test-Path $zip) { Remove-Item $zip -Force }
        # 只打包 exe 与说明，绝不把 data.db（可能含敏感历史）打进去
        Compress-Archive -Path (Join-Path $dist 'ClipHist.exe'), (Join-Path $dist '使用说明.txt') -DestinationPath $zip
        Write-Host "    $zip"
    }

    Step '完成'
    Write-Host "便携版目录：$dist"
} finally {
    Pop-Location
}
