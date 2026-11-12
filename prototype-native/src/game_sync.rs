//! cloud-games (12 Nov 2026): a game with clips in Echo Cloud shows under Folders on every PC signed in to that
//! account, with its icon - also on a PC that never ran the game. The folder itself already comes with the cloud
//! listing (clips are stored as Echo/<folder>/<clip>.mp4, so a cloud-only clip makes its folder, and the folder goes
//! when its last cloud clip does). What a 2nd PC was missing is the icon: it only lived in this PC's
//! <Echo's data folder>/icons/<game>.png. Now each PC sends the icons of its games that have cloud clips to the
//! account once (server /games, one per game per account, the same picture again is no write), and fetches the
//! account's icons for cloud games it has no icon of into icons/cloud/<folder>.png (library.rs game_icon reads them
//! after its own icons). Older servers answer 404: nothing happens.

use base64::Engine;
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

/// biggest icon sent (Echo's are 64x64 PNGs of ~1-5 KB; the server takes up to 64 KB)
const MAX_ICON: u64 = 64 * 1024;

thread_local! {
    static BUSY: Cell<bool> = const { Cell::new(false) };
    static TIMER: std::cell::RefCell<Option<slint::Timer>> = const { std::cell::RefCell::new(None) };
}

fn safe(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect()
}

/// the account's key of a game folder (the server's k: the folder name in lower case)
pub fn key(folder: &str) -> String {
    folder.trim().to_lowercase()
}

fn cloud_dir() -> PathBuf {
    crate::settings::path().parent().map(|p| p.join("icons").join("cloud")).unwrap_or_default()
}

/// where the account's icon of top folder `folder` is kept on this PC (may not exist)
pub fn icon_file(folder: &str) -> PathBuf {
    cloud_dir().join(format!("{}.png", safe(&key(folder))))
}

/// what was sent / fetched before (icons/cloud/state.json)
#[derive(Default)]
struct State {
    /// key -> "<size>-<modified>" of this PC's own icon when it was sent (sent again only when it changed)
    sent: BTreeMap<String, String>,
    /// key -> the server's hash of the icon fetched into icons/cloud
    got: BTreeMap<String, String>,
}

fn state_path() -> PathBuf {
    cloud_dir().join("state.json")
}
fn load_state() -> State {
    let v: serde_json::Value = crate::library::read_text(&state_path()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let map = |k: &str| serde_json::from_value::<BTreeMap<String, String>>(v[k].clone()).unwrap_or_default();
    State { sent: map("sent"), got: map("got") }
}
fn save_state(s: &State) {
    let _ = std::fs::create_dir_all(cloud_dir());
    let v = serde_json::json!({ "sent": s.sent, "got": s.got });
    let _ = std::fs::write(state_path(), serde_json::to_string_pretty(&v).unwrap_or_default());
}

/// the top folders (sidebar games) that have at least one clip in the cloud, by key -> name as shown
fn cloud_games() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(r) = crate::rec_core::rec() else { return out };
    for f in r.cloud_files() {
        let g = crate::library::game_of_rel(&f.rel);
        let top = g.split('/').next().unwrap_or("").trim();
        if top.is_empty() || top.starts_with('.') || top.eq_ignore_ascii_case("sounds") {
            continue;
        }
        out.entry(key(top)).or_insert_with(|| top.to_string());
    }
    out
}

