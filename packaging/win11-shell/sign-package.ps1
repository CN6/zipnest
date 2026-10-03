#Requires -Version 5.1
<#
.SYNOPSIS
    Signs a ZipNest MSIX package.

.DESCRIPTION
    Signs ZipNestShell.msix with signtool.

    Default (development): uses a self-signed code-signing certificate whose
    subject matches the manifest Publisher ("CN=ZipNest"). The certificate is
    created in Cert:\CurrentUser\My and its public key is trusted in
    Cert:\LocalMachine\TrustedPeople, so `Add-AppxPackage -Path` accepts it on
    this machine. AppX checks the machine store, so this step needs an elevated
    shell.

    Production: pass -PfxPath/-PfxPassword or -Thumbprint with a CA-issued
    code-signing certificate. No trust import is performed in that case.

.PARAMETER Msix
    Path to the .msix to sign.

.PARAMETER PfxPath
    Optional .pfx of a real code-signing certificate (production).

.PARAMETER PfxPassword
    Password for -PfxPath.

.PARAMETER Thumbprint
    Optional thumbprint of a code-signing certificate in Cert:\CurrentUser\My.

.PARAMETER Subject
    Certificate subject to create/look up when none of the above is given.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Msix,
    [string]$PfxPath,
    [string]$PfxPassword,
    [string]$Thumbprint,
    [string]$Subject = 'CN=ZipNest'
)

$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $Msix)) { throw "package not found: $Msix" }

function Find-SignTool {
    $root = 'C:\Program Files (x86)\Windows Kits\10\bin'
    $candidates = @(Get-ChildItem $root -Directory -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending |
        ForEach-Object { Join-Path $_.FullName 'x64\signtool.exe' } |
        Where-Object { Test-Path $_ })
    if ($candidates.Count -eq 0) { throw "signtool.exe not found under $root" }
    return $candidates[0]
}
$signtool = Find-SignTool

if ($PfxPath) {
    if (-not (Test-Path -LiteralPath $PfxPath)) { throw "pfx not found: $PfxPath" }
    if ($PfxPassword) {
        & $signtool sign /fd SHA256 /f $PfxPath /p $PfxPassword $Msix
    } else {
        & $signtool sign /fd SHA256 /f $PfxPath $Msix
    }
    if ($LASTEXITCODE -ne 0) { throw "signtool failed ($LASTEXITCODE)" }
    Write-Host "Signed (pfx): $Msix"
    return
}

if (-not $Thumbprint) {
    $cert = Get-ChildItem Cert:\CurrentUser\My -ErrorAction SilentlyContinue |
        Where-Object { $_.Subject -eq $Subject -and $_.HasPrivateKey } |
        Sort-Object NotAfter -Descending | Select-Object -First 1
    if (-not $cert) {
        $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject $Subject `
            -CertStoreLocation Cert:\CurrentUser\My -HashAlgorithm SHA256
    }
    $Thumbprint = $cert.Thumbprint

    # Trust the public cert so Add-AppxPackage -Path accepts the package. AppX
    # checks the machine store; per-user TrustedPeople is not enough (0x800B0109),
    # so this step needs an elevated shell.
    $cer = Join-Path $env:TEMP 'zipnest-dev-codesign.cer'
    Export-Certificate -Cert $cert -FilePath $cer -Force | Out-Null
    Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\TrustedPeople | Out-Null
}

& $signtool sign /fd SHA256 /sha1 $Thumbprint $Msix
if ($LASTEXITCODE -ne 0) { throw "signtool failed ($LASTEXITCODE)" }
Write-Host "Signed (thumbprint $Thumbprint): $Msix"
