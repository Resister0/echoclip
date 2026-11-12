//! Chat B (item 10): first-run setup ("Welcome to Echo", Echo's #setup), the mic test + microphone list, the
//! "Show me around" tour (once after the setup, again from Settings > App), and the sidebar update card wired to
//! Echo's server: GET /update/<windows|linux>/x86_64/<version> (204 = up to date, else {version, notes, url,
//! signature}) + GET /changes?since=<version> for the updates someone skipped. "Restart Echo" downloads the
//! installer, checks its minisign signature against Echo's updater key (tauri.conf.json) and runs it.
//! Test hooks: ECHO_SETUP=1 opens the setup, ECHO_TOUR=1 starts the tour, ECHO_FAKE_VERSION=1.0.50 pretends to be
//! that version (a hand-made build is 0.1.0 and never offers updates, like Echo).
use crate::settings::Store;
use crate::{AppWindow, SBox, SRow, Su, Tour, Upd, UpdOld};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

type Refresh = Rc<dyn Fn()>;

/// Echo's updater public key (src-tauri/tauri.conf.json plugins.updater.pubkey - public, not a secret)
const UPDATER_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDYwRDk3QzlDNzIzQzQ5NkYKUldSdlNUeHluSHpaWUhIc2d0NVR2cThJRXBISXh2aCtuUm5MTmRKTDBOT1FSVkRYRjNiZGxQK2QK";

const TOUR: [(&str, &str, &str); 6] = [
    ("Your clips", "Every clip lands here. Hover one for its buttons; hold Ctrl to show them on every clip.", "library"),
    ("Your save key", "Press it in a game and the last minutes become a clip. Click a key to change it.", "library"),
    ("Save without a key", "Saves a clip right now. Record next to it records until you press it again.", "library"),
    ("Find clips fast", "Filter by game, cloud or type - or just search.", "library"),
    ("Share links", "Free Echo Cloud makes links that play right inside Discord.", "settings"),
    ("Watermark & storage", "Where clips go, how big they get, and the small Echo logo on shares (you can turn it off here).", "settings"),
];

thread_local! {
    static MODEL: RefCell<Option<(Rc<VecModel<SBox>>, Vec<Rc<VecModel<SRow>>>)>> = const { RefCell::new(None) };
    static STORE: RefCell<Option<Rc<RefCell<Store>>>> = const { RefCell::new(None) };
    static PENDING: RefCell<Option<Update>> = const { RefCell::new(None) };
    static TIMERS: RefCell<Vec<slint::Timer>> = const { RefCell::new(vec![]) };
    /// gaps-settings: the window (focused() comes from winit's handler) + the Settings page refresh
    static APP: RefCell<Option<slint::Weak<AppWindow>>> = const { RefCell::new(None) };
    static REFRESH: RefCell<Option<Refresh>> = const { RefCell::new(None) };
    /// when Echo last asked for an update (None = not yet: the first check is 30 s after start)
    static UPD_LAST: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
    /// Settings' line under the version ("" = the normal text) + a check by hand is running
    static UPD_STATE: RefCell<String> = const { RefCell::new(String::new()) };
    static CHECKING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Clone)]
struct Update {
    version: String,
    url: String,
    signature: String,
}

/// the setup's rows follow every change (main.rs srefresh calls this)
pub fn refresh(a: &AppWindow, st: &Store) {
    if !a.global::<Su>().get_open() {
        return;
    }
    MODEL.with(|m| {
        let mut m = m.borrow_mut();
        let (model, inner) = m.get_or_insert_with(|| (Rc::new(VecModel::default()), vec![]));
        a.global::<Su>().set_boxes(ModelRc::from(model.clone()));
        crate::settings::apply(model, inner, st.boxes_of("setup"));
    });
}

pub fn open_setup(a: &AppWindow) {
    a.global::<Su>().set_open(true);
    STORE.with(|s| {
        if let Some(st) = s.borrow().as_ref() {
            refresh(a, &st.borrow());
        }
    });
}

/// Settings buttons this module owns. true = handled.
pub fn act(a: &AppWindow, store: &Rc<RefCell<Store>>, r: &Refresh, act: &str) -> bool {
    match act {
        "welcome" => open_setup(a),
        "tour" => start_tour(a),
        "updates" => check_updates(a, true),
        "mictest" => mic_test(a, store, r),
        _ => return false,
    }
    true
}

