#!/usr/bin/env bash
# Append GNU sha256sum lines from EXTRA files into DEST without dropping DEST lines.
# If a basename is already present in DEST, the existing line wins (so a Windows
# listing cannot replace the Linux tarball). Missing EXTRA files are skipped.
set -euo pipefail

if [ "$#" -lt 1 ]; then
  echo "usage: merge-sha256sums.sh DEST [EXTRA...]" >&2
  exit 2
fi

dest="$1"
shift

dest_dir="$(dirname "$dest")"
mkdir -p "$dest_dir"
touch "$dest"

line_basename() {
  local line="$1"
  local name=""
  # GNU: "<hash>  filename" or "<hash> *filename"
  name="$(printf '%s\n' "$line" | awk '{print $2}')"
  name="${name#\*}"
  name="${name##*/}"
  name="${name##*\\}"
  printf '%s' "$name"
}

dest_has_basename() {
  local want="$1"
  local line name
  while IFS= read -r line || [ -n "$line" ]; do
    line="${line#"${line%%[![:space:]]*}"}"
    [ -z "$line" ] && continue
    [ "${line:0:1}" = "#" ] && continue
    name="$(line_basename "$line")"
    if [ "$name" = "$want" ]; then
      return 0
    fi
  done < "$dest"
  return 1
}

if [ -s "$dest" ]; then
  last="$(tail -c 1 "$dest" || true)"
  if [ -n "$last" ]; then
    printf '\n' >> "$dest"
  fi
fi

for extra in "$@"; do
  [ -f "$extra" ] || continue
  while IFS= read -r line || [ -n "$line" ]; do
    trimmed="${line#"${line%%[![:space:]]*}"}"
    trimmed="${trimmed%"${trimmed##*[![:space:]]}"}"
    [ -z "$trimmed" ] && continue
    [ "${trimmed:0:1}" = "#" ] && continue
    name="$(line_basename "$trimmed")"
    [ -z "$name" ] && continue
    if dest_has_basename "$name"; then
      continue
    fi
    printf '%s\n' "$trimmed" >> "$dest"
  done < "$extra"
done
