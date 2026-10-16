# 内存实测：空库 / 预填 N 条历史 / 剪贴板 soak 三段采样，并给出判定
#
# 用法：
#   pwsh -File scripts\measure-memory.ps1
#   pwsh -File scripts\measure-memory.ps1 -Fill 1000 -IdleSeconds 20
#
# 说明：
# - 全程在隔离目录（target\memtest）里跑，不会碰你自己的 data.db。
# - 主指标是 PrivateMemorySize64（私有提交），不含共享页，是"这程序到底占多少"的诚实口径；
#   WorkingSet64 把共用 DLL 的页也算进来，只能作参考。
# - 第二段用程序自带的 --bench-fill 预填 N 条历史（走正式 Store 代码入库），
#   用来验证核心主张：**常驻内存与历史条数解耦**。
# - 第三段模拟连续复制，需要可用的剪贴板。若当前会话没有交互式桌面
#   （Set-Clipboard 回读为空），脚本会明确写"soak 未执行"，绝不伪造数据。

[CmdletBinding()]
param(
    [string]$Exe,
    [int]$IdleSeconds = 20,
    [int]$Fill = 500,
    [int]$SoakCopies = 50,
    [int]$IntervalSec = 3,
    [double]$IdlePrivateLimitMB = 8.0,
    [double]$GrowthLimitMB = 3.0,
    [switch]$KeepRunning
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$triple = 'x86_64-pc-windows-msvc'

function Say($msg) { Write-Host $msg }

function Resolve-Exe {
    param([string]$Given)
    if ($Given) {
        if (-not (Test-Path $Given)) { throw "指定的 exe 不存在：$Given" }
        return (Resolve-Path $Given).Path
    }
    foreach ($c in @((Join-Path $root 'dist-portable\ClipHist.exe'),
                     (Join-Path $root "target\$triple\release\ClipHist.exe"))) {
        if (Test-Path $c) { return (Resolve-Path $c).Path }
    }
    throw '找不到 ClipHist.exe，请先跑 scripts\build.ps1'
}

function Sample-Process {
    param([int]$TargetPid, [int]$Seconds, [int]$Interval, [string]$Phase)
    $samples = @()
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        $p = Get-Process -Id $TargetPid -ErrorAction SilentlyContinue
        if (-not $p) { throw "进程在「$Phase」阶段退出了（可能崩溃）" }
        $samples += [pscustomobject]@{
            Private = $p.PrivateMemorySize64
            Working = $p.WorkingSet64
            Handles = $p.HandleCount
            Threads = $p.Threads.Count
        }
        Start-Sleep -Seconds $Interval
    }
    if ($samples.Count -eq 0) { throw "「$Phase」阶段没有采到样本" }
    [pscustomobject]@{
        Phase      = $Phase
        Samples    = $samples.Count
        PrivateMin = ($samples.Private | Measure-Object -Minimum).Minimum / 1MB
        PrivateAvg = ($samples.Private | Measure-Object -Average).Average / 1MB
        PrivateMax = ($samples.Private | Measure-Object -Maximum).Maximum / 1MB
        WorkMax    = ($samples.Working | Measure-Object -Maximum).Maximum / 1MB
        HandlesMax = ($samples.Handles | Measure-Object -Maximum).Maximum
        ThreadsMax = ($samples.Threads | Measure-Object -Maximum).Maximum
    }
}

function Start-Scratch {
    param([string]$Scratch, [string]$Phase)
    $proc = Start-Process -FilePath $Scratch -PassThru -WindowStyle Hidden
    Start-Sleep -Seconds 3
    if ($proc.HasExited) { throw "「$Phase」启动后立即退出（退出码 $($proc.ExitCode)）" }
    return $proc
}

function Stop-ClipHist {
    Get-Process ClipHist -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 400
}

$exePath = Resolve-Exe $Exe
$work = Join-Path $root 'target\memtest'
New-Item -ItemType Directory -Force -Path $work | Out-Null
$scratch = Join-Path $work 'ClipHist.exe'
Copy-Item $exePath $scratch -Force
$dbPath = Join-Path $work 'data.db'

Say "被测程序：$exePath"
Say "隔离目录：$work"
Say ''

Stop-ClipHist
Remove-Item $dbPath -Force -ErrorAction SilentlyContinue

$results = @()
$notes = @()

# ① 空库空闲
$proc = Start-Scratch -Scratch $scratch -Phase '空库'
Say "已启动（空库），PID = $($proc.Id)"
$results += Sample-Process -TargetPid $proc.Id -Seconds $IdleSeconds -Interval $IntervalSec -Phase "空闲 · 空库"
Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 400

# ② 预填 N 条历史后再测：验证"内存与条数解耦"
#
# 关键：让程序**自己把结果写进文件**（--log），不要试图从父进程捕获它的标准输出。
# 在受限沙箱/非交互式嵌套 shell 里，那类管道重定向会让父子进程互相等待而卡死 ——
# 这一点是实测踩出来的，脚本改成现在这样之后才稳定。
$fillLog = Join-Path $work 'fill.log'
Remove-Item $fillLog -Force -ErrorAction SilentlyContinue
$fillProc = Start-Process -FilePath $scratch `
    -ArgumentList '--bench-fill', "$Fill", '--log', $fillLog `
    -Wait -PassThru -WindowStyle Hidden
