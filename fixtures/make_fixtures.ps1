$ErrorActionPreference = "Stop"
$7z = "C:\Program Files\7-Zip\7z.exe"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
Remove-Item (Join-Path $here "*.zip"), (Join-Path $here "*.7z") -Force -ErrorAction SilentlyContinue
$tmp = Join-Path $env:TEMP "zipnest-fx"
Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $tmp | Out-Null
$utf8 = New-Object System.Text.UTF8Encoding($false)
[IO.File]::WriteAllText((Join-Path $tmp "a.txt"), "hello zipnest`nline2", $utf8)
New-Item -ItemType Directory (Join-Path $tmp "sub") | Out-Null
[IO.File]::WriteAllText((Join-Path $tmp "sub\b.txt"), "nested content", $utf8)
# c.txt: padding file so big.zip-ish streams exist (kept in plain set for size tests)
$lines = @("padding header")
1..500 | ForEach-Object { $lines += "padding line $_" }
[IO.File]::WriteAllText((Join-Path $tmp "c.txt"), ($lines -join "`n"), $utf8)

Push-Location $tmp
& $7z a -tzip -y (Join-Path $here "plain.zip") a.txt sub c.txt | Out-Null
if ($LASTEXITCODE -ne 0) { throw "plain.zip failed" }
& $7z a -tzip -y (Join-Path $here "nested.zip") a.txt sub c.txt | Out-Null
if ($LASTEXITCODE -ne 0) { throw "nested.zip failed" }
& $7z a -tzip -y -psecret "-mem=AES256" (Join-Path $here "enc.zip") a.txt | Out-Null
if ($LASTEXITCODE -ne 0) { throw "enc.zip failed" }
& $7z a -t7z -y -psecret (Join-Path $here "enc.7z") a.txt | Out-Null
if ($LASTEXITCODE -ne 0) { throw "enc.7z failed" }
& $7z a -tzip -y (Join-Path $here "corrupt.zip") a.txt | Out-Null
if ($LASTEXITCODE -ne 0) { throw "corrupt.zip failed" }
Pop-Location

# corrupt: wipe local-file-header signature AND EOCD signature so no
# recognizable structure remains (7z can otherwise self-recover from either)
$bytes = [IO.File]::ReadAllBytes((Join-Path $here "corrupt.zip"))
for ($i = 0; $i -lt 4; $i++) { $bytes[$i] = 0 }                      # PK\x03\x04
for ($i = $bytes.Length - 22; $i -lt $bytes.Length - 18; $i++) { $bytes[$i] = 0 }  # PK\x05\x06
[IO.File]::WriteAllBytes((Join-Path $here "corrupt.zip"), $bytes)

Write-Host "fixtures ok"
