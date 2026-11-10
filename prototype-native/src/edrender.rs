//! Editor part 2: the ASS overlay mpv draws over the preview, mouse on the video, playback over the piece list,
//! and the export (ffmpeg - same filter graph as Echo's recorder.rs render_edit: pieces, other files, layers,
//! zoom / blur / colour boxes, stickers, texts, separated audio + layer sound).
use crate::editor::{ass_k, fmt_t, text_em, BoxFx, Drag, Editor, Ovl, Sel, Txt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

thread_local! {
    /// last mouse-wheel notch on a layer (one undo step per wheel burst)
    static WHEEL_AT: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// "RRGGBB" -> ASS "&HBBGGRR&"
pub fn ass_color(hex: &str) -> String {
    let h: String = hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let h = if h.len() == 6 { h } else { "FFFFFF".into() };
    format!("&H{}{}{}&", &h[4..6], &h[2..4], &h[0..2]).to_uppercase()
}

pub fn ass_escape(s: &str) -> String {
    s.replace('\\', "/").replace('{', "(").replace('}', ")").replace('\r', "").replace('\n', "\\N")
}

fn rect(x: f64, y: f64, w: f64, h: f64, col: &str, a: u8) -> String {
    format!("{{\\an7\\pos({x:.1},{y:.1})\\p1\\bord0\\shad0\\1c{col}\\1a&H{a:02X}&}}m 0 0 l {w:.1} 0 {w:.1} {h:.1} 0 {h:.1}{{\\p0}}")
}

fn outline(x: f64, y: f64, w: f64, h: f64, col: &str, th: f64) -> String {
    format!("{{\\an7\\pos({x:.1},{y:.1})\\p1\\1a&HFF&\\bord{th:.1}\\3c{col}\\3a&H00&\\shad0}}m 0 0 l {w:.1} 0 {w:.1} {h:.1} 0 {h:.1}{{\\p0}}")
}

/// 0..1 fade level of a piece at output time t
pub fn fade_gain(at: f64, len: f64, fi: f64, fo: f64, t: f64) -> f64 {
    let mut k: f64 = 1.0;
    let (a, b) = (t - at, at + len - t);
    if fi > 0.04 && a < fi {
        k = k.min((a / fi).max(0.0));
    }
    if fo > 0.04 && b < fo {
        k = k.min((b / fo).max(0.0));
    }
    k
}

/// animation state at time t (same formulas as Echo's preview): (opacity, scale x, scale y, y offset as part of h)
fn anim_state(t: &Txt, now: f64) -> (f64, f64, f64, f64) {
    if t.anim.is_empty() || now < t.start || now > t.end {
        return (1.0, 1.0, 1.0, 0.0);
    }
    let fin = 0.25f64.min((t.end - t.start) / 3.0).max(0.01);
    let p = ((now - t.start) / fin).clamp(0.0, 1.0);
    let q = ((t.end - now) / fin).clamp(0.0, 1.0);
    match t.anim.as_str() {
        "fade" => (p.min(q), 1.0, 1.0, 0.0),
        "pop" => {
            let s = if p < 2.0 / 3.0 { 0.2 + 0.92 * p * 1.5 } else { 1.12 - 0.12 * (p - 2.0 / 3.0) * 3.0 };
            (1.0, s, s, 0.0)
        }
        "slide" => (p, 1.0, 1.0, (1.0 - p) * 0.06),
        "bounce" => {
            if p < 0.5 {
                (1.0, 1.3 - 0.8 * p, 0.4 + 1.6 * p, 0.0)
            } else {
                (1.0, 0.9 + 0.2 * (p - 0.5), 1.2 - 0.4 * (p - 0.5), 0.0)
            }
        }
        _ => (1.0, 1.0, 1.0, 0.0),
    }
}

/// size of a text on screen: (width, height) for font size `px` (em)
pub fn text_size(t: &Txt, px: f64) -> (f64, f64) {
    let lines: Vec<&str> = t.text.split('\n').collect();
    let w = lines.iter().map(|l| text_em(l)).fold(0.0, f64::max) * px;
    (w.max(px * 0.6), lines.len() as f64 * px * ass_k())
}

impl Editor {
    /// the caption box (when a caption is selected, or always with "Show caption box"): (x, y, w, h, line height)
    /// in OSD px, centred like Echo's
    pub fn cap_box_rect(&self) -> Option<(f64, f64, f64, f64, f64)> {
        let i = self.cap_ref()?;
        let p = &self.p.texts[i];
        let g = self.geo;
        let (fx, fw, fh) = g.frame(self.p.vertical);
        let line_h = p.size * fh * 1.2;
        let (w, h) = (self.p.cap_w * fw, self.p.cap_lines as f64 * line_h + 8.0);
        let (x, y) = (fx + p.x * fw, g.cy + p.y * fh);
        Some((x - w / 2.0, y - h / 2.0, w, h, line_h))
    }

    /// a text's box on the video (OSD px)
    pub fn text_rect(&self, t: &Txt) -> (f64, f64, f64, f64) {
        let g = self.geo;
        let (fx, fw, fh) = g.frame(self.p.vertical);
        let px = t.size * fh;
        let (w, h) = text_size(t, px);
        let (x, y) = (fx + t.x * fw, g.cy + t.y * fh);
        let pad = 6.0 * g.sf;
        (x - w / 2.0 - pad, y - h / 2.0 - pad, w + 2.0 * pad, h + 2.0 * pad)
    }

    /// a box's rectangle on the video (OSD px) - boxes use the full picture, not the 9:16 frame
    pub fn box_rect(&self, b: &BoxFx) -> (f64, f64, f64, f64) {
        let g = self.geo;
        let (w, h) = (b.w * g.cw, b.h * g.ch);
        (g.cx + b.x * g.cw - w / 2.0, g.cy + b.y * g.ch - h / 2.0, w, h)
    }

    /// a layer's rectangle on the video (OSD px): centred at x / y of the picture, `scale` of its width, its own
    /// shape (Echo's .eovv: left / top = x / y, width = scale, height auto)
    pub fn layer_rect(&self, o: &Ovl) -> (f64, f64, f64, f64) {
        let g = self.geo;
        let w = o.scale * g.cw;
        let h = w / crate::pl_layer::aspect(self, o).max(0.05);
        (g.cx + o.x * g.cw - w / 2.0, g.cy + o.y * g.ch - h / 2.0, w, h)
    }

    /// the layer shows at the playhead
    pub fn layer_on(&self, o: &Ovl) -> bool {
        self.t >= o.at && self.t < o.at + o.len()
    }

    /// sticker boxes whose file exists (export inputs, in this order)
    pub fn sticker_boxes(&self) -> Vec<&BoxFx> {
        self.p.boxes.iter().filter(|b| b.kind == "image" && b.end - b.start >= 0.05 && b.w > 0.0 && b.h > 0.0
            && b.extra.get("src").and_then(|x| x.as_str()).is_some_and(|s| !s.is_empty() && Path::new(s).exists())).collect()
    }

    /// blur boxes in the preview: a real blur by mpv (ffmpeg's crop + gblur + overlay on the decoded frames, same
    /// as the export). The boxes are in output time, the filter sees source time: each box is enabled for the source
    /// times its span covers.
    fn blur_vf(&self) -> String {
        let (w, h) = (self.w.max(16) as f64, self.h.max(16) as f64);
        let mut g: Vec<String> = vec![];
        let mut last = String::new();
        let blurs: Vec<&BoxFx> = self.p.boxes.iter().filter(|b| b.kind == "blur" && b.end - b.start >= 0.05 && b.w > 0.0 && b.h > 0.0).collect();
        for (i, b) in blurs.iter().enumerate() {
            let mut en: Vec<String> = vec![];
            for s in self.p.segs.iter().filter(|s| !s.freeze() && !s.special()) {
                let (a0, a1) = (b.start.max(s.at), b.end.min(s.at + s.len()));
                if a1 - a0 > 0.01 {
                    en.push(format!("between(t,{:.3},{:.3})", s.start + (a0 - s.at) * s.speed, s.start + (a1 - s.at) * s.speed));
                }
            }
            if en.is_empty() {
                continue;
            }
            let bw = ((b.w.min(1.0) * w).round() as i64).max(4) / 2 * 2;
            let bh = ((b.h.min(1.0) * h).round() as i64).max(4) / 2 * 2;
            let bx = (((b.x - b.w / 2.0).max(0.0) * w).round() as i64).min(w as i64 - bw).max(0);
            let by = (((b.y - b.h / 2.0).max(0.0) * h).round() as i64).min(h as i64 - bh).max(0);
            let src = if last.is_empty() { String::new() } else { format!("[{last}]") };
            g.push(format!("{src}split[m{i}][s{i}]"));
            g.push(format!("[s{i}]crop={bw}:{bh}:{bx}:{by},gblur=sigma={:.1}[b{i}]", b.blur.clamp(1.0, 200.0)));
            g.push(format!("[m{i}][b{i}]overlay={bx}:{by}:enable='{}'[o{i}]", en.join("+")));
            last = format!("o{i}");
        }
        if g.is_empty() {
            return String::new();
        }
        // the last pad stays unlabelled = the filter's output
        let mut all = g.join(";");
        if let Some(p) = all.rfind(&format!("[{last}]")) {
            all.truncate(p);
        }
        format!("lavfi=[{all}]")
    }

    /// separated audio in the preview: mpv mixes the clip's own tracks (lavfi-complex). The main sound is silenced
    /// where its piece's audio was separated; each row plays its track with its volume for the source times it
    /// covers (a row moved away from its picture still plays at its own source time - the export places it right).
    pub fn audio_lavfi(&self) -> String {
        let tracks = self.tracks;
        let items: Vec<&crate::editor::Aud> = self.p.audio.iter().filter(|a| !a.mute && !a.other_file() && a.track < tracks && a.end - a.start >= 0.05).collect();
        let amute: Vec<&crate::editor::Seg> = self.p.segs.iter().filter(|s| s.extra.get("amute").and_then(|x| x.as_bool()) == Some(true)).collect();
        if (items.is_empty() && amute.is_empty()) || tracks == 0 {
            return String::new();
        }
        let win = |a: f64, b: f64| format!("between(t,{a:.3},{b:.3})");
        let main_off: Vec<String> = amute.iter().map(|s| win(s.start, s.end)).collect();
        let main_expr = if main_off.is_empty() { "1".to_string() } else { format!("if(gt({},0),0,1)", main_off.join("+")) };
        let mut by_track: std::collections::BTreeMap<u32, Vec<String>> = Default::default();
        for a in &items {
            by_track.entry(a.track).or_default().push(format!("{:.3}*{}", a.vol.clamp(0.0, 4.0), win(a.start, a.end)));
        }
        let mut g: Vec<String> = vec!["[vid1]null[vo]".into()];
        let uses0 = by_track.contains_key(&0);
        if uses0 {
            g.push("[aid1]asplit=2[am0][ax0]".into());
            g.push(format!("[am0]volume='{main_expr}':eval=frame[m]"));
        } else {
            g.push(format!("[aid1]volume='{main_expr}':eval=frame[m]"));
        }
        let mut ins = String::from("[m]");
        for (k, parts) in &by_track {
            let src = if *k == 0 { "[ax0]".to_string() } else { format!("[aid{}]", k + 1) };
            g.push(format!("{src}volume='{}':eval=frame[t{k}]", parts.join("+")));
            ins += &format!("[t{k}]");
        }
        g.push(format!("{ins}amix=inputs={}:normalize=0:duration=first[ao]", by_track.len() + 1));
        g.join(";")
    }

    /// colour + zoom of the preview at the playhead (mpv's video equalizer + video-zoom / pan); a selected zoom box
    /// shows the whole picture (so its area can be placed), like Echo
    pub fn preview_fx(&mut self, m: &crate::mpv::Mpv) {
        let mut fx = [0.0f64; 7];
        fx[4] = 0.0;
        if let Some(s) = self.p.segs.get(self.cur).filter(|_| !self.gap) {
            let (bri, con, sat) = (s.num("bri", 0.0), s.num("con", 1.0), s.num("sat", 1.0));
            let (mut b, mut c, mut sa, mut ga) = (bri * 50.0, (con - 1.0) * 100.0, (sat - 1.0) * 100.0, 0.0f64);
            match s.look().as_str() {
                "vivid" => { c += 8.0; sa += 35.0; }
                "punch" => { c += 20.0; sa += 15.0; ga += 5.0; }
                "bw" => { sa = -100.0; c += 10.0; }
                "cinema" => { c += 12.0; sa -= 15.0; }
                "warm" => { sa += 12.0; b += 2.0; }
                "cool" => { sa -= 5.0; b += 2.0; }
                "vintage" => { sa -= 35.0; c -= 10.0; b += 5.0; }
                _ => {}
            }
            fx[0] = b.clamp(-100.0, 100.0).round();
            fx[1] = c.clamp(-100.0, 100.0).round();
            fx[2] = sa.clamp(-100.0, 100.0).round();
            fx[3] = ga.clamp(-100.0, 100.0).round();
        }
        let zoom_sel = self.sel_box().is_some_and(|i| self.p.boxes[i].kind == "zoom");
        if !zoom_sel {
            let (mut z, mut cx, mut cy) = (1.0f64, 0.5f64, 0.5f64);
            for b in self.p.boxes.iter().filter(|b| b.kind == "zoom" && b.end - b.start >= 0.2 && b.w > 0.0 && b.h > 0.0) {
                let zf = (1.0 / b.w.max(b.h)).clamp(1.0, 5.0);
                let r = 0.4f64.min((b.end - b.start) / 2.0);
                let ramp = ((self.t - b.start) / r).min((b.end - self.t) / r).clamp(0.0, 1.0);
                z += (zf - 1.0) * ramp;
                cx += (b.x.clamp(0.0, 1.0) - 0.5) * ramp;
                cy += (b.y.clamp(0.0, 1.0) - 0.5) * ramp;
            }
            if z > 1.001 {
                let lim = (1.0 - 1.0 / z) / 2.0;
                fx[4] = z.log2();
                fx[5] = (0.5 - cx).clamp(-lim, lim);
                fx[6] = (0.5 - cy).clamp(-lim, lim);
            }
        }
        let vf = if self.loaded.is_some() { String::new() } else { self.blur_vf() };
        if vf != self.last_vf {
            if !vf.is_empty() && !self.hw_copied {
                // filters work on frames in memory: hardware decoding switches to its copy-back form while editing
                let hw = m.get("hwdec-current").unwrap_or_default();
                if !hw.is_empty() && hw != "no" && !hw.ends_with("-copy") {
                    m.set("hwdec", "auto-copy-safe");
                }
                self.hw_copied = true;
            }
            // Linux: Echo's base filter (crate::mpv::base_vf) stays last
            let full = crate::mpv::with_base_vf(&vf, self.hw_copied);
            if full.is_empty() {
                m.command(&["vf", "clr", ""]);
            } else {
                m.command(&["vf", "set", &full]);
            }
            self.last_vf = vf;
        }
        if fx != self.last_fx {
            let names = ["brightness", "contrast", "saturation", "gamma", "video-zoom", "video-pan-x", "video-pan-y"];
            for k in 0..7 {
                if fx[k] != self.last_fx[k] {
                    m.set(names[k], &format!("{:.4}", fx[k]));
                }
            }
            self.last_fx = fx;
        }
    }

    fn text_event(&self, t: &Txt, now: f64, dim: bool) -> String {
        let g = self.geo;
        let (fx, fw, fh) = g.frame(self.p.vertical);
        let px = t.size * fh;
        let fs = px * ass_k();
        // Echo's preview: text-shadow 0 0 px/14 #000 = a soft dark glow, not a hard outline
        let bord = (px / 28.0).max(1.0);
        let blur = (px / 14.0).max(2.0);
        let (mut op, sx, sy, dy) = anim_state(t, now);
        if dim {
            op = 0.45;
        }
        let a1 = ((1.0 - op) * 255.0).round().clamp(0.0, 255.0) as u8;
        let a3 = (0x50 as f64 + (255.0 - 0x50 as f64) * (1.0 - op)).round().clamp(0.0, 255.0) as u8;
        let base = ass_color(&t.color);
        let (x, y) = (fx + t.x * fw, g.cy + (t.y + dy) * fh);
        let head = format!(
            "{{\\an5\\pos({x:.1},{y:.1})\\fnSegoe UI\\b1\\fs{fs:.1}\\bord{bord:.1}\\blur{blur:.1}\\shad0\\1c{base}\\3c&H000000&\\1a&H{a1:02X}&\\3a&H{a3:02X}&\\fscx{:.0}\\fscy{:.0}\\q2}}",
            sx * 100.0,
            sy * 100.0
        );
        let body = if !t.words.is_empty() && !t.hl.is_empty() {
            let hl = ass_color(&t.hl);
            let mut b = String::new();
            for (k, w) in t.words.iter().enumerate() {
                if k > 0 {
                    b += if w.nl { "\\N" } else { " " };
                }
                let tx = ass_escape(&w.text);
                if now >= w.start && now < w.end {
                    b += &format!("{{\\1c{hl}}}{tx}{{\\1c{base}}}");
                } else {
                    b += &tx;
                }
            }
            b
        } else if t.anim == "type" && now >= t.start && now <= t.end {
            let chars: Vec<char> = t.text.chars().collect();
            let n = chars.iter().filter(|c| !c.is_whitespace()).count().max(1);
            let step = 0.06f64.min((t.end - t.start) * 0.6 / n as f64).max(0.001);
            let shown = ((now - t.start) / step).floor() as usize + 1;
            let mut k = 0;
            let mut cut = chars.len();
            for (j, c) in chars.iter().enumerate() {
                if !c.is_whitespace() {
                    if k == shown {
                        cut = j;
                        break;
                    }
                    k += 1;
                }
            }
            let a: String = chars[..cut].iter().collect();
            let z: String = chars[cut..].iter().collect();
            format!("{}{{\\1a&HFF&\\3a&HFF&}}{}", ass_escape(&a), ass_escape(&z))
        } else {
            ass_escape(t.text.trim_end())
        };
        head + &body
    }

    /// everything mpv draws over the preview, as ASS events (one per line); "" = nothing
    pub fn overlay(&self) -> String {
        let g = self.geo;
        if g.cw < 2.0 || g.ch < 2.0 {
            return String::new();
        }
        let mut ev: Vec<String> = vec![];
        let black = "&H000000&";
        let yellow = "&H3FD2FF&";
        if self.gap {
            ev.push(rect(g.cx, g.cy, g.cw, g.ch, black, 0));
        } else if let Some(s) = self.p.segs.get(self.cur) {
            let k = fade_gain(s.at, s.len(), s.fi, s.fo, self.t);
            // rough transition preview, like Echo's (a dip; the export does the real blend)
            let tr = s.tr();
            let mut dip = 0.0f64;
            if self.cur > 0 && !tr.is_empty() {
                let q = (self.t - s.at) / s.td().max(0.1);
                if (0.0..1.0).contains(&q) {
                    dip = if tr == "fadeblack" { 1.0 - q } else { (1.0 - q) * 0.45 };
                }
            }
            let o = (1.0 - k).max(dip).clamp(0.0, 1.0);
            if o > 0.005 {
                ev.push(rect(g.cx, g.cy, g.cw, g.ch, black, ((1.0 - o) * 255.0).round() as u8));
            }
        }
        if self.p.vertical.is_some() {
            let (fx, fw, _) = g.frame(self.p.vertical);
            if fx - g.cx > 0.5 {
                ev.push(rect(g.cx, g.cy, fx - g.cx, g.ch, black, 0x60));
            }
            let r = g.cx + g.cw - (fx + fw);
            if r > 0.5 {
                ev.push(rect(fx + fw, g.cy, r, g.ch, black, 0x60));
            }
            ev.push(outline(fx, g.cy, fw, g.ch, yellow, 2.0 * g.sf));
        }
        // boxes (under the texts, like the export)
        let selb = self.sel_box();
        for (i, b) in self.p.boxes.iter().enumerate() {
            let is_sel = selb == Some(i);
            let inside = self.t >= b.start && self.t <= b.end;
            if !inside && !is_sel {
                continue;
            }
            let (x, y, w, h) = self.box_rect(b);
            let dim = if inside { 0.0 } else { 0.55 };
            let a = |base: f64| ((1.0 - (1.0 - base) * (1.0 - dim)) * 255.0).round().clamp(0.0, 255.0) as u8;
            match b.kind.as_str() {
                "color" => ev.push(rect(x, y, w, h, &ass_color(&b.color), a(1.0 - b.opacity.clamp(0.0, 1.0)))),
                // blur: mpv really blurs it (blur_vf); a selected box outside its time shows a faint panel to find it
                "blur" if !inside => ev.push(rect(x, y, w, h, "&HD8D8D8&", 0xD0)),
                "zoom" if is_sel => {
                    ev.push(rect(x, y, w, h, "&HFFA24F&", 0xD8));
                }
                _ => {}
            }
            if is_sel {
                let col = if b.kind == "zoom" { "&HFFA24F&" } else { "&H3FD2FF&" }; // Echo: yellow handles (.ebx)
                ev.push(outline(x, y, w, h, col, 1.5 * g.sf));
                let hs = 8.0 * g.sf;
                for (hx, hy) in [(x, y), (x + w, y), (x, y + h), (x + w, y + h)] {
                    ev.push(rect(hx - hs / 2.0, hy - hs / 2.0, hs, hs, col, 0));
                }
                if b.kind == "zoom" {
                    let lbl = format!("Zoom {:.1}×", (1.0 / b.w.max(b.h)).clamp(1.0, 5.0));
                    ev.push(format!("{{\\an1\\pos({:.1},{:.1})\\fnSegoe UI\\b1\\fs{:.1}\\bord{:.1}\\shad0\\1c&HFFA24F&\\3c&H000000&\\3a&H40&\\q2}}{lbl}", x, y - 4.0 * g.sf, 12.0 * g.sf * ass_k(), 1.5 * g.sf));
                }
            }
        }
        let sel = self.sel_txt();
        for (i, t) in self.p.texts.iter().enumerate() {
            let is_sel = sel == Some(i) || self.msel.contains(&Sel::Txt(i));
            let inside = self.t >= t.start && self.t <= t.end;
            if (!is_sel || t.cap) && !inside {
                continue;
            }
            if t.text.trim().is_empty() && t.words.is_empty() {
                continue;
            }
            ev.push(self.text_event(t, self.t, !inside));
            if is_sel && !t.cap {
                let (x, y, w, h) = self.text_rect(t);
                ev.push(outline(x, y, w, h, "&HFFFFFF&", 1.5 * g.sf));
                let hs = 8.0 * g.sf;
                for (hx, hy) in [(x, y), (x + w, y), (x, y + h), (x + w, y + h)] {
                    ev.push(rect(hx - hs / 2.0, hy - hs / 2.0, hs, hs, "&HFFFFFF&", 0));
                }
            }
        }
        // the selected layer: a white frame just outside its picture (mpv draws the picture itself over this
        // overlay - pl_layer), so it stays visible
        if let Some(i) = self.sel_ov().filter(|&i| self.layer_on(&self.p.over[i])) {
            let (x, y, w, h) = self.layer_rect(&self.p.over[i]);
            let th = 2.0 * g.sf;
            ev.push(outline(x - th, y - th, w + 2.0 * th, h + 2.0 * th, "&HFFFFFF&", th));
        }
        if let Some((x, y, w, h, _)) = self.cap_box_rect() {
            ev.push(outline(x, y, w, h, yellow, 1.5 * g.sf));
            let hs = 8.0 * g.sf;
            for (hx, hy) in [(x, y), (x + w, y), (x, y + h), (x + w, y + h), (x + w / 2.0, y), (x + w / 2.0, y + h), (x, y + h / 2.0), (x + w, y + h / 2.0)] {
                ev.push(rect(hx - hs / 2.0, hy - hs / 2.0, hs, hs, yellow, 0));
            }
            let lbl = format!("Caption box · {}% wide · {} line{}", (self.p.cap_w * 100.0).round(), self.p.cap_lines, if self.p.cap_lines > 1 { "s" } else { "" });
            ev.push(format!(
                "{{\\an1\\pos({:.1},{:.1})\\fnSegoe UI\\b1\\fs{:.1}\\bord{:.1}\\shad0\\1c{yellow}\\3c&H000000&\\3a&H40&\\q2}}{lbl}",
                x,
                y - 4.0 * g.sf,
                12.0 * g.sf * ass_k(),
                1.5 * g.sf
            ));
        }
        ev.join("\n")
    }

    // ---------- mouse on the video (x, y in OSD px, from the video's top-left) ----------
    pub fn stage_mouse(&mut self, kind: &str, x: f64, y: f64) -> bool {
        let g = self.geo;
        let (fx, fw, fh) = g.frame(self.p.vertical);
        let tol = 9.0 * g.sf;
        // "cdown" = Ctrl / Shift held (a text joins / leaves the selection), "adown" = Alt (place without the
        // snap to the middle) - Echo's stage
        let text_hit = |e: &Editor| -> Option<usize> {
            let sel = e.sel_txt();
            (0..e.p.texts.len()).rev().find(|&i| {
                let t = &e.p.texts[i];
                let on = (e.t >= t.start && e.t <= t.end) || (sel == Some(i) && !t.cap);
                let (rx, ry, rw, rh) = e.text_rect(t);
                on && x >= rx && x <= rx + rw && y >= ry && y <= ry + rh
            })
        };
        match kind {
            "down" | "cdown" | "adown" => {
                self.drag = None;
                // a plain click outside the selected texts ends the multi-selection
                let had_msel = !self.msel.is_empty();
                if kind != "cdown" && !text_hit(self).is_some_and(|i| self.msel.contains(&Sel::Txt(i))) {
                    self.msel.clear();
                }
                // 1. the caption box (shown while a caption is selected)
                if let Some((bx, by, bw, bh, _)) = self.cap_box_rect() {
                    let near = |a: f64, b: f64| (a - b).abs() <= tol;
                    let inside = x >= bx - tol && x <= bx + bw + tol && y >= by - tol && y <= by + bh + tol;
                    if inside {
                        let (l, r, t, bo) = (near(x, bx), near(x, bx + bw), near(y, by), near(y, by + bh));
                        let edge = match (l, r, t, bo) {
                            (true, _, true, _) => "tl",
                            (_, true, true, _) => "tr",
                            (true, _, _, true) => "bl",
                            (_, true, _, true) => "br",
                            (true, ..) => "l",
                            (_, true, ..) => "r",
                            (_, _, true, _) => "t",
                            (_, _, _, true) => "b",
                            _ => "",
                        };
                        let i = self.cap_ref().unwrap_or(0);
                        let p = &self.p.texts[i];
                        let starts =self.p.texts.iter().enumerate().filter(|(_, t)| t.cap).map(|(k, t)| (k, t.x, t.y)).collect();
                        self.drag = Some(Drag::CapBox { edge: edge.into(), x0: x, y0: y, starts, s0: p.size, w0: self.p.cap_w, cx: fx + p.x * fw, cy: g.cy + p.y * fh, pushed: false });
                        return true;
                    }
                }
                // 1b. the selected box's corners = size
                if let Some(i) = self.sel_box() {
                    let (rx, ry, rw, rh) = self.box_rect(&self.p.boxes[i]);
                    for (k, (hx, hy)) in [(rx, ry), (rx + rw, ry), (rx, ry + rh), (rx + rw, ry + rh)].into_iter().enumerate() {
                        if (x - hx).abs() <= tol && (y - hy).abs() <= tol {
                            self.drag = Some(Drag::BoxSize { i, corner: (k % 2 == 1, k >= 2), orig: self.p.boxes[i].clone(), pushed: false });
                            return true;
                        }
                    }
                }
                // 2. the selected text's corner handles = size
                if let Some(i) = self.sel_txt().filter(|&i| !self.p.texts[i].cap) {
                    let t = &self.p.texts[i];
                    let (rx, ry, rw, rh) = self.text_rect(t);
                    for (hx, hy) in [(rx, ry), (rx + rw, ry), (rx, ry + rh), (rx + rw, ry + rh)] {
                        if (x - hx).abs() <= tol && (y - hy).abs() <= tol {
                            let (cx, cy) = (rx + rw / 2.0, ry + rh / 2.0);
                            let d0 = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt().max(4.0);
                            self.drag = Some(Drag::TextSize { i, cx, cy, d0, s0: t.size, pushed: false });
                            return true;
                        }
                    }
                }
                // 3. a text on screen (top-most first); Ctrl / Shift+click toggles it in the selection, dragging one
                // of several selected texts moves them all
                if let Some(i) = text_hit(self) {
                    let key = Sel::Txt(i);
                    if kind == "cdown" {
                        let mut v = std::mem::take(&mut self.msel);
                        if let Some(s) = self.sel.take() {
                            if !v.contains(&s) {
                                v.push(s);
                            }
                        }
                        let off = v.contains(&key);
                        v.retain(|s| *s != key);
                        if !off {
                            v.push(key);
                        }
                        self.set_msel(v);
                        if off {
                            return true;
                        }
                    } else if !self.msel.contains(&key) {
                        self.sel = Some(key);
                    }
                    let group: Vec<usize> = if self.msel.len() > 1 { self.msel.iter().filter_map(|s| if let Sel::Txt(k) = s { Some(*k) } else { None }).collect() } else { vec![i] };
                    let starts = group.iter().filter_map(|&k| self.p.texts.get(k).map(|t| (k, t.x, t.y))).collect();
                    self.drag = Some(Drag::TextMove { starts, x0: x, y0: y, pushed: false, free: kind == "adown" });
                    return true;
                }
                // 3b. a box on screen (the selected one first)
                let selb = self.sel_box();
                let order: Vec<usize> = selb.into_iter().chain((0..self.p.boxes.len()).rev().filter(|&i| Some(i) != selb)).collect();
                for i in order {
                    let b = &self.p.boxes[i];
                    let on = (self.t >= b.start && self.t <= b.end && b.kind != "zoom") || selb == Some(i);
                    if !on {
                        continue;
                    }
                    let (rx, ry, rw, rh) = self.box_rect(b);
                    if x >= rx && x <= rx + rw && y >= ry && y <= ry + rh {
                        self.sel = Some(Sel::Box(i));
                        self.drag = Some(Drag::BoxMove { i, x0: x, y0: y, bx0: b.x, by0: b.y, pushed: false });
                        return true;
                    }
                }
                // 3c. a layer on screen (the highest row first; the selected one before the others)
                let selo = self.sel_ov();
                let mut order: Vec<usize> = (0..self.p.over.len()).filter(|&i| self.layer_on(&self.p.over[i])).collect();
                order.sort_by_key(|&i| (Some(i) != selo, std::cmp::Reverse(self.p.over[i].row), std::cmp::Reverse(i)));
                for i in order {
                    let (rx, ry, rw, rh) = self.layer_rect(&self.p.over[i]);
                    if x >= rx && x <= rx + rw && y >= ry && y <= ry + rh {
                        let o = &self.p.over[i];
                        self.sel = Some(Sel::Ov(i));
                        self.drag = Some(Drag::LayerMove { i, x0: x, y0: y, ox: o.x, oy: o.y, pushed: false });
                        return true;
                    }
                }
                // 4. the 9:16 frame
                if let Some(v) = self.p.vertical {
                    if x >= fx && x <= fx + fw {
                        self.drag = Some(Drag::Frame { v0: v, x0: x, pushed: false });
                        return true;
                    }
                }
                let had = self.sel.is_some();
                self.sel = None;
                had || had_msel
            }
            "move" => {
                let Some(d) = self.drag.clone() else { return false };
                let first = |e: &mut Editor, pushed: bool| {
                    if !pushed {
                        e.push_hist();
                    }
                };
                match d {
                    Drag::TextMove { starts, x0, y0, pushed, free } => {
                        if !pushed && (x - x0).abs() + (y - y0).abs() < 3.0 {
                            return false;
                        }
                        first(self, pushed);
                        let (dx, dy) = ((x - x0) / fw, (y - y0) / fh);
                        let one = starts.len() == 1;
                        for &(i, sx, sy) in &starts {
                            if let Some(t) = self.p.texts.get_mut(i) {
                                t.x = (sx + dx).clamp(0.0, 1.0);
                                t.y = (sy + dy).clamp(0.0, 1.0);
                                if one && !free && (t.x - 0.5).abs() < 0.012 {
                                    t.x = 0.5; // snaps to the middle (hold Alt to place it freely)
                                }
                                if t.cap {
                                    t.own = true;
                                }
                            }
                        }
                        self.drag = Some(Drag::TextMove { starts, x0, y0, pushed: true, free });
                        true
                    }
                    Drag::TextSize { i, cx, cy, d0, s0, pushed } => {
                        first(self, pushed);
                        let k = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() / d0;
                        if let Some(t) = self.p.texts.get_mut(i) {
                            t.size = (s0 * k).clamp(0.02, 0.4);
                        }
                        self.drag = Some(Drag::TextSize { i, cx, cy, d0, s0, pushed: true });
                        true
                    }
                    Drag::CapBox { edge, x0, y0, starts, s0, w0, cx, cy, pushed } => {
                        first(self, pushed);
                        let e = edge.as_str();
                        if e.len() == 2 {
                            // corners: scale the whole caption (text size + box width together)
                            let d0 = ((x0 - cx).powi(2) + (y0 - cy).powi(2)).sqrt().max(1.0);
                            let k = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() / d0;
                            let ns = (s0 * k).clamp(0.025, 0.2);
                            for t in self.p.texts.iter_mut().filter(|t| t.cap) {
                                t.size = ns;
                            }
                            self.p.cap_w = (w0 * ns / s0).clamp(0.15, 1.0);
                        } else if e == "l" || e == "r" {
                            self.p.cap_w = ((x - cx).abs() * 2.0 / fw).clamp(0.15, 1.0);
                        } else if e == "t" || e == "b" {
                            let line_h = self.cap_ref().map(|i| self.p.texts[i].size).unwrap_or(s0) * fh * 1.2;
                            self.p.cap_lines = (((y - cy).abs() * 2.0 - 8.0) / line_h).round().clamp(1.0, 3.0) as u32;
                        } else {
                            let (dx, dy) = ((x - x0) / fw, (y - y0) / fh);
                            for &(i, sx, sy) in &starts {
                                if let Some(t) = self.p.texts.get_mut(i) {
                                    t.x = (sx + dx).clamp(0.0, 1.0);
                                    t.y = (sy + dy).clamp(0.0, 1.0);
                                }
                            }
                        }
                        if !e.is_empty() {
                            self.rechunk_caps();
                        }
                        self.drag = Some(Drag::CapBox { edge, x0, y0, starts, s0, w0, cx, cy, pushed: true });
                        true
                    }
                    Drag::BoxMove { i, x0, y0, bx0, by0, pushed } => {
                        if !pushed && (x - x0).abs() + (y - y0).abs() < 3.0 {
                            return false;
                        }
                        first(self, pushed);
                        if let Some(b) = self.p.boxes.get_mut(i) {
                            b.x = (bx0 + (x - x0) / g.cw).clamp(b.w / 2.0, 1.0 - b.w / 2.0);
                            b.y = (by0 + (y - y0) / g.ch).clamp(b.h / 2.0, 1.0 - b.h / 2.0);
                        }
                        self.drag = Some(Drag::BoxMove { i, x0, y0, bx0, by0, pushed: true });
                        true
                    }
                    Drag::BoxSize { i, corner, orig, pushed } => {
                        first(self, pushed);
                        // the opposite corner stays where it is
                        let fx = ((x - g.cx) / g.cw).clamp(0.0, 1.0);
                        let fy = ((y - g.cy) / g.ch).clamp(0.0, 1.0);
                        let ox = if corner.0 { orig.x - orig.w / 2.0 } else { orig.x + orig.w / 2.0 };
                        let oy = if corner.1 { orig.y - orig.h / 2.0 } else { orig.y + orig.h / 2.0 };
                        if let Some(b) = self.p.boxes.get_mut(i) {
                            b.w = (fx - ox).abs().max(0.03);
                            b.h = (fy - oy).abs().max(0.03);
                            // stickers keep their shape (the height follows the width)
                            if b.kind == "image" && orig.w > 0.0 {
                                b.h = (b.w * orig.h / orig.w).max(0.03);
                            }
                            let fy = if b.kind == "image" { if fy >= oy { oy + b.h } else { oy - b.h } } else { fy };
                            b.x = (fx + ox) / 2.0;
                            b.y = (fy + oy) / 2.0;
                        }
                        self.drag = Some(Drag::BoxSize { i, corner, orig, pushed: true });
                        true
                    }
                    Drag::LayerMove { i, x0, y0, ox, oy, pushed } => {
                        // moved by the drag distance (no jump on grab - Echo), centre kept on the picture
                        if !pushed && (x - x0).abs() + (y - y0).abs() < 3.0 {
                            return false;
                        }
                        first(self, pushed);
                        if let Some(o) = self.p.over.get_mut(i) {
                            o.x = (ox + (x - x0) / g.cw.max(1.0)).clamp(0.0, 1.0);
                            o.y = (oy + (y - y0) / g.ch.max(1.0)).clamp(0.0, 1.0);
                        }
                        self.drag = Some(Drag::LayerMove { i, x0, y0, ox, oy, pushed: true });
                        true
                    }
                    Drag::Frame { v0, x0, pushed } => {
                        first(self, pushed);
                        self.p.vertical = Some((v0 + (x - x0) / g.cw.max(1.0)).clamp(0.0, 1.0));
                        self.drag = Some(Drag::Frame { v0, x0, pushed: true });
                        true
                    }
                    _ => false,
                }
            }
            "up" => {
                let d = self.drag.take();
                matches!(d, Some(Drag::CapBox { .. }) | Some(Drag::TextMove { .. }) | Some(Drag::BoxMove { .. }) | Some(Drag::BoxSize { .. }) | Some(Drag::LayerMove { .. }))
            }
            w => {
                // "wheel:<px>" (up > 0). main.rs gives one wheel notch out as many small glide steps adding up to
                // 160 px, so the change goes by the amount, not by the number of events; one undo step per burst
                let notches = match w.strip_prefix("wheel:") {
                    Some(d) => d.parse::<f64>().unwrap_or(0.0) / 160.0,
                    None => if w == "wheel-up" { 1.0 } else { -1.0 },
                };
                if notches == 0.0 {
                    return false;
                }
                let first_of_burst = || !WHEEL_AT.with(|c| c.replace(Some(Instant::now()))).is_some_and(|t| t.elapsed() < Duration::from_millis(600));
                // the selected layer under the mouse: its size (Echo: 5 % a notch, 15..100 %)
                if let Some(i) = self.sel_ov().filter(|&i| self.layer_on(&self.p.over[i])) {
                    let (rx, ry, rw, rh) = self.layer_rect(&self.p.over[i]);
                    if x >= rx && x <= rx + rw && y >= ry && y <= ry + rh {
                        if first_of_burst() {
                            self.push_hist();
                        }
                        let o = &mut self.p.over[i];
                        o.scale = (o.scale * 1.05f64.powf(notches)).clamp(0.15, 1.0);
                        return true;
                    }
                }
                // the selected text: size (6 % a notch)
                let Some(i) = self.sel_txt().filter(|&i| !self.p.texts[i].cap) else { return false };
                if first_of_burst() {
                    self.push_hist();
                }
                let t = &mut self.p.texts[i];
                t.size = (t.size * 1.06f64.powf(notches)).clamp(0.02, 0.4);
                true
            }
        }
    }

    // ---------- playback over the piece list ----------
    pub fn seek_out(&mut self, m: &crate::mpv::Mpv, exact: bool) {
        if self.p.segs.is_empty() {
            return;
        }
        self.t = self.t.clamp(0.0, self.total());
        let (i, src, gap) = self.locate(self.t);
        self.cur = i;
        self.gap = gap;
        let s = self.p.segs[i].clone();
        let at = src.min(s.end - 0.01).max(0.0);
        // a piece from another file (Media): mpv loads that file (and the clip again after it). The clip's own
        // track mix (lavfi-complex) is cleared first - it names tracks the other file doesn't have.
        let want: Option<PathBuf> = s.src().map(PathBuf::from);
        if want != self.loaded {
            if !self.last_af.is_empty() {
                m.set("lavfi-complex", "");
                self.last_af.clear();
            }
            let f = want.clone().unwrap_or_else(|| self.clip.clone());
            m.command(&["loadfile", &f.to_string_lossy(), "replace", "-1", &format!("start={at:.3},pause=yes")]);
            self.loaded = want;
        } else {
            m.command(&["seek", &format!("{at:.3}"), if exact { "absolute+exact" } else { "absolute+keyframes" }]);
        }
        // freeze frame / reversed piece: mpv stays paused, tick() steps the picture by hand (Echo's isMan)
        let man = s.man();
        m.set("speed", &format!("{:.4}", if man { 1.0 } else { s.speed }));
        m.set("pause", if self.playing && !gap && !man { "no" } else { "yes" });
        self.seek_at = Some(Instant::now());
    }

    pub fn play(&mut self, m: &crate::mpv::Mpv) {
        if self.t >= self.total() - 0.05 {
            self.t = 0.0;
        }
        self.playing = true;
        self.last_tick = Some(Instant::now());
        self.seek_out(m, true);
    }

    pub fn pause(&mut self, m: &crate::mpv::Mpv) {
        self.playing = false;
        m.set("pause", "yes");
    }

    /// ~60x a second while the editor is open; true = the time moved
    pub fn tick(&mut self, m: &crate::mpv::Mpv) -> bool {
        let now = Instant::now();
        let dt = self.last_tick.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0).min(0.1);
        self.last_tick = Some(now);
        if !self.playing || self.p.segs.is_empty() {
            return false;
        }
        let total = self.total();
        if !self.gap && self.p.segs.get(self.cur).is_some_and(|s| s.man()) {
            // freeze frame: the picture stands, the clock runs by itself. Reversed piece: the same clock, and the
            // picture is stepped backwards by exact seeks (one at a time - Echo: `!v.seeking`). Silent; the export
            // plays its sound backwards.
            let s = self.p.segs[self.cur].clone();
            self.t += dt;
            if self.t >= total {
                self.t = total;
                self.pause(m);
            } else if self.t >= s.at + s.len() {
                self.seek_out(m, true);
            } else if s.rev() && self.seek_at.is_none_or(|t| t.elapsed() >= Duration::from_millis(40)) && m.get("seeking").as_deref() != Some("yes") {
                let src = (s.end - (self.t - s.at) * s.speed).clamp(s.start, s.end - 0.01);
                m.command(&["seek", &format!("{src:.3}"), "absolute+exact"]);
                self.seek_at = Some(Instant::now());
            }
            return true;
        }
        if self.gap {
            self.t += dt;
            if self.t >= total {
                self.t = total;
                self.pause(m);
            } else if self.p.segs.get(self.cur).is_some_and(|s| self.t >= s.at && self.t < s.at + s.len()) {
                self.seek_out(m, true);
            }
            return true;
        }
        let s = self.p.segs[self.cur.min(self.p.segs.len() - 1)].clone();
        if self.seek_at.is_some_and(|t| t.elapsed() < Duration::from_millis(160)) {
            self.t = (self.t + dt).min(s.at + s.len());
            return true;
        }
        let pos = m.get_f64("time-pos").unwrap_or(s.start);
        let eof = m.get("eof-reached").as_deref() == Some("yes");
        if pos >= s.end - 0.03 || eof {
            let end = s.at + s.len();
            if self.cur + 1 < self.p.segs.len() {
                self.t = end.max(self.p.segs[self.cur + 1].at.min(end + 0.0));
                self.seek_out(m, true);
            } else if end < total - 0.03 {
                // layers run on past the main track: black underneath them, the clock runs by itself
                self.t = end;
                self.gap = true;
                m.set("pause", "yes");
            } else {
                self.t = total;
                self.pause(m);
            }
        } else {
            self.t = s.at + (pos - s.start).max(0.0) / s.speed;
        }
        true
    }

    // ---------- export ----------
    /// all texts as an ASS script for libass (ffmpeg's subtitles filter) - the same drawing as the preview
    pub fn export_ass(&self, w: u32, h: u32) -> String {
        let mut out = format!(
            "[Script Info]\nScriptType: v4.00+\nPlayResX: {w}\nPlayResY: {h}\nScaledBorderAndShadow: yes\nWrapStyle: 2\n\n\
            [V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
            Style: C,Segoe UI,48,&H00FFFFFF,&H00FFFFFF,&H40000000,&H00000000,-1,0,0,0,100,100,0,0,1,3,0,5,0,0,0,1\n\n\
            [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n"
        );
        let ms = |s: f64| (s * 1000.0).round() as i64;
        for t in &self.p.texts {
            if t.text.trim().is_empty() && t.words.is_empty() {
                continue;
            }
            let px = t.size * h as f64;
            let fs = px * ass_k();
            let (x, y) = ((t.x * w as f64).round(), (t.y * h as f64).round());
            let base = ass_color(&t.color);
            let head = |first: bool, last: bool, len: f64| -> String {
                let fin = ms(0.25f64.min(len / 3.0));
                let mut a = String::new();
                let mut pos = format!("\\pos({x},{y})");
                match t.anim.as_str() {
                    "fade" => a += &format!("\\fad({},{})", if first { fin } else { 0 }, if last { fin } else { 0 }),
                    "pop" if first => a += &format!("\\fscx20\\fscy20\\t(0,{},\\fscx112\\fscy112)\\t({},{},\\fscx100\\fscy100)", fin * 2 / 3, fin * 2 / 3, fin),
                    "slide" if first => {
                        let dy = (h as f64 * 0.06).round();
                        pos = String::new();
                        a += &format!("\\move({x},{},{x},{y},0,{fin})\\fad({fin},0)", y + dy);
                    }
                    "bounce" if first => a += &format!("\\fscy40\\fscx130\\t(0,{},\\fscy120\\fscx90)\\t({},{},\\fscy100\\fscx100)", fin / 2, fin / 2, fin),
                    _ => {}
                }
                format!("{{\\an5{pos}\\fs{fs:.1}\\bord{:.1}\\blur{:.1}\\3a&H50&\\c{base}{a}}}", (px / 28.0).max(1.0), (px / 14.0).max(2.0))
            };
            if t.words.is_empty() || t.hl.is_empty() {
                let txt = ass_escape(t.text.trim_end());
                let body = if t.anim == "type" {
                    let chars: Vec<char> = txt.chars().collect();
                    let n = chars.iter().filter(|c| !c.is_whitespace()).count().max(1);
                    let step = 0.06f64.min((t.end - t.start) * 0.6 / n as f64);
                    let (mut b, mut k, mut i) = (String::new(), 0, 0);
                    while i < chars.len() {
                        if chars[i] == '\\' && chars.get(i + 1) == Some(&'N') {
                            b += "\\N";
                            i += 2;
                            continue;
                        }
                        let c = chars[i];
                        if c.is_whitespace() {
                            b.push(c);
                        } else {
                            let m0 = (k as f64 * step * 1000.0).round() as i64;
                            b += &format!("{{\\alpha&HFF&\\t({m0},{},\\alpha&H00&)}}{c}", m0 + 1);
                            k += 1;
                        }
                        i += 1;
                    }
                    b
                } else {
                    txt
                };
                out += &format!("Dialogue: 0,{},{},C,,0,0,0,,{}{body}\n", ass_time(t.start), ass_time(t.end), head(true, true, t.end - t.start));
                continue;
            }
            let hl = ass_color(&t.hl);
            let mut cuts: Vec<f64> = vec![t.start, t.end];
            for wd in &t.words {
                cuts.push(wd.start.clamp(t.start, t.end));
                cuts.push(wd.end.clamp(t.start, t.end));
            }
            cuts.sort_by(|a, b| a.partial_cmp(b).unwrap());
            cuts.dedup_by(|a, b| (*a - *b).abs() < 0.005);
            let nw = cuts.len().saturating_sub(1);
            for (k, win) in cuts.windows(2).enumerate() {
                let (a, bb) = (win[0], win[1]);
                let mid = (a + bb) / 2.0;
                let mut body = String::new();
                for (j, wd) in t.words.iter().enumerate() {
                    if j > 0 {
                        body += if wd.nl { "\\N" } else { " " };
                    }
                    let tx = ass_escape(&wd.text);
                    body += &if mid >= wd.start && mid < wd.end { format!("{{\\c{hl}}}{tx}{{\\c{base}}}") } else { tx };
                }
                let hd = if t.anim != "type" { head(k == 0, k + 1 == nw, bb - a) } else { head(false, false, bb - a) };
                out += &format!("Dialogue: 0,{},{},C,,0,0,0,,{hd}{body}\n", ass_time(a), ass_time(bb));
            }
        }
        out
    }

    /// the ffmpeg filter graph for the edit: (graph, video label, has audio, output length, output height)
    pub fn export_graph(&self, preset: &str, w: u32, h: u32, has_audio: bool, ass: Option<&Path>, sized_h: Option<u32>) -> Result<(String, String, bool, f64), String> {
        let mut segs: Vec<&crate::editor::Seg> = self.p.segs.iter().filter(|s| s.end - s.start >= 0.05).collect();
        segs.sort_by(|a, b| a.at.partial_cmp(&b.at).unwrap_or(std::cmp::Ordering::Equal));
        if segs.is_empty() {
            return Err("nothing left on the timeline".into());
        }
        // other files (Media): ffmpeg inputs 1.. in other_files() order; their pictures are fitted into the clip's size
        let files = self.other_files();
        let inp = |f: Option<&str>| f.and_then(|f| files.iter().position(|x| x == f)).map(|k| k + 1).unwrap_or(0);
        // stickers: inputs after the other files (looping pictures / GIFs - Echo's render_edit)
        let sticks: Vec<&BoxFx> = self.sticker_boxes();
        let fit = |k: usize| if k == 0 { String::new() } else { format!("scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,") };
        const VF: &str = "fps=60,setsar=1,format=yuv420p";
        const AF: &str = "aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo";
        let audio = has_audio && !self.p.mute;
        // reversed pieces: files made before the export (run_export), inputs after the stickers (Echo's reverse_piece)
        let revs = self.rev_jobs(has_audio);
        let rev_inp = |s: &crate::editor::Seg| revs.iter().position(|r| r.start == s.start && r.end == s.end && r.src == s.src().map(PathBuf::from).unwrap_or_else(|| self.clip.clone())).map(|r| 1 + files.len() + sticks.len() + r);
        let mut g: Vec<String> = vec![];
        let mut units: Vec<(String, String)> = vec![];
        // Echo's render_edit: a transition blends the end of one unit into the next (xfade + acrossfade); the piece
        // before runs on for the transition's length (a handle), so the timing after it stays the same
        let mut unit_len = 0.0f64; // output length of the last unit
        let mut last_piece = false;
        let mut pos = 0.0f64;
        let speed_of = |s: &crate::editor::Seg| if s.freeze() { 1.0 } else { s.speed.clamp(0.1, 4.0) };
        let len_of = |s: &crate::editor::Seg| (s.end - s.start) / speed_of(s);
        // transition length into piece k (0 = hard cut); the pieces must touch
        let tr_len = |k: usize, gap: f64| -> f64 {
            let s = segs[k];
            if k == 0 || s.tr().is_empty() || gap > 0.02 {
                return 0.0;
            }
            s.td().max(0.1).min(len_of(s) / 2.0).min(len_of(segs[k - 1]) / 2.0).min(3.0)
        };
        for (i, s) in segs.iter().enumerate() {
            let sp = speed_of(s);
            let gap = s.at - pos;
            if gap > 0.02 {
                g.push(format!("color=c=black:s={w}x{h}:r=60:d={gap:.3},{VF}[gv{i}]"));
                if audio {
                    g.push(format!("anullsrc=r=48000:cl=stereo,atrim=duration={gap:.3},{AF}[ga{i}]"));
                }
                units.push((format!("gv{i}"), format!("ga{i}")));
                unit_len = gap;
                last_piece = false;
            }
            let slen = len_of(s);
            // a transition into the NEXT piece needs this one to run on a little
            let ext = match segs.get(i + 1) {
                Some(nx) => tr_len(i + 1, nx.at - (pos.max(s.at) + slen)),
                None => 0.0,
            };
            let total = slen + ext;
            let gr = grade(s);
            let vf = fades("fade", s.fi, s.fo, slen);
            let af = fades("afade", s.fi, s.fo, slen);
            let k = inp(s.src());
            let amute = s.extra.get("amute").and_then(|x| x.as_bool()) == Some(true);
            let src_sound = k == 0 || s.src().and_then(|f| self.srcs.get(f)).is_some_and(|x| x.0);
            let mut silent = amute || !src_sound;
            if s.freeze() {
                g.push(format!("[{k}:v]trim=start={:.3}:duration=0.25,setpts=PTS-STARTPTS,{}{VF},trim=end_frame=1,tpad=stop_mode=clone:stop_duration={total:.3},trim=duration={total:.3},setpts=PTS-STARTPTS{gr}{vf}[v{i}]", s.start, fit(k)));
                silent = true;
            } else if let Some(r) = s.rev().then(|| rev_inp(s)).flatten() {
                let pad = if ext > 0.0 { format!(",tpad=stop_mode=clone:stop_duration={ext:.3}") } else { String::new() };
                g.push(format!("[{r}:v]setpts=(PTS-STARTPTS)/{sp:.4},{}{VF}{pad}{gr}{vf}[v{i}]", fit(k)));
                if audio && !silent && revs[r - 1 - files.len() - sticks.len()].audio {
                    let ap = if ext > 0.0 { format!(",apad=whole_dur={total:.3},atrim=duration={total:.3}") } else { String::new() };
                    g.push(format!("[{r}:a]asetpts=PTS-STARTPTS,{},{AF}{ap}{af}[a{i}]", atempo(sp)));
                } else {
                    silent = true;
                }
            } else {
                // the source's own length (handles can't run past it - the last frame is held instead)
                let s_dur = match s.src() { Some(f) => self.srcs.get(f).map(|x| x.1).unwrap_or(0.0), None => self.dur };
                let end2 = if s_dur > 0.0 { (s.end + ext * sp).min(s_dur.max(s.end)) } else { s.end + ext * sp };
                let short = ext - (end2 - s.end) / sp;
                let pad = if short > 0.01 { format!(",tpad=stop_mode=clone:stop_duration={short:.3}") } else { String::new() };
                g.push(format!("[{k}:v]trim=start={:.3}:end={end2:.3},setpts=(PTS-STARTPTS)/{sp:.4},{}{VF}{pad}{gr}{vf}[v{i}]", s.start, fit(k)));
                if audio && !silent {
                    // another file's sound can be a little shorter than its picture: padded to the exact length
                    let ap = if ext > 0.0 || k > 0 { format!(",apad=whole_dur={total:.3},atrim=duration={total:.3}") } else { String::new() };
                    g.push(format!("[{k}:a:0]atrim=start={:.3}:end={end2:.3},asetpts=PTS-STARTPTS,{},{AF}{ap}{af}[a{i}]", s.start, atempo(sp)));
                }
            }
            if audio && silent {
                g.push(format!("anullsrc=r=48000:cl=stereo,atrim=duration={total:.3},{AF}[a{i}]"));
            }
            let td = tr_len(i, gap);
            if td > 0.0 && last_piece {
                // blend with the unit before (it ran on for `td` past its end)
                let (pv, pa) = units.pop().unwrap_or_default();
                let kind: String = s.tr().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
                g.push(format!("[{pv}][v{i}]xfade=transition={kind}:duration={td:.3}:offset={unit_len:.3}[xv{i}]"));
                if audio {
                    g.push(format!("[{pa}][a{i}]acrossfade=d={td:.3}:c1=tri:c2=tri[xa{i}]"));
                }
                units.push((format!("xv{i}"), format!("xa{i}")));
                unit_len += slen;
            } else {
                units.push((format!("v{i}"), format!("a{i}")));
                unit_len = slen;
            }
            last_piece = true;
            pos = pos.max(s.at) + slen;
        }
        // layers may run past the main track: pad with black + silence underneath them (Echo)
        let ov_end = self.p.over.iter().map(|o| o.at.max(0.0) + (o.end - o.start) / o.speed.clamp(0.1, 4.0)).fold(0.0f64, f64::max);
        let tail = ov_end - pos;
        if tail > 0.02 {
            g.push(format!("color=c=black:s={w}x{h}:r=60:d={tail:.3},{VF}[gvt]"));
            if audio {
                g.push(format!("anullsrc=r=48000:cl=stereo,atrim=duration={tail:.3},{AF}[gat]"));
            }
            units.push(("gvt".into(), "gat".into()));
        }
        let total = pos.max(ov_end);
        let n = units.len();
        let cat: String = units.iter().map(|(v, a)| if audio { format!("[{v}][{a}]") } else { format!("[{v}]") }).collect();
        g.push(format!("{cat}concat=n={n}:v=1:a={}[vc]{}", audio as u8, if audio { "[ac]" } else { "" }));
 let mut last = "vc".to_string();
        // zoom boxes: zoompan over the video, eased in/out (0.4 s), centred on the box (Echo's graph)
        let zooms: Vec<&BoxFx> = self.p.boxes.iter().filter(|b| b.kind == "zoom" && b.end - b.start >= 0.2 && b.w > 0.0 && b.h > 0.0).collect();
        if !zooms.is_empty() {
            let (mut z, mut cx, mut cy) = (String::from("1"), String::from("0.5"), String::from("0.5"));
            for b in &zooms {
                let zf = (1.0 / b.w.max(b.h)).clamp(1.0, 5.0);
                let r = 0.4f64.min((b.end - b.start) / 2.0);
                let ramp = format!("clip(min((it-{:.3})/{r:.3}\\,({:.3}-it)/{r:.3})\\,0\\,1)", b.start, b.end);
                z += &format!("+{:.4}*{ramp}", zf - 1.0);
                cx += &format!("+{:.4}*{ramp}", b.x.clamp(0.0, 1.0) - 0.5);
                cy += &format!("+{:.4}*{ramp}", b.y.clamp(0.0, 1.0) - 0.5);
            }
            g.push(format!("[vc]zoompan=z='{z}':x='clip(({cx})*iw-iw/zoom/2\\,0\\,iw-iw/zoom)':y='clip(({cy})*ih-ih/zoom/2\\,0\\,ih-ih/zoom)':d=1:s={w}x{h}:fps=60,setsar=1[vz]"));
            last = "vz".into();
        }
        // video layers (Echo's overlay graph): each piece timed to its spot, scaled to `scale` of the width, centred
        // at x / y; higher rows on top. Other files are sized from the clip's width.
        let mut ovs: Vec<&Ovl> = self.p.over.iter().filter(|o| o.end - o.start >= 0.05).collect();
        ovs.sort_by_key(|o| o.row);
        for (i, o) in ovs.iter().enumerate() {
            let sp = o.speed.clamp(0.1, 4.0);
            let len = (o.end - o.start) / sp;
            let k = inp(o.src());
            let sc = if k > 0 { format!("scale={w}*{:.4}:-2", o.scale.clamp(0.05, 1.0)) } else { format!("scale=iw*{:.4}:-2", o.scale.clamp(0.05, 1.0)) };
            g.push(format!("[{k}:v]trim=start={:.3}:end={:.3},setpts=(PTS-STARTPTS)/{sp:.4}+{:.3}/TB,{sc},setsar=1[ov{i}]", o.start, o.end, o.at.max(0.0)));
            g.push(format!(
                "[{last}][ov{i}]overlay=x=main_w*{:.4}-overlay_w/2:y=main_h*{:.4}-overlay_h/2:eof_action=pass:enable='between(t,{:.3},{:.3})'[vo{i}]",
                o.x, o.y, o.at.max(0.0), o.at.max(0.0) + len));
            last = format!("vo{i}");
        }
        // blur / colour boxes (full-frame coordinates, so before the 9:16 crop)
        for (i, b) in self.p.boxes.iter().filter(|b| (b.kind == "blur" || b.kind == "color" || (b.kind == "image" && sticks.iter().any(|s| std::ptr::eq(*s, *b)))) && b.end - b.start >= 0.05 && b.w > 0.0 && b.h > 0.0).enumerate() {
            let bw = (((b.w.min(1.0) * w as f64).round() as u32).max(4) / 2 * 2).min(w);
            let bh = (((b.h.min(1.0) * h as f64).round() as u32).max(4) / 2 * 2).min(h);
            let bx = (((b.x - b.w / 2.0).max(0.0) * w as f64).round() as u32).min(w - bw);
            let by = (((b.y - b.h / 2.0).max(0.0) * h as f64).round() as u32).min(h - bh);
            let en = format!("enable='between(t,{:.3},{:.3})'", b.start.max(0.0), b.end);
            if b.kind == "image" {
                let k = 1 + files.len() + sticks.iter().position(|s| std::ptr::eq(*s, b)).unwrap_or(0);
                g.push(format!("[{k}:v]scale={bw}:{bh},format=rgba,colorchannelmixer=aa={:.2}[im{i}]", b.opacity.clamp(0.0, 1.0)));
                g.push(format!("[{last}][im{i}]overlay=x={bx}:y={by}:eof_action=pass:{en}[bx{i}]"));
            } else if b.kind == "blur" {
                let sg = b.blur.clamp(1.0, 200.0);
                g.push(format!("[{last}]split[bm{i}][bs{i}]"));
                g.push(format!("[bs{i}]crop={bw}:{bh}:{bx}:{by},gblur=sigma={sg:.1}:steps=3[bb{i}]"));
                g.push(format!("[bm{i}][bb{i}]overlay=x={bx}:y={by}:{en}[bx{i}]"));
            } else {
                let col: String = b.color.chars().filter(|c| c.is_ascii_hexdigit()).take(6).collect();
                let col = if col.len() == 6 { col } else { "000000".into() };
                g.push(format!("[{last}]drawbox=x={bx}:y={by}:w={bw}:h={bh}:color=0x{col}@{:.2}:t=fill:{en}[bx{i}]", b.opacity.clamp(0.0, 1.0)));
            }
            last = format!("bx{i}");
        }
        let vertical = if preset == "tt" { Some(self.p.vertical.unwrap_or(0.5)) } else { self.p.vertical };
        let mut ow = w;
        if let Some(cx) = vertical {
            let cw = ((h as f64 * 9.0 / 16.0).round() as u32 / 2 * 2).min(w);
            let x = (cx.clamp(0.0, 1.0) * w as f64 - cw as f64 / 2.0).clamp(0.0, (w - cw) as f64).round() as u32 / 2 * 2;
            g.push(format!("[{last}]crop={cw}:{h}:{x}:0[vcrop]"));
            last = "vcrop".into();
            ow = cw;
        }
        let _ = ow;
        if let Some(f) = ass {
            let fonts = if cfg!(windows) { ":fontsdir='C\\:/Windows/Fonts'" } else { "" };
            g.push(format!("[{last}]subtitles=filename='{}'{fonts}[subs]", ff_path(f)));
            last = "subs".into();
        }
        let out_h = match preset {
            "yt" => Some(1080),
            "tt" => Some(1920),
            _ => sized_h,
        };
        if let Some(oh) = out_h.filter(|&oh| oh != h) {
            g.push(format!("[{last}]scale=-2:{oh}:flags=lanczos,setsar=1[vout]"));
            last = "vout".into();
        }
        // 9 Nov: the Echo watermark (Settings > Watermark; off = none) like Echo's render_edit - same
        // filter from Echo's own recorder.rs, sized to the output height, after texts / scaling, before the colour fix
        if let Some(wm) = crate::rec_core::rec().and_then(|r| r.watermark_graph(&last, "vwm", out_h.filter(|&oh| oh >= 240).unwrap_or(h))) {
            g.push(wm);
            last = "vwm".into();
        }
        g.push(format!("[{last}]scale=out_color_matrix=bt709:out_range=tv,format=yuv420p[vtv]"));
        last = "vtv".into();
        if audio {
            // separated audio clips (U): their own track, delayed to their spot, mixed over the main audio (Echo)
            let tracks = self.tracks.max(1);
            let extra: Vec<&crate::editor::Aud> = self.p.audio.iter().filter(|a| !a.mute && a.end - a.start >= 0.05 && (a.other_file() || a.track < tracks)).collect();
            let mut mix = String::from("[ac]");
            for (i, a) in extra.iter().enumerate() {
                let sp = a.speed.clamp(0.1, 4.0);
                let ms = (a.at.max(0.0) * 1000.0).round() as u64;
                let k = inp(a.extra.get("src").and_then(|x| x.as_str()).filter(|x| !x.is_empty()));
                g.push(format!("[{k}:a:{}]atrim=start={:.3}:end={:.3},asetpts=PTS-STARTPTS,{},volume={:.3},{AF}{},adelay={ms}:all=1[x{i}]",
                    if k > 0 { 0 } else { a.track }, a.start, a.end, atempo(sp), a.vol.clamp(0.0, 4.0), fades("afade", a.fi, a.fo, (a.end - a.start) / sp)));
                mix += &format!("[x{i}]");
            }
            // layers with "Include its sound": their part of the file's (first) audio, delayed to their spot (Echo)
            let mut n_mix = extra.len();
            for o in self.p.over.iter().filter(|o| o.sound && o.end - o.start >= 0.05) {
                let k = inp(o.src());
                if k > 0 && !o.src().and_then(|f| self.srcs.get(f)).is_some_and(|x| x.0) {
                    continue;
                }
                let sp = o.speed.clamp(0.1, 4.0);
                let ms = (o.at.max(0.0) * 1000.0).round() as u64;
                g.push(format!("[{k}:a:0]atrim=start={:.3}:end={:.3},asetpts=PTS-STARTPTS,{},{AF},adelay={ms}:all=1[x{n_mix}]", o.start, o.end, atempo(sp)));
                mix += &format!("[x{n_mix}]");
                n_mix += 1;
            }
            let main = if n_mix == 0 {
                "ac".to_string()
            } else {
                g.push(format!("{mix}amix=inputs={}:normalize=0:duration=first[am]", n_mix + 1));
                "am".to_string()
            };
            let norm = if self.normalize { ",loudnorm=I=-14:TP=-1.5:LRA=11,aresample=48000" } else { "" };
            g.push(format!("[{main}]volume={:.3}{norm}[ao]", self.p.volume.clamp(0.0, 4.0)));
        }
        Ok((g.join(";"), last, audio, total))
    }
}

fn ass_time(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    format!("{}:{:02}:{:02}.{:02}", cs / 360000, cs / 6000 % 60, cs / 100 % 60, cs % 100)
}

fn fades(f: &str, fi: f64, fo: f64, len: f64) -> String {
    let mut out = String::new();
    let fi = fi.clamp(0.0, len);
    let fo = fo.clamp(0.0, len);
    if fi >= 0.05 {
        out += &format!(",{f}=t=in:st=0:d={fi:.3}");
    }
    if fo >= 0.05 {
        out += &format!(",{f}=t=out:st={:.3}:d={fo:.3}", (len - fo).max(0.0));
    }
    out
}

fn atempo(mut speed: f64) -> String {
    let mut v = Vec::new();
    while speed < 0.5 {
        v.push("atempo=0.5".to_string());
        speed /= 0.5;
    }
    while speed > 2.0 {
        v.push("atempo=2.0".to_string());
        speed /= 2.0;
    }
    v.push(format!("atempo={speed:.4}"));
    v.join(",")
}

/// colour of a piece (Echo's grade(): brightness / contrast / saturation + look preset), "" = untouched
fn grade(s: &crate::editor::Seg) -> String {
    let num = |k: &str, d: f64| s.extra.get(k).and_then(|v| v.as_f64()).unwrap_or(d);
    let (bri, con, sat) = (num("bri", 0.0), num("con", 1.0), num("sat", 1.0));
    let mut f = String::new();
    if bri.abs() > 0.005 || (con - 1.0).abs() > 0.005 || (sat - 1.0).abs() > 0.005 {
        f += &format!(",eq=brightness={:.3}:contrast={:.3}:saturation={:.3}", bri.clamp(-1.0, 1.0) * 0.5, con.clamp(0.2, 3.0), sat.clamp(0.0, 3.0));
    }
    f += match s.extra.get("look").and_then(|v| v.as_str()).unwrap_or("") {
        "vivid" => ",eq=contrast=1.08:saturation=1.35",
        "warm" => ",colorbalance=rs=0.06:bs=-0.06:rm=0.06:bm=-0.06:rh=0.03:bh=-0.03",
        "cool" => ",colorbalance=rs=-0.06:bs=0.06:rm=-0.05:bm=0.06:rh=-0.02:bh=0.03",
        "bw" => ",hue=s=0,eq=contrast=1.1",
        "cinema" => ",eq=contrast=1.12:saturation=0.85,colorbalance=rs=-0.06:bs=0.06:rh=0.06:bh=-0.06",
        "vintage" => ",curves=preset=vintage",
        "punch" => ",eq=contrast=1.2:saturation=1.15:gamma=0.95",
        _ => "",
    };
    f
}

fn ff_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/").replace(':', "\\:")
}

pub fn hidden_cmd(exe: &Path) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

/// ffprobe next to ffmpeg (or on PATH)
pub fn ffprobe(ff: &Path) -> PathBuf {
    if ff.file_name().is_some_and(|n| n.to_string_lossy().ends_with(".exe")) {
        ff.with_file_name("ffprobe.exe")
    } else {
        let p = ff.with_file_name("ffprobe");
        if p.exists() { p } else { PathBuf::from("ffprobe") }
    }
}

/// mean volume (dB) of every audio track (Echo's track_levels; -91 = silent / unreadable)
pub fn track_levels(ff: &Path, clip: &Path, tracks: u32) -> Vec<f64> {
    (0..tracks).map(|k| {
        let o = hidden_cmd(ff).args(["-hide_banner", "-nostats", "-i"]).arg(clip)
            .args(["-map", &format!("0:a:{k}"), "-af", "volumedetect", "-vn", "-sn", "-f", "null", "-"])
            .stdin(Stdio::null()).stdout(Stdio::null()).output();
        o.ok().and_then(|o| String::from_utf8_lossy(&o.stderr).lines().find_map(|l| l.split("mean_volume:").nth(1).and_then(|v| v.trim().trim_end_matches("dB").trim().parse::<f64>().ok()))).unwrap_or(-91.0)
    }).collect()
}

/// (width, height, has audio, audio track count) of a clip
pub fn probe(ff: &Path, clip: &Path) -> (u32, u32, bool, u32) {
    let o = hidden_cmd(&ffprobe(ff))
        .args(["-v", "error", "-show_entries", "stream=codec_type,width,height", "-of", "csv=p=0"])
        .arg(clip)
        .stdin(Stdio::null())
        .output();
    let mut r = (0, 0, false, 0);
    if let Ok(o) = o {
        for l in String::from_utf8_lossy(&o.stdout).lines() {
            let p: Vec<&str> = l.split(',').map(|x| x.trim()).collect();
            match p.first().copied() {
                Some("video") if r.0 == 0 => {
                    r.0 = p.get(1).and_then(|x| x.parse().ok()).unwrap_or(0);
                    r.1 = p.get(2).and_then(|x| x.parse().ok()).unwrap_or(0);
                }
                Some("audio") => {
                    r.2 = true;
                    r.3 += 1;
                }
                _ => {}
            }
        }
    }
    r
}

pub struct ExportPlan {
    pub ff: PathBuf,
    pub src: PathBuf,
    /// other files the edit uses (Media, then stickers) - ffmpeg inputs 1.., with their own input options
    pub inputs: Vec<(Vec<String>, PathBuf)>,
    pub out: PathBuf,
    pub graph_ass: Option<(String, String)>,
    pub graph_plain: (String, String),
    pub ass_file: PathBuf,
    pub ass_text: String,
    pub audio: bool,
    pub total: f64,
    pub kbps: u64,
    /// Discord: the bitrate is a hard budget (also for libx264), audio 128k
    pub sized: bool,
    /// reversed pieces made before the export (their files are already in `inputs`)
    pub revs: Vec<RevJob>,
}

/// a piece played backwards, rendered to its own file before the export (Echo's reverse_piece)
#[derive(Clone, Debug)]
pub struct RevJob {
    pub src: PathBuf,
    pub start: f64,
    pub end: f64,
    pub audio: bool,
    pub out: PathBuf,
}

impl Editor {
    /// the main track's reversed pieces, in order (export inputs after the other files and the stickers). Cached in
    /// %TEMP%\echo-proto-reverse by file + span, so the next export of the same edit is quick.
    pub fn rev_jobs(&self, has_audio: bool) -> Vec<RevJob> {
        use std::hash::{Hash, Hasher};
        let dir = std::env::temp_dir().join("echo-proto-reverse");
        let mut v: Vec<RevJob> = vec![];
        for s in self.p.segs.iter().filter(|s| s.rev() && !s.freeze() && s.end - s.start >= 0.05) {
            let src = s.src().map(PathBuf::from).unwrap_or_else(|| self.clip.clone());
            let audio = match s.src() { Some(f) => self.srcs.get(f).is_some_and(|x| x.0), None => has_audio };
            if v.iter().any(|r| r.src == src && r.start == s.start && r.end == s.end) {
                continue;
            }
            let mut h = std::collections::hash_map::DefaultHasher::new();
            src.hash(&mut h);
            std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0).hash(&mut h);
            let key = format!("{:016x}_{:.0}_{:.0}{}", h.finish(), s.start * 1000.0, s.end * 1000.0, if audio { "" } else { "_v" });
            v.push(RevJob { src, start: s.start, end: s.end, audio, out: dir.join(format!("{key}.mp4")) });
        }
        v
    }
}

