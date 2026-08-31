#!/usr/bin/env bash
# Stage and pack RusticDL-linux-x64.tar.gz plus SHA256SUMS.
# Expects release binaries in target/release/.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dist="$root/dist-release"
stage="$dist/linux-stage"

rm -rf "$stage"
mkdir -p "$stage/scripts" "$stage/assets/brand" "$stage/assets/linux" "$dist"

need() {
  if [ ! -f "$1" ]; then
    echo "Missing $1" >&2
    exit 1
  fi
}

need "$root/target/release/rusticdl"
need "$root/target/release/rusticdl-native-host"
need "$root/target/release/rusticdl-updater"
need "$root/scripts/install-linux.sh"
need "$root/scripts/register-native-host.sh"
need "$root/scripts/unregister-native-host.sh"

cp -f "$root/target/release/rusticdl" "$stage/rusticdl"
cp -f "$root/target/release/rusticdl-native-host" "$stage/rusticdl-native-host"
cp -f "$root/target/release/rusticdl-updater" "$stage/rusticdl-updater"
chmod +x "$stage/rusticdl" "$stage/rusticdl-native-host" "$stage/rusticdl-updater"

cp -f "$root/scripts/install-linux.sh" "$stage/install-linux.sh"
cp -f "$root/scripts/register-native-host.sh" "$stage/scripts/register-native-host.sh"
cp -f "$root/scripts/unregister-native-host.sh" "$stage/scripts/unregister-native-host.sh"
chmod +x "$stage/install-linux.sh" "$stage/scripts/"*.sh

cp -f "$root/apps/native-host/manifests/chromium.template.json" "$stage/scripts/chromium.template.json"
cp -f "$root/apps/native-host/manifests/edge.template.json" "$stage/scripts/edge.template.json"
cp -f "$root/apps/native-host/manifests/firefox.template.json" "$stage/scripts/firefox.template.json"
cp -f "$root/apps/extension/chromium-identity.json" "$stage/scripts/chromium-identity.json"
cp -f "$root/assets/linux/rusticdl.desktop" "$stage/rusticdl.desktop"
cp -f "$root/assets/brand/icon-256.png" "$stage/assets/brand/icon-256.png"

tar -czf "$dist/RusticDL-linux-x64.tar.gz" -C "$stage" \
  rusticdl \
  rusticdl-native-host \
  rusticdl-updater \
  install-linux.sh \
  rusticdl.desktop \
  scripts \
  assets

(
  cd "$dist"
  sha256sum RusticDL-linux-x64.tar.gz > SHA256SUMS
)

echo "Wrote $dist/RusticDL-linux-x64.tar.gz"
echo "Wrote $dist/SHA256SUMS"
cat "$dist/SHA256SUMS"
