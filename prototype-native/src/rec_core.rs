//! Chat A (continues Chat C, queue items 10 + 11): Echo's REAL recorder in the native app.
//! DECISION (9 Nov): Echo's backend files (src-tauri/src/recorder.rs, settings.rs, hotkey*.rs, winutil.rs,
//! linuxutil.rs, gpucap.rs, pad.rs, net.rs, keyring_linux.rs) are compiled UNCHANGED as the library crate
//! prototype-native/echo-core (#[path] includes, no copies). This file is what src-tauri/src/main.rs did around them:
//! start/stop, the 3-second watch loop (pause rules, game window, self-healing), save / long recording / screenshot /
//! bookmark / mic mute, hotkeys + controller, settings.json changes, cleanup + shrink loops, cloud sync timer, status
//! for the header. Background threads never touch the UI: they push Ev into a queue the UI timer drains.
use echo_core::recorder::{self, Recorder, Sound};
use echo_core::settings::Settings;
use slint::ComponentHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use crate::AppWindow;

static REC: OnceLock<Recorder> = OnceLock::new();
/// Another Echo (the old Tauri app or a 2nd copy) holds the single-instance lock: this one shows clips but doesn't record
pub static OTHER_ECHO: AtomicBool = AtomicBool::new(false);
static SYNC_SOON: AtomicBool = AtomicBool::new(false);
/// last start error ("" = none) - the header says so instead of "Starting…"
static START_ERR: Mutex<String> = Mutex::new(String::new());
fn start_rec(r: &Recorder) {
    let e = r.start().err().unwrap_or_default();
    if !e.is_empty() {
        recorder::log_line(&e);
    }
    *START_ERR.lock().unwrap_or_else(|x| x.into_inner()) = e;
}
/// Echo's FPS_TESTING: the watch loop leaves the recorder alone while the FPS test pauses it
pub static FPS_TESTING: AtomicBool = AtomicBool::new(false);

/// Settings > Troubleshooting > FPS test = Echo's fps_test (main.rs): PresentMon measures the game ~20 s with the
/// recorder stopped, then ~20 s recording. `say` gets the progress lines. Blocking - run it on a thread.
pub fn fps_test(say: impl Fn(String)) -> Result<String, String> {
    struct Busy;
    impl Drop for Busy {
        fn drop(&mut self) {
            FPS_TESTING.store(false, Ordering::Relaxed);
        }
    }
    let rec = rec_for_recording().ok_or("Another Echo is recording - close it first")?;
    if FPS_TESTING.swap(true, Ordering::Relaxed) {
        return Err("The FPS test is already running".into());
    }
    let _busy = Busy;
    say("Getting the measuring tool (first time only)…".into());
    let exe = recorder::presentmon_exe()?;
    let csv = std::env::temp_dir().join("echo_fps.csv");
    let _ = std::fs::remove_file(&csv);
    say("Allow the Windows prompt (the measuring tool needs admin rights)…".into());
    let args = format!("--timed 68 --terminate_after_timed --no_console_stats --v1_metrics --output_file \"{}\"", csv.display());
    #[cfg(windows)]
    if echo_core::winutil::run_elevated(&exe.to_string_lossy(), &args, false).is_none() {
        return Err("The measuring tool needs the Windows permission prompt to be allowed".into());
    }
    #[cfg(not(windows))]
    let _ = (&exe, &args);
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
            say(format!("{msg} ({left} s)"));
            std::thread::sleep(Duration::from_millis(500));
        }
    };
    until(15, "Switch to your game now and play normally");
    rec.stop();
    until(38, "Measuring WITHOUT Echo recording");
    say("Starting Echo's recorder again…".into());
    start_rec(rec);
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

/// bookmark hotkey presses (unix seconds), kept until they fall out of the buffer
static MARKS: Mutex<Vec<f64>> = Mutex::new(Vec::new());
static EVENTS: Mutex<Vec<Ev>> = Mutex::new(Vec::new());
/// hotkey ids beyond seconds (same as Echo's main.rs)
const BOOKMARK: u32 = u32::MAX;
const SESSION: u32 = u32::MAX - 1;
const MIC_MUTE: u32 = u32::MAX - 2;
/// Show / hide Echo (showkey.rs; echo-core's hotkey_linux.rs knows this id for KDE's portal)
const SHOW: u32 = crate::showkey::SHOW;

/// what background threads tell the window
pub enum Ev {
    /// in-app message (only shown when the window is visible; the popup / notification covers games)
    Toast(String),
    /// a clip was saved / a recording stopped: library refresh, the "Saving clip…" box ends
    Saved(Result<String, String>),
    /// "Saving clip…" box (seconds) + the library's "Saving clip…" card (folder the clip goes to, Echo's save_target)
    Saving(u32, String),
    /// Settings changed on disk: header hotkey chips
    Settings,
    /// tray / second launch: bring the window up
    Show,
    /// tray "Update to Echo X": download + install the pending update (setup.rs)
    Update,
    Quit,
}
pub fn push(e: Ev) {
    EVENTS.lock().unwrap_or_else(|e| e.into_inner()).push(e);
}

pub fn rec() -> Option<&'static Recorder> {
    REC.get()
}
/// false = another Echo holds the lock (this one shows clips, never records)
pub fn can_record() -> bool {
    REC.get().is_some() && !OTHER_ECHO.load(Ordering::Relaxed)
}
fn rec_for_recording() -> Option<&'static Recorder> {
    can_record().then(|| REC.get()).flatten()
}
fn now_secs() -> f64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}
fn fmt_len(s: u32) -> String {
    if s >= 60 && s % 60 == 0 { format!("{} min", s / 60) } else if s >= 60 { format!("{}:{:02}", s / 60, s % 60) } else { format!("{s} s") }
}

/// Before anything reads settings: Linux maps the Windows folder variables Echo's code uses (same as Echo's main()).
pub fn early_env() {
    #[cfg(target_os = "linux")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        if std::env::var_os("APPDATA").is_none() {
            let cfg = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{home}/.config"));
            std::env::set_var("APPDATA", cfg);
        }
        if std::env::var_os("USERPROFILE").is_none() {
            std::env::set_var("USERPROFILE", &home);
        }
        crate::linux::early();
    }
}

/// Single instance (Echo's lock): false = another Echo already runs. Then this window still opens (so the prototype
/// can be tried next to the old Echo), but it never records - two recorders would share one buffer folder.
fn take_instance_lock() -> bool {
    if std::env::var("ECHO_NO_RECORDER").is_ok() {
        return false;
    }
    #[cfg(windows)]
    return !echo_core::winutil::already_running();
    #[cfg(target_os = "linux")]
    return !echo_core::linuxutil::already_running();
    #[allow(unreachable_code)]
    true
}