/// Echo's reverse_piece: 2 s parts, each reversed in memory (ffmpeg's reverse / areverse), joined last-first
fn reverse_piece(ff: &Path, j: &RevJob, log: &dyn Fn(String)) -> Result<(), String> {
    let dir = j.out.parent().map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stem = j.out.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let t0 = Instant::now();
    let mut list = String::new();
    let mut parts: Vec<PathBuf> = vec![];
    let mut t = j.end;
    let mut ok = true;
    while t > j.start + 0.01 {
        let a = (t - 2.0).max(j.start);
        let part = dir.join(format!("{stem}_{}.mp4", parts.len()));
        let mut c = hidden_cmd(ff);
        c.args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{a:.3}"), "-t", &format!("{:.3}", t - a), "-i"]).arg(&j.src)
            .args(["-map", "0:v:0", "-vf", "reverse", "-c:v", "libx264", "-preset", "ultrafast", "-crf", "14"]);
        if j.audio {
            c.args(["-map", "0:a:0", "-af", "areverse", "-c:a", "aac", "-b:a", "192k"]);
        }
        ok = c.arg(&part).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        parts.push(part.clone());
        if !ok {
            break;
        }
        list += &format!("file '{}'\n", part.to_string_lossy().replace('\\', "/").replace('\'', "'\\''"));
        t = a;
    }
    let lf = dir.join(format!("{stem}.txt"));
    let tmp = dir.join(format!("{stem}.part.mp4"));
    if ok {
        ok = std::fs::write(&lf, list).is_ok()
            && hidden_cmd(ff).args(["-hide_banner", "-loglevel", "error", "-y", "-f", "concat", "-safe", "0", "-i"]).arg(&lf).args(["-c", "copy", "-f", "mp4"]).arg(&tmp)
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
            && std::fs::rename(&tmp, &j.out).is_ok();
    }
    for p in parts.iter().chain([&lf, &tmp]) {
        let _ = std::fs::remove_file(p);
    }
    if !ok {
        return Err("could not play a piece backwards - see the log".into());
    }
    log(format!("editor: reversed a {:.1}s piece in {:.1}s", j.end - j.start, t0.elapsed().as_secs_f32()));
    Ok(())
}

