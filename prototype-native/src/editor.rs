//! The video editor (native chat 5): Echo's #editor ported. Pieces with free positions on the output timeline
//! (gaps = black + silence), split / delete / ripple / close gaps / speed / fades, undo + redo, texts and auto
//! captions. Same project format as Echo (`{v, clip, state: {segs, texts, ...}, extra}`), so projects move between
//! the two apps.
//!
//! PREVIEW: mpv plays the source clip in the normal video spot (Windows: its own video window - Slint can't draw
//! over it), and everything drawn OVER the video (texts, captions, the selection box, the caption box, the 9:16
//! frame, black gaps and fades) is ONE ASS overlay handed to mpv (`osd-overlay`, id 63 - the fullscreen controls
//! and the seek preview use `overlay-add` ids 1 / 2, a separate list). libass draws it, exactly like the export
//! (which renders the texts with libass too), so the preview matches the file. Mouse input still reaches Slint:
//! the video window is WS_DISABLED, so clicks go to the parent window.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const OVERLAY_ID: &str = "63";
/// timeline: left gutter with the track names (Echo's LP)
pub const LP: f64 = 104.0;
/// timeline rows (y ranges inside the timeline area, logical px) - editor.slint draws the same numbers
// Echo's timeline (.eruler 26 px, .etrack 64 px + 12 px gap, text lanes 34 px, Effects track only with boxes)
pub const ROW_RULER: (f64, f64) = (0.0, 26.0);
pub const ROW_VIDEO: (f64, f64) = (38.0, 102.0);
pub const AUD_Y: f64 = 114.0;
pub const AUD_H: f64 = 44.0;
pub const LANE_H: f64 = 34.0;
/// one layer row (Echo: 72 px a row, blocks 64 px)
pub const OV_H: f64 = 72.0;

/// where the timeline's rows are (y inside the timeline area); editor.slint draws from the same numbers
#[derive(Clone, Debug, PartialEq)]
pub struct TlLayout {
    /// lane of every text (captions too), overlapping ones stack
    pub lane: Vec<usize>,
    pub lanes: usize,
    /// separated-audio rows: one per source track (Echo: rows by track), top of the Text track
    pub aud_rows: Vec<u32>,
    pub text_y: f64,
    pub fx: Option<(f64, f64)>,
    /// video layers above the main track (Echo's eotrack): rows, the main track's top, the audio track's top
    pub ov_rows: usize,
    pub video_y: f64,
    pub aud_y: f64,
    /// content height
    pub h: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
    pub start: f64,
    pub end: f64,
    pub speed: f64,
    pub at: f64,
    pub fi: f64,
    pub fo: f64,
    /// fields this port doesn't edit yet (colour, transitions, reverse, freeze, other files ...) - kept as they were
    pub extra: serde_json::Map<String, Value>,
}

