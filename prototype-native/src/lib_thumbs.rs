//! Card pictures that were never made (10 Nov 2026, "transfer summary" showed a black card): a clip on this PC
//! whose <clips>/.thumbs/<stem>.jpg is missing gets one here, like Echo's recorder.rs thumb(): 480 px at 0.5 s
//! (or the moment in <stem>.thumbt) + <stem>.dur. Before this nothing made a missing one - the player's Save copy
//! kept its picture only in a temp folder (gone after a restart), and files copied in by hand never got one.
//! One ffmpeg at a time on a worker, each clip tried once per run; the card redraws when the picture is there.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

thread_local! {
    /// clip files already asked for this run (also the ones that failed: no retry loop on a broken file)
    static TRIED: RefCell<HashSet<PathBuf>> = RefCell::new(HashSet::new());
    static WORKER: RefCell<Option<Sender<(usize, PathBuf, PathBuf)>>> = const { RefCell::new(None) };
}

/// called while a card is drawn: `idx` = the library's clip index, `clip` = the file, `thumbs` = <clips>/.thumbs
pub fn missing(idx: usize, clip: &Path, thumbs: &Path) {
    // a clip saved just now: the recorder / Save copy makes its picture itself (and the file may still be written)
    let fresh = std::fs::metadata(clip).ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_none_or(|e| e < Duration::from_secs(20));
    if fresh || !TRIED.with(|t| t.borrow_mut().insert(clip.to_path_buf())) {
        return;
    }
    WORKER.with(|w| {
        let mut w = w.borrow_mut();
        let tx = w.get_or_insert_with(start);
        if tx.send((idx, clip.to_path_buf(), thumbs.to_path_buf())).is_err() {
            *tx = start();
            let _ = tx.send((idx, clip.to_path_buf(), thumbs.to_path_buf()));
        }
    });
}

fn start() -> Sender<(usize, PathBuf, PathBuf)> {
    let (tx, rx) = std::sync::mpsc::channel::<(usize, PathBuf, PathBuf)>();
    std::thread::spawn(move || {
        let Some(ff) = crate::sheets::ffmpeg() else { return };
        for (idx, clip, thumbs) in rx {
            let (ok, dur) = make(&ff, &clip, &thumbs);
            crate::lib_net::log_line(&format!("library: card picture for {} {}", clip.file_name().unwrap_or_default().to_string_lossy(), if ok { "made (it was missing)" } else { "couldn't be made" }));
            if !ok && dur.is_none() {
                continue;
            }
            let _ = slint::invoke_from_event_loop(move || {
                if ok {
                    // made: may be asked again (deleted, then back with Undo - its picture went with the delete)
                    TRIED.with(|t| t.borrow_mut().remove(&clip));
                }
                let Some(lib) = crate::LIB.with(|l| l.borrow().clone()) else { return };
                if lib.clip(idx).path != clip {
                    return; // renamed / moved meanwhile: the next draw asks again under the new name
                }
                if let Some(d) = dur {
                    lib.probed.borrow_mut().insert(idx, d);
                }
                lib.changed(idx);
            });
        }
    });
    tx
}

/// Echo's thumb(): <stem>.jpg + <stem>.dur in `thumbs`. (picture made, length) - written to a temp name first so
/// the card loader never reads half a picture
pub fn make(ff: &Path, clip: &Path, thumbs: &Path) -> (bool, Option<f64>) {
    let stem = clip.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let _ = std::fs::create_dir_all(thumbs);
    let img = thumbs.join(format!("{stem}.jpg"));
    let dur_file = thumbs.join(format!("{stem}.dur"));
    let mut dur = std::fs::read_to_string(&dur_file).ok().and_then(|s| s.trim().parse::<f64>().ok());
    if dur.is_none() {
        dur = crate::edrender::hidden_cmd(&crate::edrender::ffprobe(ff))
            .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
            .arg(clip)
            .stdin(std::process::Stdio::null())
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok())
            .filter(|d| *d > 0.0);
        if let Some(d) = dur {
            let _ = std::fs::write(&dur_file, d.to_string());
        }
    }
    if img.exists() {
        return (true, dur);
    }
    let at = std::fs::read_to_string(thumbs.join(format!("{stem}.thumbt"))).ok().and_then(|s| s.trim().parse::<f64>().ok());
    // a clip shorter than half a second: its first picture
    let at = at.unwrap_or(if dur.is_some_and(|d| d <= 0.6) { 0.0 } else { 0.5 });
    let tmp = thumbs.join(format!("{stem}.making.jpg"));
    let ok = crate::edrender::hidden_cmd(ff)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{at:.3}"), "-i"])
        .arg(clip)
        .args(["-frames:v", "1", "-vf", "scale=480:-2", "-q:v", "4"])
        .arg(&tmp)
        .stdin(std::process::Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success())
        && std::fs::metadata(&tmp).is_ok_and(|m| m.len() > 0);
    // the recorder may have made it meanwhile: keep that one
    let ok = ok && (img.exists() || std::fs::rename(&tmp, &img).is_ok());
    let _ = std::fs::remove_file(&tmp);
    (ok || img.exists(), dur)
}

// ---------- a clip's files in .thumbs (10 Nov: delete / rename / folder merge left most of them behind) ----------

