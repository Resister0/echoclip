//! 9 Nov: the card hover preview is the REAL clip again, like Echo on Windows (a muted looping <video>
//! after 450 ms, faded in, stopped after 20 s / when Echo isn't in front). The native app showed the 60-picture
//! flipbook (Echo's Linux / weak-PC fallback) - "a picture frame scrolling through pictures, too fast".
//! ffmpeg decodes the clip in real time (-re, one software thread - see start()), scaled + cropped to 640 x 360 (the
//! card's 16:9, sharper than the card on a 1440p screen), 30 fps, no sound; the frames go straight into HoverVid.frame.
//! A frame the UI hasn't shown yet is dropped instead of queued (never falls behind, never piles up memory).
use crate::{AppWindow, HoverVid};
use slint::ComponentHandle;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub const W: u32 = 640;
pub const H: u32 = 360;

static GEN: AtomicU64 = AtomicU64::new(0);
static CHILD: Mutex<Option<Child>> = Mutex::new(None);

/// 11 Nov 2026: how big the preview is made. Cards: 640 x 360. Home's big clip (row HOME_BASE): its size in screen
/// pixels, but at most 960 x 540 - the GPU scales it up when it is drawn (the still under it stays sharp at full
/// size). ffmpeg at 1152..1920 wide cost 35-54 % of one core; capped at 960 (+ no deblocking, see start()) ~19 %,
/// the same as a card. Only while the mouse rests on it (max 20 s) and Echo is in front.
pub const HERO_MAX_W: u32 = 960;
pub fn size_for(a: &AppWindow, row: i32) -> (u32, u32) {
    if row as usize != crate::library::HOME_BASE {
        return (W, H);
    }
    let px = a.global::<crate::HomeState>().get_hero_w() * a.window().scale_factor();
    let w = ((px / 16.0).round() as u32 * 16).clamp(W, HERO_MAX_W);
    (w, (w * 9 / 16) & !1)
}

/// play `clip` on card `row` (a view row); replaces any running preview. `(w, h)` = size_for().
pub fn start(a: &AppWindow, row: i32, clip: PathBuf, (w_px, h_px): (u32, u32)) {
    stop(a);
    let Some(ff) = crate::sheets::ffmpeg() else { return };
    let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let mut c = crate::edrender::hidden_cmd(&ff);
    // -threads 1, NO hardware decoding: measured on the developer's PC (1080p clip, 4 s) - hwaccel auto 406 MB private /
    // d3d11va 319 MB (GPU frame buffers mapped in), default threads 149 MB, -threads 1 = 49 MB at ~15 % of one core
    // 11 Nov 2026: -skip_loop_filter all = no deblocking (not visible at 640 / 960 px, checked zoomed in): measured on
    // a 1080p60 clip, CPU time / clip length: 960 x 540 33 % -> 19 %, 640 x 360 23 % -> 19 % of one core
    c.args(["-hide_banner", "-loglevel", "error", "-threads", "1", "-skip_loop_filter", "all", "-stream_loop", "-1", "-re", "-i"]).arg(&clip);
    c.args([
        "-an",
        "-sn",
        "-vf",
        // bilinear also for the big hero size: bicubic at 1728 x 972 measured 84 % of one core, bilinear 39 % (11 Nov 2026)
        &format!("fps=30,scale={w_px}:{h_px}:force_original_aspect_ratio=increase:flags=bilinear,crop={w_px}:{h_px}"),
        "-pix_fmt",
        "rgb24",
        "-f",
        "rawvideo",
        "pipe:1",
    ]);
    let Ok(mut child) = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else { return };
    let Some(mut out) = child.stdout.take() else {
        let _ = child.kill();
        return;
    };
    *CHILD.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
    let w = a.as_weak();
    let busy = Arc::new(AtomicBool::new(false));
    std::thread::Builder::new()
        .name("hover-video".into())
        .spawn(move || {
            let mut buf = vec![0u8; (w_px * h_px * 3) as usize];
            while GEN.load(Ordering::SeqCst) == gen && out.read_exact(&mut buf).is_ok() {
                // the UI hasn't drawn the last frame yet: skip this one
                if busy.swap(true, Ordering::SeqCst) {
                    continue;
                }
                let px = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(&buf, w_px, h_px);
                let b = busy.clone();
                let _ = w.upgrade_in_event_loop(move |a| {
                    b.store(false, Ordering::SeqCst);
                    if GEN.load(Ordering::SeqCst) == gen {
                        let h = a.global::<HoverVid>();
                        h.set_frame(slint::Image::from_rgb8(px));
                        h.set_row(row);
                    }
                });
            }
        })
        .ok();
}

/// stop the preview (mouse left, 20 s, Echo lost focus, the list changed)
pub fn stop(a: &AppWindow) {
    GEN.fetch_add(1, Ordering::SeqCst);
    if let Some(mut c) = CHILD.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = c.kill();
        // reap it off the UI thread
        std::thread::spawn(move || {
            let _ = c.wait();
        });
    }
    let h = a.global::<HoverVid>();
    if h.get_row() != -1 {
        h.set_row(-1);
        h.set_frame(slint::Image::default());
    }
}