/// Called once from main() after the window exists.
pub fn wire(app: &AppWindow) {
    // audit (9 Nov): Echo's av1_support. Echo's WebView reports whether AV1 plays in hardware; without it Echo never
    // makes AV1 files (Settings "Shrink with AV1" silently did nothing here). Our player is mpv with hardware decoding
    // (d3d11va / auto-safe), and av1_ready() also needs a GPU that ENCODES AV1 - every such GPU decodes it in hardware.
    recorder::AV1_PLAYS.store(true, Ordering::Relaxed);
    recorder::log_line("av1: native player (mpv, hardware decoding) - AV1 allowed when the GPU encodes it");
    // the Recorder always exists (library / cloud / export functions); it only RECORDS while this app holds the lock
    let cfg = Settings::load();
    recorder::set_rclone_pass(&cfg.rclone_pass);
    let _ = REC.set(Recorder::new(cfg.clone()));
    if !take_instance_lock() {
        // release (9 Nov): the installed Echo behaves like the old one - a 2nd launch brings the running Echo to the
        // front and quits (the window isn't shown yet here). Prototype builds keep opening a clips-only window.
        if is_release_exe() && std::env::var("ECHO_NO_RECORDER").is_err() {
            recorder::log_line("native: Echo already runs - showing it, this copy quits");
            #[cfg(windows)]
            if !echo_core::winutil::signal_show() {
                echo_core::winutil::show_existing();
            }
            std::process::exit(0);
        }
        OTHER_ECHO.store(true, Ordering::Relaxed);
        app.set_rec_state("Echo is already running".into());
        app.set_rail_state("Echo is already running".into());
        app.set_rail_mode("The other Echo records - close it and restart this one to record here".into());
        app.set_rec_tip("The other Echo records - close it and restart this one to record here".into());
        // test hook for screenshots (site pictures): ECHO_FAKE_GAME shows the normal "Ready to clip" status text here
        if std::env::var("ECHO_FAKE_GAME").is_ok() {
            let b = fmt_len(cfg.buffer_seconds());
            app.set_rec_state(format!("Recording · last {b}").into());
            app.set_rec_on(true);
            app.set_rail_state("Ready to clip".into());
            app.set_rail_mode(format!("Your last {b} is always ready · GPU").into());
            app.set_rail_tag("GPU".into());
            app.set_rec_tip("Your last moments are always ready · GPU".into());
        }
        recorder::log_line("native: another Echo holds the lock - recorder off in this window");
    } else {
        std::panic::set_hook(Box::new(|info| recorder::log_line(&format!("APP CRASH: {info}"))));
        // cloud streaming servers an earlier Echo left running (10 Nov 2026: 3 were found, ~40-80 MB each)
        std::thread::spawn(Recorder::reap_stale_serves);
        // a second launch (or the old Echo started later) brings this window up
        #[cfg(windows)]
        echo_core::winutil::listen_show(|| push(Ev::Show));
        #[cfg(target_os = "linux")]
        echo_core::linuxutil::listen_show(|| push(Ev::Show));
        // 9 Nov: Echo's Discord status ("Playing Echo · Ready to clip") - only the Echo that records
        crate::discord::start();
        register_hotkeys(&cfg);
        start_hotkeys();
        echo_core::pad::set_combo(cfg.pad_buttons);
        echo_core::pad::start(Box::new(|| {
            let Some(r) = rec() else { return };
            let c = r.settings();
            let secs = if c.pad_seconds > 0 { c.pad_seconds } else { c.hotkeys.first().map_or(60, |k| k.seconds) };
            let _ = do_save(Some(secs));
        }));
        autostart(cfg.start_with_windows);
        std::thread::spawn(|| {
            let Some(r) = rec() else { return };
            r.secure_cloud();
            #[cfg(target_os = "linux")]
            crate::linux::flatpak_wait(); // Flatpak: Welcome screen -> shortcut dialog -> screen-share dialog
            start_rec(r);
            watch_loop();
        });
        background_loops();
        crate::popup::keeper();
    }
    crate::tray::start();
    #[cfg(target_os = "linux")]
    crate::linux::wire(app); // "Echo needs ffmpeg" when there is none
    crate::copy::wire(app);
    crate::titlebar::wire(app);
    // UI side: drain events + header status every 0.5 s
    let t = Box::leak(Box::new(slint::Timer::default()));
    let w = app.as_weak();
    let mut tick = 0u32;
    t.start(slint::TimerMode::Repeated, Duration::from_millis(500), move || {
        let Some(a) = w.upgrade() else { return };
        let evs: Vec<Ev> = std::mem::take(&mut *EVENTS.lock().unwrap_or_else(|e| e.into_inner()));
        for e in evs {
            handle(&a, e);
        }
        tick += 1;
        if tick % 2 == 0 {
            draw_status(&a);
        }
        // the Record button's dot blinks (on 0.5 s, dim 0.5 s) while a long recording runs; an unchanged value is no
        // repaint, so this costs nothing otherwise (theme.slint T.blink)
        let blink = a.get_rec_long_on() && tick % 2 == 1;
        let tg = a.global::<crate::T>();
        if tg.get_blink() != blink {
            tg.set_blink(blink);
        }
        // the mic icon follows within 0.5 s when the mute HOTKEY changes it (a click flips it at once, see rec_act)
        let muted = recorder::MIC_MUTED.load(Ordering::Relaxed);
        if a.get_rec_mic_off() != muted {
            a.set_rec_mic_off(muted);
        }
    });
    draw_status(app);
    // Record / Save clip / mic buttons in the header
    let w = app.as_weak();
    app.on_rec_act(move |what| {
        let Some(a) = w.upgrade() else { return };
        if !can_record() {
            crate::toast(&a, "Another Echo is recording - close it and restart this one to record here");
            return;
        }
        let what = what.to_string();
        if what == "long" && rec().is_some_and(|r| r.session_secs().is_some()) {
            a.set_rec_long("Saving…".into());
        }
        // the mic icon lagged ~1 s behind the click (header status redraws once a second) - flip it now;
        // do_mic_mute on the thread below flips the real state the same way
        if what == "mic" {
            a.set_rec_mic_off(!recorder::MIC_MUTED.load(Ordering::Relaxed));
        }
        // gaps (10 Nov): Echo's #longbtn - its toasts, then the new recording opens in the player (hdr_home.rs)
        if what == "long" {
            crate::hdr_home::record_click(&a);
            return;
        }
        std::thread::spawn(move || match what.as_str() {
            "save" => { let _ = do_save(None); }
            "long" => { let _ = do_session(); }
            "mic" => do_mic_mute(),
            "shot" => { let _ = do_screenshot(); }
            "restart" => {
                if let Some(r) = rec() {
                    r.stop();
                    start_rec(r);
                    let e = START_ERR.lock().unwrap_or_else(|x| x.into_inner()).clone();
                    push(Ev::Toast(if e.is_empty() { "Recorder restarted".into() } else { e }));
                }
            }
            _ => {}
        });
    });
}