pub fn wire(app: &AppWindow, store: Rc<RefCell<Store>>, r: Refresh) {
    STORE.with(|s| *s.borrow_mut() = Some(store.clone()));
    // setup: first start (Echo's settings.json has no setup_done yet)
    {
        let (w, store) = (app.as_weak(), store.clone());
        app.global::<Su>().on_done(move || {
            let Some(a) = w.upgrade() else { return };
            let _ = store.borrow_mut().write(&[("setup_done", serde_json::json!(true))]);
            a.global::<Su>().set_open(false);
            store.borrow_mut().listening.clear();
            let w = a.as_weak();
            slint::Timer::single_shot(Duration::from_millis(600), move || {
                if let Some(a) = w.upgrade() {
                    start_tour(&a);
                }
            });
        });
    }
    let first = !store.borrow().v.get("setup_done").and_then(|v| v.as_bool()).unwrap_or(false);
    if first || std::env::var("ECHO_SETUP").is_ok() {
        let w = app.as_weak();
        slint::Timer::single_shot(Duration::from_millis(400), move || {
            if let Some(a) = w.upgrade() {
                open_setup(&a);
            }
        });
    }
    // tour buttons
    {
        let w = app.as_weak();
        app.global::<Tour>().on_next(move || {
            if let Some(a) = w.upgrade() {
                let s = a.global::<Tour>().get_step();
                tour_go(&a, s + 1);
            }
        });
    }
    {
        let w = app.as_weak();
        app.global::<Tour>().on_skip(move || {
            if let Some(a) = w.upgrade() {
                a.global::<Tour>().set_step(-1);
            }
        });
    }
    if std::env::var("ECHO_TOUR").is_ok() {
        let w = app.as_weak();
        slint::Timer::single_shot(Duration::from_millis(1200), move || {
            if let Some(a) = w.upgrade() {
                start_tour(&a);
            }
        });
    }
    // microphones (the list takes a moment: ffmpeg / pactl on a thread)
    {
        let (store, r) = (store.clone(), r.clone());
        crate::lib_net::bg(list_mics, move |(m, def)| {
            let mut st = store.borrow_mut();
            st.mics = m;
            st.mic_default = def;
            drop(st);
            r();
        });
    }
    // update card - gaps-settings: like Echo, "What's new" soon after start, the first check after 30 s, then every
    // 2 h, and again when the window comes to the front (focused(), at most every 5 min)
    APP.with(|x| *x.borrow_mut() = Some(app.as_weak()));
    REFRESH.with(|x| *x.borrow_mut() = Some(r.clone()));
    wire_updates(app);
    let w = app.as_weak();
    slint::Timer::single_shot(Duration::from_secs(8), {
        let w = w.clone();
        move || {
            if let Some(a) = w.upgrade() {
                whats_new(&a);
            }
        }
    });
    slint::Timer::single_shot(Duration::from_secs(30), {
        let w = w.clone();
        move || {
            if let Some(a) = w.upgrade() {
                check_updates(&a, false);
            }
        }
    });
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_secs(2 * 3600), move || {
        if let Some(a) = w.upgrade() {
            check_updates(&a, false);
        }
    });
    TIMERS.with(|x| x.borrow_mut().push(t));
}

/// gaps-settings: Echo's updSoon - the window came to the front (main.rs, winit Focused): ask again when the last
/// check is more than 5 min old (people keep Echo in the tray for days). Never before the first check (30 s).
pub fn focused() {
    let due = UPD_LAST.with(|l| l.get().is_some_and(|t| t.elapsed() > Duration::from_secs(5 * 60)));
    if !due {
        return;
    }
    UPD_LAST.with(|l| l.set(Some(std::time::Instant::now())));
    // not inside winit's event handler
    slint::Timer::single_shot(Duration::from_millis(0), || {
        if let Some(a) = APP.with(|x| x.borrow().as_ref().and_then(|w| w.upgrade())) {
            check_updates(&a, false);
        }
    });
}

