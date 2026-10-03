#Requires -Version 5.1
<#
.SYNOPSIS
    Builds the ZipNest.Shell sparse MSIX package (Windows 11 modern context menu).

.DESCRIPTION
    Compiles apps/zipnest-shell (release) and packs a sparse MSIX that contains
    only AppxManifest.xml + Assets\*. The executable (zipnest.exe) and the shell
    extension DLL (zipnest_shell.dll) stay in the app's install directory and are
    linked at registration time with `Add-AppxPackage -ExternalLocation`.

    The package is intentionally left unsigned; register it with -AllowUnsigned
    (Developer Mode). No certificate, no HKLM.

.PARAMETER OutDir
    Output directory (default: <this folder>\out).
#>
[CmdletBinding()]
param(
    [string]$OutDir,
    # Sign the produced .msix (dev self-signed cert by default; see sign-package.ps1).
    [switch]$Sign,
    [string]$PfxPath,
    [string]$PfxPassword,
    [string]$Thumbprint
)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = (Resolve-Path (Join-Path $here '..\..')).Path
if (-not $OutDir) { $OutDir = Join-Path $here 'out' }

function Find-MakeAppx {
    $root = 'C:\Program Files (x86)\Windows Kits\10\bin'
    # @(...) keeps this an array even for a single SDK version; otherwise
    # $candidates[0] would return the first *character* of the path.
    $candidates = @(Get-ChildItem $root -Directory -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending |
        ForEach-Object { Join-Path $_.FullName 'x64\makeappx.exe' } |
        Where-Object { Test-Path $_ })
    if ($candidates.Count -eq 0) { throw "makeappx.exe not found under $root" }
    return $candidates[0]
}

Write-Host 'Building zipnest-shell (release)...'
Push-Location $repo
try {
    cargo build -p zipnest-shell --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
} finally {
    Pop-Location
}

$stage = Join-Path $OutDir 'pkg'
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $stage -Force | Out-Null
Copy-Item (Join-Path $here 'AppxManifest.xml') $stage
Copy-Item (Join-Path $here 'Assets') (Join-Path $stage 'Assets') -Recurse

$msix = Join-Path $OutDir 'ZipNestShell.msix'
Remove-Item $msix -Force -ErrorAction SilentlyContinue

$makeappx = Find-MakeAppx
& $makeappx pack /o /nv /d $stage /p $msix
if ($LASTEXITCODE -ne 0) { throw "makeappx failed ($LASTEXITCODE)" }

if ($Sign) {
    & (Join-Path $here 'sign-package.ps1') -Msix $msix -PfxPath $PfxPath -PfxPassword $PfxPassword -Thumbprint $Thumbprint
    if ($LASTEXITCODE -ne 0) { throw "sign-package failed ($LASTEXITCODE)" }
}

Write-Host "Built $msix"
Write-Host "Register with:  .\install.ps1 -InstallDir <app install dir>"