/// the Discord size Echo's settings pick (Copy default; "full" -> Discord Free), in MB
pub fn discord_mb() -> f64 {
    let st = crate::library::echo_settings().unwrap_or_default();
    let k = st.get("copy_default").and_then(|v| v.as_str()).filter(|k| *k != "full").unwrap_or("free").to_string();
    let d = match k.as_str() { "basic" => 50.0, "nitro" => 1000.0, "custom" => 100.0, _ => 20.0 };
    st.get(format!("discord_{k}_mb")).and_then(|v| v.as_f64()).unwrap_or(d)
}

impl Editor {
    pub fn export_plan(&self, ff: &Path, preset: &str) -> Result<ExportPlan, String> {
        // r2-threads (L30): the size / sound tracks ffprobe found when the editor opened (on a worker); probed again
        // only when that failed (the Export click ran ffprobe on the UI thread)
        let (w, h, has_audio, _) = if self.w > 16 && self.h > 16 { (self.w, self.h, self.tracks > 0, self.tracks) } else { probe(ff, &self.clip) };
        let (w, h) = if w > 0 && h > 0 { (w, h) } else { (self.w.max(16), self.h.max(16)) };
        let tmp = std::env::temp_dir().join("echo-proto-edit");
        let _ = std::fs::create_dir_all(&tmp);
        let ass_file = tmp.join("texts.ass");
        let vertical = preset == "tt" || self.p.vertical.is_some();
        let tw = if vertical { ((h as f64 * 9.0 / 16.0).round() as u32 / 2 * 2).min(w) } else { w };
        let has_text = self.p.texts.iter().any(|t| !t.text.trim().is_empty() || !t.words.is_empty());
        let ass_text = self.export_ass(tw, h);
        // Discord: ONE export that fits the size (Echo's max_mb): bitrate from the budget, smaller picture when low
        let total0 = self.total();
        let size = std::fs::metadata(&self.clip).map(|m| m.len()).unwrap_or(0) as f64;
        let mut kbps = if self.dur > 0.0 { ((size * 8.0 / self.dur / 1000.0) as u64).clamp(4000, 80000) } else { 20000 };
        let mut sized_h = None;
        let mut sized = false;
        if preset == "dc" {
            let mb = discord_mb() * 0.97;
            if mb >= 1.0 && total0 > 0.5 {
                sized = true;
                let budget = (mb * 8_000.0 * 0.95 / total0) as u64;
                let v = budget.saturating_sub(if has_audio { 128 } else { 0 }).max(250);
                kbps = kbps.min(v);
                if h > 720 && kbps < 2500 {
                    sized_h = Some(720);
                }
                if h > 480 && kbps < 900 {
                    sized_h = Some(480);
                }
            }
        }
        let graph_ass = if has_text { Some(self.export_graph(preset, w, h, has_audio, Some(&ass_file), sized_h).map(|(g, l, ..)| (g, l))?) } else { None };
        let (gp, lp, audio, total) = self.export_graph(preset, w, h, has_audio, None, sized_h)?;
        let dir = self.clip.parent().map(|d| d.to_path_buf()).unwrap_or_default();
        let stem = self.clip.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let out = (1..).map(|n| dir.join(if n == 1 { format!("{stem} (edit).mp4") } else { format!("{stem} (edit {n}).mp4") })).find(|p| !p.exists()).unwrap();
        let mut inputs: Vec<(Vec<String>, PathBuf)> = self.other_files().into_iter().map(|f| (vec![], PathBuf::from(f))).collect();
        if let Some(miss) = inputs.iter().find(|p| !p.1.exists()) {
            return Err(format!("a file this edit uses is missing: {}", miss.1.display()));
        }
        let t = format!("{:.3}", self.total() + 1.0);
        for b in self.sticker_boxes() {
            let src = b.extra.get("src").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let args: Vec<String> = if crate::pl_sticker::is_gif(&src) {
                // animated GIF: over and over (-loop / -framerate don't exist for GIFs)
                vec!["-stream_loop".into(), "-1".into(), "-t".into(), t.clone()]
            } else {
                vec!["-loop".into(), "1".into(), "-framerate".into(), "60".into(), "-t".into(), t.clone()]
            };
            inputs.push((args, PathBuf::from(src)));
        }
        let revs = self.rev_jobs(has_audio);
        for r in &revs {
            inputs.push((vec![], r.out.clone()));
        }
        Ok(ExportPlan { ff: ff.to_path_buf(), src: self.clip.clone(), inputs, out, graph_ass, graph_plain: (gp, lp), ass_file, ass_text, audio, total, kbps, sized, revs })
    }
}

