#!/usr/bin/env bash
# Capture the launched pid's RusticDL window into the evidence directory.
# Usage: screenshot.sh <filename.png>
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=env.sh
source "${SCRIPT_DIR}/env.sh"

name="${1:?filename required}"
case "${name}" in
  *..*|*/*|*\\*|.*|"")
    echo "invalid screenshot name (no slash, '..', or leading dot): ${name}" >&2
    exit 1
    ;;
esac
if [[ "${name}" != "$(basename -- "${name}")" ]]; then
  echo "invalid screenshot name: ${name}" >&2
  exit 1
fi

out="${VERIFY_EVIDENCE_DIR}/${name}"
mkdir -p "${VERIFY_EVIDENCE_DIR}"

wid_rc=0
WID="$(rusticdl_window_id)" || wid_rc=$?
if [[ "${wid_rc}" -eq 2 ]]; then
  echo "refusing screenshot: pid owns more than one RusticDL window" >&2
  exit 1
fi
[[ "${wid_rc}" -eq 0 && -n "${WID}" ]] || {
  echo "RusticDL window for launched pid not found" >&2
  exit 1
}

xdotool windowactivate --sync "${WID}"
sleep 0.2

if ! command -v import >/dev/null 2>&1; then
  echo "ImageMagick import is required to capture the window (no full-display fallback)" >&2
  exit 1
fi
if ! import -window "${WID}" "${out}"; then
  echo "window capture failed for wid=${WID}" >&2
  exit 1
fi

echo "WROTE ${out}"
