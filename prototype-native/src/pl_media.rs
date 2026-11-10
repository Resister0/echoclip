//! Chat C: things the editor (and later the player) needs from the outside world - a file picker, a file's length,
//! and the editor's extra sound files played next to the video (each one its own small audio-only mpv).
//! No Tauri, no extra crates: Windows' file dialog comes from comdlg32.dll through libloading.
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const SOUND_EXT: &[&str] = &["wav", "mp3", "ogg", "flac", "m4a", "aac", "opus", "wma"];
pub const VIDEO_EXT: &[&str] = &["mp4", "mov", "mkv", "webm", "avi", "m4v"];
pub const PICTURE_EXT: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];

pub fn is_sound(p: &Path) -> bool {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).is_some_and(|e| SOUND_EXT.contains(&e.as_str()))
}

/// Opens the system's "open file" dialog (blocking - call it on a worker thread). ECHO_PICK=<path> = test hook
/// (the cloud has no dialog): returns that path without asking.
pub fn pick_file(title: &str, filter_name: &str, exts: &[&str]) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ECHO_PICK") {
        return (!p.is_empty()).then(|| PathBuf::from(p));
    }
    #[cfg(windows)]
    {
        win::pick(title, filter_name, exts)
    }
    #[cfg(not(windows))]
    {
        // KDE's dialog, else GNOME's, else the desktop's FileChooser portal (always the portal in the Flatpak)
        let pat = exts.iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join(";");
        crate::linux::pick(title, Some((filter_name, &pat)), false)
    }
}

#[cfg(windows)]
mod win {
    //! GetOpenFileNameW, loaded at run time (no windows-sys feature needed). ⚠ untested: written in the cloud.
    use std::ffi::c_void;
    use std::path::PathBuf;

    #[repr(C)]
    struct OpenFileNameW {
        l_struct_size: u32,
        hwnd_owner: isize,
        h_instance: isize,
        lpstr_filter: *const u16,
        lpstr_custom_filter: *mut u16,
        n_max_cust_filter: u32,
        n_filter_index: u32,
        lpstr_file: *mut u16,
        n_max_file: u32,
        lpstr_file_title: *mut u16,
        n_max_file_title: u32,
        lpstr_initial_dir: *const u16,
        lpstr_title: *const u16,
        flags: u32,
        n_file_offset: u16,
        n_file_extension: u16,
        lpstr_def_ext: *const u16,
        l_cust_data: isize,
        lpfn_hook: *const c_void,
        lp_template_name: *const u16,
        pv_reserved: *mut c_void,
        dw_reserved: u32,
        flags_ex: u32,
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn pick(title: &str, filter_name: &str, exts: &[&str]) -> Option<PathBuf> {
        unsafe {
            // the dialog wants COM on its thread (single-threaded apartment)
            if let Ok(ole) = libloading::Library::new("ole32.dll") {
                if let Ok(init) = ole.get::<unsafe extern "system" fn(*mut c_void, u32) -> i32>(b"CoInitializeEx\0") {
                    init(std::ptr::null_mut(), 0x2);
                }
                std::mem::forget(ole);
            }
            let lib = libloading::Library::new("comdlg32.dll").ok()?;
            let f: libloading::Symbol<unsafe extern "system" fn(*mut OpenFileNameW) -> i32> = lib.get(b"GetOpenFileNameW\0").ok()?;
            // "Name\0*.mp4;*.mov\0All files\0*.*\0\0"
            let pat = exts.iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join(";");
            let mut filter: Vec<u16> = format!("{filter_name}\0{pat}\0All files\0*.*\0").encode_utf16().collect();
            filter.push(0);
            let title_w = wide(title);
            let mut buf = vec![0u16; 4096];
            let mut o: OpenFileNameW = std::mem::zeroed();
            o.l_struct_size = std::mem::size_of::<OpenFileNameW>() as u32;
            o.lpstr_filter = filter.as_ptr();
            o.n_filter_index = 1;
            o.lpstr_file = buf.as_mut_ptr();
            o.n_max_file = buf.len() as u32;
            o.lpstr_title = title_w.as_ptr();
            // OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR
            o.flags = 0x0008_0000 | 0x0000_1000 | 0x0000_0800 | 0x0000_0008;
            if f(&mut o) == 0 {
                return None;
            }
            let n = buf.iter().position(|c| *c == 0).unwrap_or(0);
            (n > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..n])))
        }
    }
}

