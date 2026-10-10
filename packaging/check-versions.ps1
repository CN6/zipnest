#Requires -Version 5.1
<#
.SYNOPSIS
    Fails when a version number appears somewhere and disagrees with the rest.

.DESCRIPTION
    Each release has to keep six places in sync, and two of them were silently
    stale for ten releases (the Win11 package manifest stayed at 0.4.0.0 from
    v0.4.0 to v0.4.10; UPDATE_UA stayed at 0.4.0). This is the check that makes
    that impossible to miss, and it runs in CI.

    Sources of truth:
      apps/zipnest-native/src/main.rs   const CURRENT_VERSION = "x.y.z"
      packaging/installer.nsi           !define VERSION, OutFile name
      packaging/win11-shell/AppxManifest.xml   Version="x.y.z.0"
      README.md                         "当前 v x.y.z" + both artifact names
      CHANGELOG.md                      newest "## v x.y.z" heading
      apps/zipnest-native/src/main.rs   UPDATE_UA  (should be derived, not typed)

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

$mainRs = Join-Path $repo 'apps\zipnest-native\src\main.rs'
$nsi = Join-Path $repo 'packaging\installer.nsi'
$manifest = Join-Path $repo 'packaging\win11-shell\AppxManifest.xml'
$readme = Join-Path $repo 'README.md'
$changelog = Join-Path $repo 'CHANGELOG.md'

$mainText = Read-Utf8 $mainRs
if ($mainText -notmatch 'const CURRENT_VERSION: &str = "([0-9]+\.[0-9]+\.[0-9]+)"') {
    throw "CURRENT_VERSION not found in $mainRs"
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

# UPDATE_UA is built from CURRENT_VERSION since v0.4.11; make sure nobody
# reintroduces a hand-typed copy.
if ($mainText -match 'const UPDATE_UA: &str = "ZipNest-Updater/([0-9]+\.[0-9]+\.[0-9]+)"') {
    $problems.Add("UPDATE_UA is hand-typed ($($Matches[1])); it must be derived from CURRENT_VERSION")
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
