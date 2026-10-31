#!/usr/bin/env bash
# Chromium native-messaging shim. The host manifest "path" must be absolute.
# Inherits RUN_DIR / VERIFY_RUN_DIR / NODE_BIN from the browser process env.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NODE_BIN="${NODE_BIN:-$(command -v node || true)}"
if [[ -z "${NODE_BIN}" || ! -x "${NODE_BIN}" ]]; then
  echo "mock-native-host.sh: node not found (set NODE_BIN to an absolute path)" >&2
  exit 1
fi
if [[ -z "${RUN_DIR:-}" && -z "${VERIFY_RUN_DIR:-}" ]]; then
  echo "mock-native-host.sh: RUN_DIR or VERIFY_RUN_DIR is required" >&2
  exit 1
fi
export RUN_DIR="${RUN_DIR:-$VERIFY_RUN_DIR}"
exec "${NODE_BIN}" "${SCRIPT_DIR}/mock-native-host.mjs"
