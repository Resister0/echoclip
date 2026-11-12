//! Chat C: Copy for Discord (Echo's copySized / shrink_clip / copy_to_clipboard). Copy ▾ sizes: Full size (the file
//! itself) or Discord Free / Nitro Basic / Nitro / Custom (settings.json discord_<k>_mb): a clip bigger than the size
//! is re-encoded to fit (BT.709 TV range + tags = the "too dark in Discord" fix, only the first audio track), cached
//! in <clips>/.thumbs/discord/<MB>/ like Echo, then the FILE goes on the clipboard (Windows CF_HDROP through user32 /
//! kernel32 loaded at run time ⚠ untested; Linux wl-copy / xclip text/uri-list) - paste it into Discord.
use crate::{prog_box, prog_done, prog_set, toast, AppWindow};
use slint::ComponentHandle;
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::Stdio;

/// MB for a Copy size key ("full" / unknown = 0 = the original file); `key` "" = the Copy button's own size
/// (settings copy_default). r2-threads (L4): one read of settings.json for both.
fn size_mb(key: &str) -> f64 {
    let st = crate::library::echo_settings().unwrap_or_default();
    let def = default_in(&st);
    mb_in(&st, if key.is_empty() { &def } else { key })
}

fn mb_in(st: &serde_json::Value, key: &str) -> f64 {
    if key == "full" || key.is_empty() {
        return 0.0;
    }
    let d = match key { "free" => 20.0, "basic" => 50.0, "nitro" => 1000.0, "custom" => 100.0, _ => 0.0 };
    st.get(format!("discord_{key}_mb")).and_then(|v| v.as_f64()).unwrap_or(d)
}

/// the Copy button's own size (settings copy_default)
fn default_in(st: &serde_json::Value) -> String {
    st.get("copy_default").and_then(|v| v.as_str()).unwrap_or("full").to_string()
}

/// Echo's fmtMb: a Copy size target, whole MB / GB ("20 MB", "1 GB") - PORT_LEFTOVERS L25
fn fmt_mb(mb: f64) -> String {
    if mb >= 1000.0 { format!("{} GB", mb / 1000.0) } else { format!("{mb} MB") }
}

thread_local! {
    /// r2-threads (L24): clips being copied right now (Echo's `sizing`: a 2nd start used to fight the 1st one)
    static SIZING: std::cell::RefCell<std::collections::HashSet<PathBuf>> = Default::default();
    /// r2-threads (L4): Home's "Share a clip" step is written once per run, not on every click
    static COPIED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn sizing_end(p: &Path) {
    SIZING.with(|s| s.borrow_mut().remove(p));
}

/// what a Copy click turned out to need (decided on a worker: size, AV1 probe, clipboard)
enum Step {
    /// the clip fits: it's on the clipboard (or the error)
    Done(Result<String, String>),
    /// fits, but AV1: an H.264 copy first (Echo's shareable)
    Av1,
    /// too big: make it smaller
    Shrink,
}

/// Copy ▾ / the Copy button: `key` "" = the default size. Runs the shrink on a worker thread with a progress box.
/// r2-threads (L4): the size check, the AV1 ffprobe and the clipboard run on a worker too.
pub fn copy_clip_ui(a: &AppWindow, path: PathBuf, thumbs: PathBuf, key: &str) {
    if SIZING.with(|s| s.borrow().contains(&path)) {
        toast(a, "This clip is already being copied - one moment");
        return;
    }
    // Home's Get started "Share a clip" counts a copy as done (Echo's echo_copied)
    if !COPIED.with(|c| c.replace(true)) {
        crate::lib_fx::ui_set("gs_copied", serde_json::json!(true));
    }
    let mb = size_mb(key);
    SIZING.with(|s| s.borrow_mut().insert(path.clone()));
    let av1_known = crate::lib_cards::av1_known(&path);
    let (w, p2) = (a.as_weak(), path.clone());
    crate::lib_net::bg(
        move || {
            let size = std::fs::metadata(&p2).map(|m| m.len()).unwrap_or(0);
            if mb > 0.0 && size as f64 > mb * 1e6 {
                return (Step::Shrink, None);
            }
            // audit (9 Nov): Echo's copy_clip copies rec.shareable(): an AV1 clip (shrunk old clips) becomes an H.264
            // copy first, so it plays in Discord - this copied the AV1 file as it was
            let probed = match av1_known {
                Some(_) => None,
                None if crate::rec_core::rec().is_some() => Some(echo_core::recorder::is_av1(&p2.to_string_lossy())),
                None => None,
            };
            if crate::rec_core::rec().is_some() && (av1_known.or(probed) == Some(true)) {
                return (Step::Av1, probed);
            }
            let r = clipboard_file(&p2).map(|_| if mb > 0.0 { format!(" ({})", crate::library::fmt_size(size)) } else { String::new() });
            (Step::Done(r), probed)
        },
        move |(step, probed)| {
            if let Some(is) = probed {
                crate::lib_cards::av1_note(&path, is);
            }
            let Some(a) = w.upgrade() else {
                sizing_end(&path);
                return;
            };
            match step {
                Step::Done(r) => {
                    sizing_end(&path);
                    match r {
                        Ok(sz) => toast(&a, format!("Copied{sz} - paste it into Discord or anywhere")),
                        Err(e) => toast(&a, e),
                    }
                }
                Step::Av1 => copy_av1(&a, path, mb),
                Step::Shrink => copy_shrunk(&a, path, thumbs, mb),
            }
        },
    );
}

/// the clip fits but is AV1: Echo's shareable() makes an H.264 copy, that goes on the clipboard. With a size (`mb`
/// > 0) an H.264 copy over it is shrunk to the size first (QA 11 Nov 2026: a 7.8 MB AV1 clip came out as a 31.8 MB
/// H.264 copy for a 10 MB limit - Discord refuses it)
fn copy_av1(a: &AppWindow, path: PathBuf, mb: f64) {
    let Some(r) = crate::rec_core::rec() else {
        sizing_end(&path);
        return;
    };
    let pb = prog_box("Making it play in Discord…");
    prog_set(pb, None, None, Some(-1.0));
    let w = a.as_weak();
    let src = path.to_string_lossy().to_string();
    crate::lib_net::bg(
        move || {
            r.shareable(&src)
                .and_then(|p| {
                    let big = std::fs::metadata(&p).map_or(0, |m| m.len()) as f64 > mb * 1e6;
                    if mb > 0.0 && big { r.shrink_clip(&p, mb, &|_| {}) } else { Ok(p) }
                })
                .and_then(|p| clipboard_file(Path::new(&p)).map(|_| std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0)))
        },
        move |res| {
            sizing_end(&path);
            prog_done(pb, 300);
            let Some(a) = w.upgrade() else { return };
            match res {
                Ok(sz) => toast(&a, format!("Copied ({}) - paste it into Discord or anywhere", crate::library::fmt_size(sz))),
                Err(e) => toast(&a, e),
            }
        },
    );
}

