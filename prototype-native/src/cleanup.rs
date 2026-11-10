//! Session cleanup (10 Nov 2026): more ways to free space, for Tidy up (tidy.rs) and Echo Cloud's
//! Manage space (cloud.rs space manager).
//! - an edit and its original ("x" + "x (edit)" / "x (trim 2)"), by name
//! - clips on this PC with a full copy in the cloud (cloud.json)
//! - when a clip was last opened in Echo's player (.thumbs/opened.json - new, nothing tracked it before)
//! - share links Echo made and whether the clip was uploaded only for that link (.thumbs/link_hist.json - new;
//!   links.json forgets a link once it expired, so the history is kept here)
//! - the Echo Cloud categories: share link expired / not watched in 90+ days / original + edit / on this PC too
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn read(p: &Path) -> Value {
    std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
}

/// temp file + rename, so a crash never leaves half a file
fn write(p: &Path, v: &Value) {
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, p);
    }
}

fn norm(s: &str) -> String {
    s.replace('\\', "/").to_lowercase()
}

/// the original's name of an edit: "x (edit)" / "x (edit 2)" / "x (trim)" -> "x" (an edit of an edit too).
/// None = not an edit. Same pattern as library.rs is_edited: /\((edit|trim)( \d+)?\)$/
pub fn edit_base(name: &str) -> Option<String> {
    let mut n = name;
    let mut any = false;
    while let Some(i) = n.strip_suffix(')').and_then(|s| s.rfind('(')) {
        let inner = &n[i + 1..n.len() - 1];
        let (w, num) = inner.split_once(' ').unwrap_or((inner, ""));
        if !((w == "edit" || w == "trim") && num.chars().all(|c| c.is_ascii_digit())) {
            break;
        }
        n = n[..i].trim_end();
        any = true;
    }
    (any && !n.is_empty()).then(|| n.to_string())
}

/// groups of [original, its edits…] among (id, name) - only where both the original and an edit are there
pub fn edit_pairs(named: &[(usize, String)]) -> Vec<Vec<usize>> {
    let orig: HashMap<String, usize> = named.iter().filter(|(_, n)| edit_base(n).is_none()).map(|(i, n)| (n.to_lowercase(), *i)).collect();
    let mut groups: HashMap<usize, Vec<(String, usize)>> = HashMap::new();
    for (i, n) in named {
        if let Some(o) = edit_base(n).and_then(|b| orig.get(&b.to_lowercase())) {
            groups.entry(*o).or_default().push((n.to_lowercase(), *i));
        }
    }
    let mut out: Vec<Vec<usize>> = groups
        .into_iter()
        .map(|(o, mut e)| {
            e.sort();
            std::iter::once(o).chain(e.into_iter().map(|x| x.1)).collect()
        })
        .collect();
    out.sort();
    out
}

/// cloud.json's list: rel (lower-case, "/") -> size of the cloud copy
pub fn cloud_sizes(thumbs: &Path) -> HashMap<String, u64> {
    let v = read(&thumbs.join("cloud.json"));
    // av1-upload-fix (10 Nov 2026): a cloud copy that isn't the clip itself (Echo Cloud's smaller copy, an AV1
    // clip's H.264 copy) is complete when it's the one Echo uploaded: "smaller" = {rel: [cloud size, PC size]}
    let copy = |rel: &str, size: u64| {
        v["smaller"][rel].as_array().and_then(|a| (a.first()?.as_u64()? == size).then(|| a.get(1)?.as_u64()).flatten()).unwrap_or(size)
    };
    v["files"]
        .as_array()
        .map(|a| a.iter().filter_map(|f| { let rel = f["rel"].as_str()?; Some((norm(rel), copy(rel, f["size"].as_u64()?))) }).collect())
        .unwrap_or_default()
}

// ---------- when a clip was last opened ----------
thread_local! {
    /// .thumbs/opened.json: {"since": unix s (tracking started), "clips": {name lower-case: unix s}}
    static OPENED: RefCell<Option<(PathBuf, Value)>> = const { RefCell::new(None) };
}

fn opened_load(thumbs: &Path) -> Value {
    let p = thumbs.join("opened.json");
    OPENED.with(|o| {
        let mut o = o.borrow_mut();
        if o.as_ref().map(|x| &x.0) != Some(&p) {
            let mut v = read(&p);
            if !v["since"].is_u64() {
                // nothing was tracked before this Echo: plays count from today
                v = json!({ "since": now(), "clips": v["clips"].as_object().cloned().unwrap_or_default() });
                let _ = std::fs::create_dir_all(thumbs);
                write(&p, &v);
            }
            *o = Some((p, v));
        }
        o.as_ref().map(|x| x.1.clone()).unwrap_or(Value::Null)
    })
}

