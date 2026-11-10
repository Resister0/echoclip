#!/bin/bash
# Echo - Linux build (Nobara/Fedora). Run: bash build.sh
# 2.0.0 (10 Nov 2026): builds the native app (prototype-native: Slint + mpv) - the only Echo now (no Tauri build).
cd "$(dirname "$0")"
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
if ! command -v cargo >/dev/null; then
  echo "Installing Rust..."; curl -sSf https://sh.rustup.rs | sh -s -- -y; source "$HOME/.cargo/env"
fi
# mpv-libs = libmpv.so.2, the video player (loaded at runtime); fontconfig + xkbcommon for Slint; D-Bus + ALSA for echo-core
need="dbus-devel pkgconf-pkg-config alsa-lib-devel fontconfig-devel libxkbcommon-devel mpv-libs wl-clipboard unzip libnotify"
missing=""; for p in $need; do rpm -q $p >/dev/null 2>&1 || missing="$missing $p"; done
command -v ffmpeg >/dev/null || missing="$missing ffmpeg-free"
[ -n "$missing" ] && { echo "Installing:$missing"; sudo dnf install -y --skip-broken $missing || exit 1; }
# speech recognition for auto captions (optional - the build goes on without it)
command -v whisper-cli >/dev/null || command -v whisper-cpp >/dev/null || rpm -q whisper-cpp >/dev/null 2>&1 || { echo "Installing whisper-cpp (auto captions)..."; sudo dnf install -y whisper-cpp || echo "(whisper-cpp not available - captions won't work until it's installed)"; }
(cd prototype-native && cargo build --release) || { echo "BUILD FAILED - paste the errors to Claude"; exit 1; }
bin=prototype-native/target/release/echo-native-proto
pkill -x Echo 2>/dev/null; pkill -x echo 2>/dev/null; sleep 1
cp "$bin" ./Echo || exit 1
# app menu entry + icon (Wayland shows the window/taskbar icon from the .desktop file). The window's Wayland app_id /
# X11 class is "echo" (native app: prototype-native/src/popup.rs) -> echo.desktop + StartupWMClass=echo find the icon.
mkdir -p ~/.local/share/applications ~/.local/share/icons/hicolor/256x256/apps
cp src-tauri/icons/icon.png ~/.local/share/icons/hicolor/256x256/apps/echo-clips.png
for id in dk.volkan.clipper echo; do
  nd=false; [ "$id" = echo ] && nd=true
  cat > ~/.local/share/applications/$id.desktop <<EOF
[Desktop Entry]
Type=Application
Name=Echo (dev)
Comment=Your own build from this folder
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
