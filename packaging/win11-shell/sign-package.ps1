#Requires -Version 5.1
<#
.SYNOPSIS
    Signs a ZipNest MSIX package and exports the matching public certificate.

.DESCRIPTION
    Signs ZipNestShell.msix with signtool and writes the public certificate to
    <CerOut> so the installer can ship and trust it.

    Default (self-signed, free): uses a code-signing certificate whose subject
    matches the manifest Publisher ("CN=ZipNest"). It is created in
    Cert:\CurrentUser\My and reused on subsequent builds (persistent identity).
    The installer then imports the public .cer into LocalMachine\TrustedPeople on
    the user's machine, so distribution needs no purchased certificate and no
    Developer Mode. Trade-off: the publisher is not recognized by SmartScreen.

    Optionally pass -PfxPath/-PfxPassword or -Thumbprint to use a specific
    certificate.

.PARAMETER Msix
    Path to the .msix to sign.

.PARAMETER PfxPath
    Optional .pfx with a private key (kept secret; not shipped).

.PARAMETER PfxPassword
    Password for -PfxPath.

.PARAMETER Thumbprint
    Optional thumbprint of a code-signing certificate in Cert:\CurrentUser\My.

.PARAMETER Subject
    Certificate subject to create/look up when none of the above is given.

.PARAMETER CerOut
    Where to write the public certificate. Default: <msix dir>\ZipNestCodesign.cer.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Msix,
    [string]$PfxPath,
    [string]$PfxPassword,
    [string]$Thumbprint,
    [string]$Subject = 'CN=ZipNest',
    [string]$CerOut
)

$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $Msix)) { throw "package not found: $Msix" }
if (-not $CerOut) { $CerOut = Join-Path (Split-Path -Parent $Msix) 'ZipNestCodesign.cer' }

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
    $x = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($PfxPath, $PfxPassword)
    [System.IO.File]::WriteAllBytes($CerOut, $x.Export([System.Security.Cryptography.X509Certificates.X509ContentType]::Cert))
    if ($PfxPassword) {
        & $signtool sign /fd SHA256 /f $PfxPath /p $PfxPassword $Msix
    } else {
        & $signtool sign /fd SHA256 /f $PfxPath $Msix
    }
    if ($LASTEXITCODE -ne 0) { throw "signtool failed ($LASTEXITCODE)" }
    Write-Host "Signed (pfx): $Msix"
    Write-Host "Public cert: $CerOut"
    return
}

if (-not $Thumbprint) {
    $cert = Get-ChildItem Cert:\CurrentUser\My -ErrorAction SilentlyContinue |
        Where-Object { $_.Subject -eq $Subject -and $_.HasPrivateKey } |
        Sort-Object NotAfter -Descending | Select-Object -First 1
    if (-not $cert) {
        # Long validity so the shipped package keeps installing/updating.
        $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject $Subject `
            -CertStoreLocation Cert:\CurrentUser\My -HashAlgorithm SHA256 `
            -NotAfter (Get-Date).AddYears(10)
    }
    $Thumbprint = $cert.Thumbprint
} else {
    $cert = Get-ChildItem Cert:\CurrentUser\My -ErrorAction SilentlyContinue |
        Where-Object { $_.Thumbprint -eq $Thumbprint } | Select-Object -First 1
    if (-not $cert) { throw "certificate $Thumbprint not found in Cert:\CurrentUser\My" }
}

Export-Certificate -Cert $cert -FilePath $CerOut -Force | Out-Null
& $signtool sign /fd SHA256 /sha1 $Thumbprint $Msix
if ($LASTEXITCODE -ne 0) { throw "signtool failed ($LASTEXITCODE)" }
Write-Host "Signed (thumbprint $Thumbprint): $Msix"
Write-Host "Public cert: $CerOut"
