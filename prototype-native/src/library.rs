//! The library: reads the real clips folder + Echo's existing thumbnails (<clips>/.thumbs/<stem>.jpg, .dur,
//! .scrub.jpg) and Echo's favourites + tags/notes (favorites.json, meta.json - the same files Echo uses).
//! Writes ONLY favorites.json and meta.json (atomic tmp + rename, other fields kept). Rename / move / delete are
//! look-only until the real port calls recorder.rs (they also have to update cloud.json).
//! Thumbnails are decoded on a background thread and only for cards on (or near) the screen; at most CACHE_MAX
//! stay in memory, the least recently shown are dropped first.
use crate::{Card, FLine, FOpt, GridRow, NavItem};
use slint::{Image, Model, ModelNotify, ModelRc, ModelTracker, Rgb8Pixel, SharedPixelBuffer, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::SystemTime;

// 40 (was 64): the card pictures are 720 px now (9 Nov) - still more than a screenful, about the same memory
const CACHE_MAX: usize = 40;

pub struct Clip {
    pub path: PathBuf,
    /// file name without extension = Echo's key for favourites / meta / thumbnails
    pub name: String,
    /// folder inside the clips folder ("" = top level, "FiveM/Heists" = sub-folder)
    pub folder: String,
    /// Echo's gameOf(): the folder, else the name before the date ("" = no game)
    pub game: String,
    pub shown: String,
    pub mtime: SystemTime,
    pub secs: i64,
    pub size: u64,
    pub dur: Option<f64>,
    pub thumbs: PathBuf,
    /// a picture made by the prototype itself (trimmed copies) instead of Echo's .thumbs/<name>.jpg
    pub own_thumb: Option<PathBuf>,
    pub edited: bool,
    pub rec: bool,
    pub sound: bool,
    /// the file was deleted outside Echo (file manager): kept as an id, never shown or counted
    pub gone: std::cell::Cell<bool>,
    /// Chat B: a cloud-only clip (in Echo's cloud.json, not on this PC): its rel ("Valorant/x.mp4"); path = where
    /// it would be on this PC (missing). It plays from the cloud (cloud::play_path).
    pub cloud: Option<String>,
}

impl Clip {
    pub fn thumb(&self) -> PathBuf {
        if let Some(t) = &self.own_thumb {
            return t.clone();
        }
        self.thumbs.join(format!("{}.jpg", self.name))
    }
    pub fn scrub(&self) -> PathBuf {
        self.thumbs.join(format!("{}.scrub.jpg", self.name))
    }
}

pub fn find_clips_dir() -> PathBuf {
    // a folder given on the command line (testing) - never a flag: "Start with Windows" runs Echo with
    // --autostart, which used to become the clips folder = 0 clips after a reboot until Echo was restarted by hand
    // (William, 10 Nov 2026)
    if let Some(a) = std::env::args().skip(1).find(|a| !a.starts_with('-') && Path::new(a).is_dir()) {
        return PathBuf::from(a);
    }
    if let Ok(d) = std::env::var("ECHO_CLIPS") {
        return PathBuf::from(d);
    }
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_default();
    // Echo's own settings file (same place Echo reads it)
    let mut cands = vec![];
    if let Ok(p) = std::env::var("ECHO_SETTINGS") {
        cands.push(PathBuf::from(p));
    }
    if let Ok(a) = std::env::var("APPDATA") {
        cands.push(PathBuf::from(a).join("Clipper").join("settings.json"));
    }
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        cands.push(PathBuf::from(x).join("Clipper").join("settings.json"));
    }
    cands.push(PathBuf::from(&home).join(".config").join("Clipper").join("settings.json"));
    for c in cands {
        if let Some(d) = std::fs::read_to_string(&c)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("clips_dir").and_then(|d| d.as_str()).map(PathBuf::from))
        {
            if d.is_dir() {
                return d;
            }
        }
    }
    PathBuf::from(home).join("Videos").join("EchoClips")
}

/// Echo's DATE_RE: " YYYY-MM-DD HH-MM-SS..." at the end of a name -> where it starts
fn date_at(name: &str) -> Option<usize> {
    let b = name.as_bytes();
    // " dddd-dd-dd dd-dd-dd" = 20 bytes
    (0..b.len().saturating_sub(19)).find(|&i| {
        let p = &b[i..i + 20];
        let d = |k: usize| p[k].is_ascii_digit();
        p[0] == b' ' && d(1) && d(2) && d(3) && d(4) && p[5] == b'-' && d(6) && d(7) && p[8] == b'-' && d(9) && d(10) && p[11] == b' '
            && d(12) && d(13) && p[14] == b'-' && d(15) && d(16) && p[17] == b'-' && d(18) && d(19)
    })
}

fn is_edited(name: &str) -> bool {
    // /\((edit|trim)( \d+)?\)$/
    let Some(inner) = name.strip_suffix(')').and_then(|s| s.rfind('(').map(|i| &s[i + 1..])) else { return false };
    let (w, n) = inner.split_once(' ').unwrap_or((inner, ""));
    (w == "edit" || w == "trim") && (n.is_empty() || n.chars().all(|c| c.is_ascii_digit()))
}

pub fn scan(root: &Path) -> Vec<Clip> {
    let thumbs = root.join(".thumbs");
    let mut out = vec![];
    fn walk(dir: &Path, rel: &str, depth: u32, thumbs: &Path, out: &mut Vec<Clip>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                if depth < 5 {
                    let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                    walk(&p, &r, depth + 1, thumbs, out);
                }
                continue;
            }
            let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            if !matches!(ext.as_str(), "mp4" | "mkv" | "mov" | "webm") {
                continue;
            }
            if let Some(c) = make_clip(&p, rel, thumbs, &md) {
                out.push(c);
            }
        }
    }
    walk(root, "", 0, &thumbs, &mut out);
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    out
}

fn make_clip(p: &Path, rel: &str, thumbs: &Path, md: &std::fs::Metadata) -> Option<Clip> {
    let stem = p.file_stem()?.to_string_lossy().to_string();
    let dur = std::fs::read_to_string(thumbs.join(format!("{stem}.dur"))).ok().and_then(|s| s.trim().parse().ok());
    let dated = date_at(&stem);
    let game = if !rel.is_empty() {
        rel.to_string()
    } else {
        match dated {
            Some(i) if !matches!(&stem[..i], "Clip" | "Montage" | "Recording") => stem[..i].to_string(),
            _ => String::new(),
        }
    };
    let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    Some(Clip {
        shown: shown_name(&stem),
        edited: is_edited(&stem),
        rec: stem.starts_with("Recording"),
        sound: game.eq_ignore_ascii_case("sounds"),
        thumbs: thumbs.to_path_buf(),
        own_thumb: None,
        gone: Default::default(),
        cloud: None,
        name: stem,
        folder: rel.to_string(),
        game,
        secs: mtime.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0),
        mtime,
        size: md.len(),
        dur,
        path: p.to_path_buf(),
    })
}

/// Chat B: a cloud-only clip (Echo's cloud.json entry that isn't on this PC)
pub fn cloud_clip(root: &Path, rel: &str, size: u64, modified: u64) -> Option<Clip> {
    let p = root.join(rel);
    let stem = p.file_stem()?.to_string_lossy().to_string();
    let folder = rel.rsplit_once('/').map(|x| x.0.to_string()).unwrap_or_default();
    let thumbs = root.join(".thumbs");
    let dur = std::fs::read_to_string(thumbs.join(format!("{stem}.dur"))).ok().and_then(|s| s.trim().parse().ok());
    let game = if !folder.is_empty() {
        folder.clone()
    } else {
        match date_at(&stem) {
            Some(i) if !matches!(&stem[..i], "Clip" | "Montage" | "Recording") => stem[..i].to_string(),
            _ => String::new(),
        }
    };
    let mtime = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(modified);
    Some(Clip {
        shown: shown_name(&stem),
        edited: is_edited(&stem),
        rec: stem.starts_with("Recording"),
        sound: game.eq_ignore_ascii_case("sounds"),
        thumbs,
        own_thumb: None,
        gone: Default::default(),
        cloud: Some(rel.to_string()),
        name: stem,
        folder,
        game,
        secs: modified as i64,
        mtime,
        size,
        dur,
        path: p,
    })
}

/// Echo's shownName: the name without " (Echo)" (that part is only in the file name, for Discord). Chat D: also for
/// renamed clips without a date - renaming keeps " (Echo)" in the file name, so it must not show up then either.
pub fn shown_name(stem: &str) -> String {
    if date_at(stem).is_some() {
        return stem.replace(" (Echo)", "");
    }
    stem.strip_suffix(" (Echo)").unwrap_or(stem).to_string()
}

/// a clip file that appeared after the scan (e.g. a trimmed copy), `root` = the clips folder
/// every real folder on disk under the clips folder, 5 levels deep (also empty ones, like Echo's sidebar)
fn walk_dirs(root: &Path) -> BTreeSet<String> {
    fn walk(dir: &Path, rel: &str, depth: u32, out: &mut BTreeSet<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            if !e.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            if folder_reserved(&r) {
                continue;
            }
            out.insert(r.clone());
            if depth < 4 {
                walk(&e.path(), &r, depth + 1, out);
            }
        }
    }
    let mut set = BTreeSet::new();
    walk(root, "", 0, &mut set);
    set
}

/// r2-threads (L3): what the disk sync reads from the disk - gathered on a worker, applied by `LibModel::sync_apply`
pub struct DiskScan {
    root_ok: bool,
    /// L29: the folders on disk (the sidebar's folder list)
    dirs: BTreeSet<String>,
    on_disk: HashSet<PathBuf>,
    conly: Vec<(String, u64, u64)>,
    /// clip files the library doesn't know yet, already read (settled: not written to for 3 s)
    new_clips: Vec<Clip>,
}

/// r2-threads (L3): the clips tree (depth 5), Echo's cloud-only files and the new clips (`known` = the library's
/// files, `LibModel::known_paths`). Blocking: run it on a worker.
pub fn disk_scan(root: &Path, known: HashSet<PathBuf>) -> DiskScan {
    let mut on_disk: HashSet<PathBuf> = HashSet::new();
    fn walk(dir: &Path, depth: u32, out: &mut HashSet<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => {
                    if depth < 5 {
                        walk(&p, depth + 1, out);
                    }
                }
                Ok(_) => {
                    let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
                    if matches!(ext.as_str(), "mp4" | "mkv" | "mov" | "webm") {
                        out.insert(p);
                    }
                }
                _ => {}
            }
        }
    }
    if !root.is_dir() {
        return DiskScan { root_ok: false, dirs: BTreeSet::new(), on_disk, conly: vec![], new_clips: vec![] };
    }
    walk(root, 0, &mut on_disk);
    let conly = crate::cloud::cloud_only_files();
    let mut new_clips = vec![];
    for p in on_disk.difference(&known) {
        // a new file still being written (a recording / export) shows once it's settled
        let settled = std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|d| d.as_secs() >= 3);
        if p.extension().is_some_and(|x| x == "tmp") || !settled {
            continue;
        }
        if let Some(c) = clip_from(p, root) {
            new_clips.push(c);
        }
    }
    DiskScan { root_ok: true, dirs: walk_dirs(root), on_disk, conly, new_clips }
}

pub fn clip_from(path: &Path, root: &Path) -> Option<Clip> {
    let md = std::fs::metadata(path).ok()?;
    let rel = path.parent()?.strip_prefix(root).ok()?.to_string_lossy().replace('\\', "/");
    make_clip(path, &rel, &root.join(".thumbs"), &md)
}

/// Echo's settings.json (hotkeys, storage limit), when it can be found
pub fn echo_settings() -> Option<serde_json::Value> {
    // Chat B: the same file as the Settings page / Echo's settings.rs (settings::path)
    json_cached(&crate::settings::path()) // r2-threads (L29): parsed again only when the file changed
}

/// r2-threads (L29): a JSON file, parsed again only when its size / time changed (or it was written in the last
/// 2 s - coarse file times). Per thread.
pub fn json_cached(p: &Path) -> Option<serde_json::Value> {
    type Stamp = (Option<SystemTime>, u64);
    thread_local! {
        static CACHE: RefCell<HashMap<PathBuf, (Stamp, Option<serde_json::Value>)>> = RefCell::new(HashMap::new());
    }
    let md = std::fs::metadata(p).ok();
    let stamp: Stamp = (md.as_ref().and_then(|m| m.modified().ok()), md.as_ref().map_or(0, |m| m.len()));
    let settled = stamp.0.and_then(|t| t.elapsed().ok()).is_some_and(|d| d.as_secs() >= 2);
    if md.is_some() && settled {
        if let Some(v) = CACHE.with(|c| c.borrow().get(p).filter(|x| x.0 == stamp).map(|x| x.1.clone())) {
            return v;
        }
    }
    let v: Option<serde_json::Value> = std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok());
    CACHE.with(|c| c.borrow_mut().insert(p.to_path_buf(), (stamp, v.clone())));
    v
}

/// Echo's fmtLen: 30 -> "30s", 180 -> "3m", 90 -> "1.5m"
pub fn fmt_len(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else if s % 60 == 0 {
        format!("{}m", s / 60)
    } else {
        format!("{:.1}m", s as f64 / 60.0)
    }
}