/// the library's "Saving clip…" card: its row + text (also called from the 3 s disk sync, which ends it)
pub fn saving_card(a: &AppWindow, l: &crate::library::LibModel) {
    l.saving_expire();
    l.rebuild_rows();
    let (line, done) = l.saving_text();
    let g = a.global::<crate::Lib>();
    g.set_saving_line(line.into());
    g.set_saving_done(done);
}

fn handle(a: &AppWindow, e: Ev) {
    match e {
        Ev::Toast(s) => crate::toast(a, s),
        Ev::Saving(secs, folder) => {
            a.set_rec_saving(format!("Saving the last {}…", fmt_len(secs)).into());
            if let Some(l) = crate::LIB.with(|l| l.borrow().clone()) {
                *l.saving.borrow_mut() = Some(crate::library::SavingPh { secs, at: Instant::now(), folder, path: None });
                saving_card(a, &l);
            }
        }
        Ev::Saved(r) => {
            a.set_rec_saving("".into());
            if let Some(l) = crate::LIB.with(|l| l.borrow().clone()) {
                {
                    let mut s = l.saving.borrow_mut();
                    let saved = match &r { Ok(p) if !p.is_empty() => Some(std::path::PathBuf::from(p)), _ => None };
                    // saved: "Almost ready…" until the disk sync puts the clip in the list; failed: gone now
                    match (saved, s.is_some()) {
                        (Some(p), true) => { if let Some(ph) = s.as_mut() { ph.path = Some(p); } }
                        _ => *s = None,
                    }
                }
                saving_card(a, &l);
            }
            match r {
                Ok(p) if !p.is_empty() => {
                    if !crate::hdr_home::quiet_saved() {
                        crate::toast(a, "Clip saved");
                    }
                    let _ = p; // the library picks the new file up within 3 s (disk sync)
                    #[cfg(target_os = "linux")]
                    crate::linux::clip_saved(a); // one-time KDE Do Not Disturb tip
                }
                Ok(_) => {}
                Err(e) => crate::toast(a, e),
            }
        }
        Ev::Settings => settings_hook(a),
        Ev::Show => show_window(a),
        Ev::Update => {
            // the download progress box is in the window: show it
            show_window(a);
            crate::setup::install(a);
        }
        Ev::Quit => quit(),
    }
}

thread_local! {
    static MAX_TIMER: slint::Timer = slint::Timer::default();
    /// the main window was on screen once (open_maximized)
    static WAS_SHOWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 12 Nov 2026: EVERY opening of Echo's window is maximized - start, start from Start / search, a 2nd launch, tray
/// click / "Open Echo", the show key, after closing it to the tray. The user can un-maximize it while it's open; that
/// isn't remembered (closed and opened again = maximized). Not while the player is fullscreen.
/// An existing (hidden) window is maximized before it shows (no jump). The very first window only once it is really on
/// screen: one made maximized before it existed (set_maximized before the event loop made it) was turned back into a
/// 1500 x 900 one by Slint's first size update (winit's resize request un-maximizes on Windows).
fn open_maximized(a: &AppWindow) {
    use raw_window_handle::HasWindowHandle;
    use slint::winit_030::WinitWindowAccessor;
    if a.window().is_fullscreen() {
        return;
    }
    // a window that was never on screen (start into the tray) isn't: maximizing it while hidden showed it at once,
    // then it hid and showed again (a blink)
    let exists = WAS_SHOWN.with(|s| s.get()) && a.window().window_handle().window_handle().is_ok();
    if exists && !a.window().is_maximized() {
        a.window().set_maximized(true);
    }
    // and once more when it is on screen (first window, or anything that un-maximized it while showing)
    let w = a.as_weak();
    let tries = std::cell::Cell::new(0u32);
    MAX_TIMER.with(|t| {
        t.start(slint::TimerMode::Repeated, Duration::from_millis(20), move || {
            let Some(a) = w.upgrade() else { return };
            tries.set(tries.get() + 1);
            let shown = a.window().with_winit_window(|ww| ww.is_visible() != Some(false)).unwrap_or(false) && a.window().size().width > 0;
            if !shown && tries.get() < 150 {
                return;
            }
            if shown {
                WAS_SHOWN.with(|s| s.set(true));
                if !a.window().is_maximized() {
                    a.window().set_maximized(true);
                }
            }
            // once (else it would wake Echo every 20 ms for the whole session)
            MAX_TIMER.with(|t| t.stop());
        });
    });
}

/// Tray click / tray "Open Echo" / a 2nd launch (Start, search) / the show-hide key / an update: bring the window up,
/// maximized (open_maximized).
pub fn show_window(a: &AppWindow) {
    open_maximized(a);
    let _ = a.show();
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE};
        if let Ok(h) = a.window().window_handle().window_handle() {
            if let RawWindowHandle::Win32(h) = h.as_raw() {
                unsafe {
                    let hw = h.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                    // only un-minimize. 12 Nov 2026: SW_RESTORE on a window that show() had just brought back
                    // MAXIMIZED from the tray made it a normal window again ~15 ms later ("briefly fullscreens, then
                    // undoes it")
                    if IsIconic(hw) != 0 {
                        ShowWindow(hw, SW_RESTORE);
                    }
                    SetForegroundWindow(hw);
                }
            }
        }
    }
}

/// Quit for real (tray > Quit, or closing the window with "close to tray" off): stop the recorder + its helpers first.
pub fn quit() {
    if let Some(r) = rec() {
        r.shutdown();
    }
    crate::tray::stop();
    let _ = slint::quit_event_loop();
}

/// Window close button: hide to the tray when Echo's "close to tray" is on and a tray icon exists, else quit.
pub fn close_requested(a: &AppWindow) -> slint::CloseRequestResponse {
    let to_tray = rec().is_some_and(|r| r.settings().close_to_tray) && crate::tray::exists();
    if to_tray {
        hide_hook(a);
        let _ = a.hide();
        slint::CloseRequestResponse::HideWindow
    } else {
        quit();
        slint::CloseRequestResponse::HideWindow
    }
}