/// gaps-settings: Settings > App Settings > About under "Echo X": (Echo's #updstate text, the button)
pub fn upd_state() -> (String, String) {
    let s = UPD_STATE.with(|s| s.borrow().clone());
    if version() == "0.1.0" && s.starts_with("This copy") {
        return (s, "Get Echo".into());
    }
    (if s.is_empty() { "Echo checks for updates by itself every few hours.".into() } else { s }, "Check for updates".into())
}

fn set_upd_state(s: impl Into<String>) {
    UPD_STATE.with(|x| *x.borrow_mut() = s.into());
    if let Some(r) = REFRESH.with(|x| x.borrow().clone()) {
        r();
    }
}

// ---------- tour ----------
pub fn start_tour(a: &AppWindow) {
    let has_clips = crate::LIB.with(|l| l.borrow().as_ref().is_some_and(|l| l.view_len() > 0));
    tour_go(a, if has_clips { 0 } else { 1 });
}

fn tour_go(a: &AppWindow, i: i32) {
    let t = a.global::<Tour>();
    if i < 0 || i as usize >= TOUR.len() {
        t.set_step(-1);
        return;
    }
    let (title, text, page) = TOUR[i as usize];
    if page == "library" && (a.get_view() != "library" || a.get_player_open()) {
        a.invoke_pick_filter("all".into());
    }
    if page == "settings" && a.get_view() != "settings" {
        a.invoke_page("Settings".into());
    }
    t.set_count(TOUR.len() as i32);
    t.set_title(title.into());
    t.set_text(text.into());
    t.set_step(i);
    let w = a.as_weak();
    slint::Timer::single_shot(Duration::from_millis(50), move || {
        if let Some(a) = w.upgrade() {
            a.invoke_focus_tour();
        }
    });
}

// ---------- microphones + mic test ----------
/// ((device name Echo stores in mic_device, label), the default device's name) - gaps-settings: Echo's own list_mics
/// + mic_label (what its recorder opens), so the default row can say "Windows default (<device>)"; the old lists
/// below only when Echo's finds nothing
fn list_mics() -> (Vec<(String, String)>, String) {
    let (names, def) = echo_core::recorder::list_mics();
    if !names.is_empty() {
        return (names.iter().map(|n| (n.clone(), echo_core::recorder::mic_label(n))).collect(), def);
    }
    (list_mics_old(), def)
}
fn list_mics_old() -> Vec<(String, String)> {
    #[cfg(windows)]
    {
        // DirectShow's audio inputs have the same friendly names as WASAPI's ("Microphone (HyperX Cloud II)")
        let Some(ff) = crate::sheets::ffmpeg() else { return vec![] };
        let o = crate::edrender::hidden_cmd(&ff).args(["-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"]).output();
        let t = o.map(|o| String::from_utf8_lossy(&o.stderr).to_string()).unwrap_or_default();
        let mut v = vec![];
        for l in t.lines() {
            if l.contains("(audio)") {
                if let (Some(a), Some(b)) = (l.find('"'), l.rfind('"')) {
                    if b > a + 1 {
                        let n = l[a + 1..b].to_string();
                        v.push((n.clone(), n));
                    }
                }
            }
        }
        v
    }
    #[cfg(not(windows))]
    {
        // the sound server's microphones (pactl), monitors of speakers left out - like Echo's linux_sources
        let o = std::process::Command::new("pactl").env("LC_ALL", "C").args(["list", "sources"]).stderr(std::process::Stdio::null()).output();
        let t = o.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
        let mut v = vec![];
        let (mut name, mut desc) = (String::new(), String::new());
        let flush = |name: &mut String, desc: &mut String, v: &mut Vec<(String, String)>| {
            if !name.is_empty() && !name.ends_with(".monitor") {
                v.push((name.clone(), if desc.is_empty() { name.clone() } else { desc.clone() }));
            }
            name.clear();
            desc.clear();
        };
        for l in t.lines() {
            let l2 = l.trim();
            if l.starts_with("Source #") {
                flush(&mut name, &mut desc, &mut v);
            } else if let Some(x) = l2.strip_prefix("Name:") {
                name = x.trim().to_string();
            } else if let Some(x) = l2.strip_prefix("Description:") {
                desc = x.trim().to_string();
            }
        }
        flush(&mut name, &mut desc, &mut v);
        v
    }
}