/// too big for the size: Echo's shrink_clip on a worker with a progress box
fn copy_shrunk(a: &AppWindow, path: PathBuf, thumbs: PathBuf, mb: f64) {
    let Some(ff) = crate::sheets::ffmpeg() else {
        sizing_end(&path);
        toast(a, "Making it smaller needs ffmpeg (Echo's copy wasn't found)");
        return;
    };
    let pb = prog_box(&format!("Shrinking to fit {}…", fmt_mb(mb)));
    prog_set(pb, None, None, Some(0.0));
    let w = a.as_weak();
    std::thread::spawn(move || {
        let prog = move |f: f64| {
            let _ = slint::invoke_from_event_loop(move || prog_set(pb, None, Some(&format!("{:.0}%", f * 100.0)), Some((f * 100.0) as f32)));
        };
        // 9 Nov: Echo's OWN shrink_clip - it adds the Echo watermark (Settings > Watermark) like the old app;
        // the copy below never did. Same cache folder (.thumbs/discord/<MB>), same sizes / encoders / 3 tries.
        let r = match crate::rec_core::rec() {
            Some(rec) => rec.shrink_clip(&path.to_string_lossy(), mb, &prog).map(PathBuf::from),
            None => shrink(&ff, &path, &thumbs, mb, &prog),
        }
        .and_then(|out| clipboard_file(&out).map(|_| std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0)));
        let _ = slint::invoke_from_event_loop(move || {
            sizing_end(&path);
            prog_done(pb, 400);
            let Some(a) = w.upgrade() else { return };
            match r {
                Ok(sz) => toast(&a, format!("Copied ({}) - paste it into Discord or anywhere", crate::library::fmt_size(sz))),
                Err(e) => toast(&a, e),
            }
        });
    });
}

