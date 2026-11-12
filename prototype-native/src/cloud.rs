//! Chat B (item 11): Echo Cloud + your own clouds, FULL, through Echo's own cloud code (echo-core = src-tauri's
//! recorder.rs compiled unchanged; the Recorder lives in Chat A's rec_core). This file is only the native UI side:
//! the cloud menu (Copy share link + expiry chips, Upload, Free up space, Download, Delete from the cloud, Stop
//! sharing), L key, Delete "Everywhere", moves / renames following into the cloud (cloud_follow_move), cloud-only
//! clips in the library (playing through Echo's rclone streaming server), share view counts, Settings > Cloud
//! (Echo Cloud sign-in with the emailed code, plans, own clouds, sync now), the Echo Cloud space manager and the
//! "almost full" notice. Every call into the Recorder runs on a worker thread (they talk to rclone / the server).
use crate::library::Clip;
use crate::{AppWindow, Cm, CmItem, Ecm, EcmRow};
use echo_core::recorder::Recorder;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn rec() -> Option<&'static Recorder> {
    crate::rec_core::rec()
}

/// the signed-in Echo account's token ("" = not signed in), through Echo's settings.rs (DPAPI / keyring)
pub fn token() -> String {
    // r2-threads (L50): the recorder's settings in memory (like cfg()); Settings::load reads the keyring on Linux
    // (and may wait 2 s twice when it's down) - only when there is no recorder
    cfg().ec_token
}

fn cfg() -> echo_core::settings::Settings {
    rec().map(|r| r.settings()).unwrap_or_else(echo_core::settings::Settings::load)
}

/// any cloud set up (Echo Cloud or your own)
pub fn on() -> bool {
    !cfg().cloud_accounts.is_empty()
}

pub fn has_ec() -> bool {
    cfg().cloud_accounts.iter().any(|a| a.provider == "echocloud")
}

/// "Valorant/x.mp4" (Echo's rel_of)
pub fn rel_of(c: &Clip) -> String {
    if let Some(r) = &c.cloud {
        return r.clone();
    }
    let file = c.path.file_name().unwrap_or_default().to_string_lossy().to_string();
    if c.folder.is_empty() {
        file
    } else {
        format!("{}/{file}", c.folder)
    }
}

fn lib() -> Option<Rc<crate::library::LibModel>> {
    crate::LIB.with(|l| l.borrow().clone())
}

fn sync_ui(a: &AppWindow) {
    if let Some(l) = lib() {
        l.sync_disk(&l.root());
        l.refresh();
        crate::home_fill(a, &l);
    }
    crate::UI_SYNC.with(|f| {
        if let Some(f) = f.borrow().as_ref() {
            f(true)
        }
    });
}

fn link_life(d: u32) -> String {
    match d {
        0 => "never expires".into(),
        1 => "expires in 24 hours".into(),
        d => format!("expires in {d} days"),
    }
}

/// a job with a progress box: work(progress 0..1) on a thread, done on the UI thread
/// `rel` (lower-case): the clip this job makes a share link for - while a copy of it is made first, the box says so
/// ("Converting AV1 to H.264 so it plays everywhere… 40%") and gets a Cancel (av1-upload-fix, 10 Nov 2026)
pub fn job(a: &AppWindow, title: &str, rel: Option<String>, work: impl FnOnce(&dyn Fn(f64)) -> Result<String, String> + Send + 'static, done: impl FnOnce(&AppWindow, Result<String, String>) + 'static) -> i32 {
    let id = crate::prog_box(title);
    crate::prog_set(id, None, None, Some(-1.0));
    let pct = Arc::new(AtomicU32::new(u32::MAX));
    let p2 = pct.clone();
    let t = slint::Timer::default();
    let cancel_shown = std::cell::Cell::new(false);
    t.start(slint::TimerMode::Repeated, Duration::from_millis(250), move || {
        let v = p2.load(Ordering::Relaxed);
        if v != u32::MAX {
            let making = rel.as_ref().and_then(|r| {
                let p = echo_core::recorder::UP_PROGRESS.lock().ok()?;
                (p.got == 0 && p.rel.to_lowercase() == *r).then(|| crate::cloud_live::stage_text(&p.stage, &p.dest).map(|t| (t, p.stage != "convert")))?
            });
            let txt = match making {
                Some((what, can_cancel)) => {
                    // the steps that can be stopped (Echo Cloud's share copy converts without a stop)
                    if can_cancel && !cancel_shown.replace(true) {
                        cancel_upload_btn(id);
                    }
                    format!("{what}… {}%", v / 10)
                }
                None => format!("{}%", v / 10),
            };
            crate::prog_set(id, None, Some(&txt), Some(v as f32 / 10.0));
        }
    });
    let w = a.as_weak();
    crate::lib_net::bg(
        move || {
            let p = pct;
            work(&|f: f64| p.store((f.clamp(0.0, 1.0) * 1000.0) as u32, Ordering::Relaxed))
        },
        move |r| {
            drop(t);
            crate::prog_done(id, 300);
            if let Some(a) = w.upgrade() {
                done(&a, r)
            }
        },
    );
    id
}

/// audit (9 Nov): Echo's cloud_upload_cancel - a Cancel on upload boxes (stops the upload that's running)
fn cancel_upload_btn(id: i32) {
    crate::prog_btn(id, "Cancel", || {
        if !echo_core::recorder::UP_PROGRESS.lock().map(|p| p.rel.is_empty()).unwrap_or(true) {
            echo_core::recorder::UP_CANCEL.store(true, Ordering::Relaxed);
        }
    });
}

/// a quick job without a box (toast first)
pub fn quick(a: &AppWindow, msg: &str, work: impl FnOnce() -> Result<String, String> + Send + 'static, done: impl FnOnce(&AppWindow, Result<String, String>) + 'static) {
    crate::toast(a, msg);
    let w = a.as_weak();
    crate::lib_net::bg(work, move |r| {
        if let Some(a) = w.upgrade() {
            done(&a, r)
        }
    });
}

fn shared_rels() -> HashSet<String> {
    links_where(|_| true)
}

/// av1-upload-fix: links in the user's own cloud (no Echo Cloud object, "key" empty in links.json) - "Stop sharing"
/// deletes their share copy, also without Echo Cloud, or says how to end a link to the clip itself
fn own_shared_rels() -> HashSet<String> {
    links_where(|e| e["key"].as_str().is_none_or(|k| k.is_empty()))
}

