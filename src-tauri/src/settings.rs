//! User settings, stored as JSON in %APPDATA%\Clipper\settings.json.

use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct ClipHotkey {
    /// e.g. "Alt+F10", "Shift+KeyC" (KeyboardEvent.code names)
    pub keys: String,
    /// how many seconds this hotkey saves
    pub seconds: u32,
}

/// Per-game choices (Settings > Games).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct GameRule {
    pub name: String,
    /// false = pause the replay buffer while this game runs
    pub record: bool,
    /// false = this game's clips stay in the main folder
    pub folder: bool,
}

/// One cloud account: provider ("filen", "gdrive", "mega", "pcloud", "webdav", "sftp", "s3", "folder"),
/// a label for the UI (email / server), and the folder inside it that holds Echo/.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct CloudAccount {
    pub id: u32,
    pub provider: String,
    pub label: String,
    pub root: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Settings {
    pub hotkeys: Vec<ClipHotkey>,
    /// "native" or a target height like "1440", "1080", "720"
    pub resolution: String,
    pub fps: u32,
    pub bitrate_mbps: u32,
    pub desktop_audio: bool,
    pub desktop_volume: f32,
    pub mic: bool,
    pub mic_volume: f32,
    /// microphone name ("" = Windows default recording device)
    pub mic_device: String,
    /// extra audio tracks for the game, Discord and browsers (Windows 10 2004+ per-app capture)
    pub app_tracks: bool,
    pub clips_dir: String,
    /// 0 = unlimited
    pub max_storage_gb: u32,
    pub start_with_windows: bool,
    pub start_minimized: bool,
    pub close_to_tray: bool,
    /// Discord status "Playing Echo / Clipping <game>" (discord.rs)
    pub discord_presence: bool,
    /// only while a game is detected (off = all the time Echo runs, the default)
    pub discord_game_only: bool,
    pub sound_on_save: bool,
    pub sound_on_fail: bool,
    /// 0..1
    pub sound_volume: f32,
    /// name clips after the game in front ("Valorant 2026-...") instead of "Clip ..."
    pub name_by_game: bool,
    /// save game clips into <clips>/<Game>/
    pub game_folders: bool,
    /// speech model for auto captions: "base" (fast, ~150 MB) or "small" (more accurate, ~470 MB)
    pub caption_model: String,
    /// screenshot hotkey ("" = off)
    pub screenshot_key: String,
    /// hotkey that drops a bookmark into the buffer ("" = off)
    pub bookmark_key: String,
    /// small "clip saved" popup over games
    pub show_overlay: bool,
    /// library: "On this day" bar with a clip from the same date in an earlier year
    pub on_this_day: bool,
    /// pause the replay buffer after this many idle minutes (0 = never)
    pub idle_pause_min: u32,
    /// what the plain Copy button does: "full", "free", "basic", "nitro" or "custom"
    pub copy_default: String,
    /// Discord upload limits in MB for the Copy menu
    pub discord_free_mb: f64,
    pub discord_basic_mb: f64,
    pub discord_nitro_mb: f64,
    pub discord_custom_mb: f64,
    /// per-game rules (games not listed: record + own folder)
    pub games: Vec<GameRule>,
    /// only keep the replay buffer running while a game is open
    pub only_while_game: bool,
    pub sound_on_mark: bool,
    pub sound_on_shot: bool,
    /// warning sound when the recorder stops by itself
    pub sound_on_stop: bool,
    /// custom .wav per event ("saved", "failed", "mark", "shot", "stopped"); missing = built-in sound
    pub sound_files: std::collections::BTreeMap<String, String>,
    /// hotkey that starts / stops a long (full-session) recording ("" = off)
    pub session_key: String,
    /// Share button: Cloudflare R2 bucket
    pub r2_account: String,
    pub r2_key: String,
    pub r2_secret: String,
    pub r2_bucket: String,
    /// public URL of the bucket (r2.dev or custom domain)
    pub r2_url: String,
    /// hotkey that mutes / unmutes the mic ("" = off)
    pub mic_mute_key: String,
    /// hotkey that shows Echo's window / sends it back to the tray ("" = off)
    pub show_key: String,
    /// push-to-talk: mic is only recorded while this key is held
    pub push_to_talk: bool,
    pub ptt_key: String,
    /// remove background noise (keyboard, fans) from the mic
    pub mic_denoise: bool,
    /// delete clips older than this many days (0 = never); favourites are kept
    pub cleanup_days: u32,
    /// library view: "grid" or "list"
    pub lib_view: String,
    /// monitor to record (device name like "\\.\DISPLAY2"; "" = primary)
    pub monitor: String,
    /// "screen" = whole monitor, "game" = only the game window while a game runs
    pub capture: String,
    /// "h264" (plays everywhere), "hevc" (smaller), "av1" (smallest, newest GPUs)
    pub codec: String,
    /// cloud library: "" (off), "pcloud" or "mega" (login lives in %APPDATA%\\Clipper\\rclone\\echo.conf)
    pub cloud: String,
    /// cloud accounts (rclone remotes acc_<id> in echo.conf); "echo" is an alias (1) or union (2+) over them
    pub cloud_accounts: Vec<CloudAccount>,
    /// Echo Cloud: sign-in token (from the email code), the email, and the server address ("" = built-in)
    pub ec_token: String,
    pub ec_email: String,
    pub ec_api: String,
    /// which clips upload by themselves: "off", "favs" (favourites) or "all" (every new clip + favourites)
    pub cloud_upload: String,
    /// download clips that other PCs (Windows / Linux) uploaded. OFF by default (12 Nov 2026): a clip from another
    /// PC stays in the cloud and streams when it's played; it only lands on this PC when the user asks
    pub cloud_download: bool,
    /// 0 = file from when cloud_download was on by default (switched off once in load())
    #[serde(default)]
    pub dl_v: u32,
    /// unix time the cloud was connected: "all" only uploads clips newer than this, sync only downloads newer ones
    pub cloud_since: u64,
    /// what players hear (track 1): "all" = everything on the PC, "apps" = only the apps ticked below (+ mic)
    pub mix_mode: String,
    /// per-app volume (0..2) for the Game / Discord / Browser tracks and the mix
    pub vol_game: f32,
    pub vol_discord: f32,
    pub vol_browser: f32,
    /// music apps (Spotify…) track, a:6
    pub vol_music: f32,
    /// in "apps" mode: which apps go into the mix
    pub mix_game: bool,
    pub mix_discord: bool,
    pub mix_browser: bool,
    pub mix_music: bool,
    /// mic only sends one channel: copy it to both ears
    pub mic_mono: bool,
    /// cloud: delete the PC copy after an automatic upload
    pub cloud_free_after: bool,
    /// cloud share links expire after this many days (0 = never)
    pub link_days: u32,
    /// with several clouds: where new uploads go. "" = the one with the most free space (old behaviour),
    /// "echocloud" = Echo Cloud while it has room (Discord share links), or a cloud account id ("3")
    pub cloud_target: String,
    /// copy the share link after uploading a clip by hand. OFF by default (chat 27): a link puts a copy in
    /// Echo Cloud, and Echo Cloud should only fill up when the user asks for it
    pub auto_copy_link: bool,
    /// 0 = file from when auto_copy_link was on by default (switched off once in load())
    #[serde(default)]
    pub link_v: u32,
    /// renamed / merged game folders: game (or old folder) name -> folder name ("" = main folder)
    pub folder_alias: std::collections::BTreeMap<String, String>,
    /// apps the user removed from "Your games" and never wants treated as a game
    pub not_games: Vec<String>,
    /// the first-start setup was shown
    pub setup_done: bool,
    /// re-encode clips older than shrink_days to about half their size while the PC is idle
    pub shrink_old: bool,
    pub shrink_days: u32,
    /// shrink with AV1 instead of H.264 when this PC can encode AND play AV1 in hardware (same quality at about half the size)
    pub shrink_av1: bool,
    /// record with constant quality capped at bitrate_mbps instead of a constant bitrate (smaller clips, same sharpness in action)
    pub smart_bitrate: bool,
    /// show the mouse pointer in recordings (games that hide it stay without; menus, phones, inventories show it)
    pub capture_cursor: bool,
    /// share links play a copy with the watermark (slower to make); off = the clip itself, fast
    pub share_watermark: bool,
    /// quality level for smart bitrate (0-100, higher = sharper and bigger): 72 balanced, 82 high, 90 very high
    pub smart_quality: u32,
    /// settings version for smart bitrate: 0 = file from before it existed (migrated once in load())
    #[serde(default)]
    pub smart_v: u32,
    /// 0 = file from before the Linux screenshot key moved off Shift+F10 (migrated once in load())
    #[serde(default)]
    pub shot_key_v: u32,
    /// password of the encrypted rclone cloud config (echo.conf, Windows); made by Echo, never shown
    pub rclone_pass: String,
    /// controller combo that saves a clip (XInput bits, 0 = off) and its length (0 = first hotkey's length)
    pub pad_buttons: u16,
    pub pad_seconds: u32,
    /// animated watermark on exports, Discord-size copies, montages and GIFs (recorded clips stay clean):
    /// "link" (default since 12 Nov 2026) = Echo + "echoclip.link", "kimbo" = Echo + "By Kimbo", "echo" = just Echo, "off"
    pub watermark_style: String,
    /// colour theme (Settings > Appearance): "" = Violet, "ocean", "ember", …, "custom" = theme_accent + theme_bg
    pub theme: String,
    pub theme_accent: String,
    pub theme_bg: String,
    /// small highlight colour (progress bars, trim handles…), custom theme only; "" = yellow
    pub theme_hi: String,
}