impl Seg {
    pub fn len(&self) -> f64 {
        (self.end - self.start) / self.speed.max(0.05)
    }
    fn new(start: f64, end: f64, at: f64) -> Seg {
        Seg { start, end, speed: 1.0, at, fi: 0.0, fo: 0.0, extra: Default::default() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    pub raw: String,
    pub start: f64,
    pub end: f64,
    pub nl: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Txt {
    pub text: String,
    pub raw: String,
    pub start: f64,
    pub end: f64,
    pub x: f64,
    pub y: f64,
    pub size: f64,
    pub color: String,
    pub cap: bool,
    /// a caption moved on its own (the caption box doesn't move it)
    pub own: bool,
    pub words: Vec<Word>,
    pub hl: String,
    pub anim: String,
}

/// A box over the video for a while (Echo's ed.boxes): "blur" (hide names / chat), "color" (solid fill), "zoom"
/// (zoom in on that spot) or "image" (sticker - kept, not shown by this port yet). x/y = centre, w/h = size, all as
/// fractions of the full source frame; times are output time.
#[derive(Clone, Debug, PartialEq)]
pub struct BoxFx {
    pub start: f64,
    pub end: f64,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub kind: String,
    pub color: String,
    pub opacity: f64,
    pub blur: f64,
    pub extra: serde_json::Map<String, Value>,
}

impl BoxFx {
    pub fn from_json(v: &Value) -> BoxFx {
        let mut extra = v.as_object().cloned().unwrap_or_default();
        for k in ["start", "end", "x", "y", "w", "h", "kind", "color", "opacity", "blur"] {
            extra.remove(k);
        }
        BoxFx { start: f(v, "start", 0.0), end: f(v, "end", 0.0), x: f(v, "x", 0.5), y: f(v, "y", 0.5), w: f(v, "w", 0.25), h: f(v, "h", 0.15),
            kind: { let k = s(v, "kind"); if k.is_empty() { "blur".into() } else { k } }, color: { let c = s(v, "color"); if c.is_empty() { "000000".into() } else { c } },
            opacity: f(v, "opacity", 1.0), blur: f(v, "blur", 20.0), extra }
    }
    pub fn to_json(&self) -> Value {
        let mut o = self.extra.clone();
        for (k, v) in [("start", self.start), ("end", self.end), ("x", self.x), ("y", self.y), ("w", self.w), ("h", self.h), ("opacity", self.opacity), ("blur", self.blur)] {
            o.insert(k.into(), json!(v));
        }
        o.insert("kind".into(), json!(self.kind));
        o.insert("color".into(), json!(self.color));
        Value::Object(o)
    }
}

/// Separated audio (U, Echo's ed.audio): one source track of the clip as its own clip on the timeline (source
/// seconds start..end at `speed`, placed at `at`), volume 0..2, mute, fades. track: 0 mix, 1 desktop/game,
/// 2 mic, 3 game, 4 Discord, 5 browser, 6 music.
#[derive(Clone, Debug, PartialEq)]
pub struct Aud {
    pub start: f64,
    pub end: f64,
    pub speed: f64,
    pub at: f64,
    pub vol: f64,
    pub mute: bool,
    pub track: u32,
    pub label: String,
    pub fi: f64,
    pub fo: f64,
    pub extra: serde_json::Map<String, Value>,
}

impl Aud {
    pub fn len(&self) -> f64 {
        (self.end - self.start) / self.speed.max(0.05)
    }
    pub fn from_json(v: &Value) -> Aud {
        let mut extra = v.as_object().cloned().unwrap_or_default();
        for k in ["start", "end", "speed", "at", "vol", "mute", "track", "label", "fi", "fo"] {
            extra.remove(k);
        }
        Aud { start: f(v, "start", 0.0), end: f(v, "end", 0.0), speed: f(v, "speed", 1.0).clamp(0.1, 4.0), at: f(v, "at", 0.0), vol: f(v, "vol", 1.0), mute: b(v, "mute"),
            track: f(v, "track", 0.0) as u32, label: s(v, "label"), fi: f(v, "fi", 0.0), fo: f(v, "fo", 0.0), extra }
    }
    pub fn to_json(&self) -> Value {
        let mut o = self.extra.clone();
        for (k, v) in [("start", self.start), ("end", self.end), ("speed", self.speed), ("at", self.at), ("vol", self.vol), ("fi", self.fi), ("fo", self.fo)] {
            o.insert(k.into(), json!(v));
        }
        o.insert("mute".into(), json!(self.mute));
        o.insert("track".into(), json!(self.track));
        o.insert("label".into(), json!(self.label));
        Value::Object(o)
    }
    /// another file dropped into Echo's editor (this port only plays the clip's own tracks)
    pub fn other_file(&self) -> bool {
        self.extra.get("src").and_then(|x| x.as_str()).is_some_and(|x| !x.is_empty())
    }
}

/// A video layer above the main track (Echo's ed.over): source seconds start..end at `speed`, placed at `at`, drawn
/// centred at x / y (fractions of the picture), `scale` of the picture's width, its sound mixed in when `sound`.
/// row 1 = just above the main track.
#[derive(Clone, Debug, PartialEq)]
pub struct Ovl {
    pub start: f64,
    pub end: f64,
    pub speed: f64,
    pub at: f64,
    pub x: f64,
    pub y: f64,
    pub scale: f64,
    pub sound: bool,
    pub row: u32,
    pub extra: serde_json::Map<String, Value>,
}

impl Ovl {
    pub fn len(&self) -> f64 {
        (self.end - self.start) / self.speed.max(0.05)
    }
    pub fn src(&self) -> Option<&str> {
        self.extra.get("src").and_then(|x| x.as_str()).filter(|x| !x.is_empty())
    }
    pub fn from_json(v: &Value) -> Ovl {
        let mut extra = v.as_object().cloned().unwrap_or_default();
        for k in ["start", "end", "speed", "at", "x", "y", "scale", "sound", "row"] {
            extra.remove(k);
        }
        Ovl { start: f(v, "start", 0.0), end: f(v, "end", 0.0), speed: f(v, "speed", 1.0).clamp(0.1, 4.0), at: f(v, "at", 0.0), x: f(v, "x", 0.5), y: f(v, "y", 0.5),
            scale: f(v, "scale", 1.0), sound: v.get("sound").and_then(|x| x.as_bool()).unwrap_or(true), row: (f(v, "row", 1.0) as u32).max(1), extra }
    }
    pub fn to_json(&self) -> Value {
        let mut o = self.extra.clone();
        for (k, v) in [("start", self.start), ("end", self.end), ("speed", self.speed), ("at", self.at), ("x", self.x), ("y", self.y), ("scale", self.scale)] {
            o.insert(k.into(), json!(v));
        }
        o.insert("sound".into(), json!(self.sound));
        o.insert("row".into(), json!(self.row));
        o.entry("src").or_insert(json!(""));
        Value::Object(o)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Proj {
    pub segs: Vec<Seg>,
    pub texts: Vec<Txt>,
    pub boxes: Vec<BoxFx>,
    pub audio: Vec<Aud>,
    pub over: Vec<Ovl>,
    pub vertical: Option<f64>,
    pub cap_w: f64,
    pub cap_lines: u32,
    pub volume: f64,
    pub mute: bool,
    /// Echo's boxes / audio / layers - not editable in this port yet, carried through untouched
    pub keep: serde_json::Map<String, Value>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sel {
    Seg(usize),
    Txt(usize),
    Box(usize),
    Aud(usize),
    /// a video layer
    Ov(usize),
}

pub struct CapStyle {
    pub name: &'static str,
    pub color: &'static str,
    pub size: f64,
    pub upper: bool,
    pub hl: &'static str,
}

pub const CAP_STYLES: [CapStyle; 3] = [
    CapStyle { name: "classic", color: "ffffff", size: 0.065, upper: false, hl: "ffd23f" },
    CapStyle { name: "yellow", color: "ffd23f", size: 0.075, upper: false, hl: "ffffff" },
    CapStyle { name: "loud", color: "ffffff", size: 0.085, upper: true, hl: "3ddc97" },
];

fn cap_style(name: &str) -> &'static CapStyle {
    CAP_STYLES.iter().find(|s| s.name == name).unwrap_or(&CAP_STYLES[0])
}

/// what a mouse drag on the timeline / the video is doing
#[derive(Clone, Debug)]
pub enum Drag {
    /// a piece: 'm' move, 'l' / 'r' trim an end
    Piece { i: usize, mode: char, orig: Seg, x0: f64, y0: f64, pushed: bool, follow: Vec<(usize, f64, f64, Vec<(f64, f64)>)>, fa: Vec<(usize, f64)> },
    /// a video layer: 'm' move (also up / down between rows; row 0 = back onto the main track), 'l' / 'r' trim
    Layer { i: usize, mode: char, orig: Ovl, x0: f64, y0: f64, pushed: bool },
    /// a layer on the video, moved by the drag distance
    LayerMove { i: usize, x0: f64, y0: f64, ox: f64, oy: f64, pushed: bool },
    /// a separated-audio clip: 'm' move (snaps to piece edges), 'l' / 'r' trim
    AudDrag { i: usize, mode: char, orig: Aud, x0: f64, pushed: bool },
    /// a text block on the text track: 'm' / 'l' / 'r'
    TextTime { i: usize, mode: char, start0: f64, end0: f64, x0: f64, pushed: bool, bx: bool },
    /// the playhead (ruler / empty timeline)
    Head,
    /// texts on the video, moved by the drag distance (no jump on grab)
    TextMove { starts: Vec<(usize, f64, f64)>, x0: f64, y0: f64, pushed: bool, free: bool },
    /// a text's corner handle: size follows the distance to its centre
    TextSize { i: usize, cx: f64, cy: f64, d0: f64, s0: f64, pushed: bool },
    /// the caption box: "" = move all captions, l/r = width, t/b = lines, corners = text size
    CapBox { edge: String, x0: f64, y0: f64, starts: Vec<(usize, f64, f64)>, s0: f64, w0: f64, cx: f64, cy: f64, pushed: bool },
    /// the 9:16 frame
    Frame { v0: f64, x0: f64, pushed: bool },
    /// a box on the video: moved by the drag distance
    BoxMove { i: usize, x0: f64, y0: f64, bx0: f64, by0: f64, pushed: bool },
    /// a box's corner: the opposite corner stays put
    BoxSize { i: usize, corner: (bool, bool), orig: BoxFx, pushed: bool },
    /// plain drag on empty timeline = yellow range (v2, chat 19); a click without moving = playhead
    Range { x0: f64, moved: bool },
    /// Ctrl+drag on empty timeline: a box that selects everything it touches (content coordinates)
    Marq { x0: f64, y0: f64, x1: f64, y1: f64, moved: bool },
    /// dragging a block that is part of a multi-selection moves the whole selection
    Group { x0: f64, orig: Proj, items: Vec<Sel>, pushed: bool, min_t: f64, key: Sel },
}

/// where the video is drawn, in mpv's OSD pixels (the overlay's coordinates)
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Geo {
    /// whole OSD
    pub ow: f64,
    pub oh: f64,
    /// the picture inside it (letterboxing taken off)
    pub cx: f64,
    pub cy: f64,
    pub cw: f64,
    pub ch: f64,
    /// window scale factor (handles + lines keep their size on high-DPI screens)
    pub sf: f64,
}

impl Geo {
    /// the frame texts are placed in: the picture, or the 9:16 crop of it
    pub fn frame(&self, vertical: Option<f64>) -> (f64, f64, f64) {
        match vertical {
            Some(v) => {
                let fw = (self.ch * 9.0 / 16.0).min(self.cw);
                let fx = self.cx + (v * self.cw - fw / 2.0).clamp(0.0, (self.cw - fw).max(0.0));
                (fx, fw, self.ch)
            }
            None => (self.cx, self.cw, self.ch),
        }
    }
}

pub struct Editor {
    pub clip: PathBuf,
    pub name: String,
    pub dur: f64,
    pub w: u32,
    pub h: u32,
    pub p: Proj,
    pub hist: Vec<Proj>,
    pub fut: Vec<Proj>,
    pub sel: Option<Sel>,
    pub t: f64,
    pub playing: bool,
    pub cur: usize,
    pub gap: bool,
    /// pixels per second on the timeline, and how far it is scrolled (px)
    pub pps: f64,
    pub scroll: f64,
    pub tl_w: f64,
    pub snap: bool,
    pub cap_style: String,
    pub cap_hl: bool,
    pub cap_hl_color: String,
    /// "Show caption box": the box shows without a caption selected (Echo's capShowBox, not saved)
    pub cap_show_box: bool,
    pub normalize: bool,
    pub drag: Option<Drag>,
    pub geo: Geo,
    /// last overlay handed to mpv (only sent again when it changes)
    pub last_ov: String,
    /// a seek was just sent: mpv's time-pos still shows the old spot for a moment
    pub seek_at: Option<Instant>,
    pub last_tick: Option<Instant>,
    pub from_player: bool,
    pub busy: bool,
    /// highlighted part of the timeline (output seconds): X cuts it, Export range exports it
    pub rng: Option<(f64, f64)>,
    /// colour + zoom last handed to mpv for the preview (brightness, contrast, saturation, gamma, zoom, pan x, pan y)
    pub last_fx: [f64; 7],
    /// the blur filter last handed to mpv ("" = none) + whether hardware decoding was switched to copy-back for it
    pub last_vf: String,
    pub hw_copied: bool,
    /// the clip's audio tracks + their mean volume (dB, measured in the background - empty until then)
    pub tracks: u32,
    pub levels: Vec<f64>,
    /// separated-audio mix last handed to mpv (lavfi-complex)
    pub last_af: String,
    /// multi-selection (Echo's ed.msel): Ctrl/Shift+click toggles, Ctrl+drag on empty timeline = box, Ctrl+A = all.
    /// Only means something with 2+ entries; `sel` is None then.
    pub msel: Vec<Sel>,
    /// a message for the layer drag (edui shows it as a toast)
    pub layer_msg: Option<&'static str>,
    /// the timeline mouse's last y in WINDOW px (a dragged piece follows it upward - drag up = layer)
    pub mouse_y: f64,
    /// the timeline's top in the window (logical px, editor.slint Ed.tl-top): it moves when layer rows come / go
    pub tl_top: f64,
    /// the file mpv has loaded for the preview when it isn't the clip (a piece from another file, Media)
    pub loaded: Option<PathBuf>,
    /// other files used by the edit (Media): path -> (has sound, length s)
    pub srcs: std::collections::HashMap<String, (bool, f64)>,
}

fn f(v: &Value, k: &str, d: f64) -> f64 {
    v.get(k).and_then(|x| x.as_f64()).unwrap_or(d)
}
fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(|x| x.as_bool()).unwrap_or(false)
}

impl Seg {
    pub fn from_json(v: &Value) -> Seg {
        let mut extra = v.as_object().cloned().unwrap_or_default();
        for k in ["start", "end", "speed", "at", "fi", "fo"] {
            extra.remove(k);
        }
        Seg { start: f(v, "start", 0.0), end: f(v, "end", 0.0), speed: f(v, "speed", 1.0).clamp(0.1, 4.0), at: f(v, "at", 0.0), fi: f(v, "fi", 0.0), fo: f(v, "fo", 0.0), extra }
    }
    pub fn to_json(&self) -> Value {
        let mut o = self.extra.clone();
        for (k, v) in [("start", self.start), ("end", self.end), ("speed", self.speed), ("at", self.at), ("fi", self.fi), ("fo", self.fo)] {
            o.insert(k.into(), json!(v));
        }
        Value::Object(o)
    }
    /// not a plain piece of the clip (another file, freeze frame, reversed): no captions come from it
    pub fn special(&self) -> bool {
        self.extra.get("src").and_then(|x| x.as_str()).is_some_and(|x| !x.is_empty()) || self.freeze() || self.extra.get("rev").and_then(|x| x.as_bool()) == Some(true)
    }
    /// freeze frame: shows the picture at `start` for (end - start) seconds, silent
    pub fn freeze(&self) -> bool {
        self.extra.get("freeze").and_then(|x| x.as_bool()) == Some(true)
    }
    pub fn num(&self, k: &str, d: f64) -> f64 {
        self.extra.get(k).and_then(|v| v.as_f64()).unwrap_or(d)
    }
    /// another file (Media / Echo's dropped files): its path
    pub fn src(&self) -> Option<&str> {
        self.extra.get("src").and_then(|x| x.as_str()).filter(|x| !x.is_empty())
    }
    pub fn look(&self) -> String {
        self.extra.get("look").and_then(|v| v.as_str()).unwrap_or("").to_string()
    }
    /// Play backwards (Echo's s.rev): the piece shows end -> start
    pub fn rev(&self) -> bool {
        self.extra.get("rev").and_then(|x| x.as_bool()) == Some(true)
    }
    /// previewed by stepping the picture by hand (Echo's isMan): freeze frame or reversed
    pub fn man(&self) -> bool {
        self.freeze() || self.rev()
    }
    /// transition from the piece before (Echo's s.tr: "" = cut, else an ffmpeg xfade name) + its length (s.td)
    pub fn tr(&self) -> String {
        self.extra.get("tr").and_then(|x| x.as_str()).unwrap_or("").to_string()
    }
    pub fn td(&self) -> f64 {
        self.num("td", 0.5).clamp(0.1, 3.0)
    }
}

impl Txt {
    pub fn from_json(v: &Value) -> Txt {
        let words = v.get("words").and_then(|w| w.as_array()).map(|a| {
            a.iter().map(|w| Word { text: s(w, "text"), raw: w.get("raw").and_then(|x| x.as_str()).map(String::from).unwrap_or_else(|| s(w, "text")), start: f(w, "start", 0.0), end: f(w, "end", 0.0), nl: b(w, "nl") }).collect()
        }).unwrap_or_default();
        let text = s(v, "text");
        Txt {
            raw: v.get("raw").and_then(|x| x.as_str()).map(String::from).unwrap_or_else(|| text.clone()),
            text,
            start: f(v, "start", 0.0),
            end: f(v, "end", 0.0),
            x: f(v, "x", 0.5),
            y: f(v, "y", 0.5),
            size: f(v, "size", 0.08),
            color: { let c = s(v, "color"); if c.is_empty() { "ffffff".into() } else { c } },
            cap: b(v, "cap"),
            own: b(v, "own"),
            words,
            hl: s(v, "hl"),
            anim: s(v, "anim"),
        }
    }
    pub fn to_json(&self) -> Value {
        let mut o = json!({ "text": self.text, "raw": self.raw, "start": self.start, "end": self.end, "x": self.x, "y": self.y, "size": self.size, "color": self.color });
        if self.cap {
            o["cap"] = json!(true);
        }
        if self.own {
            o["own"] = json!(true);
        }
        if !self.words.is_empty() {
            o["words"] = Value::Array(self.words.iter().map(|w| json!({ "text": w.text, "raw": w.raw, "start": w.start, "end": w.end, "nl": w.nl })).collect());
        }
        if !self.hl.is_empty() {
            o["hl"] = json!(self.hl);
        }
        if !self.anim.is_empty() {
            o["anim"] = json!(self.anim);
        }
        o
    }
}

impl Proj {
    pub fn state_json(&self) -> Value {
        let mut o = self.keep.clone();
        o.insert("segs".into(), Value::Array(self.segs.iter().map(|s| s.to_json()).collect()));
        o.insert("texts".into(), Value::Array(self.texts.iter().map(|t| t.to_json()).collect()));
        o.insert("boxes".into(), Value::Array(self.boxes.iter().map(|b| b.to_json()).collect()));
        o.insert("audio".into(), Value::Array(self.audio.iter().map(|a| a.to_json()).collect()));
        o.insert("over".into(), Value::Array(self.over.iter().map(|a| a.to_json()).collect()));
        o.insert("volume".into(), json!(self.volume));
        o.insert("mute".into(), json!(self.mute));
        o.insert("vertical".into(), self.vertical.map(|v| json!(v)).unwrap_or(Value::Null));
        o.insert("capBox".into(), json!({ "w": self.cap_w, "lines": self.cap_lines }));
        Value::Object(o)
    }
    pub fn from_state(st: &Value, dur: f64) -> Proj {
        let segs: Vec<Seg> = st.get("segs").and_then(|a| a.as_array()).map(|a| a.iter().map(Seg::from_json).collect()).unwrap_or_default();
        let texts = st.get("texts").and_then(|a| a.as_array()).map(|a| a.iter().map(Txt::from_json).collect()).unwrap_or_default();
        let mut keep = st.as_object().cloned().unwrap_or_default();
        let boxes = st.get("boxes").and_then(|a| a.as_array()).map(|a| a.iter().map(BoxFx::from_json).collect()).unwrap_or_default();
        let audio = st.get("audio").and_then(|a| a.as_array()).map(|a| a.iter().map(Aud::from_json).collect()).unwrap_or_default();
        let over = st.get("over").and_then(|a| a.as_array()).map(|a| a.iter().map(Ovl::from_json).collect()).unwrap_or_default();
        for k in ["segs", "texts", "boxes", "audio", "over", "volume", "mute", "vertical", "capBox"] {
            keep.remove(k);
        }
        Proj {
            segs: if segs.is_empty() { vec![Seg::new(0.0, dur, 0.0)] } else { segs },
            texts,
            boxes,
            audio,
            over,
            vertical: st.get("vertical").and_then(|v| v.as_f64()),
            cap_w: st.get("capBox").map(|c| f(c, "w", 0.7)).unwrap_or(0.7),
            cap_lines: st.get("capBox").map(|c| f(c, "lines", 1.0) as u32).unwrap_or(1).clamp(1, 3),
            volume: f(st, "volume", 1.0),
            mute: b(st, "mute"),
            keep,
        }
    }
}

pub fn fmt_t(t: f64) -> String {
    let t = t.max(0.0);
    let ds = (t * 10.0).floor() as u64;
    format!("{}:{:02}.{}", ds / 600, ds / 10 % 60, ds % 10)
}

// ---------- text width (for caption line breaks + hit testing on the video) ----------
thread_local! {
    static FONT: Option<(ab_glyph::FontVec, f64)> = crate::osd::load_font(&["segoeuib.ttf", "segoeui.ttf"]).map(|fv| {
        use ab_glyph::Font;
        let upem = fv.units_per_em().unwrap_or(2048.0) as f64;
        // libass sizes a font by its line height (ascent + descent), CSS / Echo's preview by the em: this factor
        // turns Echo's "size" (an em, as a fraction of the video height) into an ASS \fs
        let k = ((fv.ascent_unscaled() - fv.descent_unscaled()) as f64 / upem).clamp(1.0, 1.6);
        (fv, k)
    });
}

/// width of `s` in em (font size = 1); bold Segoe UI like the captions (DejaVu Sans where it's missing)
pub fn text_em(s: &str) -> f64 {
    FONT.with(|f| match f {
        Some((fv, _)) => {
            use ab_glyph::Font;
            let upem = fv.units_per_em().unwrap_or(2048.0) as f64;
            s.chars().map(|c| fv.h_advance_unscaled(fv.glyph_id(c)) as f64 / upem).sum::<f64>() * 1.04
        }
        None => s.chars().count() as f64 * 0.58,
    })
}

/// ASS font size for an em size (see FONT)
pub fn ass_k() -> f64 {
    FONT.with(|f| f.as_ref().map(|x| x.1).unwrap_or(1.33))
}

fn cap_join(ws: &[Word], raw: bool) -> String {
    let mut o = String::new();
    for (k, w) in ws.iter().enumerate() {
        if k > 0 {
            o.push(if w.nl { '\n' } else { ' ' });
        }
        o.push_str(if raw { &w.raw } else { &w.text });
    }
    o
}

fn ends_sentence(w: &str) -> bool {
    let t = w.trim_end_matches(|c| matches!(c, '"' | '\'' | ')' | ']'));
    t.ends_with(['.', '!', '?', '…'])
}

impl Editor {
    pub fn new(clip: PathBuf, name: String, dur: f64, w: u32, h: u32) -> Editor {
        Editor {
            clip,
            name,
            dur,
            w,
            h,
            p: Proj { segs: vec![Seg::new(0.0, dur, 0.0)], texts: vec![], boxes: vec![], audio: vec![], over: vec![], vertical: None, cap_w: 0.7, cap_lines: 1, volume: 1.0, mute: false, keep: Default::default() },
            hist: vec![],
            fut: vec![],
            sel: None,
            t: 0.0,
            playing: false,
            cur: 0,
            gap: false,
            pps: 40.0,
            scroll: 0.0,
            tl_w: 1000.0,
            snap: true,
            cap_style: "classic".into(),
            cap_hl: true,
            cap_hl_color: "ffd23f".into(),
            cap_show_box: false,
            normalize: false,
            drag: None,
            geo: Geo::default(),
            last_ov: String::new(),
            seek_at: None,
            last_tick: None,
            from_player: false,
            busy: false,
            rng: None,
            last_fx: [0.0; 7],
            last_vf: String::new(),
            hw_copied: false,
            tracks: 1,
            levels: vec![],
            last_af: String::new(),
            msel: vec![],
            layer_msg: None,
            mouse_y: 0.0,
            tl_top: 0.0,
            loaded: None,
            srcs: Default::default(),
        }
    }

    /// a copy of the edit settings with another piece list (Export range)
    pub fn with_proj(&self, p: Proj) -> Editor {
        let mut e = Editor::new(self.clip.clone(), self.name.clone(), self.dur, self.w, self.h);
        e.p = p;
        e.cap_style = self.cap_style.clone();
        e.cap_hl = self.cap_hl;
        e.cap_hl_color = self.cap_hl_color.clone();
        e.normalize = self.normalize;
        e.tracks = self.tracks;
        e.srcs = self.srcs.clone();
        e
    }

    pub fn set_rng(&mut self, a: f64, b: f64) {
        let tt = self.total();
        let (a, b) = (a.clamp(0.0, tt), b.clamp(0.0, tt));
        let (a, b) = if b < a { (b, a) } else { (a, b) };
        self.rng = if b - a >= 0.1 { Some((a, b)) } else { None };
    }

    /// take [a, b] out of the timeline: pieces are cut there, everything after slides left (texts + captions too)
    pub fn cut_span(p: &mut Proj, a: f64, b: f64) {
        let l = b - a;
        if l <= 0.0 {
            return;
        }
        let mut segs: Vec<Seg> = vec![];
        for s in p.segs.drain(..) {
            let (s0, s1) = (s.at, s.at + s.len());
            if s1 <= a + 1e-6 {
                segs.push(s);
                continue;
            }
            if s0 >= b - 1e-6 {
                segs.push(Seg { at: s.at - l, ..s });
                continue;
            }
            if s0 < a && s.rev() && !s.freeze() {
                // backwards: the part before a is the END of the source
                segs.push(Seg { start: s.end - (a - s0) * s.speed, fo: 0.0, ..s.clone() });
            } else if s0 < a {
                // the part before a
                segs.push(Seg { end: s.start + (a - s0) * s.speed, fo: 0.0, ..s.clone() });
            }
            if s0 < a && s.freeze() {
                // a freeze frame keeps its picture: just shorter
                let keep = (a - s0) + (s1 - b).max(0.0);
                segs.pop();
                segs.push(Seg { end: s.start + keep, ..s.clone() });
                continue;
            }
            if s1 > b {
                // the part after b (lands at a)
                let start = if s.freeze() || s.rev() { s.start } else { s.start + (b - s0).max(0.0) * s.speed };
                let end = if s.freeze() { s.start + (s1 - b) } else if s.rev() { s.end - (b - s0).max(0.0) * s.speed } else { s.end };
                let mut r = Seg { start, end, at: a, fi: 0.0, ..s.clone() };
                r.extra.remove("tr");
                segs.push(r);
            }
        }
        p.segs = segs.into_iter().filter(|s| s.end - s.start >= 0.02).collect();
        let shift = |t: f64| if t >= b { t - l } else if t > a { a } else { t };
        let mut texts = vec![];
        for mut t in p.texts.drain(..) {
            if t.start >= a - 1e-6 && t.end <= b + 1e-6 {
                continue; // inside: gone
            }
            if !t.words.is_empty() {
                t.words.retain(|w| { let m = (w.start + w.end) / 2.0; m < a || m > b });
                if t.words.is_empty() {
                    continue;
                }
                for w in t.words.iter_mut() {
                    w.start = shift(w.start);
                    w.end = shift(w.end);
                }
                t.words[0].nl = false;
                t.raw = cap_join(&t.words, true);
                t.text = cap_join(&t.words, false);
            }
            t.start = shift(t.start);
            t.end = shift(t.end);
            if t.end - t.start >= 0.1 {
                texts.push(t);
            }
        }
        p.texts = texts;
        let mut boxes = vec![];
        for mut bx in p.boxes.drain(..) {
            if bx.start >= a - 1e-6 && bx.end <= b + 1e-6 {
                continue;
            }
            bx.start = shift(bx.start);
            bx.end = shift(bx.end);
            if bx.end - bx.start >= 0.1 {
                boxes.push(bx);
            }
        }
        p.boxes = boxes;
        let mut auds = vec![];
        for x in p.audio.drain(..) {
            let (b0, b1) = (x.at, x.at + x.len());
            if b1 <= a + 1e-6 {
                auds.push(x);
                continue;
            }
            if b0 >= b - 1e-6 {
                auds.push(Aud { at: x.at - l, ..x });
                continue;
            }
            if b0 < a {
                auds.push(Aud { end: x.start + (a - b0) * x.speed, fo: 0.0, ..x.clone() });
            }
            if b1 > b {
                auds.push(Aud { start: x.start + (b - b0).max(0.0) * x.speed, at: a, fi: 0.0, ..x.clone() });
            }
        }
        p.audio = auds.into_iter().filter(|x| x.end - x.start >= 0.05).collect();
        let mut ovs = vec![];
        for x in p.over.drain(..) {
            let (b0, b1) = (x.at, x.at + x.len());
            if b1 <= a + 1e-6 {
                ovs.push(x);
                continue;
            }
            if b0 >= b - 1e-6 {
                ovs.push(Ovl { at: x.at - l, ..x });
                continue;
            }
            if b0 < a {
                ovs.push(Ovl { end: x.start + (a - b0) * x.speed, ..x.clone() });
            }
            if b1 > b {
                ovs.push(Ovl { start: x.start + (b - b0).max(0.0) * x.speed, at: a, ..x.clone() });
            }
        }
        p.over = ovs.into_iter().filter(|x| x.end - x.start >= 0.05).collect();
    }

    /// X: cut the highlighted part
    pub fn cut_range(&mut self) -> bool {
        let Some((a, b)) = self.rng.take() else { return false };
        self.push_hist();
        Editor::cut_span(&mut self.p, a, b);
        self.normalize_segs();
        self.sel = None;
        self.t = a.min(self.total());
        true
    }

    /// the edit limited to the highlighted part (Export range)
    pub fn range_proj(&self) -> Option<Proj> {
        let (a, b) = self.rng?;
        let mut p = self.p.clone();
        let tt = self.total();
        Editor::cut_span(&mut p, b, tt.max(b));
        Editor::cut_span(&mut p, 0.0, a);
        Some(p)
    }

    pub fn project_json(&self) -> String {
        json!({ "v": 1, "clip": self.clip.to_string_lossy(), "state": self.p.state_json(),
            "extra": { "capStyle": self.cap_style, "capHl": self.cap_hl, "capHlColor": self.cap_hl_color, "normalize": self.normalize } }).to_string()
    }

    pub fn load_project(&mut self, pj: &Value) {
        if let Some(st) = pj.get("state") {
            self.p = Proj::from_state(st, self.dur);
        }
        if let Some(x) = pj.get("extra") {
            if let Some(v) = x.get("capStyle").and_then(|v| v.as_str()) {
                self.cap_style = v.into();
            }
            if let Some(v) = x.get("capHl").and_then(|v| v.as_bool()) {
                self.cap_hl = v;
            }
            if let Some(v) = x.get("capHlColor").and_then(|v| v.as_str()) {
                self.cap_hl_color = v.into();
            }
            self.normalize = b(x, "normalize");
        }
        self.normalize_segs();
    }

    // ---------- timeline maths ----------
    pub fn total(&self) -> f64 {
        let t = self.p.segs.iter().map(|s| s.at + s.len()).fold(0.0, f64::max);
        self.p.over.iter().map(|o| o.at + o.len()).fold(t, f64::max)
    }

    /// output time -> (piece index, source time, in a gap)
    pub fn locate(&self, t: f64) -> (usize, f64, bool) {
        for (i, g) in self.p.segs.iter().enumerate() {
            if t < g.at {
                return (i, g.start, true);
            }
            if t < g.at + g.len() {
                return (i, if g.freeze() { g.start } else if g.rev() { g.end - (t - g.at) * g.speed } else { g.start + (t - g.at) * g.speed }, false);
            }
        }
        let l = self.p.segs.len().saturating_sub(1);
        (l, self.p.segs.get(l).map(|s| s.end - 0.01).unwrap_or(0.0), true)
    }

    /// sort by position, push overlapping pieces to the right
    pub fn normalize_segs(&mut self) {
        let cur = self.sel_seg().map(|i| self.p.segs[i].clone());
        self.p.segs.sort_by(|a, b| a.at.partial_cmp(&b.at).unwrap_or(std::cmp::Ordering::Equal));
        for k in 0..self.p.segs.len() {
            let min = if k > 0 { self.p.segs[k - 1].at + self.p.segs[k - 1].len() } else { 0.0 };
            let s = &mut self.p.segs[k];
            s.at = s.at.max(0.0).max(min);
        }
        if let Some(c) = cur {
            if let Some(i) = self.p.segs.iter().position(|s| s.start == c.start && s.end == c.end && s.speed == c.speed) {
                self.sel = Some(Sel::Seg(i));
            }
        }
    }

    pub fn sel_seg(&self) -> Option<usize> {
        match self.sel {
            Some(Sel::Seg(i)) if i < self.p.segs.len() => Some(i),
            _ => None,
        }
    }
    pub fn sel_box(&self) -> Option<usize> {
        match self.sel {
            Some(Sel::Box(i)) if i < self.p.boxes.len() => Some(i),
            _ => None,
        }
    }
    pub fn sel_aud(&self) -> Option<usize> {
        match self.sel {
            Some(Sel::Aud(i)) if i < self.p.audio.len() => Some(i),
            _ => None,
        }
    }
    pub fn sel_ov(&self) -> Option<usize> {
        match self.sel {
            Some(Sel::Ov(i)) if i < self.p.over.len() => Some(i),
            _ => None,
        }
    }
    pub fn sel_txt(&self) -> Option<usize> {
        match self.sel {
            Some(Sel::Txt(i)) if i < self.p.texts.len() => Some(i),
            _ => None,
        }
    }

    pub fn push_hist(&mut self) {
        self.hist.push(self.p.clone());
        self.fut.clear();
        if self.hist.len() > 200 {
            self.hist.remove(0);
        }
    }
    pub fn undo(&mut self) -> bool {
        let Some(h) = self.hist.pop() else { return false };
        self.fut.push(std::mem::replace(&mut self.p, h));
        self.sel = None;
        self.msel.clear();
        self.t = self.t.min(self.total());
        true
    }
    pub fn redo(&mut self) -> bool {
        let Some(h) = self.fut.pop() else { return false };
        self.hist.push(std::mem::replace(&mut self.p, h));
        self.sel = None;
        self.msel.clear();
        self.t = self.t.min(self.total());
        true
    }

    fn sort_texts(&mut self) {
        let cur = self.sel_txt().map(|i| self.p.texts[i].clone());
        self.p.texts.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
        if let Some(c) = cur {
            if let Some(i) = self.p.texts.iter().position(|t| *t == c) {
                self.sel = Some(Sel::Txt(i));
            }
        }
    }

    // ---------- actions ----------
    /// S: cut the piece under the playhead (Err = the reason, for a toast)
    /// cut a separated-audio clip at output time t; true = it was cut
    fn cut_aud(&mut self, k: usize, t: f64) -> bool {
        let Some(o) = self.p.audio.get(k).cloned() else { return false };
        if t - o.at < 0.1 || o.at + o.len() - t < 0.1 {
            return false;
        }
        let src = o.start + (t - o.at) * o.speed;
        self.p.audio.splice(k..=k, [Aud { end: src, fo: 0.0, ..o.clone() }, Aud { start: src, at: t, fi: 0.0, ..o }]);
        true
    }

    pub fn split(&mut self) -> Result<(), &'static str> {
        let t = self.t;
        if let Some(k) = self.sel_ov() {
            let o = self.p.over[k].clone();
            if t - o.at < 0.1 || o.at + o.len() - t < 0.1 {
                return Err("Move the playhead inside the selected piece to cut it");
            }
            self.push_hist();
            let src = o.start + (t - o.at) * o.speed;
            self.p.over.splice(k..=k, [Ovl { end: src, ..o.clone() }, Ovl { start: src, at: t, ..o }]);
            self.sel = Some(Sel::Ov(k + 1));
            return Ok(());
        }
        // a selected audio clip: cut just that one
        if let Some(k) = self.sel_aud() {
            let o = &self.p.audio[k];
            if t - o.at < 0.1 || o.at + o.len() - t < 0.1 {
                return Err("Move the playhead inside the selected piece to cut it");
            }
            self.push_hist();
            self.cut_aud(k, t);
            self.sel = Some(Sel::Aud(k + 1));
            return Ok(());
        }
        // otherwise the main video AND every separated audio clip under the playhead (they stay lined up)
        let under: Vec<usize> = (0..self.p.audio.len()).filter(|&k| { let a = &self.p.audio[k]; t - a.at >= 0.1 && a.at + a.len() - t >= 0.1 }).collect();
        {
            let (i, src, gap) = self.locate(t);
            let main_ok = self.p.segs.get(i).is_some_and(|s| !gap && if s.freeze() { t - s.at >= 0.1 && s.len() - (t - s.at) >= 0.1 } else { src - s.start >= 0.1 && s.end - src >= 0.1 });
            if !main_ok && !under.is_empty() {
                self.push_hist();
                for k in under.iter().rev() {
                    self.cut_aud(*k, t);
                }
                return Ok(());
            }
            if main_ok && !under.is_empty() {
                // cut the audio first (own undo step comes from the video cut below)
                let before = self.p.clone();
                for k in under.iter().rev() {
                    self.cut_aud(*k, t);
                }
                let r = self.split_main();
                if r.is_ok() {
                    if let Some(h) = self.hist.last_mut() {
                        *h = before;
                    }
                }
                return r;
            }
        }
        self.split_main()
    }

    fn split_main(&mut self) -> Result<(), &'static str> {
        let t = self.t;
        let (i, src, gap) = self.locate(t);
        let Some(s) = self.p.segs.get(i).cloned() else { return Err("Nothing to cut") };
        if s.freeze() {
            let dd = t - s.at;
            if gap || dd < 0.1 || s.len() - dd < 0.1 {
                return Err("Move the playhead inside a piece to cut");
            }
            self.push_hist();
            let a = Seg { end: s.start + dd, fo: 0.0, ..s.clone() };
            let bseg = Seg { end: s.start + s.len() - dd, at: t, fi: 0.0, ..s };
            self.p.segs.splice(i..=i, [a, bseg]);
            self.sel = Some(Sel::Seg(i + 1));
            return Ok(());
        }
        if gap || src - s.start < 0.1 || s.end - src < 0.1 {
            return Err("Move the playhead inside a piece to cut");
        }
        self.push_hist();
        if s.rev() {
            // backwards: the first half shows end -> src, the second src -> start (Echo)
            let a = Seg { start: src, fo: 0.0, ..s.clone() };
            let mut bseg = Seg { end: src, at: t, fi: 0.0, ..s };
            bseg.extra.remove("tr");
            self.p.segs.splice(i..=i, [a, bseg]);
            self.sel = Some(Sel::Seg(i + 1));
            return Ok(());
        }
        let a = Seg { end: src, fo: 0.0, ..s.clone() };
        let mut bseg = Seg { start: src, at: s.at + (src - s.start) / s.speed, fi: 0.0, ..s };
        bseg.extra.remove("tr");
        self.p.segs.splice(i..=i, [a, bseg]);
        self.sel = Some(Sel::Seg(i + 1));
        Ok(())
    }

    /// Del / Shift+Del (ripple: the gap closes, everything after slides left - captions and texts too)
    pub fn delete_sel(&mut self, ripple: bool) -> Result<&'static str, &'static str> {
        if self.msel.len() > 1 {
            // a multi-selection (Echo's delSel): texts / boxes / audio go, pieces right to left (the last one stays)
            self.push_hist();
            let has = |k: &Sel| self.msel.contains(k);
            let texts: Vec<Txt> = self.p.texts.iter().enumerate().filter(|(i, _)| !has(&Sel::Txt(*i))).map(|(_, t)| t.clone()).collect();
            let boxes: Vec<BoxFx> = self.p.boxes.iter().enumerate().filter(|(i, _)| !has(&Sel::Box(*i))).map(|(_, t)| t.clone()).collect();
            let audio: Vec<Aud> = self.p.audio.iter().enumerate().filter(|(i, _)| !has(&Sel::Aud(*i))).map(|(_, t)| t.clone()).collect();
            let over: Vec<Ovl> = self.p.over.iter().enumerate().filter(|(i, _)| !has(&Sel::Ov(*i))).map(|(_, t)| t.clone()).collect();
            self.p.over = over;
            let mut segs: Vec<usize> = self.msel.iter().filter_map(|s| if let Sel::Seg(i) = s { Some(*i) } else { None }).collect();
            segs.sort_unstable_by(|a, b| b.cmp(a));
            self.p.texts = texts;
            self.p.boxes = boxes;
            self.p.audio = audio;
            let mut kept = false;
            for i in segs {
                if self.p.segs.len() > 1 && i < self.p.segs.len() {
                    self.delete_seg(i, ripple);
                } else {
                    kept = true;
                }
            }
            self.msel.clear();
            self.sel = None;
            self.t = self.t.min(self.total());
            return Ok(if kept { "Deleted - the last piece stays" } else { "" });
        }
        match self.sel {
            Some(Sel::Seg(i)) if i < self.p.segs.len() => {
                if self.p.segs.len() < 2 {
                    return Err("Can't delete the last piece");
                }
                self.push_hist();
                self.delete_seg(i, ripple);
                self.sel = None;
                self.t = self.t.min(self.total());
                Ok(if ripple { "" } else { "Deleted - the gap stays (Shift+Del closes it)" })
            }
            Some(Sel::Txt(i)) if i < self.p.texts.len() => {
                self.push_hist();
                self.p.texts.remove(i);
                self.sel = None;
                Ok("")
            }
            Some(Sel::Box(i)) if i < self.p.boxes.len() => {
                self.push_hist();
                self.p.boxes.remove(i);
                self.sel = None;
                Ok("")
            }
            Some(Sel::Aud(i)) if i < self.p.audio.len() => {
                self.push_hist();
                self.p.audio.remove(i);
                self.sel = None;
                Ok("")
            }
            Some(Sel::Ov(i)) if i < self.p.over.len() => {
                self.push_hist();
                self.p.over.remove(i);
                self.sel = None;
                self.t = self.t.min(self.total());
                Ok("")
            }
            _ => Err("Select a piece or text first"),
        }
    }

    fn delete_seg(&mut self, i: usize, ripple: bool) {
        let g = self.p.segs[i].clone();
        let (a0, a1) = (g.at, g.at + g.len());
        let l = if ripple { g.len() } else { 0.0 };
        let eps = 0.01;
        let mut done: Vec<bool> = vec![false; self.p.texts.len()];
        let mut keep = vec![true; self.p.texts.len()];
        for (k, t) in self.p.texts.iter_mut().enumerate() {
            if !t.cap || t.end <= a0 + eps || t.start >= a1 - eps {
                continue;
            }
            if t.start >= a0 - eps && t.end <= a1 + eps {
                keep[k] = false;
                continue;
            }
            let mut words: Vec<Word> = t.words.iter().filter(|w| (w.start + w.end) / 2.0 < a0 || (w.start + w.end) / 2.0 > a1).cloned().collect();
            if !t.words.is_empty() && words.is_empty() {
                keep[k] = false;
                continue;
            }
            let (before, after) = (t.start < a0, t.end > a1);
            t.start = if before { t.start } else { a1 - l };
            t.end = if after { t.end - l } else { a0 };
            for w in words.iter_mut() {
                if (w.start + w.end) / 2.0 > a1 {
                    w.start -= l;
                    w.end -= l;
                }
                w.start = w.start.max(t.start);
                w.end = w.end.min(t.end);
            }
            if !t.words.is_empty() {
                if let Some(w0) = words.first_mut() {
                    w0.nl = false;
                }
                t.raw = cap_join(&words, true);
                t.text = cap_join(&words, false);
                t.words = words;
            }
            if t.end - t.start < 0.1 {
                keep[k] = false;
                continue;
            }
            done[k] = true;
        }
        if l > 0.0 {
            for (k, t) in self.p.texts.iter_mut().enumerate() {
                if keep[k] && !done[k] && t.start >= a1 - eps {
                    t.start -= l;
                    t.end -= l;
                    for w in t.words.iter_mut() {
                        w.start -= l;
                        w.end -= l;
                    }
                }
            }
        }
        let mut k = 0;
        self.p.texts.retain(|_| {
            k += 1;
            keep[k - 1]
        });
        if l > 0.0 {
            for bx in self.p.boxes.iter_mut().filter(|bx| bx.start >= a1 - eps) {
                bx.start -= l;
                bx.end -= l;
            }
            // layers after the piece slide left too (Echo)
            for o in self.p.over.iter_mut().filter(|o| o.at >= a1 - eps) {
                o.at -= l;
            }
        }
        // separated audio: gone if inside, trimmed if it crosses a cut, slides left after it (ripple)
        let mut auds = vec![];
        for mut a in self.p.audio.drain(..) {
            let (b0, b1) = (a.at, a.at + a.len());
            if b1 <= a0 + eps || b0 >= a1 - eps {
                if l > 0.0 && b0 >= a1 - eps {
                    a.at -= l;
                }
                auds.push(a);
                continue;
            }
            if b0 >= a0 - eps && b1 <= a1 + eps {
                continue;
            }
            if b0 < a0 && b1 > a1 {
                auds.push(a); // spans the whole removed part: leave it
                continue;
            }
            if b0 < a0 {
                a.end = a.start + (a0 - b0) * a.speed;
            } else {
                a.start += (a1 - b0) * a.speed;
                a.at = a1 - l;
            }
            if a.end - a.start > 0.05 {
                auds.push(a);
            }
        }
        self.p.audio = auds;
        self.p.segs.remove(i);
        if l > 0.0 {
            for s in self.p.segs.iter_mut().skip(i) {
                s.at = (s.at - l).max(0.0);
            }
        }
        self.normalize_segs();
    }

    fn shift_from(&mut self, from: f64, l: f64) {
        for s in self.p.segs.iter_mut() {
            if s.at >= from {
                s.at -= l;
            }
        }
        for bx in self.p.boxes.iter_mut().filter(|bx| bx.start >= from) {
            bx.start -= l;
            bx.end -= l;
        }
        for a in self.p.audio.iter_mut().filter(|a| a.at >= from) {
            a.at -= l;
        }
        for o in self.p.over.iter_mut().filter(|o| o.at >= from) {
            o.at -= l;
        }
        for t in self.p.texts.iter_mut() {
            if t.start >= from {
                t.start -= l;
                t.end -= l;
                for w in t.words.iter_mut() {
                    w.start -= l;
                    w.end -= l;
                }
            }
        }
    }

    /// close the empty space at output time t on the video track: everything after it slides left (Echo's closeGap)
    pub fn close_gap(&mut self, t: f64) -> Result<f64, &'static str> {
        let after = self.p.segs.iter().filter(|s| s.at >= t - 0.01).map(|s| s.at).fold(f64::INFINITY, f64::min);
        if !after.is_finite() {
            return Err("No gap there");
        }
        let prev_end = self.p.segs.iter().filter(|s| s.at + s.len() <= after + 0.01).map(|s| s.at + s.len()).fold(0.0, f64::max);
        let l = after - prev_end;
        if l < 0.01 {
            return Err("No gap there");
        }
        self.push_hist();
        self.shift_from(after - 0.01, l);
        self.normalize_segs();
        self.t = self.t.min(self.total());
        Ok(l)
    }

    pub fn close_all_gaps(&mut self) -> usize {
        let mut n = 0;
        let before = self.p.clone();
        loop {
            self.normalize_segs();
            let mut end = 0.0;
            let mut found = None;
            for s in &self.p.segs {
                if s.at - end > 0.01 {
                    found = Some((s.at - end, s.at - 0.01));
                    break;
                }
                end = s.at + s.len();
            }
            let Some((l, from)) = found else { break };
            self.shift_from(from, l);
            n += 1;
        }
        if n > 0 {
            self.hist.push(before);
            self.fut.clear();
            self.t = self.t.min(self.total());
        }
        n
    }

    pub fn set_speed(&mut self, i: usize, sp: f64) {
        if i >= self.p.segs.len() {
            return;
        }
        self.push_hist();
        let old = self.p.segs[i].len();
        self.p.segs[i].speed = sp.clamp(0.25, 2.0);
        // pieces right behind it stay attached (Echo's normalize only pushes right; a faster piece leaves a gap -
        // close it if they touched before)
        let new = self.p.segs[i].len();
        let end_old = self.p.segs[i].at + old;
        if new < old {
            let d = old - new;
            if let Some(nx) = self.p.segs.get(i + 1) {
                if (nx.at - end_old).abs() < 0.02 {
                    self.shift_from(end_old - 0.01, d);
                }
            }
        }
        self.normalize_segs();
    }

    pub fn add_text(&mut self) {
        self.push_hist();
        let tt = self.total();
        let start = self.t.min((tt - 1.0).max(0.0));
        self.p.texts.push(Txt { text: "Text".into(), raw: "Text".into(), start, end: (self.t + 3.0).min(tt).max(start + 0.5), x: 0.5, y: 0.2, size: 0.08, color: "ffffff".into(), cap: false, own: false, words: vec![], hl: String::new(), anim: String::new() });
        self.sel = Some(Sel::Txt(self.p.texts.len() - 1));
    }

    /// U: the selected piece's sound becomes its own rows (Echo's detachAudio): per-app tracks when the clip has
    /// them (Game, Discord, Browser, Music - only the ones with sound - plus All desktop sound muted, and Mic),
    /// Game + Mic for 3-track clips, every loud source for other apps' clips, else one Audio row. `levels` = mean
    /// volume (dB) per track (None = not measured yet: every track counts as loud).
    pub fn detach_audio(&mut self, tracks: u32, levels: &[f64]) -> Result<String, String> {
        let Some(i) = self.sel_seg() else { return Err("Select a video piece, then press U".into()) };
        let s = self.p.segs[i].clone();
        if s.extra.get("amute").and_then(|x| x.as_bool()) == Some(true) {
            return Err("This piece's audio is already separate".into());
        }
        if s.freeze() {
            return Err("A freeze frame has no sound".into());
        }
        if tracks == 0 {
            return Err("This clip has no audio".into());
        }
        self.push_hist();
        self.p.segs[i].extra.insert("amute".into(), json!(true));
        let base = |track: u32, label: &str, mute: bool| Aud { start: s.start, end: s.end, speed: s.speed, at: s.at, vol: 1.0, mute, track, label: label.into(), fi: 0.0, fo: 0.0, extra: Default::default() };
        let loud = |t: u32| t < tracks && levels.get(t as usize).map_or(true, |&l| l > -70.0);
        let msg;
        if tracks >= 6 {
            let rows: Vec<(u32, &str)> = [(3, "Game"), (4, "Discord"), (5, "Browser"), (6, "Music")].into_iter().filter(|(t, _)| loud(*t)).collect();
            for (t, l) in &rows {
                self.p.audio.push(base(*t, l, false));
            }
            self.p.audio.push(base(1, if rows.is_empty() { "Desktop" } else { "All desktop sound" }, !rows.is_empty()));
            let mic = loud(2);
            if mic {
                self.p.audio.push(base(2, "Mic", false));
            }
            let names: Vec<&str> = rows.iter().map(|r| r.1).chain(if mic { Some("Mic") } else { None }).collect();
            msg = if rows.is_empty() { "No game / Discord / browser / music sound in this clip - split into Desktop and Mic".to_string() } else { format!("Split into {} (All desktop sound is muted - unmute it for other apps)", names.join(", ")) };
        } else if tracks == 3 {
            self.p.audio.push(base(1, "Game", false));
            self.p.audio.push(base(2, "Mic", false));
            msg = "Split into Game and Mic tracks".into();
        } else if tracks >= 2 {
            let src: Vec<u32> = (1..tracks).filter(|&t| loud(t)).collect();
            if src.is_empty() {
                self.p.audio.push(base(0, "Audio", false));
                msg = "The extra tracks in this clip are silent - kept the full sound".into();
            } else {
                for (k, t) in src.iter().enumerate() {
                    self.p.audio.push(base(*t, &format!("Sound {}", k + 1), false));
                }
                msg = format!("Split into {} sounds from the app that recorded it - play them to find your mic, then mute or delete that row", src.len());
            }
        } else {
            self.p.audio.push(base(0, "Audio", false));
            msg = "Audio separated (this clip was recorded before separate Game/Mic tracks)".into();
        }
        self.sel = Some(Sel::Aud(self.p.audio.len() - 1));
        Ok(msg)
    }

    /// B / Z: a blur box (or zoom) for 3 s at the playhead
    pub fn add_box(&mut self, kind: &str) {
        self.push_hist();
        let tt = self.total();
        let zoom = kind == "zoom";
        let start = self.t.min((tt - 1.0).max(0.0));
        self.p.boxes.push(BoxFx { start, end: (self.t + 3.0).min(tt).max(start + 0.5), x: 0.5, y: 0.5, w: if zoom { 0.5 } else { 0.25 }, h: if zoom { 0.5 } else { 0.15 },
            kind: kind.into(), color: "000000".into(), opacity: 1.0, blur: 20.0, extra: Default::default() });
        self.sel = Some(Sel::Box(self.p.boxes.len() - 1));
    }

    /// F: a 2 s freeze frame of the picture at the playhead (the piece is cut there, the rest moves right)
    pub fn freeze_frame(&mut self) -> Result<(), &'static str> {
        let t = self.t;
        let (i, src, gap) = self.locate(t);
        let Some(s) = self.p.segs.get(i).cloned() else { return Err("Put the playhead on a normal piece first") };
        if gap || s.freeze() || s.extra.get("rev").and_then(|x| x.as_bool()) == Some(true) {
            return Err("Put the playhead on a normal piece first");
        }
        self.push_hist();
        let d = 2.0;
        let mut idx = i + 1;
        if src - s.start >= 0.05 && s.end - src >= 0.05 {
            let a = Seg { end: src, fo: 0.0, ..s.clone() };
            let mut bseg = Seg { start: src, at: t, fi: 0.0, ..s.clone() };
            bseg.extra.remove("tr");
            self.p.segs.splice(i..=i, [a, bseg]);
        } else if src - s.start < 0.05 {
            idx = i;
        }
        for k in idx..self.p.segs.len() {
            self.p.segs[k].at += d;
        }
        for tx in self.p.texts.iter_mut().filter(|tx| tx.start >= t - 0.01) {
            tx.start += d;
            tx.end += d;
            for w in tx.words.iter_mut() {
                w.start += d;
                w.end += d;
            }
        }
        for bx in self.p.boxes.iter_mut().filter(|bx| bx.start >= t - 0.01) {
            bx.start += d;
            bx.end += d;
        }
        for a in self.p.audio.iter_mut().filter(|a| a.at >= t - 0.01) {
            a.at += d;
        }
        for o in self.p.over.iter_mut().filter(|o| o.at >= t - 0.01) {
            o.at += d;
        }
        let mut fz = Seg::new(src, src + d, t);
        fz.extra.insert("freeze".into(), json!(true));
        if let Some(sp) = s.extra.get("src") {
            fz.extra.insert("src".into(), sp.clone());
        }
        self.p.segs.insert(idx, fz);
        self.sel = Some(Sel::Seg(idx));
        Ok(())
    }

