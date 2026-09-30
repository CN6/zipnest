$ErrorActionPreference = "Stop"
$7z = "C:\Program Files\7-Zip\7z.exe"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$tmp = Join-Path $env:TEMP "zipnest-fx"
Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $tmp | Out-Null
Set-Content (Join-Path $tmp "a.txt") "hello zipnest`nline2" -Encoding UTF8
New-Item -ItemType Directory (Join-Path $tmp "sub") | Out-Null
Set-Content (Join-Path $tmp "sub\b.txt") "nested content" -Encoding UTF8
# c.txt: padding file so big.zip-ish streams exist (kept in plain set for size tests)
$c = @("padding header")
1..500 | ForEach-Object { $c += "padding line $_" }
Set-Content (Join-Path $tmp "c.txt") ($c -join "`n") -Encoding UTF8

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

# corrupt: flip a byte inside the zip data area (central directory intact enough to attempt open)
$bytes = [IO.File]::ReadAllBytes((Join-Path $here "corrupt.zip"))
$bytes[$bytes.Length - 5] = $bytes[$bytes.Length - 5] -bxor 0xFF
[IO.File]::WriteAllBytes((Join-Path $here "corrupt.zip"), $bytes)

Write-Host "fixtures ok"