impl Default for Settings {
    fn default() -> Self {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| ".".into());
        Settings {
            hotkeys: vec![
                ClipHotkey { keys: "Alt+F10".into(), seconds: 30 },
                ClipHotkey { keys: "Alt+F11".into(), seconds: 180 },
            ],
            resolution: "native".into(),
            fps: 60,
            bitrate_mbps: 33,
            desktop_audio: true,
            desktop_volume: 1.0,
            mic: true,
            mic_volume: 1.0,
            mic_device: String::new(),
            app_tracks: true,
            clips_dir: PathBuf::from(home).join("Videos").join("EchoClips").to_string_lossy().into(),
            max_storage_gb: 0,
            start_with_windows: false,
            start_minimized: false,
            close_to_tray: true,
            discord_presence: true,
            discord_game_only: false,
            sound_on_save: true,
            sound_on_fail: true,
            sound_volume: 0.6,
            name_by_game: true,
            game_folders: true,
            caption_model: "base".into(),
            // Linux: Shift+F10 opens context menus in GTK/KDE apps and clashed with the developer's screenshot tool
            screenshot_key: if cfg!(target_os = "linux") { "Alt+F12" } else { "Shift+F10" }.into(),
            shot_key_v: 1,
            bookmark_key: "Shift+F9".into(),
            show_overlay: true,
            on_this_day: true,
            idle_pause_min: 0,
            copy_default: "full".into(),
            discord_free_mb: 20.0,
            discord_basic_mb: 50.0,
            discord_nitro_mb: 1000.0,
            discord_custom_mb: 100.0,
            games: Vec::new(),
            only_while_game: false,
            sound_on_mark: true,
            sound_on_shot: true,
            sound_on_stop: true,
            sound_files: Default::default(),
            session_key: String::new(),
            r2_account: String::new(),
            r2_key: String::new(),
            r2_secret: String::new(),
            r2_bucket: String::new(),
            r2_url: String::new(),
            mic_mute_key: String::new(),
            show_key: String::new(),
            push_to_talk: false,
            ptt_key: String::new(),
            mic_denoise: true,
            cleanup_days: 0,
            lib_view: "grid".into(),
            folder_alias: Default::default(),
            not_games: Vec::new(),
            setup_done: false,
            shrink_old: false,
            shrink_days: 30,
            shrink_av1: true,
            smart_bitrate: true,
            capture_cursor: true,
            share_watermark: true,
            smart_quality: 72,
            smart_v: 1,
            rclone_pass: String::new(),
            pad_buttons: 0,
            pad_seconds: 0,
            watermark_style: "link".into(),
            // chat 30: Linux follows the desktop's light/dark setting by default (AppImageHub); Windows stays Violet
            theme: if cfg!(target_os = "linux") { "system".into() } else { String::new() },
            theme_accent: String::new(),
            theme_bg: String::new(),
            theme_hi: String::new(),
            monitor: String::new(),
            mix_mode: "all".into(),
            vol_game: 1.0,
            vol_discord: 1.0,
            vol_browser: 1.0,
            vol_music: 1.0,
            mix_game: true,
            mix_discord: true,
            mix_browser: false,
            mix_music: false,
            mic_mono: false,
            cloud_free_after: false,
            link_days: 0,
            cloud_target: String::new(),
            auto_copy_link: false,
            link_v: 1,
            cloud: String::new(),
            cloud_accounts: Vec::new(),
            ec_token: String::new(),
            ec_email: String::new(),
            ec_api: String::new(),
            cloud_upload: "favs".into(),
            cloud_download: false,
            dl_v: 1,
            cloud_since: 0,
            capture: "screen".into(),
            codec: "h264".into(),
        }
    }
}

