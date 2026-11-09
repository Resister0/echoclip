//! Chat C: the editor's video layers (Echo's ed.over, PiP) in the preview. mpv plays only the main track, so each
//! layer's span is decoded once by ffmpeg into a small picture strip on disk (384..640 px wide, 15 pictures a second,
//! raw RGBA in %TEMP%\echo-proto-layers) and the picture for the playhead is handed to mpv as a bitmap overlay
//! (`overlay-add` ids 3..9, under the stickers). Low-res on purpose: it's a preview - the export uses the real video.
use crate::editor::{Editor, Ovl};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const FPS: f64 = 15.0;
const PW: u32 = 384;
const FIRST_ID: usize = 3;
const MAX_IDS: usize = 7;

#[derive(Clone)]
struct Strip {
    file: PathBuf,
    w: u32,
    h: u32,
    frames: usize,
}

thread_local! {
    /// (file, start, end) -> strip (None = being made / failed)
    static STRIPS: RefCell<HashMap<String, Option<Strip>>> = RefCell::new(HashMap::new());
    static SHOWN: RefCell<HashMap<usize, (String, Vec<u8>)>> = RefCell::new(HashMap::new());
}

fn key(file: &Path, o: &Ovl) -> String {
    format!("{}|{:.2}|{:.2}", file.display(), o.start, o.end)
}

fn dir() -> PathBuf {
    std::env::temp_dir().join("echo-proto-layers")
}

/// the layer's picture strip, made in the background the first time it's asked for
fn strip(file: &Path, o: &Ovl) -> Option<Strip> {
    let k = key(file, o);
    if let Some(v) = STRIPS.with(|s| s.borrow().get(&k).cloned()) {
        return v;
    }
    STRIPS.with(|s| s.borrow_mut().insert(k.clone(), None));
    let ff = crate::sheets::ffmpeg()?;
    let (file, start, len) = (file.to_path_buf(), o.start, (o.end - o.start).max(0.05));
    std::thread::spawn(move || {
        let (w0, h0, _, _) = crate::edrender::probe(&ff, &file);
        let (w0, h0) = (w0.max(16), h0.max(16));
        // sharper for short layers: up to 640 px wide while the strip stays under ~300 MB, never under 384 px
        let frames = (len * FPS).ceil().max(1.0);
        let pw = ((300e6 / (frames * 4.0 * h0 as f64 / w0 as f64)).sqrt() as u32).clamp(PW, 640) / 2 * 2;
        let (w, h) = (pw, ((pw as f64 * h0 as f64 / w0 as f64 / 2.0).round() as u32 * 2).max(2));
        let _ = std::fs::create_dir_all(dir());
        let mut hsh: u64 = 0xcbf29ce484222325;
        for b in k.bytes() {
            hsh = (hsh ^ b as u64).wrapping_mul(0x100000001b3);
        }
        let out = dir().join(format!("{hsh:016x}.rgba"));
        let ok = crate::edrender::hidden_cmd(&ff)
            .args(["-v", "error", "-y", "-ss", &format!("{start:.3}"), "-t", &format!("{len:.3}"), "-i"])
            .arg(&file)
            .args(["-an", "-vf", &format!("fps={FPS},scale={w}:{h}:flags=bilinear,format=rgba"), "-f", "rawvideo"])
            .arg(&out)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        let frames = std::fs::metadata(&out).map(|m| m.len() as usize / (w * h * 4) as usize).unwrap_or(0);
        let r = (ok && frames > 0).then_some(Strip { file: out, w, h, frames });
        let _ = slint::invoke_from_event_loop(move || {
            STRIPS.with(|s| s.borrow_mut().insert(k, r));
            // a paused editor draws no frames by itself: one more so the layer shows up
            crate::UI.with(|u| {
                if let Some(a) = u.borrow().as_ref().and_then(|w| w.upgrade()) {
                    slint::ComponentHandle::window(&a).request_redraw();
                }
            });
        });
    });
    None
}

/// width / height of a layer's picture (the clip's shape until its strip is known)
pub fn aspect(e: &Editor, o: &Ovl) -> f64 {
    let file = o.src().map(PathBuf::from).unwrap_or_else(|| e.clip.clone());
    STRIPS.with(|s| s.borrow().get(&key(&file, o)).cloned().flatten()).map(|s| s.w as f64 / s.h.max(1) as f64)
        .unwrap_or(if e.h > 0 { e.w as f64 / e.h as f64 } else { 16.0 / 9.0 })
}

