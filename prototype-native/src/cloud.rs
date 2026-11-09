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
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn rec() -> Option<&'static Recorder> {
    crate::rec_core::rec()
}

/// the signed-in Echo account's token ("" = not signed in), through Echo's settings.rs (DPAPI / keyring)
pub fn token() -> String {
    echo_core::settings::Settings::load().ec_token
}

fn cfg() -> echo_core::settings::Settings {
    rec().map(|r| r.settings()).unwrap_or_else(echo_core::settings::Settings::load)
}

/// any cloud set up (Echo Cloud or your own)
pub fn on() -> bool {
    !cfg().cloud_accounts.is_empty()
}

fn has_ec() -> bool {
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
pub fn job(a: &AppWindow, title: &str, work: impl FnOnce(&dyn Fn(f64)) -> Result<String, String> + Send + 'static, done: impl FnOnce(&AppWindow, Result<String, String>) + 'static) -> i32 {
    let id = crate::prog_box(title);
    crate::prog_set(id, None, None, Some(-1.0));
    let pct = Arc::new(AtomicU32::new(u32::MAX));
    let p2 = pct.clone();
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_millis(250), move || {
        let v = p2.load(Ordering::Relaxed);
        if v != u32::MAX {
            crate::prog_set(id, None, Some(&format!("{}%", v / 10)), Some(v as f32 / 10.0));
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
    let Some(l) = lib() else { return HashSet::new() };
    std::fs::read_to_string(l.thumbs_dir.join("links.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&s).ok())
        .map(|m| m.keys().map(|k| k.replace('\\', "/").to_lowercase()).collect())
        .unwrap_or_default()
}

// ---------- the cloud menu ----------
thread_local! {
    static MENU_IDX: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static VIEWS: RefCell<BTreeMap<String, u64>> = const { RefCell::new(BTreeMap::new()) };
    static BASE: RefCell<String> = const { RefCell::new(String::new()) };
    static TIMERS: RefCell<Vec<slint::Timer>> = const { RefCell::new(vec![]) };
    static ECM_ALL: RefCell<Vec<serde_json::Value>> = const { RefCell::new(vec![]) };
    static FULL_WARNED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn item(label: &str, value: &str, kind: &str) -> CmItem {
    CmItem { label: label.into(), value: value.into(), kind: kind.into() }
}

/// the card's / player's cloud button. only_link = the player's Share button (link part only, like Echo)
pub fn menu(a: &AppWindow, idx: usize, only_link: bool) {
    let Some(l) = lib() else { return };
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
            if (c.cloud.is_some() || l.in_cloud(idx) || shared) && has_ec() {
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
        crate::toast(a, "Set up the cloud first (Settings > Cloud)");
        open_settings(a);
        return;
    }
    copy_link(a, idx, None);
}

fn open_settings(a: &AppWindow) {
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
    let life = link_life(days.unwrap_or(cfg().link_days));
    let title = if local_only { "Uploading and making a link…" } else { "Making a link…" };
    let id = job(
        a,
        title,
        move |p| {
            let url = if local_only { r.upload_and_link(&path, days, p)? } else { r.cloud_link(&rel, days, p)? };
            crate::lib_net::copy_text(&url)?;
            Ok(url)
        },
        move |a, res| {
            match res {
                Ok(_) => crate::toast(a, if local_only { format!("Uploaded - link copied, {life}") } else { format!("Link copied - {life}") }),
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
            let id = job(
                a,
                "Uploading…",
                move |_| {
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
            cancel_upload_btn(id);
        }
        "free" => {
            if a.get_player_open() {
                a.global::<crate::Play>().invoke_close();
            }
            let w = a.as_weak();
            slint::Timer::single_shot(Duration::from_millis(200), move || {
                let Some(a) = w.upgrade() else { return };
                quick(&a, "Freeing up space…", move || r.cloud_remove_local(&path).map(|_| String::new()), |a, res| {
                    match res {
                        Ok(_) => crate::toast(a, "Removed from this PC - still in the cloud"),
                        Err(e) => crate::toast(a, e),
                    }
                    sync_ui(a);
                });
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
            crate::confirm_box(a, "Delete from the cloud?", &msg, "Delete", move |a| {
                if only && a.get_player_open() {
                    a.global::<crate::Play>().invoke_close();
                }
                quick(a, "Deleting from the cloud…", move || r.cloud_delete(&rel).map(|_| String::new()), |a, res| {
                    match res {
                        Ok(_) => crate::toast(a, "Deleted from the cloud"),
                        Err(e) => crate::toast(a, e),
                    }
                    sync_ui(a);
                });
            });
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

/// cloud-only clip -> this PC, with Echo's download progress (bytes of total)
pub fn download(a: &AppWindow, idx: usize) {
    let (Some(l), Some(r)) = (lib(), rec()) else { return };
    let c = l.clip(idx);
    let rel = rel_of(&c);
    let size = c.size.max(1);
    let id = crate::prog_box("Downloading from the cloud…");
    let rel2 = rel.clone();
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_millis(300), move || {
        let (got, total) = Recorder::cloud_dl_progress(&rel2);
        let tot = if total > 0 { total } else { size };
        let f = (got as f64 / tot as f64).clamp(0.0, 1.0);
        crate::prog_set(id, None, Some(&format!("{} of {} · {:.0}%", crate::library::fmt_size(got), crate::library::fmt_size(tot), f * 100.0)), Some((f * 100.0) as f32));
    });
    let w = a.as_weak();
    crate::lib_net::bg(move || r.cloud_download(&rel), move |res| {
        drop(t);
        crate::prog_done(id, 300);
        let Some(a) = w.upgrade() else { return };
        match res {
            Ok(_) => crate::toast(&a, "Downloaded to this PC"),
            Err(e) => crate::toast(&a, e),
        }
        sync_ui(&a);
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
    slint::Timer::single_shot(Duration::from_millis(250), move || {
        let Some(a) = w.upgrade() else { return };
        let (k, err) = l.delete_clips(&local);
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
                Ok(_) => crate::toast(a, if k + n <= 1 { "Deleted everywhere".to_string() } else { format!("Deleted {} clips everywhere", k.max(n)) }),
                Err(e) => crate::toast(a, e),
            }
            sync_ui(a);
        });
    });
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
        }, |a, res| {
            if let Err(e) = res {
                crate::toast(a, e);
            }
            sync_ui(a);
        });
    });
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

/// the player opens a cloud-only clip: Echo's rclone streaming server address instead of the missing file
pub fn play_path(p: &Path) -> PathBuf {
    if p.exists() || p.to_string_lossy().starts_with("http") {
        return p.to_path_buf();
    }
    let (Some(l), Some(r)) = (lib(), rec()) else { return p.to_path_buf() };
    let Ok(rel) = p.strip_prefix(l.root()) else { return p.to_path_buf() };
    let rel = rel.to_string_lossy().replace('\\', "/");
    let Some(f) = r.cloud_files().into_iter().find(|f| f.rel.eq_ignore_ascii_case(&rel)) else { return p.to_path_buf() };
    let mut base = BASE.with(|b| b.borrow().clone());
    if base.is_empty() {
        base = r.cloud_stream_base().unwrap_or_default();
        BASE.with(|b| *b.borrow_mut() = base.clone());
    }
    if base.is_empty() {
        return p.to_path_buf();
    }
    let path = if f.path.is_empty() { f.rel } else { f.path };
    let enc: Vec<String> = path.split('/').map(url_enc).collect();
    PathBuf::from(format!("{base}/{}", enc.join("/")))
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

/// thumbnails + lengths for cloud-only clips (Echo's cloud_thumb over the streaming server), one at a time
fn fetch_thumbs() {
    let Some(l) = lib() else { return };
    let want: Vec<String> = l.all().iter().filter(|c| c.cloud.is_some() && !c.gone.get() && !c.thumb().exists()).filter_map(|c| c.cloud.clone()).take(12).collect();
    if want.is_empty() {
        return;
    }
    let Some(r) = rec() else { return };
    crate::lib_net::bg(
        move || {
            for rel in &want {
                let _ = r.cloud_thumb(rel);
            }
        },
        |_| {
            if let Some(l) = lib() {
                l.changed_all();
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
        "ecsignin" => {
            crate::ask(a, "Sign in to Echo Cloud", "10 GB free, nothing to set up. You get a 6-digit code by email - no password.", &[("Cancel", "", ""), ("Send code", "ok", "primary")], Some("Your email"), "ok", move |a, v, email| {
                let email = email.trim().to_lowercase();
                if v != "ok" || email.is_empty() {
                    return;
                }
                let e2 = email.clone();
                quick(a, "Sending the code…", move || rc.echocloud_start(&e2).map(|_| String::new()), move |a, res| {
                    if let Err(e) = res {
                        crate::toast(a, e);
                        return;
                    }
                    crate::ask(a, "Check your email", &format!("Type the 6-digit code we sent to {email}."), &[("Cancel", "", ""), ("Sign in", "ok", "primary")], Some("123456"), "ok", move |a, v, code| {
                        if v != "ok" || code.trim().is_empty() {
                            return;
                        }
                        let code = code.trim().to_string();
                        quick(a, "Signing in…", move || rc.cloud_connect("echocloud", &email, "", &code, &Default::default()).map(|_| String::new()), move |a, res| {
                            match res {
                                Ok(_) => {
                                    crate::toast(a, "Signed in - Echo Cloud is ready");
                                    crate::rec_core::sync_soon();
                                }
                                Err(e) => crate::toast(a, e),
                            }
                            reload();
                            load_info(a);
                        });
                    });
                });
            });
        }
        "ecsignout" => {
            let id = cfg().cloud_accounts.iter().find(|x| x.provider == "echocloud").map(|x| x.id);
            crate::confirm_box(a, "Sign out of Echo Cloud?", "Your clips stay in Echo Cloud - sign in again with the same email to get them back.", "Sign out", move |a| {
                let Some(id) = id else { return };
                quick(a, "Signing out…", move || rc.cloud_remove_account(id).map(|_| String::new()), move |a, res| {
                    if let Err(e) = res {
                        crate::toast(a, e);
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
            crate::confirm_box(a, "Remove this cloud?", "Its clips stay in that cloud; Echo just stops using it.", "Remove", move |a| {
                quick(a, "Removing…", move || rc.cloud_remove_account(id).map(|_| String::new()), move |a, res| {
                    if let Err(e) = res {
                        crate::toast(a, e);
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

/// own clouds: Echo's login forms as one question per field (passwords hidden)
fn connect(a: &AppWindow, provider: &str, reload: Rc<dyn Fn()>) {
    let Some(rc) = rec() else { return };
    // (key, question, placeholder, secret)
    let fields: Vec<(&str, &str, &str, bool)> = match provider {
        "filen" => vec![("user", "Filen login - your email (no account yet? make one on filen.io first)", "Email", false), ("pass", "Filen password", "Password", true), ("code", "2FA code (leave empty if it's off)", "2FA code", false)],
        "mega" => vec![("user", "MEGA login - your email (no account yet? make one on mega.nz first)", "Email", false), ("pass", "MEGA password", "Password", true)],
        "webdav" => vec![("url", "WebDAV server - Nextcloud: https://your.server/remote.php/dav/files/USERNAME", "https://server/…", false), ("user", "User", "User", false), ("pass", "Password", "Password", true)],
        "sftp" => vec![("host", "SFTP server address (any Linux PC / server / NAS with SSH)", "Server address", false), ("port", "Port (empty = 22)", "22", false), ("user", "User", "User", false), ("pass", "Password (empty = your SSH key)", "Password", true), ("path", "Folder (optional)", "Folder", false)],
        "s3" => vec![("url", "S3 endpoint (MinIO, Cloudflare R2, Backblaze B2, Wasabi…)", "https://…", false), ("bucket", "Bucket (make it first)", "Bucket", false), ("user", "Access key", "Access key", false), ("pass", "Secret key", "Secret key", true)],
        _ => vec![],
    };
    let provider = provider.to_string();
    let run = {
        let reload = reload.clone();
        let provider = provider.clone();
        move |a: &AppWindow, vals: std::collections::HashMap<String, String>| {
            let reload = reload.clone();
            let p = provider.clone();
            let title = if matches!(p.as_str(), "gdrive" | "pcloud") { "Sign in in your browser…" } else { "Connecting…" };
            quick(a, title, move || {
                let g = |k: &str| vals.get(k).cloned().unwrap_or_default();
                rc.cloud_connect(&p, &g("user"), &g("pass"), &g("code"), &vals).map(|_| String::new())
            }, move |a, res| {
                match res {
                    Ok(_) => {
                        crate::toast(a, "Connected - syncing your clips");
                        crate::rec_core::sync_soon();
                    }
                    Err(e) => crate::toast(a, e),
                }
                reload();
                load_info(a);
            });
        }
    };
    match provider.as_str() {
        "gdrive" | "pcloud" => run(a, Default::default()),
        "folder" => {
            let w = a.as_weak();
            crate::lib_pick::pick_folder_ui("Folder for your clips (NAS share, second drive…)", move |p| {
                if let Some(a) = w.upgrade() {
                    let mut m = std::collections::HashMap::new();
                    m.insert("path".to_string(), p.to_string_lossy().to_string());
                    run(&a, m);
                }
            });
        }
        "own" => {
            crate::ask(a, "Your own server", "Which kind?", &[("Cancel", "", ""), ("Folder / NAS", "folder", ""), ("S3", "s3", ""), ("SFTP", "sftp", ""), ("WebDAV", "webdav", "primary")], None, "", move |a, v, _| {
                if !v.is_empty() {
                    connect(a, &v, reload.clone());
                }
            });
        }
        _ => ask_fields(a, fields.into_iter().map(|(k, q, p, s)| (k.to_string(), q.to_string(), p.to_string(), s)).collect(), Default::default(), Rc::new(run)),
    }
}

type Fields = Vec<(String, String, String, bool)>;
fn ask_fields(a: &AppWindow, mut rest: Fields, mut got: std::collections::HashMap<String, String>, done: Rc<dyn Fn(&AppWindow, std::collections::HashMap<String, String>)>) {
    if rest.is_empty() {
        done(a, got);
        return;
    }
    let (k, q, p, secret) = rest.remove(0);
    a.global::<crate::Dlg>().set_secret(secret);
    crate::ask(a, "Add a cloud", &q, &[("Cancel", "", ""), ("Next", "ok", "primary")], Some(&p), "ok", move |a, v, text| {
        a.global::<crate::Dlg>().set_secret(false);
        if v != "ok" {
            return;
        }
        got.insert(k, text.trim().to_string());
        ask_fields(a, rest, got, done);
    });
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

/// a plan card's button: Polar checkout in the browser, then watch for the bigger space (Echo's ecWaitUpgrade)
fn buy(a: &AppWindow, product: String) {
    let Some(rc) = rec() else { return };
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
        let Some(a) = w.upgrade() else { return };
        INFO.with(|i| *i.borrow_mut() = Some(v.clone()));
        // Echo's "almost full" notice: once per run, Echo Cloud over 90 %
        if let Some(ec) = v["accounts"].as_array().and_then(|x| x.iter().find(|x| x["provider"] == "echocloud")) {
            let (u, t) = (ec["used"].as_u64().unwrap_or(0), ec["total"].as_u64().unwrap_or(0));
            if t > 0 && u * 10 >= t * 9 && !FULL_WARNED.with(|f| f.replace(true)) {
                crate::ask(&a, "Echo Cloud is almost full", &format!("{} of {} used. Free up space, or get more.", crate::library::fmt_size(u), crate::library::fmt_size(t)), &[("Later", "", ""), ("More space", "plans", ""), ("Free up space", "space", "primary")], None, "space", |a, v, _| {
                    if v == "space" {
                        space_open(a);
                    } else if v == "plans" {
                        // the plans live in Settings > Cloud (Echo's #ecplans): go there and open them
                        a.invoke_page("Settings".into());
                        a.global::<crate::S>().invoke_pick("cloud".into());
                        if plans_state() == "" {
                            toggle_plans(a);
                        }
                    }
                });
            }
        }
        if let Some(f) = REFRESH.with(|r| r.borrow().clone()) {
            f();
        }
    });
}

thread_local! {
    pub static INFO: RefCell<Option<serde_json::Value>> = const { RefCell::new(None) };
    static REFRESH: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
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
fn space_open(a: &AppWindow) {
    let Some(r) = rec() else { return };
    let e = a.global::<Ecm>();
    e.set_open(true);
    e.set_sub("Loading…".into());
    e.set_rows(ModelRc::new(VecModel::<EcmRow>::default()));
    let w = a.as_weak();
    crate::lib_net::bg(move || r.echocloud_files(), move |res| {
        let Some(a) = w.upgrade() else { return };
        match res {
            Ok(v) => {
                ECM_ALL.with(|x| *x.borrow_mut() = v["files"].as_array().cloned().unwrap_or_default());
                let e = a.global::<Ecm>();
                let (u, t) = (v["used"].as_u64().unwrap_or(0), v["quota"].as_u64().unwrap_or(0));
                e.set_used(if t > 0 { (u as f32 / t as f32).clamp(0.0, 1.0) } else { 0.0 });
                e.set_sub(format!("{} of {} used. Delete what you don't need there - clips that are also on this PC stay on this PC.", crate::library::fmt_size(u), crate::library::fmt_size(t)).into());
                space_fill(&a);
            }
            Err(err) => {
                a.global::<Ecm>().set_sub(err.into());
            }
        }
    });
}

fn space_fill(a: &AppWindow) {
    let e = a.global::<Ecm>();
    let show = e.get_show().to_string();
    let sort = e.get_sort().to_string();
    let mut v: Vec<serde_json::Value> = ECM_ALL.with(|x| x.borrow().clone());
    v.retain(|f| {
        let (local, copy) = (f["local"].as_bool().unwrap_or(false), f["copy"].as_bool().unwrap_or(false));
        match show.as_str() {
            "pc" => local,
            "only" => !local && !copy,
            "cp" => copy,
            _ => true,
        }
    });
    match sort.as_str() {
        "old" => v.sort_by_key(|f| f["modified"].as_u64().unwrap_or(0)),
        "big" => v.sort_by_key(|f| std::cmp::Reverse(f["size"].as_u64().unwrap_or(0))),
        _ => v.sort_by_key(|f| std::cmp::Reverse(f["modified"].as_u64().unwrap_or(0))),
    }
    let rows: Vec<EcmRow> = v
        .iter()
        .map(|f| EcmRow {
            path: f["path"].as_str().unwrap_or("").into(),
            rel: f["rel"].as_str().unwrap_or("").into(),
            name: f["name"].as_str().unwrap_or("").replace(" (Echo)", "").into(),
            sub: format!(
                "{} · {}{}",
                crate::library::fmt_size(f["size"].as_u64().unwrap_or(0)),
                if f["folder"].as_str().unwrap_or("").is_empty() { "Main folder".to_string() } else { f["folder"].as_str().unwrap_or("").to_string() },
                if f["copy"].as_bool().unwrap_or(false) { " · share copy (deleting it ends that link)" } else if f["local"].as_bool().unwrap_or(false) { " · also on this PC" } else { " · only in the cloud" }
            )
            .into(),
            size: f["size"].as_u64().unwrap_or(0) as f32,
            local: f["local"].as_bool().unwrap_or(false),
            on: false,
        })
        .collect();
    e.set_rows(ModelRc::new(VecModel::from(rows)));
    space_sum(a);
}

fn space_sum(a: &AppWindow) {
    let e = a.global::<Ecm>();
    let rows = e.get_rows();
    let (n, b) = (0..rows.row_count()).filter_map(|i| rows.row_data(i)).filter(|r| r.on).fold((0, 0f64), |(n, b), r| (n + 1, b + r.size as f64));
    e.set_sum(if n == 0 { "".into() } else { format!("{n} selected · {}", crate::library::fmt_size(b as u64)).into() });
    e.set_any(n > 0);
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
    let e = app.global::<Ecm>();
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
            let Some(a) = w.upgrade() else { return };
            let Some(r) = rec() else { return };
            let rels: Vec<String> = space_picked(&a).into_iter().filter(|x| !x.local).map(|x| x.rel.to_string()).collect();
            if rels.is_empty() {
                crate::toast(&a, "Those are on this PC already");
                return;
            }
            job(&a, "Downloading from the cloud…", move |p| {
                let n = rels.len();
                for (i, rel) in rels.iter().enumerate() {
                    r.cloud_download(rel)?;
                    p((i + 1) as f64 / n as f64);
                }
                Ok(String::new())
            }, |a, res| {
                match res {
                    Ok(_) => crate::toast(a, "Downloaded to this PC"),
                    Err(e) => crate::toast(a, e),
                }
                sync_ui(a);
                space_open(a);
            });
        });
    }
    {
        let w = app.as_weak();
        e.on_delete(move || {
            let Some(a) = w.upgrade() else { return };
            let Some(r) = rec() else { return };
            let picked = space_picked(&a);
            let dl = a.global::<Ecm>().get_dl_first();
            let only: Vec<String> = picked.iter().filter(|x| !x.local).map(|x| x.rel.to_string()).collect();
            let paths: Vec<String> = picked.iter().map(|x| x.path.to_string()).collect();
            let n = paths.len();
            crate::confirm_box(&a, "Delete from Echo Cloud?", &format!("{n} file{} will be removed from Echo Cloud{}.", if n == 1 { "" } else { "s" }, if dl && !only.is_empty() { " (cloud-only clips are downloaded to this PC first)" } else { "" }), "Delete", move |a| {
                job(a, "Freeing up Echo Cloud space…", move |p| {
                    if dl {
                        for rel in &only {
                            r.cloud_download(rel)?;
                        }
                    }
                    p(0.5);
                    let n = r.echocloud_delete_files(&paths)?;
                    Ok(format!("{n} file{}", if n == 1 { "" } else { "s" }))
                }, |a, res| {
                    match res {
                        Ok(f) => crate::toast(a, format!("Deleted {f} from Echo Cloud")),
                        Err(e) => crate::toast(a, e),
                    }
                    crate::rec_core::sync_soon();
                    sync_ui(a);
                    space_open(a);
                });
            });
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
