#!/usr/bin/env bash
# Desktop UI helpers for the focused RusticDL window (xdotool geometry).
# Prefer vision/computerUse label clicks when available; these coords match
# the default 1120x720 window layout used in Linux verification.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=env.sh
source "${SCRIPT_DIR}/env.sh"

wid() {
  local w
  w="$(xdotool search --name '^RusticDL$' 2>/dev/null | head -1 || true)"
  [[ -n "${w}" ]] || { echo "RusticDL window not found" >&2; exit 1; }
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
    # Approximate Y positions for default sidebar density (library expanded).
    case "${label}" in
      "All downloads") ry=78 ;;
      Video) ry=110 ;;
      Audio) ry=140 ;;
      Compressed) ry=170 ;;
      Images) ry=200 ;;
      Documents) ry=230 ;;
      Programs) ry=260 ;;
      Other) ry=290 ;;
      Active) ry=330 ;;
      Completed) ry=360 ;;
      Failed) ry=390 ;;
      Settings) ry=560 ;;
      About) ry=595 ;;
      General) ry=100 ;;
      "Download Engine") ry=140 ;;
      System) ry=180 ;;
      Browser) ry=220 ;;
      Appearance) ry=260 ;;
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
    # Settings back control near top of settings sidebar.
    rel_click 40 90
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
  type-text)
    # GPUI does not reliably accept xdotool synthetic keystreams or clipboard
    # paste into InputState. Prefer a vision/computerUse agent for text entry.
    # This helper still focuses the window and attempts clipboard paste as a
    # best-effort fallback for environments where paste works.
    text="${2:?text required}"
    geom
    xdotool windowactivate --sync "${WID}"
    sleep 0.2
    if command -v xclip >/dev/null 2>&1; then
      printf '%s' "${text}" | xclip -selection clipboard
      xdotool key --clearmodifiers ctrl+v
    else
      xdotool type --clearmodifiers --delay 12 -- "${text}"
    fi
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
    echo "Usage: ui.sh {focus|click-add-download|focus-add-url|click-start-download|click-sidebar <label>|click-save-settings|leave-settings|focus-search|clear-search|type-text <text>|submit-dialog|pause-selection|resume-selection}" >&2
    exit 1
    ;;
esac
