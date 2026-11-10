# Changelog

Everything that changed in Echo, newest first. Each section is one published version (or a group of small
internal builds). Release builds since the open-source switch come from this repository's GitHub Actions.

## 2.0.2 - 10 Nov
- New hotkey: Show / hide Echo - brings Echo up over your game, or back to the tray (Settings > Hotkeys, off by default)
- Clearer notifications: icons and colours, Undo after deleting or moving clips, speed and time left on downloads and uploads, and buttons like Show in folder, Open and Play
- AV1 clips: uploads say what's happening ("Converting AV1 to H.264… 42 %"), share links work for AV1 clips already in the cloud, and Free up space works when the cloud has an H.264 copy
- Your own clouds (Google Drive, NAS, …) always get the original file, AV1 too. Echo Cloud always keeps the smaller copy
- Stop sharing works for links in your own cloud too, Google Drive included. A link Echo can't stop tells you where to stop it
- Download bars move smoothly, and Echo's cloud streaming helper closes together with Echo
- Library: missing clip pictures are made again, the list no longer jumps or flickers when it refreshes, and another folder opens at the top. Renaming, moving or deleting a clip takes its pictures along
- Sidebar: a soft edge shows where the list scrolls under the top
- Player: Replace on a clip that's in the cloud updates the cloud copy too. Home's "Get started" edit step works on clips that are only in the cloud
- The anonymous performance numbers now tell apart time in the tray, idle and in use (switch: "Send anonymous performance numbers")
- Linux: Echo's icon really shows in Alt+Tab now (KDE)
- Linux: game folders show the game's real icon (Steam, Lutris, Heroic), "Match system" follows your dark / light mode (also in the Flatpak), and sharper Echo Cloud copies
- Linux: drag and drop videos from your file manager onto Echo

## 2.0.1 - 10 Nov
- Echo 2.0 comes to the Flatpak too
- Linux: Echo's icon shows in Alt+Tab (KDE)
- Echo counts how many times it's installed: a new install sends a one-time anonymous signal with its first update check - nothing that identifies you is kept

## 2.0.0 - 10 Nov - Echo 2.0: rebuilt as a native app
- Echo is now a native app on Windows and Linux, with a new, smoother video player (mpv, GPU playback)
- About 150 MB of memory in the background and half the CPU while recording (0.6 % of the PC instead of 1.2 %)
- The window stays responsive while deleting, moving, copying and uploading; the library uses less memory with lots of clips
- Clips play when you hover them (now on Linux too), with sharper clip pictures
- Cut parts out in the player (Shift+drag, X); Save copy and Edit keep the cuts. Mouse Back / Forward buttons work
- New Home with your week at a glance and a compact status card (hover it for your storage)
- Drag and drop videos onto the window to import them; faster imports
- Echo's own right-click menu on clips; Make smaller / Convert to AV1 run as a queue with Cancel
- Sub-folders by dragging folders onto each other; Screenshots page with Copy, select and search
- The header is the window's title bar; click a hotkey there to change it, pick the clip length from a dropdown
- Editor: captions list with editable times, transitions, play backwards, Discord export copied for pasting
- Echo Cloud: smaller uploads (60-80 % smaller, up to 1080p; "Original quality in Echo Cloud" to keep the original), no duplicate uploads, share-only uploads go away with their link, cloud-only clips download when needed
- Smarter Tidy up ("Saved twice" keeps the newest clip, new "Edit and original" / "In the cloud too") and "Free up space" chips in Manage space
- Crimson theme, RGB mode, mic test in the welcome setup, plans for paid Echo Cloud users
- Optional anonymous performance numbers (Settings > App Settings > About; no ID, kept 180 days)
- Signed installer; the portable download is a zip; updates retry every 15 min when offline and show in the tray
- Linux runs the same native app (AppImage, tar.gz, Flatpak); the tar.gz needs your distro's mpv library
- Known: on Linux Wayland dragging files onto the window doesn't import yet, and Linux game folders show a letter instead of the game's icon

## 1.0.70 - 7 Nov
- AV1 clips play in the Linux AppImage
- Windows: Echo no longer shows as the game ECHO in Discord (the program is now called EchoClip.exe)
- Uses less memory while Echo waits in the tray
- Security fixes

## 1.0.68 - 7 Nov - Light themes
- Light themes: Light, Paper and Match system (follows your desktop; the default for new Linux installs)
- AV1 clips play again in the Linux AppImage instead of going black
- (1.0.69 = internal build: AppImage update information)

## 1.0.67 - 7 Nov - New Home page
- New Home page with your latest clip
- Settings refresh with seasonal themes and background effects
- One Echo account for profile and cloud
- Drag videos straight into a folder; faster imports with progress and cancel
- AV1 conversion on Linux, progress for AV1 batches
- Linux app icon, fewer false games, replay from the end with one press

