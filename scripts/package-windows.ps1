# Build a Windows NSIS installer for RusticDL via cargo-packager.
#
# Prerequisites:
#   - Rust stable (with cargo)
#   - cargo-packager 0.11.x:  cargo install cargo-packager --locked --version 0.11.8
#
# Usage (from repo root):
#   powershell -ExecutionPolicy Bypass -File scripts/package-windows.ps1
#
# Output:
#   dist-release/*-setup.exe  (and intermediate packager files under dist-release/)

[CmdletBinding()]
param(
  [switch]$SkipBuild,
  [string]$PackagerVersion = "0.11.8"
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

function Test-CargoPackager {
  $cmd = Get-Command cargo-packager -ErrorAction SilentlyContinue
  if (-not $cmd) { return $false }
  return $true
}

if (-not (Test-CargoPackager)) {
  Write-Host "cargo-packager not found; installing v$PackagerVersion..."
  cargo install cargo-packager --locked --version $PackagerVersion
}

if (-not $SkipBuild) {
  Write-Host "Building release binaries..."
  cargo build --release -p rusticdl -p rusticdl-native-host
}

$outDir = Join-Path $repoRoot "dist-release"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

Write-Host "Running cargo packager (NSIS)..."
cargo packager --release --formats nsis -p rusticdl

# Normalize the setup filename for release uploads / docs.
$setupCandidates = @(
  Get-ChildItem -Path $outDir -Filter "*-setup.exe" -ErrorAction SilentlyContinue
  Get-ChildItem -Path $outDir -Filter "*.exe" -ErrorAction SilentlyContinue | Where-Object { $_.Name -match "setup|nsis" }
)

if (-not $setupCandidates -or $setupCandidates.Count -eq 0) {
  # cargo-packager sometimes writes next to binaries or under intermediates; search dist-release recursively.
  $setupCandidates = Get-ChildItem -Path $outDir -Recurse -Filter "*setup*.exe" -ErrorAction SilentlyContinue
}

if (-not $setupCandidates -or $setupCandidates.Count -eq 0) {
  Write-Warning "Packager finished but no setup.exe was found under dist-release/. Check packager logs above."
  exit 1
}

$primary = $setupCandidates | Sort-Object LastWriteTime -Descending | Select-Object -First 1
$normalized = Join-Path $outDir "RusticDL-windows-x64-setup.exe"
if ($primary.FullName -ne $normalized) {
  Copy-Item -Force $primary.FullName $normalized
}

Write-Host ""
Write-Host "Installer ready:"
Write-Host "  $($primary.FullName)"
Write-Host "  $normalized"
Write-Host ""
Write-Host "Tip: run the setup silently with /S (NSIS). Per-user install does not require admin."
