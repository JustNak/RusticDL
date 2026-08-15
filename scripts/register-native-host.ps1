param(
  [Parameter(Mandatory = $false)]
  [string]$HostBinaryPath = "",

  [string]$DesktopBinaryPath = "",

  [string]$ChromiumExtensionId = '',

  [string]$EdgeExtensionId = '',

  [string]$FirefoxExtensionId = 'rusticdl@local',

  [string]$InstallRoot = "",

  # Quieter output for installer / automated use.
  [switch]$Quiet
)

$ErrorActionPreference = 'Stop'

$workspaceRoot = Split-Path -Parent $PSScriptRoot

# Must match apps/extension/chromium-identity.json (unpacked Chromium/Edge id).
$pinnedChromiumId = 'looccikmfpkiagfaeiocohmcneoacmom'
$identityCandidates = @(
  (Join-Path $workspaceRoot 'apps\extension\chromium-identity.json'),
  (Join-Path $PSScriptRoot 'chromium-identity.json')
)
foreach ($candidate in $identityCandidates) {
  if (-not (Test-Path $candidate)) { continue }
  try {
    $identity = Get-Content -Raw -Path $candidate | ConvertFrom-Json
    if ($identity.id) { $pinnedChromiumId = [string]$identity.id }
  } catch {
    # keep baked-in id
  }
  break
}

if (-not $ChromiumExtensionId) { $ChromiumExtensionId = $pinnedChromiumId }
if (-not $EdgeExtensionId) { $EdgeExtensionId = $pinnedChromiumId }

if (-not $HostBinaryPath) {
  $candidates = @(
    (Join-Path $workspaceRoot 'target\debug\rusticdl-native-host.exe'),
    (Join-Path $workspaceRoot 'target\release\rusticdl-native-host.exe')
  )
  foreach ($candidate in $candidates) {
    if (Test-Path $candidate) {
      $HostBinaryPath = (Resolve-Path $candidate).Path
      break
    }
  }
}

if (-not $HostBinaryPath -or -not (Test-Path $HostBinaryPath)) {
  throw @"
Native host binary not found.
Build it first:
  cargo build -p rusticdl-native-host
Then re-run this script.
"@
}

$HostBinaryPath = (Resolve-Path $HostBinaryPath).Path

if (-not $InstallRoot) {
  $InstallRoot = Split-Path -Parent $HostBinaryPath
}

if (-not $DesktopBinaryPath) {
  $desktopCandidates = @(
    (Join-Path $InstallRoot 'rusticdl.exe'),
    (Join-Path $workspaceRoot 'target\debug\rusticdl.exe'),
    (Join-Path $workspaceRoot 'target\release\rusticdl.exe')
  )
  foreach ($candidate in $desktopCandidates) {
    if (Test-Path $candidate) {
      $DesktopBinaryPath = (Resolve-Path $candidate).Path
      break
    }
  }
}

function Write-Manifest {
  param(
    [string]$TemplatePath,
    [string]$OutputPath,
    [hashtable]$Replacements
  )

  $content = Get-Content -Raw -Path $TemplatePath
  foreach ($key in $Replacements.Keys) {
    $content = $content.Replace($key, [string]$Replacements[$key])
  }
  # Firefox/Chrome require UTF-8 without BOM for native messaging manifests.
  $utf8NoBom = New-Object System.Text.UTF8Encoding $false
  [System.IO.File]::WriteAllText($OutputPath, $content, $utf8NoBom)
}

$bundledTemplatePath = Join-Path $PSScriptRoot 'chromium.template.json'
$templateRoot = if (Test-Path $bundledTemplatePath) {
  $PSScriptRoot
} else {
  Join-Path $workspaceRoot 'apps\native-host\manifests'
}

if (-not (Test-Path (Join-Path $templateRoot 'firefox.template.json'))) {
  throw "Native host templates not found under $templateRoot"
}

$manifestRoot = Join-Path $InstallRoot 'native-messaging'
New-Item -ItemType Directory -Force -Path $manifestRoot | Out-Null

