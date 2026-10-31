#!/usr/bin/env bash
# Drive the Chromium extension capture matrix against the mock native host.
# Usage: prove-extension-capture.sh [--case Cn] [--live] [--repeat N]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=env.sh
source "${SCRIPT_DIR}/env.sh"

CASE_ID=""
LIVE=0
REPEAT=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --case) CASE_ID="${2:-}"; shift 2 ;;
    --live) LIVE=1; shift ;;
    --repeat) REPEAT="${2:-1}"; shift 2 ;;
    -h|--help)
      echo "Usage: $0 [--case Cn] [--live] [--repeat N]"
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

EXT_ROOT="${ROOT}/apps/extension"
DIST_MANIFEST="${EXT_ROOT}/dist/chromium/manifest.json"
PORT="${EXT_FIXTURE_PORT:-18765}"
export EXT_FIXTURE_PORT="${PORT}"
export EXT_FIXTURE_ORIGIN="http://127.0.0.1:${PORT}"
export EXT_FIXTURE_LOCALHOST="http://localhost:${PORT}"
export EXT_NONCE="${EXT_NONCE:-$(python3 -c 'import uuid; print(uuid.uuid4())')}"
export EXT_USER_DATA_DIR="${VERIFY_RUN_DIR}/chromium-profile"
export EXT_REPEAT="${REPEAT}"
export NODE_BIN="${NODE_BIN:-$(command -v node)}"
export RUN_DIR="${VERIFY_RUN_DIR}"
mkdir -p "${VERIFY_RUN_DIR}/evidence/extension" "${EXT_USER_DATA_DIR}"
export EXT_FIXTURE_PIDFILE="${VERIFY_RUN_DIR}/fixture.pid"

cleanup_fixture() {
  if [[ -f "${EXT_FIXTURE_PIDFILE}" ]]; then
    local pid
    pid="$(tr -d '[:space:]' < "${EXT_FIXTURE_PIDFILE}")"
    if [[ "${pid}" =~ ^[0-9]+$ ]] && kill -0 "${pid}" 2>/dev/null; then
      kill "${pid}" 2>/dev/null || true
      for _ in $(seq 1 20); do
        kill -0 "${pid}" 2>/dev/null || break
        sleep 0.1
      done
      if kill -0 "${pid}" 2>/dev/null; then
        kill -9 "${pid}" 2>/dev/null || true
      fi
      echo "Stopped fixture pid=${pid}"
    fi
  fi
  echo "Cleanup done. Evidence kept at ${VERIFY_EVIDENCE_DIR}"
}
trap cleanup_fixture EXIT

if [[ ! -f "${DIST_MANIFEST}" ]]; then
  echo "building extension (missing ${DIST_MANIFEST})"
  (cd "${EXT_ROOT}" && npm run build)
fi
[[ -f "${DIST_MANIFEST}" ]] || { echo "DOCTOR FAIL: missing ${DIST_MANIFEST}" >&2; exit 1; }

IDENTITY_ID="$(python3 -c 'import json; print(json.load(open("'"${EXT_ROOT}"'/chromium-identity.json"))["id"])')"
HOST_SHIM="${SCRIPT_DIR}/mock-native-host.sh"
chmod +x "${HOST_SHIM}" "${SCRIPT_DIR}/mock-native-host.mjs" "${SCRIPT_DIR}/ext-fixture-server.mjs"
mkdir -p "${EXT_USER_DATA_DIR}/NativeMessagingHosts"
python3 - "${EXT_USER_DATA_DIR}/NativeMessagingHosts/com.rusticdl.native_host.json" "${HOST_SHIM}" "${IDENTITY_ID}" <<'PY'
import json, sys
dest, host, ext_id = sys.argv[1:4]
open(dest, "w", encoding="utf-8").write(json.dumps({
    "name": "com.rusticdl.native_host",
    "description": "RusticDL Backend",
    "path": host,
    "type": "stdio",
    "allowed_origins": [f"chrome-extension://{ext_id}/"],
}, indent=2) + "\n")
PY
HOST_MANIFEST="${EXT_USER_DATA_DIR}/NativeMessagingHosts/com.rusticdl.native_host.json"
[[ -f "${HOST_MANIFEST}" ]] || { echo "DOCTOR FAIL: host manifest missing ${HOST_MANIFEST}" >&2; exit 1; }

: > "${VERIFY_RUN_DIR}/fixture.log"
node "${SCRIPT_DIR}/ext-fixture-server.mjs" >>"${VERIFY_RUN_DIR}/fixture.log" 2>&1 &
echo $! > "${EXT_FIXTURE_PIDFILE}"
for _ in $(seq 1 50); do
  if grep -q '^READY ' "${VERIFY_RUN_DIR}/fixture.log" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
grep -q '^READY ' "${VERIFY_RUN_DIR}/fixture.log" || {
  echo "DOCTOR FAIL: fixture did not become READY" >&2
  cat "${VERIFY_RUN_DIR}/fixture.log" >&2 || true
  exit 1
}
FIXTURE_PID="$(tr -d '[:space:]' < "${EXT_FIXTURE_PIDFILE}")"
kill -0 "${FIXTURE_PID}" 2>/dev/null || { echo "DOCTOR FAIL: fixture pid ${FIXTURE_PID} not running" >&2; exit 1; }

python3 - "${PORT}" "${FIXTURE_PID}" <<'PY'
import json, sys, urllib.request
port, pid = int(sys.argv[1]), int(sys.argv[2])
with urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=2) as resp:
    data = json.load(resp)
if data.get("pid") != pid:
    sys.stderr.write(f"DOCTOR FAIL: health pid {data.get('pid')} != pidfile {pid}\n")
    sys.exit(1)
print(f"DOCTOR fixture port={port} pid={pid}")
PY

echo "DOCTOR dist=${DIST_MANIFEST}"
echo "DOCTOR host-manifest=${HOST_MANIFEST}"
echo "DOCTOR run-dir=${VERIFY_RUN_DIR}"

export VERIFY_RUN_DIR
export EXT_CASE="${CASE_ID}"
export EXT_LIVE="${LIVE}"
cd "${EXT_ROOT}"
set +e
npx playwright test -c e2e/playwright.config.ts
PW_RC=$?
set -e
if [[ ! -f "${VERIFY_RUN_DIR}/doctor.json" ]]; then
  echo "DOCTOR FAIL: sentinel ping did not write doctor.json" >&2
  exit 1
fi
python3 - "${VERIFY_RUN_DIR}/doctor.json" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
if not data.get("ok"):
    sys.stderr.write(f"DOCTOR FAIL: sentinel ping failed: {data}\n")
    sys.exit(1)
print(f"DOCTOR ping ok version={data.get('browserVersion')} manifest={data.get('manifestPath')}")
PY

echo "prove-extension-capture exit=${PW_RC} evidence=${VERIFY_EVIDENCE_DIR}/extension"
exit "${PW_RC}"