impl Settings {
    /// names of the secret fields (keyring item names on Linux)
    const SECRETS: [&'static str; 4] = ["ec_token", "r2_key", "r2_secret", "rclone_pass"];

    pub fn path() -> PathBuf {
        let base = std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
        base.join("Clipper").join("settings.json")
    }

    pub fn load() -> Self {
        // QA 11 Nov 2026: a UTF-8 byte-order mark (Windows PowerShell 5 `Set-Content -Encoding utf8`, some editors)
        // made serde refuse the file = defaults: Videos\EchoClips, signed out, and the next save overwrote it all
        let mut s: Self = fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok())
            .unwrap_or_default();
        // sign-in token and keys are saved encrypted (see `secret`); older files have them in plain text
        let plain = [&s.ec_token, &s.r2_key, &s.r2_secret, &s.rclone_pass].iter()
            .any(|v| !v.is_empty() && !v.starts_with(secret::PFX) && !v.starts_with(secret::KS));
        let stored = [s.ec_token.clone(), s.r2_key.clone(), s.r2_secret.clone(), s.rclone_pass.clone()];
        // Linux at login: the keyring may start a moment after Echo - wait for it a little (only then)
        for attempt in 0..3 {
            for ((n, v), raw) in Self::SECRETS.iter().zip([&mut s.ec_token, &mut s.r2_key, &mut s.r2_secret, &mut s.rclone_pass]).zip(&stored) {
                *v = secret::unprotect(n, raw);
                let _ = n;
            }
            if !secret::keyring_down() || attempt == 2 { break; }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        if plain && secret::available() {
            let _ = s.save();
        }
        // Smart bitrate arrived (chat 15): the old bitrate becomes the AVERAGE (Balanced = 60% of the maximum),
        // so clips stay about the same size, busy moments get up to ~1.7x the bits, and nothing gets softer.
        if s.smart_v < 1 {
            let max = (s.bitrate_mbps as f64 / 0.6).round() as u32;
            if max <= 80 {
                s.smart_bitrate = true;
                s.smart_quality = 72;
                s.bitrate_mbps = max.max(4);
            } else {
                // very high fixed bitrates (over 48 Mbps) can't keep their average under the 80 Mbps ceiling:
                // they stay on the fixed bitrate they chose, so nobody's quality drops
                s.smart_bitrate = false;
            }
            s.smart_v = 1;
            let _ = s.save();
        }
        // Linux: the old default screenshot key Shift+F10 (= "open context menu" in Linux apps) becomes Alt+F12.
        // Only the untouched default changes; a key someone picked himself stays.
        // "Copy the link after uploading" was on by default and quietly filled Echo Cloud with share copies:
        // off once for everyone (switch it on again in Settings › Sharing)
        if s.link_v < 1 {
            s.auto_copy_link = false;
            s.link_v = 1;
            let _ = s.save();
        }
        // "Get clips from my other PCs" was on by default: every new clip from another PC was downloaded here by
        // itself, while the user wanted it to stay in the cloud and stream (12 Nov 2026) - off once for everyone
        // (switch it on again in Settings › Cloud › Sync)
        if s.dl_v < 1 {
            s.cloud_download = false;
            s.dl_v = 1;
            let _ = s.save();
        }
        if s.shot_key_v < 1 {
            if cfg!(target_os = "linux") && s.screenshot_key == "Shift+F10" {
                s.screenshot_key = "Alt+F12".into();
            }
            s.shot_key_v = 1;
            let _ = s.save();
        }
        s
    }

    pub fn save(&self) -> Result<(), String> {
        let p = Self::path();
        if let Some(d) = p.parent() {
            fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let mut out = self.clone();
        for (n, v) in Self::SECRETS.iter().zip([&mut out.ec_token, &mut out.r2_key, &mut out.r2_secret, &mut out.rclone_pass]) {
            *v = secret::protect(n, v);
        }
        // write next to it, then swap: a crash mid-write can't leave a half settings file
        let tmp = p.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(&out).unwrap()).map_err(|e| e.to_string())?;
        secret::private_file(&tmp);
        fs::rename(&tmp, &p).or_else(|_| fs::copy(&tmp, &p).map(|_| ()).and_then(|_| fs::remove_file(&tmp))).map_err(|e| e.to_string())
    }

    /// Longest clip any hotkey can ask for = how much the buffer must hold.
    pub fn buffer_seconds(&self) -> u32 {
        self.hotkeys.iter().map(|h| h.seconds).max().unwrap_or(30).clamp(10, 1800)
    }

    /// Folder a game's clips go into (after renames / merges); "" = main folder.
    pub fn game_folder(&self, game: &str) -> String {
        let mut f = game.to_string();
        for _ in 0..8 {
            match self.folder_alias.iter().find(|(k, _)| k.eq_ignore_ascii_case(&f)) {
                Some((_, v)) if !v.eq_ignore_ascii_case(&f) => f = v.clone(),
                _ => break,
            }
            if f.is_empty() {
                break;
            }
        }
        f
    }

    pub fn game_rule(&self, name: &str) -> GameRule {
        self.games.iter().find(|g| g.name.eq_ignore_ascii_case(name)).cloned()
            .unwrap_or(GameRule { name: name.into(), record: true, folder: true })
    }

    /// Does changing from `self` to `new` require restarting the recorder?
    pub fn recorder_changed(&self, new: &Settings) -> bool {
        self.resolution != new.resolution
            || self.fps != new.fps
            || self.bitrate_mbps != new.bitrate_mbps
            || self.smart_bitrate != new.smart_bitrate
            || self.capture_cursor != new.capture_cursor
            || self.smart_quality != new.smart_quality
            || self.desktop_audio != new.desktop_audio
            || self.mic != new.mic
            || self.mic_device != new.mic_device
            || self.app_tracks != new.app_tracks
            || self.mic_denoise != new.mic_denoise
            || self.monitor != new.monitor
            || self.capture != new.capture
            || self.codec != new.codec
            || self.buffer_seconds() != new.buffer_seconds()
    }

}

/// Secrets in settings.json (Echo Cloud sign-in, R2 keys, the cloud config password).
/// Windows: encrypted for the signed-in Windows user (DPAPI) and saved as "enc:<hex>" - no password, no prompt.
/// Linux: kept in the desktop keyring (KDE Wallet / GNOME Keyring); the file only says "ks:".
/// Without a keyring they stay in the file, which only this user can read.
/// A copy of the file on another PC / user can't be read (Echo then just asks to sign in again).
pub mod secret {
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    pub const PFX: &str = "enc:";
    pub const KS: &str = "ks:";
    /// Linux: value last written to / read from the keyring per field (no D-Bus call when nothing changed)
    static LAST: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
    /// Linux: fields whose keyring item couldn't be read this run (keyring not running / locked):
    /// their "ks:" mark stays in the file, so nothing is lost
    static DOWN: Mutex<Option<HashSet<String>>> = Mutex::new(None);

