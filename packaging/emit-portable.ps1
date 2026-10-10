#Requires -Version 5.1
<#
.SYNOPSIS
    Builds dist-portable\ZipNest — the payload installer.nsi packages.

.DESCRIPTION
    Before v0.4.11 this directory was gitignored and nothing generated it: the
    installer's File list was satisfied by files the maintainer happened to have
    on disk. A clean checkout could not build an installer at all, and CI could
    not even syntax-check one.

    This assembles the whole payload from sources that ARE in the repo:

        dist-portable\ZipNest\zipnest.exe            target\release\zipnest-native.exe
        dist-portable\ZipNest\engines\7z.dll         vendor\7zip-bin\7z.dll
        dist-portable\ZipNest\engines\sfx\*.sfx      vendor\7zip-sfx\
        dist-portable\ZipNest\licenses\*.txt         vendor\ (see -ThirdParty)
        dist-portable\ZipNest\Win11Shell\*           packaging\win11-shell\stage-portable.ps1

    The Win11 shell package is built and signed by stage-portable.ps1 (it needs
    the code-signing certificate), so this script calls it. Pass -SkipWin11Shell
    to emit only the app + engine half, e.g. on a machine without the key.

.PARAMETER RepoRoot
    Repository root. Defaults to the parent of this script's folder.

.PARAMETER SkipWin11Shell
    Do not build/stage the Windows 11 shell package.

.PARAMETER NoThirdParty
    Skip generating licenses\THIRD-PARTY.txt from the local cargo registry. It is
    written by default because installer.nsi packages it, and a missing file
    abort the NSIS compile; this switch writes a short pointer file instead.

.EXAMPLE
    powershell -File packaging\emit-portable.ps1
#>
[CmdletBinding()]
param(
    [string]$RepoRoot,
    [switch]$SkipWin11Shell,
    [switch]$NoThirdParty
)

$ErrorActionPreference = 'Stop'

if (-not $RepoRoot) {
    $here = Split-Path -Parent $MyInvocation.MyCommand.Path
    $RepoRoot = (Resolve-Path (Join-Path $here '..')).Path
}
$repo = (Resolve-Path -LiteralPath $RepoRoot).Path
Write-Host "Repo: $repo"

$exe = Join-Path $repo 'target\release\zipnest-native.exe'
if (-not (Test-Path -LiteralPath $exe)) {
    throw "$exe not found. Run: cargo build -p zipnest-native --release"
}

$portable = Join-Path $repo 'dist-portable\ZipNest'
$engines = Join-Path $portable 'engines'
$sfx = Join-Path $engines 'sfx'
$licenses = Join-Path $portable 'licenses'
foreach ($dir in @($engines, $sfx, $licenses)) {
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
}

function Copy-Payload {
    param([string]$From, [string]$To)
    if (-not (Test-Path -LiteralPath $From)) { throw "missing source: $From" }
    Copy-Item -LiteralPath $From -Destination $To -Force
    Write-Host ("  {0,-38} <- {1}" -f (Split-Path -Leaf $To), $From)
}

# installer.nsi packages licenses\THIRD-PARTY.txt, and NSIS aborts on a `File` it
# cannot find, so the payload must always contain it - even when the license
# texts themselves cannot be collected.
function Write-PointerFile {
    $note = @(
        'The third-party license texts were not collected for this payload.',
        'They are reproducible from the repository:',
        '  packaging\emit-portable.ps1   writes this file from the local cargo registry',
        '  Cargo.lock                    lists the exact crate versions'
    ) -join "`r`n"
    [System.IO.File]::WriteAllText((Join-Path $licenses 'THIRD-PARTY.txt'), $note, (New-Object System.Text.UTF8Encoding($false)))
}

Copy-Payload $exe (Join-Path $portable 'zipnest.exe')
Copy-Payload (Join-Path $repo 'vendor\7zip-bin\7z.dll') (Join-Path $engines '7z.dll')
Copy-Payload (Join-Path $repo 'vendor\7zip-sfx\7z.sfx') (Join-Path $sfx '7z.sfx')
Copy-Payload (Join-Path $repo 'vendor\7zip-sfx\7zCon.sfx') (Join-Path $sfx '7zCon.sfx')
# The engine's own license (LGPL obligation) and the SFX stub's.
Copy-Payload (Join-Path $repo 'vendor\7zip-sdk\LICENSE') (Join-Path $licenses '7zip.txt')
Copy-Payload (Join-Path $repo 'vendor\7zip-sfx\LICENSE.txt') (Join-Path $licenses '7zip-sfx.txt')

