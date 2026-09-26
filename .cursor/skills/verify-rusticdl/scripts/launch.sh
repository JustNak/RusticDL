#!/usr/bin/env bash
# Build (if needed) and launch an isolated RusticDL instance.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=env.sh
source "${SCRIPT_DIR}/env.sh"

if [[ ! -x "${RUSTICDL_BIN}" ]]; then
  echo "Building rusticdl…"
  cargo build -p rusticdl
fi

if [[ -f "${VERIFY_RUN_DIR}/app.pid" ]]; then
  old="$(cat "${VERIFY_RUN_DIR}/app.pid")"
  if kill -0 "${old}" 2>/dev/null; then
    echo "Instance already running pid=${old} under ${VERIFY_RUN_DIR}" >&2
    exit 1
  fi
fi

# Clear stale lock/socket from a crashed prior attempt in this run dir only.
rm -f "${XDG_RUNTIME_DIR}/rusticdl.lock" "${IPC_SOCK}"

"${RUSTICDL_BIN}" >"${VERIFY_RUN_DIR}/app.log" 2>&1 &
echo $! >"${VERIFY_RUN_DIR}/app.pid"
APP_PID="$(cat "${VERIFY_RUN_DIR}/app.pid")"

ready=0
for _ in $(seq 1 40); do
  if ! kill -0 "${APP_PID}" 2>/dev/null; then
    echo "App exited during launch. Log:" >&2
    cat "${VERIFY_RUN_DIR}/app.log" >&2
    exit 1
  fi
  WID="$(xdotool search --name '^RusticDL$' 2>/dev/null | head -1 || true)"
  if [[ -n "${WID}" && -S "${IPC_SOCK}" ]]; then
    ready=1
    break
  fi
  sleep 0.5
done

if [[ "${ready}" -ne 1 ]]; then
  echo "Timed out waiting for window/socket. Log:" >&2
  cat "${VERIFY_RUN_DIR}/app.log" >&2
  exit 1
fi

echo "${WID}" >"${VERIFY_RUN_DIR}/window.id"
echo "READY pid=${APP_PID} wid=${WID} run=${VERIFY_RUN_DIR}"