/// the player opened this clip (main.rs play)
pub fn opened(thumbs: &Path, name: &str) {
    let mut v = opened_load(thumbs);
    v["clips"][name.to_lowercase()] = json!(now());
    let p = thumbs.join("opened.json");
    OPENED.with(|o| *o.borrow_mut() = Some((p.clone(), v.clone())));
    // off the UI thread (a slow / network drive)
    std::thread::spawn(move || write(&p, &v));
}

/// (tracking started, name lower-case -> last opened)
pub fn opened_all(thumbs: &Path) -> (u64, HashMap<String, u64>) {
    let v = opened_load(thumbs);
    let m = v["clips"].as_object().map(|m| m.iter().filter_map(|(k, t)| Some((k.clone(), t.as_u64()?))).collect()).unwrap_or_default();
    (v["since"].as_u64().unwrap_or_else(now), m)
}

// ---------- share links Echo made ----------
/// a link was made (cloud.rs copy_link): `up` = the clip was uploaded just for this link
pub fn link_made(thumbs: &Path, rel: &str, url: &str, days: u32, up: bool) {
    let p = thumbs.join("link_hist.json");
    let mut v = read(&p);
    if !v.is_object() {
        v = json!({});
    }
    let k = norm(rel);
    let old = v[&k].clone();
    let t = now();
    let same = old["url"].as_str() == Some(url);
    v[&k] = json!({
        "url": url,
        "key": old["key"].as_str().filter(|_| same).unwrap_or(""),
        "expires": if days > 0 { t + days as u64 * 86400 } else { 0 },
        "made": if same { old["made"].as_u64().unwrap_or(t) } else { t },
        // once uploaded for a link it stays that way while it's the same link
        "up": up || (same && old["up"].as_bool().unwrap_or(false)),
    });
    let _ = std::fs::create_dir_all(thumbs);
    write(&p, &v);
}

/// the history with links.json merged in (links.json knows the Echo Cloud object and a lifetime changed later)
fn link_hist(thumbs: &Path) -> serde_json::Map<String, Value> {
    let p = thumbs.join("link_hist.json");
    let mut h = read(&p).as_object().cloned().unwrap_or_default();
    let mut changed = false;
    for (rel, e) in read(&thumbs.join("links.json")).as_object().cloned().unwrap_or_default() {
        let k = norm(&rel);
        let cur = h.get(&k).cloned().unwrap_or(Value::Null);
        let same = cur["url"].as_str() == e["url"].as_str();
        let n = json!({
            "url": e["url"],
            "key": e["key"],
            "expires": e["expires"].as_u64().unwrap_or(0),
            "made": if same { cur["made"].as_u64().unwrap_or(0) } else { 0 },
            "up": same && cur["up"].as_bool().unwrap_or(false),
        });
        if cur != n {
            h.insert(k, n);
            changed = true;
        }
    }
    if changed {
        write(&p, &Value::Object(h.clone()));
    }
    h
}

// ---------- Echo Cloud's Manage space ----------
/// a file's place in one category: picked by itself when the category is chosen (safe), and why it's there
#[derive(Clone)]
pub struct Why {
    pub cat: &'static str,
    pub safe: bool,
    pub note: String,
}

/// the chips above the list: key, label (Echo's words), what they free
pub const CATS: [(&str, &str); 4] =
    [("exp", "Share link expired"), ("old", "Not watched in 90+ days"), ("pair", "Original + edit"), ("pc", "Already on this PC")];

const DAY: u64 = 86400;
const OLD_DAYS: u64 = 90;

fn ago(t: u64) -> String {
    crate::library::ago(UNIX_EPOCH + Duration::from_secs(t))
}

