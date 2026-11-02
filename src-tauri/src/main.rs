#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod recorder;
mod settings;
mod pad;
mod mediaserve;
#[cfg(target_os = "linux")]
mod linuxutil;
#[cfg(windows)]
mod gpucap;
#[cfg(windows)]
mod hotkey;
#[cfg(target_os = "linux")]
#[path = "hotkey_linux.rs"]
mod hotkey;
#[cfg(windows)]
use winutil as picker;
/// Linux file / folder picker: KDE's kdialog, zenity as a fallback.
#[cfg(target_os = "linux")]
mod picker {
    pub fn pick_path(title: &str, filter: Option<(&str, &str)>, folder: bool) -> Option<String> {
        let home = std::env::var("HOME").unwrap_or_default();
        let pat = filter.map(|(n, p)| format!("{n} ({})", p.replace(';', " ")));
        let mut k = std::process::Command::new("kdialog");
        k.arg("--title").arg(title);
        if folder { k.arg("--getexistingdirectory").arg(&home); } else { k.arg("--getopenfilename").arg(&home); if let Some(p) = &pat { k.arg(p); } }
        let out = match k.output() {
            Ok(o) => o,
            Err(_) => {
                let mut z = std::process::Command::new("zenity");
                z.args(["--file-selection", "--title", title]);
                if folder { z.arg("--directory"); }
                if let Some((n, p)) = filter { z.arg(format!("--file-filter={n} | {}", p.replace(';', " "))); }
                z.output().ok()?
            }
        };
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !s.is_empty()).then_some(s)
    }
}
#[cfg(windows)]
mod winutil;

use recorder::{ClipInfo, Recorder, Thumb};
use settings::Settings;
use std::sync::Mutex;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, RunEvent, State, WindowEvent,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

/// Registered hotkeys and how many seconds each one saves.
struct Hotkeys(Mutex<Vec<(Shortcut, u32)>>);

#[derive(serde::Serialize)]
struct Status {
    recording: bool,
    mode: String,
    buffer_seconds: u32,
    clips_dir: String,
    log: String,
    idle_paused: bool,
    /// "" | "idle" | "game" | "nogame"
    paused: String,
    /// seconds into a long recording (None = not running)
    session: Option<u64>,
    mic_muted: bool,
    /// health check: seconds since the buffer was last written, free disk space, late frames (GPU mode)
    buffer_age: Option<f64>,
    disk_free: u64,
    /// what the cloud sync is doing ("" = idle / off)
    cloud_busy: String,
    frames: u64,
    late: u64,
}

// ----- updates: Tauri updater, asks https://echoclip.link/update/<os>/<arch>/<version> -----
/// installed copy (Echo-Setup.exe on Windows, AppImage on Linux) = can replace itself
fn can_self_update() -> bool {
    #[cfg(windows)]
    { std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("uninstall.exe").exists())).unwrap_or(false) }
    #[cfg(not(windows))]
    { std::env::var_os("APPIMAGE").is_some() }
}

#[derive(serde::Serialize)]
struct UpdateInfo {
    version: String,
    current: String,
    notes: String,
    can_install: bool,
}

#[tauri::command]
async fn update_check(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    use tauri_plugin_updater::UpdaterExt;
    let current = app.package_info().version.to_string();
    if current == "0.1.0" {
        return Ok(None); // own build (build.cmd / build.sh): never offer updates
    }
    let u = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| {
        recorder::log_line(&format!("update: check failed: {e}"));
        "Couldn't check for updates".to_string()
    })?;
    Ok(u.map(|u| UpdateInfo { version: u.version.clone(), current, notes: u.body.clone().unwrap_or_default(), can_install: can_self_update() }))
}

#[tauri::command]
async fn update_install(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    if !can_self_update() {
        return recorder::open_web("https://echoclip.link/#download");
    }
    let u = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?
        .ok_or("Echo is already up to date")?;
    recorder::log_line(&format!("update: downloading {}", u.version));
    let bytes = u.download(|_, _| {}, || {}).await.map_err(|e| {
        recorder::log_line(&format!("update: download failed: {e}"));
        "The update couldn't be downloaded - try again later".to_string()
    })?;
    recorder::log_line(&format!("update: installing {}", u.version));
    app.state::<Recorder>().stop();
    // Windows: starts the installer (it reopens Echo when done) and quits Echo
    u.install(bytes).map_err(|e| {
        recorder::log_line(&format!("update: install failed: {e}"));
        "The update couldn't be installed - try again, or download Echo from echoclip.link".to_string()
    })?;
    app.restart()
}

#[tauri::command]
async fn get_status(rec: State<'_, Recorder>) -> Result<Status, String> {
    Ok({
    let cfg = rec.settings();
    Status {
        recording: rec.is_recording(),
        mode: rec.mode.lock().unwrap().clone(),
        buffer_seconds: cfg.buffer_seconds(),
        clips_dir: cfg.clips_dir.clone(),
        log: recorder::log_path().to_string_lossy().into(),
        idle_paused: rec.idle_paused.load(std::sync::atomic::Ordering::Relaxed),
        paused: rec.pause_reason(),
        session: rec.session_secs(),
        mic_muted: recorder::MIC_MUTED.load(std::sync::atomic::Ordering::Relaxed),
        buffer_age: rec.buffer_age(),
        disk_free: disk_free(&cfg.clips_dir),
        cloud_busy: rec.cloud_busy.lock().unwrap().clone(),
        #[cfg(windows)]
        frames: gpucap::FRAMES.load(std::sync::atomic::Ordering::Relaxed),
        #[cfg(windows)]
        late: gpucap::LATE.load(std::sync::atomic::Ordering::Relaxed),
        #[cfg(not(windows))]
        frames: 0,
        #[cfg(not(windows))]
        late: 0,
    }
    })
}

/// In-game popup ("Clip saved") - a small always-on-top window that never takes focus.
static TOAST_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn toast(app: &AppHandle, title: &str, sub: &str, ok: bool) {
    if !app.state::<Recorder>().settings().show_overlay {
        return;
    }
    #[cfg(target_os = "linux")]
    {
        linuxutil::notify(title, sub, ok);
        return;
    }
    #[allow(unreachable_code)]
    let Some(w) = app.get_webview_window("toast") else { return };
    let js = format!("window.show && show({}, {}, {})",
        serde_json::to_string(title).unwrap_or_default(), serde_json::to_string(sub).unwrap_or_default(), ok);
    let _ = w.eval(&js);
    #[cfg(windows)]
    if let Ok(h) = w.hwnd() {
        let hw = h.0 as isize;
        winutil::overlay_visible(hw, true);
        let gen = TOAST_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(2600));
            if TOAST_GEN.load(std::sync::atomic::Ordering::SeqCst) == gen {
                winutil::overlay_visible(hw, false);
            }
        });
    }
}

fn setup_toast(app: &tauri::App) {
    let w = tauri::WebviewWindowBuilder::new(app, "toast", tauri::WebviewUrl::App("toast.html".into()))
        .title("Echo popup")
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focused(false)
        .shadow(false)
        .visible(false)
        .inner_size(360.0, 84.0)
        .build();
    match w {
        Ok(w) => {
            if let Ok(Some(m)) = w.primary_monitor() {
                let (sz, pos) = (m.size(), m.position());
                let scale = m.scale_factor();
                let x = pos.x + sz.width as i32 - (380.0 * scale) as i32;
                let y = pos.y + (24.0 * scale) as i32;
                let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
            }
            #[cfg(windows)]
            if let Ok(h) = w.hwnd() {
                winutil::make_overlay(h.0 as isize);
            }
        }
        Err(e) => recorder::log_line(&format!("popup window: {e}")),
    }
}

