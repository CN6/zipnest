#Requires -Version 5.1
<#
.SYNOPSIS
    Fails when a version number appears somewhere and disagrees with the rest.

.DESCRIPTION
    Every shipped artifact must carry the same version, and two of them were
    silently stale for ten releases (the Win11 package manifest stayed at
    0.4.0.0 from v0.4.0 to v0.4.10; UPDATE_UA stayed at 0.4.0). This is the
    check that makes that impossible to miss, and it runs in CI.

    Source of truth:
      Cargo.toml                        [workspace.package].version

    Everything else is derived from it, or compared against it:
      crates/*/Cargo.toml               version.workspace = true (must not type one)
      apps/zipnest-native/src/main.rs   env!("CARGO_PKG_VERSION") (no typed copy)
      packaging/installer.nsi           !define VERSION, OutFile name
      packaging/win11-shell/AppxManifest.xml   Version="x.y.z.0"
      README.md                         "当前 v x.y.z" + both artifact names
      CHANGELOG.md                      newest "## v x.y.z" heading

.EXAMPLE
    powershell -File packaging\check-versions.ps1
#>
[CmdletBinding()]
param(
    [string]$RepoRoot
)

$ErrorActionPreference = 'Stop'

if (-not $RepoRoot) {
    $here = Split-Path -Parent $MyInvocation.MyCommand.Path
    $RepoRoot = (Resolve-Path (Join-Path $here '..')).Path
}
$repo = (Resolve-Path -LiteralPath $RepoRoot).Path

$problems = New-Object System.Collections.Generic.List[string]

function Read-Utf8([string]$path) {
    # Read the bytes and decode explicitly: these files mix UTF-8 with and
    # without a BOM, and PowerShell 5.1's default (ANSI) would mangle Chinese.
    $bytes = [System.IO.File]::ReadAllBytes($path)
    $text = [System.Text.Encoding]::UTF8.GetString($bytes)
    return $text.TrimStart([char]0xFEFF)
}

function Report([string]$what, [string]$got, [string]$want) {
    if ($got -ne $want) {
        $problems.Add("$what is '$got', expected '$want'")
    } else {
        Write-Host ("  ok   {0,-46} {1}" -f $what, $got)
    }
}

$cargoToml = Join-Path $repo 'Cargo.toml'
$nsi = Join-Path $repo 'packaging\installer.nsi'
$manifest = Join-Path $repo 'packaging\win11-shell\AppxManifest.xml'
$readme = Join-Path $repo 'README.md'
$changelog = Join-Path $repo 'CHANGELOG.md'

# The workspace manifest is the only place a version may be typed. Members
# inherit it with version.workspace = true and main.rs reads it at compile time,
# so there is no second copy that can drift.
$cargoText = Read-Utf8 $cargoToml
if ($cargoText -notmatch '(?ms)^\[workspace\.package\]\s*(.*?)(?=^\[|\Z)') {
    throw "[workspace.package] not found in $cargoToml"
}
$workspacePackage = $Matches[1]
if ($workspacePackage -notmatch '(?m)^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"') {
    throw "[workspace.package].version not found in $cargoToml"
}
$version = $Matches[1]
Write-Host "Version: $version"

$nsiText = Read-Utf8 $nsi
if ($nsiText -notmatch '!define VERSION "([0-9]+\.[0-9]+\.[0-9]+)"') { throw "VERSION not found in installer.nsi" }
Report 'installer.nsi !define VERSION' $Matches[1] $version

if ($nsiText -notmatch 'OutFile "ZipNest_([0-9]+\.[0-9]+\.[0-9]+)_x64-setup\.exe"') {
    throw "OutFile not found in installer.nsi"
}
Report 'installer.nsi OutFile' $Matches[1] $version

$manifestText = Read-Utf8 $manifest
if ($manifestText -notmatch 'Version="([0-9]+\.[0-9]+\.[0-9]+)\.[0-9]+"') { throw "Version not found in AppxManifest.xml" }
Report 'AppxManifest Version (4-part)' $Matches[1] $version

$readmeText = Read-Utf8 $readme
$readmeVersions = [regex]::Matches($readmeText, 'ZipNest_([0-9]+\.[0-9]+\.[0-9]+)_x64-') |
    ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique
if ($readmeVersions.Count -eq 0) { $problems.Add('README.md names no release artifact') }
else {
    foreach ($v in $readmeVersions) { Report 'README artifact name' $v $version }
}
if ($readmeText -notmatch "\*\*v([0-9]+\.[0-9]+\.[0-9]+)\*\*") {
    $problems.Add('README.md has no "**v x.y.z**" line')
} else {
    Report 'README current version' $Matches[1] $version
}

$changelogText = Read-Utf8 $changelog
if ($changelogText -notmatch '(?m)^## v([0-9]+\.[0-9]+\.[0-9]+)') { throw "no version heading in CHANGELOG.md" }
Report 'CHANGELOG newest heading' $Matches[1] $version

# main.rs must not carry a version of its own: it reads what Cargo.toml feeds the
# crate. UPDATE_UA is derived from it as well (since v0.4.11), so a typed copy in
# either place is a problem.
$mainRs = Join-Path $repo 'apps\zipnest-native\src\main.rs'
$mainText = Read-Utf8 $mainRs
if ($mainText -match 'const CURRENT_VERSION: &str = "') {
    $problems.Add('main.rs hard-codes CURRENT_VERSION; it must be env!("CARGO_PKG_VERSION")')
} elseif ($mainText -match 'const UPDATE_UA: &str = "ZipNest-Updater/([0-9]+\.[0-9]+\.[0-9]+)"') {
    $problems.Add("UPDATE_UA is hand-typed ($($Matches[1])); it must be derived from CURRENT_VERSION")
} else {
    Write-Host ("  ok   {0,-46} {1}" -f 'main.rs CURRENT_VERSION', 'from CARGO_PKG_VERSION')
}

# A member crate that types its own version is how the drift started
# (zipnest-shell sat at 0.3.0 while everything else said 0.4.x).
$memberManifests = @(
    'crates\archive-core\Cargo.toml',
    'crates\archive-security\Cargo.toml',
    'crates\archive-jobs\Cargo.toml',
    'crates\zipnest-ipc\Cargo.toml',
    'apps\zipnest-native\Cargo.toml',
    'apps\zipnest-shell\Cargo.toml'
)
foreach ($rel in $memberManifests) {
    $memberText = Read-Utf8 (Join-Path $repo $rel)
    $crateName = Split-Path (Split-Path $rel -Parent) -Leaf
    if ($memberText -match '(?m)^version\s*=\s*"') {
        $problems.Add("$rel types its own version; it must be version.workspace = true")
    } elseif ($memberText -match '(?m)^version\.workspace\s*=\s*true') {
        Write-Host ("  ok   {0,-46} {1}" -f "crate $crateName version", 'workspace')
    } else {
        $problems.Add("$rel has no version field; expected version.workspace = true")
    }
}

if ($problems.Count -gt 0) {
    Write-Host ''
    Write-Host 'Version mismatch:' -ForegroundColor Red
    foreach ($p in $problems) { Write-Host "  - $p" -ForegroundColor Red }
    exit 1
}
Write-Host ''
Write-Host 'All version strings agree.' -ForegroundColor Green
exit 0
