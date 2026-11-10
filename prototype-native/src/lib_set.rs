//! Chat B: Settings actions made real (item 8) + the background storage jobs Echo runs:
//! - folder pickers: clips folder ("Choose…") and Import ("Choose folder…") - no more pasted paths
//! - Clean up now (Echo's cleanup_old on the library model; the 6-hourly one runs in rec_core)
//! - storage limit: picking a smaller limit removes the oldest clips right away (the recorder enforces it after
//!   every save, like Echo)
//! - sound previews + your own .wav per sound (Choose / Reset)
//! - Sort clips into game folders, Home tips again, Send feedback, Send log (Echo's server, lib_net).
use crate::settings::Store;
use crate::AppWindow;
use serde_json::{json, Value};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

type Refresh = Rc<dyn Fn()>;

fn bin() -> &'static str {
    if cfg!(windows) {
        "Recycle Bin"
    } else {
        "Trash"
    }
}

fn lib() -> Option<Rc<crate::library::LibModel>> {
    crate::LIB.with(|l| l.borrow().clone())
}

/// after clips went away / moved: the library, Home and sidebar follow
fn lib_changed(a: &AppWindow) {
    if let Some(l) = lib() {
        l.refresh();
        crate::home_fill(a, &l);
    }
    crate::UI_SYNC.with(|f| {
        if let Some(f) = f.borrow().as_ref() {
            f(true)
        }
    });
}