fn read_frame(s: &Strip, k: usize) -> Option<Vec<u8>> {
    let sz = (s.w * s.h * 4) as usize;
    let mut f = std::fs::File::open(&s.file).ok()?;
    f.seek(SeekFrom::Start((k.min(s.frames - 1) * sz) as u64)).ok()?;
    let mut b = vec![0u8; sz];
    f.read_exact(&mut b).ok()?;
    Some(b)
}

/// every frame the editor is open: the layers showing at the playhead go to mpv (higher rows on top)
pub fn overlay(e: &Editor, m: &crate::mpv::Mpv) {
    let mut want: HashMap<usize, (String, i32, i32, u32, u32, Strip, usize)> = HashMap::new();
    let mut order: Vec<usize> = (0..e.p.over.len()).collect();
    order.sort_by_key(|&i| e.p.over[i].row);
    let mut n = 0;
    for i in order {
        let o = &e.p.over[i];
        if e.t < o.at || e.t >= o.at + o.len() || n >= MAX_IDS {
            continue;
        }
        let file = o.src().map(PathBuf::from).unwrap_or_else(|| e.clip.clone());
        let Some(st) = strip(&file, o) else { continue };
        let (x, y, w, h) = e.layer_rect(o);
        let (w, h) = (w.round().max(2.0) as u32, h.round().max(2.0) as u32);
        if w > 8192 || h > 8192 {
            continue;
        }
        let fi = (((e.t - o.at) * o.speed * FPS) as usize).min(st.frames - 1);
        let k = format!("{}|{fi}|{:.0},{:.0},{w}x{h}", st.file.display(), x, y);
        want.insert(FIRST_ID + n, (k, x.round() as i32, y.round() as i32, w, h, st, fi));
        n += 1;
    }
    SHOWN.with(|s| {
        let mut s = s.borrow_mut();
        let gone: Vec<usize> = s.keys().filter(|k| !want.contains_key(k)).copied().collect();
        for id in gone {
            m.command(&["overlay-remove", &id.to_string()]);
            s.remove(&id);
        }
        for (id, (k, x, y, w, h, st, fi)) in want {
            if s.get(&id).is_some_and(|v| v.0 == k) {
                continue;
            }
            let Some(raw) = read_frame(&st, fi) else { continue };
            let Some(src) = tiny_skia::Pixmap::from_vec(raw, tiny_skia::IntSize::from_wh(st.w, st.h).unwrap()) else { continue };
            let (cx, cy) = ((-x).max(0) as u32, (-y).max(0) as u32);
            if cx >= w || cy >= h {
                continue;
            }
            let (vw, vh) = (w - cx, h - cy);
            let Some(mut pm) = tiny_skia::Pixmap::new(vw, vh) else { continue };
            let tr = tiny_skia::Transform::from_scale(w as f32 / st.w as f32, h as f32 / st.h as f32).post_translate(-(cx as f32), -(cy as f32));
            pm.draw_pixmap(0, 0, src.as_ref(), &tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() }, tr, None);
            let mut buf = pm.take();
            for p in buf.chunks_exact_mut(4) {
                p.swap(0, 2);
            }
            let addr = buf.as_ptr() as usize;
            m.command(&["overlay-add", &id.to_string(), &x.max(0).to_string(), &y.max(0).to_string(), &format!("&{addr}"), "0", "bgra", &vw.to_string(), &vh.to_string(), &(vw * 4).to_string()]);
            s.insert(id, (k, buf));
        }
    });
}

/// editor closed: layer pictures off the video, their strips deleted
pub fn clear(m: Option<&crate::mpv::Mpv>) {
    SHOWN.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(m) = m {
            for id in s.keys() {
                m.command(&["overlay-remove", &id.to_string()]);
            }
        }
        s.clear();
    });
    STRIPS.with(|s| s.borrow_mut().clear());
    // the whole folder: strips left behind by an app that was killed go too
    let _ = std::fs::remove_dir_all(dir());
}
