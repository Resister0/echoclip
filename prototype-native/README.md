# Echo - the native app (Rust + Slint + libmpv)

This folder IS Echo 2.0: the window, library, player, editor and settings, drawn with [Slint](https://slint.dev),
clips played with libmpv. Echo's recorder, hotkeys, settings and cloud sync come from `echo-core/`, which compiles
the files in `../src-tauri/src/` unchanged (only that folder's name is left over from the old Tauri app).

## Build it

From the repository root:

- **Linux** (Fedora / Nobara): `bash build.sh` - installs the missing packages (mpv-libs, D-Bus, ALSA, fontconfig,
  xkbcommon, ffmpeg…), builds, and puts `./Echo` in the repository root plus an "Echo (dev)" menu entry.
- **Windows**: `setup.cmd` once (MSVC build tools, Rust, ffmpeg, `libmpv-2.dll`), then `build.cmd` - builds and
  starts `prototype-native\EchoProto.exe`. `libmpv-2.dll` (mpv's player library) must sit next to the exe.

Or just `cargo build --release` in this folder (binary: `target/release/echo-native-proto`).

Builds you make yourself have version `0.1.0` ("built by hand" in the log) and never update themselves. The release
builds come from `../ci/public-build.yml`.

## Layout

- `src/` - the app (one file per area: `main.rs` wires it up, `library.rs` / `lib_*.rs` the library, `pl_*.rs` the
  player, `edui.rs` the editor, `settings.rs` Settings, `cloud*.rs` Echo Cloud, `mpv.rs` the video player)
- `ui/` - the Slint interface (`app.slint` and the files it imports)
- `echo-core/` - the recorder crate (see above)

The app's own log is `proto.log` next to the exe (Settings → Send log sends it together with the recorder's log).

## Switches for testing (environment variables)

- `ECHO_VIDEO` = `window` (Windows) / `texture` / `software` - how mpv draws; it falls back by itself when one fails
- `ECHO_HWDEC` = mpv's hwdec value; `ECHO_MPV_OPTS` = extra mpv options (`key=value;key=value`); `ECHO_MPV_LOG` = file for mpv's own log
- `ECHO_SETTINGS` = settings.json to use instead of Echo's own
- `ECHO_PAGE=settings:<section>` = open Settings at that section at start