thread_local! {
    /// FPS test: (running, line under the row)
    static FPS: RefCell<(bool, String)> = const { RefCell::new((false, String::new())) };
    static FPS_TIMER: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
    /// controller "Set…" waiting for the combo
    static PAD_BUSY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
/// for settings.rs: (running, progress / result line) of the FPS test
pub fn fps_state() -> (bool, String) {
    FPS.with(|f| f.borrow().clone())
}
/// for settings.rs: the controller's "Set…" is waiting for buttons
pub fn pad_busy() -> bool {
    PAD_BUSY.with(|b| b.get())
}

pub fn act(a: &AppWindow, store: &Rc<RefCell<Store>>, r: &Refresh, act: &str) -> bool {
    match act {
        "cleannow" => {
            let days = store.borrow().n("cleanup_days") as u64;
            if days == 0 {
                crate::toast(a, "Pick when clips should go first (Auto-delete old clips)");
                return true;
            }
            crate::confirm_box(a, "Clean up now?", &format!("Clips older than {days} days go to the {}. Favourites are kept.", bin()), "Clean up", move |a| {
                let w = a.as_weak();
                cleanup(days, move |n| {
                    let Some(a) = w.upgrade() else { return };
                    lib_changed(&a);
                    crate::toast(&a, if n == 0 { "Nothing to clean up".to_string() } else { format!("{n} old clip{} moved to the {}", if n == 1 { "" } else { "s" }, bin()) });
                });
            });
        }
        "import" => {
            let clips = PathBuf::from(store.borrow().s("clips_dir"));
            let w = a.as_weak();
            crate::lib_pick::pick_folder_ui("Folder with your old clips", move |p| {
                if let Some(a) = w.upgrade() {
                    crate::lib_import::start(&a, p, clips);
                }
            });
        }
        "pickclips" => {
            let (w, store, r) = (a.as_weak(), store.clone(), r.clone());
            crate::lib_pick::pick_folder_ui("Clips folder", move |p| {
                let Some(a) = w.upgrade() else { return };
                set_clips_dir(&a, &store, &r, &p.to_string_lossy());
            });
        }
        "sortclips" => {
            // r2-threads (L13): the files move on a worker
            let w = a.as_weak();
            sort_into_folders(&store.borrow(), move |n| {
                let Some(a) = w.upgrade() else { return };
                lib_changed(&a);
                crate::toast(&a, if n == 0 { "Nothing to sort".to_string() } else { format!("Moved {n} clip{} into game folders", if n == 1 { "" } else { "s" }) });
            });
        }
        "hometips" => {
            // Echo's "Show again": skips + hide forgotten (the "copied once" mark stays, like Echo's echo_copied)
            crate::lib_fx::ui_set("gs_skip", serde_json::Value::Null);
            crate::lib_fx::ui_set("gs_hide", serde_json::Value::Null);
            crate::rec_core::set_acct_hidden(false); // and the sidebar's Echo Profile row (Echo: echo_acct_hide)
            crate::GS.with(|g| *g.borrow_mut() = (Default::default(), false, true));
            let _ = store.borrow_mut().write(&[("home_tips_hidden", json!(false))]);
            if let Some(l) = lib() {
                crate::home_fill(a, &l);
            }
            crate::toast(a, "The Get started line is back on Home");
        }
        "feedback" => feedback(a, &store.borrow().s("ec_email")),
        "sendlog" => send_log(a, &store.borrow()),
        "selftest" => self_test(a, r),
        // gaps-settings: Echo's themeReset - back to the normal theme, own colours cleared (quietly, like Echo)
        "themereset" => {
            let e = || json!("");
            match store.borrow_mut().write(&[("theme", e()), ("theme_accent", e()), ("theme_bg", e()), ("theme_hi", e())]) {
                Ok(()) => changed(true),
                Err(e) => crate::toast(a, e),
            }
            r();
        }
        // gaps-settings: Echo's drawCloudLock button
        "cloudlock" => {
            let (store, r) = (store.clone(), r.clone());
            crate::confirm_box(
                a,
                "Start over with the cloud?",
                "Echo signs out of every cloud and forgets the locked logins. Then connect Google Drive / Echo Cloud again - nothing is deleted from the clouds.",
                "Start over",
                move |a| {
                    let Some(rc) = crate::rec_core::rec() else { return };
                    let w = a.as_weak();
                    crate::lib_net::bg(
                        move || rc.cloud_disconnect(),
                        move |_| {
                            echo_core::recorder::CLOUD_LOCKED.store(false, std::sync::atomic::Ordering::Relaxed);
                            store.borrow_mut().reload();
                            r();
                            if let Some(a) = w.upgrade() {
                                crate::cloud::load_info(&a);
                                crate::toast(&a, "Signed out - connect your clouds again");
                            }
                        },
                    );
                },
            );
        }
        // gaps-settings: Echo's [Move to FiveM] on the GTA V game row
        "fivem" => {
            let ids = gta_clips();
            if ids.is_empty() {
                return true;
            }
            let n = ids.len();
            let r = r.clone();
            crate::confirm_box(
                a,
                &format!("Move {n} GTA V clip{} to FiveM?", if n == 1 { "" } else { "s" }),
                "For FiveM clips that were filed as GTA V (other apps call FiveM \"Grand Theft Auto V Legacy\"). Clips you make in GTA V itself later still go to GTA V.",
                "Move",
                move |a| {
                    let Some(l) = lib() else { return };
                    // r2-threads (L13): the files move on a worker
                    let w = a.as_weak();
                    l.move_clips_bg(&ids, "FiveM", move |moved, err| {
                        let Some(a) = w.upgrade() else { return };
                        let a = &a;
                        lib_changed(a);
                        r();
                        match err {
                            Some(e) => crate::toast(a, e),
                            None => crate::toast(a, format!("Moved {moved} clip{} to FiveM", if moved == 1 { "" } else { "s" })),
                        }
                    });
                },
            );
        }
        // gaps-settings: setup step 7 "Remind me later" (Echo's echo_impremind: 3 days, then once while Echo is open)
        "impremind" => {
            if !import_reminder_set() {
                let due = chrono::Utc::now().timestamp_millis() + 3 * 86_400_000;
                crate::lib_fx::ui_set("impremind", json!(due));
            }
            r();
        }
        _ if act.starts_with("sound:") => {
            crate::lib_sound::play(&act[6..], &store.borrow().v, true);
        }
        // Echo's restart(): stop + start the recorder (same as double-clicking the header pill)
        "restartrec" => {
            crate::toast(a, "Restarting recorder…");
            a.invoke_rec_act("restart".into());
        }
        // Echo's padSet: hold the combo on the controller, let go -> pad_buttons (at least 2 buttons)
        "padset" => {
            if PAD_BUSY.with(|b| b.replace(true)) {
                return true;
            }
            r();
            crate::toast(a, "Hold the buttons together on your controller, then let go");
            let (w, store, r) = (a.as_weak(), store.clone(), r.clone());
            crate::lib_net::bg(
                || {
                    let b = echo_core::pad::capture();
                    (b, echo_core::pad::names(b))
                },
                move |(bits, names)| {
                    PAD_BUSY.with(|b| b.set(false));
                    let Some(a) = w.upgrade() else { return };
                    if bits == 0 {
                        crate::toast(&a, "No controller buttons pressed (is an Xbox-style controller connected?)");
                    } else if bits & (bits - 1) == 0 {
                        crate::toast(&a, "Use at least 2 buttons together, so you don't save by accident");
                    } else {
                        let _ = store.borrow_mut().write(&[("pad_buttons", json!(bits))]);
                        changed(true); // gaps-settings: the combo works at once
                        crate::toast(&a, format!("Controller: {names} saves a clip"));
                    }
                    r();
                },
            );
        }
        // Echo's fpsTest: progress lines under the row, "✓ result" / "✗ error" at the end
        "fpstest" => {
            if FPS.with(|f| f.borrow().0) {
                return true;
            }
            FPS.with(|f| *f.borrow_mut() = (true, "Starting…".into()));
            r();
            let line = std::sync::Arc::new(std::sync::Mutex::new(String::from("Starting…")));
            let (l2, w, r2) = (line.clone(), a.as_weak(), r.clone());
            // the page shows the newest line twice a second
            let t = slint::Timer::default();
            t.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(500), move || {
                let now = l2.lock().map(|s| s.clone()).unwrap_or_default();
                if FPS.with(|f| f.borrow().0 && f.borrow().1 != now) {
                    FPS.with(|f| f.borrow_mut().1 = now);
                    r2();
                }
            });
            FPS_TIMER.with(|x| *x.borrow_mut() = Some(t));
            let r3 = r.clone();
            crate::lib_net::bg(
                move || {
                    let l = line.clone();
                    crate::rec_core::fps_test(move |s| {
                        if let Ok(mut g) = l.lock() {
                            *g = s;
                        }
                    })
                },
                move |res| {
                    FPS_TIMER.with(|x| *x.borrow_mut() = None);
                    FPS.with(|f| *f.borrow_mut() = (false, match res { Ok(s) => format!("✓ {s}"), Err(e) => format!("✗ {e}") }));
                    let _ = w.upgrade();
                    r3();
                },
            );
        }
        // Echo's #sndfiles: "Choose .wav" / "Change" pick a file at once, "Reset" = built-in again (no dialog)
        _ if act.starts_with("soundreset:") => {
            set_sound(store, &act[11..], None);
            r();
        }
        _ if act.starts_with("soundpick:") => {
            let (w, store, r, k) = (a.as_weak(), store.clone(), r.clone(), act[10..].to_string());
            crate::lib_pick::pick_file_ui("Pick a sound", "Sounds (.wav)", &["wav"], move |p| {
                let Some(a) = w.upgrade() else { return };
                set_sound(&store, &k, Some(&p.to_string_lossy()));
                r();
                crate::lib_sound::play(&k, &store.borrow().v, true);
                crate::toast(&a, "Sound changed");
            });
        }
        _ if act.starts_with("soundfile:") => {
            let k = act[10..].to_string();
            let has = store.borrow().v.get("sound_files").and_then(|m| m.get(&k)).and_then(|f| f.as_str()).is_some_and(|f| !f.is_empty());
            let pick = {
                let (w, store, r, k) = (a.as_weak(), store.clone(), r.clone(), k.clone());
                move || {
                    crate::lib_pick::pick_file_ui("Pick a sound", "Sounds (.wav)", &["wav"], move |p| {
                        let Some(a) = w.upgrade() else { return };
                        set_sound(&store, &k, Some(&p.to_string_lossy()));
                        r();
                        crate::lib_sound::play(&k, &store.borrow().v, true);
                        crate::toast(&a, "Sound changed");
                    });
                }
            };
            if has {
                let (store, r) = (store.clone(), r.clone());
                crate::ask(a, "Your own sound", "Pick another .wav file, or go back to Echo's built-in sound.", &[("Cancel", "", ""), ("Built-in", "reset", ""), ("Choose .wav", "pick", "primary")], None, "pick", move |_a, v, _| {
                    if v == "pick" {
                        pick();
                    } else if v == "reset" {
                        set_sound(&store, &k, None);
                        r();
                    }
                });
            } else {
                pick();
            }
        }
        _ => return crate::cloud::act(a, store, r, act) || crate::setup::act(a, store, r, act),
    }
    true
}

