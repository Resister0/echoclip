//! Fullscreen controls drawn BY MPV over the video (Windows window mode: nothing else can draw over mpv's
//! Direct3D window, and the OpenGL path isn't wanted). The prototype paints the bottom band - filmstrip,
//! progress, play head, trim handles, play/volume/fullscreen buttons, time, seek preview - into a picture (tiny-skia,
//! text with ab_glyph) and hands it to mpv with `overlay-add`; mpv blends it on the GPU: really see-through.
//! Slint still does all the mouse handling: its own (invisible) controls sit at exactly the same places.
//!
//! Pixels are premultiplied BGRA (mpv's "bgra"); tiny-skia writes RGBA, so every colour goes in with R and B swapped.
use ab_glyph::{Font, FontVec, ScaleFont};
use slint::{Rgb8Pixel, SharedPixelBuffer};
use std::time::Instant;
use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LineCap, LineJoin, LinearGradient, Mask, Paint, Path, PathBuilder, Pattern, Pixmap, PixmapMut, Point,
    Rect, SpreadMode, Stroke, Transform,
};

/// height of the band mpv draws, in logical px: preview (132 + 10) + gradient 40 + strip 66 + controls 57, rounded up
pub const BAND: f32 = 262.0;
const STRIP: f32 = 66.0;
const CTL: f32 = 57.0;

/// what to draw (logical px unless said otherwise)
#[derive(Clone, PartialEq, Default)]
pub struct State {
    pub w: f32,          // window width
    pub sf: f32,         // scale factor
    pub show: bool,      // controls visible (fade in/out)
    pub progress: f32,   // 0..1 (Slint's gliding value)
    pub paused: bool,
    pub time: String,    // "0:11 / 0:18"
    pub trimming: bool,
    pub t0: f32,
    pub t1: f32,
    pub trim_text: String, // "0:02.6 – 0:09.4"
    pub trim_len: String,  // "· 6.8s"
    pub volume: f32,
    pub muted: bool,
    pub speed: f32, // "SPEED 1.00×"
    pub preview: Option<(f32, f32, f32, String)>, // x, y (window coordinates), fraction, time label
    pub hover_strip: bool,
    // 9 Nov: same look as the normal player - theme accent (progress, volume) + highlight (trim ends),
    // the light lines on the strip, cut-out parts and the Shift+drag highlight (pl_cuts)
    pub accent: (u8, u8, u8),
    pub hi: (u8, u8, u8),
    pub cuts: Vec<(f32, f32)>,
    pub hl: Option<(f32, f32)>,
}

/// RGBA colour stored as BGRA (R and B swapped)
fn c(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::from_rgba8(b, g, r, a)
}
fn solid(col: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(col);
    p.anti_alias = true;
    p
}

fn rrect(x: f32, y: f32, w: f32, h: f32, tl: f32, tr: f32, br: f32, bl: f32) -> Option<Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(x + tl, y);
    pb.line_to(x + w - tr, y);
    pb.quad_to(x + w, y, x + w, y + tr);
    pb.line_to(x + w, y + h - br);
    pb.quad_to(x + w, y + h, x + w - br, y + h);
    pb.line_to(x + bl, y + h);
    pb.quad_to(x, y + h, x, y + h - bl);
    pb.line_to(x, y + tl);
    pb.quad_to(x, y, x + tl, y);
    pb.close();
    pb.finish()
}

pub(crate) fn load_font(names: &[&str]) -> Option<FontVec> {
    let mut cands: Vec<std::path::PathBuf> = vec![];
    if let Ok(f) = std::env::var("ECHO_FONT") {
        cands.push(f.into());
    }
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    for n in names {
        cands.push(std::path::Path::new(&windir).join("Fonts").join(n));
    }
    cands.push("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".into());
    cands.push("/usr/share/fonts/TTF/DejaVuSans.ttf".into());
    cands.push("/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf".into());
    cands.into_iter().find_map(|p| std::fs::read(p).ok().and_then(|b| FontVec::try_from_vec(b).ok()))
}

pub struct Osd {
    font: Option<FontVec>,
    bold: Option<FontVec>,
    mono: Option<FontVec>, // the editor's mono font for the time / speed
    sheet: Option<(Pixmap, f32, f32)>, // BGRA sheet, one picture's size
    sheet_id: usize,
    base: Option<Pixmap>,
    base_key: String,
    bufs: [Vec<u8>; 3],
    cur: usize,
    last_key: String,
    alpha: f32,
    last_t: Instant,
    pub on_screen: bool,
    /// debug (ECHO_DEBUG_OSD): what the last update() did - "sent y=.. WxH a=.." / "removed" / "same"
    pub dbg: String,
    // normal view (flip-model video window): the seek preview as its own small overlay (id 2)
    pv_bufs: [Vec<u8>; 3],
    pv_cur: usize,
    pv_key: String,
    pv_on: bool,
}