fn links_where(keep: impl Fn(&serde_json::Value) -> bool) -> HashSet<String> {
    let Some(l) = lib() else { return HashSet::new() };
    std::fs::read_to_string(l.thumbs_dir.join("links.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&s).ok())
        .map(|m| m.iter().filter(|(_, e)| keep(e)).map(|(k, _)| k.replace('\\', "/").to_lowercase()).collect())
        .unwrap_or_default()
}

// ---------- the cloud menu ----------
thread_local! {
    static MENU_IDX: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static VIEWS: RefCell<BTreeMap<String, u64>> = const { RefCell::new(BTreeMap::new()) };
    static TIMERS: RefCell<Vec<slint::Timer>> = const { RefCell::new(vec![]) };
    /// clip-thumbs: a fetch_thumbs round is running / cloud-only clips the streaming server was asked for this run
    static FETCHING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static TRIED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static ECM_ALL: RefCell<Vec<serde_json::Value>> = const { RefCell::new(vec![]) };
    /// cleanup: each Echo Cloud file's categories (by path) and the chip just picked (its safe files get ticked)
    static ECM_CATS: RefCell<HashMap<String, Vec<crate::cleanup::Why>>> = RefCell::new(HashMap::new());
    static ECM_PRESEL: RefCell<Option<String>> = const { RefCell::new(None) };
    /// gaps-cloud (row 43): clips with a cloud delete / free-up running (rel lower-case) - a second click waits
    static BUSY: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// gaps-cloud (row 6): clips being uploaded for a share link (their own "Uploading and making a link…" box) -
    /// the background upload box (cloud_live.rs) stays away from them
    pub static LINKING: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Echo's cloudBusy: one cloud delete / free-up per clip at a time. false = already running (toast said so).
fn busy_start(a: &AppWindow, rel: &str) -> bool {
    if !BUSY.with(|b| b.borrow_mut().insert(rel.to_lowercase())) {
        crate::toast(a, "Already working on this clip…");
        return false;
    }
    true
}

fn busy_end(rel: &str) {
    BUSY.with(|b| b.borrow_mut().remove(&rel.to_lowercase()));
}

/// Echo's wmHint (once ever): after the first share link, say the small Echo logo can be switched off
pub fn wm_hint(a: &AppWindow, kind: &str) {
    let c = cfg();
    if if kind == "link" { !c.share_watermark } else { c.watermark_style == "off" } {
        return;
    }
    if !crate::lib_fx::ui_get("wmhint").is_null() {
        return;
    }
    crate::lib_fx::ui_set("wmhint", serde_json::json!(true));
    let w = a.as_weak();
    slint::Timer::single_shot(Duration::from_millis(3200), move || {
        if let Some(a) = w.upgrade() {
            crate::toast(&a, "The small Echo logo in the corner helps other players find Echo. Don't want it? Settings → Clips & Storage → Watermark.");
        }
    });
}

/// only Filen set up (or the link failed on a Filen clip): Filen makes no links other apps can play
fn filen_only(a: &AppWindow) {
    crate::toast(a, "Filen can't make links that play in Discord - add Echo Cloud (free) under Settings → Cloud");
    open_settings(a);
}

fn item(label: &str, value: &str, kind: &str) -> CmItem {
    CmItem { label: label.into(), value: value.into(), kind: kind.into() }
}

/// the card's / player's cloud button. only_link = the player's Share button (link part only, like Echo)
pub fn menu(a: &AppWindow, idx: usize, only_link: bool) {
    let Some(l) = lib() else { return };
    // gaps-cloud (row 40): the player's Share with no cloud set up - Echo says so and opens Settings > Cloud
    if only_link && !on() {
        crate::toast(a, "Sign in to Echo Cloud (free) to make share links");
        open_settings(a);
        return;
    }
    let c = l.clip(idx);
    MENU_IDX.with(|m| m.set(Some(idx)));
    let cm = a.global::<Cm>();
    cm.set_title(c.shown.as_str().into());
    cm.set_def_days(cfg().link_days as i32);
    let mut items = vec![];
    if !on() {
        items.push(item("Set up the cloud first…", "setup", "main"));
        cm.set_chips(false);
    } else {
        items.push(item("Copy share link", "link", "main"));
        cm.set_chips(true);
        if !only_link {
            let shared = shared_rels().contains(&rel_of(&c).to_lowercase());
            if c.cloud.is_some() {
                items.push(item("Download to this PC", "download", ""));
                items.push(item("Delete from the cloud", "delete", "warn"));
            } else if l.in_cloud(idx) {
                items.push(item("Free up space on this PC", "free", ""));
                items.push(item("Delete from the cloud", "delete", "warn"));
            } else {
                items.push(item("Upload to the cloud", "upload", ""));
            }
            if ((c.cloud.is_some() || l.in_cloud(idx) || shared) && has_ec()) || own_shared_rels().contains(&rel_of(&c).to_lowercase()) {
                items.push(item("Stop sharing", "unshare", ""));
            }
        }
    }
    cm.set_items(ModelRc::new(VecModel::from(items)));
    cm.set_open(true);
}

/// L key / Home "Share a link": the link straight away (default lifetime)
pub fn quick_link(a: &AppWindow, idx: usize) {
    if !on() {
        crate::toast(a, "Set up the cloud first (Settings → Cloud)");
        open_settings(a);
        return;
    }
    copy_link(a, idx, None);
}

pub fn open_settings(a: &AppWindow) {
    if a.get_player_open() {
        a.global::<crate::Play>().invoke_close();
    }
    a.invoke_page("Settings".into());
    a.global::<crate::S>().invoke_pick("cloud".into());
}

pub fn copy_link(a: &AppWindow, idx: usize, days: Option<u32>) {
    let (Some(l), Some(r)) = (lib(), rec()) else { return };
    let c = l.clip(idx);
    let rel = rel_of(&c);
    let local_only = c.cloud.is_none() && !l.in_cloud(idx);
    let path = c.path.to_string_lossy().to_string();
    let conf = cfg();
    let life = link_life(days.unwrap_or(conf.link_days));
    // gaps-cloud (row 43): Echo's copyLink - only Filen set up: Filen makes no links that play in Discord
    let has_filen = conf.cloud_accounts.iter().any(|x| x.provider == "filen");
    if conf.cloud == "filen" && (!local_only || !has_ec()) {
        filen_only(a);
        return;
    }
    let multi_filen = conf.cloud == "multi" && has_filen;
    let title = if local_only { "Uploading and making a link…" } else { "Making a link…" };
    // av1-upload-fix: also a clip that's already in the cloud - an AV1 clip's link may upload its H.264 share copy
    LINKING.with(|s| s.borrow_mut().insert(rel.to_lowercase()));
    let key = rel.to_lowercase();
    // cleanup: remembered for Manage space's "Share link expired" (uploaded only for this link?)
    let hist = (l.thumbs_dir.clone(), rel.clone(), days.unwrap_or(conf.link_days));
    let path2 = path.clone();
    let id = job(
        a,
        title,
        Some(key.clone()),
        move |p| {
            let url = if local_only { r.upload_and_link(&path, days, p)? } else { r.cloud_link(&rel, days, p)? };
            crate::lib_net::copy_text(&url)?;
            Ok(url)
        },
        move |a, res| {
            LINKING.with(|s| s.borrow_mut().remove(&key));
            match res {
                Ok(url) => {
                    crate::cleanup::link_made(&hist.0, &hist.1, &url, hist.2, local_only);
                    // cloud-savings: once, the first time a clip goes up only for its link
                    let gone = local_only && crate::lib_fx::ui_get("sharegone").is_null() && rec().is_some_and(|r| r.cloud_share_only(&path2));
                    if gone {
                        crate::lib_fx::ui_set("sharegone", serde_json::json!(true));
                        crate::toast(a, format!("Uploaded - link copied, {life}. This upload goes away when the link expires"));
                    } else {
                        // 2.0.2: the box turns into this note, with Open (the share page in the browser)
                        let u = url.clone();
                        crate::notes::say_act(crate::notes::Kind::Done, if local_only { format!("Uploaded - link copied, {life}") } else { format!("Link copied - {life}") }, "Open", move || crate::open_url(&u));
                    }
                    wm_hint(a, "link");
                }
                // several clouds and the clip sits on Filen: the Filen message instead of rclone's error
                Err(_) if multi_filen && !local_only => filen_only(a),
                Err(e) => crate::toast(a, e),
            }
            sync_ui(a);
        },
    );
    if local_only {
        cancel_upload_btn(id);
    }
}

fn menu_pick(a: &AppWindow, v: &str) {
    a.global::<Cm>().set_open(false);
    let Some(idx) = MENU_IDX.with(|m| m.get()) else { return };
    let (Some(l), Some(r)) = (lib(), rec()) else { return };
    let c = l.clip(idx);
    let rel = rel_of(&c);
    let path = c.path.to_string_lossy().to_string();
    match v {
        "setup" => open_settings(a),
        "link" => copy_link(a, idx, None),
        _ if v.starts_with("link:") => copy_link(a, idx, v[5..].parse().ok()),
        "upload" => {
            if let Some(rr) = r.rel_of(&path) {
                echo_core::recorder::UP_SKIPPED.lock().unwrap().retain(|x| x != &rr);
            }
            let auto = cfg().auto_copy_link;
            let rel2 = rel.clone();
            // gaps-cloud (row 6): like Echo, a toast only - the upload's own box (progress, ETA, Cancel) comes from
            // the background upload poll (cloud_live.rs), so there aren't two boxes for one upload
            quick(
                a,
                "Uploading…",
                move || {
                    r.cloud_upload(&path)?;
                    if auto {
                        if let Ok(u) = r.cloud_link(&rel2, None, &|_| {}) {
                            let _ = crate::lib_net::copy_text(&u);
                            return Ok("link".into());
                        }
                    }
                    Ok(String::new())
                },
                |a, res| {
                    match res {
                        Ok(x) => crate::toast(a, if x == "link" { "Uploaded - link copied" } else { "Uploaded" }),
                        Err(e) => crate::toast(a, e),
                    }
                    sync_ui(a);
                },
            );
        }
        "free" => {
            if BUSY.with(|b| b.borrow().contains(&rel.to_lowercase())) {
                crate::toast(a, "Already working on this clip…");
                return;
            }
            let shown = c.shown.to_string();
            let w = a.as_weak();
            let r2 = r;
            let p2 = path.clone();
            // av1-upload-fix (coordinator, 10 Nov 2026): ask first only when the cloud keeps a copy, not the clip
            // itself (an AV1 clip's H.264 copy / Echo Cloud's smaller copy); the same file stays one click
            crate::lib_net::bg(move || r2.cloud_copy_info(&p2), move |(kind, cloud, local)| {
                let Some(a) = w.upgrade() else { return };
                let fsz = crate::library::fmt_size;
                let go = move |a: &AppWindow| free_local(a, r, rel, path);
                match kind {
                    "h264" => crate::confirm_box(&a, "Free up space on this PC?",
                        &format!("The cloud keeps an H.264 copy of \"{shown}\" (plays everywhere, {}). The AV1 original ({}) is deleted from this PC.", fsz(cloud), fsz(local)),
                        "Free up space", go),
                    "smaller" => crate::confirm_box(&a, "Free up space on this PC?",
                        &format!("Echo Cloud keeps a smaller copy of \"{shown}\" (up to 1080p, only the mixed sound track, {}). The original ({}) is deleted from this PC.", fsz(cloud), fsz(local)),
                        "Free up space", go),
                    _ => go(&a),
                }
            });
        }
        "download" => download(a, idx),
        "delete" => {
            let msg = if c.cloud.is_some() {
                format!("\"{}\" is only in the cloud - it will be gone for good.", c.shown)
            } else {
                format!("\"{}\" stays on this PC. If its share link plays this cloud copy, the link stops working too.", c.shown)
            };
            let only = c.cloud.is_some();
            if BUSY.with(|b| b.borrow().contains(&rel.to_lowercase())) {
                crate::toast(a, "Already working on this clip…");
                return;
            }
            let go = move |a: &AppWindow| {
                if !busy_start(a, &rel) {
                    return;
                }
                if only && a.get_player_open() {
                    a.global::<crate::Play>().invoke_close();
                }
                let rel2 = rel.clone();
                quick(a, "Deleting from the cloud…", move || r.cloud_delete(&rel2).map(|_| String::new()), move |a, res| {
                    busy_end(&rel);
                    match res {
                        Ok(_) => crate::toast(a, "Deleted from the cloud"),
                        Err(e) => crate::toast(a, e),
                    }
                    sync_ui(a);
                });
            };
            // gaps-cloud (row 43): a clip that's also on this PC asks only when it has a share link (Echo)
            if only || shared_rels().contains(&rel_of(&c).to_lowercase()) {
                crate::confirm_box(a, "Delete from the cloud?", &msg, "Delete", go);
            } else {
                go(a);
            }
        }
        "unshare" => quick(a, "Removing the link…", move || r.cloud_unshare(&rel).map(|_| String::new()), |a, res| {
            match res {
                Ok(_) => crate::toast(a, "Link removed - it no longer works"),
                Err(e) => crate::toast(a, e),
            }
            sync_ui(a);
        }),
        _ => {}
    }
}

/// "Free up space on this PC" once it's decided: delete the PC copy (the cloud keeps the clip)
fn free_local(a: &AppWindow, r: &'static Recorder, rel: String, path: String) {
    if !busy_start(a, &rel) {
        return;
    }
    if a.get_player_open() {
        a.global::<crate::Play>().invoke_close();
    }
    let w = a.as_weak();
    slint::Timer::single_shot(Duration::from_millis(200), move || {
        let Some(a) = w.upgrade() else {
            busy_end(&rel);
            return;
        };
        // cloud-savings: say so when what stays is Echo Cloud's smaller copy / an AV1 clip's H.264 copy (av1-upload-fix)
        quick(&a, "Freeing up space…", move || r.cloud_remove_local(&path).map(|_| r.cloud_copy_kind(&path).to_string()), move |a, res| {
            busy_end(&rel);
            match res {
                Ok(s) if s == "smaller" => crate::toast(a, "Removed from this PC - Echo Cloud keeps its smaller copy"),
                Ok(s) if s == "h264" => crate::toast(a, "Removed from this PC - the cloud keeps its H.264 copy (plays everywhere)"),
                Ok(_) => crate::toast(a, "Removed from this PC - still in the cloud"),
                Err(e) => crate::toast(a, e),
            }
            sync_ui(a);
        });
    });
}

/// cloud-only clip -> this PC ("Download to this PC" in the cloud menu), with Echo's download progress
pub fn download(a: &AppWindow, idx: usize) {
    fetch(a, idx, "Downloaded to this PC", Box::new(|_, _| {}));
}

// ---------- gaps-cloud (row 4): cloud-only clips - download first, then do it (Echo's ensureLocal) ----------
type Then = Box<dyn FnOnce(&AppWindow, usize)>;
thread_local! {
    /// clip downloads running (rel lower-case) -> what to do once the clip is on this PC (a second click / the
    /// player joins the download that's already running: one box, one "Downloaded")
    static LOCAL_JOBS: RefCell<HashMap<String, Vec<Then>>> = RefCell::new(HashMap::new());
    /// each running download's progress (rel lower-case -> (0..1, "12 MB of 58 MB · 20% · about 30 s left")): the
    /// player shows it inside the video while that clip is open (cloud_live.rs)
    pub static DL_SHOWN: RefCell<HashMap<String, (f32, String)>> = RefCell::new(HashMap::new());
}

/// the clip is only in the cloud: its file isn't on this PC
pub fn cloud_only(c: &Clip) -> bool {
    c.cloud.is_some() && !c.path.exists()
}

/// Echo's ensureLocal: a cloud-only clip is downloaded first (box "Downloading from the cloud…", toast
/// "Downloaded"), then `then` runs with the clip's index (now a normal clip on this PC). A clip on this PC: at once.
pub fn ensure_local(a: &AppWindow, idx: usize, then: impl FnOnce(&AppWindow, usize) + 'static) {
    fetch(a, idx, "Downloaded", Box::new(then));
}

/// An action on a cloud-only clip that needs its file. Echo's download-first actions (Copy, Copy ▾, Edit, Show in
/// folder; the player's Frame, MP3, Trim, Share, Thumbnail…) download it and then run `redo`; the few that only make
/// sense on this PC say so. true = handled here (the caller stops).
pub fn needs_local(a: &AppWindow, idx: usize, what: &str, redo: impl FnOnce(&AppWindow, usize) + 'static) -> bool {
    let Some(l) = lib() else { return false };
    if idx >= l.len() || !cloud_only(&l.clip(idx)) {
        return false;
    }
    if what.starts_with("copy") || matches!(what, "folder" | "edit" | "cap" | "frame" | "thumb" | "audio" | "wav" | "gif" | "share" | "trim") {
        ensure_local(a, idx, redo);
        return true;
    }
    if matches!(what, "smaller" | "smaller3" | "av1" | "rename" | "replace") {
        crate::toast(a, "This clip is only in the cloud - download it to this PC first");
        return true;
    }
    false
}

/// a card's button (Lib.act) on a cloud-only clip: download first, then the same button on its card again
pub fn card_needs_local(a: &AppWindow, row: i32, what: &str) -> bool {
    let Some(l) = lib() else { return false };
    let Some(idx) = l.index_at(row as usize) else { return false };
    let what2 = what.to_string();
    needs_local(a, idx, what, move |a, i| {
        let Some(l) = lib() else { return };
        // Home's cards keep their spot; library cards: where the clip is in the view now
        let r = if row as usize >= crate::library::HOME_BASE { Some(row as usize) } else { l.view_row_of(i) };
        if let Some(r) = r.filter(|&r| l.index_at(r) == Some(i)) {
            a.global::<crate::Lib>().invoke_act(r as i32, what2.as_str().into());
        }
    })
}

/// Home's Copy for Discord / ▾ on a cloud-only latest clip: download first, then the same button again
pub fn home_needs_local(a: &AppWindow, what: &str) -> bool {
    let Some(l) = lib() else { return false };
    let Some(idx) = l.index_at(crate::library::HOME_BASE) else { return false };
    let what2 = what.to_string();
    needs_local(a, idx, what, move |a, i| {
        if lib().and_then(|l| l.index_at(crate::library::HOME_BASE)) == Some(i) {
            a.global::<crate::HomeState>().invoke_act(what2.as_str().into());
        }
    })
}

/// the player's buttons (Play.act / Save copy) on a cloud-only clip: download first, then the same button again
/// while that clip is still open (Echo's ensureLocal on pcopy / pshow / pframe / pmp3 / ptrim / pedit / pshare / pthumb)
pub fn player_needs_local(a: &AppWindow, idx: usize, what: &str) -> bool {
    let what2 = what.to_string();
    needs_local(a, idx, what, move |a, i| {
        if !a.get_player_open() || crate::cloud_live::open_clip() != Some(i) {
            return;
        }
        if what2 == "trim" {
            a.global::<crate::Play>().invoke_trim_save();
        } else {
            a.global::<crate::Play>().invoke_act(what2.as_str().into());
        }
    })
}

fn fetch(a: &AppWindow, idx: usize, done_msg: &'static str, then: Then) {
    let (Some(l), Some(r)) = (lib(), rec()) else { return };
    let c = l.clip(idx);
    if !cloud_only(&c) {
        then(a, idx);
        return;
    }
    let rel = rel_of(&c);
    let key = rel.to_lowercase();
    let mut then = Some(then);
    let running = LOCAL_JOBS.with(|j| {
        let mut j = j.borrow_mut();
        match j.get_mut(&key) {
            Some(v) => {
                v.extend(then.take());
                true
            }
            None => {
                j.insert(key.clone(), then.take().into_iter().collect());
                false
            }
        }
    });
    if running {
        return;
    }
    let size = c.size.max(1);
    let id = crate::prog_box("Downloading from the cloud…");
    crate::prog_set(id, None, Some("Starting…"), Some(0.0));
    DL_SHOWN.with(|d| d.borrow_mut().insert(key.clone(), (0.0, "Starting…".into())));
    let (rel2, key2) = (rel.clone(), key.clone());
    let aw = a.as_weak();
    let t = slint::Timer::default();
    // 20x a second; the bars (bottom box, player overlay) and their text show the smoothed bytes (dl_smooth.rs: the
    // bytes arrive in 1 MiB lumps, the raw value made the bar move-stop-move - "choppy", 10 Nov 2026). Slint's linear
    // width animation (100 / 200 ms, longer than this tick) joins the steps into one steady motion.
    let mut sm = crate::dl_smooth::Smooth::new(std::time::Instant::now());
    t.start(slint::TimerMode::Repeated, Duration::from_millis(50), move || {
        let (got, total) = Recorder::cloud_dl_progress(&rel2);
        let tot = if total > 0 { total } else { size };
        let got = sm.step(got, tot, std::time::Instant::now());
        let f = (got as f64 / tot as f64).clamp(0.0, 1.0);
        // 2.0.2: speed + time left ("4.2 MB/s · 12 s left"); the % until there's a fair guess
        let txt = format!(
            "{} of {} · {}",
            crate::library::fmt_size(got),
            crate::library::fmt_size(tot),
            crate::notes::rate(id, got, tot).unwrap_or_else(|| format!("{:.0}%", f * 100.0))
        );
        crate::prog_set(id, None, Some(&txt), Some((f * 100.0) as f32));
        // the player shows this download: its bar right away, not at the player's next tick
        if let Some(a) = aw.upgrade().filter(|_| crate::cloud_live::open_clip() == Some(idx)) {
            let pl = a.global::<crate::Play>();
            if pl.get_cl_on() && pl.get_cl_title().starts_with("Downloading") {
                pl.set_cl_pct(f as f32);
            }
        }
        DL_SHOWN.with(|d| d.borrow_mut().insert(key2.clone(), (f as f32, txt)));
    });
    let w = a.as_weak();
    crate::lib_net::bg(move || r.cloud_download(&rel), move |res| {
        drop(t);
        if res.is_ok() {
            crate::prog_set(id, None, None, Some(100.0)); // the smoothed bar may be a little behind: full at the end
        }
        crate::prog_done(id, 300);
        DL_SHOWN.with(|d| d.borrow_mut().remove(&key));
        let thens = LOCAL_JOBS.with(|j| j.borrow_mut().remove(&key)).unwrap_or_default();
        let Some(a) = w.upgrade() else { return };
        match res {
            Ok(p) => {
                sync_ui(&a);
                // 2.0.2: a plain Download (nothing waiting on it) gets Play on its note
                let lib_idx = lib().and_then(|l| l.index_of_path(Path::new(&p)));
                match lib_idx.filter(|_| thens.is_empty()) {
                    Some(i) => {
                        let w = a.as_weak();
                        crate::notes::say_act(crate::notes::Kind::Done, done_msg, "Play", move || {
                            if let (Some(a), Some(l)) = (w.upgrade(), lib()) {
                                crate::hdr_home::open_clip(&a, &l, i);
                            }
                        });
                    }
                    None => crate::toast(&a, done_msg),
                }
                // the clip turned into a normal clip in place (library sync_disk); look it up by its file to be sure
                let at = lib().and_then(|l| l.index_of_path(Path::new(&p)).or_else(|| (!cloud_only(&l.clip(idx))).then_some(idx)));
                if let Some(i) = at {
                    for f in thens {
                        f(&a, i);
                    }
                }
            }
            Err(e) => crate::toast(&a, e),
        }
    });
}

// ---------- delete / move hooks ----------
/// Delete "Everywhere": this PC (Recycle Bin / Trash) + the cloud copies
pub fn delete_everywhere(a: &AppWindow, ids: Vec<usize>) {
    let (Some(l), Some(r)) = (lib(), rec()) else { return };
    let rels: Vec<String> = ids.iter().filter(|&&i| l.in_cloud(i) || l.clip(i).cloud.is_some()).map(|&i| rel_of(&l.clip(i))).collect();
    let local: Vec<usize> = ids.iter().copied().filter(|&i| l.clip(i).cloud.is_none()).collect();
    if a.get_player_open() {
        a.global::<crate::Play>().invoke_close();
    }
    let w = a.as_weak();
    // r2-threads (L5): this PC's files go on a worker first
    slint::Timer::single_shot(Duration::from_millis(250), move || l.delete_clips_bg(&local, |_, _| {}, move |_k, err| {
        let Some(a) = w.upgrade() else { return };
        let total = ids.len();
        if let Some(e) = err {
            crate::toast(&a, e);
        }
        let n = rels.len();
        quick(&a, &format!("Deleting {} from the cloud…", if n == 1 { "it".to_string() } else { format!("{n} clips") }), move || {
            let mut first = None;
            for rel in &rels {
                if let Err(e) = r.cloud_delete(rel) {
                    first.get_or_insert(e);
                }
            }
            first.map_or(Ok(String::new()), Err)
        }, move |a, res| {
            match res {
                // Echo's texts (PORT_LEFTOVERS L11)
                Ok(_) => crate::toast(a, if total <= 1 { "Deleted from this PC and the cloud".to_string() } else { format!("Deleted {total} clips from this PC and the cloud") }),
                Err(e) => crate::toast(a, e),
            }
            sync_ui(a);
        });
    }));
}

/// only cloud-only clips picked: one question, then they go from the cloud
pub fn delete_cloud_only(a: &AppWindow, ids: Vec<usize>) {
    let (Some(l), Some(r)) = (lib(), rec()) else { return };
    let rels: Vec<String> = ids.iter().map(|&i| rel_of(&l.clip(i))).collect();
    let n = rels.len();
    let msg = if n == 1 { format!("\"{}\" is only in the cloud - it will be gone for good.", l.clip(ids[0]).shown) } else { format!("These {n} clips are only in the cloud - they will be gone for good.") };
    crate::confirm_box(a, "Delete from the cloud?", &msg, "Delete", move |a| {
        quick(a, "Deleting from the cloud…", move || {
            for rel in &rels {
                r.cloud_delete(rel)?;
            }
            Ok(String::new())
        }, move |a, res| {
            match res {
                // Echo's toast (PORT_LEFTOVERS L11)
                Ok(_) => crate::toast(a, if n == 1 { "Deleted from the cloud".to_string() } else { format!("Deleted {n} clips from the cloud") }),
                Err(e) => crate::toast(a, e),
            }
            sync_ui(a);
        });
    });
}

/// Echo's trim_clip(replace): a clip that's in the cloud was overwritten with its trimmed part - upload the cut
/// version over the cloud copy (in the background; the upload box shows it). For the player's Replace (player
/// pl_actions.rs replace(), after the rename).
pub fn after_replace(path: &Path) {
    let Some(r) = rec() else { return };
    let p = path.to_string_lossy().to_string();
    std::thread::spawn(move || {
        if let Some(rel) = r.rel_of(&p) {
            if r.cloud_files().iter().any(|f| f.rel == rel) {
                let _ = r.cloud_upload(&p);
            }
        }
    });
}

/// gaps-cloud (row 22): cloud-only clips the user already confirmed (Tidy up's Biggest clips): gone from the cloud
/// for good, no second question. Toasts the first error; the library refreshes when it's done.
pub fn delete_cloud_only_now(a: &AppWindow, ids: Vec<usize>) {
    let (Some(l), Some(r)) = (lib(), rec()) else { return };
    let rels: Vec<(String, String)> = ids.iter().map(|&i| (rel_of(&l.clip(i)), l.clip(i).shown.clone())).collect();
    if rels.is_empty() {
        return;
    }
    let w = a.as_weak();
    crate::lib_net::bg(
        move || {
            let mut first = None;
            for (rel, name) in &rels {
                if let Err(e) = r.cloud_delete(rel) {
                    first.get_or_insert(format!("{name}: {e}"));
                }
            }
            first
        },
        move |err| {
            let Some(a) = w.upgrade() else { return };
            if let Some(e) = err {
                crate::toast(&a, e);
            }
            sync_ui(&a);
        },
    );
}

/// a clip / folder in the cloud was moved or renamed on this PC: its cloud copy + share link follow
pub fn follow_move(pairs: Vec<(String, String)>) {
    let Some(r) = rec() else { return };
    if pairs.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for (o, n) in pairs {
            r.cloud_follow_move(o, n);
        }
    });
}

// ---------- cloud-only clips ----------
/// (rel, size, modified unix s) of clips that are in the cloud but not on this PC (from Echo's cloud.json)
pub fn cloud_only_files() -> Vec<(String, u64, u64)> {
    let Some(r) = rec() else { return vec![] };
    r.cloud_files().into_iter().filter(|f| !f.local).map(|f| (f.rel, f.size, f.modified)).collect()
}

thread_local! {
    /// r2-threads (L6): cloud-only clip file -> (when asked, its stream address; None = not in the cloud / no server)
    static URLS: RefCell<HashMap<PathBuf, (Instant, Option<String>)>> = RefCell::new(HashMap::new());
    /// files whose address is being worked out on a worker right now -> who waits for it (one `ready` per place
    /// in the code that asked - they ask again on every tick / redraw)
    #[allow(clippy::type_complexity)]
    static RESOLVING: RefCell<HashMap<PathBuf, Vec<(&'static std::panic::Location<'static>, Box<dyn FnOnce()>)>>> = RefCell::new(HashMap::new());
    /// last keep-alive of the streaming server while a cloud clip plays
    static KEEP_AT: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Echo's ensureBase: the address is asked again when older than a minute (Echo stops the streaming server after
/// 10 minutes without use; asking starts it again)
const URL_FRESH: Duration = Duration::from_secs(60);

/// blocking (worker threads only): the stream address of cloud-only clip file `p` (`root` = the clips folder).
/// The first call starts Echo's rclone streaming server (about a second per port it tries).
fn stream_url(root: &Path, p: &Path) -> Option<String> {
    let r = rec()?;
    let t0 = Instant::now();
    let rel = p.strip_prefix(root).ok()?.to_string_lossy().replace('\\', "/");
    let f = r.cloud_files().into_iter().find(|f| f.rel.eq_ignore_ascii_case(&rel))?;
    let base = r.cloud_stream_base();
    // a slow start (rclone connecting to every cloud, new Echo Cloud keys) shows in the log
    if t0.elapsed() > Duration::from_secs(2) {
        crate::lib_net::log_line(&format!("cloud: stream address after {:.1} s{}", t0.elapsed().as_secs_f32(), base.as_ref().err().map(|e| format!(" ({e})")).unwrap_or_default()));
    }
    let base = base.ok().filter(|b| !b.is_empty())?;
    r.cloud_touch(&f.rel); // cloud-savings: Echo Cloud keeps clips in use in its fast storage
    let path = if f.path.is_empty() { f.rel } else { f.path };
    let enc: Vec<String> = path.split('/').map(url_enc).collect();
    Some(format!("{base}/{}", enc.join("/")))
}

/// r2-threads (L6): is `play_path(p)` ready without waiting? A file on this PC / an address: yes. A cloud-only
/// clip: once its stream address was worked out on a worker (started here; `ready` runs when it is).
#[track_caller]
pub fn play_ready(p: &Path, ready: impl FnOnce() + 'static) -> bool {
    let at = std::panic::Location::caller();
    if p.exists() || p.to_string_lossy().starts_with("http") {
        return true;
    }
    if URLS.with(|u| u.borrow().get(p).is_some_and(|x| x.0.elapsed() < URL_FRESH)) {
        return true;
    }
    let Some(l) = lib() else { return true };
    // already asked: this caller waits too. (Only the first caller's `ready` ran before - when the cloud overlay
    // asked first, the player's "redraw, then load" never came: the clip sat on "Starting the stream…" until
    // something else redrew the window, 10 Nov 2026.)
    let asked = RESOLVING.with(|r| {
        let mut r = r.borrow_mut();
        let first = !r.contains_key(p);
        let w = r.entry(p.to_path_buf()).or_default();
        if !w.iter().any(|(l, _)| *l == at) {
            w.push((at, Box::new(ready)));
        }
        !first
    });
    if asked {
        return false;
    }
    let (root, p1, p2) = (l.root(), p.to_path_buf(), p.to_path_buf());
    crate::lib_net::bg(
        move || stream_url(&root, &p1),
        move |url| {
            let waiting = RESOLVING.with(|r| r.borrow_mut().remove(&p2)).unwrap_or_default();
            URLS.with(|u| u.borrow_mut().insert(p2, (Instant::now(), url)));
            for (_, f) in waiting {
                f();
            }
        },
    );
    false
}

/// r2-threads (L6): while a cloud-only clip is open, ask for the streaming server every 2 minutes (Echo's
/// setInterval ensureBase) so it doesn't stop under the player
pub fn keep_stream_alive() {
    if KEEP_AT.with(|k| k.get().is_some_and(|t| t.elapsed() < Duration::from_secs(120))) {
        return;
    }
    KEEP_AT.with(|k| k.set(Some(Instant::now())));
    if let Some(r) = rec() {
        std::thread::spawn(move || {
            let _ = r.cloud_stream_base();
        });
    }
}

/// the player opens a cloud-only clip: Echo's rclone streaming server address instead of the missing file.
/// r2-threads (L6): never waits - the address comes from `play_ready` (worked out on a worker); not ready yet =
/// the missing file as it is (callers ask `play_ready` first)
pub fn play_path(p: &Path) -> PathBuf {
    if p.exists() || p.to_string_lossy().starts_with("http") {
        return p.to_path_buf();
    }
    match URLS.with(|u| u.borrow().get(p).and_then(|x| x.1.clone())) {
        Some(url) => PathBuf::from(url),
        None => p.to_path_buf(),
    }
}

fn url_enc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

/// thumbnails + lengths for cloud-only clips: first the picture the Echo Cloud account keeps (clip_thumbs.rs, a small
/// JPEG - no streaming), else Echo's cloud_thumb over the streaming server, one at a time and once per run each
/// (clip-thumbs, 12 Nov 2026: a clip it couldn't read was tried again every 20 s, with ffmpeg + rclone each time)
fn fetch_thumbs() {
    let Some(l) = lib() else { return };
    if FETCHING.with(|f| f.get()) {
        return;
    }
    let all: Vec<String> = l.all().iter().filter(|c| c.cloud.is_some() && !c.gone.get() && !c.thumb().exists()).filter_map(|c| c.cloud.clone()).collect();
    if all.is_empty() {
        return;
    }
    let (ask, rest): (Vec<String>, Vec<String>) = all.into_iter().partition(|rel| crate::clip_thumbs::may_ask(rel));
    let ask: Vec<String> = ask.into_iter().take(80).collect();
    let old: Vec<String> = TRIED.with(|t| {
        let mut t = t.borrow_mut();
        let pick: Vec<String> = rest.into_iter().filter(|rel| !t.contains(rel)).take(12).collect();
        t.extend(pick.iter().cloned());
        pick
    });
    if ask.is_empty() && old.is_empty() {
        return;
    }
    let Some(r) = rec() else { return };
    let token = token();
    FETCHING.with(|f| f.set(true));
    crate::lib_net::bg(
        move || {
            let got = !ask.is_empty() && crate::clip_thumbs::fetch(&token, &ask);
            for rel in &old {
                let _ = r.cloud_thumb(rel);
            }
            got || !old.is_empty()
        },
        |got| {
            FETCHING.with(|f| f.set(false));
            if let Some(l) = lib() {
                l.changed_all();
            }
            // Home's big "latest clip" picture isn't a card: it follows with the next sync (once, when pictures came)
            if got {
                crate::UI_SYNC.with(|f| {
                    if let Some(f) = f.borrow().as_ref() {
                        f(false)
                    }
                });
            }
        },
    );
}

/// "Shared · N views" on cards (Echo's share_views, every 5 min while signed in)
pub fn views_of(rel: &str) -> u64 {
    VIEWS.with(|v| v.borrow().get(&rel.to_lowercase()).copied().unwrap_or(0))
}

fn refresh_views() {
    let Some(r) = rec() else { return };
    if cfg().ec_token.is_empty() {
        return;
    }
    crate::lib_net::bg(move || r.share_views().unwrap_or_default(), |m| {
        VIEWS.with(|v| *v.borrow_mut() = m.into_iter().map(|(k, n)| (k.replace('\\', "/").to_lowercase(), n)).collect());
        if let Some(l) = lib() {
            l.changed_all();
        }
    });
}

// ---------- Settings > Cloud ----------
/// Settings buttons this module owns. true = handled.
pub fn act(a: &AppWindow, store: &Rc<RefCell<crate::settings::Store>>, r: &Rc<dyn Fn()>, act: &str) -> bool {
    let Some(rc) = rec() else { return false };
    let reload = {
        let (store, r) = (store.clone(), r.clone());
        move || {
            store.borrow_mut().reload();
            r();
        }
    };
    match act {
        "ecsignin" => ec_email(a, None, Rc::new(reload)),
        "ecsignout" => {
            let id = cfg().cloud_accounts.iter().find(|x| x.provider == "echocloud").map(|x| x.id);
            crate::confirm_box(a, "Sign out of Echo Cloud?", "Your clips stay in Echo Cloud - sign in again with the same email to get them back.", "Sign out", move |a| {
                let Some(id) = id else { return };
                quick(a, "Signing out…", move || rc.cloud_remove_account(id).map(|_| String::new()), move |a, res| {
                    match res {
                        Ok(_) => crate::toast(a, "Signed out of Echo Cloud"),
                        Err(e) => crate::toast(a, e),
                    }
                    crate::rec_core::sync_soon();
                    reload();
                    sync_ui(a);
                });
            });
        }
        // Echo's ecPlans: Upgrade toggles the plan cards under the account line
        "ecplans" => toggle_plans(a),
        // Echo's #ecdel: type your email to confirm, then everything in Echo Cloud is deleted for good
        "ecdelete" => {
            let email = cfg().ec_email.trim().to_string();
            let paid = cfg().cloud_accounts.iter().find(|x| x.provider == "echocloud").and_then(|x| space_of(x.id)).is_some_and(|(_, t, _)| t as f64 > 10.5e9);
            let text = format!(
                "Everything in Echo Cloud is deleted for good: all clips, share links and the account itself. Clips on this PC stay. {}Type your email address to confirm.",
                if paid { "Cancel your plan under Manage plan first. " } else { "" }
            );
            let ph = if email.is_empty() { "you@example.com".to_string() } else { email };
            crate::ask(a, "Delete your Echo Cloud account?", &text, &[("Cancel", "", ""), ("Delete account", "ok", "delb")], Some(&ph), "ok", move |a, v, typed| {
                if v != "ok" {
                    return;
                }
                let typed = typed.trim().to_string();
                let reload = reload.clone();
                crate::toast(a, "Deleting your Echo Cloud account…");
                quick(a, "Deleting your Echo Cloud account…", move || rc.echocloud_delete_account(&typed).map(|_| String::new()), move |a, res| {
                    match res {
                        Ok(_) => crate::toast(a, "Echo Cloud account deleted"),
                        Err(e) => crate::toast(a, e),
                    }
                    reload();
                    sync_ui(a);
                    load_info(a);
                });
            });
        }
        "ecmanage" => quick(a, "Opening your subscription…", move || rc.echocloud_manage().map(|_| String::new()), |a, res| {
            if let Err(e) = res {
                crate::toast(a, e)
            }
        }),
        "ecspace" => space_open(a),
        "syncnow" => {
            crate::rec_core::sync_soon();
            crate::toast(a, "Syncing…");
        }
        "cloudoff" => {
            crate::confirm_box(a, "Sign out of every cloud?", "Clips stay in the clouds and on this PC; Echo just stops syncing.", "Sign out", move |a| {
                quick(a, "Signing out…", move || {
                    rc.cloud_disconnect();
                    Ok(String::new())
                }, move |a, _| {
                    reload();
                    sync_ui(a);
                });
            });
        }
        _ if act.starts_with("cloudrm:") => {
            let Ok(id) = act[8..].parse::<u32>() else { return true };
            crate::confirm_box(a, "Remove this cloud?", "Clips already in it stay there, but Echo stops showing and syncing them. New clips go to your other clouds.", "Remove", move |a| {
                quick(a, "Removing…", move || rc.cloud_remove_account(id).map(|_| String::new()), move |a, res| {
                    match res {
                        Ok(_) => crate::toast(a, "Cloud removed"),
                        Err(e) => crate::toast(a, e),
                    }
                    crate::rec_core::sync_soon();
                    reload();
                    sync_ui(a);
                    load_info(a);
                });
            });
        }
        _ if act.starts_with("cloud:") => connect(a, &act[6..], Rc::new(reload)),
        _ => return false,
    }
    true
}

/// Echo's drawEc email step (PORT_LEFTOVERS L22): prefilled with the last email (`ec_email`); an empty email or a
/// failed send asks again instead of ending the sign-in.
fn ec_email(a: &AppWindow, prefill: Option<String>, reload: Rc<dyn Fn()>) {
    let Some(rc) = rec() else { return };
    let prefill = prefill.unwrap_or_else(|| cfg().ec_email.trim().to_string());
    crate::ask(a, "Sign in to Echo Cloud", "10 GB free, nothing to set up. You get a 6-digit code by email - no password.", &[("Cancel", "", ""), ("Send code", "ok", "primary")], Some("you@email.com"), "ok", move |a, v, email| {
        if v != "ok" {
            return;
        }
        let email = email.trim().to_lowercase();
        if email.is_empty() {
            crate::toast(a, "Enter your email");
            ec_email(a, Some(String::new()), reload);
            return;
        }
        let e2 = email.clone();
        quick(a, "Sending…", move || rc.echocloud_start(&e2).map(|_| String::new()), move |a, res| match res {
            Ok(_) => ec_code(a, email, reload),
            Err(e) => {
                crate::toast(a, e);
                ec_email(a, Some(email), reload);
            }
        });
    });
    a.global::<crate::Dlg>().set_text(prefill.into());
}

/// Echo's drawEc code step: digits only, exactly 6; a wrong code asks again; "Other email" goes back.
fn ec_code(a: &AppWindow, email: String, reload: Rc<dyn Fn()>) {
    let Some(rc) = rec() else { return };
    let msg = format!("We emailed a code to {email}. It works for 10 minutes (check spam if it doesn't show up).");
    crate::ask(a, "Check your email", &msg, &[("Cancel", "", ""), ("Other email", "back", ""), ("Sign in", "ok", "primary")], Some("123456"), "ok", move |a, v, code| {
        if v == "back" {
            ec_email(a, Some(email), reload);
            return;
        }
        if v != "ok" {
            return;
        }
        let code: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
        if code.len() != 6 {
            crate::toast(a, "Enter the 6-digit code from the email");
            ec_code(a, email, reload);
            return;
        }
        let e2 = email.clone();
        quick(a, "Signing in…", move || rc.cloud_connect("echocloud", &e2, "", &code, &Default::default()).map(|_| String::new()), move |a, res| match res {
            Ok(_) => {
                crate::toast(a, "Signed in to Echo Cloud");
                crate::rec_core::sync_soon();
                reload();
                load_info(a);
                crate::game_sync::soon(); // cloud-games: this account's game icons
                crate::clip_thumbs::soon(); // clip-thumbs: this PC's card pictures to the account
            }
            Err(e) => {
                crate::toast(a, e);
                ec_code(a, email, reload);
            }
        });
    });
}

// ---------- add a cloud (Echo's CLOUD_FORMS / loginForm / connectCloud) ----------
// PORT_LEFTOVERS L42: the login fields sit inline in Settings > Cloud under "Add a cloud" (Echo's #megarow), not one
// dialog per field; "Own server…" opens a small menu under the button (settings.slint, SDrop) -> own_pick.
type Vals = std::collections::HashMap<String, String>;
type FormFields = Vec<(&'static str, &'static str, &'static str, f32)>;
thread_local! {
    /// the form being shown: provider + what's typed so far
    static FORM: RefCell<Option<(String, Vals)>> = const { RefCell::new(None) };
    /// Settings' reload (store + page), kept from the last "Add a cloud" click
    static RELOAD: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

/// Echo's CLOUD_FORMS: title, help, fields (key, placeholder, kind "" / "password" / "button", width px)
fn form_of(p: &str) -> Option<(&'static str, &'static str, FormFields)> {
    Some(match p {
        "filen" => ("Filen login", "Your login is stored (obscured) on this PC only. No account yet? Make one on filen.io first.", vec![("user", "Email", "", 160.0), ("pass", "Password", "password", 160.0), ("code", "2FA code (if on)", "", 120.0)]),
        "mega" => ("MEGA login", "Your login is stored (obscured) on this PC only. No account yet? Make one on mega.nz first.", vec![("user", "Email", "", 160.0), ("pass", "Password", "password", 160.0)]),
        "webdav" => (
            "WebDAV server (Nextcloud, ownCloud, a NAS…)",
            "Nextcloud: https://your.server/remote.php/dav/files/USERNAME. Synology / QNAP: turn on WebDAV in the NAS settings.",
            vec![("url", "https://server/…", "", 240.0), ("user", "User", "", 160.0), ("pass", "Password", "password", 160.0)],
        ),
        "sftp" => (
            "SFTP server (any Linux PC / server / NAS with SSH)",
            "Clips go into an Echo folder in that user's home, or in the folder you type. Leave the password empty to use your SSH key.",
            vec![("host", "Server address", "", 160.0), ("port", "Port (22)", "", 80.0), ("user", "User", "", 110.0), ("pass", "Password", "password", 120.0), ("path", "Folder (optional)", "", 130.0)],
        ),
        "s3" => (
            "S3 storage (MinIO, Cloudflare R2, Backblaze B2, Wasabi…)",
            "Endpoint like https://<account>.r2.cloudflarestorage.com. Make the bucket first.",
            vec![("url", "Endpoint https://…", "", 220.0), ("bucket", "Bucket", "", 110.0), ("user", "Access key", "", 130.0), ("pass", "Secret key", "password", 130.0)],
        ),
        "folder" => (
            "A folder (NAS share, second drive, Syncthing folder…)",
            "Any folder this PC can reach. Mount the network share first, then pick it.",
            vec![("path", "Folder", "", 260.0), ("pick", "Choose…", "button", 90.0)],
        ),
        _ => return None,
    })
}

/// for settings.rs: the login row under "Add a cloud" (title, help) while a form is open
pub fn login_info() -> Option<(String, String)> {
    FORM.with(|f| f.borrow().as_ref().and_then(|(p, _)| form_of(p)).map(|(t, h, _)| (t.to_string(), h.to_string())))
}

/// the fields with what's typed in them (Slint makes new boxes when this model is set)
fn fill_fields(a: &AppWindow) {
    let rows: Vec<crate::LoginField> = FORM.with(|f| {
        let f = f.borrow();
        let Some((p, vals)) = f.as_ref() else { return vec![] };
        form_of(p)
            .map(|(_, _, fs)| {
                fs.into_iter()
                    .map(|(k, ph, kind, w)| crate::LoginField { key: k.into(), ph: ph.into(), kind: kind.into(), w, value: vals.get(k).cloned().unwrap_or_default().into() })
                    .collect()
            })
            .unwrap_or_default()
    });
    a.global::<crate::Login>().set_fields(ModelRc::new(VecModel::from(rows)));
}

fn reload_settings() {
    if let Some(r) = RELOAD.with(|r| r.borrow().clone()) {
        r();
    }
}

/// Echo's loginForm(p): the row shows that cloud's fields, the first one takes the keys
fn open_form(a: &AppWindow, p: &str) {
    FORM.with(|f| *f.borrow_mut() = Some((p.to_string(), Vals::new())));
    a.global::<crate::Login>().set_want_focus(true);
    fill_fields(a);
    reload_settings();
}

fn close_form(a: &AppWindow) {
    FORM.with(|f| *f.borrow_mut() = None);
    a.global::<crate::Login>().set_fields(ModelRc::default());
    reload_settings();
}

/// Settings' S.set("cloudown", kind): a pick in the "Own server…" menu (main.rs on_set). true = handled.
pub fn own_pick(a: &AppWindow, key: &str, kind: &str) -> bool {
    if key != "cloudown" {
        return false;
    }
    if form_of(kind).is_some() {
        open_form(a, kind);
    }
    true
}

/// "Add a cloud": Google Drive / pCloud sign in in the browser; the others show their login fields
fn connect(a: &AppWindow, provider: &str, reload: Rc<dyn Fn()>) {
    RELOAD.with(|r| *r.borrow_mut() = Some(reload));
    match provider {
        "gdrive" | "pcloud" => run_connect(a, provider.to_string(), Vals::new()),
        // the menu under the button is Slint's (settings.slint); its pick comes back as own_pick
        "own" => {}
        p => {
            if form_of(p).is_some() {
                open_form(a, p);
            }
        }
    }
}

/// Echo's connectCloud: user / pass / code + the non-empty url / host / port / path / bucket
fn run_connect(a: &AppWindow, p: String, vals: Vals) {
    let Some(rc) = rec() else { return };
    let title = match p.as_str() {
        "gdrive" | "pcloud" => "Log in in the browser window that opened…",
        "filen" => "Connecting to Filen… (the first time downloads a small Filen tool)",
        _ => "Connecting…",
    };
    quick(
        a,
        title,
        move || {
            let g = |k: &str| vals.get(k).cloned().unwrap_or_default();
            let extra: Vals = ["url", "host", "port", "path", "bucket"].iter().filter_map(|k| vals.get(*k).filter(|v| !v.is_empty()).map(|v| (k.to_string(), v.clone()))).collect();
            rc.cloud_connect(&p, &g("user"), &g("pass"), &g("code"), &extra).map(|_| String::new())
        },
        move |a, res| {
            match res {
                Ok(_) => {
                    crate::toast(a, "Cloud added - syncing");
                    crate::rec_core::sync_soon();
                    close_form(a);
                }
                Err(e) => {
                    crate::toast(a, e);
                    reload_settings();
                }
            }
            load_info(a);
        },
    );
}

/// the Login row's callbacks (typing, Connect / Enter, Choose… for a folder)
fn wire_login(app: &AppWindow) {
    let l = app.global::<crate::Login>();
    l.on_edited(|k, v| {
        FORM.with(|f| {
            if let Some((_, vals)) = f.borrow_mut().as_mut() {
                vals.insert(k.to_string(), v.to_string());
            }
        });
    });
    {
        let w = app.as_weak();
        l.on_go(move || {
            let Some(a) = w.upgrade() else { return };
            let Some((p, vals)) = FORM.with(|f| f.borrow().clone()) else { return };
            run_connect(&a, p, vals.into_iter().map(|(k, v)| (k, v.trim().to_string())).collect());
        });
    }
    {
        let w = app.as_weak();
        l.on_pick(move || {
            let w = w.clone();
            crate::lib_pick::pick_folder_ui("Folder for your clips (NAS share, second drive…)", move |p| {
                let Some(a) = w.upgrade() else { return };
                FORM.with(|f| {
                    if let Some((_, vals)) = f.borrow_mut().as_mut() {
                        vals.insert("path".into(), p.to_string_lossy().to_string());
                    }
                });
                fill_fields(&a);
            });
        });
    }
}

// ---------- Echo Cloud plans (Echo's #ecplans: cards inline in Settings > Cloud, not a dialog) ----------
thread_local! {
    static PLANS: RefCell<String> = const { RefCell::new(String::new()) }; // "" hidden / "loading" / "shown"
    static WAIT: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
}

/// for settings.rs: is the plans row shown ("" = no)
pub fn plans_state() -> String {
    PLANS.with(|p| p.borrow().clone())
}

fn set_plans(a: &AppWindow, st: &str) {
    PLANS.with(|p| *p.borrow_mut() = st.to_string());
    a.global::<crate::EcPlans>().set_state(st.into());
    if let Some(f) = REFRESH.with(|r| r.borrow().clone()) {
        f();
    }
}

/// Echo's money(): "€4.95" (Intl currency format, English style)
fn money(n: f64, cur: &str) -> String {
    let c = cur.to_uppercase();
    let sym = match c.as_str() { "EUR" => "€", "USD" => "$", "GBP" => "£", _ => "" };
    if sym.is_empty() { format!("{n:.2} {c}") } else { format!("{sym}{n:.2}") }
}

/// one card per size: month (primary) + year with Echo's "-N%" saving
fn plan_cards(v: &serde_json::Value) -> Vec<crate::EcPlan> {
    let plans = v.as_array().cloned().or_else(|| v["plans"].as_array().cloned()).unwrap_or_default();
    let mut sizes: Vec<u64> = vec![];
    for p in &plans {
        let gb = p["gb"].as_u64().unwrap_or(0);
        if !sizes.contains(&gb) {
            sizes.push(gb);
        }
    }
    sizes
        .into_iter()
        .map(|gb| {
            let find = |iv: &str| plans.iter().find(|p| p["gb"].as_u64() == Some(gb) && p["interval"] == iv);
            let (m, y) = (find("month"), find("year"));
            let price = |p: &serde_json::Value| p["price"].as_f64().unwrap_or(0.0);
            let save = match (m, y) {
                (Some(m), Some(y)) if price(m) > 0.0 => ((1.0 - price(y) / (price(m) * 12.0)) * 100.0).round() as i64,
                _ => 0,
            };
            let txt = |p: &serde_json::Value, per: &str| format!("{} / {per}", money(price(p), p["currency"].as_str().unwrap_or("eur")));
            crate::EcPlan {
                size: if gb >= 1000 { format!("{} TB", gb as f64 / 1000.0) } else { format!("{gb} GB") }.into(),
                mid: m.and_then(|p| p["id"].as_str()).unwrap_or("").into(),
                mtext: m.map(|p| txt(p, "month")).unwrap_or_default().into(),
                yid: y.and_then(|p| p["id"].as_str()).unwrap_or("").into(),
                ytext: y.map(|p| txt(p, "year")).unwrap_or_default().into(),
                save: if save > 0 { format!("-{save}%") } else { String::new() }.into(),
                // 10 Nov: paid users see the cards too - their own size is marked
                current: ec_paid_total().is_some_and(|t| (t as f64 - gb as f64 * 1e9).abs() < gb as f64 * 1e9 * 0.02),
            }
        })
        .collect()
}

/// Upgrade: show the cards (loading first) or hide them again (Echo toggles)
pub fn toggle_plans(a: &AppWindow) {
    let Some(rc) = rec() else { return };
    if plans_state() != "" {
        set_plans(a, "");
        return;
    }
    set_plans(a, "loading");
    let w = a.as_weak();
    crate::lib_net::bg(move || rc.echocloud_plans(), move |res| {
        let Some(a) = w.upgrade() else { return };
        match res {
            Ok(v) => {
                let cards = plan_cards(&v);
                if cards.is_empty() {
                    set_plans(&a, "");
                    crate::toast(&a, "No plans for sale right now");
                    return;
                }
                a.global::<crate::EcPlans>().set_plans(slint::ModelRc::new(slint::VecModel::from(cards)));
                set_plans(&a, "shown");
            }
            Err(e) => {
                set_plans(&a, "");
                crate::toast(&a, e);
            }
        }
    });
}

/// Echo Cloud's plan size in bytes when the account pays (over the free 10 GB), else None
fn ec_paid_total() -> Option<u64> {
    INFO.with(|i| {
        let v = i.borrow();
        let t = v.as_ref()?["accounts"].as_array()?.iter().find(|x| x["provider"] == "echocloud")?["total"].as_u64()?;
        (t as f64 > 10.5e9).then_some(t)
    })
}

/// a plan card's button: Polar checkout in the browser, then watch for the bigger space (Echo's ecWaitUpgrade)
fn buy(a: &AppWindow, product: String) {
    let Some(rc) = rec() else { return };
    // 10 Nov: someone who already pays changes plans on Polar's subscription page - a new checkout would
    // start a SECOND subscription
    if ec_paid_total().is_some() {
        crate::toast(a, "Switch plans on the Polar page that opens - your space changes right after");
        quick(a, "Opening your subscription…", move || rc.echocloud_manage().map(|_| String::new()), |a, res| {
            if let Err(e) = res {
                crate::toast(a, e)
            }
        });
        return;
    }
    a.global::<crate::EcPlans>().set_busy(product.clone().into());
    let w = a.as_weak();
    crate::lib_net::bg(move || rc.echocloud_checkout(&product), move |res| {
        let Some(a) = w.upgrade() else { return };
        a.global::<crate::EcPlans>().set_busy("".into());
        match res {
            Ok(_) => {
                crate::toast(&a, "Finish paying in your browser - Echo Cloud upgrades by itself");
                set_plans(&a, "");
                wait_upgrade(&a);
            }
            Err(e) => crate::toast(&a, e),
        }
    });
}

/// Echo's ecWaitUpgrade: every 10 s for 10 min, has the quota changed? Then say so and redraw the page.
pub fn wait_upgrade(a: &AppWindow) {
    let Some(rc) = rec() else { return };
    let before = std::sync::Arc::new(std::sync::Mutex::new(None::<u64>));
    {
        let b = before.clone();
        std::thread::spawn(move || {
            *b.lock().unwrap() = rc.echocloud_me().ok().and_then(|m| m["quota"].as_u64());
        });
    }
    let tries = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let t = slint::Timer::default();
    let w = a.as_weak();
    t.start(slint::TimerMode::Repeated, std::time::Duration::from_secs(10), move || {
        let n = tries.get() + 1;
        tries.set(n);
        if n > 60 {
            WAIT.with(|x| *x.borrow_mut() = None);
            return;
        }
        let Some(start) = *before.lock().unwrap() else { return };
        let w = w.clone();
        crate::lib_net::bg(move || rc.echocloud_me().ok().and_then(|m| m["quota"].as_u64()), move |q| {
            let (Some(a), Some(q)) = (w.upgrade(), q) else { return };
            if q != start && WAIT.with(|x| x.borrow_mut().take()).is_some() {
                let s = crate::library::fmt_size(q);
                crate::toast(&a, if q > start { format!("Echo Cloud upgraded to {s}") } else { format!("Echo Cloud is now {s}") });
                load_info(&a);
            }
        });
    });
    WAIT.with(|x| *x.borrow_mut() = Some(t));
}

/// Settings > Cloud opened: space per account (Echo's cloud_info - slow, on a thread) into the Store
pub fn load_info(a: &AppWindow) {
    let Some(r) = rec() else { return };
    let w = a.as_weak();
    crate::lib_net::bg(move || serde_json::to_value(r.cloud_info()).unwrap_or_default(), move |v| {
        let Some(_a) = w.upgrade() else { return };
        INFO.with(|i| *i.borrow_mut() = Some(v.clone()));
        INFO_AT.with(|t| t.set(Some(Instant::now())));
        // (gaps-cloud row 24: the "almost full" / "full" popup moved to cloud_live.rs with Echo's timing rules)
        if let Some(f) = REFRESH.with(|r| r.borrow().clone()) {
            f();
        }
    });
}

thread_local! {
    pub static INFO: RefCell<Option<serde_json::Value>> = const { RefCell::new(None) };
    static INFO_AT: Cell<Option<Instant>> = const { Cell::new(None) };
    static INFO_ASKED: Cell<Option<Instant>> = const { Cell::new(None) };
    static REFRESH: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

/// gaps-cloud (row 26): the sidebar's "☁ 3.1 GB of 10 GB in the cloud" line - Echo's cloud_info at most every
/// 10 min. Some((used, total)) once known; asks for it again when it's old.
pub fn usage_for_sidebar(a: &AppWindow) -> Option<(u64, u64)> {
    if !on() {
        return None;
    }
    let old = INFO_AT.with(|t| t.get()).is_none_or(|t| t.elapsed() > Duration::from_secs(600));
    let asked = INFO_ASKED.with(|t| t.get()).is_some_and(|t| t.elapsed() < Duration::from_secs(600));
    if old && !asked {
        INFO_ASKED.with(|t| t.set(Some(Instant::now())));
        load_info(a);
    }
    INFO.with(|i| {
        let v = i.borrow();
        let v = v.as_ref()?;
        if v["provider"].as_str().unwrap_or("").is_empty() {
            return None;
        }
        Some((v["used"].as_u64().unwrap_or(0), v["total"].as_u64().unwrap_or(0)))
    })
}

/// 10 Nov: a connected cloud's name for the sidebar - the provider only, never the account label (it can
/// hold an email, and the sidebar shows in screenshots / streams)
pub fn provider_name(p: &str) -> &'static str {
    match p {
        "echocloud" => "Echo Cloud",
        "filen" => "Filen",
        "gdrive" => "Google Drive",
        "mega" => "MEGA",
        "pcloud" => "pCloud",
        "webdav" => "WebDAV server",
        "sftp" => "SFTP server",
        "s3" => "S3 storage",
        "folder" => "Folder / NAS",
        _ => "Cloud",
    }
}

/// 10 Nov: every connected cloud on its own (name, used, total, error) - Echo Cloud first; a second account of
/// the same kind gets " 2". The old summary (usage_for_sidebar) added them all together.
pub fn usages_for_sidebar(a: &AppWindow) -> Option<Vec<(String, u64, u64, String)>> {
    usage_for_sidebar(a)?; // also asks for fresh numbers every 10 min
    INFO.with(|i| {
        let v = i.borrow();
        let mut accs: Vec<serde_json::Value> = v.as_ref()?["accounts"].as_array()?.clone();
        accs.sort_by_key(|x| x["provider"].as_str() != Some("echocloud"));
        let mut seen: std::collections::HashMap<&'static str, u32> = Default::default();
        Some(
            accs.iter()
                .map(|x| {
                    let base = provider_name(x["provider"].as_str().unwrap_or(""));
                    let n = seen.entry(base).or_insert(0);
                    *n += 1;
                    let name = if *n == 1 { base.to_string() } else { format!("{base} {n}") };
                    (name, x["used"].as_u64().unwrap_or(0), x["total"].as_u64().unwrap_or(0), x["error"].as_str().unwrap_or("").to_string())
                })
                .collect(),
        )
    })
}

/// the space manager deleted files: forget Echo Cloud's last size, and the full / almost full popup may come again
fn space_changed() {
    INFO_AT.with(|t| t.set(None));
    INFO_ASKED.with(|t| t.set(None));
    crate::lib_fx::ui_set("ecfull", serde_json::Value::Null);
}

/// (used, total, error) of one account from the last cloud_info
pub fn space_of(id: u32) -> Option<(u64, u64, String)> {
    INFO.with(|i| {
        let v = i.borrow();
        let a = v.as_ref()?["accounts"].as_array()?.iter().find(|x| x["id"].as_u64() == Some(id as u64))?.clone();
        Some((a["used"].as_u64().unwrap_or(0), a["total"].as_u64().unwrap_or(0), a["error"].as_str().unwrap_or("").to_string()))
    })
}

// ---------- Echo Cloud space manager (Echo's #ecm) ----------
thread_local! {
    /// free space on this PC's clips drive (echocloud_files' free_pc), for the footer
    static ECM_FREE: Cell<u64> = const { Cell::new(0) };
}

/// Echo's ecmState: "cp" share copy / "pc" also on this PC / "only" only in the cloud
fn ecm_state(f: &serde_json::Value) -> &'static str {
    if f["copy"].as_bool().unwrap_or(false) {
        "cp"
    } else if f["local"].as_bool().unwrap_or(false) {
        "pc"
    } else {
        "only"
    }
}

/// a dialog above the space manager closed: keys go back to the manager (Esc closes it again). Dlg.on_answer.
pub fn space_refocus(a: &AppWindow) {
    let e = a.global::<Ecm>();
    if e.get_open() {
        e.set_refocus(e.get_refocus().wrapping_add(1));
    }
}

pub fn space_open(a: &AppWindow) {
    let Some(r) = rec() else { return };
    let e = a.global::<Ecm>();
    e.set_open(true);
    e.set_sub("".into());
    e.set_empty("Looking through Echo Cloud…".into());
    e.set_sum("".into());
    e.set_all_on(false);
    e.set_rows(ModelRc::new(VecModel::<EcmRow>::default()));
    e.set_cats(ModelRc::new(VecModel::<crate::EcmCat>::default()));
    let w = a.as_weak();
    // cleanup: + the share links Echo Cloud says still work (a link renewed on another PC isn't "expired")
    let live = move || -> Option<HashSet<String>> {
        let v = r.ec_profile("GET", "/profile/me", None).ok()?;
        Some(v["shares"].as_array()?.iter().filter_map(|s| s["url"].as_str().map(String::from)).collect())
    };
    crate::lib_net::bg(move || r.echocloud_files().map(|v| (v, live())), move |res| {
        let Some(a) = w.upgrade() else { return };
        match res {
            Ok((v, live)) => {
                let files = v["files"].as_array().cloned().unwrap_or_default();
                let n = files.len();
                space_cats(&a, &files, live.as_ref());
                ECM_ALL.with(|x| *x.borrow_mut() = files);
                ECM_FREE.with(|f| f.set(v["free_pc"].as_u64().unwrap_or(0)));
                let e = a.global::<Ecm>();
                let (u, t) = (v["used"].as_u64().unwrap_or(0), v["quota"].as_u64().unwrap_or(0));
                e.set_used(if t > 0 { (u as f32 / t as f32).clamp(0.0, 1.0) } else { 0.0 });
                // Echo's #ecmsub
                e.set_sub(
                    format!(
                        "{}{} used · {n} file{}. Clips that are also on this PC stay on this PC when you delete them here.",
                        crate::library::fmt_size(u),
                        if t > 0 { format!(" of {}", crate::library::fmt_size(t)) } else { String::new() },
                        if n == 1 { "" } else { "s" }
                    )
                    .into(),
                );
                space_fill(&a);
            }
            Err(err) => {
                a.global::<Ecm>().set_empty(err.into());
            }
        }
    });
}

fn space_fill(a: &AppWindow) {
    let e = a.global::<Ecm>();
    let show = e.get_show().to_string();
    let sort = e.get_sort().to_string();
    let mut v: Vec<serde_json::Value> = ECM_ALL.with(|x| x.borrow().clone());
    let total = v.len();
    // cleanup: a category chip ("exp" / "old" / "pair") shows its files; the reason goes on the second line
    let why = |f: &serde_json::Value| ECM_CATS.with(|c| c.borrow().get(f["path"].as_str().unwrap_or("")).and_then(|w| w.iter().find(|w| w.cat == show).cloned()));
    let is_cat = matches!(show.as_str(), "exp" | "old" | "pair");
    v.retain(|f| show == "all" || ecm_state(f) == show || (is_cat && why(f).is_some()));
    // keep what was ticked (Echo's ecmDraw keeps the checked paths across a filter / sort change); a chip just
    // picked ticks its safe files instead (and nothing else)
    let presel = ECM_PRESEL.with(|p| p.take());
    let keep: HashSet<String> = if presel.is_some() { HashSet::new() } else { space_picked(a).into_iter().map(|r| r.path.to_string()).collect() };
    match sort.as_str() {
        "old" => v.sort_by_key(|f| f["modified"].as_u64().unwrap_or(0)),
        "big" => v.sort_by_key(|f| std::cmp::Reverse(f["size"].as_u64().unwrap_or(0))),
        _ => v.sort_by_key(|f| std::cmp::Reverse(f["modified"].as_u64().unwrap_or(0))),
    }
    if show == "pair" {
        // an original right above its edits
        v.sort_by_key(|f| {
            let n = f["name"].as_str().unwrap_or("");
            (crate::cleanup::edit_base(n).unwrap_or_else(|| n.to_string()).to_lowercase(), crate::cleanup::edit_base(n).is_some(), n.to_lowercase())
        });
    }
    let rows: Vec<EcmRow> = v
        .iter()
        .map(|f| {
            let name = f["name"].as_str().unwrap_or("").replace(" (Echo)", "");
            let copy = f["copy"].as_bool().unwrap_or(false);
            let modified = f["modified"].as_u64().unwrap_or(0);
            let path = f["path"].as_str().unwrap_or("").to_string();
            // Echo's .tn small: "58.8 MB · 3 hours ago · FiveM"
            let ago = if modified > 0 { crate::library::ago(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(modified)) } else { String::new() };
            let w = if is_cat { why(f) } else { None };
            let note = w.as_ref().map(|w| w.note.clone()).unwrap_or_default();
            let sub: Vec<String> = [crate::library::fmt_size(f["size"].as_u64().unwrap_or(0)), ago, f["folder"].as_str().unwrap_or("").to_string(), note].into_iter().filter(|s| !s.is_empty()).collect();
            EcmRow {
                on: if presel.is_some() { w.is_some_and(|w| w.safe) } else { keep.contains(&path) },
                path: path.into(),
                rel: f["rel"].as_str().unwrap_or("").into(),
                name: if copy { format!("Share copy of “{name}”") } else { name }.into(),
                sub: sub.join(" · ").into(),
                size: f["size"].as_u64().unwrap_or(0) as f32,
                local: f["local"].as_bool().unwrap_or(false),
                st: ecm_state(f).into(),
                thumb: Default::default(),
                has_thumb: false,
            }
        })
        .collect();
    e.set_empty(if !rows.is_empty() { "".into() } else if total > 0 { "Nothing here with this filter.".into() } else { "Echo Cloud is empty.".into() });
    e.set_rows(ModelRc::new(VecModel::from(rows)));
    space_sum(a);
    space_thumbs(a);
}

/// cleanup: the "Free up space" chips - each category's files and what it frees (cleanup.rs ecm_cats)
fn space_cats(a: &AppWindow, files: &[serde_json::Value], live: Option<&HashSet<String>>) {
    let Some(l) = lib() else { return };
    let c = cfg();
    let favs = l.favs.borrow().clone();
    let cats = crate::cleanup::ecm_cats(files, &l.thumbs_dir, live, &views_of, &c.cloud_upload, &favs);
    let chips: Vec<crate::EcmCat> = crate::cleanup::CATS
        .iter()
        .map(|&(key, label)| {
            let (n, b) = crate::cleanup::cat_size(files, &cats, key);
            crate::EcmCat {
                key: key.into(),
                label: label.into(),
                info: if n == 0 { "none".into() } else { format!("{n} · {}", crate::library::fmt_size(b)).into() },
                n: n as i32,
            }
        })
        .collect();
    ECM_CATS.with(|x| *x.borrow_mut() = cats);
    a.global::<Ecm>().set_cats(ModelRc::new(VecModel::from(chips)));
}

/// list generation: a refill (filter / sort) or a new open stops the previous picture loader
static ECM_GEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Echo's #ecm .tt pictures (10 Nov 2026: the native list had none): clips on this PC use Echo's
/// .thumbs/<name>.jpg, cloud-only ones are fetched like Echo's cloud_thumb (once, then cached there). One at a time
/// on a thread, shrunk to the row's size (a long list stays light); each row gets its picture when it's ready.
fn space_thumbs(a: &AppWindow) {
    use std::sync::atomic::Ordering;
    let gen = ECM_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let rows = a.global::<Ecm>().get_rows();
    let list: Vec<(usize, String)> = (0..rows.row_count()).filter_map(|i| rows.row_data(i).map(|r| (i, r.rel.to_string()))).filter(|(_, rel)| !rel.is_empty()).collect();
    let thumbs = crate::library::find_clips_dir().join(".thumbs");
    let r = rec();
    let w = a.as_weak();
    std::thread::spawn(move || {
        for (i, rel) in list {
            if ECM_GEN.load(Ordering::SeqCst) != gen {
                return;
            }
            let Some(stem) = std::path::Path::new(&rel).file_stem().map(|s| s.to_string_lossy().to_string()) else { continue };
            let mut p = thumbs.join(format!("{stem}.jpg"));
            if !p.exists() {
                match r.as_ref().and_then(|r| r.cloud_thumb(&rel).ok()) {
                    Some(t) => p = std::path::PathBuf::from(t.thumb),
                    None => continue,
                }
            }
            let Some(img) = image::open(&p).ok().map(|i| i.thumbnail(160, 90).to_rgb8()) else { continue };
            let px = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(img.as_raw(), img.width(), img.height());
            let w = w.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if ECM_GEN.load(Ordering::SeqCst) != gen {
                    return;
                }
                let Some(a) = w.upgrade() else { return };
                let rows = a.global::<Ecm>().get_rows();
                if let Some(mut row) = rows.row_data(i) {
                    if row.rel.as_str() == rel {
                        row.thumb = slint::Image::from_rgb8(px);
                        row.has_thumb = true;
                        rows.set_row_data(i, row);
                    }
                }
            });
        }
    });
}

/// Echo's ecmSum: "3 selected · 1.2 GB · 1 only in the cloud", or "X GB free on this PC" with nothing ticked
fn space_sum(a: &AppWindow) {
    let e = a.global::<Ecm>();
    let rows = e.get_rows();
    let all: Vec<EcmRow> = (0..rows.row_count()).filter_map(|i| rows.row_data(i)).collect();
    let sel: Vec<&EcmRow> = all.iter().filter(|r| r.on).collect();
    let b: f64 = sel.iter().map(|r| r.size as f64).sum();
    let only = sel.iter().filter(|r| r.st == "only").count();
    e.set_sum(
        if sel.is_empty() {
            format!("{} free on this PC", crate::library::fmt_size(ECM_FREE.with(|f| f.get())))
        } else {
            format!("{} selected · {}{}", sel.len(), crate::library::fmt_size(b as u64), if only > 0 { format!(" · {only} only in the cloud") } else { String::new() })
        }
        .into(),
    );
    e.set_any(!sel.is_empty());
    e.set_all_on(!all.is_empty() && sel.len() == all.len());
    // the "Download cloud-only clips…" box only matters when the selection has some
    e.set_show_dl(only > 0 || sel.is_empty());
}

/// Echo's ecmDownload: the clips only in Echo Cloud, one after another, after checking this PC has room
/// (1 GB to spare). done(true) = all arrived.
fn space_download(a: &AppWindow, list: Vec<EcmRow>, done: impl FnOnce(&AppWindow, bool) + 'static) {
    if list.is_empty() {
        done(a, true);
        return;
    }
    let Some(r) = rec() else { return };
    let need: u64 = list.iter().map(|f| f.size as u64).sum();
    let free = echo_core::recorder::free_space(&crate::library::find_clips_dir()).unwrap_or(0);
    if free > 0 && need + 1_000_000_000 > free {
        crate::ask(
            a,
            "Not enough space on this PC",
            &format!("Downloading these clips needs {} plus 1 GB to spare, but your clips folder's drive only has {} free. Pick fewer clips, untick \"Download cloud-only clips\" to delete them without keeping a copy, or free up space on this PC first.", crate::library::fmt_size(need), crate::library::fmt_size(free)),
            &[("OK", "", "primary")],
            None,
            "",
            move |a, _, _| done(a, false),
        );
        return;
    }
    let n = list.len();
    let id = crate::prog_box(&format!("Downloading {n} clip{}…", if n == 1 { "" } else { "s" }));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let stop = stop.clone();
        crate::prog_btn(id, "Cancel", move || {
            stop.store(true, Ordering::Relaxed);
            crate::prog_btn(id, "Stopping…", || {});
        });
    }
    // (index, name, bytes done before this clip, its rel) - the thread writes, the timer reads
    let at = Arc::new(std::sync::Mutex::new((0usize, String::new(), 0u64, String::new())));
    let t = slint::Timer::default();
    {
        let at = at.clone();
        // 20x a second with smoothed bytes, like a single download (dl_smooth.rs: they arrive in 1 MiB lumps; the
        // bar went in steps every 500 ms before, 10 Nov 2026)
        let mut sm = crate::dl_smooth::Smooth::new(std::time::Instant::now());
        t.start(slint::TimerMode::Repeated, Duration::from_millis(50), move || {
            let (i, name, before, rel) = at.lock().map(|x| x.clone()).unwrap_or_default();
            if rel.is_empty() {
                return;
            }
            let got = sm.step(before + Recorder::cloud_dl_progress(&rel).0, need, std::time::Instant::now());
            // 2.0.2: + speed and time left for all of them
            let rate = crate::notes::rate(id, got, need).map(|r| format!(" · {r}")).unwrap_or_default();
            crate::prog_set(id, None, Some(&format!("{} of {n} · {name}{rate}", i + 1)), Some((got as f64 / need.max(1) as f64 * 100.0).min(100.0) as f32));
        });
    }
    let stop2 = stop.clone();
    let w = a.as_weak();
    // (rel, name, size): the rows themselves hold pictures, which can't go to another thread
    let list: Vec<(String, String, u64)> = list.into_iter().map(|f| (f.rel.to_string(), f.name.to_string(), f.size as u64)).collect();
    crate::lib_net::bg(
        move || {
            let mut before = 0u64;
            for (i, (rel, name, size)) in list.iter().enumerate() {
                if stop2.load(Ordering::Relaxed) {
                    return Ok(true);
                }
                if let Ok(mut x) = at.lock() {
                    *x = (i, name.clone(), before, rel.clone());
                }
                r.cloud_download(rel).map_err(|e| format!("Couldn't download {name}: {e}"))?;
                before += size;
            }
            Ok(stop2.load(Ordering::Relaxed))
        },
        move |res: Result<bool, String>| {
            drop(t);
            let Some(a) = w.upgrade() else { return };
            match res {
                Ok(false) => {
                    crate::prog_set(id, None, None, Some(100.0));
                    crate::prog_done(id, 800);
                    done(&a, true);
                }
                Ok(true) => {
                    crate::prog_done(id, 0);
                    crate::toast(&a, "Download stopped - nothing was deleted");
                    done(&a, false);
                }
                Err(e) => {
                    crate::prog_done(id, 0);
                    crate::toast(&a, e);
                    done(&a, false);
                }
            }
        },
    );
}

