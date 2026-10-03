#Requires -Version 5.1
<#
.SYNOPSIS
    Removes the ZipNest Windows 11 context-menu integration for the current user.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

$pkg = Get-AppxPackage -Name 'ZipNest.Shell' -ErrorAction SilentlyContinue
if (-not $pkg) {
    Write-Host 'ZipNest.Shell is not registered.'
    return
}

$pkg | Remove-AppxPackage
Write-Host 'Removed ZipNest.Shell. Restart Explorer to drop the menu entry.'
