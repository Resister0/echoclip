//! Settings page: Echo's settings.json, read and written key by key.
//!
//! The file is Echo's own (%APPDATA%\Clipper\settings.json). The prototype only ever changes the keys you touch:
//! it re-reads the file, replaces that one key, and swaps the file in atomically (tmp + rename) - every other
//! field, including the encrypted secrets ("enc:..."), stays byte-for-byte what Echo wrote.
//! Note: a RUNNING Echo keeps its own copy in memory and writes it back on its next save (it doesn't watch the
//! file), so test with Echo closed - or expect Echo to undo prototype changes.
//!
//! The page itself is one table (`section()`), mirroring ui/index.html's #page-settings: titles, texts, options and
//! hints are copied from there. Slint draws whatever rows the table returns, so search, dimmed rows and live
//! values all come from this one place.
use crate::{SBox, SNav, SOpt, SRow, ThemeCard, ThemeLine};
use serde_json::{json, Map, Value};
use slint::{Color, ModelRc, SharedString, VecModel};
use std::path::PathBuf;



/// Echo's defaults (settings.rs Default) for every key the page shows
fn defaults() -> Value {
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_else(|_| ".".into());
    json!({
        "hotkeys": [{"keys": "Alt+F10", "seconds": 30}, {"keys": "Alt+F11", "seconds": 180}],
        "resolution": "native", "fps": 60, "bitrate_mbps": 33,
        "desktop_audio": true, "desktop_volume": 1.0, "mic": true, "mic_volume": 1.0, "mic_device": "",
        "app_tracks": true,
        "clips_dir": PathBuf::from(home).join("Videos").join("EchoClips").to_string_lossy(),
        "max_storage_gb": 0, "start_with_windows": false, "start_minimized": false, "close_to_tray": true,
        "discord_presence": true, "discord_game_only": false,
        "sound_on_save": true, "sound_on_fail": true, "sound_volume": 0.6,
        "name_by_game": true, "game_folders": true, "caption_model": "base",
        "screenshot_key": if cfg!(target_os = "linux") { "Alt+F12" } else { "Shift+F10" },
        "bookmark_key": "Shift+F9", "show_overlay": true, "on_this_day": true, "idle_pause_min": 0,
        "copy_default": "full", "discord_free_mb": 20.0, "discord_basic_mb": 50.0, "discord_nitro_mb": 1000.0, "discord_custom_mb": 100.0,
        "games": [], "only_while_game": false, "sound_on_mark": true, "sound_on_shot": true, "sound_on_stop": true,
        "sound_files": {}, "session_key": "", "mic_mute_key": "", "show_key": "", "push_to_talk": false, "ptt_key": "",
        "mic_denoise": true, "cleanup_days": 0, "lib_view": "grid",
        "shrink_old": false, "shrink_days": 30, "shrink_av1": true, "smart_bitrate": true, "capture_cursor": true,
        "share_watermark": true, "smart_quality": 72, "pad_buttons": 0, "pad_seconds": 0, "watermark_style": "link",
        "theme": if cfg!(target_os = "linux") { "system" } else { "" }, "theme_accent": "", "theme_bg": "", "theme_hi": "",
        "monitor": "", "mix_mode": "all", "vol_game": 1.0, "vol_discord": 1.0, "vol_browser": 1.0, "vol_music": 1.0,
        "mix_game": true, "mix_discord": true, "mix_browser": false, "mix_music": false, "mic_mono": false,
        "cloud_free_after": false, "link_days": 0, "auto_copy_link": false, "cloud_upload": "favs", "cloud_download": false, "dl_v": 1,
        "capture": "screen", "codec": "h264", "ec_email": "", "cloud_accounts": [], "cloud_target": ""
    })
}

/// Echo's settings file: ECHO_SETTINGS (test hook), else exactly the file Echo's own settings.rs uses (echo-core:
/// %APPDATA%\Clipper\settings.json; on Linux rec_core points APPDATA at ~/.config) - Chat B: one file for both
pub fn path() -> PathBuf {
    if let Ok(p) = std::env::var("ECHO_SETTINGS") {
        return p.into();
    }
    echo_core::settings::Settings::path()
}

