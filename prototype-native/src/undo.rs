//! 2.0.2: Undo on the notes for the two things that can really be undone.
//! - delete: clips go to the Trash / Recycle Bin (never a plain delete), so "Clip deleted · Undo" puts the files
//!   back where they were (Linux: the freedesktop Trash's info/*.trashinfo, which gio uses too; Windows: the
//!   Recycle Bin's $I / $R pair). Repeats add up ("3 clips deleted · Undo" puts all three back). Not offered in the
//!   Flatpak (its Trash portal doesn't say where the file went).
//! - move: "Moved N clips to X · Undo" moves them back to the folders they came from (same clip ids).
use crate::library::LibModel;
use crate::notes::{self, Kind};
use crate::AppWindow;
use slint::ComponentHandle;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

fn s(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn sync_all(a: &AppWindow, lib: &Rc<LibModel>) {
    lib.refresh();
    crate::home_fill(a, lib);
    crate::UI_SYNC.with(|f| {
        if let Some(f) = f.borrow().as_ref() {
            f(true)
        }
    });
}

// ---------------------------------------------------------------- delete

/// a deleted clip: its file and whether it was a favourite (the star comes back too)
#[derive(Clone)]
pub struct Gone {
    pub path: PathBuf,
    pub name: String,
    pub fav: bool,
}

// what the delete note on screen would put back, per kind of delete ("del" = Trash, "delpc" = only from this PC)
thread_local! {
    static PENDING: RefCell<Vec<(String, i32, Vec<Gone>)>> = const { RefCell::new(vec![]) };
}

/// can Echo find a trashed file again here?
pub fn can_restore() -> bool {
    #[cfg(windows)]
    {
        true
    }
    #[cfg(not(windows))]
    {
        !echo_core::linuxutil::in_flatpak()
    }
}

/// the clips (by id) as they are now, before a delete
pub fn before_delete(lib: &LibModel, ids: &[usize]) -> Vec<Gone> {
    let favs = lib.favs.borrow();
    ids.iter().filter(|&&i| i < lib.len()).map(|&i| {
        let c = lib.clip(i);
        Gone { path: c.path.clone(), name: c.name.clone(), fav: favs.contains(&c.name) }
    }).collect()
}

/// the delete finished: "Clip deleted · Undo" (repeats add up). `items` = the clips that really went.
pub fn deleted(a: &AppWindow, lib: &Rc<LibModel>, items: Vec<Gone>, cloud: bool) {
    let n = items.len();
    if n == 0 {
        return;
    }
    let key = if cloud { "delpc" } else { "del" };
    let text = move |t: usize| {
        if cloud {
            if t == 1 { "Deleted from this PC - it's still in your cloud".to_string() } else { format!("{t} clips deleted from this PC - still in your cloud") }
        } else if t == 1 {
            "Clip deleted".to_string()
        } else {
            format!("{t} clips deleted")
        }
    };
    if !can_restore() {
        notes::merge(key, Kind::Done, n, text, None);
        return;
    }
    let (w, l2) = (a.as_weak(), lib.clone());
    let undo: Box<dyn Fn()> = Box::new(move || {
        let list = PENDING.with(|p| {
            let mut p = p.borrow_mut();
            p.iter().position(|(k, _, _)| k == key).map(|i| p.remove(i).2).unwrap_or_default()
        });
        if let Some(a) = w.upgrade() {
            restore(&a, &l2, list);
        }
    });
    let (id, _) = notes::merge(key, Kind::Done, n, text, Some(("Undo", undo)));
    PENDING.with(|p| {
        let mut p = p.borrow_mut();
        match p.iter_mut().find(|(k, _, _)| k == key) {
            Some(e) if e.1 == id => e.2.extend(items),
            Some(e) => *e = (key.to_string(), id, items),
            None => p.push((key.to_string(), id, items)),
        }
    });
}

/// Undo: the files come back from the Trash / Recycle Bin (on a worker), the library finds them again
fn restore(a: &AppWindow, lib: &Rc<LibModel>, list: Vec<Gone>) {
    if list.is_empty() {
        return;
    }
    let total = list.len();
    let busy = notes::say(Kind::Run, if total == 1 { "Putting the clip back…".to_string() } else { format!("Putting {total} clips back…") });
    let paths: Vec<PathBuf> = list.iter().map(|g| g.path.clone()).collect();
    let (w, lib) = (a.as_weak(), lib.clone());
    crate::lib_net::bg(
        move || paths.iter().map(|p| restore_file(p)).collect::<Vec<Result<(), String>>>(),
        move |res| {
            notes::note_out(busy);
            let Some(a) = w.upgrade() else { return };
            let ok: Vec<&Gone> = list.iter().zip(&res).filter(|(_, r)| r.is_ok()).map(|(g, _)| g).collect();
            lib.sync_disk(&lib.root());
            let favs: Vec<String> = ok.iter().filter(|g| g.fav).map(|g| g.name.clone()).collect();
            if !favs.is_empty() {
                let _ = lib.set_favs(&favs, true);
            }
            sync_all(&a, &lib);
            let bin = if cfg!(windows) { "Recycle Bin" } else { "Trash" };
            match res.iter().find_map(|r| r.as_ref().err()) {
                None => {
                    notes::say(Kind::Done, if total == 1 { "Clip restored".to_string() } else { format!("{total} clips restored") });
                }
                Some(e) if ok.is_empty() => {
                    notes::say(Kind::Err, format!("Couldn't put it back ({e}) - it's still in the {bin}"));
                }
                Some(e) => {
                    notes::say(Kind::Err, format!("{} of {total} clips restored - the rest are still in the {bin} ({e})", ok.len()));
                }
            }
        },
    );
}

/// one file back from the Trash / Recycle Bin to where it was
fn restore_file(orig: &Path) -> Result<(), String> {
    if orig.exists() {
        return Err("there's a file with that name there again".into());
    }
    let (data, extra) = find_trashed(orig).ok_or_else(|| format!("it isn't in the {} any more", if cfg!(windows) { "Recycle Bin" } else { "Trash" }))?;
    if let Some(dir) = orig.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&data, orig).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(extra);
    Ok(())
}

