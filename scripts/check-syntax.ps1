#Requires -Version 5.1
# Parse every script in scripts\ without running it. Used as a pre-commit check.
$ErrorActionPreference = 'Stop'
$dir = Split-Path -Parent $MyInvocation.MyCommand.Path
$bad = 0
Get-ChildItem -Path $dir -Filter '*.ps1' | ForEach-Object {
    $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($_.FullName, [ref]$null, [ref]$errors)
    if ($errors -and $errors.Count -gt 0) {
        $bad++
        Write-Host ("PARSE FAIL {0}" -f $_.Name) -ForegroundColor Red
        $errors | ForEach-Object { Write-Host ("  line {0}: {1}" -f $_.Extent.StartLineNumber, $_.Message) }
    } else {
        Write-Host ("ok         {0}" -f $_.Name)
    }
}
if ($bad -gt 0) { exit 1 }
