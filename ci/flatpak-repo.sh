#!/bin/bash
# Builds Echo as a Flatpak and adds it to Echo's own Flatpak repo (https://flatpak.echoclip.link/repo/),
# so Discover / GNOME Software install it from https://echoclip.link/download/flatpak and show its updates.
# Run by ci/public-build.yml (job "flatpak") in the public repo, from the repo root. Needs:
#   FLATPAK_GPG_KEY            armored private GPG key that signs the repo (no passphrase)
#   R2_ACCOUNT_ID, R2_FLATPAK_ACCESS_KEY_ID, R2_FLATPAK_SECRET_ACCESS_KEY   Cloudflare R2, bucket echo-flatpak
# Optional: R2_FLATPAK_BUCKET (default echo-flatpak), FLATPAK_URL (default https://flatpak.echoclip.link)
set -euo pipefail

BUCKET=${R2_FLATPAK_BUCKET:-echo-flatpak}
BASE=${FLATPAK_URL:-https://flatpak.echoclip.link}
APP=link.echoclip.Echo
T=${RUNNER_TEMP:-/tmp}/flatpak
REPO=$T/repo
mkdir -p "$T"

V=$(tr -d ' \r\n' < ci/version.txt 2>/dev/null || true); [ -n "$V" ] || V=1.0.0
echo "== Echo $V"

# ---- signing key ----
export GNUPGHOME=$T/gnupg
mkdir -p -m 700 "$GNUPGHOME"
printf '%s\n' "$FLATPAK_GPG_KEY" | gpg --batch --quiet --import
KEY=$(gpg --batch --list-secret-keys --with-colons | awk -F: '/^fpr/{print $10; exit}')
[ -n "$KEY" ] || { echo "FLATPAK_GPG_KEY: no private key in it"; exit 1; }
PUB=$(gpg --batch --export "$KEY" | base64 -w0)
echo "signing key $KEY"

# ---- R2 (rclone reads its remote "r2" from these variables) ----
export RCLONE_CONFIG_R2_TYPE=s3 RCLONE_CONFIG_R2_PROVIDER=Cloudflare RCLONE_CONFIG_R2_REGION=auto \
  RCLONE_CONFIG_R2_ENDPOINT="https://${R2_ACCOUNT_ID}.r2.cloudflarestorage.com" \
  RCLONE_CONFIG_R2_ACCESS_KEY_ID="$R2_FLATPAK_ACCESS_KEY_ID" RCLONE_CONFIG_R2_SECRET_ACCESS_KEY="$R2_FLATPAK_SECRET_ACCESS_KEY" \
  RCLONE_CONFIG_R2_NO_CHECK_BUCKET=true
RC="rclone --fast-list --transfers 32 --checkers 32 --retries 5 --stats-one-line --stats 30s"

# ---- the repo as it is now (keeps the older versions, so updates can download only what changed) ----
mkdir -p "$REPO"
$RC sync "r2:$BUCKET/repo" "$REPO" || echo "no repo yet - starting a new one"
[ -f "$REPO/config" ] || ostree init --mode=archive-z2 --repo="$REPO"

# ---- build (same manifest as the local test build; the "echo" module builds this checkout) ----
sed -i -E "s/(ECHO_VERSION: )0\.1\.0 /\1$V /" flatpak/$APP.yml
grep -q "ECHO_VERSION: $V " flatpak/$APP.yml || { echo "couldn't set ECHO_VERSION in the manifest"; exit 1; }
# The system's flatpak-builder, not the org.flatpak.Builder Flatpak: that one installs the SDK through the host's
# D-Bus session, which a CI runner doesn't have ("Cannot autolaunch D-Bus without X11 $DISPLAY", run #34).
# SDK + runtime + Rust first, straight from Flathub.
flatpak install --user -y --noninteractive flathub org.gnome.Sdk//50 org.gnome.Platform//50 \
  org.freedesktop.Sdk.Extension.rust-stable//25.08
flatpak-builder --user --force-clean --disable-rofiles-fuse --install-deps-from=flathub \
  --default-branch=stable --state-dir="$T/state" --repo="$REPO" \
  --subject="Echo $V" "$T/build" flatpak/$APP.yml

# ---- sign the new commits, update the summary (+ deltas = small updates), keep the last 3 versions ----
for ref in $(ostree refs --repo="$REPO" | grep -E "^(app|runtime)/$APP"); do
  # sign each commit once (ostree would add the same signature again every run)
  ostree show --repo="$REPO" --gpg-homedir="$GNUPGHOME" "$ref" 2>/dev/null | grep -q "key ID ${KEY: -16}" && continue
  ostree gpg-sign --repo="$REPO" --gpg-homedir="$GNUPGHOME" "$ref" "$KEY"
done
flatpak build-update-repo --title="Echo" --default-branch=stable --generate-static-deltas --prune --prune-depth=2 \
  --gpg-sign="$KEY" --gpg-homedir="$GNUPGHOME" "$REPO"

# ---- install files: .flatpakref (one-click install) + .flatpakrepo (add the repo) ----
cat > "$T/echo.flatpakref" <<EOF
[Flatpak Ref]
Title=Echo
Name=$APP
Branch=stable
Url=$BASE/repo/
SuggestRemoteName=echoclip
Homepage=https://echoclip.link
Icon=$BASE/echo.png
RuntimeRepo=https://dl.flathub.org/repo/flathub.flatpakrepo
IsRuntime=false
GPGKey=$PUB
EOF
cat > "$T/echo.flatpakrepo" <<EOF
[Flatpak Repo]
Title=Echo
Url=$BASE/repo/
Homepage=https://echoclip.link
Comment=Echo - save your best game moments with one key
Icon=$BASE/echo.png
GPGKey=$PUB
EOF

# ---- upload: objects + deltas first, then refs/summary (clients never see a version whose files aren't there yet),
# then drop what the prune removed ----
$RC copy "$REPO/objects" "r2:$BUCKET/repo/objects"
[ -d "$REPO/deltas" ] && $RC copy "$REPO/deltas" "r2:$BUCKET/repo/deltas"
$RC sync "$REPO" "r2:$BUCKET/repo" --exclude "/tmp/**" --exclude "/state/**" --exclude "/.lock"
$RC copyto "$T/echo.flatpakref" "r2:$BUCKET/echo.flatpakref" --header-upload "Content-Type: application/vnd.flatpak.ref" --header-upload "Cache-Control: no-cache"
$RC copyto "$T/echo.flatpakrepo" "r2:$BUCKET/echo.flatpakrepo" --header-upload "Content-Type: application/vnd.flatpak.repo" --header-upload "Cache-Control: no-cache"
$RC copyto flatpak/$APP.png "r2:$BUCKET/echo.png"
echo "Flatpak repo updated: Echo $V -> $BASE/echo.flatpakref"
