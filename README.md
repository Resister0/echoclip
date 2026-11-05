# Echo

**A lightweight game clipping app for Windows and Linux.** Echo keeps the last few minutes of your game in a replay buffer. Press one key and that moment becomes a clip, ready to trim, caption and share on Discord.

Website and downloads: **https://echoclip.link**

## Why Echo

- **Light**: written in Rust with [Tauri](https://tauri.app). The app itself is about 12 MB and uses the web view your system already has (WebView2 on Windows, WebKitGTK on Linux) instead of shipping its own Chromium.
- **GPU recording**: records with your graphics card's encoder (NVIDIA or AMD), so games keep running smoothly.
- **Discord-ready**: copy a clip straight into a chat, automatically shrunk to Discord's upload limit, or share a link that plays inside Discord (Echo Cloud).
- **Editor built in**: trim, add text and captions, combine clips into a montage, make GIFs.
- **Your storage**: back up to Echo Cloud, Google Drive, MEGA, pCloud, Filen, your own WebDAV / SFTP / S3 server or a NAS.
- **No tracking**: Echo collects no usage data. Clips stay on your PC unless you connect a cloud. The only automatic connection is the update check (echoclip.link sees the version number).

## Download

Get the newest version from **https://echoclip.link**. Installed copies update themselves.

- Windows 10/11: `Echo-Setup.exe` (Echo isn't code-signed yet: if Windows warns, click *More info → Run anyway*).
- Linux: `Echo-x86_64.AppImage` (needs `gpu-screen-recorder` and `ffmpeg` from your distribution). Tested on Fedora / Nobara with KDE (Wayland).

## Build it yourself

**Windows**

```
setup.cmd     (once: installs Rust, the MSVC build tools, ffmpeg and WebView2)
build.cmd     (builds Echo.exe in this folder)
```

**Linux**

```
bash build.sh && ./Echo
```

Needs Rust plus the WebKitGTK 4.1, GTK 3, ALSA and D-Bus development packages (see `ci/build.yml` for the exact list on Ubuntu).

Builds you make yourself have version `0.1.0` and never update themselves.

## Project layout

| Folder | What's in it |
|---|---|
| `src-tauri/` | The app: recorder, hotkeys, clip library, cloud sync (Rust) |
| `ui/` | The app's interface (HTML / CSS / JavaScript) |
| `cloud/` | Echo Cloud: the server (Cloudflare Worker) and the echoclip.link website |
| `ci/` | The GitHub Actions build recipes (`public-build.yml` builds every release in this repository) |

## How releases are built

Every file on echoclip.link and every automatic update is built by GitHub Actions in this repository from the code you see here (`.github/workflows/build.yml`). What changed in each version: [CHANGELOG.md](CHANGELOG.md).

## Packaging Echo

Packagers are welcome: if you'd like Echo in your distro's repository (AUR, COPR, a PPA, Nix…), go ahead, and open an issue if you need anything changed on our side. Useful to know:

- Every release on this repository's [Releases page](https://github.com/Resister0/echoclip/releases) has `Echo-x86_64.AppImage` and `Echo-linux-x86_64.tar.gz` (a plain binary). The version number is in `ci/version.txt`.
- At runtime Echo needs WebKitGTK 4.1, `gpu-screen-recorder`, `ffmpeg`/`ffprobe`, `pactl`/`parec` (PulseAudio or PipeWire-Pulse) and `wl-clipboard`; `rclone` is used for cloud backups.
- The built-in updater only runs inside the AppImage or the Windows installer, so a packaged build never replaces itself.
- `flatpak/` has a Flatpak manifest for local builds.

## Licence

Echo is free software: you can redistribute it and/or modify it under the terms of the **GNU General Public License, version 3 or (at your option) any later version**. See [LICENSE](LICENSE).

Copyright (C) 2026 Kimbo.

Echo uses or downloads these tools, each under its own licence: [FFmpeg](https://ffmpeg.org) (GPL), [rclone](https://rclone.org) (MIT), [gpu-screen-recorder](https://git.dec05eba.com/gpu-screen-recorder/about/) (GPL-3.0, Linux), [whisper.cpp](https://github.com/ggerganov/whisper.cpp) (MIT, auto captions), [RNNoise](https://github.com/xiph/rnnoise) (BSD, noise removal), [Tauri](https://tauri.app) (MIT / Apache-2.0).

## Contact

Bugs and ideas: open an issue here, or email support@echoclip.link.
