//! Chat A (queue item 8): player extras - mouse-wheel zoom toward the mouse up to 8x + drag to pan (mpv video-zoom /
//! video-pan-x/y, reset per clip), bookmarks from Echo's <thumbs>/<stem>.marks.json (yellow triangles on the seek bar).
use std::cell::Cell;

thread_local! {
    /// zoom factor (1..8) and pan (fractions of the zoomed video, mpv's unit)
    static Z: Cell<(f64, f64, f64)> = const { Cell::new((1.0, 0.0, 0.0)) };
}

fn apply(m: &crate::mpv::Mpv, z: f64, px: f64, py: f64) {
    Z.with(|c| c.set((z, px, py)));
    m.set("video-zoom", &format!("{:.4}", z.log2()));
    m.set("video-pan-x", &format!("{px:.4}"));
    m.set("video-pan-y", &format!("{py:.4}"));
}
/// keeps the picture covering the view: pan at most (1 - 1/z) / 2 either way
fn clamp_pan(z: f64, p: f64) -> f64 {
    let lim = (1.0 - 1.0 / z) / 2.0;
    p.clamp(-lim, lim)
}

/// wheel at the mouse (fx, fy = 0..1 of the video area): `dir` = notches (+ in / - out, fractions from the glide
/// steps add up to whole notches: 1.25x per notch). Returns true when zoomed.
pub fn wheel(m: &crate::mpv::Mpv, dir: f32, fx: f32, fy: f32) -> bool {
    let (z0, px0, py0) = Z.with(|c| c.get());
    let z1 = (z0 * 1.25f64.powf(dir as f64)).clamp(1.0, 8.0);
    if (z1 - z0).abs() < 1e-6 {
        return z0 > 1.0;
    }
    // the video point under the mouse stays under the mouse
    let (fx, fy) = (fx as f64 - 0.5, fy as f64 - 0.5);
    let (ux, uy) = (fx / z0 - px0, fy / z0 - py0);
    let (px1, py1) = if z1 <= 1.0 { (0.0, 0.0) } else { (clamp_pan(z1, fx / z1 - ux), clamp_pan(z1, fy / z1 - uy)) };
    apply(m, z1, px1, py1);
    z1 > 1.0
}

/// drag while zoomed: (dx, dy) = mouse movement as fractions of the video area
pub fn pan(m: &crate::mpv::Mpv, dx: f32, dy: f32) {
    let (z, px, py) = Z.with(|c| c.get());
    if z <= 1.0 {
        return;
    }
    apply(m, z, clamp_pan(z, px + dx as f64 / z), clamp_pan(z, py + dy as f64 / z));
}

/// new clip / player closed: back to 1x
pub fn reset(m: Option<&crate::mpv::Mpv>) {
    let was = Z.with(|c| c.replace((1.0, 0.0, 0.0)));
    if let (Some(m), true) = (m, was.0 != 1.0 || was.1 != 0.0 || was.2 != 0.0) {
        apply(m, 1.0, 0.0, 0.0);
    }
}

/// Echo's bookmarks for a clip (seconds), from the bookmark hotkey (rec_core writes them on save)
pub fn marks(path: &std::path::Path, thumbs: &std::path::Path) -> Vec<f32> {
    let Some(stem) = path.file_stem() else { return vec![] };
    std::fs::read_to_string(thumbs.join(format!("{}.marks.json", stem.to_string_lossy())))
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<f64>>(&s).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|t| t as f32)
        .collect()
}

thread_local! {
    /// editor: (clip, its bookmarks in source seconds) + the last output positions sent
    static ED_MARKS: std::cell::RefCell<(std::path::PathBuf, Vec<f32>, Vec<f32>)> = std::cell::RefCell::new(Default::default());
}
/// Editor ruler: the clip's bookmarks mapped through the pieces (source time -> output time; other files / freeze skip).
pub fn ed_marks(g: &crate::Ed, e: &crate::editor::Editor) {
    ED_MARKS.with(|c| {
        let mut c = c.borrow_mut();
        if c.0 != e.clip {
            let thumbs = crate::library::find_clips_dir().join(".thumbs");
            *c = (e.clip.clone(), marks(&e.clip, &thumbs), vec![]);
        }
        let mut out = vec![];
        for &m in &c.1 {
            let m = m as f64;
            for s in &e.p.segs {
                if s.src().is_none() && !s.freeze() && m >= s.start && m < s.end {
                    out.push((s.at + (m - s.start) / s.speed.max(0.05)) as f32);
                }
            }
        }
        if out != c.2 {
            g.set_marks(slint::ModelRc::new(slint::VecModel::from(out.clone())));
            c.2 = out;
        }
    });
}
