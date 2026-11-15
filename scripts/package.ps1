# Builds a portable clipr zip in dist\: the release exe (web UI embedded) plus the beta guide.
#
#   powershell -ExecutionPolicy Bypass -File scripts\package.ps1
#
# Needs Rust (cargo) and Node (npm) on PATH.

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
Write-Host "Packaging clipr $version"

Push-Location ui
npm ci
if ($LASTEXITCODE -ne 0) { throw 'npm ci failed' }
npm run build
if ($LASTEXITCODE -ne 0) { throw 'the UI build failed' }
Pop-Location

cargo build --release --locked -p clipr
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

$name = "clipr-$version-windows-x64"
$stage = Join-Path dist $name
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item target\release\clipr.exe $stage
Copy-Item docs\beta.md (Join-Path $stage 'README.md')
Copy-Item LICENSE $stage

$zip = Join-Path dist "$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
$size = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Host "Done: $zip ($size MB)"
