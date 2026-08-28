# Releases

## Continuous integration

GitHub Actions workflows live in `.github/workflows/`:

| Workflow | When it runs | What it does |
| --- | --- | --- |
| **CI** (`ci.yml`) | Push / PR to `master` | `cargo fmt` check, `clippy`, `test`, extension typecheck + build |
| **Release** (`release.yml`) | Tag `v*` except `v*-nightly.*` (e.g. `v0.3.1`) | Build Windows release binaries, NSIS setup.exe, extension zips; publish a **Stable** GitHub Release |
| **Nightly** (`nightly.yml`) | Manual **Run workflow** only | Windows release assets plus **`RusticDL-linux-x64.tar.gz`**, stamped `X.Y.Z-nightly.YYYYMMDDHHMMSS`, published as a GitHub **pre-release** (`make_latest: false`) for testing before a Stable cut. Skips when that commit already has a nightly. Keeps the last 14 nightlies. (`release.yml` remains Windows-only.) |

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

## Contributing / attribution

Issues and pull requests are welcome.

If you **fork, modify, redistribute, or ship** RusticDL (including commercial products), keep the MIT copyright notice and license text intact, and credit the original project:

- Project: **RusticDL**
- Author / maintainer: **[JustNak](https://github.com/JustNak)**
- Upstream: https://github.com/JustNak/RusticDL

That attribution requirement is part of the MIT license terms for this repository.