if ($NoThirdParty) {
    Write-PointerFile
    Write-Host '  licenses\THIRD-PARTY.txt  <- pointer only (-NoThirdParty)'
} else {
    $registry = Join-Path $env:USERPROFILE '.cargo\registry\src'
    $lock = Join-Path $repo 'Cargo.lock'
    if (-not (Test-Path -LiteralPath $registry)) {
        Write-Warning "no cargo registry at $registry; writing a pointer file instead of the license texts"
        Write-PointerFile
    } elseif (-not (Test-Path -LiteralPath $lock)) {
        Write-Warning "no Cargo.lock; writing a pointer file instead of the license texts"
        Write-PointerFile
    } else {
        $out = Join-Path $licenses 'THIRD-PARTY.txt'
        $names = Select-String -Path $lock -Pattern '^name = "(.+)"$' |
            ForEach-Object { $_.Matches[0].Groups[1].Value } | Sort-Object -Unique
        # Most crates ship the same MIT/Apache boilerplate. Group identical texts
        # so the file stays a few hundred kilobytes instead of megabytes, while
        # every crate is still named.
        $byText = @{}
        $missing = 0
        foreach ($name in $names) {
            # The registry keeps each crate in <registry>\<index>\<name>-<version>,
            # so this has to search one level below the registry root.
            $dir = Get-ChildItem $registry -Directory -Recurse -Depth 1 -Filter "$name-*" -ErrorAction SilentlyContinue |
                Select-Object -First 1
            if (-not $dir) { $missing++; continue }
            $lic = Get-ChildItem $dir.FullName -File -ErrorAction SilentlyContinue |
                Where-Object { $_.Name -match '^(LICENSE|LICENCE)' } |
                Select-Object -First 1
            if (-not $lic) { $missing++; continue }
            $text = (Get-Content -LiteralPath $lic.FullName -Raw)
            $hash = (Get-FileHash -LiteralPath $lic.FullName -Algorithm SHA256).Hash
            if (-not $byText.ContainsKey($hash)) {
                $byText[$hash] = [pscustomobject]@{ Text = $text; Crates = (New-Object System.Collections.Generic.List[string]) }
            }
            $byText[$hash].Crates.Add($name)
        }
        $lines = New-Object System.Collections.Generic.List[string]
        $lines.Add('Third-party licenses for the dependencies linked into zipnest.exe.')
        $lines.Add("Generated by packaging\emit-portable.ps1 -ThirdParty; identical license texts are printed once.")
        $lines.Add("Crates covered: $($names.Count - $missing) of $($names.Count) (the rest ship no LICENSE file).")
        $lines.Add('')
        foreach ($entry in ($byText.Values | Sort-Object { $_.Crates[0] })) {
            $lines.Add('=' * 78)
            $lines.Add(($entry.Crates -join ', '))
            $lines.Add('=' * 78)
            $lines.Add($entry.Text)
            $lines.Add('')
        }
        # UTF-8 without BOM: a BOM would show up as a stray character in editors.
        [System.IO.File]::WriteAllText($out, ($lines -join "`r`n"), (New-Object System.Text.UTF8Encoding($false)))
        Write-Host ("  licenses\THIRD-PARTY.txt  <- {0} crates, {1} distinct license texts, {2} without one" -f `
            ($names.Count - $missing), $byText.Count, $missing)
    }
}

if (-not $SkipWin11Shell) {
    Write-Host 'Staging the Windows 11 shell package (build + sign)...'
    $stage = Join-Path $repo 'packaging\win11-shell\stage-portable.ps1'
    & $stage -PortableDir $portable
    if ($LASTEXITCODE -ne 0) { throw "stage-portable.ps1 failed ($LASTEXITCODE)" }
} else {
    Write-Warning 'Win11Shell was skipped: installer.nsi needs those files, so only a partial payload exists.'
}

Write-Host ''
Write-Host "Payload: $portable"
Get-ChildItem $portable -Recurse -File | ForEach-Object {
    $rel = $_.FullName.Substring($portable.Length + 1)
    Write-Host ("  {0,-46} {1,10:N0}  {2}" -f $rel, $_.Length, (Get-FileHash $_.FullName -Algorithm SHA256).Hash.Substring(0, 16))
}
