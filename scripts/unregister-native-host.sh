#!/usr/bin/env bash
# Remove user-level RusticDL native messaging manifests.
set -euo pipefail

HOST_NAME="com.rusticdl.native_host"
QUIET=0

while [ $# -gt 0 ]; do
  case "$1" in
    --quiet) QUIET=1; shift ;;
    -h|--help)
      echo "Usage: unregister-native-host.sh [--quiet]"
      exit 0
      ;;
    *) echo "Unknown argument: $1" >&2; exit 1 ;;
  esac
done

config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
dirs=(
  "$config_home/google-chrome/NativeMessagingHosts"
  "$config_home/google-chrome-beta/NativeMessagingHosts"
  "$config_home/google-chrome-unstable/NativeMessagingHosts"
  "$config_home/google-chrome-for-testing/NativeMessagingHosts"
  "$config_home/chromium/NativeMessagingHosts"
  "$config_home/BraveSoftware/Brave-Browser/NativeMessagingHosts"
  "$config_home/vivaldi/NativeMessagingHosts"
  "$config_home/microsoft-edge/NativeMessagingHosts"
  "$config_home/microsoft-edge-beta/NativeMessagingHosts"
  "$config_home/microsoft-edge-dev/NativeMessagingHosts"
  "$HOME/.mozilla/native-messaging-hosts"
  "$HOME/.librewolf/native-messaging-hosts"
  "$HOME/.waterfox/native-messaging-hosts"
)

for dir in "${dirs[@]}"; do
  file="$dir/${HOST_NAME}.json"
  if [ -f "$file" ]; then
    rm -f "$file"
    if [ "$QUIET" -eq 0 ]; then
      echo "Removed $file"
    fi
  elif [ "$QUIET" -eq 0 ]; then
    echo "Skip (missing): $file"
  fi
done

if [ "$QUIET" -eq 1 ]; then
  echo "Unregistered RusticDL Backend: $HOST_NAME"
fi