/// Linux: (the file in Trash/files, its .trashinfo) - the newest one for this path
#[cfg(not(windows))]
fn find_trashed(orig: &Path) -> Option<(PathBuf, PathBuf)> {
    use std::os::unix::fs::MetadataExt;
    let want = [orig.to_path_buf(), norm(orig)];
    let uid = std::fs::metadata("/proc/self").map(|m| m.uid()).ok()?;
    // (trash dir, top folder relative Path= values start from)
    let mut dirs: Vec<(PathBuf, PathBuf)> = vec![];
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    let data = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()).map(PathBuf::from).or_else(|| home.map(|h| h.join(".local/share")));
    if let Some(d) = data {
        dirs.push((d.join("Trash"), PathBuf::from("/")));
    }
    for p in want.iter() {
        let mut up = p.parent();
        while let Some(top) = up {
            for t in [top.join(format!(".Trash-{uid}")), top.join(".Trash").join(uid.to_string())] {
                if t.is_dir() && !dirs.iter().any(|(d, _)| *d == t) {
                    dirs.push((t, top.to_path_buf()));
                }
            }
            up = top.parent();
        }
    }
    let mut best: Option<(String, PathBuf, PathBuf)> = None;
    for (dir, top) in dirs {
        let Ok(rd) = std::fs::read_dir(dir.join("info")) else { continue };
        for e in rd.flatten() {
            let info = e.path();
            let Some(stem) = info.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".trashinfo")).map(String::from) else { continue };
            let Ok(txt) = std::fs::read_to_string(&info) else { continue };
            let field = |k: &str| txt.lines().find_map(|l| l.strip_prefix(k)).map(|v| v.trim().to_string());
            let Some(p) = field("Path=") else { continue };
            let p = PathBuf::from(unpercent(&p));
            let p = if p.is_absolute() { p } else { top.join(p) };
            if !want.contains(&p) {
                continue;
            }
            let when = field("DeletionDate=").unwrap_or_default();
            let file = dir.join("files").join(&stem);
            if std::fs::symlink_metadata(&file).is_ok() && best.as_ref().is_none_or(|b| when > b.0) {
                best = Some((when, file, info));
            }
        }
    }
    best.map(|(_, f, i)| (f, i))
}