/// Echo's ecmRun: Download (del = false) or Delete from Echo Cloud (cloud-only clips downloaded first when ticked)
fn space_run(a: &AppWindow, del: bool) {
    let Some(r) = rec() else { return };
    let e = a.global::<Ecm>();
    let sel = space_picked(a);
    if sel.is_empty() || e.get_busy() {
        return;
    }
    let only: Vec<EcmRow> = sel.iter().filter(|f| f.st == "only").cloned().collect();
    let copies = sel.iter().filter(|f| f.st == "cp").count();
    let dl_first = !del || e.get_dl_first();
    let bytes: u64 = sel.iter().map(|f| f.size as u64).sum();
    let paths: Vec<String> = sel.iter().map(|f| f.path.to_string()).collect();
    let n_sel = sel.len();
    // Echo's long confirm: space freed, clips only in the cloud, clips on this PC, share links that stop
    let fsz = crate::library::fmt_size;
    let k = only.len();
    let mut parts = vec![format!("{} of Echo Cloud space is freed.", fsz(bytes))];
    if k > 0 {
        let ob: u64 = only.iter().map(|f| f.size as u64).sum();
        parts.push(if dl_first {
            format!("{k} clip{} only in the cloud - {} downloaded to this PC first ({}).", if k == 1 { " is" } else { "s are" }, if k == 1 { "it's" } else { "they're" }, fsz(ob))
        } else {
            format!("{k} clip{} only in the cloud and will be gone for good.", if k == 1 { " is" } else { "s are" })
        });
    }
    if n_sel > k + copies {
        parts.push("Clips that are also on this PC stay there and aren't uploaded again by themselves.".into());
    }
    if copies > 0 {
        parts.push(format!("{copies} share link{} working.", if copies == 1 { " stops" } else { "s stop" }));
    }
    // cleanup (10 Nov 2026): cloud deletes are permanent - say so
    parts.push("Echo Cloud has no bin - this can't be undone.".into());
    let go = move |a: &AppWindow| {
        a.global::<Ecm>().set_busy(true);
        space_sum(a);
        let n_only = only.len();
        let finish = |a: &AppWindow| {
            a.global::<Ecm>().set_busy(false);
            crate::rec_core::sync_soon();
            sync_ui(a);
            if a.global::<Ecm>().get_open() {
                space_open(a);
            }
        };
        space_download(a, if dl_first { only } else { vec![] }, move |a, ok| {
            if !ok {
                finish(a);
                return;
            }
            if !del {
                crate::toast(a, if n_only > 0 { format!("{n_only} clip{} downloaded", if n_only == 1 { "" } else { "s" }) } else { "Already on this PC".to_string() });
                finish(a);
                return;
            }
            let w = a.as_weak();
            crate::lib_net::bg(move || r.echocloud_delete_files(&paths), move |res| {
                let Some(a) = w.upgrade() else { return };
                match res {
                    Ok(n) => {
                        crate::toast(&a, format!("Deleted {n} file{} from Echo Cloud - {} freed", if n == 1 { "" } else { "s" }, crate::library::fmt_size(bytes)));
                        space_changed();
                    }
                    Err(e) => crate::toast(&a, e),
                }
                finish(&a);
            });
        });
    };
    if !del {
        go(a);
        return;
    }
    crate::confirm_box(a, &format!("Delete {n_sel} file{} from Echo Cloud?", if n_sel == 1 { "" } else { "s" }), &parts.join(" "), "Delete", go);
}

