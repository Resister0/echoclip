//! clip-thumbs (12 Nov 2026): a clip that's only in Echo Cloud (not on this PC) showed a black card in the Library and
//! Home's "Your games" - with game folders following the account (game_sync.rs), a 2nd PC could show whole folders of
//! them. Now the account keeps each clip's card picture (server /thumbs/*, cloud/src/clip_thumbs.js):
//! - send: every 5 min (30 s after start, right after sign-in) each clip that is on this PC AND in Echo Cloud and has
//!   its picture (<clips>/.thumbs/<name>.jpg, the 480 px JPEG the card shows) sends it once - also clips backed up by
//!   an older Echo. Sent again only when this PC's picture changes ("Use as thumbnail"); a picture the account already
//!   has from another PC is kept.
//! - fetch: cloud.rs fetch_thumbs (every 20 s) asks the account for cloud-only clips without a picture and writes it
//!   to the same .thumbs/<name>.jpg (+ .dur) a local clip has - the card loads it like any other, no extra memory.
//!   No picture on the account yet: asked again after 10 minutes (a PC with the clip may send it meanwhile).
//! Older servers answer 404: nothing happens.

use base64::Engine;
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// biggest picture sent (the server takes up to 128 KB; Echo's are ~15-40 KB)
const MAX_JPG: u64 = 120 * 1024;
/// pictures sent per round (a PC with many old backups fills them in over a few rounds)
const PER_ROUND: usize = 200;
/// a cloud-only clip the account has no picture for: asked again after this long
const RETRY: Duration = Duration::from_secs(600);

thread_local! {
    static BUSY: Cell<bool> = const { Cell::new(false) };
    static TIMER: std::cell::RefCell<Option<slint::Timer>> = const { std::cell::RefCell::new(None) };
}
/// rel -> when the account last had no picture for it (fetch side, any thread)
static MISSED: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);
/// keys this run's server said aren't in Echo Cloud (a clip only on the user's own cloud): not sent again this run
static NOT_EC: Mutex<Option<HashSet<String>>> = Mutex::new(None);
/// the old server answered 404 on /thumbs/*: nothing more this run
static OLD_SERVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn thumbs_dir() -> PathBuf {
    crate::library::find_clips_dir().join(".thumbs")
}

/// the clip's key on the account ("Echo/<path in the cloud>"), None for hidden folders (.shared, .swap)
fn ec_key(f: &echo_core::recorder::CloudFile) -> Option<String> {
    let p = if f.path.is_empty() { &f.rel } else { &f.path };
    let p = p.trim_start_matches('/');
    if p.is_empty() || p.split('/').any(|s| s.is_empty() || s.starts_with('.') || s == "..") {
        return None;
    }
    Some(format!("Echo/{p}"))
}

fn stem_of(rel: &str) -> String {
    std::path::Path::new(rel).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

fn stamp(p: &std::path::Path) -> Option<String> {
    let md = std::fs::metadata(p).ok()?;
    let t = md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some(format!("{}-{t}", md.len()))
}

fn old_server(e: &str) -> bool {
    // ec_call's error for an unknown route (the server's 404 page is HTML: "Echo Cloud answered 404")
    let old = e.contains("answered 404") || e.eq_ignore_ascii_case("not found");
    if old {
        OLD_SERVER.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    old
}

// ---------- send ----------

/// what this PC sent (<clips>/.thumbs/cloud_thumbs.json): the account it was for + key -> stamp of the picture
struct State {
    account: String,
    sent: BTreeMap<String, String>,
}
fn state_path() -> PathBuf {
    thumbs_dir().join("cloud_thumbs.json")
}
fn load_state(account: &str) -> State {
    let v: serde_json::Value = crate::library::read_text(&state_path()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    if v["account"].as_str() != Some(account) {
        return State { account: account.to_string(), sent: BTreeMap::new() };
    }
    State { account: account.to_string(), sent: serde_json::from_value(v["sent"].clone()).unwrap_or_default() }
}
fn save_state(s: &State) {
    let v = serde_json::json!({ "account": s.account, "sent": s.sent });
    let tmp = state_path().with_extension("tmp");
    if std::fs::write(&tmp, serde_json::to_string(&v).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, state_path());
    }
}

/// one round (worker thread): the pictures of this PC's clips in Echo Cloud that the account doesn't have yet
fn send_round(token: &str, account: &str) {
    if OLD_SERVER.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let Some(r) = crate::rec_core::rec() else { return };
    let dir = thumbs_dir();
    let not_ec = NOT_EC.lock().unwrap().clone().unwrap_or_default();
    let mut st = load_state(account);
    // key -> (picture, its stamp, rel)
    let mut cand: Vec<(String, PathBuf, String, String)> = vec![];
    let mut live: HashSet<String> = HashSet::new();
    for f in r.cloud_files() {
        let Some(key) = ec_key(&f) else { continue };
        live.insert(key.clone());
        if !f.local || not_ec.contains(&key) {
            continue;
        }
        let jpg = dir.join(format!("{}.jpg", stem_of(&f.rel)));
        let Some(s) = stamp(&jpg) else { continue };
        if st.sent.get(&key) == Some(&s) {
            continue;
        }
        cand.push((key, jpg, s, f.rel.clone()));
    }
    // clips no longer in the cloud: forgotten (the server deleted their pictures with them)
    let before = st.sent.len();
    st.sent.retain(|k, _| live.contains(k));
    let mut dirty = st.sent.len() != before;
    if cand.is_empty() {
        if dirty {
            save_state(&st);
        }
        return;
    }
    let mut server: HashMap<String, String> = HashMap::new();
    for part in cand.chunks(1000) {
        let paths: Vec<&str> = part.iter().map(|c| c.0.as_str()).collect();
        match crate::lib_net::ec_call("POST", "/thumbs/have", token, Some(serde_json::json!({ "paths": paths }))) {
            Ok(v) => {
                if let Some(m) = v["thumbs"].as_object() {
                    server.extend(m.iter().map(|(k, h)| (k.clone(), h.as_str().unwrap_or("").to_string())));
                }
            }
            Err(e) => {
                if !old_server(&e) {
                    crate::lib_net::log_line(&format!("clip pictures: list skipped ({e})"));
                }
                if dirty {
                    save_state(&st);
                }
                return;
            }
        }
    }
    let (mut sent, mut kept) = (0, 0);
    for (key, jpg, s, rel) in cand {
        // the account has one (from another PC, or this one before its state file was lost): kept, unless this PC's
        // own picture changed after it sent it
        if server.contains_key(&key) && !st.sent.contains_key(&key) {
            st.sent.insert(key, s);
            kept += 1;
            dirty = true;
            continue;
        }
        if sent >= PER_ROUND {
            continue;
        }
        let Ok(bytes) = std::fs::read(&jpg) else { continue };
        if bytes.len() < 100 || bytes.len() as u64 > MAX_JPG || !bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            continue;
        }
        let dur = std::fs::read_to_string(dir.join(format!("{}.dur", stem_of(&rel)))).ok().and_then(|t| t.trim().parse::<f64>().ok()).unwrap_or(0.0);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        match crate::lib_net::ec_call("POST", "/thumbs/put", token, Some(serde_json::json!({ "path": key, "jpg": b64, "dur": dur }))) {
            Ok(_) => {
                st.sent.insert(key, s);
                sent += 1;
                dirty = true;
            }
            Err(e) if e.contains("isn't in Echo Cloud") => {
                // only on the user's own cloud (or deleted meanwhile): not again this run
                NOT_EC.lock().unwrap().get_or_insert_with(HashSet::new).insert(key);
            }
            Err(e) => {
                crate::lib_net::log_line(&format!("clip pictures: stopped after {sent} ({e})"));
                break;
            }
        }
    }
    if dirty {
        save_state(&st);
    }
    if sent > 0 || kept > 0 {
        crate::lib_net::log_line(&format!("clip pictures: {sent} sent, {kept} already on the account"));
    }
}

/// a send round now (after sign-in, a new upload), on a worker
pub fn soon() {
    let token = crate::cloud::token();
    if token.is_empty() || !crate::cloud::has_ec() || BUSY.with(|b| b.replace(true)) {
        return;
    }
    let account = crate::rec_core::rec().map(|r| r.settings().ec_email).unwrap_or_default();
    crate::lib_net::bg(move || send_round(&token, &account), |_| BUSY.with(|b| b.set(false)));
}

/// 30 s after start, then every 5 minutes (like the cloud listing)
pub fn start() {
    slint::Timer::single_shot(Duration::from_secs(30), soon);
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_secs(300), soon);
    TIMER.with(|x| *x.borrow_mut() = Some(t));
}

