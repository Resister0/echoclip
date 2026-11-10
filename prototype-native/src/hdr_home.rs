//! Header + Home gap fixes (10 Nov 2026, NATIVE_GAPS rows 3, 11, 23, 28, 50; the UI is in app.slint / home.slint /
//! otd.slint):
//! - KDE's shortcut portal (Linux): the header chips + Settings show the keys KDE really uses (Echo's loadKdeKeys)
//! - clip milestones + the "On this day" bar above the library (Echo's charm())
//! - the Record button: toasts, then the new recording opens in the player (Echo's #longbtn)
//! - error toasts go into Echo's log (Echo's toast() -> ui_log)

use crate::library::LibModel;
use crate::{AppWindow, Lib, Otd, S};
use chrono::Datelike;
use slint::ComponentHandle;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

// ---------- KDE shortcut portal (Linux): KDE decides the keys ----------
pub const KDE_TIP: &str = "Your desktop decides Echo's keys: change them in System Settings > Shortcuts > Echo";

thread_local! {
    /// Echo's key setting -> the key KDE uses ("" = none); None = not using KDE's portal (Windows: always None)
    static KDE: RefCell<Option<HashMap<String, String>>> = const { RefCell::new(None) };
    /// main.rs's Settings refresh (also redraws the header chips)
    static REFRESH: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
    static TIMERS: RefCell<Vec<slint::Timer>> = const { RefCell::new(Vec::new()) };
}