/// GPU encoders that can't work here are skipped (AMF / QSV Windows only, NVENC needs NVIDIA's libcuda)
fn skip_enc(venc: &[&str]) -> bool {
    if cfg!(windows) {
        return false;
    }
    let cuda = ["/usr/lib64/libcuda.so.1", "/usr/lib/x86_64-linux-gnu/libcuda.so.1", "/usr/lib/libcuda.so.1", "/usr/lib64/nvidia/libcuda.so.1"].iter().any(|p| Path::new(p).exists());
    venc.iter().any(|a| a.ends_with("_amf") || a.ends_with("_qsv") || (a.ends_with("_nvenc") && !cuda))
}

/// runs the export (on a worker thread); `progress` gets 0..1. Ok(out file) / Err(reason)
pub fn run_export(plan: &ExportPlan, progress: &dyn Fn(f64), log: &dyn Fn(String)) -> Result<PathBuf, String> {
    let tmp_out = plan.out.with_extension("render.tmp");
    let b = format!("{}k", plan.kbps);
    let run = |graph: &str, last: &str, venc: &[&str]| -> bool {
        if skip_enc(venc) {
            return false;
        }
        let mut c = hidden_cmd(&plan.ff);
        c.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(&plan.src);
        for (args, f) in &plan.inputs {
            c.args(args).arg("-i").arg(f);
        }
        c.args(["-filter_complex", graph, "-map", &format!("[{last}]")]);
        if plan.audio {
            c.args(["-map", "[ao]", "-c:a", "aac", "-b:a", if plan.sized { "128k" } else { "192k" }]);
        }
        c.args(venc).args(["-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"]);
        c.args(["-movflags", "+faststart", "-progress", "pipe:1", "-nostats", "-f", "mp4"]).arg(&tmp_out);
        let Ok(mut ch) = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() else { return false };
        let errp = ch.stderr.take();
        let errt = std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut e) = errp {
                use std::io::Read;
                let _ = e.read_to_string(&mut s);
            }
            s
        });
        if let Some(out) = ch.stdout.take() {
            use std::io::BufRead;
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                if let Some(v) = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms=")) {
                    if let Ok(us) = v.trim().parse::<f64>() {
                        progress(if plan.total > 0.0 { (us / 1e6 / plan.total).clamp(0.0, 0.99) } else { 0.0 });
                    }
                }
            }
        }
        let ok = ch.wait().map(|s| s.success()).unwrap_or(false);
        let e = errt.join().unwrap_or_default();
        if !ok {
            log(format!("editor: export with {} failed: {}", venc.get(1).unwrap_or(&"?"), e.trim().lines().last().unwrap_or("")));
        }
        ok && std::fs::metadata(&tmp_out).map_or(false, |m| m.len() > 0)
    };
    let encode = |graph: &str, last: &str| -> bool {
        run(graph, last, &["-c:v", "h264_nvenc", "-preset", "p5", "-b:v", &b, "-maxrate", &b])
            || run(graph, last, &["-c:v", "h264_amf", "-quality", "quality", "-b:v", &b, "-maxrate", &b])
            || run(graph, last, &["-c:v", "h264_qsv", "-b:v", &b, "-maxrate", &b])
            || (plan.sized && run(graph, last, &["-c:v", "libx264", "-preset", "veryfast", "-b:v", &b, "-maxrate", &b, "-bufsize", &b]))
            || (!plan.sized && run(graph, last, &["-c:v", "libx264", "-preset", "veryfast", "-crf", "18"]))
    };
    // Play backwards: those pieces first (cached)
    for r in plan.revs.iter().filter(|r| !r.out.exists()) {
        reverse_piece(&plan.ff, r, log)?;
    }
    let mut ok = false;
    if let Some((g, l)) = &plan.graph_ass {
        std::fs::write(&plan.ass_file, &plan.ass_text).map_err(|e| e.to_string())?;
        ok = encode(g, l);
        if !ok {
            log("editor: export with texts failed - trying without them".into());
        }
    }
    if !ok && plan.graph_ass.is_some() {
        let _ = std::fs::remove_file(&tmp_out);
        return Err("the texts couldn't be drawn (ffmpeg without libass?) - see the log".into());
    }
    if !ok {
        ok = encode(&plan.graph_plain.0, &plan.graph_plain.1);
    }
    if !ok {
        let _ = std::fs::remove_file(&tmp_out);
        return Err("export failed - see the log".into());
    }
    std::fs::rename(&tmp_out, &plan.out).map_err(|e| e.to_string())?;
    progress(1.0);
    Ok(plan.out.clone())
}

