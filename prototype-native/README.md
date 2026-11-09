# Echo native UI prototype (Slint + libmpv)

Standalone experiment - NOT part of Echo. It reads your clips folder and Echo's existing thumbnails
(`<clips>/.thumbs/<name>.jpg`, `.dur`, `.scrub.jpg`). It WRITES only Echo's `favorites.json` and `meta.json`
(favourites, tags, notes - same format Echo uses, temp file + rename, other fields kept). Rename / Move / Delete /
Make smaller only show a message until the real port calls Echo's recorder.rs.

- Windows: `build-proto.cmd` (offers to fetch libmpv-2.dll the first time, builds, starts `EchoProto.exe`)
- Linux (Nobara): `bash build-proto.sh` (installs mpv-libs + 2 dev packages, builds, starts `./EchoProto`)

Clips folder: Echo's `clips_dir` from its settings.json, else `~/Videos/EchoClips`. Override: `EchoProto <folder>`.

## Library
Sidebar: All clips / Today / This week / Favourites / Edited, one entry per game folder, one per tag.
Toolbar: Filter (Game / Type / Tags - several picks in one section = any, sections combine; picks show as pills,
click a pill to remove it), search (name, folder, tags, note), sort (Newest / Oldest / Largest / Longest / Name),
grid / list view. Date groups (Today / Yesterday / This week / month) when sorted by date.
Cards: hover = lift + flipbook preview (60 pictures from the seek sheet) + Favourite / Show in folder / Delete
buttons; right-click = menu; tick in the corner or Ctrl/Shift+click = select (then a plain click selects too),
Ctrl+A = all, Esc = clear; the selection bar can favourite them. Tooltips on buttons.

## Player
Click a card. Space = pause, Left/Right = 5 s, Shift+Left/Right = previous / next clip, Esc or click beside = close.
Hover the seek bar = picture preview. Side panel: tags (Enter adds, x removes) + note (saved as you type).
The bar glides smoothly between mpv's 4-per-second position updates.

Drawing modes, best first, with automatic fallback (the sidebar card + `proto.log` next to the exe say what happened):
1. Windows: **window** - own Direct3D 11 child window, frames stay on the GPU
2. **texture** - mpv draws into an OpenGL texture inside Slint (Linux starts here)
3. **software** - mpv draws on the CPU into memory (VMs, remote desktop, broken drivers)

mpv starts with the first clip and shuts down 60 s after the player closes (less memory in the background).
Seek pictures: Echo's `<name>.scrub.jpg` if it's there and newer than the clip, else the prototype makes one with
ffmpeg in its OWN temp folder (`%TEMP%\echo-proto-sheets`) - never in Echo's `.thumbs`.

## Experiment switches (environment variables)
- `ECHO_VIDEO` = `window` / `texture` / `software` (starting mode)
- `ECHO_FAIL` = `window,texture` pretends those modes fail (tests the fallback chain)
- `ECHO_HWDEC` = mpv hwdec value (default `auto-safe`; window mode `d3d11va`)
- `ECHO_MPV_OPTS` = extra mpv options, `key=value;key=value`
- `ECHO_MPV_LOG` = file for mpv's own log
- `ECHO_FFMPEG` = ffmpeg to use for seek sheets
- `ECHO_IDLE_SECS` = mpv shutdown delay after closing (default 60)
- `ECHO_SETTINGS` = settings.json to use (default: Echo's own, %APPDATA%\Clipper\settings.json)
- `ECHO_PAGE=settings:<section>` (or `settings:?<search>`) = open the Settings page at start
- `ECHO_DEBUG_OPEN=1` = log "player: first picture after N ms" for every clip opened
- `ECHO_AUTOPLAY` = card number to open 1.5 s after start; `ECHO_HOVER` = card to hover; `ECHO_PICK` = select the first n
