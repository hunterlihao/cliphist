# 验证主窗口：先确认窗口属于本次启动的进程，再发托盘单击
$ErrorActionPreference = 'Stop'
$EXE = 'D:\code\AI-WorkSpace\clipboard-history\target\e2e\ClipHist.exe'
Get-Process ClipHist -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 800
$procs = @(Get-Process ClipHist -ErrorAction SilentlyContinue)
"残留进程: $($procs.Count)"

Add-Type -Namespace Q -Name W -MemberDefinition @'
[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr parent, IntPtr after, string cls, string win);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
[DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint msg, IntPtr wp, IntPtr lp);
[StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
'@

$p = Start-Process -FilePath $EXE -PassThru
Start-Sleep -Seconds 2
"本次 PID: $($p.Id) 存活: $(-not $p.HasExited)"

function Find-All([string]$class) {
    $out = @()
    $after = [IntPtr]::Zero
    while ($true) {
        $h = [Q.W]::FindWindowExW([IntPtr]::Zero, $after, $class, $null)
        if ($h -eq [IntPtr]::Zero) { break }
        $pid2 = 0
        [Q.W]::GetWindowThreadProcessId($h, [ref]$pid2) | Out-Null
        $r = New-Object Q.W+RECT
        [Q.W]::GetWindowRect($h, [ref]$r) | Out-Null
        $out += [pscustomobject]@{
            Hwnd = $h; Pid = $pid2; Class = $class; Visible = [Q.W]::IsWindowVisible($h)
            X = $r.Left; Y = $r.Top; W = ($r.Right-$r.Left); H = ($r.Bottom-$r.Top)
        }
        $after = $h
    }
    return $out
}

"=== 控制器窗口 ==="
$ctls = Find-All 'ClipHist.Ctl'
$ctls | Format-Table -AutoSize
"=== 列表窗口 ==="
Find-All 'ClipHist.Popup' | Format-Table -AutoSize

$ctl = $ctls | Where-Object { $_.Pid -eq $p.Id } | Select-Object -First 1
if (-not $ctl) { "FAIL 找不到本次进程的控制器"; Stop-Process -Id $p.Id -Force; exit 1 }
"向 PID $($ctl.Pid) 的控制器发托盘单击"
[Q.W]::PostMessageW($ctl.Hwnd, 0x8001, [IntPtr]::Zero, [IntPtr]0x0202) | Out-Null
Start-Sleep -Milliseconds 1000
"=== 之后的列表窗口 ==="
$lists = Find-All 'ClipHist.Popup'
$lists | Format-Table -AutoSize
$main = $lists | Where-Object { $_.Pid -eq $p.Id -and $_.Visible -and $_.W -ge 700 } | Select-Object -First 1
if ($main) { "PASS 主窗口已打开：($($main.X),$($main.Y)) $($main.W)x$($main.H)" } else { "FAIL 主窗口未打开" }
Stop-Process -Id $p.Id -Force