    // ---------- captions ----------
    /// source time -> output time (None when that moment was cut)
    pub fn src_to_out(&self, t: f64) -> Option<f64> {
        self.p.segs.iter().filter(|s| !s.special()).find(|s| t >= s.start && t < s.end).map(|s| s.at + (t - s.start) / s.speed)
    }

    /// words from whisper (clip time) -> captions on the edited timeline
    pub fn set_words(&mut self, words: &[(String, f64, f64)]) -> usize {
        let mut wt: Vec<Word> = vec![];
        for (text, a, bb) in words {
            let Some(s0) = self.src_to_out(*a) else { continue };
            let e0 = self.src_to_out(a.max(bb - 0.01)).unwrap_or(s0 + (bb - a));
            wt.push(Word { text: text.trim().into(), raw: text.trim().into(), start: s0, end: e0.max(s0 + 0.08), nl: false });
        }
        if wt.is_empty() {
            return 0;
        }
        self.push_hist();
        self.p.texts.retain(|t| !t.cap);
        let (a, z) = (wt[0].start, wt[wt.len() - 1].end);
        self.p.texts.push(Txt { text: String::new(), raw: String::new(), start: a, end: z, x: 0.5, y: 0.83, size: 0.065, color: "ffffff".into(), cap: true, own: false, words: wt, hl: String::new(), anim: String::new() });
        let st = self.cap_style.clone();
        self.apply_cap_style(&st);
        self.rechunk_caps();
        self.apply_cap_hl();
        self.p.texts.iter().filter(|t| t.cap).count()
    }

