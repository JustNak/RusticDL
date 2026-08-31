#!/usr/bin/env bash
# Register RusticDL Backend (native messaging host) for user-level Chrome / Firefox.
# The JSON "path" field must be an absolute filesystem path (no ~, no $PATH).
set -euo pipefail

HOST_NAME="com.rusticdl.native_host"
FIREFOX_ID="${FIREFOX_EXTENSION_ID:-rusticdl@local}"
CHROMIUM_ID="${CHROMIUM_EXTENSION_ID:-}"
EDGE_ID="${EDGE_EXTENSION_ID:-}"
HOST_BINARY_PATH="${HOST_BINARY_PATH:-}"
DESKTOP_BINARY_PATH="${DESKTOP_BINARY_PATH:-}"
INSTALL_ROOT="${INSTALL_ROOT:-}"
QUIET=0

usage() {
  cat <<'EOF'
Usage: register-native-host.sh [options]

  --host-binary PATH     Absolute path to rusticdl-native-host
  --desktop-binary PATH  Path to rusticdl (for display / sibling lookup)
  --install-root PATH    Directory that holds the host (default: host parent)
  --chromium-id ID       Unpacked Chromium extension id
  --edge-id ID           Unpacked Edge extension id
  --firefox-id ID        Firefox extension id (default rusticdl@local)
  --quiet                Less output
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --host-binary) HOST_BINARY_PATH="${2:-}"; shift 2 ;;
    --desktop-binary) DESKTOP_BINARY_PATH="${2:-}"; shift 2 ;;
    --install-root) INSTALL_ROOT="${2:-}"; shift 2 ;;
    --chromium-id) CHROMIUM_ID="${2:-}"; shift 2 ;;
    --edge-id) EDGE_ID="${2:-}"; shift 2 ;;
    --firefox-id) FIREFOX_ID="${2:-}"; shift 2 ;;
    --quiet) QUIET=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage >&2; exit 1 ;;
  esac
done

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace_root="$(cd "$script_dir/.." && pwd)"

pinned_chromium_id="looccikmfpkiagfaeiocohmcneoacmom"
for identity in \
  "$workspace_root/apps/extension/chromium-identity.json" \
  "$script_dir/chromium-identity.json"
do
  if [ -f "$identity" ]; then
    extracted="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("id",""))' "$identity" 2>/dev/null || true)"
    if [ -n "$extracted" ]; then
      pinned_chromium_id="$extracted"
    fi
    break
  fi
done

if [ -z "$CHROMIUM_ID" ]; then
  CHROMIUM_ID="$pinned_chromium_id"
fi
if [ -z "$EDGE_ID" ]; then
  EDGE_ID="$CHROMIUM_ID"
fi

if [ -z "$HOST_BINARY_PATH" ]; then
  for candidate in \
    "$workspace_root/target/debug/rusticdl-native-host" \
    "$workspace_root/target/release/rusticdl-native-host" \
    "$script_dir/../rusticdl-native-host" \
    "$workspace_root/rusticdl-native-host"
  do
    if [ -f "$candidate" ]; then
      HOST_BINARY_PATH="$candidate"
      break
    fi
  done
fi

if [ -z "$HOST_BINARY_PATH" ] || [ ! -f "$HOST_BINARY_PATH" ]; then
  cat >&2 <<'EOF'
Native host binary not found.
Build it first:
  cargo build -p rusticdl-native-host
Then re-run this script with --host-binary /absolute/path/to/rusticdl-native-host
EOF
  exit 1
fi

HOST_BINARY_PATH="$(readlink -f "$HOST_BINARY_PATH")"
if [ ! -x "$HOST_BINARY_PATH" ]; then
  chmod +x "$HOST_BINARY_PATH" || true
fi

if [ -z "$INSTALL_ROOT" ]; then
  INSTALL_ROOT="$(dirname "$HOST_BINARY_PATH")"
fi
INSTALL_ROOT="$(readlink -f "$INSTALL_ROOT")"

if [ -z "$DESKTOP_BINARY_PATH" ]; then
  for candidate in \
    "$INSTALL_ROOT/rusticdl" \
    "$workspace_root/target/debug/rusticdl" \
    "$workspace_root/target/release/rusticdl"
  do
    if [ -f "$candidate" ]; then
      DESKTOP_BINARY_PATH="$(readlink -f "$candidate")"
      break
    fi
  done
fi

template_root="$script_dir"
if [ ! -f "$template_root/firefox.template.json" ]; then
  template_root="$workspace_root/apps/native-host/manifests"
fi
if [ ! -f "$template_root/firefox.template.json" ]; then
  echo "Native host templates not found under $template_root" >&2
  exit 1