/// one round (worker thread): send this PC's icons the account lacks, fetch the account's icons this PC lacks.
/// true = an icon file in icons/cloud changed (the sidebar is redrawn).
fn run(token: &str) -> bool {
    let games = cloud_games();
    if games.is_empty() {
        return false;
    }
    let list = match crate::lib_net::ec_call("GET", "/games", token, None) {
        Ok(v) => v,
        Err(e) => {
            crate::lib_net::log_line(&format!("cloud games: list skipped ({e})"));
            return false;
        }
    };
    // server key -> hash
    let server: HashMap<String, String> = list["games"]
        .as_array()
        .map(|a| a.iter().filter_map(|g| Some((g["k"].as_str()?.to_string(), g["hash"].as_str().unwrap_or("").to_string()))).collect())
        .unwrap_or_default();
    let alias: BTreeMap<String, String> = crate::library::echo_settings()
        .and_then(|v| v.get("folder_alias").cloned())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let mut st = load_state();
    let (mut sent, mut fetched) = (0, 0);
    let mut need: Vec<String> = vec![];
    for (k, name) in &games {
        match crate::library::LibModel::local_icon_file(name, &alias) {
            Some(p) => {
                let Ok(md) = std::fs::metadata(&p) else { continue };
                let stamp = format!("{}-{}", md.len(), md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs()));
                if st.sent.get(k) == Some(&stamp) || md.len() == 0 || md.len() > MAX_ICON {
                    continue;
                }
                // the account already has an icon for this game (another PC sent it): kept - this PC sends its own
                // only when it changes later
                if server.contains_key(k) && !st.sent.contains_key(k) {
                    st.sent.insert(k.clone(), stamp);
                    continue;
                }
                let Ok(bytes) = std::fs::read(&p) else { continue };
                let png = base64::engine::general_purpose::STANDARD.encode(&bytes);
                match crate::lib_net::ec_call("POST", "/games/icon", token, Some(serde_json::json!({ "name": name, "png": png }))) {
                    Ok(_) => {
                        st.sent.insert(k.clone(), stamp);
                        sent += 1;
                    }
                    Err(e) => crate::lib_net::log_line(&format!("cloud games: icon of {name} not sent ({e})")),
                }
            }
            None => {
                if let Some(h) = server.get(k) {
                    if st.got.get(k) != Some(h) || !icon_file(name).is_file() {
                        need.push(k.clone());
                    }
                }
            }
        }
    }
    if !need.is_empty() {
        match crate::lib_net::ec_call("POST", "/games/icons", token, Some(serde_json::json!({ "keys": need }))) {
            Ok(v) => {
                let _ = std::fs::create_dir_all(cloud_dir());
                for k in &need {
                    let Some(b64) = v["icons"][k.as_str()].as_str() else { continue };
                    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) else { continue };
                    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                        continue;
                    }
                    let to = icon_file(games.get(k).map_or(k.as_str(), |n| n.as_str()));
                    let tmp = to.with_extension("tmp");
                    if std::fs::write(&tmp, &bytes).is_ok() && std::fs::rename(&tmp, &to).is_ok() {
                        st.got.insert(k.clone(), v["hashes"][k.as_str()].as_str().unwrap_or("").to_string());
                        fetched += 1;
                    }
                }
            }
            Err(e) => crate::lib_net::log_line(&format!("cloud games: icons not fetched ({e})")),
        }
    }
    save_state(&st);
    if sent > 0 || fetched > 0 {
        crate::lib_net::log_line(&format!("cloud games: {sent} icon(s) sent, {fetched} fetched ({} games in the cloud)", games.len()));
    }
    fetched > 0
}

/// a round now (after sign-in), on a worker; the sidebar follows when an icon arrived
pub fn soon() {
    let token = crate::cloud::token();
    if token.is_empty() || !crate::cloud::has_ec() || BUSY.with(|b| b.replace(true)) {
        return;
    }
    crate::lib_net::bg(
        move || run(&token),
        |changed| {
            BUSY.with(|b| b.set(false));
            if changed {
                crate::UI_SYNC.with(|f| {
                    if let Some(f) = f.borrow().as_ref() {
                        f(true)
                    }
                });
            }
        },
    );
}

/// 30 s after start, then every 5 minutes (like Echo's cloud listing)
pub fn start() {
    slint::Timer::single_shot(Duration::from_secs(30), soon);
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_secs(300), soon);
    TIMER.with(|x| *x.borrow_mut() = Some(t));
}