## 1.0.66 - 7 Nov
- Choose when Echo shows in your Discord status
- (1.0.63 - 1.0.65 = internal builds: README)

## 1.0.62 - 7 Nov - Discord status
- Discord status (Playing Echo / Clipping your game)
- Cleaner captions
- Linux: Space works in the editor
- Google Drive and pCloud sign-in fixes

## 1.0.61 - 6 Nov
- Nicer page after signing in to Google Drive or pCloud
- New clips go to your own cloud after reconnecting it
- Cloud usage in the sidebar updates right after freeing up Echo Cloud space

## 1.0.60 - 6 Nov - Manage your Echo Cloud space
- Security fixes
- Linux: captions work in the AppImage
- Google Drive now connects as Echo
- Cloud clips keep their date after Free up space and play right away
- Your chosen thumbnail now shows in share link and Discord previews
- Manage your Echo Cloud space (Settings > Cloud > Manage space)
- Popup when Echo Cloud is almost full
- Clips you delete from the cloud stay on this PC and aren't uploaded again; Tidy up's delete confirmation shows again

## 1.0.56 - 1.0.59 - 5-6 Nov
- Internal and Flatpak builds (Flatpak start fix and others)

## 1.0.54 - 5 Nov - Linux Flatpak, fullscreen and update fixes
- Linux Flatpak, fullscreen and update fixes, safer share links, less Echo Cloud space used
- (1.0.55 = internal build: Flatpak build fix)

