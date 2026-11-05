#!/bin/bash
# Writes flatpak/flathub/link.echoclip.Echo.yml: the same manifest, but building the PUBLISHED code
# (github.com/Resister0/echoclip at one exact commit) instead of this folder - that's what Flathub needs.
# Usage: bash flatpak/make-flathub-manifest.sh [commit]   (default: the public repo's newest commit)
set -e
cd "$(dirname "$0")"
REPO=https://github.com/Resister0/echoclip.git
C=${1:-$(git ls-remote "$REPO" refs/heads/main | cut -f1)}
[ -n "$C" ] || { echo "couldn't read the public repo"; exit 1; }
V=$(curl -fsSL "https://raw.githubusercontent.com/Resister0/echoclip/$C/ci/version.txt" | tr -d ' \r\n')
[ -n "$V" ] || { echo "no ci/version.txt in $C"; exit 1; }
mkdir -p flathub
cp cargo-sources.json libayatana-appindicator3-0.1.yml flathub/
python3 - "$C" "$V" <<'PY'
import sys, re
c, v = sys.argv[1], sys.argv[2]
s = open("link.echoclip.Echo.yml").read()
# the "echo" module's local folder source -> the public repo at that commit
s = re.sub(r"      - type: dir\n        path: \.\.\n        skip:\n(?:          - .*\n)+",
           f"      - type: git\n        url: https://github.com/Resister0/echoclip.git\n        commit: {c}\n", s)
s = s.replace("ECHO_VERSION: 0.1.0", f"ECHO_VERSION: {v}")
assert "path: ..\n" not in s and c in s
open("flathub/link.echoclip.Echo.yml", "w").write(s)
PY
echo "flathub/link.echoclip.Echo.yml -> Echo $V (commit $C)"
echo "Also add <release version=\"$V\" .../> to link.echoclip.Echo.metainfo.xml if it's new."
