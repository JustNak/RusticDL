#!/usr/bin/env bash
# Desktop UI helpers for the launched pid's RusticDL window (xdotool geometry).
# Prefer vision/computerUse label clicks when available; these coords match
# the default 1120x720 window layout used in Linux verification.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=env.sh
source "${SCRIPT_DIR}/env.sh"

wid() {
  local w
  local rc=0
  w="$(rusticdl_window_id)" || rc=$?
  if [[ "${rc}" -eq 2 ]]; then
    echo "pid owns more than one RusticDL window; refusing to click" >&2
    exit 1
  fi
  [[ "${rc}" -eq 0 && -n "${w}" ]] || { echo "RusticDL window for launched pid not found" >&2; exit 1; }
  echo "${w}"
}

geom() {
  local w
  w="$(wid)"
  eval "$(xdotool getwindowgeometry --shell "${w}")"
  export WID="${w}" GX="${X}" GY="${Y}" GW="${WIDTH}" GH="${HEIGHT}"
}

click_xy() {
  local absx="$1" absy="$2"
  xdotool windowactivate --sync "${WID}"
  sleep 0.15
  xdotool mousemove --sync "${absx}" "${absy}"
  sleep 0.05
  xdotool click 1
  sleep 0.25
}

rel_click() {
  # Relative to window client origin from getwindowgeometry.
  local rx="$1" ry="$2"
  geom
  click_xy $((GX + rx)) $((GY + ry))
}

cmd="${1:-}"
case "${cmd}" in
  focus)
    geom
    xdotool windowactivate --sync "${WID}"
    ;;
  click-add-download)
    # Title-bar primary button, left of window controls.
    geom
    rel_click $((GW - 160)) 24
    sleep 0.6
    ;;
  focus-add-url)
    # Centered dialog URL field (non-advanced, single URL).
    geom
    rel_click $((GW / 2)) 210
    sleep 0.2
    ;;
  click-start-download)
    # Dialog footer primary button (right side of dialog).
    geom
    rel_click $((GW / 2 + 90)) 318
    sleep 0.8
    ;;
  click-sidebar)
    label="${2:?sidebar label required}"
    geom
    # Brand 48 + pt_1 4; type rows 32, queue/settings rows 36, gap 2.
    # Settings/About sit under flex_1 (bottom pad 12). Settings-sidebar
    # Back is the first row; categories follow divider + SETTINGS header.
    case "${label}" in
      "All downloads") ry=70 ;;
      Video) ry=106 ;;
      Audio) ry=140 ;;
      Compressed) ry=174 ;;
      Images) ry=208 ;;
      Documents) ry=242 ;;
      Programs) ry=276 ;;
      Other) ry=310 ;;
      Active) ry=346 ;;
      Completed) ry=384 ;;
      Failed) ry=422 ;;
      Settings) ry=$((GH - 68)) ;;
      About) ry=$((GH - 30)) ;;
      General) ry=145 ;;
      "Download Engine") ry=183 ;;
      System) ry=221 ;;
      Browser) ry=259 ;;
      Appearance) ry=297 ;;
      *) echo "Unknown sidebar label: ${label}" >&2; exit 1 ;;
    esac
    # Sidebar is ~220px wide; click mid-label.
    rel_click 110 "${ry}"
    sleep 0.5
    ;;
  click-save-settings)
    geom
    # Sticky footer Save settings (right side).
    rel_click $((GW - 120)) $((GH - 28))
    sleep 0.5
    ;;
  leave-settings)
    geom
    # Settings Back row (36px) starts at y=52.
    rel_click 40 70
    sleep 0.5
    ;;
  focus-search)
    geom
    rel_click $((GW / 2)) 24
    sleep 0.2
    ;;
  clear-search)
    geom
    rel_click $((GW / 2)) 24
    xdotool key ctrl+a BackSpace
    sleep 0.2
    ;;
  submit-dialog)
    # Confirm dialogs accept Return when the primary action is default.
    geom
    xdotool windowactivate --sync "${WID}"
    xdotool key --clearmodifiers Return
    ;;
  pause-selection|resume-selection)
    # Space toggles pause/resume for current selection.
    geom
    xdotool windowactivate --sync "${WID}"
    xdotool key space
    sleep 0.5
    ;;
  *)
    echo "Usage: ui.sh {focus|click-add-download|focus-add-url|click-start-download|click-sidebar <label>|click-save-settings|leave-settings|focus-search|clear-search|submit-dialog|pause-selection|resume-selection}" >&2
    exit 1
    ;;
esac
