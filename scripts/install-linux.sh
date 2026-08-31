#!/usr/bin/env bash
# Per-user Linux install: copy binaries to ~/.local/lib/RusticDL, put rusticdl on PATH,
# write a desktop entry, and register the native messaging host (no sudo).
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Tarball layout is flat (binaries next to this script) or scripts/ lives beside binaries.
if [ -f "$script_dir/rusticdl" ]; then
  source_root="$script_dir"
elif [ -f "$script_dir/../rusticdl" ]; then
  source_root="$(cd "$script_dir/.." && pwd)"
else
  echo "Could not find rusticdl next to install-linux.sh." >&2
  echo "Extract RusticDL-linux-x64.tar.gz and run ./install-linux.sh from that folder." >&2
  exit 1
fi

prefix="${RUSTICDL_INSTALL_PREFIX:-$HOME/.local/lib/RusticDL}"
bin_dir="${RUSTICDL_BIN_DIR:-$HOME/.local/bin}"
app_dir="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
icon_dir="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/256x256/apps"

mkdir -p "$prefix" "$bin_dir" "$app_dir" "$icon_dir" "$prefix/scripts"

copy_if_present() {
  local src="$1"
  local dest="$2"
  if [ -f "$src" ]; then
    cp -f "$src" "$dest"
    chmod +x "$dest" 2>/dev/null || true
  fi
}

copy_if_present "$source_root/rusticdl" "$prefix/rusticdl"
copy_if_present "$source_root/rusticdl-native-host" "$prefix/rusticdl-native-host"
copy_if_present "$source_root/rusticdl-updater" "$prefix/rusticdl-updater"

if [ ! -x "$prefix/rusticdl" ]; then
  echo "rusticdl was not copied to $prefix" >&2
  exit 1
fi

for name in register-native-host.sh unregister-native-host.sh install-linux.sh; do
  if [ -f "$source_root/scripts/$name" ]; then
    cp -f "$source_root/scripts/$name" "$prefix/scripts/$name"
    chmod +x "$prefix/scripts/$name"
  elif [ -f "$source_root/$name" ]; then
    cp -f "$source_root/$name" "$prefix/scripts/$name"
    chmod +x "$prefix/scripts/$name"
  fi
done

for template in chromium.template.json edge.template.json firefox.template.json chromium-identity.json; do
  if [ -f "$source_root/scripts/$template" ]; then
    cp -f "$source_root/scripts/$template" "$prefix/scripts/$template"
  fi
done

ln -sfn "$prefix/rusticdl" "$bin_dir/rusticdl"

icon_src=""
for candidate in \
  "$source_root/assets/brand/icon-256.png" \
  "$source_root/icon-256.png" \
  "$script_dir/../assets/brand/icon-256.png"
do
  if [ -f "$candidate" ]; then
    icon_src="$candidate"
    break
  fi
done
if [ -n "$icon_src" ]; then
  cp -f "$icon_src" "$icon_dir/rusticdl.png"
fi

desktop_src=""
for candidate in \
  "$source_root/rusticdl.desktop" \
  "$source_root/assets/linux/rusticdl.desktop" \
  "$script_dir/../assets/linux/rusticdl.desktop"
do
  if [ -f "$candidate" ]; then
    desktop_src="$candidate"
    break
  fi
done

desktop_dest="$app_dir/rusticdl.desktop"
if [ -n "$desktop_src" ]; then
  sed "s|^Exec=.*|Exec=$prefix/rusticdl|" "$desktop_src" > "$desktop_dest"
  chmod 644 "$desktop_dest"
else
  cat > "$desktop_dest" <<EOF
[Desktop Entry]
Version=1.0
Type=Application
Name=RusticDL
Comment=Local-first HTTP(S) download manager
Exec=$prefix/rusticdl
Icon=rusticdl
Terminal=false
Categories=Network;FileTransfer;
StartupWMClass=RusticDL
EOF
fi

if [ -x "$prefix/scripts/register-native-host.sh" ] && [ -f "$prefix/rusticdl-native-host" ]; then
  "$prefix/scripts/register-native-host.sh" \
    --host-binary "$prefix/rusticdl-native-host" \
    --desktop-binary "$prefix/rusticdl" \
    --install-root "$prefix" \
    --quiet
fi

echo "Installed RusticDL to $prefix"
echo "  PATH symlink : $bin_dir/rusticdl"
echo "  Desktop entry: $desktop_dest"
echo
echo "If '$bin_dir' is not on your PATH, add it to ~/.profile:"
echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
echo
echo "Load the browser extension separately (see docs/browser-extension.md)."
echo "Snap/Flatpak browsers may not see user-level native messaging hosts."