/// the path with its nearest existing folder resolved (a symlinked clips folder: gio writes the real path)
#[cfg(not(windows))]
fn norm(p: &Path) -> PathBuf {
    let mut base = p.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = vec![];
    while !base.exists() {
        match (base.file_name().map(|f| f.to_os_string()), base.parent()) {
            (Some(f), Some(up)) => {
                rest.push(f);
                base = up.to_path_buf();
            }
            _ => return p.to_path_buf(),
        }
    }
    let mut out = std::fs::canonicalize(&base).unwrap_or(base);
    for f in rest.into_iter().rev() {
        out.push(f);
    }
    out
}

/// the Trash spec's Path= is URL-escaped ("%20")
#[cfg_attr(windows, allow(dead_code))]
fn unpercent(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Windows: (the $R file, its $I file) in <drive>\$Recycle.Bin\<your SID> - the newest one for this path
#[cfg(windows)]
fn find_trashed(orig: &Path) -> Option<(PathBuf, PathBuf)> {
    let drive = orig.components().next()?;
    let bin = PathBuf::from(format!("{}\\$Recycle.Bin", drive.as_os_str().to_string_lossy()));
    let want = orig.to_string_lossy().to_lowercase();
    let mut best: Option<(u64, PathBuf, PathBuf)> = None;
    for sid in std::fs::read_dir(bin).ok()?.flatten() {
        let Ok(rd) = std::fs::read_dir(sid.path()) else { continue }; // other users' folders can't be read
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(tail) = name.strip_prefix("$I") else { continue };
            let Some((when, p)) = std::fs::read(e.path()).ok().and_then(|b| parse_recycle_info(&b)) else { continue };
            if p.to_lowercase() != want {
                continue;
            }
            let data = sid.path().join(format!("$R{tail}"));
            if data.exists() && best.as_ref().is_none_or(|b| when > b.0) {
                best = Some((when, data, e.path()));
            }
        }
    }
    best.map(|(_, d, i)| (d, i))
}

/// a Recycle Bin $I file: (deleted at, original path). Version 1 (Vista-8): the path is a fixed 260 chars at 24;
/// version 2 (10/11): its length (chars, with the NUL) at 24, the path from 28.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_recycle_info(b: &[u8]) -> Option<(u64, String)> {
    let u64_at = |o: usize| b.get(o..o + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap()));
    let ver = u64_at(0)?;
    let when = u64_at(16)?;
    let (start, chars) = match ver {
        1 => (24, 260),
        2 => (28, u32::from_le_bytes(b.get(24..28)?.try_into().ok()?) as usize),
        _ => return None,
    };
    let raw = b.get(start..(start + chars * 2).min(b.len()))?;
    let w: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
    Some((when, String::from_utf16_lossy(&w)))
}

// ---------------------------------------------------------------- move

/// a moved clip: (id, its name - to be sure it's still the same clip, the folder it came from)
pub type Was = (usize, String, String);

/// the clips (by id) and their folders now, before a move
pub fn before_move(lib: &LibModel, ids: &[usize]) -> Vec<Was> {
    ids.iter().filter(|&&i| i < lib.len()).map(|&i| {
        let c = lib.clip(i);
        (i, c.name.clone(), c.folder.clone())
    }).collect()
}