impl Osd {
    pub fn new() -> Osd {
        Osd {
            font: load_font(&["segoeui.ttf"]),
            bold: load_font(&["segoeuib.ttf", "seguisb.ttf", "segoeui.ttf"]),
            mono: load_font(&["CascadiaMono.ttf", "consola.ttf", "segoeui.ttf"]),
            sheet: None,
            sheet_id: usize::MAX,
            base: None,
            base_key: String::new(),
            bufs: [vec![], vec![], vec![]],
            cur: 0,
            dbg: String::new(),
            last_key: String::new(),
            alpha: 0.0,
            last_t: Instant::now(),
            on_screen: false,
            pv_bufs: [vec![], vec![], vec![]],
            pv_cur: 0,
            pv_key: String::new(),
            pv_on: false,
        }
    }

    /// the player's 60-picture sheet (10 x 6), as BGRA
    pub fn set_sheet(&mut self, id: usize, px: &SharedPixelBuffer<Rgb8Pixel>, tw: u32, th: u32) {
        if self.sheet_id == id {
            return;
        }
        let (w, h) = (px.width(), px.height());
        let Some(mut pm) = Pixmap::new(w, h) else { return };
        let src = px.as_bytes();
        for (d, s) in pm.data_mut().chunks_exact_mut(4).zip(src.chunks_exact(3)) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = 255;
        }
        self.sheet = Some((pm, tw as f32, th as f32));
        self.sheet_id = id;
        self.base_key.clear();
    }
    pub fn drop_sheet(&mut self) {
        self.sheet = None;
        self.sheet_id = usize::MAX;
        self.base_key.clear();
    }

    /// QA E10 (11 Nov 2026): `size` is a CSS / Slint font size (the em). ab_glyph's scale is the line height
    /// (ascent - descent), ~1.33 em for Segoe UI - so every text here came out ~25 % smaller than Slint's.
    fn em(font: &FontVec, size: f32) -> ab_glyph::PxScale {
        let upem = font.units_per_em().unwrap_or(1000.0).max(1.0);
        ab_glyph::PxScale::from(size * font.height_unscaled() / upem)
    }

    fn text_width(font: &FontVec, size: f32, s: &str) -> f32 {
        let f = font.as_scaled(Self::em(font, size));
        let mut w = 0.0;
        let mut prev = None;
        for ch in s.chars() {
            let id = f.glyph_id(ch);
            if let Some(p) = prev {
                w += f.kern(p, id);
            }
            w += f.h_advance(id);
            prev = Some(id);
        }
        w
    }

    /// draw text (premultiplied BGRA), `y_mid` = vertical centre of the line
    fn text(pm: &mut PixmapMut, font: &FontVec, size: f32, x: f32, y_mid: f32, s: &str, rgb: (u8, u8, u8), a: f32) {
        let size = Self::em(font, size);
        let f = font.as_scaled(size);
        let base = y_mid + (f.ascent() + f.descent()) / 2.0;
        let (w, h) = (pm.width() as i32, pm.height() as i32);
        let data = pm.data_mut();
        let mut cx = x;
        let mut prev = None;
        for ch in s.chars() {
            let id = f.glyph_id(ch);
            if let Some(p) = prev {
                cx += f.kern(p, id);
            }
            let g = id.with_scale_and_position(size, ab_glyph::point(cx, base));
            if let Some(o) = font.outline_glyph(g) {
                let b = o.px_bounds();
                o.draw(|gx, gy, cov| {
                    let px = b.min.x as i32 + gx as i32;
                    let py = b.min.y as i32 + gy as i32;
                    if px < 0 || py < 0 || px >= w || py >= h {
                        return;
                    }
                    let k = cov.clamp(0.0, 1.0) * a;
                    let i = ((py * w + px) * 4) as usize;
                    let inv = 1.0 - k;
                    // BGRA, premultiplied
                    data[i] = (rgb.2 as f32 * k + data[i] as f32 * inv) as u8;
                    data[i + 1] = (rgb.1 as f32 * k + data[i + 1] as f32 * inv) as u8;
                    data[i + 2] = (rgb.0 as f32 * k + data[i + 2] as f32 * inv) as u8;
                    data[i + 3] = (255.0 * k + data[i + 3] as f32 * inv) as u8;
                });
            }
            cx += f.h_advance(id);
            prev = Some(id);
        }
    }

    /// stroke icon from 24x24 polylines / path, centred in a `box_` px square at (x, y)
    fn icon(pm: &mut PixmapMut, path: &Path, x: f32, y: f32, size: f32, fill: bool, sf: f32) {
        let k = size / 24.0;
        let t = Transform::from_scale(k, k).post_translate(x, y);
        let paint = solid(c(255, 255, 255, 255));
        if fill {
            pm.fill_path(path, &paint, FillRule::Winding, t, None);
        } else {
            // the transform scales the stroke too: 1.8 logical px = 1.8 * sf / k in the 24 grid
            let st = Stroke { width: 1.8 * sf / k, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
            pm.stroke_path(path, &paint, &st, t, None);
        }
    }

    fn poly(points: &[&[(f32, f32)]]) -> Option<Path> {
        let mut pb = PathBuilder::new();
        for line in points {
            for (i, &(x, y)) in line.iter().enumerate() {
                if i == 0 {
                    pb.move_to(x, y)
                } else {
                    pb.line_to(x, y)
                }
            }
        }
        pb.finish()
    }

    /// everything that doesn't move every frame
    fn draw_base(&mut self, st: &State, pw: u32, ph: u32) -> Option<Pixmap> {
        let sf = st.sf;
        let mut pm = Pixmap::new(pw, ph)?;
        let s = |v: f32| v * sf;
        let w = st.w;
        let top = BAND - STRIP - CTL; // strip top inside the band (logical)
        {
            let mut m = pm.as_mut();
            // gradient above the strip
            if let Some(g) = LinearGradient::new(
                Point::from_xy(0.0, s(top - 40.0)),
                Point::from_xy(0.0, s(top)),
                vec![GradientStop::new(0.0, c(0, 0, 0, 0)), GradientStop::new(1.0, c(0, 0, 0, 170))],
                SpreadMode::Pad,
                Transform::identity(),
            ) {
                let mut p = Paint::default();
                p.shader = g;
                m.fill_rect(Rect::from_xywh(0.0, s(top - 40.0), s(w), s(40.0))?, &p, Transform::identity(), None);
            }
            // strip + controls background (#000a)
            m.fill_rect(Rect::from_xywh(0.0, s(top), s(w), s(STRIP + CTL))?, &solid(c(0, 0, 0, 170)), Transform::identity(), None);
            // ---- the filmstrip ----
            let (sx, sy, sw, sh) = (s(18.0), s(top + 14.0), s(w - 36.0), s(46.0));
            let mut mask = Mask::new(pw, ph)?;
            mask.fill_path(&rrect(sx, sy, sw, sh, s(10.0), s(10.0), s(10.0), s(10.0))?, FillRule::Winding, true, Transform::identity());
            m.fill_rect(Rect::from_xywh(sx, sy, sw, sh)?, &solid(c(42, 42, 50, 255)), Transform::identity(), Some(&mask));
            if let Some((sheet, tw, th)) = self.sheet.as_ref() {
                let n = ((w - 36.0) / (46.0 * 16.0 / 9.0)).round().max(1.0) as usize;
                let dw = sw / n as f32;
                for i in 0..n {
                    let f = (((i as f32 + 0.5) * 60.0 / n as f32).floor() as usize).min(59);
                    let (x0, y0) = ((f % 10) as f32 * tw, (f / 10) as f32 * th);
                    let dx = sx + i as f32 * dw;
                    let t = Transform::from_translate(-x0, -y0).post_scale(dw / tw, sh / th).post_translate(dx, sy);
                    let mut p = Paint::default();
                    p.shader = Pattern::new(sheet.as_ref(), SpreadMode::Pad, FilterQuality::Bilinear, 1.0, t);
                    if let Some(r) = Rect::from_xywh(dx, sy, dw + 0.5, sh) {
                        m.fill_rect(r, &p, Transform::identity(), Some(&mask));
                    }
                }
            }
            // dimmed so it reads as UI (brighter while the mouse is on it)
            let dim = if st.hover_strip { 0x3b } else { 0x70 };
            m.fill_rect(Rect::from_xywh(sx, sy, sw, sh)?, &solid(c(0, 0, 0, dim)), Transform::identity(), Some(&mask));
            // outside the trim selection
            if st.t0 > 0.0 {
                m.fill_rect(Rect::from_xywh(sx, sy, sw * st.t0, sh)?, &solid(c(0, 0, 0, 0xa0)), Transform::identity(), Some(&mask));
            }
            if st.t1 < 1.0 {
                m.fill_rect(Rect::from_xywh(sx + sw * st.t1, sy, sw * (1.0 - st.t1), sh)?, &solid(c(0, 0, 0, 0xa0)), Transform::identity(), Some(&mask));
            }
            // cut-out parts (player.slint: nearly black, red edges)
            for &(a, b) in &st.cuts {
                let (x0, x1) = (sx + sw * a, sx + sw * b);
                if let Some(r) = Rect::from_xywh(x0, sy, (x1 - x0).max(1.0), sh) {
                    m.fill_rect(r, &solid(c(0, 0, 0, 0xd8)), Transform::identity(), Some(&mask));
                }
                for ex in [x0, x1 - s(1.5)] {
                    if let Some(r) = Rect::from_xywh(ex, sy, s(1.5), sh) {
                        m.fill_rect(r, &solid(c(0xff, 0x5a, 0x5a, 0xcc)), Transform::identity(), Some(&mask));
                    }
                }
            }
            let (hr, hg, hb) = st.hi;
            // the Shift+drag highlight: tinted, lines top + bottom
            if let Some((a, b)) = st.hl {
                let (x0, x1) = (sx + sw * a.min(b), sx + sw * a.max(b));
                let ww = (x1 - x0).max(s(2.0));
                if let Some(r) = Rect::from_xywh(x0, sy, ww, sh) {
                    m.fill_rect(r, &solid(c(hr, hg, hb, 56)), Transform::identity(), Some(&mask));
                }
                for yy in [sy, sy + sh - s(2.0)] {
                    if let Some(r) = Rect::from_xywh(x0, yy, ww, s(2.0)) {
                        m.fill_rect(r, &solid(c(hr, hg, hb, 255)), Transform::identity(), Some(&mask));
                    }
                }
            }
            // the light line along the top + bottom of the kept part, between the yellow ends (not while trimming)
            if !st.trimming {
                let (x0, ww) = (sx + sw * st.t0 + s(12.0), sw * (st.t1 - st.t0) - s(24.0));
                if ww > 0.0 {
                    for yy in [sy, sy + sh - s(2.0)] {
                        if let Some(r) = Rect::from_xywh(x0, yy, ww, s(2.0)) {
                            m.fill_rect(r, &solid(c(255, 255, 255, 0x8c)), Transform::identity(), Some(&mask));
                        }
                    }
                }
            }
            let hi = c(hr, hg, hb, 255);
            if st.trimming {
                let r = rrect(sx + sw * st.t0, sy, sw * (st.t1 - st.t0), sh, 0.0, 0.0, 0.0, 0.0)?;
                let stroke = Stroke { width: s(2.0), ..Default::default() };
                m.stroke_path(&r, &solid(hi), &stroke, Transform::identity(), Some(&mask));
            }
            // trim handles (always there)
            let hw = s(12.0);
            let hcol = c(hr, hg, hb, 204);
            m.fill_path(&rrect(sx + sw * st.t0, sy, hw, sh, s(7.0), 0.0, 0.0, s(7.0))?, &solid(hcol), FillRule::Winding, Transform::identity(), None);
            m.fill_path(&rrect(sx + sw * st.t1 - hw, sy, hw, sh, 0.0, s(7.0), s(7.0), 0.0)?, &solid(hcol), FillRule::Winding, Transform::identity(), None);
            for hx in [sx + sw * st.t0 + s(5.0), sx + sw * st.t1 - hw + s(5.0)] {
                m.fill_rect(Rect::from_xywh(hx, sy + s(14.0), s(2.0), sh - s(28.0))?, &solid(c(0, 0, 0, 0x77)), Transform::identity(), None);
            }
            // outline
            let stroke = Stroke { width: s(1.0), ..Default::default() };
            m.stroke_path(&rrect(sx, sy, sw, sh, s(10.0), s(10.0), s(10.0), s(10.0))?, &solid(c(255, 255, 255, 0x2e)), &stroke, Transform::identity(), None);

            // ---- controls row (11 Nov 2026): the same places as player.slint's bar (padding 12, spacing 4):
            // play 36 | 6 | time ... SPEED 1.00× | 10 | VOL + 10 ticks (71) | 8 | fullscreen 34
            let cy = s(top + STRIP + CTL / 2.0);
            let white = c(255, 255, 255, 255);
            // play: Home's optically centred triangle (22 px in the 36 px button); pause: two 3 px bars, 4 px apart
            if st.paused {
                let mut pb = PathBuilder::new();
                pb.move_to(8.7, 5.5);
                pb.line_to(8.7, 18.5);
                pb.line_to(19.7, 12.0);
                pb.close();
                if let Some(p) = pb.finish() {
                    let k = s(22.0) / 24.0;
                    let t = Transform::from_scale(k, k).post_translate(s(12.0 + 7.0), cy - s(11.0));
                    m.fill_path(&p, &solid(white), FillRule::Winding, t, None);
                    let stroke = Stroke { width: 1.5, line_join: LineJoin::Round, ..Default::default() };
                    m.stroke_path(&p, &solid(white), &stroke, t, None);
                }
            } else {
                for bx in [12.0 + 13.0, 12.0 + 20.0] {
                    m.fill_path(&rrect(s(bx), cy - s(7.0), s(3.0), s(14.0), s(1.0), s(1.0), s(1.0), s(1.0))?, &solid(white), FillRule::Winding, Transform::identity(), None);
                }
            }
            let mono = self.mono.as_ref().or(self.font.as_ref());
            // time: current white, " / total" muted
            if let Some(f) = mono {
                let (cur, rest) = match st.time.split_once(" / ") {
                    Some((c0, d0)) => (c0.to_string(), format!(" / {d0}")),
                    None => (st.time.clone(), String::new()),
                };
                let x0 = s(12.0 + 36.0 + 4.0 + 6.0 + 4.0);
                Self::text(&mut m, f, s(14.0), x0, cy, &cur, (0xff, 0xff, 0xff), 1.0);
                let cw = Self::text_width(f, s(14.0), &cur);
                Self::text(&mut m, f, s(14.0), x0 + cw, cy, &rest, (0xbb, 0xbb, 0xbb), 1.0);
            }
            // small spaced capitals ("SPEED", "VOL"): 10 px semibold, letter spacing 1.4 px
            let caps_w = |t: &str| -> f32 {
                match self.bold.as_ref() {
                    Some(b) => t.chars().map(|ch| Self::text_width(b, s(10.0), &ch.to_string()) + s(1.4)).sum(),
                    None => 0.0,
                }
            };
            let caps = |m: &mut PixmapMut, x: f32, t: &str, rgb: (u8, u8, u8)| {
                if let Some(b) = self.bold.as_ref() {
                    let mut xx = x;
                    for ch in t.chars() {
                        let cs = ch.to_string();
                        Self::text(m, b, s(10.0), xx, cy, &cs, rgb, 1.0);
                        xx += Self::text_width(b, s(10.0), &cs) + s(1.4);
                    }
                }
            };
            // right side, from the right edge (logical px)
            let fs_x = w - 12.0 - 34.0;
            let vol_x = fs_x - 4.0 - 8.0 - 4.0 - 71.0;
            let lbl = if st.muted { "MUTED" } else { "VOL" };
            let lbl_w = caps_w(lbl) / sf + 8.0;
            let lbl_x = vol_x - 4.0 - lbl_w;
            // exit fullscreen: rounded corners (Ic2.unfs), 20 px in the 34 px button
            {
                let mut pb = PathBuilder::new();
                pb.move_to(9.0, 4.0);
                pb.line_to(9.0, 6.5);
                pb.quad_to(9.0, 9.0, 6.5, 9.0);
                pb.line_to(4.0, 9.0);
                pb.move_to(15.0, 4.0);
                pb.line_to(15.0, 6.5);
                pb.quad_to(15.0, 9.0, 17.5, 9.0);
                pb.line_to(20.0, 9.0);
                pb.move_to(20.0, 15.0);
                pb.line_to(17.5, 15.0);
                pb.quad_to(15.0, 15.0, 15.0, 17.5);
                pb.line_to(15.0, 20.0);
                pb.move_to(4.0, 15.0);
                pb.line_to(6.5, 15.0);
                pb.quad_to(9.0, 15.0, 9.0, 17.5);
                pb.line_to(9.0, 20.0);
                if let Some(p) = pb.finish() {
                    Self::icon(&mut m, &p, s(fs_x + 7.0), cy - s(10.0), s(20.0), false, sf);
                }
            }
            // VOL / MUTED (accent while muted)
            let (ar, ag, ab) = st.accent;
            caps(&mut m, s(lbl_x + 4.0), lbl, if st.muted { (ar, ag, ab) } else { (0xbb, 0xbb, 0xbb) });
            // 10 slanted ticks (4 x 12, skewed): dim, then the lit part cut at the volume (faint while muted)
            let tick = |i: usize| -> Option<Path> {
                let x = vol_x + i as f32 * 7.0;
                Self::poly(&[&[(s(x + 3.9), cy - s(6.0)), (s(x + 7.9), cy - s(6.0)), (s(x + 4.0), cy + s(6.0)), (s(x), cy + s(6.0)), (s(x + 3.9), cy - s(6.0))]])
            };
            for i in 0..10 {
                if let Some(p) = tick(i) {
                    m.fill_path(&p, &solid(c(255, 255, 255, 46)), FillRule::Winding, Transform::identity(), None);
                }
            }
            let v = st.volume.clamp(0.0, 1.0);
            if v > 0.0 {
                let mut vm = Mask::new(pw, ph)?;
                if let Some(r) = Rect::from_xywh(s(vol_x), 0.0, s(71.0 * v), ph as f32) {
                    vm.fill_path(&PathBuilder::from_rect(r), FillRule::Winding, false, Transform::identity());
                }
                let lit = if st.muted { c(255, 255, 255, 82) } else { c(255, 255, 255, 235) };
                for i in 0..10 {
                    if let Some(p) = tick(i) {
                        m.fill_path(&p, &solid(lit), FillRule::Winding, Transform::identity(), Some(&vm));
                    }
                }
            }
            // SPEED 1.00× (not while trimming - the trim times sit there)
            if !st.trimming {
                if let Some(f) = mono {
                    let val = format!("{:.2}×", st.speed);
                    let vw_ = Self::text_width(f, s(13.0), &val);
                    let sp_w = caps_w("SPEED") + s(6.0) + vw_; // content width (physical)
                    let box_w = (sp_w / sf + 16.0).max(34.0);
                    let box_x = lbl_x - 4.0 - 10.0 - 4.0 - box_w;
                    let x0 = s(box_x) + (s(box_w) - sp_w) / 2.0;
                    caps(&mut m, x0, "SPEED", (0xbb, 0xbb, 0xbb));
                    let col = if (st.speed - 1.0).abs() > 0.001 { (ar, ag, ab) } else { (0xff, 0xff, 0xff) };
                    Self::text(&mut m, f, s(13.0), x0 + caps_w("SPEED") + s(6.0), cy, &val, col, 1.0);
                }
            }
            let mute_x = lbl_x; // the trim times end before the volume
            // trim selection times, right-aligned before the speaker
            if st.trimming {
                if let (Some(b), Some(f)) = (self.bold.as_ref(), self.font.as_ref()) {
                    let lw = Self::text_width(f, s(12.5), &st.trim_len);
                    let tw = Self::text_width(b, s(12.5), &st.trim_text);
                    let end = s(mute_x - 12.0);
                    Self::text(&mut m, f, s(12.5), end - lw, cy, &st.trim_len, (0xa0, 0xa0, 0xab), 1.0);
                    Self::text(&mut m, b, s(12.5), end - lw - s(8.0) - tw, cy, &st.trim_text, (0xf5, 0xf5, 0xf7), 1.0);
                }
            }
        }
        Some(pm)
    }

    /// Draw + hand to mpv when something changed. Returns true while fading (wants another frame soon).
    /// `win_h` = window height in physical px (the band sits at the bottom).
    pub fn update(&mut self, m: &crate::mpv::Mpv, st: &State, win_h: u32) -> bool {
        let now = Instant::now();
        let dt = (now - self.last_t).as_secs_f32().min(0.1);
        self.last_t = now;
        let target = if st.show { 1.0 } else { 0.0 };
        if self.alpha < target {
            self.alpha = (self.alpha + dt / 0.25).min(1.0);
        } else if self.alpha > target {
            self.alpha = (self.alpha - dt / 0.25).max(0.0);
        }
        self.dbg = "same".into();
        if self.alpha <= 0.0 {
            self.dbg = "off".into();
            if self.on_screen {
                self.dbg = "removed".into();
                m.command(&["overlay-remove", "1"]);
                self.on_screen = false;
                self.last_key.clear();
            }
            return false;
        }
        let sf = st.sf;
        let pw = (st.w * sf).round().max(1.0) as u32;
        let ph = (BAND * sf).round().max(1.0) as u32;
        // static part
        let base_key = format!(
            "{pw}x{ph} {} {} {} {:.4} {:.4} {} {} {:.3} {} {} {} {} {:?} {:?} {:?} {:?} {:.2}",
            self.sheet_id, st.trimming, st.paused, st.t0, st.t1, st.time, st.trim_text, st.volume, st.muted, st.hover_strip, st.trim_len, sf,
            st.accent, st.hi, st.cuts, st.hl, st.speed
        );
        if base_key != self.base_key || self.base.is_none() {
            self.base = self.draw_base(st, pw, ph);
            self.base_key = base_key.clone();
        }
        let Some(base) = self.base.as_ref() else { return false };
        // moving part: progress + play head + preview
        let top = BAND - STRIP - CTL;
        let (sx, sw) = (18.0 * sf, (st.w - 36.0) * sf);
        let head = (sx + sw * st.progress.clamp(0.0, 1.0)).round() as i32;
        // win_h too: a maximized window going fullscreen keeps its width, only the band's y changes
        let key = format!("{base_key} {win_h} {head} {:?} {:.2}", st.preview.as_ref().map(|p| ((p.0 * sf) as i32, (p.1 * sf) as i32, (p.2 * 60.0) as i32)), self.alpha);
        if key == self.last_key && self.on_screen {
            return self.alpha < target || self.alpha > target;
        }
        self.last_key = key;
        self.cur = (self.cur + 1) % 3;
        let buf = &mut self.bufs[self.cur];
        buf.clear();
        buf.extend_from_slice(base.data());
        let Some(mut pm) = PixmapMut::from_bytes(buf, pw, ph) else { return false };
        let (sy, sh) = ((top + 14.0) * sf, 46.0 * sf);
        let mut mask = Mask::new(pw, ph);
        if let (Some(mk), Some(r)) = (mask.as_mut(), rrect(sx, sy, sw, sh, 10.0 * sf, 10.0 * sf, 10.0 * sf, 10.0 * sf)) {
            mk.fill_path(&r, FillRule::Winding, true, Transform::identity());
        }
        if let Some(r) = Rect::from_xywh(sx, sy + sh - 4.0 * sf, (head as f32 - sx).max(0.0), 4.0 * sf) {
            pm.fill_rect(r, &solid(c(st.accent.0, st.accent.1, st.accent.2, 255)), Transform::identity(), mask.as_ref());
        }
        if let Some(r) = Rect::from_xywh(head as f32 - 1.5 * sf, sy, 3.0 * sf, sh) {
            pm.fill_rect(r, &solid(c(255, 255, 255, 255)), Transform::identity(), mask.as_ref());
        }
        // seek preview (#ptip): 228 x 132, white border, time at the bottom
        if let (Some((px, py, frac, label)), Some((sheet, tw, th))) = (st.preview.as_ref(), self.sheet.as_ref()) {
            let (x, y, w, h) = (px * sf, py * sf, 228.0 * sf, 132.0 * sf);
            if let Some(r) = rrect(x, y, w, h, 9.0 * sf, 9.0 * sf, 9.0 * sf, 9.0 * sf) {
                pm.fill_path(&r, &solid(c(255, 255, 255, 0xd9)), FillRule::Winding, Transform::identity(), None);
            }
            let f = ((frac * 60.0).floor() as usize).min(59);
            let (x0, y0) = ((f % 10) as f32 * tw, (f / 10) as f32 * th);
            let (ix, iy, iw, ih) = (x + 2.0 * sf, y + 2.0 * sf, w - 4.0 * sf, h - 4.0 * sf);
            let t = Transform::from_translate(-x0, -y0).post_scale(iw / tw, ih / th).post_translate(ix, iy);
            let mut p = Paint::default();
            p.shader = Pattern::new(sheet.as_ref(), SpreadMode::Pad, FilterQuality::Bilinear, 1.0, t);
            if let (Some(rr), Some(mut mk)) = (rrect(ix, iy, iw, ih, 7.0 * sf, 7.0 * sf, 7.0 * sf, 7.0 * sf), Mask::new(pw, ph)) {
                mk.fill_path(&rr, FillRule::Winding, true, Transform::identity());
                if let Some(r) = Rect::from_xywh(ix, iy, iw, ih) {
                    pm.fill_rect(r, &p, Transform::identity(), Some(&mk));
                }
            }
            if let Some(b) = self.bold.as_ref() {
                let lw = Self::text_width(b, 12.5 * sf, label);
                Self::text(&mut pm, b, 12.5 * sf, x + (w - lw) / 2.0 + sf, y + h - 14.0 * sf + sf, label, (0, 0, 0), 0.8);
                Self::text(&mut pm, b, 12.5 * sf, x + (w - lw) / 2.0, y + h - 14.0 * sf, label, (255, 255, 255), 1.0);
            }
        }
        // fade
        if self.alpha < 1.0 {
            let a = self.alpha;
            for v in pm.data_mut() {
                *v = (*v as f32 * a) as u8;
            }
        }
        let addr = buf.as_ptr() as usize;
        let y = win_h.saturating_sub(ph);
        m.command(&[
            "overlay-add",
            "1",
            "0",
            &y.to_string(),
            &format!("&{addr}"),
            "0",
            "bgra",
            &pw.to_string(),
            &ph.to_string(),
            &(pw * 4).to_string(),
        ]);
        self.on_screen = true;
        self.dbg = format!("sent y={y} {pw}x{ph} a={:.2}", self.alpha);
        self.alpha != target
    }

    /// mpv was restarted: its overlays are gone
    pub fn forget(&mut self) {
        self.on_screen = false;
        self.last_key.clear();
        self.pv_on = false;
        self.pv_key.clear();
    }

    pub fn hide(&mut self, m: &crate::mpv::Mpv) {
        if self.on_screen {
            m.command(&["overlay-remove", "1"]);
            self.on_screen = false;
        }
        self.alpha = 0.0;
        self.last_key.clear();
    }
}

