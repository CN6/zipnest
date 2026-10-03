#Requires -Version 5.1
<#
.SYNOPSIS
    Registers the ZipNest Windows 11 context-menu integration for the current user.

.DESCRIPTION
    Registers the sparse "ZipNest.Shell" package so Explorer shows the
    "添加到压缩包…" verb in the Windows 11 modern context menu.

    The package manifest + Assets live in <InstallDir>\Win11Shell; the real
    binaries (zipnest.exe, zipnest_shell.dll) stay in <InstallDir> and are linked
    through the package's external location. Nothing machine-wide is touched:
    no HKLM, no certificate, no classic-menu toggle. Only a per-user package is
    registered and it is removed with `Remove-AppxPackage`.

    NOTE: an unsigned package cannot be installed with `Add-AppxPackage -Path`
    (that needs a trusted signing certificate). Developer Mode lets us register
    the loose manifest instead, which is what this script does.

.PARAMETER InstallDir
    Absolute path to the folder holding zipnest.exe and zipnest_shell.dll.

.PARAMETER PackageDir
    Folder holding AppxManifest.xml + Assets. Defaults to <InstallDir>\Win11Shell.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$InstallDir,
    [string]$PackageDir
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $InstallDir)) { throw "InstallDir not found: $InstallDir" }
$InstallDir = (Resolve-Path -LiteralPath $InstallDir).Path

foreach ($file in @('zipnest.exe', 'zipnest_shell.dll')) {
    if (-not (Test-Path -LiteralPath (Join-Path $InstallDir $file))) {
        throw "$file not found in $InstallDir"
    }
}

if (-not $PackageDir) { $PackageDir = Join-Path $InstallDir 'Win11Shell' }
if (-not (Test-Path -LiteralPath $PackageDir)) { throw "PackageDir not found: $PackageDir" }
$PackageDir = (Resolve-Path -LiteralPath $PackageDir).Path
$manifest = Join-Path $PackageDir 'AppxManifest.xml'
if (-not (Test-Path -LiteralPath $manifest)) { throw "AppxManifest.xml not found in $PackageDir" }

# Replace any previous registration (same version cannot be added twice).
Get-AppxPackage -Name 'ZipNest.Shell' -ErrorAction SilentlyContinue | Remove-AppxPackage -ErrorAction SilentlyContinue

Add-AppxPackage -Register $manifest -ExternalLocation $InstallDir
Write-Host "Registered ZipNest.Shell. Restart Explorer if the menu does not appear."
Write-Host "Uninstall with: .\uninstall.ps1"
