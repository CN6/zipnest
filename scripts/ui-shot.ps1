#Requires -Version 5.1
<#
.SYNOPSIS
    Capture one window to a PNG, and print its rect so the numbers are usable.

.DESCRIPTION
    Replaces "launch, sleep 5, take a full-desktop screenshot, guess where the
    window is". The capture is bounded to the window (or its client area), so the
    image is small enough to read directly and the printed rect says exactly where
    it was on screen.

    ZipNest draws with egui/glow (OpenGL), so PrintWindow usually comes back
    black. The default -Method screen raises the window and copies the pixels;
    -Method print exists for the cases where the window must not take focus.

.PARAMETER ProcessId
    Owning process. Defaults to any running zipnest-native.exe.

.PARAMETER ProcessName
    Used when -ProcessId is omitted.

.PARAMETER Out
    PNG path. Defaults to %TEMP%\zipnest-shot-<hhmmss>.png.

.PARAMETER ClientOnly
    Capture the client area instead of the whole window frame.

.PARAMETER Method
    screen (default) or print.

.PARAMETER Largest
    When several windows match, take the largest by area. Default behaviour.

.EXAMPLE
    powershell -File scripts\ui-shot.ps1 -Out shot.png

.EXAMPLE
    powershell -File scripts\ui-shot.ps1 -ClientOnly -Out client.png
#>
[CmdletBinding()]
param(
    [int]$ProcessId = 0,
    [string]$ProcessName = 'zipnest-native',
    [string]$Out,
    [switch]$ClientOnly,
    [ValidateSet('screen', 'print')]
    [string]$Method = 'screen',
    [int]$SettleMs = 250
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

if (-not ('W32Shot' -as [type])) {
    Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;

public class W32Shot {
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr p);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();

    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }

    public static IntPtr FindLargest(uint wantPid) {
        IntPtr best = IntPtr.Zero;
        long bestArea = 0;
        EnumWindows(delegate(IntPtr h, IntPtr p) {
            uint pid;
            GetWindowThreadProcessId(h, out pid);
            if (wantPid != 0 && pid != wantPid) return true;
            if (!IsWindowVisible(h)) return true;
            if (IsIconic(h)) return true;
            RECT r;
            if (!GetWindowRect(h, out r)) return true;
            long a = (long)(r.Right - r.Left) * (r.Bottom - r.Top);
            if (a > bestArea) { bestArea = a; best = h; }
            return true;
        }, IntPtr.Zero);
        return best;
    }

    public static string TitleOf(IntPtr h) {
        var sb = new StringBuilder(512);
        GetWindowTextW(h, sb, sb.Capacity);
        return sb.ToString();
    }
}
"@
}

[void][W32Shot]::SetProcessDPIAware()

if (-not $ProcessId) {
    $proc = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue |
        Where-Object { $_.MainWindowHandle -ne 0 } |
        Sort-Object -Property Id |
        Select-Object -First 1
    if (-not $proc) {
        $proc = Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
    }
    if (-not $proc) { throw "no process named '$ProcessName' is running" }
    $ProcessId = $proc.Id
}

$hwnd = [W32Shot]::FindLargest([uint32]$ProcessId)
if ($hwnd -eq [IntPtr]::Zero) {
    throw "process $ProcessId has no visible top-level window"
}

$wrap = New-Object W32Shot+RECT
if ($ClientOnly) {
    $pt = New-Object W32Shot+POINT
    [void][W32Shot]::GetClientRect($hwnd, [ref]$wrap)
    $pt.X = 0
    $pt.Y = 0
    [void][W32Shot]::ClientToScreen($hwnd, [ref]$pt)
    $x = $pt.X
    $y = $pt.Y
    $w = $wrap.Right - $wrap.Left
    $h = $wrap.Bottom - $wrap.Top
} else {
    [void][W32Shot]::GetWindowRect($hwnd, [ref]$wrap)
    $x = $wrap.Left
    $y = $wrap.Top
    $w = $wrap.Right - $wrap.Left
    $h = $wrap.Bottom - $wrap.Top
}

if ($w -le 0 -or $h -le 0) { throw "window $hwnd has an empty rect ${w}x${h}" }

if (-not $Out) {
    $Out = Join-Path $env:TEMP ("zipnest-shot-{0}.png" -f (Get-Date -Format 'HHmmss'))
}
$outDir = Split-Path -Parent $Out
if ($outDir -and -not (Test-Path -LiteralPath $outDir)) {
    New-Item -ItemType Directory -Force -Path $outDir | Out-Null
}

$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
try {
    if ($Method -eq 'print') {
        $hdc = $g.GetHdc()
        try {
            # 2 = PW_RENDERFULLCONTENT
            [void][W32Shot]::PrintWindow($hwnd, $hdc, 2)
        } finally {
            $g.ReleaseHdc($hdc)
        }
    } else {
        [void][W32Shot]::ShowWindow($hwnd, 9)   # SW_RESTORE
        [void][W32Shot]::SetForegroundWindow($hwnd)
        if ($SettleMs -gt 0) { Start-Sleep -Milliseconds $SettleMs }
        $g.CopyFromScreen($x, $y, 0, 0, (New-Object System.Drawing.Size $w, $h))
    }
} finally {
    $g.Dispose()
}

$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()

$title = [W32Shot]::TitleOf($hwnd)
Write-Output ("hwnd={0} pid={1} rect={2},{3} {4}x{5} region={6} png={7} bytes={8}" -f `
    $hwnd, $ProcessId, $x, $y, $w, $h, $(if ($ClientOnly) { 'client' } else { 'window' }), `
    $Out, (Get-Item -LiteralPath $Out).Length)
Write-Output ("title={0}" -f $title)