/// after a value changed on the page (main.rs on_set calls this)
pub fn after_set(a: &AppWindow, store: &Rc<RefCell<Store>>, key: &str) {
    match key {
        "clips_dir" => restart_offer(a),
        "max_storage_gb" => {
            let gb = store.borrow().n("max_storage_gb") as u64;
            if gb > 0 {
                let (_, total) = over_limit(gb);
                if total > gb * 1_000_000_000 {
                    crate::toast(a, format!("Your clips use {} - the oldest go to the {} until they fit", crate::library::fmt_size(total), bin()));
                    let w = a.as_weak();
                    slint::Timer::single_shot(Duration::from_secs(4), move || {
                        if let Some(a) = w.upgrade() {
                            let w2 = a.as_weak();
                            enforce_storage(gb, move |n| {
                                if let Some(a) = w2.upgrade().filter(|_| n > 0) {
                                    lib_changed(&a);
                                }
                            });
                        }
                    });
                }
            }
        }
        _ => {}
    }
}

fn set_clips_dir(a: &AppWindow, store: &Rc<RefCell<Store>>, r: &Refresh, p: &str) {
    let msg = store.borrow_mut().set("clips_dir", p);
    r();
    match msg {
        Some(m) => crate::toast(a, m),
        None => restart_offer(a),
    }
}