/// 3 s from the chosen mic (or the first / default one) -> loudest peak (ffmpeg volumedetect)
fn mic_test(a: &AppWindow, store: &Rc<RefCell<Store>>, r: &Refresh) {
    let Some(ff) = crate::sheets::ffmpeg() else {
        crate::toast(a, "The mic test needs ffmpeg (Echo downloads it - start Echo once)");
        return;
    };
    let (dev, mics, def) = {
        let st = store.borrow();
        (st.s("mic_device"), st.mics.clone(), st.mic_default.clone())
    };
    if store.borrow().mic_test == LISTENING {
        return; // already listening (a double click)
    }
    store.borrow_mut().mic_test = LISTENING.into();
    r();
    // 11 Nov 2026 (2.0.4 bug: "Listening…" never ended): the result goes to the UI thread with
    // invoke_from_event_loop; mic_record itself gives up after 10 s
    std::thread::spawn(move || {
        let res = mic_record(&ff, dev, mics, def);
        let _ = slint::invoke_from_event_loop(move || mic_done(res));
    });
}

const LISTENING: &str = "Listening… say something";

/// the mic test's answer on the UI thread
fn mic_done(res: Result<f64, String>) {
    let Some(store) = STORE.with(|s| s.borrow().clone()) else { return };
    store.borrow_mut().mic_test = match res {
        Ok(db) if db > -45.0 => format!("✓ Your mic works (loudest {db:.0} dB)."),
        Ok(db) => format!("× Almost silent (loudest {db:.0} dB) - is the mic muted, or is it another device?"),
        Err(e) => format!("× Couldn't record from the mic: {}", e.chars().take(140).collect::<String>()),
    };
    if let Some(r) = REFRESH.with(|x| x.borrow().clone()) {
        r();
    }
}

