//! Editor wiring: opening / closing, the Ed global (models + inspector), mouse + keys, the 60 Hz playback tick,
//! captions and export jobs. The edit itself lives in editor.rs / edrender.rs.
use crate::editor::{fmt_t, Editor, Proj, Sel, LP};
use crate::{confirm_box, library, log_to, prog_box, prog_done, prog_set, sheets, toast, AppWindow, Ed, EdAud, EdCap, EdMenu, EdSeg, EdTick, EdTxt, Play, Player, LIB};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

thread_local! {
    pub static ED: RefCell<Option<Editor>> = const { RefCell::new(None) };
    static OV_SHOWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static MODELS: (Rc<VecModel<EdSeg>>, Rc<VecModel<EdTxt>>, Rc<VecModel<EdTick>>) = (Rc::new(VecModel::default()), Rc::new(VecModel::default()), Rc::new(VecModel::default()));
    static AUDS: Rc<VecModel<EdAud>> = Rc::new(VecModel::default());
    /// video layers (Chat C, PiP): EdSeg rows with `row` = rows from the top of the Layers track
    static LAYS: Rc<VecModel<EdSeg>> = Rc::new(VecModel::default());
    static MENU: Rc<VecModel<EdMenu>> = Rc::new(VecModel::default());
    /// the edit before a slider / text-box burst started (one undo step per burst)
    static LIVE: RefCell<Option<(Proj, Instant)>> = const { RefCell::new(None) };
    static PL: RefCell<Option<Rc<RefCell<Player>>>> = const { RefCell::new(None) };
    static TICK_KEY: std::cell::Cell<Option<(u64, i64)>> = const { std::cell::Cell::new(None) };
    static PLAY: RefCell<Option<Rc<dyn Fn(i32)>>> = const { RefCell::new(None) };
    /// the captions list (Echo's #caplist)
    static CAPS: Rc<VecModel<EdCap>> = Rc::new(VecModel::default());
    /// the next push only updates the captions list's rows in place (typing / wheel in a row: a rebuild would take
    /// the keyboard away)
    static CAPS_SOFT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// a caption's words when typing in its row started (texts index, start time, words): same word count = same
    /// timings, even after a word was half retyped
    static CAP_BASE: RefCell<Option<(usize, f64, Vec<crate::editor::Word>)>> = const { RefCell::new(None) };
    /// r2-threads (L30): the editor is opening (ffprobe on a worker)
    static OPENING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// r2-threads (L30): the player's trim + cuts waiting for the editor to finish opening
    static CARRY: RefCell<Option<(PathBuf, crate::pl_cuts::Carry)>> = const { RefCell::new(None) };
    /// last wheel nudge on a caption time (one undo step per wheel burst)
    static NUDGE_AT: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
    /// "Part highlighted - X cuts it ..." once per session (Echo's rngHinted)
    static RNG_HINTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// an action to run once the editor is open (Home "Add captions"), with when it was asked
    static PENDING: RefCell<Option<(String, Instant)>> = const { RefCell::new(None) };
    /// the clips folder (projects + card pictures go to its .thumbs, like Echo's save_project)
    static ROOT: RefCell<PathBuf> = RefCell::new(PathBuf::new());
    /// build_tiles: the newest request (an older one finishing later is dropped)
    static TILE_GEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// 11 Nov 2026 (choppy timeline scrolling): the filmstrip's 60 pictures as small ready-made images, cut once from the
/// player's 10x6 picture sheet. The timeline used to draw cut-outs of the whole sheet (6400x2160): every scroll step
/// showed new cut-outs, each one a new picture for the renderer - 56-76 ms per frame while scrolling a 10-minute clip
/// (8 ms standing still, 7-9 ms with the filmstrip hidden). Measured with ECHO_TL_BENCH (ed_bench.rs).
fn build_tiles(a: &AppWindow) {
    let pl = a.global::<Play>();
    if !pl.get_sheet_ok() || !a.global::<Ed>().get_open() {
        return;
    }
    let (tw, th) = (pl.get_sheet_tw().max(1) as u32, pl.get_sheet_th().max(1) as u32);
    let sheet = pl.get_sheet();
    // the pixels go to a worker as they are (no copy here); RGBA sheets are turned into RGB there
    let src: Option<Result<slint::SharedPixelBuffer<slint::Rgb8Pixel>, slint::SharedPixelBuffer<slint::Rgba8Pixel>>> =
        sheet.to_rgb8().map(Ok).or_else(|| sheet.to_rgba8().map(Err));
    let Some(src) = src else { return };
    let gen = TILE_GEN.with(|g| {
        g.set(g.get() + 1);
        g.get()
    });
    let w = a.as_weak();
    std::thread::spawn(move || {
        let sheet = match src {
            Ok(b) => image::RgbImage::from_raw(b.width(), b.height(), b.as_bytes().to_vec()),
            Err(b) => image::RgbaImage::from_raw(b.width(), b.height(), b.as_bytes().to_vec()).map(|i| image::DynamicImage::ImageRgba8(i).to_rgb8()),
        };
        let Some(sheet) = sheet else { return };
        // 144 px tall (the strip is 64 px: sharp up to 225 %), the sheet's own shape
        let (oh, ow) = (144u32, ((144 * tw) as f32 / th as f32).round().max(1.0) as u32);
        let mut tiles = Vec::with_capacity(60);
        for f in 0..60u32 {
            let (x, y) = ((f % 10) * tw, (f / 10) * th);
            if x + tw > sheet.width() || y + th > sheet.height() {
                break;
            }
            let t = image::imageops::crop_imm(&sheet, x, y, tw, th).to_image();
            let t = image::imageops::resize(&t, ow, oh, image::imageops::FilterType::Triangle);
            tiles.push(slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(t.as_raw(), ow, oh));
        }
        let _ = slint::invoke_from_event_loop(move || {
            let Some(a) = w.upgrade() else { return };
            if TILE_GEN.with(|g| g.get()) != gen || !a.global::<Ed>().get_open() {
                return;
            }
            let imgs: Vec<slint::Image> = tiles.into_iter().map(slint::Image::from_rgb8).collect();
            a.global::<Ed>().set_tiles(ModelRc::new(VecModel::from(imgs)));
        });
    });
}

fn elog(line: String) {
    log_to(&mut Vec::new(), line);
}

/// where projects are saved: next to the clip cache, <clips>/.thumbs/<name>.project.json (Echo's save_project), so
/// either app finds them and they live as long as the clips
fn proj_dir() -> PathBuf {
    ROOT.with(|r| r.borrow().join(".thumbs"))
}

/// before 10 Nov 2026 the prototype saved projects in %TEMP% - still read from there
fn old_proj_dir() -> PathBuf {
    std::env::temp_dir().join("echo-proto-projects")
}

fn find_project(clip: &Path) -> Option<serde_json::Value> {
    let echo = clip.ancestors().map(|d| d.join(".thumbs")).find(|d| d.is_dir()).map(|d| crate::editor::project_path(&d, clip));
    [Some(crate::editor::project_path(&proj_dir(), clip)), echo, Some(crate::editor::project_path(&old_proj_dir(), clip))]
        .into_iter().flatten().find_map(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok())
}

/// Echo's fmtMs: m:ss.mmm
fn fmt_ms(t: f64) -> String {
    let t = t.max(0.0);
    let m = (t / 60.0).floor();
    format!("{}:{:06.3}", m as u64, t - m * 60.0)
}

/// Echo's parseTime: "1:02.345", "62.345" or "62" (a comma works as the dot)
fn parse_time(v: &str) -> Option<f64> {
    let v = v.trim().replace(',', ".");
    let (m, s) = match v.split_once(':') {
        Some((m, s)) => (m.parse::<u64>().ok()?, s),
        None => (0, v.as_str()),
    };
    let s = s.parse::<f64>().ok().filter(|x| *x >= 0.0 && x.is_finite())?;
    Some(m as f64 * 60.0 + s)
}

/// Echo's fmtMb: 20 -> "20 MB", 1000 -> "1 GB"
fn fmt_mb(mb: f64) -> String {
    if mb >= 1000.0 { format!("{} GB", mb / 1000.0) } else { format!("{mb} MB") }
}

/// run an editor action once the editor is open (Home "Add captions": the editor may first ask "Edit the
/// project?", and opens a moment later). Echo waited 700 ms; here it waits for the editor itself.
pub fn act_when_open(a: &AppWindow, cmd: &str) {
    if ED.with(|e| e.borrow().is_some()) {
        run_soon(a, cmd.to_string());
    } else {
        PENDING.with(|p| *p.borrow_mut() = Some((cmd.to_string(), Instant::now())));
    }
}

fn run_soon(a: &AppWindow, cmd: String) {
    let w = a.as_weak();
    slint::Timer::single_shot(Duration::from_millis(250), move || {
        if let Some(a) = w.upgrade() {
            if a.global::<Ed>().get_open() {
                a.global::<Ed>().invoke_act(cmd.into());
            }
        }
    });
}

/// h 0..360, s / v 0..1 -> "rrggbb"
fn hsv_hex(h: f64, s: f64, v: f64) -> String {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 { 0 => (c, x, 0.0), 1 => (x, c, 0.0), 2 => (0.0, c, x), 3 => (0.0, x, c), 4 => (x, 0.0, c), _ => (c, 0.0, x) };
    let m = v - c;
    let k = |q: f64| ((q + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    format!("{:02x}{:02x}{:02x}", k(r), k(g), k(b))
}

/// "rrggbb" -> (h, s, v)
fn hex_hsv(hex: &str) -> Option<(f64, f64, f64)> {
    let h: String = hex.trim().trim_start_matches('#').chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if h.len() != 6 {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map(|x| x as f64 / 255.0).unwrap_or(0.0);
    let (r, g, b) = (p(0), p(2), p(4));
    let mx = r.max(g).max(b);
    let d = mx - r.min(g).min(b);
    let hue = if d < 1e-9 { 0.0 } else if mx == r { 60.0 * ((g - b) / d).rem_euclid(6.0) } else if mx == g { 60.0 * ((b - r) / d + 2.0) } else { 60.0 * ((r - g) / d + 4.0) };
    Some((hue, if mx > 0.0 { d / mx } else { 0.0 }, mx))
}

fn with_ed<R>(f: impl FnOnce(&mut Editor) -> R) -> Option<R> {
    ED.with(|e| e.borrow_mut().as_mut().map(f))
}

fn shape_eq<T: Clone + PartialEq + 'static>(m: &VecModel<T>, v: Vec<T>) {
    // same number of rows: change only the rows that differ (a drag touches one or two - redrawing every row each
    // mouse move made dragging stutter); else rebuild
    if m.row_count() == v.len() {
        for (i, x) in v.into_iter().enumerate() {
            if m.row_data(i).as_ref() != Some(&x) {
                m.set_row_data(i, x);
            }
        }
    } else {
        m.set_vec(v);
    }
}

fn ticks(e: &Editor) -> Vec<EdTick> {
    let steps = [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0];
    let step = *steps.iter().find(|s| *s * e.pps >= 70.0).unwrap_or(&600.0);
    let end = e.total() + 200.0 / e.pps;
    let n = ((end / step) as usize).min(2000);
    (0..=n).map(|k| {
        let t = k as f64 * step;
        let label = if step < 1.0 { format!("{}:{:04.1}", (t / 60.0) as u64, t % 60.0) } else { format!("{}:{:02}", (t / 60.0) as u64, (t % 60.0).round() as u64) };
        EdTick { t: t as f32, label: label.into() }
    }).collect()
}

/// everything in the Ed global from the edit (after any change)
pub fn push(a: &AppWindow, e: &Editor) {
    let g = a.global::<Ed>();
    MODELS.with(|(ms, mt, mk)| {
        let dragging = match &e.drag {
            Some(crate::editor::Drag::Piece { i, mode: 'm', pushed: true, .. }) => Some(*i),
            _ => None,
        };
        shape_eq(ms, e.p.segs.iter().enumerate().map(|(i, s)| {
            let look = match s.look().as_str() { "vivid" => "Vivid", "punch" => "Punchy", "warm" => "Warm", "cool" => "Cool", "cinema" => "Cinematic", "bw" => "B&W", "vintage" => "Vintage", _ => "" };
            let lab: Vec<String> = [if s.speed != 1.0 && !s.freeze() { format!("{}x", s.speed) } else { String::new() }, if s.freeze() { "Freeze".into() } else { String::new() }, if s.rev() && !s.freeze() { "Reverse".into() } else { String::new() }, look.to_string(), if s.extra.get("amute").and_then(|x| x.as_bool()) == Some(true) { "audio separated".into() } else { String::new() }]
                .into_iter().filter(|x| !x.is_empty()).collect();
            EdSeg { at: s.at as f32, len: s.len() as f32, start: s.start as f32, speed: if s.freeze() { 0.0 } else { s.speed as f32 }, sel: e.is_sel(Sel::Seg(i)), fi: s.fi as f32, fo: s.fo as f32,
                label: if let Some(f) = s.src() { let n = Path::new(f).file_name().unwrap_or_default().to_string_lossy().to_string(); if lab.is_empty() { n } else { format!("{n} · {}", lab.join(" · ")) } } else { lab.join(" · ") }.into(),
                drag: dragging == Some(i), other: s.src().is_some(), row: 0, down: false, rev: s.rev() && !s.freeze(), tr: i > 0 && !s.tr().is_empty() }
        }).collect());
        let lay = e.layout();
        // layers: a dragged block below row 1 will drop back onto the main track (faded, Echo)
        let (lay_drag, lay_down) = match &e.drag {
            Some(crate::editor::Drag::Layer { i, mode: 'm', pushed: true, y0, orig, .. }) => (Some(*i), orig.row as f64 - ((e.mouse_y - y0) / crate::editor::OV_H).round() < 1.0),
            _ => (None, false),
        };
        LAYS.with(|m| shape_eq(m, e.p.over.iter().enumerate().map(|(i, o)| {
            let name = o.src().map(|f| Path::new(f).file_name().unwrap_or_default().to_string_lossy().to_string());
            let label = [Some(format!("Layer {}", o.row)), name, (o.speed != 1.0).then(|| format!("{}x", o.speed)), (!o.sound).then(|| "no sound".to_string())]
                .into_iter().flatten().collect::<Vec<_>>().join(" · ");
            EdSeg { at: o.at as f32, len: o.len() as f32, start: o.start as f32, speed: o.speed as f32, sel: e.is_sel(Sel::Ov(i)), fi: 0.0, fo: 0.0, label: label.into(),
                drag: lay_drag == Some(i), other: o.src().is_some(), row: (lay.ov_rows as i32 - o.row as i32).max(0), down: lay_drag == Some(i) && lay_down, rev: false, tr: false }
        }).collect()));
        g.set_ov_rows(lay.ov_rows as i32);
        g.set_video_y(lay.video_y as f32);
        g.set_aud_y(lay.aud_y as f32);
        // a piece being dragged follows the mouse upward (drag up = becomes a layer)
        g.set_drag_dy(match &e.drag {
            Some(crate::editor::Drag::Piece { mode: 'm', pushed: true, y0, .. }) => (e.mouse_y - y0).min(0.0) as f32,
            _ => 0.0,
        });
        let mut rows: Vec<EdTxt> = e.p.texts.iter().enumerate().map(|(i, t)| EdTxt { start: t.start as f32, len: (t.end - t.start) as f32, label: { let l = t.text.split('\n').next().unwrap_or("").to_string(); if l.is_empty() { "Text".into() } else { l.into() } },
            sel: e.is_sel(Sel::Txt(i)), kind: if t.cap { 1 } else { 0 }, lane: lay.lane[i] as i32 }).collect();
        rows.extend(e.p.boxes.iter().enumerate().map(|(i, b)| EdTxt { start: b.start as f32, len: (b.end - b.start) as f32,
            label: match b.kind.as_str() { "image" => "Sticker".into(), "blur" => "Blur".into(), "zoom" => format!("Zoom {:.1}×", (1.0 / b.w.max(b.h)).clamp(1.0, 5.0)).into(), _ => "Box".into() },
            sel: e.is_sel(Sel::Box(i)), kind: 2, lane: 0 }));
        shape_eq(mt, rows);
        // the ruler only changes with zoom / length
        let key = (e.pps.to_bits(), (e.total() * 10.0).round() as i64);
        if TICK_KEY.with(|k| k.replace(Some(key))) != Some(key) {
            shape_eq(mk, ticks(e));
        }
        g.set_lanes(lay.lanes as i32);
        g.set_aud_rows(lay.aud_rows.len() as i32);
        g.set_text_y(lay.text_y as f32);
        AUDS.with(|m| shape_eq(m, e.p.audio.iter().enumerate().map(|(i, a)| EdAud {
            at: a.at as f32, len: a.len() as f32, label: format!("{}{} · {}%", if a.label.is_empty() { "Audio" } else { a.label.as_str() }, if a.mute { " (muted)" } else { "" }, (a.vol * 100.0).round()).into(),
            sel: e.is_sel(Sel::Aud(i)), row: lay.aud_rows.iter().position(|r| *r == a.track).unwrap_or(0) as i32, mute: a.mute, mic: a.track == 2,
            fi: a.fi as f32, fo: a.fo as f32,
        }).collect()));
        g.set_fx_y(lay.fx.map(|f| f.0 as f32).unwrap_or(-1.0));
        g.set_tl_content(lay.h as f32);
    });
    crate::pl_zoom::ed_marks(&g, e); // Chat A: bookmarks on the ruler
    g.set_total(e.total() as f32);
    g.set_pps(e.pps as f32);
    g.set_scroll(e.scroll as f32);
    g.set_can_undo(!e.hist.is_empty());
    g.set_can_redo(!e.fut.is_empty());
    g.set_snap(e.snap);
    g.set_vertical(e.p.vertical.is_some());
    g.set_has_caps(e.p.texts.iter().any(|t| t.cap));
    g.set_normalize(e.normalize);
    g.set_cap_style(e.cap_style.clone().into());
    g.set_cap_hl(e.cap_hl);
    g.set_cap_hl_color(e.cap_hl_color.clone().into());
    g.set_cap_lines(e.p.cap_lines as i32);
    g.set_cap_w(e.p.cap_w as f32);
    // the captions panel (Echo's capPanel): list, size, box, this caption
    {
        let idx = e.cap_idx();
        g.set_cap_count(idx.len() as i32);
        let first = idx.first().map(|&i| &e.p.texts[i]);
        g.set_cap_size(first.map(|t| t.size).unwrap_or(0.065) as f32);
        g.set_cap_show_box(e.cap_show_box);
        let rows: Vec<EdCap> = idx.iter().map(|&i| {
            let t = &e.p.texts[i];
            EdCap { i: i as i32, a: fmt_ms(t.start).into(), b: fmt_ms(t.end).into(), text: t.raw.clone().into(), sel: e.is_sel(Sel::Txt(i)), on: e.t >= t.start && e.t < t.end }
        }).collect();
        CAPS.with(|m| {
            let soft = CAPS_SOFT.with(|c| c.replace(false));
            let same = m.row_count() == rows.len() && rows.iter().enumerate().all(|(k, r)| m.row_data(k).is_some_and(|o| o.i == r.i && o.a == r.a && o.b == r.b && o.text == r.text));
            if soft || same {
                shape_eq(m, rows);
            } else {
                // texts / times changed from elsewhere (undo, style, a time typed): fresh rows, so every box shows it
                m.set_vec(rows);
            }
        });
        if let Some(t) = e.sel_txt().map(|i| &e.p.texts[i]).filter(|t| t.cap) {
            g.set_cap_own(t.own);
            g.set_cap_y(t.y as f32);
            g.set_cap_anim(t.anim.clone().into());
        }
    }
    g.set_rng_on(e.rng.is_some());
    if let Some((ra, rb)) = e.rng {
        g.set_rng_a(ra as f32);
        g.set_rng_b(rb as f32);
        g.set_rng_text(format!("{} – {} · {:.1}s", fmt_t(ra), fmt_t(rb), rb - ra).into());
    }
    g.set_volume(e.p.volume as f32);
    g.set_mute(e.p.mute);
    g.set_zoom_frac(((e.pps / 2.0).ln() / 400f64.ln()).clamp(0.0, 1.0) as f32);
    // Ctrl+drag box on the timeline
    match &e.drag {
        Some(crate::editor::Drag::Marq { x0, y0, x1, y1, moved: true }) => {
            g.set_marq(true);
            g.set_marq_x(x0.min(*x1) as f32);
            g.set_marq_y(y0.min(*y1) as f32);
            g.set_marq_w((x1 - x0).abs() as f32);
            g.set_marq_h((y1 - y0).abs() as f32);
        }
        _ => g.set_marq(false),
    }
    if e.msel.len() > 1 {
        let n = |f: fn(&Sel) -> bool| e.msel.iter().filter(|s| f(s)).count();
        let parts: Vec<String> = [(n(|s| matches!(s, Sel::Seg(_))), "piece"), (n(|s| matches!(s, Sel::Aud(_))), "audio clip"), (n(|s| matches!(s, Sel::Txt(_))), "text"), (n(|s| matches!(s, Sel::Box(_))), "effect"), (n(|s| matches!(s, Sel::Ov(_))), "layer piece")]
            .iter().filter(|(c, _)| *c > 0).map(|(c, w)| format!("{c} {w}{}", if *c == 1 { "" } else { "s" })).collect();
        g.set_sel_kind("multi".into());
        g.set_sel_info(format!("{} selected: {}", e.msel.len(), parts.join(", ")).into());
        time(a, e);
        a.window().request_redraw();
        return;
    }
    match e.sel {
        Some(Sel::Seg(i)) if i < e.p.segs.len() => {
            let s = &e.p.segs[i];
            g.set_sel_kind("seg".into());
            g.set_sel_speed(s.speed as f32);
            g.set_sel_fi(s.fi as f32);
            g.set_sel_fo(s.fo as f32);
            g.set_sel_fmax(s.len().min(10.0).max(0.2) as f32 / 2.0);
            g.set_sel_freeze(s.freeze());
            g.set_sel_hold(s.len() as f32);
            g.set_sel_look(s.look().into());
            g.set_sel_bri(s.num("bri", 0.0) as f32);
            g.set_sel_con(s.num("con", 1.0) as f32);
            g.set_sel_sat(s.num("sat", 1.0) as f32);
            // Play backwards + the transition from the piece before (only the 2nd piece on; "only when they touch")
            g.set_sel_rev(s.rev());
            g.set_sel_tr(s.tr().into());
            g.set_sel_td(s.td() as f32);
            g.set_sel_has_prev(i > 0);
            g.set_sel_near(i > 0 && { let p = &e.p.segs[i - 1]; (p.at + p.len() - s.at).abs() < 0.03 });
            g.set_sel_amute(s.extra.get("amute").and_then(|x| x.as_bool()) == Some(true));
            g.set_sel_info(if s.freeze() { format!("Shows the picture at {} of the clip", fmt_t(s.start)) } else { format!("Source {} – {} · plays {:.1}s", fmt_t(s.start), fmt_t(s.end), s.len()) }.into());
        }
        Some(Sel::Aud(i)) if i < e.p.audio.len() => {
            let a = &e.p.audio[i];
            g.set_sel_kind("aud".into());
            g.set_aud_label(if a.label.is_empty() { "Audio".into() } else { a.label.clone().into() });
            g.set_aud_vol(a.vol as f32);
            g.set_aud_mute(a.mute);
            g.set_sel_fi(a.fi as f32);
            g.set_sel_fo(a.fo as f32);
            g.set_sel_fmax(a.len().min(10.0).max(0.2) as f32 / 2.0);
            g.set_sel_info(format!("Source {} – {} · starts at {}", fmt_t(a.start), fmt_t(a.end), fmt_t(a.at)).into());
        }
        Some(Sel::Ov(i)) if i < e.p.over.len() => {
            // a layer piece (Echo's layer panel)
            let o = &e.p.over[i];
            g.set_sel_kind("ov".into());
            g.set_lay_row(o.row as i32);
            g.set_lay_scale(o.scale as f32);
            g.set_lay_sound(o.sound);
            g.set_sel_speed(o.speed as f32);
            g.set_sel_info(format!("Source {} – {} · shows {} – {}", fmt_t(o.start), fmt_t(o.end), fmt_t(o.at), fmt_t(o.at + o.len())).into());
        }
        Some(Sel::Box(i)) if i < e.p.boxes.len() => {
            let b = &e.p.boxes[i];
            g.set_sel_kind("box".into());
            g.set_box_kind(b.kind.clone().into());
            g.set_box_blur(b.blur as f32);
            g.set_box_color(b.color.clone().into());
            g.set_box_opacity(b.opacity as f32);
            g.set_box_zoom(format!("{:.1}", (1.0 / b.w.max(b.h)).clamp(1.0, 5.0)).into());
            g.set_sel_info(format!("Shows {} – {} · drag it on the video to move, corners to resize", fmt_t(b.start), fmt_t(b.end)).into());
        }
        Some(Sel::Txt(i)) if i < e.p.texts.len() => {
            let t = &e.p.texts[i];
            g.set_sel_kind(if t.cap { "cap" } else { "txt" }.into());
            if g.get_sel_text().as_str() != t.raw && !LIVE.with(|l| l.borrow().is_some()) {
                g.set_sel_text(if t.cap { t.raw.clone() } else { t.text.clone() }.into());
            }
            g.set_sel_color(t.color.clone().into());
            g.set_sel_anim(t.anim.clone().into());
            g.set_sel_size(t.size as f32);
            g.set_sel_info(format!("Shows {} – {} · drag it on the video to move{}", fmt_t(t.start), fmt_t(t.end), if t.cap { "" } else { " (it snaps to the middle; hold Alt to place freely)" }).into());
            // the colour picker follows the text (not while it is being dragged: then they are the same already)
            if !t.cap && g.get_pick_hex().as_str() != t.color {
                if let Some((h, s, v)) = hex_hsv(&t.color) {
                    g.set_pick_h(h as f32);
                    g.set_pick_s(s as f32);
                    g.set_pick_v(v as f32);
                }
                g.set_pick_hex(t.color.clone().into());
            }
            if t.cap {
                g.set_pick_open(false);
            }
        }
        _ => {
            g.set_sel_kind("".into());
            g.set_pick_open(false);
        }
    }
    time(a, e);
    a.window().request_redraw();
}

/// the playhead only (60x a second while playing)
fn time(a: &AppWindow, e: &Editor) {
    let g = a.global::<Ed>();
    g.set_t(e.t as f32);
    g.set_playing(e.playing);
    g.set_time_text(format!("{} / {}", fmt_t(e.t), fmt_t(e.total())).into());
    g.set_t_text(fmt_t(e.t).into());
    g.set_scroll(e.scroll as f32);
    // the caption showing right now is lit in the list (Echo's markCapRows)
    CAPS.with(|m| {
        for k in 0..m.row_count() {
            if let Some(mut r) = m.row_data(k) {
                let on = e.p.texts.get(r.i as usize).is_some_and(|t| e.t >= t.start && e.t < t.end);
                if r.on != on {
                    r.on = on;
                    m.set_row_data(k, r);
                }
            }
        }
    });
}

fn view_w(a: &AppWindow) -> f64 {
    let sf = a.window().scale_factor() as f64;
    (a.window().size().width as f64 / sf - LP).max(100.0)
}

fn clamp_scroll(e: &mut Editor) {
    let max = (e.total() * e.pps + 200.0 - e.tl_w).max(0.0);
    e.scroll = e.scroll.clamp(0.0, max);
}

/// test hook ECHO_TL_BENCH (ed_bench.rs): a zoom (pixels per second), scrolled to the start
pub fn bench_zoom(a: &AppWindow, pps: f64) {
    with_ed(|e| {
        e.tl_w = view_w(a);
        e.pps = pps.clamp(5.0, 400.0);
        e.scroll = 0.0;
        clamp_scroll(e);
        push(a, e);
    });
}

/// test hook ECHO_TL_BENCH: (scroll now, the most it can scroll)
pub fn bench_scroll() -> (f64, f64) {
    with_ed(|e| (e.scroll, (e.total() * e.pps + 200.0 - e.tl_w).max(0.0))).unwrap_or((0.0, 0.0))
}

/// the frame at the playhead (paused) / jump there (playing)
fn seek(p: &Rc<RefCell<Player>>, e: &mut Editor, exact: bool) {
    if let Some(m) = p.borrow().mpv.as_ref() {
        e.seek_out(m, exact);
    }
}

/// the ASS overlay for this frame (render hook, all drawing modes). Nothing when the editor is closed.
pub fn overlay_frame(a: &AppWindow, m: &crate::mpv::Mpv) {
    ED.with(|ed| {
        let mut ed = ed.borrow_mut();
        let Some(e) = ed.as_mut() else {
            if OV_SHOWN.with(|o| o.replace(false)) {
                m.command(&["osd-overlay", crate::editor::OVERLAY_ID, "none", ""]);
            }
            return;
        };
        let num = |k: &str| m.get(&format!("osd-dimensions/{k}")).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        let (ow, oh) = (num("w"), num("h"));
        if ow < 2.0 || oh < 2.0 {
            return;
        }
        let (ml, mt, mr, mb) = (num("ml"), num("mt"), num("mr"), num("mb"));
        let vw = a.get_video_w() as f64;
        // the picture inside the OSD from the clip's shape (not mpv's margins: they move with the zoom preview)
        let _ = (ml, mt, mr, mb);
        let ar = if e.h > 0 { e.w as f64 / e.h as f64 } else { 16.0 / 9.0 };
        let (cw, ch) = if ow / oh > ar { (oh * ar, oh) } else { (ow, ow / ar) };
        e.geo = crate::editor::Geo { ow, oh, cx: (ow - cw) / 2.0, cy: (oh - ch) / 2.0, cw, ch, sf: if vw > 0.0 { ow / vw } else { 1.0 } };
        e.preview_fx(m);
        // separated audio: mpv mixes the tracks (applied when nothing is being dragged - each change reloads it)
        if e.drag.is_none() && LIVE.with(|l| l.borrow().is_none()) && e.loaded.is_none() {
            let af = e.audio_lavfi();
            if af != e.last_af {
                m.set("lavfi-complex", &af);
                e.last_af = af;
                e.seek_out(m, true);
            }
        }
        crate::pl_layer::overlay(e, m); // Chat C: video layers (PiP) - mpv bitmaps 3..9, under the stickers
        crate::pl_sticker::overlay(e, m);
        let ov = e.overlay();
        let key = format!("{ow}x{oh}|{ov}");
        if key != e.last_ov {
            e.last_ov = key;
            if ov.is_empty() {
                m.command(&["osd-overlay", crate::editor::OVERLAY_ID, "none", ""]);
            } else {
                m.command(&["osd-overlay", crate::editor::OVERLAY_ID, "ass-events", &ov, &format!("{}", ow as i64), &format!("{}", oh as i64), "0", "no", "no"]);
            }
            OV_SHOWN.with(|o| o.set(true));
        }
    });
}

/// open the editor on a clip (the player opens it first if needed). `play` = main.rs's open-in-player.
pub fn open(a: &AppWindow, p: &Rc<RefCell<Player>>, idx: usize, play: &Rc<dyn Fn(i32)>, ask_project: bool) {
    let Some(lib) = LIB.with(|l| l.borrow().clone()) else { return };
    let c = lib.clip(idx);
    // an exported video made in the editor: offer to continue its project (Esc = cancel, chat 21 W7)
    if ask_project {
        if let Some(pj) = find_project(&c.path) {
            let orig = pj.get("clip").and_then(|v| v.as_str()).map(PathBuf::from);
            let oidx = orig.as_ref().and_then(|o| lib.index_of_path(o));
            if let Some(oi) = oidx.filter(|&oi| oi != idx) {
                let (w, p2, play2) = (a.as_weak(), p.clone(), play.clone());
                crate::ask(a, "Edit the project?", &format!("\"{}\" was made in the editor. Open its project to change the pieces, texts and captions again - or edit the exported video as it is.", c.shown),
                    &[("Edit the video", "video", ""), ("Open project", "proj", "delb")], None, "proj", move |a, v, _| {
                        let _ = &w;
                        match v.as_str() {
                            "proj" => open_with(a, &p2, oi, &play2, Some(pj)),
                            "video" => open_with(a, &p2, idx, &play2, None),
                            // cancelled: a waiting Home "Add captions" goes too
                            _ => PENDING.with(|p| *p.borrow_mut() = None),
                        }
                    });
                return;
            }
            if oidx.is_none() && pj.get("clip").is_some() {
                // Echo: the project's clip was deleted / moved - edit the exported file as it is
                toast(a, "The original clip of this project is gone - editing the exported video");
            }
        }
    }
    open_with(a, p, idx, play, None);
}

/// 9 Nov: E / Edit from the player - the player's trim + strip cuts become cuts in the fresh edit (one undo
/// step: Ctrl+Z gives the whole clip back) and the play head lands where the player was. Skipped when the editor
/// opened a project instead (or asked first): those pieces aren't the plain clip.
pub fn carry_in(a: &AppWindow, p: &Rc<RefCell<Player>>, clip: &Path, cv: crate::pl_cuts::Carry) {
    // r2-threads (L30): the editor is still opening (ffprobe on a worker): this runs once it's open
    if OPENING.with(|o| o.get()) {
        CARRY.with(|x| *x.borrow_mut() = Some((clip.to_path_buf(), cv)));
        return;
    }
    let done = with_ed(|e| {
        let fresh = e.clip == clip && e.p.segs.len() == 1 && e.p.segs[0].at.abs() < 1e-6 && e.p.segs[0].start.abs() < 1e-6 && e.p.texts.is_empty();
        if !fresh {
            return None;
        }
        let total = e.total();
        let mut rm: Vec<(f64, f64)> = cv.removed.iter().map(|&(x, y)| (x.max(0.0), y.min(total))).filter(|(x, y)| y - x > 0.01).collect();
        rm.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
        if !rm.is_empty() {
            e.push_hist();
            // from the end backwards: earlier times stay where they are
            for &(x, y) in rm.iter().rev() {
                crate::editor::Editor::cut_span(&mut e.p, x, y);
            }
            e.normalize_segs();
        }
        // the play head on the cut-down timeline
        let mut t = cv.pos;
        for &(x, y) in &rm {
            if cv.pos > x {
                t -= cv.pos.min(y) - x;
            }
        }
        // QA B18 (11 Nov 2026): played to the end (or within 0.5 s of it) - start at 0 instead of past the last frame
        // (the preview was black until the play head moved)
        if t >= e.total() - 0.5 {
            t = 0.0;
        }
        e.t = t.clamp(0.0, e.total());
        e.pps = ((e.tl_w - 60.0) / e.total().max(1.0)).clamp(5.0, 400.0);
        e.scroll = 0.0;
        seek(p, e, true);
        push(a, e);
        Some(!rm.is_empty())
    })
    .flatten();
    if done == Some(true) {
        let n = cv.cuts;
        toast(a, if n > 0 { format!("Your trim and {n} cut{} came along - Ctrl+Z gives the whole clip back", if n == 1 { "" } else { "s" }) } else { "Your trim came along - Ctrl+Z gives the whole clip back".into() });
    }
}

fn open_with(a: &AppWindow, p: &Rc<RefCell<Player>>, idx: usize, play: &Rc<dyn Fn(i32)>, pj: Option<serde_json::Value>) {
    let Some(lib) = LIB.with(|l| l.borrow().clone()) else { return };
    let c = lib.clip(idx);
    let from_player = { let pl = p.borrow(); pl.open && pl.clip == Some(idx) };
    if !from_player {
        // load it in the (hidden) player first: same mpv, same video spot
        // not in the current view (filter / search): show all clips first, so it can open
        if lib.view_row_of(idx).is_none() {
            a.invoke_pick_filter("all".into());
        }
        let row = lib.view_row_of(idx).map(|r| r as i32);
        match row {
            Some(r) => {
                // 11 Nov 2026: Edit goes straight to the editor - the player stays hidden (and its video window
                // parked) while the clip loads and ffprobe runs; open_rest / the worker's answer clears it
                if !OPENING.with(|o| o.get()) {
                    a.global::<Ed>().set_opening(true);
                }
                play(r)
            }
            None => {
                toast(a, "That clip isn't in the current view - open it from the library");
                PENDING.with(|p| *p.borrow_mut() = None);
                return;
            }
        }
        p.borrow_mut().start_paused = true;
    }
    // r2-threads (L30): ffprobe (the clip + a saved project's other files) runs on a worker, the editor opens when
    // it's back (a second open meanwhile is ignored)
    if OPENING.with(|o| o.replace(true)) {
        return;
    }
    let ff = sheets::ffmpeg();
    let others: Vec<String> = pj
        .as_ref()
        .map(|pj| {
            let mut t = Editor::new(c.path.clone(), String::new(), c.dur.unwrap_or(1.0).max(0.1), 16, 16);
            t.load_project(pj);
            t.other_files()
        })
        .unwrap_or_default();
    let (aw, p2, clip) = (a.as_weak(), p.clone(), c.path.clone());
    // QA B42 (11 Nov 2026): straight from Home / Library the player hasn't read the clip yet - a clip whose length isn't
    // known (fresh library, saved seconds ago) opened as a 0.1 s timeline. Its length is read here then.
    let need_dur = c.dur.is_none();
    crate::lib_net::bg(
        move || {
            let main = ff.as_ref().map(|f| crate::edrender::probe(f, &clip)).unwrap_or((1920, 1080, true, 1));
            let len = if need_dur { ff.as_ref().and_then(|f| crate::pl_media::duration(f, &clip)) } else { None };
            let srcs: Vec<(String, (bool, f64))> = match ff.as_ref() {
                Some(ff) => others
                    .into_iter()
                    .map(|f| {
                        let (_, _, au, _) = crate::edrender::probe(ff, Path::new(&f));
                        let d = crate::pl_media::duration(ff, Path::new(&f)).unwrap_or(0.0);
                        (f, (au, d))
                    })
                    .collect(),
                None => vec![],
            };
            (main, srcs, len)
        },
        move |(main, srcs, len)| {
            OPENING.with(|o| o.set(false));
            if let Some(a) = aw.upgrade() {
                open_rest(&a, &p2, idx, pj, from_player, main, srcs, len);
                // after Ed.open, and one drawn frame later: the D3D video window moves at once, the editor's UI only
                // on the next frame - the video stood over Home for that frame (seen in a frame grab, 11 Nov 2026)
                if a.global::<Ed>().get_opening() {
                    let w = a.as_weak();
                    slint::Timer::single_shot(Duration::from_millis(50), move || {
                        if let Some(a) = w.upgrade() {
                            a.global::<Ed>().set_opening(false);
                            a.window().request_redraw();
                        }
                    });
                }
            }
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn open_rest(a: &AppWindow, p: &Rc<RefCell<Player>>, idx: usize, pj: Option<serde_json::Value>, from_player: bool, main: (u32, u32, bool, u32), srcs: Vec<(String, (bool, f64))>, len: Option<f64>) {
    let Some(lib) = LIB.with(|l| l.borrow().clone()) else { return };
    if idx >= lib.len() {
        return;
    }
    let c = lib.clip(idx);
    let ff = sheets::ffmpeg();
    let (w, h, _, tracks) = main;
    let dur = c.dur.or(len).unwrap_or(p.borrow().duration).max(0.1);
    let mut e = Editor::new(c.path.clone(), c.shown.clone(), dur, w.max(16), h.max(16));
    e.from_player = from_player;
    e.tracks = tracks;
    // "Even out loudness" is remembered between editor sessions (Echo: localStorage "normalize")
    e.normalize = crate::lib_fx::ui_get("ed_normalize").as_bool().unwrap_or(false);
    // how loud each track is (for U: only rows that have sound) - measured in the background like Echo's track_levels
    if let (Some(ff), true) = (ff.clone(), tracks >= 2) {
        let (clip, w) = (c.path.clone(), a.as_weak());
        std::thread::spawn(move || {
            let lv = crate::edrender::track_levels(&ff, &clip, tracks);
            let _ = slint::invoke_from_event_loop(move || {
                let _ = &w;
                with_ed(|e| {
                    if e.clip == clip {
                        e.levels = lv;
                    }
                });
            });
        });
    }
    if let Some(pj) = pj.as_ref() {
        e.load_project(pj);
        e.srcs.extend(srcs); // r2-threads (L30): probed on the worker
        toast(a, "Project opened - export again when you're done");
    }
    e.tl_w = view_w(a);
    e.pps = ((e.tl_w - 60.0) / e.total().max(1.0)).clamp(5.0, 400.0);
    if let Some(m) = p.borrow().mpv.as_ref() {
        e.pause(m);
        m.set("volume-max", "200");
    }
    preview_volume(p, &e);
    {
        let pl = p.borrow();
        if let Some(m) = pl.mpv.as_ref() {
            e.seek_out(m, true);
        }
    }
    let g = a.global::<Ed>();
    MODELS.with(|(ms, mt, mk)| {
        g.set_segs(ModelRc::from(ms.clone()));
        g.set_texts(ModelRc::from(mt.clone()));
        g.set_ticks(ModelRc::from(mk.clone()));
    });
    AUDS.with(|m| g.set_auds(ModelRc::from(m.clone())));
    LAYS.with(|m| g.set_layers(ModelRc::from(m.clone())));
    g.set_name(c.shown.as_str().into());
    g.set_dur(dur as f32);
    g.set_busy(false);
    g.set_listening(false);
    g.set_export_menu(false);
    g.set_pick_open(false);
    g.set_export_pct(0.0);
    // Echo's export menu: "One export that fits under 10 MB, copied for pasting"
    g.set_dc_text(format!("One export that fits under {}, copied for pasting", fmt_mb(crate::edrender::discord_mb())).into());
    CAPS.with(|m| {
        m.set_vec(vec![]);
        g.set_caps(ModelRc::from(m.clone()));
    });
    CAP_BASE.with(|c| *c.borrow_mut() = None);
    push(a, &e);
    ED.with(|x| *x.borrow_mut() = Some(e));
    g.set_open(true);
    a.global::<Play>().set_paused(true);
    let w = a.as_weak();
    slint::Timer::single_shot(Duration::from_millis(0), move || {
        if let Some(a) = w.upgrade() {
            a.invoke_focus_editor();
        }
    });
    elog(format!("editor: opened {}{}", c.name, if from_player { " (from the player)" } else { "" }));
    // r2-threads (L30): the player's trim + cuts that waited for the editor to open (carry_in)
    if let Some((clip, cv)) = CARRY.with(|x| x.borrow_mut().take()) {
        carry_in(a, p, &clip, cv);
    }
    // Home "Add captions" asked before the editor was open (it may have asked "Edit the project?" first)
    if let Some((cmd, at)) = PENDING.with(|p| p.borrow_mut().take()) {
        if at.elapsed() < Duration::from_secs(120) {
            run_soon(a, cmd);
        }
    }
    build_tiles(a); // the filmstrip pictures (when the sheet comes later: editor.slint asks again)
    crate::ed_bench::arm(a); // test hook ECHO_TL_BENCH
}

pub fn close(a: &AppWindow, p: &Rc<RefCell<Player>>, force: bool) {
    let dirty = with_ed(|e| !e.hist.is_empty() && !e.busy).unwrap_or(false);
    if dirty && !force {
        let p2 = p.clone();
        confirm_box(a, "Leave editor?", "Your edits haven't been exported and will be lost.", "Leave", move |a| close(a, &p2, true));
        return;
    }
    let (had_vf, hw_copied) = with_ed(|e| (!e.last_vf.is_empty(), e.hw_copied)).unwrap_or((false, false));
    let had_af = with_ed(|e| !e.last_af.is_empty()).unwrap_or(false);
    let other_loaded = with_ed(|e| e.loaded.is_some().then(|| e.clip.clone())).flatten();
    crate::pl_media::stop_sounds();
    ED.with(|e| *e.borrow_mut() = None);
    LIVE.with(|l| *l.borrow_mut() = None);
    if let Some(m) = p.borrow().mpv.as_ref() {
        // the player's own speed (the bar's SPEED menu) again - the editor ran at each piece's speed
        m.set("speed", &format!("{}", a.global::<Play>().get_speed()));
        m.set("pause", "yes");
        for k in ["brightness", "contrast", "saturation", "gamma", "video-zoom", "video-pan-x", "video-pan-y"] {
            m.set(k, "0");
        }
        if had_af {
            let pos = m.get_f64("time-pos").unwrap_or(0.0);
            m.set("lavfi-complex", "");
            m.command(&["seek", &format!("{pos:.3}"), "absolute+exact"]);
        }
        if hw_copied {
            m.set("hwdec", &p.borrow().hwdec.clone());
        }
        // AFTER the decoder is back: the player's own filter for its kind of frames (Linux, see mpv::base_vf) -
        // the GPU filter on in-memory frames would fail
        if had_vf || hw_copied {
            let base = crate::mpv::with_base_vf("", crate::mpv::hwdec_copies(&p.borrow().hwdec));
            if base.is_empty() {
                m.command(&["vf", "clr", ""]);
            } else {
                m.command(&["vf", "set", &base]);
            }
        }
        // a piece from another file was showing: the player gets its clip back
        if let Some(c) = other_loaded {
            m.command(&["loadfile", &c.to_string_lossy(), "replace", "-1", "start=0,pause=yes"]);
        }
        let ps = a.global::<Play>();
        m.set("volume", &format!("{:.0}", (ps.get_volume() * 100.0).clamp(0.0, 100.0)));
        m.set("mute", if ps.get_muted() { "yes" } else { "no" });
        if OV_SHOWN.with(|o| o.replace(false)) {
            m.command(&["osd-overlay", crate::editor::OVERLAY_ID, "none", ""]);
        }
        crate::pl_sticker::clear(Some(m));
        crate::pl_layer::clear(Some(m));
    }
    crate::pl_layer::clear(None); // the layers' picture strips in %TEMP% go too
    a.global::<Ed>().set_sticker_menu(false);
    a.global::<Ed>().set_open(false);
    a.global::<Ed>().set_tiles(ModelRc::default()); // the filmstrip pictures go with the editor
    a.global::<Play>().set_paused(true);
    // like Echo's closeEditor - leaving the editor goes back to the library / Home, never to the player
    // (it used to reopen the player when the editor was opened from it)
    a.global::<Play>().invoke_close();
    a.window().request_redraw();
}

/// every Ed callback + the playback tick
pub fn setup(app: &AppWindow, player: Rc<RefCell<Player>>, play: Rc<dyn Fn(i32)>, clips_root: PathBuf) {
    PL.with(|x| *x.borrow_mut() = Some(player.clone()));
    PLAY.with(|x| *x.borrow_mut() = Some(play.clone()));
    ROOT.with(|r| *r.borrow_mut() = clips_root.clone());
    let g = app.global::<Ed>();
    {
        let (w, p) = (app.as_weak(), player.clone());
        let root = clips_root.clone();
        let play = play.clone();
        g.on_act(move |cmd| {
            let Some(a) = w.upgrade() else { return };
            act(&a, &p, cmd.as_str(), &root, &play);
        });
    }
    {
        let (w, p) = (app.as_weak(), player.clone());
        g.on_tl(move |kind, x, y| {
            let Some(a) = w.upgrade() else { return };
            let vw = view_w(&a);
            let tl_top = a.global::<Ed>().get_tl_top() as f64;
            let r = with_ed(|e| {
                e.tl_w = vw;
                e.tl_top = tl_top;
                let cx = x as f64 + e.scroll;
                match kind.as_str() {
                    "hover" => {
                        let c = match e.tl_hit(cx, y as f64) {
                            Some((_, _, 'l')) | Some((_, _, 'r')) => "ew",
                            Some(_) => "move",
                            None => "",
                        };
                        a.global::<Ed>().set_cursor(c.into());
                        (false, false)
                    }
                    // wheel / Ctrl+wheel: "<kind>:<px>" - one notch = 160 px of glide steps (80 px scroll, 1.25x zoom)
                    k if k.starts_with("wheel:") => {
                        let notches = k[6..].parse::<f64>().unwrap_or(0.0) / 160.0;
                        e.scroll -= notches * 80.0;
                        clamp_scroll(e);
                        // 11 Nov 2026: scrolling only moves the view - no full push() of every row per glide step
                        a.global::<Ed>().set_scroll(e.scroll as f32);
                        (false, false)
                    }
                    // the scrollbar (editor.slint sbar): drag the thumb / click the track
                    k if k.starts_with("scrollto:") => {
                        e.scroll = k[9..].parse::<f64>().unwrap_or(e.scroll);
                        clamp_scroll(e);
                        a.global::<Ed>().set_scroll(e.scroll as f32);
                        (false, false)
                    }
                    k if k.starts_with("ctrl-wheel:") => {
                        let notches = k[11..].parse::<f64>().unwrap_or(0.0) / 160.0;
                        zoom(e, 1.25f64.powf(notches), x as f64);
                        (true, false)
                    }
                    "menu" => {
                        // right-click: Echo's timeline menu for what's under the mouse
                        let t = ((cx - LP) / e.pps).clamp(0.0, e.total());
                        let item = |label: &str, key: &str, act: String, off: bool| EdMenu { label: label.into(), key: key.into(), act: act.into(), off, sep: false };
                        let sep = || EdMenu { sep: true, ..Default::default() };
                        let mut items: Vec<EdMenu> = vec![];
                        match e.tl_hit(cx, y as f64) {
                            Some((k, i, _)) => {
                                e.msel.clear();
                                e.sel = Some(match k { 's' => Sel::Seg(i), 'a' => Sel::Aud(i), 'b' => Sel::Box(i), 'o' => Sel::Ov(i), _ => Sel::Txt(i) });
                                items.push(item("Split at playhead", "S", "split".into(), false));
                                items.push(item("Delete", "Del", "del".into(), false));
                                if k == 's' {
                                    let sg = &e.p.segs[i];
                                    items.push(item("Ripple delete", "Shift+Del", "ripple".into(), false));
                                    items.push(sep());
                                    items.push(item("Separate audio", "U", "audio-sep".into(), sg.extra.get("amute").and_then(|x| x.as_bool()) == Some(true) || sg.freeze()));
                                    for v in ["0.5", "1", "2"] {
                                        items.push(item(&format!("Speed {v}x"), "", format!("speed:{v}"), sg.freeze()));
                                    }
                                    items.push(sep());
                                    items.push(item("Freeze frame here", "F", format!("seekthen:{t:.3}:freeze"), sg.freeze()));
                                    items.push(item(if sg.rev() { "Play forwards" } else { "Play backwards" }, "", "rev".into(), sg.freeze()));
                                }
                                if k == 'o' {
                                    items.push(sep());
                                    for v in ["0.5", "1", "2"] {
                                        items.push(item(&format!("Speed {v}x"), "", format!("speed:{v}"), false));
                                    }
                                }
                                if k == 'a' {
                                    items.push(sep());
                                    items.push(item(if e.p.audio[i].mute { "Unmute" } else { "Mute" }, "", "audmute".into(), false));
                                }
                                if k == 't' && !e.p.texts[i].cap {
                                    items.push(item("Duplicate", "Ctrl+D", "dup".into(), false));
                                }
                            }
                            None if { let vy = e.layout().video_y; y as f64 >= vy && (y as f64) < vy + 64.0 } => {
                                items.push(item("Close this gap", "", format!("closegap:{t:.3}"), false));
                                items.push(item("Close all gaps", "", "gaps".into(), false));
                                items.push(sep());
                                items.push(item("Move playhead here", "", format!("seek:{t:.3}"), false));
                            }
                            None => {
                                items.push(item("Move playhead here", "", format!("seek:{t:.3}"), false));
                                items.push(item("Add text here", "T", format!("seekthen:{t:.3}:text"), false));
                                items.push(item("Add blur box here", "B", format!("seekthen:{t:.3}:box"), false));
                                items.push(item("Add zoom here", "Z", format!("seekthen:{t:.3}:zoomfx"), false));
                            }
                        }
                        let g = a.global::<Ed>();
                        MENU.with(|m| m.set_vec(items));
                        MENU.with(|m| g.set_menu_items(ModelRc::from(m.clone())));
                        g.set_menu_x(x);
                        g.set_menu_y(y);
                        g.set_menu_open(true);
                        (true, false)
                    }
                    k => {
                        let playing = e.playing;
                        if (k == "down" || k == "cdown" || k == "sdown") && playing {
                            if let Some(m) = p.borrow().mpv.as_ref() {
                                e.pause(m);
                            }
                        }
                        e.tl_mouse(k, cx, y as f64)
                    }
                }
            });
            if let Some((ui, sk)) = r {
                with_ed(|e| {
                    if sk {
                        let dragging = matches!(e.drag, Some(crate::editor::Drag::Head));
                        seek(&p, e, !dragging || kind == "up");
                    }
                    if ui {
                        push(&a, e);
                    }
                });
            }
            // a piece dragged up onto a layer (or refused): Echo's toast
            if let Some(m) = with_ed(|e| e.layer_msg.take()).flatten() {
                toast(&a, m);
            }
        });
    }
    {
        let (w, p) = (app.as_weak(), player.clone());
        g.on_stage(move |kind, x, y| {
            let Some(a) = w.upgrade() else { return };
            let changed = with_ed(|e| {
                let k = if e.geo.sf > 0.0 { e.geo.sf } else { a.window().scale_factor() as f64 };
                if kind == "dbl" {
                    if e.sel_txt().is_some() {
                        focus_text(&a);
                    }
                    return false;
                }
                if kind.ends_with("down") && e.playing {
                    if let Some(m) = p.borrow().mpv.as_ref() {
                        e.pause(m);
                    }
                }
                e.stage_mouse(kind.as_str(), x as f64 * k, y as f64 * k)
            });
            if changed == Some(true) {
                with_ed(|e| push(&a, e));
            }
        });
    }
    {
        let (w, p) = (app.as_weak(), player.clone());
        g.on_set(move |key, val| {
            let Some(a) = w.upgrade() else { return };
            set(&a, &p, key.as_str(), val.as_str());
        });
    }
    {
        let (w, p) = (app.as_weak(), player.clone());
        let root = clips_root.clone();
        let play = play.clone();
        g.on_key(move |text, ctrl, shift, repeat| {
            let Some(a) = w.upgrade() else { return false };
            key(&a, &p, text.as_str(), ctrl, shift, repeat, &root, &play)
        });
    }
    // the playhead: 60x a second while playing (mpv's time-pos -> output time, jumps at piece ends)
    let timer = slint::Timer::default();
    {
        let (w, p) = (app.as_weak(), player.clone());
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(16), move || {
            let Some(a) = w.upgrade() else { return };
            let pl = p.borrow();
            let Some(m) = pl.mpv.as_ref() else { return };
            with_ed(|e| {
                let was = e.playing;
                crate::pl_media::sync_sounds(e);
                if e.tick(m) || was != e.playing {
                    // keep the playhead in view
                    let x = e.t * e.pps;
                    if x > e.scroll + e.tl_w - 40.0 || x < e.scroll {
                        e.scroll = (x - 80.0).max(0.0);
                        clamp_scroll(e);
                    }
                    time(&a, e);
                    a.window().request_redraw();
                }
            });
        });
    }
    crate::idle::keep(timer); // pauses while Echo is hidden in the tray
}

/// pub(crate): gaps-library row 1 - pictures / GIFs dropped on the editor become stickers (drop.rs)
pub(crate) fn add_sticker(a: &AppWindow, p: PathBuf) {
    // r2-threads (L30): the picture's size (ffprobe) is read on a worker
    let ff = sheets::ffmpeg();
    let (w, p2) = (a.as_weak(), p.clone());
    crate::lib_net::bg(
        move || ff.map(|ff| crate::pl_sticker::picture_size(&ff, &p2)).unwrap_or((1, 1)),
        move |(iw, ih)| {
            let Some(a) = w.upgrade() else { return };
            with_ed(|e| {
                e.add_sticker(&p, iw, ih);
                push(&a, e);
            });
            toast(&a, "Sticker added - drag it on the video, corners to resize, its block on the timeline sets when");
        },
    );
}

/// Media: pick a video or a sound (file dialog on a worker thread), then it goes onto the timeline
fn media_pick(a: &AppWindow) {
    let Some(ff) = sheets::ffmpeg() else {
        toast(a, "Adding files needs ffmpeg (Echo's copy wasn't found)");
        return;
    };
    if let Some(m) = PL.with(|p| p.borrow().clone()) {
        with_ed(|e| {
            if let Some(mp) = m.borrow().mpv.as_ref() {
                e.pause(mp);
            }
        });
    }
    let w = a.as_weak();
    std::thread::spawn(move || {
        let mut exts: Vec<&str> = crate::pl_media::VIDEO_EXT.to_vec();
        exts.extend_from_slice(crate::pl_media::SOUND_EXT);
        let Some(path) = crate::pl_media::pick_file("Add a video or sound to the timeline", "Videos and sounds", &exts) else { return };
        let (vw, _, au, _) = crate::edrender::probe(&ff, &path);
        let dur = crate::pl_media::duration(&ff, &path);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(a) = w.upgrade() else { return };
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            let Some(dur) = dur else {
                toast(&a, format!("{name}: couldn't read it"));
                return;
            };
            let r = with_ed(|e| {
                let r = e.add_media(&path, au, vw > 0, dur);
                push(&a, e);
                r
            });
            match r {
                Some(Ok(m)) | Some(Err(m)) => toast(&a, m),
                None => {}
            }
            elog(format!("editor: media added {}", path.display()));
        });
    });
}

/// gaps-library row 1: a video or sound dropped on the editor goes onto the timeline - media_pick without the dialog
pub(crate) fn add_media_file(a: &AppWindow, path: PathBuf) {
    let Some(ff) = sheets::ffmpeg() else {
        toast(a, "Adding files needs ffmpeg (Echo's copy wasn't found)");
        return;
    };
    let w = a.as_weak();
    std::thread::spawn(move || {
        let (vw, _, au, _) = crate::edrender::probe(&ff, &path);
        let dur = crate::pl_media::duration(&ff, &path);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(a) = w.upgrade() else { return };
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            let Some(dur) = dur else {
                toast(&a, format!("{name}: couldn't read it"));
                return;
            };
            let r = with_ed(|e| {
                let r = e.add_media(&path, au, vw > 0, dur);
                push(&a, e);
                r
            });
            match r {
                Some(Ok(m)) | Some(Err(m)) => toast(&a, m),
                None => {}
            }
            elog(format!("editor: media dropped {}", path.display()));
        });
    });
}

/// put the keyboard into the inspector's text box (it only exists once the text is selected: next frame)
fn focus_text(a: &AppWindow) {
    let w = a.as_weak();
    slint::Timer::single_shot(Duration::from_millis(40), move || {
        if let Some(a) = w.upgrade() {
            let g = a.global::<Ed>();
            g.set_text_focus(g.get_text_focus() + 1);
        }
    });
}

/// the edit's volume / mute heard in the preview (mpv's volume, 100 = as recorded)
fn preview_volume(p: &Rc<RefCell<Player>>, e: &Editor) {
    if let Some(m) = p.borrow().mpv.as_ref() {
        m.set("volume", &format!("{:.0}", (e.p.volume * 100.0).clamp(0.0, 200.0)));
        m.set("mute", if e.p.mute { "yes" } else { "no" });
    }
}

fn zoom(e: &mut Editor, k: f64, mouse_x: f64) {
    let t = (mouse_x + e.scroll - LP) / e.pps;
    e.pps = (e.pps * k).clamp(2.0, 800.0);
    e.scroll = t * e.pps + LP - mouse_x;
    clamp_scroll(e);
}

/// QA B37 (11 Nov 2026): where the zoom buttons / slider zoom around - the play head while it's in view (it used to be
/// the middle of the view, so zooming in moved the play head out of sight), else the middle
fn zoom_anchor(e: &Editor) -> f64 {
    let x = LP + e.t * e.pps - e.scroll;
    if x >= LP && x <= LP + e.tl_w {
        x
    } else {
        e.tl_w / 2.0
    }
}

fn act(a: &AppWindow, p: &Rc<RefCell<Player>>, cmd: &str, root: &Path, play: &Rc<dyn Fn(i32)>) {
    let _ = play;
    if cmd == "back" {
        if with_ed(|e| e.drag.is_some()).unwrap_or(false) {
            return;
        }
        close(a, p, false);
        return;
    }
    if cmd == "blur" {
        LIVE.with(|l| *l.borrow_mut() = None);
        CAP_BASE.with(|c| *c.borrow_mut() = None);
        a.invoke_focus_editor();
        return;
    }
    // the player's picture sheet came in while the editor is open (editor.slint sheet-ready)
    if cmd == "tiles" {
        build_tiles(a);
        return;
    }
    a.global::<Ed>().set_menu_open(false);
    CAP_BASE.with(|c| *c.borrow_mut() = None);
    // the captions list: a row (select + playhead there, again = deselect), its play button, its ×
    if let Some((what, i)) = cmd.split_once(':').filter(|(w, _)| matches!(*w, "cap-row" | "cap-sel" | "cap-play" | "cap-del")).and_then(|(w, i)| i.parse::<usize>().ok().map(|i| (w, i))) {
        with_ed(|e| {
            if i >= e.p.texts.len() {
                return;
            }
            if let Some(m) = p.borrow().mpv.as_ref() {
                e.pause(m);
            }
            match what {
                // cap-sel: its text box got the keyboard - selected (never deselected)
                "cap-row" | "cap-sel" => {
                    e.msel.clear();
                    if e.sel == Some(Sel::Txt(i)) && what == "cap-row" {
                        e.sel = None;
                    } else {
                        e.sel = Some(Sel::Txt(i));
                        e.t = e.p.texts[i].start.min(e.total());
                    }
                }
                "cap-play" => {
                    e.t = e.p.texts[i].start.min(e.total());
                }
                _ => {
                    e.push_hist();
                    e.p.texts.remove(i);
                    e.sel = None;
                    e.msel.clear();
                }
            }
            seek(p, e, true);
            if what == "cap-play" {
                if let Some(m) = p.borrow().mpv.as_ref() {
                    e.play(m);
                }
            }
            push(a, e);
        });
        return;
    }
    // the text colour picker ("Any colour"): opens on the selected text's colour
    if cmd == "pick" {
        let g = a.global::<Ed>();
        if !g.get_pick_open() {
            if let Some((h, s, v)) = hex_hsv(g.get_sel_color().as_str()) {
                g.set_pick_h(h as f32);
                g.set_pick_s(s as f32);
                g.set_pick_v(v as f32);
            }
            g.set_pick_hex(g.get_sel_color());
        }
        g.set_pick_open(!g.get_pick_open());
        return;
    }
    // the timeline menu's actions
    if let Some(rest) = cmd.strip_prefix("seekthen:") {
        if let Some((t, then)) = rest.split_once(':') {
            act(a, p, &format!("seek:{t}"), root, play);
            act(a, p, then, root, play);
        }
        return;
    }
    if let Some(t) = cmd.strip_prefix("seek:").and_then(|v| v.parse::<f64>().ok()) {
        with_ed(|e| {
            e.t = t.clamp(0.0, e.total());
            seek(p, e, true);
            push(a, e);
        });
        return;
    }
    if let Some(t) = cmd.strip_prefix("closegap:").and_then(|v| v.parse::<f64>().ok()) {
        let r = with_ed(|e| {
            let r = e.close_gap(t);
            seek(p, e, true);
            push(a, e);
            r
        });
        match r {
            Some(Ok(l)) => toast(a, format!("Closed a {l:.1}s gap")),
            Some(Err(x)) => toast(a, x),
            None => {}
        }
        return;
    }
    if let Some(v) = cmd.strip_prefix("speed:") {
        set(a, p, "speed", v);
        return;
    }
    if cmd == "audmute" {
        let m = with_ed(|e| e.sel_aud().map(|i| e.p.audio[i].mute)).flatten();
        if let Some(m) = m {
            set(a, p, "aud-mute", if m { "0" } else { "1" });
        }
        return;
    }
    if let Some(preset) = cmd.strip_prefix("export:") {
        export(a, p, preset, root);
        return;
    }
    if cmd == "cap" {
        captions(a, p, false);
        return;
    }
    if cmd == "media" {
        media_pick(a);
        return;
    }
    if cmd == "sticker" {
        let g = a.global::<Ed>();
        if g.get_emoji().row_count() == 0 {
            g.set_emoji(ModelRc::new(VecModel::from(crate::pl_sticker::emoji_images())));
        }
        g.set_sticker_menu(!g.get_sticker_menu());
        return;
    }
    if let Some(k) = cmd.strip_prefix("emoji:").and_then(|v| v.parse::<usize>().ok()) {
        match crate::pl_sticker::save_emoji(crate::pl_sticker::EMOJI.get(k).copied().unwrap_or("🔥")) {
            Ok(p) => add_sticker(a, p),
            Err(x) => toast(a, x),
        }
        return;
    }
    if cmd == "sticker-pic" {
        let w = a.as_weak();
        std::thread::spawn(move || {
            let Some(src) = crate::pl_media::pick_file("Pick a picture", "Pictures", crate::pl_media::PICTURE_EXT) else { return };
            let r = crate::pl_sticker::copy_picture(&src);
            let _ = slint::invoke_from_event_loop(move || {
                let Some(a) = w.upgrade() else { return };
                match r {
                    Ok(p) => add_sticker(&a, p),
                    Err(x) => toast(&a, format!("Couldn't add that picture: {x}")),
                }
            });
        });
        return;
    }
    let mut msg: Option<String> = None;
    let r = with_ed(|e| {
        let pl = p.borrow();
        let m = pl.mpv.as_ref();
        let mut reseek = true;
        match cmd {
            "play" => {
                if let Some(m) = m {
                    if e.playing { e.pause(m) } else { e.play(m) }
                }
                reseek = false;
            }
            "undo" => {
                if !e.undo() {
                    msg = Some("Nothing to undo".into());
                }
            }
            "redo" => {
                e.redo();
            }
            "split" => {
                if let Err(x) = e.split() {
                    msg = Some(x.into());
                }
            }
            "del" | "ripple" => match e.delete_sel(cmd == "ripple") {
                Ok(x) if !x.is_empty() => msg = Some(x.into()),
                Err(x) => msg = Some(x.into()),
                _ => {}
            },
            "gaps" => {
                let n = e.close_all_gaps();
                msg = Some(if n == 0 { "No gaps".into() } else { format!("Closed {n} gap{}", if n == 1 { "" } else { "s" }) });
            }
            "text" => {
                e.add_text();
                a.global::<Ed>().set_sel_text("Text".into());
                focus_text(a);
                reseek = false;
            }
            "selall" => {
                e.select_all();
                if e.msel.len() > 1 {
                    msg = Some(format!("{} selected - drag one to move them all, Del to delete", e.msel.len()));
                }
                reseek = false;
            }
            "snap" => {
                e.snap = !e.snap;
                msg = Some(if e.snap { "Snapping on".into() } else { "Snapping off".into() });
                reseek = false;
            }
            "zoom-in" | "zoom-out" => {
                let mid = zoom_anchor(e);
                zoom(e, if cmd == "zoom-in" { 1.5 } else { 1.0 / 1.5 }, mid);
                reseek = false;
            }
            "vertical" => {
                e.push_hist();
                e.p.vertical = if e.p.vertical.is_some() { None } else { Some(0.5) };
                e.rechunk_caps();
                if e.p.vertical.is_some() {
                    msg = Some("Vertical 9:16 - drag the yellow frame to choose what stays in view".into());
                }
                reseek = false;
            }
            "cap-txt" => {
                let txt = e.captions_txt();
                let dir = e.clip.parent().map(|d| d.to_path_buf()).unwrap_or_default();
                let stem = e.clip.file_stem().unwrap_or_default().to_string_lossy().to_string();
                let f = dir.join(format!("{stem} (captions).txt"));
                msg = Some(match std::fs::write(&f, txt) {
                    Ok(()) => format!("Saved as \"{}\" next to the clip", f.file_name().unwrap_or_default().to_string_lossy()),
                    Err(x) => format!("Couldn't save the captions: {x}"),
                });
                reseek = false;
            }
            "box" | "zoomfx" => {
                e.add_box(if cmd == "box" { "blur" } else { "zoom" });
                if cmd == "zoomfx" {
                    msg = Some("Zoom added - make the blue box smaller to zoom closer; click elsewhere to preview".into());
                }
                reseek = false;
            }
            "freeze" => match e.freeze_frame() {
                Ok(()) => msg = Some("Freeze frame added - change how long it holds in the panel".into()),
                Err(x) => msg = Some(x.into()),
            },
            "dup" => {
                if let Some(i) = e.sel_txt().filter(|&i| !e.p.texts[i].cap) {
                    e.push_hist();
                    let mut c = e.p.texts[i].clone();
                    c.y = (c.y + c.size * 1.3).min(0.97);
                    e.p.texts.push(c);
                    e.sel = Some(Sel::Txt(e.p.texts.len() - 1));
                } else if let Some(i) = e.sel_box() {
                    e.push_hist();
                    let mut c = e.p.boxes[i].clone();
                    c.x = (c.x + 0.03).min(1.0 - c.w / 2.0);
                    c.y = (c.y + 0.03).min(1.0 - c.h / 2.0);
                    e.p.boxes.push(c);
                    e.sel = Some(Sel::Box(e.p.boxes.len() - 1));
                } else {
                    msg = Some("Only texts and boxes can be duplicated".into());
                }
                reseek = false;
            }
            "color-all" => {
                if let Some(i) = e.sel_seg() {
                    e.push_hist();
                    let src = e.p.segs[i].clone();
                    for o in e.p.segs.iter_mut() {
                        for k in ["look", "bri", "con", "sat"] {
                            match src.extra.get(k) {
                                Some(v) => { o.extra.insert(k.into(), v.clone()); }
                                None => { o.extra.remove(k); }
                            }
                        }
                    }
                    msg = Some("Colour copied to every piece".into());
                }
            }
            "color-reset" => {
                if let Some(i) = e.sel_seg() {
                    e.push_hist();
                    for k in ["look", "bri", "con", "sat"] {
                        e.p.segs[i].extra.remove(k);
                    }
                }
            }
            "audio-sep" => {
                let (tr, lv) = (e.tracks, e.levels.clone());
                msg = Some(match e.detach_audio(tr, &lv) {
                    Ok(x) => x,
                    Err(x) => x,
                });
                reseek = false;
            }
            // Echo: I = from the playhead to the end (or takes the highlight away); O = up to the playhead, keeping
            // the start (so I, move, O gives a part) - O again at the same spot takes it away
            "rng-in" | "rng-out" => {
                let (t, tt) = (e.t, e.total());
                if cmd == "rng-in" && e.rng.is_some() {
                    e.rng = None;
                } else if cmd == "rng-out" && e.rng.is_some_and(|(_, b)| (b - t).abs() < 0.05) {
                    e.rng = None;
                } else {
                    match (cmd, e.rng) {
                        ("rng-in", _) => e.set_rng(t, tt),
                        (_, Some((ra, _))) => e.set_rng(ra.min(t - 0.1), t),
                        _ => e.set_rng(0.0, t),
                    }
                    if e.rng.is_some() && !RNG_HINTED.with(|h| h.replace(true)) {
                        msg = Some("Part highlighted - X cuts it · I, Esc, ✕ or a click outside it clears it".into());
                    }
                }
                reseek = false;
            }
            "rev" => {
                // Play backwards / forwards (the timeline menu)
                if let Some(i) = e.sel_seg().filter(|&i| !e.p.segs[i].freeze()) {
                    e.push_hist();
                    let s = &mut e.p.segs[i];
                    if s.rev() { s.extra.remove("rev"); } else { s.extra.insert("rev".into(), serde_json::json!(true)); }
                }
            }
            "cap-add" => {
                e.add_caption();
                focus_text(a);
                reseek = false;
            }
            "cap-above" | "cap-below" => {
                if let Some(i) = e.sel_txt().filter(|&i| e.p.texts[i].cap) {
                    e.stack_caption(i, if cmd == "cap-above" { -1.0 } else { 1.0 });
                    focus_text(a);
                }
                reseek = false;
            }
            "cap-back" => {
                if let Some(i) = e.sel_txt().filter(|&i| e.p.texts[i].cap) {
                    e.cap_back(i);
                }
                reseek = false;
            }
            "rng-clear" => {
                e.rng = None;
                reseek = false;
            }
            "rng-cut" => {
                if e.rng.is_none() {
                    msg = Some("Highlight a part first (I / O, or Shift+drag on the ruler)".into());
                } else {
                    e.cut_range();
                    msg = Some("Cut - Ctrl+Z to undo".into());
                }
            }
            "cap-clear" => {
                e.push_hist();
                e.p.texts.retain(|t| !t.cap);
                e.sel = None;
                msg = Some("Captions removed".into());
                reseek = false;
            }
            _ => reseek = false,
        }
        drop(pl);
        if reseek && !e.playing {
            seek(p, e, true);
        }
        push(a, e);
    });
    if r.is_some() {
        if let Some(m) = msg {
            toast(a, m);
        }
    }
}

fn set(a: &AppWindow, p: &Rc<RefCell<Player>>, key: &str, val: &str) {
    let num = val.parse::<f64>().unwrap_or(0.0);
    let mut msg: Option<String> = None;
    with_ed(|e| {
        // one undo step per slider drag / typing burst
        let live = |e: &mut Editor| {
            LIVE.with(|l| {
                let mut l = l.borrow_mut();
                match l.as_mut() {
                    Some((_, t)) if t.elapsed() < Duration::from_millis(1200) => *t = Instant::now(),
                    _ => *l = Some((e.p.clone(), Instant::now())),
                }
            });
        };
        let commit = |e: &mut Editor| {
            if let Some((before, _)) = LIVE.with(|l| l.borrow_mut().take()) {
                if before != e.p {
                    e.hist.push(before);
                    e.fut.clear();
                }
            }
        };
        if key != "cap-text" {
            CAP_BASE.with(|c| *c.borrow_mut() = None);
        }
        // "<texts index>|<rest>" from a captions-list row
        let row = || val.split_once('|').and_then(|(i, r)| i.parse::<usize>().ok().map(|i| (i, r)));
        let cap_ok = |e: &Editor, i: usize| e.p.texts.get(i).is_some_and(|t| t.cap);
        match key {
            // ----- the captions panel (Echo's capPanel) -----
            "cap-size-live" | "cap-size" => {
                live(e);
                for t in e.p.texts.iter_mut().filter(|t| t.cap) {
                    t.size = num.clamp(0.03, 0.15);
                }
                e.rechunk_caps();
                if key == "cap-size" {
                    commit(e);
                }
            }
            "cap-w-live" | "cap-w" => {
                live(e);
                e.p.cap_w = num.clamp(0.15, 1.0);
                e.rechunk_caps();
                if key == "cap-w" {
                    commit(e);
                }
            }
            "cap-box" => e.cap_show_box = val == "1",
            "cap-y-live" | "cap-y" => {
                if let Some(i) = e.sel_txt().filter(|&i| e.p.texts[i].cap) {
                    live(e);
                    let t = &mut e.p.texts[i];
                    t.y = num.clamp(0.03, 0.97);
                    t.own = true;
                    if key == "cap-y" {
                        commit(e);
                    }
                }
            }
            "cap-anim" => {
                e.push_hist();
                for t in e.p.texts.iter_mut().filter(|t| t.cap) {
                    t.anim = val.into();
                }
            }
            "cap-text" => {
                if let Some((i, txt)) = row().filter(|(i, _)| cap_ok(e, *i)) {
                    let start = e.p.texts[i].start;
                    // the first letter typed in this row = one undo step; the words as they were then
                    let base = CAP_BASE.with(|c| {
                        let mut c = c.borrow_mut();
                        match c.as_ref() {
                            Some((k, s, w)) if *k == i && *s == start => Some(w.clone()),
                            _ => {
                                *c = Some((i, start, e.p.texts[i].words.clone()));
                                None
                            }
                        }
                    });
                    if base.is_none() {
                        e.push_hist();
                    }
                    let base = base.or_else(|| CAP_BASE.with(|c| c.borrow().as_ref().map(|x| x.2.clone())));
                    e.set_cap_text(i, txt, base);
                    CAPS_SOFT.with(|c| c.set(true));
                }
            }
            "cap-start" | "cap-end" => {
                if let Some((i, v)) = row().filter(|(i, _)| cap_ok(e, *i)) {
                    let before = e.p.clone();
                    let ok = parse_time(v).is_some_and(|v| e.set_cap_time(i, key == "cap-end", v));
                    if ok && before != e.p {
                        e.hist.push(before);
                        e.fut.clear();
                    }
                    // a fresh list: the box shows the time as it is now (m:ss.mmm), or the old one when refused
                    CAPS.with(|m| m.set_vec(vec![]));
                }
            }
            "cap-nudge" => {
                // the mouse wheel on a time: 10 ms a notch (Shift: 100 ms), one undo step per wheel burst
                let mut it = val.split('|');
                if let (Some(i), Some(f), Some(d)) = (it.next().and_then(|x| x.parse::<usize>().ok()), it.next(), it.next().and_then(|x| x.parse::<f64>().ok())) {
                    if cap_ok(e, i) {
                        if !NUDGE_AT.with(|c| c.replace(Some(Instant::now()))).is_some_and(|t| t.elapsed() < Duration::from_millis(700)) {
                            e.push_hist();
                        }
                        let end = f == "end";
                        let cur = if end { e.p.texts[i].end } else { e.p.texts[i].start };
                        if e.set_cap_time(i, end, cur + d) {
                            let s = if end { e.p.texts.iter().find(|t| t.cap && t.end == cur + d).map(|t| t.start) } else { Some(cur + d) };
                            if let Some(s) = s {
                                e.t = s.clamp(0.0, e.total());
                                seek(p, e, true);
                            }
                        }
                        CAPS_SOFT.with(|c| c.set(true));
                    }
                }
            }
            // ----- a piece: Play backwards, transition from the piece before -----
            "rev" => {
                if let Some(i) = e.sel_seg().filter(|&i| !e.p.segs[i].freeze()) {
                    e.push_hist();
                    let s = &mut e.p.segs[i];
                    if val == "1" { s.extra.insert("rev".into(), serde_json::json!(true)); } else { s.extra.remove("rev"); }
                    seek(p, e, true);
                }
            }
            "tr" => {
                if let Some(i) = e.sel_seg().filter(|&i| i > 0) {
                    e.push_hist();
                    let s = &mut e.p.segs[i];
                    if val.is_empty() {
                        s.extra.remove("tr");
                    } else {
                        s.extra.insert("tr".into(), serde_json::json!(val));
                        s.extra.entry("td").or_insert(serde_json::json!(0.5));
                        // Echo: shows it - the playhead just before the piece
                        e.t = (e.p.segs[i].at - 0.3).max(0.0);
                        seek(p, e, true);
                    }
                }
            }
            "td-live" | "td" => {
                if let Some(i) = e.sel_seg() {
                    live(e);
                    e.p.segs[i].extra.insert("td".into(), serde_json::json!((num * 10.0).round() / 10.0));
                    if key == "td" {
                        commit(e);
                    }
                }
            }
            // ----- the text colour picker ("Any colour"): "h,s,v" while dragging, "pick-end" on release -----
            "pick" | "pick-end" => {
                if let Some(i) = e.sel_txt().filter(|&i| !e.p.texts[i].cap) {
                    let mut it = val.split(',').map(|x| x.parse::<f64>().unwrap_or(0.0));
                    let (h, s, v) = (it.next().unwrap_or(0.0), it.next().unwrap_or(0.0), it.next().unwrap_or(1.0));
                    live(e);
                    let hex = hsv_hex(h, s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
                    a.global::<Ed>().set_pick_hex(hex.clone().into());
                    e.p.texts[i].color = hex;
                    if key == "pick-end" {
                        commit(e);
                    }
                }
            }
            "speed" => {
                if let Some(i) = e.sel_seg() {
                    e.set_speed(i, num);
                    seek(p, e, true);
                } else if let Some(i) = e.sel_ov() {
                    e.push_hist();
                    e.p.over[i].speed = num.clamp(0.1, 4.0);
                }
            }
            // the selected layer (Echo's layer panel): Size slider, Full frame / Corner (PiP), Include its sound
            "lay-scale-live" | "lay-scale" => {
                if let Some(i) = e.sel_ov() {
                    live(e);
                    e.p.over[i].scale = num.clamp(0.15, 1.0);
                    if key == "lay-scale" {
                        commit(e);
                    }
                }
            }
            "lay-full" | "lay-pip" => {
                if let Some(i) = e.sel_ov() {
                    e.push_hist();
                    let o = &mut e.p.over[i];
                    (o.x, o.y, o.scale) = if key == "lay-full" { (0.5, 0.5, 1.0) } else { (0.79, 0.77, 0.36) };
                }
            }
            "lay-sound" => {
                if let Some(i) = e.sel_ov() {
                    e.push_hist();
                    e.p.over[i].sound = val == "1";
                }
            }
            "fi-live" | "fo-live" | "fi" | "fo" => {
                if let Some(i) = e.sel_seg() {
                    live(e);
                    let s = &mut e.p.segs[i];
                    if key.starts_with("fi") { s.fi = num } else { s.fo = num }
                    if !key.ends_with("live") {
                        commit(e);
                    }
                } else if let Some(i) = e.sel_aud() {
                    live(e);
                    let a = &mut e.p.audio[i];
                    if key.starts_with("fi") { a.fi = num } else { a.fo = num }
                    if !key.ends_with("live") {
                        commit(e);
                    }
                }
            }
            "aud-vol-live" | "aud-vol" => {
                if let Some(i) = e.sel_aud() {
                    live(e);
                    e.p.audio[i].vol = num.clamp(0.0, 2.0);
                    if key == "aud-vol" {
                        commit(e);
                    }
                }
            }
            "aud-mute" => {
                if let Some(i) = e.sel_aud() {
                    e.push_hist();
                    e.p.audio[i].mute = val == "1";
                }
            }
            "size-live" | "size" => {
                if let Some(i) = e.sel_txt() {
                    live(e);
                    e.p.texts[i].size = num.clamp(0.02, 0.4);
                    if key == "size" {
                        commit(e);
                    }
                }
            }
            "text" => {
                if let Some(i) = e.sel_txt() {
                    live(e);
                    if e.p.texts[i].cap {
                        // a caption: same word count = same word timings (captions list)
                        e.set_cap_text(i, val, None);
                    } else {
                        let t = &mut e.p.texts[i];
                        t.text = val.to_string();
                        t.raw = val.to_string();
                    }
                }
            }
            "color" | "anim" => {
                // a colour: a swatch, or "#rrggbb" typed in the colour picker
                let hex: String = val.trim().trim_start_matches('#').chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_lowercase();
                if let Some(i) = e.sel_txt().filter(|_| key == "anim" || hex.len() == 6) {
                    e.push_hist();
                    if key == "color" {
                        e.p.texts[i].color = hex.clone();
                        a.global::<Ed>().set_pick_hex(hex.clone().into());
                        if let Some((h, s, v)) = hex_hsv(&hex) {
                            let g = a.global::<Ed>();
                            g.set_pick_h(h as f32);
                            g.set_pick_s(s as f32);
                            g.set_pick_v(v as f32);
                        }
                    } else {
                        e.p.texts[i].anim = val.into()
                    }
                }
            }
            "cap-style" => {
                e.push_hist();
                e.apply_cap_style(val);
                e.rechunk_caps();
            }
            "cap-hl" => {
                e.push_hist();
                e.cap_hl = val == "1";
                e.apply_cap_hl();
            }
            "cap-hl-color" => {
                e.push_hist();
                e.cap_hl_color = val.into();
                e.cap_hl = true;
                e.apply_cap_hl();
            }
            "cap-pos" => {
                e.push_hist();
                for t in e.p.texts.iter_mut().filter(|t| t.cap) {
                    t.y = num;
                    t.own = false;
                    t.x = 0.5;
                }
            }
            "cap-lines" => {
                e.push_hist();
                e.p.cap_lines = (num as u32).clamp(1, 3);
                e.rechunk_caps();
                msg = Some(format!("Captions now use up to {} line{}", e.p.cap_lines, if e.p.cap_lines > 1 { "s" } else { "" }));
            }
            "normalize" => {
                e.normalize = val == "1";
                crate::lib_fx::ui_set("ed_normalize", serde_json::json!(e.normalize)); // remembered (Echo)
            }
            "bri-live" | "bri" | "con-live" | "con" | "sat-live" | "sat" => {
                if let Some(i) = e.sel_seg() {
                    live(e);
                    let k = key.trim_end_matches("-live");
                    e.p.segs[i].extra.insert(k.into(), serde_json::json!(num));
                    if !key.ends_with("live") {
                        commit(e);
                    }
                }
            }
            "look" => {
                if let Some(i) = e.sel_seg() {
                    e.push_hist();
                    e.p.segs[i].extra.insert("look".into(), serde_json::json!(val));
                }
            }
            "hold-live" | "hold" => {
                if let Some(i) = e.sel_seg().filter(|&i| e.p.segs[i].freeze()) {
                    live(e);
                    let s = &mut e.p.segs[i];
                    s.end = s.start + num.clamp(0.5, 10.0);
                    if key == "hold" {
                        e.normalize_segs();
                        commit(e);
                    }
                }
            }
            "box-kind" | "box-color" => {
                if let Some(i) = e.sel_box() {
                    e.push_hist();
                    if key == "box-kind" { e.p.boxes[i].kind = val.into() } else { e.p.boxes[i].color = val.into() }
                }
            }
            "box-blur-live" | "box-blur" | "box-opacity-live" | "box-opacity" => {
                if let Some(i) = e.sel_box() {
                    live(e);
                    if key.starts_with("box-blur") { e.p.boxes[i].blur = num.clamp(1.0, 200.0) } else { e.p.boxes[i].opacity = num.clamp(0.1, 1.0) }
                    if !key.ends_with("live") {
                        commit(e);
                    }
                }
            }
            "volume-live" | "volume" => {
                live(e);
                e.p.volume = num.clamp(0.0, 2.0);
                if key == "volume" {
                    commit(e);
                }
                preview_volume(p, e);
            }
            "mute" => {
                e.push_hist();
                e.p.mute = val == "1";
                preview_volume(p, e);
            }
            "zoom" => {
                let mid = zoom_anchor(e);
                let target = 2.0 * 400f64.powf(num.clamp(0.0, 1.0));
                zoom(e, target / e.pps, mid);
            }
            _ => {}
        }
        push(a, e);
    });
    if let Some(m) = msg {
        toast(a, m);
    }
}

#[allow(clippy::too_many_arguments)]
fn key(a: &AppWindow, p: &Rc<RefCell<Player>>, t: &str, ctrl: bool, shift: bool, repeat: bool, root: &Path, play: &Rc<dyn Fn(i32)>) -> bool {
    use slint::platform::Key;
    let is = |k: Key| t == SharedString::from(k).as_str();
    // Ctrl+letter can arrive as a control character (Ctrl+Z = \u{1a})
    let lower = match t.chars().next() {
        Some(c) if ctrl && t.chars().count() == 1 && (1..=26).contains(&(c as u32)) => ((c as u8) + 96).to_string(),
        _ => t.to_lowercase(),
    };
    let cmd: &str = if ctrl && lower == "d" {
        "dup"
    } else if ctrl && lower == "a" {
        "selall"
    } else if ctrl && (lower == "z") {
        if shift { "redo" } else { "undo" }
    } else if ctrl && lower == "y" {
        "redo"
    } else if ctrl {
        return false;
    } else if t == " " {
        if repeat {
            return true;
        }
        "play"
    } else if lower == "s" {
        "split"
    } else if is(Key::Delete) || is(Key::Backspace) {
        if shift { "ripple" } else { "del" }
    } else if lower == "t" {
        "text"
    } else if lower == "c" {
        "cap"
    } else if lower == "b" {
        "box"
    } else if lower == "z" {
        "zoomfx"
    } else if lower == "f" {
        "freeze"
    } else if lower == "u" {
        "audio-sep"
    } else if lower == "i" {
        "rng-in"
    } else if lower == "o" {
        "rng-out"
    } else if lower == "x" {
        "rng-cut"
    } else if is(Key::Escape) {
        if a.global::<Ed>().get_export_menu() {
            a.global::<Ed>().set_export_menu(false);
            return true;
        }
        if a.global::<Ed>().get_pick_open() {
            a.global::<Ed>().set_pick_open(false);
            return true;
        }
        // Echo: Esc only deselects / clears the highlight - the editor stays open (Back leaves it)
        with_ed(|e| {
            e.rng = None;
            e.sel = None;
            e.msel.clear();
            push(a, e);
        });
        return true;
    } else if is(Key::LeftArrow) || is(Key::RightArrow) || is(Key::Home) || is(Key::End) {
        with_ed(|e| {
            let step = if shift { 0.1 } else { 2.0 }; // Echo: arrows 2 s, Shift 0.1 s
            e.t = if is(Key::Home) { 0.0 } else if is(Key::End) { e.total() } else if is(Key::LeftArrow) { e.t - step } else { e.t + step };
            e.t = e.t.clamp(0.0, e.total());
            seek(p, e, true);
            push(a, e);
        });
        return true;
    } else {
        return false;
    };
    act(a, p, cmd, root, play);
    true
}

// ---------- captions ----------
fn captions(a: &AppWindow, p: &Rc<RefCell<Player>>, confirmed: bool) {
    if a.global::<Ed>().get_listening() {
        return;
    }
    let Some(ff) = sheets::ffmpeg() else {
        toast(a, "Captions need ffmpeg (Echo's copy wasn't found)");
        return;
    };
    let fake = std::env::var("ECHO_FAKE_WORDS").is_ok();
    // Echo's captions_ready for the model picked in Settings > Speech model (Fast ~150 MB / Accurate ~470 MB)
    let model = crate::rec_core::rec().map(|r| r.settings().caption_model).unwrap_or_else(|| "base".into());
    if !fake && !confirmed && !echo_core::recorder::captions_ready(&model) {
        let p2 = p.clone();
        let mb = if model == "small" { "about 470 MB" } else { "about 150 MB" };
        confirm_box(a, "Download speech recognition?", &format!("The first time, Echo downloads whisper.cpp and a speech model ({mb}). It runs fully offline on your PC afterwards."), "Download", move |a| captions(a, &p2, true));
        return;
    }
    let Some(clip) = with_ed(|e| {
        if let Some(m) = p.borrow().mpv.as_ref() {
            e.pause(m);
        }
        e.clip.clone()
    }) else { return };
    a.global::<Ed>().set_listening(true);
    let w = a.as_weak();
    std::thread::spawn(move || {
        let (_, _, _, tracks) = crate::edrender::probe(&ff, &clip);
        let r = crate::edrender::transcribe(&ff, &clip, tracks, &|l| {
            let _ = slint::invoke_from_event_loop(move || elog(l));
        });
        let _ = slint::invoke_from_event_loop(move || {
            let Some(a) = w.upgrade() else { return };
            a.global::<Ed>().set_listening(false);
            match r {
                Ok(words) => {
                    let n = with_ed(|e| {
                        if e.clip != clip {
                            return None;
                        }
                        let n = e.set_words(&words);
                        push(&a, e);
                        Some(n)
                    }).flatten();
                    match n {
                        Some(0) => toast(&a, "No speech found (check that your mic was recorded)"),
                        Some(n) => toast(&a, format!("Added {n} captions - click one to edit or change the style")),
                        None => {}
                    }
                }
                Err(e) => toast(&a, e),
            }
        });
    });
}

// ---------- export ----------
fn export(a: &AppWindow, p: &Rc<RefCell<Player>>, preset: &str, root: &Path) {
    // L14: one export at a time (Export range had no busy check - a 2nd click started a 2nd ffmpeg)
    if a.global::<Ed>().get_busy() {
        return;
    }
    let Some(ff) = sheets::ffmpeg() else {
        toast(a, "Export needs ffmpeg (Echo's copy wasn't found)");
        return;
    };
    let range = preset == "range";
    let plan = with_ed(|e| {
        if let Some(m) = p.borrow().mpv.as_ref() {
            e.pause(m);
        }
        if range {
            let Some(rp) = e.range_proj() else { return Err("Highlight a part first".to_string()) };
            let re = e.with_proj(rp);
            e.busy = true;
            return re.export_plan(&ff, "").map(|pl| (pl, re.project_json()));
        }
        e.busy = true;
        e.export_plan(&ff, preset).map(|pl| (pl, e.project_json()))
    });
    let Some(plan) = plan else { return };
    let (plan, proj) = match plan {
        Ok(x) => x,
        Err(x) => {
            with_ed(|e| e.busy = false);
            toast(a, x);
            return;
        }
    };
    a.global::<Ed>().set_busy(true);
    let pb = prog_box(if range { "Exporting the highlighted part…" } else if preset == "tt" { "Exporting for TikTok…" } else if preset == "yt" { "Exporting for YouTube…" } else { "Exporting video…" });
    prog_set(pb, None, None, Some(0.0));
    a.global::<Ed>().set_export_pct(0.0);
    let (w, root2) = (a.as_weak(), root.to_path_buf());
    let tdir = proj_dir();
    let dc = preset == "dc";
    std::thread::spawn(move || {
        let last = std::sync::Mutex::new(Instant::now() - Duration::from_secs(1));
        let t0 = Instant::now();
        let r = crate::edrender::run_export(&plan, &|f| {
            let mut l = last.lock().unwrap();
            if l.elapsed() > Duration::from_millis(200) || f >= 1.0 {
                *l = Instant::now();
                // Echo's exportProgress: "NN% · about N s left" (once it can tell), and the Export button fills
                let secs = t0.elapsed().as_secs_f64();
                let left = (f > 0.03).then(|| (secs * (1.0 - f) / f).round().max(0.0) as u64);
                let txt = match left {
                    Some(s) if s > 2 => format!("{:.0}% · about {} left", f * 100.0, if s < 60 { format!("{s} s") } else { format!("{} min", (s as f64 / 60.0).round()) }),
                    _ => format!("{:.0}%", f * 100.0),
                };
                let w2 = w.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    prog_set(pb, None, Some(&txt), Some((f * 100.0) as f32));
                    if let Some(a) = w2.upgrade() {
                        a.global::<Ed>().set_export_pct((f * 100.0) as f32);
                    }
                });
            }
        }, &|l| {
            let _ = slint::invoke_from_event_loop(move || elog(l));
        });
        // its card picture in the clip cache (<clips>/.thumbs/<name>.jpg, Echo's thumb) + length
        let extra = r.as_ref().ok().map(|out| {
            let _ = std::fs::create_dir_all(&tdir);
            let thumb = tdir.join(format!("{}.jpg", out.file_stem().unwrap_or_default().to_string_lossy()));
            let _ = crate::edrender::hidden_cmd(&plan.ff).args(["-hide_banner", "-loglevel", "error", "-y", "-ss", "0.5", "-i"]).arg(out).args(["-frames:v", "1", "-vf", "scale=480:-2", "-q:v", "4"]).arg(&thumb).stdin(std::process::Stdio::null()).output();
            (thumb, plan.total)
        });
        let _ = slint::invoke_from_event_loop(move || {
            prog_done(pb, 0);
            let Some(a) = w.upgrade() else { return };
            a.global::<Ed>().set_busy(false);
            with_ed(|e| e.busy = false);
            a.global::<Ed>().set_export_pct(0.0);
            match r {
                Ok(out) => {
                    // the project next to the clip cache (Echo's save_project): "Edit" on the export finds it, in
                    // either app
                    let _ = std::fs::create_dir_all(proj_dir());
                    if let Err(x) = std::fs::write(crate::editor::project_path(&proj_dir(), &out), &proj) {
                        elog(format!("editor: couldn't save the project: {x}"));
                    }
                    elog(format!("editor: exported {}", out.display()));
                    let mut new_idx = None;
                    if let (Some(lib), Some((thumb, len))) = (LIB.with(|l| l.borrow().clone()), extra) {
                        if let Some(mut c) = library::clip_from(&out, &root2) {
                            c.dur = Some(len);
                            if thumb.exists() {
                                c.own_thumb = Some(thumb);
                            }
                            new_idx = Some(lib.add(c));
                            lib.refresh();
                        }
                        a.invoke_pick_filter(a.get_filter());
                        crate::UI_SYNC.with(|f| {
                            if let Some(f) = f.borrow().as_ref() {
                                f(true)
                            }
                        });
                    }
                    if range {
                        toast(&a, format!("Exported the highlighted part as \"{}\"", out.file_stem().unwrap_or_default().to_string_lossy()));
                        return;
                    }
                    if let Some(p2) = PL.with(|x| x.borrow().clone()) {
                        close(&a, &p2, true);
                    }
                    // like Echo: the new video opens in the player
                    let row = new_idx.and_then(|i| LIB.with(|l| l.borrow().as_ref().and_then(|l| l.view_row_of(i))));
                    if let (Some(r), Some(play)) = (row, PLAY.with(|x| x.borrow().clone())) {
                        play(r as i32);
                    }
                    if dc {
                        // Discord: it already fits - the finished file goes on the clipboard (Echo's copySized)
                        let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
                        match crate::copy::clipboard_file(&out) {
                            Ok(()) => toast(&a, format!("Copied ({}) - paste it into Discord or anywhere", library::fmt_size(size))),
                            Err(x) => toast(&a, format!("Exported as \"{}\", but {x}", out.file_stem().unwrap_or_default().to_string_lossy())),
                        }
                        return;
                    }
                    // 2.0.2: with Show in folder (the export box turns into this note)
                    let f = out.clone();
                    crate::notes::say_act(
                        crate::notes::Kind::Done,
                        format!("Exported as \"{}\" - press Edit on it later to change the project again", out.file_stem().unwrap_or_default().to_string_lossy()),
                        "Show in folder",
                        move || crate::show_in_folder(&f),
                    );
                }
                Err(x) => toast(&a, format!("Couldn't export: {x}")),
            }
        });
    });
}
