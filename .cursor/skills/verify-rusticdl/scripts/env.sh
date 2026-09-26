#!/usr/bin/env bash
# Shared environment for control-rusticdl. Source from repo root:
#   export RUN_ID=...
#   source .cursor/skills/verify-rusticdl/scripts/env.sh
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "verify-rusticdl helpers run on Linux only (Windows is not isolated)." >&2
  return 1 2>/dev/null || exit 1
fi

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

# Never inherit a live-app socket path; the Unix listener unlinks then binds it.
unset RUSTICDL_PIPE_PATH

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

# Print the single X window titled RusticDL owned by the pidfile process.
# Exit 1 if none, 2 if more than one (never pick by XQueryTree order).
rusticdl_window_id() {
  local pid w wp
  local -a matches=()
  if [[ ! -f "${VERIFY_RUN_DIR}/app.pid" ]]; then
    echo "missing ${VERIFY_RUN_DIR}/app.pid" >&2
    return 1
  fi
  pid="$(tr -d '[:space:]' < "${VERIFY_RUN_DIR}/app.pid")"
  if [[ ! "${pid}" =~ ^[0-9]+$ ]]; then
    echo "invalid pid in app.pid: ${pid}" >&2
    return 1
  fi
  while read -r w; do
    [[ -n "${w}" ]] || continue
    wp="$(xdotool getwindowpid "${w}" 2>/dev/null || true)"
    if [[ "${wp}" == "${pid}" ]]; then
      matches+=("${w}")
    fi
  done < <(xdotool search --name '^RusticDL$' 2>/dev/null || true)
  if [[ ${#matches[@]} -eq 0 ]]; then
    echo "no RusticDL window owned by pid ${pid}" >&2
    return 1
  fi
  if [[ ${#matches[@]} -gt 1 ]]; then
    echo "pid ${pid} owns ${#matches[@]} RusticDL windows (${matches[*]}); refusing to guess" >&2
    return 2
  fi
  echo "${matches[0]}"
}