/// worker: 3 s from the mic -> loudest peak in dB
fn mic_record(ff: &std::path::Path, dev: String, mics: Vec<(String, String)>, def: String) -> Result<f64, String> {
    #[cfg(windows)]
    let input: Vec<String> = {
        // DirectShow's names can be cut short (31 characters): the mic Echo uses, or the default one, matched
        // to DirectShow's list by its start
        let ds = list_mics_old();
        let want = if dev.is_empty() { def } else { dev };
        let d = ds
            .iter()
            .find(|m| m.0 == want)
            .or_else(|| ds.iter().find(|m| !want.is_empty() && want.starts_with(m.0.as_str())))
            .map(|m| m.0.clone())
            .or_else(|| ds.first().map(|m| m.0.clone()))
            .unwrap_or_else(|| mics.first().map(|m| m.0.clone()).unwrap_or_default());
        if d.is_empty() {
            return Err("No microphone found".to_string());
        }
        vec!["-f".into(), "dshow".into(), "-i".into(), format!("audio={d}")]
    };
    #[cfg(not(windows))]
    let input: Vec<String> = {
        let _ = (&mics, &def);
        vec!["-f".into(), "pulse".into(), "-i".into(), if dev.is_empty() { "default".into() } else { dev }]
    };
    // 11 Nov 2026: at most 10 s - a mic that opens but never sends sound kept ffmpeg (and "Listening…") going
    // forever, since "-t 3" counts recorded sound, not time
    let mut ch = crate::edrender::hidden_cmd(&ff)
        .args(["-hide_banner", "-nostats", "-nostdin"])
        .args(&input)
        .args(["-t", "3", "-af", "volumedetect", "-f", "null", "-"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let reader = ch.stderr.take().map(|mut e| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::Read::read_to_string(&mut e, &mut s);
            s
        })
    });
    let t0 = std::time::Instant::now();
    let timed_out = loop {
        match ch.try_wait() {
            Ok(Some(_)) | Err(_) => break false,
            Ok(None) if t0.elapsed() > Duration::from_secs(10) => {
                let _ = ch.kill();
                let _ = ch.wait();
                break true;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    let t = reader.and_then(|r| r.join().ok()).unwrap_or_default();
    if timed_out {
        return Err("the mic sent no sound for 10 s - is it plugged in and switched on?".to_string());
    }
    let max = t.lines().find_map(|l| l.split("max_volume:").nth(1)).and_then(|x| x.trim().trim_end_matches("dB").trim().parse::<f64>().ok());
    match max {
        Some(db) => Ok(db),
        None => Err(t.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("couldn't open the microphone").to_string()),
    }
}

// ---------- updates ----------
fn version() -> String {
    std::env::var("ECHO_FAKE_VERSION").unwrap_or_else(|_| crate::lib_set::app_version())
}

fn ver_gt(a: &str, b: &str) -> bool {
    let p = |s: &str| s.split(['.', '+', '-']).take(3).map(|x| x.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>();
    let (x, y) = (p(a), p(b));
    for i in 0..3 {
        let (u, w) = (x.get(i).copied().unwrap_or(0), y.get(i).copied().unwrap_or(0));
        if u != w {
            return u > w;
        }
    }
    false
}

/// installed copy that can replace itself (Windows: next to uninstall.exe; Linux: the AppImage)
fn can_self_update() -> bool {
    #[cfg(windows)]
    {
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("uninstall.exe").exists())).unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("APPIMAGE").is_some()
    }
}

/// gaps-settings: Echo's linux_update_cmd (src-tauri main.rs) for the plain tar.gz build: a terminal one-liner that
/// downloads the newest tar.gz, swaps the Echo program next to this one, then restarts Echo. None on Windows, for the
/// AppImage and for Flatpak. ⚠ not run here (Windows PC)
fn linux_update_cmd() -> Option<String> {
    #[cfg(windows)]
    {
        None
    }
    #[cfg(not(windows))]
    {
        #[cfg(target_os = "linux")]
        let flatpak = echo_core::linuxutil::in_flatpak();
        #[cfg(not(target_os = "linux"))]
        let flatpak = false;
        if flatpak || can_self_update() {
            return None;
        }
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?.to_string_lossy().to_string();
        let name = exe.file_name()?.to_string_lossy().to_string();
        let q = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        Some(format!(
            "d={}; t=$(mktemp -d) && curl -fsSL https://echoclip.link/download/linux-tar | tar -xz -C \"$t\" && cp \"$t/Echo-linux/Echo\" \"$d/.echo-new\" && chmod +x \"$d/.echo-new\" && mv -f \"$d/.echo-new\" \"$d/\"{} && rm -rf \"$t\" && {{ pkill -x {}; sleep 1; (nohup \"$d/\"{} >/dev/null 2>&1 &); echo 'Echo updated and restarted'; }}",
            q(&dir),
            q(&name),
            q(&name.chars().take(15).collect::<String>()),
            q(&name)
        ))
    }
}

/// the card's button: Echo's updCheck labels
fn install_label() -> &'static str {
    if can_self_update() {
        "Restart Echo"
    } else if linux_update_cmd().is_some() {
        "Copy update command"
    } else {
        "Download"
    }
}

fn wire_updates(app: &AppWindow) {
    let w = app.as_weak();
    app.global::<Upd>().on_clicked(move || {
        let Some(a) = w.upgrade() else { return };
        let u = a.global::<Upd>();
        if u.get_busy() {
            return;
        }
        if u.get_btn() == "Got it" {
            u.set_on(false);
            u.set_older_on(false);
        } else {
            install(&a);
        }
    });
    app.global::<Upd>().on_all_clicked(|| crate::open_url("https://echoclip.link/changelog"));
}

/// the card's earlier-updates list from /changes (newest first)
fn fill_older(a: &AppWindow, list: &[(String, String)], after: bool) {
    let u = a.global::<Upd>();
    let n = list.len();
    let s = if n > 1 { "s" } else { "" };
    u.set_more(match (n, after) {
        (0, _) => "".into(),
        (_, true) => format!("View {n} earlier update{s} you missed").into(),
        (_, false) => format!("+ {n} more update{s} included").into(),
    });
    let show: Vec<UpdOld> = list.iter().take(if n > 8 { 6 } else { 8 }).map(|(v, t)| UpdOld { v: v.into(), notes: t.into() }).collect();
    let rest = n - show.len();
    u.set_older(Rc::new(VecModel::from(show)).into());
    u.set_all(if rest > 0 { format!("+ {rest} older update{} on echoclip.link", if rest > 1 { "s" } else { "" }) } else { "All changes on echoclip.link".into() }.into());
}