/// main()'s app.run(): show unless started with the PC into the tray, run until quit() (the window can be hidden).
pub fn run(app: &AppWindow) {
    let w = app.as_weak();
    app.window().on_close_requested(move || match w.upgrade() { Some(a) => close_requested(&a), None => slint::CloseRequestResponse::HideWindow });
    let autostarted = std::env::args().any(|a| a == "--autostart");
    let tray_start = autostarted && crate::tray::exists() && rec().is_some_and(|r| { let c = r.settings(); c.start_minimized || c.close_to_tray });
    if !tray_start {
        let _ = app.show();
        open_maximized(app);
    }
    let _ = slint::run_event_loop_until_quit();
    if let Some(r) = rec() {
        r.shutdown();
    }
}

thread_local! {
    /// the sidebar's Echo Profile row was hidden with its x (native_ui.json "acct_hide"; read once)
    static ACCT_HIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}
fn acct_hidden() -> bool {
    ACCT_HIDE.with(|h| {
        if h.get().is_none() {
            h.set(Some(crate::lib_fx::ui_get("acct_hide").as_bool() == Some(true)));
        }
        h.get() == Some(true)
    })
}
/// x on the row (true) / Settings > Home tips > Show again (false)
pub fn set_acct_hidden(on: bool) {
    ACCT_HIDE.with(|h| h.set(Some(on)));
    crate::lib_fx::ui_set("acct_hide", if on { serde_json::json!(true) } else { serde_json::Value::Null });
}

fn draw_status(a: &AppWindow) {
    let Some(r) = rec_for_recording() else { return };
    let cfg = r.settings();
    let on = r.is_recording();
    let paused = r.pause_reason();
    let state = match paused.as_str() {
        "idle" => "Paused · you're idle".to_string(),
        "game" => "Paused · this game isn't recorded".to_string(),
        "nogame" => "Waiting for a game".to_string(),
        _ if on => format!("Recording · last {}", fmt_len(cfg.buffer_seconds())),
        _ if !START_ERR.lock().map_or(true, |e| e.is_empty()) => "Recorder didn't start".to_string(),
        _ if r.mode.lock().map_or(true, |m| m.is_empty()) => "Starting…".to_string(),
        _ => "Not recording".to_string(),
    };
    a.set_rec_state(state.into());
    a.set_rec_on(on && paused.is_empty());
    let mode = r.mode.lock().map(|m| m.clone()).unwrap_or_default();
    let err = START_ERR.lock().map(|e| e.clone()).unwrap_or_default();
    a.set_rec_tip(match mode.as_str() {
        _ if !err.is_empty() && !on => format!("{err} - double-click the status to try again").into(),
        "gpu" => "Your last moments are always ready · GPU".into(),
        "" => "Settings → App → Restart recorder".into(),
        _ => "Your last moments are always ready · CPU fallback".into(),
    });
    // Echo's sidebar statcard: #railstate / #railmode / #railhealth (ui/index.html refresh + drawHealth)
    a.set_rail_state(match paused.as_str() {
        "idle" => "Paused · you're idle".to_string(),
        "game" => "Paused · this game isn't recorded".to_string(),
        "nogame" => "Waiting for a game".to_string(),
        _ if on => "Ready to clip".to_string(),
        _ if r.mode.lock().map_or(true, |m| m.is_empty()) && err.is_empty() => "Starting…".to_string(),
        _ => "Recorder stopped".to_string(),
    }.into());
    a.set_rail_mode(if on {
        format!("Your last {} is always ready · {}", fmt_len(cfg.buffer_seconds()), if mode == "gpu" { "GPU" } else { "CPU fallback" })
    } else if !err.is_empty() {
        format!("{err} - double-click the status to try again")
    } else {
        "Settings → App → Restart recorder".to_string()
    }.into());
    // 10 Nov: compact status card - "GPU" / "CPU" on the right, the cloud's usage in the bar
    a.set_rail_tag(if !on { "" } else if mode == "gpu" { "GPU" } else { "CPU" }.into());
    // 10 Nov: each connected cloud on its own (they were added together): the card shows the first one
    // (Echo Cloud when there is one), the hover panel all of them
    match crate::cloud::usages_for_sidebar(a).filter(|v| !v.is_empty()) {
        Some(list) => {
            let short = |b: u64| {
                let b = b as f64;
                if b >= 1e12 {
                    format!("{:.1} TB", b / 1e12)
                } else if b >= 1e10 {
                    format!("{:.0} GB", b / 1e9)
                } else if b >= 1e9 {
                    format!("{:.1} GB", b / 1e9)
                } else {
                    format!("{:.0} MB", b / 1e6)
                }
            };
            let text = |used: u64, total: u64, err: &str| {
                if !err.is_empty() && used == 0 && total == 0 {
                    "space unknown right now".to_string()
                } else if total > 0 {
                    format!("{} of {}", short(used), short(total))
                } else {
                    format!("{} used", short(used))
                }
            };
            let frac = |used: u64, total: u64| if total > 0 { used as f32 / total as f32 } else { 0.0 };
            let (n0, u0, t0, e0) = &list[0];
            a.set_cloud_on(true);
            a.set_cloud_name(n0.as_str().into());
            a.set_cloud_frac(frac(*u0, *t0));
            a.set_cloud_text(text(*u0, *t0, e0).into());
            let rows: Vec<crate::CloudUse> = list
                .iter()
                .map(|(n, u, t, e)| crate::CloudUse { name: n.as_str().into(), text: text(*u, *t, e).into(), frac: frac(*u, *t) })
                .collect();
            // 11 Nov (idle CPU): a NEW list every second rebuilt the hover panel's rows and repainted the window
            // once a second for nothing - only replace it when a row changed
            let same = {
                use slint::Model;
                let m = a.get_cloud_uses();
                m.row_count() == rows.len() && rows.iter().enumerate().all(|(i, r)| m.row_data(i).as_ref() == Some(r))
            };
            if !same {
                a.set_cloud_uses(slint::ModelRc::new(slint::VecModel::from(rows)));
            }
        }
        None => a.set_cloud_on(false),
    }
    {
        let mut out: Vec<(i32, String)> = vec![];
        if recorder::CLOUD_LOCKED.load(Ordering::Relaxed) {
            out.push((2, "Cloud logins locked - Settings → Cloud".into()));
        }
        if !paused.is_empty() || !on {
            if out.is_empty() {
                a.set_rail_health("".into());
            }
        } else {
            // r2-threads (L12): the buffer folder and free space are read on a worker every 4 s (Echo's get_status)
            let (age, free) = crate::bgwork::health(r, std::path::Path::new(&cfg.clips_dir));
            if let Some(age) = age.filter(|a| *a > 8.0) {
                out.push((2, format!("Stalled - no new video for {} s (Settings → App → Restart)", age.round())));
            }
            if free > 0 && free < 5_000_000_000 {
                out.push((if free < 1_500_000_000 { 2 } else { 1 }, format!("Low disk space - {} left", crate::library::fmt_size(free))));
            }
            out.extend(crate::cloud_live::health_lines(a, r)); // gaps-cloud (row 26): GPU busy, cloud busy, cloud usage
            if out.is_empty() {
                out.push((0, if free > 0 { format!("✓ Healthy · {} free", crate::library::fmt_size(free)) } else { "✓ Healthy".into() }));
            }
        }
        if !out.is_empty() {
            a.set_rail_health_k(out.iter().map(|o| o.0).max().unwrap_or(0));
            a.set_rail_health(out.into_iter().map(|o| o.1).collect::<Vec<_>>().join("\n").into());
        }
    }
    // Echo's #acctrow: "Signed in ✓" / "Get started →" (hidden with its x until Settings > Home tips > Show again)
    let signed = cfg.cloud_accounts.iter().any(|x| x.provider == "echocloud");
    a.set_acct_signed(signed);
    a.set_acct_on(signed || !acct_hidden());
    match r.session_secs() {
        Some(s) => {
            a.set_rec_long_on(true);
            a.set_rec_long(format!("Stop · {}:{:02}", s / 60, s % 60).into());
        }
        None => {
            a.set_rec_long_on(false);
            a.set_rec_long("Record".into());
        }
    }
    a.set_rec_mic_off(recorder::MIC_MUTED.load(Ordering::Relaxed));
    draw_game(a, r, &paused, on);
}