/// bookmark hotkey presses (unix seconds), kept until they fall out of the buffer
static MARKS: std::sync::Mutex<Vec<f64>> = std::sync::Mutex::new(Vec::new());
fn now_secs() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}
const BOOKMARK: u32 = u32::MAX;
const SESSION: u32 = u32::MAX - 1;
const MIC_MUTE: u32 = u32::MAX - 2;

fn do_mic_mute(app: &AppHandle) {
    use std::sync::atomic::Ordering;
    let muted = !recorder::MIC_MUTED.load(Ordering::Relaxed);
    recorder::MIC_MUTED.store(muted, Ordering::Relaxed);
    let cfg = app.state::<Recorder>().settings();
    recorder::play_sound(if muted { recorder::Sound::Mark } else { recorder::Sound::Saved }, &cfg, false);
    toast(app, if muted { "Mic muted" } else { "Mic on" }, if muted { "Your voice isn't recorded" } else { "Your voice is recorded again" }, !muted);
    let _ = app.emit("mic-muted", muted);
}

#[tauri::command]
async fn rename_folder(rec: State<'_, Recorder>, old: String, new: String) -> Result<String, String> {
    rec.rename_folder(&old, &new)
}

/// Free bytes on the drive holding `dir` (0 = unknown).
fn disk_free(dir: &str) -> u64 {
    #[cfg(windows)]
    {
        return winutil::disk_free(dir);
    }
    #[cfg(target_os = "linux")]
    return linuxutil::disk_free(dir);
    #[allow(unreachable_code)]
    {
        let _ = dir;
        0
    }
}

// ----- cloud library -----
/// set when something should sync soon (new clip saved, setting changed)
static SYNC_SOON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

#[tauri::command]
async fn cloud_connect(rec: State<'_, Recorder>, provider: String, user: String, pass: String, code: Option<String>,
                       extra: Option<std::collections::HashMap<String, String>>) -> Result<recorder::CloudInfo, String> {
    let r = rec.cloud_connect(&provider, &user, &pass, code.as_deref().unwrap_or(""), &extra.unwrap_or_default());
    SYNC_SOON.store(true, std::sync::atomic::Ordering::Relaxed);
    r
}
#[tauri::command]
fn echocloud_ready(rec: State<'_, Recorder>) -> bool {
    rec.echocloud_ready()
}
#[tauri::command]
async fn echocloud_plans(rec: State<'_, Recorder>) -> Result<serde_json::Value, String> {
    rec.echocloud_plans()
}
#[tauri::command]
async fn echocloud_checkout(rec: State<'_, Recorder>, product: String) -> Result<(), String> {
    rec.echocloud_checkout(&product)
}
#[tauri::command]
async fn echocloud_manage(rec: State<'_, Recorder>) -> Result<(), String> {
    rec.echocloud_manage()
}
#[tauri::command]
async fn echocloud_me(rec: State<'_, Recorder>) -> Result<serde_json::Value, String> {
    rec.echocloud_me()
}
#[tauri::command]
async fn echocloud_start(rec: State<'_, Recorder>, email: String) -> Result<(), String> {
    rec.echocloud_start(&email)
}

#[tauri::command]
async fn cloud_remove_account(rec: State<'_, Recorder>, id: u32) -> Result<(), String> {
    rec.cloud_remove_account(id)
}
#[tauri::command]
async fn cloud_disconnect(rec: State<'_, Recorder>) -> Result<(), String> {
    rec.cloud_disconnect();
    Ok(())
}
#[tauri::command]
async fn cloud_info(rec: State<'_, Recorder>) -> Result<recorder::CloudInfo, String> {
    Ok(rec.cloud_info())
}
#[tauri::command]
async fn cloud_files(rec: State<'_, Recorder>) -> Result<Vec<recorder::CloudFile>, String> {
    Ok({
    rec.cloud_files()
    })
}
#[tauri::command]
fn cloud_sync_now() {
    SYNC_SOON.store(true, std::sync::atomic::Ordering::Relaxed);
}
#[tauri::command]
async fn cloud_upload(rec: State<'_, Recorder>, path: String) -> Result<(), String> {
    rec.cloud_upload(&path)
}
#[tauri::command]
async fn cloud_download(rec: State<'_, Recorder>, rel: String) -> Result<String, String> {
    rec.cloud_download(&rel)
}
#[tauri::command]
fn hotkey_mode() -> String {
    #[cfg(target_os = "linux")]
    return if hotkey::using_portal() { "portal".into() } else { "evdev".into() };
    #[allow(unreachable_code)]
    "hook".into()
}
#[tauri::command]
fn hotkeys_paused(on: bool) {
    #[cfg(any(windows, target_os = "linux"))]
    hotkey::pause(on);
    #[cfg(not(any(windows, target_os = "linux")))]
    let _ = on;
}
#[tauri::command]
fn cloud_up_progress() -> recorder::UpProgress {
    recorder::UP_PROGRESS.lock().unwrap().clone()
}
#[tauri::command]
fn export_progress() -> u32 {
    recorder::EXPORT_PROGRESS.load(std::sync::atomic::Ordering::Relaxed)
}
#[tauri::command]
async fn list_folders(rec: State<'_, Recorder>) -> Result<Vec<String>, String> {
    Ok(rec.list_folders())
}
#[tauri::command]
async fn create_folder(rec: State<'_, Recorder>, name: String) -> Result<String, String> {
    rec.create_folder(&name)
}
#[tauri::command]
async fn delete_folder(rec: State<'_, Recorder>, name: String) -> Result<(), String> {
    rec.delete_folder(&name)
}
#[tauri::command]
async fn save_project(rec: State<'_, Recorder>, path: String, data: String) -> Result<(), String> {
    rec.save_project(&path, &data)
}
#[tauri::command]
async fn load_project(rec: State<'_, Recorder>, path: String) -> Result<Option<String>, String> {
    Ok(rec.load_project(&path))
}
#[tauri::command]
async fn probe_media(rec: State<'_, Recorder>, path: String) -> Result<(u32, u32, bool, f64), String> {
    Ok(rec.probe_media(&path))
}
#[tauri::command]
fn cloud_dl_progress(rel: String) -> (u64, u64) {
    Recorder::cloud_dl_progress(&rel)
}
#[tauri::command]
async fn cloud_remove_local(rec: State<'_, Recorder>, path: String) -> Result<(), String> {
    rec.cloud_remove_local(&path)
}
#[tauri::command]
async fn cloud_delete(rec: State<'_, Recorder>, rel: String) -> Result<(), String> {
    rec.cloud_delete(&rel)
}
#[tauri::command]
fn cloud_forget_local(rec: State<Recorder>, path: String) {
    rec.cloud_forget_local(&path)
}
#[tauri::command]
async fn cloud_stream_base(rec: State<'_, Recorder>) -> Result<String, String> {
    rec.cloud_stream_base()
}
#[tauri::command]
async fn cloud_thumb(rec: State<'_, Recorder>, rel: String) -> Result<recorder::Thumb, String> {
    rec.cloud_thumb(&rel)
}

#[tauri::command]
async fn compress_clip(rec: State<'_, Recorder>, path: String, factor: f64) -> Result<(u64, u64), String> {
    rec.compress_clip(&path, factor)
}

