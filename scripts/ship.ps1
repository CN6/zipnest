#Requires -Version 5.1
<#
.SYNOPSIS
    The whole Windows release for one already-bumped version, in one call.

.DESCRIPTION
    AGENTS.md "Shipping a release" is eight numbered steps, and every one of
    them has been typed by hand on every release: test, clippy, two release
    builds, emit-portable, makensis, Compress-Archive, copy the exe over
    D:\ZipNest\zipnest.exe, cut the release notes out of CHANGELOG.md, tag,
    push, upload, delete the local copies. Steps 1 (version bump + CHANGELOG)
    stay manual - they are decisions - and the script refuses to start until
    check-versions.ps1 agrees that they are done.

    Publishing (tag, push, gh release create) only happens with -Publish. A bare
    run builds every artifact, verifies them, updates the installed copy, and
    stops: that matches the standing rule that a release needs an explicit
    go-ahead, and it means the go-ahead never triggers a 15-minute rebuild.

.PARAMETER Version
    Target version, e.g. 0.4.39. Defaults to the version in Cargo.toml.

.PARAMETER Publish
    Tag, push and create the GitHub release, then delete the local installers.

.PARAMETER SkipTests
    Skip cargo test / clippy (they run in CI anyway). Not for a real release.

.PARAMETER SkipBuild
    Skip the cargo builds and reuse what is already in target\release. Pair with
    -SkipPayload to re-cut only the installer, e.g. after fixing installer.nsi.

.PARAMETER SkipPayload
    Skip emit-portable.ps1 and reuse dist-portable\.

.PARAMETER SkipInstalledCopy
    Do not copy the exe over D:\ZipNest\zipnest.exe.

.PARAMETER InstalledExe
    Installed copy to replace. Default D:\ZipNest\zipnest.exe.

.EXAMPLE
    pwsh -File scripts\ship.ps1                 # build + verify, stop before publishing

.EXAMPLE
    pwsh -File scripts\ship.ps1 -Publish        # build, verify, publish
#>
[CmdletBinding()]
param(
    [string]$Version,
    [switch]$Publish,
    [switch]$SkipTests,
    [switch]$SkipBuild,
    [switch]$SkipPayload,
    [switch]$SkipInstalledCopy,
    [string]$InstalledExe = 'D:\ZipNest\zipnest.exe'
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$repo = (Resolve-Path (Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) '..')).Path
Set-Location -LiteralPath $repo

$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path -LiteralPath $cargoBin) { $env:Path = "$cargoBin;$env:Path" }

$stage = 0
function Step([string]$what) {
    $script:stage++
    Write-Host ''
    Write-Host ("[{0}] {1}" -f $script:stage, $what) -ForegroundColor Cyan
}
function Fail([string]$why) {
    Write-Host ''
    Write-Host "FAILED: $why" -ForegroundColor Red
    exit 1
}
function Warn([string]$what) { Write-Host "  ! $what" -ForegroundColor DarkYellow }

# Run a native command and decide on its exit code alone: 5.1 turns cargo's
# stderr into a terminating error under 'Stop', and cargo always writes its
# progress there.
function Invoke-Native {
    param([string]$Exe, [string[]]$Arguments, [switch]$Quiet)
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $out = & $Exe @Arguments 2>&1
    $code = $LASTEXITCODE
    $ErrorActionPreference = $prev
    if (-not $Quiet) {
        $out | ForEach-Object {
            $line = "$_"
            if ($line -match '^(error|warning)') { Write-Host $line -ForegroundColor DarkYellow }
        }
    }
    return [pscustomobject]@{ Code = $code; Output = $out }
}

# ---------------------------------------------------------------- 0. preflight
Step 'preflight'

if (-not $Version) {
    $cargoText = [System.IO.File]::ReadAllText((Join-Path $repo 'Cargo.toml'))
    if ($cargoText -notmatch '(?ms)^\[workspace\.package\].*?^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"') {
        Fail 'cannot read [workspace.package].version from Cargo.toml'
    }
    $Version = $Matches[1]
}
Write-Host "  version: $Version"

$dirty = & git status --porcelain
if ($dirty) {
    Warn 'working tree is not clean:'
    $dirty | ForEach-Object { Write-Host "    $_" }
    if ($Publish) {
        # Refused on purpose: the release commit should hold exactly the
        # version bump, CHANGELOG and code, reviewed as one change. The script
        # used to commit for you with --allow-empty, which produced a second,
        # empty commit on top of the real one (v0.4.39 shipped that way).
        Fail 'refusing to publish from a dirty tree - stage and commit the release first'
    }
}