/// the library reads the clips folder at start: offer a restart so it shows the new one
fn restart_offer(a: &AppWindow) {
    crate::ask(a, "Clips folder changed", "New clips are saved there from now on. Restart Echo to show the clips in that folder?", &[("Later", "", ""), ("Restart now", "go", "primary")], None, "go", |_a, v, _| {
        if v == "go" {
            restart_app();
        }
    });
}

pub fn restart_app() {
    if let Ok(exe) = std::env::current_exe() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if std::process::Command::new(exe).args(args).spawn().is_ok() {
            let _ = slint::quit_event_loop();
            std::process::exit(0);
        }
    }
}

fn set_sound(store: &Rc<RefCell<Store>>, k: &str, file: Option<&str>) {
    let mut m = store.borrow().v.get("sound_files").and_then(|x| x.as_object()).cloned().unwrap_or_default();
    match file {
        Some(f) => {
            m.insert(k.to_string(), json!(f));
        }
        None => {
            m.remove(k);
        }
    }
    if store.borrow_mut().write(&[("sound_files", Value::Object(m))]).is_ok() {
        changed(true); // gaps-settings: the recorder plays the new sound from the next save on
    }
}

// ---------- storage jobs ----------
fn favs() -> std::collections::HashSet<String> {
    lib().map(|l| l.favs.borrow().clone()).unwrap_or_default()
}

