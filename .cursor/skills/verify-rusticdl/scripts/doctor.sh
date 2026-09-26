#!/usr/bin/env bash
# Read-only health check for the launched verification instance.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=env.sh
source "${SCRIPT_DIR}/env.sh"

fail() { echo "DOCTOR FAIL: $*" >&2; exit 1; }

[[ -f "${VERIFY_RUN_DIR}/app.pid" ]] || fail "missing app.pid in ${VERIFY_RUN_DIR}"
APP_PID="$(tr -d '[:space:]' < "${VERIFY_RUN_DIR}/app.pid")"
kill -0 "${APP_PID}" 2>/dev/null || fail "pid ${APP_PID} not running"

# Confirm the pid is our binary (best-effort).
cmd="$(ps -p "${APP_PID}" -o args= 2>/dev/null || true)"
[[ "${cmd}" == *rusticdl* ]] || fail "pid ${APP_PID} command does not look like rusticdl: ${cmd}"

wid_rc=0
WID="$(rusticdl_window_id)" || wid_rc=$?
if [[ "${wid_rc}" -eq 2 ]]; then
  fail "pid ${APP_PID} owns more than one RusticDL window"
fi
[[ "${wid_rc}" -eq 0 && -n "${WID}" ]] || fail "no RusticDL window owned by pid ${APP_PID}"
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

download_dir="$(
  python3 - "${settings}" "${VERIFY_RUN_DIR}" <<'PY'
import json, os, sys

settings_path, run_dir = sys.argv[1], sys.argv[2]
with open(settings_path, encoding="utf-8") as handle:
    data = json.load(handle)
raw = data.get("downloadDirectory")
if not isinstance(raw, str) or not raw.strip():
    sys.stderr.write("downloadDirectory missing or not a string\n")
    sys.exit(1)
real_dl = os.path.realpath(raw)
real_run = os.path.realpath(run_dir)
if real_dl != real_run and not real_dl.startswith(real_run + os.sep):
    sys.stderr.write(f"downloadDirectory outside VERIFY_RUN_DIR: {raw}\n")
    sys.exit(1)
print(raw)
PY
)" || fail "downloadDirectory is not under VERIFY_RUN_DIR"

echo "DOCTOR OK pid=${APP_PID} wid=${WID} downloads=${download_dir}"
echo "${resp}"
