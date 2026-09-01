# Releases

## Continuous integration

GitHub Actions workflows live in `.github/workflows/`:

| Workflow | When it runs | What it does |
| --- | --- | --- |
| **CI** (`ci.yml`) | Push / PR to `master` | `cargo fmt` check, `clippy`, `test`, extension typecheck + build |
| **Release** (`release.yml`) | Tag `v*` except `v*-nightly.*` (e.g. `v0.3.1`) | Build Windows release binaries, Authenticode-sign them, NSIS setup.exe, extension zips, plus **`RusticDL-linux-x64.tar.gz`** and **`SHA256SUMS`**; publish a **Stable** GitHub Release. Windows publish **fails closed** unless Azure Trusted Signing is configured (unsigned `setup.exe` is not uploaded). |
| **Nightly** (`nightly.yml`) | Manual **Run workflow** only | Same Windows signing gate as Stable, plus the full Linux tarball (app + native host + updater + `install-linux.sh`) and **`SHA256SUMS`**, stamped `X.Y.Z-nightly.YYYYMMDDHHMMSS`, published as a GitHub **pre-release** (`make_latest: false`) for testing before a Stable cut. Skips when that commit already has a nightly. Keeps the last 14 nightlies. |

To cut a new **stable** release from a clean tree:

```bash
git tag v0.1.1
git push origin v0.1.1
```

The release workflow builds assets and attaches them to the GitHub Release automatically.

To publish a **nightly** (when you want testers to try new work):

1. Actions → **Nightly** → **Run workflow**
2. Optionally check **Publish even if this commit already has a nightly**

The in-app updater on the Nightly channel follows tags matching `vX.Y.Z-nightly.*`. Stable still uses `/releases/latest`. Switching channels installs that stream’s current build even when its version number is lower.

The Windows updater runs `WinVerifyTrust` on `RusticDL-windows-x64-setup.exe` before silent install. Unsigned or untrusted setup files are rejected and the helper offers the GitHub release page. Local `scripts/package-windows.ps1` builds stay unsigned for development; only GitHub **Release** and **Nightly** sign.

## Windows Authenticode (Azure Trusted Signing)

Stable and Nightly Windows jobs sign `rusticdl.exe`, `rusticdl-updater.exe`, `rusticdl-native-host.exe`, then the NSIS `RusticDL-windows-x64-setup.exe`, via [Azure Trusted Signing](https://learn.microsoft.com/azure/trusted-signing/) (OIDC, no PFX in GitHub). Missing identity **fails the job**; unsigned assets are not published.

Identity validation for a public-trust certificate profile often takes several business days. Create the GitHub environment **`windows-signing`** (no required reviewers unless you want them) so the federated credential subject is `repo:JustNak/RusticDL:environment:windows-signing`.

**GitHub Secrets**

- `AZURE_CLIENT_ID` — App Registration application (client) ID
- `AZURE_TENANT_ID`
- `AZURE_SUBSCRIPTION_ID`

**GitHub Variables**

- `AZURE_TRUSTED_SIGNING_ENDPOINT` — regional URI, for example `https://eus.codesigning.azure.net/`
- `AZURE_TRUSTED_SIGNING_ACCOUNT` — Trusted Signing account name
- `AZURE_TRUSTED_SIGNING_PROFILE` — public-trust certificate profile name

**Azure**

- Trusted Signing account + public-trust certificate profile (identity validation completed)
- App Registration with a **federated credential** for `repo:JustNak/RusticDL:environment:windows-signing`
- Role **Trusted Signing Certificate Profile Signer** on that account

The workflow requests `id-token: write` for OIDC. After the first signed Nightly, in-app update from a post-Authenticode install should run setup.exe instead of opening the release page.

## Contributing / attribution

Issues and pull requests are welcome.

If you **fork, modify, redistribute, or ship** RusticDL (including commercial products), keep the MIT copyright notice and license text intact, and credit the original project:

- Project: **RusticDL**
- Author / maintainer: **[JustNak](https://github.com/JustNak)**
- Upstream: https://github.com/JustNak/RusticDL

That attribution requirement is part of the MIT license terms for this repository.