/// Echo's cleanup_old: clips older than `days` (file time) to the Recycle Bin / Trash, favourites kept
/// r2-threads (L5): the files go on a worker; `done(n)` on the UI thread
pub fn cleanup(days: u64, done: impl FnOnce(usize) + 'static) {
    let Some(l) = lib() else { return done(0) };
    let cutoff = SystemTime::now() - Duration::from_secs(days * 86400);
    let favs = favs();
    let ids: Vec<usize> = (0..l.len()).filter(|&i| {
        let c = l.clip(i);
        !c.gone.get() && c.mtime < cutoff && !favs.contains(&c.name) && !c.folder.eq_ignore_ascii_case("Screenshots")
    }).collect();
    if ids.is_empty() {
        return done(0);
    }
    l.delete_clips_bg(&ids, |_, _| {}, move |n, _| {
        crate::lib_net::log_line(&format!("cleanup: {n} clips older than {days} days sent to the {}", bin()));
        done(n);
    });
}

/// (clip ids oldest first, total bytes) of the library
fn over_limit(_gb: u64) -> (Vec<usize>, u64) {
    let Some(l) = lib() else { return (vec![], 0) };
    let mut ids: Vec<usize> = (0..l.len()).filter(|&i| !l.clip(i).gone.get()).collect();
    ids.sort_by_key(|&i| l.clip(i).mtime);
    let total = ids.iter().map(|&i| l.clip(i).size).sum();
    (ids, total)
}

/// Echo's enforce_storage: the oldest clips go while the folder is over the limit (the newest always stays)
/// r2-threads (L5): the files go on a worker; `done(n)` on the UI thread
pub fn enforce_storage(gb: u64, done: impl FnOnce(usize) + 'static) {
    if gb == 0 {
        return done(0);
    }
    let Some(l) = lib() else { return done(0) };
    let limit = gb * 1_000_000_000;
    let (ids, mut total) = over_limit(gb);
    let mut del = vec![];
    for &i in ids.iter().take(ids.len().saturating_sub(1)) {
        if total <= limit {
            break;
        }
        total = total.saturating_sub(l.clip(i).size);
        del.push(i);
    }
    if del.is_empty() {
        return done(0);
    }
    l.delete_clips_bg(&del, |_, _| {}, move |n, _| {
        crate::lib_net::log_line(&format!("storage: {n} oldest clips removed (limit {gb} GB)"));
        done(n);
    });
}

/// Echo's sort_into_folders: clips in the main folder / "Exported Clips"… with a game in their name move into
/// that game's folder (folder_alias followed)
fn sort_into_folders(st: &Store, done: impl FnOnce(usize) + 'static) {
    const LOOSE: &[&str] = &["", "exported clips", "shared clips", "clips", "videos", "highlights", "exports"];
    let Some(l) = lib() else { return done(0) };
    let alias = st.v.get("folder_alias").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let not: Vec<String> = st.v.get("not_games").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_lowercase())).collect()).unwrap_or_default();
    let mut by: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
    for i in 0..l.len() {
        let c = l.clip(i);
        if c.gone.get() || !LOOSE.contains(&c.folder.to_lowercase().as_str()) || c.game.is_empty() || not.contains(&c.game.to_lowercase()) {
            continue;
        }
        let g = alias.iter().find(|(k, _)| k.eq_ignore_ascii_case(&c.game)).and_then(|(_, v)| v.as_str()).unwrap_or(&c.game).to_string();
        let g = crate::library::clean_folder(&g);
        if g.is_empty() || g.eq_ignore_ascii_case(&c.folder) {
            continue;
        }
        by.entry(g).or_default().push(i);
    }
    // r2-threads (L13): one worker for all the game folders
    l.move_many_bg(by.into_iter().map(|(g, ids)| (ids, g)).collect(), move |n, _| done(n));
}