// ---------- captions: whisper.cpp (the copy Echo downloaded, else downloaded here like Echo does) ----------
fn whisper_dir() -> PathBuf {
    if let Ok(d) = std::env::var("ECHO_WHISPER_DIR") {
        return PathBuf::from(d);
    }
    // r2-threads (L31): echo_core's folder on every system ($APPDATA/Clipper/whisper - on Linux rec_core::early_env
    // sets APPDATA = ~/.config), so captions_ready and the download Echo made match what transcribe uses
    let base = std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
    base.join("Clipper").join("whisper")
}

/// r2-threads (L31): where this app downloaded whisper on Linux before (~/.local/share/Clipper/whisper) - still
/// looked in, so an earlier download isn't fetched again
#[cfg(not(windows))]
fn old_whisper_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share"));
    base.join("Clipper").join("whisper")
}

fn find_file(dir: &Path, names: &[&str], depth: u32) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut dirs = vec![];
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            dirs.push(p);
        } else if names.iter().any(|n| p.file_name().is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(n))) {
            return Some(p);
        }
    }
    if depth == 0 {
        return None;
    }
    dirs.into_iter().find_map(|d| find_file(&d, names, depth - 1))
}

pub fn whisper_cli() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ECHO_WHISPER_CLI") {
        return Some(PathBuf::from(p));
    }
    #[cfg(not(windows))]
    for d in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        for n in ["whisper-cli", "whisper-cpp"] {
            if d.join(n).is_file() {
                return Some(d.join(n));
            }
        }
    }
    // whisper-cli first in EVERY folder: in new releases main.exe (same folder, sorts first) is only a stub that
    // prints "deprecated" and exits - that was the empty "whisper failed:" on Windows (native chat 6)
    let found = find_file(&whisper_dir(), &["whisper-cli.exe", "whisper-cli"], 3).or_else(|| find_file(&whisper_dir(), &["main.exe"], 3));
    #[cfg(not(windows))]
    let found = found.or_else(|| find_file(&old_whisper_dir(), &["whisper-cli"], 3)); // r2-threads (L31)
    found
}