# ------------------------------------------------------- 1. version agreement
Step 'version strings agree (packaging\check-versions.ps1)'
$cv = Invoke-Native powershell @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $repo 'packaging\check-versions.ps1'))
$cv.Output | ForEach-Object { Write-Host "  $_" }
if ($cv.Code -ne 0) { Fail 'check-versions.ps1 reported a mismatch; fix step 1 of AGENTS.md first' }

# ------------------------------------------------------------- 2. tests/clippy
if (-not $SkipTests) {
    Step 'cargo test --workspace'
    $t = Invoke-Native cargo @('test', '--workspace')
    ($t.Output | Select-String 'test result:|^error|FAILED') | ForEach-Object { Write-Host "  $_" }
    if ($t.Code -ne 0) { Fail 'tests failed' }

    Step 'cargo clippy --workspace --all-targets'
    # Warnings must not block: only a non-zero exit does. -D warnings is CI's job.
    $c = Invoke-Native cargo @('clippy', '--workspace', '--all-targets', '--message-format', 'short')
    $warnCount = ($c.Output | Select-String '^warning' | Measure-Object).Count
    Write-Host ("  warnings: {0}" -f $warnCount)
    if ($c.Code -ne 0) { Fail 'clippy failed' }
} else {
    Warn 'tests and clippy skipped (-SkipTests)'
}

# ---------------------------------------------------------------- 3. builds
Step 'build release binaries (zipnest-native, zipnest-shell)'
# A running copy of the exe holds the file open and cargo cannot replace it
# ("failed to remove file ... os error 5"). This is the same trap the dev loop
# hits; closing it here means a release never fails on a forgotten window.
$devRunning = Get-Process -Name 'zipnest-native' -ErrorAction SilentlyContinue
if ($devRunning) {
    Write-Host ("  closing {0} running zipnest-native instance(s)" -f @($devRunning).Count)
    $devRunning | ForEach-Object { try { $_.Kill() } catch { } }
    $lockDeadline = (Get-Date).AddSeconds(10)
    while ((Get-Process -Name 'zipnest-native' -ErrorAction SilentlyContinue) -and (Get-Date) -lt $lockDeadline) {
        Start-Sleep -Milliseconds 150
    }
    if (Get-Process -Name 'zipnest-native' -ErrorAction SilentlyContinue) {
        Fail 'zipnest-native.exe is still running and would lock the build output'
    }
}
foreach ($pkg in @('zipnest-native', 'zipnest-shell')) {
    if ($SkipBuild) { Break }
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $b = Invoke-Native cargo @('build', '-p', $pkg, '--release', '--message-format', 'short')
    $sw.Stop()
    if ($b.Code -ne 0) {
        $b.Output | Select-Object -Last 25 | ForEach-Object { Write-Host "  $_" }
        Fail "cargo build -p $pkg --release failed"
    }
    Write-Host ("  {0,-16} ok in {1:N1} s" -f $pkg, $sw.Elapsed.TotalSeconds)
}
if ($SkipBuild) { Warn 'binaries reused from target\release (-SkipBuild)' }

$exe = Join-Path $repo 'target\release\zipnest-native.exe'
$dll = Join-Path $repo 'target\release\zipnest_shell.dll'

# The console-subsystem incident (v0.2.1-0.2.3) came from shipping a build that
# pops a black window. Read the PE header rather than trusting the target dir.
$bytes = [System.IO.File]::ReadAllBytes($exe)
$peOff = [BitConverter]::ToInt32($bytes, 0x3C)
$subsystem = [BitConverter]::ToUInt16($bytes, $peOff + 24 + 68)
if ($subsystem -ne 2) { Fail "release exe subsystem is $subsystem, expected 2 (GUI)" }
Write-Host '  zipnest.exe subsystem = GUI (2)'
if (-not (Test-Path -LiteralPath $dll)) { Fail 'zipnest_shell.dll was not built' }

# --------------------------------------------------------------- 4. payload
Step 'emit portable payload (packaging\emit-portable.ps1)'
if ($SkipPayload) {
    Warn 'payload reused from dist-portable (-SkipPayload)'
} else {
    $ep = Invoke-Native powershell @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $repo 'packaging\emit-portable.ps1'))
    $ep.Output | ForEach-Object { Write-Host "  $_" }
    if ($ep.Code -ne 0) { Fail 'emit-portable.ps1 failed' }
}