#[tauri::command]
async fn cloud_link(rec: State<'_, Recorder>, rel: String) -> Result<String, String> {
    let url = rec.cloud_link(&rel)?;
    recorder::copy_text(&url)?;
    Ok(url)
}

#[tauri::command]
async fn self_test(rec: State<'_, Recorder>) -> Result<Vec<(String, bool, String)>, String> {
    Ok(rec.self_test())
}

#[derive(serde::Serialize)]
struct Monitor {
    name: String,
    label: String,
    hdr: bool,
}

/// Monitors for Settings > Video.
#[tauri::command]
fn list_monitors() -> Vec<Monitor> {
    #[cfg(windows)]
    {
        return gpucap::list_monitors().into_iter().enumerate().map(|(i, m)| Monitor {
            label: format!("Monitor {} · {}×{}{}{}", i + 1, m.width, m.height, if m.primary { " · main" } else { "" }, if m.hdr { " · HDR" } else { "" }),
            name: m.name,
            hdr: m.hdr,
        }).collect();
    }
    #[allow(unreachable_code)]
    Vec::new()
}

#[tauri::command]
async fn clip_meta(rec: State<'_, Recorder>) -> Result<std::collections::BTreeMap<String, recorder::ClipMeta>, String> {
    Ok({
    rec.clip_meta()
    })
}

#[tauri::command]
fn set_meta(rec: State<Recorder>, name: String, tags: Vec<String>, note: String) -> Result<(), String> {
    rec.set_meta(&name, tags, note)
}

#[tauri::command]
async fn set_thumb(rec: State<'_, Recorder>, path: String, t: f64) -> Result<String, String> {
    rec.set_thumb(&path, t)
}

#[tauri::command]
async fn cleanup_now(rec: State<'_, Recorder>) -> Result<u32, String> {
    let days = rec.settings().cleanup_days;
    Ok(rec.cleanup_old(days))
}

