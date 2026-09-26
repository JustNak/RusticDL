#!/usr/bin/env bash
# Stop the verification instance started by launch.sh. Preserves evidence.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=env.sh
source "${SCRIPT_DIR}/env.sh"

if [[ ! -f "${VERIFY_RUN_DIR}/app.pid" ]]; then
  echo "No app.pid at ${VERIFY_RUN_DIR}; nothing to kill"
  exit 0
fi

APP_PID="$(cat "${VERIFY_RUN_DIR}/app.pid")"
if kill -0 "${APP_PID}" 2>/dev/null; then
  kill "${APP_PID}" 2>/dev/null || true
  for _ in $(seq 1 20); do
    kill -0 "${APP_PID}" 2>/dev/null || break
    sleep 0.25
  done
  if kill -0 "${APP_PID}" 2>/dev/null; then
    kill -9 "${APP_PID}" 2>/dev/null || true
  fi
  echo "Stopped pid=${APP_PID}"
else
  echo "Pid ${APP_PID} already stopped"
fi

rm -f "${VERIFY_RUN_DIR}/app.pid" "${VERIFY_RUN_DIR}/window.id"
# Keep evidence/, data/, app.log, and artifacts/ for inspection.
echo "Cleanup done. Evidence kept at ${VERIFY_EVIDENCE_DIR}"