$portable = Join-Path $repo 'dist-portable\ZipNest'
$payloadExe = Join-Path $portable 'zipnest.exe'
if (-not (Test-Path -LiteralPath $payloadExe)) { Fail "payload has no zipnest.exe" }
if ((Get-FileHash $payloadExe).Hash -ne (Get-FileHash $exe).Hash) {
    Fail 'payload zipnest.exe is not the release build just produced'
}

# ------------------------------------------------------------------ 5. NSIS
Step 'build the installer (makensis)'
$makensis = Join-Path $env:LOCALAPPDATA 'tauri\NSIS\makensis.exe'
if (-not (Test-Path -LiteralPath $makensis)) { Fail "makensis not found at $makensis" }
# installer.nsi must keep its UTF-8 BOM or makensis aborts with "Bad text encoding".
$nsi = Join-Path $repo 'packaging\installer.nsi'
$nsiBytes = [System.IO.File]::ReadAllBytes($nsi)
if (-not ($nsiBytes.Length -ge 3 -and $nsiBytes[0] -eq 0xEF -and $nsiBytes[1] -eq 0xBB -and $nsiBytes[2] -eq 0xBF)) {
    Warn 'installer.nsi has no UTF-8 BOM (makensis may abort)'
}
# Run from packaging\: installer.nsi resolves its File paths relative to the
# script directory, and a bare filename from the repo root means makensis cannot
# find the script at all.
$pkgDir = Join-Path $repo 'packaging'
Push-Location -LiteralPath $pkgDir
try {
    $ms = Invoke-Native $makensis @('/V2', 'installer.nsi')
} finally {
    Pop-Location
}
$ms.Output | Select-String 'error|warning|Output:|Can''t open' | ForEach-Object { Write-Host "  $_" }
if ($ms.Code -ne 0) {
    $ms.Output | Select-Object -Last 15 | ForEach-Object { Write-Host "  $_" }
    Fail "makensis failed (exit $($ms.Code))"
}

$installer = Join-Path $repo ("packaging\ZipNest_{0}_x64-setup.exe" -f $Version)
if (-not (Test-Path -LiteralPath $installer)) { Fail "installer not found at $installer" }
Write-Host ("  installer  {0:N0} bytes" -f (Get-Item -LiteralPath $installer).Length)

# -------------------------------------------------------------- 6. portable zip
Step 'compress the portable zip'
# The zip lives at the repo root: /ZipNest_*.zip is gitignored there, while a
# copy under packaging\ was not, so it showed up as an untracked file.
$zip = Join-Path $repo ("ZipNest_{0}_x64_portable.zip" -f $Version)
if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
Compress-Archive -Path $portable -DestinationPath $zip -CompressionLevel Optimal
Write-Host ("  portable   {0:N0} bytes" -f (Get-Item -LiteralPath $zip).Length)

# ---------------------------------------------------------- 7. installed copy
if (-not $SkipInstalledCopy) {
    Step "install on this machine ($InstalledExe)"
    $running = Get-Process -Name 'zipnest' -ErrorAction SilentlyContinue
    if ($running) {
        Write-Host ("  closing {0} running instance(s)" -f @($running).Count)
        $running | ForEach-Object { try { $_.CloseMainWindow() | Out-Null } catch { } }
        $deadline = (Get-Date).AddSeconds(10)
        while ((Get-Process -Name 'zipnest' -ErrorAction SilentlyContinue) -and (Get-Date) -lt $deadline) {
            Start-Sleep -Milliseconds 200
        }
        Get-Process -Name 'zipnest' -ErrorAction SilentlyContinue | ForEach-Object { try { $_.Kill() } catch { } }
        Start-Sleep -Milliseconds 300
    }
    $destDir = Split-Path -Parent $InstalledExe
    if (-not (Test-Path -LiteralPath $destDir)) { New-Item -ItemType Directory -Force -Path $destDir | Out-Null }
    Copy-Item -LiteralPath $exe -Destination $InstalledExe -Force
    if ((Get-FileHash $InstalledExe).Hash -ne (Get-FileHash $exe).Hash) {
        Fail "installed copy at $InstalledExe does not match the build"
    }
    Write-Host "  replaced $InstalledExe"
}

# ---------------------------------------------------------------- 8. publish
if (-not $Publish) {
    Write-Host ''
    Write-Host 'Artifacts are ready; nothing was tagged, pushed or uploaded.' -ForegroundColor Green
    Write-Host ("  {0}" -f $installer)
    Write-Host ("  {0}" -f $zip)
    Write-Host 'Re-run with -Publish once the build has been looked at.'
    exit 0
}

