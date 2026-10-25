#!/usr/bin/env bash
# Build (if needed) and launch an isolated RusticDL instance.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=env.sh
source "${SCRIPT_DIR}/env.sh"

kill_launched() {
  local pid="${APP_PID:-}"
  if [[ -z "${pid}" && -f "${VERIFY_RUN_DIR}/app.pid" ]]; then
    pid="$(tr -d '[:space:]' < "${VERIFY_RUN_DIR}/app.pid")"
  fi
  if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
    kill "${pid}" 2>/dev/null || true
    for _ in $(seq 1 20); do
      kill -0 "${pid}" 2>/dev/null || break
      sleep 0.25
    done
    if kill -0 "${pid}" 2>/dev/null; then
      kill -9 "${pid}" 2>/dev/null || true
    fi
  fi
  rm -f "${VERIFY_RUN_DIR}/app.pid" "${VERIFY_RUN_DIR}/window.id"
}

fail_launch() {
  echo "$*" >&2
  if [[ -f "${VERIFY_RUN_DIR}/app.log" ]]; then
    echo "Log:" >&2
    cat "${VERIFY_RUN_DIR}/app.log" >&2
  fi
  kill_launched
  exit 1
}

if [[ ! -x "${RUSTICDL_BIN}" ]]; then
  echo "Building rusticdl…"
  cargo build -p rusticdl
fi

host_bin="$(dirname "${RUSTICDL_BIN}")/rusticdl-native-host"
if [[ -e "${host_bin}" ]]; then
  echo "Refusing to launch: sibling ${host_bin} would rewrite browser native-messaging manifests." >&2
  exit 1
fi

if [[ -f "${VERIFY_RUN_DIR}/app.pid" ]]; then
  old="$(tr -d '[:space:]' < "${VERIFY_RUN_DIR}/app.pid")"
  if [[ -n "${old}" ]] && kill -0 "${old}" 2>/dev/null; then
    echo "Instance already running pid=${old} under ${VERIFY_RUN_DIR}" >&2
    exit 1
  fi
fi

# Seed settings the app will load so downloadDirectory is never dirs::download_dir().
VERIFY_DOWNLOADS_DIR="${VERIFY_RUN_DIR}/downloads"
mkdir -p "${XDG_DATA_HOME}/RusticDL" "${VERIFY_DOWNLOADS_DIR}"
python3 - "${XDG_DATA_HOME}/RusticDL/settings.json" "${VERIFY_DOWNLOADS_DIR}" <<'PY'
import json, os, sys

path, downloads = sys.argv[1], sys.argv[2]
data = {}
if os.path.isfile(path):
    try:
        with open(path, encoding="utf-8") as handle:
            loaded = json.load(handle)
        if isinstance(loaded, dict):
            data = loaded
    except (OSError, json.JSONDecodeError):
        data = {}
data["downloadDirectory"] = downloads
data.setdefault("maxConcurrentDownloads", 3)
data.setdefault("autoRetryAttempts", 6)
data.setdefault("speedLimitKibPerSecond", 0)
data.setdefault("theme", "light")
os.makedirs(os.path.dirname(path), exist_ok=True)
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2)
    handle.write("\n")
PY

# Clear stale lock/socket from a crashed prior attempt in this run dir only.
rm -f "${XDG_RUNTIME_DIR}/rusticdl.lock" "${IPC_SOCK}"

"${RUSTICDL_BIN}" >"${VERIFY_RUN_DIR}/app.log" 2>&1 &
echo $! >"${VERIFY_RUN_DIR}/app.pid"
APP_PID="$(tr -d '[:space:]' < "${VERIFY_RUN_DIR}/app.pid")"

ready=0
WID=""
for _ in $(seq 1 40); do
  if ! kill -0 "${APP_PID}" 2>/dev/null; then
    fail_launch "App exited during launch."
  fi
  wid_rc=0
  WID="$(rusticdl_window_id)" || wid_rc=$?
  if [[ "${wid_rc}" -eq 2 ]]; then
    fail_launch "Launch matched more than one RusticDL window for pid ${APP_PID}."
  fi
  if [[ "${wid_rc}" -eq 0 && -n "${WID}" && -S "${IPC_SOCK}" ]]; then
    ready=1
    break
  fi
  sleep 0.5
done

if [[ "${ready}" -ne 1 ]]; then
  fail_launch "Timed out waiting for pid-owned window/socket."
fi

echo "${WID}" >"${VERIFY_RUN_DIR}/window.id"
echo "READY pid=${APP_PID} wid=${WID} run=${VERIFY_RUN_DIR}"
