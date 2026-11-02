#!/bin/bash
# Echo - Linux build (Nobara/Fedora). Run: bash build.sh
cd "$(dirname "$0")"
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
if ! command -v cargo >/dev/null; then
  echo "Installing Rust..."; curl -sSf https://sh.rustup.rs | sh -s -- -y; source "$HOME/.cargo/env"
fi
need="dbus-devel pkgconf-pkg-config webkit2gtk4.1-devel gtk3-devel libsoup3-devel librsvg2-devel alsa-lib-devel openssl-devel libappindicator-gtk3-devel wl-clipboard unzip libnotify"
missing=""; for p in $need; do rpm -q $p >/dev/null 2>&1 || missing="$missing $p"; done
command -v ffmpeg >/dev/null || missing="$missing ffmpeg-free"
[ -n "$missing" ] && { echo "Installing:$missing"; sudo dnf install -y --skip-broken $missing || exit 1; }
# speech recognition for auto captions (optional - the build goes on without it)
command -v whisper-cli >/dev/null || command -v whisper-cpp >/dev/null || rpm -q whisper-cpp >/dev/null 2>&1 || { echo "Installing whisper-cpp (auto captions)..."; sudo dnf install -y whisper-cpp || echo "(whisper-cpp not available - captions won't work until it's installed)"; }
touch src-tauri/src/main.rs
(cd src-tauri && cargo build --release) || { echo "BUILD FAILED - paste the errors to Claude"; exit 1; }
pkill -x echo 2>/dev/null; sleep 1
cp src-tauri/target/release/echo ./Echo || exit 1
# app menu entry + icon (Wayland shows the window/taskbar icon from the .desktop file)
mkdir -p ~/.local/share/applications ~/.local/share/icons/hicolor/256x256/apps
cp src-tauri/icons/icon.png ~/.local/share/icons/hicolor/256x256/apps/echo-clips.png
for id in dk.volkan.clipper echo; do
  nd=false; [ "$id" = echo ] && nd=true
  cat > ~/.local/share/applications/$id.desktop <<EOF
[Desktop Entry]
Type=Application
Name=Echo
Comment=Game clip recorder
Exec=$PWD/Echo
Path=$PWD
Icon=echo-clips
Terminal=false
Categories=AudioVideo;Video;
StartupWMClass=echo
NoDisplay=$nd
EOF
done
update-desktop-database ~/.local/share/applications 2>/dev/null
echo "Done: run ./Echo (or open Echo from the app menu)"