impl Osd {
    /// Normal view: the seek preview drawn BY MPV where it overlaps the video (mpv's flip-model Direct3D window is
    /// always on top of Slint, so Slint's own preview is only visible outside the video; both are drawn at the same
    /// spot). `p` = x, y relative to the video (logical px), fraction, time label; `sheet` = the player's sheet
    /// (RGB, 10 x 6 pictures of tw x th); `vw`/`vh` = video window size in physical px. Only the needed picture is
    /// converted (no 14 MB BGRA copy of the sheet).
    pub fn update_preview(&mut self, m: &crate::mpv::Mpv, p: Option<(f32, f32, f32, String)>, sheet: Option<&(usize, SharedPixelBuffer<Rgb8Pixel>, u32, u32)>, sf: f32, vw: i32, vh: i32) {
        let (Some((x, y, frac, label)), Some((sid, sp, tw, th))) = (p, sheet) else {
            self.hide_preview(m);
            return;
        };
        let f = ((frac * 60.0).floor() as usize).min(59);
        let (pw, ph) = ((228.0 * sf).round() as i32, (132.0 * sf).round() as i32);
        let (ox, oy) = ((x * sf).round() as i32, (y * sf).round() as i32);
        // part of the preview inside the video window
        let (c0, r0) = ((-ox).max(0), (-oy).max(0));
        let (c1, r1) = ((vw - ox).min(pw), (vh - oy).min(ph));
        if c1 <= c0 || r1 <= r0 {
            self.hide_preview(m);
            return;
        }
        let key = format!("{sid} {f} {ox} {oy} {vw} {vh} {pw} {label}");
        if key == self.pv_key && self.pv_on {
            return;
        }
        // the one picture, as a small BGRA pixmap
        let (tw, th) = (*tw, *th);
        let Some(mut tile) = Pixmap::new(tw, th) else { return };
        let (x0, y0) = ((f % 10) as u32 * tw, (f / 10) as u32 * th);
        let src = sp.as_bytes();
        let sw = sp.width();
        if x0 + tw > sw || y0 + th > sp.height() {
            return;
        }
        for row in 0..th {
            let s0 = (((y0 + row) * sw + x0) * 3) as usize;
            let d0 = (row * tw * 4) as usize;
            let (s, d) = (&src[s0..s0 + tw as usize * 3], &mut tile.data_mut()[d0..d0 + tw as usize * 4]);
            for (d, s) in d.chunks_exact_mut(4).zip(s.chunks_exact(3)) {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
                d[3] = 255;
            }
        }
        let Some(mut pm) = Pixmap::new(pw as u32, ph as u32) else { return };
        let (w, h) = (pw as f32, ph as f32);
        if let Some(r) = rrect(0.0, 0.0, w, h, 9.0 * sf, 9.0 * sf, 9.0 * sf, 9.0 * sf) {
            pm.fill_path(&r, &solid(c(255, 255, 255, 0xd9)), FillRule::Winding, Transform::identity(), None);
        }
        let (ix, iy, iw, ih) = (2.0 * sf, 2.0 * sf, w - 4.0 * sf, h - 4.0 * sf);
        let t = Transform::from_scale(iw / tw as f32, ih / th as f32).post_translate(ix, iy);
        let mut paint = Paint::default();
        paint.shader = Pattern::new(tile.as_ref(), SpreadMode::Pad, FilterQuality::Bilinear, 1.0, t);
        if let (Some(rr), Some(mut mk)) = (rrect(ix, iy, iw, ih, 7.0 * sf, 7.0 * sf, 7.0 * sf, 7.0 * sf), Mask::new(pw as u32, ph as u32)) {
            mk.fill_path(&rr, FillRule::Winding, true, Transform::identity());
            if let Some(r) = Rect::from_xywh(ix, iy, iw, ih) {
                pm.fill_rect(r, &paint, Transform::identity(), Some(&mk));
            }
        }
        if let Some(b) = self.bold.as_ref() {
            let mut pmm = pm.as_mut();
            let lw = Self::text_width(b, 12.5 * sf, &label);
            Self::text(&mut pmm, b, 12.5 * sf, (w - lw) / 2.0 + sf, h - 14.0 * sf + sf, &label, (0, 0, 0), 0.8);
            Self::text(&mut pmm, b, 12.5 * sf, (w - lw) / 2.0, h - 14.0 * sf, &label, (255, 255, 255), 1.0);
        }
        self.pv_key = key;
        self.pv_cur = (self.pv_cur + 1) % 3;
        let buf = &mut self.pv_bufs[self.pv_cur];
        buf.clear();
        buf.extend_from_slice(pm.data());
        let addr = buf.as_ptr() as usize + ((r0 * pw + c0) * 4) as usize;
        m.command(&[
            "overlay-add",
            "2",
            &(ox + c0).to_string(),
            &(oy + r0).to_string(),
            &format!("&{addr}"),
            "0",
            "bgra",
            &(c1 - c0).to_string(),
            &(r1 - r0).to_string(),
            &(pw * 4).to_string(),
        ]);
        self.pv_on = true;
    }

    pub fn hide_preview(&mut self, m: &crate::mpv::Mpv) {
        if self.pv_on {
            m.command(&["overlay-remove", "2"]);
            self.pv_on = false;
        }
        self.pv_key.clear();
    }
}

impl Default for Osd {
    fn default() -> Self {
        Self::new()
    }
}