fn changes_since(since: &str) -> Vec<(String, String)> {
    let v = crate::lib_net::ec_call("GET", &format!("/changes?since={since}"), "", None).unwrap_or_default();
    v["releases"].as_array().map(|a| a.iter().filter_map(|r| Some((r["version"].as_str()?.to_string(), r["notes"].as_str().unwrap_or("").to_string()))).collect()).unwrap_or_default()
}

/// "Updated to Echo X" once after an update (settings.json native_seen_version = the last version that ran).
/// gaps-settings: like Echo's whatsNew the card shows at once - with the notes saved when the update was found
/// (native_ui.json "whatsnew", Echo's echo_whatsnew), else "Echo X is installed." - also offline; /changes then
/// fills in the notes and the updates skipped in between.
fn whats_new(a: &AppWindow) {
    let cur = version();
    if cur == "0.1.0" {
        return;
    }
    let Some(store) = STORE.with(|s| s.borrow().clone()) else { return };
    let seen = store.borrow().s("native_seen_version");
    let saved = crate::lib_fx::ui_get("whatsnew");
    let stored = if saved["v"].as_str() == Some(cur.as_str()) { saved["notes"].as_str().unwrap_or("").to_string() } else { String::new() };
    if seen == cur || (seen.is_empty() && saved.is_null()) || (!seen.is_empty() && !ver_gt(&cur, &seen)) {
        if seen != cur {
            let _ = store.borrow_mut().write(&[("native_seen_version", serde_json::json!(cur))]);
        }
        return;
    }
    let _ = store.borrow_mut().write(&[("native_seen_version", serde_json::json!(cur))]);
    crate::lib_fx::ui_set("whatsnew", serde_json::Value::Null);
    let u = a.global::<Upd>();
    u.set_title(format!("Updated to Echo {cur}").into());
    u.set_text(if stored.is_empty() { format!("Echo {cur} is installed.") } else { format!("What's new: {stored}") }.into());
    u.set_btn("Got it".into());
    u.set_primary(false);
    u.set_busy(false);
    u.set_more("".into());
    u.set_older_on(false);
    u.set_on(true);
    if seen.is_empty() {
        return;
    }
    let w = a.as_weak();
    crate::lib_net::bg(move || changes_since(&seen), move |list| {
        let Some(a) = w.upgrade() else { return };
        let u = a.global::<Upd>();
        // an update check took the card meanwhile
        if u.get_btn() != "Got it" {
            return;
        }
        let Some(top) = list.iter().find(|(v, _)| !ver_gt(v, &cur)).cloned() else { return };
        if !top.1.is_empty() {
            u.set_text(format!("What's new: {}", top.1).into());
        }
        let older: Vec<(String, String)> = list.into_iter().filter(|(v, _)| *v != top.0).collect();
        fill_older(&a, &older, true);
    });
}

