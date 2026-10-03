#Requires -Version 5.1
<#
.SYNOPSIS
    Removes the ZipNest Windows 11 context-menu integration for the current user.

.DESCRIPTION
    Removes the per-user "ZipNest.Shell" package and, if the installer shipped a
    self-signed certificate, the matching certificate from
    Cert:\LocalMachine\TrustedPeople (requires an elevated shell).

.PARAMETER PackageDir
    Folder that contains the shipped ZipNestCodesign.cer (optional).
#>
[CmdletBinding()]
param([string]$PackageDir)

$ErrorActionPreference = 'Stop'

$pkg = Get-AppxPackage -Name 'ZipNest.Shell' -ErrorAction SilentlyContinue
if ($pkg) {
    $pkg | Remove-AppxPackage
    Write-Host 'Removed ZipNest.Shell.'
} else {
    Write-Host 'ZipNest.Shell is not registered.'
}

if ($PackageDir) {
    $cer = Join-Path $PackageDir 'ZipNestCodesign.cer'
    if (Test-Path -LiteralPath $cer) {
        $thumb = (New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($cer)).Thumbprint
        Get-ChildItem Cert:\LocalMachine\TrustedPeople -ErrorAction SilentlyContinue |
            Where-Object { $_.Thumbprint -eq $thumb } |
            Remove-Item -ErrorAction SilentlyContinue
        Write-Host "Removed trusted certificate $thumb."
    }
}

Write-Host 'Restart Explorer to drop the menu entry.'
