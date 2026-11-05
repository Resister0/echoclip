# Echo as a Flatpak (own repo on flatpak.echoclip.link → Discover, GNOME Software)

App id: `link.echoclip.Echo`. Command inside the sandbox: `echo-clips`.

## Files

| File | What |
|---|---|
| `link.echoclip.Echo.yml` | the manifest: GNOME 50 runtime, Rust build, plus ffmpeg, gpu-screen-recorder, rclone, pactl/parec, wl-copy, tray library |
| `cargo-sources.json` | every Rust crate, so the build works offline (Flathub builds without internet). Regenerate with `bash flatpak/update-cargo-sources.sh` whenever `src-tauri/Cargo.lock` changes |
| `link.echoclip.Echo.metainfo.xml` | store page text, keywords, screenshots, releases |
| `link.echoclip.Echo.desktop` / `.png` | app menu entry + icon |
| `libayatana-appindicator3-0.1.yml` | tray icon library (copied from Flathub's GPU Screen Recorder manifest) |
| `make-flathub-manifest.sh` | (unused since Flathub is off) writes `flatpak/flathub/` = the manifest building the published code |

## What changes in Echo when it runs as a Flatpak (code: `linuxutil::in_flatpak()`)

- **Updates are off.** Flathub updates the app. The log says "update: Flatpak … - updates come from the software store".
- **Recording uses the ScreenCast portal** (`gpu-screen-recorder -w portal -restore-portal-session yes`). The KMS capture needs root rights, which the sandbox can't have. KDE/GNOME ask once which screen to share.
- **Hotkeys use the GlobalShortcuts portal** automatically, because the sandbox can't read `/dev/input`. KDE shows a dialog to confirm the keys.
- **File and folder pickers use the FileChooser portal** (kdialog/zenity aren't in the sandbox).
- **"Start with the computer" goes through the Background portal.**
- **rclone comes from `/app/bin`** instead of being downloaded.
- **Game detection uses sound streams.** The sandbox can't see other programs, so Echo finds the game by its sound (pactl): Wine/Proton `.exe` games, Minecraft, and native games Echo has seen before. The log says "game: X (Flatpak: found by its sound …)". A game is only found once it has made a sound.
- **Captions use a bundled whisper.cpp** (`whisper-cli`). The speech models still download on first use.

## Stage 2 - William's test on Nobara (about 15 min plus the build)

```bash
# one-time: the builder + Flathub remote
flatpak install -y flathub org.flatpak.Builder
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo

# build + install (from the repo root; first build ~20-40 min: ffmpeg, gpu-screen-recorder, Echo)
cd ~/Desktop/Echo
flatpak run org.flatpak.Builder --user --install --force-clean --install-deps-from=flathub build-dir flatpak/link.echoclip.Echo.yml

# run it (close the normal Echo first - only one Echo can run)
flatpak run link.echoclip.Echo
```

If the build fails, paste the last ~40 lines to Claude. Check `pactl` in the runtime:
`flatpak run --command=sh link.echoclip.Echo -c 'ls -l /usr/bin/pactl /usr/bin/parec /app/bin/pactl'`
(if `/usr/bin` has both, the pulseaudio-utils module can be removed).

Checklist (then Settings → Send log):

1. Echo opens with the right icon in the taskbar and the tray.
2. Recording: the screen-share dialog shows. Pick the monitor and allow "remember". Restart Echo: it should not ask again.
3. Hotkeys: KDE's shortcut dialog shows. Confirm it, then press the save key in a game. The clip appears.
4. Audio: game sound, mic, and per-app tracks (Discord/browser) in the saved clip.
5. Player: clips play smoothly, and seek-bar previews and thumbnails show.
6. Copy a clip and paste it into Discord (wl-copy).
7. Cloud: sign in, upload, make a share link.
8. Export from the editor (ffmpeg), MP3 export, captions (see "Not done yet").
9. Settings → "Start with the computer" (the Background portal may ask), then log out and back in.
10. Settings → Clips folder → Change… (the portal's folder dialog).

Settings live in `~/.var/app/link.echoclip.Echo/config/Clipper`. They are not shared with the normal Echo, so you sign in to the cloud again.
Clips stay in `~/Videos/EchoClips` (shared).

## Echo's own Flatpak repo (what users install)

Flathub is off: its rules don't allow AI-written manifests or apps that are mostly AI-written (see `Claude outputs/ECHO_HANDOFF_chat23.md`, chat 27).
Instead every publish builds the Flatpak in GitHub Actions and adds it to Echo's own repo:

- **Build**: job `flatpak` in `ci/public-build.yml` runs `ci/flatpak-repo.sh` in the public repo: this manifest, `ECHO_VERSION` from `ci/version.txt`, signed with GPG, last 3 versions kept, small update downloads (static deltas).
- **Hosting**: Cloudflare R2 bucket `echo-flatpak`, public on `https://flatpak.echoclip.link` (`repo/` = the OSTree repo, `echo.flatpakref` = one-click install, `echo.flatpakrepo` = add the repo).
- **Website**: "Install with Flatpak" on echoclip.link/download → `/download/flatpak` (Worker, counted like the other downloads) → `echo.flatpakref`. Discover / GNOME Software open it; the GNOME runtime comes from Flathub (`RuntimeRepo`), and updates show up in the software center.
- **Secrets** (in github.com/Resister0/echoclip → Settings → Secrets → Actions): `FLATPAK_GPG_KEY`, `R2_ACCOUNT_ID`, `R2_FLATPAK_ACCESS_KEY_ID`, `R2_FLATPAK_SECRET_ACCESS_KEY`. Without them the job is skipped and nothing else changes.
- **By hand from the command line**: `flatpak install --user https://flatpak.echoclip.link/echo.flatpakref`
- **Release notes** in the software center come from the metainfo: `ci/build.yml` adds a `<release>` for every publish to the public copy (`ci/metainfo-release.py`).
- **Screenshots** for the software center: `cloud/site/screenshots/{library,player,editor,settings}.png` (deployed with the website).

## Not done yet (Flatpak only)

- **Silent native games**: a native Linux game Echo hasn't seen before isn't found by its sound. That would need the active window from KWin / GNOME Shell (as GPU Screen Recorder does).
- **Clips folder outside ~/Videos**: the sandbox only sees `xdg-videos`. A folder picked in Settings goes through the portal, which gives Echo access to it (its path looks like `/run/user/…/doc/…`). Otherwise use Flatseal.
- **In-game popup over fullscreen Wayland games**: not possible (same as the normal build). Notifications are used instead.
- **Flathub rules**: brand names (Medal, ShadowPlay, OBS…) are left out of the keywords on purpose.