fn read_file(p: &PathBuf) -> Option<Map<String, Value>> {
    let s = crate::library::read_text(p)?; // QA 11 Nov 2026: without a UTF-8 byte-order mark
    match serde_json::from_str::<Value>(&s).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

pub struct Store {
    pub file: PathBuf,
    /// file + defaults for missing keys
    pub v: Map<String, Value>,
    /// the file had keys we can't parse / no file: still writable, but we say so
    pub found: bool,
    /// which button is waiting for keys: "hk:0", "shot", "mark", "long", "mute", "ptt"
    pub listening: String,
    pub sec: String,
    pub query: String,
    /// filled by main.rs: library folders (games list), sizes per folder, total size
    pub games: Vec<String>,
    pub usage: Vec<(String, u64)>,
    pub total: u64,
    pub monitors: Vec<(String, String)>,
    /// Chat B: microphones (name, label) found at start (lib_setup), and the last mic test's result
    pub mics: Vec<(String, String)>,
    pub mic_test: String,
    /// gaps-settings: the device "Windows default" / "System default" is now (Echo's list_mics default)
    pub mic_default: String,
}

impl Store {
    pub fn load() -> Store {
        let file = path();
        let raw = read_file(&file);
        let found = raw.is_some();
        let mut v = raw.unwrap_or_default();
        // echo-core's Settings::load does the same once (dl_v): "Get clips from my other PCs" was on by default and
        // downloaded every clip from another PC by itself - off once, a clip from another PC streams (12 Nov 2026).
        // Here too, so a Store read before echo-core's load can't save the old "on" back.
        if found && v.get("dl_v").and_then(|x| x.as_u64()).unwrap_or(0) < 1 {
            v.insert("cloud_download".into(), Value::Bool(false));
            v.insert("dl_v".into(), Value::from(1));
        }
        if let Value::Object(d) = defaults() {
            for (k, dv) in d {
                v.entry(k).or_insert(dv);
            }
        }
        Store { file, v, found, listening: String::new(), sec: "hotkeys".into(), query: String::new(), games: vec![], usage: vec![], total: 0, monitors: vec![], mics: vec![], mic_test: String::new(), mic_default: String::new() }
    }

    /// Chat B: settings.json changed behind our back (Echo's recorder saved the cloud sign-in): read it again
    pub fn reload(&mut self) {
        let fresh = Store::load();
        self.v = fresh.v;
        self.found = fresh.found;
    }

    pub fn b(&self, k: &str) -> bool {
        self.v.get(k).and_then(|x| x.as_bool()).unwrap_or(false)
    }
    pub fn n(&self, k: &str) -> f64 {
        self.v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0)
    }
    pub fn s(&self, k: &str) -> String {
        match self.v.get(k) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.as_f64().map(|f| if f.fract() == 0.0 { format!("{}", f as i64) } else { f.to_string() }).unwrap_or_default(),
            Some(Value::Bool(b)) => b.to_string(),
            _ => String::new(),
        }
    }

    /// Write these keys: re-read the file, replace only them, atomic swap. Err = message for a toast.
    pub fn write(&mut self, keys: &[(&str, Value)]) -> Result<(), String> {
        for (k, val) in keys {
            self.v.insert(k.to_string(), val.clone());
        }
        let mut disk = read_file(&self.file).unwrap_or_default();
        for (k, val) in keys {
            disk.insert(k.to_string(), val.clone());
        }
        if let Some(d) = self.file.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let tmp = self.file.with_extension("json.proto-tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&Value::Object(disk)).unwrap()).map_err(|e| e.to_string())?;
        // Windows: rename over a file Echo has open can fail for a moment - retry a few times
        let mut last = String::new();
        for _ in 0..5 {
            match std::fs::rename(&tmp, &self.file) {
                Ok(()) => {
                    self.found = true;
                    return Ok(());
                }
                Err(e) => last = e.to_string(),
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        let _ = std::fs::remove_file(&tmp);
        Err(format!("couldn't save settings.json: {last}"))
    }

    /// One value from the page (all values arrive as text). Returns a toast message, if any.
    pub fn set(&mut self, key: &str, val: &str) -> Option<String> {
        let num = |s: &str| s.trim().parse::<f64>().ok();
        let r = match key {
            // the slider shows the AVERAGE; the file keeps the encoder's maximum (Echo: avg / smart pct)
            "bitrate_avg" => {
                let avg = num(val)?;
                let max = (avg / self.smart_pct()).round().clamp(4.0, 80.0);
                self.write(&[("bitrate_mbps", json!(max as u64))])
            }
            // switching smart bitrate keeps the average where it was
            "smart_bitrate" => {
                let on = val == "true";
                let pct = Self::pct_of(self.n("smart_quality"));
                let br = self.n("bitrate_mbps");
                let nb = (if on { br / pct } else { br * pct }).round().clamp(4.0, 80.0);
                self.write(&[("smart_bitrate", json!(on)), ("bitrate_mbps", json!(nb as u64))])
            }
            "preset" => {
                let p = PRESETS.iter().find(|p| p.0 == val)?;
                let smart = self.smart_on();
                let max = if smart { (p.4 as f64 / 0.6).round() as u64 } else { p.4 as u64 };
                let mut k = vec![("resolution", json!(p.2)), ("fps", json!(p.3)), ("bitrate_mbps", json!(max))];
                if smart {
                    k.push(("smart_quality", json!(72)));
                }
                self.write(&k)
            }
            "hk_add" => {
                let mut hk = self.hotkeys();
                hk.push(json!({"keys": "Alt+F9", "seconds": 60}));
                self.write(&[("hotkeys", Value::Array(hk))])
            }
            _ if key.starts_with("hk_secs:") => {
                let i: usize = key[8..].parse().ok()?;
                let mut hk = self.hotkeys();
                hk.get_mut(i)?["seconds"] = json!(num(val)? as u64);
                self.write(&[("hotkeys", Value::Array(hk))])
            }
            _ if key.starts_with("hk_del:") => {
                let i: usize = key[7..].parse().ok()?;
                let mut hk = self.hotkeys();
                if hk.len() <= 1 {
                    return Some("Keep at least one hotkey".into());
                }
                if i < hk.len() {
                    hk.remove(i);
                }
                self.write(&[("hotkeys", Value::Array(hk))])
            }
            _ if key.starts_with("game_record:") || key.starts_with("game_folder:") => {
                let (field, name) = key.split_once(':')?;
                let field = if field == "game_record" { "record" } else { "folder" };
                let mut g = self.v.get("games").and_then(|x| x.as_array()).cloned().unwrap_or_default();
                let pos = g.iter().position(|r| r.get("name").and_then(|n| n.as_str()).is_some_and(|n| n.eq_ignore_ascii_case(name)));
                let i = match pos {
                    Some(i) => i,
                    None => {
                        g.push(json!({"name": name, "record": true, "folder": true}));
                        g.len() - 1
                    }
                };
                g[i][field] = json!(val == "true");
                self.write(&[("games", Value::Array(g))])
            }
            // Chat D (chat 28 rule): never a whole drive, the home folder itself or a system folder
            "clips_dir" => {
                let p = val.trim().trim_matches('"').trim_end_matches(['\\', '/']).to_string();
                if let Some(e) = bad_clips_dir(&p) {
                    return Some(e);
                }
                // QA 11 Nov 2026: a typed folder that doesn't exist yet is made now - the library only opens a clips
                // folder that exists (library.rs find_clips_dir), so after the restart it showed Videos\EchoClips
                // while new clips went to the typed folder
                if std::fs::create_dir_all(&p).is_err() {
                    return Some("Couldn't make that folder - check the drive and the name".into());
                }
                self.write(&[("clips_dir", json!(p))])
            }
            // Settings > Games: "remove" = this isn't a game (Echo's not_games), its rule goes too
            "not_game" => {
                let mut not = self.v.get("not_games").and_then(|x| x.as_array()).cloned().unwrap_or_default();
                if !not.iter().any(|x| x.as_str().is_some_and(|x| x.eq_ignore_ascii_case(val))) {
                    not.push(json!(val));
                }
                let mut g = self.v.get("games").and_then(|x| x.as_array()).cloned().unwrap_or_default();
                g.retain(|r| !r.get("name").and_then(|n| n.as_str()).is_some_and(|n| n.eq_ignore_ascii_case(val)));
                self.games.retain(|n| !n.eq_ignore_ascii_case(val));
                self.write(&[("not_games", Value::Array(not)), ("games", Value::Array(g))])
            }
            "theme" => {
                let mut k = vec![("theme", json!(val))];
                // custom without colours yet: start from the current look
                if val == "custom" && self.s("theme_accent").is_empty() {
                    let p = theme_vars(&self.s("theme"), "", "", "");
                    k.push(("theme_accent", json!(p.accent_hex)));
                    k.push(("theme_bg", json!(p.bg_hex)));
                }
                self.write(&k)
            }
            "theme_accent" | "theme_bg" | "theme_hi" => {
                let h = val.trim();
                let h = if h.starts_with('#') { h.to_string() } else { format!("#{h}") };
                if parse_hex(&h).is_none() {
                    return Some("Use a colour like #7c5cff".into());
                }
                self.write(&[(key, json!(h.to_lowercase()))])
            }
            _ => {
                // plain keys: the type comes from Echo's default (bool / whole number / decimal / text)
                let d = defaults();
                let v = match d.get(key) {
                    Some(Value::Bool(_)) => json!(val == "true"),
                    Some(Value::Number(n)) if n.is_f64() => json!((num(val)? * 1000.0).round() / 1000.0),
                    Some(Value::Number(_)) => json!(num(val)?.round().max(0.0) as u64),
                    Some(Value::String(_)) => json!(val),
                    _ => return Some(format!("{key}: unknown setting")),
                };
                self.write(&[(key, v)])
            }
        };
        match r {
            // gaps-settings (Echo's changed()): applied at once + "Settings saved" (theme changes quietly)
            Ok(()) => {
                crate::lib_set::changed(key.starts_with("theme"));
                None
            }
            Err(e) => Some(e),
        }
    }

    fn hotkeys(&self) -> Vec<Value> {
        self.v.get("hotkeys").and_then(|x| x.as_array()).cloned().unwrap_or_default()
    }

    /// a key combo arrived for the button that's listening (Echo's rules: one combo = one action, swap on clash)
    pub fn key(&mut self, combo: &str) -> Option<String> {
        let me = std::mem::take(&mut self.listening);
        if me.is_empty() {
            return None;
        }
        if me == "ptt" {
            return self.write(&[("ptt_key", json!(combo))]).err();
        }
        let slot = |i: &str| match i {
            "shot" => Some("screenshot_key"),
            "mark" => Some("bookmark_key"),
            "long" => Some("session_key"),
            "mute" => Some("mic_mute_key"),
            "show" => Some("show_key"),
            _ => None,
        };
        let names = |i: &str| match i {
            "shot" => "Screenshot",
            "mark" => "Bookmark",
            "long" => "Record",
            "mute" => "Mute mic",
            "show" => "Show / hide Echo",
            _ => "another save key",
        };
        let mut hk = self.hotkeys();
        let get = |s: &Store, hk: &Vec<Value>, i: &str| -> String {
            match slot(i) {
                Some(k) => s.s(k),
                None => i.strip_prefix("hk:").and_then(|n| n.parse::<usize>().ok()).and_then(|n| hk.get(n)).and_then(|h| h["keys"].as_str()).unwrap_or("").to_string(),
            }
        };
        let mut all: Vec<String> = ["shot", "mark", "long", "mute", "show"].iter().map(|s| s.to_string()).collect();
        all.extend((0..hk.len()).map(|i| format!("hk:{i}")));
        let old = get(self, &hk, &me);
        let other = all.iter().find(|i| **i != me && get(self, &hk, i) == combo).cloned();
        let mut writes: Vec<(String, Value)> = vec![];
        let mut put = |i: &str, v: &str, hk: &mut Vec<Value>| match slot(i) {
            Some(k) => writes.push((k.to_string(), json!(v))),
            None => {
                if let Some(h) = i.strip_prefix("hk:").and_then(|n| n.parse::<usize>().ok()).and_then(|n| hk.get_mut(n)) {
                    h["keys"] = json!(v);
                }
            }
        };
        put(&me, combo, &mut hk);
        let mut msg = None;
        if let Some(o) = &other {
            put(o, &old, &mut hk);
            msg = Some(format!("{} was used by {} - swapped", combo.replace('+', " + "), names(o)));
        }
        let mut w: Vec<(&str, Value)> = writes.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        w.push(("hotkeys", Value::Array(hk)));
        if let Err(e) = self.write(&w) {
            return Some(e);
        }
        // gaps-settings: the new key works right away (not on the recorder's next 3 s look at the file)
        crate::lib_set::changed(false);
        msg
    }

    fn pct_of(q: f64) -> f64 {
        if q <= 72.0 {
            0.6
        } else if q <= 82.0 {
            0.75
        } else {
            0.9
        }
    }
    /// Linux records a constant bitrate (gpu-screen-recorder): no "average" there
    fn smart_on(&self) -> bool {
        self.b("smart_bitrate") && !cfg!(target_os = "linux")
    }
    fn smart_pct(&self) -> f64 {
        if self.smart_on() {
            Self::pct_of(self.n("smart_quality"))
        } else {
            1.0
        }
    }

    /// quality hero: big line, GPU line, per minute, per clip, clip label
    pub fn hero(&self) -> (String, String, String, String, String) {
        let res = match self.s("resolution").as_str() {
            "native" => "Native".to_string(),
            r => format!("{r}p"),
        };
        let fps = self.s("fps");
        let br = self.n("bitrate_mbps");
        let big = if self.smart_on() {
            format!("{res}  ·  {fps} fps  ·  avg {} Mbps", (br * self.smart_pct()).round())
        } else {
            format!("{res}  ·  {fps} fps  ·  {br} Mbps")
        };
        let codec = match self.s("codec").as_str() {
            "hevc" => "HEVC",
            "av1" => "AV1",
            _ => "H.264",
        };
        let sub = format!("Zero-copy GPU pipeline (hardware {codec})");
        let mb_min = br * 60.0 / 8.0 * self.smart_pct() + 2.0;
        let secs = self.hotkeys().first().and_then(|h| h["seconds"].as_f64()).unwrap_or(60.0);
        (
            big,
            sub,
            format!("≈ {}", crate::library::fmt_size((mb_min * 1e6) as u64)),
            format!("≈ {}", crate::library::fmt_size((mb_min * 1e6 * secs / 60.0) as u64)),
            format!("per {} clip", crate::library::fmt_len(secs as u64)),
        )
    }
}

const PRESETS: [(&str, &str, &str, u32, u32, &str); 4] = [
    ("low", "Low", "720", 30, 6, "720p · 30 FPS · 6 Mbps"),
    ("std", "Standard", "1080", 60, 12, "1080p · 60 FPS · 12 Mbps"),
    ("high", "High", "native", 60, 20, "Native · 60 FPS · 20 Mbps"),
    ("ultra", "Ultra", "native", 60, 40, "Native · 60 FPS · 40 Mbps"),
];
const LENGTHS: [u64; 9] = [15, 30, 60, 120, 180, 300, 600, 900, 1200];

pub const NAV: [(&str, &str, &str, &str); 9] = [
    ("hotkeys", "Capture Hotkeys", "RECORDING", "M4 6h16a2 2 0 0 1 2 2v8a2 2 0 0 1 -2 2h-16a2 2 0 0 1 -2 -2v-8a2 2 0 0 1 2 -2z M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"),
    ("quality", "Quality", "", "M4 6h10M18 6h2M4 12h4M12 12h8M4 18h12M20 18h0 M14 6a2 2 0 1 0 4 0a2 2 0 1 0 -4 0z M8 12a2 2 0 1 0 4 0a2 2 0 1 0 -4 0z M16 18a2 2 0 1 0 4 0a2 2 0 1 0 -4 0z"),
    ("audio", "Audio", "", "M4 9v6h4l5 4V5L8 9z M16 9a4 4 0 0 1 0 6M19 6a8 8 0 0 1 0 12"),
    ("games", "Games", "", "M7 7h10a5 5 0 0 1 5 5v1a5 5 0 0 1 -5 5h-10a5 5 0 0 1 -5 -5v-1a5 5 0 0 1 5 -5z M7 11v3M5.5 12.5h3 M15.2 11.5a0.8 0.8 0 1 0 1.6 0a0.8 0.8 0 1 0 -1.6 0z M17.2 13.5a0.8 0.8 0 1 0 1.6 0a0.8 0.8 0 1 0 -1.6 0z"),
    ("storage", "Clips & Storage", "STORAGE", "M5 4h14a2 2 0 0 1 2 2v3a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-3a2 2 0 0 1 2 -2z M5 13h14a2 2 0 0 1 2 2v3a2 2 0 0 1 -2 2h-14a2 2 0 0 1 -2 -2v-3a2 2 0 0 1 2 -2z M7 7.5h.01M7 16.5h.01"),
    ("cloud", "Cloud", "", "M7 19a5 5 0 0 1-.7-9.95A6 6 0 0 1 18 9a4.5 4.5 0 0 1 0 10z"),
    ("profile", "Echo Profile", "APP", "M8 8a4 4 0 1 0 8 0a4 4 0 1 0 -8 0z M4 21a8 8 0 0 1 16 0"),
    ("appearance", "Appearance", "", "M12 3a9 9 0 1 0 0 18c1.1 0 1.6-.8 1.6-1.6 0-.5-.2-.8-.4-1.1-.3-.3-.4-.7-.4-1.1 0-.9.7-1.6 1.6-1.6H16a5 5 0 0 0 5-5C21 6.6 17 3 12 3z M6.5 11a1 1 0 1 0 2 0a1 1 0 1 0 -2 0z M9 7a1 1 0 1 0 2 0a1 1 0 1 0 -2 0z M13.5 7a1 1 0 1 0 2 0a1 1 0 1 0 -2 0z"),
    ("app", "App Settings", "", "M9 12a3 3 0 1 0 6 0a3 3 0 1 0 -6 0z M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M4.9 19.1L7 17M17 7l2.1-2.1"),
];

pub fn nav() -> ModelRc<SNav> {
    let v: Vec<SNav> = NAV.iter().map(|(id, label, head, icon)| SNav { id: (*id).into(), label: (*label).into(), head: (*head).into(), icon: (*icon).into() }).collect();
    ModelRc::new(VecModel::from(v))
}

// ---------- rows ----------

/// one row while building (turned into Slint's SRow at the end)
#[derive(Default, Clone)]
struct R {
    kind: &'static str,
    key: String,
    key2: String,
    label: String,
    sub: String,
    on: bool,
    on2: bool,
    value: String,
    num: f32,
    min: f32,
    max: f32,
    step: f32,
    vtext: String,
    opts: Vec<(String, String, String)>,
    keys: Vec<String>,
    btn: String,
    act: String,
    btn2: String,
    act2: String,
    dep: bool,
    why: String,
    listen: bool,
    col: bool,
    /// gaps-settings: a picture file shown in the row (a game's icon)
    icon: String,
}

struct B {
    title: String,
    btn: String,
    act: String,
    rows: Vec<R>,
}

fn opt(v: &str, l: &str) -> (String, String, String) {
    (v.into(), l.into(), String::new())
}
fn opth(v: &str, l: &str, h: &str) -> (String, String, String) {
    (v.into(), l.into(), h.into())
}
fn days_opts(v: &[(u32, &str)]) -> Vec<(String, String, String)> {
    v.iter().map(|(n, l)| opt(&n.to_string(), l)).collect()
}

/// KeyboardEvent.code names -> what Echo shows (KeyC -> C, Digit1 -> 1, Control -> Ctrl)
/// Chat D: Echo's clips-folder rule (chat 28) - refuse a drive root, the home folder itself and system folders
pub fn bad_clips_dir(p: &str) -> Option<String> {
    if p.is_empty() {
        return Some("Pick a folder for your clips".into());
    }
    let path = std::path::Path::new(p);
    if !path.is_absolute() {
        return Some("Use a full path, e.g. D:\\Clips".into());
    }
    let low = p.replace('\\', "/").trim_end_matches('/').to_lowercase();
    // "C:" / "/" / "/mnt/d": a whole drive
    if path.parent().is_none() || low.len() <= 2 || (low.starts_with("/mnt/") && low.matches('/').count() == 2) {
        return Some("Pick a folder, not a whole drive - e.g. D:\\Clips".into());
    }
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_default().replace('\\', "/").trim_end_matches('/').to_lowercase();
    if !home.is_empty() && low == home {
        return Some("Pick a folder inside your home folder (e.g. Videos/EchoClips), not the home folder itself".into());
    }
    let env = |k: &str| std::env::var(k).unwrap_or_default().replace('\\', "/").trim_end_matches('/').to_lowercase();
    let mut sys: Vec<String> = vec![env("SystemRoot"), env("ProgramFiles"), env("ProgramFiles(x86)"), env("ProgramData"), env("APPDATA"), env("LOCALAPPDATA")];
    sys.extend(["c:/windows", "c:/program files", "c:/program files (x86)", "c:/programdata"].map(String::from));
    if cfg!(not(windows)) {
        sys.extend(["/usr", "/etc", "/bin", "/sbin", "/lib", "/lib64", "/boot", "/dev", "/proc", "/sys", "/var", "/opt", "/root", "/run", "/tmp", "/home", "/media", "/mnt"].map(String::from));
    }
    if sys.iter().filter(|s| !s.is_empty()).any(|s| low == *s || (low.starts_with(&format!("{s}/")) && !s.ends_with("/tmp") && s != "/home" && s != "/media" && s != "/mnt" && s != "/root" && s != "/var" && s != "/opt")) {
        return Some("That's a system folder - pick a folder of your own, e.g. Videos/EchoClips".into());
    }
    None
}

/// gaps-settings: the self-test's button + result lines (lib_set keeps the state). Settings = Echo's #stres (every
/// check + a summary); setup = Echo's #sutest (only the problems in red, or one green line). opts: (kind, name, detail)
fn self_test_row(r: &mut R, setup: bool) {
    let (running, res) = crate::lib_set::self_test_state();
    if running {
        // 11 Nov 2026: counts down the 6 s of recording (lib_set ticks once a second), then the checks
        let s = crate::lib_set::self_test_secs();
        let recording = s < 6;
        r.btn = if recording { format!("Testing… ({} s)", 6 - s) } else { "Checking…".into() };
        let line = if recording {
            "Recording a short test clip - play some sound and say something now"
        } else {
            "Checking the test clip - video, picture and every audio track…"
        };
        r.opts = vec![opth("wait", "", line)];
        return;
    }
    r.opts = match res {
        None => vec![],
        Some(Err(e)) => vec![opth(if setup { "badline" } else { "bad" }, "", &e)],
        Some(Ok(rows)) if setup => {
            let bad: Vec<_> = rows.iter().filter(|x| !x.1).map(|(n, _, d)| opth("badline", "", &format!("× {n}: {d}"))).collect();
            if bad.is_empty() {
                vec![opth("okline", "", "✓ Everything works - you're ready to clip.")]
            } else {
                bad
            }
        }
        Some(Ok(rows)) => {
            let bad = rows.iter().filter(|x| !x.1).count();
            let mut v: Vec<_> = rows.iter().map(|(n, ok, d)| opth(if *ok { "ok" } else { "bad" }, n, d)).collect();
            v.push(opth("", "", &if bad > 0 { format!("{bad} problem{} found - details are in the log too.", if bad == 1 { "" } else { "s" }) } else { "Everything works.".into() }));
            v
        }
    };
}

/// gaps-settings: Echo's game icon file (<Echo's data folder>/icons/<safe name>.png, recorder.rs safe_name), "" = none
fn game_icon_file(name: &str) -> String {
    let safe: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect();
    let p = echo_core::recorder::app_dir_pub().join("icons").join(format!("{safe}.png"));
    if p.is_file() {
        p.to_string_lossy().into_owned()
    } else {
        String::new()
    }
}

thread_local! {
    /// game icons already loaded: file -> (modified, picture)
    static ICONS: std::cell::RefCell<std::collections::HashMap<String, (Option<std::time::SystemTime>, slint::Image)>> = Default::default();
}
fn icon_image(file: &str) -> slint::Image {
    if file.is_empty() {
        return slint::Image::default();
    }
    let t = std::fs::metadata(file).and_then(|m| m.modified()).ok();
    ICONS.with(|c| {
        let mut c = c.borrow_mut();
        if let Some((t0, img)) = c.get(file) {
            if *t0 == t {
                return img.clone();
            }
        }
        let img = slint::Image::load_from_path(std::path::Path::new(file)).unwrap_or_default();
        c.insert(file.to_string(), (t, img.clone()));
        img
    })
}

pub fn key_parts(keys: &str) -> Vec<String> {
    if keys.is_empty() {
        return vec![];
    }
    keys.split('+').map(|k| k.trim_start_matches("Key").trim_start_matches("Digit").replace("Control", "Ctrl")).collect()
}

fn pad_name(bits: u64) -> String {
    let n = [
        (0x20, "Back"), (0x10, "Start"), (0x100, "LB"), (0x200, "RB"), (0x400, "LT"), (0x800, "RT"), (0x1000, "A"), (0x2000, "B"), (0x4000, "X"), (0x8000, "Y"),
        (0x40, "L-stick"), (0x80, "R-stick"), (0x1, "Up"), (0x2, "Down"), (0x4, "Left"), (0x8, "Right"),
    ];
    let v: Vec<&str> = n.iter().filter(|(b, _)| bits & b != 0).map(|(_, s)| *s).collect();
    if v.is_empty() {
        "Off".into()
    } else {
        v.join(" + ")
    }
}

impl Store {
    fn sw(&self, key: &str, label: &str, sub: &str) -> R {
        R { kind: "switch", key: key.into(), label: label.into(), sub: sub.into(), on: self.b(key), ..Default::default() }
    }
    fn sel(&self, key: &str, label: &str, sub: &str, opts: Vec<(String, String, String)>) -> R {
        R { kind: "select", key: key.into(), label: label.into(), sub: sub.into(), value: self.s(key), opts, ..Default::default() }
    }
    /// segmented buttons, hint of the picked option under the label (Echo's select.segsrc)
    fn seg(&self, key: &str, label: &str, opts: Vec<(String, String, String)>) -> R {
        let value = self.s(key);
        let hint = opts.iter().find(|o| o.0 == value).map(|o| o.2.clone()).unwrap_or_default();
        R { kind: "seg", key: key.into(), label: label.into(), sub: hint, value, opts, col: true, ..Default::default() }
    }
    fn pct_slider(&self, key: &str, label: &str, sub: &str, max: f32, sw: Option<&str>) -> R {
        let n = self.n(key) as f32;
        R {
            kind: "slider",
            key: key.into(),
            key2: sw.unwrap_or("").into(),
            on2: sw.map(|k| self.b(k)).unwrap_or(false),
            label: label.into(),
            sub: sub.into(),
            num: n,
            min: 0.0,
            max,
            step: 0.05,
            vtext: format!("{}%", (n * 100.0).round()),
            ..Default::default()
        }
    }
    fn button(&self, label: &str, sub: &str, btn: &str, act: &str) -> R {
        R { kind: "button", label: label.into(), sub: sub.into(), btn: btn.into(), act: act.into(), ..Default::default() }
    }
    fn hk(&self, id: &str, label: &str, sub: &str, keys: &str) -> R {
        // gaps (10 Nov): Linux with KDE's portal - the key KDE really uses (Echo's keyShow; not for Mute mic / PTT)
        let kde = if id == "mute" || id == "ptt" { None } else { crate::hdr_home::kde_show(keys) };
        R {
            kind: "hotkey",
            key: id.into(),
            label: label.into(),
            sub: sub.into(),
            btn: if kde.as_ref().is_some_and(|k| k.is_empty()) { "No key in KDE".into() } else { String::new() },
            keys: kde.unwrap_or_else(|| key_parts(keys)),
            listen: self.listening == id,
            act: format!("hkoff:{id}"),
            ..Default::default()
        }
    }

    /// title, intro text, boxes, footnote
    fn section(&self, sec: &str) -> (String, String, Vec<B>, String) {
        let bx = |title: &str, rows: Vec<R>| B { title: title.into(), btn: String::new(), act: String::new(), rows };
        match sec {
            "setup" => {
                let find = |sec: &str, f: &dyn Fn(&R) -> bool| -> Option<R> { self.section(sec).2.into_iter().flat_map(|b| b.rows).find(|r| f(r)) };
                let mut out = vec![];
                if let Some(mut hk) = find("hotkeys", &|r| r.kind == "hotkey") {
                    hk.label = "Save key".into();
                    hk.act.clear();
                    hk.sub = "Press it in a game to save what just happened. Click it to pick another key. More keys and lengths: Settings › Capture Hotkeys.".into();
                    out.push(bx("1 · Save key", vec![hk]));
                }
                let mut mic = vec![];
                if let Some(mut m) = find("audio", &|r| r.key == "mic_device") {
                    m.dep = false;
                    m.why.clear();
                    m.key2 = "mic".into();
                    m.on2 = self.b("mic");
                    m.sub = "Record my mic (switch) - pick your headset if the default is something else.".into();
                    mic.push(m);
                }
                let mut t = self.button("Mic test", "Say something during the 3 seconds.", "Test mic", "mictest");
                if !self.mic_test.is_empty() {
                    t.sub = self.mic_test.clone();
                }
                mic.push(t);
                out.push(bx("2 · Microphone", mic));
                if let Some(p) = find("quality", &|r| r.kind == "presets") {
                    out.push(bx("3 · Quality", vec![p]));
                }
                if let Some(p) = find("hotkeys", &|r| r.act == "padset") {
                    out.push(bx("4 · Controller (optional)", vec![p]));
                }
                out.push(bx(
                    "5 · Save disk space (optional)",
                    vec![self.sw("shrink_old", "Shrink old clips", "Shrink clips older than 30 days to about half their size while the PC is idle. Favourites stay as they are.")],
                ));
                // gaps-settings: Echo's step "6 · Test it" (setupTest): only the problems in red, or one green line
                let mut test = self.button("", "Play some sound and say something while it runs.", "Run self-test", "selftest");
                self_test_row(&mut test, true);
                out.push(bx("6 · Test it", vec![test]));
                // Echo's drawSetupImport: the text, the apps found, then "Somewhere else?" / "Nothing found
                // automatically." + [Choose folder…] [Remind me later]
                let mut imp: Vec<R> = vec![R {
                    kind: "info",
                    sub: "Used Medal, SteelSeries Moments, Outplayed or something else? Echo copies your old clips in, with their dates and game folders. The old app's files aren't touched.".into(),
                    ..Default::default()
                }];
                imp.extend(
                    crate::import::lines(std::path::Path::new(&self.s("clips_dir")))
                        .into_iter()
                        .enumerate()
                        .map(|(k, (label, sub, btn))| self.button(&label, &sub, &btn, &format!("import:{k}"))),
                );
                let mut more = self.button("", if imp.len() > 1 { "Somewhere else?" } else { "Nothing found automatically." }, "Choose folder…", "import");
                more.btn2 = if crate::lib_set::import_reminder_set() { "Will remind you".into() } else { "Remind me later".into() };
                more.act2 = "impremind".into();
                imp.push(more);
                out.push(bx("7 · Clips from another app (optional)", imp));
                ("Welcome to Echo".into(), String::new(), out, String::new())
            }
            "hotkeys" => {
                let mut rows = vec![];
                // gaps (10 Nov): Linux with KDE's shortcut portal (Echo's renderHotkeys)
                if crate::hdr_home::kde_on() {
                    rows.push(R {
                        kind: "info",
                        label: "Keys are set in KDE".into(),
                        sub: format!("{}. Echo shows the keys KDE uses.", crate::hdr_home::KDE_TIP),
                        ..Default::default()
                    });
                }
                for (i, h) in self.hotkeys().iter().enumerate() {
                    let mut r = self.hk(&format!("hk:{i}"), "Clip", "Save the previous moments of gameplay", h["keys"].as_str().unwrap_or(""));
                    r.opts = LENGTHS.iter().map(|s| opt(&s.to_string(), &crate::library::fmt_len(*s))).collect();
                    r.value = h["seconds"].as_u64().unwrap_or(30).to_string();
                    r.key2 = format!("hk_secs:{i}");
                    r.act = format!("hkdel:{i}");
                    rows.push(r);
                }
                rows.push(self.hk("shot", "Screenshot", "Saves a full-resolution PNG to your clips folder › Screenshots", &self.s("screenshot_key")));
                rows.push(self.hk("mark", "Bookmark", "Marks a moment in the buffer - it shows as a yellow marker on the next clip you save", &self.s("bookmark_key")));
                rows.push(self.hk("long", "Record", "Start / stop recording by hand (like the Record button) - saved as a clip when you stop", &self.s("session_key")));
                rows.push(self.hk("mute", "Mute mic", "Turns your mic off / on in recordings (a popup shows which)", &self.s("mic_mute_key")));
                rows.push(self.hk("show", "Show / hide Echo", "Brings Echo up over your game, or back to the tray - no alt-tab", &self.s("show_key")));
                let ptt_sub = format!(
                    "{} - a single key (e.g. V, Caps Lock, Left Alt)",
                    if self.b("push_to_talk") { "Hold it to record your mic" } else { "Turn on Push-to-talk under Audio to use it" }
                );
                rows.push(self.hk("ptt", "Push-to-talk key", &ptt_sub, &self.s("ptt_key")));
                let mut hkbox = bx("Hotkeys", rows);
                hkbox.btn = "+ Add hotkey".into();
                hkbox.act = "hkadd".into();
                let pad = R {
                    kind: "button",
                    label: "Save with a controller".into(),
                    sub: if cfg!(windows) {
                        "A button combo on an Xbox-style (XInput) controller saves a clip. PlayStation pads work through Steam Input or DS4Windows."
                    } else {
                        "A button combo on a controller saves a clip."
                    }
                    .into(),
                    vtext: pad_name(self.n("pad_buttons") as u64),
                    btn: if crate::lib_set::pad_busy() { "Press the combo…".into() } else { "Set…".into() },
                    act: "padset".into(),
                    btn2: "Off".into(),
                    act2: "padoff".into(),
                    ..Default::default()
                };
                let padlen = self.sel(
                    "pad_seconds",
                    "Controller clip length",
                    "",
                    days_opts(&[(0, "Same as the first hotkey"), (30, "30 s"), (60, "1 min"), (120, "2 min"), (180, "3 min"), (300, "5 min"), (600, "10 min")]),
                );
                let buf = self.hotkeys().iter().filter_map(|h| h["seconds"].as_u64()).max().unwrap_or(30);
                (
                    "Capture Hotkeys".into(),
                    "Each hotkey saves a different amount of the moments you just played. Click a hotkey to change it.".into(),
                    vec![hkbox, bx("Controller", vec![pad, padlen])],
                    format!("Echo keeps the last {} in a rolling buffer (your longest hotkey).", crate::library::fmt_len(buf)),
                )
            }
            "quality" => {
                let smart = self.smart_on();
                let pct = self.smart_pct();
                let br = self.n("bitrate_mbps");
                let cur = PRESETS
                    .iter()
                    .find(|p| {
                        let max = if smart { (p.4 as f64 / 0.6).round() } else { p.4 as f64 };
                        self.s("resolution") == p.2 && self.n("fps") as u32 == p.3 && br == max && (!smart || self.n("smart_quality") as u32 == 72)
                    })
                    .map(|p| p.0)
                    .unwrap_or("");
                let presets = R {
                    kind: "presets",
                    key: "preset".into(),
                    value: cur.into(),
                    opts: PRESETS.iter().map(|p| opth(p.0, p.1, p.5)).collect(),
                    ..Default::default()
                };
                let res = self.s("resolution");
                let rec = match res.as_str() {
                    "720" => "6-8",
                    "1080" => "12",
                    "1440" => "20",
                    "native" => "20 (1440p) or 40 (4K)",
                    _ => "20",
                };
                let rec_txt = format!(
                    "About {rec} Mbps {}is plenty for {} at {} fps.",
                    if smart { "average " } else { "" },
                    if res == "native" { "native resolution".to_string() } else { format!("{res}p") },
                    self.s("fps")
                );
                let avg = (br * pct).round() as f32;
                let bitrate = R {
                    kind: "slider",
                    key: "bitrate_avg".into(),
                    label: "Bitrate".into(),
                    sub: if smart { format!("Average. Busy moments can use up to {br} Mbps. {rec_txt}") } else { rec_txt },
                    num: avg,
                    min: 4.0,
                    max: (80.0 * pct).floor() as f32,
                    step: 1.0,
                    vtext: format!("{avg} Mbps"),
                    ..Default::default()
                };
                let mut q = vec![
                    presets,
                    self.seg("resolution", "Resolution", vec![opt("native", "Native"), opt("1440", "1440p"), opt("1080", "1080p"), opt("720", "720p")]),
                    self.seg("fps", "Frame rate", vec![opt("30", "30"), opt("60", "60"), opt("120", "120"), opt("144", "144")]),
                    bitrate,
                ];
                // Resolution's own hint (Echo shows it in the label line)
                q[1].sub = "Scaled on the GPU, never upscaled".into();
                if cfg!(windows) {
                    q.push(self.sw(
                        "smart_bitrate",
                        "Smart bitrate",
                        "Busy moments (running, driving, fights) can go well above the bitrate, calm moments use much less. Clips stay about the same size, but action looks sharper.",
                    ));
                    if smart {
                        q.push(self.seg(
                            "smart_quality",
                            "Smart bitrate quality",
                            vec![
                                opth("72", "Balanced", "Busy moments get up to 1.7x the bitrate. The presets use this."),
                                opth("82", "High", "Same maximum as Balanced, but a higher average: about 25% bigger clips, a little sharper overall."),
                                opth("90", "Very high", "Same maximum, highest average: the biggest clips, close to always using the maximum."),
                            ],
                        ));
                    }
                }
                let mut mons = vec![opt("", "Main monitor")];
                for (id, name) in &self.monitors {
                    mons.push(opt(id, name));
                }
                let curmon = self.s("monitor");
                if !curmon.is_empty() && !mons.iter().any(|m| m.0 == curmon) {
                    mons.push(opt(&curmon, &format!("{curmon} (not connected)")));
                }
                let mut cap = vec![self.sel("monitor", "Monitor", "HDR monitors are converted to normal colours automatically.", mons)];
                if cfg!(windows) {
                    cap.push(self.seg(
                        "capture",
                        "Record",
                        vec![
                            opth("screen", "Whole monitor", "Records everything on the monitor, including Discord or anything else on top."),
                            opth("game", "Game window when open", "Switches to just the game window while a game is open, so overlays on top stay out of the clip. Switching takes a few seconds and starts a fresh buffer."),
                        ],
                    ));
                }
                cap.push(self.sw("capture_cursor", "Mouse pointer", "Show the pointer in clips, e.g. in menus, phones and inventories. Games that hide it while you play stay clean."));
                cap.push(self.seg(
                    "codec",
                    "Video format",
                    vec![
                        opth("h264", "H.264", "Plays everywhere: Discord, phones, browsers."),
                        opth("hevc", "HEVC", "About 30-40% smaller at the same quality, but may not play in Discord."),
                        opth("av1", "AV1", "The smallest. Needs a Radeon RX 7000 / GeForce RTX 40 or newer."),
                    ],
                ));
                cap.push(R { kind: "info", sub: "Edited and shrunk copies are always H.264.".into(), ..Default::default() });
                ("Quality".into(), "How sharp and smooth your clips are, and how much space they take.".into(), vec![bx("Preset", q), bx("Capture", cap)], String::new())
            }
            "audio" => {
                let mic_off = !self.b("mic");
                let apps = self.b("app_tracks");
                let mix_apps = apps && self.s("mix_mode") == "apps";
                let dim = |mut r: R, off: bool, why: &str| {
                    r.dep = off;
                    r.why = if off { why.into() } else { String::new() };
                    r
                };
                let mic_why = "The microphone is off";
                // Echo's loadMics: "Windows default (<device>)", a saved mic that's unplugged = "<name> (not connected)"
                let sys = if cfg!(windows) { "Windows default" } else { "System default" };
                let sys = if self.mic_default.is_empty() { sys.to_string() } else { format!("{sys} ({})", self.mic_default) };
                let mut mics = vec![opt("", &sys)];
                let curmic = self.s("mic_device");
                for (n, l) in &self.mics {
                    mics.push(opt(n, l));
                }
                if !curmic.is_empty() && !self.mics.iter().any(|m| m.0 == curmic) {
                    mics.push(opt(&curmic, &format!("{} (not connected)", echo_core::recorder::mic_label(&curmic))));
                }
                let src = vec![
                    self.pct_slider("desktop_volume", "Desktop audio", "Everything playing on your default output device", 2.0, Some("desktop_audio")),
                    self.pct_slider("mic_volume", "Microphone", if cfg!(windows) { "Your default Windows input device" } else { "Your default input device" }, 2.0, Some("mic")),
                    dim(self.sel("mic_device", "Microphone device", if cfg!(windows) { "Pick your headset mic if Windows' default is something else (like a controller)" } else { "Pick your headset mic if the default is something else (like a controller)" }, mics), mic_off, mic_why),
                    dim(self.sw("mic_denoise", "Remove mic background noise", "Filters out keyboard clicks, fans and hum from your mic (AI noise filter, light on the CPU)"), mic_off, mic_why),
                    dim(self.sw("mic_mono", "Mono microphone", "Turn on if your voice is only in one ear in clips"), mic_off, mic_why),
                    dim(self.sw("push_to_talk", "Push-to-talk", "Only record your mic while you hold the push-to-talk key (set it under Hotkeys)"), mic_off, mic_why),
                ];
                let app_lbl = |orig: &str, own: &str| -> String {
                    if apps && !mix_apps {
                        own.into()
                    } else {
                        orig.into()
                    }
                };
                let mixrow = |key: &str, sw: &str, label: &str, orig: &str, own: &str| {
                    let mut r = self.pct_slider(key, label, &app_lbl(orig, own), 2.0, Some(sw));
                    r.dep = !apps;
                    r.why = if !apps { "Needs Per-app tracks (below) - sets the volume of that app's own track".into() } else { String::new() };
                    // the switch alone is dimmed when the mix has everything on the PC
                    r.step = 0.05;
                    if apps && !mix_apps {
                        r.kind = "slider-dimsw";
                    }
                    r
                };
                let mix = vec![
                    dim(
                        self.sel(
                            "mix_mode",
                            "Mix contains",
                            "\"Only the apps below\" leaves out everything else on the PC (notifications, other apps…). Needs Per-app tracks.",
                            vec![opt("all", "Everything on the PC + mic"), opt("apps", "Only the apps below + mic")],
                        ),
                        !apps,
                        "Turn on Per-app tracks (below) to choose which apps are in the mix",
                    ),
                    mixrow("vol_game", "mix_game", "Game", "In the mix (when \"only the apps below\") · volume", "Volume of its own track (the mix has everything on the PC)"),
                    mixrow("vol_discord", "mix_discord", "Discord", "In the mix · volume", "Volume of its own track (the mix has everything on the PC)"),
                    mixrow("vol_browser", "mix_browser", "Browsers", "In the mix · volume (Chrome, Brave, Edge, Firefox, Opera)", "Volume of its own track (Chrome, Brave, Edge, Firefox, Opera)"),
                    mixrow("vol_music", "mix_music", "Music apps", "In the mix · volume (Spotify, Apple Music, TIDAL, Deezer, VLC…)", "Volume of its own track (Spotify, Apple Music, TIDAL, Deezer, VLC…)"),
                ];
                let tracks = vec![self.sw(
                    "app_tracks",
                    "Per-app tracks",
                    "Also record the game, Discord, your browser (YouTube, Twitch…) and music apps (Spotify…) on their own tracks, so you can mute or lower each one in the editor",
                )];
                (
                    "Audio".into(),
                    "What gets recorded into your clips. Volume changes apply instantly.".into(),
                    vec![bx("Sources", src), bx("Mix (what players hear)", mix), bx("Separate tracks", tracks)],
                    "Clips also store Mic and each app as separate tracks - press U in the editor to split them. Any microphone works (it's resampled automatically).".into(),
                )
            }
            "games" => {
                let when = vec![self.sw(
                    "only_while_game",
                    "Only record while a game is running",
                    "Echo waits until a game is open (handy if you don't want desktop clips). Starts a few seconds after the game appears.",
                )];
                let rules = self.v.get("games").and_then(|x| x.as_array()).cloned().unwrap_or_default();
                let rule = |n: &str| {
                    rules
                        .iter()
                        .find(|r| r["name"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(n)))
                        .map(|r| (r["record"].as_bool().unwrap_or(true), r["folder"].as_bool().unwrap_or(true)))
                        .unwrap_or((true, true))
                };
                let mut names: Vec<String> = self.games.clone();
                for r in &rules {
                    if let Some(n) = r["name"].as_str() {
                        if !names.iter().any(|x| x.eq_ignore_ascii_case(n)) {
                            names.push(n.into());
                        }
                    }
                }
                let not: Vec<String> = self.v.get("not_games").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_lowercase())).collect()).unwrap_or_default();
                names.retain(|n| !not.contains(&n.to_lowercase()));
                let mut list: Vec<R> = names
                    .iter()
                    .map(|n| {
                        let (rec, fol) = rule(n);
                        let mut r = R { kind: "game", key: n.clone(), label: n.clone(), value: n.chars().next().unwrap_or('?').to_uppercase().to_string(), on: rec, on2: fol, act: format!("gameremove:{n}"), ..Default::default() };
                        // gaps-settings: Echo's game icon (g.icon) and, for GTA V clips, [Move to FiveM]
                        r.icon = game_icon_file(n);
                        if n == "GTA V" && !crate::lib_set::gta_clips().is_empty() {
                            r.btn = "Move to FiveM".into();
                            r.act2 = "fivem".into();
                        }
                        r
                    })
                    .collect();
                if list.is_empty() {
                    list.push(R { kind: "info", sub: "No games yet - play something with Echo running.".into(), ..Default::default() });
                }
                (
                    "Games".into(),
                    "Choose per game whether Echo records it and gives it its own folder. Games show up here after you've played them with Echo running.".into(),
                    vec![bx("When to record", when), bx("Your games", list)],
                    String::new(),
                )
            }
            "storage" => {
                let mut acc = 0.0;
                let starts: Vec<String> = self
                    .usage
                    .iter()
                    .map(|(_, b)| {
                        let s = format!("{acc:.4}");
                        acc += if self.total > 0 { *b as f64 / self.total as f64 } else { 0.0 };
                        s
                    })
                    .collect();
                let usage = R {
                    kind: "usage",
                    keys: starts,
                    opts: self
                        .usage
                        .iter()
                        .map(|(n, b)| (format!("{:.4}", if self.total > 0 { *b as f64 / self.total as f64 } else { 0.0 }), n.clone(), crate::library::fmt_size(*b)))
                        .collect(),
                    ..Default::default()
                };
                let mut path = R { kind: "path", key: "clips_dir".into(), label: "Clips folder".into(), value: self.s("clips_dir"), btn: "Open".into(), act: "openclips".into(), ..Default::default() };
                path.sub = "Type a folder and press Enter, or Choose… one. New clips are saved there; Echo restarts to show its clips.".into();
                path.btn2 = "Choose…".into();
                path.act2 = "pickclips".into();
                let mut shrink = self.sel(
                    "shrink_days",
                    "Shrink old clips",
                    "While the PC is idle and no game runs, clips older than this are re-encoded at a steady quality: calm clips shrink a lot, busy ones less, and clips that wouldn't get at least 15% smaller are left alone. Each clip once; favourites stay as they are.",
                    days_opts(&[(7, "After 7 days"), (14, "After 14 days"), (30, "After 30 days"), (60, "After 60 days"), (90, "After 90 days")]),
                );
                shrink.key2 = "shrink_old".into();
                shrink.on2 = self.b("shrink_old");
                let mut cleanup = self.sel(
                    "cleanup_days",
                    "Auto-delete old clips",
                    if cfg!(windows) { "Clips older than this go to the Recycle Bin (checked at start and every few hours). Favourites are always kept." } else { "Clips older than this go to the Trash (checked at start and every few hours). Favourites are always kept." },
                    days_opts(&[(0, "Never"), (7, "After 7 days"), (14, "After 14 days"), (30, "After 30 days"), (60, "After 60 days"), (90, "After 90 days"), (180, "After 6 months")]),
                );
                cleanup.btn = "Clean up now".into();
                cleanup.act = "cleannow".into();
                let limits = R {
                    kind: "numbers",
                    label: "Discord limits (MB)".into(),
                    sub: "Free / Nitro Basic / Nitro / Custom - change them if Discord does".into(),
                    opts: ["discord_free_mb", "discord_basic_mb", "discord_nitro_mb", "discord_custom_mb"].iter().map(|k| opt(k, &self.s(k))).collect(),
                    ..Default::default()
                };
                let mut st = vec![
                    path,
                    self.sel(
                        "max_storage_gb",
                        "Storage limit",
                        if cfg!(windows) { "When full, the oldest clips go to the Recycle Bin" } else { "When full, the oldest clips go to the Trash" },
                        vec![opt("0", "Unlimited"), opt("25", "25 GB"), opt("50", "50 GB"), opt("100", "100 GB"), opt("250", "250 GB"), opt("500", "500 GB"), opt("1000", "1000 GB")],
                    ),
                    cleanup,
                    shrink,
                ];
                // gaps-settings: Echo's #av1row, only when the graphics card makes AV1 (av1_ready, probed 15 s after start)
                if crate::lib_set::av1_ok() {
                    st.push(self.sw(
                        "shrink_av1",
                        "Shrink with AV1",
                        "Your graphics card can make AV1 files: the same quality at about half the size. Copies, share links and cloud uploads still go out as H.264, so they play on every phone and PC.",
                    ));
                }
                st.extend([
                    self.sw("name_by_game", "Name clips after the game", "\"Valorant 2026-10-31 …\" instead of \"Clip 2026-10-31 …\", based on the window in front when you press the hotkey"),
                    self.sw("game_folders", "Game folders", "Save each game's clips in its own folder (…\\Valorant\\…). Clips outside a game stay in the main folder."),
                    self.button(
                        "Sort older clips into game folders",
                        "Moves clips with a game in their name (\"FiveM 2026-…\", \"fivem_chase.mp4\", \"GTA V - Moments\") from the main folder and \"Exported Clips\" into that game's folder. Imports do this by themselves.",
                        "Sort clips",
                        "sortclips",
                    ),
                    self.sel(
                        "copy_default",
                        "Copy button copies",
                        "Clips bigger than the size are shrunk (cached copy, the original is kept). The small arrow next to Copy, or Ctrl+click, lets you pick each time.",
                        vec![opt("full", "Full size"), opt("free", "Discord Free"), opt("basic", "Nitro Basic"), opt("nitro", "Nitro"), opt("custom", "Custom")],
                    ),
                    self.sel(
                        "watermark_style",
                        "Watermark",
                        "An animated Echo logo in the bottom-right corner of editor exports, Discord-size copies, montages and GIFs, and on share link pages. Your recorded clips and full-size copies stay clean.",
                        vec![opt("link", "Echo · echoclip.link"), opt("kimbo", "Echo · By Kimbo"), opt("echo", "Echo"), opt("off", "Off")],
                    ),
                    limits,
                    R { kind: "button", label: "Currently used".into(), vtext: crate::library::fmt_size(self.total), ..Default::default() },
                ]);
                // other apps' clip folders, read again each time (counts must follow clips deleted there)
                let mut imp: Vec<R> = crate::import::lines(std::path::Path::new(&self.s("clips_dir")))
                    .into_iter()
                    .enumerate()
                    .map(|(k, (label, sub, btn))| self.button(&label, &sub, &btn, &format!("import:{k}")))
                    .collect();
                imp.push(self.button(
                    "Import old clips",
                    "Copies clips from Medal, SteelSeries Moments, Outplayed or any folder. Game folders and the original dates are kept, so they sort in by date. Your old app's files aren't touched, and running it again only adds new clips.",
                    "Choose folder…",
                    "import",
                ));
                (
                    "Clips & Storage".into(),
                    "Where clips are saved and how much space they may use.".into(),
                    vec![bx("What's using space", vec![usage]), bx("Storage", st), bx("Bring in clips from other apps", imp)],
                    String::new(),
                )
            }
            "cloud" => {
                // Chat B: real (cloud.rs + Echo's cloud code). Space per account comes from cloud::load_info.
                let accs: Vec<Value> = self.v.get("cloud_accounts").and_then(|x| x.as_array()).cloned().unwrap_or_default();
                let ec = accs.iter().find(|a| a["provider"] == "echocloud");
                let size = |b: u64| crate::library::fmt_size(b);
                let ec_rows = match ec {
                    None => vec![self.button("Sign in with your email", "10 GB free, nothing to set up. You get a 6-digit code by email - no password.", "Sign in", "ecsignin")],
                    // Echo's drawEc: "Signed in · email", "X of Y used" + .ecbar, Upgrade / Manage plan (paid),
                    // Manage space, Sign out, Delete account… - one row; plan cards (#ecplans) right under it
                    Some(a) => {
                        let email = a["label"].as_str().map(|s| s.to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| self.s("ec_email"));
                        let info = crate::cloud::space_of(a["id"].as_u64().unwrap_or(0) as u32);
                        let (sub, num, err) = match &info {
                            Some((_, _, e)) if !e.is_empty() => (e.clone(), -1.0, true),
                            Some((u, t, _)) if *t > 0 => {
                                let p = *u as f32 / *t as f32;
                                (format!("{} of {} used{}", size(*u), size(*t), if p > 0.9 { " - almost full" } else { "" }), p, false)
                            }
                            Some(_) => ("Clips you upload go here".into(), -1.0, false),
                            None => ("Checking space…".into(), -1.0, false),
                        };
                        let paid = matches!(info, Some((_, t, _)) if t as f64 > 10.5e9);
                        let mut rows = vec![R {
                            kind: "ecacct",
                            label: format!("Signed in · {email}"),
                            sub,
                            num,
                            vtext: if err { "err".into() } else { String::new() },
                            opts: {
                                let mut o = vec![
                                    if paid { opt("ecmanage", "Manage plan") } else { opt("ecplans", "Upgrade") },
                                    opt("ecspace", "Manage space"),
                                    opt("ecsignout", "Sign out"),
                                    opth("ecdelete", "Delete account…", "sm"),
                                ];
                                // 10 Nov: paid users see the plans too ("Plans" next to Manage plan)
                                if paid {
                                    o.insert(1, opt("ecplans", "Plans"));
                                }
                                o
                            },
                            ..Default::default()
                        }];
                        if crate::cloud::plans_state() != "" {
                            rows.push(R { kind: "ecplans", ..Default::default() });
                        }
                        rows
                    }
                };
                let names = |p: &str| match p {
                    "filen" => "Filen",
                    "gdrive" => "Google Drive",
                    "mega" => "MEGA",
                    "pcloud" => "pCloud",
                    "webdav" => "WebDAV server",
                    "sftp" => "SFTP server",
                    "s3" => "S3 storage",
                    "folder" => "Folder / NAS",
                    _ => "Cloud",
                };
                let own_accs: Vec<&Value> = accs.iter().filter(|a| a["provider"] != "echocloud").collect();
                // Echo (compared side by side, night 9 Nov): a "Connected to …" line with the total + Sign out of all,
                // then one line per account: service name, its label below, "X of Y" on the right, Remove
                let mut own: Vec<R> = vec![];
                if !own_accs.is_empty() {
                    let mut svc: Vec<&str> = own_accs.iter().map(|a| names(a["provider"].as_str().unwrap_or(""))).collect();
                    svc.dedup();
                    let (mut u, mut t) = (0u64, 0u64);
                    for a in &own_accs {
                        if let Some((au, at, _)) = crate::cloud::space_of(a["id"].as_u64().unwrap_or(0) as u32) {
                            u += au;
                            t += at;
                        }
                    }
                    own.push(self.button(
                        &format!("Connected to {}", svc.join(", ")),
                        &if t > 0 { format!("{} of {} used in total", size(u), size(t)) } else { "Checking space…".into() },
                        "Sign out of all",
                        "cloudoff",
                    ));
                }
                own.extend(own_accs.iter().map(|a| {
                    let label = a["label"].as_str().unwrap_or("");
                    let mut r = self.button(names(a["provider"].as_str().unwrap_or("")), label, "Remove", &format!("cloudrm:{}", a["id"].as_u64().unwrap_or(0)));
                    r.vtext = match crate::cloud::space_of(a["id"].as_u64().unwrap_or(0) as u32) {
                        Some((_, _, e)) if !e.is_empty() => format!("Couldn't check: {e}"),
                        Some((u, t, _)) if t > 0 => format!("{} of {}", size(u), size(t)),
                        Some((u, _, _)) => format!("{} used", size(u)),
                        None => String::new(),
                    };
                    r
                }));
                if own.is_empty() {
                    own.push(self.button(
                        "Not connected",
                        "Add one or more clouds. When one is full, new clips go to the one with the most free space. Filen: 10 GB free. MEGA: 20 GB free. Google Drive: 15 GB free (or your Google One space). Or use your own server / NAS.",
                        "",
                        "",
                    ));
                }
                own.push(R {
                    kind: "buttons",
                    label: "Add a cloud".into(),
                    sub: "You can add several accounts, also two of the same service".into(),
                    opts: vec![opt("cloud:filen", "Filen"), opt("cloud:gdrive", "Google Drive"), opt("cloud:mega", "MEGA"), opt("cloud:pcloud", "pCloud"), opt("cloud:own", "Own server…")],
                    ..Default::default()
                });
                // r2-texts L42: Echo's #megarow - the login fields of the cloud being added (cloud.rs open_form)
                if let Some((t, h)) = crate::cloud::login_info() {
                    own.push(R { kind: "login", label: t, sub: h, ..Default::default() });
                }
                // gaps-settings: Echo's drawCloudLock (the health line points here) - above the cloud list
                if echo_core::recorder::CLOUD_LOCKED.load(std::sync::atomic::Ordering::Relaxed) {
                    let mut lock = self.button(
                        "Echo can't unlock your cloud logins",
                        "The password that protects them on this PC is gone (for example when the Flatpak version of Echo signed in to a cloud too). Uploads, downloads and the cloud list don't work until you connect again. Your clips stay where they are - in the clouds and on this PC.",
                        "Sign out and start over",
                        "cloudlock",
                    );
                    lock.key = "red".into();
                    own.insert(0, lock);
                }
                let mut sync = vec![self.sel(
                    "cloud_upload",
                    "Upload automatically",
                    "Anything else: the cloud button on a clip, or Upload in the player",
                    vec![opt("off", "Nothing"), opt("favs", "Favourites"), opt("game", "Every new clip, when the game closes"), opt("all", "Every new clip, right away")],
                )];
                // Echo's #ctrow (fillCloudTargets): only with 2+ clouds; the options are the connected clouds
                if accs.len() > 1 {
                    let mut o = vec![opt("", if own_accs.len() > 1 { "Your own clouds (the one with the most free space)" } else { "Your own cloud (Echo Cloud only for share links)" })];
                    for a in &accs {
                        if a["provider"] == "echocloud" {
                            o.push(opt("echocloud", "Echo Cloud first (then the others when it's full)"));
                        } else {
                            let label = a["label"].as_str().unwrap_or("");
                            o.push(opt(&a["id"].as_u64().unwrap_or(0).to_string(), &format!("{}{}", names(a["provider"].as_str().unwrap_or("")), if label.is_empty() { String::new() } else { format!(" · {label}") })));
                        }
                    }
                    sync.push(self.sel("cloud_target", "Upload new clips to", "With several clouds. Echo Cloud first gives you share links that play in Discord; when it's full, clips go to your other clouds.", o));
                }
                sync.extend([
                    self.sw("cloud_free_after", "Free up space after uploading", "Automatic uploads are removed from this PC once they're safely in the cloud (they stay in your library and play from the cloud - from Echo Cloud as its smaller copy)"),
                    // the "Original quality in Echo Cloud" switch was removed (10 Nov 2026): Echo Cloud always
                    // gets the smaller copy
                    self.sel(
                        "link_days",
                        "Share links expire",
                        "\"Copy share link\" on a clip in the cloud makes a link anyone can open",
                        days_opts(&[(0, "Never"), (1, "After 1 day"), (7, "After 7 days"), (14, "After 14 days"), (30, "After 30 days")]),
                    ),
                    self.sw(
                        "share_watermark",
                        "Watermark on share links",
                        "The share page shows the Echo logo over the video, and it's on the preview picture in Discord (style under Clips & Storage). The clip itself is uploaded as it is, so links are quick.",
                    ),
                    self.sw("auto_copy_link", "Copy the link after uploading", "When you upload a clip yourself, its share link lands on your clipboard"),
                    self.sw("cloud_download", "Get clips from my other PCs", "New clips uploaded by Echo on another PC (or Linux) download here by themselves. Off: they stay in the cloud and play from there"),
                    self.button("Sync now", "Syncs every 5 minutes and right after you save a clip", "Sync now", "syncnow"),
                ]);
                (
                    "Cloud".into(),
                    "Keep clips in the cloud, watch them from anywhere, free up space on this PC, and get clips from your other PCs (Windows / Linux) automatically.".into(),
                    vec![bx("Echo Cloud", ec_rows), bx("Use your own storage instead", own), bx("Sync", sync)],
                    "\"Remove from PC\" on a clip keeps it only in the cloud: it stays in your library with a cloud badge, plays straight from the cloud, and downloads again when you edit it.".into(),
                )
            }
            "profile" => {
                // gaps-settings: Echo moves its whole Echo Cloud box here (chat 30, h3 "Sign in"): email-code sign-in,
                // usage bar, Upgrade / Manage plan, Manage space, Sign out, Delete account…, plan cards. Same rows as
                // Settings > Cloud (taken from there, so both stay one box). The section keeps the name the developer picked,
                // "Echo Profile".
                let rows: Vec<R> = self.section("cloud").2.into_iter().next().map(|b| b.rows).unwrap_or_default();
                (
                    "Echo Profile".into(),
                    "One free account for everything: your profile page, share links with view counts and 10 GB of Echo Cloud backup. Signing in here also signs you in under Cloud, and the other way round.".into(),
                    vec![bx("Sign in", rows)], // the profile itself: lib_profile.slint (ProfilePanel) under it
                    String::new(),
                )
            }
            "appearance" => {
                let mut rows = vec![R { kind: "themes", ..Default::default() }];
                if self.s("theme") == "custom" {
                    rows.push(R {
                        kind: "colors",
                        label: "Your own colours".into(),
                        sub: "Pick two colours, Echo works out the rest. Dark backgrounds read best. Click a colour to change it; double-click the round preview in the picker = RGB (the colour cycles through the rainbow).".into(),
                        opts: vec![
                            opt("theme_accent", &self.s("theme_accent")),
                            opt("theme_bg", &self.s("theme_bg")),
                            opt("theme_hi", &self.s("theme_hi")),
                        ],
                        // gaps-settings: Echo's themeReset (theme + the three colours cleared)
                        btn: "Reset".into(),
                        act: "themereset".into(),
                        ..Default::default()
                    });
                }
                // Chat B (night): real (lib_fx.rs) - Echo's select + "Still" checkbox, kept per PC in native_ui.json
                let (fx, still) = crate::lib_fx::state();
                rows.push(R {
                    kind: "selectchk",
                    key: "bgfx".into(),
                    key2: "bgfx_still".into(),
                    label: "Background effect".into(),
                    sub: "A light animation behind Echo. It pauses while Echo isn't in front (so it never costs your game anything) and hides in the player and editor.".into(),
                    value: fx,
                    on2: still,
                    opts: vec![opt("", "Off"), opt("snow", "Snow"), opt("stars", "Stars"), opt("fireflies", "Fireflies"), opt("embers", "Embers"), opt("leaves", "Leaves"), opt("rain", "Rain")],
                    ..Default::default()
                });
                // 11 Nov 2026 (1080p vs 1440p): Echo's size on top of the system's scale - uiscale.rs, native_ui.json
                let size = R {
                    kind: "seg",
                    key: "ui_scale".into(),
                    label: "How big Echo looks".into(),
                    sub: crate::uiscale::hint(),
                    value: crate::uiscale::pick(),
                    opts: crate::uiscale::PICKS.iter().map(|(v, l)| opt(v, l)).collect(),
                    col: true,
                    ..Default::default()
                };
                (
                    "Appearance".into(),
                    "Pick a look for Echo - the cards show a tiny preview of the app in each theme. Seasonal themes are at the end.".into(),
                    vec![bx("Theme", rows), bx("Size", vec![size])],
                    String::new(),
                )
            }
            _ => {
                // gaps-settings: Echo's #updstate under the version ("Checking…", "You have the newest version.", …)
                let (upd_sub, upd_btn) = crate::setup::upd_state();
                let about = vec![
                    self.button(&format!("Echo {}", crate::lib_set::app_version()), &upd_sub, &upd_btn, "updates"),
                    // r2-perf: lives in native_ui.json (perf.rs), not settings.json
                    R {
                        kind: "switch",
                        key: "send_perf".into(),
                        label: "Send anonymous performance numbers".into(),
                        sub: "With the update check: how much CPU and memory Echo used, your PC's model names (processor, graphics card, RAM, system) and recording settings. No ID, nothing about you.".into(),
                        on: crate::perf::enabled(),
                        ..Default::default()
                    },
                    self.button("New to Echo?", "A quick walk past the most useful buttons.", "Show me around", "tour"),
                    self.button("Something wrong, or an idea?", "Goes straight to the person who makes Echo. You can attach Echo's log so problems are easier to find.", "Send feedback", "feedback"),
                    R {
                        kind: "button",
                        label: "Echo community".into(),
                        sub: "Help, clips, ideas and news about new versions - on Discord. Or read every change since the first version.".into(),
                        btn: "Changelog".into(),
                        act: "changelog".into(),
                        btn2: "Join the Discord".into(),
                        act2: "discord".into(),
                        ..Default::default()
                    },
                ];
                let os = if cfg!(windows) { "Windows" } else { "your PC" };
                let mut beh = vec![
                    self.sw("start_with_windows", &format!("Start with {}", if cfg!(windows) { "Windows" } else { "your PC" }), "Starts quietly in the tray and begins buffering"),
                    self.sw("start_minimized", "Start minimized", "When Echo starts with your PC it stays in the tray (opening Echo yourself always shows the window)"),
                    self.sw("on_this_day", "\"On this day\"", "Shows a clip from the same date in an earlier year above your library"),
                    self.sw("show_overlay", "In-game popup", "A small \"Clip saved\" notice in the top-right corner (works in borderless / windowed games)"),
                    self.sel(
                        "idle_pause_min",
                        "Pause when idle",
                        "Pause background recording while you're away from the PC; it starts again on your next key or mouse move",
                        days_opts(&[(0, "Never"), (5, "After 5 min"), (10, "After 10 min"), (20, "After 20 min"), (30, "After 30 min")]),
                    ),
                    self.sw("close_to_tray", "Close to tray", "The X button hides Echo; it keeps recording. Quit from the tray icon."),
                    self.sw("discord_presence", "Show in Discord", "Your Discord status shows \"Playing Echo\" and the game you're clipping, with a Get Echo button."),
                ];
                let mut dg = self.sw("discord_game_only", "Only while playing", "Show the Discord status only while Echo sees a game running.");
                dg.dep = !self.b("discord_presence");
                dg.why = if dg.dep { "Show in Discord is off".into() } else { String::new() };
                beh.push(dg);
                beh.push(self.button("Home tips", "Bring back the \"Get started\" line on Home and the Echo Profile box in the sidebar after hiding them.", "Show again", "hometips"));
                let _ = os;
                let snd = |k: &str, label: &str, sub: &str, ev: &str| {
                    let mut r = self.sw(k, label, sub);
                    r.btn = "Preview".into();
                    r.act = format!("sound:{ev}");
                    r
                };
                let files = self.v.get("sound_files").and_then(|x| x.as_object()).cloned().unwrap_or_default();
                let names = [("saved", "Clip saved"), ("failed", "Failed"), ("mark", "Bookmark"), ("shot", "Screenshot"), ("stopped", "Recorder stopped")];
                let sounds = vec![
                    snd("sound_on_save", "Clip saved", "Short chime, also while you're in a fullscreen game", "saved"),
                    snd("sound_on_fail", "Saving failed", "Low buzz if a clip couldn't be saved", "failed"),
                    snd("sound_on_mark", "Bookmark", "Tick when you drop a bookmark or start / stop recording", "mark"),
                    snd("sound_on_shot", "Screenshot", "Camera-style double chime", "shot"),
                    snd("sound_on_stop", "Recorder stopped", "Falling tones if recording stops by itself (Echo then tries to restart it)", "stopped"),
                    // Echo's #sndfiles: a column of "Clip saved: built-in [Choose .wav]" lines on the right
                    // (opts: value = key, label = "Clip saved", hint = file name or "" for built-in)
                    R {
                        kind: "soundfiles",
                        label: "Your own sounds".into(),
                        sub: "Use a .wav file instead of a built-in sound (custom sounds ignore the volume slider)".into(),
                        opts: names
                            .iter()
                            .map(|(k, n)| opth(
                                k,
                                n,
                                &files.get(*k).and_then(|f| f.as_str()).filter(|f| !f.is_empty()).map(|f| f.rsplit(['\\', '/']).next().unwrap_or(f).to_string()).unwrap_or_default(),
                            ))
                            .collect(),
                        ..Default::default()
                    },
                    self.pct_slider("sound_volume", "Volume", "", 1.0, None),
                ];
                let caps = vec![self.sel(
                    "caption_model",
                    "Speech model",
                    "Runs offline on your PC. Downloaded the first time you use Captions in the editor.",
                    vec![opt("base", "Fast (~150 MB)"), opt("small", "Accurate (~470 MB)")],
                )];
                // gaps-settings: Echo's #stres - the results under the row instead of a dialog
                let mut test = self.button("Self-test", "Records about 6 seconds and checks the video and every audio track. Play some sound and say something while it runs.", "Run self-test", "selftest");
                self_test_row(&mut test, false);
                let mut tr = vec![
                    test,
                    self.button(
                        "Send log",
                        "Uploads Echo's log and settings (passwords, keys and email addresses left out) and copies a private link - paste it into Discord for whoever helps you. Links stop working after 14 days.",
                        "Send log",
                        "sendlog",
                    ),
                ];
                if cfg!(windows) {
                    // Echo's #fpsbtn / #fpsres: "Running…" + the progress line, then "✓ result" / "✗ error" under the text
                    let (running, line) = crate::lib_set::fps_state();
                    let desc = "How much does recording cost in your game? Open the game first, click Start, allow the Windows prompt, then switch to the game within 15 s and play normally for about a minute.";
                    tr.push(self.button(
                        "FPS test",
                        &if line.is_empty() { desc.to_string() } else { format!("{desc}\n{line}") },
                        if running { "Running…" } else { "Start" },
                        "fpstest",
                    ));
                }
                tr.push(self.button("Welcome setup", "The quick setup shown on the first start", "Show again", "welcome"));
                tr.push(self.button("Restart recorder", "", "Restart", "restartrec"));
                (
                    "App Settings".into(),
                    format!("How Echo behaves on your PC. File: {}{}", self.file.display(), if self.found { "" } else { " (not found yet - the first change creates it)" }),
                    vec![bx("About", about), bx("Behaviour", beh), bx("Sounds", sounds), bx("Auto captions", caps), bx("Troubleshooting", tr)],
                    String::new(),
                )
            }
        }
    }

    /// what the page shows now: (title, intro, boxes, note). With a search: matching rows of every section.
    /// Chat B: one section's boxes ready for Slint (the setup dialog draws section "setup" with these)
    pub fn boxes_of(&self, sec: &str) -> Vec<PBox> {
        to_boxes(self.section(sec).2)
    }

    pub fn page(&self) -> (String, String, Vec<PBox>, String) {
        let q = self.query.trim().to_lowercase();
        if q.is_empty() {
            let (t, i, b, n) = self.section(&self.sec);
            return (t, i, to_boxes(b), n);
        }
        let mut out = vec![];
        for (id, label, _, _) in NAV {
            let (_, _, boxes, _) = self.section(id);
            for b in boxes {
                let box_hit = b.title.to_lowercase().contains(&q) || label.to_lowercase().contains(&q);
                let rows: Vec<R> = b
                    .rows
                    .into_iter()
                    .filter(|r| {
                        box_hit
                            || r.label.to_lowercase().contains(&q)
                            || r.sub.to_lowercase().contains(&q)
                            || r.opts.iter().any(|o| o.1.to_lowercase().contains(&q))
                    })
                    .collect();
                if !rows.is_empty() {
                    out.push(B { title: format!("{label} · {}", b.title), btn: b.btn, act: b.act, rows });
                }
            }
        }
        let n = out.iter().map(|b| b.rows.len()).sum::<usize>();
        let intro = if n == 0 { format!("Nothing found for \"{}\".", self.query.trim()) } else { format!("{n} setting{} found", if n == 1 { "" } else { "s" }) };
        ("Search".into(), intro, to_boxes(out), String::new())
    }
}