// ---------- fetch ----------

/// should the account be asked for the picture of cloud-only clip `rel` now (signed in, not missed lately)
pub fn may_ask(rel: &str) -> bool {
    if OLD_SERVER.load(std::sync::atomic::Ordering::Relaxed) || crate::cloud::token().is_empty() || !crate::cloud::has_ec() {
        return false;
    }
    MISSED.lock().unwrap().as_ref().and_then(|m| m.get(rel)).is_none_or(|t| t.elapsed() > RETRY)
}

/// (worker thread) fetch the account's pictures of these cloud-only clips into .thumbs; true = at least one arrived
pub fn fetch(token: &str, rels: &[String]) -> bool {
    let Some(r) = crate::rec_core::rec() else { return false };
    let keys: HashMap<String, String> = r.cloud_files().iter().filter(|f| rels.contains(&f.rel)).filter_map(|f| Some((ec_key(f)?, f.rel.clone()))).collect();
    let dir = thumbs_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut got: HashSet<String> = HashSet::new();
    let list: Vec<&String> = keys.keys().collect();
    for part in list.chunks(40) {
        let v = match crate::lib_net::ec_call("POST", "/thumbs/get", token, Some(serde_json::json!({ "paths": part }))) {
            Ok(v) => v,
            Err(e) => {
                if !old_server(&e) {
                    crate::lib_net::log_line(&format!("clip pictures: fetch skipped ({e})"));
                }
                return !got.is_empty();
            }
        };
        let Some(m) = v["thumbs"].as_object() else { continue };
        for (key, t) in m {
            let Some(rel) = keys.get(key) else { continue };
            let Some(bytes) = t["jpg"].as_str().and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok()) else { continue };
            if !bytes.starts_with(&[0xff, 0xd8, 0xff]) {
                continue;
            }
            let stem = stem_of(rel);
            let to = dir.join(format!("{stem}.jpg"));
            let tmp = dir.join(format!("{stem}.jpg.part"));
            if std::fs::write(&tmp, &bytes).is_err() || std::fs::rename(&tmp, &to).is_err() {
                let _ = std::fs::remove_file(&tmp);
                continue;
            }
            let dur = t["dur"].as_f64().unwrap_or(0.0);
            let dfile = dir.join(format!("{stem}.dur"));
            if dur > 0.0 && !dfile.exists() {
                let _ = std::fs::write(&dfile, dur.to_string());
            }
            got.insert(rel.clone());
        }
    }
    let mut g = MISSED.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    for rel in rels {
        if got.contains(rel) {
            m.remove(rel);
        } else {
            m.insert(rel.clone(), Instant::now());
        }
    }
    if !got.is_empty() {
        crate::lib_net::log_line(&format!("clip pictures: {} fetched from the account", got.len()));
    }
    !got.is_empty()
}