Say ''
Say "    --bench-fill $Fill 退出码 $($fillProc.ExitCode)"
if (Test-Path $fillLog) {
    Get-Content $fillLog | Where-Object { $_ -match '=' } | ForEach-Object { "    $_" }
} else {
    Say '    （未拿到 bench-fill 结果文件）'
}
$proc = Start-Scratch -Scratch $scratch -Phase '预填后'
Say "已启动（$Fill 条历史），PID = $($proc.Id)"
$results += Sample-Process -TargetPid $proc.Id -Seconds $IdleSeconds -Interval $IntervalSec -Phase "空闲 · $Fill 条历史"
Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 400

# ③ 剪贴板 soak：先探测剪贴板是否真的可用，避免"假装测过"
$soakDone = 0
$clipboardOk = $false
try {
    $token = "cliphist-probe-$([guid]::NewGuid().ToString('N'))"
    Set-Clipboard -Value $token -ErrorAction Stop
    Start-Sleep -Milliseconds 200
    $clipboardOk = ((Get-Clipboard -Raw -ErrorAction Stop) -eq $token)
} catch {
    $clipboardOk = $false
}

if (-not $clipboardOk) {
    $notes += 'soak 未执行：当前会话剪贴板不可用（Set-Clipboard 后回读为空）。请在正常桌面会话里重跑本脚本。'
} else {
    $proc = Start-Scratch -Scratch $scratch -Phase 'soak'
    Add-Type -AssemblyName System.Windows.Forms -ErrorAction SilentlyContinue
    $sampleFile = Join-Path $work 'soak-file.txt'
    Set-Content -Path $sampleFile -Value 'soak' -Encoding UTF8
    $fileDropOk = $true
    for ($i = 1; $i -le $SoakCopies; $i++) {
        if ($i % 3 -eq 0 -and $fileDropOk) {
            try {
                [System.Windows.Forms.Clipboard]::SetFileDropList(
                    [System.Collections.Specialized.StringCollection]@($sampleFile))
            } catch {
                $fileDropOk = $false
                $notes += "文件复制模拟不可用（$($_.Exception.Message)），已全部改用文本复制。"
            }
        } else {
            Set-Clipboard -Value ("soak-{0}-{1}" -f $i, [guid]::NewGuid().ToString('N'))
        }
        $soakDone = $i
        Start-Sleep -Milliseconds 120
    }
    Start-Sleep -Seconds 2
    $results += Sample-Process -TargetPid $proc.Id -Seconds ([Math]::Max(9, $IntervalSec * 3)) -Interval $IntervalSec -Phase "soak 后 · $soakDone 次复制"
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
}

Say ''
Say '阶段                        样本 私有最小 私有平均 私有峰值 工作集峰值 句柄 线程'
Say '-------------------------------------------------------------------------------------'
foreach ($r in $results) {
    Say ("{0,-24} {1,5} {2,8:N1} {3,8:N1} {4,8:N1} {5,10:N1} {6,5} {7,5}" -f `
        $r.Phase, $r.Samples, $r.PrivateMin, $r.PrivateAvg, $r.PrivateMax, $r.WorkMax, $r.HandlesMax, $r.ThreadsMax)
}

if (Test-Path $dbPath) {
    Say ''
    Say ("data.db：{0:N1} KB" -f ((Get-Item $dbPath).Length / 1KB))
}

# 判定
Say ''
$fail = @()
$empty = $results | Where-Object { $_.Phase -like '*空库*' } | Select-Object -First 1
$filled = $results | Where-Object { $_.Phase -like "*$Fill 条历史*" } | Select-Object -First 1

if ($empty -and $empty.PrivateMax -le $IdlePrivateLimitMB) {
    Say ("PASS  空库空闲私有内存 {0:N1} MB <= {1:N1} MB" -f $empty.PrivateMax, $IdlePrivateLimitMB)
} elseif ($empty) {
    Say ("FAIL  空库空闲私有内存 {0:N1} MB > {1:N1} MB" -f $empty.PrivateMax, $IdlePrivateLimitMB)
    $fail += 'idle-empty'
}

if ($empty -and $filled) {
    $delta = $filled.PrivateMax - $empty.PrivateMax
    if ($delta -le $GrowthLimitMB) {
        Say ("PASS  {0} 条历史只多占 {1:N1} MB（内存与条数解耦，阈值 {2:N1} MB）" -f $Fill, $delta, $GrowthLimitMB)
    } else {
        Say ("FAIL  {0} 条历史多占 {1:N1} MB > {2:N1} MB" -f $Fill, $delta, $GrowthLimitMB)
        $fail += 'fill-growth'
    }
}

if ($soakDone -gt 0) {
    $after = $results | Where-Object { $_.Phase -like 'soak*' } | Select-Object -First 1
    $growth = $after.PrivateMax - $filled.PrivateMax
    if ($growth -le $GrowthLimitMB) {
        Say ("PASS  soak 后私有内存增长 {0:N1} MB <= {1:N1} MB（无明显泄漏）" -f $growth, $GrowthLimitMB)
    } else {
        Say ("FAIL  soak 后私有内存增长 {0:N1} MB > {1:N1} MB" -f $growth, $GrowthLimitMB)
        $fail += 'soak-growth'
    }
}

foreach ($n in $notes) { Say "NOTE  $n" }

if (-not $KeepRunning) {
    Say ''
    Say '已结束被测进程。'
}

if ($fail.Count -gt 0) { exit 1 }
exit 0