thread_local! {
    /// game name -> its icon (Echo's icons/<game>.png), looked up once per game
    static GAME_ICON: std::cell::RefCell<(String, Option<slint::Image>)> = std::cell::RefCell::new((String::new(), None));
}
/// Title bar chip (sketch D): the game Echo sees right now (seen in the last 10 s; ECHO_FAKE_GAME=<name> for tests).
fn draw_game(a: &AppWindow, r: &Recorder, paused: &str, on: bool) {
    let name = std::env::var("ECHO_FAKE_GAME").ok().or_else(|| {
        r.last_game.lock().ok().and_then(|g| g.as_ref().filter(|(_, t)| t.elapsed().as_secs() < 10).map(|(n, _)| n.clone()))
    }).unwrap_or_default();
    if name != a.get_rec_game().as_str() {
        let icon = GAME_ICON.with(|c| {
            let mut c = c.borrow_mut();
            if c.0 != name {
                // toast_game looks the icon up from a clip name: "<Game> <date>"
                let (_, url) = recorder::toast_game(&format!("{name} 2000-01-01 00-00-00"));
                let img = url.strip_prefix("data:image/png;base64,").and_then(recorder::unb64)
                    .and_then(|b| image::load_from_memory_with_format(&b, image::ImageFormat::Png).ok())
                    .map(|i| { let i = i.to_rgba8(); slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(i.as_raw(), i.width(), i.height())) });
                *c = (name.clone(), img);
            }
            c.1.clone()
        });
        a.set_rec_game_has_icon(icon.is_some());
        a.set_rec_game_icon(icon.unwrap_or_default());
        a.set_rec_game_letter(name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default().into());
        a.set_rec_game(name.clone().into());
    }
    if name.is_empty() {
        return;
    }
    let on = on || std::env::var("ECHO_FAKE_GAME").is_ok(); // test hook shows the recording look
    let off = paused == "game";
    a.set_rec_game_off(off || !on);
    a.set_rec_game_sub(if off { "not recorded" } else if !on { "not recording" } else if r.window.load(Ordering::Relaxed) != 0 { "recording its window" } else { "recording" }.into());
}

// ---------- hotkeys ----------
fn register_hotkeys(cfg: &Settings) {
    recorder::PTT_ON.store(cfg.push_to_talk && !cfg.ptt_key.is_empty(), Ordering::Relaxed);
    #[cfg(any(windows, target_os = "linux"))]
    {
        let mut list: Vec<(String, u32)> = cfg.hotkeys.iter().map(|h| (h.keys.clone(), h.seconds)).collect();
        if !cfg.screenshot_key.is_empty() { list.push((cfg.screenshot_key.clone(), 0)); }
        if !cfg.bookmark_key.is_empty() { list.push((cfg.bookmark_key.clone(), BOOKMARK)); }
        if !cfg.session_key.is_empty() { list.push((cfg.session_key.clone(), SESSION)); }
        if !cfg.mic_mute_key.is_empty() { list.push((cfg.mic_mute_key.clone(), MIC_MUTE)); }
        if !cfg.show_key.is_empty() { list.push((cfg.show_key.clone(), SHOW)); }
        echo_core::hotkey::set_ptt(if cfg.push_to_talk { cfg.ptt_key.as_str() } else { "" });
        for bad in echo_core::hotkey::set(&list) {
            recorder::log_line(&format!("hotkey {bad} invalid"));
        }
    }
}
fn start_hotkeys() {
    #[cfg(any(windows, target_os = "linux"))]
    echo_core::hotkey::start(Box::new(|secs| match secs {
        0 => { let _ = do_screenshot(); }
        BOOKMARK => do_bookmark(),
        SESSION => { let _ = do_session(); }
        MIC_MUTE => do_mic_mute(),
        SHOW => crate::showkey::pressed(),
        s => { let _ = do_save(Some(s)); }
    }));
}
/// Settings > "Press keys…" is picking a key: save keys must not fire meanwhile (for Chat B's settings page).
pub fn hotkeys_paused(on: bool) {
    #[cfg(any(windows, target_os = "linux"))]
    echo_core::hotkey::pause(on);
    let _ = on;
}

// ---------- actions (same as Echo's main.rs do_save / do_session / ...) ----------
pub fn toast(title: &str, sub: &str, ok: bool) {
    crate::popup::show(title, sub, ok, "");
}

