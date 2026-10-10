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

# Install (or upgrade) the package.
#
# This used to remove whatever was registered and then add the new one, so every
# update had a window where the Win11 context menu did not exist at all — and if
# the re-add failed (broken AppX deployment store, certificate import refused)
# the user simply lost the menu, with only a warning dialog to show for it.
#
# A higher manifest version upgrades in place, so try that first. The old
# remove-then-add path stays as the fallback, for the cases Windows refuses an
# in-place add: 0x80073D06 (a newer version is already installed) and 0x80073CFB
# (the package is already present with the same contents).
function Install-ZipNestShellPackage {
    param([string]$Path, [string]$ExternalLocation)
    if (-not $ExternalLocation) {
        Add-AppxPackage -Path $Path
        return
    }
    try {
        Add-AppxPackage -Path $Path -ExternalLocation $ExternalLocation
        Write-Host 'Installed the shell package in place (upgrade).'
    } catch {
        $code = $_.Exception.HResult
        Write-Host ("In-place install failed (0x{0:X8}); removing the old registration and retrying." -f $code)
        Get-AppxPackage -Name 'ZipNest.Shell' -ErrorAction SilentlyContinue |
            Remove-AppxPackage -ErrorAction SilentlyContinue
        Add-AppxPackage -Path $Path -ExternalLocation $ExternalLocation
        Write-Host 'Installed the shell package after replacing the old registration.'
    }
}

if (Test-Path -LiteralPath $msix) {
    # Trust the shipped self-signed certificate (public key only) so the signed
    # package installs without Developer Mode. Needs an elevated shell.
    $cer = Join-Path $PackageDir 'ZipNestCodesign.cer'
    if (Test-Path -LiteralPath $cer) {
        try {
            Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\TrustedPeople | Out-Null
        } catch {
            throw "Could not trust the signing certificate (administrator required): $($_.Exception.Message)"
        }
    }
    Install-ZipNestShellPackage -Path $msix -ExternalLocation $InstallDir
} elseif (Test-Path -LiteralPath $manifest) {
    Get-AppxPackage -Name 'ZipNest.Shell' -ErrorAction SilentlyContinue |
        Remove-AppxPackage -ErrorAction SilentlyContinue
    Add-AppxPackage -Register $manifest -ExternalLocation $InstallDir
    Write-Host 'Registered ZipNest.Shell from the loose manifest (developer mode).'
} else {
    throw "neither ZipNestShell.msix nor AppxManifest.xml found in $PackageDir"
}

Write-Host 'Restart Explorer if the menu does not appear. Uninstall with: .\uninstall.ps1'