    pub fn apply_cap_style(&mut self, name: &str) {
        let st = cap_style(name);
        self.cap_style = st.name.into();
        self.cap_hl_color = st.hl.into();
        for t in self.p.texts.iter_mut().filter(|t| t.cap) {
            t.color = st.color.into();
            t.size = st.size;
            t.text = if st.upper { t.raw.to_uppercase() } else { t.raw.clone() };
            for w in t.words.iter_mut() {
                w.text = if st.upper { w.raw.to_uppercase() } else { w.raw.clone() };
            }
        }
        self.apply_cap_hl();
    }

    pub fn apply_cap_hl(&mut self) {
        let (on, c) = (self.cap_hl, self.cap_hl_color.clone());
        for t in self.p.texts.iter_mut().filter(|t| t.cap) {
            t.hl = if on && !t.words.is_empty() { c.clone() } else { String::new() };
        }
    }

    /// Echo's rechunkCaps: split all spoken words into captions that fit the caption box (width x lines). A new
    /// sentence, a pause or a cut starts a new caption; a sentence that doesn't fit never leaves 1-2 lonely words
    /// for the next caption (a few words are pulled over so the next one starts cleanly).
    pub fn rechunk_caps(&mut self) {
        let timed: Vec<Txt> = self.p.texts.iter().filter(|t| t.cap && !t.words.is_empty()).cloned().collect();
        if timed.is_empty() {
            return;
        }
        let mut p = timed.iter().find(|t| !t.own).unwrap_or(&timed[0]).clone();
        p.own = false;
        let ar = if self.p.vertical.is_some() { 9.0 / 16.0 } else if self.h > 0 { self.w as f64 / self.h as f64 } else { 16.0 / 9.0 };
        let owned: Vec<(f64, f64, f64, f64)> = timed.iter().filter(|t| t.own).map(|t| (t.start, t.end, t.x, t.y)).collect();
        let max_em = (self.p.cap_w * ar / p.size).max(2.0);
        let mut words: Vec<Word> = timed.iter().flat_map(|t| t.words.iter().map(|w| Word { nl: false, ..w.clone() })).collect();
        words.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
        let lines_max = self.p.cap_lines.max(1);
        struct Chunk {
            words: Vec<Word>,
            seg: i64,
            lines: u32,
            last: f64,
        }
        let seg_at = |e: &Editor, t: f64| -> i64 {
            let (i, _, gap) = e.locate(t + 0.001);
            if gap { -1 } else { i as i64 }
        };
        let mut chunks: Vec<Chunk> = vec![];
        let mut line = String::new();
        let mut j = 0;
        while j < words.len() {
            let mut w = words[j].clone();
            let seg = seg_at(self, w.start);
            let wider = !line.is_empty() && text_em(&format!("{line} {}", w.text)) > max_em;
            let cur = chunks.last();
            let pause = cur.is_some_and(|c| w.start - c.last > if lines_max > 1 { 1.5 } else { 0.6 });
            let sent_end = cur.is_some_and(|c| c.words.last().is_some_and(|x| ends_sentence(&x.text)));
            let full = cur.is_some_and(|c| wider && c.lines >= lines_max);
            if cur.is_none() || cur.is_some_and(|c| seg != c.seg) || pause || sent_end || full {
                let mut carry: Vec<Word> = vec![];
                if let Some(c) = chunks.last_mut() {
                    if full && !pause && !sent_end && seg == c.seg {
                        let mut n = 0;
                        for k in j..words.len() {
                            n += 1;
                            if ends_sentence(&words[k].text) || n > 2 {
                                break;
                            }
                        }
                        if n <= 2 {
                            while carry.len() < 3 - n && c.words.len() > 2 && !c.words.last().is_some_and(|x| ends_sentence(&x.text)) {
                                carry.insert(0, c.words.pop().unwrap());
                            }
                        }
                        if !carry.is_empty() {
                            c.last = c.words.iter().map(|x| x.end).fold(0.0, f64::max);
                        }
                    }
                }
                let mut nc = Chunk { words: vec![], seg, lines: 1, last: w.end };
                for mut c in carry.into_iter() {
                    c.nl = false;
                    nc.words.push(c);
                }
                line = nc.words.iter().map(|c| c.text.as_str()).chain(std::iter::once(w.text.as_str())).collect::<Vec<_>>().join(" ");
                if !nc.words.is_empty() && text_em(&line) > max_em && lines_max > 1 {
                    nc.lines += 1;
                    w.nl = true;
                    line = w.text.clone();
                }
                chunks.push(nc);
            } else if wider {
                let c = chunks.last_mut().unwrap();
                c.lines += 1;
                w.nl = true;
                line = w.text.clone();
            } else {
                line.push(' ');
                line.push_str(&w.text);
            }
            let c = chunks.last_mut().unwrap();
            c.last = c.last.max(w.end);
            c.words.push(w);
            j += 1;
        }
        let old = self.sel_txt().map(|i| self.p.texts[i].clone());
        self.p.texts.retain(|t| !t.cap || t.words.is_empty());
        let n = chunks.len();
        for i in 0..n {
            let next_start = chunks.get(i + 1).map(|c| c.words[0].start);
            let c = &mut chunks[i];
            let start = c.words[0].start;
            let mut end = c.last.max(start + 0.45);
            if let Some(ns) = next_start {
                end = end.min(ns);
            }
            let m = c.words.len();
            for k in 0..m {
                let lim = if k + 1 < m { c.words[k + 1].start } else { end };
                c.words[k].end = c.words[k].end.min(lim);
            }
            let mut t = Txt { text: cap_join(&c.words, false), raw: cap_join(&c.words, true), start, end, words: c.words.clone(), ..p.clone() };
            if let Some(o) = owned.iter().find(|q| start >= q.0 - 0.05 && start < q.1) {
                t.x = o.2;
                t.y = o.3;
                t.own = true;
            }
            self.p.texts.push(t);
        }
        self.sort_texts();
        // keep a caption selected after re-splitting (the caption box stays up while resizing)
        if let Some(o) = old {
            if !self.p.texts.contains(&o) && o.cap {
                let tt = self.t;
                let k = self.p.texts.iter().position(|t| t.cap && t.start <= tt + 0.01 && t.end >= tt - 0.01).or_else(|| self.p.texts.iter().position(|t| t.cap));
                self.sel = k.map(Sel::Txt);
            }
        }
    }

