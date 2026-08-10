$ErrorActionPreference = 'Stop'
$hostName = 'com.rusticdl.native_host'

$keys = @(
  "Software\Google\Chrome\NativeMessagingHosts\$hostName",
  "Software\Microsoft\Edge\NativeMessagingHosts\$hostName",
  "Software\Mozilla\NativeMessagingHosts\$hostName"
)

foreach ($subKey in $keys) {
  try {
    [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKeyTree($subKey, $false)
    Write-Host "Removed HKCU:\$subKey"
  } catch {
    Write-Host "Skip (missing): HKCU:\$subKey"
  }
}
