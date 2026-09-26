#!/usr/bin/env bash
# Capture the RusticDL window (or full display) into the evidence directory.
# Usage: screenshot.sh <filename.png>
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=env.sh
source "${SCRIPT_DIR}/env.sh"

name="${1:?filename required}"
out="${VERIFY_EVIDENCE_DIR}/${name}"
mkdir -p "${VERIFY_EVIDENCE_DIR}"

WID="$(xdotool search --name '^RusticDL$' 2>/dev/null | head -1 || true)"
if [[ -n "${WID}" ]]; then
  xdotool windowactivate --sync "${WID}" || true
  sleep 0.2
fi

# Prefer ImageMagick import of the window; fall back to full-display scrot.
if [[ -n "${WID}" ]] && command -v import >/dev/null 2>&1; then
  import -window "${WID}" "${out}" || scrot "${out}"
elif command -v scrot >/dev/null 2>&1; then
  scrot "${out}"
else
  echo "No screenshot tool (import/scrot) available" >&2
  exit 1
fi

echo "WROTE ${out}"
