#!/usr/bin/env bash
# Read-only health check for the launched verification instance.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=env.sh
source "${SCRIPT_DIR}/env.sh"

fail() { echo "DOCTOR FAIL: $*" >&2; exit 1; }

[[ -f "${VERIFY_RUN_DIR}/app.pid" ]] || fail "missing app.pid in ${VERIFY_RUN_DIR}"
APP_PID="$(cat "${VERIFY_RUN_DIR}/app.pid")"
kill -0 "${APP_PID}" 2>/dev/null || fail "pid ${APP_PID} not running"

# Confirm the pid is our binary (best-effort).
cmd="$(ps -p "${APP_PID}" -o args= 2>/dev/null || true)"
[[ "${cmd}" == *rusticdl* ]] || fail "pid ${APP_PID} command does not look like rusticdl: ${cmd}"

WID="$(xdotool search --name '^RusticDL$' 2>/dev/null | head -1 || true)"
[[ -n "${WID}" ]] || fail "no window titled RusticDL"
echo "${WID}" >"${VERIFY_RUN_DIR}/window.id"

[[ -S "${IPC_SOCK}" ]] || fail "IPC socket missing: ${IPC_SOCK}"

resp="$("${SCRIPT_DIR}/ipc.sh" get_status)"
echo "${resp}" | grep -q '"ok":true\|"ok": true' || fail "get_status not ok: ${resp}"
echo "${resp}" | grep -q '"appState":"running"' || fail "appState not running: ${resp}"
echo "${resp}" | grep -q '"connectionState":"connected"' || fail "not connected: ${resp}"
echo "${resp}" | grep -q '"appVersion"' || fail "missing appVersion: ${resp}"

settings="${XDG_DATA_HOME}/RusticDL/settings.json"
[[ -f "${settings}" ]] || fail "settings.json missing under isolated data home: ${settings}"
case "${settings}" in
  "${VERIFY_RUN_DIR}"/*) ;;
  *) fail "settings path not under VERIFY_RUN_DIR (refusing shared instance): ${settings}" ;;
esac

echo "DOCTOR OK pid=${APP_PID} wid=${WID}"
echo "${resp}"
