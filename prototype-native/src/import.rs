//! Settings > Clips & Storage > "Bring in clips from other apps": Medal / SteelSeries Moments / Outplayed / Insights
//! folders found on this PC (port of Echo's recorder.rs import_sources).
//! William (native chat 5): in the old Echo the Medal line kept showing the OLD number of clips / GB after a friend
//! deleted most of his Medal clips in Explorer. Here the folders are read again every time the page shows them and
//! every few seconds while it's open, files that are gone are simply not counted, and clips Echo already imported
//! (Echo's .thumbs/imported.json) are counted apart - so the line always says what an import would bring in now.
//! The import itself stays look-only in the prototype; the real port re-reads the folder when Import is clicked
//! (a file deleted in between is skipped, not an error).
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, PartialEq)]
pub struct Source {
    pub app: String,
    pub path: PathBuf,
    /// clips an import would copy now
    pub new: usize,
    pub new_bytes: u64,
    /// clips still there that Echo already imported
    pub done: usize,
}

const VIDEO_EXTS: [&str; 12] = ["mp4", "mov", "m4v", "mkv", "webm", "avi", "ts", "flv", "wmv", "mpg", "mpeg", "3gp"];

/// Medal keeps clips in <capture folder>\Clips next to Editor / Thumbnails: start there when it exists
fn import_root(p: &Path) -> PathBuf {
    let c = p.join("Clips");
    if c.is_dir() {
        c
    } else {
        p.to_path_buf()
    }
}

/// every video under `dir` that is there NOW, with its size (a file deleted while we look is skipped)
fn collect_videos(dir: &Path, depth: u32, out: &mut Vec<(PathBuf, u64)>) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let n = e.file_name().to_string_lossy().to_lowercase();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            // "Shared Clips" (SteelSeries) are extra copies made for sharing - skip them
            if n.starts_with('.') || ["thumbnails", "thumbs", "editor", "cache", "temp", "tmp", "screenshots", "logs", "shared clips"].contains(&n.as_str()) {
                continue;
            }
            collect_videos(&p, depth + 1, out);
        } else {
            let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            if !VIDEO_EXTS.contains(&ext.as_str()) || n.contains(".tmp") || n.contains(".part") {
                continue;
            }
            // metadata fails for a file deleted a moment ago / a broken shortcut: not counted
            if let Ok(md) = std::fs::metadata(&p) {
                if md.is_file() {
                    out.push((p, md.len()));
                }
            }
        }
    }
}

/// folder paths ("D:\Clips") written inside small settings files of another app (covers a moved capture folder)
fn paths_in_settings(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if depth > 3 || out.len() > 40 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            paths_in_settings(&p, depth + 1, out);
            continue;
        }
        let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !["json", "ini", "cfg", "config", "xml"].contains(&ext.as_str()) || std::fs::metadata(&p).map_or(true, |m| m.len() > 2_000_000) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let b = text.as_bytes();
        for i in 1..b.len().saturating_sub(2) {
            if b[i] == b':' && b[i + 1] == b'\\' && b[i - 1].is_ascii_alphabetic() && (i < 2 || !b[i - 2].is_ascii_alphanumeric()) {
                let rest = &text[i - 1..];
                let end = rest.find(|c: char| c == '"' || c == '\'' || c == '\n' || c == '\r' || c == '<' || c == ',').unwrap_or(rest.len());
                let raw = rest[..end].replace("\\\\", "\\").replace('/', "\\");
                let q = PathBuf::from(raw.trim_end_matches('\\'));
                // a clip file (Medal's list of clips, also ones deleted since) -> its folder; folders that are
                // gone are dropped later (is_dir)
                let q = if q.extension().is_some_and(|x| VIDEO_EXTS.contains(&x.to_string_lossy().to_lowercase().as_str())) {
                    q.parent().map(|p| p.to_path_buf()).unwrap_or(q)
                } else {
                    q
                };
                if !out.contains(&q) && out.len() <= 40 {
                    out.push(q);
                }
            }
        }
    }
}