fn read_kde() -> Option<HashMap<String, String>> {
    #[cfg(target_os = "linux")]
    {
        echo_core::hotkey::kde_keys() // ⚠ Linux: not compiled / run on this PC
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Echo's keys go through KDE's portal (Settings shows "Keys are set in KDE")
pub fn kde_on() -> bool {
    KDE.with(|k| k.borrow().is_some())
}

/// What one of Echo's key settings shows: None = Echo's own setting counts; Some(parts) = the key KDE uses
/// (empty = "No key in KDE"). Echo's keyShow().
pub fn kde_show(keys: &str) -> Option<Vec<String>> {
    if keys.is_empty() {
        return None;
    }
    let t = KDE.with(|k| k.borrow().as_ref().and_then(|m| m.get(keys).cloned()))?;
    Some(t.split('+').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
}

/// "Click to change a key" (header chip or Settings row): with KDE's portal the desktop decides, so Echo only says
/// where to change it (Echo's listenHotkey). True = blocked (the toast is shown).
pub fn kde_blocks(a: &AppWindow, st: &crate::settings::Store, id: &str) -> bool {
    let cur = match id {
        "shot" => st.s("screenshot_key"),
        "mark" => st.s("bookmark_key"),
        "long" => st.s("session_key"),
        "mute" => st.s("mic_mute_key"),
        "show" => st.s("show_key"),
        _ => id
            .strip_prefix("hk:")
            .and_then(|n| n.parse::<usize>().ok())
            .and_then(|n| st.v.get("hotkeys")?.as_array()?.get(n)?.get("keys")?.as_str().map(String::from))
            .unwrap_or_default(),
    };
    if kde_show(&cur).is_none() {
        return false;
    }
    crate::toast(a, KDE_TIP);
    true
}

/// re-read KDE's keys; a change redraws the header chips + Settings
pub fn kde_poll() {
    let new = read_kde();
    let changed = KDE.with(|k| {
        let mut k = k.borrow_mut();
        if *k == new {
            false
        } else {
            *k = new;
            true
        }
    });
    if changed {
        if let Some(r) = REFRESH.with(|r| r.borrow().clone()) {
            r();
        }
    }
}

/// the window came to the front: KDE's keys may have changed in System Settings (Echo: focus -> loadKdeKeys)
pub fn on_focus() {
    if cfg!(target_os = "linux") {
        kde_poll();
    }
}

/// once at start (after main.rs made its Settings refresh)
pub fn wire(a: &AppWindow, refresh: Rc<dyn Fn()>) {
    REFRESH.with(|r| *r.borrow_mut() = Some(refresh));
    START.with(|s| s.set(Some(Instant::now())));
    // Echo: first read 4 s after start, then every 15 s while Echo is in front
    if cfg!(target_os = "linux") {
        slint::Timer::single_shot(Duration::from_secs(4), kde_poll);
        let t = slint::Timer::default();
        t.start(slint::TimerMode::Repeated, Duration::from_secs(15), || {
            if crate::WIN_FOCUSED.with(|f| f.get()) {
                kde_poll();
            }
        });
        TIMERS.with(|v| v.borrow_mut().push(t));
    }
    // "On this day": Watch / Hide for today / Don't show again
    let w = a.as_weak();
    a.global::<Otd>().on_act(move |k| {
        let Some(a) = w.upgrade() else { return };
        match k.as_str() {
            "watch" => {
                if let (Some(idx), Some(lib)) = (OTD_IDX.with(|o| o.get()), crate::LIB.with(|l| l.borrow().clone())) {
                    open_clip(&a, &lib, idx);
                }
            }
            "today" => {
                crate::lib_fx::ui_set("otd_hidden", serde_json::json!(today_key()));
                a.global::<Otd>().set_show(false);
            }
            "never" => {
                a.global::<S>().invoke_set("on_this_day".into(), "false".into());
                a.global::<Otd>().set_show(false);
                crate::toast(&a, "Turned off - switch it back on in Settings → App");
            }
            _ => {}
        }
    });
}

// ---------- milestones + "On this day" (Echo's charm) ----------
const MILESTONES: [usize; 8] = [10, 50, 100, 250, 500, 1000, 2500, 5000];

thread_local! {
    static START: Cell<Option<Instant>> = const { Cell::new(None) };
    /// (clips, today, setting on) the last time charm() looked - it only works when one of them changes
    static CHARM_KEY: RefCell<(usize, String, bool)> = RefCell::new((usize::MAX, String::new(), true));
    static OTD_IDX: Cell<Option<usize>> = const { Cell::new(None) };
}

fn today_key() -> String {
    chrono::Local::now().date_naive().to_string()
}

/// after the library list changed (main.rs home_fill)
pub fn charm(a: &AppWindow, lib: &LibModel) {
    let n_all = lib.len();
    let on = crate::library::echo_settings().and_then(|s| s.get("on_this_day").and_then(|v| v.as_bool())) != Some(false);
    let today = today_key();
    let key = (lib.all().len(), today.clone(), on);
    if CHARM_KEY.with(|k| *k.borrow() == key) {
        return;
    }
    CHARM_KEY.with(|k| *k.borrow_mut() = key);
    let clips: Vec<_> = (0..n_all).map(|i| (i, lib.clip(i))).filter(|(_, c)| !c.gone.get()).collect();

    // milestones: a quiet note when the library passes 10, 50, 100… clips. Never at first start: the first look only
    // remembers where it is, and so does the first 15 s (the list is still filling: cloud clips join later).
    let n = clips.iter().filter(|(_, c)| !c.edited).count();
    let top = MILESTONES.iter().copied().filter(|&m| n >= m).last().unwrap_or(0);
    let early = START.with(|s| s.get().is_none_or(|t| t.elapsed() < Duration::from_secs(15)));
    match crate::lib_fx::ui_get("milestone").as_u64() {
        Some(had) if top as u64 > had && !early => {
            crate::lib_fx::ui_set("milestone", serde_json::json!(top));
            let w = a.as_weak();
            slint::Timer::single_shot(Duration::from_millis(1500), move || {
                if let Some(a) = w.upgrade() {
                    crate::toast(&a, format!("That's {top} clips saved with Echo. Nice."));
                }
            });
        }
        Some(had) if top as u64 <= had => {}
        _ => crate::lib_fx::ui_set("milestone", serde_json::json!(top)),
    }

    // on this day: a clip from the same date in an earlier year, once a day until it's closed (or switched off)
    let o = a.global::<Otd>();
    let hidden = crate::lib_fx::ui_get("otd_hidden").as_str() == Some(today.as_str());
    if !on || hidden {
        o.set_show(false);
        OTD_IDX.with(|x| x.set(None));
        return;
    }
    let now = chrono::Local::now();
    let favs = lib.favs.borrow();
    let hit = clips
        .iter()
        .filter_map(|(i, c)| {
            let d: chrono::DateTime<chrono::Local> = c.mtime.into();
            (d.month() == now.month() && d.day() == now.day() && d.year() < now.year()).then_some((*i, c, d.year()))
        })
        .max_by(|x, y| favs.contains(&x.1.name).cmp(&favs.contains(&y.1.name)).then(x.1.size.cmp(&y.1.size)));
    match hit {
        Some((i, c, year)) => {
            let yrs = now.year() - year;
            o.set_text(format!("{} ago: {}", if yrs == 1 { "A year".to_string() } else { format!("{yrs} years") }, c.shown).into());
            o.set_show(true);
            OTD_IDX.with(|x| x.set(Some(i)));
        }
        None => {
            o.set_show(false);
            OTD_IDX.with(|x| x.set(None));
        }
    }
}

/// open a clip (library id) in the player: at its spot in the library view, else from Home's list
pub fn open_clip(a: &AppWindow, lib: &LibModel, idx: usize) {
    let row = lib.view_row_of(idx).map(|r| r as i32).or_else(|| {
        (0..16).map(|i| crate::library::HOME_BASE + i).find(|&r| lib.index_at(r) == Some(idx)).map(|r| r as i32)
    });
    match row {
        Some(r) => a.global::<Lib>().invoke_open(r, false),
        None => {
            // not in the view (a filter / search hides it): show all clips, then open it
            a.invoke_pick_filter("all".into());
            if let Some(r) = lib.view_row_of(idx) {
                a.global::<Lib>().invoke_open(r as i32, false);
            }
        }
    }
}

// ---------- the Record button (Echo's #longbtn) ----------
static BUSY: AtomicBool = AtomicBool::new(false);
/// the button's own stop: rec_core's "Clip saved" toast stays quiet (this says "Recording saved")
static QUIET_SAVED: AtomicBool = AtomicBool::new(false);

/// rec_core's Ev::Saved: true = don't toast "Clip saved" (the Record button toasts "Recording saved" itself)
pub fn quiet_saved() -> bool {
    QUIET_SAVED.swap(false, Ordering::Relaxed)
}

/// Record / Stop clicked: "Saving recording…" -> "Recording saved" + the recording opens in the player, or
/// "Recording - press Stop when you're done". A second click while it works does nothing.
pub fn record_click(a: &AppWindow) {
    if BUSY.swap(true, Ordering::Relaxed) {
        return;
    }
    a.set_rec_busy(true); // L15: Echo's #longbtn.disabled while it works
    let stopping = crate::rec_core::rec().is_some_and(|r| r.session_secs().is_some());
    if stopping {
        QUIET_SAVED.store(true, Ordering::Relaxed);
        crate::toast(a, "Saving recording…");
    }
    let w = a.as_weak();
    std::thread::spawn(move || {
        let r = crate::rec_core::do_session();
        let _ = slint::invoke_from_event_loop(move || {
            BUSY.store(false, Ordering::Relaxed);
            if !matches!(r, Ok(Some(_))) {
                QUIET_SAVED.store(false, Ordering::Relaxed);
            }
            let Some(a) = w.upgrade() else { return };
            a.set_rec_busy(false);
            match r {
                Ok(Some(p)) => {
                    let lib = crate::LIB.with(|l| l.borrow().clone());
                    if let Some(lib) = lib {
                        // Echo: refresh, then open the new recording (don't wait for the 3 s disk sync)
                        if lib.sync_disk(&lib.root()) {
                            lib.refresh();
                            crate::home_fill(&a, &lib);
                            if a.get_view() == "library" {
                                a.invoke_pick_filter(a.get_filter());
                            }
                            crate::UI_SYNC.with(|f| {
                                if let Some(f) = f.borrow().as_ref() {
                                    f(true)
                                }
                            });
                        }
                        crate::toast(&a, "Recording saved");
                        if let Some(idx) = lib.index_of_path(std::path::Path::new(&p)) {
                            open_clip(&a, &lib, idx);
                        }
                    } else {
                        crate::toast(&a, "Recording saved");
                    }
                }
                Ok(None) => crate::toast(&a, "Recording - press Stop when you're done"),
                Err(e) => crate::toast(&a, e),
            }
        });
    });
}

// ---------- toasts -> Echo's log ----------
/// Echo's toast(): messages that look like an error also go into the log, so "Send log" has them
pub fn log_toast(msg: &str) {
    let l = msg.to_lowercase();
    if ["os error", "error", "couldn't", "could not", "can't", "failed", "is open in"].iter().any(|k| l.contains(k)) {
        echo_core::recorder::log_line(&format!("ui: {}", msg.chars().take(400).collect::<String>()));
    }
}