pub fn whisper_model() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ECHO_WHISPER_MODEL") {
        return Some(PathBuf::from(p));
    }
    let m = whisper_dir().join("models");
    let found = ["ggml-small.bin", "ggml-base.bin"].iter().map(|n| m.join(n)).find(|p| p.exists());
    #[cfg(not(windows))]
    let found = found.or_else(|| {
        let m = old_whisper_dir().join("models"); // r2-threads (L31): an earlier download of this app
        ["ggml-small.bin", "ggml-base.bin"].iter().map(|n| m.join(n)).find(|p| p.exists())
    });
    found
}

/// first-time download (Windows: whisper.cpp's release zip + the base model, with the curl.exe / tar.exe Windows
/// ships; the real port uses recorder.rs's checked download)
pub fn whisper_download(log: &dyn Fn(String)) -> Result<(), String> {
    let dir = whisper_dir();
    std::fs::create_dir_all(dir.join("models")).map_err(|e| e.to_string())?;
    let curl = |url: &str, to: &Path| -> Result<(), String> {
        log(format!("captions: downloading {url}"));
        let ok = hidden_cmd(Path::new("curl")).args(["-L", "-f", "-s", "-o"]).arg(to).arg(url).stdin(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        if ok && to.exists() { Ok(()) } else { let _ = std::fs::remove_file(to); Err(format!("couldn't download {url}")) }
    };
    if whisper_cli().is_none() {
        if cfg!(windows) {
            let zip = dir.join("whisper.zip");
            curl("https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.2/whisper-bin-x64.zip", &zip)?;
            let ok = hidden_cmd(Path::new("tar")).arg("-xf").arg(&zip).arg("-C").arg(&dir).stdin(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
            let _ = std::fs::remove_file(&zip);
            if !ok {
                return Err("couldn't unpack whisper.cpp".into());
            }
        } else {
            return Err("whisper-cli isn't installed (Echo downloads its own build the first time you use Captions there)".into());
        }
    }
    if whisper_model().is_none() {
        let f = dir.join("models").join("ggml-base.bin");
        curl("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin", &f)?;
    }
    Ok(())
}

fn read_wav16(p: &Path) -> Vec<i16> {
    let b = std::fs::read(p).unwrap_or_default();
    let mut i = 12;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let len = u32::from_le_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]) as usize;
        if id == b"data" {
            let end = (i + 8 + len).min(b.len());
            return b[i + 8..end].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
        }
        i += 8 + len + (len & 1);
    }
    Vec::new()
}