## 1.0.51 - 1.0.53 - 5 Nov - Linux fixes, smoother clips
- Library and Settings are back at the top of the sidebar - clicking Library always takes you back to All clips
- Echo tidies the cloud by itself: empty folders in your own cloud go once a week, unused share copies in Echo Cloud are cleaned up daily
- "Copy the link after uploading" is now off by default (switch it on in Settings › Sharing) - uploads no longer make Echo Cloud share copies unless you ask
- "Upload to the cloud" puts the clip in the cloud you chose (e.g. Google Drive) even when it also copies a share link - it used to land in Echo Cloud instead
- Deleting, moving or renaming a shared clip keeps its share link - Echo still shows it as shared, and Stop sharing works
- Echo tells you when it can't unlock your cloud logins (Settings › Cloud) and lets you start over in one click
- Moving clips no longer leaves empty folders in Google Drive
- Linux: fullscreen in the player (F) no longer crashes Echo
- The "Saving clip…" card only shows in the folder the clip goes to
- Linux: the quality page shows the real bitrate (Linux records a constant bitrate, there's no "average")
- "Stop sharing" also for clips that are shared but not backed up in the cloud
- Linux: Settings › Hotkeys lost its yellow KDE note (the KDE row below says the same)
- Linux: the log no longer fills up with a line from the recorder every second
- Linux: recording works straight after an automatic update (before, Echo had to be closed and opened again)
- Linux: the Flatpak and the normal Echo no longer overwrite each other's cloud password
- Linux: sharper, smoother pictures while dragging the player's seek bar
- Linux and weaker PCs: hovering a clip shows a quick time-lapse of it (sharp pictures, no sound, lighter on the PC)
- Linux (KDE): Echo shows the keys KDE really uses and says where to change them
- Zoom into screenshots and videos with the mouse wheel (drag to look around)
- Holding Ctrl shows the extra clip buttons instantly
- The key hints under the player no longer flicker as the mouse comes near
- Linux: clips no longer flash a squashed picture when they start in the player
- Linux: dragging through the editor's timeline shows sharp pictures (every 0.1 s) instead of black flashes
- Player: the black box no longer jumps in size when a clip starts
- Linux: pages can no longer be dragged blue like selected text; text fields select in your theme colour
- Linux: the screenshot key is now Alt+F12 (Shift+F10 opens menus in Linux apps)
- Linux: recording, thumbnails and seek-bar previews work in the AppImage (it no longer breaks the system's ffmpeg / gpu-screen-recorder)
- Linux: clips no longer freeze in the player, and it recovers by itself if a clip gets stuck
- Smoother clips: no more tiny hitch every 2 seconds
- Linux: better picture quality (records at the full bitrate you picked)
- Linux: Echo no longer takes your headset mic away from Discord
- Linux: separate audio tracks for game, Discord, browser and music
- Linux: cloud clips stream and download much faster
- Linux: notifications no longer flicker; library cards no longer turn grey
- Player closes with Esc / a click beside it every time, Space no longer gets stuck
- Clips no longer start playing in the background after closing the player
- Ctrl+A selects only the clips you're looking at (was every clip in the library); deleting says if some selected clips are in other folders
- Clearer cloud badge
- Seek-bar previews line up with the video again for edited / imported clips (they could be seconds off)
- Linux: screenshots work (the screenshot key used to fail)
- Linux: dragging the seek bar no longer hides the player's buttons
- Lighter seek-bar dragging on slower PCs: Echo notices when jumping around a clip is slow and shows quick preview pictures while you drag instead
- Screenshots can be selected and deleted together (Ctrl+click, Ctrl+A)
- Clip cards no longer jump when you hover them
- Save keys: Echo renews its keyboard hook by itself if Windows ever drops it, and says so when a game runs as administrator (Windows hides those keys from normal programs)

## 1.0.50 - Ctrl+scroll, saving clip card, captions as .txt, clearer cloud folders
- Cloud date folders are easier to read: 2026 / 11 - November / 04 - Wednesday (older clips stay where they are)
- Scrolling works while holding Ctrl in the library
- A new clip shows as "Saving clip…" at the top right away and becomes clickable once it's ready
- Save keys also work while Echo's own window is in front
- Editor: save the captions as a .txt file
- Copy → Custom size: clicking it lets you type the size first (Enter copies); no more two shrinks fighting each other
- Uploads: the bar follows the file being sent (a retried Google Drive upload showed double the size and sat at 100%), says when it's waiting for the cloud to finish, and the log shows Google Drive's errors as they happen

## 1.0.49 - More reliable update checks
- Every new version now reaches everyone's update check right away (no change in the app itself)

## 1.0.48 - No restarts from game launchers, Linux update command
- "Game window only" recording no longer restarts (and loses the replay) for small launcher / loading windows while a game starts, e.g. FiveM
- Linux (plain tar.gz build): the update card copies a one-line terminal command that updates and restarts Echo

## 1.0.47 - Build fix
- The Linux build no longer hangs when an Ubuntu download mirror is slow (no change in the app itself)

## 1.0.46 - Simpler sidebar, echoclip.link watermark
- Simpler sidebar: no separate "Library" row - All clips, filters and folders open your clips; Settings sits at the bottom
- New watermark style "Echo · echoclip.link" (Settings → Clips & Storage → Watermark)
- Fewer "file is being used by another process" errors: Echo stops its hover preview before renaming, moving or deleting, and retries saving favourites / titles when another program briefly holds the file; errors are now written to Echo's log
- Fixed: Ctrl+C on the clip under the mouse sometimes did nothing; the Ctrl key hint no longer covers the selection bar
- The update card stays compact after skipping many versions ("Restart Echo" never goes off screen)
- The Windows installer shows a short privacy and licence note (with links to the privacy policy and the new code signing policy)

## 1.0.45 - Game icons, new themes, On this day
- The in-game "Clip saved" popup shows the game's icon and the clip's length
- 4 new colour themes: Frost, Lime, Gold and Midnight
- "On this day": a clip from the same date a year ago shows above your library (✕ hides it for today or for good; also a switch in Settings → App)
- A small note when your library reaches 10, 50, 100… clips

## 1.0.44 - View counts, one-step delete, faster Discord export
- Share links count views: the share page shows how often the clip was watched, and Echo shows it on the clip ("Shared · 12 views")
- Deleting a clip that's also in the cloud asks once: only from this PC, or everywhere (no second popup right after)
- The player's Share button lets you pick how long the link lasts before it's made
- Editor: "Discord" export renders once at a size that fits, instead of exporting and then shrinking it again
- Hold Ctrl in the library to see every clip's buttons, plus keys for the clip under the mouse (Enter, E, F, F2, Ctrl+C, L, O, Del)
- New users: the setup can bring in clips from Medal and other apps (or remind you later), and a short "Show me around" tour (Settings → App)
- Stretched resolutions (e.g. 1440x1080 in Fortnite / CS2) record properly, stretched like on your screen
- No mouse pointer in VR clips, or in games that keep a hidden pointer in the middle of the screen
- Settings that do nothing in your setup are greyed out (e.g. the mix switches while the mix has everything on the PC)
- Clips filed as GTA V can be moved to FiveM in one click (Settings → Games); imported "GTA V Legacy" clips go to FiveM if you play FiveM
- Fixed: Enter didn't save a new folder name; moving a clip into a folder could fail with "being used by another process"
- Fixed: Esc on "Edit the project?" opened the editor instead of cancelling
- Fixed: after Trim → Replace the player still showed the old video
- Fixed: sub-folders showed up as games in Settings → Games
- Faster seek-bar previews; the editor's shortcut list is up to date

## Server (no app update needed) - 4 Nov evening
- Fixed: Echo could keep offering an older version (1.0.37) when a newer one was out; the server now reads new versions straight from the download page when GitHub's API is busy, and the changelog shows the newest version too

## 1.0.43 - Clearer profile page
- Settings → Profile: clearer which clips are public - "On your page" (with Remove) and "Your share links" (only you see them, "+ Add to page")

## 1.0.42 - (Echo) in clip file names
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