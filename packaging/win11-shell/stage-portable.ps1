#Requires -Version 5.1
<#
.SYNOPSIS
    Stages the signed Windows 11 shell integration into the portable/installer payload.

.DESCRIPTION
    Builds apps/zipnest-shell, packs and signs Win11Shell\ZipNestShell.msix, and
    lays out the files the NSIS installer needs inside the portable directory
    (default: dist-portable\ZipNest):

        zipnest_shell.dll
        Assets\*.png
        Win11Shell\ZipNestShell.msix
        Win11Shell\install.ps1
        Win11Shell\uninstall.ps1

    Run this before makensis.

    Signing: production passes -PfxPath/-PfxPassword (or -Thumbprint) for a
    CA-issued code-signing certificate. Otherwise a self-signed dev certificate
    (subject CN=ZipNest) is created and trusted per-user.

.PARAMETER PortableDir
    Target portable directory. Defaults to <repo>\dist-portable\ZipNest.
#>
[CmdletBinding()]
param(
    [string]$PortableDir,
    [string]$PfxPath,
    [string]$PfxPassword,
    [string]$Thumbprint
)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = (Resolve-Path (Join-Path $here '..\..')).Path
if (-not $PortableDir) { $PortableDir = Join-Path $repo 'dist-portable\ZipNest' }
if (-not (Test-Path -LiteralPath $PortableDir)) {
    throw "portable dir not found: $PortableDir (build/emit the portable first)"
}

$payload = Join-Path $PortableDir 'Win11Shell'
Remove-Item $payload -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $payload -Force | Out-Null

& (Join-Path $here 'build-package.ps1') -OutDir $payload -Sign `
    -PfxPath $PfxPath -PfxPassword $PfxPassword -Thumbprint $Thumbprint
if ($LASTEXITCODE -ne 0) { throw "build-package failed ($LASTEXITCODE)" }

Copy-Item (Join-Path $repo 'target\release\zipnest_shell.dll') $PortableDir -Force
Copy-Item (Join-Path $here 'install.ps1') $payload
Copy-Item (Join-Path $here 'uninstall.ps1') $payload

# The package is registered with `-ExternalLocation <install dir>`, and the
# shell then resolves the manifest's relative `Assets\…` logo paths against that
# directory — NOT against the copy inside the .msix. Measured on Windows 11
# 26H1 (28020): with the assets only inside the package the taskbar button came
# out as Windows' generic grey square for every launch that carried package
# identity (i.e. every context-menu launch), while an unpackaged launch drew the
# exe's blue Z; dropping this same folder next to zipnest.exe turned the button
# blue immediately, no reinstall or Explorer restart needed.
# Removed first: `Copy-Item –Recurse` into an existing folder nests it one level
# deeper (Assets\Assets), which the payload listing showed on the second run.
$assets = Join-Path $PortableDir 'Assets'
Remove-Item $assets -Recurse -Force -ErrorAction SilentlyContinue
Copy-Item (Join-Path $here 'Assets') $assets -Recurse -Force


Write-Host "Staged signed Windows 11 shell integration into $PortableDir"