pub fn do_save(seconds: Option<u32>) -> Result<String, String> {
    let Some(rec) = rec_for_recording() else { return Err("The recorder isn't running".into()) };
    let pressed = now_secs();
    let cfg = rec.settings();
    let paused = rec.pause_reason();
    if !paused.is_empty() {
        let why = match paused.as_str() { "idle" => "you were away", "game" => "this game is set to not record", _ => "no game is running" };
        recorder::play_sound(Sound::Failed, &cfg, false);
        toast("Not recording", &format!("Paused - {why}"), false);
        let e: Result<String, String> = Err(format!("Recording is paused ({why})"));
        push(Ev::Saved(e.clone()));
        return e;
    }
    // chime + popup right away on the keypress (saving long clips takes a moment); buzz later only if it fails
    recorder::play_sound(Sound::Saved, &cfg, false);
    let secs = seconds.unwrap_or_else(|| cfg.hotkeys.first().map(|h| h.seconds).unwrap_or(30));
    toast("Saving clip…", &format!("Last {}", fmt_len(secs)), true);
    push(Ev::Saving(secs, rec.save_target(secs)));
    let r = rec.save(seconds);
    match &r {
        Ok(p) => {
            let dur = rec.thumb(p).map(|t| t.duration).unwrap_or(secs as f64);
            let start = pressed - dur;
            let marks: Vec<f64> = MARKS.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|t| **t >= start && **t <= pressed).map(|t| ((t - start) * 100.0).round() / 100.0).collect();
            if !marks.is_empty() {
                rec.write_marks(p, &marks);
            }
            let name = std::path::Path::new(p).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let (game, icon) = recorder::toast_game(&name);
            let len = if dur >= 60.0 { format!("{}:{:02}", dur as u64 / 60, dur as u64 % 60) } else { format!("{} s", dur.round() as u64) };
            let sub = match game { Some(g) => format!("{g} · {len}"), None => format!("{len} · in your library") };
            crate::popup::show("Clip saved", &sub, true, &icon);
            crate::discord::CLIPS.fetch_add(1, Ordering::Relaxed); // Discord status "N clips saved"
            strip_soon(p);
            SYNC_SOON.store(true, Ordering::Relaxed);
        }
        Err(e) => {
            recorder::play_sound(Sound::Failed, &cfg, false);
            toast("Couldn't save clip", e, false);
        }
    }
    push(Ev::Saved(r.clone()));
    r
}

fn strip_soon(p: &str) {
    let p = p.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        if let Some(r) = rec() {
            let _ = r.filmstrip(&p);
        }
    });
}

pub fn do_screenshot() -> Result<String, String> {
    let Some(rec) = rec_for_recording() else { return Err("The recorder isn't running".into()) };
    let cfg = rec.settings();
    recorder::play_sound(Sound::Shot, &cfg, false);
    let r = rec.screenshot();
    match &r {
        Ok(_) => toast("Screenshot saved", "Clips folder › Screenshots", true),
        Err(e) => {
            recorder::play_sound(Sound::Failed, &cfg, false);
            toast("Screenshot failed", e, false);
        }
    }
    r
}

fn do_bookmark() {
    let Some(rec) = rec() else { return };
    let cfg = rec.settings();
    let now = now_secs();
    let keep = cfg.buffer_seconds() as f64 + 30.0;
    let n = {
        let mut m = MARKS.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|t| now - *t < keep);
        m.push(now);
        m.len()
    };
    recorder::play_sound(Sound::Mark, &cfg, false);
    toast("Bookmarked", &format!("{n} in the buffer · shows on the next saved clip"), true);
}

pub fn do_mic_mute() {
    let muted = !recorder::MIC_MUTED.load(Ordering::Relaxed);
    recorder::MIC_MUTED.store(muted, Ordering::Relaxed);
    if let Some(r) = rec() {
        recorder::play_sound(if muted { Sound::Mark } else { Sound::Saved }, &r.settings(), false);
    }
    toast(if muted { "Mic muted" } else { "Mic on" }, if muted { "Your voice isn't recorded" } else { "Your voice is recorded again" }, !muted);
}

/// Start or stop the long (full-session) recording.
pub fn do_session() -> Result<Option<String>, String> {
    let Some(rec) = rec_for_recording() else { return Err("The recorder isn't running".into()) };
    let cfg = rec.settings();
    let r = if rec.session_secs().is_some() {
        toast("Saving recording…", "One moment", true);
        let r = rec.stop_session().map(Some);
        match &r {
            Ok(Some(p)) => {
                let _ = rec.thumb(p);
                strip_soon(p);
                toast("Recording saved", &std::path::Path::new(p).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(), true);
            }
            Err(e) => toast("Recording failed", e, false),
            _ => {}
        }
        push(Ev::Saved(r.clone().map(|o| o.unwrap_or_default())));
        r
    } else {
        let r = rec.start_session().map(|_| None);
        match &r {
            Ok(_) => {
                recorder::play_sound(Sound::Mark, &cfg, false);
                toast("Recording", "Press the key again (or Stop in Echo) to save it", true);
            }
            Err(e) => toast("Couldn't start recording", e, false),
        }
        r
    };
    if r.is_err() {
        recorder::play_sound(Sound::Failed, &cfg, false);
    }
    r
}

// ---------- start with the PC ----------
/// Only the RELEASE exe writes the autostart entry: a prototype build (EchoProto.exe, echo-native-proto) must never
/// take over the old Echo's "start with Windows" entry while both exist. Release 9 Nov: the installed exe is
/// EchoClip.exe (chat 31 rename) - it checked for "echo" only, so after the update nobody's autostart was rewritten.
/// the installed app (EchoClip.exe / Echo.exe on Windows, echo / echo-clips on Linux), not a prototype build
pub fn is_release_exe() -> bool {
    let exe = std::env::current_exe().unwrap_or_default();
    let name = exe.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    matches!(name.as_str(), "echo" | "echoclip" | "echo-clips")
}

fn autostart(on: bool) {
    let exe = std::env::current_exe().unwrap_or_default();
    if !is_release_exe() {
        return;
    }
    #[cfg(windows)]
    {
        let cmd = format!("\"{}\" --autostart", exe.display());
        echo_core::winutil::set_run_entry("Echo", on.then_some(cmd.as_str()));
        echo_core::winutil::set_run_entry("Clipper", None);
    }
    #[cfg(target_os = "linux")]
    if echo_core::linuxutil::in_flatpak() {
        crate::linux::flatpak_autostart(on); // ~/.config/autostart is outside the sandbox: the Background portal
        return;
    }
    #[cfg(target_os = "linux")]
    {
        let dir = std::env::var("XDG_CONFIG_HOME").map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")).join("autostart");
        let f = dir.join("echo.desktop");
        let exe = std::env::var("APPIMAGE").ok().filter(|p| !p.is_empty()).unwrap_or_else(|| exe.to_string_lossy().to_string());
        // 10 Nov 2026: leave an entry alone that starts ANOTHER Echo which still exists - a dev build (./Echo in the
        // repo, also named "echo") kept replacing the developer's AppImage entry, and turning the switch off in it removed it
        let other = std::fs::read_to_string(&f).ok().and_then(|s| {
            let e = s.lines().find_map(|l| l.strip_prefix("Exec="))?.trim().to_string();
            let path = match e.strip_prefix('"') { Some(r) => r.split('"').next()?.to_string(), None => e.split_whitespace().next()?.to_string() };
            (path != exe && std::path::Path::new(&path).exists()).then_some(path)
        });
        if let Some(p) = other {
            echo_core::recorder::log_line(&format!("autostart: left alone - it starts another Echo ({p})"));
            return;
        }
        if on {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(&f, format!("[Desktop Entry]\nType=Application\nName=Echo\nExec=\"{exe}\" --autostart\nIcon=link.echoclip.Echo\nX-GNOME-Autostart-enabled=true\n"));
        } else {
            let _ = std::fs::remove_file(&f);
        }
    }
}

