# Changelog

Everything that changed in Echo, newest first. Each section is one published version (or a group of small
internal builds). Release builds since the open-source switch come from this repository's GitHub Actions.

## Next release
- New clips are saved as e.g. "FiveM 2026-11-04 15-50-51 (Echo).mp4", so people see where a clip came from when it's pasted into Discord; the library and share pages show the name without it

## 1.0.41 - Faster seek bar previews
- The frame strip in the player's seek bar shows up much sooner: it's made in the background right after a clip is saved (and when you hover a clip), cloud clips reuse the strip from when they were on this PC, and otherwise start building after a few seconds instead of 20
- Clips that are already AV1 no longer offer "Convert to AV1" or "Make smaller" (it would only make them look worse)
- Fixed: on clips that were shared and in the cloud, the "Shared" and cloud badges sat on top of each other ("Sha☁"); they now sit side by side

## 1.0.40 - Mouse pointer in clips
- The mouse pointer shows in clips (menus, phones, inventories, the desktop); games that hide it while you play stay clean. Switch it off in Settings → Quality → Capture → Mouse pointer

## 1.0.39 - Faster share links, cancel uploads
- Faster share links when Echo Cloud and another cloud (e.g. Google Drive) are both connected: the link no longer waits for the whole clip to upload to the other cloud first (it sat at 0% for most of a minute); "Making link" shows real upload progress
- Uploads say where they're going ("Uploading X to Google Drive") and have a Cancel button; while the cloud is still getting ready (Google Drive can take a while to find the folder) the box says so instead of sitting at 0%. A cancelled clip isn't uploaded again by itself until Echo restarts

## 1.0.38 - Update button fix
- Fixed: clicking "Restart Echo" on the update card could say "Echo is already up to date" - Echo now installs the update it just found instead of asking the server again
- Copies of Echo made by hand (not installed from echoclip.link) now say they never update themselves, with a "Get Echo" button, instead of "You have the newest version"
- The log notes each Echo run's update check (version, installed or not) to help with update problems

## 1.0.37 - Highlight with a drag
- Editor: drag on empty timeline space to highlight a part in yellow (X cuts it); I, Esc, its ✕ button or a click outside it takes the highlight away. Box-select is now Ctrl+drag

## 1.0.34 - 1.0.36 - Profiles and a Music track
- Profiles (optional, Settings → Profile): your own page at echoclip.link/u/yourname with a name, picture, bio, colour and up to 6 pinned clips; your share links show who clipped them; follow friends to keep their pages one click away (no feeds)
- Music apps (Spotify, Apple Music, TIDAL, Deezer, VLC…) get their own audio track, like the game, Discord and browsers - cut the music out of a clip or keep only it (editor: U)
- Cleaner player toolbar: Copy, Share, Trim and Edit keep their names, the other buttons are grouped icons (hover for the name); keyboard hints show while the mouse is on the video
- Lighter in the tray: while Echo is hidden the window really sleeps, and the upload check slows down when nothing is uploading
- Quieter "Join the Discord" button in Settings
- Fixed: since 1.0.31 some small texts in Settings were invisible - the FPS test's progress and result, the buffer size note, a few hints and notes on clip cards

## 1.0.32 - 1.0.33 - Tray update checks
- Coming back from the tray now really checks for updates (at most every 5 minutes)
- Fixed: the frame strip in the player's seek bar went black near the end of some (mostly short) clips

## 1.0.31 - Stacking notifications and GIF stickers
- Custom theme: pick your own highlight colour (the yellow in progress bars, trim handles, favourites and the editor)
- Editor: GIF stickers move in exports too (they were stuck on one frame); drop pictures or GIFs onto the editor to add them as stickers
- A "Join the Discord" button at the bottom of the settings menu
- Messages and progress bars at the bottom (uploading, downloading, shrinking, exporting) now stack above each other instead of covering one another
- Fixed: the "Downloading from the cloud" progress jumped back and forth to 0%, and the same clip could download twice at once
- Cloud clips play smoother: the seek bar previews no longer load the whole clip in the background or compete with the player
- echoclip.link counts visits and downloads anonymously (no cookies, no IP addresses stored) - explained on the Privacy page