/// every file's categories (by its path under Echo/). `live` = share links Echo Cloud says still work (None =
/// couldn't ask). `views` = share page views by clip name. `cfg` = cloud_upload and favourites decide whether a
/// clip was in the cloud only for its link.
pub fn ecm_cats(files: &[Value], thumbs: &Path, live: Option<&HashSet<String>>, views: &dyn Fn(&str) -> u64, upload: &str, favs: &HashSet<String>) -> HashMap<String, Vec<Why>> {
    let t = now();
    let mut out: HashMap<String, Vec<Why>> = HashMap::new();
    let mut add = |path: &str, w: Why| out.entry(path.to_string()).or_default().push(w);
    let hist = link_hist(thumbs);
    // links that still work (this PC's links.json, or Echo Cloud's own list - a link renewed on another PC)
    let works = |e: &Value| {
        let exp = e["expires"].as_u64().unwrap_or(0);
        exp == 0 || exp > t || e["url"].as_str().is_some_and(|u| live.is_some_and(|l| l.contains(u)))
    };
    let live_keys: HashSet<String> = hist.values().filter(|e| works(e)).filter_map(|e| e["key"].as_str()).map(norm).collect();
    let (since, opened) = opened_all(thumbs);
    let favs: HashSet<String> = favs.iter().map(|f| f.to_lowercase()).collect();
    // originals in the cloud, by name, for "Original + edit"
    let clips: Vec<&Value> = files.iter().filter(|f| !f["copy"].as_bool().unwrap_or(false)).collect();
    let named: Vec<(usize, String)> = clips.iter().enumerate().map(|(i, f)| (i, f["name"].as_str().unwrap_or("").to_string())).collect();
    for g in edit_pairs(&named) {
        let orig = clips[g[0]]["name"].as_str().unwrap_or("").to_string();
        for (k, &i) in g.iter().enumerate() {
            let note = if k == 0 { format!("original - {} in Echo Cloud too", if g.len() > 2 { "its edits are" } else { "its edit is" }) } else { format!("edit of “{orig}”") };
            add(clips[i]["path"].as_str().unwrap_or(""), Why { cat: "pair", safe: false, note });
        }
    }
    for f in files {
        let path = f["path"].as_str().unwrap_or("");
        let name = f["name"].as_str().unwrap_or("").to_lowercase();
        let copy = f["copy"].as_bool().unwrap_or(false);
        let local = f["local"].as_bool().unwrap_or(false);
        let modified = f["modified"].as_u64().unwrap_or(0);
        // Share link expired
        let key = norm(&format!("Echo/{path}"));
        if copy {
            // a share copy whose link (made on this PC) ended - Echo Cloud also deletes those by itself once a day
            let ended = hist.values().find(|e| e["key"].as_str().map(norm).as_deref() == Some(key.as_str()) && !works(e));
            if let Some(e) = ended.filter(|_| !live_keys.contains(&key)) {
                add(path, Why { cat: "exp", safe: true, note: format!("its link ended {}", ago(e["expires"].as_u64().unwrap_or(t))) });
            }
        } else if let Some(e) = hist.get(&norm(f["rel"].as_str().unwrap_or(""))) {
            let made = e["made"].as_u64().unwrap_or(0);
            // uploaded for the link (not later by the backup), the backup wouldn't have uploaded it, the link ended
            let for_link = e["up"].as_bool().unwrap_or(false) && made > 0 && modified <= made + DAY;
            let backup = upload == "all" || (upload == "favs" && favs.contains(&name));
            if for_link && !backup && !works(e) && !live_keys.contains(&key) {
                let note = format!("uploaded for a share link that ended {}{}", ago(e["expires"].as_u64().unwrap_or(t)), if local { "" } else { " - the only copy" });
                add(path, Why { cat: "exp", safe: local, note });
            }
        }
        if copy {
            continue;
        }
        // Not watched in 90+ days: not opened in Echo's player and no share page views since it went up. Plays only
        // count from when this Echo started tracking them (`since`), so nothing shows here for the first 90 days.
        let last = modified.max(since).max(opened.get(&name).copied().unwrap_or(0));
        if modified > 0 && t.saturating_sub(last) >= OLD_DAYS * DAY && views(&name) == 0 {
            // (rclone's ModTime can be the clip's own time rather than the upload, so no "uploaded …" date here)
            let seen = opened.get(&name).map(|&o| format!("last opened {}", ago(o))).unwrap_or_else(|| "not opened or watched in 90+ days".into());
            add(path, Why { cat: "old", safe: false, note: seen });
        }
        if local {
            add(path, Why { cat: "pc", safe: false, note: String::new() });
        }
    }
    out
}

/// what choosing a category frees: (files, bytes). "Original + edit" counts the originals (keep the edits).
pub fn cat_size(files: &[Value], cats: &HashMap<String, Vec<Why>>, cat: &str) -> (usize, u64) {
    let mut n = 0;
    let mut b = 0;
    for f in files {
        let Some(w) = cats.get(f["path"].as_str().unwrap_or("")).and_then(|v| v.iter().find(|w| w.cat == cat)) else { continue };
        n += 1;
        if cat != "pair" || w.note.starts_with("original") {
            b += f["size"].as_u64().unwrap_or(0);
        }
    }
    (n, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bases() {
        assert_eq!(edit_base("Valorant 2026-11-01 (edit)").as_deref(), Some("Valorant 2026-11-01"));
        assert_eq!(edit_base("x (edit 2)").as_deref(), Some("x"));
        assert_eq!(edit_base("x (trim) (edit)").as_deref(), Some("x"));
        assert_eq!(edit_base("x (final)"), None);
        assert_eq!(edit_base("x"), None);
        let named = vec![(0, "a".to_string()), (1, "a (edit)".to_string()), (2, "b (edit)".to_string()), (3, "c".to_string())];
        assert_eq!(edit_pairs(&named), vec![vec![0, 1]]);
    }
}