pub fn fmt_dur(s: f64) -> String {
    let s = s.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Echo's ago()
pub fn ago(t: SystemTime) -> String {
    let d = SystemTime::now().duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
    let dt: chrono::DateTime<chrono::Local> = t.into();
    if d < 60 {
        "just now".into()
    } else if d < 3600 {
        format!("{} min ago", d / 60)
    } else if d < 86400 {
        let h = d / 3600;
        format!("{h} hour{} ago", if h > 1 { "s" } else { "" })
    } else if d < 7 * 86400 {
        dt.format("%A").to_string()
    } else {
        dt.format("%-d %b %Y").to_string()
    }
}

/// Echo's fmtSize (decimal units)
pub fn fmt_size(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else {
        format!("{:.1} MB", b / 1e6)
    }
}

fn day_start() -> i64 {
    let now = chrono::Local::now();
    now.date_naive().and_hms_opt(0, 0, 0).and_then(|d| d.and_local_timezone(chrono::Local).earliest()).map(|d| d.timestamp()).unwrap_or(0)
}

/// Echo's groupName(): Today / Yesterday / This week / "November 2026"
fn group_name(secs: i64, d0: i64) -> String {
    if secs >= d0 {
        "Today".into()
    } else if secs >= d0 - 86400 {
        "Yesterday".into()
    } else if secs >= d0 - 6 * 86400 {
        "This week".into()
    } else {
        chrono::DateTime::from_timestamp(secs, 0)
            .map(|d| d.with_timezone(&chrono::Local).format("%B %Y").to_string())
            .unwrap_or_default()
    }
}

/// natural order ("Clip 2" before "Clip 10"), case-insensitive - like localeCompare(..., {numeric: true})
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = a.peek().copied().filter(|c| c.is_ascii_digit()) {
                    na.push(c);
                    a.next();
                }
                let mut nb = String::new();
                while let Some(c) = b.peek().copied().filter(|c| c.is_ascii_digit()) {
                    nb.push(c);
                    b.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then(ta.cmp(tb));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                let o = x.to_lowercase().cmp(y.to_lowercase());
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// write a small JSON file the way Echo can't half-read it: temp file + rename
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("json.proto-tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    // Windows: rename over an existing file can fail for a moment while Echo reads it - retry briefly
    let mut last = String::new();
    for _ in 0..20 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = e.to_string();
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(last)
}

// ---------- folders (Echo's recorder.rs rules) ----------
/// Echo's own folders at the top level: never a game folder, never moved
const RESERVED_DIRS: [&str; 4] = ["Screenshots", "GIFs", "Audio", "Discord"];

/// Echo's clean_folder: "FiveM / Staff evidence\\" -> "FiveM/Staff evidence"
pub fn clean_folder(s: &str) -> String {
    s.split(|c| c == '/' || c == '\\')
        .map(|p| p.trim().chars().filter(|c| !r#":*?"<>|"#.contains(*c)).collect::<String>().trim().trim_end_matches('.').trim().to_string())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// hidden, or one of Echo's own folders at the top level
pub fn folder_reserved(rel: &str) -> bool {
    rel.split('/').any(|p| p.starts_with('.')) || rel.split('/').next().is_some_and(|f| RESERVED_DIRS.iter().any(|r| r.eq_ignore_ascii_case(f)))
}

/// "FiveM/Heists" -> "FiveM", "FiveM" -> ""
pub fn parent_of(f: &str) -> &str {
    f.rfind('/').map(|i| &f[..i]).unwrap_or("")
}

/// Windows: a folder with a clip open in a player / editor can't be moved for a moment (os error 5 / 32 / 33):
/// retry ~4 s like Echo's rename_patient, then a friendly message
fn rename_patient(from: &Path, to: &Path) -> Result<(), String> {
    let mut tries = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if tries < 16 && matches!(e.raw_os_error(), Some(5) | Some(32) | Some(33)) => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Err(e) => {
                return Err(if matches!(e.raw_os_error(), Some(5) | Some(32) | Some(33)) {
                    "A clip in this folder is open in another program (a video player or editor) - close it and try again".into()
                } else {
                    e.to_string()
                })
            }
        }
    }
}

/// to the Recycle Bin (Windows) / Trash (Linux: linux::trash - gio, else the Trash spec; never a plain delete) - Echo's delete_clip
fn recycle(p: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::{SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW};
        // pFrom must end with two NULs
        let mut from: Vec<u16> = p.as_os_str().to_string_lossy().encode_utf16().collect();
        from.extend([0, 0]);
        for k in 0..2 {
            let mut op = SHFILEOPSTRUCTW {
                hwnd: std::ptr::null_mut(),
                wFunc: FO_DELETE,
                pFrom: from.as_ptr(),
                pTo: std::ptr::null(),
                fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16,
                fAnyOperationsAborted: 0,
                hNameMappings: std::ptr::null_mut(),
                lpszProgressTitle: std::ptr::null(),
            };
            let r = unsafe { SHFileOperationW(&mut op) };
            if r == 0 && op.fAnyOperationsAborted == 0 && !p.exists() {
                return Ok(());
            }
            if k == 0 {
                // a preview / seek sheet of ours may still be reading it
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
        Err("Couldn't delete - the clip is open somewhere else (close the player / editor and try again)".into())
    }
    #[cfg(not(windows))]
    {
        // gio, else the Trash portal / the freedesktop Trash spec; never a permanent delete (gaps-linux row 39)
        crate::linux::trash(p)
    }
}

/// Chat D: Recycle Bin / Trash for any file (Screenshots page)
pub fn recycle_file(p: &Path) -> Result<(), String> {
    recycle(p)
}

/// r2-threads (L13): one clip to move (`LibModel::move_many_bg`), plain data for the worker
pub struct MoveJob {
    idx: usize,
    from: PathBuf,
    /// the folder on disk it goes to / its name in the library ("" = the main folder)
    dir: PathBuf,
    folder: String,
    shown: String,
}

/// r2-threads (L13): Echo's move_clip for one clip, on a worker. Err((the folder couldn't be made, message)).
fn move_file(j: &MoveJob, root: &Path) -> Result<PathBuf, (bool, String)> {
    std::fs::create_dir_all(&j.dir).map_err(|e| (true, e.to_string()))?;
    let new = j.dir.join(j.from.file_name().unwrap_or_default());
    if new.exists() {
        return Err((false, format!("{} already has a clip called \"{}\"", if j.folder.is_empty() { "The main folder" } else { &j.folder }, j.shown)));
    }
    rename_patient(&j.from, &new).map_err(|e| (false, e.replace("A clip in this folder is", &format!("\"{}\" is", j.shown))))?;
    // the old folder goes when it's empty now (Echo's remove_if_empty; an .echo-folder marker keeps it)
    if let Some(par) = j.from.parent().filter(|p| *p != root) {
        let _ = std::fs::remove_dir(par);
    }
    Ok(new)
}

/// r2-threads (L13): what moving a folder's files did (on a worker)
pub struct FolderFiles {
    /// merge: which file went where (None = the folder itself was renamed)
    merged: Option<HashMap<PathBuf, PathBuf>>,
    /// clips renamed on a name clash ("x" -> "x (2)"): their tags / note / favourite follow on the UI thread
    renamed: Vec<(String, String)>,
    err: Option<String>,
}

/// r2-threads (L13): Echo's folder merge, on a worker: every clip of `src` goes into `dst` (renamed "name (2)" on
/// clashes, their .thumbs files follow), its sub-folders come along unless `dst` has one with that name; `src`
/// goes when it's done
fn merge_files(src: &Path, dst: &Path, thumbs: &Path) -> FolderFiles {
    let mut ff = FolderFiles { merged: Some(HashMap::new()), renamed: vec![], err: None };
    let rd = match std::fs::read_dir(src) {
        Ok(rd) => rd,
        Err(e) => {
            ff.err = Some(e.to_string());
            return ff;
        }
    };
    for f in rd.flatten() {
        let p = f.path();
        if p.is_dir() {
            let to = dst.join(f.file_name());
            if !to.exists() {
                let _ = rename_patient(&p, &to);
            }
            continue;
        }
        if !p.is_file() || f.file_name() == ".echo-folder" {
            continue;
        }
        let stem = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let ext = p.extension().map(|x| format!(".{}", x.to_string_lossy())).unwrap_or_default();
        let mut to = dst.join(format!("{stem}{ext}"));
        let mut k = 2;
        while to.exists() {
            to = dst.join(format!("{stem} ({k}){ext}"));
            k += 1;
        }
        if let Err(e) = rename_patient(&p, &to) {
            ff.err = Some(e);
            return ff;
        }
        if let Some(m) = ff.merged.as_mut() {
            m.insert(p.clone(), to.clone());
        }
        let new_stem = to.file_stem().unwrap_or_default().to_string_lossy().to_string();
        if new_stem != stem {
            for x in ["jpg", "dur", "scrub.jpg", "strip.jpg", "strip2.jpg", "strip3.jpg", "a1.m4a", "a2.m4a", "a3.m4a", "a4.m4a", "a5.m4a", "marks.json", "levels.json", "words.json", "project.json"] {
                let _ = std::fs::rename(thumbs.join(format!("{stem}.{x}")), thumbs.join(format!("{new_stem}.{x}")));
            }
            ff.renamed.push((stem, new_stem));
        }
    }
    let _ = std::fs::remove_file(src.join(".echo-folder"));
    let _ = std::fs::remove_dir(src);
    ff
}

/// r2-threads (L7): a checked clip rename (`LibModel::rename_prep`): the file move can run on a worker
pub struct RenamePrep {
    idx: usize,
    from: PathBuf,
    to: PathBuf,
    name: String,
    /// its cloud rel before (the cloud copy follows)
    follow: Option<String>,
}

/// r2-threads (L5): one clip to delete (`LibModel::delete_plan`), plain data for the worker
pub struct DelJob {
    idx: usize,
    path: PathBuf,
    name: String,
    /// in the cloud / shared: its rel goes on cloud.json's keep_remote list
    cloud: bool,
    rel: String,
}

/// r2-threads (L5): Echo's delete_clip for one clip, on a worker: Recycle Bin / Trash, an emptied folder goes, its
/// thumbnails go
fn delete_file(j: &DelJob, root: &Path, thumbs: &Path) -> Result<(), String> {
    recycle(&j.path)?;
    if let Some(par) = j.path.parent().filter(|p| *p != root) {
        let _ = std::fs::remove_dir(par);
    }
    for x in ["jpg", "dur", "thumbt", "scrub.jpg", "strip.jpg", "strip2.jpg", "strip3.jpg", "a0.m4a", "a1.m4a", "a2.m4a", "a3.m4a", "a4.m4a", "a5.m4a"] {
        let _ = std::fs::remove_file(thumbs.join(format!("{}.{x}", j.name)));
    }
    Ok(())
}

/// Cloud clips deleted from this PC: on cloud.json's keep_remote list (like Echo's "Only from this PC") so they
/// aren't downloaded again
fn keep_remote(thumbs: &Path, keep: Vec<String>) {
    if keep.is_empty() {
        return;
    }
    let p = thumbs.join("cloud.json");
    if let Some(mut v) = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) {
        if let Some(o) = v.as_object_mut() {
            let list = o.entry("keep_remote").or_insert_with(|| serde_json::json!([]));
            if let Some(a) = list.as_array_mut() {
                for k in keep {
                    if !a.iter().any(|x| x.as_str() == Some(k.as_str())) {
                        a.push(k.into());
                    }
                }
            }
            let _ = write_atomic(&p, &serde_json::to_string(&v).unwrap_or_default());
        }
    }
}

/// Write keys into Echo's settings.json: re-read, replace only these keys, temp file + rename (like the Settings page)
fn settings_write(keys: &[(&str, serde_json::Value)]) -> Result<(), String> {
    let path = crate::settings::path();
    let mut disk = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    for (k, v) in keys {
        disk.insert(k.to_string(), v.clone());
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    write_atomic(&path, &serde_json::to_string_pretty(&serde_json::Value::Object(disk)).unwrap_or_default())
}

pub const WHERE: [(&str, &str); 4] = [("pc", "On this PC"), ("cloud", "In the cloud"), ("only", "Cloud only"), ("link", "Has a share link")];

pub const TYPES: [(&str, &str); 5] = [("clip", "Clips"), ("rec", "Recordings"), ("edit", "Edited"), ("sound", "Sounds"), ("fav", "Favourites")];

#[derive(Default, Clone)]
pub struct Meta {
    pub tags: Vec<String>,
    pub note: String,
    /// gaps-library row 44: what was said (auto captions: <thumbs>/<name>.words.json, Echo's clip_meta) - search only
    pub said: String,
}

pub struct LibModel {
    /// every clip (index = stable id; new clips are appended)
    all: RefCell<Vec<Rc<Clip>>>,
    /// r2-threads (L37): the first read of the clips folder is still running on a worker
    loading: Cell<bool>,
    pub thumbs_dir: PathBuf,
    view: RefCell<Vec<usize>>,
    cache: RefCell<HashMap<usize, (Image, u64)>>,
    pending: RefCell<HashSet<usize>>,
    tick: Cell<u64>,
    notify: ModelNotify,
    tx: Sender<(usize, PathBuf, Option<PathBuf>)>,
    // state
    pub favs: RefCell<HashSet<String>>,
    pub meta: RefCell<HashMap<String, Meta>>,
    pub picked: RefCell<HashSet<usize>>,
    pub nav: RefCell<String>,
    pub search: RefCell<String>,
    pub sort: RefCell<String>,
    /// filter panel picks: group -> keys ("game" / "type" / "tag")
    pub fsel: RefCell<BTreeMap<String, Vec<String>>>,
    pub cols: Cell<usize>,
    pub rows: Rc<VecModel<GridRow>>,
    /// r2-layout (L9): the grid card's width (Slint: Over.grid-card-w), for the tag lines
    pub card_w: Cell<f32>,
    /// window minimized for a while: no thumbnails held or decoded
    suspended: Cell<bool>,
    /// Home: the newest clips (Card.idx = HOME_BASE + position, so Lib's callbacks work for them too)
    home: RefCell<Vec<usize>>,
    home_notify: ModelNotify,
    /// sub-folder order per parent folder (settings.json "folder_order": {"FiveM": ["Heists", "Evidence"]}),
    /// set by dragging sub-folders in the sidebar; folders not listed follow A-Z
    pub folder_order: RefCell<BTreeMap<String, Vec<String>>>,
    /// game icons Echo saved (<Echo's data folder>/icons/<game>.png), decoded once per file version
    icon_cache: RefCell<HashMap<PathBuf, (SystemTime, Option<Image>)>>,
    /// Chat D badges: clips in Echo's cloud.json / links.json (lower-case rel paths); r2-threads (L36): re-read when
    /// one of the files changed (size / time per file), checked at most every 300 ms
    #[allow(clippy::type_complexity)]
    cloud_cache: RefCell<Option<(std::time::Instant, [Option<(Option<SystemTime>, u64)>; 2], HashSet<String>, HashSet<String>)>>,
    /// r2-threads (L29): the folders on disk (sidebar), from the disk sync; None = walk once more (Echo changed folders)
    disk_dirs: RefCell<Option<BTreeSet<String>>>,
    /// Echo's savingPh: the "Saving clip…" card at the top while a clip is saved (until it is in the list)
    pub saving: RefCell<Option<SavingPh>>,
    /// gaps-library row 44: lengths ffprobe found for clips without a .dur file (sort "Longest", lib_prefs::probe_long)
    pub probed: RefCell<HashMap<usize, f64>>,
    /// gaps-library row 21: the filter panel's "Search games and tags…" text
    pub fq: RefCell<String>,
}

pub struct SavingPh {
    pub secs: u32,
    pub at: std::time::Instant,
    /// where the clip goes (recorder save_target: its folder, else its game name, else "")
    pub folder: String,
    /// set once it is saved ("Clip saved · Opens in a moment")
    pub path: Option<PathBuf>,
}

/// Card.idx of Home's cards starts here (library view rows are below it)
pub const HOME_BASE: usize = 1_000_000;

impl LibModel {
    pub fn new(all: Vec<Clip>, thumbs_dir: PathBuf, tx: Sender<(usize, PathBuf, Option<PathBuf>)>) -> Self {
        let view = (0..all.len()).collect();
        let all = RefCell::new(all.into_iter().map(Rc::new).collect());
        let m = LibModel {
            all,
            thumbs_dir,
            view: RefCell::new(view),
            cache: Default::default(),
            pending: Default::default(),
            tick: Cell::new(0),
            notify: Default::default(),
            tx,
            favs: Default::default(),
            meta: Default::default(),
            picked: Default::default(),
            nav: RefCell::new("all".into()),
            search: Default::default(),
            sort: RefCell::new("new".into()),
            fsel: Default::default(),
            cols: Cell::new(4),
            rows: Rc::new(VecModel::default()),
            card_w: Cell::new(330.0),
            suspended: Cell::new(false),
            home: Default::default(),
            home_notify: Default::default(),
            icon_cache: Default::default(),
            cloud_cache: Default::default(),
            disk_dirs: Default::default(),
            loading: Cell::new(false),
            saving: Default::default(),
            probed: Default::default(),
            fq: Default::default(),
            folder_order: RefCell::new(
                echo_settings().and_then(|v| v.get("folder_order").cloned()).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default(),
            ),
        };
        m.load_favs_meta();
        m
    }

    pub fn clip(&self, idx: usize) -> Rc<Clip> {
        self.all.borrow()[idx].clone()
    }
    pub fn len(&self) -> usize {
        self.all.borrow().len()
    }
    /// every clip still on disk (for counts / lists - NOT index-aligned)
    pub fn all(&self) -> Vec<Rc<Clip>> {
        self.all.borrow().iter().filter(|c| !c.gone.get()).cloned().collect()
    }
    /// every clip by id (index-aligned, incl. ones deleted outside Echo)
    fn by_id(&self) -> Vec<Rc<Clip>> {
        self.all.borrow().clone()
    }
    /// a clip's id by its file (only clips still on disk)
    pub fn index_of_path(&self, p: &Path) -> Option<usize> {
        self.all.borrow().iter().position(|c| c.path == p && !c.gone.get())
    }
    /// files deleted / added outside Echo (file manager, another app): the library follows. True = something changed.
    pub fn sync_disk(&self, root: &Path) -> bool {
        let scan = disk_scan(root, self.known_paths());
        self.sync_apply(root, scan)
    }
    /// r2-threads (L3): every clip file Echo knows (incl. ones deleted outside Echo), for `disk_scan` on a worker
    pub fn known_paths(&self) -> HashSet<PathBuf> {
        self.all.borrow().iter().map(|c| c.path.clone()).collect()
    }
    /// r2-threads (L37): the first read of the clips folder hasn't come back yet (the list is still empty)
    pub fn loading(&self) -> bool {
        self.loading.get()
    }
    pub fn set_loading(&self, on: bool) {
        self.loading.set(on);
    }
    /// r2-threads (L37): the clips the first read found (on a worker); ones the library got meanwhile stay
    pub fn load_scanned(&self, clips: Vec<Clip>) {
        self.loading.set(false);
        let known = self.known_paths();
        let mut a = self.all.borrow_mut();
        a.extend(clips.into_iter().filter(|c| !known.contains(&c.path)).map(Rc::new));
    }
    /// r2-threads (L3): the library follows a `disk_scan` (made on a worker). The scan is a moment old, so a clip
    /// file only flips (gone <-> here) when the disk still agrees now - what Echo did itself meanwhile wins.
    pub fn sync_apply(&self, root: &Path, scan: DiskScan) -> bool {
        let DiskScan { root_ok, dirs, on_disk, conly, new_clips } = scan;
        if !root_ok {
            return false; // folder unplugged / renamed: don't empty the library
        }
        let mut changed = false;
        // L29: the sidebar's folders on disk; a folder that differs from what the sidebar knew is checked once more
        // (Echo may have made / removed it since the scan) - a folder made in the file manager shows up now too
        let old = self.disk_dirs.borrow().clone();
        let dirs = match old {
            Some(old) => {
                let mut d: BTreeSet<String> = old.intersection(&dirs).cloned().collect();
                d.extend(old.symmetric_difference(&dirs).filter(|f| root.join(f).is_dir()).cloned());
                changed |= d != old;
                d
            }
            None => dirs,
        };
        *self.disk_dirs.borrow_mut() = Some(dirs);
        let mut known: HashSet<PathBuf> = HashSet::new();
        // Chat B: cloud-only clips (rel lower-case -> (rel, size, modified))
        let conly: HashMap<String, (String, u64, u64)> = conly.into_iter().map(|(r, s, m)| (r.to_lowercase(), (r, s, m))).collect();
        let mut promote: Vec<usize> = vec![];
        for (i, c) in self.all.borrow().iter().enumerate() {
            let on = on_disk.contains(&c.path);
            let mut here = if let Some(rel) = &c.cloud {
                if on {
                    promote.push(i); // downloaded: becomes a normal clip below
                    true
                } else {
                    conly.contains_key(&rel.to_lowercase())
                }
            } else {
                on
            };
            if c.gone.get() == here && (c.cloud.is_none() || on) {
                here = c.path.exists(); // only for a flip: one stat
            }
            if c.gone.get() == here {
                c.gone.set(!here);
                changed = true;
            }
            known.insert(c.path.clone());
        }
        for i in promote {
            let p = self.clip(i).path.clone();
            if let Some(c) = clip_from(&p, root) {
                self.all.borrow_mut()[i] = Rc::new(c);
                changed = true;
            }
        }
        // path (lower-case) -> index, to turn a clip that just left this PC into its cloud-only card in place
        let known_l: HashMap<String, usize> = self.all.borrow().iter().enumerate().map(|(i, c)| (c.path.to_string_lossy().replace('\\', "/").to_lowercase(), i)).collect();
        for (rel, size, m) in conly.values() {
            let p = root.join(rel);
            if let Some(&i) = known_l.get(&p.to_string_lossy().replace('\\', "/").to_lowercase()) {
                let c = self.clip(i);
                if c.cloud.is_none() && c.gone.get() {
                    if let Some(nc) = cloud_clip(root, rel, *size, *m) {
                        self.all.borrow_mut()[i] = Rc::new(nc);
                        changed = true;
                    }
                }
                continue;
            }
            if let Some(c) = cloud_clip(root, rel, *size, *m) {
                known.insert(c.path.clone());
                self.add(c);
                changed = true;
            }
        }
        // new files (read on the worker); one Echo added itself meanwhile is known by now
        for c in new_clips {
            if known.contains(&c.path) || !c.path.exists() {
                continue;
            }
            known.insert(c.path.clone());
            self.add(c);
            changed = true;
        }
        if changed {
            let gone: Vec<usize> = self.all.borrow().iter().enumerate().filter(|(_, c)| c.gone.get()).map(|(i, _)| i).collect();
            self.picked.borrow_mut().retain(|i| !gone.contains(i));
        }
        changed
    }
    /// a new clip file (e.g. a trimmed copy): added to the library
    pub fn add(&self, c: Clip) -> usize {
        let mut a = self.all.borrow_mut();
        a.push(Rc::new(c));
        a.len() - 1
    }
    /// gaps-player: clip `idx` was rewritten in place (player Replace): read its size / length / date again
    pub fn reload(&self, idx: usize, root: &Path) {
        let old = self.clip(idx);
        if let Some(mut c) = clip_from(&old.path, root) {
            c.shown = old.shown.clone();
            self.all.borrow_mut()[idx] = Rc::new(c);
        }
    }

    // ---------- Echo's files ----------
    fn favs_path(&self) -> PathBuf {
        self.thumbs_dir.join("favorites.json")
    }
    fn meta_path(&self) -> PathBuf {
        self.thumbs_dir.join("meta.json")
    }
    pub fn load_favs_meta(&self) {
        let f: Vec<String> = std::fs::read_to_string(self.favs_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        *self.favs.borrow_mut() = f.into_iter().collect();
        let v: serde_json::Map<String, serde_json::Value> =
            std::fs::read_to_string(self.meta_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let mut m = HashMap::new();
        for (k, e) in v {
            let tags = e.get("tags").and_then(|t| t.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            let note = e.get("note").and_then(|n| n.as_str()).unwrap_or("").to_string();
            m.insert(k, Meta { tags, note, said: String::new() });
        }
        // Echo's clip_meta: the words auto captions heard (<name>.words.json = one JSON string)
        if let Ok(rd) = std::fs::read_dir(&self.thumbs_dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if let Some(stem) = n.strip_suffix(".words.json") {
                    let said: String = std::fs::read_to_string(e.path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
                    m.entry(stem.to_string()).or_default().said = said;
                }
            }
        }
        *self.meta.borrow_mut() = m;
    }

    /// star / unstar clips (Echo's set_favorite: by name, list order = when starred)
    pub fn set_favs(&self, names: &[String], on: bool) -> Result<(), String> {
        // read the file again: Echo may have changed it since
        let mut f: Vec<String> = std::fs::read_to_string(self.favs_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        f.retain(|x| !names.contains(x));
        if on {
            f.extend(names.iter().cloned());
        }
        std::fs::create_dir_all(&self.thumbs_dir).map_err(|e| e.to_string())?;
        write_atomic(&self.favs_path(), &serde_json::to_string(&f).map_err(|e| e.to_string())?)?;
        *self.favs.borrow_mut() = f.into_iter().collect();
        // Echo's set_favorite command: a new favourite syncs soon (Upload automatically = Favourites uploads it now,
        // not at the next 5-minute sync)
        if on {
            crate::rec_core::sync_soon();
        }
        Ok(())
    }

    /// Echo's set_meta: tags trimmed + deduped, entry removed when both are empty; any other field Echo keeps there
    /// (e.g. future ones) stays as it is
    pub fn set_meta(&self, name: &str, tags: Vec<String>, note: String) -> Result<(), String> {
        let mut v: serde_json::Map<String, serde_json::Value> =
            std::fs::read_to_string(self.meta_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let mut clean: Vec<String> = vec![];
        for t in tags.into_iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
            if !clean.contains(&t) {
                clean.push(t);
            }
        }
        let note = note.trim().to_string();
        if clean.is_empty() && note.is_empty() {
            v.remove(name);
        } else {
            let e = v.entry(name.to_string()).or_insert_with(|| serde_json::json!({}));
            if !e.is_object() {
                *e = serde_json::json!({});
            }
            e["tags"] = serde_json::json!(clean);
            e["note"] = serde_json::json!(note);
        }
        std::fs::create_dir_all(&self.thumbs_dir).map_err(|e| e.to_string())?;
        write_atomic(&self.meta_path(), &serde_json::to_string(&v).map_err(|e| e.to_string())?)?;
        let mut m = self.meta.borrow_mut();
        let said = m.get(name).map(|x| x.said.clone()).unwrap_or_default();
        if clean.is_empty() && note.is_empty() && said.is_empty() {
            m.remove(name);
        } else {
            m.insert(name.to_string(), Meta { tags: clean, note, said });
        }
        Ok(())
    }

    pub fn meta_of(&self, name: &str) -> Meta {
        self.meta.borrow().get(name).cloned().unwrap_or_default()
    }

    // ---------- filtering ----------
    fn in_folder(c: &Clip, g: &str) -> bool {
        c.game == g || c.folder.starts_with(&format!("{g}/"))
    }

    fn type_ok(&self, c: &Clip, k: &str) -> bool {
        match k {
            "clip" => !c.edited && !c.rec && !c.sound,
            "rec" => c.rec,
            "edit" => c.edited,
            "sound" => c.sound,
            "fav" => self.favs.borrow().contains(&c.name),
            _ => true,
        }
    }

    /// Chat B: Echo's "Where" filter
    fn where_ok(&self, c: &Clip, k: &str) -> bool {
        let (inc, shared) = self.cloud_flags(c);
        match k {
            "pc" => c.cloud.is_none(),
            "cloud" => c.cloud.is_some() || inc,
            "only" => c.cloud.is_some(),
            "link" => shared,
            _ => true,
        }
    }

    fn nav_ok(&self, c: &Clip, nav: &str, d0: i64) -> bool {
        match nav {
            "today" => c.secs >= d0,
            "week" => c.secs >= d0 - 6 * 86400,
            "fav" => self.favs.borrow().contains(&c.name),
            "edited" => c.edited,
            "cloud" => c.cloud.is_some() || self.cloud_flags(c).0,
            n if n.starts_with("game:") => Self::in_folder(c, &n[5..]),
            n if n.starts_with("tag:") => self.meta.borrow().get(&c.name).map(|m| m.tags.iter().any(|t| t == &n[4..])).unwrap_or(false),
            _ => true,
        }
    }

    fn fpass(&self, c: &Clip) -> bool {
        for (g, keys) in self.fsel.borrow().iter() {
            if keys.is_empty() {
                continue;
            }
            let ok = keys.iter().any(|k| match g.as_str() {
                "game" => {
                    if k.is_empty() {
                        c.game.is_empty()
                    } else {
                        Self::in_folder(c, k)
                    }
                }
                "type" => self.type_ok(c, k),
                "where" => self.where_ok(c, k),
                "tag" => self.meta.borrow().get(&c.name).map(|m| m.tags.contains(k)).unwrap_or(false),
                _ => true,
            });
            if !ok {
                return false;
            }
        }
        true
    }

    fn search_text(&self, c: &Clip) -> String {
        let m = self.meta.borrow();
        let mut s = format!("{} {} {}", c.name, c.folder, c.game);
        if let Some(m) = m.get(&c.name) {
            for t in &m.tags {
                s.push(' ');
                s.push_str(t);
            }
            s.push(' ');
            s.push_str(&m.note);
            s.push(' ');
            s.push_str(&m.said);
        }
        s.to_lowercase()
    }

    /// recompute the view (nav + filter panel + search + sort) and the rows
    pub fn refresh(&self) {
        let d0 = day_start();
        let nav = self.nav.borrow().clone();
        let q = self.search.borrow().to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        let all = self.by_id();
        let mut v: Vec<usize> = (0..all.len())
            .filter(|&i| {
                let c = &all[i];
                !c.gone.get() && self.nav_ok(c, &nav, d0) && self.fpass(c) && (words.is_empty() || {
                    let st = self.search_text(c);
                    words.iter().all(|w| st.contains(w))
                })
            })
            .collect();
        match self.sort.borrow().as_str() {
            "old" => v.sort_by(|&a, &b| all[a].mtime.cmp(&all[b].mtime)),
            "big" => v.sort_by(|&a, &b| all[b].size.cmp(&all[a].size)),
            "long" => {
                // gaps-library row 44: clips without a .dur use the length ffprobe found (lib_prefs::probe_long)
                let pr = self.probed.borrow();
                let d = |i: usize| all[i].dur.or_else(|| pr.get(&i).copied()).unwrap_or(0.0);
                v.sort_by(|&a, &b| d(b).total_cmp(&d(a)))
            }
            "name" => v.sort_by(|&a, &b| natural_cmp(&all[a].name, &all[b].name)),
            _ => v.sort_by(|&a, &b| all[b].mtime.cmp(&all[a].mtime)),
        }
        *self.view.borrow_mut() = v;
        self.notify.reset();
        self.saving_expire();
        self.rebuild_rows();
    }

    /// Echo's savingHere(): does the clip being saved belong in what the library shows now? Old Echo showed the
    /// card in ANY folder for a moment (William: in a folder unrelated to the game, until it fixed itself).
    fn saving_here(&self) -> bool {
        let s = self.saving.borrow();
        let Some(s) = s.as_ref() else { return false };
        let nav = self.nav.borrow();
        match nav.as_str() {
            "all" | "today" | "week" => true,
            n if n.starts_with("game:") => {
                let g = &n[5..];
                s.folder == g || s.folder.starts_with(&format!("{g}/"))
            }
            _ => false, // favourites, edited, cloud, tags: a new clip isn't in them
        }
    }

    /// drops the card once the saved clip is in the list (disk sync, every 3 s) or after 90 s; true = dropped
    pub fn saving_expire(&self) -> bool {
        let gone = match self.saving.borrow().as_ref() {
            None => return false,
            Some(s) => {
                s.at.elapsed().as_secs() > 90
                    || s.path.as_ref().is_some_and(|p| {
                        let p = p.to_string_lossy().replace('\\', "/").to_lowercase();
                        self.all.borrow().iter().any(|c| !c.gone.get() && c.path.to_string_lossy().replace('\\', "/").to_lowercase() == p)
                    })
            }
        };
        if gone {
            *self.saving.borrow_mut() = None;
        }
        gone
    }

    /// the card's text (Echo: "Saving the last 3 min…" -> "Almost ready…")
    pub fn saving_text(&self) -> (String, bool) {
        match self.saving.borrow().as_ref() {
            Some(s) if s.path.is_some() => ("Almost ready…".into(), true),
            Some(s) => (format!("Saving the last {}…", fmt_len(s.secs as u64)), false),
            None => (String::new(), false),
        }
    }

    /// header rows + rows of `cols` cards (Slint calls set_cols when the window width changes the column count)
    pub fn rebuild_rows(&self) {
        let cols = self.cols.get().max(1);
        let d0 = day_start();
        let sort = self.sort.borrow().clone();
        let view = self.view.borrow();
        let mut groups: Vec<(String, usize, usize)> = vec![]; // name, first, n
        for (i, &ci) in view.iter().enumerate() {
            let name = match sort.as_str() {
                "big" => "Largest first".to_string(),
                "long" => "Longest first".to_string(),
                "name" => "By name".to_string(),
                _ => group_name(self.all.borrow()[ci].secs, d0),
            };
            match groups.last_mut() {
                Some(g) if g.0 == name => g.2 += 1,
                _ => groups.push((name, i, 1)),
            }
        }
        let mut rows = vec![];
        // Echo prepends the "Saving clip…" card to the top group (its own "Today" group when nothing is shown yet);
        // in a 1-column view (list) it gets a row of its own
        let mut ph = self.saving_here();
        if ph && groups.is_empty() {
            rows.push(GridRow { header: "Today".into(), count: "".into(), first: 0, n: 0, extra: 0, last: false, saving: false });
            rows.push(GridRow { header: "".into(), count: "".into(), first: 0, n: 0, extra: 0, last: true, saving: true });
        }
        for (name, first, n) in groups {
            rows.push(GridRow { header: name.into(), count: n.to_string().into(), first: first as i32, n: 0, extra: 0, last: false, saving: false });
            let mut k = 0;
            while k < n {
                let slot = std::mem::take(&mut ph);
                let m = (cols - slot as usize).min(n - k);
                // cards in a row line up: room for the tag lines / note line of the card in the row that needs most
                // (r2-layout L9: tags wrap - over::tag_lines)
                let (mut tags, mut note) = (0, false);
                {
                    let meta = self.meta.borrow();
                    let all = self.all.borrow();
                    for &ci in &view[first + k..first + k + m] {
                        if let Some(e) = meta.get(&all[ci].name) {
                            tags = tags.max(crate::over::tag_lines(&e.tags, self.card_w.get()));
                            note |= !e.note.trim().is_empty();
                        }
                    }
                }
                let extra = crate::over::tag_extra(tags) + if note { 18 } else { 0 };
                rows.push(GridRow { header: "".into(), count: "".into(), first: (first + k) as i32, n: m as i32, extra, last: k + m >= n, saving: slot });
                k += m;
            }
        }
        self.rows.set_vec(rows);
    }

    pub fn set_cols(&self, cols: usize) {
        if cols != self.cols.get() {
            self.cols.set(cols);
            self.rebuild_rows();
        }
    }

    /// r2-layout (L9): the grid card's width changed (window resize) - the tag lines may change. Rows are rebuilt
    /// only when a row's room really changes (not on every pixel of a resize).
    pub fn set_card_w(&self, w: f32) {
        if (w - self.card_w.get()).abs() < 0.5 {
            return;
        }
        let old = self.card_w.replace(w);
        let changed = {
            let meta = self.meta.borrow();
            meta.values().any(|e| !e.tags.is_empty() && crate::over::tag_lines(&e.tags, old) != crate::over::tag_lines(&e.tags, w))
        };
        if changed {
            self.rebuild_rows();
        }
    }

    pub fn clip_at(&self, row: usize) -> Option<Rc<Clip>> {
        self.index_at(row).map(|i| self.clip(i))
    }
    pub fn index_at(&self, row: usize) -> Option<usize> {
        if row >= HOME_BASE {
            return self.home.borrow().get(row - HOME_BASE).copied();
        }
        self.view.borrow().get(row).copied()
    }
    /// where a clip is in the library view (Home's cards open the player at that spot, so prev / next work)
    pub fn view_row_of(&self, idx: usize) -> Option<usize> {
        self.view.borrow().iter().position(|&i| i == idx)
    }

    // ---------- Home ----------
    /// Echo's homeList: every clip, newest first; Home shows the first `n` (1 big + one row of recent ones)
    pub fn refresh_home(&self, n: usize) {
        let all = self.by_id();
        let mut v: Vec<usize> = (0..all.len()).filter(|&i| !all[i].gone.get()).collect();
        v.sort_by(|&a, &b| all[b].mtime.cmp(&all[a].mtime));
        v.truncate(n);
        if *self.home.borrow() != v {
            *self.home.borrow_mut() = v;
            self.home_notify.reset();
        }
    }
    /// px for tag / note lines under the recent cards' names (all cards in the row line up, like the grid)
    pub fn home_extra(&self) -> i32 {
        let (mut tags, mut note) = (false, false);
        let meta = self.meta.borrow();
        let all = self.all.borrow();
        for &ci in self.home.borrow().iter().skip(1) {
            if let Some(e) = meta.get(&all[ci].name) {
                tags |= !e.tags.is_empty();
                note |= !e.note.trim().is_empty();
            }
        }
        (if tags { 24 } else { 0 }) + if note { 18 } else { 0 }
    }
    /// Echo's drawStrip: clips this week, total, bytes on this PC, games, clips per week for the last 6 weeks (oldest first)
    pub fn home_stats(&self) -> (usize, usize, u64, usize, [usize; 6]) {
        let now = SystemTime::now();
        let age = |c: &Clip| now.duration_since(c.mtime).map(|d| d.as_secs()).unwrap_or(0);
        let wk = 7 * 86400;
        let all = self.all();
        let week = all.iter().filter(|c| age(c) < wk).count();
        let bytes = all.iter().map(|c| c.size).sum();
        let games: HashSet<String> = all.iter().filter(|c| !c.game.is_empty()).map(|c| c.game.split('/').next().unwrap_or("").to_string()).collect();
        let mut bars = [0usize; 6];
        for c in all.iter() {
            let w = (age(c) / wk) as usize;
            if w < 6 {
                bars[5 - w] += 1;
            }
        }
        (week, all.len(), bytes, games.len(), bars)
    }
    pub fn view_len(&self) -> usize {
        self.view.borrow().len()
    }

    /// a card's data changed (favourite, selection, tags): repaint just that card
    pub fn changed(&self, idx: usize) {
        if let Some(row) = self.view.borrow().iter().position(|&i| i == idx) {
            self.notify.row_changed(row);
        }
        if let Some(k) = self.home.borrow().iter().position(|&i| i == idx) {
            self.home_notify.row_changed(k);
        }
    }
    pub fn changed_all(&self) {
        for r in 0..self.view.borrow().len() {
            self.notify.row_changed(r);
        }
        for k in 0..self.home.borrow().len() {
            self.home_notify.row_changed(k);
        }
    }

    // ---------- selection ----------
    pub fn toggle_pick(&self, row: usize) {
        let Some(i) = self.index_at(row) else { return };
        {
            let mut p = self.picked.borrow_mut();
            if !p.remove(&i) {
                p.insert(i);
            }
        }
        let _ = row;
        self.changed(i);
    }
    pub fn pick_all(&self) {
        let v = self.view.borrow().clone();
        self.picked.borrow_mut().extend(v);
        self.changed_all();
    }
    pub fn clear_picks(&self) {
        self.picked.borrow_mut().clear();
        self.changed_all();
    }

    // ---------- sidebar + filter panel ----------
    /// top-level games (first part of the folder / name), biggest first
    fn games(&self) -> Vec<(String, usize)> {
        let mut g: Vec<(String, usize)> = vec![];
        for c in self.all().iter() {
            if c.game.is_empty() {
                continue;
            }
            let top = c.game.split('/').next().unwrap_or("").to_string();
            match g.iter_mut().find(|x| x.0 == top) {
                Some(x) => x.1 += 1,
                None => g.push((top, 1)),
            }
        }
        g
    }
    fn tag_counts(&self) -> Vec<(String, usize)> {
        let mut n: HashMap<String, usize> = HashMap::new();
        let m = self.meta.borrow();
        for c in self.all().iter() {
            if let Some(e) = m.get(&c.name) {
                for t in &e.tags {
                    *n.entry(t.clone()).or_default() += 1;
                }
            }
        }
        let mut v: Vec<(String, usize)> = n.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(natural_cmp(&a.0, &b.0)));
        v
    }

    // ---------- folders ----------
    /// the clips folder
    pub fn root(&self) -> PathBuf {
        self.thumbs_dir.parent().map(|p| p.to_path_buf()).unwrap_or_default()
    }
    /// every folder the sidebar lists: real folders on disk (also empty ones, like Echo), folders of clips,
    /// and games that only exist as a name prefix ("FiveM 2026-..." in the main folder)
    fn folder_set(&self) -> BTreeSet<String> {
        // r2-threads (L29): the folders on disk come from the disk sync's worker (every 3 s); walked here only
        // the first time and after Echo changed folders itself
        let cached = self.disk_dirs.borrow().clone();
        let mut set: BTreeSet<String> = match cached {
            Some(s) => s,
            None => {
                let s = walk_dirs(&self.root());
                *self.disk_dirs.borrow_mut() = Some(s.clone());
                s
            }
        };
        for c in self.all().iter() {
            if !c.folder.is_empty() {
                let parts: Vec<&str> = c.folder.split('/').collect();
                for k in 1..=parts.len() {
                    set.insert(parts[..k].join("/"));
                }
            } else if !c.game.is_empty() {
                set.insert(c.game.clone());
            }
        }
        set
    }
    /// the sub-folders of `parent`, in the remembered order (the rest A-Z after them)
    fn kids_sorted(&self, parent: &str, set: &BTreeSet<String>) -> Vec<String> {
        let mut kids: Vec<String> = set.iter().filter(|f| parent_of(f) == parent && f.len() > parent.len() && !parent.is_empty()).cloned().collect();
        kids.sort_by(|a, b| natural_cmp(a, b));
        if let Some(order) = self.folder_order.borrow().get(parent) {
            let pos = |f: &String| {
                let leaf = f.rsplit('/').next().unwrap_or(f);
                order.iter().position(|o| o.eq_ignore_ascii_case(leaf)).unwrap_or(usize::MAX)
            };
            kids.sort_by_key(|f| pos(f)); // stable: unlisted ones keep A-Z at the end
        }
        kids
    }
    /// while a sidebar folder is dragged: what a drop on folder `f` would do
    /// ("into" = becomes its sub-folder, "sort" = reorder among siblings, "" = nothing)
    pub fn drop_mode(&self, src: &str, f: &str) -> &'static str {
        if src.is_empty() || f.is_empty() || f == src || f.starts_with(&format!("{src}/")) || folder_reserved(f) {
            return "";
        }
        let sp = parent_of(src);
        if !sp.is_empty() && parent_of(f) == sp {
            return "sort";
        }
        if f == sp {
            return ""; // already in there
        }
        "into"
    }
    /// Drag a sub-folder above / below a sibling: the new order is remembered (settings.json "folder_order")
    pub fn reorder_folder(&self, src: &str, target: &str, after: bool) -> Result<(), String> {
        let parent = parent_of(src).to_string();
        if parent.is_empty() || parent_of(target) != parent || src == target {
            return Ok(());
        }
        let leaf = |f: &str| f.rsplit('/').next().unwrap_or(f).to_string();
        let mut kids: Vec<String> = self.kids_sorted(&parent, &self.folder_set()).iter().map(|f| leaf(f)).collect();
        let s = leaf(src);
        kids.retain(|k| k != &s);
        let t = kids.iter().position(|k| *k == leaf(target)).unwrap_or(kids.len());
        kids.insert(if after { (t + 1).min(kids.len()) } else { t }, s);
        self.folder_order.borrow_mut().insert(parent, kids);
        self.save_folder_order(None)
    }
    fn save_folder_order(&self, alias: Option<serde_json::Value>) -> Result<(), String> {
        let order = serde_json::to_value(&*self.folder_order.borrow()).unwrap_or_default();
        let mut keys = vec![("folder_order", order)];
        if let Some(a) = alias {
            keys.push(("folder_alias", a));
        }
        settings_write(&keys)
    }
    /// clips of this folder that are in the cloud or have a share link (Echo's cloud.json / links.json):
    /// moving those needs the cloud copies moved too - that comes with the real port
    fn folder_in_cloud(&self, f: &str) -> bool {
        let pre = format!("{}/", f.replace('\\', "/").to_lowercase());
        // r2-threads (L36): the cached sets (already lower-case with '/')
        self.cloud_sets(|cl, li| cl.iter().chain(li.iter()).any(|r| r.starts_with(&pre)))
    }
    /// Is there already a folder with this name (any upper / lower case)? Returns its real name.
    pub fn folder_exists(&self, f: &str) -> Option<String> {
        let mut dir = self.root();
        let mut real: Vec<String> = vec![];
        for part in f.split('/') {
            let hit = std::fs::read_dir(&dir).ok()?.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.file_name().to_string_lossy().to_string()).find(|n| n.eq_ignore_ascii_case(part))?;
            dir = dir.join(&hit);
            real.push(hit);
        }
        Some(real.join("/"))
    }
    /// Echo's create_folder: an empty folder ("Parent/Child" too); a hidden .echo-folder marker keeps it when its
    /// last clip moves out. Returns the folder.
    pub fn create_folder(&self, name: &str) -> Result<String, String> {
        let rel = clean_folder(name);
        if rel.is_empty() {
            return Err("Type a folder name".into());
        }
        if folder_reserved(&rel) {
            return Err("That folder name is reserved".into());
        }
        if self.folder_exists(&rel).is_some() {
            return Err("That folder already exists".into());
        }
        let dir = self.root().join(&rel);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let _ = std::fs::write(dir.join(".echo-folder"), "");
        *self.disk_dirs.borrow_mut() = None; // r2-threads (L29)
        Ok(rel)
    }
    /// Echo's delete_folder: only a folder without clips (its empty sub-folders go too)
    pub fn delete_folder(&self, name: &str) -> Result<(), String> {
        let rel = clean_folder(name);
        if rel.is_empty() || folder_reserved(&rel) {
            return Err("pick a folder".into());
        }
        let dir = self.root().join(&rel);
        fn has_files(d: &Path) -> bool {
            std::fs::read_dir(d).map(|rd| rd.flatten().any(|e| {
                let p = e.path();
                if p.is_dir() { has_files(&p) } else { e.file_name() != ".echo-folder" }
            })).unwrap_or(false)
        }
        if has_files(&dir) {
            return Err("Move or delete the clips in it first".into());
        }
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
        *self.disk_dirs.borrow_mut() = None; // r2-threads (L29)
        let par = parent_of(&rel).to_string();
        let leaf = rel.rsplit('/').next().unwrap_or(&rel).to_string();
        let mut ord = self.folder_order.borrow_mut();
        if let Some(v) = ord.get_mut(&par) {
            v.retain(|x| !x.eq_ignore_ascii_case(&leaf));
        }
        ord.remove(&rel);
        drop(ord);
        let _ = self.save_folder_order(None);
        Ok(())
    }

    /// Echo's rename_folder: move / rename folder `old` to `new` (sub-folders come along); if `new` already exists
    /// the two are merged (clips renamed "name (2)" on clashes, their thumbnails / tags / favourite follow).
    /// New clips of that game follow too (settings.json folder_alias). Clips keep their ids (open player,
    /// selection). Returns the new folder.
    /// r2-threads (L13): the files move on a worker (a locked clip waits up to ~4 s there); `done` on the UI thread
    pub fn move_folder_bg(self: &Rc<Self>, old: &str, new: &str, done: impl FnOnce(Result<String, String>) + 'static) {
        let (old, new) = (clean_folder(old), clean_folder(new));
        let follow = match self.move_folder_check(&old, &new) {
            Ok(f) => f,
            Err(e) => return done(Err(e)),
        };
        let (root, thumbs, o2, n2) = (self.root(), self.thumbs_dir.clone(), old.clone(), new.clone());
        let lib = self.clone();
        crate::lib_net::bg(
            move || {
                let (src, dst) = (root.join(&o2), root.join(&n2));
                if n2.eq_ignore_ascii_case(&o2) || !dst.exists() {
                    if let Some(par) = dst.parent() {
                        let _ = std::fs::create_dir_all(par);
                    }
                    FolderFiles { merged: None, renamed: vec![], err: rename_patient(&src, &dst).err() }
                } else {
                    // merge: move every clip over, renaming on name clashes
                    merge_files(&src, &dst, &thumbs)
                }
            },
            move |ff| {
                let r = lib.move_folder_after(&old, &new, ff);
                if r.is_ok() {
                    crate::cloud::follow_move(follow.into_iter().map(|(i, o)| (o, crate::cloud::rel_of(&lib.clip(i)))).filter(|(o, n)| o != n).collect());
                }
                done(r);
            },
        );
    }
    /// move_folder's checks; Ok = the clips whose cloud copies follow (id, rel before)
    fn move_folder_check(&self, old: &str, new: &str) -> Result<Vec<(usize, String)>, String> {
        let (old, new) = (old.to_string(), new.to_string());
        if old.is_empty() || new.is_empty() {
            return Err("pick a folder".into());
        }
        if folder_reserved(&new) || folder_reserved(&old) {
            return Err("that folder name is reserved".into());
        }
        if new.to_lowercase().starts_with(&format!("{}/", old.to_lowercase())) {
            return Err("a folder can't go inside itself".into());
        }
        let root = self.root();
        let src = root.join(&old);
        if !src.is_dir() {
            return Err(format!("\"{old}\" isn't a folder yet - its clips are in the main folder"));
        }
        // Chat B: clips in the cloud follow (cloud_follow_move after the move)
        let follow: Vec<(usize, String)> = if self.folder_in_cloud(&old) {
            let pre = format!("{}/", old.to_lowercase());
            (0..self.len())
                .filter(|&i| {
                    let c = self.clip(i);
                    !c.gone.get() && c.cloud.is_none() && (c.folder.eq_ignore_ascii_case(&old) || c.folder.to_lowercase().starts_with(&pre)) && self.in_cloud(i)
                })
                .map(|i| (i, crate::cloud::rel_of(&self.clip(i))))
                .collect()
        } else {
            vec![]
        };
        Ok(follow)
    }
    /// after a merge on the worker: renamed clips' tags / note / favourite follow (their .thumbs files moved there)
    fn apply_renamed(&self, renamed: &[(String, String)]) {
        for (stem, new_stem) in renamed {
            let m = self.meta.borrow().get(stem).cloned();
            if let Some(m) = m {
                let _ = self.set_meta(new_stem, m.tags, m.note);
                let _ = self.set_meta(stem, vec![], String::new());
            }
            if self.favs.borrow().contains(stem) {
                let _ = self.set_favs(&[stem.clone()], false);
                let _ = self.set_favs(&[new_stem.clone()], true);
            }
        }
    }
    /// gaps-library row 29: Echo's "Merge into the main folder" (rename_folder with new = ""): the folder's clips go
    /// to the top of the clips folder (renamed on name clashes), its sub-folders become top folders, new clips of it
    /// go there too (folder_alias old -> ""), cloud copies follow. The moved clips come back through sync_disk.
    /// r2-threads (L13): the files move on a worker (then the clips folder is read there too); `done` on the UI thread
    pub fn merge_into_main_bg(self: &Rc<Self>, old: &str, done: impl FnOnce(Result<(), String>) + 'static) {
        let old = clean_folder(old);
        if old.is_empty() {
            return done(Err("pick a folder".into()));
        }
        if folder_reserved(&old) {
            return done(Err("that folder name is reserved".into()));
        }
        let root = self.root();
        let src = root.join(&old);
        if !src.is_dir() {
            return done(Err(format!("\"{old}\" isn't a folder yet - its clips are in the main folder")));
        }
        let lo = old.to_lowercase();
        let pre = format!("{lo}/");
        let under = |f: &str| {
            let f = f.to_lowercase();
            f == lo || f.starts_with(&pre)
        };
        // clips in the cloud follow (their path before, their rel before)
        let follow: Vec<(PathBuf, String)> = (0..self.len())
            .filter(|&i| {
                let c = self.clip(i);
                !c.gone.get() && c.cloud.is_none() && under(&c.folder) && self.in_cloud(i)
            })
            .map(|i| (self.clip(i).path.clone(), crate::cloud::rel_of(&self.clip(i))))
            .collect();
        let (known, thumbs) = (self.known_paths(), self.thumbs_dir.clone());
        let lib = self.clone();
        crate::lib_net::bg(
            move || {
                // sub-folders come along (unless the main folder already has one with that name)
                let ff = merge_files(&src, &root, &thumbs);
                let scan = disk_scan(&root, known);
                (ff, scan)
            },
            move |(ff, scan)| done(lib.merge_after(old, follow, ff, scan)),
        );
    }
    fn merge_after(&self, old: String, follow: Vec<(PathBuf, String)>, ff: FolderFiles, scan: DiskScan) -> Result<(), String> {
        *self.disk_dirs.borrow_mut() = None; // r2-threads (L29): folders may have changed - the sidebar walks again
        self.apply_renamed(&ff.renamed);
        if let Some(e) = ff.err {
            return Err(e);
        }
        let moved = ff.merged.unwrap_or_default();
        let root = self.root();
        let src = root.join(&old);
        let lo = old.to_lowercase();
        let pre = format!("{lo}/");
        let under = |f: &str| {
            let f = f.to_lowercase();
            f == lo || f.starts_with(&pre)
        };
        let pairs: Vec<(String, String)> = follow
            .into_iter()
            .filter_map(|(p, orel)| {
                let np = moved.get(&p).cloned().or_else(|| p.strip_prefix(&src).ok().map(|r| root.join(r)).filter(|n| n.is_file()))?;
                let nrel = np.strip_prefix(&root).ok()?.to_string_lossy().replace('\\', "/");
                (nrel != orel).then_some((orel, nrel))
            })
            .collect();
        crate::cloud::follow_move(pairs);
        // the remembered sub-folder order: "old/x" keys move up one level, the old parent forgets it
        {
            let mut ord = self.folder_order.borrow_mut();
            let keys: Vec<String> = ord.keys().filter(|k| under(k)).cloned().collect();
            for k in keys {
                if let Some(v) = ord.remove(&k) {
                    let nk = k[old.len()..].trim_start_matches('/').to_string();
                    if !nk.is_empty() {
                        ord.insert(nk, v);
                    }
                }
            }
            let (op, ol) = (parent_of(&old).to_string(), old.rsplit('/').next().unwrap_or(&old).to_string());
            if let Some(v) = ord.get_mut(&op) {
                v.retain(|x| !x.eq_ignore_ascii_case(&ol));
            }
        }
        // new clips of this game go to the main folder too (Echo's folder_alias old -> "")
        let mut alias: BTreeMap<String, String> =
            echo_settings().and_then(|v| v.get("folder_alias").cloned()).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        for v in alias.values_mut() {
            if v.eq_ignore_ascii_case(&old) {
                v.clear();
            }
        }
        alias.insert(old.clone(), String::new());
        if let Err(e) = self.save_folder_order(Some(serde_json::to_value(&alias).unwrap_or_default())) {
            eprintln!("folders: settings.json not saved: {e}");
        }
        {
            let mut nav = self.nav.borrow_mut();
            if nav.strip_prefix("game:").is_some_and(|f| under(f)) {
                *nav = "all".into();
            }
        }
        self.sync_apply(&root, scan); // r2-threads (L13): read on the worker
        Ok(())
    }
    /// move_folder after the files moved on the worker: the clips follow in place, the folder order / alias / filter too
    fn move_folder_after(&self, old: &str, new: &str, ff: FolderFiles) -> Result<String, String> {
        *self.disk_dirs.borrow_mut() = None; // r2-threads (L29): folders may have changed - the sidebar walks again
        let (old, new) = (old.to_string(), new.to_string());
        let root = self.root();
        self.apply_renamed(&ff.renamed);
        if let Some(e) = ff.err {
            return Err(e);
        }
        // merge: which file went where (a clip with the same name may already be there)
        let merged = ff.merged;
        // the clips follow in place (same ids); merged clips that got a new name are picked up by sync_disk
        let lo = old.to_lowercase();
        {
            let mut all = self.all.borrow_mut();
            for slot in all.iter_mut() {
                let c = slot.clone();
                let fl = c.folder.to_lowercase();
                if c.gone.get() || !(fl == lo || fl.starts_with(&format!("{lo}/"))) {
                    continue;
                }
                let folder = format!("{new}{}", &c.folder[old.len()..]);
                let path = match &merged {
                    // merged: only clips that kept their name stay the same entry; renamed ones ("x (2)") are
                    // dropped here and come back through sync_disk under their new name
                    Some(map) if fl == lo => match map.get(&c.path) {
                        Some(to) if to.file_stem() == c.path.file_stem() => to.clone(),
                        _ => {
                            c.gone.set(true);
                            continue;
                        }
                    },
                    // sub-folders of a merged folder that already existed in the target stay where they were
                    Some(_) if !root.join(&folder).join(c.path.file_name().unwrap_or_default()).is_file() || c.path.is_file() => continue,
                    _ => root.join(&folder).join(c.path.file_name().unwrap_or_default()),
                };
                if !path.is_file() {
                    continue;
                }
                *slot = Rc::new(Clip {
                    path,
                    name: c.name.clone(),
                    game: folder.clone(),
                    folder,
                    shown: c.shown.clone(),
                    mtime: c.mtime,
                    secs: c.secs,
                    size: c.size,
                    dur: c.dur,
                    thumbs: c.thumbs.clone(),
                    own_thumb: c.own_thumb.clone(),
                    edited: c.edited,
                    rec: c.rec,
                    sound: c.sound,
                    gone: Default::default(),
                    cloud: None,
                });
            }
        }
        // the remembered sub-folder order follows: keys under the old folder move, the old parent forgets it
        {
            let mut ord = self.folder_order.borrow_mut();
            let moved: Vec<String> = ord.keys().filter(|k| k.to_lowercase() == lo || k.to_lowercase().starts_with(&format!("{lo}/"))).cloned().collect();
            for k in moved {
                if let Some(v) = ord.remove(&k) {
                    ord.insert(format!("{new}{}", &k[old.len()..]), v);
                }
            }
            let (op, ol) = (parent_of(&old).to_string(), old.rsplit('/').next().unwrap_or(&old).to_string());
            if parent_of(&new) != op {
                if let Some(v) = ord.get_mut(&op) {
                    v.retain(|x| !x.eq_ignore_ascii_case(&ol));
                }
            } else if let Some(v) = ord.get_mut(&op) {
                // plain rename inside the same parent: keep its place
                let nl = new.rsplit('/').next().unwrap_or(&new).to_string();
                for x in v.iter_mut() {
                    if x.eq_ignore_ascii_case(&ol) {
                        *x = nl.clone();
                    }
                }
            }
        }
        // new clips of this game go to the new folder too (Echo's folder_alias rules)
        let mut alias: BTreeMap<String, String> =
            echo_settings().and_then(|v| v.get("folder_alias").cloned()).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        for v in alias.values_mut() {
            if v.eq_ignore_ascii_case(&old) {
                *v = new.clone();
            }
        }
        alias.retain(|k, _| !k.eq_ignore_ascii_case(&new));
        alias.insert(old.clone(), new.clone());
        if let Err(e) = self.save_folder_order(Some(serde_json::to_value(&alias).unwrap_or_default())) {
            eprintln!("folders: settings.json not saved: {e}");
        }
        // the open filter follows the folder
        {
            let mut nav = self.nav.borrow_mut();
            if let Some(f) = nav.strip_prefix("game:").map(|f| f.to_string()) {
                if f.to_lowercase() == lo || f.to_lowercase().starts_with(&format!("{lo}/")) {
                    *nav = format!("game:{new}{}", &f[old.len()..]);
                }
            }
        }
        eprintln!("folders: '{old}' -> '{new}'");
        Ok(new)
    }

    /// Echo's move_clip for the picked clips: into `folder` ("" = the main folder). Clips keep their ids.
    /// r2-threads (L13): the files move on a worker (a locked clip waits up to ~4 s there); `done(moved, message
    /// for the clips that couldn't move)` on the UI thread.
    pub fn move_clips_bg(self: &Rc<Self>, ids: &[usize], folder: &str, done: impl FnOnce(usize, Option<String>) + 'static) {
        self.move_many_bg(vec![(ids.to_vec(), folder.to_string())], done);
    }
    /// several moves in one go (Sort into folders: one batch per game folder), one worker
    pub fn move_many_bg(self: &Rc<Self>, batches: Vec<(Vec<usize>, String)>, done: impl FnOnce(usize, Option<String>) + 'static) {
        let root = self.root();
        let mut err = None::<String>;
        let mut jobs: Vec<MoveJob> = vec![];
        // Chat B: the moved cloud clips' cloud copies follow (cloud_follow_move)
        let mut follow: Vec<(usize, String)> = vec![];
        for (ids, folder) in batches {
            let folder = clean_folder(&folder);
            if folder_reserved(&folder) {
                err = Some("that folder name is reserved".into());
                continue;
            }
            let folder = self.folder_exists(&folder).unwrap_or(folder);
            let dir = if folder.is_empty() { root.clone() } else { root.join(&folder) };
            for &i in &ids {
                let c = self.clip(i);
                if c.gone.get() || c.folder == folder {
                    continue;
                }
                // Chat B: clips in the cloud / with a share link: their cloud copy follows (cloud_follow_move)
                if c.cloud.is_some() {
                    err = Some(format!("\"{}\" is only in the cloud - download it first to move it", c.shown));
                    continue;
                }
                let file = c.path.file_name().unwrap_or_default().to_string_lossy().to_string();
                let rel = if c.folder.is_empty() { file } else { format!("{}/{file}", c.folder) };
                if self.rel_in_cloud(&rel) {
                    follow.push((i, rel));
                }
                jobs.push(MoveJob { idx: i, from: c.path.clone(), dir: dir.clone(), folder: folder.clone(), shown: c.shown.clone() });
            }
        }
        let lib = self.clone();
        let r0 = root.clone();
        let state = Rc::new(RefCell::new((0usize, err)));
        let (s1, s2) = (state.clone(), state);
        crate::bgwork::stream(
            move |tx| {
                let mut broken: Option<PathBuf> = None;
                for j in jobs {
                    if broken.as_ref() == Some(&j.dir) {
                        continue; // the folder couldn't be made (Echo: the rest of that move stops)
                    }
                    let r = move_file(&j, &r0);
                    if r.as_ref().is_err_and(|e| e.0) {
                        broken = Some(j.dir.clone());
                    }
                    let _ = tx.send((j, r.map_err(|e| e.1)));
                }
            },
            move |(j, r): (MoveJob, Result<PathBuf, String>)| {
                let mut s = s1.borrow_mut();
                match r {
                    Ok(new) => {
                        lib.moved(&j, new);
                        s.0 += 1;
                    }
                    Err(e) => s.1 = Some(e),
                }
            },
            move || {
                if let Some(l) = crate::LIB.with(|l| l.borrow().clone()) {
                    let pairs: Vec<(String, String)> = follow.into_iter().map(|(i, o)| (o, crate::cloud::rel_of(&l.clip(i)))).filter(|(o, n)| o != n).collect();
                    crate::cloud::follow_move(pairs);
                }
                let (n, e) = s2.borrow_mut().clone();
                done(n, e);
            },
        );
    }
    /// r2-threads (L13): clip `j.idx` is in its new folder now (`new` = its file): the library entry follows
    fn moved(&self, j: &MoveJob, new: PathBuf) {
        *self.disk_dirs.borrow_mut() = None; // r2-threads (L29): folders may have changed - the sidebar walks again
        let i = j.idx;
        let c = self.clip(i);
        if c.path != j.from {
            return; // changed meanwhile: the disk sync sorts it out
        }
        let folder = j.folder.clone();
        {
            let game = if folder.is_empty() {
                match date_at(&c.name) {
                    Some(k) if !matches!(&c.name[..k], "Clip" | "Montage" | "Recording") => c.name[..k].to_string(),
                    _ => String::new(),
                }
            } else {
                folder.clone()
            };
            self.all.borrow_mut()[i] = Rc::new(Clip {
                path: new,
                name: c.name.clone(),
                folder: folder.clone(),
                sound: game.eq_ignore_ascii_case("sounds"),
                game,
                shown: c.shown.clone(),
                mtime: c.mtime,
                secs: c.secs,
                size: c.size,
                dur: c.dur,
                thumbs: c.thumbs.clone(),
                own_thumb: c.own_thumb.clone(),
                edited: c.edited,
                rec: c.rec,
                gone: Default::default(),
                cloud: None,
            });
        }
    }
    /// Echo's rename_clip (Chat D): new file name in the same folder, " (Echo)" kept, favourite + tags/note + Echo's
    /// .thumbs files follow. The clip keeps its id. Cloud clips refuse (their cloud copy must follow - real port).
    /// r2-threads (L7): the file rename (it waits up to ~4 s on a locked file) on a worker;
    /// `done` on the UI thread with the shown name or the error
    pub fn rename_clip_bg(self: &Rc<Self>, idx: usize, name: &str, done: impl FnOnce(Result<String, String>) + 'static) {
        let prep = match self.rename_prep(idx, name) {
            Err(e) => return done(Err(e)),
            Ok(Err(shown)) => return done(Ok(shown)),
            Ok(Ok(p)) => p,
        };
        let (from, to) = (prep.from.clone(), prep.to.clone());
        let lib = self.clone();
        crate::lib_net::bg(
            move || rename_patient(&from, &to).map_err(|e| e.replace("A clip in this folder is", "The clip is")),
            move |r| {
                // the clip changed meanwhile (moved / deleted / renamed again): the file went, the library follows
                // on the next disk sync
                done(r.and_then(|_| lib.rename_finish(prep)));
            },
        );
    }
    /// the checks of rename_clip: Ok(Err(shown)) = nothing to do (same name)
    fn rename_prep(&self, idx: usize, name: &str) -> Result<Result<RenamePrep, String>, String> {
        let c = self.clip(idx);
        if c.gone.get() {
            return Err("that clip isn't there any more".into());
        }
        let mut name: String = name.trim().chars().filter(|ch| !r#"\/:*?"<>|"#.contains(*ch)).collect();
        name = name.trim().trim_end_matches('.').trim().to_string();
        if name.is_empty() {
            return Err("The name can't be empty".into());
        }
        if c.name.ends_with(" (Echo)") && !name.ends_with(" (Echo)") {
            name.push_str(" (Echo)");
        }
        if name == c.name {
            return Ok(Err(c.shown.clone()));
        }
        if c.cloud.is_some() {
            return Err(format!("\"{}\" is only in the cloud - download it first to rename it", c.shown));
        }
        let follow = self.in_cloud(idx).then(|| crate::cloud::rel_of(&c));
        let ext = c.path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_else(|| "mp4".into());
        let new = c.path.with_file_name(format!("{name}.{ext}"));
        // only a change of upper/lower case may hit the same file (Windows)
        if new.exists() && !name.eq_ignore_ascii_case(&c.name) {
            return Err("A clip with that name already exists".into());
        }
        Ok(Ok(RenamePrep { idx, from: c.path.clone(), to: new, name, follow }))
    }
    /// after the file was renamed: favourite, tags / note, Echo's .thumbs files and the cloud copy follow
    fn rename_finish(&self, prep: RenamePrep) -> Result<String, String> {
        let RenamePrep { idx, from, to: new, name, follow } = prep;
        let c = self.clip(idx);
        if c.path != from || c.gone.get() {
            return Err("That clip changed meanwhile - try again".into());
        }
        let r = self.rename_clip_inner(idx, name, new);
        if let (Ok(_), Some(o)) = (&r, follow) {
            let n = crate::cloud::rel_of(&self.clip(idx));
            if n != o {
                crate::cloud::follow_move(vec![(o, n)]);
            }
        }
        r
    }
    fn rename_clip_inner(&self, idx: usize, name: String, new: PathBuf) -> Result<String, String> {
        let c = self.clip(idx);
        let old = c.name.clone();
        if self.favs.borrow().contains(&old) {
            let _ = self.set_favs(&[old.clone()], false);
            let _ = self.set_favs(&[name.clone()], true);
        }
        let m = self.meta.borrow().get(&old).cloned();
        if let Some(m) = m {
            let _ = self.set_meta(&old, vec![], String::new());
            let _ = self.set_meta(&name, m.tags, m.note);
        }
        for e in ["jpg", "dur", "scrub.jpg", "thumbt", "strip.jpg", "strip2.jpg", "strip3.jpg", "a1.m4a", "a2.m4a", "a3.m4a", "a4.m4a", "a5.m4a", "marks.json", "levels.json", "words.json", "project.json"] {
            let _ = std::fs::rename(c.thumbs.join(format!("{old}.{e}")), c.thumbs.join(format!("{name}.{e}")));
        }
        let own_thumb = c.own_thumb.as_ref().map(|t| {
            let f = t.file_name().map(|f| f.to_string_lossy().replace(&old, &name)).unwrap_or_default();
            let to = t.with_file_name(f);
            if std::fs::rename(t, &to).is_ok() { to } else { t.clone() }
        });
        let shown = shown_name(&name);
        let root_game = match date_at(&name) {
            Some(k) if !matches!(&name[..k], "Clip" | "Montage" | "Recording") => name[..k].to_string(),
            _ => String::new(),
        };
        self.all.borrow_mut()[idx] = Rc::new(Clip {
            path: new,
            edited: is_edited(&name),
            rec: name.starts_with("Recording"),
            shown: shown.clone(),
            name,
            folder: c.folder.clone(),
            game: if !c.folder.is_empty() { c.game.clone() } else { root_game },
            mtime: c.mtime,
            secs: c.secs,
            size: c.size,
            dur: c.dur,
            thumbs: c.thumbs.clone(),
            own_thumb,
            sound: c.sound,
            gone: Default::default(),
            cloud: None,
        });
        self.changed(idx);
        Ok(shown)
    }

    /// is this clip in the cloud / has a share link (Echo's cloud.json / links.json)?
    pub fn in_cloud(&self, idx: usize) -> bool {
        let c = self.clip(idx);
        let file = c.path.file_name().unwrap_or_default().to_string_lossy().to_string();
        self.rel_in_cloud(&if c.folder.is_empty() { file } else { format!("{}/{file}", c.folder) })
    }
    /// Echo's delete_clip for these clips: Recycle Bin / Trash, its thumbnails + favourite go, an emptied folder goes.
    /// Cloud clips: only the copy on this PC goes; the clip is put on cloud.json's keep_remote list (like Echo's
    /// "Only from this PC") so it isn't downloaded again. Returns (deleted, first error).
    /// r2-threads (L5): the files go on a worker (`delete_clips_bg`, a clip at a time: Recycle Bin / Trash can wait
    /// on a locked file); `each(done, total)` after every clip and `done(deleted, first error)` run on the UI thread.
    pub fn delete_clips_bg(self: &Rc<Self>, ids: &[usize], mut each: impl FnMut(usize, usize) + 'static, done: impl FnOnce(usize, Option<String>) + 'static) {
        let jobs = self.delete_plan(ids);
        let total = jobs.len();
        let (root, thumbs) = (self.root(), self.thumbs_dir.clone());
        let lib = self.clone();
        let (mut i, mut n, mut err) = (0usize, 0usize, None::<String>);
        let res = Rc::new(RefCell::new((0usize, None::<String>)));
        let r2 = res.clone();
        crate::bgwork::stream(
            move |tx| {
                let mut keep = vec![];
                for j in jobs {
                    let r = delete_file(&j, &root, &thumbs);
                    if r.is_ok() && j.cloud {
                        keep.push(j.rel.clone());
                    }
                    let _ = tx.send((j, r));
                }
                keep_remote(&thumbs, keep);
            },
            move |(j, r): (DelJob, Result<(), String>)| {
                i += 1;
                match r {
                    Ok(()) => {
                        lib.deleted(&j);
                        n += 1;
                    }
                    Err(e) => {
                        err.get_or_insert(e);
                    }
                }
                *r2.borrow_mut() = (n, err.clone());
                each(i, total);
            },
            move || {
                let (n, e) = res.borrow_mut().clone();
                done(n, e);
            },
        );
    }
    /// r2-threads (L5): the clips to delete, as plain data a worker can take (gone ones skipped)
    pub fn delete_plan(&self, ids: &[usize]) -> Vec<DelJob> {
        ids.iter()
            .filter(|&&i| i < self.len() && !self.clip(i).gone.get())
            .map(|&i| {
                let c = self.clip(i);
                let file = c.path.file_name().unwrap_or_default().to_string_lossy().to_string();
                DelJob { idx: i, path: c.path.clone(), name: c.name.clone(), cloud: self.in_cloud(i), rel: if c.folder.is_empty() { file } else { format!("{}/{file}", c.folder) } }
            })
            .collect()
    }
    /// r2-threads (L5): the clip's file went (delete_file on a worker): its favourite goes, the library forgets it
    pub fn deleted(&self, j: &DelJob) {
        *self.disk_dirs.borrow_mut() = None; // r2-threads (L29): folders may have changed - the sidebar walks again
        if self.favs.borrow().contains(&j.name) {
            let _ = self.set_favs(&[j.name.clone()], false);
        }
        if j.idx < self.len() {
            self.clip(j.idx).gone.set(true);
        }
        self.picked.borrow_mut().remove(&j.idx);
    }

    /// one clip (rel = "Valorant/x.mp4") in Echo's cloud.json / links.json
    fn rel_in_cloud(&self, rel: &str) -> bool {
        let r = rel.replace('\\', "/").to_lowercase();
        self.cloud_sets(|cl, li| cl.contains(&r) || li.contains(&r)) // r2-threads (L36): cached
    }
    /// the folders the Move menu offers (sidebar order, sub-folders indented): (folder, depth)
    pub fn move_targets(&self) -> Vec<(String, usize)> {
        self.nav_items().iter().filter(|n| !n.folder.is_empty()).map(|n| (n.folder.to_string(), n.depth as usize)).collect()
    }

    /// Echo's game icon for a top folder (the old sidebar's gameIcons): <Echo's data folder>/icons/<safe name>.png
    /// of the folder's own name, else of a game whose new clips go to this folder (settings.json folder_alias)
    fn game_icon(&self, folder: &str, alias: &BTreeMap<String, String>) -> Option<Image> {
        let dir = crate::settings::path().parent()?.join("icons");
        let safe = |s: &str| s.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect::<String>();
        let mut names = vec![folder.to_string()];
        names.extend(alias.iter().filter(|(_, v)| v.eq_ignore_ascii_case(folder)).map(|(k, _)| k.clone()));
        for n in names {
            let p = dir.join(format!("{}.png", safe(&n)));
            let Ok(t) = std::fs::metadata(&p).and_then(|m| m.modified()) else { continue };
            let mut cache = self.icon_cache.borrow_mut();
            if let Some((t0, img)) = cache.get(&p) {
                if *t0 == t {
                    if img.is_some() {
                        return img.clone();
                    }
                    continue;
                }
            }
            let img = Image::load_from_path(&p).ok().filter(|i| i.size().width > 0);
            cache.insert(p, (t, img.clone()));
            if img.is_some() {
                return img;
            }
        }
        None
    }

    pub fn nav_items(&self) -> Vec<NavItem> {
        let d0 = day_start();
        let cnt = |n: usize| SharedString::from(if n > 0 { n.to_string() } else { String::new() });
        let all = self.all();
        let count = |f: &str| all.iter().filter(|c| self.nav_ok(c, f, d0)).count();
        let item = |id: &str, label: &str, n: usize, icon: &str, section: &str| NavItem {
            id: id.into(),
            label: label.into(),
            count: cnt(n),
            initial: "".into(),
            icon: icon.into(),
            section: section.into(),
            depth: 0,
            folder: "".into(),
            real: false,
            drop: "".into(),
            has_img: false,
            img: Default::default(),
            merge: Default::default(),
        };
        // Echo's sidebar: CLIPS (Cloud + Screenshots come with the real port), FOLDERS, TAGS
        let mut nav = vec![
            item("all", "All clips", all.len(), crate::icons::GRID, "CLIPS"),
            item("today", "Today", count("today"), crate::icons::CLOCK, ""),
            item("week", "This week", count("week"), crate::icons::CAL, ""),
            item("fav", "Favourites", count("fav"), crate::icons::STAR, ""),
            item("edited", "Edited", count("edited"), crate::icons::EDIT, ""),
            item("cloud", "Cloud", count("cloud"), crate::icons::CLOUD, ""), // gaps-library row 19
            item("shots", "Screenshots", crate::lib_shots::count(&self.root()), crate::icons::SHOTS, ""),
        ];
        // folders: real folders on disk (also empty ones) + folders of clips + games that only exist as a name
        // prefix; each top folder (A-Z), then its sub-folders in the remembered order (drag to reorder)
        let set = self.folder_set();
        // top folders: most clips first, like Echo's sidebar (ties A-Z)
        let all_c = self.all();
        let n_in = |f: &str| all_c.iter().filter(|c| Self::in_folder(c, f)).count();
        let mut tops: Vec<(String, usize)> = set.iter().filter(|f| !f.contains('/')).map(|f| (f.clone(), n_in(f))).collect();
        tops.sort_by(|a, b| b.1.cmp(&a.1).then(natural_cmp(&a.0, &b.0)));
        let tops: Vec<String> = tops.into_iter().map(|t| t.0).collect();
        let mut folders: Vec<String> = vec![];
        fn add(f: &str, set: &BTreeSet<String>, m: &LibModel, out: &mut Vec<String>) {
            out.push(f.to_string());
            for k in m.kids_sorted(f, set) {
                add(&k, set, m, out);
            }
        }
        for t in &tops {
            add(t, &set, self, &mut folders);
        }
        let root = self.root();
        let alias: BTreeMap<String, String> =
            echo_settings().and_then(|v| v.get("folder_alias").cloned()).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        for (k, f) in folders.iter().enumerate() {
            let icon = if f.contains('/') { None } else { self.game_icon(f, &alias) };
            let depth = f.matches('/').count();
            let label = f.rsplit('/').next().unwrap_or(f);
            let n = all.iter().filter(|c| Self::in_folder(c, f)).count();
            nav.push(NavItem {
                id: format!("game:{f}").into(),
                label: label.into(),
                count: cnt(n),
                initial: if depth > 0 { "".into() } else { label.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default().into() },
                icon: crate::icons::FOLDER.into(),
                section: if k == 0 { "FOLDERS".into() } else { "".into() },
                depth: depth as i32,
                folder: f.as_str().into(),
                real: root.join(f).is_dir() && !folder_reserved(f),
                drop: "".into(),
                has_img: icon.is_some(),
                img: icon.unwrap_or_default(),
                // gaps-library row 29: the right-click menu's "Merge into <folder>" (every other real folder)
                merge: ModelRc::new(VecModel::from(
                    folders
                        .iter()
                        .filter(|o| *o != f && !o.starts_with(&format!("{f}/")) && root.join(o.as_str()).is_dir() && !folder_reserved(o))
                        .map(|o| SharedString::from(o.as_str()))
                        .collect::<Vec<_>>(),
                )),
            });
        }
        for (k, (t, n)) in self.tag_counts().iter().enumerate() {
            nav.push(NavItem {
                id: format!("tag:{t}").into(),
                label: t.as_str().into(),
                count: cnt(*n),
                initial: "#".into(),
                icon: "".into(),
                section: if k == 0 { "TAGS".into() } else { "".into() },
                depth: 0,
                folder: "".into(),
                real: false,
                drop: "".into(),
                has_img: false,
                img: Default::default(),
                merge: Default::default(),
            });
        }
        nav
    }

    /// filter panel: sections as lines of chips (Slint has no flow layout: ~ `width` characters per line)
    pub fn filter_lines(&self) -> Vec<FLine> {
        let fsel = self.fsel.borrow();
        let on = |g: &str, k: &str| fsel.get(g).map(|v| v.iter().any(|x| x == k)).unwrap_or(false);
        let all = self.all();
        let cnt = |f: &dyn Fn(&Clip) -> bool| all.iter().filter(|c| f(c)).count() as i32;
        // gaps-library row 21: Echo's #fmenu - Game / Where / Type / Tags (Where always listed); the search box
        // filters the Game and Tags chips by name; Game chips carry the game's icon
        let q = self.fq.borrow().trim().to_lowercase();
        let hit = |label: &str| q.is_empty() || label.to_lowercase().contains(&q);
        let mut sections: Vec<(&str, &str, Vec<FOpt>)> = vec![];
        let mut games = self.games();
        games.sort_by(|a, b| natural_cmp(&a.0, &b.0));
        let alias: BTreeMap<String, String> =
            echo_settings().and_then(|v| v.get("folder_alias").cloned()).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        let mut g: Vec<FOpt> = games
            .iter()
            .filter(|(n, _)| hit(n))
            .map(|(n, c)| {
                let icon = self.game_icon(n, &alias);
                FOpt { group: "game".into(), key: n.as_str().into(), label: n.as_str().into(), count: *c as i32, on: on("game", n), has_img: icon.is_some(), img: icon.unwrap_or_default() }
            })
            .collect();
        let none = cnt(&|c| c.game.is_empty());
        if none > 0 && hit("No game") {
            g.push(FOpt { group: "game".into(), key: "".into(), label: "No game".into(), count: none, on: on("game", ""), ..Default::default() });
        }
        sections.push(("game", "GAME", g));
        sections.push((
            "where",
            "WHERE",
            WHERE
                .iter()
                .map(|(k, l)| FOpt { group: "where".into(), key: (*k).into(), label: (*l).into(), count: cnt(&|c| self.where_ok(c, k)), on: on("where", k), ..Default::default() })
                .collect(),
        ));
        sections.push((
            "type",
            "TYPE",
            TYPES
                .iter()
                .map(|(k, l)| FOpt { group: "type".into(), key: (*k).into(), label: (*l).into(), count: cnt(&|c| self.type_ok(c, k)), on: on("type", k), ..Default::default() })
                .collect(),
        ));
        let tags: Vec<FOpt> = self
            .tag_counts()
            .into_iter()
            .filter(|(t, _)| hit(&format!("#{t}")))
            .map(|(t, n)| FOpt { group: "tag".into(), key: t.as_str().into(), label: format!("#{t}").into(), count: n as i32, on: on("tag", &t), ..Default::default() })
            .collect();
        sections.push(("tag", "TAGS", tags));
        let mut lines = vec![];
        for (_, head, opts) in sections {
            if opts.is_empty() {
                continue;
            }
            let mut cur: Vec<FOpt> = vec![];
            let mut width = 0usize;
            let mut first = true;
            for o in opts {
                // chip width in "characters": label + count + padding
                let w = o.label.chars().count() + o.count.to_string().len() + 6 + if o.on { 2 } else { 0 } + if o.has_img { 3 } else { 0 };
                if !cur.is_empty() && width + w > 64 {
                    lines.push(FLine { head: if first { head.into() } else { "".into() }, opts: ModelRc::new(VecModel::from(std::mem::take(&mut cur))) });
                    first = false;
                    width = 0;
                }
                width += w;
                cur.push(o);
            }
            lines.push(FLine { head: if first { head.into() } else { "".into() }, opts: ModelRc::new(VecModel::from(cur)) });
        }
        lines
    }

    pub fn factive(&self) -> Vec<FOpt> {
        let mut v = vec![];
        for (g, keys) in self.fsel.borrow().iter() {
            for k in keys {
                let label = match g.as_str() {
                    "game" => if k.is_empty() { "No game".to_string() } else { k.clone() },
                    "tag" => format!("#{k}"),
                    "where" => WHERE.iter().find(|t| t.0 == k).map(|t| t.1.to_string()).unwrap_or(k.clone()),
                    _ => TYPES.iter().find(|t| t.0 == k).map(|t| t.1.to_string()).unwrap_or(k.clone()),
                };
                v.push(FOpt { group: g.as_str().into(), key: k.as_str().into(), label: label.into(), count: 0, on: true, ..Default::default() });
            }
        }
        v
    }

    pub fn ftoggle(&self, g: &str, k: &str) {
        let mut f = self.fsel.borrow_mut();
        let v = f.entry(g.to_string()).or_default();
        if let Some(p) = v.iter().position(|x| x == k) {
            v.remove(p);
        } else {
            v.push(k.to_string());
        }
    }

    // ---------- thumbnails ----------
    /// a thumbnail finished decoding (called on the UI thread)
    pub fn thumb_ready(&self, idx: usize, img: Image) {
        self.pending.borrow_mut().remove(&idx);
        if self.suspended.get() {
            return;
        }
        let t = self.tick.get() + 1;
        self.tick.set(t);
        {
            let mut c = self.cache.borrow_mut();
            c.insert(idx, (img, t));
            while c.len() > CACHE_MAX {
                let oldest = c.iter().min_by_key(|(_, (_, t))| *t).map(|(k, _)| *k).unwrap();
                c.remove(&oldest);
            }
        }
        self.changed(idx);
    }

    /// minimized: let go of every thumbnail (cards show empty until the window is back)
    pub fn suspend(&self) {
        if self.suspended.replace(true) {
            return;
        }
        self.cache.borrow_mut().clear();
        self.pending.borrow_mut().clear();
        self.notify.reset();
        self.home_notify.reset();
    }
    pub fn resume(&self) {
        if self.suspended.replace(false) {
            self.notify.reset();
            self.home_notify.reset();
        }
    }
    pub fn is_suspended(&self) -> bool {
        self.suspended.get()
    }

    /// the clip's thumbnail if it's loaded (shown in the player until the video's first picture)
    pub fn thumb_img(&self, idx: usize) -> Option<Image> {
        self.cache.borrow().get(&idx).map(|e| e.0.clone())
    }

}

impl Model for LibModel {
    type Data = Card;
    fn row_count(&self) -> usize {
        self.view.borrow().len()
    }
    fn row_data(&self, row: usize) -> Option<Card> {
        let idx = *self.view.borrow().get(row)?;
        Some(self.card_for(idx, row))
    }
    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

/// Home's cards: the same Card data as the library, for LibModel's `home` list (idx = HOME_BASE + position)
pub struct HomeCards(pub Rc<LibModel>);

impl Model for HomeCards {
    type Data = Card;
    fn row_count(&self) -> usize {
        self.0.home.borrow().len()
    }
    fn row_data(&self, row: usize) -> Option<Card> {
        let idx = *self.0.home.borrow().get(row)?;
        Some(self.0.card_for(idx, HOME_BASE + row))
    }
    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.0.home_notify
    }
}

impl LibModel {
    /// one card's data (`row` = what Card.idx says: a view row, or HOME_BASE + Home position)
    fn card_for(&self, idx: usize, row: usize) -> Card {
        let c = self.clip(idx);
        let t = self.tick.get() + 1;
        self.tick.set(t);
        let thumb = match self.cache.borrow_mut().get_mut(&idx) {
            Some(e) => {
                e.1 = t;
                Some(e.0.clone())
            }
            None => None,
        };
        let has = thumb.is_some();
        // Chat B: a picture made later (cloud-only clips get theirs from the cloud) is picked up on the next draw
        let p = c.thumb();
        if !has && !self.suspended.get() && p.exists() && self.pending.borrow_mut().insert(idx) {
            // + the clip file (local clips): the loader makes a sharper 720 px still of the same moment once
            let _ = self.tx.send((idx, p, if c.cloud.is_none() { Some(c.path.clone()) } else { None }));
        }
        let (in_cloud, shared) = self.cloud_flags(&c);
        let m = self.meta.borrow();
        let (tags, note): (Vec<SharedString>, String) = match m.get(&c.name) {
            Some(e) => (e.tags.iter().map(|t| SharedString::from(t.as_str())).collect(), e.note.replace('\n', " ")),
            None => (vec![], String::new()),
        };
        // William (9 Nov): Echo's .gico - the game's icon top-left on the picture (gameIcons[gameOf(c)]); the same
        // icons/<game>.png + cache as the sidebar folders
        let game_top = c.game.split('/').next().unwrap_or("").to_string();
        let gicon = if game_top.is_empty() { None } else { self.game_icon(&game_top, &BTreeMap::new()) };
        let job = crate::lib_cards::job_of(&c.path);
        Card {
            has_gicon: gicon.is_some(),
            gicon: gicon.unwrap_or_default(),
            idx: row as i32,
            title: SharedString::from(c.shown.as_str()),
            when: ago(c.mtime).into(),
            size: fmt_size(c.size).into(),
            dur: c.dur.or_else(|| self.probed.borrow().get(&idx).copied()).map(fmt_dur).unwrap_or_default().into(),
            // gaps-library row 2: Echo's per-card job ("Waiting…" / "Making smaller 42%")
            job: job.as_ref().map(|j| j.0.as_str()).unwrap_or("").into(),
            job_pct: job.as_ref().map(|j| j.1).unwrap_or(0.0),
            job_wait: job.as_ref().is_some_and(|j| j.2),
            game: SharedString::from(c.game.split('/').next().unwrap_or("")),
            thumb: thumb.unwrap_or_default(),
            has_thumb: has,
            fav: self.favs.borrow().contains(&c.name),
            picked: self.picked.borrow().contains(&idx),
            edited: c.edited,
            tags: ModelRc::new(VecModel::from(tags)),
            note: note.into(),
            cloud: in_cloud && c.cloud.is_none(),
            shared,
            cloud_only: c.cloud.is_some(),
            views: if shared {
                match crate::cloud::views_of(&crate::cloud::rel_of(&c)) {
                    0 => "Shared".into(),
                    1 => "Shared · 1 view".into(),
                    n => format!("Shared · {n} views").into(),
                }
            } else {
                "".into()
            },
            // L35: Echo's title on the Shared badge
            views_tip: if shared {
                match crate::cloud::views_of(&crate::cloud::rel_of(&c)) {
                    0 => "Shared - link copied before".into(),
                    n => format!("Shared - watched {n} time{} on its share page", if n == 1 { "" } else { "s" }).into(),
                }
            } else {
                "".into()
            },
        }
    }
    /// (in the cloud, has a share link) for badges - from the cache of cloud.json / links.json
    fn cloud_flags(&self, c: &Clip) -> (bool, bool) {
        let file = c.path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let rel = if c.folder.is_empty() { file } else { format!("{}/{file}", c.folder) }.to_lowercase();
        self.cloud_sets(|cl, li| (cl.contains(&rel), li.contains(&rel)))
    }
    /// r2-threads (L36): Echo's cloud.json rels + links.json keys (lower-case, '/'), parsed again only when one of
    /// the two files changed (size / time, looked at no more than every 300 ms) - was a parse on every lookup
    fn cloud_sets<T>(&self, f: impl FnOnce(&HashSet<String>, &HashSet<String>) -> T) -> T {
        let recent = self.cloud_cache.borrow().as_ref().is_some_and(|x| x.0.elapsed() < std::time::Duration::from_millis(300));
        if !recent {
            let stamp = |n: &str| std::fs::metadata(self.thumbs_dir.join(n)).ok().map(|m| (m.modified().ok(), m.len()));
            let key = [stamp("cloud.json"), stamp("links.json")];
            let same = self.cloud_cache.borrow().as_ref().is_some_and(|x| x.1 == key);
            if same {
                if let Some(x) = self.cloud_cache.borrow_mut().as_mut() {
                    x.0 = std::time::Instant::now();
                }
            } else {
                let read = |n: &str| std::fs::read_to_string(self.thumbs_dir.join(n)).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
                let norm = |x: &str| x.replace('\\', "/").to_lowercase();
                let cloud: HashSet<String> = read("cloud.json")
                    .and_then(|v| v.get("files").and_then(|f| f.as_array()).cloned())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|x| x.get("rel").and_then(|r| r.as_str()).map(norm))
                    .collect();
                let links: HashSet<String> = read("links.json").and_then(|v| v.as_object().cloned()).map(|m| m.keys().map(|k| norm(k)).collect()).unwrap_or_default();
                *self.cloud_cache.borrow_mut() = Some((std::time::Instant::now(), key, cloud, links));
            }
        }
        let cc = self.cloud_cache.borrow();
        let (_, _, cl, li) = cc.as_ref().unwrap();
        f(cl, li)
    }
}

/// the sharper card picture next to Echo's 480 px one: <name>.jpg -> <name>.card720.jpg
pub fn card_hi_path(thumb: &Path) -> PathBuf {
    let stem = thumb.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    thumb.with_file_name(format!("{stem}.card720.jpg"))
}

/// made, and not older than Echo's picture (a new "Use as thumbnail" makes it again)
pub fn card_hi_fresh(thumb: &Path, hi: &Path) -> bool {
    let m = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (m(hi), m(thumb)) {
        (Some(h), Some(t)) => h >= t,
        (Some(_), None) => true,
        _ => false,
    }
}

/// 720 px still of the moment Echo's thumbnail shows (<name>.thumbt, else 0.5 s like Echo's thumb())
pub fn make_card_hi(clip: &Path, thumb: &Path, hi: &Path) -> bool {
    if !clip.exists() {
        return false;
    }
    let Some(ff) = crate::sheets::ffmpeg() else { return false };
    let stem = thumb.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let at = std::fs::read_to_string(thumb.with_file_name(format!("{stem}.thumbt"))).ok().and_then(|s| s.trim().parse::<f64>().ok()).unwrap_or(0.5);
    let tmp = hi.with_extension("tmp.jpg");
    let ok = crate::edrender::hidden_cmd(&ff)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{at:.3}"), "-i"])
        .arg(clip)
        .args(["-frames:v", "1", "-vf", "scale=720:-2:flags=lanczos", "-q:v", "3"])
        .arg(&tmp)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    ok && std::fs::rename(&tmp, hi).is_ok()
}

/// Background decoder: JPEG -> RGB pixels (Send), turned into a Slint Image on the UI thread.
pub fn decode(path: &Path) -> Option<SharedPixelBuffer<Rgb8Pixel>> {
    let img = image::open(path).ok()?.to_rgb8();
    Some(SharedPixelBuffer::<Rgb8Pixel>::clone_from_slice(img.as_raw(), img.width(), img.height()))
}
