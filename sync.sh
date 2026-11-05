#!/bin/bash
# Echo (Linux) - get the newest code from GitHub and save your changes there, WITHOUT a release.
# Run it when you start working on this PC and before you switch to Windows. Releases: publish.cmd on Windows.
set -e
cd "$(dirname "$0")"
git switch -q main 2>/dev/null || true
git pull -q --rebase --autostash origin main
mkdir -p .github/workflows && cp ci/build.yml .github/workflows/build.yml
git add -A
git add -f "Claude outputs" 2>/dev/null || true
git commit -q -m "Sync (Linux)" || echo "(nothing new to save)"
git push -q origin HEAD:main
echo "Saved to GitHub - no release. On Windows: sync.cmd (or publish.cmd to release)."