    /// captions as plain text, one per line (Save captions as .txt)
    pub fn captions_txt(&self) -> String {
        self.p.texts.iter().filter(|t| t.cap).map(|t| format!("[{}] {}", fmt_t(t.start), t.raw.replace('\n', " "))).collect::<Vec<_>>().join("\n")
    }

    /// the caption the caption box follows: the selected caption, else ("Show caption box") the first one in the
    /// common spot
    pub fn cap_ref(&self) -> Option<usize> {
        if let Some(i) = self.sel_txt().filter(|&i| self.p.texts[i].cap) {
            return Some(i);
        }
        if !self.cap_show_box {
            return None;
        }
        self.p.texts.iter().position(|t| t.cap && !t.own).or_else(|| self.p.texts.iter().position(|t| t.cap))
    }

    /// a caption's text typed in the captions list (Echo's caplist oninput). The same number of words as `base`
    /// (the words when the typing started) = every word keeps its own timing, so the spoken-word highlight stays in
    /// sync; Shift+Enter starts a 2nd line. A word more or less = the words are spread over the caption's time (the
    /// highlight keeps running; Echo dropped it).
    pub fn set_cap_text(&mut self, i: usize, val: &str, base: Option<Vec<Word>>) {
        let upper = cap_style(&self.cap_style).upper;
        let Some(t) = self.p.texts.get_mut(i) else { return };
        let up = |s: &str| if upper { s.to_uppercase() } else { s.to_string() };
        let mut parts: Vec<String> = vec![];
        let mut nls: Vec<bool> = vec![];
        for (li, ln) in val.split('\n').enumerate() {
            for (k, w) in ln.split_whitespace().enumerate() {
                parts.push(w.to_string());
                nls.push(li > 0 && k == 0);
            }
        }
        let base = base.filter(|b| !b.is_empty()).unwrap_or_else(|| t.words.clone());
        if base.is_empty() {
            // a caption without spoken words (added by hand): just its text
            t.raw = val.to_string();
            t.text = up(val);
            return;
        }
        if parts.is_empty() {
            // emptied while retyping: the timings stay for the new words
            t.words = base.iter().map(|w| Word { text: String::new(), raw: String::new(), ..w.clone() }).collect();
            t.raw.clear();
            t.text.clear();
            return;
        }
        if parts.len() == base.len() {
            t.words = base.iter().zip(parts.iter()).zip(nls.iter()).map(|((w, p), nl)| Word { text: up(p), raw: p.clone(), nl: *nl, ..w.clone() }).collect();
        } else {
            let n = parts.len() as f64;
            let (s0, l) = (t.start, t.end - t.start);
            t.words = parts.iter().zip(nls.iter()).enumerate().map(|(k, (w, nl))| Word { text: up(w), raw: w.clone(), start: s0 + l * k as f64 / n, end: s0 + l * (k + 1) as f64 / n, nl: *nl }).collect();
        }
        t.raw = cap_join(&t.words, true);
        t.text = cap_join(&t.words, false);
    }