/// Background jobs (auto-delete every 6 h, shrink old clips while idle, start with Windows) run in Chat A's
/// rec_core.rs with Echo's own recorder.rs code (cleanup_old / shrink_old_clips / autostart) - not here.
/// gaps-settings: the window for the "Settings saved" toasts + the import reminder (main.rs calls this twice: once)
pub fn wire(app: &AppWindow, _store: Rc<RefCell<Store>>) {
    if APP.with(|x| x.borrow().is_some()) {
        return;
    }
    APP.with(|x| *x.borrow_mut() = Some(app.as_weak()));
    // Echo's "Remind me later" (setup step 7): 60 s after a start once it's due, a question while Echo is open
    let w = app.as_weak();
    slint::Timer::single_shot(Duration::from_secs(60), move || {
        let Some(a) = w.upgrade() else { return };
        let due = crate::lib_fx::ui_get("impremind").as_i64().unwrap_or(0);
        if due == 0 || chrono::Utc::now().timestamp_millis() < due || !a.window().is_visible() {
            return;
        }
        crate::lib_fx::ui_set("impremind", Value::Null);
        crate::ask(
            &a,
            "Bring in your old clips?",
            "You asked Echo to remind you: clips from Medal, SteelSeries Moments, Outplayed or any folder can be copied in, with their dates and game folders.",
            &[("Not now", "", ""), ("Show me", "go", "primary")],
            None,
            "go",
            |a, v, _| {
                if v == "go" {
                    a.invoke_page("Settings".into());
                    a.global::<crate::S>().invoke_pick("storage".into());
                }
            },
        );
    });
}

thread_local! {
    static APP: RefCell<Option<slint::Weak<AppWindow>>> = const { RefCell::new(None) };
    /// Echo's changed(): 450 ms after the last change, one apply + one toast
    static SAVE_TIMER: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
    /// a save is waiting: Some(quiet) - quiet = no toast unless the recorder restarts
    static SAVE_QUIET: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    /// self-test: running, last result (rows of (name, ok, detail) / error)
    static SELFTEST: RefCell<(bool, Option<Result<Vec<(String, bool, String)>, String>>)> = const { RefCell::new((false, None)) };
}

/// gaps-settings (Echo's changed()): a setting was written - the recorder takes it at once (new hotkeys work right
/// away, no wait for its 3 s look at the file) and a toast says "Settings saved" or "Saved · recorder restarting with
/// new settings". Changes within 450 ms are one save (sliders). `quiet` = no toast unless the recorder restarts.
pub fn changed(quiet: bool) {
    SAVE_QUIET.with(|q| q.set(Some(q.get().map_or(quiet, |p| p && quiet))));
    SAVE_TIMER.with(|t| {
        // the same timer, started again: changes within 450 ms are one save
        t.borrow_mut().get_or_insert_with(slint::Timer::default).start(slint::TimerMode::SingleShot, Duration::from_millis(450), || {
            let quiet = SAVE_QUIET.with(|q| q.take()).unwrap_or(true);
            crate::lib_net::bg(crate::rec_core::apply_settings_now, move |restarted| {
                let Some(a) = APP.with(|x| x.borrow().as_ref().and_then(|w| w.upgrade())) else { return };
                if restarted {
                    crate::toast(&a, "Saved · recorder restarting with new settings");
                } else if !quiet {
                    crate::toast(&a, "Settings saved");
                }
            });
        });
    });
}

/// gaps-settings: this PC's graphics card encodes AV1 (lib_cards probes Echo's av1_ready 15 s after start)
pub fn av1_ok() -> bool {
    APP.with(|x| x.borrow().as_ref().and_then(|w| w.upgrade())).is_some_and(|a| a.global::<crate::Lib>().get_av1_ok())
}

/// gaps-settings: the clips filed as GTA V on this PC (Echo's Move to FiveM)
pub fn gta_clips() -> Vec<usize> {
    let Some(l) = lib() else { return vec![] };
    (0..l.len())
        .filter(|&i| {
            let c = l.clip(i);
            !c.gone.get() && c.cloud.is_none() && c.folder == "GTA V"
        })
        .collect()
}