fn write_wav16(p: &Path, pcm: &[i16], rate: u32) -> Result<(), String> {
    let n = (pcm.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + n as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + n).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    for x in [16u32.to_le_bytes().as_slice(), &1u16.to_le_bytes(), &1u16.to_le_bytes(), &rate.to_le_bytes(), &(rate * 2).to_le_bytes(), &2u16.to_le_bytes(), &16u16.to_le_bytes()] {
        v.extend_from_slice(x);
    }
    v.extend_from_slice(b"data");
    v.extend_from_slice(&n.to_le_bytes());
    for s in pcm {
        v.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(p, v).map_err(|e| e.to_string())
}

/// Echo's speech_regions: where someone is talking (whisper invents text for silence, so only these go to it)
pub fn speech_regions(pcm: &[i16], rate: u32) -> Vec<(f64, f64)> {
    let hop = (rate / 50) as usize;
    if pcm.len() < hop {
        return Vec::new();
    }
    let db: Vec<f64> = pcm.chunks(hop).map(|c| {
        let e = c.iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum::<f64>() / c.len() as f64;
        10.0 * (e + 1e-12).log10()
    }).collect();
    let mut sorted = db.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let floor = sorted[sorted.len() / 5];
    let thr = (floor + 14.0).max(-48.0).min(-34.0);
    let fs = hop as f64 / rate as f64;
    let mut regs: Vec<(f64, f64)> = Vec::new();
    for (i, &d) in db.iter().enumerate() {
        if d > thr {
            let t = i as f64 * fs;
            match regs.last_mut() {
                Some(r) if t - r.1 < 0.6 => r.1 = t + fs,
                _ => regs.push((t, t + fs)),
            }
        }
    }
    let total = pcm.len() as f64 / rate as f64;
    let mut out: Vec<(f64, f64)> = regs.into_iter().filter(|r| r.1 - r.0 >= 0.25).map(|(a, b)| ((a - 0.3).max(0.0), (b + 0.3).min(total))).collect();
    out.dedup_by(|b, a| if b.0 <= a.1 { a.1 = a.1.max(b.1); true } else { false });
    while out.len() > 10 {
        let k = (1..out.len()).min_by(|&x, &y| (out[x].0 - out[x - 1].1).partial_cmp(&(out[y].0 - out[y - 1].1)).unwrap()).unwrap();
        out[k - 1].1 = out[k].1;
        out.remove(k);
    }
    out
}

/// drop whisper's loops ("the the the", a phrase repeated over and over)
fn drop_repeats(words: &mut Vec<(String, f64, f64)>) {
    let norm = |s: &str| s.trim().trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    // same word 3+ times in a row -> keep the first
    let mut out: Vec<(String, f64, f64)> = vec![];
    let mut run = 0;
    for w in words.drain(..) {
        if out.last().is_some_and(|l| norm(&l.0) == norm(&w.0) && !norm(&w.0).is_empty()) {
            run += 1;
            if run >= 2 {
                continue;
            }
        } else {
            run = 0;
        }
        out.push(w);
    }
    // a phrase of 2-6 words repeated right after itself 2+ times -> once
    let mut n = 2;
    while n <= 6 {
        let mut i = 0;
        while i + 2 * n <= out.len() {
            let same = (0..n).all(|k| norm(&out[i + k].0) == norm(&out[i + n + k].0));
            if same {
                out.drain(i + n..i + 2 * n);
            } else {
                i += 1;
            }
        }
        n += 1;
    }
    *words = out;
}

/// whisper over the clip's speech -> words (text, start, end) in clip time. ECHO_FAKE_WORDS="word@start-end ..." =
/// test words without whisper (the cloud test box can't download the model).
pub fn transcribe(ff: &Path, clip: &Path, tracks: u32, log: &dyn Fn(String)) -> Result<Vec<(String, f64, f64)>, String> {
    if let Ok(fw) = std::env::var("ECHO_FAKE_WORDS") {
        return Ok(fw.split_whitespace().filter_map(|x| {
            let (w, t) = x.rsplit_once('@')?;
            let (a, b) = t.split_once('-')?;
            Some((w.replace('_', " "), a.parse().ok()?, b.parse().ok()?))
        }).collect());
    }
    // audit (9 Nov): Echo's own transcribe (echo-core) - it follows Settings > Speech model (this copy always
    // took "small if it's there"), listens to the mic + the Discord track when Discord had sound (friends' voices
    // get captions too), downloads the chosen model with Echo's checksums. The code below is only the fallback.
    if let Some(r) = crate::rec_core::rec() {
        let model = r.settings().caption_model;
        log(format!("captions: Echo's transcribe, model {model}"));
        return r.transcribe(&clip.to_string_lossy(), &model).map(|w| w.into_iter().map(|w| (w.text, w.start, w.end)).collect());
    }
    if tracks == 0 {
        return Err("this clip has no audio".into());
    }
    if whisper_cli().is_none() || whisper_model().is_none() {
        whisper_download(log)?;
    }
    let cli = whisper_cli().ok_or("whisper.cpp wasn't found")?;
    let model = whisper_model().ok_or("the speech model wasn't found")?;
    let threads = std::thread::available_parallelism().map(|n| n.get().min(8)).unwrap_or(4).to_string();
    // your mic first on Windows (track 2), the full mix otherwise (Echo also mixes in Discord when it had sound)
    let cands: Vec<(&str, &str)> = if tracks >= 3 && cfg!(windows) { vec![("mic", "[0:a:2]"), ("mix", "[0:a:0]")] } else if tracks >= 3 { vec![("mix", "[0:a:0]"), ("mic", "[0:a:2]")] } else { vec![("track 0", "[0:a:0]")] };
    let tmp = std::env::temp_dir().join("echo-proto-edit");
    let _ = std::fs::create_dir_all(&tmp);
    for (label, input) in cands {
        let wav = tmp.join("caption.wav");
        let graph = format!("{input}aformat=channel_layouts=mono,highpass=f=80,lowpass=f=7500,dynaudnorm=f=150:g=15:p=0.9,aresample=16000[o]");
        let ok = hidden_cmd(ff).args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(clip).args(["-filter_complex", &graph, "-map", "[o]", "-ac", "1", "-c:a", "pcm_s16le"]).arg(&wav).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            continue;
        }
        let pcm = read_wav16(&wav);
        let _ = std::fs::remove_file(&wav);
        let regions = speech_regions(&pcm, 16000);
        log(format!("captions: {label}: speech at {}", regions.iter().map(|r| format!("{:.1}-{:.1}s", r.0, r.1)).collect::<Vec<_>>().join(", ")));
        if regions.is_empty() {
            continue;
        }
        const GAP: f64 = 0.4;
        let mut joined: Vec<i16> = Vec::new();
        let mut map: Vec<(f64, f64, f64)> = Vec::new();
        for (a, b) in &regions {
            let part = &pcm[(a * 16000.0) as usize..((b * 16000.0) as usize).min(pcm.len())];
            map.push((joined.len() as f64 / 16000.0, *a, part.len() as f64 / 16000.0));
            joined.extend_from_slice(part);
            joined.extend(std::iter::repeat(0i16).take((GAP * 16000.0) as usize));
        }
        joined.resize(joined.len().max(16000 * 3 / 2), 0);
        let to_clip = |t: f64| -> Option<f64> {
            let k = map.iter().rposition(|m| m.0 <= t + 0.05)?;
            let (j0, c0, len) = map[k];
            Some(c0 + (t - j0).clamp(0.0, len))
        };
        let rw = tmp.join("caption_part.wav");
        write_wav16(&rw, &joined, 16000)?;
        let base = tmp.join("caption");
        let _ = std::fs::remove_file(base.with_extension("json"));
        let o = hidden_cmd(&cli)
            .current_dir(cli.parent().unwrap_or(Path::new(".")))
            .args(["-m"]).arg(&model).args(["-f"]).arg(&rw)
            .args(["-l", "auto", "-ml", "1", "-sow", "-sns", "-mc", "0", "-oj", "-t", &threads, "-of"]).arg(&base)
            .stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&rw);
        if !o.status.success() {
            let code = o.status.code().unwrap_or(-1);
            let all = format!("{}\n{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            let tail: Vec<&str> = all.lines().filter(|l| !l.trim().is_empty()).collect();
            log(format!("captions: whisper failed (exit {code:#x}, {}): {}", cli.display(), tail[tail.len().saturating_sub(6)..].join(" | ")));
            if code as u32 == 0xC000_0135 {
                return Err("a Windows runtime file (Visual C++) is missing for speech recognition".into());
            }
            return Err("speech recognition failed - see the log".into());
        }
        let js: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(base.with_extension("json")).unwrap_or_default()).unwrap_or_default();
        let mut words = vec![];
        for seg in js["transcription"].as_array().cloned().unwrap_or_default() {
            let text = seg["text"].as_str().unwrap_or("").trim().to_string();
            let (a, b) = (seg["offsets"]["from"].as_f64(), seg["offsets"]["to"].as_f64());
            if text.is_empty() || text.starts_with('[') || (text.starts_with('(') && text.ends_with(')')) {
                continue;
            }
            if let (Some(a), Some(b)) = (a, b) {
                if let (Some(s0), Some(e0)) = (to_clip(a / 1000.0), to_clip(b / 1000.0)) {
                    words.push((text, s0, e0.max(s0 + 0.05)));
                }
            }
        }
        drop_repeats(&mut words);
        log(format!("captions: {label}: {} words", words.len()));
        if !words.is_empty() {
            return Ok(words);
        }
    }
    Ok(vec![])
}

#[allow(dead_code)]
pub fn describe(e: &Editor) -> String {
    format!("{} pieces, {} texts, {}", e.p.segs.len(), e.p.texts.len(), fmt_t(e.total()))
}