# JSON path must use escaped backslashes.
$escapedHostPath = $HostBinaryPath.Replace('\', '\\')
$hostName = 'com.rusticdl.native_host'

$chromiumManifestPath = Join-Path $manifestRoot "$hostName.chrome.json"
$edgeManifestPath = Join-Path $manifestRoot "$hostName.edge.json"
$firefoxManifestPath = Join-Path $manifestRoot "$hostName.firefox.json"

Write-Manifest -TemplatePath (Join-Path $templateRoot 'chromium.template.json') -OutputPath $chromiumManifestPath -Replacements @{
  '__HOST_PATH__' = $escapedHostPath
  '__CHROMIUM_EXTENSION_ID__' = $ChromiumExtensionId
}

Write-Manifest -TemplatePath (Join-Path $templateRoot 'edge.template.json') -OutputPath $edgeManifestPath -Replacements @{
  '__HOST_PATH__' = $escapedHostPath
  '__EDGE_EXTENSION_ID__' = $EdgeExtensionId
}

Write-Manifest -TemplatePath (Join-Path $templateRoot 'firefox.template.json') -OutputPath $firefoxManifestPath -Replacements @{
  '__HOST_PATH__' = $escapedHostPath
  '__FIREFOX_EXTENSION_ID__' = $FirefoxExtensionId
}

function Set-RegistryDefaultValue {
  param(
    [string]$SubKey,
    [string]$Value
  )

  $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($SubKey)
  if ($null -eq $key) {
    throw "Could not create registry key HKCU:\$SubKey"
  }

  try {
    $key.SetValue('', $Value, [Microsoft.Win32.RegistryValueKind]::String)
  } finally {
    $key.Dispose()
  }
}

Set-RegistryDefaultValue -SubKey "Software\Google\Chrome\NativeMessagingHosts\$hostName" -Value $chromiumManifestPath
Set-RegistryDefaultValue -SubKey "Software\Chromium\NativeMessagingHosts\$hostName" -Value $chromiumManifestPath
Set-RegistryDefaultValue -SubKey "Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\$hostName" -Value $chromiumManifestPath
Set-RegistryDefaultValue -SubKey "Software\Microsoft\Edge\NativeMessagingHosts\$hostName" -Value $edgeManifestPath
Set-RegistryDefaultValue -SubKey "Software\Mozilla\NativeMessagingHosts\$hostName" -Value $firefoxManifestPath

if ($Quiet) {
  Write-Host "Registered RusticDL Backend: $hostName"
  Write-Host "  Backend     : $HostBinaryPath"
  if ($DesktopBinaryPath) {
    Write-Host "  RusticDL    : $DesktopBinaryPath"
  }
  Write-Host "  Chrome JSON : $chromiumManifestPath"
  Write-Host "  Edge JSON   : $edgeManifestPath"
  Write-Host "  Firefox JSON: $firefoxManifestPath"
  return
}

Write-Host ""
Write-Host "Registered RusticDL Backend: $hostName"
Write-Host "  Backend     : $HostBinaryPath"
if ($DesktopBinaryPath) {
  Write-Host "  RusticDL    : $DesktopBinaryPath"
  Write-Host "  (RusticDL Backend auto-launches RusticDL if the pipe is down)"
} else {
  Write-Host "  RusticDL    : (not found - start rusticdl.exe manually)"
}
Write-Host "  Chrome JSON : $chromiumManifestPath"
Write-Host "  Edge JSON   : $edgeManifestPath"
Write-Host "  Firefox JSON: $firefoxManifestPath"
Write-Host "  Chromium id : $ChromiumExtensionId"
Write-Host "  Edge id     : $EdgeExtensionId"
Write-Host "  Firefox id  : $FirefoxExtensionId"
Write-Host ""
Write-Host "Next steps:"
Write-Host "  1. Start RusticDL (rusticdl.exe)"
Write-Host "  2. Load the browser extension (see README)"
Write-Host "  3. Reload an already-loaded unpacked Chromium extension so it picks up the pinned id"
Write-Host "  4. Open the extension popup and confirm status is Connected"
Write-Host ""
Write-Host "Firefox manifest preview:"
Get-Content -Raw $firefoxManifestPath
