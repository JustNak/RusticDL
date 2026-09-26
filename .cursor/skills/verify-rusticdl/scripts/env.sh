#!/usr/bin/env bash
# Shared environment for control-rusticdl. Source from repo root:
#   export RUN_ID=...
#   source .cursor/skills/verify-rusticdl/scripts/env.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
cd "$ROOT"

RUN_ID="${RUN_ID:-$(date +%Y%m%d%H%M%S)}"
VERIFY_RUN_DIR="${VERIFY_RUN_DIR:-/tmp/rusticdl-verify-${RUN_ID}}"
VERIFY_EVIDENCE_DIR="${VERIFY_EVIDENCE_DIR:-${VERIFY_RUN_DIR}/evidence}"
VERIFY_ARTIFACTS_DIR="${VERIFY_ARTIFACTS_DIR:-${ROOT}/.cursor/skills/verify-rusticdl/artifacts}"
RUSTICDL_BIN="${RUSTICDL_BIN:-${ROOT}/target/debug/rusticdl}"

mkdir -p \
  "${VERIFY_RUN_DIR}/runtime" \
  "${VERIFY_RUN_DIR}/data" \
  "${VERIFY_EVIDENCE_DIR}"

export RUN_ID VERIFY_RUN_DIR VERIFY_EVIDENCE_DIR VERIFY_ARTIFACTS_DIR RUSTICDL_BIN ROOT

# Isolate lock + socket + app data. Keep real HOME for X11 auth.
export XDG_RUNTIME_DIR="${VERIFY_RUN_DIR}/runtime"
export XDG_DATA_HOME="${VERIFY_RUN_DIR}/data"

export HOME="${HOME:-/home/ubuntu}"
export XAUTHORITY="${XAUTHORITY:-${HOME}/.Xauthority}"
export DISPLAY="${DISPLAY:-:1}"

# Ubuntu gcc private libstdc++ path (harmless if unused elsewhere).
if [[ -d /usr/lib/gcc/x86_64-linux-gnu/13 ]]; then
  export LIBRARY_PATH="/usr/lib/gcc/x86_64-linux-gnu/13${LIBRARY_PATH:+:$LIBRARY_PATH}"
elif [[ -d /usr/lib/gcc/x86_64-linux-gnu/14 ]]; then
  export LIBRARY_PATH="/usr/lib/gcc/x86_64-linux-gnu/14${LIBRARY_PATH:+:$LIBRARY_PATH}"
fi

# Software Vulkan when no GPU (Mesa lavapipe).
if [[ -f /usr/share/vulkan/icd.d/lvp_icd.json ]]; then
  export VK_ICD_FILENAMES="${VK_ICD_FILENAMES:-/usr/share/vulkan/icd.d/lvp_icd.json}"
  export VK_DRIVER_FILES="${VK_DRIVER_FILES:-/usr/share/vulkan/icd.d/lvp_icd.json}"
fi

IPC_SOCK="${XDG_RUNTIME_DIR}/rusticdl.v1.sock"
export IPC_SOCK