/// gaps-settings: "Remind me later" was clicked and the reminder hasn't come yet
pub fn import_reminder_set() -> bool {
    crate::lib_fx::ui_get("impremind").as_i64().is_some_and(|d| d > chrono::Utc::now().timestamp_millis())
}

/// for settings.rs: (running, last result) of the self-test
pub fn self_test_state() -> (bool, Option<Result<Vec<(String, bool, String)>, String>>) {
    SELFTEST.with(|s| s.borrow().clone())
}

// ---------- feedback + log ----------
pub fn app_version() -> String {
    option_env!("ECHO_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")).to_string()
}

/// gaps-settings: Echo's sendFeedback - message (several lines), "Your email, if you'd like an answer (optional)"
/// (filled with the Echo Cloud email) and "Attach Echo's log…" (on). dialog.slint's form fields.
fn feedback(a: &AppWindow, email: &str) {
    crate::ask(
        a,
        "Send feedback",
        "What happened, or what would make Echo better?",
        &[("Cancel", "", ""), ("Send", "ok", "primary")],
        Some("e.g. the share link didn't work for a clip from Google Drive…"),
        // L45: Enter is never Send here (Echo's textarea: Enter = new line; the email field: nothing)
        "",
        |a, v, text| {
            let d = a.global::<crate::Dlg>();
            d.set_form(false);
            let (contact, with_log) = (d.get_text2().trim().to_string(), d.get_checked());
            if v != "ok" {
                return;
            }
            let msg = text.trim().to_string();
            if msg.is_empty() {
                // L45: Echo keeps the form open on an empty Send (focus back in the message); the email and the
                // log box stay as they were
                let raw = d.get_text2().to_string();
                feedback(a, &raw);
                a.global::<crate::Dlg>().set_checked(with_log);
                return;
            }
            let tail = if with_log { crate::lib_net::log_tail(400, 150_000) } else { String::new() };
            let body = json!({ "message": msg.chars().take(4000).collect::<String>(), "contact": contact.chars().take(200).collect::<String>(), "version": app_version(),
                "os": if cfg!(windows) { "Windows" } else { "Linux" }, "log": tail });
            let token = crate::cloud::token();
            crate::lib_net::call_ui(a, "POST", "/feedback", token, Some(body), |a, r| match r {
                Ok(_) => crate::toast(a, "Thanks - your feedback was sent"),
                Err(e) => crate::toast(a, e),
            });
        },
    );
    let d = a.global::<crate::Dlg>();
    d.set_form(true);
    d.set_placeholder2("Your email, if you'd like an answer (optional)".into());
    d.set_text2(email.into());
    d.set_checked(true);
    d.set_check_text("Attach Echo's log (the last few hundred lines: what Echo did, clip and file names, no passwords or keys)".into());
}

