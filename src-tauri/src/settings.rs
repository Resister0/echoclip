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
    /// download clips that other PCs (Windows / Linux) uploaded
    pub cloud_download: bool,
    /// unix time the cloud was connected: "all" only uploads clips newer than this, sync only downloads newer ones
    pub cloud_since: u64,
    /// what players hear (track 1): "all" = everything on the PC, "apps" = only the apps ticked below (+ mic)
    pub mix_mode: String,
    /// per-app volume (0..2) for the Game / Discord / Browser tracks and the mix
    pub vol_game: f32,
    pub vol_discord: f32,
    pub vol_browser: f32,
    /// in "apps" mode: which apps go into the mix
    pub mix_game: bool,
    pub mix_discord: bool,
    pub mix_browser: bool,
    /// mic only sends one channel: copy it to both ears
    pub mic_mono: bool,
    /// cloud: delete the PC copy after an automatic upload
    pub cloud_free_after: bool,
    /// cloud share links expire after this many days (0 = never)
    pub link_days: u32,
    /// with several clouds: where new uploads go. "" = the one with the most free space (old behaviour),
    /// "echocloud" = Echo Cloud while it has room (Discord share links), or a cloud account id ("3")
    pub cloud_target: String,
    /// copy the share link after uploading a clip by hand
    pub auto_copy_link: bool,
    /// renamed / merged game folders: game (or old folder) name -> folder name ("" = main folder)
    pub folder_alias: std::collections::BTreeMap<String, String>,
    /// apps the user removed from "Your games" and never wants treated as a game
    pub not_games: Vec<String>,
    /// the first-start setup was shown
    pub setup_done: bool,
    /// re-encode clips older than shrink_days to about half their size while the PC is idle
    pub shrink_old: bool,
    pub shrink_days: u32,
    /// controller combo that saves a clip (XInput bits, 0 = off) and its length (0 = first hotkey's length)
    pub pad_buttons: u16,
    pub pad_seconds: u32,
    /// animated watermark on exports, Discord-size copies, montages and GIFs (recorded clips stay clean):
    /// "kimbo" = Echo + "By Kimbo", "echo" = just Echo, "off"
    pub watermark_style: String,
    /// colour theme (Settings > Appearance): "" = Violet, "ocean", "ember", …, "custom" = theme_accent + theme_bg
    pub theme: String,
    pub theme_accent: String,
    pub theme_bg: String,
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
            bitrate_mbps: 20,
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
            sound_on_save: true,
            sound_on_fail: true,
            sound_volume: 0.6,
            name_by_game: true,
            game_folders: true,
            caption_model: "base".into(),
            screenshot_key: "Shift+F10".into(),
            bookmark_key: "Shift+F9".into(),
            show_overlay: true,
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
            pad_buttons: 0,
            pad_seconds: 0,
            watermark_style: "kimbo".into(),
            theme: String::new(),
            theme_accent: String::new(),
            theme_bg: String::new(),
            monitor: String::new(),
            mix_mode: "all".into(),
            vol_game: 1.0,
            vol_discord: 1.0,
            vol_browser: 1.0,
            mix_game: true,
            mix_discord: true,
            mix_browser: false,
            mic_mono: false,
            cloud_free_after: false,
            link_days: 0,
            cloud_target: String::new(),
            auto_copy_link: true,
            cloud: String::new(),
            cloud_accounts: Vec::new(),
            ec_token: String::new(),
            ec_email: String::new(),
            ec_api: String::new(),
            cloud_upload: "favs".into(),
            cloud_download: true,
            cloud_since: 0,
            capture: "screen".into(),
            codec: "h264".into(),
        }
    }
}

impl Settings {
    fn path() -> PathBuf {
        let base = std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
        base.join("Clipper").join("settings.json")
    }

    pub fn load() -> Self {
        fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let p = Self::path();
        if let Some(d) = p.parent() {
            fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        fs::write(p, serde_json::to_string_pretty(self).unwrap()).map_err(|e| e.to_string())
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