fn space_picked(a: &AppWindow) -> Vec<EcmRow> {
    let rows = a.global::<Ecm>().get_rows();
    (0..rows.row_count()).filter_map(|i| rows.row_data(i)).filter(|r| r.on).collect()
}

pub fn wire(app: &AppWindow, srefresh: Rc<dyn Fn()>) {
    REFRESH.with(|r| *r.borrow_mut() = Some(srefresh));
    {
        let w = app.as_weak();
        app.global::<crate::EcPlans>().on_buy(move |id| {
            if let Some(a) = w.upgrade() {
                buy(&a, id.to_string());
            }
        });
    }
    {
        let w = app.as_weak();
        app.global::<Cm>().on_pick(move |v| {
            if let Some(a) = w.upgrade() {
                menu_pick(&a, &v);
            }
        });
    }
    wire_login(app); // PORT_LEFTOVERS L42
    let e = app.global::<Ecm>();
    // PORT_LEFTOVERS L17: closed (Esc / backdrop / x) - keys go back to the page behind it
    {
        let w = app.as_weak();
        e.on_closed(move || {
            let Some(a) = w.upgrade() else { return };
            if a.get_player_open() {
                a.invoke_focus_player();
            } else if a.get_view() == "settings" {
                a.invoke_focus_settings();
            } else {
                a.invoke_focus_library();
            }
        });
    }
    {
        let w = app.as_weak();
        e.on_toggle(move |i| {
            let Some(a) = w.upgrade() else { return };
            let rows = a.global::<Ecm>().get_rows();
            if let Some(mut r) = rows.row_data(i as usize) {
                r.on = !r.on;
                rows.set_row_data(i as usize, r);
            }
            space_sum(&a);
        });
    }
    {
        let w = app.as_weak();
        e.on_all(move |on| {
            let Some(a) = w.upgrade() else { return };
            let rows = a.global::<Ecm>().get_rows();
            for i in 0..rows.row_count() {
                if let Some(mut r) = rows.row_data(i) {
                    r.on = on;
                    rows.set_row_data(i, r);
                }
            }
            space_sum(&a);
        });
    }
    // cleanup: a "Free up space" chip - show that category and tick only its safe files (again = back to Everything)
    {
        let w = app.as_weak();
        e.on_cat(move |key| {
            let Some(a) = w.upgrade() else { return };
            let e = a.global::<Ecm>();
            let off = e.get_show() == key;
            e.set_show(if off { "all".into() } else { key.clone() });
            ECM_PRESEL.with(|p| *p.borrow_mut() = Some(key.to_string()));
            space_fill(&a);
        });
    }
    {
        let w = app.as_weak();
        e.on_refill(move || {
            if let Some(a) = w.upgrade() {
                space_fill(&a);
            }
        });
    }
    {
        let w = app.as_weak();
        e.on_download(move || {
            if let Some(a) = w.upgrade() {
                space_run(&a, false);
            }
        });
    }
    {
        let w = app.as_weak();
        e.on_delete(move || {
            if let Some(a) = w.upgrade() {
                space_run(&a, true);
            }
        });
    }
    // share views every 5 min, cloud-only thumbnails every 20 s, Echo Cloud space once a minute after start
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_secs(300), refresh_views);
    let t2 = slint::Timer::default();
    t2.start(slint::TimerMode::Repeated, Duration::from_secs(20), fetch_thumbs);
    let w = app.as_weak();
    slint::Timer::single_shot(Duration::from_secs(20), move || {
        refresh_views();
        if let Some(a) = w.upgrade() {
            if has_ec() {
                load_info(&a);
            }
        }
    });
    TIMERS.with(|x| x.borrow_mut().extend([t, t2]));
}
