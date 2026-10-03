#Requires -Version 5.1
<#
.SYNOPSIS
    Registers the ZipNest Windows 11 context-menu integration for the current user.

.DESCRIPTION
    Registers the "ZipNest.Shell" package so Explorer shows the
    "添加到压缩包…" verb in the Windows 11 modern context menu.

    Preferred: a signed sparse package (Win11Shell\ZipNestShell.msix), installed
    with Add-AppxPackage -Path and the install dir as external location.
    Fallback (developer mode only): the loose Win11Shell\AppxManifest.xml.

    Nothing machine-wide is touched: no manual HKLM writes, no classic-menu
    toggle. Only a per-user package is registered and Remove-AppxPackage removes it.

.PARAMETER InstallDir
    Absolute path to the folder holding zipnest.exe and zipnest_shell.dll.

.PARAMETER PackageDir
    Folder holding ZipNestShell.msix (or AppxManifest.xml). Defaults to
    <InstallDir>\Win11Shell.
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

$msix = Join-Path $PackageDir 'ZipNestShell.msix'
$manifest = Join-Path $PackageDir 'AppxManifest.xml'

# Replace any previous registration (same version cannot be added twice).
Get-AppxPackage -Name 'ZipNest.Shell' -ErrorAction SilentlyContinue | Remove-AppxPackage -ErrorAction SilentlyContinue

if (Test-Path -LiteralPath $msix) {
    Add-AppxPackage -Path $msix -ExternalLocation $InstallDir
    Write-Host 'Registered ZipNest.Shell from the signed package.'
} elseif (Test-Path -LiteralPath $manifest) {
    Add-AppxPackage -Register $manifest -ExternalLocation $InstallDir
    Write-Host 'Registered ZipNest.Shell from the loose manifest (developer mode).'
} else {
    throw "neither ZipNestShell.msix nor AppxManifest.xml found in $PackageDir"
}

Write-Host 'Restart Explorer if the menu does not appear. Uninstall with: .\uninstall.ps1'
