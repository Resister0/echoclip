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
        crate::lib_net::bg(list_mics, move |m| {
            store.borrow_mut().mics = m;
            r();
        });
    }
    // update card
    wire_updates(app);
    let w = app.as_weak();
    slint::Timer::single_shot(Duration::from_secs(8), {
        let w = w.clone();
        move || {
            if let Some(a) = w.upgrade() {
                whats_new(&a);
                check_updates(&a, false);
            }
        }
    });
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_secs(4 * 3600), move || {
        if let Some(a) = w.upgrade() {
            check_updates(&a, false);
        }
    });
    TIMERS.with(|x| x.borrow_mut().push(t));
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
/// (device name Echo stores in mic_device, label)
fn list_mics() -> Vec<(String, String)> {
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
    let (dev, mics) = {
        let st = store.borrow();
        (st.s("mic_device"), st.mics.clone())
    };
    store.borrow_mut().mic_test = "Listening… say something".into();
    r();
    let (store, r) = (store.clone(), r.clone());
    crate::lib_net::bg(
        move || {
            #[cfg(windows)]
            let input: Vec<String> = {
                let d = if dev.is_empty() { mics.first().map(|m| m.0.clone()).unwrap_or_default() } else { dev };
                if d.is_empty() {
                    return Err("No microphone found".to_string());
                }
                vec!["-f".into(), "dshow".into(), "-i".into(), format!("audio={d}")]
            };
            #[cfg(not(windows))]
            let input: Vec<String> = {
                let _ = &mics;
                vec!["-f".into(), "pulse".into(), "-i".into(), if dev.is_empty() { "default".into() } else { dev }]
            };
            let o = crate::edrender::hidden_cmd(&ff)
                .args(["-hide_banner", "-nostats"])
                .args(&input)
                .args(["-t", "3", "-af", "volumedetect", "-f", "null", "-"])
                .stdin(std::process::Stdio::null())
                .output()
                .map_err(|e| e.to_string())?;
            let t = String::from_utf8_lossy(&o.stderr).to_string();
            let max = t.lines().find_map(|l| l.split("max_volume:").nth(1)).and_then(|x| x.trim().trim_end_matches("dB").trim().parse::<f64>().ok());
            match max {
                Some(db) => Ok(db),
                None => Err(t.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("couldn't open the microphone").to_string()),
            }
        },
        move |res: Result<f64, String>| {
            store.borrow_mut().mic_test = match res {
                Ok(db) if db > -45.0 => format!("✓ Your mic works (loudest {db:.0} dB)."),
                Ok(db) => format!("✗ Almost silent (loudest {db:.0} dB) - is the mic muted, or is it another device?"),
                Err(e) => format!("✗ Couldn't record from the mic: {}", e.chars().take(140).collect::<String>()),
            };
            r();
        },
    );
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

fn wire_updates(app: &AppWindow) {
    let w = app.as_weak();
    app.global::<Upd>().on_clicked(move || {
        let Some(a) = w.upgrade() else { return };
        let u = a.global::<Upd>();
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

/// "Updated to Echo X" once after an update (settings.json native_seen_version = the last version that ran)
fn whats_new(a: &AppWindow) {
    let cur = version();
    if cur == "0.1.0" {
        return;
    }
    let Some(store) = STORE.with(|s| s.borrow().clone()) else { return };
    let seen = store.borrow().s("native_seen_version");
    let _ = store.borrow_mut().write(&[("native_seen_version", serde_json::json!(cur))]);
    if seen.is_empty() || !ver_gt(&cur, &seen) {
        return;
    }
    let w = a.as_weak();
    crate::lib_net::bg(move || changes_since(&seen), move |list| {
        let Some(a) = w.upgrade() else { return };
        let Some(top) = list.iter().find(|(v, _)| !ver_gt(v, &cur)).cloned() else { return };
        let u = a.global::<Upd>();
        u.set_title(format!("Updated to Echo {cur}").into());
        u.set_text(format!("What's new: {}", top.1).into());
        u.set_btn("Got it".into());
        u.set_primary(false);
        let older: Vec<(String, String)> = list.into_iter().filter(|(v, _)| *v != top.0).collect();
        fill_older(&a, &older, true);
        u.set_on(true);
    });
}

pub fn check_updates(a: &AppWindow, by_hand: bool) {
    let cur = version();
    if cur == "0.1.0" {
        if by_hand {
            crate::toast(a, "This is a hand-made build (0.1.0) - it never updates itself");
        }
        return;
    }
    let os = if cfg!(windows) { "windows" } else { "linux" };
    let w = a.as_weak();
    let cur2 = cur.clone();
    crate::lib_net::bg(
        move || -> Result<Option<(serde_json::Value, Vec<(String, String)>)>, String> {
            // + the anonymous memory / CPU averages since the last check (perf.rs; ignored by the server for now)
            let url = format!("{}/update/{os}/x86_64/{cur2}{}", crate::lib_net::ec_api(), crate::perf::query());
            let (code, body) = crate::lib_net::request("GET", &url, vec![], None, 30)?;
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
            let Some(a) = w.upgrade() else { return };
            match r {
                Ok(None) => {
                    crate::lib_net::log_line(&format!("update: up to date ({cur})"));
                    if by_hand {
                        crate::toast(&a, "Echo is up to date");
                    }
                }
                Err(e) => {
                    crate::lib_net::log_line(&format!("update: check failed: {e}"));
                    if by_hand {
                        crate::toast(&a, "Couldn't check for updates");
                    }
                }
                Ok(Some((v, list))) => {
                    let ver = v["version"].as_str().unwrap_or("").to_string();
                    let notes = v["notes"].as_str().unwrap_or("").to_string();
                    crate::lib_net::log_line(&format!("update: {ver} available (this is {cur})"));
                    PENDING.with(|p| {
                        *p.borrow_mut() = Some(Update { version: ver.clone(), url: v["url"].as_str().unwrap_or("").into(), signature: v["signature"].as_str().unwrap_or("").into() })
                    });
                    let u = a.global::<Upd>();
                    u.set_title("Update ready".into());
                    u.set_text(format!("Echo {ver} is here (you have {cur}). What's new: {notes}").into());
                    u.set_btn(if can_self_update() { "Restart Echo" } else { "Download" }.into());
                    u.set_primary(true);
                    let older: Vec<(String, String)> = list.into_iter().filter(|(v, _)| *v != ver).collect();
                    fill_older(&a, &older, false);
                    u.set_on(true);
                }
            }
        },
    );
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

fn install(a: &AppWindow) {
    let Some(u) = PENDING.with(|p| p.borrow().clone()) else {
        check_updates(a, true);
        return;
    };
    if !can_self_update() || u.url.is_empty() {
        crate::open_url("https://echoclip.link/download");
        return;
    }
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
            Ok(f)
        },
        move |r| {
            let Some(a) = w.upgrade() else { return };
            crate::prog_done(id, 400);
            match r {
                Err(e) => crate::toast(&a, e),
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
                        crate::toast(&a, "The installer couldn't start - download Echo from echoclip.link");
                    }
                    #[cfg(not(windows))]
                    {
                        // AppImage: swap the file, then start the new one
                        let Some(target) = std::env::var_os("APPIMAGE").map(PathBuf::from) else { return };
                        let tmp = target.with_extension("new");
                        let ok = std::fs::copy(&f, &tmp).is_ok() && {
                            use std::os::unix::fs::PermissionsExt;
                            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
                            std::fs::rename(&tmp, &target).is_ok()
                        };
                        if ok && std::process::Command::new(&target).spawn().is_ok() {
                            let _ = slint::quit_event_loop();
                            std::process::exit(0);
                        }
                        crate::toast(&a, "The update couldn't be installed - download Echo from echoclip.link");
                    }
                }
            }
        },
    );
}
