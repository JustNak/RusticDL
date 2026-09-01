# Fail closed unless Azure Trusted Signing identity is present in the environment.
# Used by Nightly/Stable Windows publish so unsigned setup.exe is never uploaded.
#
# Expected env (GitHub Secrets / Variables mapped by the workflow):
#   AZURE_CLIENT_ID
#   AZURE_TENANT_ID
#   AZURE_SUBSCRIPTION_ID
#   AZURE_TRUSTED_SIGNING_ENDPOINT
#   AZURE_TRUSTED_SIGNING_ACCOUNT
#   AZURE_TRUSTED_SIGNING_PROFILE

[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"

$required = @(
    "AZURE_CLIENT_ID",
    "AZURE_TENANT_ID",
    "AZURE_SUBSCRIPTION_ID",
    "AZURE_TRUSTED_SIGNING_ENDPOINT",
    "AZURE_TRUSTED_SIGNING_ACCOUNT",
    "AZURE_TRUSTED_SIGNING_PROFILE"
)

$missing = @()
foreach ($name in $required) {
    $value = [Environment]::GetEnvironmentVariable($name)
    if ([string]::IsNullOrWhiteSpace($value)) {
        $missing += $name
    }
}

if ($missing.Count -gt 0) {
    $list = $missing -join ", "
    Write-Error @"
Azure Trusted Signing is required to publish Windows builds.
The in-app updater rejects unsigned setup.exe (WinVerifyTrust).
Missing: $list

Enroll a public-trust Trusted Signing profile, add the GitHub secrets/vars
listed in docs/releases.md, and use GitHub environment 'windows-signing'.
Unsigned assets must not be published.
"@
    exit 1
}

Write-Host "Azure Trusted Signing identity is configured."
