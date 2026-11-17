#!/bin/bash
# Echo (Linux) - get the newest code from GitHub and save your changes there, WITHOUT a release.
# Run it when you start working on this PC and before you switch to Windows. Releases: publish.cmd on Windows.
set -e
cd "$(dirname "$0")"
# Sonela master (../CLAUDE.md, its own private repo Resister0/sonela-dev): save + get it first, whichever app is synced
if [ -d ../.git ]; then
  { git -C .. add -A && { git -C .. diff --cached --quiet || git -C .. commit -q -m "Sync (Linux)"; } &&
    git -C .. pull -q --rebase --autostash && git -C .. push -q && echo "Sonela master: synced"; } ||
    echo "(Sonela master: sync failed - the app sync goes on)"
fi
# 17 Nov 2026: syncs the branch you're on (main, or beta / dev for the update channels)
BR=$(git rev-parse --abbrev-ref HEAD); [ "$BR" != HEAD ] || { git switch -q main 2>/dev/null || true; BR=main; }
if git ls-remote --exit-code --heads origin "$BR" >/dev/null 2>&1; then
  git pull -q --rebase --autostash origin "$BR"
fi
mkdir -p .github/workflows && cp ci/build.yml .github/workflows/build.yml
git add -A
git add -f "Claude outputs" 2>/dev/null || true
git commit -q -m "Sync (Linux)" || echo "(nothing new to save)"
git push -q origin "HEAD:$BR"
echo "Saved to GitHub - no release. On Windows: sync.cmd (or publish.cmd to release)."