pub fn check_updates(a: &AppWindow, by_hand: bool) {
    let cur = version();
    if cur == "0.1.0" {
        // Echo's updManual: say it plainly, then the button takes you to the installer
        if by_hand {
            if upd_state().1 == "Get Echo" {
                crate::open_url("https://echoclip.link/download");
            } else {
                set_upd_state("This copy of Echo was built by hand and never updates itself. Install Echo from echoclip.link to get updates.");
            }
        }
        return;
    }
    if by_hand {
        if CHECKING.with(|c| c.replace(true)) {
            return;
        }
        set_upd_state("Checking…");
    }
    UPD_LAST.with(|l| l.set(Some(std::time::Instant::now())));
    let os = if cfg!(windows) { "windows" } else { "linux" };
    let w = a.as_weak();
    let cur2 = cur.clone();
    crate::lib_net::bg(
        move || -> Result<Option<(serde_json::Value, Vec<(String, String)>)>, String> {
            // + the anonymous memory / CPU averages since the last check + the PC's specs (perf.rs, specs.rs; "" when
            // switched off in Settings > About)
            let mut url = format!("{}/update/{os}/x86_64/{cur2}{}", crate::lib_net::ec_api(), crate::perf::query());
            // + "first=1" once per install for the server's all-time installs count (no ID, just one anonymous +1 like the
            // daily check-in, so not tied to the perf switch); the marker file is only written once the server answered
            let counted = echo_core::recorder::app_dir_pub().join("install-counted");
            let first = !counted.exists();
            if first {
                url.push_str(if url.contains('?') { "&first=1" } else { "?first=1" });
            }
            let (code, body) = crate::lib_net::request("GET", &url, vec![], None, 30)?;
            if first && (code == 200 || code == 204) {
                let _ = std::fs::write(&counted, b"1");
            }
            if code == 204 || body.trim().is_empty() {
                return Ok(None);
            }
            if code >= 300 {
                return Err(format!("update server answered {code}"));
            }
            let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
            let list = changes_since(&cur2);
            Ok(Some((v, list)))
        },
        move |r| {
            if by_hand {
                CHECKING.with(|c| c.set(false));
            }
            let Some(a) = w.upgrade() else { return };
            match r {
                Ok(None) => {
                    crate::lib_net::log_line(&format!("update: up to date ({cur})"));
                    if by_hand {
                        set_upd_state("You have the newest version.");
                    }
                }
                Err(e) => {
                    crate::lib_net::log_line(&format!("update: check failed: {e}"));
                    if by_hand {
                        set_upd_state(format!("Couldn't check: {e}"));
                    } else {
                        // offline at start (right after boot / sleep) waited 4 h for the next check: try again in
                        // 15 min (one chain; it ends at the first answer)
                        let w = a.as_weak();
                        slint::Timer::single_shot(Duration::from_secs(15 * 60), move || {
                            if let Some(a) = w.upgrade() {
                                check_updates(&a, false);
                            }
                        });
                    }
                }
                Ok(Some((v, list))) => {
                    let ver = v["version"].as_str().unwrap_or("").to_string();
                    let notes = v["notes"].as_str().unwrap_or("").to_string();
                    crate::lib_net::log_line(&format!("update: {ver} available (this is {cur})"));
                    PENDING.with(|p| {
                        *p.borrow_mut() = Some(Update { version: ver.clone(), url: v["url"].as_str().unwrap_or("").into(), signature: v["signature"].as_str().unwrap_or("").into() })
                    });
                    // remembered for "What's new" after the restart, also when /changes can't be reached then
                    crate::lib_fx::ui_set("whatsnew", serde_json::json!({ "v": ver, "notes": notes }));
                    let u = a.global::<Upd>();
                    // a download / install already running keeps its "Updating…" button
                    if !u.get_busy() {
                        u.set_title("Update ready".into());
                        u.set_text(if notes.is_empty() { format!("Echo {ver} is here (you have {cur}).") } else { format!("Echo {ver} is here (you have {cur}). What's new: {notes}") }.into());
                        u.set_btn(install_label().into());
                        u.set_primary(true);
                        let older: Vec<(String, String)> = list.into_iter().filter(|(v, _)| *v != ver).collect();
                        fill_older(&a, &older, false);
                        u.set_on(true);
                    }
                    if by_hand {
                        set_upd_state(format!("Echo {ver} is ready - click \"{}\" on the left.", install_label()));
                    }
                    // Echo hidden in the tray never shows the card: the tray menu offers it too
                    if can_self_update() {
                        crate::tray::update_ready(&ver);
                    }
                }
            }
        },
    );
}

/// r2-threads (L55): AppImage update - the new file over the running AppImage ($APPIMAGE), on a worker. Ok = the
/// AppImage to start. ⚠ Linux only, not compiled on the PC it was written on.
#[cfg(not(windows))]
fn swap_appimage(f: &std::path::Path) -> Result<PathBuf, String> {
    let msg = || "The update couldn't be installed - download Echo from echoclip.link".to_string();
    let target = std::env::var_os("APPIMAGE").map(PathBuf::from).ok_or_else(msg)?;
    let tmp = target.with_extension("new");
    let ok = std::fs::copy(f, &tmp).is_ok() && {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
        std::fs::rename(&tmp, &target).is_ok()
    };
    if ok {
        Ok(target)
    } else {
        Err(msg())
    }
}

