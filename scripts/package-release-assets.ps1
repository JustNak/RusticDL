# Build the GitHub Release asset set under dist-release/.
#
# Prerequisites (from repo root):
#   - target/release/rusticdl.exe, rusticdl-updater.exe, rusticdl-native-host.exe
#   - apps/extension/dist/{chromium,firefox} built
#   - cargo-packager 0.11.x on PATH
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts/package-release-assets.ps1
#
# Output:
#   dist-release/RusticDL-windows-x64-setup.exe
#   dist-release/RusticDL-windows-x64.zip
#   dist-release/RusticDL-full-windows-x64.zip
#   dist-release/extension-chromium.zip
#   dist-release/extension-firefox.zip

[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

$packageWindows = Join-Path $PSScriptRoot "package-windows.ps1"
if (-not (Test-Path $packageWindows)) {
    throw "Missing $packageWindows"
}

Write-Host "Packaging NSIS installer..."
& $packageWindows -SkipBuild
if ($LASTEXITCODE -ne 0) {
    throw "package-windows.ps1 failed with exit code $LASTEXITCODE"
}

$dist = Join-Path $repoRoot "dist-release"
$setup = Join-Path $dist "RusticDL-windows-x64-setup.exe"
if (-not (Test-Path $setup)) {
    throw "NSIS setup.exe not found at $setup"
}
Write-Host "Packed RusticDL-windows-x64-setup.exe ($([math]::Round((Get-Item $setup).Length / 1MB, 2)) MB)"

function Assert-File([string]$Path) {
    if (-not (Test-Path $Path)) {
        throw "Required file missing: $Path"
    }
}

Assert-File "target/release/rusticdl.exe"
Assert-File "target/release/rusticdl-updater.exe"
Assert-File "target/release/rusticdl-native-host.exe"
Assert-File "apps/extension/dist/chromium/manifest.json"
Assert-File "apps/extension/dist/firefox/manifest.json"

# App-only zip (portable)
$appDir = Join-Path $dist "app"
if (Test-Path $appDir) {
    Remove-Item -Recurse -Force $appDir
}
New-Item -ItemType Directory -Force -Path $appDir | Out-Null
Copy-Item "target/release/rusticdl.exe" $appDir
Copy-Item "target/release/rusticdl-updater.exe" $appDir
Copy-Item "LICENSE" $appDir
Copy-Item "README.md" $appDir
Compress-Archive -Path (Join-Path $appDir "*") -DestinationPath (Join-Path $dist "RusticDL-windows-x64.zip") -Force

# Extension zips
Compress-Archive -Path "apps/extension/dist/chromium/*" -DestinationPath (Join-Path $dist "extension-chromium.zip") -Force
Compress-Archive -Path "apps/extension/dist/firefox/*" -DestinationPath (Join-Path $dist "extension-firefox.zip") -Force

# Full package (portable)
$fullDir = Join-Path $dist "full"
if (Test-Path $fullDir) {
    Remove-Item -Recurse -Force $fullDir
}
New-Item -ItemType Directory -Force -Path $fullDir | Out-Null
Copy-Item "target/release/rusticdl.exe" $fullDir
Copy-Item "target/release/rusticdl-updater.exe" $fullDir
Copy-Item "target/release/rusticdl-native-host.exe" $fullDir
Copy-Item "LICENSE" $fullDir
Copy-Item "README.md" $fullDir
New-Item -ItemType Directory -Force -Path (Join-Path $fullDir "scripts") | Out-Null
Copy-Item "scripts/register-native-host.ps1" (Join-Path $fullDir "scripts")
Copy-Item "scripts/unregister-native-host.ps1" (Join-Path $fullDir "scripts")
$nativeManifests = "apps/native-host/manifests"
if (Test-Path $nativeManifests) {
    Copy-Item "$nativeManifests/*" (Join-Path $fullDir "scripts") -ErrorAction SilentlyContinue
}
New-Item -ItemType Directory -Force -Path (Join-Path $fullDir "extension/chromium") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $fullDir "extension/firefox") | Out-Null
Copy-Item "apps/extension/dist/chromium/*" (Join-Path $fullDir "extension/chromium") -Recurse
Copy-Item "apps/extension/dist/firefox/*" (Join-Path $fullDir "extension/firefox") -Recurse
Compress-Archive -Path (Join-Path $fullDir "*") -DestinationPath (Join-Path $dist "RusticDL-full-windows-x64.zip") -Force

Get-ChildItem $dist -Filter *.zip | ForEach-Object {
    Write-Host "Packed $($_.Name) ($([math]::Round($_.Length / 1MB, 2)) MB)"
}