## 1.0.26 - 1.0.30 - Seek bar previews and log links
- The update card (bottom left) shows what's new - before you update ("+ 2 more updates included") and after ("View earlier updates you missed")
- Settings → App: Join the Echo Discord, Changelog
- Hover over (or drag) the player's seek bar to see a preview of that moment, like YouTube - for clips on this PC and in the cloud
- Cloud clips get frame previews in the seek bar too
- "Send log" uploads the log and copies a private link to paste into Discord (email addresses removed, deleted after 14 days); without internet the file is copied like before
- Echo looks for updates when you open or click its window (at most every 15 minutes) and every 2 hours, so you don't have to press "Check for updates"
- Echo Cloud over your plan (e.g. after a paid plan ends): a red note in Settings → Cloud and a daily reminder show the date the oldest clips above the plan will be removed
- Fixed: the recorder kept restarting (and losing the replay buffer) on busy PCs while a heavy game ran - short slowdowns are now caught up instead

## 1.0.25 - Surround headsets and VR games
- **Fixed: Echo couldn't record at all with surround / virtual 7.1 headsets** (e.g. Logitech G733); surround sound is now mixed down to stereo, voices from the centre speaker included
- VR games (Quest Link / Air Link, SteamVR, Virtual Desktop): Echo records the game's own window instead of the desktop, automatically (not tested on a headset yet)

## 1.0.24 - Sound in installed Echo
- **Fixed: installed Echo (Echo-Setup.exe) recorded clips without any sound** - the installer now includes ffmpeg 9.0.2 instead of 8.0
- More details in the log when the self-test finds silent tracks

## 1.0.23 - Recorder that repairs itself
- Fixed: with mic noise reduction on, the mix and mic tracks could stop recording (the noise filter put out invalid values); this also made clips and the self-test fail to save
- Clips (and the self-test) no longer fail to save when an audio track had no sound data - that track is saved as silence
- A broken sound source can no longer stop the whole recording (invalid audio values are muted)
- The recorder restarts itself when it breaks while still running: screen capture failed mid-game (clips would have no picture), a headset or mic disappeared (no sound), or ffmpeg got stuck / one of its encoders stopped
- Sound follows your devices: when a headset comes back (or you change the default speakers / mic in Windows), Echo switches to it within about 10 seconds
- "Save log" shows the real Echo version; the log keeps one file per Echo start

## 1.0.22 - Simpler sharing
- Removed the old "own Cloudflare R2 bucket" sharing (Settings → Sharing). Share links now always go through your cloud, with Echo Cloud first so they play in Discord (the free plan included). Links made with the old option keep working.
- The player's Share button makes the same kind of link as "Copy share link"

## Same share link every time, safer stored logins
- "Copy share link" hands out the same link again instead of making a new one (changing the lifetime keeps the link)
- Signing out of a cloud removes its clips from the library right away
- Sign-in tokens and storage keys are stored encrypted: Windows data protection (DPAPI) on Windows, the desktop keyring (KDE Wallet / GNOME Keyring) on Linux; the cloud login file (rclone config) is encrypted too
- The window never receives the Echo Cloud sign-in token
- Names from files and clouds can no longer inject HTML into the window
- "Send feedback" in Settings → About (optional log attached)
- "What's new" card after an automatic update
- The cloud streaming server stops after 10 minutes without use (less memory on small PCs)
- Game icons load as files instead of being embedded in every library refresh
- Lighter drawing on weak PCs (no blur effects, calmer hover previews)
- Clip names use the full card width until the hover buttons appear
- Tray icon brings the window back exactly as it was (no maximize flash); closing with X minimizes to the tray
- Installer shows the Echo icon and artwork
- Release builds move to the public repository (traceable builds)

## Safer Echo Cloud uploads
- Echo Cloud is read-only inside the combined cloud; uploads, deletes and moves go through the server (no writing past your plan)
- Faster cloud delete (other clouds are cleaned up in the background)
- No thumbnail flashing on rename or upload
- Share copies are deleted when you stop sharing or a link expires

## Smart bitrate, AV1 and the Quality page
- Smart bitrate: same average size, more bits for fast action (peak-limited VBR on the GPU encoder)
- Convert clips to AV1 (about half the size) on GPUs that encode and decode it; everything that leaves the PC stays H.264
- New Quality page with presets and size estimates
- Library filter (game, where, type, tags)
- "Tidy up": clips saved twice, very short clips, biggest clips
- Faster share links (no re-encoding; the share page shows the watermark)
- Simpler cloud menu
- No hidden `tar`: downloaded tools are unpacked inside Echo
- Echo Cloud account deletion and "Stop sharing"

