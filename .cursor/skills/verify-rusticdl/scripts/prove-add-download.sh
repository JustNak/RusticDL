#!/usr/bin/env bash
# End-to-end proof scaffold for features/add-download.md.
#
# Launch + doctor + evidence dirs are fully scripted here. GPUI text entry into
# the Add download URL field must be performed by a vision/computerUse agent
# (see SKILL.md Drive section) between the markers below — xdotool cannot
# reliably type into GPUI InputState.
#
# Usage:
#   Interactive/agent proof (recommended):
#     export RUN_ID=prove-add-$(date +%Y%m%d%H%M%S)
#     .cursor/skills/verify-rusticdl/scripts/prove-add-download.sh prepare
#     # agent: open Add download, type https://httpbin.org/bytes/4096, Start download
#     # agent: save dialog-filled.png + queue-after.png into $VERIFY_EVIDENCE_DIR
#     .cursor/skills/verify-rusticdl/scripts/prove-add-download.sh finish
#
#   Or run with PROVE_URL_ALREADY_DONE=1 after the agent filled evidence files.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODE="${1:-all}"
export RUN_ID="${RUN_ID:-prove-add-$(date +%Y%m%d%H%M%S)}"
# shellcheck source=env.sh
source "${SCRIPT_DIR}/env.sh"

FEATURE_ID=add-download
ART_DIR="${VERIFY_ARTIFACTS_DIR}/${FEATURE_ID}/${RUN_ID}"
PROOF_URL='https://httpbin.org/bytes/4096'
mkdir -p "${ART_DIR}" "${VERIFY_EVIDENCE_DIR}"

cleanup_on_fail() {
  echo "Proof failed — running cleanup" >&2
  "${SCRIPT_DIR}/cleanup.sh" || true
}

prepare() {
  "${SCRIPT_DIR}/launch.sh"
  "${SCRIPT_DIR}/doctor.sh" | tee "${VERIFY_EVIDENCE_DIR}/doctor.txt"
  "${SCRIPT_DIR}/ipc.sh" get_status | tee "${VERIFY_EVIDENCE_DIR}/status-before.json"
  "${SCRIPT_DIR}/ui.sh" click-add-download
  sleep 0.8
  "${SCRIPT_DIR}/screenshot.sh" dialog-open.png
  cat >"${VERIFY_EVIDENCE_DIR}/AGENT_DRIVE.txt" <<EOF
Drive steps for computerUse / vision agent:
1. Focus the RusticDL window (dialog titled Add download should be open).
2. Click the URL field (placeholder https://example.com/file.zip).
3. Type exactly: ${PROOF_URL}
4. Save screenshot: ${VERIFY_EVIDENCE_DIR}/dialog-filled.png (URL visible).
5. Click Start download.
6. Wait until a queue row appears (completed is OK).
7. Save screenshot: ${VERIFY_EVIDENCE_DIR}/queue-after.png
Then run: RUN_ID=${RUN_ID} ${SCRIPT_DIR}/prove-add-download.sh finish
EOF
  echo "PREPARE OK run=${VERIFY_RUN_DIR}"
  echo "Next: follow ${VERIFY_EVIDENCE_DIR}/AGENT_DRIVE.txt then run finish"
}

finish() {
  trap cleanup_on_fail ERR
  [[ -f "${VERIFY_EVIDENCE_DIR}/dialog-filled.png" ]] || {
    echo "missing dialog-filled.png — agent must type the URL first" >&2
    exit 1
  }
  [[ -f "${VERIFY_EVIDENCE_DIR}/queue-after.png" ]] || {
    echo "missing queue-after.png — agent must start the download first" >&2
    exit 1
  }

  ok=0
  for _ in $(seq 1 20); do
    AFTER="$("${SCRIPT_DIR}/ipc.sh" get_status)"
    echo "${AFTER}" >"${VERIFY_EVIDENCE_DIR}/status-after.json"
    if echo "${AFTER}" | grep -Eq '"total":[1-9]'; then
      ok=1
      break
    fi
    sleep 0.5
  done
  [[ "${ok}" -eq 1 ]] || { echo "Queue never showed a job" >&2; exit 1; }

  {
    echo "feature=${FEATURE_ID}"
    echo "run_id=${RUN_ID}"
    echo "pid=$(cat "${VERIFY_RUN_DIR}/app.pid" 2>/dev/null || echo unknown)"
    echo "url=${PROOF_URL}"
    echo "entry=Add download button + Start download (vision-typed URL)"
    echo "commands=launch.sh doctor.sh ui.sh click-add-download computerUse-type ipc.sh get_status cleanup.sh"
  } | tee "${VERIFY_EVIDENCE_DIR}/meta.txt" >"${ART_DIR}/meta.txt"

  cp -a "${VERIFY_EVIDENCE_DIR}/." "${ART_DIR}/"
  "${SCRIPT_DIR}/cleanup.sh"
  trap - ERR

  [[ -f "${ART_DIR}/queue-after.png" ]] || { echo "artifact missing after cleanup" >&2; exit 1; }
  [[ -f "${ART_DIR}/status-after.json" ]] || { echo "status artifact missing" >&2; exit 1; }
  [[ -f "${VERIFY_EVIDENCE_DIR}/queue-after.png" ]] || { echo "evidence dir wiped" >&2; exit 1; }
  echo "PROVE OK artifacts=${ART_DIR}"
}

case "${MODE}" in
  prepare) prepare ;;
  finish) finish ;;
  all)
    echo "Mode 'all' requires a vision agent for URL typing." >&2
    echo "Run: $0 prepare   then drive per AGENT_DRIVE.txt   then $0 finish" >&2
    exit 2
    ;;
  *)
    echo "Usage: $0 {prepare|finish}" >&2
    exit 2
    ;;
esac