/// Echo's imported.json: "path in lower case|size" of every file it already copied
fn imported(clips_dir: &Path) -> HashSet<String> {
    std::fs::read_to_string(clips_dir.join(".thumbs").join("imported.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// read the folders now (no cache)
pub fn scan(clips_dir: &Path) -> Vec<Source> {
    let env = |k: &str| std::env::var(k).map(PathBuf::from).unwrap_or_default();
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).map(PathBuf::from).unwrap_or_default();
    let (appdata, progdata) = (env("APPDATA"), env("ProgramData"));
    let vids = home.join("Videos");
    let mut cands: Vec<(String, PathBuf)> = [
        ("Medal", vids.join("Medal")),
        ("Medal", home.join("Medal")),
        ("SteelSeries Moments", vids.join("SteelSeries Moments")),
        ("SteelSeries Moments", vids.join("SteelSeries")),
        ("SteelSeries Moments", vids.join("Moments")),
        ("SteelSeries Moments", home.join("SteelSeries Moments")),
        ("Outplayed", vids.join("Overwolf").join("Outplayed")),
        ("Outplayed", vids.join("Outplayed")),
        ("Insights", vids.join("Insights Capture")),
    ]
    .into_iter()
    .map(|(a, p)| (a.to_string(), p))
    .collect();
    if cfg!(windows) {
        for d in ["C", "D", "E"] {
            cands.insert(0, ("Medal".into(), PathBuf::from(format!(r"{d}:\Medal"))));
        }
        for (app, d) in [("Medal", appdata.join("Medal")), ("SteelSeries Moments", progdata.join("SteelSeries").join("GG"))] {
            let mut found = Vec::new();
            if !d.as_os_str().is_empty() {
                paths_in_settings(&d, 0, &mut found);
            }
            cands.extend(found.into_iter().map(|p| (app.to_string(), p)));
        }
    }
    // test hook: ECHO_IMPORT_DIRS="Medal=/some/folder;Outplayed=/other"
    if let Ok(t) = std::env::var("ECHO_IMPORT_DIRS") {
        for part in t.split(';') {
            if let Some((app, p)) = part.split_once('=') {
                cands.insert(0, (app.to_string(), PathBuf::from(p)));
            }
        }
    }
    let seen = imported(clips_dir);
    let mut out: Vec<Source> = Vec::new();
    for (app, p) in cands {
        let p = import_root(&p);
        if !p.is_dir() || p.starts_with(clips_dir) || clips_dir.starts_with(&p) {
            continue;
        }
        if out.iter().any(|s| p.starts_with(&s.path) || s.path.starts_with(&p)) {
            continue;
        }
        let mut v = Vec::new();
        collect_videos(&p, 0, &mut v);
        if v.is_empty() {
            continue; // everything deleted: the line goes
        }
        let (mut new, mut new_bytes, mut done) = (0, 0u64, 0);
        for (f, n) in &v {
            if seen.contains(&format!("{}|{}", f.to_string_lossy().to_lowercase(), n)) {
                done += 1;
            } else {
                new += 1;
                new_bytes += n;
            }
        }
        out.push(Source { app, path: p, new, new_bytes, done });
    }
    out
}

thread_local! {
    static LAST: RefCell<Option<(Instant, PathBuf, Vec<Source>)>> = const { RefCell::new(None) };
    /// r2-threads (L28): a scan is running on a worker
    static BUSY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// r2-threads (L28): redraws the Settings page when a scan found something new (main.rs)
    static REFRESH: RefCell<Option<std::rc::Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

/// r2-threads (L28): who redraws the Settings page when the other apps' folders changed
pub fn set_refresh(f: std::rc::Rc<dyn Fn()>) {
    REFRESH.with(|r| *r.borrow_mut() = Some(f));
}

/// r2-threads (L28): read the folders again on a worker; the page is redrawn when the answer changed
fn rescan(clips_dir: &Path) {
    if BUSY.with(|b| b.replace(true)) {
        return;
    }
    let (d, d2) = (clips_dir.to_path_buf(), clips_dir.to_path_buf());
    crate::lib_net::bg(
        move || scan(&d),
        move |v| {
            BUSY.with(|b| b.set(false));
            let same = LAST.with(|l| l.borrow().as_ref().is_some_and(|x| x.1 == d2 && x.2 == v));
            LAST.with(|l| *l.borrow_mut() = Some((Instant::now(), d2, v)));
            if !same {
                if let Some(f) = REFRESH.with(|r| r.borrow().clone()) {
                    f();
                }
            }
        },
    );
}

/// the folders as last read (never waits: r2-threads L28); read again on a worker when that was more than 1.5 s ago
/// (the Settings page rebuilds on every change)
pub fn sources(clips_dir: &Path) -> Vec<Source> {
    let (v, fresh) = LAST.with(|l| match l.borrow().as_ref().filter(|x| x.1 == clips_dir) {
        Some((t, _, v)) => (v.clone(), t.elapsed() < Duration::from_millis(1500)),
        None => (vec![], false),
    });
    if !fresh {
        rescan(clips_dir);
    }
    v
}

/// read again (on a worker); the page is redrawn by `set_refresh`'s callback when something changed (clips deleted
/// / added in the other app). Always false now - kept for the Settings timer in main.rs.
pub fn changed(clips_dir: &Path) -> bool {
    rescan(clips_dir);
    false
}

/// one Settings line per app: (label, sub, button)
pub fn lines(clips_dir: &Path) -> Vec<(String, String, String)> {
    let n = |k: usize, w: &str| format!("{k} {w}{}", if k == 1 { "" } else { "s" });
    sources(clips_dir)
        .into_iter()
        .map(|s| {
            let path = s.path.to_string_lossy().to_string();
            if s.new == 0 {
                {
                let all = if s.done == 1 { "its 1 clip is already in Echo".to_string() } else { format!("all {} are already in Echo", n(s.done, "clip")) };
                (format!("{}: nothing new", s.app), format!("{path} · {all}"), String::new())
            }
            } else {
                let done = if s.done > 0 { format!(" · {} already in Echo", n(s.done, "clip")) } else { String::new() };
                (format!("{}: {} ({})", s.app, n(s.new, "clip"), crate::library::fmt_size(s.new_bytes)), format!("{path}{done}"), "Import".into())
            }
        })
        .collect()
}