    fn last(name: &str) -> Option<String> { LAST.lock().ok()?.as_ref()?.get(name).cloned() }
    fn set_last(name: &str, v: &str) { if let Ok(mut g) = LAST.lock() { g.get_or_insert_with(HashMap::new).insert(name.into(), v.into()); } }
    fn is_down(name: &str) -> bool { DOWN.lock().ok().and_then(|g| g.as_ref().map(|s| s.contains(name))).unwrap_or(false) }
    fn set_down(name: &str, down: bool) {
        if let Ok(mut g) = DOWN.lock() {
            let s = g.get_or_insert_with(HashSet::new);
            if down { s.insert(name.into()); } else { s.remove(name); }
        }
    }
    /// Some secret couldn't be read because the keyring wasn't there (Linux).
    pub fn keyring_down() -> bool { DOWN.lock().ok().and_then(|g| g.as_ref().map(|s| !s.is_empty())).unwrap_or(false) }

    pub fn available() -> bool {
        #[cfg(windows)]
        { true }
        #[cfg(target_os = "linux")]
        { crate::keyring_linux::available() }
        #[cfg(not(any(windows, target_os = "linux")))]
        { false }
    }

    pub fn protect(name: &str, v: &str) -> String {
        if v.starts_with(PFX) || v.starts_with(KS) { return v.to_string(); }
        #[cfg(target_os = "linux")]
        {
            if v.is_empty() {
                // couldn't read it this run: keep the mark; signed out for real: remove it from the keyring
                if is_down(name) { return KS.into(); }
                if last(name).map_or(false, |l| !l.is_empty()) { crate::keyring_linux::delete(name); set_last(name, ""); }
                return String::new();
            }
            if !available() { return v.to_string(); }
            if last(name).as_deref() == Some(v) { return KS.into(); }
            return match crate::keyring_linux::store(name, v) {
                Ok(()) => { set_last(name, v); set_down(name, false); KS.into() }
                Err(_) => v.to_string(),
            };
        }
        #[allow(unreachable_code)]
        {
            let _ = (name, last as fn(&str) -> Option<String>, set_last as fn(&str, &str), is_down as fn(&str) -> bool);
            if v.is_empty() || !available() { return v.to_string(); }
            match imp::protect(v.as_bytes()) {
                Some(b) => format!("{PFX}{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>()),
                None => v.to_string(),
            }
        }
    }

    /// "" when it can't be read (file from another PC / user, keyring not there).
    pub fn unprotect(name: &str, v: &str) -> String {
        if v == KS {
            #[cfg(target_os = "linux")]
            return match crate::keyring_linux::load(name) {
                Ok(s) => { set_last(name, &s); set_down(name, false); s }
                Err(crate::keyring_linux::Miss::NotFound) => { set_down(name, false); String::new() }
                Err(crate::keyring_linux::Miss::Unavailable) => { set_down(name, true); String::new() }
            };
            #[allow(unreachable_code)]
            { let _ = (name, set_down as fn(&str, bool)); return String::new(); }
        }
        let Some(h) = v.strip_prefix(PFX) else { return v.to_string() };
        if h.len() % 2 != 0 { return String::new(); }
        let bytes: Option<Vec<u8>> = (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).ok()).collect();
        bytes.and_then(|b| imp::unprotect(&b)).and_then(|b| String::from_utf8(b).ok()).unwrap_or_default()
    }

    /// Only this user may read the file (Linux; on Windows %APPDATA% is per-user already).
    pub fn private_file(p: &std::path::Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
        }
        #[cfg(not(unix))]
        let _ = p;
    }

    #[cfg(windows)]
    mod imp {
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        use windows::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};

        fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
            let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
            unsafe { let _ = LocalFree(HLOCAL(out.pbData as *mut core::ffi::c_void)); }
            v
        }
        pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
            let inp = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
            let mut out = CRYPT_INTEGER_BLOB::default();
            unsafe { CryptProtectData(&inp, windows::core::w!("Echo"), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out) }.ok()?;
            Some(take(out))
        }
        pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
            let inp = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
            let mut out = CRYPT_INTEGER_BLOB::default();
            unsafe { CryptUnprotectData(&inp, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out) }.ok()?;
            Some(take(out))
        }
    }

    #[cfg(not(windows))]
    mod imp {
        #[allow(dead_code)]
        pub fn protect(_: &[u8]) -> Option<Vec<u8>> { None }
        pub fn unprotect(_: &[u8]) -> Option<Vec<u8>> { None }
    }
}