/// length of a media file in seconds (ffprobe), None = unreadable
pub fn duration(ff: &Path, p: &Path) -> Option<f64> {
    let o = crate::edrender::hidden_cmd(&crate::edrender::ffprobe(ff))
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(p)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok().filter(|d| *d > 0.05)
}

// ---------- extra sound files in the editor preview ----------
thread_local! {
    /// one audio-only mpv per sound clip on the timeline (keyed by its index + file)
    static SOUNDS: RefCell<HashMap<(usize, PathBuf), crate::mpv::Mpv>> = RefCell::new(HashMap::new());
}

/// keys of the layers' sound players (Chat C layers): LAYER_KEY + the layer's index
const LAYER_KEY: usize = 1_000_000;

/// called ~60x a second by the editor: sound clips from other files play while the playhead is over them, and so
/// does the sound of every layer with "Include its sound" (the main mpv plays only the main track)
pub fn sync_sounds(e: &crate::editor::Editor) {
    let mut wanted: Vec<(usize, PathBuf, f64, f64, f64)> = e.p.audio.iter().enumerate().filter_map(|(i, a)| {
        let src = a.extra.get("src").and_then(|x| x.as_str()).filter(|x| !x.is_empty())?;
        if a.mute || e.p.mute || !e.playing || e.t < a.at || e.t >= a.at + a.len() {
            return None;
        }
        let pos = a.start + (e.t - a.at) * a.speed;
        let g = crate::edrender::fade_gain(a.at, a.len(), a.fi, a.fo, e.t);
        Some((i, PathBuf::from(src), pos, (a.vol * e.p.volume * g * 100.0).clamp(0.0, 400.0), a.speed))
    }).collect();
    wanted.extend(e.p.over.iter().enumerate().filter_map(|(i, o)| {
        if !o.sound || e.p.mute || !e.playing || e.t < o.at || e.t >= o.at + o.len() {
            return None;
        }
        let f = o.src().map(PathBuf::from).unwrap_or_else(|| e.clip.clone());
        Some((LAYER_KEY + i, f, o.start + (e.t - o.at) * o.speed, (e.p.volume * 100.0).clamp(0.0, 400.0), o.speed))
    }));
    // the file a key plays (None = that clip / layer is gone)
    let file_of = |i: usize| -> Option<PathBuf> {
        if i >= LAYER_KEY {
            let o = e.p.over.get(i - LAYER_KEY).filter(|o| o.sound)?;
            Some(o.src().map(PathBuf::from).unwrap_or_else(|| e.clip.clone()))
        } else {
            e.p.audio.get(i).and_then(|a| a.extra.get("src")).and_then(|x| x.as_str()).map(PathBuf::from)
        }
    };
    SOUNDS.with(|s| {
        let mut s = s.borrow_mut();
        // the ones not wanted now: paused (kept a while for quick restarts), forgotten when their clip is gone
        s.retain(|(i, p), m| {
            let keep = file_of(*i).as_ref() == Some(p);
            if keep && !wanted.iter().any(|w| w.0 == *i) {
                m.set("pause", "yes");
            }
            keep
        });
        for (i, p, pos, vol, sp) in wanted {
            let key = (i, p.clone());
            if !s.contains_key(&key) {
                let Ok(m) = crate::mpv::Mpv::new("no", None) else { continue };
                m.set("vid", "no");
                m.set("volume-max", "400");
                m.command(&["loadfile", &p.to_string_lossy(), "replace", "-1", &format!("start={pos:.3},pause=no")]);
                s.insert(key.clone(), m);
            }
            let m = &s[&key];
            m.drain_events();
            m.set("volume", &format!("{vol:.0}"));
            m.set("speed", &format!("{sp:.3}"));
            let cur = m.get_f64("time-pos");
            if cur.is_some_and(|c| (c - pos).abs() > 0.25) {
                m.command(&["seek", &format!("{pos:.3}"), "absolute+exact"]);
            }
            if m.get("pause").as_deref() != Some("no") {
                m.set("pause", "no");
            }
        }
    });
}

/// editor closed: the sound players go
pub fn stop_sounds() {
    SOUNDS.with(|s| s.borrow_mut().clear());
}