// ---------- settings.json changed (the Settings page writes it key by key) ----------
fn settings_mtime() -> Option<SystemTime> {
    std::fs::metadata(Settings::path()).and_then(|m| m.modified()).ok()
}
fn settings_changed(rec: &Recorder) -> bool {
    // gaps-settings: the Settings page applies at once too (apply_settings_now) - one at a time with the watch loop
    static APPLYING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = APPLYING.lock().unwrap_or_else(|e| e.into_inner());
    let new = Settings::load();
    register_hotkeys(&new);
    echo_core::pad::set_combo(new.pad_buttons);
    autostart(new.start_with_windows);
    SYNC_SOON.store(true, Ordering::Relaxed);
    let restart = rec.apply(new);
    if restart {
        recorder::log_line("settings changed - restarting the recorder");
        rec.stop();
        start_rec(rec);
    }
    push(Ev::Settings);
    restart
}
/// gaps-settings (Echo's set_settings): the Settings page wrote settings.json - take it now, not on the watch loop's
/// next look (new hotkeys work at once). true = the recorder restarts. Worker thread (a restart takes a moment);
/// only the Echo that records.
pub fn apply_settings_now() -> bool {
    rec_for_recording().is_some_and(settings_changed)
}

// ---------- self-healing watch loop (Echo's main.rs, Windows + Linux in one) ----------
fn watch_loop() {
    let Some(rec) = rec() else { return };
    let mut game: Option<(String, u32)> = None;
    #[cfg(windows)]
    let (mut game_hwnd, mut vr_hwnd, mut want_seen, mut admin_warned): (isize, isize, (isize, u32), std::collections::HashSet<String>) = (0, 0, (0, 0), Default::default());
    let mut down = 0u32;
    let mut last_fix = Instant::now() - Duration::from_secs(120);
    let mut mtime = settings_mtime();
    // Linux: games seen before without an icon get one (background, once)
    recorder::backfill_game_icons();
    loop {
        std::thread::sleep(Duration::from_secs(3));
        let m = settings_mtime();
        if m != mtime {
            mtime = m;
            settings_changed(rec);
        }
        let cfg = rec.settings();
        #[cfg(windows)]
        {
            use echo_core::winutil;
            if let Some((name, pid, exe)) = recorder::game_in_front() {
                if game.as_ref().map(|g| g.1) != Some(pid) {
                    recorder::remember_game(&name, &exe);
                }
                *rec.last_game.lock().unwrap() = Some((name.clone(), Instant::now()));
                if echo_core::hotkey::key_age() > 60 && winutil::idle_seconds() < 10 && !admin_warned.contains(&name) && winutil::elevated_vs_echo(pid) {
                    admin_warned.insert(name.clone());
                    recorder::log_line(&format!("hotkey: {name} runs as administrator - its keys don't reach Echo"));
                    toast("Save keys can't reach this game", &format!("{name} runs as administrator. Start Echo as administrator too, or the game normally."), false);
                }
                game = Some((name, pid));
                game_hwnd = winutil::foreground_hwnd();
            }
            match winutil::vr_game().and_then(|(pid, hwnd, stem, title)| recorder::vr_game_name(&stem, &title).map(|n| (n, pid, hwnd))) {
                Some((name, pid, hwnd)) => {
                    if hwnd != vr_hwnd {
                        recorder::log_line(&format!("vr: {name} is a VR game - recording its window"));
                        recorder::remember_game(&name, &winutil::exe_path(pid).unwrap_or_default());
                    }
                    vr_hwnd = hwnd;
                    if game.as_ref().map_or(true, |g| g.1 == pid || !winutil::pid_alive(g.1)) {
                        game = Some((name, pid));
                        game_hwnd = hwnd;
                    }
                }
                None => {
                    if vr_hwnd != 0 {
                        recorder::log_line("vr: VR game closed");
                    }
                    vr_hwnd = 0;
                }
            }
            recorder::VR_WINDOW.store(vr_hwnd, Ordering::Relaxed);
            if game.as_ref().is_some_and(|g| !winutil::pid_alive(g.1)) {
                game = None;
                SYNC_SOON.store(true, Ordering::Relaxed);
            }
            if game.is_none() || (game_hwnd != 0 && !winutil::window_alive(game_hwnd)) {
                game_hwnd = 0;
            }
        }
        #[cfg(target_os = "linux")]
        {
            use echo_core::linuxutil;
            if game.as_ref().map_or(true, |g| !linuxutil::pid_alive(g.1)) {
                if game.take().is_some() {
                    SYNC_SOON.store(true, Ordering::Relaxed);
                }
                if let Some((name, pid, exe)) = recorder::game_in_front() {
                    recorder::log_line(&format!("game: {name} (pid {pid})"));
                    recorder::remember_game_pid(&name, &exe, pid);
                    game = Some((name, pid));
                }
            }
        }
        if let Some(g) = &game {
            *rec.last_game.lock().unwrap() = Some((g.0.clone(), Instant::now()));
        }
        #[cfg(windows)]
        let idle_now = echo_core::winutil::idle_seconds;
        #[cfg(target_os = "linux")]
        let idle_now = echo_core::linuxutil::idle_seconds;
        #[cfg(not(any(windows, target_os = "linux")))]
        let idle_now = || 0u64;
        let idle = if cfg.idle_pause_min > 0 { idle_now() } else { 0 };
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
        if cur.is_empty() && !rec.busy() && rec.session_secs().is_none() && recorder::RESTART_REC.swap(false, Ordering::Relaxed) {
            recorder::log_line("restarting the recorder (clock jump)");
            start_rec(rec);
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
                start_rec(rec);
            } else {
                rec.set_pause(reason);
            }
            down = 0;
            continue;
        }
        // "Game window only" / VR window; launcher-size windows (FiveM's 524x524) never count; switch only when the
        // same target was asked for twice in a row (~6 s) - each switch empties the replay buffer
        #[cfg(windows)]
        {
            use echo_core::winutil;
            let mut want = if cfg.capture == "game" { game_hwnd } else if vr_hwnd != 0 && !winutil::window_alive(vr_hwnd) { 0 } else { vr_hwnd };
            if want != 0 && want != vr_hwnd {
                let (ww, wh) = winutil::window_size(want);
                if ww < 800 || wh < 450 {
                    want = 0;
                }
            }
            want_seen = if want_seen.0 == want { (want, want_seen.1 + 1) } else { (want, 1) };
            if cur.is_empty() && want != rec.window.load(Ordering::Relaxed) && want_seen.1 >= 2 && rec.session_secs().is_none() && !rec.busy() && !FPS_TESTING.load(Ordering::Relaxed) {
                rec.window.store(want, Ordering::Relaxed);
                recorder::log_line(if want != 0 { "capture: switching to the game window" } else { "capture: back to the whole monitor" });
                start_rec(rec);
                down = 0;
                continue;
            }
        }
        // running but broken (stuck, or an encoder inside ffmpeg died): restart, once a minute at most
        if cur.is_empty() && !rec.busy() && rec.session_secs().is_none() && !FPS_TESTING.load(Ordering::Relaxed) && last_fix.elapsed().as_secs() > 60 {
            if let Some(why) = rec.health() {
                last_fix = Instant::now();
                recorder::log_line(&format!("recorder unhealthy ({why}) - restarting it"));
                start_rec(rec);
                down = 0;
                continue;
            }
        }
        // watchdog: should be recording but isn't
        let started_once = !rec.mode.lock().unwrap().is_empty();
        if cur.is_empty() && started_once && !rec.busy() && !rec.is_recording() && !FPS_TESTING.load(Ordering::Relaxed) {
            down += 1;
        } else {
            down = 0;
        }
        if down >= 2 {
            down = 0;
            recorder::log_line("recorder stopped unexpectedly");
            recorder::play_sound(Sound::Stopped, &cfg, false);
            if last_fix.elapsed().as_secs() > 60 {
                last_fix = Instant::now();
                toast("Recorder stopped", "Restarting it…", false);
                start_rec(rec);
            } else {
                toast("Recorder stopped", "Couldn't keep it running - open Echo", false);
            }
        }
    }
}