    /// "+ Add at playhead" (Echo's ecapadd): an empty caption for 1.5 s at the bottom
    pub fn add_caption(&mut self) -> usize {
        self.push_hist();
        let st = cap_style(&self.cap_style);
        let tt = self.t;
        let c = Txt { text: if st.upper { "CAPTION".into() } else { "Caption".into() }, raw: "Caption".into(), start: tt, end: (tt + 1.5).min(self.total().max(tt + 0.5)),
            x: 0.5, y: 0.83, size: st.size, color: st.color.into(), cap: true, own: false, words: vec![], hl: String::new(), anim: String::new() };
        self.p.texts.push(c.clone());
        self.msel.clear();
        self.sel = Some(Sel::Txt(self.p.texts.len() - 1));
        self.sort_texts();
        self.sel_txt().unwrap_or(0)
    }

    /// "+ Line above" / "+ Line below" (Echo's stack): a second caption at the same time, one line above / below
    pub fn stack_caption(&mut self, i: usize, dir: f64) -> Option<usize> {
        let t = self.p.texts.get(i)?.clone();
        self.push_hist();
        let st = cap_style(&self.cap_style);
        let gap_y = t.size * 1.25 * (1.0 + (t.text.split('\n').count() as f64 - 1.0) * 0.5);
        let c = Txt { text: if st.upper { "CAPTION".into() } else { "Caption".into() }, raw: "Caption".into(), start: t.start, end: t.end, x: t.x, y: (t.y + dir * gap_y).clamp(0.03, 0.97),
            size: t.size, color: t.color.clone(), cap: true, own: true, words: vec![], hl: String::new(), anim: t.anim.clone() };
        self.p.texts[i].own = true;
        self.p.texts.push(c);
        self.msel.clear();
        self.sel = Some(Sel::Txt(self.p.texts.len() - 1));
        self.sort_texts();
        self.sel_txt()
    }

    /// "Back to the other captions' spot"
    pub fn cap_back(&mut self, i: usize) {
        if i >= self.p.texts.len() {
            return;
        }
        let o = self.p.texts.iter().enumerate().find(|(k, x)| *k != i && x.cap && !x.own).map(|(_, x)| (x.x, x.y));
        self.push_hist();
        let t = &mut self.p.texts[i];
        t.own = false;
        if let Some(o) = o {
            (t.x, t.y) = o;
        }
    }

    /// a caption's start / end typed or nudged in the list; the words move with the start (Echo). false = refused
    pub fn set_cap_time(&mut self, i: usize, end: bool, v: f64) -> bool {
        let Some(t) = self.p.texts.get_mut(i) else { return false };
        let ok = v >= 0.0 && if end { v > t.start + 0.01 } else { v < t.end - 0.01 };
        if !ok {
            return false;
        }
        if end {
            t.end = v;
        } else {
            let d = v - t.start;
            for w in t.words.iter_mut() {
                w.start += d;
                w.end += d;
            }
            t.start = v;
        }
        self.sort_texts();
        true
    }

