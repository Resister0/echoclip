#!/bin/bash
# Echo native prototype - Linux build (Nobara/Fedora). Run: bash build-proto.sh
cd "$(dirname "$0")"
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
need="mpv-libs fontconfig-devel libxkbcommon-devel"
missing=""; for p in $need; do rpm -q $p >/dev/null 2>&1 || missing="$missing $p"; done
[ -n "$missing" ] && { echo "Installing:$missing"; sudo dnf install -y $missing || exit 1; }
cargo build --release || { echo "BUILD FAILED - paste the errors to Claude"; exit 1; }
cp target/release/echo-native-proto ./EchoProto && echo "Built: $PWD/EchoProto - starting it..." && ./EchoProto