/// minisign check of the downloaded file against Echo's updater key (Tauri's .sig = base64 of the .minisig text)
fn verify(file: &std::path::Path, sig_b64: &str) -> Result<(), String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let pk_txt = String::from_utf8(b64.decode(UPDATER_PUBKEY).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let pk = minisign_verify::PublicKey::decode(&pk_txt).map_err(|e| e.to_string())?;
    let sig_txt = String::from_utf8(b64.decode(sig_b64.trim()).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let sig = minisign_verify::Signature::decode(&sig_txt).map_err(|e| e.to_string())?;
    let data = std::fs::read(file).map_err(|e| e.to_string())?;
    pk.verify(&data, &sig, false).map_err(|e| e.to_string())
}

pub fn install(a: &AppWindow) {
    let Some(u) = PENDING.with(|p| p.borrow().clone()) else {
        check_updates(a, true);
        return;
    };
    if !can_self_update() || u.url.is_empty() {
        // gaps-settings: Echo's update_install for the Linux tar.gz build: the command goes on the clipboard
        if let Some(cmd) = linux_update_cmd() {
            match crate::lib_net::copy_text(&cmd) {
                Ok(_) => crate::toast(a, "Update command copied - paste it in a terminal and press Enter. Echo restarts by itself."),
                Err(e) => crate::toast(a, e),
            }
            return;
        }
        crate::open_url("https://echoclip.link/download");
        return;
    }
    // gaps-settings: Echo's updInstall - "Updating…" (no second click), "Try again" when it fails
    let card = a.global::<Upd>();
    if card.get_busy() {
        return;
    }
    card.set_busy(true);
    card.set_btn("Updating…".into());
    let failed = |a: &AppWindow, e: String| {
        let card = a.global::<Upd>();
        card.set_busy(false);
        card.set_btn("Try again".into());
        crate::toast(a, e);
    };
    let id = crate::prog_box(&format!("Downloading Echo {}…", u.version));
    crate::prog_set(id, None, None, Some(-1.0));
    let w = a.as_weak();
    crate::lib_net::bg(
        move || -> Result<PathBuf, String> {
            let name = u.url.rsplit('/').next().unwrap_or("echo-update").split('?').next().unwrap_or("echo-update").to_string();
            let f = std::env::temp_dir().join(format!("echo-update-{}-{name}", u.version));
            crate::lib_net::download(&u.url, &f)?;
            if let Err(e) = verify(&f, &u.signature) {
                let _ = std::fs::remove_file(&f);
                crate::lib_net::log_line(&format!("update: signature check failed: {e}"));
                return Err("The update's signature didn't match - not installed".into());
            }
            // r2-threads (L55): the AppImage swap (a ~100 MB copy) runs here on the worker, not in the done step
            #[cfg(not(windows))]
            let f = swap_appimage(&f)?;
            Ok(f)
        },
        move |r| {
            let Some(a) = w.upgrade() else { return };
            crate::prog_done(id, 400);
            match r {
                Err(e) => failed(&a, e),
                Ok(f) => {
                    crate::lib_net::log_line(&format!("update: installing {}", f.display()));
                    #[cfg(windows)]
                    {
                        // the NSIS installer like Tauri's passive mode: progress only, reopens Echo when done ⚠
                        // /UPDATE like Tauri's updater: shortcuts the person deleted aren't made again
                        if std::process::Command::new(&f).args(["/P", "/R", "/UPDATE"]).spawn().is_ok() {
                            let _ = slint::quit_event_loop();
                            std::process::exit(0);
                        }
                        failed(&a, "The installer couldn't start - download Echo from echoclip.link".into());
                    }
                    #[cfg(not(windows))]
                    {
                        // AppImage: the file was swapped on the worker (swap_appimage): start the new one
                        let target = f;
                        // 12 Nov 2026: else the new Echo keeps the old AppImage runtime alive (linux.rs)
                        #[cfg(target_os = "linux")]
                        crate::linux::no_inherit_fds();
                        if std::process::Command::new(&target).spawn().is_ok() {
                            let _ = slint::quit_event_loop();
                            std::process::exit(0);
                        }
                        failed(&a, "The update couldn't be installed - download Echo from echoclip.link".into());
                    }
                }
            }
        },
    );
}
