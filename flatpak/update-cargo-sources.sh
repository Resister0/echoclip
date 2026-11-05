#!/bin/bash
# Regenerates flatpak/cargo-sources.json (the offline copy list of every Rust crate) after Cargo.lock changes.
# Run from anywhere: bash flatpak/update-cargo-sources.sh
set -e
cd "$(dirname "$0")"
t=$(mktemp -d)
git clone --depth 1 -q https://github.com/flatpak/flatpak-builder-tools.git "$t/fbt"
python3 -m venv "$t/venv" && "$t/venv/bin/pip" install -q aiohttp tomlkit
"$t/venv/bin/python" "$t/fbt/cargo/flatpak-cargo-generator.py" -o cargo-sources.json ../src-tauri/Cargo.lock
rm -rf "$t"
echo "cargo-sources.json updated ($(grep -c static.crates.io cargo-sources.json) crates)"
