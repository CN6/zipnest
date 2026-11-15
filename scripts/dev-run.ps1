#Requires -Version 5.1
<#
.SYNOPSIS
    Rebuild and relaunch zipnest-native in one call, returning once the window exists.

.DESCRIPTION
    The dev loop had four hand-typed steps every time: taskkill, cargo build,
    start the exe, sleep 5, then screenshot and hope the window was up. Two of
    those steps are gone here:

    - the running instance is killed and its lock released before the link step,
      so cargo is never fighting a locked zipnest.exe;
    - the script waits for the window to actually appear (polling the process's
      MainWindowHandle) instead of sleeping a fixed number of seconds, so it
      returns in ~1 s on a warm build and never returns on a dead one - it fails.

    It then prints the window rect, which is what the UI-driving tools need.

    It does NOT touch the installed copy under D:\ZipNest, the shell integration,
    or any archive. It only builds and runs target\<config>\zipnest-native.exe.

.PARAMETER Release
    Build and run the release binary instead of debug.

.PARAMETER NoBuild
    Skip cargo entirely and just relaunch the existing binary.

.PARAMETER NoKill
    Leave a running instance alone (it still gets killed if it locks the binary,
    because otherwise the link step fails).

.PARAMETER TimeoutSeconds
    How long to wait for the window. Default 60.

.PARAMETER ExtraArgs
    Arguments for the app, e.g. -ExtraArgs 'D:\claude\zipnest\fixtures\plain.zip'.

.EXAMPLE
    pwsh -File scripts\dev-run.ps1

.EXAMPLE
    pwsh -File scripts\dev-run.ps1 -Release -ExtraArgs 'fixtures\plain.zip'
#>
[CmdletBinding()]
param(
    [switch]$Release,
    [switch]$NoBuild,
    [switch]$NoKill,
    [int]$TimeoutSeconds = 60,
    [string[]]$ExtraArgs = @()
)

$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path (Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) '..')).Path
$profileName = if ($Release) { 'release' } else { 'debug' }
$exe = Join-Path $repo ("target\{0}\zipnest-native.exe" -f $profileName)

# PowerShell 5.1 starts plain: no cargo on PATH, and a non-UTF8 console mangles
# the Chinese the app may print.
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path -LiteralPath $cargoBin) { $env:Path = "$cargoBin;$env:Path" }
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

function Stop-Running {
    $procs = Get-Process -Name 'zipnest-native' -ErrorAction SilentlyContinue
    if (-not $procs) { return }
    $procs | ForEach-Object {
        try { $_.Kill() } catch { }
    }
    # The exe file stays locked until the process is really gone; wait for that
    # rather than sleeping and hoping.
    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Process -Name 'zipnest-native' -ErrorAction SilentlyContinue) -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 100
    }
    $left = Get-Process -Name 'zipnest-native' -ErrorAction SilentlyContinue
    if ($left) { throw "zipnest-native.exe (pid $($left.Id -join ',')) did not exit" }
}

if (-not $NoKill) { Stop-Running }

if (-not $NoBuild) {
    if (-not $Release) { Stop-Running }   # the linker needs the file free
    Write-Host "cargo build -p zipnest-native ($profileName) ..."
    $argList = @('build', '-p', 'zipnest-native')
    if ($Release) { $argList += '--release' }
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    # cargo writes progress to stderr; 5.1 would treat that as a terminating
    # error under 'Stop'. Only the exit code decides success here.
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & cargo @argList 2>&1 | ForEach-Object {
        $line = "$_"
        if ($line -match '^(error|warning: unused)') { Write-Host $line -ForegroundColor DarkYellow }
    }
    $code = $LASTEXITCODE
    $ErrorActionPreference = $prev
    $sw.Stop()
    if ($code -ne 0) { throw "cargo build failed (exit $code)" }
    Write-Host ("build ok in {0:N1} s" -f $sw.Elapsed.TotalSeconds)
}

if (-not (Test-Path -LiteralPath $exe)) {
    throw "$exe not found. Run without -NoBuild first."
}

$quoted = $ExtraArgs | ForEach-Object { if ($_ -match '\s') { '"' + $_ + '"' } else { $_ } }
$argText = if ($quoted) { ' ' + ($quoted -join ' ') } else { '' }
Write-Host "launch: $exe$argText"
$proc = if ($argText) {
    Start-Process -FilePath $exe -ArgumentList $argText -PassThru
} else {
    Start-Process -FilePath $exe -PassThru
}

# Wait for the real window, not for a fixed delay - and not for "any window".
# Two traps here, both measured: MainWindowHandle goes non-zero while the app is
# still a 16x16 startup-flash window (AGENTS.md "Startup flash"), and the handle
# is replaced when Windows applies the saved size. So poll for the *largest*
# visible window of the process and require its rect to be unchanged across two
# samples, which means the size has settled.
Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class W32DevRun {
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr p);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }

    public static IntPtr Largest(uint pid, out int w, out int h) {
        // out/ref cannot be captured by the lambda, so the search works on
        // locals and hands them back at the end.
        IntPtr best = IntPtr.Zero; long area = 0; int bw = 0, bh = 0;
        EnumWindows(delegate(IntPtr hw, IntPtr p) {
            uint got;
            GetWindowThreadProcessId(hw, out got);
            if (got != pid || !IsWindowVisible(hw) || IsIconic(hw)) return true;
            RECT r;
            if (!GetWindowRect(hw, out r)) return true;
            long a = (long)(r.Right - r.Left) * (r.Bottom - r.Top);
            if (a > area) { area = a; best = hw; bw = r.Right - r.Left; bh = r.Bottom - r.Top; }
            return true;
        }, IntPtr.Zero);
        w = bw; h = bh;
        return best;
    }
}
"@ -ErrorAction SilentlyContinue
[void][W32DevRun]::SetProcessDPIAware()

$deadline = (Get-Date).AddSeconds($TimeoutSeconds)
$hwnd = [IntPtr]::Zero
$w = 0; $h = 0
$lastSig = ''
$stable = 0
while ((Get-Date) -lt $deadline) {
    if ($proc.HasExited) {
        throw ("zipnest-native exited during startup with code {0}" -f $proc.ExitCode)
    }
    $cand = [W32DevRun]::Largest([uint32]$proc.Id, [ref]$w, [ref]$h)
    # 200x150: anything smaller is the startup flash, never the main window
    # (WINDOW_SIZE minimum is 400).
    if ($cand -ne [IntPtr]::Zero -and $w -ge 200 -and $h -ge 150) {
        $sig = "$cand/$w/$h"
        if ($sig -eq $lastSig) { $stable++ } else { $stable = 0; $lastSig = $sig }
        if ($stable -ge 2) { $hwnd = $cand; break }
    }
    Start-Sleep -Milliseconds 150
}
if ($hwnd -eq [IntPtr]::Zero) {
    throw "the main window never settled within $TimeoutSeconds s (pid $($proc.Id))"
}

# The handle and size exist before egui has painted; one short beat covers the
# first frame without the old blanket sleep 5.
Start-Sleep -Milliseconds 400

$r = New-Object W32DevRun+RECT
[void][W32DevRun]::GetWindowRect($hwnd, [ref]$r)
Write-Output ("pid={0} hwnd={1} rect={2},{3} {4}x{5}" -f `
    $proc.Id, $hwnd, $r.Left, $r.Top, ($r.Right - $r.Left), ($r.Bottom - $r.Top))
Write-Output ("ready: powershell -File scripts\ui-shot.ps1 -ProcessId {0} -Out shot.png" -f $proc.Id)