/// Echo's shrink_clip without the recorder: the same size fitting (echo-core's fit_to_size: aim at 95 %, measure,
/// redo when over or far under), no watermark
pub fn shrink(ff: &Path, src: &Path, thumbs: &Path, target_mb: f64, progress: &dyn Fn(f64)) -> Result<PathBuf, String> {
    use echo_core::recorder::{fit_to_size, probe_len, FIT_ENCS, FIT_REUSE};
    let limit = (target_mb * 1_000_000.0) as u64;
    let src_meta = std::fs::metadata(src).map_err(|e| e.to_string())?;
    let name = src.file_name().ok_or("bad clip path")?.to_owned();
    let dir = thumbs.join("discord").join(format!("{}", target_mb.round() as u64));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let out = dir.join(&name);
    // QA 11 Nov 2026: cached by file name only - a clip with the same name in another folder got this one's copy;
    // reuse only a copy made from this exact file (echo-core shrink_clip's .sig)
    let sig = echo_core::recorder::shrink_sig(src, &src_meta);
    let sig_file = PathBuf::from(format!("{}.sig", out.to_string_lossy()));
    // made earlier (newer than the clip, BT.709-tagged, not far under the size, from this file): reuse
    if let (Ok(m), Ok(sm)) = (std::fs::metadata(&out), src_meta.modified()) {
        let colours_ok = crate::edrender::hidden_cmd(&crate::edrender::ffprobe(ff)).args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=color_space", "-of", "csv=p=0"])
            .arg(&out).output().map_or(false, |o| String::from_utf8_lossy(&o.stdout).trim() == "bt709");
        let same_src = std::fs::read_to_string(&sig_file).map_or(false, |s| s.trim() == sig);
        if m.len() > 0 && m.len() <= limit && m.len() as f64 >= limit as f64 * FIT_REUSE && m.modified().map_or(false, |t| t >= sm) && colours_ok && same_src {
            return Ok(out);
        }
    }
    let _ = std::fs::remove_file(&sig_file);
    let dur = probe_len(src).or_else(|| crate::pl_media::duration(ff, src)).ok_or("could not read the clip length")?;
    let (_, h, audio, _) = crate::edrender::probe(ff, src);
    let mk = || crate::edrender::hidden_cmd(ff);
    fit_to_size(&mk, src, &dir.join("shrinking.tmp"), &out, limit, dur, h.max(2), audio, &FIT_ENCS, &|_| None, progress).map_err(|e| {
        let mut c = e.chars();
        c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
    })?;
    let _ = std::fs::write(&sig_file, &sig);
    Ok(out)
}

/// the file itself on the clipboard (paste into Discord / Explorer)
pub fn clipboard_file(p: &Path) -> Result<(), String> {
    let abs = std::fs::canonicalize(p).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        let s = abs.to_string_lossy().trim_start_matches(r"\\?\").to_string();
        // ECHO_CLIP_OUT=<file>: test hook - writes the path instead of using the clipboard (a test copy on Windows
        // put its files on the real clipboard, QA 11 Nov 2026)
        if let Ok(f) = std::env::var("ECHO_CLIP_OUT") {
            return std::fs::write(f, &s).map_err(|e| e.to_string());
        }
        if win::clipboard_files(&[&s]) { Ok(()) } else { Err("Couldn't copy - another app is holding the clipboard".into()) }
    }
    #[cfg(not(windows))]
    {
        let uri: String = abs.to_string_lossy().bytes().map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        }).collect();
        let data = format!("file://{uri}\r\n");
        // ECHO_CLIP_OUT=<file>: test hook - writes what would go on the clipboard
        if let Ok(f) = std::env::var("ECHO_CLIP_OUT") {
            return std::fs::write(f, data).map_err(|e| e.to_string());
        }
        let run = |prog: &str, args: &[&str]| -> bool {
            use std::io::Write;
            let Ok(mut c) = std::process::Command::new(prog).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else { return false };
            if let Some(mut i) = c.stdin.take() {
                let _ = i.write_all(data.as_bytes());
            }
            c.wait().is_ok_and(|s| s.success())
        };
        if run("wl-copy", &["--type", "text/uri-list"]) || run("xclip", &["-selection", "clipboard", "-t", "text/uri-list"]) {
            Ok(())
        } else {
            Err("Copy failed - install wl-clipboard (sudo dnf install wl-clipboard)".into())
        }
    }
}

#[cfg(windows)]
mod win {
    //! Echo's winutil::clipboard_files without the windows crate: user32 / kernel32 at run time. ⚠ untested
    use std::ffi::c_void;
    type Hwnd = isize;