/// a box ready for Slint: title, button, action, rows
pub type PBox = (SharedString, SharedString, SharedString, Vec<SRow>);

fn to_boxes(b: Vec<B>) -> Vec<PBox> {
    b.into_iter()
        .map(|b| {
            (
                b.title.to_uppercase().into(),
                b.btn.into(),
                b.act.into(),
                b.rows
                    .into_iter()
                    .map(|mut r| {
                        // selects show the picked option's text
                        if (r.kind == "select" || r.kind == "selectchk" || r.kind == "hotkey") && r.vtext.is_empty() {
                            r.vtext = r.opts.iter().find(|o| o.0 == r.value).map(|o| o.1.clone()).unwrap_or_else(|| r.value.clone());
                        }
                        r
                    })
                    .map(|r| SRow {
                        kind: r.kind.into(),
                        key: r.key.into(),
                        key2: r.key2.into(),
                        label: r.label.into(),
                        sub: r.sub.into(),
                        on: r.on,
                        on2: r.on2,
                        value: r.value.into(),
                        num: r.num,
                        min: r.min,
                        max: r.max,
                        step: r.step,
                        vtext: r.vtext.into(),
                        opts: ModelRc::new(VecModel::from(r.opts.into_iter().map(|(v, l, s)| SOpt { value: v.into(), label: l.into(), sub: s.into() }).collect::<Vec<_>>())),
                        keys: ModelRc::new(VecModel::from(r.keys.into_iter().map(SharedString::from).collect::<Vec<_>>())),
                        btn: r.btn.into(),
                        act: r.act.into(),
                        btn2: r.btn2.into(),
                        act2: r.act2.into(),
                        dep: r.dep,
                        why: r.why.into(),
                        listen: r.listen,
                        col: r.col,
                        icon: icon_image(&r.icon),
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// Put a page into Slint's model. Same shape as before (same boxes, same row counts) = rows are updated IN PLACE,
/// so a slider being dragged or an open menu isn't rebuilt under the mouse.
pub fn apply(model: &VecModel<SBox>, inner: &mut Vec<std::rc::Rc<VecModel<SRow>>>, page: Vec<PBox>) {
    use slint::Model;
    let same = model.row_count() == page.len()
        && inner.len() == page.len()
        && page.iter().enumerate().all(|(i, p)| model.row_data(i).is_some_and(|b| b.title == p.0) && inner[i].row_count() == p.3.len());
    if same {
        for (i, p) in page.into_iter().enumerate() {
            if let Some(mut b) = model.row_data(i) {
                if b.btn != p.1 || b.act != p.2 {
                    b.btn = p.1;
                    b.act = p.2;
                    model.set_row_data(i, b);
                }
            }
            for (j, r) in p.3.into_iter().enumerate() {
                if inner[i].row_data(j).as_ref() != Some(&r) {
                    inner[i].set_row_data(j, r);
                }
            }
        }
        return;
    }
    inner.clear();
    let boxes: Vec<SBox> = page
        .into_iter()
        .map(|(title, btn, act, rows)| {
            let m = std::rc::Rc::new(VecModel::from(rows));
            inner.push(m.clone());
            SBox { title, btn, act, rows: ModelRc::from(m) }
        })
        .collect();
    model.set_vec(boxes);
}

// ---------- themes (Echo's THEMES + themeVars) ----------

pub struct Palette {
    pub bg: Color,
    pub rail: Color,
    pub panel: Color,
    pub panel2: Color,
    pub line: Color,
    pub hover: Color,
    pub text: Color,
    pub muted: Color,
    pub dim: Color,
    pub accent: Color,
    pub accent2: Color,
    pub hi: Color,
    pub grad: [Color; 3],
    /// 11 Nov 2026: text / icons drawn ON the accent or its gradient (Save clip, Done, switches, chips): near-black
    /// on a light accent, white on a dark one (William: white text on a #ffffff accent was invisible)
    pub on_accent: Color,
    /// the same for things filled with the highlight colour
    pub on_hi: Color,
    /// the highlight used AS text / icon on the panels (selected chips, active buttons): the highlight itself when it
    /// reads well there, else moved toward the text colour until it has 3:1 (William's #613849 on #1a1a1e: 1.8:1)
    pub hi_text: Color,
    pub accent_hex: String,
    pub bg_hex: String,
}

/// WCAG relative luminance of a #rrggbb colour (0 = black, 1 = white)
fn rel_lum(h: &str) -> f32 {
    let (r, g, b) = parse_hex(h).unwrap_or((0.0, 0.0, 0.0));
    let lin = |c: f32| {
        let c = c / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// the text colour for something filled with `fill`: white, unless white on it is below 2.3:1 (WCAG contrast) - then
/// near-black. Plain WCAG ("whichever is higher") would turn Ocean / Ember / Rose... dark too, which read fine in
/// white; this only switches the really light colours (Lime, Winter, Gold, Frost, a white or yellow custom accent).
/// `c` made readable as text on `on`: mixed toward `toward` in 10 % steps until the WCAG contrast is at least 3:1
fn readable(c: &str, on: &str, toward: &str) -> String {
    let contrast = |a: &str, b: &str| {
        let (x, y) = (rel_lum(a), rel_lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    };
    let mut out = c.to_string();
    for i in 1..=10 {
        if contrast(&out, on) >= 3.0 {
            break;
        }
        out = mix(c, toward, i as f32 * 0.1);
    }
    out
}

pub fn on_colour(fill: &str) -> &'static str {
    let white_contrast = 1.05 / (rel_lum(fill) + 0.05);
    if white_contrast < 2.3 {
        "#0d0d10"
    } else {
        "#ffffff"
    }
}

const THEMES: [(&str, &str, &str, &str, [&str; 3], &str); 19] = [
    ("", "Violet", "#7c5cff", "#9d85ff", ["#8b6cff", "#6a4cf5", "#4f7bff"], "#0d0d10"),
    ("ocean", "Ocean", "#2fa8e0", "#7cc9f0", ["#4cc3f0", "#2f8fe0", "#3b5cf0"], "#0b0e12"),
    ("ember", "Ember", "#f07a1a", "#ffa154", ["#ffa154", "#f07a1a", "#e0484a"], "#100d0b"),
    ("emerald", "Emerald", "#2fbf86", "#6fd9ab", ["#6fd9ab", "#2fbf86", "#1d9a9a"], "#0b100e"),
    // 9 Nov: Crimson = his Custom look (accent #e03232 on #0d0d10; the rest derived like Custom does)
    ("crimson", "Crimson", "#e03232", "#e97070", ["#e55151", "#e03232", "#a82626"], "#0d0d10"),
    ("rose", "Rosé", "#e0609e", "#f29ac4", ["#f29ac4", "#e0609e", "#9b5be0"], "#100c0f"),
    ("slate", "Slate", "#8a96a8", "#c3ccd8", ["#c3ccd8", "#8a96a8", "#5d6a7c"], "#0c0d0f"),
    ("frost", "Frost", "#4fb8f0", "#9ad8f8", ["#bfe8ff", "#4fb8f0", "#5a7cf0"], "#0a0e12"),
    ("lime", "Lime", "#8fcf3c", "#b8e57e", ["#c5ef8a", "#8fcf3c", "#2fae6f"], "#0c0f0a"),
    ("gold", "Gold", "#e0a93a", "#f0cb7a", ["#f5d68a", "#e0a93a", "#c06a2a"], "#100e0a"),
    ("midnight", "Midnight", "#6a6ff0", "#a3a6fa", ["#a3a6fa", "#6a6ff0", "#3a3fb8"], "#08080f"),
    ("christmas", "Christmas", "#e0484a", "#f08a8a", ["#f08a8a", "#e0484a", "#2f9e5a"], "#0a100c"),
    ("autumn", "Autumn", "#e07a2a", "#f0a868", ["#f5c27a", "#e07a2a", "#a8402a"], "#110d09"),
    ("halloween", "Halloween", "#f08a1a", "#ffb15a", ["#ffb15a", "#f08a1a", "#8a3ae0"], "#0e0a12"),
    ("winter", "Winter", "#7cc4ec", "#b8e2f8", ["#e2f4ff", "#7cc4ec", "#5a86d8"], "#0a0d12"),
    ("rain", "Rain", "#6f9bd1", "#a3c2e8", ["#c4d8f0", "#6f9bd1", "#4a6a9a"], "#0d1015"),
    ("light", "Light", "#6a4cf5", "#7c5cff", ["#9d85ff", "#6a4cf5", "#4f7bff"], "#f4f4f7"),
    ("paper", "Paper", "#c0602a", "#d9844a", ["#e8a06a", "#c0602a", "#9a3f2a"], "#f6f2ea"),
    ("system", "Match system", "", "", ["", "", ""], ""),
];

fn parse_hex(h: &str) -> Option<(f32, f32, f32)> {
    let h = h.trim().trim_start_matches('#');
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let n = u32::from_str_radix(h, 16).ok()?;
    Some((((n >> 16) & 255) as f32, ((n >> 8) & 255) as f32, (n & 255) as f32))
}
fn hex((r, g, b): (f32, f32, f32)) -> String {
    format!("#{:02x}{:02x}{:02x}", r.round().clamp(0.0, 255.0) as u8, g.round().clamp(0.0, 255.0) as u8, b.round().clamp(0.0, 255.0) as u8)
}
fn mix(a: &str, b: &str, t: f32) -> String {
    let (x, y) = (parse_hex(a).unwrap_or((0.0, 0.0, 0.0)), parse_hex(b).unwrap_or((0.0, 0.0, 0.0)));
    hex((x.0 + (y.0 - x.0) * t, x.1 + (y.1 - x.1) * t, x.2 + (y.2 - x.2) * t))
}
fn lum(h: &str) -> f32 {
    let (r, g, b) = parse_hex(h).unwrap_or((0.0, 0.0, 0.0));
    (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0
}
fn col(h: &str) -> Color {
    let (r, g, b) = parse_hex(h).unwrap_or((0.0, 0.0, 0.0));
    Color::from_rgb_u8(r as u8, g as u8, b as u8)
}

/// Chat B: does the desktop use a light theme? (Windows: AppsUseLightTheme; Linux: GNOME color-scheme / KDE
/// colour scheme name). Asked once per minute at most.
pub fn system_light() -> bool {
    use std::sync::Mutex;
    static C: Mutex<Option<(std::time::Instant, bool)>> = Mutex::new(None);
    let mut g = C.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((t, v)) = *g {
        if t.elapsed().as_secs() < 60 {
            return v;
        }
        // PORT_LEFTOVERS L54: on Linux the check runs gsettings / kreadconfig (processes): after the first answer
        // it runs on a worker - the UI keeps the last answer and the theme follows when the desktop changed
        if cfg!(not(windows)) {
            *g = Some((std::time::Instant::now(), v));
            drop(g);
            std::thread::spawn(move || {
                let nv = ask_light();
                *C.lock().unwrap_or_else(|e| e.into_inner()) = Some((std::time::Instant::now(), nv));
                if nv != v {
                    let _ = slint::invoke_from_event_loop(|| {
                        if let Some(a) = crate::UI.with(|u| u.borrow().as_ref().and_then(|w| w.upgrade())) {
                            crate::apply_theme(&a, &Store::load());
                        }
                    });
                }
            });
            return v;
        }
    }
    // the first answer (start-up, before the window shows) and Windows (one registry read) ask right here
    let v = ask_light();
    *g = Some((std::time::Instant::now(), v));
    v
}
#[cfg(windows)]
fn ask_light() -> bool {
    // RegGetValueW(HKCU, ...\Themes\Personalize, AppsUseLightTheme, RRF_RT_REG_DWORD) ⚠ untested
    unsafe {
        let Ok(adv) = libloading::Library::new("advapi32.dll") else { return false };
        type F = unsafe extern "system" fn(isize, *const u16, *const u16, u32, *mut u32, *mut u32, *mut u32) -> i32;
        let Ok(f) = adv.get::<F>(b"RegGetValueW\0") else { return false };
        let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (k, n) = (w(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"), w("AppsUseLightTheme"));
        let (mut v, mut sz) = (0u32, 4u32);
        f(0x8000_0001u32 as i32 as isize, k.as_ptr(), n.as_ptr(), 0x10, std::ptr::null_mut(), &mut v, &mut sz) == 0 && v == 1
    }
}
#[cfg(not(windows))]
fn ask_light() -> bool {
    let out = |c: &str, a: &[&str]| std::process::Command::new(c).args(a).stderr(std::process::Stdio::null()).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase()).unwrap_or_default();
    // 10 Nov 2026: the desktop portal first - the standard light/dark answer on KDE, GNOME and others, and the only
    // one that works inside the Flatpak (no gsettings / kreadconfig there). color-scheme: 1 = dark, 2 = light,
    // 0 = no preference (-> the checks below). ReadOne is newer; older portals only have Read (same answer).
    for m in ["ReadOne", "Read"] {
        let p = out("dbus-send", &["--session", "--print-reply=literal", "--reply-timeout=1500", "--dest=org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop", &format!("org.freedesktop.portal.Settings.{m}"), "string:org.freedesktop.appearance", "string:color-scheme"]);
        match p.split_whitespace().last() {
            Some("1") => return false,
            Some("2") => return true,
            Some("0") => break,
            _ => {}
        }
    }
    let g = out("gsettings", &["get", "org.gnome.desktop.interface", "color-scheme"]);
    if g.contains("dark") {
        return false;
    }
    if g.contains("light") {
        return true;
    }
    let k = out("kreadconfig6", &["--group", "General", "--key", "ColorScheme"]);
    let k = if k.trim().is_empty() { out("kreadconfig5", &["--group", "General", "--key", "ColorScheme"]) } else { k };
    !k.trim().is_empty() && !k.contains("dark")
}

/// Echo's themeVars(): the whole palette from a theme id (+ custom colours)
pub fn theme_vars(id: &str, accent: &str, bg: &str, hi_c: &str) -> Palette {
    // "Match system" (Chat B): the desktop's light / dark setting -> Light / Violet, like Echo
    let id = if id == "system" { if system_light() { "light" } else { "" } } else { id };
    let t = THEMES.iter().find(|t| t.0 == id && id != "system");
    let (a, a2, g, b) = match t {
        Some(t) if id != "custom" => (t.2.to_string(), t.3.to_string(), [t.4[0].to_string(), t.4[1].to_string(), t.4[2].to_string()], t.5.to_string()),
        _ => {
            let a = if parse_hex(accent).is_some() { accent.to_string() } else { "#7c5cff".into() };
            let mut b = if parse_hex(bg).is_some() { bg.to_string() } else { "#0d0d10".into() };
            for _ in 0..12 {
                if lum(&b) <= 0.16 {
                    break;
                }
                b = mix(&b, "#000000", 0.2);
            }
            (a.clone(), mix(&a, "#ffffff", 0.3), [mix(&a, "#ffffff", 0.15), a.clone(), mix(&a, "#000000", 0.25)], b)
        }
    };
    let hi = if id == "custom" && parse_hex(hi_c).is_some() { hi_c.to_string() } else { "#ffd23f".into() };
    let light = lum(&b) > 0.5;
    let (rail, panel, panel2, line, hover, text, muted, dim) = if light {
        (mix(&b, "#000000", 0.035), "#ffffff".into(), mix(&b, "#000000", 0.045), mix(&b, "#000000", 0.11), mix(&b, "#000000", 0.18), "#16161c", "#55555f", "#86868f")
    } else {
        (mix(&b, "#000000", 0.22), mix(&b, "#ffffff", 0.05), mix(&b, "#ffffff", 0.085), mix(&b, "#ffffff", 0.13), mix(&b, "#ffffff", 0.2), "#f5f5f7", "#a0a0ab", "#6b6b76")
    };
    Palette {
        bg: col(&b),
        rail: col(&rail),
        panel: col(&panel),
        panel2: col(&panel2),
        line: col(&line),
        hover: col(&hover),
        text: col(text),
        muted: col(muted),
        dim: col(dim),
        accent: col(&a),
        accent2: col(&a2),
        hi: col(&hi),
        grad: [col(&g[0]), col(&g[1]), col(&g[2])],
        on_accent: col(on_colour(&a)),
        on_hi: col(on_colour(&hi)),
        hi_text: col(&readable(&hi, &panel, text)),
        accent_hex: a,
        bg_hex: b,
    }
}

/// the theme cards, 4 per line (Echo's .themegrid)
pub fn theme_lines(cur: &str, accent: &str, bg: &str, hi: &str) -> ModelRc<ThemeLine> {
    let mut all: Vec<(&str, &str)> = THEMES.iter().map(|t| (t.0, t.1)).collect();
    all.push(("custom", "Custom"));
    let cards: Vec<ThemeCard> = all
        .iter()
        .map(|(id, name)| {
            let p = theme_vars(id, accent, bg, hi);
            ThemeCard { id: (*id).into(), name: (*name).into(), on: *id == cur, bg: p.bg, rail: p.rail, panel: p.panel, accent: p.accent, text: p.text }
        })
        .collect();
    let lines: Vec<ThemeLine> = cards.chunks(4).map(|c| ThemeLine { items: ModelRc::new(VecModel::from(c.to_vec())) }).collect();
    ModelRc::new(VecModel::from(lines))
}

// ---------- keys ----------

/// Slint key event -> Echo's combo ("Alt+F10", "Shift+KeyC"). None = only a modifier so far (wait for the real key).
pub fn combo(text: &str, ctrl: bool, alt: bool, shift: bool, meta: bool, single: bool) -> Option<String> {
    use slint::platform::Key;
    let k = |key: Key| -> SharedString { key.into() };
    let named: [(Key, &str); 34] = [
        (Key::F1, "F1"), (Key::F2, "F2"), (Key::F3, "F3"), (Key::F4, "F4"), (Key::F5, "F5"), (Key::F6, "F6"), (Key::F7, "F7"), (Key::F8, "F8"),
        (Key::F9, "F9"), (Key::F10, "F10"), (Key::F11, "F11"), (Key::F12, "F12"), (Key::F13, "F13"), (Key::F14, "F14"), (Key::F15, "F15"),
        (Key::Space, "Space"), (Key::Tab, "Tab"), (Key::Return, "Enter"), (Key::Backspace, "Backspace"), (Key::Delete, "Delete"), (Key::Insert, "Insert"),
        (Key::Home, "Home"), (Key::End, "End"), (Key::PageUp, "PageUp"), (Key::PageDown, "PageDown"), (Key::UpArrow, "ArrowUp"), (Key::DownArrow, "ArrowDown"),
        (Key::LeftArrow, "ArrowLeft"), (Key::RightArrow, "ArrowRight"), (Key::CapsLock, "CapsLock"), (Key::Pause, "Pause"), (Key::ScrollLock, "ScrollLock"),
        (Key::SysReq, "PrintScreen"), (Key::Menu, "ContextMenu"),
    ];
    let mods: [(Key, &str); 7] = [
        (Key::Shift, "ShiftLeft"), (Key::ShiftR, "ShiftRight"), (Key::Control, "ControlLeft"), (Key::ControlR, "ControlRight"), (Key::Alt, "AltLeft"), (Key::AltGr, "AltRight"), (Key::Meta, "MetaLeft"),
    ];
    if let Some((_, code)) = mods.iter().find(|(key, _)| k(key.clone()) == text) {
        // push-to-talk takes a modifier alone (Left Alt); other keys wait for the real key
        return if single { Some(code.to_string()) } else { None };
    }
    let code = if let Some((_, c)) = named.iter().find(|(key, _)| k(key.clone()) == text) {
        c.to_string()
    } else {
        let ch = text.chars().next()?;
        // shifted symbols on a US layout -> their key
        let base = match ch {
            '!' => '1', '@' => '2', '#' => '3', '$' => '4', '%' => '5', '^' => '6', '&' => '7', '*' => '8', '(' => '9', ')' => '0',
            '_' => '-', '+' => '=', '{' => '[', '}' => ']', ':' => ';', '"' => '\'', '~' => '`', '|' => '\\', '<' => ',', '>' => '.', '?' => '/',
            c => c,
        };
        match base {
            c if c.is_ascii_alphabetic() => format!("Key{}", c.to_ascii_uppercase()),
            c if c.is_ascii_digit() => format!("Digit{c}"),
            '-' => "Minus".into(),
            '=' => "Equal".into(),
            '[' => "BracketLeft".into(),
            ']' => "BracketRight".into(),
            ';' => "Semicolon".into(),
            '\'' => "Quote".into(),
            '`' => "Backquote".into(),
            '\\' => "Backslash".into(),
            ',' => "Comma".into(),
            '.' => "Period".into(),
            '/' => "Slash".into(),
            _ => return None,
        }
    };
    if single {
        return Some(code);
    }
    let mut parts = vec![];
    if ctrl {
        parts.push("Control".to_string());
    }
    if alt {
        parts.push("Alt".to_string());
    }
    if shift {
        parts.push("Shift".to_string());
    }
    if meta {
        parts.push("Super".to_string());
    }
    parts.push(code);
    Some(parts.join("+"))
}

/// Windows: the monitors (device name, label) - Echo's own list_monitors (gpucap, the list the recorder picks
/// from): "Monitor 1 · 2560×1440 · main · HDR"
#[cfg(windows)]
pub fn monitors() -> Vec<(String, String)> {
    echo_core::gpucap::list_monitors()
        .into_iter()
        .enumerate()
        .map(|(i, m)| (m.name, format!("Monitor {} · {}×{}{}{}", i + 1, m.width, m.height, if m.primary { " · main" } else { "" }, if m.hdr { " · HDR" } else { "" })))
        .collect()
}
#[cfg(not(windows))]
pub fn monitors() -> Vec<(String, String)> {
    vec![]
}

#[cfg(test)]
mod show_key_tests {
    use super::*;

    fn store(name: &str) -> Store {
        let file = std::env::temp_dir().join(format!("echo-showkey-test-{}-{name}.json", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let Value::Object(v) = defaults() else { panic!() };
        Store { file, v, found: false, listening: String::new(), sec: "hotkeys".into(), query: String::new(), games: vec![], usage: vec![], total: 0, monitors: vec![], mics: vec![], mic_test: String::new(), mic_default: String::new() }
    }

    #[test]
    fn show_key_off_by_default() {
        assert_eq!(defaults()["show_key"], json!(""));
        assert_eq!(echo_core::settings::Settings::default().show_key, "");
    }

    #[test]
    fn show_key_set_and_swapped() {
        let mut s = store("swap");
        s.listening = "show".into();
        assert_eq!(s.key("Alt+F9"), None);
        assert_eq!(s.s("show_key"), "Alt+F9");
        // a key another action uses: they swap (the save key gets the old Show / hide key)
        s.listening = "show".into();
        let msg = s.key("Alt+F10").unwrap_or_default();
        assert!(msg.contains("swapped"), "{msg}");
        assert_eq!(s.s("show_key"), "Alt+F10");
        assert_eq!(s.hotkeys()[0]["keys"], json!("Alt+F9"));
        // and the other way: the screenshot key takes the Show / hide key
        s.listening = "shot".into();
        let msg = s.key("Alt+F10").unwrap_or_default();
        assert!(msg.contains("Show / hide Echo"), "{msg}");
        let _ = std::fs::remove_file(&s.file);
    }
}