/// the move finished: Echo's "Moved N clips to X" (+ the last error after it) with Undo
pub fn moved(a: &AppWindow, lib: &Rc<LibModel>, was: Vec<Was>, n: usize, to: &str, err: Option<String>) {
    let dest = if to.is_empty() { "the main folder".to_string() } else { to.to_string() }; // Echo: no quotes (r2-texts L48)
    let msg = format!("Moved {n} clip{} to {dest}{}", s(n), err.map(|e| format!(" - {e}")).unwrap_or_default());
    // only the clips that really went (a clash / a locked file keeps some where they were)
    let was: Vec<Was> = was.into_iter().filter(|(i, name, from)| *i < lib.len() && lib.clip(*i).name == *name && lib.clip(*i).folder != *from).collect();
    if was.is_empty() {
        notes::say(Kind::Done, msg);
        return;
    }
    let (w, lib) = (a.as_weak(), lib.clone());
    notes::say_act(Kind::Done, msg, "Undo", move || {
        if let Some(a) = w.upgrade() {
            move_back(&a, &lib, was.clone());
        }
    });
}

fn move_back(a: &AppWindow, lib: &Rc<LibModel>, was: Vec<Was>) {
    // grouped by the folder they came from; a clip that changed meanwhile (gone, renamed, moved again) is left alone
    let mut by: Vec<(Vec<usize>, String)> = vec![];
    for (i, name, from) in was {
        if i >= lib.len() || lib.clip(i).gone.get() || lib.clip(i).name != name || lib.clip(i).folder == from {
            continue;
        }
        match by.iter_mut().find(|(_, f)| *f == from) {
            Some(b) => b.0.push(i),
            None => by.push((vec![i], from)),
        }
    }
    if by.is_empty() {
        notes::say(Kind::Info, "Nothing to move back - those clips changed since");
        return;
    }
    // the player lets go of the file first (Windows: os error 32)
    if a.get_player_open() && !a.global::<crate::Ed>().get_open() {
        a.global::<crate::Play>().invoke_close();
    }
    let (w, l2) = (a.as_weak(), lib.clone());
    lib.move_many_bg(by, move |n, err| {
        let Some(a) = w.upgrade() else { return };
        match (n, err) {
            (0, e) => {
                notes::say(Kind::Err, format!("Couldn't move them back{}", e.map(|e| format!(" - {e}")).unwrap_or_default()));
            }
            (n, None) => {
                notes::say(Kind::Done, format!("Moved {n} clip{} back", s(n)));
            }
            (n, Some(e)) => {
                notes::say(Kind::Err, format!("Moved {n} clip{} back - {e}", s(n)));
            }
        }
        sync_all(&a, &l2);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recycle_info() {
        // version 2: "C:\a.mp4"
        let p: Vec<u16> = "C:\\a.mp4\0".encode_utf16().collect();
        let mut b = vec![];
        b.extend(2u64.to_le_bytes());
        b.extend(1234u64.to_le_bytes());
        b.extend(99u64.to_le_bytes());
        b.extend((p.len() as u32).to_le_bytes());
        for c in p {
            b.extend(c.to_le_bytes());
        }
        assert_eq!(parse_recycle_info(&b), Some((99, "C:\\a.mp4".to_string())));
        assert_eq!(unpercent("/home/a%20b/c%2Cd.mp4"), "/home/a b/c,d.mp4");
        assert_eq!(unpercent("/x/100%"), "/x/100%");
    }

    /// a real round trip through this PC's Trash (a tiny file under ~/.cache): `cargo test --release --bin
    /// echo-native-proto -- --ignored trash_roundtrip`
    #[cfg(not(windows))]
    #[test]
    #[ignore]
    fn trash_roundtrip() {
        let dir = PathBuf::from(std::env::var("HOME").unwrap()).join(".cache/echo-undo-test/a b,c");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("clip 100% (2).mp4");
        std::fs::write(&f, b"echo undo test").unwrap();
        crate::linux::trash(&f).unwrap();
        assert!(!f.exists());
        restore_file(&f).unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"echo undo test");
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }
}
