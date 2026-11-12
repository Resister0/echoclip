//! 9 Nov: the card hover preview is the REAL clip again, like Echo on Windows (a muted looping <video>
//! after 450 ms, faded in, stopped after 20 s / when Echo isn't in front). The native app showed the 60-picture
//! flipbook (Echo's Linux / weak-PC fallback) - "a picture frame scrolling through pictures, too fast".
//! ffmpeg decodes the clip in real time (-re, one software thread - see start()), scaled + cropped to the hovered
//! picture's real screen size (size_for), 30 fps, no sound; the frames go straight into HoverVid.frame.
//! A frame the UI hasn't shown yet is dropped instead of queued (never falls behind, never piles up memory).
//! 11 Nov 2026: plays only while Echo's window is in front (may_play), checked at start and on every frame.
use crate::{AppWindow, HoverVid};
use slint::ComponentHandle;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// fallback size when the picture's size isn't known (HoverVid.want-w not set yet)
pub const W: u32 = 640;
pub const H: u32 = 360;

static GEN: AtomicU64 = AtomicU64::new(0);
static CHILD: Mutex<Option<Child>> = Mutex::new(None);

/// 11 Nov 2026 ("a higher quality hover preview"): the preview is made at the hovered picture's REAL size in screen
/// pixels (Thumb sets HoverVid.want-w / want-h; Echo's UI size and the system scale included), so it is drawn 1:1.
/// It was always 640 x 360, then scaled again by the GPU into a 200..780 px card = a second, blurry resample.
/// At most 960 px wide (Home's big clip): the GPU scales it up when it is drawn (the still under it stays sharp at
/// full size). ffmpeg at 1152..1920 wide cost 35-54 % of one core.
pub const HERO_MAX_W: u32 = 960;
pub fn size_for(a: &AppWindow, row: i32) -> (u32, u32) {
    let hv = a.global::<HoverVid>();
    let sf = a.window().scale_factor();
    let (lw, lh) = (hv.get_want_w(), hv.get_want_h());
    let (mut w, mut h) = if lw >= 16.0 && lh >= 9.0 {
        (lw * sf, lh * sf)
    } else if row as usize == crate::library::HOME_BASE {
        let w = a.global::<crate::HomeState>().get_hero_w() * sf;
        (w, w * 9.0 / 16.0)
    } else {
        return (W, H);
    };
    if w > HERO_MAX_W as f32 {
        h *= HERO_MAX_W as f32 / w;
        w = HERO_MAX_W as f32;
    }
    // even sizes (ffmpeg's crop), never tiny
    let even = |x: f32, min: u32| ((x / 2.0).round() as u32 * 2).max(min);
    (even(w, 64), even(h, 36))
}

/// 11 Nov 2026 (William: no preview "if you're hovering above it while tabbed out", especially on Linux, where a
/// window that isn't focused still gets hover events): a preview plays only while Echo's window is in front - the
/// Focused event (WIN_FOCUSED) AND winit's own focus state (a window that never got focus, e.g. shown from the tray
/// without activation on KDE Wayland, never sent Focused(false)), visible and not minimised.
/// ECHO_HOVER_ANYWAY=1: test hook, skips the focus part only (a test copy started from a script doesn't get focus).
fn may_play(a: &AppWindow) -> bool {
    use slint::winit_030::WinitWindowAccessor;
    static ANYWAY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let anyway = *ANYWAY.get_or_init(|| std::env::var_os("ECHO_HOVER_ANYWAY").is_some());
    a.window().is_visible()
        && (anyway || crate::WIN_FOCUSED.with(|f| f.get()))
        && a.window().with_winit_window(|w| (anyway || w.has_focus()) && w.is_minimized() != Some(true)).unwrap_or(false)
}

/// play `clip` on card `row` (a view row); replaces any running preview. `(w, h)` = size_for().
pub fn start(a: &AppWindow, row: i32, clip: PathBuf, (w_px, h_px): (u32, u32)) {
    stop(a);
    if !may_play(a) {
        return;
    }
    let Some(ff) = crate::sheets::ffmpeg() else { return };
    let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let mut c = crate::edrender::hidden_cmd(&ff);
    // -threads 1, NO hardware decoding: measured on the developer's PC (1080p clip, 4 s) - hwaccel auto 406 MB private /
    // d3d11va 319 MB (GPU frame buffers mapped in), default threads 149 MB, -threads 1 = 49 MB at ~15 % of one core
    // 11 Nov 2026: deblocking is ON again (was -skip_loop_filter all): skipping it leaves blocks that pile up until the
    // next keyframe - the main reason the preview looked "low quality". Measured on a GPU-encoded 1080p60 20 Mbps
    // clip, one core: old (640 px, no deblocking, bilinear) 22 %, now (540 px card, deblocking, bicubic) 27 %;
    // picture vs a full-quality reference (SSIM) 0.895 -> 0.998.
    c.args(["-hide_banner", "-loglevel", "error", "-threads", "1", "-stream_loop", "-1", "-re", "-i"]).arg(&clip);
    c.args([
        "-an",
        "-sn",
        "-vf",
        // bicubic at the card's size costs ~2 % of a core more than bilinear and is sharper; the size is capped at
        // 960 px (size_for) - bicubic at 1728 x 972 measured 84 % of one core (11 Nov 2026)
        &format!("fps=30,scale={w_px}:{h_px}:force_original_aspect_ratio=increase:flags=bicubic,crop={w_px}:{h_px}"),
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
                        // Echo went behind another window / to the tray / minimised without a focus event: stop
                        if !may_play(&a) {
                            stop(&a);
                            return;
                        }
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