/// Echo's other background loops: cloud sync (5 min / soon after a save), cleanup of old clips (6 h), shrink old
/// clips while idle (10 min, off by default).
fn background_loops() {
    std::thread::spawn(|| {
        let mut last = Instant::now();
        loop {
            std::thread::sleep(Duration::from_secs(10));
            let Some(rec) = rec() else { return };
            rec.stop_idle_serve(600);
            // faster-share-links: the smaller copies of share-first uploads (Echo Cloud), on a thread of their own
            if rec.swaps_waiting() { std::thread::spawn(move || rec.run_swaps()); }
            let soon = SYNC_SOON.swap(false, Ordering::Relaxed);
            if !soon && last.elapsed().as_secs() < 300 {
                continue;
            }
            last = Instant::now();
            let _ = rec.cloud_sync(); // the library follows the disk; cloud.json changes show with Chat B's cloud code
        }
    });
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_secs(60));
        let Some(rec) = rec() else { return };
        rec.remove_stale_tmp();
        let days = rec.settings().cleanup_days;
        if days > 0 {
            rec.cleanup_old(days);
        }
        std::thread::sleep(Duration::from_secs(6 * 3600));
    });
    #[cfg(any(windows, target_os = "linux"))]
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_secs(600));
        let Some(rec) = rec() else { return };
        let c = rec.settings();
        #[cfg(windows)]
        let idle = echo_core::winutil::idle_seconds;
        #[cfg(target_os = "linux")]
        let idle = echo_core::linuxutil::idle_seconds;
        let calm = || idle() >= 300 && recorder::detect_game().is_none() && rec.session_secs().is_none();
        if c.shrink_old && calm() {
            rec.shrink_old_clips(c.shrink_days, calm);
        }
        // faster-share-links: once a day while idle - Echo Cloud files that are still the clip itself get its smaller copy
        if calm() && rec.heal_due() {
            rec.heal_originals(&calm);
        }
    });
}

/// "Upload when the game closes" etc: ask the cloud loop to sync soon (for Chat B's cloud code).
pub fn sync_soon() {
    SYNC_SOON.store(true, Ordering::Relaxed);
}

// ---------- small bridges into main.rs's window (UI thread) ----------
/// Settings changed on disk: the header's hotkey chips follow.
pub fn settings_hook(a: &AppWindow) {
    use slint::{ModelRc, VecModel};
    a.set_hotkeys(ModelRc::new(VecModel::from(crate::hotkey_pills(crate::library::echo_settings().as_ref()))));
    crate::copy::fill_menu(a);
}
/// Hiding into the tray: an open clip is closed first (mpv lets go, memory goes down); the editor stays as it is.
pub fn hide_hook(a: &AppWindow) {
    use slint::ComponentHandle as _;
    if a.get_player_open() && !a.global::<crate::Ed>().get_open() {
        a.global::<crate::Play>().invoke_close();
    }
}
/// the theme's accent colours (popup icon gradient, like Echo's toast.html)
pub fn accent() -> Option<(slint::Color, slint::Color)> {
    crate::UI.with(|u| u.borrow().as_ref().and_then(|w| w.upgrade())).map(|a| {
        let t = a.global::<crate::T>();
        (t.get_accent(), t.get_accent2())
    })
}

/// L14: one montage at a time (a double click made two)
static COMBINING: AtomicBool = AtomicBool::new(false);

/// Selection bar "Combine": Echo's montage (one clip from the picked ones, in time order) with a progress box.
pub fn combine_ui(a: &AppWindow, paths: Vec<String>) {
    let Some(r) = rec() else { return };
    if paths.len() < 2 {
        crate::toast(a, "Select at least 2 clips to combine");
        return;
    }
    if COMBINING.swap(true, Ordering::Relaxed) {
        return;
    }
    a.set_combine_busy(true); // L15: disabled look while it runs
    let id = crate::prog_box(&format!("Combining {} clips", paths.len()));
    crate::prog_set(id, None, Some("Making the montage…"), None);
    std::thread::spawn(move || {
        let res = r.combine_clips(&paths);
        if let Ok(p) = &res {
            let _ = r.thumb(p); // picture + length for the new card, like Echo
        }
        let _ = slint::invoke_from_event_loop(move || {
            crate::prog_done(id, 0);
            COMBINING.store(false, Ordering::Relaxed);
            if let Some(a) = crate::UI.with(|u| u.borrow().as_ref().and_then(|w| w.upgrade())) {
                a.set_combine_busy(false);
                match res {
                    Ok(p) => crate::toast(&a, format!("Montage saved - {}", std::path::Path::new(&p).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default())),
                    Err(e) => crate::toast(&a, format!("Couldn't combine: {e}")),
                }
            }
        });
    });
}
