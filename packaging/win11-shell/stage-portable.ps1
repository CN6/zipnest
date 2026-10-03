#Requires -Version 5.1
<#
.SYNOPSIS
    Stages the Windows 11 shell integration into the portable/installer payload.

.DESCRIPTION
    Builds apps/zipnest-shell (release) and lays out everything the NSIS
    installer needs inside the portable directory (default: dist-portable\ZipNest):

        zipnest_shell.dll
        Win11Shell\AppxManifest.xml
        Win11Shell\install.ps1
        Win11Shell\uninstall.ps1
        Win11Shell\Assets\*.png

    Run this before makensis so packaging\installer.nsi can find those files.

.PARAMETER PortableDir
    Target portable directory. Defaults to <repo>\dist-portable\ZipNest.
#>
[CmdletBinding()]
param([string]$PortableDir)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = (Resolve-Path (Join-Path $here '..\..')).Path
if (-not $PortableDir) { $PortableDir = Join-Path $repo 'dist-portable\ZipNest' }
if (-not (Test-Path -LiteralPath $PortableDir)) {
    throw "portable dir not found: $PortableDir (build/emit the portable first)"
}

Push-Location $repo
try {
    cargo build -p zipnest-shell --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
} finally {
    Pop-Location
}

Copy-Item (Join-Path $repo 'target\release\zipnest_shell.dll') $PortableDir -Force

$dst = Join-Path $PortableDir 'Win11Shell'
Remove-Item $dst -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $dst -Force | Out-Null
Copy-Item (Join-Path $here 'AppxManifest.xml') $dst
Copy-Item (Join-Path $here 'install.ps1') $dst
Copy-Item (Join-Path $here 'uninstall.ps1') $dst
Copy-Item (Join-Path $here 'Assets') (Join-Path $dst 'Assets') -Recurse

Write-Host "Staged Windows 11 shell integration into $PortableDir"
