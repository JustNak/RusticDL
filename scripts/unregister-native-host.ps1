param(
  # Quieter output for installer / automated use.
  [switch]$Quiet
)

$ErrorActionPreference = 'Stop'
$hostName = 'com.rusticdl.native_host'

$keys = @(
  "Software\Google\Chrome\NativeMessagingHosts\$hostName",
  "Software\Chromium\NativeMessagingHosts\$hostName",
  "Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\$hostName",
  "Software\Microsoft\Edge\NativeMessagingHosts\$hostName",
  "Software\Mozilla\NativeMessagingHosts\$hostName"
)

foreach ($subKey in $keys) {
  try {
    [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKeyTree($subKey, $false)
    if (-not $Quiet) {
      Write-Host "Removed HKCU:\$subKey"
    }
  } catch {
    if (-not $Quiet) {
      Write-Host "Skip (missing): HKCU:\$subKey"
    }
  }
}

if ($Quiet) {
  Write-Host "Unregistered RusticDL Backend: $hostName"
}