/// per-clip cache files in .thumbs, <stem>.<this> (Echo's pictures, lengths, sheets, audio tracks, editor data)
const FIXED: &[&str] = &[
    "jpg", "dur", "card720.jpg", "thumbt", "scrub.jpg", "strip.jpg", "strip2.jpg", "strip3.jpg",
    "a0.m4a", "a1.m4a", "a2.m4a", "a3.m4a", "a4.m4a", "a5.m4a", "a6.m4a", "a7.m4a",
    "marks.json", "levels.json", "words.json", "project.json",
];
/// made by the user (chosen picture moment, bookmarks, editor project) or slow to make (captions): a delete keeps
/// them so Undo (undo.rs, back from the Trash) brings the clip back as it was; its picture is made again (above)
const KEEP_ON_DELETE: &[&str] = &["thumbt", "marks.json", "project.json", "words.json"];

/// <stem>.hero<w>.<size>.jpg (Echo's hero_frame), <stem>.av1pack.<size> / .av1fix.<size> (AV1 player fixes)
fn sized(rest: &str) -> bool {
    let num = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if let Some(r) = rest.strip_prefix("hero").and_then(|r| r.strip_suffix(".jpg")) {
        return r.split_once('.').is_some_and(|(w, s)| num(w) && num(s));
    }
    rest.strip_prefix("av1pack.").or_else(|| rest.strip_prefix("av1fix.")).is_some_and(num)
}

/// every file / folder in `thumbs` that belongs to the clip `file` (its file name, e.g. "Clip 1.mp4"):
/// (path, part after "<stem>." or the folder kind) - also editframes/<stem>, playerframes/<stem>, discord/<MB>/<file>
fn files_of(thumbs: &Path, file: &str) -> Vec<(PathBuf, String)> {
    let stem = Path::new(file).file_stem().unwrap_or_default().to_string_lossy().to_string();
    let mut v = vec![];
    let pre = format!("{stem}.");
    if let Ok(rd) = std::fs::read_dir(thumbs) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if let Some(rest) = n.strip_prefix(&pre) {
                if (FIXED.contains(&rest) || sized(rest)) && e.path().is_file() {
                    v.push((e.path(), rest.to_string()));
                }
            }
        }
    }
    for d in ["editframes", "playerframes"] {
        let p = thumbs.join(d).join(&stem);
        if p.is_dir() {
            v.push((p, d.to_string()));
        }
    }
    if let Ok(rd) = std::fs::read_dir(thumbs.join("discord")) {
        for e in rd.flatten() {
            let p = e.path().join(file);
            if p.is_file() {
                v.push((p, "discord".into()));
            }
        }
    }
    v
}

/// the clip `file` was deleted: its .thumbs files go (not the ones Undo should bring back, KEEP_ON_DELETE)
pub fn remove_files(thumbs: &Path, file: &str) {
    for (p, rest) in files_of(thumbs, file) {
        if KEEP_ON_DELETE.contains(&rest.as_str()) {
            continue;
        }
        let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
    }
}

/// the clip `old_file` is now `new_file` (rename, folder merge "name (2)"): its .thumbs files follow; one already
/// there under the new name (a leftover) is replaced
pub fn rename_files(thumbs: &Path, old_file: &str, new_file: &str) {
    let new_stem = Path::new(new_file).file_stem().unwrap_or_default().to_string_lossy().to_string();
    for (p, rest) in files_of(thumbs, old_file) {
        let to = match rest.as_str() {
            "editframes" | "playerframes" => p.with_file_name(&new_stem),
            "discord" => p.with_file_name(new_file),
            _ => p.with_file_name(format!("{new_stem}.{rest}")),
        };
        if to.is_dir() {
            let _ = std::fs::remove_dir_all(&to);
        }
        let _ = std::fs::rename(&p, &to);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_and_remove_follow_every_file() {
        let t = std::env::temp_dir().join(format!("echo_lib_thumbs_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(t.join("editframes/Clip 1")).unwrap();
        std::fs::create_dir_all(t.join("discord/10")).unwrap();
        for f in ["Clip 1.jpg", "Clip 1.dur", "Clip 1.card720.jpg", "Clip 1.hero960.123.jpg", "Clip 1.av1pack.123", "Clip 1.thumbt", "Clip 1.marks.json", "discord/10/Clip 1.mp4", "editframes/Clip 1/f_1.jpg", "Clip 1.x.jpg", "Clip 10.jpg", "links.json"] {
            std::fs::write(t.join(f), b"x").unwrap();
        }
        rename_files(&t, "Clip 1.mp4", "My clip.mp4");
        for f in ["My clip.jpg", "My clip.dur", "My clip.card720.jpg", "My clip.hero960.123.jpg", "My clip.av1pack.123", "My clip.thumbt", "My clip.marks.json", "discord/10/My clip.mp4", "editframes/My clip/f_1.jpg"] {
            assert!(t.join(f).exists(), "{f} after rename");
        }
        // not the clip's: a clip named "Clip 1.x", another clip, Echo's own lists
        for f in ["Clip 1.x.jpg", "Clip 10.jpg", "links.json"] {
            assert!(t.join(f).exists(), "{f} must stay");
        }
        remove_files(&t, "My clip.mp4");
        for f in ["My clip.jpg", "My clip.dur", "My clip.card720.jpg", "My clip.hero960.123.jpg", "My clip.av1pack.123", "discord/10/My clip.mp4", "editframes/My clip"] {
            assert!(!t.join(f).exists(), "{f} after delete");
        }
        for f in ["My clip.thumbt", "My clip.marks.json", "Clip 10.jpg"] {
            assert!(t.join(f).exists(), "{f} kept for Undo / not the clip's");
        }
        let _ = std::fs::remove_dir_all(&t);
    }
}