fn stickers_dir() -> std::path::PathBuf {
    std::env::var("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(|_| std::env::temp_dir()).join("Clipper").join("stickers")
}

/// Emoji sticker drawn by the editor (PNG data URL) -> file in %APPDATA%\Clipper\stickers.
#[tauri::command]
fn save_sticker(data: String) -> Result<String, String> {
    let b64 = data.split(',').nth(1).ok_or("bad image")?;
    let bytes = recorder::unb64(b64).ok_or("bad image")?;
    let mut h: u64 = 0xcbf29ce484222325;
    for b in &bytes {
        h = (h ^ *b as u64).wrapping_mul(0x100000001b3);
    }
    let dir = stickers_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let p = dir.join(format!("emoji_{h:016x}.png"));
    std::fs::write(&p, bytes).map_err(|e| e.to_string())?;
    Ok(p.to_string_lossy().into())
}

/// Pick a picture for a sticker; it's copied next to the emoji stickers so edits keep working.
#[tauri::command]
async fn pick_image() -> Result<String, String> {
    #[cfg(any(windows, target_os = "linux"))]
    {
        let src = picker::pick_path("Pick a picture", Some(("Pictures", "*.png;*.jpg;*.jpeg;*.webp;*.gif;*.bmp")), false).unwrap_or_default();
        if src.is_empty() {
            return Ok(String::new());
        }
        let srcp = std::path::Path::new(&src);
        let dir = stickers_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let name = srcp.file_name().ok_or("bad file")?.to_string_lossy().to_string();
        let mut dst = dir.join(&name);
        let mut n = 2;
        while dst.exists() && std::fs::metadata(&dst).map(|m| m.len()).ok() != std::fs::metadata(srcp).map(|m| m.len()).ok() {
            dst = dir.join(format!("{n}_{name}"));
            n += 1;
        }
        std::fs::copy(srcp, &dst).map_err(|e| e.to_string())?;
        return Ok(dst.to_string_lossy().into());
    }
    #[allow(unreachable_code)]
    Ok(String::new())
}

#[tauri::command]
fn toggle_mic(app: AppHandle) -> bool {
    do_mic_mute(&app);
    recorder::MIC_MUTED.load(std::sync::atomic::Ordering::Relaxed)
}

fn do_bookmark(app: &AppHandle) {
    let rec = app.state::<Recorder>();
    let cfg = rec.settings();
    let now = now_secs();
    let keep = cfg.buffer_seconds() as f64 + 30.0;
    let mut m = MARKS.lock().unwrap();
    m.retain(|t| now - *t < keep);
    m.push(now);
    let n = m.len();
    drop(m);
    recorder::play_sound(recorder::Sound::Mark, &cfg, false);
    let _ = &cfg;
    toast(app, "Bookmarked", &format!("{n} in the buffer · shows on the next saved clip"), true);
}

/// Save a clip, tell the UI, and play the saved / failed sound.
fn do_save(app: &AppHandle, seconds: Option<u32>) -> Result<String, String> {
    let pressed = now_secs();
    let rec = app.state::<Recorder>();
    let cfg = rec.settings();
    let paused = rec.pause_reason();
    if !paused.is_empty() {
        let why = match paused.as_str() { "idle" => "you were away", "game" => "this game is set to not record", _ => "no game is running" };
        recorder::play_sound(recorder::Sound::Failed, &cfg, false);
        toast(app, "Not recording", &format!("Paused - {why}"), false);
        let e: Result<String, String> = Err(format!("Recording is paused ({why})"));
        let _ = app.emit("clip-saved", &e);
        return e;
    }
    // chime + popup right away on the keypress (saving long clips takes a moment); buzz later only if it fails
    recorder::play_sound(recorder::Sound::Saved, &cfg, false);
    let secs = seconds.unwrap_or_else(|| cfg.hotkeys.first().map(|h| h.seconds).unwrap_or(30));
    toast(app, "Saving clip…", &format!("Last {}", if secs >= 60 { format!("{} min", secs / 60) } else { format!("{secs} s") }), true);
    let _ = app.emit("clip-saving", secs);
    let r = rec.save(seconds);
    match &r {
        Ok(p) => {
            // bookmarks that fall inside this clip -> seconds from its start
            let dur = rec.thumb(p).map(|t| t.duration).unwrap_or(secs as f64);
            let start = pressed - dur;
            let marks: Vec<f64> = MARKS.lock().unwrap().iter().filter(|t| **t >= start && **t <= pressed).map(|t| ((t - start) * 100.0).round() / 100.0).collect();
            if !marks.is_empty() {
                rec.write_marks(p, &marks);
            }
            let name = std::path::Path::new(p).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            toast(app, "Clip saved", &name, true);
            SYNC_SOON.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Err(e) => {
            recorder::play_sound(recorder::Sound::Failed, &cfg, false);
            toast(app, "Couldn't save clip", e, false);
        }
    }
    let _ = app.emit("clip-saved", &r);
    r
}

fn do_screenshot(app: &AppHandle) -> Result<String, String> {
    let rec = app.state::<Recorder>();
    let cfg = rec.settings();
    recorder::play_sound(recorder::Sound::Shot, &cfg, false);
    let r = rec.screenshot();
    match &r {
        Ok(_) => toast(app, "Screenshot saved", "Clips folder › Screenshots", true),
        Err(e) => {
            recorder::play_sound(recorder::Sound::Failed, &cfg, false);
            toast(app, "Screenshot failed", e, false);
        }
    }
    let _ = app.emit("screenshot-saved", &r);
    r
}

#[tauri::command]
async fn take_screenshot(app: AppHandle) -> Result<String, String> {
    do_screenshot(&app)
}

#[tauri::command]
async fn get_favorites(rec: State<'_, Recorder>) -> Result<Vec<String>, String> {
    Ok({
    rec.favorites()
    })
}

#[tauri::command]
fn set_favorite(rec: State<Recorder>, name: String, on: bool) -> Result<(), String> {
    if on {
        SYNC_SOON.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    rec.set_favorite(&name, on)
}

#[tauri::command]
async fn save_clip(app: AppHandle, seconds: Option<u32>) -> Result<String, String> {
    do_save(&app, seconds)
}

/// Settings "preview" button.
#[tauri::command]
fn test_sound(rec: State<Recorder>, kind: String) {
    let k = match kind.as_str() {
        "fail" | "failed" => recorder::Sound::Failed,
        "mark" => recorder::Sound::Mark,
        "shot" => recorder::Sound::Shot,
        "stopped" => recorder::Sound::Stopped,
        _ => recorder::Sound::Saved,
    };
    recorder::play_sound(k, &rec.settings(), true);
}

#[tauri::command]
async fn get_marks(rec: State<'_, Recorder>, path: String) -> Result<Vec<f64>, String> {
    Ok({
    rec.marks(&path)
    })
}

#[tauri::command]
async fn list_shots(rec: State<'_, Recorder>) -> Result<Vec<ClipInfo>, String> {
    Ok({
    rec.list_shots()
    })
}

#[tauri::command]
async fn list_clips(rec: State<'_, Recorder>) -> Result<Vec<ClipInfo>, String> {
    Ok({
    rec.list_clips()
    })
}

#[tauri::command]
async fn thumb(rec: State<'_, Recorder>, path: String) -> Result<Thumb, String> {
    rec.thumb(&path)
}

#[tauri::command]
async fn delete_clip(rec: State<'_, Recorder>, path: String) -> Result<(), String> {
    rec.delete_clip(&path)
}

/// Start or stop the long (full-session) recording. Returns the saved clip when it stops.
fn do_session(app: &AppHandle) -> Result<Option<String>, String> {
    let rec = app.state::<Recorder>();
    let cfg = rec.settings();
    let r = if rec.session_secs().is_some() {
        toast(app, "Saving recording…", "One moment", true);
        let r = rec.stop_session().map(Some);
        match &r {
            Ok(Some(p)) => toast(app, "Recording saved", &std::path::Path::new(p).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(), true),
            Err(e) => toast(app, "Recording failed", e, false),
            _ => {}
        }
        let _ = app.emit("clip-saved", &r.clone().map(|o| o.unwrap_or_default()));
        r
    } else {
        let r = rec.start_session().map(|_| None);
        match &r {
            Ok(_) => {
                recorder::play_sound(recorder::Sound::Mark, &cfg, false);
                toast(app, "Recording", "Press the key again (or Stop in Echo) to save it", true);
            }
            Err(e) => toast(app, "Couldn't start recording", e, false),
        }
        r
    };
    if r.is_err() {
        recorder::play_sound(recorder::Sound::Failed, &cfg, false);
    }
    r
}

#[tauri::command]
async fn toggle_session(app: AppHandle) -> Result<Option<String>, String> {
    do_session(&app)
}

#[tauri::command]
async fn list_games(rec: State<'_, Recorder>) -> Result<Vec<recorder::GameInfo>, String> {
    Ok({
    rec.list_games()
    })
}

#[tauri::command]
async fn share_clip(rec: State<'_, Recorder>, path: String) -> Result<String, String> {
    let url = rec.share_clip(&path)?;
    let _ = recorder::copy_text(&url);
    Ok(url)
}

#[tauri::command]
async fn shared_clips(rec: State<'_, Recorder>) -> Result<Vec<String>, String> {
    Ok({
    rec.shared_clips()
    })
}

/// Windows file picker for a custom .wav ("" = cancelled).
#[tauri::command]
async fn pick_wav() -> String {
    #[cfg(any(windows, target_os = "linux"))]
    {
        return picker::pick_path("Pick a sound", Some(("Sounds (.wav)", "*.wav")), false).unwrap_or_default();
    }
    #[allow(unreachable_code)]
    String::new()
}

#[tauri::command]
async fn move_clip(rec: State<'_, Recorder>, path: String, folder: String) -> Result<String, String> {
    rec.move_clip(&path, &folder)
}

#[tauri::command]
async fn sort_into_folders(rec: State<'_, Recorder>) -> Result<u32, String> {
    Ok(rec.sort_into_folders())
}

#[tauri::command]
async fn rename_clip(rec: State<'_, Recorder>, path: String, name: String) -> Result<String, String> {
    rec.rename_clip(&path, &name)
}

#[tauri::command]
async fn trim_clip(rec: State<'_, Recorder>, path: String, start: f64, end: f64, exact: bool, replace: bool) -> Result<String, String> {
    rec.trim_clip(&path, start, end, exact, replace)
}

#[tauri::command]
async fn import_clips(rec: State<'_, Recorder>, paths: Vec<String>) -> Result<(Vec<String>, Vec<String>), String> {
    Ok(rec.import_clips(&paths))
}

/// the watchdog leaves the recorder alone while the FPS test pauses it
static FPS_TESTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Log + settings as a .txt on the clipboard (paste it into Discord to send it).
#[tauri::command]
async fn send_log(rec: State<'_, Recorder>) -> Result<String, String> {
    let f = rec.support_file()?;
    recorder::copy_to_clipboard(&f)?;
    Ok(f)
}

/// Waits for the user to press a controller combo; returns (bits, names).
#[tauri::command]
async fn pad_capture() -> Result<(u16, String), String> {
    let b = pad::capture();
    Ok((b, pad::names(b)))
}

#[tauri::command]
fn pad_names(bits: u16) -> String {
    pad::names(bits)
}

/// FPS test: PresentMon measures the game ~20 s with the recorder paused, then ~20 s recording.
/// Progress goes out as "fps-test" events (text).
#[tauri::command]
async fn fps_test(app: AppHandle, rec: State<'_, Recorder>) -> Result<String, String> {
    use std::time::{Duration, Instant};
    struct Busy;
    impl Drop for Busy {
        fn drop(&mut self) { FPS_TESTING.store(false, std::sync::atomic::Ordering::Relaxed); }
    }
    if FPS_TESTING.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return Err("The FPS test is already running".into());
    }
    let _busy = Busy;
    let say = |t: &str| { let _ = app.emit("fps-test", t.to_string()); };
    say("Getting the measuring tool (first time only)…");
    let exe = recorder::presentmon_exe()?;
    let csv = std::env::temp_dir().join("echo_fps.csv");
    let _ = std::fs::remove_file(&csv);
    say("Allow the Windows prompt (the measuring tool needs admin rights)…");
    let args = format!("--timed 68 --terminate_after_timed --no_console_stats --v1_metrics --output_file \"{}\"", csv.display());
    #[cfg(windows)]
    if winutil::run_elevated(&exe.to_string_lossy(), &args, false).is_none() {
        return Err("The measuring tool needs the Windows permission prompt to be allowed".into());
    }
    // the capture has started once its file appears
    let wait = Instant::now();
    while !csv.exists() {
        if wait.elapsed() > Duration::from_secs(40) {
            return Err("The measuring tool didn't start (was the Windows prompt declined?)".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let t0 = Instant::now();
    let until = |s: u64, msg: &str| {
        while t0.elapsed() < Duration::from_secs(s) {
            let left = s.saturating_sub(t0.elapsed().as_secs());
            let _ = app.emit("fps-test", format!("{msg} ({left} s)"));
            std::thread::sleep(Duration::from_millis(500));
        }
    };
    until(15, "Switch to your game now and play normally");
    rec.stop();
    until(38, "Measuring WITHOUT Echo recording");
    say("Starting Echo's recorder again…");
    let _ = rec.start();
    until(70, "Measuring WITH Echo recording");
    std::thread::sleep(Duration::from_secs(2));
    let off = recorder::fps_stats(&csv, 18.0, 38.0);
    let on = recorder::fps_stats(&csv, 45.0, 65.0);
    let (Some((game, a0, l0, _)), Some((_, a1, l1, _))) = (off, on) else {
        return Err("Not enough frames measured - was a game in front the whole time?".into());
    };
    let pct = (a1 - a0) / a0 * 100.0;
    let r = format!("{game}: without Echo {a0:.0} FPS (1% low {l0:.0}) · with Echo {a1:.0} FPS (1% low {l1:.0}) · {pct:+.1}%");
    recorder::log_line(&format!("fps: {r}"));
    Ok(r)
}

#[tauri::command]
async fn forget_game(rec: State<'_, Recorder>, name: String, never: bool) -> Result<(), String> {
    rec.forget_game(&name, never);
    Ok(())
}

/// Medal / SteelSeries Moments / Outplayed folders found on this PC.
#[tauri::command]
async fn import_sources(rec: State<'_, Recorder>) -> Result<Vec<recorder::ImportSource>, String> {
    Ok(rec.import_sources())
}

/// Imports a whole folder (game subfolders kept, original dates kept); emits "import-progress" (done, total).
#[tauri::command]
async fn import_folder(app: AppHandle, rec: State<'_, Recorder>, path: String) -> Result<(usize, usize, Vec<String>), String> {
    let (ok, skipped, bad) = rec.import_folder(&path, |d, t, _| {
        let _ = app.emit("import-progress", (d, t));
    });
    Ok((ok.len(), skipped, bad))
}

#[tauri::command]
async fn pick_media() -> String {
    #[cfg(any(windows, target_os = "linux"))]
    {
        return picker::pick_path("Add a video or sound to the timeline",
            Some(("Videos and sounds", "*.mp4;*.mov;*.mkv;*.webm;*.avi;*.m4v;*.wav;*.mp3;*.ogg;*.flac;*.m4a;*.aac;*.opus")), false).unwrap_or_default();
    }
    #[allow(unreachable_code)]
    String::new()
}

#[tauri::command]
async fn pick_folder() -> String {
    #[cfg(any(windows, target_os = "linux"))]
    {
        return picker::pick_path("Folder with your old clips", None, true).unwrap_or_default();
    }
    #[allow(unreachable_code)]
    String::new()
}

#[tauri::command]
async fn frame_png(rec: State<'_, Recorder>, path: String, t: f64) -> Result<String, String> {
    let p = rec.frame_png(&path, t)?;
    recorder::copy_to_clipboard(&p)?;
    Ok(p)
}

#[tauri::command]
async fn audio_mp3(rec: State<'_, Recorder>, path: String) -> Result<String, String> {
    let p = rec.audio_mp3(&path)?;
    recorder::copy_to_clipboard(&p)?;
    Ok(p)
}

/// The clip's sound as .wav (sound effects) - saved in Audio and copied.
#[tauri::command]
async fn audio_wav(rec: State<'_, Recorder>, path: String) -> Result<String, String> {
    let p = rec.audio_file(&path, true)?;
    recorder::copy_to_clipboard(&p)?;
    Ok(p)
}

#[tauri::command]
async fn combine_clips(rec: State<'_, Recorder>, paths: Vec<String>) -> Result<String, String> {
    rec.combine_clips(&paths)
}

#[tauri::command]
async fn make_gif(rec: State<'_, Recorder>, path: String, start: f64, end: f64) -> Result<String, String> {
    let p = rec.make_gif(&path, start, end)?;
    recorder::copy_to_clipboard(&p)?;
    Ok(p)
}

#[tauri::command]
async fn filmstrip(rec: State<'_, Recorder>, path: String) -> Result<String, String> {
    rec.filmstrip(&path)
}

#[tauri::command]
async fn render_edit(rec: State<'_, Recorder>, path: String, edit: recorder::Edit) -> Result<String, String> {
    rec.render_edit(&path, &edit)
}

#[tauri::command]
async fn fix_dark_clips(rec: State<'_, Recorder>) -> Result<u32, String> {
    rec.fix_dark_clips()
}

#[tauri::command]
async fn audio_tracks(rec: State<'_, Recorder>, path: String) -> Result<u32, String> {
    Ok(rec.audio_tracks(&path))
}

#[tauri::command]
async fn track_levels(rec: State<'_, Recorder>, path: String) -> Result<Vec<f64>, String> {
    Ok(rec.track_levels(&path))
}

#[tauri::command]
async fn extract_track(rec: State<'_, Recorder>, path: String, track: u32) -> Result<String, String> {
    rec.extract_track(&path, track)
}

#[derive(serde::Serialize)]
struct Mics {
    names: Vec<String>,
    default: String,
    labels: Vec<String>,
}

#[tauri::command]
fn media_base() -> String {
    if cfg!(target_os = "linux") { mediaserve::base() } else { String::new() }
}

#[tauri::command]
async fn list_mics() -> Mics {
    let (names, default) = recorder::list_mics();
    let labels = names.iter().map(|n| recorder::mic_label(n)).collect();
    Mics { names, default, labels }
}

#[tauri::command]
async fn transcribe(rec: State<'_, Recorder>, path: String) -> Result<Vec<recorder::Word>, String> {
    let model = rec.settings().caption_model;
    rec.transcribe(&path, &model)
}

#[tauri::command]
fn captions_ready(rec: State<Recorder>) -> bool {
    recorder::captions_ready(&rec.settings().caption_model)
}

#[tauri::command]
async fn copy_clip(path: String) -> Result<(), String> {
    recorder::copy_to_clipboard(&path)
}

/// Copy a clip shrunk to fit `mb` megabytes (0 = original). Returns (copied file, its size in bytes).
#[tauri::command]
async fn copy_clip_sized(app: AppHandle, rec: State<'_, Recorder>, path: String, mb: f64) -> Result<(String, u64), String> {
    let p = if mb > 0.0 {
        let last = std::sync::Mutex::new(-1i32);
        rec.shrink_clip(&path, mb, &|f: f64| {
            let pct = (f * 100.0) as i32;
            let mut l = last.lock().unwrap();
            if pct != *l { *l = pct; let _ = app.emit("copy-progress", (path.clone(), f)); }
        })?
    } else { path };
    recorder::copy_to_clipboard(&p)?;
    let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
    Ok((p, size))
}

#[tauri::command]
async fn restart_recorder(rec: State<'_, Recorder>) -> Result<(), String> {
    rec.stop();
    rec.start()
}

#[tauri::command]
fn get_settings(rec: State<Recorder>) -> Settings {
    rec.settings()
}

/// Save settings, re-register hotkeys, and restart the recorder only if needed.
#[tauri::command]
async fn set_settings(app: AppHandle, rec: State<'_, Recorder>, settings: Settings) -> Result<bool, String> {
    // fields only the Rust side changes (the UI's copy may be stale)
    let mut settings = settings;
    let cur = rec.settings();
    settings.cloud = cur.cloud;
    settings.cloud_accounts = cur.cloud_accounts;
    settings.cloud_since = cur.cloud_since;
    settings.folder_alias = cur.folder_alias;
    settings.not_games = cur.not_games;
    settings.save()?;
    SYNC_SOON.store(true, std::sync::atomic::Ordering::Relaxed);
    allow_clips_dir(&app, &settings.clips_dir);
    set_autostart(settings.start_with_windows);
    register_hotkeys(&app, &settings);
    pad::set_combo(settings.pad_buttons);
    let restart = rec.apply(settings);
    if restart {
        let h = app.clone();
        std::thread::spawn(move || {
            let rec = h.state::<Recorder>();
            rec.stop();
            if let Err(e) = rec.start() {
                recorder::log_line(&e);
            }
        });
    }
    Ok(restart)
}

#[tauri::command]
fn open_in_folder(path: String) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let p = std::path::Path::new(&path);
        let arg = if p.is_dir() { format!("\"{}\"", path) } else { format!("/select,\"{}\"", path) };
        let _ = std::process::Command::new("explorer").raw_arg(arg).spawn();
    }
    #[cfg(not(windows))]
    {
        // open the folder with the clip selected (Dolphin etc.), else just the folder
        let p = std::path::Path::new(&path);
        let uri = format!("file://{}", p.to_string_lossy().replace('%', "%25").replace(' ', "%20"));
        let ok = !p.is_dir() && std::process::Command::new("dbus-send")
            .args(["--session", "--dest=org.freedesktop.FileManager1", "--type=method_call", "/org/freedesktop/FileManager1",
                   "org.freedesktop.FileManager1.ShowItems", &format!("array:string:{uri}"), "string:"])
            .status().map_or(false, |s| s.success());
        let dir = if p.is_dir() { p.to_path_buf() } else { p.parent().map(|p| p.to_path_buf()).unwrap_or_default() };
        if !ok { let _ = std::process::Command::new("xdg-open").arg(dir).spawn(); }
    }
}

/// Let the webview load videos/previews from the clips folder (also when the user changes it).
fn allow_clips_dir(app: &AppHandle, dir: &str) {
    let _ = app.asset_protocol_scope().allow_directory(dir, true);
}

fn register_hotkeys(app: &AppHandle, cfg: &Settings) {
    recorder::PTT_ON.store(cfg.push_to_talk && !cfg.ptt_key.is_empty(), std::sync::atomic::Ordering::Relaxed);
    #[cfg(any(windows, target_os = "linux"))]
    {
        let mut list: Vec<(String, u32)> = cfg.hotkeys.iter().map(|h| (h.keys.clone(), h.seconds)).collect();
        if !cfg.screenshot_key.is_empty() {
            list.push((cfg.screenshot_key.clone(), 0)); // 0 seconds = screenshot
        }
        if !cfg.bookmark_key.is_empty() {
            list.push((cfg.bookmark_key.clone(), BOOKMARK));
        }
        if !cfg.session_key.is_empty() {
            list.push((cfg.session_key.clone(), SESSION));
        }
        if !cfg.mic_mute_key.is_empty() {
            list.push((cfg.mic_mute_key.clone(), MIC_MUTE));
        }
        hotkey::set_ptt(if cfg.push_to_talk { cfg.ptt_key.as_str() } else { "" });
        for bad in hotkey::set(&list) {
            recorder::log_line(&format!("hotkey {bad} invalid"));
        }
        let _ = app;
        return;
    }
    #[allow(unreachable_code)]
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let mut list = Vec::new();
    for h in &cfg.hotkeys {
        match h.keys.parse::<Shortcut>() {
            Ok(sc) => match gs.register(sc) {
                Ok(()) => list.push((sc, h.seconds)),
                Err(e) => recorder::log_line(&format!("hotkey {} unavailable: {e}", h.keys)),
            },
            Err(e) => recorder::log_line(&format!("hotkey {} invalid: {e}", h.keys)),
        }
    }
    *app.state::<Hotkeys>().0.lock().unwrap() = list;
}

/// Start with Windows via the per-user Run key.
fn set_autostart(on: bool) {
    #[cfg(windows)]
    {
        let exe = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let cmd = format!("\"{exe}\" --autostart");
        winutil::set_run_entry("Echo", on.then_some(cmd.as_str()));
        // the app used to be called Clipper: drop its old autostart entry
        winutil::set_run_entry("Clipper", None);
    }
    #[cfg(not(windows))]
    {
        // ~/.config/autostart/echo.desktop
        let dir = std::env::var("XDG_CONFIG_HOME").map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")).join("autostart");
        let f = dir.join("echo.desktop");
        if on {
            // AppImage: start the .AppImage file itself (current_exe is inside its temporary mount)
            let exe = std::env::var("APPIMAGE").ok().filter(|p| !p.is_empty())
                .unwrap_or_else(|| std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default());
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(&f, format!("[Desktop Entry]\nType=Application\nName=Echo\nExec=\"{exe}\" --autostart\nIcon=echo-clips\nX-GNOME-Autostart-enabled=true\n"));
        } else {
            let _ = std::fs::remove_file(&f);
        }
    }
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        // Echo always opens full size; re-maximize so a window that came back oversized fits the screen again
        if w.is_maximized().unwrap_or(false) {
            let _ = w.unmaximize();
        }
        let _ = w.maximize();
        let _ = w.set_focus();
    }
}

fn main() {
    // Linux: map the Windows folders the code uses to their Linux homes
    #[cfg(target_os = "linux")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        if std::env::var_os("APPDATA").is_none() {
            let cfg = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{home}/.config"));
            std::env::set_var("APPDATA", cfg);
        }
        if std::env::var_os("USERPROFILE").is_none() { std::env::set_var("USERPROFILE", &home); }
    }
    // only one Echo: a second launch just brings the running one to the front
    #[cfg(target_os = "linux")]
    if linuxutil::already_running() {
        return;
    }
    #[cfg(target_os = "linux")]
    mediaserve::start();
    std::panic::set_hook(Box::new(|info| recorder::log_line(&format!("APP CRASH: {info}"))));
    // only one Clipper: a second launch just brings the running one to the front
    #[cfg(windows)]
    if winutil::already_running() {
        if !winutil::signal_show() {
            winutil::show_existing();
        }
        return;
    }
    let cfg = Settings::load();

    let app = tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, sc, ev| {
                    if ev.state() != ShortcutState::Pressed {
                        return;
                    }
                    let secs = app.state::<Hotkeys>().0.lock().unwrap().iter().find(|(s, _)| s == sc).map(|(_, n)| *n);
                    if let Some(secs) = secs {
                        let app = app.clone();
                        std::thread::spawn(move || {
                            let _ = do_save(&app, Some(secs));
                        });
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(Hotkeys(Mutex::new(Vec::new())))
        .manage(Recorder::new(cfg.clone()))
        .setup(move |app| {
            let handle = app.handle().clone();
            // turn off the browser's own shortcuts (Ctrl+F find bar, F5 reload, Ctrl+P print…) so Echo's own keys work
            #[cfg(windows)]
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.with_webview(|wv| unsafe {
                    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3;
                    use wincore62::Interface;
                    if let Ok(core) = wv.controller().CoreWebView2() {
                        if let Ok(s) = core.Settings() {
                            if let Ok(s3) = s.cast::<ICoreWebView2Settings3>() {
                                let _ = s3.SetAreBrowserAcceleratorKeysEnabled(false);
                            }
                        }
                    }
                });
            }
            #[cfg(any(windows, target_os = "linux"))]
            {
                let h = handle.clone();
                hotkey::start(Box::new(move |secs| {
                    if secs == 0 {
                        let _ = do_screenshot(&h);
                    } else if secs == BOOKMARK {
                        do_bookmark(&h);
                    } else if secs == SESSION {
                        let _ = do_session(&h);
                    } else if secs == MIC_MUTE {
                        do_mic_mute(&h);
                    } else {
                        let _ = do_save(&h, Some(secs));
                    }
                }));
            }
            allow_clips_dir(&handle, &cfg.clips_dir);
            #[cfg(target_os = "linux")]
            {
                let h = handle.clone();
                linuxutil::listen_show(move || {
                    let h2 = h.clone();
                    let _ = h.run_on_main_thread(move || show_main(&h2));
                });
            }
            #[cfg(windows)]
            {
                let h = handle.clone();
                winutil::listen_show(move || {
                    let h2 = h.clone();
                    let _ = h.run_on_main_thread(move || show_main(&h2));
                });
            }
            let _ = std::fs::create_dir_all(stickers_dir());
            allow_clips_dir(&handle, &stickers_dir().to_string_lossy());
            register_hotkeys(&handle, &cfg);
            if cfg.start_with_windows {
                set_autostart(true); // keeps the Run entry pointing at this exe (Echo.exe)
            }

            // tray icon
            let open = MenuItem::with_id(app, "open", "Open Echo", true, None::<&str>)?;
            let save = MenuItem::with_id(app, "save", "Save clip", true, None::<&str>)?;
            let long = MenuItem::with_id(app, "long", "Start / stop recording", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &save, &long, &quit])?;
            let mut tray = TrayIconBuilder::new()
                .tooltip("Echo")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id.as_ref() {
                    "open" => show_main(app),
                    "save" => {
                        let app = app.clone();
                        std::thread::spawn(move || {
                            let _ = do_save(&app, None);
                        });
                    }
                    "long" => {
                        let app = app.clone();
                        std::thread::spawn(move || {
                            let _ = do_session(&app);
                        });
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                        show_main(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            setup_toast(app);

            // cloud sync: every 5 minutes, or soon after a clip is saved / settings change
            {
                let h = handle.clone();
                std::thread::spawn(move || {
                    let mut last = std::time::Instant::now();
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(10));
                        let soon = SYNC_SOON.swap(false, std::sync::atomic::Ordering::Relaxed);
                        if !soon && last.elapsed().as_secs() < 300 {
                            continue;
                        }
                        last = std::time::Instant::now();
                        let rec = h.state::<Recorder>();
                        let changed = rec.cloud_sync();
                        let _ = h.emit("cloud-changed", changed);
                        if changed {
                            let _ = h.emit("clips-changed", ());
                        }
                    }
                });
            }

            // controller save button
            pad::set_combo(cfg.pad_buttons);
            {
                let h = handle.clone();
                pad::start(Box::new(move || {
                    let c = h.state::<Recorder>().settings();
                    let secs = if c.pad_seconds > 0 { c.pad_seconds } else { c.hotkeys.first().map_or(60, |k| k.seconds) };
                    let _ = do_save(&h, Some(secs));
                }));
            }

            // shrink old clips while the PC is idle and no game runs (off by default)
            #[cfg(any(windows, target_os = "linux"))]
            {
                let h = handle.clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(600));
                    let rec = h.state::<Recorder>();
                    let c = rec.settings();
                    #[cfg(windows)]
                    let idle = winutil::idle_seconds;
                    #[cfg(target_os = "linux")]
                    let idle = linuxutil::idle_seconds;
                    let calm = || idle() >= 300 && recorder::detect_game().is_none() && rec.session_secs().is_none();
                    if c.shrink_old && calm() && rec.shrink_old_clips(c.shrink_days, calm) > 0 {
                        let _ = h.emit("clips-changed", ());
                    }
                });
            }

            // auto-cleanup of old clips: shortly after launch, then every 6 hours
            {
                let h = handle.clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_secs(60));
                    let rec = h.state::<Recorder>();
                    rec.remove_stale_tmp();
                    let days = rec.settings().cleanup_days;
                    if days > 0 && rec.cleanup_old(days) > 0 {
                        let _ = h.emit("clips-changed", ());
                    }
                    std::thread::sleep(std::time::Duration::from_secs(6 * 3600));
                });
            }

            // Linux watch loop (every 3 s): game in the clip names / game folders, per-game rules, idle pause,
            // "upload when the game closes", and restart the recorder if it died
            #[cfg(target_os = "linux")]
            {
                let h = handle.clone();
                std::thread::spawn(move || {
                    let mut game: Option<(String, u32)> = None;
                    let mut down = 0u32;
                    let mut last_fix = std::time::Instant::now() - std::time::Duration::from_secs(120);
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(3));
                        let rec = h.state::<Recorder>();
                        let cfg = rec.settings();
                        if game.as_ref().map_or(true, |g| !linuxutil::pid_alive(g.1)) {
                            if game.take().is_some() {
                                SYNC_SOON.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                            if let Some((name, pid, exe)) = recorder::game_in_front() {
                                recorder::log_line(&format!("game: {name} (pid {pid})"));
                                recorder::remember_game(&name, &exe);
                                game = Some((name, pid));
                            }
                        }
                        if let Some(g) = &game {
                            *rec.last_game.lock().unwrap() = Some((g.0.clone(), std::time::Instant::now()));
                        }
                        let idle = if cfg.idle_pause_min > 0 { linuxutil::idle_seconds() } else { 0 };
                        let reason = if cfg.idle_pause_min > 0 && idle >= cfg.idle_pause_min as u64 * 60 {
                            "idle"
                        } else if game.as_ref().is_some_and(|g| !cfg.game_rule(&g.0).record) {
                            "game"
                        } else if cfg.only_while_game && game.is_none() {
                            "nogame"
                        } else {
                            ""
                        };
                        let cur = rec.pause_reason();
                        // audio/video clocks split (PC slept, clock jumped): a fresh start fixes the clips
                        if cur.is_empty() && !rec.busy() && rec.session_secs().is_none() && recorder::RESTART_REC.swap(false, std::sync::atomic::Ordering::Relaxed) {
                            recorder::log_line("restarting the recorder (clock jump)");
                            if let Err(e) = rec.start() {
                                recorder::log_line(&e);
                            }
                            down = 0;
                            continue;
                        }
                        if reason != cur && rec.session_secs().is_none() {
                            if cur.is_empty() {
                                recorder::log_line(&format!("pausing recorder ({reason}; idle {idle}s)"));
                                rec.set_pause(reason);
                                rec.stop();
                            } else if reason.is_empty() {
                                recorder::log_line(&format!("resuming recorder (was {cur})"));
                                rec.set_pause("");
                                if let Err(e) = rec.start() {
                                    recorder::log_line(&e);
                                }
                            } else {
                                rec.set_pause(reason);
                            }
                            down = 0;
                            continue;
                        }
                        let started_once = !rec.mode.lock().unwrap().is_empty();
                        if cur.is_empty() && started_once && !rec.busy() && !rec.is_recording() {
                            down += 1;
                        } else {
                            down = 0;
                        }
                        if down >= 2 {
                            down = 0;
                            recorder::log_line("recorder stopped unexpectedly");
                            recorder::play_sound(recorder::Sound::Stopped, &cfg, false);
                            if last_fix.elapsed().as_secs() > 60 {
                                last_fix = std::time::Instant::now();
                                toast(&h, "Recorder stopped", "Restarting it…", false);
                                if let Err(e) = rec.start() {
                                    recorder::log_line(&e);
                                }
                            } else {
                                toast(&h, "Recorder stopped", "Couldn't keep it running - open Echo", false);
                            }
                        }
                    }
                });
            }
            // every 3 s: pause / resume the buffer (idle, per-game "don't record", "only while a game runs"),
            // remember games for icons, and notice when the recorder died by itself
            #[cfg(windows)]
            {
                let h = handle.clone();
                std::thread::spawn(move || {
                    let mut game: Option<(String, u32)> = None;
                    let mut game_hwnd: isize = 0;
                    let mut down = 0u32;
                    let mut last_fix = std::time::Instant::now() - std::time::Duration::from_secs(120);
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(3));
                        let rec = h.state::<Recorder>();
                        let cfg = rec.settings();
                        if let Some((name, pid, exe)) = recorder::game_in_front() {
                            if game.as_ref().map(|g| g.1) != Some(pid) {
                                recorder::remember_game(&name, &exe);
                            }
                            *rec.last_game.lock().unwrap() = Some((name.clone(), std::time::Instant::now()));
                            game = Some((name, pid));
                            game_hwnd = winutil::foreground_hwnd();
                        }
                        if game.as_ref().is_some_and(|g| !winutil::pid_alive(g.1)) {
                            game = None;
                            SYNC_SOON.store(true, std::sync::atomic::Ordering::Relaxed); // "upload when the game closes"
                        }
                        // the game still runs (even if you tabbed out): clips keep its name
                        if let Some(g) = &game {
                            *rec.last_game.lock().unwrap() = Some((g.0.clone(), std::time::Instant::now()));
                        }
                        if game.is_none() || (game_hwnd != 0 && !winutil::window_alive(game_hwnd)) {
                            game_hwnd = 0;
                        }
                        let idle = winutil::idle_seconds();
                        let reason = if cfg.idle_pause_min > 0 && idle >= cfg.idle_pause_min as u64 * 60 {
                            "idle"
                        } else if game.as_ref().is_some_and(|g| !cfg.game_rule(&g.0).record) {
                            "game"
                        } else if cfg.only_while_game && game.is_none() {
                            "nogame"
                        } else {
                            ""
                        };
                        let cur = rec.pause_reason();
                        // audio/video clocks split (PC slept, clock jumped): a fresh start fixes the clips
                        if cur.is_empty() && !rec.busy() && rec.session_secs().is_none() && recorder::RESTART_REC.swap(false, std::sync::atomic::Ordering::Relaxed) {
                            recorder::log_line("restarting the recorder (clock jump)");
                            if let Err(e) = rec.start() {
                                recorder::log_line(&e);
                            }
                            down = 0;
                            continue;
                        }
                        if reason != cur && rec.session_secs().is_none() {
                            if cur.is_empty() {
                                recorder::log_line(&format!("pausing recorder ({reason}; idle {idle}s)"));
                                rec.set_pause(reason);
                                rec.stop();
                            } else if reason.is_empty() {
                                recorder::log_line(&format!("resuming recorder (was {cur})"));
                                rec.set_pause("");
                                if let Err(e) = rec.start() {
                                    recorder::log_line(&e);
                                }
                            } else {
                                rec.set_pause(reason);
                            }
                            down = 0;
                            continue;
                        }
                        // "Game window only": record just the game's window while it runs (restart when it changes)
                        let want = if cfg.capture == "game" { game_hwnd } else { 0 };
                        if cur.is_empty() && want != rec.window.load(std::sync::atomic::Ordering::Relaxed) && rec.session_secs().is_none() && !rec.busy() && !FPS_TESTING.load(std::sync::atomic::Ordering::Relaxed) {
                            rec.window.store(want, std::sync::atomic::Ordering::Relaxed);
                            recorder::log_line(if want != 0 { "capture: switching to the game window" } else { "capture: back to the whole monitor" });
                            if let Err(e) = rec.start() {
                                recorder::log_line(&e);
                            }
                            down = 0;
                            continue;
                        }
                        // watchdog: should be recording but isn't
                        let started_once = !rec.mode.lock().unwrap().is_empty();
                        if cur.is_empty() && started_once && !rec.busy() && !rec.is_recording() && !FPS_TESTING.load(std::sync::atomic::Ordering::Relaxed) {
                            down += 1;
                        } else {
                            down = 0;
                        }
                        if down >= 2 {
                            down = 0;
                            recorder::log_line("recorder stopped unexpectedly");
                            recorder::play_sound(recorder::Sound::Stopped, &cfg, false);
                            if last_fix.elapsed().as_secs() > 60 {
                                last_fix = std::time::Instant::now();
                                toast(&h, "Recorder stopped", "Restarting it…", false);
                                if let Err(e) = rec.start() {
                                    recorder::log_line(&e);
                                }
                            } else {
                                toast(&h, "Recorder stopped", "Couldn't keep it running - open Echo", false);
                            }
                        }
                    }
                });
            }

            // window starts hidden (see tauri.conf.json); opening Echo.exe yourself always shows it,
            // only the start with Windows stays in the tray ("start minimized" / "close to tray")
            let autostarted = std::env::args().any(|a| a == "--autostart");
            if !(autostarted && (cfg.start_minimized || cfg.close_to_tray)) {
                show_main(&handle);
            }

            // start recording in the background so the window opens instantly
            std::thread::spawn(move || {
                let _ = handle.state::<Recorder>().start();
            });
            Ok(())
        })
        .on_window_event(|w, ev| {
            if let WindowEvent::CloseRequested { api, .. } = ev {
                if w.app_handle().state::<Recorder>().settings().close_to_tray {
                    api.prevent_close();
                    let _ = w.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            update_check,
            update_install,
            hotkey_mode,
            hotkeys_paused,
            cloud_remove_account,
            echocloud_start,
            echocloud_ready,
            echocloud_plans,
            echocloud_checkout,
            echocloud_manage,
            echocloud_me,
            cloud_dl_progress,
            export_progress,
            cloud_up_progress,
            list_folders,
            create_folder,
            delete_folder,
            save_project,
            load_project,
            probe_media,
            pick_media,
            media_base,
            toggle_mic,
            cloud_link,
            compress_clip,
            cloud_connect,
            cloud_disconnect,
            cloud_info,
            cloud_files,
            cloud_sync_now,
            cloud_upload,
            cloud_download,
            cloud_remove_local,
            cloud_delete,
            cloud_forget_local,
            cloud_stream_base,
            cloud_thumb,
            self_test,
            list_monitors,
            rename_folder,
            save_sticker,
            pick_image,
            clip_meta,
            set_meta,
            set_thumb,
            cleanup_now,
            get_status, save_clip, test_sound, take_screenshot, get_favorites, set_favorite, list_clips, thumb, delete_clip, rename_clip, move_clip, sort_into_folders, toggle_session, list_games, share_clip, shared_clips, pick_wav, trim_clip, make_gif, combine_clips, import_clips, import_sources, import_folder, pick_folder, forget_game, audio_wav, send_log, pad_capture, pad_names, fps_test, frame_png, audio_mp3, fix_dark_clips, audio_tracks, track_levels, extract_track, list_mics, transcribe, captions_ready, filmstrip, render_edit, copy_clip, copy_clip_sized, list_shots, get_marks,
            restart_recorder, get_settings, set_settings, open_in_folder
        ])
        .build(tauri::generate_context!())
        .expect("error while building Echo");

    app.run(|app, event| {
        if let RunEvent::Exit = event {
            app.state::<Recorder>().stop();
        }
    });
}