fn send_log(a: &AppWindow, st: &Store) {
    // audit (9 Nov): Echo's own send_log - support_file (recorder log + settings, secrets left out) + the native UI's
    // proto.log tail, upload_log, and Echo's fallback when the upload fails: the FILE goes on the clipboard instead
    if let Some(r) = crate::rec_core::rec() {
        crate::toast(a, "Sending the log…");
        let w = a.as_weak();
        crate::lib_net::bg(
            move || -> Result<String, String> {
                let f = r.support_file()?;
                let native = crate::lib_net::redact_emails(&crate::lib_net::log_tail(400, 120_000));
                if let Ok(mut fh) = std::fs::OpenOptions::new().append(true).open(&f) {
                    use std::io::Write;
                    let _ = write!(fh, "\n--- native app log (Echo {} native) ---\n{native}\n", app_version());
                }
                match r.upload_log(&f) {
                    Ok(url) => {
                        echo_core::recorder::copy_text(&url)?;
                        Ok(url)
                    }
                    Err(e) => {
                        echo_core::recorder::log_line(&format!("log: upload failed ({e}) - copying the file"));
                        echo_core::recorder::copy_to_clipboard(&f)?;
                        Ok(String::new())
                    }
                }
            },
            move |res| {
                let Some(a) = w.upgrade() else { return };
                match res {
                    Ok(u) if !u.is_empty() => crate::toast(&a, "Log sent - link copied, paste it into Discord"),
                    Ok(_) => crate::toast(&a, "Couldn't upload the log - the file is copied instead, paste it into Discord"),
                    Err(e) => crate::toast(&a, format!("Couldn't send the log: {e}")),
                }
            },
        );
        return;
    }
    // (fallback without Echo's recorder) log + settings, passwords / keys / tokens / emails left out
    let mut cfg = Value::Object(st.v.clone());
    if let Some(m) = cfg.as_object_mut() {
        for (k, v) in m.iter_mut() {
            let k = k.to_lowercase();
            if (k.contains("secret") || k.contains("token") || k.contains("pass") || k.contains("email") || (k.contains("key") && k.starts_with("r2"))) && v.as_str().is_some_and(|s| !s.is_empty()) {
                *v = json!("***");
            }
        }
    }
    let text = format!(
        "Echo {} (native) - {}\n\n--- settings ---\n{}\n\n--- log ---\n{}\n",
        app_version(),
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        serde_json::to_string_pretty(&cfg).unwrap_or_default(),
        crate::lib_net::log_tail(800, 380_000)
    );
    let text = crate::lib_net::redact_emails(&text);
    let body = json!({ "text": text, "version": app_version(), "os": if cfg!(windows) { "Windows" } else { "Linux" } });
    crate::toast(a, "Sending the log…");
    crate::lib_net::call_ui(a, "POST", "/log", String::new(), Some(body), |a, r| match r.and_then(|v| v["url"].as_str().filter(|u| !u.is_empty()).map(|u| u.to_string()).ok_or_else(|| "no link".to_string())) {
        Ok(url) => {
            let _ = crate::lib_net::copy_text(&url);
            crate::toast(a, "Log sent - link copied, paste it into Discord");
        }
        Err(e) => crate::toast(a, format!("Couldn't send the log: {e}")),
    });
}

// ---------- Make smaller (card menu) + Self-test (Chat B sweep) ----------
/// Echo's compress_clip from the card menu: factor 0.5 = about half, 0.33 = about a third (the original is replaced;
/// its date is kept; the cloud copy follows)
pub fn smaller(a: &AppWindow, idx: usize, factor: f64) {
    // gaps-library row 2: asks "Make this clip smaller?" first, then joins the one-at-a-time queue with its per-card
    // progress (lib_cards.rs, Echo's queueJobs) - was its own progress box per click
    crate::lib_cards::shrink_at(a, idx, factor);
}

/// Settings > Troubleshooting > Self-test and setup step 6: Echo's self_test (records ~6 s, checks video + every audio
/// track). gaps-settings: like Echo the button reads "Testing… (6 s)" and the result shows under the row
/// (settings.rs self_test_row), no dialog.
pub fn self_test(a: &AppWindow, refresh: &Refresh) {
    let Some(rc) = crate::rec_core::rec() else { return };
    if !crate::rec_core::can_record() {
        crate::toast(a, "Another Echo is recording - close it, then run the self-test here");
        return;
    }
    if SELFTEST.with(|s| s.borrow().0) {
        return;
    }
    SELFTEST.with(|s| *s.borrow_mut() = (true, None));
    refresh();
    let refresh = refresh.clone();
    crate::lib_net::bg(
        move || rc.self_test(),
        move |rows| {
            let res = if rows.is_empty() { Err("The self-test couldn't run - see the log".to_string()) } else { Ok(rows) };
            SELFTEST.with(|s| *s.borrow_mut() = (false, Some(res)));
            refresh();
        },
    );
}