    /// Echo's capIdx: captions in time order (indexes into texts)
    pub fn cap_idx(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.p.texts.len()).filter(|&i| self.p.texts[i].cap).collect();
        v.sort_by(|&a, &b| self.p.texts[a].start.partial_cmp(&self.p.texts[b].start).unwrap_or(std::cmp::Ordering::Equal));
        v
    }

    // ---------- timeline mouse (x in content px incl. scroll; y inside the timeline area) ----------
    fn snap_t(&self, t: f64, l: f64, skip: Option<usize>) -> f64 {
        if !self.snap {
            return t;
        }
        let tol = 10.0 / self.pps;
        let mut edges = vec![0.0, self.t];
        for (k, o) in self.p.segs.iter().enumerate() {
            if Some(k) != skip {
                edges.push(o.at);
                edges.push(o.at + o.len());
            }
        }
        for x in edges {
            if (t - x).abs() < tol {
                return x;
            }
            if l > 0.0 && (t + l - x).abs() < tol {
                return (x - l).max(0.0);
            }
        }
        t
    }

    /// Echo's text lanes: texts / captions showing at the same time get their own row (greedy by start)
    pub fn layout(&self) -> TlLayout {
        let mut order: Vec<usize> = (0..self.p.texts.len()).collect();
        order.sort_by(|&a, &b| self.p.texts[a].start.partial_cmp(&self.p.texts[b].start).unwrap_or(std::cmp::Ordering::Equal));
        let mut lane = vec![0; self.p.texts.len()];
        let mut ends: Vec<f64> = vec![];
        for i in order {
            let t = &self.p.texts[i];
            let l = ends.iter().position(|&e| e <= t.start + 0.001).unwrap_or(ends.len());
            if l == ends.len() {
                ends.push(t.end);
            } else {
                ends[l] = t.end;
            }
            lane[i] = l;
        }
        let lanes = ends.len().max(1);
        let mut aud_rows: Vec<u32> = self.p.audio.iter().map(|a| a.track).collect();
        aud_rows.sort();
        aud_rows.dedup();
        let ov_rows = self.p.over.iter().map(|o| o.row as usize).max().unwrap_or(0);
        let video_y = ROW_VIDEO.0 + ov_rows as f64 * OV_H;
        let aud_y = AUD_Y + ov_rows as f64 * OV_H;
        let text_y = if aud_rows.is_empty() { aud_y } else { aud_y + aud_rows.len() as f64 * AUD_H - 4.0 + 12.0 };
        let text_end = text_y + lanes as f64 * LANE_H;
        let fx = if self.p.boxes.is_empty() { None } else { Some((text_end + 12.0, text_end + 12.0 + LANE_H)) };
        let h = fx.map(|f| f.1).unwrap_or(text_end) + 16.0;
        TlLayout { lane, lanes, aud_rows, text_y, fx, h, ov_rows, video_y, aud_y }
    }

    /// which piece / text block is at (x, y): (kind 's'/'t'/'b', index, part 'l'/'m'/'r')
    pub fn tl_hit(&self, x: f64, y: f64) -> Option<(char, usize, char)> {
        let t = (x - LP) / self.pps;
        let edge = 9.0 / self.pps;
        let lay = self.layout();
        let part = |a: f64, z: f64| if z - a > 3.0 * edge && t - a < edge { 'l' } else if z - a > 3.0 * edge && z - t < edge { 'r' } else { 'm' };
        // layers: row r sits (ov_rows - r) rows above the main track
        if lay.ov_rows > 0 && y >= ROW_VIDEO.0 && y < lay.video_y {
            let r = lay.ov_rows as u32 - ((y - ROW_VIDEO.0) / OV_H) as u32;
            for (i, o) in self.p.over.iter().enumerate().rev() {
                if o.row == r && t >= o.at && t < o.at + o.len().max(8.0 / self.pps) {
                    return Some(('o', i, part(o.at, o.at + o.len())));
                }
            }
        }
        if y >= lay.video_y && y < lay.video_y + 64.0 {
            for (i, s) in self.p.segs.iter().enumerate() {
                if t >= s.at && t < s.at + s.len() {
                    // a reversed piece has no trim handles (Echo: isMan)
                    return Some(('s', i, if s.rev() { 'm' } else { part(s.at, s.at + s.len()) }));
                }
            }
        }
        if let Some(fx) = lay.fx.filter(|f| y >= f.0 && y < f.1) {
            let _ = fx;
            for (i, bx) in self.p.boxes.iter().enumerate().rev() {
                if t >= bx.start && t < bx.end.max(bx.start + 6.0 / self.pps) {
                    return Some(('b', i, part(bx.start, bx.end)));
                }
            }
        }
        if !lay.aud_rows.is_empty() && y >= lay.aud_y && y < lay.aud_y + lay.aud_rows.len() as f64 * AUD_H {
            let r = ((y - lay.aud_y) / AUD_H) as usize;
            for (i, a) in self.p.audio.iter().enumerate().rev() {
                if lay.aud_rows.get(r) == Some(&a.track) && t >= a.at && t < a.at + a.len().max(8.0 / self.pps) {
                    return Some(('a', i, part(a.at, a.at + a.len())));
                }
            }
        }
        if y >= lay.text_y && y < lay.text_y + lay.lanes as f64 * LANE_H {
            let l = ((y - lay.text_y) / LANE_H) as usize;
            for (i, tx) in self.p.texts.iter().enumerate().rev() {
                if lay.lane[i] == l && t >= tx.start && t < tx.end.max(tx.start + 8.0 / self.pps) {
                    return Some(('t', i, part(tx.start, tx.end)));
                }
            }
        }
        None
    }

    fn follow_piece(&mut self, follow: &[(usize, f64, f64, Vec<(f64, f64)>)], d: f64) {
        for (k, s0, e0, ws) in follow {
            if let Some(t) = self.p.texts.get_mut(*k) {
                t.start = s0 + d;
                t.end = e0 + d;
                for (w, (a, b)) in t.words.iter_mut().zip(ws.iter()) {
                    w.start = a + d;
                    w.end = b + d;
                }
            }
        }
    }

    /// selected, alone or as part of a multi-selection
    pub fn is_sel(&self, s: Sel) -> bool {
        self.sel == Some(s) || self.msel.contains(&s)
    }

    /// every block on the timeline with its rectangle (content x0, x1, y0, y1) - box-select hit test
    pub fn block_rects(&self) -> Vec<(Sel, f64, f64, f64, f64)> {
        let lay = self.layout();
        let px = |t: f64| LP + t * self.pps;
        let mut v = vec![];
        for (i, g) in self.p.segs.iter().enumerate() {
            v.push((Sel::Seg(i), px(g.at), px(g.at + g.len()), lay.video_y, lay.video_y + 64.0));
        }
        for (i, a) in self.p.audio.iter().enumerate() {
            let r = lay.aud_rows.iter().position(|t| *t == a.track).unwrap_or(0) as f64;
            v.push((Sel::Aud(i), px(a.at), px(a.at + a.len()).max(px(a.at) + 8.0), lay.aud_y + r * AUD_H, lay.aud_y + r * AUD_H + 40.0));
        }
        for (i, o) in self.p.over.iter().enumerate() {
            let y = ROW_VIDEO.0 + (lay.ov_rows as f64 - o.row as f64) * OV_H;
            v.push((Sel::Ov(i), px(o.at), px(o.at + o.len()).max(px(o.at) + 8.0), y, y + 64.0));
        }
        for (i, t) in self.p.texts.iter().enumerate() {
            let y = lay.text_y + lay.lane[i] as f64 * LANE_H;
            v.push((Sel::Txt(i), px(t.start), px(t.end).max(px(t.start) + 8.0), y, y + 32.0));
        }
        if let Some(fx) = lay.fx {
            for (i, b) in self.p.boxes.iter().enumerate() {
                v.push((Sel::Box(i), px(b.start), px(b.end).max(px(b.start) + 8.0), fx.0, fx.0 + 32.0));
            }
        }
        v
    }

    /// every other file the edit uses (pieces + sounds), in a fixed order (export inputs 1..)
    pub fn other_files(&self) -> Vec<String> {
        let mut v: Vec<String> = vec![];
        let segs = self.p.segs.iter().filter_map(|g| g.src().map(String::from));
        let auds = self.p.audio.iter().filter_map(|a| a.extra.get("src").and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map(String::from));
        let ovs = self.p.over.iter().filter_map(|o| o.src().map(String::from));
        for f in segs.chain(auds).chain(ovs) {
            if !v.contains(&f) {
                v.push(f);
            }
        }
        v
    }

    /// Media: a video goes to the end of the video track, a sound becomes an audio row at the playhead (Echo's
    /// addFilesToEditor). Ok(toast) / Err(reason)
    pub fn add_media(&mut self, path: &Path, has_audio: bool, has_video: bool, dur: f64) -> Result<String, String> {
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let ps = path.to_string_lossy().to_string();
        let sound = crate::pl_media::is_sound(path) || !has_video;
        if sound && !has_audio {
            return Err(format!("{name}: no sound in it"));
        }
        self.srcs.insert(ps.clone(), (has_audio, dur));
        self.push_hist();
        self.msel.clear();
        if sound {
            let mut extra = serde_json::Map::new();
            extra.insert("src".into(), json!(ps));
            // own row above the clip's tracks (Echo: track 0 + src); the label is the file name
            self.p.audio.push(Aud { start: 0.0, end: dur, speed: 1.0, at: self.t, vol: 1.0, mute: false, track: 0, label: name.clone(), fi: 0.0, fo: 0.0, extra });
            self.sel = Some(Sel::Aud(self.p.audio.len() - 1));
        } else {
            let end = self.total();
            let mut g = Seg::new(0.0, dur, end);
            g.extra.insert("src".into(), json!(ps));
            self.p.segs.push(g);
            self.normalize_segs();
            self.sel = self.p.segs.iter().position(|s| s.src() == Some(ps.as_str()) && (s.at - end).abs() < 0.001).map(Sel::Seg);
        }
        Ok(format!("Added {name} to the timeline · drag it to move it"))
    }

    /// a sticker (Echo's addSticker): 18 % of the width, its own shape, 3 s from the playhead
    pub fn add_sticker(&mut self, path: &Path, iw: u32, ih: u32) {
        let ar = if self.h > 0 { self.w as f64 / self.h as f64 } else { 16.0 / 9.0 };
        let mut w = 0.18;
        let mut h = w * ih.max(1) as f64 / iw.max(1) as f64 * ar;
        if h > 0.5 {
            w *= 0.5 / h;
            h = 0.5;
        }
        let tt = self.total();
        self.push_hist();
        self.msel.clear();
        let mut extra = serde_json::Map::new();
        extra.insert("src".into(), json!(path.to_string_lossy()));
        let start = self.t.min((tt - 1.0).max(0.0));
        self.p.boxes.push(BoxFx { start, end: (self.t + 3.0).min(tt).max(start + 0.5), x: 0.5, y: 0.5, w, h, kind: "image".into(), color: String::new(), opacity: 1.0, blur: 0.0, extra });
        self.sel = Some(Sel::Box(self.p.boxes.len() - 1));
    }

    /// Ctrl+A: everything on the timeline (Echo: pieces, audio, layers, texts)
    pub fn select_all(&mut self) {
        let mut v: Vec<Sel> = (0..self.p.segs.len()).map(Sel::Seg).collect();
        v.extend((0..self.p.audio.len()).map(Sel::Aud));
        v.extend((0..self.p.texts.len()).map(Sel::Txt));
        v.extend((0..self.p.boxes.len()).map(Sel::Box));
        v.extend((0..self.p.over.len()).map(Sel::Ov));
        self.set_msel(v);
    }

    /// set the multi-selection; one entry = a plain selection (the inspector shows it)
    pub fn set_msel(&mut self, v: Vec<Sel>) {
        self.sel = None;
        self.msel.clear();
        match v.len() {
            0 => {}
            1 => self.sel = Some(v[0]),
            _ => self.msel = v,
        }
    }

    /// keep the multi-selection on the same objects while `f` re-sorts the lists
    fn keep_msel(&mut self, f: impl FnOnce(&mut Self)) {
        let segs: Vec<Seg> = self.msel.iter().filter_map(|s| if let Sel::Seg(i) = s { self.p.segs.get(*i).cloned() } else { None }).collect();
        let txts: Vec<Txt> = self.msel.iter().filter_map(|s| if let Sel::Txt(i) = s { self.p.texts.get(*i).cloned() } else { None }).collect();
        let rest: Vec<Sel> = self.msel.iter().filter(|s| matches!(s, Sel::Box(_) | Sel::Aud(_) | Sel::Ov(_))).copied().collect();
        f(self);
        let mut v = rest;
        for g in segs {
            if let Some(i) = self.p.segs.iter().position(|s| s.start == g.start && s.end == g.end && s.speed == g.speed && s.extra == g.extra) {
                v.push(Sel::Seg(i));
            }
        }
        for t in txts {
            if let Some(i) = self.p.texts.iter().position(|x| *x == t) {
                v.push(Sel::Txt(i));
            }
        }
        v.dedup();
        self.msel = v;
    }

    /// returns what changed: (timeline, seek needed). "cdown" = Ctrl / Shift held (toggle / box-select)
    pub fn tl_mouse(&mut self, kind: &str, x: f64, y: f64) -> (bool, bool) {
        self.mouse_y = y + self.tl_top;
        // piece / layer drags measure up / down from the window (the timeline grows when a layer row comes or goes)
        let ay = y + self.tl_top;
        match kind {
            "down" | "cdown" | "sdown" => {
                self.drag = None;
                if y < ROW_RULER.1 {
                    if kind == "sdown" {
                        // Shift+drag on the ruler highlights a range (Echo)
                        self.drag = Some(Drag::Range { x0: x, moved: false });
                        return (true, false);
                    }
                    self.drag = Some(Drag::Head);
                    self.t = ((x - LP) / self.pps).clamp(0.0, self.total());
                    return (true, true);
                }
                let hit = self.tl_hit(x, y);
                let key_of = |k: char, i: usize| match k { 's' => Sel::Seg(i), 'a' => Sel::Aud(i), 'b' => Sel::Box(i), 'o' => Sel::Ov(i), _ => Sel::Txt(i) };
                if kind == "cdown" || kind == "sdown" {
                    match hit {
                        Some((k, i, _)) => {
                            // Ctrl/Shift+click toggles the block in the selection
                            let key = key_of(k, i);
                            let mut v = std::mem::take(&mut self.msel);
                            if let Some(s) = self.sel.take() {
                                if !v.contains(&s) {
                                    v.push(s);
                                }
                            }
                            if let Some(p) = v.iter().position(|s| *s == key) { v.remove(p); } else { v.push(key); }
                            self.set_msel(v);
                        }
                        None => self.drag = Some(Drag::Marq { x0: x, y0: y, x1: x, y1: y, moved: false }),
                    }
                    return (true, false);
                }
                if self.msel.len() > 1 {
                    // a block's middle drags the whole selection; its trim handles trim just that block (Echo)
                    if let Some((k, i, 'm')) = hit {
                        let key = key_of(k, i);
                        if self.msel.contains(&key) {
                            let mut min_t = f64::MAX;
                            for s in &self.msel {
                                min_t = min_t.min(match *s {
                                    Sel::Seg(i) => self.p.segs.get(i).map(|g| g.at),
                                    Sel::Txt(i) => self.p.texts.get(i).map(|t| t.start),
                                    Sel::Box(i) => self.p.boxes.get(i).map(|b| b.start),
                                    Sel::Aud(i) => self.p.audio.get(i).map(|a| a.at),
                                    Sel::Ov(i) => self.p.over.get(i).map(|a| a.at),
                                }.unwrap_or(f64::MAX));
                            }
                            self.drag = Some(Drag::Group { x0: x, orig: self.p.clone(), items: self.msel.clone(), pushed: false, min_t: if min_t == f64::MAX { 0.0 } else { min_t }, key });
                            return (true, false);
                        }
                    }
                }
                self.msel.clear();
                match hit {
                    Some(('s', i, part)) => {
                        self.sel = Some(Sel::Seg(i));
                        // captions spoken in this piece follow it when it's moved (Echo); one moved elsewhere by hand doesn't
                        let o = &self.p.segs[i];
                        let (lo, hi) = (o.at - 0.01, o.at + o.len() + 0.01);
                        let follow = self.p.texts.iter().enumerate().filter(|(_, t)| t.cap && t.start >= lo && t.end <= hi)
                            .map(|(k, t)| (k, t.start, t.end, t.words.iter().map(|w| (w.start, w.end)).collect())).collect();
                        // separated audio of this piece follows it too
                        let fa = self.p.audio.iter().enumerate().filter(|(_, a)| a.at >= lo && a.at + a.len() <= hi).map(|(k, a)| (k, a.at)).collect();
                        self.drag = Some(Drag::Piece { i, mode: part, orig: o.clone(), x0: x, y0: ay, pushed: false, follow, fa });
                        (true, false)
                    }
                    Some(('o', i, part)) => {
                        self.sel = Some(Sel::Ov(i));
                        self.drag = Some(Drag::Layer { i, mode: part, orig: self.p.over[i].clone(), x0: x, y0: ay, pushed: false });
                        (true, false)
                    }
                    Some(('a', i, part)) => {
                        self.sel = Some(Sel::Aud(i));
                        self.drag = Some(Drag::AudDrag { i, mode: part, orig: self.p.audio[i].clone(), x0: x, pushed: false });
                        (true, false)
                    }
                    Some(('b', i, part)) => {
                        self.sel = Some(Sel::Box(i));
                        let bx = &self.p.boxes[i];
                        self.drag = Some(Drag::TextTime { i, mode: part, start0: bx.start, end0: bx.end, x0: x, pushed: false, bx: true });
                        (true, false)
                    }
                    Some((_, i, part)) => {
                        self.sel = Some(Sel::Txt(i));
                        let t = &self.p.texts[i];
                        self.drag = Some(Drag::TextTime { i, mode: part, start0: t.start, end0: t.end, x0: x, pushed: false, bx: false });
                        (true, false)
                    }
                    None => {
                        // empty timeline: a drag highlights a range, a click moves the playhead (and clears the range)
                        self.sel = None;
                        self.drag = Some(Drag::Range { x0: x, moved: false });
                        (true, false)
                    }
                }
            }
            "move" => {
                let Some(d) = self.drag.clone() else { return (false, false) };
                match d {
                    Drag::Marq { x0, y0, moved, .. } => {
                        let moved = moved || (x - x0).abs() > 4.0 || (y - y0).abs() > 4.0;
                        self.drag = Some(Drag::Marq { x0, y0, x1: x, y1: y, moved });
                        (moved, false)
                    }
                    Drag::Group { x0, orig, items, pushed, min_t, key } => {
                        if !pushed && (x - x0).abs() < 4.0 {
                            return (false, false);
                        }
                        if !pushed {
                            self.hist.push(orig.clone());
                            self.fut.clear();
                            self.drag = Some(Drag::Group { x0, orig: orig.clone(), items: items.clone(), pushed: true, min_t, key });
                        }
                        let dt = ((x - x0) / self.pps).max(-min_t);
                        for it in &items {
                            match *it {
                                Sel::Seg(i) => if let (Some(g), Some(o)) = (self.p.segs.get_mut(i), orig.segs.get(i)) { g.at = o.at + dt },
                                Sel::Box(i) => if let (Some(b), Some(o)) = (self.p.boxes.get_mut(i), orig.boxes.get(i)) { b.start = o.start + dt; b.end = o.end + dt },
                                Sel::Aud(i) => if let (Some(a), Some(o)) = (self.p.audio.get_mut(i), orig.audio.get(i)) { a.at = o.at + dt },
                                Sel::Ov(i) => if let (Some(a), Some(o)) = (self.p.over.get_mut(i), orig.over.get(i)) { a.at = o.at + dt },
                                Sel::Txt(i) => if let (Some(t), Some(o)) = (self.p.texts.get_mut(i), orig.texts.get(i)) {
                                    t.start = o.start + dt;
                                    t.end = o.end + dt;
                                    for (w, ow) in t.words.iter_mut().zip(o.words.iter()) {
                                        w.start = ow.start + dt;
                                        w.end = ow.end + dt;
                                    }
                                },
                            }
                        }
                        (true, false)
                    }
                    Drag::Head => {
                        self.t = ((x - LP) / self.pps).clamp(0.0, self.total());
                        (true, true)
                    }
                    Drag::Range { x0, moved } => {
                        if !moved && (x - x0).abs() < 4.0 {
                            return (false, false);
                        }
                        self.drag = Some(Drag::Range { x0, moved: true });
                        let (a, b) = ((x0 - LP) / self.pps, (x - LP) / self.pps);
                        let (a, b) = (self.snap_t(a.max(0.0), 0.0, None), self.snap_t(b.max(0.0), 0.0, None));
                        self.set_rng(a, b);
                        (true, false)
                    }
                    Drag::AudDrag { i, mode, orig, x0, pushed } => {
                        if !pushed && (x - x0).abs() < 4.0 {
                            return (false, false);
                        }
                        if !pushed {
                            self.push_hist();
                            self.drag = Some(Drag::AudDrag { i, mode, orig: orig.clone(), x0, pushed: true });
                        }
                        let dx = (x - x0) / self.pps;
                        let dur = self.dur;
                        let edges: Vec<f64> = self.p.segs.iter().flat_map(|g| [g.at, g.at + g.len()]).collect();
                        let tol = if self.snap { 10.0 / self.pps } else { 0.0 };
                        let Some(a) = self.p.audio.get_mut(i) else { return (false, false) };
                        match mode {
                            'l' => {
                                a.start = (orig.start + dx * orig.speed).clamp(0.0, a.end - 0.2);
                                a.at = (orig.at + (a.start - orig.start) / orig.speed).max(0.0);
                            }
                            'r' => a.end = (orig.end + dx * orig.speed).clamp(a.start + 0.2, dur.max(orig.end)),
                            _ => {
                                a.at = (orig.at + dx).max(0.0);
                                if let Some(e) = edges.iter().find(|e| (a.at - **e).abs() < tol) {
                                    a.at = *e;
                                }
                            }
                        }
                        (true, false)
                    }
                    Drag::Layer { i, mode, orig, x0, y0, pushed } => {
                        if !pushed && (x - x0).abs() < 4.0 && (ay - y0).abs() < 4.0 {
                            return (false, false);
                        }
                        if !pushed {
                            self.push_hist();
                            self.drag = Some(Drag::Layer { i, mode, orig: orig.clone(), x0, y0, pushed: true });
                        }
                        let dt = (x - x0) / self.pps;
                        // the source's length (the clip, or the other file it came from)
                        let src_len = orig.src().and_then(|f| self.srcs.get(f)).map(|x| x.1).filter(|d| *d > 0.0).unwrap_or(self.dur).max(orig.end);
                        // at most one new row above the highest other layer
                        let top = self.p.over.iter().enumerate().filter(|(k, _)| *k != i).map(|(_, o)| o.row).max().unwrap_or(0) + 1;
                        let Some(o) = self.p.over.get_mut(i) else { return (false, false) };
                        match mode {
                            'l' => {
                                o.start = (orig.start + dt * orig.speed).clamp(0.0, o.end - 0.2);
                                o.at = (orig.at + (o.start - orig.start) / orig.speed).max(0.0);
                            }
                            'r' => o.end = (orig.end + dt * orig.speed).min(src_len).max(o.start + 0.2),
                            _ => {
                                o.at = (orig.at + dt).max(0.0);
                                // up = a row higher; below row 1 = drops back onto the main track on release (shown as row 1)
                                o.row = (orig.row as f64 - ((ay - y0) / OV_H).round()).clamp(1.0, top as f64) as u32;
                            }
                        }
                        (true, false)
                    }
                    Drag::Piece { i, mode, orig, x0, y0, pushed, follow, fa } => {
                        let dx = (x - x0) / self.pps;
                        if !pushed && (x - x0).abs() < 4.0 && (ay - y0).abs() < 6.0 {
                            return (false, false);
                        }
                        if !pushed {
                            self.push_hist();
                            self.drag = Some(Drag::Piece { i, mode, orig: orig.clone(), x0, y0, pushed: true, follow: follow.clone(), fa: fa.clone() });
                        }
                        if i >= self.p.segs.len() {
                            return (false, false);
                        }
                        let min_len = 0.1;
                        match mode {
                            'l' => {
                                let mut start = (orig.start + dx * orig.speed).clamp(0.0, orig.end - min_len * orig.speed);
                                let mut at = orig.at + (start - orig.start) / orig.speed;
                                if at < 0.0 {
                                    start -= at * orig.speed;
                                    at = 0.0;
                                }
                                // can't run into the piece before it
                                let prev_end = if i > 0 { self.p.segs[i - 1].at + self.p.segs[i - 1].len() } else { 0.0 };
                                if at < prev_end {
                                    start += (prev_end - at) * orig.speed;
                                    at = prev_end;
                                }
                                let s = &mut self.p.segs[i];
                                s.start = start;
                                s.at = at;
                            }
                            'r' => {
                                // the piece's own source length (another file's, not the clip's)
                                let src_len = orig.src().and_then(|f| self.srcs.get(f)).map(|x| x.1).filter(|d| *d > 0.0).unwrap_or(self.dur);
                                let mut end = (orig.end + dx * orig.speed).clamp(orig.start + min_len * orig.speed, src_len.max(orig.end));
                                if let Some(nx) = self.p.segs.get(i + 1) {
                                    let lim = orig.start + (nx.at - orig.at) * orig.speed;
                                    end = end.min(lim.max(orig.start + min_len * orig.speed));
                                }
                                self.p.segs[i].end = end;
                            }
                            _ => {
                                let at = self.snap_t((orig.at + dx).max(0.0), orig.len(), Some(i));
                                self.p.segs[i].at = at;
                                self.follow_piece(&follow, at - orig.at);
                                for (k, a0) in &fa {
                                    if let Some(a) = self.p.audio.get_mut(*k) {
                                        a.at = a0 + at - orig.at;
                                    }
                                }
                            }
                        }
                        (true, false)
                    }
                    Drag::TextTime { i, mode, start0, end0, x0, pushed, bx } => {
                        if !pushed && (x - x0).abs() < 3.0 {
                            return (false, false);
                        }
                        if !pushed {
                            self.push_hist();
                            self.drag = Some(Drag::TextTime { i, mode, start0, end0, x0, pushed: true, bx });
                        }
                        let dx = (x - x0) / self.pps;
                        let tt = self.total();
                        let l = end0 - start0;
                        // Echo's snapT: texts / boxes / captions stick to the playhead and piece edges too
                        let (sl, sr, sm) = (self.snap_t(start0 + dx, 0.0, None), self.snap_t(end0 + dx, 0.0, None), self.snap_t((start0 + dx).max(0.0), l, None));
                        if bx {
                            let Some(b) = self.p.boxes.get_mut(i) else { return (false, false) };
                            match mode {
                                'l' => b.start = sl.clamp(0.0, end0 - 0.2),
                                'r' => b.end = sr.clamp(start0 + 0.2, tt.max(end0)),
                                _ => {
                                    b.start = sm.clamp(0.0, (tt - l).max(0.0));
                                    b.end = b.start + l;
                                }
                            }
                            return (true, false);
                        }
                        let Some(t) = self.p.texts.get_mut(i) else { return (false, false) };
                        match mode {
                            'l' => t.start = sl.clamp(0.0, end0 - 0.2),
                            'r' => t.end = sr.clamp(start0 + 0.2, tt.max(end0)),
                            _ => {
                                let ns = sm.clamp(0.0, (tt - l).max(0.0));
                                let d = ns - t.start;
                                t.start = ns;
                                t.end = ns + l;
                                for w in t.words.iter_mut() {
                                    w.start += d;
                                    w.end += d;
                                }
                            }
                        }
                        (true, false)
                    }
                    _ => (false, false),
                }
            }
            _ => {
                // up
                let d = self.drag.take();
                if let Some(Drag::Marq { x0, y0, x1, y1, moved }) = d {
                    if !moved {
                        // a Ctrl+click on empty space: playhead there, nothing selected
                        self.set_msel(vec![]);
                        self.t = ((x0 - LP) / self.pps).clamp(0.0, self.total());
                        return (true, true);
                    }
                    let (ax, bx, ay, by) = (x0.min(x1), x0.max(x1), y0.min(y1), y0.max(y1));
                    let hit: Vec<Sel> = self.block_rects().into_iter().filter(|r| r.2 > ax && r.1 < bx && r.4 > ay && r.3 < by).map(|r| r.0).collect();
                    self.set_msel(hit);
                    return (true, false);
                }
                if let Some(Drag::Group { pushed: true, .. }) = d {
                    self.keep_msel(|e| {
                        e.normalize_segs();
                        e.sort_texts();
                    });
                    return (true, true);
                }
                if let Some(Drag::Piece { i, mode: 'm', pushed: true, y0, orig, .. }) = &d {
                    // dragged UP: becomes a layer above the main video (Echo)
                    if ay - y0 < -40.0 && *i < self.p.segs.len() {
                        if self.p.segs.len() < 2 || orig.freeze() {
                            // back to how it was before the drag (its captions / audio followed it)
                            match self.hist.pop() {
                                Some(p) => self.p = p,
                                None => self.p.segs[*i] = orig.clone(),
                            }
                            if orig.freeze() {
                                self.layer_msg = Some("A freeze frame can't go on a layer");
                                return (true, true);
                            }
                            self.layer_msg = Some("Split the clip first - the main track needs at least one piece");
                            return (true, true);
                        }
                        let s = self.p.segs.remove(*i);
                        let mut extra = serde_json::Map::new();
                        extra.insert("src".into(), json!(s.src().unwrap_or("")));
                        self.p.over.push(Ovl { start: s.start, end: s.end, speed: s.speed, at: s.at, x: 0.5, y: 0.5, scale: 1.0,
                            sound: s.extra.get("amute").and_then(|x| x.as_bool()) != Some(true), row: 1, extra });
                        self.sel = Some(Sel::Ov(self.p.over.len() - 1));
                        self.layer_msg = Some("Moved to a layer above - resize it in the panel or drag it on the video");
                        return (true, true);
                    }
                }
                if let Some(Drag::Layer { i, mode: 'm', pushed: true, y0, orig, .. }) = &d {
                    // dragged DOWN below row 1: back onto the main track
                    if orig.row as f64 - ((ay - y0) / OV_H).round() < 1.0 && *i < self.p.over.len() {
                        let o = self.p.over.remove(*i);
                        let mut g = Seg::new(o.start, o.end, o.at);
                        g.speed = o.speed;
                        if let Some(sr) = o.src() {
                            g.extra.insert("src".into(), json!(sr));
                        }
                        if !o.sound {
                            g.extra.insert("amute".into(), json!(true));
                        }
                        self.p.segs.push(g);
                        self.normalize_segs();
                        self.sel = None;
                        self.t = self.t.min(self.total());
                        return (true, true);
                    }
                    return (true, true);
                }
                if let Some(Drag::Layer { pushed: false, .. }) = &d {
                    // a click on a layer block: playhead there (Echo)
                    self.t = ((x - LP) / self.pps).clamp(0.0, self.total());
                    return (true, true);
                }
                if let Some(Drag::Piece { i, mode: 'm', orig, pushed: true, follow, fa, .. }) = &d {
                    // dropped: overlapping pieces are pushed right - its captions get the same final shift
                    let moved = self.p.segs.get(*i).cloned();
                    self.normalize_segs();
                    if let Some(mv) = moved {
                        if let Some(fin) = self.p.segs.iter().find(|s| s.start == mv.start && s.end == mv.end && s.speed == mv.speed) {
                            let d = fin.at - orig.at;
                            self.follow_piece(follow, d);
                            for (k, a0) in fa {
                                if let Some(a) = self.p.audio.get_mut(*k) {
                                    a.at = a0 + d;
                                }
                            }
                        }
                    }
                    self.sort_texts();
                    return (true, true);
                }
                if let Some(Drag::Head) = d {
                    return (true, true); // one exact seek where the playhead was let go
                }
                if let Some(Drag::Range { x0, moved: false }) = d {
                    self.rng = None;
                    self.t = ((x0 - LP) / self.pps).clamp(0.0, self.total());
                    return (true, true);
                }
                if let Some(Drag::Piece { pushed: true, .. }) = d {
                    self.normalize_segs();
                    return (true, true);
                }
                if let Some(Drag::Piece { x0, pushed: false, .. }) = d {
                    // a click on a piece: selected AND the playhead goes there (Echo's seekOut(clickT))
                    self.t = ((x0 - LP) / self.pps).clamp(0.0, self.total());
                    return (true, true);
                }
                if let Some(Drag::TextTime { pushed: true, .. }) = d {
                    self.sort_texts();
                    return (true, false);
                }
                (false, false)
            }
        }
    }
}

pub fn project_path(dir: &Path, clip: &Path) -> PathBuf {
    dir.join(format!("{}.project.json", clip.file_stem().unwrap_or_default().to_string_lossy()))
}