Step 'release notes from CHANGELOG.md'
$changelogPath = Join-Path $repo 'CHANGELOG.md'
$lines = [System.IO.File]::ReadAllText($changelogPath).TrimStart([char]0xFEFF) -split "`r?`n"
$start = -1
for ($i = 0; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -match "^##\s+v$([regex]::Escape($Version))\b") { $start = $i; break }
}
if ($start -lt 0) { Fail "CHANGELOG.md has no '## v$Version' section" }
$end = $lines.Count
for ($i = $start + 1; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -match '^##\s+v[0-9]') { $end = $i; break }
}
# The first heading line repeats the version and the summary the release page
# already shows, so the body starts at the first ### section.
$body = $lines[($start + 1)..($end - 1)]
while ($body.Count -gt 0 -and $body[0].Trim() -eq '') { $body = $body[1..($body.Count - 1)] }
if ($body.Count -eq 0) { Fail "CHANGELOG section for v$Version is empty" }
$notesPath = Join-Path $env:TEMP ("zipnest-notes-$Version.md")
[System.IO.File]::WriteAllText($notesPath, ($body -join "`r`n"), (New-Object System.Text.UTF8Encoding($false)))
Write-Host "  notes: $notesPath ($($body.Count) lines)"

Step "commit, tag and push v$Version"
# The tree is committed by the caller when -Publish is used (see the dirty-tree
# check in preflight), so this step only tags and pushes. It used to
# `git add -A; git commit --allow-empty`, which on an already-committed tree
# made a *second* empty commit with the same subject as the real release commit
# - that happened during v0.4.39 and put two identical "release: v0.4.39"
# entries in the history. Commit first, then publish.
$head = & git log -1 --format=%s
if ($head -ne "release: v$Version") {
    Fail "HEAD is '$head', expected 'release: $Version'. Stage and commit the release first: git add -A; git commit -m `"release: v$Version`""
}
& git tag -f -a "v$Version" -m "ZipNest v$Version"
if ($LASTEXITCODE -ne 0) { Fail 'git tag failed' }
& git push origin HEAD | ForEach-Object { Write-Host "  $_" }
& git push --force origin "+refs/tags/v$Version" | ForEach-Object { Write-Host "  $_" }
if ($LASTEXITCODE -ne 0) { Fail 'tag push failed' }

Step "gh release create v$Version"
# An existing release for this tag means a re-cut: delete it first, which also
# removes the tag (--cleanup-tag), so the force-pushed tag is the one uploaded.
#
# `gh release view` on a tag that has no release yet writes "release not
# found" to **stderr**, and PowerShell 5.1 under $ErrorActionPreference='Stop'
# turns a native command's stderr into a terminating error. That aborted the
# v0.4.39 publish *after* the tag was pushed and before the release existed,
# so the check is run with a local 'Continue' preference instead of `2>$null`
# (which only silences the stream, not the error it raises).
$hadRelease = $false
$prev = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
try {
    $existing = & gh release view "v$Version" --json tagName -q .tagName 2>&1
    $hadRelease = ($LASTEXITCODE -eq 0 -and $existing)
} finally {
    $ErrorActionPreference = $prev
}
if ($hadRelease) {
    Write-Host "  re-cutting: deleting the existing release v$Version"
    & gh release delete "v$Version" --yes --cleanup-tag | ForEach-Object { Write-Host "  $_" }
    & git tag -f -a "v$Version" -m "ZipNest v$Version" | Out-Null
    & git push --force origin "+refs/tags/v$Version" | Out-Null
}
# Title is exactly "ZipNest v<ver>" - the version plus the summary is a duplicate.
& gh release create "v$Version" $installer $zip --title "ZipNest v$Version" --notes-file $notesPath | ForEach-Object { Write-Host "  $_" }
if ($LASTEXITCODE -ne 0) { Fail 'gh release create failed' }

Step 'clean up local installers'
# The maintainer does not keep installers or portable zips on disk.
Remove-Item -LiteralPath $installer -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $zip -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $notesPath -Force -ErrorAction SilentlyContinue
Write-Host '  removed the local installer, portable zip and notes file'

Write-Host ''
Write-Host ("Released v{0}: https://github.com/CN6/zipnest/releases/tag/v{0}" -f $Version) -ForegroundColor Green
