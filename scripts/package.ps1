# Builds clipr's downloads in dist\: a portable zip (the release exe with the web UI embedded, the guide and
# the license) and the installer (installer\clipr.iss, when Inno Setup 6 is installed).
#
#   powershell -ExecutionPolicy Bypass -File scripts\package.ps1 [-SkipBuild] [-NoInstaller]
#
# Needs Rust (cargo) and Node (npm) on PATH. -SkipBuild packages an exe that's already built (CI does this).

param([switch]$SkipBuild, [switch]$NoInstaller)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# The workspace version ([workspace.package] is the only `version =` at the start of a line in Cargo.toml).
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
Write-Host "Packaging clipr $version"

if (-not $SkipBuild) {
    Push-Location ui
    npm ci
    if ($LASTEXITCODE -ne 0) { throw 'npm ci failed' }
    npm run build
    if ($LASTEXITCODE -ne 0) { throw 'the UI build failed' }
    Pop-Location
    if (-not (Test-Path ui\dist\index.html)) { throw 'ui\dist\index.html is missing after the UI build' }

    cargo build --release --locked -p clipr
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
}
if (-not (Test-Path target\release\clipr.exe)) { throw 'target\release\clipr.exe is missing; build it first' }

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
Write-Host "Zip: $zip ($size MB)"

if ($NoInstaller) { return }
$iscc = @(
    (Get-Command ISCC.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source),
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $iscc) {
    Write-Warning 'Inno Setup 6 not found (https://jrsoftware.org/isdl.php): skipped the installer'
    return
}
& $iscc /Q "/DAppVersion=$version" "/DSourceDir=$(Resolve-Path $stage)" "/DOutputDir=$(Resolve-Path dist)" installer\clipr.iss
if ($LASTEXITCODE -ne 0) { throw 'the installer build failed' }
$setup = Join-Path dist "clipr-$version-setup.exe"
$size = [math]::Round((Get-Item $setup).Length / 1MB, 1)
Write-Host "Installer: $setup ($size MB)"