fi

write_manifest() {
  local template="$1"
  local output="$2"
  local host_path="$3"
  local chromium="$4"
  local edge="$5"
  local firefox="$6"
  python3 - "$template" "$output" "$host_path" "$chromium" "$edge" "$firefox" <<'PY'
import sys
from pathlib import Path
template, output, host, chromium, edge, firefox = sys.argv[1:7]
text = Path(template).read_text(encoding="utf-8")
text = (text
        .replace("__HOST_PATH__", host)
        .replace("__CHROMIUM_EXTENSION_ID__", chromium)
        .replace("__EDGE_EXTENSION_ID__", edge)
        .replace("__FIREFOX_EXTENSION_ID__", firefox))
Path(output).parent.mkdir(parents=True, exist_ok=True)
Path(output).write_text(text, encoding="utf-8")
PY
}

manifest_root="$INSTALL_ROOT/native-messaging"
mkdir -p "$manifest_root"
chromium_manifest="$manifest_root/${HOST_NAME}.chrome.json"
edge_manifest="$manifest_root/${HOST_NAME}.edge.json"
firefox_manifest="$manifest_root/${HOST_NAME}.firefox.json"

write_manifest "$template_root/chromium.template.json" "$chromium_manifest" \
  "$HOST_BINARY_PATH" "$CHROMIUM_ID" "$EDGE_ID" "$FIREFOX_ID"
write_manifest "$template_root/edge.template.json" "$edge_manifest" \
  "$HOST_BINARY_PATH" "$CHROMIUM_ID" "$EDGE_ID" "$FIREFOX_ID"
write_manifest "$template_root/firefox.template.json" "$firefox_manifest" \
  "$HOST_BINARY_PATH" "$CHROMIUM_ID" "$EDGE_ID" "$FIREFOX_ID"

config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
chromium_dirs=(
  "google-chrome"
  "google-chrome-beta"
  "google-chrome-unstable"
  "google-chrome-for-testing"
  "chromium"
  "BraveSoftware/Brave-Browser"
  "vivaldi"
)
edge_dirs=(
  "microsoft-edge"
  "microsoft-edge-beta"
  "microsoft-edge-dev"
)
firefox_dirs=(
  "$HOME/.mozilla/native-messaging-hosts"
  "$HOME/.librewolf/native-messaging-hosts"
  "$HOME/.waterfox/native-messaging-hosts"
)

install_copy() {
  local src="$1"
  local dest_dir="$2"
  mkdir -p "$dest_dir"
  cp -f "$src" "$dest_dir/${HOST_NAME}.json"
}

for rel in "${chromium_dirs[@]}"; do
  install_copy "$chromium_manifest" "$config_home/$rel/NativeMessagingHosts"
done
for rel in "${edge_dirs[@]}"; do
  install_copy "$edge_manifest" "$config_home/$rel/NativeMessagingHosts"
done
for dest in "${firefox_dirs[@]}"; do
  install_copy "$firefox_manifest" "$dest"
done

if [ "$QUIET" -eq 1 ]; then
  echo "Registered RusticDL Backend: $HOST_NAME"
  echo "  Backend     : $HOST_BINARY_PATH"
  [ -n "$DESKTOP_BINARY_PATH" ] && echo "  RusticDL    : $DESKTOP_BINARY_PATH"
  echo "  Chrome JSON : $chromium_manifest"
  echo "  Edge JSON   : $edge_manifest"
  echo "  Firefox JSON: $firefox_manifest"
  exit 0
fi

echo
echo "Registered RusticDL Backend: $HOST_NAME"
echo "  Backend     : $HOST_BINARY_PATH"
if [ -n "${DESKTOP_BINARY_PATH:-}" ]; then
  echo "  RusticDL    : $DESKTOP_BINARY_PATH"
  echo "  (RusticDL Backend auto-launches RusticDL if the socket is down)"
else
  echo "  RusticDL    : (not found - start rusticdl manually)"
fi
echo "  Chrome JSON : $chromium_manifest"
echo "  Edge JSON   : $edge_manifest"
echo "  Firefox JSON: $firefox_manifest"
echo "  Chromium id : $CHROMIUM_ID"
echo "  Edge id     : $EDGE_ID"
echo "  Firefox id  : $FIREFOX_ID"
echo
echo "Next steps:"
echo "  1. Start RusticDL (./rusticdl)"
echo "  2. Load the browser extension (see docs/browser-extension.md)"
echo "  3. Reload an already-loaded unpacked Chromium extension so it picks up the pinned id"
echo "  4. Open the extension popup and confirm status is Connected"
echo
echo "Firefox manifest preview:"
cat "$firefox_manifest"
echo