    pub fn clipboard_files(paths: &[&str]) -> bool {
        // DROPFILES (20 bytes: list offset, pt, fNC, fWide = 1) + NUL-separated UTF-16 paths + an extra NUL
        let mut d: Vec<u8> = vec![];
        d.extend(20u32.to_le_bytes());
        d.extend(0i32.to_le_bytes());
        d.extend(0i32.to_le_bytes());
        d.extend(0u32.to_le_bytes());
        d.extend(1u32.to_le_bytes());
        for p in paths {
            for c in p.encode_utf16().chain(std::iter::once(0)) {
                d.extend(c.to_le_bytes());
            }
        }
        d.extend(0u16.to_le_bytes());
        unsafe {
            let (Ok(u), Ok(k)) = (libloading::Library::new("user32.dll"), libloading::Library::new("kernel32.dll")) else { return false };
            let (Ok(open), Ok(empty), Ok(set), Ok(close), Ok(reg), Ok(galloc), Ok(glock), Ok(gunlock)) = (
                u.get::<unsafe extern "system" fn(Hwnd) -> i32>(b"OpenClipboard\0"),
                u.get::<unsafe extern "system" fn() -> i32>(b"EmptyClipboard\0"),
                u.get::<unsafe extern "system" fn(u32, *mut c_void) -> *mut c_void>(b"SetClipboardData\0"),
                u.get::<unsafe extern "system" fn() -> i32>(b"CloseClipboard\0"),
                u.get::<unsafe extern "system" fn(*const u16) -> u32>(b"RegisterClipboardFormatW\0"),
                k.get::<unsafe extern "system" fn(u32, usize) -> *mut c_void>(b"GlobalAlloc\0"),
                k.get::<unsafe extern "system" fn(*mut c_void) -> *mut c_void>(b"GlobalLock\0"),
                k.get::<unsafe extern "system" fn(*mut c_void) -> i32>(b"GlobalUnlock\0"),
            ) else { return false };
            let name: Vec<u16> = "Preferred DropEffect".encode_utf16().chain(std::iter::once(0)).collect();
            let effect = reg(name.as_ptr());
            let mut opened = false;
            for _ in 0..10 {
                if open(0) != 0 {
                    opened = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            if !opened {
                return false;
            }
            empty();
            let mut ok = true;
            for (fmt, data) in [(15u32, d), (effect, 1u32.to_le_bytes().to_vec())] {
                let h = galloc(0x0002, data.len()); // GMEM_MOVEABLE
                let p = if h.is_null() { std::ptr::null_mut() } else { glock(h) as *mut u8 };
                if p.is_null() {
                    ok = false;
                    continue;
                }
                std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
                gunlock(h);
                ok &= !set(fmt, h).is_null();
            }
            close();
            ok
        }
    }
}

/// Chat A: the Copy menus' sizes ("20 MB", "1 GB") and the default badge from Echo's settings.
pub fn fill_menu(a: &AppWindow) {
    let c = a.global::<crate::CopyCfg>();
    let st = crate::library::echo_settings().unwrap_or_default(); // r2-threads: read once, not 9 times
    let size_mb = |k: &str| mb_in(&st, k);
    let f = |k: &str| {
        let mb = size_mb(k);
        if mb >= 1000.0 { format!("{} GB", (mb / 1000.0 * 10.0).round() / 10.0) } else { format!("{mb:.0} MB") }
    };
    c.set_def(default_in(&st).into());
    c.set_free(f("free").into());
    c.set_basic(f("basic").into());
    c.set_nitro(f("nitro").into());
    c.set_custom(f("custom").into());
    // gaps-player (row 14): the numbers too, for the "· fits" hints and the Custom box
    c.set_free_mb(size_mb("free") as f32);
    c.set_basic_mb(size_mb("basic") as f32);
    c.set_nitro_mb(size_mb("nitro") as f32);
    c.set_custom_mb(size_mb("custom") as f32);
}
/// Right-click a size = the Copy button's default (Echo's copy_default), once at start.
pub fn wire(a: &AppWindow) {
    fill_menu(a);
    let w = a.as_weak();
    a.global::<crate::CopyCfg>().on_set_default(move |k| {
        let Some(a) = w.upgrade() else { return };
        let mut st = crate::settings::Store::load();
        match st.write(&[("copy_default", serde_json::Value::String(k.to_string()))]) {
            Ok(()) => {
                fill_menu(&a);
                let name = match k.as_str() { "free" => "Discord Free", "basic" => "Nitro Basic", "nitro" => "Nitro", "custom" => "Custom", _ => "Full size" };
                toast(&a, format!("Copy button now copies: {name}"));
            }
            Err(e) => toast(&a, e),
        }
    });
    // the clip's size from its "22.7 MB" / "1.20 GB" text (library::fmt_size), for the "· fits" hints
    a.global::<crate::CopyCfg>().on_mb_of(|t| {
        let mut it = t.split_whitespace();
        let n = it.next().and_then(|x| x.parse::<f32>().ok()).unwrap_or(-1.0);
        match it.next() {
            Some("GB") => n * 1000.0,
            Some("KB") => n / 1000.0,
            _ => n,
        }
    });
    // Echo's Custom box: a whole number of MB, at least 1, saved to discord_custom_mb (Enter = copy, done by the menu)
    let w = a.as_weak();
    a.global::<crate::CopyCfg>().on_set_custom(move |t| {
        let Some(a) = w.upgrade() else { return };
        let v = t.trim().replace(',', ".").parse::<f64>().unwrap_or(0.0).round().max(1.0);
        if (v - size_mb("custom")).abs() < 0.5 {
            return;
        }
        let mut st = crate::settings::Store::load();
        match st.write(&[("discord_custom_mb", serde_json::json!(v))]) {
            Ok(()) => fill_menu(&a),
            Err(e) => toast(&a, e),
        }
    });
}