## Version number and smoother updates
- Version shown in Settings → About, "Check for updates" button
- Snappier library: reloads only when files changed, long libraries build in steps
- Installer closes Echo's helper programs before updating
- Share page works on phones (tap controls, fullscreen on iPhone and Android)

## Share links without uploading first
- "Copy share link" works for clips that are only on this PC
- Link lifetime menu (never / 1 / 7 / 30 days)
- Choose which cloud new clips upload to
- Nicer share page with Echo's own video player
- Fewer antivirus false alarms: no `rundll32`, no hidden `curl` (Echo makes its own HTTPS requests)

## Discord embeds
- Share links show title, size and a preview image in Discord and other chat apps
- Installer fix for "Error opening file for writing"
- Editor fixes, uploads pause when Echo Cloud is full

## Open source
- Echo is now GPL-3.0-or-later; the code is public
- Themes (presets and your own colours)
- Website updates

## Automatic updates and the website
- Echo updates itself (signed updates, you choose when to restart)
- echoclip.link website with downloads, pricing, terms and privacy
- Security fixes

## Echo Cloud plans
- Paid Echo Cloud plans (250 GB, 500 GB, 1 TB)
- echoclip.link domain for share links and sign-in emails
- Faster cloud settings page, security fixes (random addresses for the local streaming servers)

## Echo Cloud share links
- Fast "Copy share link" through Echo Cloud
- Better streaming fallback (downloads the clip when streaming stalls)
- Clearer error messages

## Installer and clock-jump fix
- Windows installer (Echo-Setup.exe) with ffmpeg, ffprobe and rclone included
- Clips no longer come out hours long after the PC slept or the clock jumped
- Echo Cloud groundwork

## Editor and library upgrades
- AppImage build for Linux
- Timeline drag and drop, Media button, editable projects
- Export, upload and download progress
- Better captions with positions, moving several items at once
- Your own folders (nested)
- Fix for dark colours in Discord copies

## Linux version
- Echo runs on Linux (gpu-screen-recorder, PipeWire audio, KDE global shortcuts)
- Several clouds at once, clips sorted into date folders in the cloud
- GitHub builds the Linux version too

## Antivirus-friendly Windows build
- No PowerShell or `reg.exe`: clipboard, Recycle Bin, autostart, file pickers and game icons use Windows APIs directly
- Publisher info in the exe
- Sound files can be imported and saved as MP3 / WAV
- Animated watermark with a style setting
- Smarter game sorting, leftover temp file cleanup, player and editor fixes

## Imports and welcome setup
- Welcome setup on first start
- "Send log" button
- Shrink old clips automatically (optional)
- Save clips with a controller button combo
- FPS test (how much Echo costs in your game)
- Import clips from Medal, SteelSeries Moments and other apps, sorted by game
- New app icon

## Cloud library
- Your own cloud for your clips (Filen, MEGA, pCloud, Google Drive, WebDAV, SFTP, S3 or a folder) through rclone
- Upload favourites or every clip, get clips from your other PCs, stream cloud-only clips
- Share links through Cloudflare R2

## Capture and audio upgrades
- Record a game window only, choose the monitor, HDR to SDR
- HEVC and AV1 recording, NVIDIA and Intel encoders
- Per-app audio tracks (game, Discord, browser) with their own volumes, mic noise filter, push-to-talk, mic mute hotkey
- Health check and self-test
- "Record" button for long recordings

## Editor
- Timeline editor: pieces, speed, gaps, texts, several video layers, separated audio
- Auto captions (whisper.cpp) with styles, animations and karaoke highlight
- Transitions, colour looks, freeze frames, reverse, stickers, fades
- Export presets (YouTube, TikTok / Shorts 9:16, Discord), even-out loudness
- GIF export, montages, frame grabs, MP3

## Library
- Clip library with thumbnails, hover previews, search (including spoken words), tags and notes
- Game folders with icons, favourites, screenshots, bookmarks
- Copy a clip into Discord, also shrunk to Discord's size limits
- Drag and drop import

## First version
- Replay buffer with near-zero cost: own zero-copy GPU pipeline (Desktop Duplication → hardware H.264 encoder)
- Hotkeys save the last N seconds, desktop audio and microphone on separate tracks
- Quick trim, tray icon, start with Windows
