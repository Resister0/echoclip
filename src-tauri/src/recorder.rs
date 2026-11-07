//! Replay-buffer recorder.
//! Video (mode "gpu"): own zero-copy pipeline in gpucap.rs -> H.264 over a named pipe.
//! Video (mode "cpu"): ffmpeg ddagrab -> hwdownload -> AMF (fallback).
//! Audio: WASAPI loopback (+ optional mic) via cpal -> mixed, paced PCM into ffmpeg stdin -> AAC.
//! Output: rolling 2s mpegts segments; save = concat last N seconds with -c copy.

use crate::settings::Settings;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime},
};

const SEG: u32 = 2; // seconds per segment
/// sub-folders of the clips folder that are not clip folders
const RESERVED_DIRS: [&str; 4] = ["Screenshots", "GIFs", "Audio", "Discord"];

/// "FiveM / Staff evidence\\" -> "FiveM/Staff evidence": safe folder path, "/" between levels.
fn clean_folder(s: &str) -> String {
    s.split(|c| c == '/' || c == '\\')
        .map(|p| p.trim().chars().filter(|c| !r#":*?"<>|"#.contains(*c)).collect::<String>().trim().trim_end_matches('.').trim().to_string())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>().join("/")
}

/// Hidden or one of Echo's own folders at the top level.
fn folder_reserved(rel: &str) -> bool {
    rel.split('/').any(|p| p.starts_with('.'))
        || rel.split('/').next().map_or(false, |f| RESERVED_DIRS.iter().any(|r| r.eq_ignore_ascii_case(f)))
}

/// For the in-game "Clip saved" popup: the game's name and its icon as a data: URL (the popup window can't
/// load files), from a clip file stem like "FiveM 2026-11-04 15-50-51 (Echo)".
pub fn toast_game(stem: &str) -> (Option<String>, String) {
    let Some(g) = game_of_name(stem) else { return (None, String::new()) };
    let icon = fs::read(app_dir().join("icons").join(format!("{}.png", safe_name(&g)))).ok()
        .filter(|b| !b.is_empty() && b.len() < 200_000)
        .map(|b| format!("data:image/png;base64,{}", base64(&b))).unwrap_or_default();
    (Some(g), icon)
}

/// "Valorant 2026-10-31 23-39-26 (edit)" -> Some("Valorant"); "Clip ..." / other names -> None.
fn game_of_name(name: &str) -> Option<String> {
    let b = name.as_bytes();
    for i in 0..b.len() {
        if b[i] == b' ' && b.len() >= i + 20 && name.is_char_boundary(i + 1) && name.is_char_boundary(i + 20) {
            let d = &name[i + 1..i + 20];
            let ok = d.char_indices().all(|(k, c)| match k {
                4 | 7 | 13 | 16 => c == '-',
                10 => c == ' ',
                _ => c.is_ascii_digit(),
            });
            if ok {
                let g = name[..i].trim();
                return (!g.is_empty() && g != "Clip" && g != "Montage" && g != "Recording" && g != "Session").then(|| g.to_string());
            }
        }
    }
    None
}

pub struct Recorder {
    child: Mutex<Option<Child>>,
    audio_stop: Mutex<Arc<AtomicBool>>,
    cfg: Mutex<Settings>,
    /// live volumes (f32 bits): desktop, mic
    volumes: Arc<[AtomicU32; 2]>,
    pub buf_dir: PathBuf,
    pub mode: Mutex<String>,
    /// one save at a time (they share the buffer and can be 500 MB of reading)
    save_lock: Mutex<()>,
    /// start() and stop() never overlap (a settings restart during start-up used to run two recorders)
    life: Mutex<()>,
    /// recorder stopped because the PC is idle (resumes on input)
    pub idle_paused: AtomicBool,
    /// game window to record when Settings > Video > "Game window only" is on (0 = whole monitor)
    pub window: std::sync::atomic::AtomicIsize,
    /// why the buffer is paused: "" running, "idle", "game" (game set to not record), "nogame"
    pub pause_reason: Mutex<String>,
    /// long (full-session) recording in progress
    session: Mutex<Option<Session>>,
    /// cloud: local "rclone serve http" for streaming cloud-only clips (port)
    cloud_serve: Mutex<Option<(Child, u16)>>,
    /// cloud: what the background sync is doing right now ("" = idle)
    pub cloud_busy: Mutex<String>,
    cloud_lock: Mutex<()>,
    /// last game the watch loop saw in front (name, when) - used when the hotkey is pressed outside the game
    pub last_game: Mutex<Option<(String, Instant)>>,
}

struct Session {
    dir: PathBuf,
    started: Instant,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[derive(Serialize)]
pub struct GameInfo {
    pub name: String,
    /// path of the game's exe icon (PNG in the app folder; "" = none)
    pub icon: String,
    /// its change time (cache-buster for the window)
    pub icon_v: u64,
}

#[derive(Serialize)]
pub struct ClipInfo {
    pub path: String,
    pub name: String,
    /// sub-folder inside the clips folder ("" = top level)
    pub folder: String,
    pub size: u64,
    /// unix seconds
    pub modified: u64,
}

/// A share link Echo already made (.thumbs/links.json, by clip): "Copy share link" hands the same link out again
/// instead of making a new one (and re-making the share copy) every time.
#[derive(Serialize, Deserialize, Clone, Default)]
struct LinkEntry {
    url: String,
    /// Echo Cloud object the link points to ("Echo/…"); empty = another cloud's link
    #[serde(default)] key: String,
    #[serde(default)] days: u32,
    /// unix s, 0 = never
    #[serde(default)] expires: u64,
    /// watermark style at the time + clip size/time: when either changes the link is made again
    #[serde(default)] wm: String,
    #[serde(default)] sig: String,
    /// what the share page shows (sent again when only the lifetime changes)
    #[serde(default)] meta: serde_json::Value,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct ClipMeta {
    pub tags: Vec<String>,
    pub note: String,
    /// words spoken in the clip (from auto captions), only filled for the library search
    #[serde(skip_serializing_if = "String::is_empty")]
    pub said: String,
}

#[derive(Serialize)]
pub struct Thumb {
    pub thumb: String,
    pub duration: f64,
}

/// One kept piece of the source clip (source seconds) played at `speed`.
#[derive(Deserialize, Clone)]
pub struct Seg {
    pub start: f64,
    pub end: f64,
    pub speed: f64,
    /// position on the output timeline (gaps before it render as black + silence)
    #[serde(default)]
    pub at: f64,
    /// audio moved to its own track (piece itself plays silent)
    #[serde(default)]
    pub amute: bool,
    /// fade in / out seconds (video from/to black + audio)
    #[serde(default)]
    pub fi: f64,
    #[serde(default)]
    pub fo: f64,
    /// colour: brightness (-1..1, 0 = normal), contrast + saturation (1 = normal), look preset name
    #[serde(default)]
    pub bri: f64,
    #[serde(default = "one")]
    pub con: f64,
    #[serde(default = "one")]
    pub sat: f64,
    #[serde(default)]
    pub look: String,
    /// play backwards
    #[serde(default)]
    pub rev: bool,
    /// freeze frame: shows the frame at `start` for (end - start) seconds, silent
    #[serde(default)]
    pub freeze: bool,
    /// transition from the previous piece ("" = cut; xfade names: fade, fadeblack, wipeleft, slideleft, circleopen, zoomin, pixelize)
    #[serde(default)]
    pub tr: String,
    #[serde(default)]
    pub td: f64,
    /// another video dropped into the editor ("" = the clip being edited)
    #[serde(default)]
    pub src: String,
}

/// ffmpeg colour filters for a piece ("" = untouched)
fn grade(s: &Seg) -> String {
    let mut f = String::new();
    if s.bri.abs() > 0.005 || (s.con - 1.0).abs() > 0.005 || (s.sat - 1.0).abs() > 0.005 {
        f += &format!(",eq=brightness={:.3}:contrast={:.3}:saturation={:.3}", s.bri.clamp(-1.0, 1.0) * 0.5, s.con.clamp(0.2, 3.0), s.sat.clamp(0.0, 3.0));
    }
    f += match s.look.as_str() {
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

/// Text animations as ASS override tags for a line lasting `len` seconds. `first`/`last`: this slice starts / ends the text.
fn ass_anim(anim: &str, x: f64, y: f64, h: u32, len: f64, first: bool, last: bool) -> String {
    let ms = |s: f64| (s * 1000.0).round() as i64;
    let mut t = String::new();
    let fin = ms(0.25f64.min(len / 3.0));
    match anim {
        "fade" => t += &format!("\\fad({},{})", if first { fin } else { 0 }, if last { fin } else { 0 }),
        "pop" if first => t += &format!("\\fscx20\\fscy20\\t(0,{},\\fscx112\\fscy112)\\t({},{},\\fscx100\\fscy100)", fin * 2 / 3, fin * 2 / 3, fin),
        "slide" if first => {
            let dy = (h as f64 * 0.06).round();
            t += &format!("\\move({:.0},{:.0},{:.0},{:.0},0,{fin})\\fad({fin},0)", x, y + dy, x, y);
        }
        "bounce" if first => t += &format!("\\fscy40\\fscx130\\t(0,{},\\fscy120\\fscx90)\\t({},{},\\fscy100\\fscx100)", fin / 2, fin / 2, fin),
        _ => {}
    }
    t
}

/// Detached audio clip (source seconds, played at `speed`, placed at `at` on the output timeline).
#[derive(Deserialize, Clone)]
pub struct Aud {
    pub start: f64,
    pub end: f64,
    pub speed: f64,
    pub at: f64,
    pub vol: f64,
    #[serde(default)]
    pub mute: bool,
    /// source audio track: 0 = mix, 1 = game/desktop, 2 = mic
    #[serde(default)]
    pub track: u32,
    #[serde(default)]
    pub fi: f64,
    #[serde(default)]
    pub fo: f64,
    /// another file dropped into the editor ("" = the clip being edited)
    #[serde(default)]
    pub src: String,
}

/// ",fade=..." / ",afade=..." for a piece of output length `len` (empty when no fades)
fn fades(f: &str, fi: f64, fo: f64, len: f64) -> String {
    let mut out = String::new();
    let fi = fi.clamp(0.0, len);
    let fo = fo.clamp(0.0, len);
    if fi >= 0.05 { out += &format!(",{f}=t=in:st=0:d={fi:.3}"); }
    if fo >= 0.05 { out += &format!(",{f}=t=out:st={:.3}:d={fo:.3}", (len - fo).max(0.0)); }
    out
}

/// Text overlay; times are on the OUTPUT timeline, x/y = centre (0..1), size = fraction of video height.
#[derive(Deserialize, Clone)]
pub struct Txt {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub x: f64,
    pub y: f64,
    pub size: f64,
    pub color: String,
    /// spoken words with times (output timeline) - for highlighting the current word
    #[serde(default)]
    pub words: Vec<TWord>,
    /// highlight colour for the spoken word ("" = off)
    #[serde(default)]
    pub hl: String,
    /// "" | "fade" | "pop" | "slide" | "bounce" | "type"
    #[serde(default)]
    pub anim: String,
}

#[derive(Deserialize, Clone)]
pub struct TWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// this word starts a new line
    #[serde(default)]
    pub nl: bool,
}

fn ass_time(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    format!("{}:{:02}:{:02}.{:02}", cs / 360000, cs / 6000 % 60, cs / 100 % 60, cs % 100)
}

/// "RRGGBB" -> ASS "&HBBGGRR&"
fn ass_color(hex: &str) -> String {
    let h: String = hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let h = if h.len() == 6 { h } else { "FFFFFF".into() };
    format!("&H{}{}{}&", &h[4..6], &h[2..4], &h[0..2])
}

fn ass_escape(s: &str) -> String {
    s.replace('\\', "/").replace('{', "(").replace('}', ")").replace("\r", "").replace('\n', "\\N")
}

/// Captions with a highlighted spoken word, plus animated texts, as an ASS subtitle script (rendered by libass).
fn captions_ass(texts: &[&Txt], w: u32, h: u32) -> String {
    let mut out = format!("[Script Info]\nScriptType: v4.00+\nPlayResX: {w}\nPlayResY: {h}\nScaledBorderAndShadow: yes\nWrapStyle: 2\n\n\
        [V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
        Style: C,Segoe UI,48,&H00FFFFFF,&H00FFFFFF,&H40000000,&H00000000,-1,0,0,0,100,100,0,0,1,3,0,5,0,0,0,1\n\n\
        [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n");
    for t in texts {
        let px = ((t.size * h as f64).round() as u32).max(8);
        let (x, y) = ((t.x * w as f64).round(), (t.y * h as f64).round());
        let head = |first: bool, last: bool, len: f64| {
            let anim = ass_anim(&t.anim, x, y, h, len, first, last);
            let pos = if anim.contains("\\move") { String::new() } else { format!("\\pos({x},{y})") };
            format!("{{\\an5{pos}\\fs{px}\\bord{}\\c{}{anim}}}", (px / 14).max(2), ass_color(&t.color))
        };
        let base = ass_color(&t.color);
        if t.words.is_empty() || t.hl.is_empty() {
            // plain (animated) text
            let txt = ass_escape(t.text.trim_end());
            let body = if t.anim == "type" {
                // typewriter: each letter appears in turn
                let chars: Vec<char> = txt.chars().collect();
                let n = chars.iter().filter(|c| !c.is_whitespace()).count().max(1);
                let step = (0.06f64).min((t.end - t.start) * 0.6 / n as f64);
                let mut b = String::new();
                let mut k = 0;
                let mut i = 0;
                while i < chars.len() {
                    // keep "\N" line breaks intact
                    if chars[i] == '\\' && chars.get(i + 1) == Some(&'N') {
                        b += "\\N";
                        i += 2;
                        continue;
                    }
                    let c = chars[i];
                    if c.is_whitespace() {
                        b.push(c);
                    } else {
                        let ms = (k as f64 * step * 1000.0).round() as i64;
                        b += &format!("{{\\alpha&HFF&\\t({ms},{},\\alpha&H00&)}}{c}", ms + 1);
                        k += 1;
                    }
                    i += 1;
                }
                b
            } else { txt };
            out += &format!("Dialogue: 0,{},{},C,,0,0,0,,{}{body}\n", ass_time(t.start), ass_time(t.end), head(true, true, t.end - t.start));
            continue;
        }
        let hl = ass_color(&t.hl);
        // split the caption's time at every word boundary; in each slice colour the word being spoken
        let mut cuts: Vec<f64> = vec![t.start, t.end];
        for wd in &t.words {
            cuts.push(wd.start.clamp(t.start, t.end));
            cuts.push(wd.end.clamp(t.start, t.end));
        }
        cuts.sort_by(|a, b| a.partial_cmp(b).unwrap());
        cuts.dedup_by(|a, b| (*a - *b).abs() < 0.005);
        let nw = cuts.len().saturating_sub(1);
        for (k, win) in cuts.windows(2).enumerate() {
            let (a, b) = (win[0], win[1]);
            let mid = (a + b) / 2.0;
            let mut body = String::new();
            for (k, wd) in t.words.iter().enumerate() {
                if k > 0 { body += if wd.nl { "\\N" } else { " " }; }
                let txt = ass_escape(&wd.text);
                body += &if mid >= wd.start && mid < wd.end { format!("{{\\c{hl}}}{txt}{{\\c{base}}}") } else { txt };
            }
            // animation only on the first (in) / last (out) slice; "type" doesn't apply to word captions
            let anim_ok = t.anim != "type";
            let hd = if anim_ok { head(k == 0, k + 1 == nw, b - a) } else { head(false, false, b - a) };
            out += &format!("Dialogue: 0,{},{},C,,0,0,0,,{hd}{body}\n", ass_time(a), ass_time(b));
        }
    }
    out
}

fn one() -> f64 { 1.0 }

/// A rectangle drawn over the video for a while: "blur" (hide names / chat) or "color" (solid fill).
/// Position/size are fractions of the full source frame (x, y = centre); times are output time.
#[derive(Deserialize, Clone)]
pub struct BoxFx {
    pub start: f64,
    pub end: f64,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub kind: String,
    #[serde(default)]
    pub color: String,
    #[serde(default = "one")]
    pub opacity: f64,
    /// blur strength (gaussian sigma in source pixels)
    #[serde(default)]
    pub blur: f64,
    /// kind "image": picture / sticker file
    #[serde(default)]
    pub src: String,
}

/// A piece lifted onto a video layer above the main track (overlay / picture-in-picture).
#[derive(Deserialize, Clone)]
pub struct Ovl {
    pub start: f64,
    pub end: f64,
    pub speed: f64,
    pub at: f64,
    /// centre position (0..1) and width as a fraction of the video
    pub x: f64,
    pub y: f64,
    pub scale: f64,
    #[serde(default)]
    pub sound: bool,
    #[serde(default)]
    pub row: u32,
    #[serde(default)]
    pub src: String,
}

#[derive(Deserialize, Clone)]
pub struct Edit {
    pub segments: Vec<Seg>,
    pub texts: Vec<Txt>,
    #[serde(default)]
    pub audio: Vec<Aud>,
    #[serde(default)]
    pub overlays: Vec<Ovl>,
    #[serde(default)]
    pub boxes: Vec<BoxFx>,
    pub volume: f64,
    pub mute: bool,
    /// vertical 9:16 export: centre of the crop window (0..1 of the width); None = normal
    #[serde(default)]
    pub vertical: Option<f64>,
    /// even out loudness (-14 LUFS, like YouTube / TikTok)
    #[serde(default)]
    pub normalize: bool,
    /// scale the result to this height (export presets); None = source size
    #[serde(default)]
    pub out_h: Option<u32>,
    /// "Discord" export: aim the bitrate so the file fits in this many MB - ONE encode instead of exporting at
    /// full size and shrinking that again (chat 21)
    #[serde(default)]
    pub max_mb: Option<f64>,
}

/// Path inside a filtergraph option: forward slashes, escaped drive colon.
fn ff_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/").replace(':', "\\:")
}

/// atempo only takes 0.5..2 per instance, so chain it.
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

/// Feedback sounds, synthesised as a small WAV (no asset files needed).
#[derive(Clone, Copy)]
pub enum Sound {
    Saved,
    Failed,
    Mark,
    Shot,
    Stopped,
}

fn sound_wav(kind: Sound, volume: f32) -> Vec<u8> {
    const RATE: u32 = 44100;
    // (frequency Hz, seconds, square-ish?)
    let notes: &[(f32, f32, bool)] = match kind {
        Sound::Saved => &[(880.0, 0.07, false), (1318.5, 0.13, false)],
        Sound::Failed => &[(311.0, 0.12, true), (220.0, 0.22, true)],
        Sound::Mark => &[(1318.5, 0.06, false)],
        Sound::Shot => &[(1046.5, 0.05, false), (1568.0, 0.09, false)],
        Sound::Stopped => &[(659.3, 0.12, false), (523.3, 0.12, false), (392.0, 0.28, false)],
    };
    let mut pcm: Vec<i16> = Vec::new();
    for &(f, secs, buzzy) in notes {
        let n = (secs * RATE as f32) as usize;
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            let env = (t / 0.005).min(1.0) * (1.0 - i as f32 / n as f32).powf(1.6);
            let w = (t * f * std::f32::consts::TAU).sin();
            let w = if buzzy { (w * 3.0).tanh() * 0.6 } else { w };
            pcm.push((w * env * volume.clamp(0.0, 1.0) * 0.5 * i16::MAX as f32) as i16);
        }
    }
    let data_len = (pcm.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // PCM
    v.extend_from_slice(&1u16.to_le_bytes()); // mono
    v.extend_from_slice(&RATE.to_le_bytes());
    v.extend_from_slice(&(RATE * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

/// Play a feedback sound on a background thread (if enabled in settings).
pub fn play_sound(kind: Sound, cfg: &Settings, force: bool) {
    let (on, key) = match kind {
        Sound::Saved => (cfg.sound_on_save, "saved"),
        Sound::Failed => (cfg.sound_on_fail, "failed"),
        Sound::Mark => (cfg.sound_on_mark, "mark"),
        Sound::Shot => (cfg.sound_on_shot, "shot"),
        Sound::Stopped => (cfg.sound_on_stop, "stopped"),
    };
    if !(force || on) || cfg.sound_volume <= 0.0 {
        return;
    }
    let custom = cfg.sound_files.get(key).filter(|f| !f.is_empty() && Path::new(f).is_file()).cloned();
    let wav = sound_wav(kind, cfg.sound_volume);
    std::thread::spawn(move || {
        #[cfg(windows)]
        match custom {
            Some(f) => crate::winutil::play_file(&f),
            None => crate::winutil::play_wav(&wav),
        }
        #[cfg(not(windows))]
        {
            let f = match custom {
                Some(f) => PathBuf::from(f),
                None => {
                    let f = std::env::temp_dir().join(format!("echo_sound_{}.wav", std::process::id()));
                    if fs::write(&f, &wav).is_err() { return; }
                    f
                }
            };
            let ok = sys_cmd("pw-play").arg(&f).stderr(Stdio::null()).status().map_or(false, |s| s.success());
            if !ok { let _ = sys_cmd("paplay").arg(&f).stderr(Stdio::null()).status(); }
        }
    });
}

/// Name of the game in front, for clip names. None = not a game (desktop, browser, Clipper...).
pub fn detect_game() -> Option<String> {
    #[cfg(windows)]
    {
        let (stem, title) = crate::winutil::foreground_app()?;
        return game_name(&stem, &title, crate::winutil::foreground_fullscreen());
    }
    #[cfg(target_os = "linux")]
    return linux_game().map(|g| g.0);
    #[allow(unreachable_code)]
    None
}

/// The game in front: (name, pid, exe path).
pub fn game_in_front() -> Option<(String, u32, String)> {
    #[cfg(windows)]
    {
        let (pid, stem, title) = crate::winutil::foreground_pid()?;
        if pid == std::process::id() {
            return None;
        }
        let name = game_name(&stem, &title, crate::winutil::foreground_fullscreen())?;
        return Some((name, pid, crate::winutil::exe_path(pid).unwrap_or_default()));
    }
    #[cfg(target_os = "linux")]
    return linux_game();
    #[allow(unreachable_code)]
    None
}

/// Name for a VR game found by winutil::vr_game() (None = the user said it's not a game, or a browser etc.).
#[cfg(windows)]
pub fn vr_game_name(stem: &str, title: &str) -> Option<String> {
    game_name(stem, title, true)
}

/// Linux: Wayland doesn't tell apps which window is in front, so look for a running game instead:
/// Steam games (steamapps/common/<Game>/), Wine / Proton / Lutris / Heroic games (*.exe), Minecraft.
/// Several running: the one using the most memory. Returns (name, pid, exe path).
#[cfg(target_os = "linux")]
pub fn linux_game() -> Option<(String, u32, String)> {
    // Flatpak: the sandbox only sees its own processes in /proc - find the game by its sound instead
    if crate::linuxutil::in_flatpak() { return linux_game_from_streams(); }
    const WINE_HELPERS: &[&str] = &["wineserver", "services", "explorer", "winedevice", "plugplay", "svchost", "rpcss", "steam",
        "conhost", "start", "tabtip", "rundll32", "winemenubuilder", "steamwebhelper", "crashhandler", "crashreporter",
        "unitycrashhandler64", "easyanticheat", "easyanticheat_eos", "battleye", "beservice", "eosoverlayrenderer-win64-shipping",
        "iexplore", "msiexec", "wineboot", "winecfg", "proton", "pressure-vessel", "reaper", "steam-runtime", "gameoverlayui",
        "legendary", "galaxyclient", "epicgameslauncher", "socialclubhelper", "launcher", "rockstarservice", "subprocess"];
    let me = std::process::id();
    let mut best: Option<(u64, String, u32, String)> = None;
    for e in fs::read_dir("/proc").ok()?.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
        if pid == me { continue; }
        let Ok(cmd) = fs::read(e.path().join("cmdline")) else { continue };
        if cmd.is_empty() { continue; }
        let args: Vec<String> = cmd.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        let exe = fs::read_link(e.path().join("exe")).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let a0 = args.first().cloned().unwrap_or_default().replace('\\', "/");
        let mut found: Option<(String, String)> = None; // (stem, title hint)
        if a0.to_lowercase().ends_with(".exe") {
            // Wine / Proton: argv[0] is the Windows exe
            let stem = Path::new(&a0).file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
            if !WINE_HELPERS.contains(&stem.to_lowercase().as_str()) {
                let title = a0.split("/common/").nth(1).and_then(|r| r.split('/').next()).unwrap_or("").to_string();
                found = Some((stem, title));
            }
        } else if let Some(r) = exe.split("/steamapps/common/").nth(1) {
            // native Steam game
            let title = r.split('/').next().unwrap_or("").to_string();
            let stem = Path::new(&exe).file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
            if !title.to_lowercase().starts_with("proton") && !title.to_lowercase().starts_with("steamlinuxruntime") && !title.to_lowercase().starts_with("steamworks") {
                found = Some((stem, title));
            }
        } else if exe.ends_with("/java") && args.iter().any(|a| a.contains("minecraft")) {
            found = Some(("minecraft".into(), "Minecraft".into()));
        }
        let Some((stem, title)) = found else { continue };
        let Some(name) = game_name(&stem, &title, true) else { continue };
        let rss = fs::read_to_string(e.path().join("statm")).ok().and_then(|t| t.split_whitespace().nth(1).and_then(|x| x.parse::<u64>().ok())).unwrap_or(0);
        if rss < 25_000 { continue; } // < ~100 MB: launchers / helpers, not the game
        if best.as_ref().map_or(true, |b| rss > b.0) {
            best = Some((rss, name, pid, if exe.is_empty() { a0.clone() } else { exe.clone() }));
        }
    }
    best.map(|(_, n, p, x)| (n, p, x))
}

/// Flatpak version of linux_game(): the playing sound streams (pactl) say which program plays them.
/// Wine / Proton games (Steam, Lutris, Heroic) show up as "<game>.exe" or as wine with the exe as the app
/// name; a native game counts once it was seen as a game before (games.json) or is Minecraft.
/// A game that hasn't made a sound yet isn't found (logged once per game when it is).
#[cfg(target_os = "linux")]
fn linux_game_from_streams() -> Option<(String, u32, String)> {
    const WINE: &[&str] = &["wine", "wine64", "wine-preloader", "wine64-preloader", "wineserver", "proton"];
    const NOT_GAMES: &[&str] = &["steam", "steamwebhelper", "pipewire", "wireplumber", "kwin_wayland", "plasmashell", "gnome-shell",
        "speech-dispatcher", "sd_espeak-ng", "xdg-desktop-portal", "telegram-desktop", "signal-desktop", "slack", "zoom", "teams",
        "mpv", "totem", "celluloid", "haruna", "smplayer", "kdeconnectd", "obs", "gpu-screen-recorder", "echo", "echo-clips"];
    let known: std::collections::BTreeMap<String, String> = fs::read_to_string(app_dir().join("games.json")).ok()
        .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let known_stems: Vec<String> = known.values().filter_map(|x| Path::new(&x.replace('\\', "/")).file_stem().map(|s| s.to_string_lossy().to_lowercase())).collect();
    for st in linux_sink_inputs().unwrap_or_default() {
        if linux_app_group(&st.binary, &st.name).is_some() { continue; }
        let b = st.binary.to_lowercase();
        if NOT_GAMES.contains(&b.as_str()) { continue; }
        let (stem, exe) = if b.ends_with(".exe") {
            (Path::new(&st.binary).file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default(), st.binary.clone())
        } else if WINE.contains(&b.as_str()) {
            // wine plays the sound; the app name is usually the game's exe ("GTA5.exe") or its title
            let n = st.name.trim();
            if n.is_empty() || n.eq_ignore_ascii_case("wine") || n.to_lowercase().contains("wine ") { continue; }
            (n.trim_end_matches(".exe").trim_end_matches(".EXE").to_string(), n.to_string())
        } else if st.name.to_lowercase().contains("minecraft") || b == "java" && st.name.to_lowercase().contains("minecraft") {
            ("minecraft".to_string(), "Minecraft".to_string())
        } else if !b.is_empty() && known_stems.contains(&b) {
            (st.binary.clone(), st.binary.clone())
        } else { continue };
        let title = if stem == "minecraft" { "Minecraft".to_string() } else { String::new() };
        let Some(name) = game_name(&stem, &title, true) else { continue };
        static SAID: Mutex<Vec<String>> = Mutex::new(Vec::new());
        if let Ok(mut said) = SAID.lock() {
            if !said.contains(&name) {
                log_line(&format!("game: {name} (Flatpak: found by its sound - stream {}, {} / {})", st.index, st.binary, st.name));
                said.push(name.clone());
            }
        }
        return Some((name, st.pid.unwrap_or(0), exe));
    }
    None
}

/// Longest stretch (seconds) between two keyframes in the clip's video, from packet flags (no decoding: fast).
/// 0 when it can't be read.
fn max_key_gap(clip: &str) -> f64 {
    let Ok(o) = hidden("ffprobe").args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts_time,flags", "-of", "csv=p=0"])
        .arg(clip).stdin(Stdio::null()).stderr(Stdio::null()).output() else { return 0.0 };
    let (mut last, mut gap, mut end) = (None::<f64>, 0f64, 0f64);
    for l in String::from_utf8_lossy(&o.stdout).lines() {
        let mut it = l.split(',');
        let Some(t) = it.next().and_then(|x| x.trim().parse::<f64>().ok()) else { continue };
        end = end.max(t);
        if it.next().map_or(false, |f| f.starts_with('K')) {
            if let Some(p) = last { gap = gap.max(t - p); }
            last = Some(t);
        }
    }
    if let Some(p) = last { gap = gap.max(end - p); }
    gap
}

pub fn app_dir_pub() -> PathBuf { app_dir() }
fn app_dir() -> PathBuf {
    std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir()).join("Clipper")
}

#[allow(dead_code)]
fn b64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::with_capacity(data.len() * 4 / 3 + 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            o.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    o
}

/// Standard base64 -> bytes (None if malformed).
pub fn unb64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c { b'A'..=b'Z' => c - b'A', b'a'..=b'z' => c - b'a' + 26, b'0'..=b'9' => c - b'0' + 52, b'+' | b'-' => 62, b'/' | b'_' => 63, _ => return None } as u32)
    };
    let s: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').collect();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for ch in s.chunks(4) {
        let mut n = 0u32;
        for (i, c) in ch.iter().enumerate() {
            n |= val(*c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if ch.len() > 2 { out.push((n >> 8) as u8); }
        if ch.len() > 3 { out.push(n as u8); }
    }
    Some(out)
}

fn safe_name(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect()
}

/// Games seen while recording ("games.json": name -> exe) + their icons, for the sidebar / Settings > Games.
pub fn remember_game(name: &str, exe: &str) {
    let p = app_dir().join("games.json");
    let mut m: std::collections::BTreeMap<String, String> = fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    if m.get(name).map_or(false, |e| e == exe) {
        return;
    }
    m.insert(name.into(), exe.into());
    let _ = fs::create_dir_all(app_dir());
    let _ = fs::write(&p, serde_json::to_string_pretty(&m).unwrap_or_default());
    // exe icon -> png (once)
    let icon = app_dir().join("icons").join(format!("{}.png", safe_name(name)));
    if !exe.is_empty() && !icon.exists() {
        let _ = fs::create_dir_all(app_dir().join("icons"));
        #[cfg(windows)]
        if let Some(px) = crate::winutil::exe_icon_bgra(exe, 64) {
            // raw pixels -> PNG with ffmpeg
            let raw = std::env::temp_dir().join("echo_icon.raw");
            if fs::write(&raw, &px).is_ok() {
                let _ = ffmpeg().args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgra", "-s", "64x64", "-i"])
                    .arg(&raw).args(["-frames:v", "1"]).arg(&icon).stdout(Stdio::null()).stderr(Stdio::null()).status();
                let _ = fs::remove_file(&raw);
            }
        }
    }
}

#[allow(dead_code)]
/// `fullscreen`: the window covers its whole monitor. Unknown apps only count as games when they do
/// (so Bitwarden, Lightshot, Echo itself… in a normal window aren't "games").
fn game_name(stem: &str, title: &str, fullscreen: bool) -> Option<String> {
    let name = game_name_raw(stem, title, fullscreen)?;
    let not = NOT_GAMES.lock().unwrap();
    let (n, s) = (name.to_lowercase(), stem.to_lowercase());
    if not.iter().any(|x| *x == n || *x == s) { None } else { Some(name) }
}

fn game_name_raw(stem: &str, title: &str, fullscreen: bool) -> Option<String> {
    let s = stem.to_lowercase();
    const IGNORE: &[&str] = &[
        "explorer", "clipper", "applicationframehost", "searchhost", "searchapp", "shellexperiencehost", "startmenuexperiencehost",
        "lockapp", "taskmgr", "chrome", "msedge", "firefox", "brave", "opera", "opera_gx", "discord", "spotify", "steam",
        "steamwebhelper", "epicgameslauncher", "code", "devenv", "obs64", "medal", "claude", "cmd", "powershell", "windowsterminal",
        "wt", "notepad", "textinputhost", "systemsettings", "battle.net", "riotclientux", "riotclientservices", "eadesktop",
        "ubisoftconnect", "upc", "galaxyclient", "vlc", "mpc-hc64", "photos", "whatsapp", "telegram", "teams", "ms-teams",
        "echo", "filen", "bitwarden", "lightshot", "1password", "keepass", "keepassxc", "snippingtool", "screenclippinghost",
        "sharex", "greenshot", "nvidia app", "radeonsoftware", "amdrsserv", "msiafterburner", "rtss", "outlook", "winword",
        "excel", "powerpnt", "onenote", "slack", "zoom", "notion", "obsidian", "signal", "thunderbird", "mspaint", "notepad++",
    ];
    if IGNORE.contains(&s.as_str()) {
        return None;
    }
    const KNOWN: &[(&str, &str)] = &[
        ("fivem", "FiveM"), ("gta5", "GTA V"), ("gtav", "GTA V"), ("playgtav", "GTA V"), ("valorant", "Valorant"),
        ("cs2", "Counter-Strike 2"), ("csgo", "CS GO"), ("fortniteclient", "Fortnite"), ("r5apex", "Apex Legends"),
        ("rocketleague", "Rocket League"), ("league of legends", "League of Legends"), ("overwatch", "Overwatch 2"),
        ("destiny2", "Destiny 2"), ("eldenring", "Elden Ring"), ("cod", "Call of Duty"), ("rainbowsix", "Rainbow Six Siege"),
        ("tslgame", "PUBG"), ("robloxplayerbeta", "Roblox"), ("dota2", "Dota 2"), ("rustclient", "Rust"),
        ("marvel-win64", "Marvel Rivals"), ("helldivers2", "Helldivers 2"), ("escapefromtarkov", "Escape from Tarkov"),
        ("eft", "Escape from Tarkov"), ("project8", "Deadlock"), ("deadlock", "Deadlock"), ("bf2042", "Battlefield 2042"),
        ("rdr2", "Red Dead Redemption 2"), ("minecraft", "Minecraft"), ("thefinals", "The Finals"), ("discovery", "The Finals"),
        ("palworld", "Palworld"), ("sotgame", "Sea of Thieves"), ("warframe", "Warframe"), ("forzahorizon5", "Forza Horizon 5"),
    ];
    for (k, name) in KNOWN {
        if s.starts_with(k) {
            return Some(name.to_string());
        }
    }
    if (s == "javaw" || s == "java") && title.to_lowercase().contains("minecraft") {
        return Some("Minecraft".into());
    }
    if s == "javaw" || s == "java" || !fullscreen {
        return None;
    }
    // unknown: short window title if it looks like a name, else a tidied exe name
    let clean = |x: &str| -> String {
        x.chars().filter(|c| !r#"\/:*?"<>|"#.contains(*c)).collect::<String>().trim().to_string()
    };
    let t = clean(title);
    if !t.is_empty() && t.chars().count() <= 32 && t.chars().any(|c| c.is_alphabetic()) {
        return Some(t);
    }
    let mut n = stem.to_string();
    for suf in ["-Win64-Shipping", "-WinGDK-Shipping", "_x64", "-Shipping", "Win64", "_dx12", "_dx11", "_BE"] {
        if let Some(i) = n.find(suf) {
            n.truncate(i);
        }
    }
    let n = clean(&n);
    if n.is_empty() { None } else { Some(n) }
}

/// One spoken word with its time in the SOURCE clip (seconds).
#[derive(Serialize, Clone)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

fn whisper_dir() -> PathBuf {
    let base = std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
    base.join("Clipper").join("whisper")
}

/// Tools shipped with the installer (Echo-Setup.exe puts ffmpeg / rclone in "tools" next to Echo.exe):
/// used first, so an installed Echo doesn't download and run programs by itself.
fn bundled(names: &[&str]) -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.join("tools");
    find_file(&dir, names)
}

fn find_file(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    for e in fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_file(&p, names) {
                return Some(f);
            }
        } else if p.file_name().map_or(false, |n| names.iter().any(|x| n.eq_ignore_ascii_case(x))) {
            return Some(p);
        }
    }
    None
}

/// ffmpeg + ffprobe: use the ones on PATH; otherwise (a friend's PC) the copy in %APPDATA%\Clipper\ffmpeg,
/// downloaded once. The folder is put in front of this process' PATH so every "ffmpeg"/"ffprobe" call finds it.
pub fn ensure_ffmpeg() {
    let has = |c: &str| hidden(c).arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().map_or(false, |s| s.success());
    let dir = app_dir().join("ffmpeg");
    let use_dir = |d: &Path| {
        let old = std::env::var_os("PATH").unwrap_or_default();
        let mut parts = vec![d.to_path_buf()];
        parts.extend(std::env::split_paths(&old));
        if let Ok(p) = std::env::join_paths(parts) {
            std::env::set_var("PATH", p);
        }
    };
    let local = || find_file(&dir, &["ffmpeg.exe"]).filter(|f| f.with_file_name("ffprobe.exe").exists());
    if let Some(f) = bundled(&["ffmpeg.exe"]).filter(|f| f.with_file_name("ffprobe.exe").exists()).or_else(local) {
        if let Some(d) = f.parent() {
            use_dir(d);
        }
        return;
    }
    if has("ffmpeg") && has("ffprobe") {
        return;
    }
    log_line("ffmpeg: not installed - downloading it once (~100 MB)");
    let _ = fs::create_dir_all(&dir);
    let zip = dir.join("ffmpeg.zip");
    let mut res = Err(String::new());
    // one pinned build, checked by fingerprint (chat 28 - the floating "release-essentials" link can't be checked)
    for _ in 0..2 {
        res = download_checked("https://github.com/GyanD/codexffmpeg/releases/download/9.0.2/ffmpeg-9.0.2-essentials_build.zip", &zip,
            "sha256:60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba");
        if res.is_ok() {
            break;
        }
    }
    if let Err(e) = res {
        log_line(&format!("ffmpeg: download failed: {e}"));
        return;
    }
    let ok = unzip_to(&zip, &dir).map_err(|e| log_line(&format!("ffmpeg: unpack failed: {e}"))).is_ok();
    let _ = fs::remove_file(&zip);
    match local() {
        Some(f) if ok => {
            log_line(&format!("ffmpeg: ready ({})", f.display()));
            if let Some(d) = f.parent() {
                use_dir(d);
            }
        }
        _ => log_line("ffmpeg: couldn't unpack the download"),
    }
}

/// Downloads like download(), then checks the file's fingerprint ("sha256:<hex>" or "sha1:<hex>") before it's used -
/// a hacked or broken download is deleted instead of being run (chat 28 security pass). "" = no check possible.
fn download_checked(url: &str, to: &Path, want: &str) -> Result<(), String> {
    download(url, to)?;
    if want.is_empty() { return Ok(()); }
    let (alg, hex) = want.split_once(':').ok_or("bad checksum")?;
    let algo = match alg { "sha256" => &ring::digest::SHA256, "sha1" => &ring::digest::SHA1_FOR_LEGACY_USE_ONLY, _ => return Err("bad checksum".into()) };
    let mut ctx = ring::digest::Context::new(algo);
    let mut f = fs::File::open(to).map_err(err)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = std::io::Read::read(&mut f, &mut buf).map_err(err)?;
        if n == 0 { break; }
        ctx.update(&buf[..n]);
    }
    let got: String = ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect();
    if !got.eq_ignore_ascii_case(hex) {
        let _ = fs::remove_file(to);
        log_line(&format!("download: fingerprint mismatch for {url} (got {got}) - file deleted"));
        return Err(format!("the download of {} didn't match its fingerprint", url.rsplit('/').next().unwrap_or(url)));
    }
    Ok(())
}

fn download(url: &str, to: &Path) -> Result<(), String> {
    let tmp = to.with_extension("part");
    if let Err(e) = crate::net::download(url, &tmp) {
        log_line(&format!("download failed: {e}"));
    }
    if fs::metadata(&tmp).map_or(true, |m| m.len() < 1000) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("download failed: {url}"));
    }
    fs::rename(&tmp, to).map_err(err)
}

/// whisper.cpp needs the Microsoft Visual C++ runtime, which Windows LTSC doesn't include.
fn ensure_vc_runtime() -> Result<(), String> {
    #[cfg(windows)]
    {
        let sys = PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join("System32");
        let missing: Vec<&str> = ["msvcp140.dll", "vcruntime140_1.dll", "vcomp140.dll"]
            .into_iter().filter(|d| !sys.join(d).exists()).collect();
        if missing.is_empty() {
            return Ok(());
        }
        log_line(&format!("captions: Visual C++ runtime missing ({}), installing", missing.join(", ")));
        let exe = whisper_dir().join("vc_redist.x64.exe");
        if !exe.exists() {
            download("https://aka.ms/vs/17/release/vc_redist.x64.exe", &exe)
                .map_err(|e| format!("Couldn't download the Microsoft Visual C++ runtime ({e})"))?;
        }
        // needs admin: Windows shows one permission prompt
        let code = crate::winutil::run_elevated(&exe.to_string_lossy(), "/install /quiet /norestart", true).unwrap_or(-1);
        log_line(&format!("captions: vc_redist exit code {code}"));
        // 0 = ok, 3010 = ok (restart recommended), 1638 = newer version already installed
        if ![0, 3010, 1638].contains(&code) && !sys.join("msvcp140.dll").exists() {
            return Err("The Microsoft Visual C++ runtime wasn't installed (the permission prompt was declined?)".into());
        }
    }
    Ok(())
}

/// whisper.cpp command-line tool, downloaded the first time captions are used.
fn whisper_cli() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("CLIPPER_WHISPER_CLI") {
        return Ok(PathBuf::from(p));
    }
    #[cfg(not(windows))]
    {
        // Linux: a whisper-cli already on the system (or downloaded before), else Echo's own build from the
        // echoclip release (Fedora's whisper-cpp package has only the library, no command-line tool - chat 28)
        if let Some(p) = linux_whisper() {
            return Ok(p);
        }
        if crate::linuxutil::in_flatpak() { return Err("Speech recognition is missing from this Echo build".into()); }
        return linux_whisper_download();
    }
    #[allow(unreachable_code)]
    let dir = whisper_dir();
    // prefer whisper-cli.exe: in new releases main.exe is only a stub that says "renamed" and exits
    let find = |d: &Path| find_file(d, &["whisper-cli.exe"]).or_else(|| find_file(d, &["main.exe"]));
    if let Some(p) = find(&dir) {
        return Ok(p);
    }
    fs::create_dir_all(&dir).map_err(err)?;
    let zip = dir.join("whisper.zip");
    // pinned + fingerprint-checked (chat 28; "latest" can't be checked)
    let urls = ["https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.2/whisper-bin-x64.zip"];
    let mut last = String::new();
    for u in urls {
        match download_checked(u, &zip, "sha256:49dcc16de826f20bd53d44f947a1ae49dfa81f86cad67a64d80820cb192d674a") {
            Ok(()) => {
                log_line(&format!("captions: downloaded {u}"));
                last.clear();
                break;
            }
            Err(e) => last = e,
        }
    }
    if !last.is_empty() {
        return Err(format!("Couldn't download the speech tool ({last})"));
    }
    let ok = unzip_to(&zip, &dir).map_err(|e| log_line(&format!("unpack failed: {e}"))).is_ok();
    let _ = fs::remove_file(&zip);
    if !ok {
        return Err("couldn't unpack the speech tool".into());
    }
    find(&dir).ok_or_else(|| "speech tool not found after download".into())
}

/// Download Echo's whisper-cli (built by the public repo's Linux job, release file whisper-cli-x86_64-linux) into
/// the whisper folder, checked against GitHub's SHA-256 of the file.
#[cfg(not(windows))]
fn linux_whisper_download() -> Result<PathBuf, String> {
    if !cfg!(target_arch = "x86_64") { return Err("Speech recognition isn't available for this processor yet".into()); }
    let dir = whisper_dir();
    fs::create_dir_all(&dir).map_err(err)?;
    let exe = dir.join("whisper-cli");
    log_line("captions: downloading whisper-cli (Echo's Linux build)");
    let v = crate::net::get_json("https://api.github.com/repos/Resister0/echoclip/releases/latest")
        .map_err(|e| { log_line(&format!("captions: {e}")); "Couldn't reach GitHub to get the speech tool".to_string() })?;
    let asset = v["assets"].as_array().into_iter().flatten()
        .find(|a| a["name"].as_str() == Some("whisper-cli-x86_64-linux"))
        .ok_or("The speech tool isn't in the newest Echo release yet - try again after the next update")?;
    let url = asset["browser_download_url"].as_str().unwrap_or_default().to_string();
    let digest = asset["digest"].as_str().unwrap_or_default().to_string();
    if digest.is_empty() { return Err("GitHub gave no fingerprint for the speech tool - try again later".into()); }
    let tmp = dir.join("whisper-cli.download");
    download_checked(&url, &tmp, &digest).map_err(|e| format!("Couldn't download the speech tool ({e})"))?;
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).map_err(err)?;
    }
    fs::rename(&tmp, &exe).map_err(err)?;
    log_line(&format!("captions: downloaded {url}"));
    Ok(exe)
}

fn whisper_model(name: &str) -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("CLIPPER_WHISPER_MODEL") {
        return Ok(PathBuf::from(p));
    }
    let name = if name == "small" { "small" } else { "base" };
    let dir = whisper_dir().join("models");
    fs::create_dir_all(&dir).map_err(err)?;
    let f = dir.join(format!("ggml-{name}.bin"));
    if !f.exists() {
        // checksums (SHA-1) as published in whisper.cpp's models/README.md
        let want = if name == "small" { "sha1:55356645c2b361a969dfd0ef2c5a50d530afd8d5" } else { "sha1:465707469ff3a37a2b9b8d8f89f2f99de7299dac" };
        download_checked(&format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{name}.bin"), &f, want)
            .map_err(|e| format!("Couldn't download the speech model ({e})"))?;
        log_line(&format!("captions: downloaded model {name}"));
    }
    Ok(f)
}

/// Is everything for captions already downloaded? (so the UI can warn about the first-time download)
/// whisper.cpp's command-line tool on PATH (Linux packages name it differently).
#[cfg(not(windows))]
fn linux_whisper() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for name in ["whisper-cli", "whisper-cpp", "whisper.cpp", "whisper-cpp-cli"] {
        for d in std::env::split_paths(&path).chain([PathBuf::from("/usr/bin"), PathBuf::from("/usr/local/bin")]) {
            let f = d.join(name);
            if f.is_file() {
                return Some(f);
            }
        }
    }
    // other package names: any whisper*cli* in /usr/bin
    if let Ok(rd) = fs::read_dir("/usr/bin") {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_lowercase();
            if n.starts_with("whisper") && n.contains("cli") {
                return Some(e.path());
            }
        }
    }
    find_file(&whisper_dir(), &["whisper-cli"])
}

pub fn captions_ready(model: &str) -> bool {
    let m = if model == "small" { "small" } else { "base" };
    #[cfg(not(windows))]
    let tool = linux_whisper().is_some();
    #[cfg(windows)]
    let tool = find_file(&whisper_dir(), &["whisper-cli.exe", "main.exe"]).is_some();
    tool
        && whisper_dir().join("models").join(format!("ggml-{m}.bin")).exists()
}

/// 16-bit mono PCM from a WAV file made by ffmpeg.
fn read_wav16(p: &Path) -> Vec<i16> {
    let b = fs::read(p).unwrap_or_default();
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
    for x in [16u32.to_le_bytes().as_slice(), &1u16.to_le_bytes(), &1u16.to_le_bytes(), &rate.to_le_bytes(),
              &(rate * 2).to_le_bytes(), &2u16.to_le_bytes(), &16u16.to_le_bytes()] {
        v.extend_from_slice(x);
    }
    v.extend_from_slice(b"data");
    v.extend_from_slice(&n.to_le_bytes());
    for s in pcm {
        v.extend_from_slice(&s.to_le_bytes());
    }
    fs::write(p, v).map_err(err)
}

/// Where someone is actually talking (seconds), from the loudness of the mic audio.
/// Whisper invents text for silence and smears timestamps, so only these parts are sent to it.
fn speech_regions(pcm: &[i16], rate: u32) -> Vec<(f64, f64)> {
    let hop = (rate / 50) as usize; // 20 ms frames
    if pcm.len() < hop {
        return Vec::new();
    }
    let db: Vec<f64> = pcm.chunks(hop).map(|c| {
        let e = c.iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum::<f64>() / c.len() as f64;
        10.0 * (e + 1e-12).log10()
    }).collect();
    let mut sorted = db.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let floor = sorted[sorted.len() / 5];            // background noise level
    // above the background, but never higher than normal speaking level (so non-stop talking still counts)
    let thr = (floor + 14.0).max(-48.0).min(-34.0);
    let fs = hop as f64 / rate as f64;
    let mut regs: Vec<(f64, f64)> = Vec::new();
    for (i, &d) in db.iter().enumerate() {
        if d > thr {
            let t = i as f64 * fs;
            match regs.last_mut() {
                Some(r) if t - r.1 < 0.6 => r.1 = t + fs,  // bridge short pauses between words
                _ => regs.push((t, t + fs)),
            }
        }
    }
    let total = pcm.len() as f64 / rate as f64;
    let mut out: Vec<(f64, f64)> = regs.into_iter()
        .filter(|r| r.1 - r.0 >= 0.25)
        .map(|(a, b)| ((a - 0.3).max(0.0), (b + 0.3).min(total)))
        .collect();
    // merge overlaps, and keep the number of whisper runs small
    out.dedup_by(|b, a| if b.0 <= a.1 { a.1 = a.1.max(b.1); true } else { false });
    while out.len() > 10 {
        let k = (1..out.len()).min_by(|&x, &y| (out[x].0 - out[x - 1].1).partial_cmp(&(out[y].0 - out[y - 1].1)).unwrap()).unwrap();
        out[k - 1].1 = out[k].1;
        out.remove(k);
    }
    out
}

fn parse_whisper_json(s: &str) -> Vec<Word> {
    let v: serde_json::Value = match serde_json::from_str(s) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for seg in v["transcription"].as_array().cloned().unwrap_or_default() {
        let text = seg["text"].as_str().unwrap_or("").trim().to_string();
        let (a, b) = (seg["offsets"]["from"].as_f64(), seg["offsets"]["to"].as_f64());
        // skip empty bits and whisper's non-speech tags like [MUSIC] / (laughs)
        if text.is_empty() || text.starts_with('[') || (text.starts_with('(') && text.ends_with(')')) {
            continue;
        }
        if let (Some(a), Some(b)) = (a, b) {
            out.push(Word { text, start: a / 1000.0, end: (b / 1000.0).max(a / 1000.0 + 0.05) });
        }
    }
    out
}

pub fn log_path() -> PathBuf {
    std::env::temp_dir().join("clipper_ffmpeg.log")
}

/// GPU encoders that can't work on this system: AMD AMF and Intel QSV are Windows-only here, NVENC on Linux needs
/// NVIDIA's libcuda. Each one tried in vain cost an ffmpeg start + decoding (exports were slow on Linux).
#[allow(dead_code)]
fn skip_enc(venc: &[&str]) -> bool {
    if cfg!(windows) { return false; }
    static CUDA: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let cuda = *CUDA.get_or_init(|| ["/usr/lib64/libcuda.so.1", "/usr/lib/x86_64-linux-gnu/libcuda.so.1", "/usr/lib/libcuda.so.1", "/usr/lib64/nvidia/libcuda.so.1"]
        .iter().any(|p| Path::new(p).exists()));
    venc.iter().any(|a| a.ends_with("_amf") || a.ends_with("_qsv") || (a.ends_with("_nvenc") && !cuda))
}

/// ffmpeg error lines that health() treats as "the recorder's encoder stopped" - but written by log_line (an
/// export's / shrink's ffmpeg output), not by the recorder's own ffmpeg. health() subtracts these.
const REC_FAIL_MARKS: [&str; 3] = ["Terminating thread with return code", "Error submitting audio frame", "Error submitting video frame"];
static LOGGED_MARKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Linux: the web view's GStreamer warnings/errors (main.rs sets GST_DEBUG_FILE to this); part of "Send log".
pub fn gst_log_path() -> PathBuf {
    std::env::temp_dir().join("clipper_gst.log")
}

pub fn log_line(msg: &str) {
    let marks: u64 = REC_FAIL_MARKS.iter().map(|m| msg.matches(m).count() as u64).sum();
    if marks > 0 { LOGGED_MARKS.fetch_add(marks, Ordering::Relaxed); }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = writeln!(f, "{msg}");
    }
}

/// 128 random bits as hex (OS-seeded; for secret localhost addresses).
pub fn random_token() -> String {
    // the operating system's secure random generator (chat 28; was a hash of the clock with random keys)
    use ring::rand::SecureRandom;
    let mut b = [0u8; 16];
    if ring::rand::SystemRandom::new().fill(&mut b).is_err() {
        use std::hash::{BuildHasher, Hasher};
        for (i, x) in b.iter_mut().enumerate() {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u64(i as u64 ^ now_unix());
            *x = h.finish() as u8;
        }
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}
/// secret path of the cloud streaming server (rclone serve --baseurl)
static SERVE_TOKEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
fn serve_token() -> &'static str {
    SERVE_TOKEN.get_or_init(random_token)
}
/// for the local video server: it only passes requests on to the cloud streaming server (chat 28)
pub fn serve_token_pub() -> &'static str { serve_token() }

/// A Command for a program from the system. Inside the AppImage, Echo's own bundled
/// libraries (LD_LIBRARY_PATH, GIO/GTK/GStreamer paths pointing into the mount) are
/// removed from its environment - otherwise the system's gpu-screen-recorder, ffmpeg,
/// parec... load the AppImage's older libgnutls/libssl and quit at once
/// ("version `GNUTLS_3_7_7' not found").
pub fn sys_cmd(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(target_os = "linux")]
    for (k, v) in appimage_env_fixes() { match v { Some(v) => { c.env(k, v); } None => { c.env_remove(k); } } }
    c
}

/// Environment changes for programs started from inside the AppImage (empty outside it).
#[cfg(target_os = "linux")]
pub fn appimage_env_fixes() -> &'static [(String, Option<String>)] {
    static FIX: std::sync::OnceLock<Vec<(String, Option<String>)>> = std::sync::OnceLock::new();
    FIX.get_or_init(|| {
        if std::env::var_os("APPIMAGE").is_none() { return Vec::new(); }
        let dir = std::env::var("APPDIR").ok().filter(|d| d.len() > 1).or_else(|| {
            // .../.mount_XXXX/usr/bin/echo -> .../.mount_XXXX
            std::env::current_exe().ok()?.parent()?.parent()?.parent().map(|p| p.to_string_lossy().into_owned())
        });
        let Some(dir) = dir else { return Vec::new() };
        let dir = dir.trim_end_matches('/').to_string();
        // also any OTHER AppImage's mount: after an update Echo restarts itself from the old version, so its
        // variables still hold the old /tmp/.mount_EchoXXXX paths next to the new ones (chat 27: recording
        // failed after every auto-update until Echo was quit and started again)
        let inside = |p: &str| p == dir || p.starts_with(&format!("{dir}/")) || p.contains("/.mount_");
        let mut out = Vec::new();
        for (k, v) in std::env::vars() {
            if matches!(k.as_str(), "APPIMAGE" | "APPDIR" | "ARGV0" | "OWD") || !v.split(':').any(|p| inside(p)) { continue; }
            let keep: Vec<&str> = v.split(':').filter(|p| !p.is_empty() && !inside(p)).collect();
            if keep.is_empty() { out.push((k, None)); } else { out.push((k, Some(keep.join(":")))); }
        }
        if !out.is_empty() {
            let names: Vec<&str> = out.iter().map(|(k, _)| k.as_str()).collect();
            log_line(&format!("appimage: system programs start without Echo's bundled paths ({})", names.join(", ")));
        }
        out
    })
}

fn hidden(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = sys_cmd(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

fn ffmpeg() -> Command {
    hidden("ffmpeg")
}

/// Unpack a .zip into `dir` inside Echo (paths that would escape `dir` are skipped).
pub fn unzip_to(zip_path: &Path, dir: &Path) -> Result<(), String> {
    let f = fs::File::open(zip_path).map_err(err)?;
    let mut z = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| format!("bad zip: {e}"))?;
    for i in 0..z.len() {
        let mut e = z.by_index(i).map_err(err)?;
        let Some(rel) = e.enclosed_name() else { continue };
        let out = dir.join(rel);
        if e.is_dir() {
            fs::create_dir_all(&out).map_err(err)?;
            continue;
        }
        if let Some(p) = out.parent() {
            fs::create_dir_all(p).map_err(err)?;
        }
        let mut w = fs::File::create(&out).map_err(err)?;
        std::io::copy(&mut e, &mut w).map_err(err)?;
        #[cfg(unix)]
        if let Some(mode) = e.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&out, fs::Permissions::from_mode(mode & 0o777));
        }
    }
    Ok(())
}

/// Set by the UI at start: Echo's video player decodes AV1 smoothly (in hardware). Without it Echo never makes AV1 files,
/// so PCs with older graphics cards never end up with clips that stutter.
pub static AV1_PLAYS: AtomicBool = AtomicBool::new(false);
static AV1_ENC: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();

/// The hardware AV1 encoder of this PC (Radeon RX 7000+, GeForce RTX 40+, Intel Arc), tested once with a tiny encode
/// that Echo's own ffmpeg must also be able to read back (thumbnails, editor, exports). None on Linux for now.
pub fn av1_encoder() -> Option<&'static str> {
    *AV1_ENC.get_or_init(|| {
        if !cfg!(windows) {
            return None;
        }
        let tmp = std::env::temp_dir().join("echo_av1_test.mp4");
        for enc in ["av1_amf", "av1_nvenc", "av1_qsv"] {
            let ok = ffmpeg().args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=640x360:r=30",
                    "-frames:v", "10", "-pix_fmt", "yuv420p", "-c:v", enc, "-b:v", "1M", "-f", "mp4"]).arg(&tmp)
                .stdout(Stdio::null()).stderr(Stdio::null()).status().map_or(false, |s| s.success())
                && fs::metadata(&tmp).map_or(false, |m| m.len() > 0)
                && ffmpeg().args(["-hide_banner", "-loglevel", "error", "-i"]).arg(&tmp).args(["-f", "null", "-"])
                    .stdout(Stdio::null()).stderr(Stdio::null()).status().map_or(false, |s| s.success());
            let _ = fs::remove_file(&tmp);
            if ok {
                log_line(&format!("av1: hardware encoder {enc} works"));
                return Some(enc);
            }
        }
        log_line("av1: no hardware AV1 encoder - shrinking stays H.264");
        None
    })
}

/// AV1 is used only when the GPU encodes it AND the player decodes it in hardware.
pub fn av1_ready() -> bool {
    AV1_PLAYS.load(Ordering::Relaxed) && av1_encoder().is_some()
}

/// share links get the watermarked copy (Settings → Sharing → "Watermark on share links", and a watermark style)
/// Watermark shown on share links: the share page lays it over the video and it's on the preview image
/// (the clip itself is uploaded as it is - re-encoding for a burned-in logo was too slow, chat 16). "off" = none.
fn share_wm(cfg: &crate::settings::Settings) -> &str {
    if cfg.share_watermark && cfg.watermark_style != "off" { match cfg.watermark_style.as_str() { "echo" => "echo", "link" => "link", _ => "kimbo" } } else { "off" }
}

pub fn is_av1(path: &str) -> bool {
    hidden("ffprobe").args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name", "-of", "csv=p=0"])
        .arg(path).output().map_or(false, |o| String::from_utf8_lossy(&o.stdout).trim() == "av1")
}

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// The seek-bar strip being made right now (clip path + its ffmpeg), so a move/rename/delete of that clip
/// can stop it instead of failing with "being used by another process" (os error 32, chat 21).
static STRIP_RUN: Mutex<Option<(PathBuf, std::process::Child)>> = Mutex::new(None);

fn stop_strip_for(p: &Path) {
    if let Ok(mut g) = STRIP_RUN.lock() {
        if let Some((cp, ch)) = g.as_mut() {
            if cp.as_path() == p {
                let _ = ch.kill();
                log_line(&format!("filmstrip: stopped for {} (clip is being moved)", p.display()));
            }
        }
    }
}

/// fs::rename that waits a moment when Windows says the file is in use (a preview, a strip being made,
/// antivirus): stops our own strip build for it, then retries for up to ~4 s.
/// fs::write for Echo's small json files (favourites, titles/tags, links…): if Windows says the file is in use
/// (a sync app / antivirus reading it), wait a moment and try again instead of showing "os error 32"
fn write_patient(p: &Path, data: &str) -> Result<(), String> {
    let mut tries = 0;
    loop {
        match fs::write(p, data) {
            Ok(()) => return Ok(()),
            Err(e) if tries < 12 && matches!(e.raw_os_error(), Some(32) | Some(33) | Some(5)) => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(150));
            }
            Err(e) => {
                log_line(&format!("write {}: {e}", p.display()));
                return Err(if e.raw_os_error() == Some(32) {
                    format!("{} is open in another program (a sync app?) - try again in a moment", p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())
                } else { e.to_string() });
            }
        }
    }
}

fn rename_patient(from: &Path, to: &Path) -> Result<(), String> {
    let mut tries = 0;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if tries < 16 && matches!(e.raw_os_error(), Some(32) | Some(33) | Some(5)) => {
                if tries == 0 {
                    stop_strip_for(from);
                }
                tries += 1;
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => {
                return Err(if e.raw_os_error() == Some(32) {
                    "The clip is open in another program (a video player or editor) - close it and try again".into()
                } else {
                    e.to_string()
                })
            }
        }
    }
}

/// Kill ffmpeg recorders left behind by a crashed Clipper (they block screen capture).
fn kill_leftovers() {
    #[cfg(windows)]
    crate::winutil::kill_orphans(&["ffmpeg.exe", "rclone.exe"]);
}

/// Format of the default output device (what loopback delivers).
fn loopback_format() -> Result<(u32, u16), String> {
    #[cfg(target_os = "linux")]
    return Ok((48000, 2));
    #[allow(unreachable_code)]
    let dev = cpal::default_host().default_output_device().ok_or("No audio output device")?;
    let cfg = dev.default_output_config().map_err(err)?;
    Ok((cfg.sample_rate().0, cfg.channels()))
}

type Buf = Arc<Mutex<VecDeque<f32>>>;

/// Linear resampler + channel mapper (mics often run at 16/44.1 kHz and mono).
struct Resampler {
    step: f64,
    t: f64,
    cin: usize,
    cout: usize,
    prev: Vec<f32>,
}

impl Resampler {
    fn new(in_rate: u32, out_rate: u32, cin: usize, cout: usize) -> Self {
        Resampler { step: in_rate as f64 / out_rate as f64, t: 0.0, cin: cin.max(1), cout, prev: vec![0.0; cout] }
    }
    fn push(&mut self, input: &[f32], q: &mut VecDeque<f32>) {
        let mut cur = vec![0.0f32; self.cout];
        for f in input.chunks(self.cin) {
            if self.cin >= 6 && self.cout == 2 && f.len() >= 6 {
                // surround -> stereo (Windows order FL FR FC LFE BL BR [SL SR]): centre (voices) and the
                // rear/side speakers go into both sides, LFE is left out
                let g = |i: usize| *f.get(i).unwrap_or(&0.0);
                let (sl, sr) = (g(4) + g(6), g(5) + g(7));
                cur[0] = g(0) + 0.707 * g(2) + 0.5 * sl;
                cur[1] = g(1) + 0.707 * g(2) + 0.5 * sr;
            } else {
                for (c, v) in cur.iter_mut().enumerate() {
                    *v = if self.cin == 1 { f[0] } else { *f.get(c).unwrap_or(&0.0) };
                }
            }
            while self.t < 1.0 {
                let t = self.t as f32;
                for c in 0..self.cout {
                    q.push_back(self.prev[c] + (cur[c] - self.prev[c]) * t);
                }
                self.t += self.step;
            }
            self.t -= 1.0;
            self.prev.copy_from_slice(&cur);
        }
    }
}

/// Opens desktop loopback (`mic == false`) or the default microphone on its own thread and
/// pushes samples, converted to `ch` channels, into `buf`. Never blocks the caller.
/// Microphones Windows knows about: (names, default name).
pub fn list_mics() -> (Vec<String>, String) {
    // Linux: the sound server's microphones (PipeWire / PulseAudio), never the raw ALSA cards - opening those
    // directly took the headset mic away from PipeWire (Discord lost it, even after Echo closed)
    #[cfg(target_os = "linux")]
    {
        let srcs = linux_sources();
        // name of the current default ("System default (DualShock 4 …)") - so a controller that took over is visible
        let def = sys_cmd("pactl").env("LC_ALL", "C").arg("get-default-source").stdin(Stdio::null()).stderr(Stdio::null()).output().ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .and_then(|n| srcs.iter().find(|(a, _)| *a == n).map(|(_, d)| d.clone()))
            .unwrap_or_default();
        return (srcs.into_iter().map(|(n, _)| n).collect(), def);
    }
    #[allow(unreachable_code)]
    let host = cpal::default_host();
    #[allow(unused_mut)]
    let mut names: Vec<String> = host.input_devices().map(|it| it.filter_map(|d| d.name().ok()).collect()).unwrap_or_default();
    #[allow(unused_mut)]
    let mut def = host.default_input_device().and_then(|d| d.name().ok()).unwrap_or_default();
    // Linux/ALSA lists every plug-in variant of each card; keep one entry per real card
    #[cfg(target_os = "linux")]
    {
        names.retain(|n| n.starts_with("sysdefault:CARD="));
        if def == "default" || def == "pipewire" || def == "sysdefault" { def = String::new(); }
    }
    (names, def)
}

/// Friendly mic name: on Linux "sysdefault:CARD=Wireless" -> the card's full name from /proc/asound/cards
pub fn mic_label(name: &str) -> String {
    #[cfg(target_os = "linux")]
    if let Some(d) = SRC_DESC.lock().unwrap().desc_of(name) { return d.clone(); }
    #[cfg(target_os = "linux")]
    { if let Some(id) = name.strip_prefix("sysdefault:CARD=") {
        if let Ok(t) = std::fs::read_to_string("/proc/asound/cards") {
            for l in t.lines() {
                // " 1 [Wireless       ]: USB-Audio - HyperX Cloud II Wireless"
                if let (Some(a), Some(b)) = (l.find('['), l.find(']')) {
                    if a < b && l[a + 1..b].trim() == id {
                        if let Some(p) = l.find(" - ") { return l[p + 3..].trim().to_string(); }
                    }
                }
            }
        }
        return id.to_string();
    } }
    name.to_string()
}

/// Linux: sound-server source name -> its description ("HyperX Cloud Alpha Wireless Mono"), from the last list
#[cfg(target_os = "linux")]
static SRC_DESC: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
#[cfg(target_os = "linux")]
trait DescGet { fn desc_of(&self, n: &str) -> Option<&String>; }
#[cfg(target_os = "linux")]
impl DescGet for Vec<(String, String)> { fn desc_of(&self, n: &str) -> Option<&String> { self.iter().find(|(a, _)| a == n).map(|(_, d)| d) } }

/// Linux: microphones of the sound server (`pactl list sources`, monitors of speakers left out): (name, description)
#[cfg(target_os = "linux")]
fn linux_sources() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    if let Ok(o) = sys_cmd("pactl").env("LC_ALL", "C").args(["list", "sources"]).stdin(Stdio::null()).stderr(Stdio::null()).output() {
        let t = String::from_utf8_lossy(&o.stdout);
        let (mut name, mut desc, mut monitor) = (String::new(), String::new(), false);
        let mut flush = |name: &mut String, desc: &mut String, monitor: &mut bool| {
            if !name.is_empty() && !*monitor && !name.ends_with(".monitor") {
                out.push((name.clone(), if desc.is_empty() { name.clone() } else { desc.clone() }));
            }
            name.clear(); desc.clear(); *monitor = false;
        };
        for l in t.lines() {
            let l2 = l.trim();
            if l.starts_with("Source #") { flush(&mut name, &mut desc, &mut monitor); }
            else if let Some(v) = l2.strip_prefix("Name:") { name = v.trim().to_string(); }
            else if let Some(v) = l2.strip_prefix("Description:") { desc = v.trim().to_string(); }
            else if l2.starts_with("Monitor of Sink:") { monitor = l2 != "Monitor of Sink: n/a"; }
            else if l2.starts_with("device.class = \"monitor\"") { monitor = true; }
        }
        flush(&mut name, &mut desc, &mut monitor);
    } else {
        log_line("audio: listing microphones needs pactl (sudo dnf install pulseaudio-utils)");
    }
    *SRC_DESC.lock().unwrap() = out.clone();
    out
}

/// Linux mic through the sound server (parec), like desktop sound: the device stays shared with Discord etc.
/// `mic_name` = a source name from list_mics, "" = system default. Old settings ("sysdefault:CARD=…", raw ALSA)
/// are matched to the source with the same card name, else the default.
#[cfg(target_os = "linux")]
fn linux_mic(mic_name: String, buf: Buf, stop: Arc<AtomicBool>, rate: u32, ch: u16) {
    use std::io::Read;
    let srcs = linux_sources();
    let dev = if mic_name.is_empty() { None }
        else if srcs.iter().any(|(n, _)| *n == mic_name) { Some(mic_name.clone()) }
        else {
            let card = mic_label(&mic_name).to_lowercase();
            let hit = if mic_name.starts_with("sysdefault:") && !card.is_empty() {
                srcs.iter().find(|(_, d)| d.to_lowercase().contains(&card)).map(|(n, _)| n.clone())
            } else { None };
            if hit.is_none() { log_line(&format!("audio: mic '{mic_name}' not found, using the system default")); }
            hit
        };
    let device = dev.clone().unwrap_or_else(|| "@DEFAULT_SOURCE@".into());
    let mut child = match sys_cmd("parec")
        .args([&format!("--device={device}"), "--format=float32le", &format!("--rate={rate}"), &format!("--channels={ch}"), "--raw", "--latency-msec=20",
            "--client-name=Echo", "--stream-name=Echo microphone"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
    {
        Ok(c) => c,
        Err(e) => { log_line(&format!("audio: mic capture needs parec (sudo dnf install pulseaudio-utils): {e}")); return; }
    };
    CAP_DEV.lock().unwrap()[1] = device.clone();
    log_line(&format!("audio: mic via parec {} (shared with other apps)",
        dev.as_deref().map(|d| format!("'{}'", mic_label(d))).unwrap_or_else(|| "system default".into())));
    let mut out = child.stdout.take().unwrap();
    let mut b = vec![0u8; 4096 * 4];
    let mut rest: Vec<u8> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        let n = match out.read(&mut b) {
            Ok(0) | Err(_) => { if !stop.load(Ordering::Relaxed) { log_line("audio: mic capture ended (device gone?)"); } break; }
            Ok(n) => n,
        };
        rest.extend_from_slice(&b[..n]);
        let whole = rest.len() / 4 * 4;
        {
            let mut q = buf.lock().unwrap();
            for c in rest[..whole].chunks_exact(4) { q.push_back(f32::from_le_bytes([c[0], c[1], c[2], c[3]])); }
        }
        rest.drain(..whole);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(target_os = "linux")]
fn linux_desktop_audio(buf: Buf, stop: Arc<AtomicBool>, rate: u32, ch: u16) {
    use std::io::Read;
    let mut child = match sys_cmd("parec")
        .args(["--device=@DEFAULT_MONITOR@", "--format=float32le", &format!("--rate={rate}"), &format!("--channels={ch}"), "--raw", "--latency-msec=20"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
    {
        Ok(c) => c,
        Err(e) => { log_line(&format!("audio: desktop capture needs parec (sudo dnf install pulseaudio-utils): {e}")); return; }
    };
    log_line("audio: desktop via parec @DEFAULT_MONITOR@");
    let mut out = child.stdout.take().unwrap();
    let mut b = vec![0u8; 4096 * 4];
    let mut rest: Vec<u8> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        let n = match out.read(&mut b) { Ok(0) | Err(_) => break, Ok(n) => n };
        rest.extend_from_slice(&b[..n]);
        let whole = rest.len() / 4 * 4;
        {
            let mut q = buf.lock().unwrap();
            for c in rest[..whole].chunks_exact(4) { q.push_back(f32::from_le_bytes([c[0], c[1], c[2], c[3]])); }
        }
        rest.drain(..whole);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Video timing of one buffer segment (mpegts), read straight from the file's PES headers (no ffprobe):
/// (first video PTS - first PTS of any stream, video length incl. its last frame), both in seconds.
/// The concat demuxer otherwise steps from segment to segment by the LONGEST stream (audio runs a few ms
/// past the video), which left a one-frame hole in the video at every 2 s segment join = clips looked choppy.
fn ts_video_timing(path: &std::path::Path) -> Option<(f64, f64)> {
    let d = std::fs::read(path).ok()?;
    let pts_of = |p: &[u8]| -> u64 {
        (((p[0] as u64 >> 1) & 7) << 30) | ((p[1] as u64) << 22) | (((p[2] as u64) >> 1) << 15) | ((p[3] as u64) << 7) | ((p[4] as u64) >> 1)
    };
    let (mut vmin, mut vmax, mut smin) = (u64::MAX, 0u64, u64::MAX);
    let mut vpts: Vec<u64> = Vec::new();
    let mut i = 0;
    while i + 188 <= d.len() {
        let pk = &d[i..i + 188];
        i += 188;
        if pk[0] != 0x47 || pk[1] & 0x40 == 0 { continue; } // only packets that start a PES
        let afc = (pk[3] >> 4) & 3;
        if afc & 1 == 0 { continue; }
        let mut o = 4;
        if afc & 2 != 0 { o += 1 + pk[4] as usize; }
        if o + 14 > 188 { continue; }
        let p = &pk[o..];
        if p[0] != 0 || p[1] != 0 || p[2] != 1 { continue; }
        let sid = p[3];
        if p[7] & 0x80 == 0 { continue; } // no PTS
        let pts = pts_of(&p[9..14]);
        smin = smin.min(pts);
        if (0xE0..=0xEF).contains(&sid) { vmin = vmin.min(pts); vmax = vmax.max(pts); vpts.push(pts); }
    }
    if vpts.len() < 2 || smin == u64::MAX { return None; }
    vpts.sort_unstable();
    let mut deltas: Vec<u64> = vpts.windows(2).map(|w| w[1] - w[0]).filter(|&x| x > 0).collect();
    if deltas.is_empty() { return None; }
    deltas.sort_unstable();
    let frame = deltas[deltas.len() / 2];
    Some(((vmin - smin) as f64 / 90000.0, (vmax - vmin + frame) as f64 / 90000.0))
}

/// concat list for the segments: with "duration" lines so the VIDEO of each segment joins the next one exactly
/// (falls back to plain "file" lines if a segment can't be read).
fn concat_list(segs: &[std::path::PathBuf]) -> String {
    let t: Vec<Option<(f64, f64)>> = segs.iter().map(|p| ts_video_timing(p)).collect();
    let all = t.iter().all(|x| x.is_some());
    let mut body = String::new();
    for (k, p) in segs.iter().enumerate() {
        body += &format!("file '{}'\n", p.to_string_lossy().replace('\\', "/").replace('\'', "'\\''"));
        if all && k + 1 < segs.len() {
            let (lead, vdur) = t[k].unwrap();
            let (next_lead, _) = t[k + 1].unwrap();
            // next segment's video must start right where this one's video ends
            let dur = vdur + lead - next_lead;
            if dur > 0.0 { body += &format!("duration {dur:.6}\n"); }
        }
    }
    body
}

/// Device each capture is using right now: [desktop, mic] ("" = none / not started). health() compares them
/// with Windows' current defaults (headset switched off and on again, default speakers changed).
static CAP_DEV: Mutex<[String; 2]> = Mutex::new([String::new(), String::new()]);
/// unix time of the last default-device check (every 10 s)
static DEV_CHECKED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn spawn_capture(mic: bool, mic_name: String, buf: Buf, stop: Arc<AtomicBool>, rate: u32, ch: u16) {
    CAP_DEV.lock().unwrap()[mic as usize] = String::new();
    #[cfg(target_os = "linux")]
    {
        if mic { std::thread::spawn(move || linux_mic(mic_name, buf, stop, rate, ch)); }
        else { std::thread::spawn(move || linux_desktop_audio(buf, stop, rate, ch)); }
        return;
    }
    std::thread::spawn(move || {
        let what = if mic { "mic" } else { "desktop" };
        let res = (|| -> Result<cpal::Stream, String> {
            let host = cpal::default_host();
            let chosen = if mic && !mic_name.is_empty() {
                let d = host.input_devices().ok().and_then(|mut it| it.find(|d| d.name().map_or(false, |n| n == mic_name)));
                if d.is_none() {
                    log_line(&format!("audio: mic '{mic_name}' not found, using the {} default", if cfg!(windows) { "Windows" } else { "system" }));
                }
                d
            } else {
                None
            };
            let dev = match chosen {
                Some(d) => Some(d),
                None if mic => host.default_input_device(),
                None => host.default_output_device(),
            }
            .ok_or("no device")?;
            CAP_DEV.lock().unwrap()[mic as usize] = dev.name().unwrap_or_default();
            let cfg = if mic { dev.default_input_config() } else { dev.default_output_config() }.map_err(err)?;
            log_line(&format!("audio: {what} '{}' {:?} {}Hz {}ch",
                dev.name().unwrap_or_default(), cfg.sample_format(), cfg.sample_rate().0, cfg.channels()));
            let (in_rate, cin, cout) = (cfg.sample_rate().0, cfg.channels() as usize, ch as usize);
            if in_rate != rate {
                log_line(&format!("audio: {what} resampling {in_rate} -> {rate} Hz"));
            }
            let errf = move |e: cpal::StreamError| {
                log_line(&format!("audio: {what} stream error: {e}"));
                // headset unplugged / wireless dongle off: this source stays silent until a restart picks the
                // (new) default device
                if matches!(e, cpal::StreamError::DeviceNotAvailable) {
                    ask_restart(&format!("audio: {what} device disappeared"));
                }
            };
            let b = buf.clone();
            let mut rs = Resampler::new(in_rate, rate, cin, cout);
            let mut tmp: Vec<f32> = Vec::new();
            let s = match cfg.sample_format() {
                cpal::SampleFormat::F32 => dev.build_input_stream(
                    &cfg.config(),
                    move |d: &[f32], _: &cpal::InputCallbackInfo| rs.push(d, &mut b.lock().unwrap()),
                    errf, None),
                cpal::SampleFormat::I16 => dev.build_input_stream(
                    &cfg.config(),
                    move |d: &[i16], _: &cpal::InputCallbackInfo| {
                        tmp.clear();
                        tmp.extend(d.iter().map(|s| *s as f32 / 32768.0));
                        rs.push(&tmp, &mut b.lock().unwrap());
                    },
                    errf, None),
                cpal::SampleFormat::I32 => dev.build_input_stream(
                    &cfg.config(),
                    move |d: &[i32], _: &cpal::InputCallbackInfo| {
                        tmp.clear();
                        tmp.extend(d.iter().map(|s| *s as f32 / 2147483648.0));
                        rs.push(&tmp, &mut b.lock().unwrap());
                    },
                    errf, None),
                f => return Err(format!("unsupported sample format {f:?}")),
            }
            .map_err(err)?;
            s.play().map_err(err)?;
            Ok(s)
        })();
        match res {
            Ok(_stream) => {
                log_line(&format!("audio: {what} capture running"));
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
            Err(e) => log_line(&format!("audio: {what} unavailable: {e}")),
        }
    });
}

/// One per-app capture (a process tree) feeding its own queue.
struct AppSrc {
    pid: u32,
    buf: Buf,
    stop: Arc<AtomicBool>,
}
/// All captures of one app track (e.g. several browsers on the Browser track).
type Group = Arc<Mutex<Vec<AppSrc>>>;

/// Extra tracks, in file order after mix/desktop/mic: a:3 game, a:4 Discord, a:5 browsers, a:6 music apps
/// (a:6 since chat 19 - older clips have 6 tracks, the editor and self-test handle both).
pub const APP_TRACKS: [&str; 4] = ["Game", "Discord", "Browser", "Music"];

#[allow(dead_code)]
const DISCORD_EXES: &[&str] = &["discord.exe", "discordcanary.exe", "discordptb.exe"];
#[allow(dead_code)]
const BROWSER_EXES: &[&str] = &["chrome.exe", "brave.exe", "msedge.exe", "firefox.exe", "opera.exe", "opera_gx.exe", "vivaldi.exe", "librewolf.exe", "zen.exe"];
/// music / media players (Spotify etc.): their own track, so music can be cut out or kept in the editor
#[allow(dead_code)]
const MUSIC_EXES: &[&str] = &["spotify.exe", "applemusic.exe", "itunes.exe", "tidal.exe", "deezer.exe", "amazon music.exe",
    "musicbee.exe", "foobar2000.exe", "aimp.exe", "winamp.exe", "vlc.exe", "youtube music.exe", "youtube music desktop app.exe"];

/// Every ~2 s: works out which processes are the game / Discord / browsers and starts or
/// stops a per-app loopback capture for each (whole process tree).
#[cfg(windows)]
fn app_supervisor(groups: [Group; 4], stop: Arc<AtomicBool>, rate: u32, ch: u16) {
    std::thread::spawn(move || {
        let mut game: Option<u32> = None;
        let mut failed: std::collections::HashSet<u32> = Default::default();
        while !stop.load(Ordering::Relaxed) {
            let procs = crate::winutil::processes();
            let alive = |pid: u32| procs.iter().any(|p| p.0 == pid);
            // game: the game in front; kept while it runs and no other game takes over
            if let Some((pid, stem, title)) = crate::winutil::foreground_pid() {
                if pid != std::process::id() && game_name(&stem, &title, crate::winutil::foreground_fullscreen()).is_some() {
                    game = Some(pid);
                }
            }
            if game.is_some_and(|g| !alive(g)) {
                game = None;
            }
            // root processes of a set of exes (child processes are included by the capture)
            let roots = |set: &[&str]| -> Vec<u32> {
                procs.iter()
                    .filter(|p| set.contains(&p.2.as_str()))
                    .filter(|p| !procs.iter().any(|q| q.0 == p.1 && set.contains(&q.2.as_str())))
                    .map(|p| p.0)
                    .collect()
            };
            let mut want: [Vec<u32>; 4] = [game.into_iter().collect(), roots(DISCORD_EXES), roots(BROWSER_EXES), roots(MUSIC_EXES)];
            // a music app that is also the game in front (unlikely) stays on the Game track only
            let g0 = want[0].clone();
            want[3].retain(|p| !g0.contains(p));
            for (k, g) in groups.iter().enumerate() {
                let mut list = g.lock().unwrap();
                list.retain(|s| {
                    let keep = want[k].contains(&s.pid) && !s.stop.load(Ordering::Relaxed);
                    if !keep {
                        s.stop.store(true, Ordering::Relaxed);
                    }
                    keep
                });
                for &pid in &want[k] {
                    if list.iter().any(|s| s.pid == pid) || failed.contains(&pid) {
                        continue;
                    }
                    let src = AppSrc { pid, buf: Arc::new(Mutex::new(VecDeque::new())), stop: Arc::new(AtomicBool::new(false)) };
                    let (b, st, all) = (src.buf.clone(), src.stop.clone(), stop.clone());
                    let name = procs.iter().find(|p| p.0 == pid).map(|p| p.2.clone()).unwrap_or_default();
                    let what = APP_TRACKS[k];
                    log_line(&format!("audio: {what} track <- {name} (pid {pid})"));
                    std::thread::spawn(move || {
                        let st2 = st.clone();
                        let r = crate::winutil::app_loopback(pid, rate, ch, st.clone(), |x| {
                            if all.load(Ordering::Relaxed) { st2.store(true, Ordering::Relaxed); }
                            b.lock().unwrap().extend(x.iter().copied());
                        });
                        if let Err(e) = r {
                            log_line(&format!("audio: {what} capture of pid {pid} failed: {e}"));
                        }
                        st.store(true, Ordering::Relaxed); // supervisor drops it
                    });
                    list.push(src);
                }
                // a capture that ended by itself (error) is not retried for the same pid
                for s in list.iter() {
                    if s.stop.load(Ordering::Relaxed) {
                        failed.insert(s.pid);
                    }
                }
            }
            for _ in 0..20 {
                if stop.load(Ordering::Relaxed) { break; }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        for g in &groups {
            for s in g.lock().unwrap().iter() {
                s.stop.store(true, Ordering::Relaxed);
            }
        }
    });
}

/// Linux: the same 4 app tracks from PipeWire / PulseAudio. Every ~2 s the playback streams ("sink inputs")
/// are listed with pactl and sorted onto the tracks by their app (binary / name / process); each one is
/// recorded on its own with `parec --monitor-stream=<index>` (only listens - what you hear is unchanged).
/// AppSrc.pid holds the stream index here.
#[cfg(target_os = "linux")]
fn app_supervisor(groups: [Group; 4], stop: Arc<AtomicBool>, rate: u32, ch: u16) {
    std::thread::spawn(move || {
        let mut said_missing = false;
        let mut logged: std::collections::HashSet<u32> = Default::default();
        while !stop.load(Ordering::Relaxed) {
            let streams = linux_sink_inputs();
            if streams.is_none() && !said_missing {
                said_missing = true;
                log_line("audio: per-app tracks need pactl + parec (sudo dnf install pulseaudio-utils)");
            }
            let streams = streams.unwrap_or_default();
            // the game and everything it started (Wine / Proton games play sound from their own process)
            let game_pids: Vec<u32> = match linux_game() { Some((_, pid, _)) => linux_process_tree(pid), None => Vec::new() };
            let mut want: [Vec<u32>; 4] = Default::default();
            for s in &streams {
                let k = if s.pid.is_some_and(|p| game_pids.contains(&p)) { 0 } else if let Some(k) = linux_app_group(&s.binary, &s.name) { k } else { continue };
                want[k].push(s.index);
                if logged.insert(s.index) {
                    log_line(&format!("audio: {} track <- {} (stream {}{})", APP_TRACKS[k], if s.binary.is_empty() { &s.name } else { &s.binary }, s.index,
                        s.pid.map(|p| format!(", pid {p}")).unwrap_or_default()));
                }
            }
            for (k, g) in groups.iter().enumerate() {
                let mut list = g.lock().unwrap();
                list.retain(|s| {
                    let keep = want[k].contains(&s.pid) && !s.stop.load(Ordering::Relaxed);
                    if !keep { s.stop.store(true, Ordering::Relaxed); }
                    keep
                });
                for &idx in &want[k] {
                    if list.iter().any(|s| s.pid == idx) { continue; }
                    let src = AppSrc { pid: idx, buf: Arc::new(Mutex::new(VecDeque::new())), stop: Arc::new(AtomicBool::new(false)) };
                    let (b, st, all) = (src.buf.clone(), src.stop.clone(), stop.clone());
                    std::thread::spawn(move || linux_stream_capture(idx, b, st, all, rate, ch));
                    list.push(src);
                }
            }
            for _ in 0..20 {
                if stop.load(Ordering::Relaxed) { break; }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        for g in &groups {
            for s in g.lock().unwrap().iter() { s.stop.store(true, Ordering::Relaxed); }
        }
    });
}

#[cfg(not(any(windows, target_os = "linux")))]
fn app_supervisor(_: [Group; 4], _: Arc<AtomicBool>, _: u32, _: u16) {}

#[cfg(target_os = "linux")]
struct LinuxStream { index: u32, pid: Option<u32>, binary: String, name: String }

/// Playback streams from `pactl list sink-inputs` (text output: works with PulseAudio and pipewire-pulse,
/// old and new pactl). None = pactl missing / no sound server.
#[cfg(target_os = "linux")]
fn linux_sink_inputs() -> Option<Vec<LinuxStream>> {
    let o = sys_cmd("pactl").env("LC_ALL", "C").args(["list", "sink-inputs"]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !o.status.success() { return None; }
    Some(parse_sink_inputs(&String::from_utf8_lossy(&o.stdout)))
}

#[cfg(target_os = "linux")]
fn parse_sink_inputs(text: &str) -> Vec<LinuxStream> {
    let mut out: Vec<LinuxStream> = Vec::new();
    // properties look like:   application.process.binary = "firefox"
    let prop = |l: &str, key: &str| -> Option<String> {
        let rest = l.trim().strip_prefix(key)?.trim_start().strip_prefix('=')?.trim();
        Some(rest.trim_matches('"').to_string())
    };
    for l in text.lines() {
        if let Some(n) = l.trim().strip_prefix("Sink Input #") {
            if let Ok(index) = n.trim().parse() { out.push(LinuxStream { index, pid: None, binary: String::new(), name: String::new() }); }
            continue;
        }
        let Some(s) = out.last_mut() else { continue };
        if let Some(v) = prop(l, "application.process.id") { s.pid = v.parse().ok(); }
        else if let Some(v) = prop(l, "application.process.binary") { s.binary = v; }
        else if let Some(v) = prop(l, "application.name") { s.name = v; }
    }
    // Echo's own sounds (save / bookmark beeps) and capture helpers are never an app track
    out.retain(|s| !matches!(s.binary.as_str(), "echo" | "echo-clips" | "parec" | "pw-play" | "paplay"));
    out
}

/// Which app track a stream belongs on (1 Discord, 2 browsers, 3 music), from its binary or app name.
#[cfg(target_os = "linux")]
fn linux_app_group(binary: &str, name: &str) -> Option<usize> {
    let b = binary.to_lowercase();
    let b = b.trim_end_matches(".exe");
    let n = name.to_lowercase();
    const DISCORD: &[&str] = &["discord", "discordcanary", "discordptb", "discord-canary", "discord-ptb", "vesktop", "webcord", "legcord", "armcord", "equibop"];
    const BROWSER: &[&str] = &["firefox", "firefox-bin", "firefox-esr", "chrome", "google-chrome", "chromium", "chromium-browser", "brave", "brave-browser",
        "msedge", "microsoft-edge", "opera", "vivaldi", "vivaldi-bin", "librewolf", "zen", "zen-bin", "floorp", "waterfox", "thorium"];
    const MUSIC: &[&str] = &["spotify", "tidal-hifi", "deezer", "vlc", "rhythmbox", "elisa", "strawberry", "clementine", "audacious", "amberol",
        "lollypop", "youtube-music", "youtube-music-desktop-app", "cider", "feishin", "spotube", "tauon", "quodlibet", "musicbee", "foobar2000", "aimp"];
    if DISCORD.contains(&b) || n.contains("discord") || n == "vesktop" || n == "webrtc voiceengine" && b.contains("discord") { return Some(1); }
    if BROWSER.contains(&b) || ["firefox", "chromium", "google chrome", "brave", "microsoft edge", "vivaldi", "librewolf", "zen"].iter().any(|x| n == *x) { return Some(2); }
    if MUSIC.contains(&b) || ["spotify", "vlc media player", "rhythmbox", "elisa", "strawberry", "tidal", "deezer", "cider", "amberol"].iter().any(|x| n == *x) { return Some(3); }
    None
}

/// `pid` and all its child processes (from /proc/<pid>/stat parent ids).
#[cfg(target_os = "linux")]
fn linux_process_tree(pid: u32) -> Vec<u32> {
    let mut parent: Vec<(u32, u32)> = Vec::new();
    if let Ok(rd) = fs::read_dir("/proc") {
        for e in rd.flatten() {
            let Ok(p) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
            let Ok(st) = fs::read_to_string(e.path().join("stat")) else { continue };
            // "pid (comm) state ppid ..." - comm can contain spaces / brackets: read after the last ')'
            let Some(rest) = st.rfind(')').map(|i| &st[i + 1..]) else { continue };
            if let Some(pp) = rest.split_whitespace().nth(1).and_then(|x| x.parse().ok()) { parent.push((p, pp)); }
        }
    }
    let mut tree = vec![pid];
    let mut i = 0;
    while i < tree.len() && tree.len() < 4096 {
        let p = tree[i];
        for &(c, pp) in &parent { if pp == p && !tree.contains(&c) { tree.push(c); } }
        i += 1;
    }
    tree
}

/// Records one playback stream (sink input) into `buf` until it's not wanted or the stream ends.
#[cfg(target_os = "linux")]
fn linux_stream_capture(idx: u32, buf: Buf, st: Arc<AtomicBool>, all: Arc<AtomicBool>, rate: u32, ch: u16) {
    use std::io::Read;
    let mut child = match sys_cmd("parec")
        .args([&format!("--monitor-stream={idx}"), "--format=float32le", &format!("--rate={rate}"), &format!("--channels={ch}"), "--raw", "--latency-msec=20",
            "--client-name=Echo", "--stream-name=Echo app track"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
    {
        Ok(c) => c,
        Err(e) => { log_line(&format!("audio: could not record stream {idx}: {e}")); st.store(true, Ordering::Relaxed); return; }
    };
    let mut out = child.stdout.take().unwrap();
    // a reader thread, so a quiet stream (no data) can still be stopped at once
    let (b2, st2) = (buf.clone(), st.clone());
    let reader = std::thread::spawn(move || {
        let mut b = vec![0u8; 4096 * 4];
        let mut rest: Vec<u8> = Vec::new();
        loop {
            let n = match out.read(&mut b) { Ok(0) | Err(_) => break, Ok(n) => n };
            rest.extend_from_slice(&b[..n]);
            let whole = rest.len() / 4 * 4;
            {
                let mut q = b2.lock().unwrap();
                for c in rest[..whole].chunks_exact(4) { q.push_back(f32::from_le_bytes([c[0], c[1], c[2], c[3]])); }
                // nobody reading (should not happen): keep at most ~2 s
                let cap = rate as usize * ch as usize * 2;
                if q.len() > cap { let d = q.len() - cap; q.drain(..d); }
            }
            rest.drain(..whole);
        }
        st2.store(true, Ordering::Relaxed);   // stream ended: the supervisor drops it
    });
    while !st.load(Ordering::Relaxed) && !all.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
    }
    st.store(true, Ordering::Relaxed);
    let _ = child.kill();
    let _ = child.wait();
    let _ = reader.join();
}

/// Mixes desktop + mic and writes to ffmpeg at exactly real-time pace,
/// padding with silence when nothing is playing (keeps A/V in sync).
/// Tracks per frame: mix, desktop, mic (+ game, Discord, browsers when `apps`).
#[allow(clippy::too_many_arguments)]
/// mic gate: hotkey mute + push-to-talk (live, read by the audio thread)
/// cloud download progress: (rel, bytes so far, total)
/// cloud downloads running now: rel -> (bytes so far, total); also stops the same clip downloading twice at once
static DL_PROGRESS: std::sync::LazyLock<Mutex<std::collections::HashMap<String, (u64, u64)>>> = std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));
/// editor export progress in 1/1000 (polled by the UI)
pub static EXPORT_PROGRESS: AtomicU32 = AtomicU32::new(0);
/// cloud upload in progress, for the UI: rel ("" = none), bytes sent, total, which one (1-based) of how many
#[derive(Serialize, Clone, Default)]
pub struct UpProgress {
    pub rel: String,
    /// where it's going ("Google Drive", "Echo Cloud"…), shown in the progress box
    pub dest: String,
    pub got: u64,
    pub total: u64,
    pub idx: u32,
    pub count: u32,
}
pub static UP_PROGRESS: Mutex<UpProgress> = Mutex::new(UpProgress { rel: String::new(), dest: String::new(), got: 0, total: 0, idx: 0, count: 0 });
/// "Cancel" on the upload box: stops the running upload (rclone is stopped, an Echo Cloud upload is aborted)
pub static UP_CANCEL: AtomicBool = AtomicBool::new(false);
/// clips whose upload was cancelled: the background sync leaves them alone until Echo restarts
/// (uploading one by hand takes it off the list)
pub static UP_SKIPPED: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// a cloud's name for people ("Google Drive", "Echo Cloud"…)
pub fn cloud_name(provider: &str) -> &'static str {
    match provider {
        "echocloud" => "Echo Cloud", "gdrive" => "Google Drive", "pcloud" => "pCloud", "mega" => "MEGA", "filen" => "Filen",
        "webdav" => "your WebDAV server", "sftp" => "your SFTP server", "s3" => "your S3 storage", "folder" => "your folder / NAS",
        "onedrive" => "OneDrive", "dropbox" => "Dropbox",
        _ => "the cloud",
    }
}
pub static MIC_MUTED: AtomicBool = AtomicBool::new(false);
/// set when the audio clock and the wall clock (video timestamps) split apart - sleep / hibernate, a big
/// clock change, or a long stall. Clips would come out hours long with broken sound; the watch loop restarts the recorder.
pub static RESTART_REC: AtomicBool = AtomicBool::new(false);
/// where ffmpeg's part of the log starts for the running recorder (health(): new error lines after it)
/// unix ms of the audio thread's last finished write to ffmpeg (0 = not running). A write that never
/// returns = ffmpeg stopped reading our audio (then every clip is silent / unsaveable).
static AUDIO_WROTE_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn now_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
static REC_STARTED_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static REC_LOG_POS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// unix time of the last restart asked for by a capture thread (at most one per minute: no restart loops)
static LAST_SELF_RESTART: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// A capture part broke while ffmpeg keeps running (GPU capture ended, audio device gone): restart once a minute at most.
fn ask_restart(why: &str) {
    let now = now_unix();
    if now.saturating_sub(LAST_SELF_RESTART.load(Ordering::Relaxed)) < 60 {
        log_line(&format!("{why} - not restarting again within a minute"));
        return;
    }
    LAST_SELF_RESTART.store(now, Ordering::Relaxed);
    log_line(&format!("{why} - restarting the recorder"));
    RESTART_REC.store(true, Ordering::Relaxed);
}
/// live mixer settings: per-app volume (f32 bits) and "mix only these apps"
static APP_VOL: [AtomicU32; 4] = [AtomicU32::new(0x3f80_0000), AtomicU32::new(0x3f80_0000), AtomicU32::new(0x3f80_0000), AtomicU32::new(0x3f80_0000)];
static MIX_APPS: AtomicBool = AtomicBool::new(false);
static MIX_INC: [AtomicBool; 4] = [AtomicBool::new(true), AtomicBool::new(true), AtomicBool::new(false), AtomicBool::new(false)];
static MIC_MONO: AtomicBool = AtomicBool::new(false);
/// the mic is added to the mix by ffmpeg (after the noise filter), not here
static MIX_NO_MIC: AtomicBool = AtomicBool::new(false);
/// names the user said are not games (Settings > Games > remove)
static NOT_GAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());
fn set_mixer(c: &Settings) {
    *NOT_GAMES.lock().unwrap() = c.not_games.iter().map(|n| n.to_lowercase()).collect();
    for (i, v) in [c.vol_game, c.vol_discord, c.vol_browser, c.vol_music].iter().enumerate() {
        APP_VOL[i].store(v.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
    }
    MIX_APPS.store(c.mix_mode == "apps" && c.app_tracks, Ordering::Relaxed);
    for (i, b) in [c.mix_game, c.mix_discord, c.mix_browser, c.mix_music].iter().enumerate() {
        MIX_INC[i].store(*b, Ordering::Relaxed);
    }
    MIC_MONO.store(c.mic_mono, Ordering::Relaxed);
}
pub static PTT_ON: AtomicBool = AtomicBool::new(false);
pub static PTT_HELD: AtomicBool = AtomicBool::new(false);

/// RNNoise model for the mic noise filter (downloaded once); None = use ffmpeg's afftdn instead.
fn denoise_model() -> Option<PathBuf> {
    let p = app_dir().join("rnnoise-sh.rnnn");
    if fs::metadata(&p).map_or(false, |m| m.len() > 1000) {
        return Some(p);
    }
    let _ = fs::create_dir_all(app_dir());
    match download_checked("https://raw.githubusercontent.com/GregorR/rnnoise-models/master/somnolent-hogwash-2018-09-01/sh.rnnn", &p,
        "sha256:70bb6685eb0c2a1d18e2918dca3fbfbd39317010b1802eb1b6ea73a92f3fdec0") {
        Ok(()) => Some(p),
        Err(e) => { log_line(&format!("audio: noise model download failed ({e}), using afftdn")); None }
    }
}

fn audio_thread(mut out: ChildStdin, stop: Arc<AtomicBool>, rate: u32, ch: u16, start: Instant,
                desktop: bool, mic: bool, mic_name: String, vols: Arc<[AtomicU32; 2]>, apps: bool) {
    let bd: Buf = Arc::new(Mutex::new(VecDeque::new()));
    let bm: Buf = Arc::new(Mutex::new(VecDeque::new()));
    if desktop {
        spawn_capture(false, String::new(), bd.clone(), stop.clone(), rate, ch);
    }
    if mic {
        spawn_capture(true, mic_name.clone(), bm.clone(), stop.clone(), rate, ch);
    }
    let groups: [Group; 4] = Default::default();
    if apps {
        app_supervisor(groups.clone(), stop.clone(), rate, ch);
    }
    let ch = ch as usize;
    let max_buffered = rate as usize * 3 / 10 * ch; // keep at most 300 ms queued (whole frames)
    let mut last_slow_log = Instant::now() - Duration::from_secs(60);
    let mut written: u64 = 0; // frames
    let mut gain = 1.0f32; // mic gate, ramped to avoid clicks
    let mut primed: std::collections::HashMap<usize, bool> = std::collections::HashMap::new();
    let prime = rate as usize * ch as usize * 40 / 1000; // 40 ms
    // video is stamped with the wall clock, audio is paced by `start` (monotonic): they must stay together
    let wall0 = SystemTime::now();
    let mono0 = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(10));
        let target = (start.elapsed().as_secs_f64() * rate as f64) as u64;
        let behind = target.saturating_sub(written);
        let wall = SystemTime::now().duration_since(wall0).map(|d| d.as_secs_f64()).unwrap_or(-1e9);
        let drift = wall - mono0.elapsed().as_secs_f64();
        // A real clock jump / PC sleep: wall clock and monotonic clock disagree, or we're absurdly far behind
        // (sleep). ffmpeg only being busy for a few seconds (heavy game on a mid-range PC: 2-9 s stalls,
        // 1660 Super friend, chat 18) is NOT one: restarting threw the replay buffer away every minute. Then
        // the catch-up below writes the missed time (silence) so audio stays in sync with the video.
        if drift.abs() > 3.0 || behind > rate as u64 * 30 {
            log_line(&format!("audio: clock jump (PC sleep / clock change?): wall clock {drift:+.1} s, audio {:.1} s behind - restarting the recorder",
                behind as f64 / rate as f64));
            RESTART_REC.store(true, Ordering::Relaxed);
            return;
        }
        let n = (behind as usize) * ch;
        if n > rate as usize * ch / 6 && last_slow_log.elapsed() > Duration::from_secs(10) {
            // writing to ffmpeg took long (it read our audio slowly) - only logged, handled below
            log_line(&format!("audio: writer fell behind by {} ms", n / ch * 1000 / rate as usize));
            last_slow_log = Instant::now();
        }
        let vd = f32::from_bits(vols[0].load(Ordering::Relaxed));
        let vm = f32::from_bits(vols[1].load(Ordering::Relaxed));
        // jitter buffer: Windows hands audio over in bursts. Reading it the moment it arrives left tiny gaps
        // (choppy sound right after a start until it settled). A source plays once ~40 ms are queued;
        // if it runs dry it waits for 40 ms again instead of stuttering sample by sample.
        let take = |q: &Buf, v: f32, into: &mut [f32], primed: &mut std::collections::HashMap<usize, bool>| {
            let key = Arc::as_ptr(q) as usize;
            let mut q = q.lock().unwrap();
            while q.len() > max_buffered { q.pop_front(); }
            let p = primed.entry(key).or_insert(false);
            if !*p {
                if q.len() < prime.min(max_buffered / 2) {
                    return; // still filling: silence
                }
                *p = true;
            }
            // play what is queued (a big catch-up after a slow write is filled at its end, so the newest
            // sound lines up with "now"); only when the queue runs completely dry wait for 40 ms again
            let k = q.len().min(into.len());
            let at = into.len() - k;
            for x in into[at..].iter_mut() { *x += q.pop_front().unwrap_or(0.0) * v; }
            if q.is_empty() {
                *p = false;
            }
        };
        let mut d = vec![0.0f32; n];
        let mut m = vec![0.0f32; n];
        take(&bd, vd, &mut d, &mut primed);
        take(&bm, vm, &mut m, &mut primed);
        let open = !MIC_MUTED.load(Ordering::Relaxed) && (!PTT_ON.load(Ordering::Relaxed) || PTT_HELD.load(Ordering::Relaxed));
        let want = if open { 1.0f32 } else { 0.0 };
        let step = 50.0 / rate as f32; // ~20 ms ramp
        for f in 0..(n / ch) {
            gain = if gain < want { (gain + step).min(want) } else { (gain - step).max(want) };
            if gain < 1.0 {
                for i in f * ch..f * ch + ch { m[i] *= gain; }
            }
        }
        if MIC_MONO.load(Ordering::Relaxed) && ch == 2 {
            for f in m.chunks_exact_mut(2) {
                let v = (f[0] + f[1]).clamp(-1.0, 1.0);
                f[0] = v;
                f[1] = v;
            }
        }
        let mut g: Vec<Vec<f32>> = Vec::new();
        if apps {
            for (gi, grp) in groups.iter().enumerate() {
                let mut a = vec![0.0f32; n];
                let av = vd * f32::from_bits(APP_VOL[gi].load(Ordering::Relaxed));
                for src in grp.lock().unwrap().iter() {
                    take(&src.buf, av, &mut a, &mut primed);
                }
                g.push(a);
            }
        }
        let tracks = 3 + g.len();
        let mut chunk = Vec::with_capacity(n * tracks * 4);
        // NaN/Inf from a capture source must never reach ffmpeg: its AAC encoder stops for good on them
        // ("Input contains (near) NaN/+-Inf", the whole recording dies). clamp() lets NaN through.
        let put = |chunk: &mut Vec<u8>, x: f32| chunk.extend_from_slice(&(if x.is_finite() { x.clamp(-1.0, 1.0) } else { 0.0 }).to_le_bytes());
        let mix_apps = MIX_APPS.load(Ordering::Relaxed);
        let no_mic = MIX_NO_MIC.load(Ordering::Relaxed);
        let inc = [MIX_INC[0].load(Ordering::Relaxed), MIX_INC[1].load(Ordering::Relaxed), MIX_INC[2].load(Ordering::Relaxed), MIX_INC[3].load(Ordering::Relaxed)];
        for f in 0..(n / ch) {
            let r = f * ch..f * ch + ch;
            // mix: everything, or only the apps ticked in Settings > Audio (+ mic)
            if mix_apps && g.len() == 4 {
                for i in r.clone() {
                    let mut x = if no_mic { 0.0 } else { m[i] };
                    for (gi, a) in g.iter().enumerate() {
                        if inc[gi] { x += a[i]; }
                    }
                    put(&mut chunk, x);
                }
            } else {
                for i in r.clone() { put(&mut chunk, if no_mic { d[i] } else { d[i] + m[i] }); }
            }
            for i in r.clone() { put(&mut chunk, d[i]); }
            for i in r.clone() { put(&mut chunk, m[i]); }
            for a in &g {
                for i in r.clone() { put(&mut chunk, a[i]); }
            }
        }
        if out.write_all(&chunk).is_err() {
            break; // ffmpeg exited
        }
        AUDIO_WROTE_AT.store(now_ms(), Ordering::Relaxed);
        written = target;
    }
}

/// The window of the VR game running right now (0 = none), set by main's game loop. Recorded even in "whole
/// screen" mode, and never with the mouse pointer: the desktop pointer sits on the VR mirror window although
/// the player never sees it (chat 21, Blade & Sorcery clip had a pointer in the middle)
#[allow(dead_code)]
pub static VR_WINDOW: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// Starts the zero-copy capture thread; returns ffmpeg input args + pipe name.
#[cfg(windows)]
fn gpu_input(i: usize, stop: &Arc<AtomicBool>, cfg: &Settings, window: isize) -> Option<(Vec<String>, String)> {
    let name = format!(r"\\.\pipe\clipper_v_{}_{}_{}", std::process::id(), i,
        SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0));
    let pipe = match crate::gpucap::create_pipe(&name) {
        Ok(p) => p,
        Err(e) => {
            log_line(&format!("gpu: {e}"));
            return None;
        }
    };
    let s = stop.clone();
    let vr = VR_WINDOW.load(Ordering::Relaxed);
    if window != 0 && window == vr {
        log_line("capture: VR game window, without the mouse pointer");
    }
    let opts = crate::gpucap::Opts {
        fps: cfg.fps,
        bitrate_mbps: cfg.bitrate_mbps,
        gop: cfg.fps * SEG,
        target_h: cfg.resolution.parse().unwrap_or(0),
        monitor: cfg.monitor.clone(),
        codec: cfg.codec.clone(),
        window: if cfg.capture == "game" || (window != 0 && window == vr) { window } else { 0 },
        smart_bitrate: cfg.smart_bitrate,
        smart_quality: cfg.smart_quality,
        cursor: cfg.capture_cursor && !(window != 0 && window == vr),
    };
    let fps = cfg.fps;
    std::thread::spawn(move || {
        let began = Instant::now();
        crate::gpucap::run(pipe, s.clone(), opts, log_line);
        // ended without being stopped (encoder error, capture failed): ffmpeg would go on with audio only.
        // Only after it ran a while: a failure during start() is handled there (next mode / CPU fallback).
        if !s.load(Ordering::Relaxed) && began.elapsed() > Duration::from_secs(10) {
            ask_restart("gpu: capture ended unexpectedly");
        }
    });
    let args = vec![
        "-thread_queue_size".into(), "1024".into(),
        "-use_wallclock_as_timestamps".into(), "1".into(),
        "-probesize".into(), "1000000".into(), "-analyzeduration".into(), "0".into(),
        "-f".into(), match cfg.codec.as_str() { "hevc" => "hevc", "av1" => "obu", _ => "h264" }.into(), "-framerate".into(), fps.to_string(),
        "-i".into(), name.clone(),
        "-map".into(), "1:v".into(),
        "-c:v".into(), "copy".into(),
    ];
    Some((args, name))
}

/// gpu-screen-recorder runs constant bitrate: with smart bitrate on, cfg.bitrate_mbps is the Windows encoder's
/// MAXIMUM (average / 0.6 etc.), so Linux records at the average instead (before: 33 Mbps CBR for "20 avg")
#[cfg(target_os = "linux")]
fn linux_kbps(cfg: &Settings) -> u32 {
    // gpu-screen-recorder runs a CONSTANT bitrate: it can't go up in busy scenes like Windows' smart bitrate does,
    // so the old "60-90 % of the maximum" (12 Mbps for 20) starved fast motion / foliage = blocky clips (chat 24).
    // Linux records at the full bitrate the user picked.
    let _ = (cfg.smart_bitrate, cfg.smart_quality);
    (cfg.bitrate_mbps * 1000).max(1000) as u32
}

/// Linux: gpu-screen-recorder captures the screen and encodes on the GPU (VAAPI),
/// writes MPEG-TS into a FIFO, ffmpeg copies it into the same segment buffer as on Windows.
#[cfg(target_os = "linux")]
fn gpu_input(i: usize, stop: &Arc<AtomicBool>, cfg: &Settings, _window: isize) -> Option<(Vec<String>, String)> {
    let _ = sys_cmd("pkill").args(["-f", "gpu-screen-recorder.*clipper_v_"]).status();
    // a new name every start: the previous run's clean-up (below) deleted the pipe of the NEXT start when both had
    // the same name ("Error opening input … No such file" right after a restart)
    static RUN: AtomicU32 = AtomicU32::new(0);
    let name = std::env::temp_dir().join(format!("clipper_v_{}_{}_{}.ts", std::process::id(), i, RUN.fetch_add(1, Ordering::Relaxed)));
    let _ = fs::remove_file(&name);
    let ok = sys_cmd("mkfifo").arg(&name).status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        log_line("gsr: could not create the video pipe (mkfifo)");
        return None;
    }
    let codec = match cfg.codec.as_str() { "hevc" => "hevc", "av1" => "av1", _ => "h264" };
    // Flatpak: no KMS capture (needs root rights the sandbox can't have) - the desktop's screen-share portal
    // instead (KDE/GNOME ask once which screen; "-restore-portal-session" remembers the answer)
    let target: Vec<String> = if crate::linuxutil::in_flatpak() {
        vec!["-w".into(), "portal".into(), "-restore-portal-session".into(), "yes".into()]
    } else { vec!["-w".into(), "screen".into()] };
    let mut a: Vec<String> = target;
    a.extend::<Vec<String>>(vec![
        "-f".into(), cfg.fps.to_string(), "-k".into(), codec.into(),
        "-c".into(), "mpegts".into(), "-bm".into(), "cbr".into(), "-q".into(), linux_kbps(cfg).to_string(),
        "-keyint".into(), SEG.to_string(), "-cursor".into(), if cfg.capture_cursor { "yes" } else { "no" }.into(), "-fm".into(), "cfr".into(),
        // no "update fps: 61, damage fps: 61" line every second: it filled the log and pushed out everything else
        "-v".into(), "no".into(),
    ]);
    if let Ok(h) = cfg.resolution.parse::<u32>() {
        a.extend(["-s".into(), format!("{}x{h}", h * 16 / 9)]);
    }
    a.extend(["-o".into(), name.to_string_lossy().into_owned()]);
    log_line(&format!("gsr: gpu-screen-recorder {}", a.join(" ")));
    let log = fs::OpenOptions::new().create(true).append(true).open(log_path()).ok();
    let mut cmd = sys_cmd("gpu-screen-recorder");
    cmd.args(&a).stdin(Stdio::null()).stdout(Stdio::null());
    if let Some(l) = log { cmd.stderr(l); }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            log_line(&format!("gsr: could not start gpu-screen-recorder (sudo dnf install gpu-screen-recorder): {e}"));
            let _ = fs::remove_file(&name);
            return None;
        }
    };
    let (s, n2) = (stop.clone(), name.clone());
    std::thread::spawn(move || {
        loop {
            if s.load(Ordering::Relaxed) { break; }
            if let Ok(Some(st)) = child.try_wait() { log_line(&format!("gsr: exited ({st})")); break; }
            std::thread::sleep(Duration::from_millis(200));
        }
        let _ = sys_cmd("kill").args(["-INT", &child.id().to_string()]).status();
        for _ in 0..25 {
            if matches!(child.try_wait(), Ok(Some(_))) { break; }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
        let _ = fs::remove_file(&n2);
    });
    let args = vec![
        "-thread_queue_size".into(), "1024".into(),
        "-use_wallclock_as_timestamps".into(), "1".into(),
        "-probesize".into(), "2000000".into(), "-analyzeduration".into(), "1000000".into(),
        "-f".into(), "mpegts".into(),
        "-i".into(), name.to_string_lossy().into_owned(),
        "-map".into(), "1:v".into(),
        "-c:v".into(), "copy".into(),
        // Wall-clock stamps (arrival time from the pipe) jitter - big keyframes arrive late, which left a one-frame
        // hole after some keyframes = a hitch every 2 s (chat 24). gpu-screen-recorder runs at a constant frame
        // rate (-fm cfr), so keep the wall-clock time of the FIRST frame (A/V sync) and number the rest evenly.
        "-bsf:v".into(), format!("setts=ts=STARTPTS+N/({}*TB)", cfg.fps.max(1)),
    ];
    Some((args, name.to_string_lossy().into_owned()))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn gpu_input(_: usize, _: &Arc<AtomicBool>, _: &Settings, _: isize) -> Option<(Vec<String>, String)> {
    None
}

/// Filter tail for re-encodes: convert whatever came in to BT.709 TV-range yuv420p (players everywhere expect that).
const TV_YUV: &str = "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p";
/// Matching output tags (so Discord / Chrome / phones don't guess the colours).
const TV_TAGS: [&str; 8] = ["-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"];

/// Share of pixels outside TV range (16-235) in a few small grey frames: full-range videos
/// (real blacks) have lots of them, correctly made TV-range videos almost none.
fn full_range_share(p: &Path) -> f64 {
    let o = ffmpeg()
        .args(["-hide_banner", "-v", "error", "-i"]).arg(p)
        .args(["-an", "-vf", "framestep=60,scale=320:-2:flags=neighbor,format=gray", "-frames:v", "10", "-f", "rawvideo", "-"])
        .stdout(Stdio::piped()).stderr(Stdio::null()).output();
    let px = o.map(|o| o.stdout).unwrap_or_default();
    if px.is_empty() {
        return 0.0;
    }
    px.iter().filter(|&&b| b < 10 || b > 245).count() as f64 / px.len() as f64
}

/// Re-encode a full-range video (labelled or not) into real BT.709 TV range, all streams kept.
fn to_tv_range(src: &Path, out: &Path) -> bool {
    let vf = "scale=in_range=pc:out_range=tv:out_color_matrix=bt709,format=yuv420p";
    let run = |venc: &[&str]| -> bool {
        if skip_enc(venc) { return false; }
        ffmpeg().args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(src)
            .args(["-map", "0:v:0", "-map", "0:a?", "-vf", vf]).args(venc).args(TV_TAGS)
            .args(["-c:a", "copy", "-movflags", "+faststart", "-f", "mp4"]).arg(out)
            .stdout(Stdio::null()).stderr(Stdio::null()).status().map_or(false, |s| s.success())
            && fs::metadata(out).map_or(false, |m| m.len() > 0)
    };
    run(&["-c:v", "h264_nvenc", "-preset", "p5", "-cq", "20"])
        || run(&["-c:v", "libx264", "-preset", "veryfast", "-crf", "18"])
}

fn modified(p: &Path) -> Option<SystemTime> {
    fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl Recorder {
    pub fn new(cfg: Settings) -> Self {
        set_mixer(&cfg);
        ec_sync_state(&cfg, 0);
        let volumes = Arc::new([
            AtomicU32::new(cfg.desktop_volume.to_bits()),
            AtomicU32::new(cfg.mic_volume.to_bits()),
        ]);
        Self {
            child: Mutex::new(None),
            audio_stop: Mutex::new(Arc::new(AtomicBool::new(true))),
            cfg: Mutex::new(cfg),
            volumes,
            buf_dir: std::env::temp_dir().join("clipper_buffer"),
            mode: Mutex::new(String::new()),
            save_lock: Mutex::new(()),
            life: Mutex::new(()),
            idle_paused: AtomicBool::new(false),
            window: std::sync::atomic::AtomicIsize::new(0),
            pause_reason: Mutex::new(String::new()),
            session: Mutex::new(None),
            cloud_serve: Mutex::new(None),
            cloud_busy: Mutex::new(String::new()),
            cloud_lock: Mutex::new(()),
            last_game: Mutex::new(None),
        }
    }

    pub fn settings(&self) -> Settings {
        self.cfg.lock().unwrap().clone()
    }

    /// Apply new settings. Returns true if the recorder needs a restart.
    pub fn apply(&self, new: Settings) -> bool {
        self.volumes[0].store(new.desktop_volume.to_bits(), Ordering::Relaxed);
        self.volumes[1].store(new.mic_volume.to_bits(), Ordering::Relaxed);
        set_mixer(&new);
        let mut cur = self.cfg.lock().unwrap();
        let restart = cur.recorder_changed(&new);
        *cur = new;
        restart
    }

    pub fn clips_dir(&self) -> PathBuf {
        PathBuf::from(&self.cfg.lock().unwrap().clips_dir)
    }

    pub fn is_recording(&self) -> bool {
        match self.child.lock().unwrap().as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    pub fn start(&self) -> Result<(), String> {
        // Flatpak first start: the screen-share dialog waits for the Welcome screen + shortcut dialog
        // (main.rs starts recording once they're done)
        #[cfg(target_os = "linux")]
        if !crate::linuxutil::flatpak_may_record() { return Ok(()); }
        let _life = self.life.lock().unwrap_or_else(|e| e.into_inner());
        self.stop_inner();
        let cfg = self.settings();
        fs::create_dir_all(&cfg.clips_dir).map_err(err)?;
        // keep the previous session's log for troubleshooting - only on the first start of this Echo run:
        // recorder restarts (device changed, unhealthy, settings) keep writing to the same log, so the reason
        // for a restart isn't pushed out of it
        AUDIO_WROTE_AT.store(0, Ordering::Relaxed);
        static ROTATED: AtomicBool = AtomicBool::new(false);
        // (or when it grew past 5 MB, Echo running for weeks)
        if !ROTATED.swap(true, Ordering::Relaxed) || fs::metadata(log_path()).map_or(false, |m| m.len() > 5_000_000) {
            let _ = fs::rename(log_path(), log_path().with_extension("prev.log"));
            REC_LOG_POS.store(0, Ordering::Relaxed);
        }
        log_line(&format!("start: {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S")));
        ensure_ffmpeg();
        kill_leftovers();
        let (rate, ch) = loopback_format().unwrap_or((48000, 2));
        // Echo's tracks are mono or stereo. Surround speakers (virtual 7.1 headsets like the Logitech G733
        // report 8 channels) used to build "pan=stereo|c0=c0|…|c7=c7" -> ffmpeg refused, the recorder
        // couldn't start at all (1660 Super friend, chat 18). Record stereo; the capture mixes surround down.
        if ch > 2 {
            log_line(&format!("audio: speakers have {ch} channels (surround) - recording stereo"));
        }
        let ch = ch.min(2);
        #[cfg(windows)]
        let out_idx = crate::gpucap::output_index(&cfg.monitor);
        #[cfg(not(windows))]
        let out_idx = 0;
        let grab = format!("ddagrab=output_idx={out_idx}:framerate={}:draw_mouse={}", cfg.fps, cfg.capture_cursor as u8);
        let scale = match cfg.resolution.parse::<u32>() {
            Ok(h) => format!(",scale=-2:{h}"),
            Err(_) => String::new(),
        };
        // Mode "gpu": own zero-copy pipeline (gpucap.rs). Mode "cpu": ffmpeg ddagrab + CPU copy + AMF.
        // ffmpeg-only zero-copy routes all fail on this PC (scale_d3d11, AMF hwmap/vpp_amf, h264_mf).
        // CPU fallback tries each vendor's encoder: AMD, NVIDIA, Intel.
        #[cfg(windows)]
        let all = ["gpu", "cpu", "cpu-nvenc", "cpu-qsv"];
        #[cfg(not(windows))]
        let all = ["gpu"];
        let mode_file = std::env::temp_dir().join("clipper_mode_v7.txt");
        let last: usize = fs::read_to_string(&mode_file).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        // always try the zero-copy GPU path first; among the fallbacks, start with the one that worked last
        let mut order: Vec<usize> = (0..all.len()).collect();
        // the graphics card's own encoder first among the fallbacks (Intel -> Quick Sync, NVIDIA -> NVENC)
        #[cfg(windows)]
        {
            let first = match crate::gpucap::monitor_vendor(&cfg.monitor) { 0x10DE => Some(2), 0x8086 => Some(3), _ => None };
            if let Some(f) = first {
                order.retain(|&i| i != f);
                order.insert(1, f);
            }
        }
        if last > 0 && last < all.len() {
            order.retain(|&i| i != last);
            order.insert(1, last);
        }
        for i in order {
            let mode = all[i];
            let _ = fs::remove_dir_all(&self.buf_dir);
            fs::create_dir_all(&self.buf_dir).map_err(err)?;
            let stop = Arc::new(AtomicBool::new(false));
            let mut pipe_name = None;
            let video: Vec<String> = if mode == "gpu" {
                match gpu_input(i, &stop, &cfg, self.window.load(Ordering::Relaxed)) {
                    Some((args, name)) => {
                        pipe_name = Some(name);
                        args
                    }
                    None => continue,
                }
            } else {
                let c = match cfg.codec.as_str() { "hevc" => "hevc", "av1" => "av1", _ => "h264" };
                // smart bitrate: constant quality with the bitrate setting as the ceiling (see gpucap.rs); else CBR
                let peak = format!("{}M", cfg.bitrate_mbps);
                let buf = format!("{}M", cfg.bitrate_mbps * 2);
                let rc: Vec<&str> = match (mode, cfg.smart_bitrate) {
                    ("cpu-nvenc", true) => vec!["-rc", "vbr", "-maxrate", &peak, "-bufsize", &buf],
                    ("cpu-qsv", true) => vec!["-maxrate", &peak, "-bufsize", &buf],
                    (_, true) => vec!["-rc", "vbr_peak", "-maxrate", &peak],
                    ("cpu-nvenc", false) | (_, false) if mode != "cpu-qsv" => vec!["-rc", "cbr"],
                    _ => vec![],
                };
                let mut enc: Vec<String> = match mode {
                    "cpu-nvenc" => vec!["-c:v".into(), format!("{c}_nvenc"), "-preset".into(), "p3".into(), "-tune".into(), "ll".into()],
                    "cpu-qsv" => vec!["-c:v".into(), format!("{c}_qsv"), "-preset".into(), "veryfast".into()],
                    _ => vec!["-c:v".into(), format!("{c}_amf"), "-quality".into(), "speed".into()],
                };
                enc.extend(rc.iter().map(|x| x.to_string()));
                // smart bitrate: average target below the hard ceiling (-maxrate), see gpucap.rs
                let pct = if cfg.smart_quality <= 72 { 60 } else if cfg.smart_quality <= 82 { 75 } else { 90 };
                let avg = if cfg.smart_bitrate { format!("{}k", cfg.bitrate_mbps * 10 * pct) } else { peak.clone() };
                // explicit full-range RGB -> TV-range BT.709 conversion (else clips come out dark)
                let mut v: Vec<String> = vec![
                    "-filter_complex".into(), format!("{grab},hwdownload,format=bgra{scale},scale=in_range=pc:out_range=tv:out_color_matrix=bt709,format=nv12[v]"),
                    "-map".into(), "[v]".into(),
                ];
                v.extend(enc);
                v.extend(["-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"].map(String::from));
                v.extend(["-b:v".to_string(), avg, "-g".to_string(), (cfg.fps * SEG).to_string()]);
                v
            };
            let t0 = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_err(err)?.as_secs_f64();
            let audio_start = Instant::now();
            let offset = if mode == "gpu" { Some(t0) } else { None };
            let tracks = if cfg.app_tracks { 7 } else { 3 };
            let nr = if cfg.mic && cfg.mic_denoise {
                Some(match denoise_model() {
                    Some(p) => format!("arnndn=m='{}':mix=0.9", ff_path(&p)),
                    None => "afftdn=nf=-25".into(),
                })
            } else { None };
            // The noise filter can output NaN (ffmpeg 8/9 arnndn: on silence and at the end; tested chat 18).
            // One NaN stops that track's AAC encoder for good -> mix + mic tracks empty/dead, clips can't save.
            // aeval turns NaN/Inf into 0 (cheap: 4 s of audio in 0.13 s).
            let nr = nr.map(|f| {
                let e = (0..if ch == 1 { 1 } else { 2 }).map(|i| format!("if(isnan(val({i}))+isinf(val({i})),0,val({i}))")).collect::<Vec<_>>().join("|");
                format!("{f},aeval=exprs='{e}'")
            });
            MIX_NO_MIC.store(nr.is_some(), Ordering::Relaxed);
            let mut child = self.spawn(video, offset, rate, ch, cfg.buffer_seconds(), tracks, nr)?;
            if let Some(stdin) = child.stdin.take() {
                let (s, v) = (stop.clone(), self.volumes.clone());
                let (d, m, mn, apps) = (cfg.desktop_audio, cfg.mic, cfg.mic_device.clone(), cfg.app_tracks);
                std::thread::spawn(move || audio_thread(stdin, s, rate, ch, audio_start, d, m, mn, v, apps));
            }
            std::thread::sleep(Duration::from_secs(3));
            if matches!(child.try_wait(), Ok(None)) {
                *self.child.lock().unwrap() = Some(child);
                *self.audio_stop.lock().unwrap() = stop;
                *self.mode.lock().unwrap() = mode.into();
                let _ = fs::write(&mode_file, i.to_string());
                REC_LOG_POS.store(fs::metadata(log_path()).map(|m| m.len()).unwrap_or(0), Ordering::Relaxed);
                REC_STARTED_MS.store(now_ms(), Ordering::Relaxed);
                log_line(&format!("recording: {} ({} Hz, {} ch)",
                    if mode == "gpu" { "ZERO-COPY gpu pipeline" } else { "cpu copy fallback" }, rate, ch));
                return Ok(());
            }
            stop.store(true, Ordering::Relaxed);
            // unblock a capture thread still waiting for ffmpeg to connect
            if let Some(n) = pipe_name {
                let _ = fs::OpenOptions::new().read(true).open(n);
            }
            log_line(&format!("--- {mode} mode failed, trying next ---"));
        }
        Err("Recorder failed to start - see the ffmpeg log".into())
    }

    fn spawn(&self, video: Vec<String>, audio_offset: Option<f64>, rate: u32, ch: u16, buffer_secs: u32, tracks: usize,
             denoise: Option<String>) -> Result<Child, String> {
        let log = fs::OpenOptions::new().create(true).append(true).open(log_path()).map_err(err)?;
        // generous spare segments: a long save reads the oldest ones while recording continues
        let wrap = buffer_secs / SEG + 20;
        let mut args: Vec<String> = vec![
            "-hide_banner".into(), "-nostdin".into(), "-loglevel".into(), "error".into(), "-y".into(),
            // input 0: audio from our stdin pipe
            "-thread_queue_size".into(), "8192".into(),
            "-f".into(), "f32le".into(), "-ar".into(), rate.to_string(), "-ac".into(), (ch as usize * tracks).to_string(),
        ];
        if let Some(t0) = audio_offset {
            // gpu mode: video carries wall-clock timestamps; put audio on the same clock.
            args.extend(["-itsoffset".into(), format!("{t0:.6}")]);
        }
        args.extend(["-i".into(), "pipe:0".into()]);
        // audio tracks: 0 = mix (what players play), 1 = all desktop sound, 2 = mic, (3 = game, 4 = Discord, 5 = browsers)
        let c = ch as usize;
        let pan = |o: usize| (0..c).map(|i| format!("c{i}=c{}", o + i)).collect::<Vec<_>>().join("|");
        let lay = if c == 1 { "mono" } else { "stereo" };
        let mut fc = (0..tracks).map(|t| format!("[0:a]pan={lay}|{}[a{t}]", pan(t * c))).collect::<Vec<_>>().join(";");
        if let Some(nr) = denoise {
            // mic cleaned by the noise filter; the mix track is rebuilt from desktop + cleaned mic
            fc = format!("[0:a]asplit={tracks}{}", (0..tracks).map(|t| format!("[s{t}]")).collect::<String>());
            // track 0 from the app arrives WITHOUT the mic here (MIX_NO_MIC): add the cleaned mic to it
            for t in 0..tracks {
                fc += &format!(";[s{t}]pan={lay}|{}", pan(t * c));
                match t {
                    0 => fc += "[dm]",
                    1 => fc += "[a1]",
                    2 => fc += &format!("[m1];[m1]{nr},asplit[a2][mm]"),
                    _ => fc += &format!("[a{t}]"),
                }
            }
            fc += ";[dm][mm]amix=inputs=2:normalize=0:duration=longest[a0]";
        }
        let mut tail: Vec<String> = vec!["-filter_complex".into(), fc];
        for t in 0..tracks {
            tail.extend(["-map".into(), format!("[a{t}]")]);
        }
        tail.extend::<Vec<String>>(vec![
            "-c:a".into(), "aac".into(), "-b:a".into(), "160k".into(), "-ac".into(), "2".into(),
            "-f".into(), "segment".into(),
            "-segment_time".into(), SEG.to_string(),
            "-segment_wrap".into(), wrap.to_string(),
            "-reset_timestamps".into(), "1".into(),
            "-segment_format".into(), "mpegts".into(),
        ]);
        ffmpeg()
            .args(&args)
            .args(&video)
            .args(&tail)
            .arg(self.buf_dir.join("seg_%04d.ts"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .map_err(|e| format!("Could not start ffmpeg (is it installed?): {e}"))
    }

    pub fn stop(&self) {
        let _life = self.life.lock().unwrap_or_else(|e| e.into_inner());
        self.stop_inner();
    }

    /// Echo is quitting or updating: also stop the helpers that outlive the recorder (the cloud streaming
    /// server). Left running, it kept tools\rclone.exe locked so the installer couldn't replace it.
    pub fn shutdown(&self) {
        self.stop();
        self.stop_serve();
    }

    fn stop_inner(&self) {
        self.audio_stop.lock().unwrap().store(true, Ordering::Relaxed);
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill(); // mpegts segments are safe to cut
            let _ = c.wait();
        }
    }

    /// Save the last `seconds` (default: first hotkey's length).
    pub fn save(&self, seconds: Option<u32>) -> Result<String, String> {
        self.save_to(seconds, None)
    }

    /// Where a clip saved now goes, for the library's "Saving clip…" card (only shown in the folder it lands in):
    /// the clip's folder, else its game name (name_by_game), else "" (no folder / game). Same rules as save_to.
    pub fn save_target(&self, seconds: u32) -> String {
        let cfg = self.settings();
        let game = detect_game().or_else(|| {
            self.last_game.lock().unwrap().as_ref()
                .filter(|(_, t)| t.elapsed().as_secs() <= seconds as u64 + 30)
                .map(|(g, _)| g.clone())
        });
        let folder = game.as_ref().map(|g| cfg.game_folder(g)).filter(|g| !g.is_empty() && cfg.game_folders && cfg.game_rule(g).folder && !RESERVED_DIRS.iter().any(|r| r.eq_ignore_ascii_case(g)));
        match folder {
            Some(f) => f.trim_end_matches('.').to_string(),
            None => if cfg.name_by_game { game.unwrap_or_default() } else { String::new() },
        }
    }

    /// `to`: write the clip there instead of the library (self-test); no thumbnail / storage check then.
    fn save_to(&self, seconds: Option<u32>, to: Option<PathBuf>) -> Result<String, String> {
        let cfg = self.settings();
        let seconds = seconds.unwrap_or_else(|| cfg.hotkeys.first().map(|h| h.seconds).unwrap_or(30));
        let _one_at_a_time = self.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        let t0 = Instant::now();
        let list = || -> Result<Vec<(SystemTime, PathBuf)>, String> {
            let mut v: Vec<(SystemTime, PathBuf)> = fs::read_dir(&self.buf_dir)
                .map_err(err)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().map_or(false, |x| x == "ts"))
                .filter_map(|p| modified(&p).map(|t| (t, p)))
                .collect();
            v.sort();
            Ok(v)
        };
        let mut segs = list()?;
        if segs.is_empty() {
            return Err("Buffer is empty - recording isn't running. Check the ffmpeg log.".into());
        }
        // The newest segment is still being written (ffmpeg holds back its last second or so).
        // Wait until ffmpeg moves on to the next file, so the clip reaches the moment of the keypress.
        let live = segs.last().unwrap().1.clone();
        if self.is_recording() {
            let wait = Instant::now();
            while wait.elapsed() < Duration::from_millis(SEG as u64 * 1000 * 2 + 1500) {
                std::thread::sleep(Duration::from_millis(100));
                let now = list()?;
                if now.last().map_or(false, |(_, p)| *p != live) {
                    segs = now;
                    // keep everything up to (and including) the segment that was live at the keypress
                    if let Some(i) = segs.iter().position(|(_, p)| *p == live) {
                        segs.truncate(i + 1);
                    }
                    break;
                }
            }
        }
        let need = (seconds / SEG + 1) as usize;
        let start = segs.len().saturating_sub(need);

        fs::create_dir_all(&cfg.clips_dir).map_err(err)?;
        // game in front now, else the last game seen during this clip
        let game = detect_game().or_else(|| {
            self.last_game.lock().unwrap().as_ref()
                .filter(|(_, t)| t.elapsed().as_secs() <= seconds as u64 + 30)
                .map(|(g, _)| g.clone())
        });
        let prefix = if cfg.name_by_game { game.clone() } else { None }.unwrap_or_else(|| "Clip".into());
        // " (Echo)" only in the file name (Discord shows it when the file is pasted); the library hides it
        let name = format!("{prefix} {}", chrono::Local::now().format("%Y-%m-%d %H-%M-%S (Echo).mp4"));
        let dir = match game.map(|g| cfg.game_folder(&g)).filter(|g| !g.is_empty() && cfg.game_folders && cfg.game_rule(g).folder && !RESERVED_DIRS.iter().any(|r| r.eq_ignore_ascii_case(g))) {
            Some(g) => PathBuf::from(&cfg.clips_dir).join(g.trim_end_matches('.')),
            None => PathBuf::from(&cfg.clips_dir),
        };
        fs::create_dir_all(&dir).map_err(err)?;
        let test = to.is_some();
        let out = to.unwrap_or_else(|| dir.join(name));
        // write next to it under a temp name: the library never shows a half-written (black) clip
        let fin = out.clone();
        let out = out.with_extension("saving.tmp");
        // Try with the newest (still-being-written) segment; if that breaks the join, drop it.
        // Still failing: drop the oldest segments one by one (up to 3 = 6 s) - a broken oldest segment
        // (e.g. half overwritten by the buffer wrap) shouldn't lose the whole clip. (An audio track without
        // any data is handled inside join() with a silent track.)
        let mut ok = false;
        'tries: for skip in 0..=3usize {
            for end in [segs.len(), segs.len().saturating_sub(1)] {
                if end <= start + skip {
                    break 'tries;
                }
                if self.join(&segs[start + skip..end], &out)? {
                    if skip > 0 {
                        log_line(&format!("save: worked without the oldest {skip} segment(s)"));
                    }
                    ok = true;
                    break 'tries;
                }
            }
        }
        if !ok {
            return Err("ffmpeg failed to join the clip".into());
        }
        fs::rename(&out, &fin).map_err(err)?;
        let out = fin.to_string_lossy().to_string();
        log_line(&format!("save: {seconds}s -> {out} in {:.1}s", t0.elapsed().as_secs_f32()));
        if test {
            return Ok(out);
        }
        if let Ok(t) = self.thumb(&out) {
            // a clip far longer than asked for = audio and video timestamps split apart: restart for the next one
            if t.duration > seconds as f64 + 60.0 {
                log_line(&format!("save: clip is {:.0} s long, wanted {seconds} s - timestamps broke, restarting the recorder", t.duration));
                RESTART_REC.store(true, Ordering::Relaxed);
            }
        }
        self.enforce_storage(&cfg);
        Ok(out)
    }

    fn join(&self, segs: &[(SystemTime, PathBuf)], out: &PathBuf) -> Result<bool, String> {
        let list = self.buf_dir.join(format!("list_{}.txt", std::process::id()));
        let paths: Vec<PathBuf> = segs.iter().map(|(_, p)| p.clone()).collect();
        let body = concat_list(&paths);
        fs::write(&list, body).map_err(err)?;
        let status = ffmpeg()
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "concat", "-safe", "0", "-i"])
            .arg(&list)
            .args(["-map", "0:v", "-map", "0:a", "-c", "copy", "-bsf:a", "aac_adtstoasc", "-movflags", "+faststart"])
            .args(if self.settings().codec == "hevc" { &["-tag:v", "hvc1"][..] } else { &[][..] })
            .args(["-f", "mp4"])
            .arg(out)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(err)?;
        let msg = String::from_utf8_lossy(&status.stderr);
        if !msg.trim().is_empty() {
            log_line(&format!("save: ffmpeg: {}", msg.trim().lines().take(5).collect::<Vec<_>>().join(" | ")));
        }
        let mut ok = status.status.success();
        // An audio track with NO packets in these segments has no sample rate, and the mp4 can't be written
        // at all ("sample rate not set"; friend's self-test, 2 Nov). Put a silent track in its place so the
        // clip still saves with all its tracks, and log which track was empty.
        if !ok && msg.contains("sample rate not set") {
            ok = self.join_with_silence(&list, segs.len(), out).unwrap_or(false);
        }
        let _ = fs::remove_file(&list);
        Ok(ok)
    }

    fn join_with_silence(&self, list: &PathBuf, n_segs: usize, out: &PathBuf) -> Result<bool, String> {
        let probe = hidden("ffprobe")
            .args(["-v", "error", "-f", "concat", "-safe", "0", "-i"]).arg(list)
            .args(["-select_streams", "a", "-count_packets", "-show_entries", "stream=nb_read_packets", "-of", "csv=p=0"])
            .output().map_err(err)?;
        let counts: Vec<u64> = String::from_utf8_lossy(&probe.stdout).lines()
            .map(|l| l.trim().parse().unwrap_or(0)).collect();
        if counts.is_empty() || !counts.iter().any(|&c| c == 0) {
            return Ok(false);
        }
        let names = ["mix", "desktop", "mic", "game", "Discord", "browsers", "music"];
        let empty: Vec<String> = counts.iter().enumerate().filter(|(_, c)| **c == 0)
            .map(|(i, _)| names.get(i).map(|n| n.to_string()).unwrap_or_else(|| format!("#{i}"))).collect();
        log_line(&format!("save: audio track(s) {} had no sound data at all - saved with silence there", empty.join(", ")));
        let secs = (n_segs as u32 * SEG + 4).to_string();
        let mut cmd = ffmpeg();
        cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "concat", "-safe", "0", "-i"]).arg(list)
            .args(["-f", "lavfi", "-t", &secs, "-i", "anullsrc=r=48000:cl=stereo", "-map", "0:v"]);
        for (i, &c) in counts.iter().enumerate() {
            if c == 0 { cmd.args(["-map", "1:a"]); } else { cmd.args(["-map".to_string(), format!("0:a:{i}")]); }
        }
        cmd.args(["-c", "copy"]);
        for (i, &c) in counts.iter().enumerate() {
            if c == 0 { cmd.args([format!("-c:a:{i}"), "aac".into(), format!("-b:a:{i}"), "32k".into()]); }
        }
        cmd.args(["-bsf:a", "aac_adtstoasc", "-shortest", "-movflags", "+faststart"])
            .args(if self.settings().codec == "hevc" { &["-tag:v", "hvc1"][..] } else { &[][..] })
            .args(["-f", "mp4"]).arg(out)
            .stdout(Stdio::null()).stderr(Stdio::piped());
        let o = cmd.output().map_err(err)?;
        let m = String::from_utf8_lossy(&o.stderr);
        if !m.trim().is_empty() {
            log_line(&format!("save: ffmpeg (with silence): {}", m.trim().lines().take(3).collect::<Vec<_>>().join(" | ")));
        }
        Ok(o.status.success())
    }

    /// Recorder runs but is broken: no new buffer segment for 20 s (ffmpeg stuck), or an encoder inside ffmpeg
    /// stopped (e.g. "Input contains NaN" -> "Terminating thread"; ffmpeg itself keeps running, so the
    /// "stopped unexpectedly" watchdog never sees it). Called by main's 3 s loop.
    pub fn health(&self) -> Option<String> {
        if !self.is_recording() {
            return None;
        }
        if let Some(age) = self.buffer_age() {
            if age > 20.0 {
                return Some(format!("no new recording data for {age:.0} s"));
            }
        }
        let wrote = AUDIO_WROTE_AT.load(Ordering::Relaxed);
        if wrote > 0 && now_ms().saturating_sub(wrote) > 15_000 {
            return Some(format!("ffmpeg stopped taking audio {} s ago", now_ms().saturating_sub(wrote) / 1000));
        }
        #[cfg(windows)]
        if let Some(why) = self.device_changed() {
            return Some(why);
        }
        use std::io::{Read, Seek, SeekFrom};
        let pos = REC_LOG_POS.load(Ordering::Relaxed);
        let mut f = fs::File::open(log_path()).ok()?;
        let len = f.metadata().ok()?.len();
        if len < pos {
            REC_LOG_POS.store(len, Ordering::Relaxed); // log was rotated
            return None;
        }
        let mut new = Vec::new();
        f.seek(SeekFrom::Start(pos.max(len.saturating_sub(256 * 1024)))).ok()?;
        f.take(256 * 1024).read_to_end(&mut new).ok()?;
        REC_LOG_POS.store(len, Ordering::Relaxed);
        let t = String::from_utf8_lossy(&new);
        // only the recorder's ffmpeg writes straight into the log; exports etc. go through log_line (counted there):
        // an export failing on an encoder restarted the recorder over and over ("Recorder stopped" spam, chat 23)
        let seen: u64 = REC_FAIL_MARKS.iter().map(|m| t.matches(m).count() as u64).sum();
        let other = LOGGED_MARKS.swap(0, Ordering::Relaxed);
        if seen > other {
            return Some("an encoder inside ffmpeg stopped".into());
        }
        None
    }

    /// Every 10 s: is Echo still capturing the devices Windows uses now? Desktop sound follows the default
    /// output (headset back on -> Windows switches to it, Echo was still on the speakers = silent clips);
    /// the mic follows the default input, or the chosen mic once it's plugged in again.
    #[cfg(windows)]
    fn device_changed(&self) -> Option<String> {
        let now = now_unix();
        if now.saturating_sub(DEV_CHECKED.load(Ordering::Relaxed)) < 10 {
            return None;
        }
        DEV_CHECKED.store(now, Ordering::Relaxed);
        let cfg = self.settings();
        let cur = CAP_DEV.lock().unwrap().clone();
        let host = cpal::default_host();
        if cfg.desktop_audio {
            if let Some(n) = host.default_output_device().and_then(|d| d.name().ok()) {
                if n != cur[0] {
                    return Some(format!("default speakers are now '{n}' (was '{}')", cur[0]));
                }
            }
        }
        if cfg.mic && !MIC_MUTED.load(Ordering::Relaxed) {
            let want = if cfg.mic_device.is_empty() {
                host.default_input_device().and_then(|d| d.name().ok())
            } else if cur[1] != cfg.mic_device {
                // the chosen mic was missing at start: is it back?
                host.input_devices().ok().and_then(|mut it| it.find(|d| d.name().map_or(false, |n| n == cfg.mic_device)))
                    .map(|_| cfg.mic_device.clone())
            } else { None };
            if let Some(n) = want {
                if n != cur[1] {
                    return Some(format!("microphone is now '{n}' (was '{}')", cur[1]));
                }
            }
        }
        None
    }

    /// Seconds since ffmpeg last wrote to the replay buffer (None = no buffer yet).
    pub fn buffer_age(&self) -> Option<f64> {
        let newest = fs::read_dir(&self.buf_dir).ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map_or(false, |x| x == "ts"))
            .filter_map(|p| modified(&p))
            .max()?;
        Some(SystemTime::now().duration_since(newest).map(|d| d.as_secs_f64()).unwrap_or(0.0))
    }

    /// Records a short test clip (outside the library) and checks video + every audio track.
    /// Returns (check, ok, detail) rows. Needs the recorder running.
    pub fn self_test(&self) -> Vec<(String, bool, String)> {
        let mut r: Vec<(String, bool, String)> = Vec::new();
        let cfg = self.settings();
        let mode = self.mode.lock().unwrap().clone();
        if !self.is_recording() {
            r.push(("Recorder running".into(), false, "Not recording - press Restart recorder, then try again".into()));
            return r;
        }
        r.push(("Recorder running".into(), true, match mode.as_str() {
            "gpu" => "Zero-copy GPU capture".into(),
            m => format!("CPU fallback ({m}) - works, but costs more performance"),
        }));
        log_line("selftest: recording 6 s");
        std::thread::sleep(Duration::from_secs(6));
        let out = std::env::temp_dir().join("clipper_selftest.mp4");
        let _ = fs::remove_file(&out);
        if let Err(e) = self.save_to(Some(5), Some(out.clone())) {
            r.push(("Save a clip".into(), false, e));
            return r;
        }
        r.push(("Save a clip".into(), true, format!("{:.1} MB for ~5 s", fs::metadata(&out).map(|m| m.len()).unwrap_or(0) as f64 / 1e6)));
        // video
        let probe = hidden("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name,width,height,avg_frame_rate:format=duration", "-of", "default=nw=1"])
            .arg(&out).output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
        let get = |k: &str| probe.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).unwrap_or("").to_string();
        let fps = { let f = get("avg_frame_rate"); let mut it = f.split('/'); let a: f64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0.0); let b: f64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(1.0); if b > 0.0 { a / b } else { 0.0 } };
        let dur: f64 = get("duration").parse().unwrap_or(0.0);
        let vok = !get("codec_name").is_empty() && dur >= 3.0;
        r.push(("Video".into(), vok, if vok {
            format!("{} {}x{} · {:.0} FPS (set: {}) · {:.1} s", get("codec_name").to_uppercase(), get("width"), get("height"), fps, cfg.fps, dur)
        } else { "No usable video in the test clip - see the log".into() }));
        if vok && fps > 0.0 && fps < cfg.fps as f64 * 0.85 {
            r.push(("Frame rate".into(), false, format!("Only {fps:.0} of {} FPS - try a lower resolution / FPS, or the GPU is very busy", cfg.fps)));
        }
        // a nearly black picture usually means the capture shows nothing (e.g. protected content / wrong monitor)
        let luma = ffmpeg().args(["-hide_banner", "-nostats", "-i"]).arg(&out)
            .args(["-vf", "signalstats,metadata=print:key=lavfi.signalstats.YAVG", "-frames:v", "30", "-f", "null", "-"])
            .output().map(|o| String::from_utf8_lossy(&o.stderr).to_string()).unwrap_or_default();
        let ys: Vec<f64> = luma.lines().filter_map(|l| l.split("YAVG=").nth(1).and_then(|v| v.trim().parse().ok())).collect();
        if !ys.is_empty() {
            let avg = ys.iter().sum::<f64>() / ys.len() as f64;
            r.push(("Picture".into(), avg > 18.0, if avg > 18.0 { "Not black".into() } else { "The picture is (almost) black - wrong monitor, or the screen was dark".into() }));
        }
        // audio tracks
        let names = ["Mix (what players hear)", "All desktop sound", "Microphone", "Game", "Discord", "Browsers", "Music apps"];
        let n = self.audio_tracks(&out.to_string_lossy());
        let want = if cfg.app_tracks { 7 } else { 3 };
        r.push(("Audio tracks".into(), n as usize == want, format!("{n} of {want}")));
        for t in 0..n.min(7) {
            let o = ffmpeg().args(["-hide_banner", "-nostats", "-i"]).arg(&out)
                .args(["-map", &format!("0:a:{t}"), "-af", "volumedetect", "-f", "null", "-"])
                .output().map(|o| String::from_utf8_lossy(&o.stderr).to_string()).unwrap_or_default();
            let max: f64 = o.lines().find_map(|l| l.split("max_volume:").nth(1)).and_then(|v| v.trim().trim_end_matches("dB").trim().parse().ok()).unwrap_or(-91.0);
            let silent = max <= -60.0;
            let name = names[t as usize];
            // apps that aren't running are silent on purpose; desktop + mic should have sound if they're on
            let expect = match t { 1 => cfg.desktop_audio, 2 => cfg.mic && !MIC_MUTED.load(Ordering::Relaxed) && !PTT_ON.load(Ordering::Relaxed), 0 => cfg.desktop_audio || cfg.mic, _ => false };
            let detail = if silent {
                match t {
                    1 => "Silent - play some sound while testing".to_string(),
                    2 => "Silent - say something while testing (or check the mic device)".to_string(),
                    0 => "Silent".to_string(),
                    _ => "Silent (fine if that app isn't running / playing)".to_string(),
                }
            } else { format!("Sound (peak {max:.0} dB)") };
            r.push((format!("Audio: {name}"), !(silent && expect), detail));
        }
        // silent desktop/mic: write what the recorder is really doing to the log (friend's PC, chat 18:
        // all 6 tracks empty, not reproducible elsewhere)
        if r.iter().any(|(a, ok, _)| !ok && a.starts_with("Audio")) {
            let wrote = AUDIO_WROTE_AT.load(Ordering::Relaxed);
            log_line(&format!("selftest: audio writer last wrote {} ms ago, recorder running {} s",
                if wrote > 0 { now_ms().saturating_sub(wrote) as i64 } else { -1 },
                now_ms().saturating_sub(REC_STARTED_MS.load(Ordering::Relaxed)) / 1000));
            let mut segs: Vec<(SystemTime, PathBuf)> = fs::read_dir(&self.buf_dir).into_iter().flatten()
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().map_or(false, |x| x == "ts"))
                .filter_map(|p| modified(&p).map(|t| (t, p))).collect();
            segs.sort();
            let v = ffmpeg().arg("-version").output().map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").to_string()).unwrap_or_default();
            log_line(&format!("selftest: {v}"));
            // the newest finished segment (the very newest is still being written)
            if let Some((_, seg)) = segs.iter().rev().nth(1) {
                let o = hidden("ffprobe").args(["-v", "error", "-count_packets", "-show_entries",
                    "stream=index,codec_type,sample_rate,start_time,nb_read_packets", "-of", "csv=p=0"]).arg(seg)
                    .output().map(|o| format!("{} {}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
                    .unwrap_or_default();
                log_line(&format!("selftest: {} -> {}", seg.file_name().unwrap_or_default().to_string_lossy(),
                    o.split_whitespace().collect::<Vec<_>>().join(" ")));
            }
        }
        let _ = fs::remove_file(&out);
        log_line(&format!("selftest: {}", r.iter().map(|(a, ok, b)| format!("{a}={}{}", if *ok { "ok" } else { "FAIL " }, if *ok { String::new() } else { b.clone() })).collect::<Vec<_>>().join(", ")));
        r
    }

    fn thumbs_dir(&self) -> PathBuf {
        self.clips_dir().join(".thumbs")
    }

    /// Cached preview image + duration for `clip`, generated with ffmpeg/ffprobe if missing.
    pub fn thumb(&self, clip: &str) -> Result<Thumb, String> {
        let clip = PathBuf::from(clip);
        let dir = self.thumbs_dir();
        fs::create_dir_all(&dir).map_err(err)?;
        let stem = clip.file_stem().ok_or("bad clip path")?.to_string_lossy().to_string();
        let img = dir.join(format!("{stem}.jpg"));
        let dur_file = dir.join(format!("{stem}.dur"));
        if !img.exists() {
            let at = self.thumb_time(&stem).map_or("0.5".to_string(), |t| format!("{t:.3}"));
            let _ = ffmpeg()
                .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &at, "-i"])
                .arg(&clip)
                .args(["-frames:v", "1", "-vf", "scale=480:-2", "-q:v", "4"])
                .arg(&img)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let duration = match fs::read_to_string(&dur_file).ok().and_then(|s| s.trim().parse().ok()) {
            Some(d) => d,
            None => {
                let out = hidden("ffprobe")
                    .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"])
                    .arg(&clip)
                    .output()
                    .map_err(err)?;
                let d: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0.0);
                if d > 0.0 {
                    let _ = fs::write(&dur_file, d.to_string());
                }
                d
            }
        };
        Ok(Thumb { thumb: img.to_string_lossy().into(), duration })
    }

    /// Top level of the clips folder plus its clip sub-folders (per game / custom).
    fn clip_dirs(&self) -> Vec<(String, PathBuf)> {
        let root = self.clips_dir();
        let mut v = vec![(String::new(), root.clone())];
        // folders and sub-folders ("FiveM/Staff evidence"), a few levels deep
        fn walk(dir: &Path, rel: &str, depth: u32, v: &mut Vec<(String, PathBuf)>) {
            if depth > 5 {
                return;
            }
            if let Ok(rd) = fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    let n = e.file_name().to_string_lossy().to_string();
                    if !p.is_dir() || n.starts_with('.') || (rel.is_empty() && RESERVED_DIRS.iter().any(|r| r.eq_ignore_ascii_case(&n))) {
                        continue;
                    }
                    let r = if rel.is_empty() { n } else { format!("{rel}/{n}") };
                    v.push((r.clone(), p.clone()));
                    walk(&p, &r, depth + 1, v);
                }
            }
        }
        walk(&root, "", 1, &mut v);
        v
    }

    /// All clip folders (also empty ones you made), as "Folder/Sub folder" paths.
    pub fn list_folders(&self) -> Vec<String> {
        // the cloud's date folders ("2026/11 - November/05 - Thursday") showed up as EMPTY clip folders on Windows
        // (chat 26, cause unknown): remove empty ones once per run and say so in the log (with any clips inside: kept)
        static TIDIED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !TIDIED.swap(true, Ordering::Relaxed) { self.tidy_date_dirs(); }
        let mut v: Vec<String> = self.clip_dirs().into_iter().map(|(f, _)| f).filter(|f| !f.is_empty()).collect();
        v.sort_by_key(|f| f.to_lowercase());
        v
    }

    fn tidy_date_dirs(&self) {
        let root = self.clips_dir();
        let Ok(rd) = fs::read_dir(&root) else { return };
        fn files_in(d: &Path, out: &mut Vec<PathBuf>) {
            for e in fs::read_dir(d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() { files_in(&p, out); } else if e.file_name() != ".echo-folder" { out.push(p); }
            }
        }
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            // a year folder ("2026") whose sub folders look like the cloud's months ("11 - November", "11", "November")
            if !(name.len() == 4 && name.bytes().all(|b| b.is_ascii_digit()) && e.path().is_dir()) { continue; }
            let months: Vec<String> = fs::read_dir(e.path()).into_iter().flatten().flatten().map(|m| m.file_name().to_string_lossy().to_string()).collect();
            let monthish = |m: &str| { let w = m.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches(" - ").trim();
                MONTHS.contains(&w) || (w.is_empty() && m.len() == 2) };
            if months.is_empty() || !months.iter().all(|m| monthish(m)) { continue; }
            let mut files = Vec::new();
            files_in(&e.path(), &mut files);
            if files.is_empty() {
                let made = e.metadata().and_then(|m| m.modified()).ok().map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
                let r = fs::remove_dir_all(e.path());
                log_line(&format!("folders: removed the empty cloud-date folder '{name}' ({}, last changed {made})", r.map_or_else(|e| format!("failed: {e}"), |_| "ok".into())));
            } else {
                log_line(&format!("folders: '{name}' looks like a cloud date folder but has {} file(s), e.g. {} - kept", files.len(),
                    files[0].strip_prefix(&root).unwrap_or(&files[0]).display()));
            }
        }
    }

    /// Make an (empty) folder; `name` may be "Parent/Child". A hidden marker keeps it when its last clip moves out.
    pub fn create_folder(&self, name: &str) -> Result<String, String> {
        let rel = clean_folder(name);
        if rel.is_empty() {
            return Err("type a folder name".into());
        }
        if folder_reserved(&rel) {
            return Err("that folder name is reserved".into());
        }
        let dir = self.clips_dir().join(&rel);
        if dir.is_dir() {
            return Err("that folder already exists".into());
        }
        fs::create_dir_all(&dir).map_err(err)?;
        let _ = fs::write(dir.join(".echo-folder"), "");
        log_line(&format!("folders: made '{rel}'"));
        Ok(rel)
    }

    /// Remove a folder that has no clips (sub-folders without clips go too).
    pub fn delete_folder(&self, name: &str) -> Result<(), String> {
        let rel = clean_folder(name);
        if rel.is_empty() {
            return Err("pick a folder".into());
        }
        let dir = self.clips_dir().join(&rel);
        fn has_files(d: &Path) -> bool {
            fs::read_dir(d).map(|rd| rd.flatten().any(|e| {
                let p = e.path();
                if p.is_dir() { has_files(&p) } else { e.file_name() != ".echo-folder" }
            })).unwrap_or(false)
        }
        if has_files(&dir) {
            return Err("move or delete the clips in it first".into());
        }
        fs::remove_dir_all(&dir).map_err(err)?;
        log_line(&format!("folders: removed '{rel}'"));
        Ok(())
    }

    /// Is this file a clip in the clips folder or one of its clip sub-folders?
    fn is_clip(&self, p: &Path) -> bool {
        let Some(parent) = p.parent() else { return false };
        let root = self.clips_dir();
        let Ok(rel) = parent.strip_prefix(&root) else { return false };
        let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
        parts.len() <= 6
            && parts.iter().all(|n| !n.is_empty() && !n.starts_with('.') && n != "..")
            && parts.first().map_or(true, |n| !RESERVED_DIRS.iter().any(|r| r.eq_ignore_ascii_case(n)))
    }

    /// Move a clip into `folder` ("" = top level). Returns the new path.
    pub fn move_clip(&self, path: &str, folder: &str) -> Result<String, String> {
        let p = PathBuf::from(path);
        if !self.is_clip(&p) {
            return Err("not a Clipper clip".into());
        }
        let folder = clean_folder(folder);
        if folder_reserved(&folder) {
            return Err("that folder name is reserved".into());
        }
        let dir = if folder.is_empty() { self.clips_dir() } else { self.clips_dir().join(&folder) };
        if p.parent() == Some(dir.as_path()) {
            return Ok(path.into());
        }
        fs::create_dir_all(&dir).map_err(err)?;
        let new = dir.join(p.file_name().ok_or("bad clip path")?);
        if new.exists() {
            return Err(format!("{} already has a clip with that name", if folder.is_empty() { "The main folder" } else { &folder }));
        }
        let old_rel = self.rel_of(path);
        rename_patient(&p, &new)?;
        self.remove_if_empty(p.parent());
        if let (Some(o), Some(n)) = (old_rel, self.rel_of(&new.to_string_lossy())) {
            self.cloud_follow_move(o, n);
        }
        Ok(new.to_string_lossy().into())
    }

    /// Rename a clip folder; if `new` already exists the two are merged ("" = merge into the main folder).
    /// Future clips of that game follow (settings.folder_alias).
    pub fn rename_folder(&self, old: &str, new: &str) -> Result<String, String> {
        let (old, new) = (clean_folder(old), clean_folder(new));
        if old.is_empty() {
            return Err("pick a folder".into());
        }
        if folder_reserved(&new) || folder_reserved(&old) {
            return Err("that folder name is reserved".into());
        }
        if new.to_lowercase().starts_with(&format!("{}/", old.to_lowercase())) {
            return Err("a folder can't go inside itself".into());
        }
        let root = self.clips_dir();
        let src = root.join(&old);
        if !src.is_dir() {
            return Err("that game has no folder yet".into());
        }
        let dst = if new.is_empty() { root.clone() } else { root.join(&new) };
        let same_dir = !new.is_empty() && new.eq_ignore_ascii_case(&old);
        if same_dir || !dst.exists() {
            // plain rename (also fixes upper / lower case); sub-folders move along
            if let Some(par) = dst.parent() {
                let _ = fs::create_dir_all(par);
            }
            fs::rename(&src, &dst).map_err(err)?;
        } else {
            // merge: move every clip over, renaming on name clashes
            let rd = fs::read_dir(&src).map_err(err)?;
            for f in rd.flatten() {
                let p = f.path();
                if p.is_dir() {
                    // sub-folders come along (unless the target already has one with that name)
                    let to = dst.join(f.file_name());
                    if !to.exists() {
                        let _ = fs::rename(&p, &to);
                    }
                    continue;
                }
                if !p.is_file() || f.file_name() == ".echo-folder" {
                    continue;
                }
                let stem = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
                let ext = p.extension().map(|x| format!(".{}", x.to_string_lossy())).unwrap_or_default();
                let mut to = dst.join(format!("{stem}{ext}"));
                let mut k = 2;
                while to.exists() {
                    to = dst.join(format!("{stem} ({k}){ext}"));
                    k += 1;
                }
                fs::rename(&p, &to).map_err(err)?;
                let new_stem = to.file_stem().unwrap_or_default().to_string_lossy().to_string();
                if new_stem != stem {
                    for x in ["jpg", "dur", "strip.jpg", "strip2.jpg", "strip3.jpg", "a1.m4a", "a2.m4a", "a3.m4a", "a4.m4a", "a5.m4a", "marks.json", "levels.json", "words.json", "project.json"] {
                        let _ = fs::rename(self.thumbs_dir().join(format!("{stem}.{x}")), self.thumbs_dir().join(format!("{new_stem}.{x}")));
                    }
                    let mut meta = self.meta_all();
                    if let Some(m) = meta.remove(&stem) {
                        meta.insert(new_stem.clone(), m);
                        let _ = self.write_meta(&meta);
                    }
                    if self.favorites().contains(&stem) {
                        let _ = self.set_favorite(&stem, false);
                        let _ = self.set_favorite(&new_stem, true);
                    }
                }
            }
            let _ = fs::remove_file(src.join(".echo-folder"));
            let _ = fs::remove_dir(&src);
        }
        // new clips of this game go to the new folder too
        {
            let mut c = self.cfg.lock().unwrap();
            for v in c.folder_alias.values_mut() {
                if v.eq_ignore_ascii_case(&old) {
                    *v = new.clone();
                }
            }
            c.folder_alias.retain(|k, _| !k.eq_ignore_ascii_case(&new) || new.is_empty());
            c.folder_alias.insert(old.clone(), new.clone());
            let _ = c.save();
        }
        // cloud copies follow (file by file, so merges work too)
        if self.cloud_on() {
            let lo = old.to_lowercase();
            let moves: Vec<(String, String)> = self.cloud_state().files.iter().filter(|f| {
                let fl = f.folder.replace('\\', "/").to_lowercase();
                fl == lo || fl.starts_with(&format!("{lo}/"))
            }).map(|f| {
                let fol = f.folder.replace('\\', "/");
                let sub = fol.get(old.len()..).unwrap_or("").trim_start_matches('/').to_string();
                let file = Path::new(&f.rel).file_name().unwrap_or_default().to_string_lossy().to_string();
                let nf = [new.as_str(), sub.as_str()].iter().filter(|x| !x.is_empty()).cloned().collect::<Vec<_>>().join("/");
                (f.rel.clone(), if nf.is_empty() { file } else { format!("{nf}/{file}") })
            }).collect();
            for (o, n) in moves {
                self.cloud_follow_move(o, n);
            }
        }
        log_line(&format!("folders: '{old}' -> '{}'", if new.is_empty() { "main folder" } else { &new }));
        Ok(new)
    }

    fn remove_if_empty(&self, dir: Option<&Path>) {
        if let Some(d) = dir {
            if d != self.clips_dir() {
                let _ = fs::remove_dir(d); // only succeeds when empty
            }
        }
    }

    /// Move older top-level clips named "<Game> 2026-..." into their game's folder. Returns how many moved.
    /// A game named anywhere in a file name ("FiveM_2026…", "Valorant clip 3", "GTA V - Moments"), from the
    /// games Echo knows (seen, folders, rules) plus common names. Longest match wins.
    fn guess_game(&self, name: &str) -> Option<String> {
        let norm = |s: &str| format!(" {} ", s.to_lowercase().chars()
            .map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect::<String>()
            .split_whitespace().collect::<Vec<_>>().join(" "));
        const HINTS: &[(&str, &str)] = &[
            ("fivem", "FiveM"), ("grand theft auto v", "GTA V"), ("gta v", "GTA V"), ("gtav", "GTA V"), ("gta5", "GTA V"),
            ("valorant", "Valorant"), ("counter strike 2", "Counter-Strike 2"), ("cs2", "Counter-Strike 2"), ("fortnite", "Fortnite"),
            ("apex legends", "Apex Legends"), ("rocket league", "Rocket League"), ("league of legends", "League of Legends"),
            ("overwatch", "Overwatch 2"), ("minecraft", "Minecraft"), ("roblox", "Roblox"), ("call of duty", "Call of Duty"),
            ("warzone", "Call of Duty"), ("rainbow six", "Rainbow Six Siege"), ("pubg", "PUBG"), ("dota 2", "Dota 2"),
            ("marvel rivals", "Marvel Rivals"), ("helldivers 2", "Helldivers 2"), ("tarkov", "Escape from Tarkov"),
            ("elden ring", "Elden Ring"), ("destiny 2", "Destiny 2"), ("red dead redemption 2", "Red Dead Redemption 2"),
        ];
        let n = norm(name);
        // names without spaces too ("MedalTVGrandTheftAutoVFiveM2026…"): compare with spaces removed, for names of 5+ letters
        let compact = |s: &str| s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect::<String>();
        let nc = compact(name);
        let not = self.settings().not_games;
        let mut cands: Vec<(String, String, bool)> = self.list_games().into_iter().map(|g| (norm(&g.name), g.name, true)).collect();
        cands.extend(HINTS.iter().map(|(k, v)| (norm(k), v.to_string(), false)));
        cands.into_iter()
            .filter(|(k, v, _)| k.trim().len() >= 3 && k.trim() != n.trim() && !v.eq_ignore_ascii_case("echo")
                && !["exported clips", "shared clips", "clips", "videos", "highlights", "exports", "screenshots", "gifs", "audio", "sounds"].contains(&v.to_lowercase().as_str())
                && !not.iter().any(|x| x.eq_ignore_ascii_case(v)))
            .filter_map(|(k, v, known)| {
                let word = n.contains(k.as_str());
                let kc = compact(&k);
                (word || (kc.len() >= 5 && nc.contains(&kc))).then(|| (known, word, k.len(), v))
            })
            // games Echo already knows beat built-in names, whole words beat glued-together text, then the longest
            .max_by_key(|(known, word, len, _)| (*known, *word, *len))
            .map(|(_, _, _, v)| v)
            // other apps (Medal…) call FiveM clips "Grand Theft Auto V (Legacy)": those go to FiveM, but only for
            // people who play FiveM (played with Echo open, a FiveM folder, or FiveM installed) - chat 21
            .map(|v| if v == "GTA V" && self.fivem_player() { "FiveM".to_string() } else { v })
    }

    /// Does this person play FiveM? A FiveM game/folder Echo knows, or FiveM installed (Windows: %LOCALAPPDATA%\FiveM).
    pub fn fivem_player(&self) -> bool {
        if self.settings().not_games.iter().any(|x| x.eq_ignore_ascii_case("FiveM")) {
            return false;
        }
        let seen: std::collections::BTreeMap<String, String> = fs::read_to_string(app_dir().join("games.json")).ok()
            .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        if seen.keys().any(|k| k.eq_ignore_ascii_case("FiveM")) || self.clips_dir().join("FiveM").is_dir() {
            return true;
        }
        #[cfg(windows)]
        if let Ok(la) = std::env::var("LOCALAPPDATA") {
            if Path::new(&la).join("FiveM").join("FiveM.exe").exists() {
                return true;
            }
        }
        false
    }

    /// The Echo watermark ("Echo / By Kimbo", bottom-right) as filtergraph text: video [inl] of height `h` -> [outl]. None = off.
    /// Every re-encode ends in plain BT.709 TV-range yuv420p. Without it ffmpeg 8 sometimes wrote the
    /// watermarked Discord copies as full-range "gbr" video, which Discord / browsers show far too dark.
    /// The watermark APNG for a style ("kimbo" / "link" = "echoclip.link" under the name / "echo"), written next to the settings once; None for "off".
    fn watermark_file(style: &str) -> Option<PathBuf> {
        // animated (APNG, 1 s): the logo animates in, then the last frame stays for the rest of the video
        const KIMBO: &[u8] = include_bytes!("watermark.apng");
        const ECHO: &[u8] = include_bytes!("watermark_echo.apng");
        const LINK: &[u8] = include_bytes!("watermark_link.apng");
        let (png, file) = match style {
            "off" => return None,
            "echo" => (ECHO, "watermark_echo.apng"),
            "link" => (LINK, "watermark_link.apng"),
            _ => (KIMBO, "watermark.apng"),
        };
        let p = app_dir().join(file);
        if fs::metadata(&p).map(|m| m.len()).ok() != Some(png.len() as u64) {
            fs::create_dir_all(app_dir()).ok()?;
            fs::write(&p, png).ok()?;
        }
        Some(p)
    }

    fn watermark_graph(&self, inl: &str, outl: &str, h: u32) -> Option<String> {
        let p = Self::watermark_file(self.settings().watermark_style.as_str())?;
        let wh = ((h as f64 * 0.075).round() as u32).max(20);
        let m = ((h as f64 * 0.025).round() as u32).max(6);
        Some(format!("movie='{}':f=apng,format=rgba,scale=-1:{wh}[wmk{outl}];[{inl}][wmk{outl}]overlay=W-w-{m}:H-h-{m}:format=yuv420,{TV_YUV}[{outl}]", ff_path(&p)))
    }

    /// Half-written files left by a crash or a closed app (imports, saves, shrinks, trims) older than an hour.
    pub fn remove_stale_tmp(&self) {
        let old = SystemTime::now() - Duration::from_secs(3600);
        for (_, dir) in self.clip_dirs() {
            for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
                let n = e.file_name().to_string_lossy().to_lowercase();
                if [".importing.tmp", ".saving.tmp", ".compress.tmp", ".trimming.tmp", ".giftmp"].iter().any(|x| n.ends_with(x))
                    && modified(&e.path()).map_or(false, |t| t < old) && fs::remove_file(e.path()).is_ok() {
                    log_line(&format!("cleanup: removed leftover {n}"));
                }
            }
        }
    }

    /// Moves clips whose name contains a game (main folder and catch-all folders like "Exported Clips") into that game's folder.
    pub fn sort_into_folders(&self) -> u32 {
        let mut n = 0;
        const LOOSE: &[&str] = &["", "exported clips", "shared clips", "clips", "videos", "highlights", "exports"];
        for c in self.list_clips().into_iter().filter(|c| LOOSE.contains(&c.folder.to_lowercase().as_str())) {
            let game = game_of_name(&c.name).or_else(|| self.guess_game(&c.name));
            if let Some(g) = game.map(|g| self.settings().game_folder(&g)).filter(|g| !g.is_empty() && !g.eq_ignore_ascii_case(&c.folder)) {
                if self.move_clip(&c.path, &g).is_ok() {
                    n += 1;
                }
            }
        }
        log_line(&format!("folders: sorted {n} clips into game folders"));
        n
    }

    pub fn pause_reason(&self) -> String {
        self.pause_reason.lock().unwrap().clone()
    }
    pub fn set_pause(&self, r: &str) {
        *self.pause_reason.lock().unwrap() = r.into();
        self.idle_paused.store(r == "idle", Ordering::Relaxed);
    }
    /// start()/stop() running right now
    pub fn busy(&self) -> bool {
        self.life.try_lock().is_err()
    }

    /// Known games (seen while recording, clip folders, rules) with icons.
    pub fn list_games(&self) -> Vec<GameInfo> {
        let mut names: std::collections::BTreeSet<String> = Default::default();
        let seen: std::collections::BTreeMap<String, String> = fs::read_to_string(app_dir().join("games.json")).ok()
            .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        names.extend(seen.keys().cloned());
        // top-level folders only: sub-folders you made ("FiveM/Evidence") aren't games (chat 21)
        names.extend(self.clip_dirs().into_iter().map(|d| d.0).filter(|n| !n.is_empty() && !n.contains('/')));
        names.extend(self.settings().games.iter().map(|g| g.name.clone()));
        let not = self.settings().not_games;
        names.retain(|n| !not.iter().any(|x| x.eq_ignore_ascii_case(n)));
        names.into_iter().map(|name| {
            // the file's path (the window loads it like a thumbnail) instead of the whole picture as base64
            // in every library refresh; the change time makes a new icon show up straight away
            let p = app_dir().join("icons").join(format!("{}.png", safe_name(&name)));
            let (icon, icon_v) = match fs::metadata(&p).and_then(|m| m.modified()) {
                Ok(t) => (p.to_string_lossy().to_string(), t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)),
                Err(_) => (String::new(), 0),
            };
            GameInfo { name, icon, icon_v }
        }).collect()
    }

    /// Removes a wrongly detected "game" from the list (its clips stay where they are).
    /// `never` = also never treat it as a game again (clip names, game folders, Game audio track).
    pub fn forget_game(&self, name: &str, never: bool) {
        let path = app_dir().join("games.json");
        let mut seen: std::collections::BTreeMap<String, String> = fs::read_to_string(&path).ok()
            .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        seen.retain(|k, _| !k.eq_ignore_ascii_case(name));
        let _ = fs::write(&path, serde_json::to_string(&seen).unwrap_or_default());
        let _ = fs::remove_file(app_dir().join("icons").join(format!("{}.png", safe_name(name))));
        let mut c = self.cfg.lock().unwrap();
        c.games.retain(|g| !g.name.eq_ignore_ascii_case(name));
        c.not_games.retain(|g| !g.eq_ignore_ascii_case(name));
        if never {
            c.not_games.push(name.to_string());
        }
        let _ = c.save();
        set_mixer(&c);
        log_line(&format!("games: removed '{name}'{}", if never { " (never a game)" } else { "" }));
    }

    /// Seconds into the long recording (None = not recording one).
    pub fn session_secs(&self) -> Option<u64> {
        self.session.lock().unwrap().as_ref().map(|s| s.started.elapsed().as_secs())
    }

    /// Start a long (full-session) recording: finished buffer segments are copied aside until stop_session().
    pub fn start_session(&self) -> Result<(), String> {
        let mut g = self.session.lock().unwrap();
        if g.is_some() {
            return Err("already recording".into());
        }
        if !self.is_recording() {
            return Err("the recorder isn't running".into());
        }
        let dir = self.clips_dir().join(".session");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).map_err(err)?;
        let stop = Arc::new(AtomicBool::new(false));
        let (st, d, buf) = (stop.clone(), dir.clone(), self.buf_dir.clone());
        let since = SystemTime::now() - Duration::from_secs(SEG as u64);
        let worker = std::thread::spawn(move || {
            let mut last = since;
            let mut n = 0u32;
            loop {
                let done = st.load(Ordering::Relaxed);
                if done {
                    std::thread::sleep(Duration::from_millis(SEG as u64 * 1000 + 500)); // let the current segment finish
                }
                let mut segs: Vec<(SystemTime, PathBuf)> = fs::read_dir(&buf).map(|rd| rd.flatten().map(|e| e.path())
                    .filter(|p| p.extension().map_or(false, |x| x == "ts"))
                    .filter_map(|p| modified(&p).map(|t| (t, p))).collect()).unwrap_or_default();
                segs.sort();
                if !done {
                    segs.pop(); // newest is still being written
                }
                let from = last;
                for (t, p) in segs.into_iter().filter(|(t, _)| *t > from) {
                    if fs::copy(&p, d.join(format!("{n:06}.ts"))).is_ok() {
                        n += 1;
                    }
                    last = t;
                }
                if done {
                    break;
                }
                std::thread::sleep(Duration::from_millis(700));
            }
        });
        *g = Some(Session { dir, started: Instant::now(), stop, worker: Some(worker) });
        log_line("session: manual recording started");
        Ok(())
    }

    /// Stop the long recording and join it into one clip. Returns the clip path.
    pub fn stop_session(&self) -> Result<String, String> {
        let mut s = self.session.lock().unwrap().take().ok_or("not recording")?;
        s.stop.store(true, Ordering::Relaxed);
        if let Some(w) = s.worker.take() {
            let _ = w.join();
        }
        let mut parts: Vec<PathBuf> = fs::read_dir(&s.dir).map_err(err)?.flatten().map(|e| e.path()).collect();
        parts.sort();
        if parts.is_empty() {
            let _ = fs::remove_dir_all(&s.dir);
            return Err("nothing was recorded".into());
        }
        let cfg = self.settings();
        let game = detect_game();
        let prefix = if cfg.name_by_game { game.clone() } else { None }.unwrap_or_else(|| "Recording".into());
        let dir = match game.map(|g| cfg.game_folder(&g)).filter(|g| !g.is_empty() && cfg.game_folders && cfg.game_rule(g).folder && !RESERVED_DIRS.iter().any(|r| r.eq_ignore_ascii_case(g))) {
            Some(g) => self.clips_dir().join(g.trim_end_matches('.')),
            None => self.clips_dir(),
        };
        fs::create_dir_all(&dir).map_err(err)?;
        let out = dir.join(format!("{prefix} {} (Echo).mp4", chrono::Local::now().format("%Y-%m-%d %H-%M-%S")));
        let list: Vec<(SystemTime, PathBuf)> = parts.into_iter().map(|p| (SystemTime::UNIX_EPOCH, p)).collect();
        let tmp = out.with_extension("saving.tmp");
        let ok = self.join(&list, &tmp)? && fs::rename(&tmp, &out).is_ok();
        let _ = fs::remove_dir_all(&s.dir);
        if !ok {
            return Err("ffmpeg failed to save the recording".into());
        }
        let out = out.to_string_lossy().to_string();
        log_line(&format!("session: {} min -> {out}", s.started.elapsed().as_secs() / 60));
        let _ = self.thumb(&out);
        Ok(out)
    }

    /// Names of clips that were shared (for the cloud badge): old links from the removed "own R2 bucket"
    /// sharing (.thumbs/shared.json, they keep working) + links made through the cloud.
    pub fn shared_clips(&self) -> Vec<String> {
        let m: std::collections::BTreeMap<String, String> = fs::read_to_string(self.thumbs_dir().join("shared.json")).ok()
            .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let mut v: Vec<String> = m.keys().filter_map(|k| k.split('|').next().map(String::from)).collect();
        // links made through the cloud ("Copy share link") that still work
        let now = now_unix();
        for (rel, e) in self.links() {
            if e.expires == 0 || e.expires > now {
                if let Some(n) = Path::new(&rel).file_stem() { v.push(n.to_string_lossy().to_string()); }
            }
        }
        v.sort();
        v.dedup();
        v
    }

    /// Cheap fingerprint of everything the library shows (folder + file change times). The UI asks for
    /// this every few seconds and only reloads the clip list, metadata, cloud list etc. when it changed.
    pub fn library_stamp(&self) -> String {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let mut add = |p: &Path| {
            p.hash(&mut h);
            if let Ok(m) = fs::metadata(p) {
                m.len().hash(&mut h);
                if let Ok(t) = m.modified() { t.hash(&mut h); }
            }
        };
        for (_, d) in self.clip_dirs() {
            add(&d);
        }
        let th = self.thumbs_dir();
        add(&self.clips_dir().join("Screenshots"));
        add(&th);
        for f in ["cloud.json", "meta.json", "favorites.json", "shared.json", "links.json"] {
            add(&th.join(f));
        }
        add(&app_dir().join("games.json"));
        add(&app_dir().join("icons"));
        add(&crate::settings::Settings::path());
        format!("{:x}", h.finish())
    }

    pub fn list_clips(&self) -> Vec<ClipInfo> {
        let mut v: Vec<ClipInfo> = self.clip_dirs().into_iter().flat_map(|(folder, dir)| fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().map_or(false, |x| x.eq_ignore_ascii_case("mp4")))
                    .filter_map(|p| {
                        let m = fs::metadata(&p).ok()?;
                        Some(ClipInfo {
                            name: p.file_stem()?.to_string_lossy().into(),
                            folder: folder.clone(),
                            path: p.to_string_lossy().into(),
                            size: m.len(),
                            modified: m.modified().ok()?.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()).collect();
        v.sort_by(|a, b| b.modified.cmp(&a.modified));
        v
    }

    /// Editor project of an exported video (what it was made from + every edit), so it can be edited again.
    pub fn save_project(&self, clip: &str, data: &str) -> Result<(), String> {
        let stem = Path::new(clip).file_stem().ok_or("bad clip path")?.to_string_lossy().to_string();
        fs::create_dir_all(self.thumbs_dir()).map_err(err)?;
        fs::write(self.thumbs_dir().join(format!("{stem}.project.json")), data).map_err(err)
    }
    pub fn load_project(&self, clip: &str) -> Option<String> {
        let stem = Path::new(clip).file_stem()?.to_string_lossy().to_string();
        fs::read_to_string(self.thumbs_dir().join(format!("{stem}.project.json"))).ok()
    }
    /// (width, height, has sound, seconds) of any video / sound file
    pub fn probe_media(&self, path: &str) -> (u32, u32, bool, f64) {
        let (w, h, a) = self.probe(Path::new(path));
        let d = hidden("ffprobe").args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"]).arg(path)
            .output().ok().and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok()).unwrap_or(0.0);
        (w, h, a, d)
    }

    /// Bookmarks (seconds into the clip) saved next to its thumbnail.
    pub fn write_marks(&self, clip: &str, marks: &[f64]) {
        if let Some(stem) = Path::new(clip).file_stem() {
            let _ = fs::create_dir_all(self.thumbs_dir());
            let _ = fs::write(self.thumbs_dir().join(format!("{}.marks.json", stem.to_string_lossy())), serde_json::to_string(marks).unwrap_or_default());
        }
    }
    pub fn marks(&self, clip: &str) -> Vec<f64> {
        Path::new(clip).file_stem()
            .and_then(|s| fs::read_to_string(self.thumbs_dir().join(format!("{}.marks.json", s.to_string_lossy()))).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Screenshots (<clips>/Screenshots/*.png|jpg), newest first.
    pub fn list_shots(&self) -> Vec<ClipInfo> {
        let mut v: Vec<ClipInfo> = fs::read_dir(self.clips_dir().join("Screenshots"))
            .map(|rd| {
                rd.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().map_or(false, |x| x.eq_ignore_ascii_case("png") || x.eq_ignore_ascii_case("jpg")))
                    .filter_map(|p| {
                        let m = fs::metadata(&p).ok()?;
                        Some(ClipInfo {
                            name: p.file_stem()?.to_string_lossy().into(),
                            folder: String::new(),
                            path: p.to_string_lossy().into(),
                            size: m.len(),
                            modified: m.modified().ok()?.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by(|a, b| b.modified.cmp(&a.modified));
        v
    }

    /// Delete a clip (to the Recycle Bin) and its cached preview.
    pub fn delete_clip(&self, path: &str) -> Result<(), String> {
        let p = PathBuf::from(path);
        let shot = p.parent() == Some(self.clips_dir().join("Screenshots").as_path());
        if !self.is_clip(&p) && !shot {
            return Err("not a Clipper clip".into());
        }
        #[cfg(windows)]
        {
            let mut ok = crate::winutil::recycle(path);
            if !ok {
                // usually a thumbnail / filmstrip / audio scan of ours still has the file open: stop those
                // helpers (never the recorder itself) and retry
                let keep: Vec<u32> = self.child.lock().unwrap().as_ref().map(|c| c.id()).into_iter().collect();
                crate::winutil::kill_children(&["ffmpeg.exe", "ffprobe.exe"], &keep);
                std::thread::sleep(Duration::from_millis(500));
                ok = crate::winutil::recycle(path);
            }
            if !ok {
                return Err("Couldn't delete - the clip is open somewhere else (close the player / editor and try again)".into());
            }
        }
        #[cfg(not(windows))]
        {
            // to the Trash (KDE / freedesktop), plain delete if gio is missing
            let ok = sys_cmd("gio").arg("trash").arg(&p).stderr(Stdio::null()).status().map_or(false, |s| s.success());
            if !ok { fs::remove_file(&p).map_err(err)?; }
        }
        if !shot {
            self.remove_if_empty(p.parent());
        }
        if let Some(stem) = p.file_stem().filter(|_| !shot) {
            let stem = stem.to_string_lossy();
            let _ = fs::remove_file(self.thumbs_dir().join(format!("{stem}.jpg")));
            let _ = self.set_favorite(&stem, false);
            let _ = fs::remove_file(self.thumbs_dir().join(format!("{stem}.dur")));
            let _ = fs::remove_file(self.thumbs_dir().join(format!("{stem}.thumbt")));
            let _ = fs::remove_file(self.thumbs_dir().join(format!("{stem}.strip.jpg")));
            let _ = fs::remove_file(self.thumbs_dir().join(format!("{stem}.strip3.jpg")));
            for t in 0..6 {
                let _ = fs::remove_file(self.thumbs_dir().join(format!("{stem}.a{t}.m4a")));
            }
            let _ = fs::remove_file(self.thumbs_dir().join(format!("{stem}.levels.json")));
            let _ = fs::remove_file(self.thumbs_dir().join(format!("{stem}.marks.json")));
            let _ = fs::remove_file(self.thumbs_dir().join(format!("{stem}.words.json")));
            let mut meta = self.meta_all();
            if meta.remove(stem.as_ref()).is_some() {
                let _ = self.write_meta(&meta);
            }
            // shrunk Discord copies
            if let (Some(name), Ok(rd)) = (p.file_name(), fs::read_dir(self.thumbs_dir().join("discord"))) {
                for d in rd.flatten() {
                    let _ = fs::remove_file(d.path().join(name));
                }
            }
        }
        Ok(())
    }

    /// Rename a clip (and its cached preview). Returns the new path.
    pub fn rename_clip(&self, path: &str, name: &str) -> Result<String, String> {
        let name: String = name.trim().chars().filter(|c| !r#"\/:*?"<>|"#.contains(*c)).collect();
        if name.is_empty() {
            return Err("empty name".into());
        }
        let p = PathBuf::from(path);
        let new = p.with_file_name(format!("{name}.mp4"));
        if new.exists() {
            return Err("a clip with that name already exists".into());
        }
        let old_rel = self.rel_of(path);
        rename_patient(&p, &new)?;
        if let (Some(o), Some(n)) = (old_rel, self.rel_of(&new.to_string_lossy())) {
            self.cloud_follow_move(o, n);
        }
        if let Some(old) = p.file_stem() {
            let old = old.to_string_lossy();
            if self.favorites().iter().any(|x| x == old.as_ref()) {
                let _ = self.set_favorite(&old, false);
                let _ = self.set_favorite(&name, true);
            }
            let mut meta = self.meta_all();
            if let Some(m) = meta.remove(old.as_ref()) {
                meta.insert(name.clone(), m);
                let _ = self.write_meta(&meta);
            }
            for ext in ["jpg", "dur", "strip.jpg", "strip2.jpg", "strip3.jpg", "a1.m4a", "a2.m4a", "a3.m4a", "a4.m4a", "a5.m4a", "marks.json", "levels.json", "words.json", "project.json"] {
                let _ = fs::rename(self.thumbs_dir().join(format!("{old}.{ext}")), self.thumbs_dir().join(format!("{name}.{ext}")));
            }
        }
        Ok(new.to_string_lossy().into())
    }

    /// Cut `start..end` (seconds) out of a clip.
    /// fast: stream copy (instant, snaps to the keyframe before `start`, every 2s).
    /// exact: re-encode (AMF, falls back to x264) for frame-accurate cuts.
    /// replace: overwrite the original instead of saving "<name> (trim).mp4".
    /// Re-encode a clip so it fits in `target_mb` megabytes (Discord upload limits). Returns the file to copy:
    /// the original when it already fits, otherwise a cached copy with the same name in .thumbs/discord/<mb>/.
    pub fn shrink_clip(&self, path: &str, target_mb: f64, progress: &dyn Fn(f64)) -> Result<String, String> {
        let src = PathBuf::from(path);
        let limit = (target_mb * 1_000_000.0) as u64;
        let src_meta = fs::metadata(&src).map_err(err)?;
        if src_meta.len() <= limit {
            return Ok(path.into());
        }
        let name = src.file_name().ok_or("bad clip path")?.to_owned();
        let dir = self.thumbs_dir().join("discord").join(format!("{}", target_mb.round() as u64));
        fs::create_dir_all(&dir).map_err(err)?;
        let out = dir.join(&name);
        if let (Ok(m), Ok(sm)) = (fs::metadata(&out), src_meta.modified()) {
            // older copies could have broken (too dark) colours: only reuse ones tagged BT.709
            let colours_ok = hidden("ffprobe").args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=color_space", "-of", "csv=p=0"])
                .arg(&out).output().map_or(false, |o| String::from_utf8_lossy(&o.stdout).trim() == "bt709");
            if m.len() > 0 && m.len() <= limit && m.modified().map_or(false, |t| t >= sm) && colours_ok {
                return Ok(out.to_string_lossy().into());   // made earlier
            }
        }
        let dur = self.thumb(path).map(|t| t.duration).unwrap_or(0.0);
        if dur <= 0.0 {
            return Err("could not read the clip length".into());
        }
        let (_w, h, audio) = self.probe(&src);
        let tmp = dir.join("shrinking.tmp");
        let t0 = Instant::now();
        let mut factor = 0.92;
        for attempt in 0..3 {
            let total = limit as f64 * 8.0 / 1000.0 / dur * factor;   // kbit/s for everything
            let a_kbps = if !audio { 0.0 } else if total > 3000.0 { 160.0 } else if total > 800.0 { 96.0 } else { 64.0 };
            let v_kbps = total - a_kbps;
            if v_kbps < 120.0 {
                return Err(format!("this clip is too long to fit in {} MB - trim it first", target_mb));
            }
            // lower resolution / fps when the bitrate is low, so it stays sharp instead of blocky
            let th = if v_kbps >= 8000.0 { h } else if v_kbps >= 4000.0 { 1080 } else if v_kbps >= 1800.0 { 720 } else if v_kbps >= 800.0 { 540 } else { 360 }.min(h);
            let mut vf: Vec<String> = Vec::new();
            if th < h { vf.push(format!("scale=-2:{th}")); }
            if v_kbps < 1800.0 { vf.push("fps=30".into()); }
            vf.push(TV_YUV.into());
            let b = format!("{}k", v_kbps as u64);
            let buf = format!("{}k", (v_kbps * 2.0) as u64);
            let ab = format!("{}k", a_kbps as u64);
            let run = |venc: &[&str]| -> Result<bool, String> {
                if skip_enc(venc) { return Ok(false); }
                let mut c = ffmpeg();
                c.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(&src).args(["-map", "0:v:0", "-map", "0:a:0?"]);
                match self.watermark_graph("v0", "out", th) {
                    Some(w) => { c.args(["-vf", &format!("[in]{}[v0];{w}", if vf.is_empty() { "null".to_string() } else { vf.join(",") })]); }
                    None => if !vf.is_empty() { c.args(["-vf", &vf.join(",")]); },
                }
                c.args(venc).args(["-b:v", &b, "-maxrate", &b, "-bufsize", &buf, "-pix_fmt", "yuv420p"]).args(TV_TAGS);
                if audio { c.args(["-c:a", "aac", "-b:a", &ab, "-ac", "2"]); }
                let mut child = c.args(["-movflags", "+faststart", "-progress", "pipe:1", "-nostats", "-f", "mp4"]).arg(&tmp)
                    .stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(err)?;
                // ffmpeg reports "out_time_us=<microseconds>" while encoding
                if let Some(out) = child.stdout.take() {
                    use std::io::BufRead;
                    for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                        if let Some(v) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok()) {
                            let f = (v / 1e6 / dur).clamp(0.0, 1.0);
                            progress((attempt as f64 + f) / (attempt as f64 + 1.0));
                        }
                    }
                }
                let st = child.wait().map_err(err)?;
                Ok(st.success() && fs::metadata(&tmp).map_or(false, |m| m.len() > 0))
            };
            progress(0.0);
            let ok = run(&["-c:v", "h264_amf", "-quality", "quality", "-rc", "cbr"])?
                || run(&["-c:v", "h264_nvenc", "-preset", "p5", "-rc", "cbr"])?
                || run(&["-c:v", "h264_qsv"])?
                || run(&["-c:v", "libx264", "-preset", "veryfast"])?;
            if !ok {
                let _ = fs::remove_file(&tmp);
                return Err("ffmpeg could not shrink this clip".into());
            }
            let got = fs::metadata(&tmp).map(|m| m.len()).unwrap_or(u64::MAX);
            log_line(&format!("shrink: {} -> {:.1} MB (target {target_mb} MB, {v_kbps:.0} kbps, {th}p, try {attempt}) in {:.1}s",
                name.to_string_lossy(), got as f64 / 1e6, t0.elapsed().as_secs_f32()));
            if got <= limit {
                let _ = fs::remove_file(&out);
                fs::rename(&tmp, &out).map_err(err)?;
                return Ok(out.to_string_lossy().into());
            }
            factor *= limit as f64 / got as f64 * 0.95;   // overshot: aim lower
        }
        let _ = fs::remove_file(&tmp);
        Err(format!("could not get this clip under {target_mb} MB"))
    }

    /// Import videos dropped onto the window: copy (or convert) them into the clips folder as .mp4.
    pub fn import_clips(&self, paths: &[String]) -> (Vec<String>, Vec<String>) {
        let (mut ok, mut bad) = (Vec::new(), Vec::new());
        let dir = self.clips_dir();
        let _ = fs::create_dir_all(&dir);
        for p in paths {
            let src = PathBuf::from(p);
            // a dropped folder (e.g. Medal's clips folder): import everything in it, game subfolders kept
            if src.is_dir() {
                let (o, _, b) = self.import_folder(p, |_, _, _| {});
                ok.extend(o);
                bad.extend(b);
                continue;
            }
            let name = src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            // sound files (.wav, .mp3…) become playable/editable clips in the "Sounds" folder
            if is_audio_file(&src) {
                match self.import_audio(&src) {
                    Ok(out) => ok.push(out.to_string_lossy().to_string()),
                    Err(e) => bad.push(format!("{name}: {e}")),
                }
                continue;
            }
            if !is_video_file(&src) {
                bad.push(format!("{name}: not a video"));
                continue;
            }
            if self.is_clip(&src) {
                continue;
            }
            match self.import_one(&src, &dir) {
                Ok(out) => ok.push(out.to_string_lossy().to_string()),
                Err(e) => bad.push(format!("{name}: {e}")),
            }
        }
        (ok, bad)
    }

    /// A sound file as a clip: its waveform as the picture (so the player, trim and editor all work on it),
    /// saved in <clips>/Sounds. Keyframe every 0.5 s so quick trims are precise enough for short sound effects.
    fn import_audio(&self, src: &Path) -> Result<PathBuf, String> {
        let dir = self.clips_dir().join("Sounds");
        fs::create_dir_all(&dir).map_err(err)?;
        let stem = src.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let out = (1..).map(|n| dir.join(if n == 1 { format!("{stem}.mp4") } else { format!("{stem} ({n}).mp4") }))
            .find(|p| !p.exists()).unwrap();
        let pic = std::env::temp_dir().join("echo_wave.png");
        let tmp = dir.join(format!("{stem}.importing.tmp"));
        let ok = ffmpeg().args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(src)
            .args(["-filter_complex", "[0:a]aformat=channel_layouts=mono,showwavespic=s=1600x420:colors=0xa78bfa:scale=sqrt[w];color=c=0x15121f:s=1920x1080[bg];[bg][w]overlay=(W-w)/2:(H-h)/2",
                   "-frames:v", "1"]).arg(&pic)
            .stdout(Stdio::null()).stderr(Stdio::null()).status().map_or(false, |s| s.success());
        if !ok {
            return Err("not a sound file Echo can read".into());
        }
        let ok = ffmpeg().args(["-hide_banner", "-loglevel", "error", "-y", "-loop", "1", "-framerate", "30", "-i"]).arg(&pic)
            .arg("-i").arg(src)
            .args(["-map", "0:v", "-map", "1:a:0", "-c:v", "libx264", "-preset", "ultrafast", "-tune", "stillimage", "-g", "15",
                   "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "256k", "-ar", "48000", "-ac", "2", "-shortest",
                   "-movflags", "+faststart", "-f", "mp4"]).arg(&tmp)
            .stdout(Stdio::null()).stderr(Stdio::null()).status().map_or(false, |s| s.success())
            && fs::metadata(&tmp).map_or(false, |m| m.len() > 0);
        let _ = fs::remove_file(&pic);
        if !ok || fs::rename(&tmp, &out).is_err() {
            let _ = fs::remove_file(&tmp);
            return Err("could not convert this sound".into());
        }
        log_line(&format!("import: sound {} -> {}", src.display(), out.display()));
        Ok(out)
    }

    /// Converts / copies one video into `dir` as .mp4 and keeps its original date (the library sorts by it).
    fn import_one(&self, src: &Path, dir: &Path) -> Result<PathBuf, String> {
        let _ = fs::create_dir_all(dir);
        let p = src.to_string_lossy().to_string();
        let stem = src.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let mut n = 1;
        let out = loop {
            let f = if n == 1 { format!("{stem}.mp4") } else { format!("{stem} ({n}).mp4") };
            let q = dir.join(f);
            if !q.exists() { break q; }
            n += 1;
        };
        let tmp = dir.join(format!("{stem}.importing.tmp"));
        // codecs WebView2 can play without converting
        let probe = |sel: &str| -> String {
            hidden("ffprobe").args(["-v", "error", "-select_streams", sel, "-show_entries", "stream=codec_name", "-of", "csv=p=0"])
                .arg(src).output().map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string()).unwrap_or_default()
        };
        let (vc, ac) = (probe("v:0"), probe("a:0"));
        if vc.is_empty() {
            return Err("no video stream".into());
        }
        // full-range video (Medal, phones, some OBS setups): labelled "pc" or simply out of TV range.
        // Players (Linux WebKitGTK, Discord) show those too dark, so convert them to real TV range.
        let vinfo = hidden("ffprobe").args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=pix_fmt,color_range", "-of", "csv=p=0"])
            .arg(src).output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
        let full = vinfo.contains("pc") || vinfo.contains("yuvj") || full_range_share(src) > 0.005;
        let copy_ok = vc == "h264" && !full && vinfo.trim().starts_with("yuv420p") && (ac.is_empty() || ac == "aac" || ac == "mp3");
        let vf = if full { "scale=in_range=pc:out_range=tv:out_color_matrix=bt709,format=yuv420p".to_string() } else { TV_YUV.to_string() };
        let run = |args: &[&str]| -> bool {
            if skip_enc(args) { return false; }
            let reenc = !args.contains(&"copy");
            let mut c = ffmpeg();
            c.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(src)
                .args(["-map", "0:v:0", "-map", "0:a?"]).args(args);
            if reenc {
                c.args(["-vf", &vf]).args(TV_TAGS);
            }
            let st = c.args(["-movflags", "+faststart", "-f", "mp4"]).arg(&tmp)
                .stdout(Stdio::null()).stderr(Stdio::null()).status();
            st.map_or(false, |s| s.success()) && fs::metadata(&tmp).map_or(false, |m| m.len() > 0)
        };
        let aenc = ["-c:a", "aac", "-b:a", "192k", "-ac", "2"];
        let done = (copy_ok && run(&["-c", "copy"]))
            || run(&[&["-c:v", "h264_amf", "-quality", "quality", "-rc", "qvbr", "-qvbr_quality_level", "22"][..], &aenc[..]].concat())
            || run(&[&["-c:v", "h264_nvenc", "-preset", "p5", "-cq", "22"][..], &aenc[..]].concat())
            || run(&[&["-c:v", "h264_qsv", "-global_quality", "22"][..], &aenc[..]].concat())
            || run(&[&["-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-pix_fmt", "yuv420p"][..], &aenc[..]].concat());
        if done && fs::rename(&tmp, &out).is_ok() {
            // keep the date the clip was recorded, not the import time
            if let (Ok(t), Ok(f)) = (fs::metadata(src).and_then(|m| m.modified()), fs::File::options().write(true).open(&out)) {
                let _ = f.set_modified(t);
            }
            log_line(&format!("import: {} -> {} ({vc}/{ac}{}{})", p, out.display(), if copy_ok { ", copied" } else { "" }, if full { ", full range -> tv" } else { "" }));
            Ok(out)
        } else {
            let _ = fs::remove_file(&tmp);
            log_line(&format!("import: failed {p} ({vc}/{ac})"));
            Err("could not convert".into())
        }
    }

    /// Other clipping apps' folders found on this PC (Medal, SteelSeries Moments, Outplayed…) that hold videos.
    pub fn import_sources(&self) -> Vec<ImportSource> {
        let env = |k: &str| std::env::var(k).map(PathBuf::from).unwrap_or_default();
        let (home, appdata, progdata) = (env("USERPROFILE"), env("APPDATA"), env("ProgramData"));
        let vids = home.join("Videos");
        let mut cands: Vec<(&str, PathBuf)> = vec![
            ("Medal", PathBuf::from(r"C:\Medal")), ("Medal", PathBuf::from(r"D:\Medal")), ("Medal", PathBuf::from(r"E:\Medal")),
            ("Medal", vids.join("Medal")), ("Medal", home.join("Medal")),
            ("SteelSeries Moments", vids.join("SteelSeries Moments")), ("SteelSeries Moments", vids.join("SteelSeries")),
            ("SteelSeries Moments", vids.join("Moments")), ("SteelSeries Moments", home.join("SteelSeries Moments")),
            ("Outplayed", vids.join("Overwolf").join("Outplayed")), ("Outplayed", vids.join("Outplayed")),
            ("Insights", vids.join("Insights Capture")),
        ];
        // folders the apps wrote into their own settings (covers a moved capture folder)
        for (app, d) in [("Medal", appdata.join("Medal")), ("SteelSeries Moments", progdata.join("SteelSeries").join("GG"))] {
            let mut found = Vec::new();
            paths_in_settings(&d, 0, &mut found);
            cands.extend(found.into_iter().map(|p| (app, p)));
        }
        let own = self.clips_dir();
        let mut out: Vec<ImportSource> = Vec::new();
        for (app, p) in cands {
            let p = import_root(&p);
            if !p.is_dir() || p.starts_with(&own) || own.starts_with(&p) {
                continue;
            }
            if out.iter().any(|s| p.starts_with(&s.path) || Path::new(&s.path).starts_with(&p)) {
                continue;
            }
            let mut v = Vec::new();
            collect_videos(&p, 0, &mut v);
            if v.is_empty() {
                continue;
            }
            let bytes = v.iter().filter_map(|f| fs::metadata(f).ok()).map(|m| m.len()).sum();
            out.push(ImportSource { app: app.into(), path: p.to_string_lossy().to_string(), count: v.len(), bytes });
        }
        out
    }

    /// Imports every video under `root` (copies; the originals stay). Clips in a subfolder go into the Echo folder
    /// with that name (Medal / Moments keep one folder per game). Already imported files are skipped.
    /// Returns (imported clips, skipped count, failures).
    pub fn import_folder(&self, root: &str, progress: impl Fn(usize, usize, &str)) -> (Vec<String>, usize, Vec<String>) {
        let root = import_root(Path::new(root));
        let own = self.clips_dir();
        let mut files = Vec::new();
        collect_videos(&root, 0, &mut files);
        files.retain(|f| !f.starts_with(&own));
        // oldest first, so a half-finished import still has a sensible order
        files.sort_by_key(|f| modified(f));
        let list = self.thumbs_dir().join("imported.json");
        let mut seen: std::collections::HashSet<String> = fs::read_to_string(&list).ok()
            .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let cfg = self.settings();
        let (mut ok, mut skipped, mut bad) = (Vec::new(), 0usize, Vec::new());
        let total = files.len();
        // don't fill the disk: everything still to copy + 5 GB spare must fit
        #[cfg(windows)]
        {
            let need: u64 = files.iter().filter_map(|f| fs::metadata(f).ok().map(|m| (f, m.len())))
                .filter(|(f, n)| !seen.contains(&format!("{}|{}", f.to_string_lossy().to_lowercase(), n))).map(|(_, n)| n).sum();
            let free = crate::winutil::disk_free(&own.to_string_lossy());
            if free > 0 && need + 5_000_000_000 > free {
                let gb = |b: u64| b as f64 / 1e9;
                log_line(&format!("import: not enough space ({:.1} GB needed, {:.1} GB free)", gb(need), gb(free)));
                bad.push(format!("Not enough space: these clips need {:.0} GB but only {:.0} GB is free on the clips drive. Free up space or pick a clips folder on a bigger drive (Settings > Clips & Storage).", gb(need), gb(free)));
                return (ok, skipped, bad);
            }
        }
        for (i, f) in files.iter().enumerate() {
            let name = f.file_name().unwrap_or_default().to_string_lossy().to_string();
            progress(i, total, &name);
            let key = format!("{}|{}", f.to_string_lossy().to_lowercase(), fs::metadata(f).map(|m| m.len()).unwrap_or(0));
            if seen.contains(&key) {
                skipped += 1;
                continue;
            }
            // first folder under the root = the game (Medal: Clips\<Game>\…, Moments: <Game>\…)
            let rel = f.strip_prefix(&root).unwrap_or(f);
            let game = rel.components().rev().skip(1).last()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .filter(|g| !["clips", "videos", "recordings", "captures", "moments", "exported clips", "exports", "exported", "highlights"].contains(&g.to_lowercase().as_str()))
                .unwrap_or_default();
            // settings re-read per clip: a folder renamed / merged during the import is followed
            let c = self.settings();
            let game = if game.is_empty() { game } else { self.guess_game(&game).unwrap_or(game) };
            let folder = if game.is_empty() || !c.game_folders { String::new() } else { c.game_folder(&game) };
            let dir = if folder.is_empty() { own.clone() } else { own.join(&folder) };
            match self.import_one(f, &dir) {
                Ok(out) => {
                    ok.push(out.to_string_lossy().to_string());
                    seen.insert(key);
                    if ok.len() % 10 == 0 {
                        let _ = fs::write(&list, serde_json::to_string(&seen).unwrap_or_default());
                    }
                }
                Err(e) => bad.push(format!("{name}: {e}")),
            }
        }
        let _ = fs::create_dir_all(self.thumbs_dir());
        let _ = fs::write(&list, serde_json::to_string(&seen).unwrap_or_default());
        // clips that came without a game folder: sort them by the game in their name
        if cfg.game_folders && !ok.is_empty() {
            self.sort_into_folders();
        }
        progress(total, total, "");
        log_line(&format!("import: {} from {} ({} new, {skipped} already there, {} failed)", total, root.display(), ok.len(), bad.len()));
        (ok, skipped, bad)
    }

    /// Save the frame at `t` seconds as a PNG in <clips>/Screenshots.
    pub fn frame_png(&self, path: &str, t: f64) -> Result<String, String> {
        let src = PathBuf::from(path);
        let stem = src.file_stem().ok_or("bad clip path")?.to_string_lossy().to_string();
        let dir = self.clips_dir().join("Screenshots");
        fs::create_dir_all(&dir).map_err(err)?;
        let out = dir.join(format!("{stem} @{:.1}s.png", t.max(0.0)));
        let ok = ffmpeg()
            .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{:.3}", t.max(0.0)), "-i"])
            .arg(&src).args(["-frames:v", "1", "-update", "1"]).arg(&out)
            .stdout(Stdio::null()).stderr(Stdio::null()).status().map_err(err)?.success();
        if ok && out.exists() { Ok(out.to_string_lossy().to_string()) } else { Err("could not grab this frame".into()) }
    }

    /// Save the clip's sound (mix track) as MP3 in <clips>/Audio.
    pub fn audio_mp3(&self, path: &str) -> Result<String, String> {
        self.audio_file(path, false)
    }

    /// The clip's sound (mix track) as <clips>/Audio/<name>.mp3 or .wav (16-bit 48 kHz, for sound effects).
    pub fn audio_file(&self, path: &str, wav: bool) -> Result<String, String> {
        let src = PathBuf::from(path);
        let stem = src.file_stem().ok_or("bad clip path")?.to_string_lossy().to_string();
        let dir = self.clips_dir().join("Audio");
        fs::create_dir_all(&dir).map_err(err)?;
        let out = dir.join(format!("{stem}.{}", if wav { "wav" } else { "mp3" }));
        let enc: &[&str] = if wav { &["-c:a", "pcm_s16le", "-ar", "48000"] } else { &["-c:a", "libmp3lame", "-b:a", "192k"] };
        let ok = ffmpeg()
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(&src).args(["-map", "0:a:0", "-vn"]).args(enc).arg(&out)
            .stdout(Stdio::null()).stderr(Stdio::null()).status().map_err(err)?.success();
        if ok && out.exists() { Ok(out.to_string_lossy().to_string()) } else { Err("could not export the audio".into()) }
    }

    /// Join several clips (in the given order) into "Montage <date>.mp4". Re-encodes so mixed sizes work.
    pub fn combine_clips(&self, paths: &[String]) -> Result<String, String> {
        if paths.len() < 2 {
            return Err("select at least 2 clips".into());
        }
        let probe = hidden("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height,avg_frame_rate", "-of", "csv=p=0"])
            .arg(&paths[0]).output().map_err(err)?;
        let s = String::from_utf8_lossy(&probe.stdout).trim().to_string();
        let f: Vec<&str> = s.split(',').collect();
        let w: u32 = f.first().and_then(|x| x.parse().ok()).unwrap_or(1920);
        let h: u32 = f.get(1).and_then(|x| x.parse().ok()).unwrap_or(1080);
        let fps = f.get(2).and_then(|x| {
            let mut p = x.split('/');
            let a: f64 = p.next()?.parse().ok()?;
            let b: f64 = p.next().unwrap_or("1").parse().ok()?;
            if b > 0.0 { Some((a / b).round()) } else { None }
        }).filter(|v| *v >= 10.0 && *v <= 240.0).unwrap_or(60.0);
        let (w, h) = (w / 2 * 2, h / 2 * 2);
        let mut fc = String::new();
        for i in 0..paths.len() {
            fc += &format!("[{i}:v:0]scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps={fps},format=yuv420p[v{i}];[{i}:a:0]aresample=48000,aformat=channel_layouts=stereo[a{i}];");
        }
        for i in 0..paths.len() {
            fc += &format!("[v{i}][a{i}]");
        }
        fc += &format!("concat=n={}:v=1:a=1[v0][a]", paths.len());
        match self.watermark_graph("v0", "v", h) {
            Some(w) => fc += &format!(";{w}"),
            None => fc += ";[v0]null[v]",
        }
        let dir = self.clips_dir();
        let out = dir.join(format!("Montage {}", chrono::Local::now().format("%Y-%m-%d %H-%M-%S.mp4")));
        let tmp = dir.join("montage.tmp");
        let kb = ((w as u64 * h as u64 * fps as u64) / 6000).clamp(6000, 60000);
        let b = format!("{kb}k");
        let mut last = String::new();
        let encs: [&[&str]; 4] = [
            &["-c:v", "h264_amf", "-quality", "quality", "-b:v", b.as_str(), "-maxrate", b.as_str()],
            &["-c:v", "h264_nvenc", "-preset", "p5", "-b:v", b.as_str(), "-maxrate", b.as_str()],
            &["-c:v", "h264_qsv", "-b:v", b.as_str(), "-maxrate", b.as_str()],
            &["-c:v", "libx264", "-preset", "veryfast", "-crf", "20"],
        ];
        for enc in encs {
            let mut c = ffmpeg();
            c.args(["-hide_banner", "-loglevel", "error", "-y"]);
            for p in paths {
                c.arg("-i").arg(p);
            }
            let o = c.args(["-filter_complex", &fc, "-map", "[v]", "-map", "[a]"]).args(enc)
                .args(["-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart", "-f", "mp4"]).arg(&tmp)
                .stdout(Stdio::null()).stderr(Stdio::piped()).output().map_err(err)?;
            if o.status.success() && fs::metadata(&tmp).map_or(false, |m| m.len() > 0) {
                fs::rename(&tmp, &out).map_err(err)?;
                log_line(&format!("montage: {} clips -> {}", paths.len(), out.display()));
                return Ok(out.to_string_lossy().to_string());
            }
            last = String::from_utf8_lossy(&o.stderr).trim().to_string();
        }
        let _ = fs::remove_file(&tmp);
        log_line(&format!("montage: failed: {last}"));
        Err("could not combine these clips".into())
    }

    /// Make a GIF of [start, end] into <clips>/GIFs/. Shrinks until it is under ~10 MB.
    pub fn make_gif(&self, path: &str, start: f64, end: f64) -> Result<String, String> {
        let src = PathBuf::from(path);
        let len = (end - start).min(30.0);
        if !(start >= 0.0 && len >= 0.3) {
            return Err("selection is too short".into());
        }
        let stem = src.file_stem().ok_or("bad clip path")?.to_string_lossy().to_string();
        let dir = self.clips_dir().join("GIFs");
        fs::create_dir_all(&dir).map_err(err)?;
        let mut n = 1;
        let out = loop {
            let name = if n == 1 { format!("{stem}.gif") } else { format!("{stem} ({n}).gif") };
            let p = dir.join(name);
            if !p.exists() { break p; }
            n += 1;
        };
        let tmp = dir.join(format!("{stem}.giftmp"));
        let mut last = String::new();
        for (w, fps) in [(640, 20), (480, 15), (360, 12), (270, 10)] {
            let head = match self.watermark_graph("g0", "g1", w * 9 / 16) {
                Some(x) => format!("[0:v]fps={fps},scale={w}:-2:flags=lanczos[g0];{x};[g1]"),
                None => format!("[0:v]fps={fps},scale={w}:-2:flags=lanczos,"),
            };
            let vf = format!("{head}split[a][b];[a]palettegen=max_colors=192:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle");
            let o = ffmpeg()
                .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{start:.3}"), "-t", &format!("{len:.3}"), "-i"])
                .arg(&src)
                .args(["-filter_complex", &vf, "-loop", "0", "-f", "gif"])
                .arg(&tmp)
                .stdout(Stdio::null()).stderr(Stdio::piped()).output().map_err(err)?;
            let size = fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
            log_line(&format!("gif: {w}px {fps}fps {len:.1}s -> {} KB", size / 1024));
            if !o.status.success() || size == 0 {
                last = String::from_utf8_lossy(&o.stderr).trim().to_string();
                continue;
            }
            if size <= 10 * 1024 * 1024 || w == 270 {
                fs::rename(&tmp, &out).map_err(err)?;
                return Ok(out.to_string_lossy().to_string());
            }
        }
        let _ = fs::remove_file(&tmp);
        log_line(&format!("gif: failed: {last}"));
        Err("could not make a GIF of this clip".into())
    }

    pub fn trim_clip(&self, path: &str, start: f64, end: f64, exact: bool, replace: bool) -> Result<String, String> {
        let src = PathBuf::from(path);
        if !self.is_clip(&src) {
            return Err("not a Clipper clip".into());
        }
        let len = end - start;
        if !(start >= 0.0 && len >= 0.3) {
            return Err("selection is too short".into());
        }
        let stem = src.file_stem().ok_or("bad clip path")?.to_string_lossy().to_string();
        let dir = src.parent().map(|d| d.to_path_buf()).unwrap_or_else(|| self.clips_dir());
        let tmp = dir.join(format!("{stem}.trimming.tmp"));
        let out = if replace {
            src.clone()
        } else {
            let mut n = 1;
            loop {
                let name = if n == 1 { format!("{stem} (trim).mp4") } else { format!("{stem} (trim {n}).mp4") };
                let p = dir.join(name);
                if !p.exists() {
                    break p;
                }
                n += 1;
            }
        };

        let run = |venc: &[&str]| -> Result<bool, String> {
            if skip_enc(venc) { return Ok(false); }
            let mut c = ffmpeg();
            c.args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{start:.3}"), "-i"])
                .arg(&src)
                .args(["-t", &format!("{len:.3}"), "-map", "0:v:0", "-map", "0:a?"]);
            if venc.is_empty() {
                c.args(["-c", "copy", "-avoid_negative_ts", "make_zero"]);
            } else {
                c.args(venc).args(["-c:a", "aac", "-b:a", "192k"]);
            }
            let st = c.args(["-movflags", "+faststart", "-f", "mp4"]).arg(&tmp)
                .stdout(Stdio::null()).stderr(Stdio::null()).status().map_err(err)?;
            Ok(st.success() && fs::metadata(&tmp).map_or(false, |m| m.len() > 0))
        };

        let ok = if exact {
            // keep roughly the source bitrate
            let dur = self.thumb(path).map(|t| t.duration).unwrap_or(0.0);
            let size = fs::metadata(&src).map(|m| m.len()).unwrap_or(0) as f64;
            let kbps = if dur > 0.0 { ((size * 8.0 / dur / 1000.0) as u64).clamp(4000, 80000) } else { 20000 };
            let b = format!("{kbps}k");
            run(&["-c:v", "h264_amf", "-quality", "quality", "-b:v", &b, "-maxrate", &b])?
                || run(&["-c:v", "h264_nvenc", "-preset", "p5", "-b:v", &b, "-maxrate", &b])?
                || run(&["-c:v", "h264_qsv", "-b:v", &b, "-maxrate", &b])?
                || run(&["-c:v", "libx264", "-preset", "veryfast", "-crf", "18"])?
        } else {
            run(&[])?
        };
        if !ok {
            let _ = fs::remove_file(&tmp);
            return Err("ffmpeg could not trim this clip".into());
        }

        if replace {
            // the player may still hold the file for a moment
            let mut done = false;
            for _ in 0..20 {
                if fs::remove_file(&src).is_ok() || !src.exists() {
                    done = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(150));
            }
            if !done {
                let _ = fs::remove_file(&tmp);
                return Err("original is in use - close it and try again".into());
            }
            for ext in ["jpg", "dur", "strip.jpg", "strip2.jpg", "strip3.jpg", "a1.m4a", "a2.m4a"] {
                let _ = fs::remove_file(self.thumbs_dir().join(format!("{stem}.{ext}")));
            }
        }
        fs::rename(&tmp, &out).map_err(err)?;
        let out = out.to_string_lossy().to_string();
        let _ = self.thumb(&out);
        // replaced a clip that's in the cloud: upload the cut version over it
        if replace && self.cloud_on() {
            if let Some(rel) = self.rel_of(&out) {
                if self.cloud_state().files.iter().any(|f| f.rel == rel) {
                    let _ = self.cloud_upload(&out);
                }
            }
        }
        Ok(out)
    }

    /// What leaves the PC (Discord copy, cloud upload, share link): AV1 clips as a cached H.264 copy in .thumbs/h264/
    /// (plays on every phone and browser), every other clip as itself.
    pub fn shareable(&self, path: &str) -> Result<String, String> {
        if !is_av1(path) {
            return Ok(path.into());
        }
        let src = PathBuf::from(path);
        let dir = self.thumbs_dir().join("h264");
        fs::create_dir_all(&dir).map_err(err)?;
        let name = src.file_name().ok_or("bad clip path")?;
        let out = dir.join(name);
        let sm = fs::metadata(&src).map_err(err)?;
        if let Ok(m) = fs::metadata(&out) {
            if m.len() > 0 && m.modified().ok() >= sm.modified().ok() {
                return Ok(out.to_string_lossy().into());
            }
        }
        let dur = self.thumb(path).map(|t| t.duration).unwrap_or(0.0).max(1.0);
        // H.264 needs about twice the bitrate of AV1 for the same quality
        let kbps = ((sm.len() as f64 * 8.0 / dur / 1000.0 - 192.0 * self.audio_tracks(path).max(1) as f64) * 2.0).clamp(4000.0, 60000.0) as u64;
        let b = format!("{kbps}k");
        let tmp = dir.join(format!("{}.tmp", src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()));
        let t0 = Instant::now();
        let run = |venc: &[&str]| -> bool {
            if skip_enc(venc) { return false; }
            ffmpeg().args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(&src)
                .args(["-map", "0", "-c:a", "copy"]).args(venc)
                .args(["-movflags", "+faststart", "-f", "mp4"]).arg(&tmp)
                .stdout(Stdio::null()).stderr(Stdio::null()).status().map_or(false, |s| s.success())
                && fs::metadata(&tmp).map_or(false, |m| m.len() > 0)
        };
        let ok = run(&["-c:v", "h264_amf", "-quality", "quality", "-b:v", &b, "-maxrate", &b])
            || run(&["-c:v", "h264_nvenc", "-preset", "p5", "-b:v", &b, "-maxrate", &b])
            || run(&["-c:v", "h264_qsv", "-b:v", &b, "-maxrate", &b])
            || run(&["-c:v", "libx264", "-preset", "veryfast", "-b:v", &b, "-maxrate", &b, "-bufsize", &b, "-pix_fmt", "yuv420p"]);
        if !ok {
            let _ = fs::remove_file(&tmp);
            return Err("Couldn't make a playable copy of this AV1 clip - see the log".into());
        }
        let _ = fs::remove_file(&out);
        fs::rename(&tmp, &out).map_err(err)?;
        log_line(&format!("av1: H.264 copy of {} for sharing in {:.0}s", name.to_string_lossy(), t0.elapsed().as_secs_f32()));
        Ok(out.to_string_lossy().into())
    }

    /// Make a clip smaller in place: re-encode the video at `factor` of its bitrate (all audio tracks kept, length unchanged).
    /// av1: use the GPU's AV1 encoder when av1_ready() (same quality at `factor` 0.5); the cloud copy then stays H.264.
    pub fn compress_clip(&self, path: &str, factor: f64, av1: bool, progress: &dyn Fn(f64)) -> Result<(u64, u64), String> {
        let src = PathBuf::from(path);
        if !self.is_clip(&src) {
            return Err("not a Clipper clip".into());
        }
        // AV1 already: converting again gains nothing, and H.264 at half its bitrate would look much worse
        if is_av1(path) {
            return Err("This clip is already AV1 - it's as small as it gets".into());
        }
        let before = fs::metadata(&src).map_err(err)?.len();
        let mtime = fs::metadata(&src).and_then(|m| m.modified()).ok();
        let dur = self.thumb(path)?.duration.max(1.0);
        let kbps = ((before as f64 * 8.0 / dur / 1000.0 - 192.0 * self.audio_tracks(path).max(1) as f64) * factor.clamp(0.1, 0.9)).max(800.0) as u64;
        let b = format!("{kbps}k");
        let tmp = src.with_extension("compress.tmp");
        let t0 = Instant::now();
        // ffmpeg reports "out_time_us=<microseconds>" on stdout while encoding -> progress 0..1
        let run = |venc: &[&str]| -> bool {
            if skip_enc(venc) { return false; }
            progress(0.0);
            let child = ffmpeg().args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(&src)
                .args(["-map", "0", "-c:a", "copy"]).args(venc)
                .args(["-movflags", "+faststart", "-progress", "pipe:1", "-nostats", "-f", "mp4"]).arg(&tmp)
                .stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
            let Ok(mut child) = child else { return false };
            if let Some(out) = child.stdout.take() {
                use std::io::BufRead;
                for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                    if let Some(v) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok()) {
                        progress((v / 1e6 / dur).clamp(0.0, 1.0));
                    }
                }
            }
            child.wait().map_or(false, |s| s.success()) && fs::metadata(&tmp).map_or(false, |m| m.len() > 0)
        };
        let enc = if av1 && av1_ready() { av1_encoder() } else { None };
        let peak = format!("{}k", kbps * 3 / 2);
        // factor 0 = "steady quality": calm clips shrink a lot, busy ones less, every clip looks about the same
        // (auto shrink of old clips). The ceiling is the clip's own bitrate, so nothing gets bigger.
        let quality = factor <= 0.0 && enc.is_none();
        let orig = format!("{}k", (before as f64 * 8.0 / dur / 1000.0) as u64);
        let orig2 = format!("{}k", (before as f64 * 16.0 / dur / 1000.0) as u64);
        let ok = if quality {
            run(&["-c:v", "h264_amf", "-quality", "quality", "-rc", "qvbr", "-qvbr_quality_level", "27", "-b:v", &orig, "-maxrate", &orig])
                || run(&["-c:v", "h264_nvenc", "-preset", "p5", "-rc", "vbr", "-cq", "27", "-b:v", "0", "-maxrate", &orig, "-bufsize", &orig2])
                || run(&["-c:v", "h264_qsv", "-global_quality", "27"])
                || run(&["-c:v", "libx264", "-preset", "veryfast", "-crf", "24", "-maxrate", &orig, "-bufsize", &orig2, "-pix_fmt", "yuv420p"])
        } else { match enc {
            Some("av1_amf") => run(&["-c:v", "av1_amf", "-quality", "quality", "-rc", "vbr_peak", "-b:v", &b, "-maxrate", &peak, "-pix_fmt", "yuv420p"]),
            Some("av1_nvenc") => run(&["-c:v", "av1_nvenc", "-preset", "p5", "-rc", "vbr", "-b:v", &b, "-maxrate", &peak, "-pix_fmt", "yuv420p"]),
            Some(e) => run(&["-c:v", e, "-b:v", &b, "-maxrate", &peak, "-pix_fmt", "yuv420p"]),
            None => run(&["-c:v", "h264_amf", "-quality", "quality", "-b:v", &b, "-maxrate", &b])
                || run(&["-c:v", "h264_nvenc", "-preset", "p5", "-b:v", &b, "-maxrate", &b])
                || run(&["-c:v", "h264_qsv", "-b:v", &b, "-maxrate", &b])
                || run(&["-c:v", "libx264", "-preset", "veryfast", "-b:v", &b, "-maxrate", &b, "-bufsize", &b]),
        } };
        let after = fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
        // steady quality: not worth replacing the clip for less than 15% saved
        if !ok || after == 0 || after >= before || (quality && after as f64 > before as f64 * 0.85) {
            let _ = fs::remove_file(&tmp);
            return Err(if ok { "It's already about as small as it gets".into() } else { "Couldn't re-encode the clip - see the log".into() });
        }
        fs::rename(&tmp, &src).map_err(err)?;
        // keep its date so it stays in place in the library
        if let (Some(t), Ok(f)) = (mtime, fs::File::options().write(true).open(&src)) {
            let _ = f.set_modified(t);
        }
        if let Some(stem) = src.file_stem() {
            let stem = stem.to_string_lossy();
            for x in ["strip.jpg", "strip2.jpg", "strip3.jpg", "levels.json", "a1.m4a", "a2.m4a", "a3.m4a", "a4.m4a", "a5.m4a"] {
                let _ = fs::remove_file(self.thumbs_dir().join(format!("{stem}.{x}")));
            }
        }
        log_line(&format!("compress: {} MB -> {} MB{} in {:.0}s", before / 1_000_000, after / 1_000_000,
            if enc.is_some() { " (AV1)" } else { "" }, t0.elapsed().as_secs_f32()));
        // the cloud copy gets the smaller version too - but not AV1: the cloud copy stays H.264 so links play everywhere
        if self.cloud_on() && enc.is_none() {
            if let Some(rel) = self.rel_of(path) {
                if self.cloud_state().files.iter().any(|f| f.rel == rel) {
                    let _ = self.cloud_upload(path);
                }
            }
        }
        Ok((before, after))
    }

    /// A horizontal strip of N frames for the editor timeline.
    pub fn filmstrip(&self, clip: &str) -> Result<String, String> {
        const N: u32 = 40;
        let t = self.thumb(clip)?;
        let stem = PathBuf::from(clip).file_stem().ok_or("bad clip path")?.to_string_lossy().to_string();
        // ".strip2": older strips of short clips ended in black (see below) - made again once
        let img = self.thumbs_dir().join(format!("{stem}.strip3.jpg"));
        // one at a time: hover, open and the after-save build can ask for the same strip at once
        static STRIP_LOCK: Mutex<()> = Mutex::new(());
        let _one = STRIP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if !img.exists() {
            let _ = fs::remove_file(self.thumbs_dir().join(format!("{stem}.strip.jpg")));
            let _ = fs::remove_file(self.thumbs_dir().join(format!("{stem}.strip2.jpg")));
            let fps = N as f64 / t.duration.max(1.0);
            // keyframes only is exact enough for Echo's recordings (a keyframe every 2 s), but edited / imported
            // clips can have one every 4-10 s (a 10 s black gap from the editor showed up seconds late in the
            // seek-bar preview, chat 25): those decode every frame (packet flags are read without decoding)
            let gap = max_key_gap(clip);
            let keys_only = gap <= 2.5 || t.duration > 600.0;
            let t0 = Instant::now();
            // decode keyframes only (one every ~2 s) - 10+ minute clips took ages otherwise.
            // tpad repeats the last keyframe to the end: without it the tiles after the last keyframe stayed
            // black (a 6 s clip lost its last third). Below-normal priority: often made in the background
            // right after a save, while a game runs
            let mut cmd = ffmpeg();
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000 | 0x0000_4000); // CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS
            }
            // -discard nokey drops non-key packets already in the demuxer (with -skip_frame alone some clips were
            // still decoded frame by frame: 48 s for a 3 min clip, chat 21); -an/-sn: the 6 audio tracks aren't read
            // into the decoder at all; 2 threads keep it light next to a game
            let child = cmd
                .args(["-hide_banner", "-loglevel", "error", "-y", "-threads", if keys_only { "2" } else { "4" }])
                .args(if keys_only { &["-discard", "nokey", "-skip_frame", "nokey"][..] } else { &[][..] })
                .args(["-an", "-sn", "-dn", "-i"])
                .arg(clip)
                .args(["-map", "0:v:0", "-vf", &format!("tpad=stop_mode=clone:stop_duration={:.2},fps={fps:.5},scale=-2:72:flags=fast_bilinear,tile={N}x1", t.duration.max(1.0) + 1.0),
                    "-frames:v", "1", "-q:v", "5"])
                .arg(&img)
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(err)?;
            *STRIP_RUN.lock().unwrap_or_else(|e| e.into_inner()) = Some((PathBuf::from(clip), child));
            let ok = loop {
                let st = {
                    let mut g = STRIP_RUN.lock().unwrap_or_else(|e| e.into_inner());
                    match g.as_mut() {
                        Some((_, ch)) => ch.try_wait().map(|o| o.map(|x| x.success())),
                        None => Ok(Some(false)),
                    }
                };
                match st {
                    Ok(Some(done)) => break done,
                    Ok(None) => std::thread::sleep(Duration::from_millis(40)),
                    Err(_) => break false,
                }
            };
            *STRIP_RUN.lock().unwrap_or_else(|e| e.into_inner()) = None;
            if !ok {
                let _ = fs::remove_file(&img);
                log_line(&format!("filmstrip: {stem} failed (ffmpeg could not read the clip, {:.0} s long)", t.duration));
                return Err("could not build filmstrip".into());
            }
            log_line(&format!("filmstrip: {stem} ({:.0} s clip) in {} ms", t.duration, t0.elapsed().as_millis()));
        }
        Ok(img.to_string_lossy().into())
    }

    /// A seek-bar strip made earlier for this clip (e.g. before it was moved to the cloud only), without
    /// making one - cloud clips have no local file for ffmpeg.
    pub fn cached_strip(&self, rel: &str) -> Option<String> {
        let stem = Path::new(rel).file_stem()?.to_string_lossy().to_string();
        let img = self.thumbs_dir().join(format!("{stem}.strip3.jpg"));
        img.exists().then(|| img.to_string_lossy().into())
    }

    /// Seek-bar strip (40 tiles, same file as filmstrip()) for a cloud-only clip, made by ffmpeg straight from
    /// the streaming server: one pass over the clip, keyframes only. Used on Linux,
    /// where WebKitGTK can't draw <video> frames into a canvas (the strip and hover frames stayed empty).
    pub fn cloud_strip(&self, rel: &str) -> Result<String, String> {
        if let Some(p) = self.cached_strip(rel) { return Ok(p); }
        const N: u32 = 40;
        let d = self.cloud_thumb(rel)?.duration;
        if d <= 0.0 { return Err("clip length unknown".into()); }
        let stem = Path::new(rel).file_stem().ok_or("bad path")?.to_string_lossy().to_string();
        let img = self.thumbs_dir().join(format!("{stem}.strip3.jpg"));
        let url = format!("{}/{}", self.cloud_stream_base()?, self.cpath(rel).split('/').map(url_part).collect::<Vec<_>>().join("/"));
        static CLOUD_STRIP_LOCK: Mutex<()> = Mutex::new(());
        let _one = CLOUD_STRIP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if img.exists() { return Ok(img.to_string_lossy().into()); }
        let t0 = Instant::now();
        // ONE pass over the clip, only its keyframes decoded, picked at 40 even spots (the last keyframe repeated to
        // the end). Before: 40 separate inputs = 40 look-ups at the cloud one after another (21 s for a 30 s clip on
        // Google Drive) while the player's own stream waited behind them (chat 24).
        let vf = format!("tpad=stop_mode=clone:stop_duration=5,fps={N}/{d:.3}:start_time=0,scale=-2:72:flags=fast_bilinear,setsar=1,tile={N}x1");
        let out = ffmpeg().args(["-hide_banner", "-loglevel", "error", "-y", "-threads", "2", "-skip_frame", "nokey", "-an", "-sn", "-dn", "-i", &url,
                "-vf", &vf, "-frames:v", "1", "-q:v", "5"]).arg(&img)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).output().map_err(err)?;
        if !out.status.success() || !img.exists() {
            let _ = fs::remove_file(&img);
            let e = String::from_utf8_lossy(&out.stderr);
            log_line(&format!("filmstrip: cloud {stem} failed: {}", e.lines().last().unwrap_or("").chars().take(200).collect::<String>()));
            return Err("could not build filmstrip".into());
        }
        log_line(&format!("filmstrip: cloud {stem} ({d:.0} s clip) in {} ms", t0.elapsed().as_millis()));
        Ok(img.to_string_lossy().into())
    }

    /// Linux, dragging the seek bar: WebKitGTK can't show the video jumping around (grey patches), so a picture
    /// covers it while dragging. This is the sharper set for that: 60 frames at 640x360 in a 10x6 grid
    /// ("<name>.scrub.jpg" next to the thumbnails). Local clip (`path`) or cloud clip (`rel`, read over the
    /// streaming server). Clips up to 90 s: every frame decoded (exact spots); longer: keyframes only (one per ~2 s).
    pub fn scrub_sheet(&self, path: Option<&str>, rel: Option<&str>) -> Result<String, String> {
        const COLS: u32 = 10;
        const ROWS: u32 = 6;
        let n = COLS * ROWS;
        let (input, stem, d) = match (path, rel) {
            (Some(p), _) => (p.to_string(), Path::new(p).file_stem().ok_or("bad path")?.to_string_lossy().to_string(), self.thumb(p)?.duration),
            (None, Some(r)) => (
                format!("{}/{}", self.cloud_stream_base()?, self.cpath(r).split('/').map(url_part).collect::<Vec<_>>().join("/")),
                Path::new(r).file_stem().ok_or("bad path")?.to_string_lossy().to_string(),
                self.cloud_thumb(r)?.duration,
            ),
            _ => return Err("no clip".into()),
        };
        if d <= 0.0 { return Err("clip length unknown".into()); }
        let img = self.thumbs_dir().join(format!("{stem}.scrub.jpg"));
        static SCRUB_LOCK: Mutex<()> = Mutex::new(());
        let _one = SCRUB_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // a local clip changed since (trimmed / replaced): make it again
        let fresh = match (fs::metadata(&img).and_then(|m| m.modified()), path.map(|p| fs::metadata(p).and_then(|m| m.modified()))) {
            (Ok(_), None) => true,
            (Ok(a), Some(Ok(b))) => a >= b,
            _ => false,
        };
        if fresh { return Ok(img.to_string_lossy().into()); }
        let t0 = Instant::now();
        let mut cmd = ffmpeg();
        cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-threads", "4"]);
        if d > 90.0 { cmd.args(["-discard", "nokey", "-skip_frame", "nokey"]); }
        let vf = format!("tpad=stop_mode=clone:stop_duration=5,fps={n}/{d:.3}:start_time=0,scale=640:360:force_original_aspect_ratio=decrease:flags=bilinear,pad=640:360:-1:-1,setsar=1,tile={COLS}x{ROWS}");
        let out = cmd.args(["-an", "-sn", "-dn", "-i", &input, "-map", "0:v:0", "-vf", &vf, "-frames:v", "1", "-q:v", "4"]).arg(&img)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).output().map_err(err)?;
        if !out.status.success() || !img.exists() {
            let _ = fs::remove_file(&img);
            let e = String::from_utf8_lossy(&out.stderr);
            log_line(&format!("scrub pictures: {stem} failed: {}", e.lines().last().unwrap_or("").chars().take(200).collect::<String>()));
            return Err("could not make the scrub pictures".into());
        }
        log_line(&format!("scrub pictures: {stem} ({d:.0} s clip) in {} ms", t0.elapsed().as_millis()));
        Ok(img.to_string_lossy().into())
    }

    /// Editor timeline dragging: sharp single pictures every 0.1 s (at most 450, spread over longer clips), up to
    /// 1280x720, good JPEG quality with full colour detail (the player's 640x360 sheet looked grainy/green in the
    /// editor, where lining up exact moments matters - William, chat 26). Kept for the last 3 clips opened.
    /// Returns {prefix, step, count}: picture k = `<prefix><k:04>.jpg` = time k * step.
    /// `player` = the lighter set for dragging the player's seek bar on Linux (chat 26): one every 0.2 s (max 300),
    /// 960x540, -q:v 3, last 5 clips kept - between the 60-picture sheet (grainy) and the editor's set.
    pub fn edit_frames(&self, path: &str, player: bool) -> Result<serde_json::Value, String> {
        let d = self.thumb(path)?.duration;
        if d <= 0.0 { return Err("clip length unknown".into()); }
        let stem = Path::new(path).file_stem().ok_or("bad path")?.to_string_lossy().to_string();
        let (per_s, max_n, w, h, q, keep, folder) = if player { (5.0, 300u32, 960, 540, "3", 4usize, "playerframes") } else { (10.0, 450u32, 1280, 720, "2", 2usize, "editframes") };
        let root = self.thumbs_dir().join(folder);
        let dir = root.join(&stem);
        let n = ((d * per_s).ceil() as u32).clamp(1, max_n);
        let step = d / n as f64;
        let prefix = dir.join("f_").to_string_lossy().to_string();
        let answer = |count: u32| serde_json::json!({ "prefix": prefix, "step": step, "count": count });
        static LOCK: Mutex<()> = Mutex::new(());
        let _one = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let done = dir.join("done");
        let fresh = match (fs::metadata(&done).and_then(|m| m.modified()), fs::metadata(path).and_then(|m| m.modified())) {
            (Ok(a), Ok(b)) => a >= b,
            _ => false,
        };
        if fresh {
            let count = fs::read_to_string(&done).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
            if count > 0 {
                // mark as just used (the folder's time = which sets are kept)
                let u = dir.join("used"); let _ = fs::write(&u, b""); let _ = fs::remove_file(&u);
                return Ok(answer(count));
            }
        }
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).map_err(err)?;
        // only the 3 most recently used clips keep their pictures (each set is some tens of MB)
        if let Ok(rd) = fs::read_dir(&root) {
            let mut sets: Vec<(std::time::SystemTime, PathBuf)> = rd.flatten().filter(|e| e.path() != dir && e.path().is_dir())
                .map(|e| (e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), e.path())).collect();
            sets.sort_by(|a, b| b.0.cmp(&a.0));
            for (_, p) in sets.into_iter().skip(keep) { let _ = fs::remove_dir_all(p); }
        }
        let t0 = Instant::now();
        let vf = format!("fps={n}/{d:.4}:start_time=0:round=near,scale={w}:{h}:force_original_aspect_ratio=decrease:flags=lanczos,setsar=1,format=yuvj444p");
        let out = ffmpeg().args(["-hide_banner", "-loglevel", "error", "-y", "-threads", "0", "-an", "-sn", "-dn", "-i", path, "-map", "0:v:0",
                "-vf", &vf, "-frames:v", &n.to_string(), "-q:v", q, "-start_number", "0"])
            .arg(format!("{prefix}%04d.jpg"))
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).output().map_err(err)?;
        let count = (0..n).take_while(|k| Path::new(&format!("{prefix}{k:04}.jpg")).exists()).count() as u32;
        if !out.status.success() || count == 0 {
            let _ = fs::remove_dir_all(&dir);
            let e = String::from_utf8_lossy(&out.stderr);
            log_line(&format!("editor pictures: {stem} failed: {}", e.lines().last().unwrap_or("").chars().take(200).collect::<String>()));
            return Err("could not make the editor pictures".into());
        }
        let _ = fs::write(&done, count.to_string());
        log_line(&format!("{} pictures: {stem} ({d:.0} s clip) - {count} pictures every {:.2} s in {} ms", if player { "seek-bar" } else { "editor" }, step, t0.elapsed().as_millis()));
        Ok(answer(count))
    }

    fn probe(&self, clip: &Path) -> (u32, u32, bool) {
        let out = hidden("ffprobe")
            .args(["-v", "error", "-show_entries", "stream=codec_type,width,height", "-of", "csv=p=0"])
            .arg(clip)
            .output();
        let s = out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
        let (mut w, mut h, mut audio) = (1920, 1080, false);
        for l in s.lines() {
            let f: Vec<&str> = l.split(',').map(|x| x.trim()).collect();
            if f.first() == Some(&"video") && f.len() >= 3 {
                w = f[1].parse().unwrap_or(w);
                h = f[2].parse().unwrap_or(h);
            }
            if f.first() == Some(&"audio") {
                audio = true;
            }
        }
        (w, h, audio)
    }

    /// Speech-to-text for a clip: listens to the Mic track (falls back to the full mix), returns words.
    pub fn transcribe(&self, clip: &str, model: &str) -> Result<Vec<Word>, String> {
        let cli = whisper_cli()?;
        let model = whisper_model(model)?;
        ensure_vc_runtime()?;
        let tracks = self.audio_tracks(clip);
        if tracks == 0 {
            return Err("this clip has no audio".into());
        }
        let threads = std::thread::available_parallelism().map(|n| n.get().min(8)).unwrap_or(4).to_string();
        // what to listen to: your mic + the Discord track when Discord had sound (so friends' voices are captioned too);
        // otherwise the full mix (Linux puts Discord in the desktop sound) or the mic. Tracks: 0 mix, 1 desktop, 2 mic, 4 Discord.
        let levels = if tracks >= 5 { self.track_levels(clip) } else { Vec::new() };
        let discord = levels.get(4).map_or(false, |&l| l > -50.0);
        let candidates: Vec<(&str, String)> = if discord {
            vec![("mic + Discord", "[0:a:2][0:a:4]amix=inputs=2:normalize=0,".into()), ("mix", "[0:a:0]".into())]
        } else if tracks >= 3 && cfg!(windows) {
            vec![("mic", "[0:a:2]".into()), ("mix", "[0:a:0]".into())]
        } else if tracks >= 3 {
            vec![("mix", "[0:a:0]".into()), ("mic", "[0:a:2]".into())]
        } else {
            vec![("track 0", "[0:a:0]".into())]
        };
        for (label, input) in candidates {
            let wav = std::env::temp_dir().join("clipper_caption.wav");
            // voice band + level out quiet voices (Discord friends are often much quieter than your mic)
            let graph = format!("{input}aformat=channel_layouts=mono,highpass=f=80,lowpass=f=7500,dynaudnorm=f=150:g=15:p=0.9,aresample=16000[o]");
            let ok = ffmpeg()
                .args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(clip)
                .args(["-filter_complex", &graph, "-map", "[o]", "-ac", "1", "-c:a", "pcm_s16le"])
                .arg(&wav).stdout(Stdio::null()).stderr(Stdio::null()).status().map_err(err)?.success();
            if !ok {
                continue;
            }
            let pcm = read_wav16(&wav);
            let _ = fs::remove_file(&wav);
            let regions = speech_regions(&pcm, 16000);
            log_line(&format!("captions: {label}: speech at {}", regions.iter().map(|r| format!("{:.1}-{:.1}s", r.0, r.1)).collect::<Vec<_>>().join(", ")));
            if regions.is_empty() {
                continue;
            }
            let t0 = Instant::now();
            // all speech joined into ONE file (short silences between) -> one whisper run: it sees whole
            // sentences and detects the language once, which is much more accurate than many tiny pieces
            const GAP: f64 = 0.4;
            let mut joined: Vec<i16> = Vec::new();
            let mut map: Vec<(f64, f64, f64)> = Vec::new(); // (start in joined, start in clip, length)
            for (a, b) in &regions {
                let part = &pcm[(a * 16000.0) as usize..((b * 16000.0) as usize).min(pcm.len())];
                map.push((joined.len() as f64 / 16000.0, *a, part.len() as f64 / 16000.0));
                joined.extend_from_slice(part);
                joined.extend(std::iter::repeat(0i16).take((GAP * 16000.0) as usize));
            }
            joined.resize(joined.len().max(16000 * 3 / 2), 0); // whisper wants >= ~1 s
            let to_clip = |t: f64| -> Option<f64> {
                let k = map.iter().rposition(|m| m.0 <= t + 0.05)?;
                let (j0, c0, len) = map[k];
                Some(c0 + (t - j0).clamp(0.0, len))
            };
            let rw = std::env::temp_dir().join("clipper_caption_part.wav");
            write_wav16(&rw, &joined, 16000)?;
            let base = std::env::temp_dir().join("clipper_caption");
            let _ = fs::remove_file(base.with_extension("json"));
            let o = hidden(&cli.to_string_lossy())
                .current_dir(cli.parent().unwrap_or(Path::new(".")))
                .args(["-m"]).arg(&model).args(["-f"]).arg(&rw)
                .args(["-l", "auto", "-ml", "1", "-sow", "-sns", "-mc", "0", "-oj", "-t", &threads, "-of"]).arg(&base)
                .stdout(Stdio::piped()).stderr(Stdio::piped()).output().map_err(err)?;
            let _ = fs::remove_file(&rw);
            if !o.status.success() {
                let code = o.status.code().unwrap_or(-1);
                let all = format!("{}\n{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                let tail: Vec<&str> = all.lines().filter(|l| !l.trim().is_empty()).collect();
                log_line(&format!("captions: whisper failed (exit {code:#x}): {}", tail[tail.len().saturating_sub(6)..].join(" | ")));
                if code as u32 == 0xC000_0135 {
                    return Err("A Windows runtime file is missing for speech recognition - see the log".into());
                }
                return Err("speech recognition failed - see the log".into());
            }
            let mut words: Vec<Word> = Vec::new();
            for w in parse_whisper_json(&fs::read_to_string(base.with_extension("json")).unwrap_or_default()) {
                // back to clip time; drop anything whisper placed in the silence padding
                if let (Some(s0), Some(e0)) = (to_clip(w.start), to_clip(w.end)) {
                    words.push(Word { text: w.text, start: s0, end: e0.max(s0 + 0.05) });
                }
            }
            let before = words.len();
            drop_repeats(&mut words);
            if words.len() < before { log_line(&format!("captions: dropped {} repeated words (whisper loop)", before - words.len())); }
            log_line(&format!("captions: {label}: {} words in {:.1}s", words.len(), t0.elapsed().as_secs_f32()));
            if !words.is_empty() {
                // kept so the library search can find clips by what was said
                if let Some(stem) = Path::new(clip).file_stem() {
                    let text = words.iter().map(|w| w.text.trim()).collect::<Vec<_>>().join(" ");
                    let _ = fs::write(self.thumbs_dir().join(format!("{}.words.json", stem.to_string_lossy())), serde_json::to_string(&text).unwrap_or_default());
                }
                return Ok(words);
            }
        }
        Ok(Vec::new())
    }

    /// Full-resolution PNG of the screen right now -> <clips>/Screenshots/<Game> <date>.png
    pub fn screenshot(&self) -> Result<String, String> {
        let cfg = self.settings();
        let dir = self.clips_dir().join("Screenshots");
        fs::create_dir_all(&dir).map_err(err)?;
        let prefix = if cfg.name_by_game { detect_game() } else { None }.unwrap_or_else(|| "Screenshot".into());
        let out = dir.join(format!("{prefix} {}", chrono::Local::now().format("%Y-%m-%d %H-%M-%S.png")));
        // Linux: the ffmpeg screen grabbers below are Windows-only (screenshots never worked on Linux).
        // 1) the desktop's Screenshot portal (full resolution, works in the Flatpak; KDE may ask once),
        // 2) the newest picture in the replay buffer (recording resolution)
        #[cfg(target_os = "linux")]
        {
            if crate::linuxutil::portal_screenshot(&out) {
                log_line(&format!("screenshot: {} (portal)", out.display()));
                return Ok(out.to_string_lossy().into());
            }
            let mut segs: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(&self.buf_dir).map(|rd| rd.flatten()
                .filter(|e| e.path().extension().map_or(false, |x| x == "ts"))
                .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).collect()).unwrap_or_default();
            segs.sort();
            // the newest segment is still being written: take the last frame of the one before it if needed
            for (_, seg) in segs.iter().rev().take(2) {
                let ok = ffmpeg().args(["-hide_banner", "-loglevel", "error", "-y", "-sseof", "-0.5", "-i"]).arg(seg)
                    .args(["-update", "1", "-frames:v", "30"]).arg(&out)
                    .stdout(Stdio::null()).stderr(Stdio::null()).status().map_or(false, |s| s.success());
                if ok && fs::metadata(&out).map_or(false, |m| m.len() > 0) {
                    log_line(&format!("screenshot: {} (from the replay buffer)", out.display()));
                    return Ok(out.to_string_lossy().into());
                }
            }
            log_line("screenshot: failed (no portal answer, no recording to take it from)");
            return Err("screenshot failed - is Echo recording?".into());
        }
        // desktop duplication (works for borderless/fullscreen games), GDI capture as a fallback
        #[allow(unreachable_code)]
        let tries: [Vec<&str>; 2] = [
            vec!["-filter_complex", "ddagrab=output_idx=0:draw_mouse=0,hwdownload,format=bgra"],
            vec!["-f", "gdigrab", "-draw_mouse", "0", "-i", "desktop"],
        ];
        for args in tries.iter() {
            let ok = ffmpeg()
                .args(["-hide_banner", "-loglevel", "error", "-y"]).args(args)
                .args(["-frames:v", "1"]).arg(&out)
                .stdout(Stdio::null()).stderr(Stdio::null()).status().map_err(err)?.success();
            if ok && fs::metadata(&out).map_or(false, |m| m.len() > 0) {
                log_line(&format!("screenshot: {}", out.display()));
                return Ok(out.to_string_lossy().into());
            }
        }
        Err("screenshot failed".into())
    }

    fn meta_path(&self) -> PathBuf {
        self.thumbs_dir().join("meta.json")
    }
    fn meta_all(&self) -> std::collections::BTreeMap<String, ClipMeta> {
        fs::read_to_string(self.meta_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    fn write_meta(&self, m: &std::collections::BTreeMap<String, ClipMeta>) -> Result<(), String> {
        fs::create_dir_all(self.thumbs_dir()).map_err(err)?;
        write_patient(&self.meta_path(), &serde_json::to_string(m).map_err(err)?)
    }
    /// Tags + notes per clip, plus what was said (from captions) for search.
    pub fn clip_meta(&self) -> std::collections::BTreeMap<String, ClipMeta> {
        let mut m = self.meta_all();
        if let Ok(rd) = fs::read_dir(self.thumbs_dir()) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if let Some(stem) = n.strip_suffix(".words.json") {
                    let said: String = fs::read_to_string(e.path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
                    m.entry(stem.to_string()).or_default().said = said;
                }
            }
        }
        m
    }
    pub fn set_meta(&self, name: &str, tags: Vec<String>, note: String) -> Result<(), String> {
        let mut m = self.meta_all();
        let mut tags: Vec<String> = tags.into_iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        tags.dedup();
        if tags.is_empty() && note.trim().is_empty() {
            m.remove(name);
        } else {
            m.insert(name.into(), ClipMeta { tags, note: note.trim().into(), said: String::new() });
        }
        self.write_meta(&m)
    }
    /// Use the frame at `t` seconds as the clip's thumbnail.
    pub fn set_thumb(&self, clip: &str, t: f64) -> Result<String, String> {
        let stem = Path::new(clip).file_stem().ok_or("bad clip path")?.to_string_lossy().to_string();
        let img = self.thumbs_dir().join(format!("{stem}.jpg"));
        let ok = ffmpeg()
            .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{:.3}", t.max(0.0)), "-i"])
            .arg(clip)
            .args(["-frames:v", "1", "-vf", "scale=480:-2", "-q:v", "4"])
            .arg(&img)
            .stdout(Stdio::null()).stderr(Stdio::null()).status().map_err(err)?.success();
        if !ok {
            return Err("could not grab that frame".into());
        }
        // remember the moment: share links / Discord previews and a re-made thumbnail (cloud-only clip, new cache)
        // use the same frame instead of the default one (chat 28)
        let _ = fs::write(self.thumbs_dir().join(format!("{stem}.thumbt")), format!("{:.3}", t.max(0.0)));
        Ok(img.to_string_lossy().into())
    }
    /// The moment picked with "Use as thumbnail" (seconds), if any.
    fn thumb_time(&self, stem: &str) -> Option<f64> {
        fs::read_to_string(self.thumbs_dir().join(format!("{stem}.thumbt"))).ok()?.trim().parse().ok()
    }
    /// Auto-cleanup: send clips older than `days` to the Recycle Bin (favourites are kept). Returns how many.
    pub fn cleanup_old(&self, days: u32) -> u32 {
        if days == 0 {
            return 0;
        }
        let favs = self.favorites();
        let cutoff = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
            .saturating_sub(days as u64 * 86400);
        let mut n = 0;
        for c in self.list_clips() {
            if c.modified < cutoff && !favs.contains(&c.name) && self.delete_clip(&c.path).is_ok() {
                n += 1;
            }
        }
        if n > 0 {
            log_line(&format!("cleanup: {n} clips older than {days} days sent to the Recycle Bin"));
        }
        n
    }

    /// Shrinks clips older than `days` (not favourites, each only once) to about half their size.
    /// `keep_going` is checked between clips (stops when the user is back / a game starts).
    pub fn shrink_old_clips(&self, days: u32, keep_going: impl Fn() -> bool) -> u32 {
        let list = self.thumbs_dir().join("shrunk.json");
        let mut done: std::collections::BTreeSet<String> = fs::read_to_string(&list).ok()
            .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let favs = self.favorites();
        let cutoff = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
            .saturating_sub(days.max(1) as u64 * 86400);
        let mut n = 0;
        let mut old: Vec<ClipInfo> = self.list_clips().into_iter()
            .filter(|c| c.modified < cutoff && !favs.contains(&c.name) && !done.contains(&c.path)).collect();
        old.sort_by_key(|c| std::cmp::Reverse(c.size));
        for c in old {
            if !keep_going() {
                break;
            }
            // AV1 PCs: AV1 at half the bitrate (same quality); others: H.264 at steady quality
            let av1 = self.settings().shrink_av1 && av1_ready();
            match self.compress_clip(&c.path, if av1 { 0.5 } else { 0.0 }, av1, &|_| {}) {
                Ok(_) => n += 1,
                Err(e) => log_line(&format!("shrink: skipped {} ({e})", c.name)),
            }
            done.insert(c.path.clone());
            let _ = fs::write(&list, serde_json::to_string(&done).unwrap_or_default());
        }
        if n > 0 {
            log_line(&format!("shrink: {n} old clips made smaller"));
        }
        n
    }

    /// Writes "Echo log <time>.txt" (log + previous log + settings without passwords/keys) for sending to a friend.
    pub fn support_file(&self) -> Result<String, String> {
        // ECHO_VERSION is set by the GitHub build (1.0.<n>); own builds say 0.1.0
        let mut out = format!("Echo {} - {}\n", option_env!("ECHO_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")), chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        let mut cfg = serde_json::to_value(self.settings()).unwrap_or_default();
        if let Some(m) = cfg.as_object_mut() {
            for (k, v) in m.iter_mut() {
                let k = k.to_lowercase();
                if (k.contains("secret") || k.contains("key") && k.starts_with("r2") || k.contains("pass") || k.contains("token"))
                    && v.as_str().map_or(false, |s| !s.is_empty()) {
                    *v = serde_json::Value::String("***".into());
                }
            }
        }
        out += &format!("\n--- settings ---\n{}\n", serde_json::to_string_pretty(&cfg).unwrap_or_default());
        let tail = |p: PathBuf| -> String {
            let s = fs::read_to_string(p).unwrap_or_default();
            let lines: Vec<&str> = s.lines().collect();
            lines[lines.len().saturating_sub(400)..].join("\n")
        };
        // start-up facts (libraries, Flatpak, webview) - a long run pushes them out of the last 400 lines (chat 26)
        {
            let s = fs::read_to_string(log_path()).unwrap_or_default();
            let keep: Vec<&str> = s.lines().filter(|l| ["linux: libraries", "flatpak:", "webview:", "hotkey:", "update:"].iter().any(|k| l.contains(k))).take(40).collect();
            if !keep.is_empty() { out += &format!("\n--- start-up ---\n{}\n", keep.join("\n")); }
        }
        out += &format!("\n--- log ---\n{}\n", tail(log_path()));
        out += &format!("\n--- previous run ---\n{}\n", tail(log_path().with_extension("prev.log")));
        #[cfg(target_os = "linux")]
        {
            // video playback engine of the window (GStreamer): only its last 300 lines, it can be chatty
            let s = fs::read_to_string(gst_log_path()).unwrap_or_default();
            let lines: Vec<&str> = s.lines().collect();
            // Flatpak: which plugins loaded (GPU "va" vs CPU "libav"/"openh264" decoders) - early lines, keep them
            let loaded: Vec<&str> = lines.iter().copied().filter(|l| l.contains("GST_PLUGIN_LOADING") && l.contains("loaded")).take(80).collect();
            if !loaded.is_empty() {
                out += &format!("\n--- player video engine (plugins loaded) ---\n{}\n", loaded.join("\n"));
            }
            let warn: Vec<&str> = lines.iter().copied().filter(|l| !l.contains("GST_PLUGIN_LOADING")).collect();
            out += &format!("\n--- player video engine (GStreamer warnings) ---\n{}\n", warn[warn.len().saturating_sub(300)..].join("\n"));
        }
        let file = std::env::temp_dir().join(format!("Echo log {}.txt", chrono::Local::now().format("%Y-%m-%d %H-%M")));
        fs::write(&file, out).map_err(err)?;
        Ok(file.to_string_lossy().to_string())
    }

    /// Versions newer than `since` with their "What changed?" text (echoclip.link/changes) - the What's new card
    /// lists the updates someone skipped.
    pub fn release_notes(&self, since: &str) -> Result<serde_json::Value, String> {
        let cfg = self.settings();
        let since: String = since.chars().filter(|c| c.is_ascii_digit() || *c == '.').take(20).collect();
        ec_call(&ec_api(&cfg), "GET", &format!("/changes?since={since}"), "", None)
    }

    /// "Send log": uploads the log file to echoclip.link (email addresses removed) and returns the link
    /// (William pastes it to Claude, who reads it directly). Kept 14 days by the server.
    pub fn upload_log(&self, file: &str) -> Result<String, String> {
        let text = fs::read_to_string(file).map_err(err)?;
        let mut text = redact_emails(&text);
        if text.len() > 390_000 {
            let mut cut = text.len() - 390_000;
            while !text.is_char_boundary(cut) { cut += 1; }
            text = format!("(start cut off)\n{}", &text[cut..]);
        }
        let cfg = self.settings();
        let body = serde_json::json!({
            "text": text,
            "version": option_env!("ECHO_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")),
            "os": if cfg!(windows) { "Windows" } else { "Linux" },
        });
        let v = ec_call(&ec_api(&cfg), "POST", "/log", "", Some(body))?;
        let url = v["url"].as_str().unwrap_or("").to_string();
        if url.is_empty() { return Err("no link".into()); }
        log_line(&format!("log: uploaded {url}"));
        Ok(url)
    }

    fn favs_path(&self) -> PathBuf {
        self.thumbs_dir().join("favorites.json")
    }
    /// Starred clips (by file name without extension).
    pub fn favorites(&self) -> Vec<String> {
        fs::read_to_string(self.favs_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    pub fn set_favorite(&self, name: &str, on: bool) -> Result<(), String> {
        let mut f = self.favorites();
        f.retain(|x| x != name);
        if on {
            f.push(name.to_string());
        }
        fs::create_dir_all(self.thumbs_dir()).map_err(err)?;
        write_patient(&self.favs_path(), &serde_json::to_string(&f).map_err(err)?)
    }

    /// Number of audio tracks in a clip (3 = mix, desktop, mic; 6 adds game, Discord, browsers; 7 also music apps).
    pub fn audio_tracks(&self, clip: &str) -> u32 {
        let out = hidden("ffprobe")
            .args(["-v", "error", "-select_streams", "a", "-show_entries", "stream=index", "-of", "csv=p=0"])
            .arg(clip)
            .output();
        out.map(|o| String::from_utf8_lossy(&o.stdout).lines().filter(|l| !l.trim().is_empty()).count() as u32).unwrap_or(0)
    }

    /// Loudest peak (dB) of each audio track, so the editor can skip silent ones (-99 = silent). Cached.
    pub fn track_levels(&self, clip: &str) -> Vec<f64> {
        let n = self.audio_tracks(clip);
        let stem = PathBuf::from(clip).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let cache = self.thumbs_dir().join(format!("{stem}.levels.json"));
        if let Ok(v) = fs::read_to_string(&cache).map(|s| serde_json::from_str::<Vec<f64>>(&s)) {
            if let Ok(v) = v {
                if v.len() == n as usize && modified(&cache) >= modified(Path::new(clip)) {
                    return v;
                }
            }
        }
        let hs: Vec<_> = (0..n).map(|t| {
            let clip = clip.to_string();
            std::thread::spawn(move || {
                let o = hidden("ffmpeg")
                    .args(["-hide_banner", "-nostats", "-i"]).arg(&clip)
                    .args(["-map", &format!("0:a:{t}"), "-af", "volumedetect", "-vn", "-f", "null", "-"])
                    .output();
                o.ok().and_then(|o| {
                    let e = String::from_utf8_lossy(&o.stderr).to_string();
                    e.lines().find_map(|l| l.split("max_volume:").nth(1).and_then(|x| x.trim().trim_end_matches("dB").trim().parse::<f64>().ok()))
                }).unwrap_or(-99.0)
            })
        }).collect();
        let v: Vec<f64> = hs.into_iter().map(|h| h.join().unwrap_or(-99.0)).collect();
        let _ = fs::create_dir_all(self.thumbs_dir());
        let _ = fs::write(&cache, serde_json::to_string(&v).unwrap_or_default());
        v
    }

    /// One audio track as its own small file (cached), so the editor can preview it.
    pub fn extract_track(&self, clip: &str, track: u32) -> Result<String, String> {
        let stem = PathBuf::from(clip).file_stem().ok_or("bad clip path")?.to_string_lossy().to_string();
        fs::create_dir_all(self.thumbs_dir()).map_err(err)?;
        let out = self.thumbs_dir().join(format!("{stem}.a{track}.m4a"));
        if !out.exists() {
            let ok = ffmpeg()
                .args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(clip)
                .args(["-map", &format!("0:a:{track}"), "-c", "copy", "-movflags", "+faststart"])
                .arg(&out)
                .stdout(Stdio::null()).stderr(Stdio::null()).status().map_err(err)?.success();
            if !ok {
                return Err("could not read that audio track".into());
            }
        }
        Ok(out.to_string_lossy().into())
    }

    /// Render an editor project (pieces + speed + text + volume) to "<name> (edit).mp4".
    /// A reversed copy of src[start..end] (cached in %TEMP%), made in 2 s chunks so memory stays small.
    fn reverse_piece(&self, src: &Path, start: f64, end: f64, audio: bool) -> Result<PathBuf, String> {
        let dir = std::env::temp_dir().join("clipper_reverse");
        fs::create_dir_all(&dir).map_err(err)?;
        let key = format!("{}_{:.0}_{:.0}", safe_name(&src.file_stem().unwrap_or_default().to_string_lossy()), start * 1000.0, end * 1000.0);
        let out = dir.join(format!("{key}.mp4"));
        if out.exists() {
            return Ok(out);
        }
        let t0 = Instant::now();
        let mut list = String::new();
        let mut t = end;
        let mut k = 0;
        while t > start + 0.01 {
            let a = (t - 2.0).max(start);
            let part = dir.join(format!("{key}_{k}.mp4"));
            let mut c = ffmpeg();
            c.args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{a:.3}"), "-t", &format!("{:.3}", t - a), "-i"]).arg(src)
                .args(["-map", "0:v:0", "-vf", "reverse", "-c:v", "libx264", "-preset", "ultrafast", "-crf", "14"]);
            if audio {
                c.args(["-map", "0:a:0", "-af", "areverse", "-c:a", "aac", "-b:a", "192k"]);
            }
            let ok = c.arg(&part).stdout(Stdio::null()).stderr(Stdio::null()).status().map_err(err)?.success();
            if !ok {
                return Err("could not reverse a piece".into());
            }
            list += &format!("file '{}'\n", part.to_string_lossy().replace('\\', "/"));
            t = a;
            k += 1;
        }
        let lf = dir.join(format!("{key}.txt"));
        fs::write(&lf, list).map_err(err)?;
        let ok = ffmpeg().args(["-hide_banner", "-loglevel", "error", "-y", "-f", "concat", "-safe", "0", "-i"]).arg(&lf)
            .args(["-c", "copy"]).arg(&out).stdout(Stdio::null()).stderr(Stdio::null()).status().map_err(err)?.success();
        for j in 0..k {
            let _ = fs::remove_file(dir.join(format!("{key}_{j}.mp4")));
        }
        let _ = fs::remove_file(&lf);
        if !ok {
            return Err("could not reverse a piece".into());
        }
        log_line(&format!("edit: reversed {:.1}s piece in {:.1}s", end - start, t0.elapsed().as_secs_f32()));
        Ok(out)
    }

    pub fn render_edit(&self, path: &str, e: &Edit) -> Result<String, String> {
        let src = PathBuf::from(path);
        if !self.is_clip(&src) {
            return Err("not a Clipper clip".into());
        }
        let out_dir = src.parent().map(|d| d.to_path_buf()).unwrap_or_else(|| self.clips_dir());
        let mut segs: Vec<&Seg> = e.segments.iter().filter(|s| s.end - s.start >= 0.05).collect();
        segs.sort_by(|a, b| a.at.partial_cmp(&b.at).unwrap_or(std::cmp::Ordering::Equal));
        if segs.is_empty() {
            return Err("nothing left on the timeline".into());
        }
        let (w, h, has_audio) = self.probe(&src);
        let tracks = self.audio_tracks(path);
        let mut g = Vec::new();
        let srcdur = self.thumb(path).map(|t| t.duration).unwrap_or(0.0);
        // extra inputs after the source (-i args); input index = 1 + position
        let mut inputs: Vec<Vec<String>> = Vec::new();
        // other videos / sounds dropped into the editor: one input each -> (input index, has audio, duration)
        let mut extra_src: std::collections::HashMap<String, (usize, bool, f64)> = std::collections::HashMap::new();
        {
            let all = e.segments.iter().map(|x| &x.src).chain(e.overlays.iter().map(|x| &x.src)).chain(e.audio.iter().map(|x| &x.src));
            for sp in all {
                if sp.is_empty() || sp == path || extra_src.contains_key(sp) {
                    continue;
                }
                if !Path::new(sp).exists() {
                    return Err(format!("{} is missing", Path::new(sp).file_name().unwrap_or_default().to_string_lossy()));
                }
                let (_, _, a) = self.probe(Path::new(sp));
                let d = self.thumb(sp).map(|t| t.duration).unwrap_or(0.0);
                inputs.push(vec!["-i".into(), sp.clone()]);
                extra_src.insert(sp.clone(), (inputs.len(), a, d));
            }
        }
        // (input index, has audio, duration, is another file) for a piece's source
        let src_of = |sp: &str| -> (usize, bool, f64, bool) {
            match extra_src.get(sp) {
                Some(&(k, a, d)) if !sp.is_empty() && sp != path => (k, a, d, true),
                _ => (0, has_audio, srcdur, false),
            }
        };
        let audio = (has_audio || extra_src.values().any(|x| x.1)) && !e.mute;
        // other videos are fitted into the clip's frame (black bars when the shape differs)
        let fit = format!(",scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black,setsar=1");
        // timeline units joined by concat: (video label, audio label)
        let mut units: Vec<(String, String)> = Vec::new();
        let mut unit_len = 0.0f64; // output length of the last unit
        let mut last_piece = false;
        let mut pos = 0.0f64; // output time reached so far
        const VF: &str = "fps=60,setsar=1,format=yuv420p";
        const AF: &str = "aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo";
        let speed_of = |s: &Seg| if s.freeze { 1.0 } else { s.speed.clamp(0.1, 4.0) };
        let len_of = |s: &Seg| (s.end - s.start) / speed_of(s);
        // transition length into piece k (0 = hard cut); needs the pieces to touch
        let tr_len = |k: usize, gap: f64| -> f64 {
            let s = segs[k];
            if k == 0 || s.tr.is_empty() || gap > 0.02 {
                return 0.0;
            }
            s.td.max(0.1).min(len_of(s) / 2.0).min(len_of(segs[k - 1]) / 2.0).min(3.0)
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
            // a transition into the NEXT piece needs this one to run on a little (handle)
            let ext = match segs.get(i + 1) {
                Some(nx) => tr_len(i + 1, nx.at - (pos.max(s.at) + slen)),
                None => 0.0,
            };
            let total = slen + ext;
            let gr = grade(s);
            let vf_fades = fades("fade", s.fi, s.fo, slen);
            let af_fades = fades("afade", s.fi, s.fo, slen);
            let (si, s_audio, s_dur, other) = src_of(&s.src);
            let fitf = if other { fit.as_str() } else { "" };
            let mut silent = s.amute || !s_audio;
            if s.freeze {
                g.push(format!("[{si}:v]trim=start={:.3}:duration=0.25,setpts=PTS-STARTPTS{fitf},{VF},trim=end_frame=1,tpad=stop_mode=clone:stop_duration={total:.3},trim=duration={total:.3},setpts=PTS-STARTPTS{gr}{vf_fades}[v{i}]", s.start));
                silent = true;
            } else if s.rev {
                let rsrc = if other { PathBuf::from(&s.src) } else { src.clone() };
                let rv = self.reverse_piece(&rsrc, s.start, s.end, s_audio)?;
                inputs.push(vec!["-i".into(), rv.to_string_lossy().to_string()]);
                let k = inputs.len();
                let pad = if ext > 0.0 { format!(",tpad=stop_mode=clone:stop_duration={ext:.3}") } else { String::new() };
                g.push(format!("[{k}:v]setpts=(PTS-STARTPTS)/{sp:.4}{fitf},{VF}{pad}{gr}{vf_fades}[v{i}]"));
                if audio && !silent {
                    let ap = if ext > 0.0 { format!(",apad=whole_dur={total:.3},atrim=duration={total:.3}") } else { String::new() };
                    g.push(format!("[{k}:a]asetpts=PTS-STARTPTS,{},{AF}{ap}{af_fades}[a{i}]", atempo(sp)));
                }
            } else {
                let end2 = if s_dur > 0.0 { (s.end + ext * sp).min(s_dur) } else { s.end + ext * sp };
                let short = ext - (end2 - s.end) / sp;
                let pad = if short > 0.01 { format!(",tpad=stop_mode=clone:stop_duration={short:.3}") } else { String::new() };
                g.push(format!("[{si}:v]trim=start={:.3}:end={end2:.3},setpts=(PTS-STARTPTS)/{sp:.4}{fitf},{VF}{pad}{gr}{vf_fades}[v{i}]", s.start));
                if audio && !silent {
                    // a dropped video's sound can be a little shorter than its picture: pad to the exact length
                    let ap = if ext > 0.0 || other { format!(",apad=whole_dur={total:.3},atrim=duration={total:.3}") } else { String::new() };
                    g.push(format!("[{si}:a:0]atrim=start={:.3}:end={end2:.3},asetpts=PTS-STARTPTS,{},{AF}{ap}{af_fades}[a{i}]", s.start, atempo(sp)));
                }
            }
            if audio && silent {
                g.push(format!("anullsrc=r=48000:cl=stereo,atrim=duration={total:.3},{AF}[a{i}]"));
            }
            let td = tr_len(i, gap);
            if td > 0.0 && last_piece {
                // blend with the previous piece: it ran on for `td` past its end, so later timing is unchanged
                let (pv, pa) = units.pop().unwrap();
                let kind: String = s.tr.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
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
        // layers may run past the main track: pad with black + silence underneath them
        let ov_end = e.overlays.iter().map(|o| o.at.max(0.0) + (o.end - o.start) / o.speed.clamp(0.1, 4.0)).fold(0.0f64, f64::max);
        let tail = ov_end - pos;
        if tail > 0.02 {
            g.push(format!("color=c=black:s={w}x{h}:r=60:d={tail:.3},{VF}[gvt]"));
            if audio {
                g.push(format!("anullsrc=r=48000:cl=stereo,atrim=duration={tail:.3},{AF}[gat]"));
            }
            units.push(("gvt".into(), "gat".into()));
        }
        let total_len = pos.max(ov_end);
        let n = units.len();
        let cat: String = units.iter().map(|(v, a)| if audio { format!("[{v}][{a}]") } else { format!("[{v}]") }).collect();
        g.push(format!("{cat}concat=n={n}:v=1:a={}[vc]{}", audio as u8, if audio { "[ac]" } else { "" }));

        // text overlays (text goes through files -> no escaping trouble)
        let tmp = std::env::temp_dir();
        let font = if cfg!(windows) { "fontfile='C\\:/Windows/Fonts/segoeuib.ttf':" } else { "" };
        let mut last = "vc".to_string();
        // zoom boxes: zoompan over the main video, eased in/out (0.4 s), centred on the box
        let zooms: Vec<&BoxFx> = e.boxes.iter().filter(|b| b.kind == "zoom" && b.end - b.start >= 0.2 && b.w > 0.0 && b.h > 0.0).collect();
        if !zooms.is_empty() {
            let (mut z, mut cx, mut cy) = (String::from("1"), String::from("0.5"), String::from("0.5"));
            for b in &zooms {
                let zf = (1.0 / b.w.max(b.h)).clamp(1.0, 5.0);
                let r = (0.4f64).min((b.end - b.start) / 2.0);
                let ramp = format!("clip(min((it-{:.3})/{r:.3}\\,({:.3}-it)/{r:.3})\\,0\\,1)", b.start, b.end);
                z += &format!("+{:.4}*{ramp}", zf - 1.0);
                cx += &format!("+{:.4}*{ramp}", b.x.clamp(0.0, 1.0) - 0.5);
                cy += &format!("+{:.4}*{ramp}", b.y.clamp(0.0, 1.0) - 0.5);
            }
            g.push(format!(
                "[vc]zoompan=z='{z}':x='clip(({cx})*iw-iw/zoom/2\\,0\\,iw-iw/zoom)':y='clip(({cy})*ih-ih/zoom/2\\,0\\,ih-ih/zoom)':d=1:s={w}x{h}:fps=60,setsar=1[vz]"));
            last = "vz".into();
        }
        let mut ovs: Vec<&Ovl> = e.overlays.iter().filter(|o| o.end - o.start >= 0.05).collect();
        ovs.sort_by_key(|o| o.row);
        for (i, o) in ovs.iter().enumerate() {
            let sp = o.speed.clamp(0.1, 4.0);
            let len = (o.end - o.start) / sp;
            let (oi, _, _, other) = src_of(&o.src);
            // a dropped video on a layer: sized relative to the clip's width
            let sc = if other { format!("scale={w}*{:.4}:-2", o.scale.clamp(0.05, 1.0)) } else { format!("scale=iw*{:.4}:-2", o.scale.clamp(0.05, 1.0)) };
            g.push(format!(
                "[{oi}:v]trim=start={:.3}:end={:.3},setpts=(PTS-STARTPTS)/{sp:.4}+{:.3}/TB,{sc},setsar=1[ov{i}]",
                o.start, o.end, o.at.max(0.0)));
            g.push(format!(
                "[{last}][ov{i}]overlay=x=main_w*{:.4}-overlay_w/2:y=main_h*{:.4}-overlay_h/2:eof_action=pass:enable='between(t,{:.3},{:.3})'[vo{i}]",
                o.x, o.y, o.at.max(0.0), o.at.max(0.0) + len));
            last = format!("vo{i}");
        }
        // blur / colour boxes (full-frame coordinates, so before the 9:16 crop)
        for (i, b) in e.boxes.iter().filter(|b| b.kind != "zoom" && b.end - b.start >= 0.05 && b.w > 0.0 && b.h > 0.0).enumerate() {
            let bw = (((b.w.min(1.0) * w as f64).round() as u32).max(4) / 2 * 2).min(w);
            let bh = (((b.h.min(1.0) * h as f64).round() as u32).max(4) / 2 * 2).min(h);
            let bx = (((b.x - b.w / 2.0).max(0.0) * w as f64).round() as u32).min(w - bw);
            let by = (((b.y - b.h / 2.0).max(0.0) * h as f64).round() as u32).min(h - bh);
            let en = format!("enable='between(t,{:.3},{:.3})'", b.start.max(0.0), b.end);
            if b.kind == "image" {
                if b.src.is_empty() || !Path::new(&b.src).exists() {
                    continue;
                }
                let t = format!("{:.3}", total_len + 1.0);
                if b.src.to_lowercase().ends_with(".gif") {
                    // animated GIF: play it over and over (the picture options -loop / -framerate don't exist for GIFs)
                    inputs.push(vec!["-stream_loop".into(), "-1".into(), "-t".into(), t, "-i".into(), b.src.clone()]);
                } else {
                    inputs.push(vec!["-loop".into(), "1".into(), "-framerate".into(), "60".into(), "-t".into(), t, "-i".into(), b.src.clone()]);
                }
                let k = inputs.len();
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
        // vertical 9:16: crop before the texts so captions are placed inside the tall frame
        let mut w = w;
        if let Some(cx) = e.vertical {
            let cw = ((h as f64 * 9.0 / 16.0).round() as u32 / 2 * 2).min(w);
            let x = (cx.clamp(0.0, 1.0) * w as f64 - cw as f64 / 2.0).clamp(0.0, (w - cw) as f64).round() as u32 / 2 * 2;
            g.push(format!("[{last}]crop={cw}:{h}:{x}:0[vcrop]"));
            last = "vcrop".into();
            w = cw;
        }
        let base_g = g.clone();
        let base_last = last.clone();
        let in_ass = |t: &Txt| (!t.words.is_empty() && !t.hl.is_empty()) || (!t.anim.is_empty() && !t.text.trim().is_empty());
        let karaoke: Vec<&Txt> = e.texts.iter().filter(|t| in_ass(t)).collect();
        // returns (filters, last label); `ass`: render highlighted captions with libass
        let build_text = |ass: bool| -> Result<(Vec<String>, String), String> {
            let mut g: Vec<String> = Vec::new();
            let mut last = base_last.clone();
            for (i, t) in e.texts.iter().enumerate() {
                if t.text.trim().is_empty() || (ass && in_ass(t)) {
                    continue;
                }
                let f = tmp.join(format!("clipper_text_{i}.txt"));
                fs::write(&f, t.text.replace("\r", "")).map_err(err)?;
                let color: String = t.color.chars().filter(|c| c.is_ascii_hexdigit()).take(6).collect();
                let px = ((t.size * h as f64).round() as u32).max(8);
                g.push(format!(
                    "[{last}]drawtext={font}textfile='{}':fontsize={px}:fontcolor=0x{color}:borderw={}:bordercolor=black@0.75:line_spacing={}{}:x=w*{:.4}-text_w/2:y=h*{:.4}-text_h/2:enable='between(t,{:.3},{:.3})'[t{i}]",
                    ff_path(&f), (px / 14).max(2), px / 6, if t.text.contains('\n') { ":text_align=C" } else { "" }, t.x, t.y, t.start, t.end));
                last = format!("t{i}");
            }
            if ass && !karaoke.is_empty() {
                let f = tmp.join("clipper_captions.ass");
                fs::write(&f, captions_ass(&karaoke, w, h)).map_err(err)?;
                let fonts = if cfg!(windows) { ":fontsdir='C\\:/Windows/Fonts'" } else { "" };
                g.push(format!("[{last}]subtitles=filename='{}'{fonts}[subs]", ff_path(&f)));
                last = "subs".into();
            }
            Ok((g, last))
        };
        let mut g: Vec<String> = Vec::new();
        if audio {
            // detached audio clips, delayed to their spot and mixed over the main audio
            let ov_sound: Vec<Aud> = e.overlays.iter().filter(|o| o.sound).map(|o| Aud {
                start: o.start, end: o.end, speed: o.speed, at: o.at, vol: 1.0, mute: false, track: 0, fi: 0.0, fo: 0.0, src: o.src.clone(),
            }).collect();
            let extra: Vec<&Aud> = e.audio.iter().chain(ov_sound.iter())
                .filter(|a| !a.mute && a.end - a.start >= 0.05 && src_of(&a.src).1).collect();
            let mut mix = String::from("[ac]");
            for (i, a) in extra.iter().enumerate() {
                let sp = a.speed.clamp(0.1, 4.0);
                let ms = (a.at.max(0.0) * 1000.0).round() as u64;
                let (ai, _, _, other) = src_of(&a.src);
                let stream = if other { format!("{ai}:a:0") } else { format!("0:a:{}", a.track.min(tracks.saturating_sub(1))) };
                g.push(format!(
                    "[{stream}]atrim=start={:.3}:end={:.3},asetpts=PTS-STARTPTS,{},volume={:.3},{AF}{},adelay={ms}:all=1[x{i}]",
                    a.start, a.end, atempo(sp), a.vol.clamp(0.0, 4.0), fades("afade", a.fi, a.fo, (a.end - a.start) / sp)));
                mix += &format!("[x{i}]");
            }
            let main = if extra.is_empty() {
                "ac".to_string()
            } else {
                g.push(format!("{mix}amix=inputs={}:normalize=0:duration=first[am]", extra.len() + 1));
                "am".to_string()
            };
            let norm = if e.normalize { ",loudnorm=I=-14:TP=-1.5:LRA=11,aresample=48000" } else { "" };
            g.push(format!("[{main}]volume={:.3}{norm}[ao]", e.volume.clamp(0.0, 4.0)));
        }

        let stem = src.file_stem().ok_or("bad clip path")?.to_string_lossy().to_string();
        let out = (1..).map(|n| out_dir.join(if n == 1 { format!("{stem} (edit).mp4") } else { format!("{stem} (edit {n}).mp4") }))
            .find(|p| !p.exists()).unwrap();
        let tmp_out = out_dir.join(format!("{stem}.render.tmp"));
        let dur = self.thumb(path).map(|t| t.duration).unwrap_or(0.0);
        let size = fs::metadata(&src).map(|m| m.len()).unwrap_or(0) as f64;
        let mut kbps = if dur > 0.0 { ((size * 8.0 / dur / 1000.0) as u64).clamp(4000, 80000) } else { 20000 };
        let mut akbps = 192u64;
        let mut out_h = e.out_h;
        // size target (Discord): video bitrate from the budget, 5% kept for the mp4 container; low budgets get a
        // smaller picture (a sharp 720p beats a blocky 1080p)
        let sized = e.max_mb.filter(|&mb| mb >= 1.0 && total_len > 0.5);
        if let Some(mb) = sized {
            akbps = if audio { 128 } else { 0 };
            let budget = (mb * 8_000.0 * 0.95 / total_len) as u64;   // kbit/s for everything
            let v = budget.saturating_sub(akbps).max(250);
            if v < kbps {
                kbps = v;
            }
            if out_h.is_none() && h > 720 && kbps < 2500 {
                out_h = Some(720);
            }
            if out_h.is_none() && h > 480 && kbps < 900 {
                out_h = Some(480);
            }
            log_line(&format!("edit: export sized for {mb} MB - {kbps} kbps video{}", out_h.map(|x| format!(", {x}p")).unwrap_or_default()));
        }
        let b = format!("{kbps}k");
        let ab = format!("{akbps}k");
        let audio_g = g;
        let variant = |ass: bool| -> Result<(String, String), String> {
            let (mut tg, mut last) = build_text(ass)?;
            if let Some(oh) = out_h.filter(|&oh| oh >= 240 && oh != h) {
                tg.push(format!("[{last}]scale=-2:{oh}:flags=lanczos,setsar=1[vout]"));
                last = "vout".into();
            }
            if let Some(w) = self.watermark_graph(&last, "vwm", out_h.filter(|&oh| oh >= 240).unwrap_or(h)) {
                tg.push(w);
                last = "vwm".into();
            }
            // colour looks / stickers / texts can leave RGB behind: always finish as BT.709 TV-range yuv420p
            tg.push(format!("[{last}]{TV_YUV}[vtv]"));
            last = "vtv".into();
            let all: Vec<String> = base_g.iter().chain(tg.iter()).chain(audio_g.iter()).cloned().collect();
            Ok((all.join(";"), last))
        };
        let run_with = |graph: &str, last: &str, venc: &[&str]| -> Result<bool, String> {
            if skip_enc(venc) { return Ok(false); }
            let mut c = ffmpeg();
            c.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(&src);
            for inp in &inputs {
                c.args(inp);
            }
            c.args(["-filter_complex", graph, "-map", &format!("[{last}]")]);
            if audio {
                c.args(["-map", "[ao]", "-c:a", "aac", "-b:a", &ab]);
            }
            // -progress on stdout: "out_time_us=…" lines -> EXPORT_PROGRESS for the UI's bar
            EXPORT_PROGRESS.store(0, Ordering::Relaxed);
            let mut ch = c.args(venc).args(TV_TAGS).args(["-movflags", "+faststart", "-progress", "pipe:1", "-nostats", "-f", "mp4"]).arg(&tmp_out)
                .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(err)?;
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
                            let f = if total_len > 0.0 { (us / 1e6 / total_len).clamp(0.0, 0.99) } else { 0.0 };
                            EXPORT_PROGRESS.store((f * 1000.0) as u32, Ordering::Relaxed);
                        }
                    }
                }
            }
            let st = ch.wait().map_err(err)?;
            let e = errt.join().unwrap_or_default();
            if !st.success() {
                log_line(&format!("edit: {}", e.trim()));
            }
            Ok(st.success() && fs::metadata(&tmp_out).map_or(false, |m| m.len() > 0))
        };
        let encode = |graph: &str, last: &str| -> Result<bool, String> {
            Ok(run_with(graph, last, &["-c:v", "h264_amf", "-quality", "quality", "-b:v", &b, "-maxrate", &b])?
                || run_with(graph, last, &["-c:v", "h264_nvenc", "-preset", "p5", "-b:v", &b, "-maxrate", &b])?
                || run_with(graph, last, &["-c:v", "h264_qsv", "-b:v", &b, "-maxrate", &b])?
                || (sized.is_some() && run_with(graph, last, &["-c:v", "libx264", "-preset", "veryfast", "-b:v", &b, "-maxrate", &b, "-bufsize", &b])?)
                || (sized.is_none() && run_with(graph, last, &["-c:v", "libx264", "-preset", "veryfast", "-crf", "18"])?))
        };
        let mut ok = false;
        if !karaoke.is_empty() {
            let (gr, l) = variant(true)?;
            ok = encode(&gr, &l)?;
            if !ok {
                log_line("edit: word-highlight / animated text failed, exporting plain text instead");
            }
        }
        if !ok {
            let (gr, l) = variant(false)?;
            ok = encode(&gr, &l)?;
        }
        if !ok {
            let _ = fs::remove_file(&tmp_out);
            return Err("render failed - see the log".into());
        }
        fs::rename(&tmp_out, &out).map_err(err)?;
        let out = out.to_string_lossy().to_string();
        let _ = self.thumb(&out);
        Ok(out)
    }

    /// Older clips were recorded with full-range colours but labelled TV-range, so they play too dark.
    /// Detect those (brightness values outside 16-235). Windows: just fix the label (no re-encode).
    /// Linux: the player (WebKitGTK) ignores the full-range label, so convert the picture to real TV range.
    pub fn fix_dark_clips(&self) -> Result<u32, String> {
        let mut fixed = 0;
        for c in self.list_clips() {
            let p = PathBuf::from(&c.path);
            let info = hidden("ffprobe")
                .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=color_range,codec_name", "-of", "csv=p=0"])
                .arg(&p).output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
            if !info.contains("h264") {
                continue;
            }
            let labelled_pc = info.contains("pc");
            if labelled_pc && cfg!(windows) {
                continue;
            }
            let out_of_range = if labelled_pc { 1.0 } else { full_range_share(&p) };
            if out_of_range <= 0.005 {
                continue;
            }
            let tmp = p.with_extension("colorfix.tmp");
            let ok = if cfg!(windows) {
                ffmpeg()
                    .args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(&p)
                    .args(["-map", "0", "-c", "copy", "-bsf:v", "h264_metadata=video_full_range_flag=1", "-color_range", "pc", "-movflags", "+faststart", "-f", "mp4"])
                    .arg(&tmp).stdout(Stdio::null()).stderr(Stdio::null()).status().map_err(err)?.success()
            } else {
                to_tv_range(&p, &tmp)
            };
            if !ok {
                let _ = fs::remove_file(&tmp);
                continue;
            }
            let mtime = fs::metadata(&p).and_then(|m| m.modified()).ok();
            if fs::rename(&tmp, &p).is_err() {
                let _ = fs::remove_file(&tmp);
                continue;
            }
            // keep the clip's date so the library order doesn't change
            if let (Some(t), Ok(f)) = (mtime, fs::File::options().write(true).open(&p)) {
                let _ = f.set_modified(t);
            }
            if let Some(stem) = p.file_stem() {
                let _ = fs::remove_file(self.thumbs_dir().join(format!("{}.jpg", stem.to_string_lossy())));
                let _ = fs::remove_file(self.thumbs_dir().join(format!("{}.strip.jpg", stem.to_string_lossy())));
                let _ = fs::remove_file(self.thumbs_dir().join(format!("{}.strip3.jpg", stem.to_string_lossy())));
            }
            log_line(&format!("colorfix: {} ({:.1}% pixels outside TV range)", c.name, out_of_range * 100.0));
            fixed += 1;
        }
        Ok(fixed)
    }

    /// Delete oldest clips while the folder is over the size limit.
    fn enforce_storage(&self, cfg: &Settings) {
        if cfg.max_storage_gb == 0 {
            return;
        }
        let limit = cfg.max_storage_gb as u64 * 1_000_000_000;
        let mut clips = self.list_clips(); // newest first
        let mut total: u64 = clips.iter().map(|c| c.size).sum();
        while total > limit && clips.len() > 1 {
            let c = clips.pop().unwrap();
            total -= c.size;
            log_line(&format!("storage: removing oldest clip {}", c.name));
            let _ = self.delete_clip(&c.path);
        }
    }
}

/// Put a clip on the clipboard as a file (paste straight into Discord, Explorer...).
/// rclone (for the Share button), downloaded to %APPDATA%\\Clipper\\rclone on first use.
fn ensure_rclone() -> Result<PathBuf, String> {
    #[cfg(not(windows))]
    {
        // Flatpak: rclone comes with Echo (/app/bin) - no downloading programs inside the sandbox
        #[cfg(target_os = "linux")]
        if crate::linuxutil::in_flatpak() {
            return Ok(PathBuf::from("/app/bin/rclone").exists().then(|| PathBuf::from("/app/bin/rclone")).unwrap_or_else(|| PathBuf::from("rclone")));
        }
        // own up-to-date copy (distro rclone is often too old for Filen): ~/.config/Clipper/rclone
        let dir = app_dir().join("rclone");
        if let Some(p) = find_file(&dir, &["rclone"]).filter(|p| p.is_file()) {
            return Ok(p);
        }
        fs::create_dir_all(&dir).map_err(err)?;
        let zip = dir.join("rclone.zip");
        log_line("cloud: downloading rclone (linux)");
        // pinned version + fingerprint (from rclone's SHA256SUMS), chat 28
        download_checked("https://github.com/rclone/rclone/releases/download/v1.75.2/rclone-v1.75.2-linux-amd64.zip", &zip,
            "sha256:349ac8fba6ff65d6247043f1750cdcb518ec5d500ef91463a10d37c0ccdf3702")?;
        let ok = unzip_to(&zip, &dir).map_err(|e| log_line(&format!("cloud: unpack failed: {e}"))).is_ok();
        let _ = fs::remove_file(&zip);
        let p = find_file(&dir, &["rclone"]).filter(|p| p.is_file());
        if let (true, Some(p)) = (ok, p) {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&p, fs::Permissions::from_mode(0o755));
            return Ok(p);
        }
        log_line("cloud: rclone download/unpack failed, using the system rclone");
        return Ok(PathBuf::from("rclone"));
    }
    #[allow(unreachable_code)]
    let dir = app_dir().join("rclone");
    if let Some(p) = bundled(&["rclone.exe"]).or_else(|| find_file(&dir, &["rclone.exe"])) {
        return Ok(p);
    }
    fs::create_dir_all(&dir).map_err(err)?;
    let zip = dir.join("rclone.zip");
    log_line("share: downloading rclone");
    download_checked("https://github.com/rclone/rclone/releases/download/v1.75.2/rclone-v1.75.2-windows-amd64.zip", &zip,
        "sha256:8a9db2bad1dc79a65b3348fd857069598e2aab7cf4d118b77b1df23055aff818")?;
    let ok = unzip_to(&zip, &dir).map_err(|e| log_line(&format!("unpack failed: {e}"))).is_ok();
    let _ = fs::remove_file(&zip);
    if !ok {
        return Err("could not unpack rclone".into());
    }
    find_file(&dir, &["rclone.exe"]).ok_or_else(|| "rclone.exe missing after download".into())
}

/// Put text (a share link) on the clipboard.
/// "name@example.com" -> "[email]" (uploaded logs: settings and cloud labels contain the Echo Cloud email)
fn redact_emails(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let local = |ch: char| ch.is_alphanumeric() || "._%+-".contains(ch);
    let dom = |ch: char| ch.is_alphanumeric() || ".-".contains(ch);
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        if c[i] == '@' {
            let tail: Vec<char> = out.chars().collect();
            let mut k = tail.len();
            while k > 0 && local(tail[k - 1]) { k -= 1; }
            let mut j = i + 1;
            while j < c.len() && dom(c[j]) { j += 1; }
            let d: String = c[i + 1..j].iter().collect();
            let d = d.trim_end_matches('.');
            if k < tail.len() && d.contains('.') {
                let a = tail[..k].iter().collect::<String>().len();
                out.truncate(a);
                out.push_str("[email]");
                i += 1 + d.chars().count();
                continue;
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

pub fn copy_text(text: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        if crate::winutil::clipboard_text(text) { Ok(()) } else { Err("copy failed".into()) }
    }
    #[cfg(not(windows))]
    {
        wl_copy(&[], text.as_bytes())
    }
}

pub fn copy_to_clipboard(path: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        if crate::winutil::clipboard_files(&[path]) { Ok(()) } else { Err("copy failed".into()) }
    }
    #[cfg(not(windows))]
    {
        // file:// URI list: Dolphin, Discord, browsers paste it as a file
        let abs = fs::canonicalize(path).map_err(err)?;
        let uri = format!("file://{}", uri_escape(&abs.to_string_lossy()));
        wl_copy(&["--type", "text/uri-list"], format!("{uri}\r\n").as_bytes())
    }
}

#[cfg(not(windows))]
fn uri_escape(p: &str) -> String {
    p.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        _ => format!("%{b:02X}"),
    }).collect()
}

/// Linux clipboard via wl-copy (wl-clipboard); xclip as a fallback on X11.
#[cfg(not(windows))]
fn wl_copy(args: &[&str], data: &[u8]) -> Result<(), String> {
    let run = |prog: &str, a: Vec<&str>| -> bool {
        let Ok(mut c) = sys_cmd(prog).args(&a).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else { return false };
        if let Some(mut i) = c.stdin.take() { let _ = i.write_all(data); }
        c.wait().map_or(false, |s| s.success())
    };
    if run("wl-copy", args.to_vec()) { return Ok(()); }
    let t = if args.len() == 2 { args[1] } else { "UTF8_STRING" };
    if run("xclip", vec!["-selection", "clipboard", "-t", t]) { return Ok(()); }
    Err("copy failed - install wl-clipboard (sudo dnf install wl-clipboard)".into())
}


// ---------------------------------------------------------------------------------------------
// Cloud library (pCloud / MEGA through rclone): upload, free up space, stream, delete, and sync
// clips between PCs. Cloud layout mirrors the clips folder: echo:Echo/<folder>/<name>.mp4
// ---------------------------------------------------------------------------------------------

/// A clip in the cloud. `rel` = path inside the Echo folder ("Valorant/x.mp4").
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct CloudFile {
    /// path as on the PC: "<folder>/<name>.mp4"
    pub rel: String,
    /// where it really is under Echo/ in the cloud: "2026/11/01/<folder>/<name>.mp4" (older uploads: = rel)
    #[serde(default)]
    pub path: String,
    pub name: String,
    pub folder: String,
    pub size: u64,
    pub modified: u64,
    /// also on this PC
    pub local: bool,
}

/// Remembered between runs (.thumbs/cloud.json).
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct CloudState {
    /// last cloud listing
    files: Vec<CloudFile>,
    /// clips you chose to keep only in the cloud / deleted from this PC only: never auto-download them
    keep_remote: std::collections::BTreeSet<String>,
    /// clips whose cloud copy you deleted while keeping them on this PC: never auto-upload them again
    /// (uploading one by hand takes it off this list) - chat 29, "Manage Echo Cloud space"
    no_upload: std::collections::BTreeSet<String>,
}

/// One file in Echo Cloud for Settings → Cloud → "Manage Echo Cloud space" (chat 29).
#[derive(Serialize, Clone, Default)]
pub struct EcFile {
    /// where it is under Echo/ in Echo Cloud ("2026/11 - November/06 - Friday/Valorant/x.mp4")
    pub path: String,
    /// path as on the PC ("Valorant/x.mp4")
    pub rel: String,
    pub name: String,
    pub folder: String,
    pub size: u64,
    pub modified: u64,
    /// the same clip (same size) is on this PC
    pub local: bool,
    /// a share copy (Echo/.shared/…) that only exists for a link - deleting it ends that link
    pub copy: bool,
}

/// Free bytes on the drive that holds `dir` (None = couldn't tell).
pub fn free_space(dir: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let w: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let mut avail: u64 = 0;
        let ok = unsafe {
            windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(windows::core::PCWSTR(w.as_ptr()), Some(&mut avail as *mut u64), None, None)
        };
        return ok.is_ok().then_some(avail);
    }
    #[cfg(not(windows))]
    {
        let o = sys_cmd("df").args(["-B1", "--output=avail", "--"]).arg(dir).output().ok()?;
        String::from_utf8_lossy(&o.stdout).lines().last()?.trim().parse().ok()
    }
}

#[derive(Serialize)]
pub struct CloudInfo {
    pub provider: String,
    pub used: u64,
    pub total: u64,
    pub accounts: Vec<AccountInfo>,
    pub busy: String,
}

#[derive(Serialize, Clone)]
pub struct AccountInfo {
    pub id: u32,
    pub provider: String,
    pub label: String,
    pub used: u64,
    pub total: u64,
    pub error: String,
}

fn acc_remote(a: &crate::settings::CloudAccount) -> String {
    let root = a.root.trim_end_matches(['/', '\\']);
    format!("acc_{}:{root}", a.id)
}

/// "acc_N:<root>/<sub>" (or "acc_N:<sub>" when the account has no root folder)
fn acc_path(a: &crate::settings::CloudAccount, sub: &str) -> String {
    let r = acc_remote(a);
    if r.ends_with(':') { format!("{r}{sub}") } else { format!("{r}/{sub}") }
}

/// With several clouds and a chosen upload target: the account new clips go to.
/// None = let the union pick (most free space) - also when Echo Cloud is picked but the clip doesn't fit.
/// `for_link`: the clip is uploaded to make a share link - Echo Cloud first (links that play in Discord).
fn upload_target(cfg: &crate::settings::Settings, size: u64, for_link: bool) -> Option<crate::settings::CloudAccount> {
    if cfg.cloud_accounts.len() < 2 || (cfg.cloud_target.is_empty() && !for_link) {
        return None;
    }
    let acc = if for_link || cfg.cloud_target == "echocloud" {
        cfg.cloud_accounts.iter().find(|a| a.provider == "echocloud")?
    } else {
        cfg.cloud_accounts.iter().find(|a| a.id.to_string() == cfg.cloud_target)?
    };
    if acc.provider == "echocloud" && (ec_full() || ec_space_left().map_or(false, |left| size > left)) {
        return None; // Echo Cloud is full: this one goes to the other clouds
    }
    Some(acc.clone())
}

/// Union upstream for an account: Echo Cloud is read-only there (":ro") - its uploads, deletes and moves go
/// through the server, so its storage keys never need write access (UPLOAD_KEYS="ro" on the server).
fn union_upstream(a: &crate::settings::CloudAccount) -> String {
    if a.provider == "echocloud" { format!("{}:ro", acc_remote(a)) } else { acc_remote(a) }
}

/// Rebuild the "echo" remote over all accounts: alias for one, union for several
/// (new clips go to the account with the most free space; reads/moves/deletes look everywhere).
fn rebuild_echo_remote(accs: &[crate::settings::CloudAccount]) -> Result<(), String> {
    let _ = rclone_out(&["config", "delete", "echo"]);
    match accs.len() {
        0 => Ok(()),
        1 => rclone_out(&["config", "create", "echo", "alias", "remote", &acc_remote(&accs[0])]).map(|_| ()),
        _ => {
            let ups = accs.iter().map(union_upstream).collect::<Vec<_>>().join(" ");
            rclone_out(&["config", "create", "echo", "union", "upstreams", &ups,
                "action_policy", "epall", "create_policy", "mfs", "search_policy", "ff"]).map(|_| ())
        }
    }
}

/// Old single-account setup ([echo] was the provider itself) -> account acc_1 + alias.
/// Also: unions made before chat 16 had Echo Cloud writable -> rebuilt once with Echo Cloud ":ro".
fn migrate_cloud(cfg: &mut crate::settings::Settings) {
    if cfg.cloud_accounts.len() > 1 && cfg.cloud_accounts.iter().any(|a| a.provider == "echocloud") {
        let want = cfg.cloud_accounts.iter().map(union_upstream).collect::<Vec<_>>().join(" ");
        let t = rclone_out(&["config", "show", "echo"]).unwrap_or_default();
        let have = t.lines().find_map(|l| l.trim().strip_prefix("upstreams").map(|v| v.trim_start_matches([' ', '=']).trim().to_string()));
        if have.as_deref().map_or(false, |h| h != want) {
            match rebuild_echo_remote(&cfg.cloud_accounts) {
                Ok(()) => log_line("cloud: Echo Cloud is now read-only in the combined cloud (writes go through the server)"),
                Err(e) => log_line(&format!("cloud: couldn't update the combined cloud: {e}")),
            }
        }
    }
    if cfg.cloud.is_empty() || !cfg.cloud_accounts.is_empty() {
        return;
    }
    let Ok(t) = fs::read_to_string(cloud_conf()) else { return };
    if !t.contains("[echo]") || t.contains("type = alias") || t.contains("type = union") {
        return;
    }
    let _ = fs::write(cloud_conf(), t.replace("[echo]", "[acc_1]"));
    let acc = crate::settings::CloudAccount { id: 1, provider: cfg.cloud.clone(), label: String::new(), root: String::new() };
    if rebuild_echo_remote(std::slice::from_ref(&acc)).is_ok() {
        cfg.cloud_accounts.push(acc);
        let _ = cfg.save();
        log_line("cloud: moved the old cloud login to account 1");
    } else {
        let _ = fs::write(cloud_conf(), t);
    }
}

fn cloud_conf() -> PathBuf {
    app_dir().join("rclone").join("echo.conf")
}

/// Older rclone downloads may miss newer backends (Filen needs 1.73+): fetch the current rclone if so.
fn rclone_supports(backend: &str) -> Result<(), String> {
    let ok = |p: &Path| hidden(&p.to_string_lossy()).args(["help", "backend", backend]).output().map_or(false, |o| o.status.success());
    let exe = ensure_rclone()?;
    if ok(&exe) {
        return Ok(());
    }
    if cfg!(windows) {
        log_line(&format!("cloud: rclone too old for {backend} - updating"));
        let _ = fs::remove_dir_all(app_dir().join("rclone").join("bin"));
        // keep echo.conf; drop the old rclone folders
        if let Ok(rd) = fs::read_dir(app_dir().join("rclone")) {
            for e in rd.flatten() {
                if e.path().is_dir() {
                    let _ = fs::remove_dir_all(e.path());
                }
            }
        }
        let exe = ensure_rclone()?;
        if ok(&exe) {
            return Ok(());
        }
    }
    Err(format!("This rclone version can't use {backend} yet"))
}

/// Filen's rclone backend needs an API key that only the Filen CLI can export:
/// download the CLI (once) and run `export-api-key` with the login.
fn filen_api_key(email: &str, pass: &str, code: &str) -> Result<String, String> {
    let dir = app_dir().join("filen");
    fs::create_dir_all(&dir).map_err(err)?;
    let exe = dir.join(if cfg!(windows) { "filen.exe" } else { "filen" });
    if !exe.exists() {
        log_line("cloud: downloading the Filen CLI");
        let v = crate::net::get_json("https://api.github.com/repos/FilenCloudDienste/filen-cli-releases/releases/latest")
            .map_err(|e| { log_line(&format!("filen: {e}")); "Couldn't reach GitHub to get the Filen tool".to_string() })?;
        let want_os = if cfg!(windows) { "windows" } else { "linux-gnu" };
        let asset = v["assets"].as_array().into_iter().flatten()
            .find(|a| a["browser_download_url"].as_str().map_or(false, |u| { let l = u.to_lowercase(); l.contains("x86_64") && l.contains(want_os) }))
            .ok_or("No Filen tool for this system in the latest release")?;
        let url = asset["browser_download_url"].as_str().unwrap_or_default().to_string();
        // GitHub's own SHA-256 of the file ("digest": "sha256:…") - checked before it's run (chat 28)
        let digest = asset["digest"].as_str().unwrap_or_default().to_string();
        if digest.is_empty() { log_line("filen: no fingerprint from GitHub for this file - downloading without a check"); }
        let tmp = dir.join(url.rsplit('/').next().unwrap_or("filen.bin"));
        download_checked(&url, &tmp, &digest)?;
        let low = tmp.to_string_lossy().to_lowercase();
        if low.ends_with(".zip") || low.ends_with(".tar.gz") || low.ends_with(".tgz") {
            if low.ends_with(".zip") {
                unzip_to(&tmp, &dir)?;
            } else {
                // .tar.gz only exists for Linux, where tar is a normal system tool
                hidden("tar").arg("-xf").arg(&tmp).arg("-C").arg(&dir).status().map_err(err)?;
            }
            let _ = fs::remove_file(&tmp);
            let found = find_file(&dir, &[if cfg!(windows) { "filen.exe" } else { "filen" }])
                .or_else(|| fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).find(|p| p.is_file() && p.extension().map_or(cfg!(not(windows)), |x| x == "exe")))
                .ok_or("Filen tool missing after unpacking")?;
            if found != exe {
                fs::rename(&found, &exe).map_err(err)?;
            }
        } else {
            fs::rename(&tmp, &exe).map_err(err)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&exe, fs::Permissions::from_mode(0o755));
        }
    }
    // 1) login flags before the command, 2) login through FILEN_CLI_* environment variables
    let _ = fs::create_dir_all(dir.join("config")); // the CLI won't create it itself
    let run = |via_env: bool| -> Result<std::process::Output, String> {
        let mut c = hidden(&exe.to_string_lossy());
        c.arg("--config-dir").arg(dir.join("config"));
        if via_env {
            c.env("FILEN_CLI_EMAIL", email).env("FILEN_CLI_PASSWORD", pass);
            if !code.is_empty() {
                c.env("FILEN_CLI_2FA_CODE", code);
            }
        } else {
            c.args(["--email", email, "--password", pass]);
            if !code.is_empty() {
                c.args(["--two-factor-code", code]);
            }
        }
        c.arg("export-api-key").stdin(Stdio::null()).output().map_err(err)
    };
    let mut o = run(false)?;
    if !o.status.success() {
        log_line(&format!("cloud: filen (flags) failed: {}", String::from_utf8_lossy(&o.stdout).lines().chain(String::from_utf8_lossy(&o.stderr).lines()).map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" | ")));
        o = run(true)?;
    }
    let out = format!("{}\n{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    // the key: the longest token of letters / digits / - _ in the output
    let key = out.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'))
        .filter(|t| t.len() >= 24).max_by_key(|t| t.len()).map(|t| t.to_string());
    match key {
        Some(k) if o.status.success() => Ok(k),
        _ => {
            let lines: Vec<&str> = out.lines().map(str::trim).filter(|l| !l.is_empty() && !l.contains("report it at")).collect();
            let msg = lines.last().copied().unwrap_or("login failed").trim_start_matches(['✘', ' ']).to_string();
            log_line(&format!("cloud: filen export-api-key failed (exit {:?}): {}", o.status.code(), lines.join(" | ")));
            if msg.to_lowercase().contains("two") || msg.to_lowercase().contains("2fa") {
                Err("Filen asks for your 2FA code - enter it and try again".into())
            } else {
                Err(format!("Filen login failed: {msg}"))
            }
        }
    }
}

fn rclone_cmd() -> Result<Command, String> {
    ec_ensure();
    rclone_cmd_raw()
}

fn rclone_cmd_raw() -> Result<Command, String> {
    let mut c = hidden(&ensure_rclone()?.to_string_lossy());
    c.arg("--config").arg(cloud_conf());
    // encrypted cloud config (Windows): the password goes in the child's environment, never on the command line;
    // without it rclone must fail instead of waiting for someone to type a password into a hidden window
    let pass = RCLONE_PASS.lock().map(|p| p.clone()).unwrap_or_default();
    if !pass.is_empty() {
        c.env("RCLONE_CONFIG_PASS", pass);
    }
    c.arg("--ask-password=false");
    Ok(c)
}

/// Password of the encrypted echo.conf (from settings, decrypted with the Windows login).
static RCLONE_PASS: Mutex<String> = Mutex::new(String::new());

pub fn set_rclone_pass(p: &str) {
    if let Ok(mut g) = RCLONE_PASS.lock() { *g = p.to_string(); }
}

/// `Echo.exe --rclone-pass`: rclone asks Echo itself for the password while encrypting echo.conf
/// (so the password never appears on a command line).
pub fn print_rclone_pass() {
    use std::io::Write;
    let _ = std::io::stdout().write_all(crate::settings::Settings::load().rclone_pass.as_bytes());
    let _ = std::io::stdout().flush();
}

/// echo.conf holds the cloud logins (Google Drive sign-in, Filen, MEGA…). Windows: encrypt it once with a
/// random password that Echo keeps DPAPI-encrypted in settings.json. Returns true when `cfg` changed.
fn secure_cloud_conf(cfg: &mut crate::settings::Settings) -> bool {
    if !crate::settings::secret::available() {
        crate::settings::secret::private_file(&cloud_conf());
        return false;
    }
    let conf = cloud_conf();
    let Ok(t) = fs::read_to_string(&conf) else { return false };
    if t.starts_with("# Encrypted rclone configuration") {
        if !cfg.rclone_pass.is_empty() { return false; }
        if crate::settings::secret::keyring_down() {
            // Linux: the keyring isn't open (yet) - the logins are fine, just not usable this run
            log_line("cloud: the keyring (KDE Wallet) isn't open - cloud paused until Echo restarts");
            return false;
        }
        // encrypted, but the password is gone (settings from another PC / Windows user): the logins can't be used
        let _ = fs::rename(&conf, conf.with_extension("conf.locked"));
        cfg.cloud.clear();
        cfg.cloud_accounts.clear();
        cfg.ec_token.clear();
        let _ = cfg.save();
        log_line("cloud: the saved cloud logins belong to another PC or Windows user - sign in again");
        return true;
    }
    if t.trim().is_empty() { return false; }
    // the one-time account move still reads the file in plain text: do it first
    migrate_cloud(cfg);
    let mut changed = false;
    if cfg.rclone_pass.is_empty() {
        cfg.rclone_pass = format!("{}{}", random_token(), random_token());
        changed = true;
    }
    if cfg.save().is_err() { return changed; }
    set_rclone_pass(&cfg.rclone_pass);
    // AppImage: the outer file (the inner binary needs the AppImage's libraries)
    let Some(exe) = std::env::var_os("APPIMAGE").map(PathBuf::from).or_else(|| std::env::current_exe().ok()) else { return changed };
    let bak = conf.with_extension("conf.bak");
    if fs::copy(&conf, &bak).is_err() { return changed; }
    let ok = ensure_rclone().ok().and_then(|r| hidden(&r.to_string_lossy())
        .arg("--config").arg(&conf).arg("--ask-password=false")
        .args(["config", "encryption", "set", "--password-command"]).arg(format!("\"{}\" --rclone-pass", exe.to_string_lossy()))
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).output().ok())
        .map_or(false, |o| o.status.success())
        && fs::read_to_string(&conf).map_or(false, |t| t.starts_with("# Encrypted"))
        && rclone_out(&["listremotes"]).map_or(false, |l| t.contains('[') == !l.trim().is_empty());
    if ok {
        log_line("cloud: cloud logins are now stored encrypted");
    } else {
        // anything odd: put the plain file back, Echo keeps working as before
        let _ = fs::copy(&bak, &conf);
        set_rclone_pass("");
        log_line("cloud: couldn't encrypt the cloud logins - kept as they were");
    }
    let _ = fs::remove_file(&bak);
    changed
}

/// The cloud config (logins) can't be decrypted any more - its password is gone (chat 27: the Flatpak had
/// overwritten it in the shared keyring). The UI shows a warning + "Sign out and start over".
pub static CLOUD_LOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn rclone_out(args: &[&str]) -> Result<String, String> {
    let o = rclone_cmd()?.args(args).output().map_err(err)?;
    CLOUD_LOCKED.store(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("decrypt configuration"), Ordering::Relaxed);
    if !o.status.success() {
        let e = String::from_utf8_lossy(&o.stderr).to_string();
        let last = e.trim().lines().last().unwrap_or("rclone failed").to_string();
        log_line(&format!("cloud: rclone {} failed: {last}", args.first().unwrap_or(&"")));
        return Err(last);
    }
    Ok(String::from_utf8_lossy(&o.stdout).to_string())
}

/// rclone with live byte counts: JSON stats every 0.5 s on stderr -> on_bytes(sent, total)
fn rclone_progress(args: &[&str], on_bytes: impl Fn(u64, u64)) -> Result<(), String> {
    use std::io::BufRead;
    let mut ch = rclone_cmd()?.args(args).args(["--use-json-log", "--stats", "500ms", "-v"])
        .stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(err)?;
    let mut last_err = String::new();
    let mut logged = 0;
    let stderr = ch.stderr.take();
    // "Cancel" on the upload box: stop rclone (its unfinished upload is thrown away by the cloud)
    let ch = std::sync::Arc::new(Mutex::new(ch));
    let done = std::sync::Arc::new(AtomicBool::new(false));
    let watch = { let (ch, done) = (ch.clone(), done.clone()); std::thread::spawn(move || {
        while !done.load(Ordering::Relaxed) {
            if UP_CANCEL.load(Ordering::Relaxed) { let _ = ch.lock().unwrap().kill(); return true; }
            std::thread::sleep(Duration::from_millis(200));
        }
        false
    }) };
    if let Some(e) = stderr {
        for line in std::io::BufReader::new(e).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            // the file being sent right now (rclone's totals count a retried upload twice: "1.43 GB of 1.43 GB"
            // for a 716 MB clip, stuck at 100% - chat 22)
            let cur = &v["stats"]["transferring"][0];
            if let (Some(b), Some(t)) = (cur["bytes"].as_u64(), cur["size"].as_u64()) {
                on_bytes(b, t);
            } else if let (Some(b), Some(t)) = (v["stats"]["bytes"].as_u64(), v["stats"]["totalBytes"].as_u64()) {
                if b < t { on_bytes(b, t); }
            }
            let lvl = v["level"].as_str().unwrap_or("");
            if lvl == "error" || lvl == "critical" || lvl == "warning" || lvl == "notice" {
                let m = v["msg"].as_str().unwrap_or("").trim().to_string();
                // say it now (retries, Google Drive slowness), not only when rclone gives up
                if logged < 12 && !m.is_empty() { logged += 1; log_line(&format!("cloud: rclone {lvl}: {}", m.chars().take(300).collect::<String>())); }
                if lvl != "notice" { last_err = m; }
            }
        }
    }
    let st = ch.lock().unwrap().wait();
    done.store(true, Ordering::Relaxed);
    if watch.join().unwrap_or(false) { return Err("Upload cancelled".into()); }
    let st = st.map_err(err)?;
    if !st.success() {
        let e = if last_err.is_empty() { "rclone failed".to_string() } else { last_err };
        log_line(&format!("cloud: rclone {} failed: {e}", args.first().unwrap_or(&"")));
        return Err(e);
    }
    Ok(())
}

const CLOUD_ROOT: &str = "echo:Echo";

// ----- Echo Cloud: our own storage (Cloudflare R2 behind the echo-cloud Worker, see cloud/ in the repo) -----
// Sign in with an email code -> token. The Worker hands out R2 keys (a few hours, only for u/<id>/) that
// go into an rclone s3 remote like any other account; they're renewed before they run out.
const EC_API_DEFAULT: &str = "https://echoclip.link";

struct EcState {
    api: String,
    token: String,
    /// rclone remote name (acc_N)
    acc: String,
    /// when the current keys expire (unix s); 0 = get keys first
    exp: u64,
    /// over the quota: keys only read
    read_only: bool,
    /// bytes in Echo Cloud and the plan's size (from the server; `used` also counts our own uploads since then).
    /// Echo pauses uploads that wouldn't fit instead of relying on the server alone. 0 quota = not known yet.
    used: u64,
    quota: u64,
}
static EC: Mutex<Option<EcState>> = Mutex::new(None);
/// new keys were written: the streaming server must restart to use them
static EC_NEW_KEYS: AtomicBool = AtomicBool::new(false);
/// Echo's Google Drive login (see cloud_connect "gdrive")
const GDRIVE_CLIENT_ID: &str = "38239043070-6q95iidaueoc8bij14u1fpj8g1qdjs9p.apps.googleusercontent.com";
/// Its secret comes in at build time (build.rs: env ECHO_GDRIVE_SECRET or src-tauri/gdrive.secret.txt), so it isn't in
/// the public code - GitHub's push protection blocks Google client secrets (chat 29). Empty = rclone's own Google login.
const GDRIVE_CLIENT_SECRET: &str = env!("ECHO_GDRIVE_SECRET");
/// The page the browser shows after signing in to Google Drive / pCloud (rclone's own is a bare "Success! All done.
/// Please go back to rclone."). Passed as rclone's config_template key (Go template: .OK .Name .Description .Code);
/// it isn't saved in rclone.conf - tested with rclone 1.75.2 (chat 29). ONE line: rclone refuses values with \n.
const OAUTH_PAGE: &str = r##"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{{ if .OK }}Connected to Echo{{ else }}Echo couldn't connect{{ end }}</title><style>*{box-sizing:border-box}body{margin:0;min-height:100vh;display:grid;place-items:center;background:radial-gradient(900px 500px at 50% -10%,#2a2150 0%,#0e0f13 60%);color:#eceef2;font:15px/1.55 system-ui,"Segoe UI",Roboto,Arial,sans-serif;padding:24px}.c{width:100%;max-width:440px;background:#16181d;border:1px solid #262a31;border-radius:18px;padding:34px 30px 28px;text-align:center;box-shadow:0 30px 80px #0008}.logo{width:56px;height:56px;border-radius:15px;margin:0 auto 18px;display:grid;place-items:center;background:linear-gradient(135deg,#8b7cf6,#6d5ce8);box-shadow:0 10px 30px #7c5cff55}.st{width:44px;height:44px;border-radius:50%;margin:-4px auto 14px;display:grid;place-items:center;font-size:22px;font-weight:700}.ok{background:#3fb27f22;color:#3fb27f}.bad{background:#e5534b22;color:#e5534b}h1{font-size:22px;margin:0 0 8px}p{color:#a0a6b1;margin:0 0 6px}.small{font-size:12.5px;color:#6b7280;margin-top:18px}.err{margin-top:14px;text-align:left;background:#0e0f13;border:1px solid #262a31;border-radius:10px;padding:10px 12px;font-size:12.5px;color:#c9ccd3;word-break:break-word}</style></head><body><div class="c"><div class="logo"><svg viewBox="0 0 256 256" width="30" height="30" aria-hidden="true"><circle cx="128" cy="128" r="80" fill="none" stroke="#fff" stroke-width="22" stroke-linecap="round" stroke-dasharray="390 503" transform="rotate(-200 128 128)"/><path d="M48 104 L58 70 L82 96 Z" fill="#fff" stroke="#fff" stroke-width="8" stroke-linejoin="round"/><path d="M106 86 L176 128 L106 170 Z" fill="#fff" stroke="#fff" stroke-width="12" stroke-linejoin="round"/></svg></div>{{ if .OK }}<div class="st ok">&#10003;</div><h1>You're connected</h1><p>Your cloud is now linked to Echo.</p><p>You can close this tab and go back to Echo - it finishes setting up by itself.</p>{{ else }}<div class="st bad">!</div><h1>That didn't work</h1><p>Echo couldn't connect to your cloud. Close this tab, go back to Echo and try again.</p><div class="err">{{ .Name }}{{ if .Description }}<br>{{ .Description }}{{ end }}{{ if .Code }}<br>Code: {{ .Code }}{{ end }}</div>{{ end }}<p class="small">Echo · echoclip.link</p></div></body></html>"##;
/// a clip was uploaded since "rclone serve" listed the folders: restart it before the next stream (chat 28)
static SERVE_STALE: AtomicBool = AtomicBool::new(false);
/// port of the last streaming server (reused, the player keeps the address)
static SERVE_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
/// last time the window asked for the streaming server (unix s)
static SERVE_USED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn ec_api(cfg: &crate::settings::Settings) -> String {
    let a = std::env::var("ECHO_CLOUD_API").ok().filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| if cfg.ec_api.trim().is_empty() { EC_API_DEFAULT.to_string() } else { cfg.ec_api.trim().to_string() });
    a.trim_end_matches('/').to_string()
}

/// Call the Echo Cloud server (JSON in and out). Server errors come back as Err(message).
fn ec_call(api: &str, method: &str, path: &str, token: &str, body: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
    let mut h = vec![("content-type".to_string(), "application/json".to_string())];
    if !token.is_empty() {
        h.push(("authorization".into(), format!("Bearer {token}")));
    }
    let (code, json) = match crate::net::request(method, &format!("{api}{path}"), h, body.map(|b| b.to_string()), 30) {
        Ok(r) => r,
        Err(e) => {
            // log why (DNS, TLS, timeout…)
            log_line(&format!("echo cloud: {api}{path} unreachable: {e}"));
            return Err("Couldn't reach Echo Cloud - check your internet".into());
        }
    };
    let json = json.as_str();
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    if let Some(e) = v["error"].as_str() {
        return Err(e.to_string());
    }
    if code >= 300 {
        return Err(format!("Echo Cloud answered {code}"));
    }
    Ok(v)
}

/// Put fresh keys into the rclone remote.
fn ec_apply(acc: &str, j: &serde_json::Value) -> Result<(), String> {
    let g = |k: &str| j[k].as_str().unwrap_or("").to_string();
    let (id, secret, tok) = (g("access_key_id"), g("secret_access_key"), g("session_token"));
    if id.is_empty() || secret.is_empty() {
        return Err("Echo Cloud sent no storage keys".into());
    }
    let o = rclone_cmd_raw()?.args(["config", "update", acc, "access_key_id", &id, "secret_access_key", &secret, "session_token", &tok])
        .stdout(Stdio::null()).stderr(Stdio::piped()).output().map_err(err)?;
    if !o.status.success() {
        return Err(format!("couldn't save the Echo Cloud keys: {}", String::from_utf8_lossy(&o.stderr).trim()));
    }
    Ok(())
}

/// Renew the keys when they run out in the next 10 minutes (called before every rclone command).
fn ec_ensure() {
    let mut g = EC.lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = g.as_mut() else { return };
    if s.exp > now_unix() + 600 {
        return;
    }
    match ec_call(&s.api, "POST", "/creds", &s.token, Some(serde_json::json!({}))) {
        Ok(j) => match ec_apply(&s.acc, &j) {
            Ok(()) => {
                s.exp = j["expires"].as_u64().unwrap_or(now_unix() + 3600);
                s.read_only = j["read_only"].as_bool().unwrap_or(false);
                s.used = j["used"].as_u64().unwrap_or(s.used);
                s.quota = j["quota"].as_u64().unwrap_or(s.quota);
                EC_NEW_KEYS.store(true, Ordering::Relaxed);
                log_line(&format!("echo cloud: new storage keys{}", if s.read_only { " (read only - over the quota)" } else { "" }));
            }
            Err(e) => { log_line(&format!("echo cloud: {e}")); s.exp = now_unix() + 660; }
        },
        Err(e) => {
            log_line(&format!("echo cloud: no new keys: {e}"));
            // don't ask again on every rclone call: retry in a minute
            s.exp = now_unix() + 660;
        }
    }
}

/// Point the key renewal at the current Echo Cloud account (after start, sign-in, removal).
fn ec_sync_state(cfg: &crate::settings::Settings, exp: u64) {
    let acc = cfg.cloud_accounts.iter().find(|a| a.provider == "echocloud");
    let mut g = EC.lock().unwrap_or_else(|e| e.into_inner());
    *g = match acc {
        Some(a) if !cfg.ec_token.is_empty() => Some(EcState { api: ec_api(cfg), token: cfg.ec_token.clone(), acc: format!("acc_{}", a.id), exp, read_only: false, used: 0, quota: 0 }),
        _ => None,
    };
}

/// The plan got bigger (paid) while the keys were read-only: get new keys on the next cloud command.
fn ec_room_again(used: u64, total: u64) {
    if let Ok(mut g) = EC.lock() {
        if let Some(s) = g.as_mut() {
            if s.read_only && used < total { s.read_only = false; s.exp = 0; }
            if total > 0 { s.used = used; s.quota = total; }
        }
    }
}

/// Standard base64 (for small files sent to Echo Cloud as JSON).
fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::with_capacity((b.len() + 2) / 3 * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        o.push(T[(n >> 18 & 63) as usize] as char);
        o.push(T[(n >> 12 & 63) as usize] as char);
        o.push(if c.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        o.push(if c.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    o
}

/// Free space left in Echo Cloud as far as Echo knows (None = not known yet).
fn ec_space_left() -> Option<u64> {
    EC.lock().ok().and_then(|g| g.as_ref().and_then(|s| (s.quota > 0).then(|| s.quota.saturating_sub(s.used))))
}

/// Count an upload against the space left until the server's next number arrives.
fn ec_add_used(bytes: u64) {
    if let Ok(mut g) = EC.lock() {
        if let Some(s) = g.as_mut() { s.used = s.used.saturating_add(bytes); }
    }
}

/// Open a web page in the default browser (only https links from Echo Cloud).
pub fn open_web(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") || url.contains('"') {
        return Err("Echo Cloud sent no link - try again".into());
    }
    #[cfg(windows)]
    if !crate::winutil::open_url(url) {
        return Err("Couldn't open the browser".into());
    }
    #[cfg(not(windows))]
    sys_cmd("xdg-open").arg(url).spawn().map_err(err)?;
    Ok(())
}

fn ec_full() -> bool {
    EC.lock().map(|g| g.as_ref().map_or(false, |s| s.read_only)).unwrap_or(false)
}

const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// "2026/November/November 1st/FiveM/x.mp4" (or older "2026/11/01/…") -> "FiveM/x.mp4"
/// (the cloud's date folders aren't part of the PC layout)
fn strip_date(p: &str) -> String {
    let parts: Vec<&str> = p.splitn(4, '/').collect();
    let num = |x: &str, n: usize| x.len() == n && x.bytes().all(|b| b.is_ascii_digit());
    if parts.len() == 4 && num(parts[0], 4) {
        let numeric = num(parts[1], 2) && num(parts[2], 2);
        let words = MONTHS.contains(&parts[1]) && parts[2].starts_with(parts[1])
            && parts[2][parts[1].len()..].trim().chars().next().map_or(false, |c| c.is_ascii_digit());
        // "11 November" / "01 November 1st"
        let both = parts[1].len() > 3 && num(&parts[1][..2], 2) && MONTHS.contains(&&parts[1][3..])
            && parts[2].len() > 3 && num(&parts[2][..2], 2) && parts[2][3..].starts_with(&parts[1][3..]);
        // "11 - November" / "04 - Wednesday" (chat 22)
        let dashed = |x: &str| x.get(..2).map_or(false, |n| num(n, 2)) && x.get(2..5) == Some(" - ");
        let dash = dashed(parts[1]) && dashed(parts[2]) && parts[1].get(5..).map_or(false, |w| MONTHS.contains(&w))
            && parts[2].get(5..).map_or(false, |w| DAYS.contains(&w) || w == "Day");
        if numeric || words || both || dash {
            return parts[3].to_string();
        }
    }
    p.to_string()
}


/// Cloud date folder for a clip, e.g. "2026/11 - November/01 - Sunday": the date in its name
/// ("… 2026-11-01 08-18-00"), else the file's date.
fn date_folder(local: &Path) -> String {
    let stem = local.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let mut ymd: Option<(i32, u32, u32)> = None;
    for i in 0..stem.len().saturating_sub(9) {
        if !stem.is_char_boundary(i) || !stem.is_char_boundary(i + 10) { continue; }
        let w = &stem[i..i + 10];
        let ok = w.bytes().enumerate().all(|(k, c)| if k == 4 || k == 7 { c == b'-' } else { c.is_ascii_digit() });
        if ok {
            let (y, m, d) = (w[0..4].parse().unwrap_or(0), w[5..7].parse().unwrap_or(0), w[8..10].parse().unwrap_or(0));
            if (1..=12).contains(&m) && (1..=31).contains(&d) { ymd = Some((y, m, d)); break; }
        }
    }
    let (y, m, d) = ymd.unwrap_or_else(|| {
        use chrono::Datelike;
        let t: chrono::DateTime<chrono::Local> = modified(local).unwrap_or_else(SystemTime::now).into();
        (t.year(), t.month(), t.day())
    });
    let month = MONTHS[(m - 1) as usize];
    // number first so cloud websites sort them in calendar order, words after for reading:
    // "2026/11 - November/04 - Wednesday" (was "11 November/04 November 4th", read like a date - chat 22)
    use chrono::Datelike;
    let wd = chrono::NaiveDate::from_ymd_opt(y, m, d).map(|x| DAYS[x.weekday().num_days_from_monday() as usize]).unwrap_or("Day");
    format!("{y}/{m:02} - {month}/{d:02} - {wd}")
}

fn remote(rel: &str) -> String {
    format!("{CLOUD_ROOT}/{}", rel.trim_start_matches('/'))
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Recorder {
    fn cloud_state(&self) -> CloudState {
        fs::read_to_string(self.thumbs_dir().join("cloud.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    fn save_cloud_state(&self, st: &CloudState) {
        let _ = fs::create_dir_all(self.thumbs_dir());
        let _ = write_patient(&self.thumbs_dir().join("cloud.json"), &serde_json::to_string(st).unwrap_or_default());
    }
    /// Where a clip lives in the cloud (with its date folder), from the last listing; rel itself if unknown.
    fn cpath(&self, rel: &str) -> String {
        self.cloud_state().files.into_iter().find(|f| f.rel == rel && !f.path.is_empty()).map(|f| f.path).unwrap_or_else(|| rel.to_string())
    }
    fn cloud_on(&self) -> bool {
        !self.settings().cloud.is_empty() && cloud_conf().exists()
    }
    fn set_busy(&self, s: &str) {
        *self.cloud_busy.lock().unwrap() = s.to_string();
    }

    /// Path of a local clip inside the cloud folder ("Valorant/x.mp4"); None for non-clips.
    pub fn rel_of(&self, path: &str) -> Option<String> {
        let p = PathBuf::from(path);
        if !self.is_clip(&p) {
            return None;
        }
        let rel = p.strip_prefix(self.clips_dir()).ok()?.to_string_lossy().replace('\\', "/");
        Some(rel)
    }

    /// Add a cloud account. pCloud / Google Drive: browser login (rclone OAuth). MEGA / Filen: email + password
    /// (+ 2FA for Filen). Own server: WebDAV / SFTP / S3 / a folder (NAS share) - details in `extra`.
    pub fn cloud_connect(&self, provider: &str, user: &str, pass: &str, code: &str,
                         extra: &std::collections::HashMap<String, String>) -> Result<CloudInfo, String> {
        let _ = fs::create_dir_all(app_dir().join("rclone"));
        { let mut c = self.cfg.lock().unwrap(); migrate_cloud(&mut c); }
        let accs = self.settings().cloud_accounts;
        let id = accs.iter().map(|a| a.id).max().unwrap_or(0) + 1;
        let name = format!("acc_{id}");
        let x = |k: &str| extra.get(k).map(|v| v.trim().to_string()).unwrap_or_default();
        let need = |v: &str, what: &str| if v.trim().is_empty() { Err(format!("Enter the {what}")) } else { Ok(()) };
        log_line(&format!("cloud: adding {provider} as {name}"));
        let (mut label, mut root) = (user.trim().to_string(), String::new());
        let mut ec_exp = 0u64;
        let made = match provider {
            "pcloud" => rclone_out(&["config", "create", &name, "pcloud", "config_template", OAUTH_PAGE]),
            // drive.file: Echo only sees the files it made itself, not the rest of the Drive.
            // Echo's own Google login (project "Echo" in Google Cloud, owner w@volkan.dk, desktop client "Echo app
            // (rclone)", chat 28) instead of rclone's shared one, which Google is retiring. A desktop app's
            // "secret" isn't secret (Google says so - it ships inside every copy). Accounts connected before keep
            // rclone's login: drive.file only shows files made with the SAME login, so switching them would hide
            // their clips.
            "gdrive" if GDRIVE_CLIENT_SECRET.is_empty() => {
                log_line("cloud: this build has no Google login of its own - using rclone's");
                rclone_out(&["config", "create", &name, "drive", "scope", "drive.file", "config_template", OAUTH_PAGE])
            }
            "gdrive" => rclone_out(&["config", "create", &name, "drive", "scope", "drive.file",
                "client_id", GDRIVE_CLIENT_ID, "client_secret", GDRIVE_CLIENT_SECRET, "config_template", OAUTH_PAGE]),
            "mega" => {
                need(user, "MEGA email")?; need(pass, "MEGA password")?;
                rclone_out(&["config", "create", &name, "mega", "user", user.trim(), "pass", pass, "--obscure"])
            }
            "filen" => {
                need(user, "Filen email")?; need(pass, "Filen password")?;
                rclone_supports("filen")?;
                let key = filen_api_key(user.trim(), pass, code.trim())?;
                rclone_out(&["config", "create", &name, "filen", "email", user.trim(), "password", pass, "api_key", &key, "--obscure"])
            }
            "webdav" => {
                let url = x("url"); need(&url, "server address")?;
                label = url.clone();
                let vendor = if x("vendor").is_empty() { "other".to_string() } else { x("vendor") };
                rclone_out(&["config", "create", &name, "webdav", "url", &url, "vendor", &vendor,
                    "user", user.trim(), "pass", pass, "--obscure"])
            }
            "sftp" => {
                let host = x("host"); need(&host, "server address")?; need(user, "user name")?;
                let port = if x("port").is_empty() { "22".to_string() } else { x("port") };
                label = format!("{}@{host}", user.trim());
                root = x("path");
                let mut a = vec!["config", "create", &name, "sftp", "host", &host, "user", user.trim(), "port", &port];
                if !pass.is_empty() { a.extend(["pass", pass]); }
                a.push("--obscure");
                rclone_out(&a)
            }
            "s3" => {
                let (ep, bucket) = (x("url"), x("bucket")); need(&ep, "endpoint address")?; need(&bucket, "bucket name")?;
                need(user, "access key")?; need(pass, "secret key")?;
                label = format!("{bucket} @ {ep}");
                root = bucket;
                rclone_out(&["config", "create", &name, "s3", "provider", "Other", "endpoint", &ep,
                    "access_key_id", user.trim(), "secret_access_key", pass])
            }
            "echocloud" => {
                if accs.iter().any(|a| a.provider == "echocloud") {
                    return Err("You're already signed in to Echo Cloud".into());
                }
                let email = user.trim().to_lowercase();
                need(&email, "email")?; need(code, "code from the email")?;
                let api = ec_api(&self.settings());
                let v = ec_call(&api, "POST", "/auth/verify", "", Some(serde_json::json!({ "email": email, "code": code.trim() })))?;
                let token = v["token"].as_str().unwrap_or("").to_string();
                if token.is_empty() { return Err("Echo Cloud didn't sign you in - try again".into()); }
                {
                    let mut c = self.cfg.lock().unwrap();
                    c.ec_token = token.clone();
                    c.ec_email = email.clone();
                    let _ = c.save();
                }
                let j = ec_call(&api, "POST", "/creds", &token, Some(serde_json::json!({})))?;
                let g = |k: &str| j[k].as_str().unwrap_or("").to_string();
                label = email;
                root = g("root");
                ec_exp = j["expires"].as_u64().unwrap_or(0);
                rclone_out(&["config", "create", &name, "s3", "provider", "Cloudflare", "endpoint", &g("endpoint"), "region", "auto",
                    "access_key_id", &g("access_key_id"), "secret_access_key", &g("secret_access_key"), "session_token", &g("session_token"),
                    "no_check_bucket", "true"])
            }
            "folder" => {
                let path = x("path"); need(&path, "folder")?;
                if !Path::new(&path).is_dir() { return Err("That folder doesn't exist (is the drive / network share connected?)".into()); }
                label = path.clone();
                root = path.clone();
                rclone_out(&["config", "create", &name, "local"])
            }
            _ => return Err("unknown cloud".into()),
        };
        if let Err(e) = made {
            let _ = rclone_out(&["config", "delete", &name]);
            return Err(e);
        }
        let echo_cloud = provider == "echocloud";
        if label.is_empty() {
            label = format!("account {id}");
        }
        let acc = crate::settings::CloudAccount { id, provider: provider.into(), label, root };
        if let Err(e) = rclone_out(&["mkdir", &acc_path(&acc, "Echo")]) {
            let _ = rclone_out(&["config", "delete", &name]);
            return Err(format!("Signed in, but the cloud didn't answer: {e}"));
        }
        let mut all = accs.clone();
        all.push(acc);
        rebuild_echo_remote(&all)?;
        {
            let mut c = self.cfg.lock().unwrap();
            c.cloud = if all.len() > 1 { "multi".into() } else { provider.into() };
            c.cloud_accounts = all;
            // an own cloud was (re)connected: new clips go there, not on into Echo Cloud ("Echo Cloud first" or a
            // removed account's id was left over - chat 29: after reconnecting Google Drive, uploads kept filling Echo Cloud)
            if !echo_cloud && !c.cloud_target.is_empty() {
                log_line(&format!("cloud: uploads now go to your own clouds (was \"{}\")", c.cloud_target));
                c.cloud_target.clear();
            }
            if c.cloud_since == 0 {
                c.cloud_since = now_unix();
            }
            let _ = c.save();
        }
        if echo_cloud {
            ec_sync_state(&self.settings(), ec_exp);
        }
        let _ = rclone_out(&["mkdir", CLOUD_ROOT]);
        self.secure_cloud();
        let _ = self.cloud_refresh();
        Ok(self.cloud_info())
    }

    /// Remove one account (its clips stay in that cloud, Echo just stops using it).
    pub fn cloud_remove_account(&self, id: u32) -> Result<(), String> {
        { let mut c = self.cfg.lock().unwrap(); migrate_cloud(&mut c); }
        self.stop_serve();
        let mut all = self.settings().cloud_accounts;
        if all.iter().any(|a| a.id == id && a.provider == "echocloud") {
            self.ec_sign_out();
        }
        if all.iter().any(|a| a.id == id && a.provider == "echocloud") {
            // its links aren't handed out again (signing in to another account must not reuse them)
            let mut m = self.links();
            m.retain(|_, e| e.key.is_empty());
            self.save_links(&m);
        }
        all.retain(|a| a.id != id);
        let _ = rclone_out(&["config", "delete", &format!("acc_{id}")]);
        // the last listing still has that cloud's clips: the library must not show them any more.
        // The other clouds' clips come back with the next listing (the sync runs right after).
        let mut st = self.cloud_state();
        st.files.clear();
        self.save_cloud_state(&st);
        rebuild_echo_remote(&all)?;
        let mut c = self.cfg.lock().unwrap();
        c.cloud = match all.len() { 0 => String::new(), 1 => all[0].provider.clone(), _ => "multi".into() };
        // the upload target was that account (or only one cloud is left): back to the default
        if c.cloud_target == id.to_string() || all.len() < 2 { c.cloud_target.clear(); }
        c.cloud_accounts = all;
        let _ = c.save();
        ec_sync_state(&c, 0);
        log_line(&format!("cloud: removed account {id}"));
        Ok(())
    }

    /// Startup / after adding a cloud: make sure the cloud logins are stored encrypted (Windows).
    pub fn secure_cloud(&self) {
        let mut c = self.cfg.lock().unwrap();
        set_rclone_pass(&c.rclone_pass);
        secure_cloud_conf(&mut c);
    }

    /// Is the Echo Cloud server set up (address filled in)?
    pub fn echocloud_ready(&self) -> bool {
        !ec_api(&self.settings()).contains("REPLACE_ME")
    }

    /// Echo Cloud sign-in step 1: email the 6-digit code.
    pub fn echocloud_start(&self, email: &str) -> Result<(), String> {
        let email = email.trim().to_lowercase();
        if email.is_empty() { return Err("Enter your email".into()); }
        ec_call(&ec_api(&self.settings()), "POST", "/auth/start", "", Some(serde_json::json!({ "email": email }))).map(|_| ())
    }

    /// Echo Cloud plans for sale (the server reads them from Polar).
    pub fn echocloud_plans(&self) -> Result<serde_json::Value, String> {
        let v = ec_call(&ec_api(&self.settings()), "GET", "/plans", "", None)?;
        Ok(v["plans"].clone())
    }

    /// Open the checkout for a plan in the browser (already paying: the page to change or cancel it).
    pub fn echocloud_checkout(&self, product_id: &str) -> Result<(), String> {
        let cfg = self.settings();
        if cfg.ec_token.is_empty() { return Err("Sign in to Echo Cloud first".into()); }
        let j = ec_call(&ec_api(&cfg), "POST", "/checkout", &cfg.ec_token, Some(serde_json::json!({ "product_id": product_id })))?;
        log_line("echo cloud: opened the checkout");
        open_web(j["url"].as_str().unwrap_or(""))
    }

    /// Open the subscription page (change plan, cancel, invoices, card).
    pub fn echocloud_manage(&self) -> Result<(), String> {
        let cfg = self.settings();
        if cfg.ec_token.is_empty() { return Err("Sign in to Echo Cloud first".into()); }
        let j = ec_call(&ec_api(&cfg), "POST", "/portal", &cfg.ec_token, Some(serde_json::json!({})))?;
        open_web(j["url"].as_str().unwrap_or(""))
    }

    /// Plan + usage of the signed-in Echo Cloud account.
    /// Echo profile (echoclip.link/u/<name>): only these Echo Cloud calls are allowed from the window
    pub fn ec_profile(&self, method: &str, path: &str, body: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
        const OK: [(&str, &str); 6] = [("GET", "/profile/me"), ("POST", "/profile"), ("POST", "/profile/pin"), ("POST", "/profile/delete"),
            ("POST", "/follow"), ("POST", "/profile/avatar")];
        if !OK.contains(&(method, path)) {
            return Err("not allowed".into());
        }
        let cfg = self.settings();
        if cfg.ec_token.is_empty() {
            return Err("Sign in to Echo Cloud first (Settings → Cloud)".into());
        }
        ec_call(&ec_api(&cfg), method, path, &cfg.ec_token, body)
    }

    /// Profile picture: any picture (first frame of a GIF) -> 256x256 JPEG, centre crop -> Echo Cloud
    pub fn profile_avatar(&self, path: &str) -> Result<serde_json::Value, String> {
        let tmp = std::env::temp_dir().join(format!("echo-avatar-{}.jpg", std::process::id()));
        let ok = ffmpeg()
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(path)
            .args(["-vf", "scale=256:256:force_original_aspect_ratio=increase,crop=256:256", "-frames:v", "1", "-q:v", "4"])
            .arg(&tmp)
            .stdout(Stdio::null()).stderr(Stdio::null()).status().map_err(err)?.success();
        let bytes = fs::read(&tmp);
        let _ = fs::remove_file(&tmp);
        let bytes = match (ok, bytes) {
            (true, Ok(b)) if !b.is_empty() => b,
            _ => return Err("Couldn't read that picture - try a PNG or JPG".into()),
        };
        self.ec_profile("POST", "/profile/avatar", Some(serde_json::json!({ "img": base64(&bytes) })))
    }

    /// How often each shared clip was watched on its share page: {clip name (no .mp4): views}. Only Echo Cloud
    /// links (echoclip.link/s/<id>) count; other clouds' links can't be counted.
    pub fn share_views(&self) -> Result<std::collections::BTreeMap<String, u64>, String> {
        let cfg = self.settings();
        let mut out = std::collections::BTreeMap::new();
        if cfg.ec_token.is_empty() {
            return Ok(out);
        }
        let now = now_unix();
        let ids: Vec<(String, String)> = self.links().into_iter()
            .filter(|(_, e)| e.expires == 0 || e.expires > now)
            .filter_map(|(rel, e)| {
                let id = e.url.split("/s/").nth(1)?.split(['?', '#', '.']).next()?.to_string();
                let stem = Path::new(&rel).file_stem()?.to_string_lossy().to_string();
                (!id.is_empty()).then_some((stem, id))
            }).collect();
        if ids.is_empty() {
            return Ok(out);
        }
        let v = ec_call(&ec_api(&cfg), "POST", "/shares/views", &cfg.ec_token,
            Some(serde_json::json!({ "ids": ids.iter().map(|(_, i)| i.clone()).collect::<Vec<_>>() })))?;
        for (stem, id) in ids {
            if let Some(n) = v["views"][&id].as_u64() {
                *out.entry(stem).or_insert(0) += n;
            }
        }
        Ok(out)
    }

    pub fn echocloud_me(&self) -> Result<serde_json::Value, String> {
        let cfg = self.settings();
        if cfg.ec_token.is_empty() { return Err("Sign in to Echo Cloud first".into()); }
        ec_call(&ec_api(&cfg), "GET", "/me?fresh=1", &cfg.ec_token, None)
    }

    fn ec_sign_out(&self) {
        let cfg = self.settings();
        if !cfg.ec_token.is_empty() {
            let _ = ec_call(&ec_api(&cfg), "POST", "/auth/logout", &cfg.ec_token, Some(serde_json::json!({})));
        }
        let mut c = self.cfg.lock().unwrap();
        c.ec_token.clear();
        let _ = c.save();
    }

    pub fn cloud_disconnect(&self) {
        self.stop_serve();
        self.ec_sign_out();
        let _ = fs::remove_file(cloud_conf());
        CLOUD_LOCKED.store(false, Ordering::Relaxed);
        // signed out: no cloud-only clips in the library, no old links handed out
        let mut st = self.cloud_state();
        st.files.clear();
        self.save_cloud_state(&st);
        let _ = fs::remove_file(self.thumbs_dir().join("links.json"));
        let mut c = self.cfg.lock().unwrap();
        c.cloud.clear();
        c.cloud_accounts.clear();
        let _ = c.save();
        ec_sync_state(&c, 0);
        log_line("cloud: signed out");
    }

    pub fn cloud_info(&self) -> CloudInfo {
        { let mut c = self.cfg.lock().unwrap(); migrate_cloud(&mut c); }
        let on = self.cloud_on();
        let cfg = self.settings();
        let provider = if on { cfg.cloud.clone() } else { String::new() };
        let (mut used, mut total) = (0, 0);
        let mut accounts = Vec::new();
        if on {
            for a in &cfg.cloud_accounts {
                let mut ai = AccountInfo { id: a.id, provider: a.provider.clone(), label: a.label.clone(), used: 0, total: 0, error: String::new() };
                if a.provider == "echocloud" {
                    let t0 = Instant::now();
                    let r = ec_call(&ec_api(&cfg), "GET", "/me", &cfg.ec_token, None);
                    if t0.elapsed() > Duration::from_secs(2) { log_line(&format!("cloud: Echo Cloud space check took {:.1} s", t0.elapsed().as_secs_f32())); }
                    match r {
                        Ok(v) => { ai.used = v["used"].as_u64().unwrap_or(0); ai.total = v["quota"].as_u64().unwrap_or(0); ec_room_again(ai.used, ai.total); }
                        Err(e) => ai.error = e,
                    }
                    used += ai.used;
                    total += ai.total;
                    accounts.push(ai);
                    continue;
                }
                let t0 = Instant::now();
                let r = rclone_out(&["about", &acc_remote(a), "--json"]);
                if t0.elapsed() > Duration::from_secs(2) { log_line(&format!("cloud: {} space check took {:.1} s", a.provider, t0.elapsed().as_secs_f32())); }
                match r {
                    Ok(j) => if let Ok(v) = serde_json::from_str::<serde_json::Value>(&j) {
                        ai.used = v["used"].as_u64().unwrap_or(0);
                        ai.total = v["total"].as_u64().unwrap_or(0);
                    },
                    Err(e) => ai.error = e,
                }
                used += ai.used;
                total += ai.total;
                accounts.push(ai);
            }
        }
        CloudInfo { provider, used, total, accounts, busy: self.cloud_busy.lock().unwrap().clone() }
    }

    /// List the cloud folder (and remember it).
    pub fn cloud_refresh(&self) -> Result<Vec<CloudFile>, String> {
        if !self.cloud_on() {
            return Ok(Vec::new());
        }
        let j = match rclone_out(&["lsjson", "-R", "--files-only", CLOUD_ROOT]) {
            Ok(j) => j,
            Err(e) if e.contains("not found") => {
                // Echo folder missing (deleted on the website?) - recreate it
                rclone_out(&["mkdir", CLOUD_ROOT])?;
                "[]".into()
            }
            Err(e) => return Err(e),
        };
        let v: Vec<serde_json::Value> = serde_json::from_str(&j).map_err(err)?;
        let root = self.clips_dir();
        let files: Vec<CloudFile> = v.iter().filter_map(|f| {
            let path = f["Path"].as_str()?.to_string();
            let rel = strip_date(&path);
            if !rel.to_lowercase().ends_with(".mp4") || rel.split('/').any(|p| p.starts_with('.')) {
                return None;
            }
            let p = Path::new(&rel);
            let folder = p.parent().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
            let modified = f["ModTime"].as_str().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()).map(|t| t.timestamp().max(0) as u64).unwrap_or(0);
            Some(CloudFile {
                name: p.file_stem()?.to_string_lossy().to_string(),
                folder,
                size: f["Size"].as_u64().unwrap_or(0),
                modified,
                local: root.join(&rel).exists(),
                rel,
                path,
            })
        }).collect();
        let mut st = self.cloud_state();
        st.files = files.clone();
        self.save_cloud_state(&st);
        Ok(files)
    }

    /// Last known cloud listing (fast, no network), with "on this PC" refreshed.
    pub fn cloud_files(&self) -> Vec<CloudFile> {
        if !self.cloud_on() {
            return Vec::new();
        }
        let root = self.clips_dir();
        self.cloud_state().files.into_iter().map(|mut f| { f.local = root.join(&f.rel).exists(); f }).collect()
    }

    pub fn cloud_upload(&self, path: &str) -> Result<(), String> {
        self.upload_inner(path, false)
    }

    /// "Copy share link" on a clip that's only on this PC: upload it (to Echo Cloud when there is one and it
    /// has room, so the link plays in Discord), then make the link. Returns the link (also copied by the caller).
    pub fn upload_and_link(&self, path: &str, days: Option<u32>, progress: &dyn Fn(f64)) -> Result<String, String> {
        let rel = self.rel_of(path).ok_or("not a clip")?;
        let cfg = self.settings();
        let has_ec = cfg.cloud_accounts.iter().any(|a| a.provider == "echocloud") && !cfg.ec_token.is_empty();
        // linked before: the same link again, nothing to upload
        if let Ok(u) = self.reuse_link(&rel, days.map(|d| d.min(3650)).unwrap_or(cfg.link_days)) {
            progress(1.0);
            return Ok(u);
        }
        // a share copy (watermark / AV1) is all the link needs: the whole clip doesn't have to go up first
        if !(has_ec && is_av1(path)) && !self.cloud_state().files.iter().any(|f| f.rel == rel) {
            let fsize = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let to_ec = upload_target(&cfg, fsize, true).map_or(cfg.cloud_accounts.len() == 1, |a| a.provider == "echocloud");
            if has_ec && !to_ec {
                // the clip would go to the other cloud (Google Drive took 47 s for 69 MB at "0%", chat 20) and the link
                // then needs a copy in Echo Cloud anyway: only that copy goes up (cloud_link -> link_via_copy)
                log_line(&format!("cloud: link for {rel} without uploading the whole clip first"));
            } else {
                // the upload's progress shows on the card as "Making link N%" (it used to sit at 0% until the link)
                let r = std::thread::scope(|sc| {
                    let h = sc.spawn(|| self.upload_inner(path, true));
                    while !h.is_finished() {
                        let f = { let p = UP_PROGRESS.lock().unwrap(); (p.rel == rel && p.total > 0).then(|| p.got as f64 / p.total as f64) };
                        if let Some(f) = f { progress(f.min(1.0) * 0.95); }
                        std::thread::sleep(Duration::from_millis(250));
                    }
                    h.join().unwrap_or_else(|_| Err("upload crashed".into()))
                });
                r?;
            }
        }
        self.cloud_link(&rel, days, progress)
    }

    fn upload_inner(&self, path: &str, for_link: bool) -> Result<(), String> {
        let rel = self.rel_of(path).ok_or("not a clip")?;
        ec_ensure();
        let only_ec = self.settings().cloud_accounts.len() == 1 && EC.lock().map(|g| g.is_some()).unwrap_or(false);
        if ec_full() && only_ec {
            return Err("Echo Cloud is full - delete some clips from the cloud or upgrade your plan".into());
        }
        // pause instead of going over the plan: a clip that doesn't fit isn't uploaded
        // (the server also stops handing out upload keys once the account is full)
        if only_ec {
            let need = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            if let Some(left) = ec_space_left() {
                if need > left {
                    log_line(&format!("echo cloud: upload paused - {rel} needs {:.2} GB, {:.2} GB left", need as f64 / 1e9, left as f64 / 1e9));
                    return Err(format!("Echo Cloud is full - this clip needs {:.1} GB but only {:.1} GB is left. Uploads are paused until you free up space or upgrade your plan.",
                        need as f64 / 1e9, left as f64 / 1e9));
                }
            }
        }
        let t0 = Instant::now();
        // the clip's own date: the cloud entry keeps it (now_unix() made a "freed up" clip jump to the top of the
        // library as if it were brand new until the next cloud listing - chat 28)
        let made = fs::metadata(path).ok().and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or_else(now_unix, |d| d.as_secs());
        // AV1 clips go up as H.264, so share links play on every phone and browser
        let up = self.shareable(path)?;
        let path = up.as_str();
        // new uploads go into Echo/<year>/<month>/<day>/<folder>/; re-uploads overwrite where the clip already is
        let known = self.cloud_state().files.into_iter().find(|f| f.rel == rel).map(|f| if f.path.is_empty() { f.rel } else { f.path });
        let cp = known.unwrap_or_else(|| format!("{}/{rel}", date_folder(Path::new(path))));
        // live progress for the UI (card bar, bottom bar, player)
        let fsize = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        // several clouds: the one picked in Settings → Cloud ("Upload new clips to"), else the union decides
        let cfg = self.settings();
        let target = upload_target(&cfg, fsize, for_link);
        // where it's going, for the progress box. The union puts new files on the writable cloud with the most
        // free space (Echo Cloud is read-only there): with one other cloud that's always it
        let dest = match &target {
            Some(a) => cloud_name(&a.provider).to_string(),
            None => {
                let w: Vec<_> = cfg.cloud_accounts.iter().filter(|a| a.provider != "echocloud").collect();
                match (cfg.cloud_accounts.len(), w.len()) {
                    (1, _) => cloud_name(&cfg.cloud_accounts[0].provider).to_string(),
                    (_, 1) => cloud_name(&w[0].provider).to_string(),
                    _ => "the cloud with the most free space".to_string(),
                }
            }
        };
        UP_CANCEL.store(false, Ordering::Relaxed);
        {
            let mut p = UP_PROGRESS.lock().unwrap();
            if p.count == 0 { p.idx = 1; p.count = 1; }
            p.rel = rel.clone(); p.got = 0; p.total = fsize; p.dest = dest.clone();
        }
        // Echo Cloud: one size-checked upload link per file from the server (keys can't upload past the plan)
        let to_ec = match &target {
            Some(a) => a.provider == "echocloud",
            None => cfg.cloud_accounts.len() == 1 && cfg.cloud_accounts[0].provider == "echocloud",
        } && !cfg.ec_token.is_empty();
        let ec_key = format!("Echo/{}", cp.trim_start_matches('/'));
        let mut r: Result<(), String> = Err(String::new());
        let mut via_server = false;
        if to_ec {
            match ec_call(&ec_api(&cfg), "POST", "/upload", &cfg.ec_token, Some(serde_json::json!({ "path": ec_key, "size": fsize }))) {
                Ok(j) => {
                    via_server = true;
                    let url = j["url"].as_str().unwrap_or("").to_string();
                    let rel2 = rel.clone();
                    r = crate::net::put_file(&url, Path::new(path), move |b, t| {
                        let mut p = UP_PROGRESS.lock().unwrap();
                        if p.rel == rel2 { p.got = b; p.total = t; }
                    }, || UP_CANCEL.load(Ordering::Relaxed)).map_err(|e| {
                        if UP_CANCEL.load(Ordering::Relaxed) { return "Upload cancelled".to_string(); }
                        log_line(&format!("echo cloud: upload of {rel} failed: {e}")); format!("Upload to Echo Cloud failed: {e}")
                    });
                }
                // older server without /upload: the keys still work
                Err(e) if e.contains("not found") => log_line("echo cloud: server has no /upload yet - uploading with keys"),
                Err(e) => r = Err(e),
            }
        }
        if !via_server && (r.as_ref().err().map_or(true, |e| e.is_empty())) {
            let dst = target.as_ref()
                .map(|a| acc_path(a, &ec_key))
                .unwrap_or_else(|| remote(&cp));
            r = rclone_progress(&["copyto", path, &dst], |b, t| {
                let mut p = UP_PROGRESS.lock().unwrap();
                if p.rel == rel { p.got = b; if t > 0 { p.total = t; } }
            });
        }
        {
            let mut p = UP_PROGRESS.lock().unwrap();
            if p.rel == rel { p.rel.clear(); if p.idx >= p.count { p.idx = 0; p.count = 0; } }
        }
        if UP_CANCEL.swap(false, Ordering::Relaxed) && r.is_err() {
            log_line(&format!("cloud: upload of {rel} to {dest} cancelled"));
            let mut sk = UP_SKIPPED.lock().unwrap();
            if !sk.contains(&rel) { sk.push(rel.clone()); }
            return Err("Upload cancelled".into());
        }
        r?;
        ec_add_used(fsize);
        // the streaming server's folder listing is from before this upload: it couldn't find the new file
        // (player stuck at 0 s for ~15 s, then "Couldn't stream this clip" - chat 28) -> fresh start on next use
        if self.cloud_serve.lock().unwrap().is_some() { SERVE_STALE.store(true, Ordering::Relaxed); }
        log_line(&format!("cloud: uploaded {rel} to {dest} in {:.0}s ({})", t0.elapsed().as_secs_f32(),
            if via_server { "Echo Cloud, size-checked upload link" } else if to_ec { "Echo Cloud, keys" } else { "rclone" }));
        let mut st = self.cloud_state();
        st.keep_remote.remove(&rel);
        st.no_upload.remove(&rel);
        let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        st.files.retain(|f| f.rel != rel);
        let p = Path::new(&rel);
        st.files.push(CloudFile {
            rel: rel.clone(),
            path: cp,
            name: p.file_stem().unwrap_or_default().to_string_lossy().into(),
            folder: p.parent().map(|x| x.to_string_lossy().to_string()).unwrap_or_default(),
            size, modified: made, local: true,
        });
        self.save_cloud_state(&st);
        Ok(())
    }

    /// Download a cloud clip into the clips folder (same sub-folder). Returns the local path.
    /// (bytes so far, total bytes) of the cloud download in progress for `rel`
    pub fn cloud_dl_progress(rel: &str) -> (u64, u64) {
        DL_PROGRESS.lock().unwrap().get(rel).copied().unwrap_or((0, 0))
    }

    pub fn cloud_download(&self, rel: &str) -> Result<String, String> {
        let out = self.clips_dir().join(rel);
        if let Some(d) = out.parent() {
            fs::create_dir_all(d).map_err(err)?;
        }
        let t0 = Instant::now();
        // progress for the UI: watch the (partial) file grow next to the destination
        let total = self.cloud_state().files.iter().find(|f| f.rel == rel).map_or(0, |f| f.size);
        // the same clip already downloading (player + a button, or "get clips from my other PCs"): two rclone runs
        // into one file made the progress jump back to 0 - wait for that one instead
        let mut waited = false;
        loop {
            let mut p = DL_PROGRESS.lock().unwrap();
            if !p.contains_key(rel) { p.insert(rel.to_string(), (0, total)); break; }
            drop(p);
            waited = true;
            std::thread::sleep(Duration::from_millis(300));
        }
        if waited && out.exists() && (total == 0 || fs::metadata(&out).map_or(0, |m| m.len()) == total) {
            DL_PROGRESS.lock().unwrap().remove(rel);
            return Ok(out.to_string_lossy().to_string());
        }
        let done = Arc::new(AtomicBool::new(false));
        {
            let (done, out, rel) = (done.clone(), out.clone(), rel.to_string());
            std::thread::spawn(move || {
                let name = out.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                while !done.load(Ordering::Relaxed) {
                    // fs::metadata on the path: on Windows the folder listing's size of a file that's still being
                    // written is out of date (often 0)
                    let got: u64 = out.parent().and_then(|d| fs::read_dir(d).ok()).into_iter().flatten().flatten()
                        .filter(|e| e.file_name().to_string_lossy().starts_with(&name))
                        .filter_map(|e| fs::metadata(e.path()).ok()).map(|m| m.len()).max().unwrap_or(0);
                    if let Some(p) = DL_PROGRESS.lock().unwrap().get_mut(&rel) { p.0 = p.0.max(got); }
                    std::thread::sleep(Duration::from_millis(300));
                }
            });
        }
        // fast path: through the streaming server that's already running (its folder look-ups are cached) - a new
        // rclone process per download first searched the folders again: ~20 s at 0 % even for a 2 MB clip (chat 24)
        let fast = self.serve_download(rel, &out);
        if let Err(e) = &fast { log_line(&format!("cloud: quick download of {rel} didn't work ({e}) - using rclone")); }
        let r = if fast.is_ok() { Ok(String::new()) } else { rclone_out(&["copyto", &remote(&self.cpath(rel)), &out.to_string_lossy()]) };
        done.store(true, Ordering::Relaxed);
        DL_PROGRESS.lock().unwrap().remove(rel);
        r?;
        log_line(&format!("cloud: downloaded {rel} in {:.0}s{}", t0.elapsed().as_secs_f32(), if fast.is_ok() { " (streaming server)" } else { "" }));
        let mut st = self.cloud_state();
        st.keep_remote.remove(rel);
        self.save_cloud_state(&st);
        let out = out.to_string_lossy().to_string();
        let _ = self.thumb(&out);
        Ok(out)
    }

    /// Download one cloud clip over the local streaming server (rclone serve http) into `out`, keeping the clip's
    /// date (Last-Modified). Writes "<name>.echodl" first (the progress watcher sees it grow), renamed when complete.
    fn serve_download(&self, rel: &str, out: &Path) -> Result<(), String> {
        use std::io::{BufRead, BufReader, Write};
        let base = self.cloud_stream_base()?;
        let rest = base.strip_prefix("http://").ok_or("bad address")?;
        let (host, tok) = rest.split_once('/').ok_or("bad address")?;
        let enc = |seg: &str| seg.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
        let path = format!("/{tok}/{}", self.cpath(rel).trim_start_matches('/').split('/').map(enc).collect::<Vec<_>>().join("/"));
        // the server process may still be starting (it answers only once it has connected to the cloud):
        // "connection refused" -> try again for up to 20 s
        let t0 = Instant::now();
        let mut sock = loop {
            match std::net::TcpStream::connect(host) {
                Ok(c) => break c,
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused && t0.elapsed() < Duration::from_secs(20) => {
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(e) => return Err(err(e)),
            }
        };
        if t0.elapsed() > Duration::from_secs(1) { log_line(&format!("cloud: streaming server answered after {:.1} s", t0.elapsed().as_secs_f32())); }
        let _ = sock.set_read_timeout(Some(Duration::from_secs(60)));
        // HTTP/1.0: a plain body, no chunked encoding
        write!(sock, "GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n").map_err(err)?;
        let mut r = BufReader::with_capacity(1 << 20, sock);
        let mut line = String::new();
        r.read_line(&mut line).map_err(err)?;
        if line.split_whitespace().nth(1) != Some("200") { return Err(format!("server answered {}", line.trim())); }
        let (mut len, mut modified): (Option<u64>, Option<SystemTime>) = (None, None);
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).map_err(err)? == 0 || h.trim().is_empty() { break; }
            let (k, v) = match h.split_once(':') { Some((k, v)) => (k.trim().to_ascii_lowercase(), v.trim().to_string()), None => continue };
            if k == "content-length" { len = v.parse().ok(); }
            if k == "last-modified" {
                modified = chrono::DateTime::parse_from_rfc2822(&v).ok()
                    .and_then(|d| u64::try_from(d.timestamp()).ok()).map(|t| SystemTime::UNIX_EPOCH + Duration::from_secs(t));
            }
        }
        let tmp = PathBuf::from(format!("{}.echodl", out.to_string_lossy()));
        let mut f = fs::File::create(&tmp).map_err(err)?;
        let n = match std::io::copy(&mut r, &mut f) { Ok(n) => n, Err(e) => { drop(f); let _ = fs::remove_file(&tmp); return Err(err(e)); } };
        if let Some(t) = modified { let _ = f.set_modified(t); }
        drop(f);
        if n == 0 || len.map_or(false, |l| l != n) {
            let _ = fs::remove_file(&tmp);
            return Err(format!("got {n} of {} bytes", len.map_or("?".into(), |l| l.to_string())));
        }
        fs::rename(&tmp, out).map_err(err)
    }

    /// Free up space: delete the PC copy once the cloud has the whole file (thumbnail stays).
    pub fn cloud_remove_local(&self, path: &str) -> Result<(), String> {
        let rel = self.rel_of(path).ok_or("not a clip")?;
        let size = fs::metadata(path).map_err(err)?.len();
        let j = rclone_out(&["lsjson", &remote(&self.cpath(&rel))]).map_err(|_| "This clip isn't in the cloud yet - upload it first".to_string())?;
        let v: Vec<serde_json::Value> = serde_json::from_str(&j).unwrap_or_default();
        if v.first().and_then(|f| f["Size"].as_u64()) != Some(size) {
            return Err("The cloud copy isn't complete yet - upload it first".into());
        }
        fs::remove_file(path).map_err(err)?;
        self.remove_if_empty(Path::new(path).parent());
        let mut st = self.cloud_state();
        st.keep_remote.insert(rel.clone());
        self.save_cloud_state(&st);
        log_line(&format!("cloud: removed PC copy of {rel}"));
        Ok(())
    }

    pub fn cloud_delete(&self, rel: &str) -> Result<(), String> {
        // a link that plays a share copy (Echo/.shared/…) keeps working after the clip's cloud copy is deleted
        // (the server only ends links of the deleted file) - so remember it, or "Shared" + "Stop sharing" vanished
        // while the link still worked (chat 27)
        if !self.links().get(rel).map_or(false, |e| e.key.contains("/.shared/")) { self.forget_link(rel); }
        let cp = self.cpath(rel);
        let cfg = self.settings();
        // Echo Cloud: delete through the server (works with read-only keys); other clouds: rclone as before
        let mut server_ok = false;
        if cfg.cloud_accounts.iter().any(|a| a.provider == "echocloud") && !cfg.ec_token.is_empty() {
            server_ok = ec_call(&ec_api(&cfg), "POST", "/delete", &cfg.ec_token,
                Some(serde_json::json!({ "paths": [format!("Echo/{}", cp.trim_start_matches('/'))] }))).is_ok();
        }
        let only_ec = cfg.cloud_accounts.len() == 1 && cfg.cloud_accounts[0].provider == "echocloud";
        if server_ok && !only_ec {
            // Echo Cloud's copy is gone; a copy on the other clouds (Google Drive answers slowly) is removed in the
            // background, so the button doesn't wait ~10 s for a copy that usually isn't there
            let cp2 = cp.clone();
            std::thread::spawn(move || { let _ = rclone_out(&["deletefile", "--retries", "1", &remote(&cp2)]); });
        } else if !server_ok {
            rclone_out(&["deletefile", "--retries", "1", &remote(&cp)])?;
        }
        let mut st = self.cloud_state();
        st.files.retain(|f| f.rel != rel);
        st.keep_remote.remove(rel);
        // still on this PC: the background sync must not upload it straight back
        if self.clips_dir().join(rel).exists() { st.no_upload.insert(rel.to_string()); }
        self.save_cloud_state(&st);
        log_line(&format!("cloud: deleted {rel}"));
        Ok(())
    }

    /// The Echo Cloud account (signed in) or an error for the UI.
    fn ec_account(&self) -> Result<crate::settings::CloudAccount, String> {
        let cfg = self.settings();
        match cfg.cloud_accounts.iter().find(|a| a.provider == "echocloud") {
            Some(a) if !cfg.ec_token.is_empty() => Ok(a.clone()),
            _ => Err("Sign in to Echo Cloud first".into()),
        }
    }

    /// Everything in Echo Cloud (clips + share copies), newest first, with the account's use and plan.
    pub fn echocloud_files(&self) -> Result<serde_json::Value, String> {
        let acc = self.ec_account()?;
        ec_ensure();
        let j = match rclone_out(&["lsjson", "-R", "--files-only", &acc_path(&acc, "Echo")]) {
            Ok(j) => j,
            Err(e) if e.contains("not found") => "[]".into(),
            Err(e) => return Err(format!("Couldn't list Echo Cloud: {e}")),
        };
        let v: Vec<serde_json::Value> = serde_json::from_str(&j).map_err(err)?;
        let root = self.clips_dir();
        let mut files: Vec<EcFile> = v.iter().filter_map(|f| {
            let path = f["Path"].as_str()?.to_string();
            let copy = path.split('/').any(|p| p.starts_with('.'));
            let rel = strip_date(&path);
            let p = Path::new(&rel);
            let size = f["Size"].as_u64().unwrap_or(0);
            let modified = f["ModTime"].as_str().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()).map(|t| t.timestamp().max(0) as u64).unwrap_or(0);
            Some(EcFile {
                name: p.file_stem()?.to_string_lossy().to_string(),
                folder: if copy { String::new() } else { p.parent().map(|x| x.to_string_lossy().to_string()).unwrap_or_default() },
                local: !copy && fs::metadata(root.join(&rel)).map_or(false, |m| m.len() == size),
                size, modified, copy, rel, path,
            })
        }).collect();
        files.sort_by(|a, b| b.modified.cmp(&a.modified));
        // the library's listing should know every Echo Cloud clip (downloads look their path up there)
        let mut st = self.cloud_state();
        let mut added = false;
        for f in files.iter().filter(|f| !f.copy && f.rel.to_lowercase().ends_with(".mp4")) {
            if !st.files.iter().any(|x| x.rel == f.rel) {
                st.files.push(CloudFile { rel: f.rel.clone(), path: f.path.clone(), name: f.name.clone(), folder: f.folder.clone(), size: f.size, modified: f.modified, local: f.local });
                added = true;
            }
        }
        if added { self.save_cloud_state(&st); }
        let me = self.echocloud_me().unwrap_or(serde_json::Value::Null);
        let used: u64 = files.iter().map(|f| f.size).sum();
        log_line(&format!("echo cloud: space manager listed {} files ({:.2} GB)", files.len(), used as f64 / 1e9));
        Ok(serde_json::json!({
            "files": files,
            "used": me["used"].as_u64().unwrap_or(used),
            "quota": me["quota"].as_u64().unwrap_or(0),
            "free_pc": free_space(&root).unwrap_or(0),
        }))
    }

    /// Delete these files (paths under Echo/ from echocloud_files) from Echo Cloud. Clips that are still on this PC
    /// stay there and aren't uploaded again by the background sync. Returns how many were deleted.
    pub fn echocloud_delete_files(&self, paths: &[String]) -> Result<u64, String> {
        self.ec_account()?;
        let cfg = self.settings();
        let paths: Vec<String> = paths.iter().map(|p| p.trim_start_matches('/').to_string())
            .filter(|p| !p.is_empty() && !p.split('/').any(|s| s.is_empty() || s == "..")).collect();
        let mut done: u64 = 0;
        for chunk in paths.chunks(500) {
            let keys: Vec<String> = chunk.iter().map(|p| format!("Echo/{p}")).collect();
            ec_call(&ec_api(&cfg), "POST", "/delete", &cfg.ec_token, Some(serde_json::json!({ "paths": keys })))
                .map_err(|e| if done > 0 { format!("{e} (after {done} were deleted)") } else { e })?;
            done += chunk.len() as u64;
            let root = self.clips_dir();
            let mut st = self.cloud_state();
            for p in chunk {
                let rel = strip_date(p);
                if p.split('/').any(|s| s.starts_with('.')) { continue; }
                // the share link of that clip ended with it (unless it plays a share copy)
                if !self.links().get(&rel).map_or(false, |e| e.key.contains("/.shared/")) { self.forget_link(&rel); }
                let only_ec = cfg.cloud_accounts.len() == 1;
                if only_ec { st.files.retain(|f| f.rel != rel); }
                st.keep_remote.remove(&rel);
                if root.join(&rel).exists() { st.no_upload.insert(rel); }
            }
            self.save_cloud_state(&st);
        }
        if let Ok(mut g) = EC.lock() { if let Some(s) = g.as_mut() { s.exp = 0; } } // fresh keys/usage next time (may not be full anymore)
        log_line(&format!("echo cloud: space manager deleted {done} files"));
        Ok(done)
    }

    /// Settings → About → "Send feedback": the message (+ the end of Echo's log if the user ticked it) goes to
    /// support@echoclip.link through the Echo Cloud server (no account needed; signed in = the server adds the email).
    pub fn send_feedback(&self, message: &str, contact: &str, with_log: bool, version: &str) -> Result<(), String> {
        let message = message.trim();
        if message.is_empty() { return Err("Write a few words first".into()); }
        let cfg = self.settings();
        let mut body = serde_json::json!({
            "message": message.chars().take(4000).collect::<String>(),
            "contact": contact.trim(),
            "version": version,
            "os": if cfg!(windows) { "Windows" } else { "Linux" },
        });
        if with_log {
            let t = fs::read_to_string(log_path()).unwrap_or_default();
            let lines: Vec<&str> = t.lines().collect();
            let mut tail = lines[lines.len().saturating_sub(400)..].join("\n");
            // at most ~150 KB, the newest part
            if tail.len() > 150_000 {
                let mut cut = tail.len() - 150_000;
                while !tail.is_char_boundary(cut) { cut += 1; }
                tail = tail[cut..].to_string();
            }
            body["log"] = serde_json::json!(tail);
        }
        ec_call(&ec_api(&cfg), "POST", "/feedback", &cfg.ec_token, Some(body))?;
        log_line("feedback: sent");
        Ok(())
    }

    /// "Stop sharing": the clip's Echo Cloud link stops working (the clip stays).
    pub fn cloud_unshare(&self, rel: &str) -> Result<(), String> {
        let cfg = self.settings();
        if cfg.ec_token.is_empty() { return Err("Sign in to Echo Cloud first".into()); }
        let mut paths = vec![format!("Echo/{}", self.cpath(rel).trim_start_matches('/'))];
        if let Some(n) = Path::new(rel).file_name() { paths.push(format!("Echo/.shared/{}", n.to_string_lossy())); }
        // the exact object the link plays (a share copy keeps its old name when the clip is renamed later)
        if let Some(e) = self.links().get(rel) { if !e.key.is_empty() && !paths.contains(&e.key) { paths.push(e.key.clone()); } }
        let mut removed = 0;
        for p in paths {
            let j = ec_call(&ec_api(&cfg), "POST", "/share/remove", &cfg.ec_token, Some(serde_json::json!({ "path": p })))?;
            removed += j["removed"].as_u64().unwrap_or(0);
        }
        self.forget_link(rel);
        log_line(&format!("cloud: stopped sharing {rel} ({removed} link(s))"));
        if removed == 0 { return Err("This clip had no share link".into()); }
        Ok(())
    }

    /// Delete the Echo Cloud account and everything in it (the server needs the email typed as confirmation).
    pub fn echocloud_delete_account(&self, confirm: &str) -> Result<(), String> {
        let cfg = self.settings();
        if cfg.ec_token.is_empty() { return Err("Not signed in to Echo Cloud".into()); }
        for _ in 0..200 {
            let j = ec_call(&ec_api(&cfg), "POST", "/account/delete", &cfg.ec_token, Some(serde_json::json!({ "confirm": confirm })))?;
            if j["done"].as_bool().unwrap_or(false) {
                log_line("echo cloud: account deleted");
                if let Some(a) = cfg.cloud_accounts.iter().find(|a| a.provider == "echocloud") {
                    let _ = self.cloud_remove_account(a.id);
                }
                return Ok(());
            }
        }
        Err("Deleting is taking long - try again, it continues where it stopped".into())
    }

    /// A local clip was deleted from this PC only: don't download it again from the cloud.
    pub fn cloud_forget_local(&self, path: &str) {
        if let Some(rel) = self.rel_of(path) {
            let mut st = self.cloud_state();
            if st.files.iter().any(|f| f.rel == rel) {
                st.keep_remote.insert(rel);
                self.save_cloud_state(&st);
            }
        }
    }

    /// Keep the cloud copy in step when a clip is renamed / moved on this PC.
    pub fn cloud_follow_move(&self, old_rel: String, new_rel: String) {
        if old_rel == new_rel { return; }
        // the clip's share link keeps working after a move / rename (the server moves the clip's link with it, a share
        // copy doesn't move at all): carry the link over instead of forgetting it - forgetting it left links that still
        // played but that Echo no longer showed or could stop (chat 27)
        let old_p = self.cpath(&old_rel);
        let new_p = match old_p.strip_suffix(old_rel.as_str()) { Some(pre) => format!("{pre}{new_rel}"), None => new_rel.clone() };
        {
            let mut m = self.links();
            if let Some(mut e) = m.remove(&old_rel) {
                if e.key == format!("Echo/{}", old_p.trim_start_matches('/')) { e.key = format!("Echo/{}", new_p.trim_start_matches('/')); }
                m.insert(new_rel.clone(), e);
                self.save_links(&m);
            }
        }
        if !self.cloud_on() || !self.cloud_state().files.iter().any(|f| f.rel == old_rel) {
            return;
        }
        let mut st = self.cloud_state();
        // keep the date folder: only the part after it changes (old_p / new_p above)
        for f in st.files.iter_mut().filter(|f| f.rel == old_rel) {
            f.rel = new_rel.clone();
            f.path = new_p.clone();
        }
        self.save_cloud_state(&st);
        let cfg = self.settings();
        let has_ec = cfg.cloud_accounts.iter().any(|a| a.provider == "echocloud") && !cfg.ec_token.is_empty();
        let only_ec = has_ec && cfg.cloud_accounts.len() == 1;
        std::thread::spawn(move || {
            // Echo Cloud's copy moves on the server (its keys may be read-only); other clouds through rclone -
            // in the combined cloud Echo Cloud is read-only, so rclone only touches the other accounts
            if has_ec {
                let b = serde_json::json!({ "from": format!("Echo/{}", old_p.trim_start_matches('/')), "to": format!("Echo/{}", new_p.trim_start_matches('/')) });
                match ec_call(&ec_api(&cfg), "POST", "/move", &cfg.ec_token, Some(b)) {
                    Ok(_) => if only_ec { return },
                    Err(e) if e.contains("not found") && only_ec => {}   // older server: keys
                    Err(e) => { if only_ec { log_line(&format!("echo cloud: move failed: {e}")); return; } }   // multi: maybe not in Echo Cloud
                }
            }
            if rclone_out(&["moveto", "--retries", "1", &remote(&old_p), &remote(&new_p)]).is_err() {
                // the clip wasn't in that cloud (e.g. only in Echo Cloud, which moved on the server above): rclone had
                // already made the new folder - remove it again if it's empty, so no empty "Mixed" folders are left
                // in Google Drive (chat 27). rmdir never removes a folder that has files.
                if let Some((dir, _)) = new_p.rsplit_once('/') {
                    if rclone_out(&["rmdir", &remote(dir)]).is_ok() { log_line(&format!("cloud: removed the empty folder {dir} again")); }
                }
            }
        });
    }

    fn stop_serve(&self) {
        if let Some((mut c, _)) = self.cloud_serve.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// The streaming server (rclone, ~40-80 MB RAM) wasn't asked for in `secs`: stop it. The window asks
    /// again (cloud_stream_base) before every cloud video and every couple of minutes while one plays,
    /// and a restart reuses the same port + address.
    pub fn stop_idle_serve(&self, secs: u64) {
        let running = self.cloud_serve.lock().map(|g| g.is_some()).unwrap_or(false);
        if running && now_unix().saturating_sub(SERVE_USED.load(Ordering::Relaxed)) > secs {
            self.stop_serve();
            log_line(&format!("cloud: streaming server stopped (not used for {} min)", secs / 60));
        }
    }

    /// Local streaming address for cloud clips (starts "rclone serve http" once).
    pub fn cloud_stream_base(&self) -> Result<String, String> {
        SERVE_USED.store(now_unix(), Ordering::Relaxed);
        ec_ensure();
        if SERVE_STALE.swap(false, Ordering::Relaxed) {
            log_line("cloud: restarting the streaming server (new uploads since it listed the folders)");
            self.stop_serve();
        }
        if EC_NEW_KEYS.swap(false, Ordering::Relaxed) {
            self.stop_serve(); // restart with the new Echo Cloud keys (same port: the player keeps its address)
        }
        let mut g = self.cloud_serve.lock().unwrap();
        if let Some((c, port)) = g.as_mut() {
            if matches!(c.try_wait(), Ok(None)) {
                return Ok(format!("http://127.0.0.1:{port}/{}", serve_token()));
            }
        }
        let pref = SERVE_PORT.load(Ordering::Relaxed);
        for port in std::iter::once(pref).filter(|p| *p != 0).chain(53980u16..53990) {
            let mut c = rclone_cmd()?
                .args(["serve", "http", CLOUD_ROOT, "--read-only", "--addr", &format!("127.0.0.1:{port}"), "--vfs-cache-mode", "minimal",
                       "--baseurl", &format!("/{}", serve_token())])
                .stdout(Stdio::null()).stderr(Stdio::null()).spawn().map_err(err)?;
            std::thread::sleep(Duration::from_millis(900));
            if matches!(c.try_wait(), Ok(None)) {
                log_line(&format!("cloud: streaming server on port {port}"));
                SERVE_PORT.store(port, Ordering::Relaxed);
                *g = Some((c, port));
                return Ok(format!("http://127.0.0.1:{port}/{}", serve_token()));
            }
        }
        Err("could not start the streaming server".into())
    }

    /// Thumbnail + length of a cloud-only clip (read over the streaming server, cached like local ones).
    pub fn cloud_thumb(&self, rel: &str) -> Result<Thumb, String> {
        let stem = Path::new(rel).file_stem().ok_or("bad path")?.to_string_lossy().to_string();
        let img = self.thumbs_dir().join(format!("{stem}.jpg"));
        let dur_file = self.thumbs_dir().join(format!("{stem}.dur"));
        if img.exists() && dur_file.exists() {
            return Ok(Thumb { thumb: img.to_string_lossy().into(), duration: fs::read_to_string(&dur_file).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0.0) });
        }
        let url = format!("{}/{}", self.cloud_stream_base()?, self.cpath(rel).split('/').map(url_part).collect::<Vec<_>>().join("/"));
        fs::create_dir_all(self.thumbs_dir()).map_err(err)?;
        let at = self.thumb_time(&stem).map_or("0.5".to_string(), |t| format!("{t:.3}"));
        let _ = ffmpeg().args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &at, "-i", &url, "-frames:v", "1", "-vf", "scale=480:-2", "-q:v", "4"])
            .arg(&img).stdout(Stdio::null()).stderr(Stdio::null()).status();
        let o = hidden("ffprobe").args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1", &url]).output().map_err(err)?;
        let d: f64 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(0.0);
        if d > 0.0 {
            let _ = fs::write(&dur_file, d.to_string());
        }
        Ok(Thumb { thumb: img.to_string_lossy().into(), duration: d })
    }

    /// What an Echo Cloud share page / Discord embed shows: the clip's name, video size, length and a
    /// thumbnail (1280 px from the clip on this PC, else the cached library thumbnail). All best-effort.
    /// What a share link plays: H.264 (plays on every phone and browser) with the Echo watermark when it's on
    /// (Settings → watermark). Cached in .thumbs/shared/. A clip without watermark and not AV1 is used as it is.
    fn share_copy(&self, src: &Path, progress: &dyn Fn(f64)) -> Result<PathBuf, String> {
        let cfg = self.settings();
        // no burned-in watermark any more (the share page shows it): a copy is only made for AV1 clips
        let _ = &cfg;
        let style = "off".to_string();
        let s = src.to_string_lossy().to_string();
        let av1 = is_av1(&s);
        if style == "off" && !av1 {
            return Ok(src.to_path_buf());
        }
        let dir = self.thumbs_dir().join("shared");
        fs::create_dir_all(&dir).map_err(err)?;
        let stem = src.file_stem().ok_or("bad clip path")?.to_string_lossy().to_string();
        let out = dir.join(format!("{stem}.{style}.mp4"));
        let sm = fs::metadata(src).map_err(err)?;
        if let Ok(m) = fs::metadata(&out) {
            if m.len() > 0 && m.modified().ok() >= sm.modified().ok() {
                return Ok(out);
            }
        }
        let dur = self.thumb(&s).map(|t| t.duration).unwrap_or(0.0).max(1.0);
        let (_w, h, audio) = self.probe(src);
        let tracks = self.audio_tracks(&s).max(1) as f64;
        // like the clip (AV1 needs about twice the bitrate in H.264), but capped for sharing: plenty for Discord and
        // phones, and a much smaller upload; only the first (mixed) sound track
        let cap = if h <= 720 { 5000.0 } else if h <= 1080 { 8000.0 } else if h <= 1440 { 12000.0 } else { 20000.0 };
        let kbps = ((sm.len() as f64 * 8.0 / dur / 1000.0 - 192.0 * tracks) * if av1 { 2.0 } else { 1.0 }).clamp(2500.0, cap) as u64;
        let b = format!("{kbps}k");
        let tmp = dir.join(format!("{stem}.sharing.tmp"));
        let graph = match if style == "off" { None } else { self.watermark_graph("v0", "out", h) } {
            Some(w) => format!("[in]null[v0];{w}"),
            None => TV_YUV.to_string(),
        };
        let t0 = Instant::now();
        let run = |venc: &[&str]| -> bool {
            if skip_enc(venc) { return false; }
            let mut c = ffmpeg();
            c.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(src).args(["-map", "0:v:0"]);
            if audio { c.args(["-map", "0:a:0", "-c:a", "copy"]); }
            c.args(["-vf", &graph]).args(venc).args(["-b:v", &b, "-maxrate", &b]).args(TV_TAGS)
                .args(["-movflags", "+faststart", "-progress", "pipe:1", "-nostats", "-f", "mp4"]).arg(&tmp)
                .stdout(Stdio::piped()).stderr(Stdio::null());
            let Ok(mut child) = c.spawn() else { return false };
            if let Some(out) = child.stdout.take() {
                use std::io::BufRead;
                for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                    if let Some(v) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok()) {
                        progress((v / 1e6 / dur).clamp(0.0, 1.0));
                    }
                }
            }
            child.wait().map_or(false, |s| s.success()) && fs::metadata(&tmp).map_or(false, |m| m.len() > 0)
        };
        let ok = run(&["-c:v", "h264_amf", "-quality", "speed"])
            || run(&["-c:v", "h264_nvenc", "-preset", "p3"])
            || run(&["-c:v", "h264_qsv", "-preset", "veryfast"])
            || run(&["-c:v", "libx264", "-preset", "superfast", "-pix_fmt", "yuv420p"]);
        if !ok {
            let _ = fs::remove_file(&tmp);
            return Err("Couldn't make the share copy - see the log".into());
        }
        let _ = fs::remove_file(&out);
        fs::rename(&tmp, &out).map_err(err)?;
        log_line(&format!("share: copy of {stem} ({}{}, {kbps} kbps, {:.0} MB) in {:.0}s", if style == "off" { "no watermark" } else { "watermark" },
            if av1 { ", from AV1" } else { "" }, fs::metadata(&out).map(|m| m.len()).unwrap_or(0) as f64 / 1e6, t0.elapsed().as_secs_f32()));
        Ok(out)
    }

    /// Put a file into Echo Cloud at `key` ("Echo/…"): the server's size-checked upload link, or the keys on an older server.
    fn ec_put(&self, file: &Path, key: &str, progress: &dyn Fn(f64)) -> Result<(), String> {
        let cfg = self.settings();
        let size = fs::metadata(file).map_err(err)?.len();
        let t0 = Instant::now();
        match ec_call(&ec_api(&cfg), "POST", "/upload", &cfg.ec_token, Some(serde_json::json!({ "path": key, "size": size }))) {
            Ok(j) => {
                // the upload runs on its own thread; this one passes its progress on
                let sent = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                let (s2, url, f2) = (sent.clone(), j["url"].as_str().unwrap_or("").to_string(), file.to_path_buf());
                let h = std::thread::spawn(move || crate::net::put_file(&url, &f2, move |b, _| s2.store(b, Ordering::Relaxed), || false));
                while !h.is_finished() {
                    progress(sent.load(Ordering::Relaxed) as f64 / size.max(1) as f64);
                    std::thread::sleep(Duration::from_millis(250));
                }
                h.join().unwrap_or_else(|_| Err("upload crashed".into())).map_err(|e| format!("Upload to Echo Cloud failed: {e}"))?;
                log_line(&format!("echo cloud: put {key} ({:.0} MB, size-checked link) in {:.0}s", size as f64 / 1e6, t0.elapsed().as_secs_f32()));
            }
            Err(e) if e.contains("not found") => {
                ec_ensure();
                let acc = cfg.cloud_accounts.iter().find(|a| a.provider == "echocloud").ok_or("Echo Cloud isn't connected")?;
                rclone_out(&["copyto", &file.to_string_lossy(), &acc_path(acc, key)])?;
            }
            Err(e) => return Err(e),
        }
        ec_add_used(size);
        Ok(())
    }

    /// Share link through a copy in Echo Cloud (Echo/.shared/<name>.mp4): H.264 + watermark, also for clips that
    /// are only on another cloud (Google Drive links don't play in Discord). The clip itself isn't touched.
    fn link_via_copy(&self, rel: &str, days: u32, progress: &dyn Fn(f64), try_old: bool) -> Result<(String, String, u64, serde_json::Value), String> {
        let cfg = self.settings();
        let local = self.clips_dir().join(rel);
        let name = Path::new(rel).file_name().ok_or("bad clip path")?.to_string_lossy().to_string();
        let key = format!("Echo/.shared/{name}");
        // shared before (by an older Echo, or links.json was lost): the copy is still in Echo Cloud -
        // link it again instead of fetching, converting and uploading the whole clip once more
        if try_old {
            let mut req = serde_json::json!({ "path": key, "days": days });
            self.share_preview(rel, &mut req);
            if let Ok(j) = ec_call(&ec_api(&cfg), "POST", "/share", &cfg.ec_token, Some(req.clone())) {
                if let Some(url) = j["url"].as_str().filter(|u| u.starts_with("http")) {
                    log_line(&format!("cloud: echo cloud link for {rel} (share copy from before)"));
                    return Ok((url.to_string(), key, j["expires"].as_u64().unwrap_or(0), req));
                }
            }
        }
        let mut fetched = None;
        let src = if local.exists() { local } else {
            // only in a cloud: fetch it next to the share copies, just for this
            let tmp = self.thumbs_dir().join("shared").join(format!("dl_{name}"));
            fs::create_dir_all(tmp.parent().unwrap()).map_err(err)?;
            rclone_out(&["copyto", &remote(&self.cpath(rel)), &tmp.to_string_lossy()]).map_err(|e| format!("Couldn't fetch the clip from the cloud: {e}"))?;
            fetched = Some(tmp.clone());
            tmp
        };
        progress(0.0);
        let copy = self.share_copy(&src, &|f| progress(f * 0.45));
        let copy = match copy { Ok(c) => c, Err(e) => { if let Some(f) = &fetched { let _ = fs::remove_file(f); } return Err(e); } };
        let up = self.ec_put(&copy, &key, &|f| progress(0.45 + f * 0.55));
        if let Some(f) = &fetched { let _ = fs::remove_file(f); }
        if fetched.is_some() && copy != src { let _ = fs::remove_file(&copy); }
        up?;
        let mut req = serde_json::json!({ "path": key, "days": days });
        self.share_preview(rel, &mut req);
        let j = ec_call(&ec_api(&cfg), "POST", "/share", &cfg.ec_token, Some(req.clone()))?;
        let url = j["url"].as_str().unwrap_or("").to_string();
        if !url.starts_with("http") { return Err("Echo Cloud didn't return a link".into()); }
        log_line(&format!("cloud: echo cloud link for {rel} (share copy)"));
        Ok((url, key, j["expires"].as_u64().unwrap_or(0), req))
    }

    fn share_preview(&self, rel: &str, req: &mut serde_json::Value) {
        let stem = Path::new(rel).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        req["title"] = serde_json::json!(stem.replacen(" (Echo)", "", 1));   // the share page has its own Echo branding
        let wm = share_wm(&self.settings()).to_string();
        req["wm"] = serde_json::json!(wm);
        let local = self.clips_dir().join(rel);
        let tdir = self.thumbs_dir();
        if let Some(d) = fs::read_to_string(tdir.join(format!("{stem}.dur"))).ok().and_then(|s| s.trim().parse::<f64>().ok()) {
            req["duration"] = serde_json::json!(d);
        }
        let mut jpg: Option<Vec<u8>> = None;
        if local.exists() {
            let mut size = None;
            if let Ok(o) = hidden("ffprobe").args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0:s=x"])
                .arg(&local).output() {
                let t = String::from_utf8_lossy(&o.stdout);
                if let Some((w, h)) = t.trim().lines().next().and_then(|l| l.split_once('x')) {
                    if let (Ok(w), Ok(h)) = (w.trim().parse::<u32>(), h.trim().parse::<u32>()) { req["width"] = w.into(); req["height"] = h.into(); size = Some((w, h)); }
                }
            }
            let tmp = std::env::temp_dir().join(format!("echo_share_{}.jpg", std::process::id()));
            // the clip's own thumbnail moment ("Use as thumbnail" in the player), else 1 s in
            let share_at = self.thumb_time(&stem).map_or("1".to_string(), |t| format!("{t:.3}"));
            let still = |graph: Option<String>| -> bool {
                let mut c = ffmpeg();
                c.args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &share_at, "-i"]).arg(&local).args(["-frames:v", "1"]);
                match graph {
                    Some(g) => { c.args(["-filter_complex", &g]); }
                    None => { c.args(["-vf", "scale='min(1280,iw)':-2"]); }
                }
                c.args(["-q:v", "5"]).arg(&tmp).stdout(Stdio::null()).stderr(Stdio::null()).status().map_or(false, |s| s.success())
            };
            // the preview image (Discord shows it before the video plays) gets the logo in the same spot as the page;
            // reverse = the APNG's last frame (the finished logo) comes first
            let logo = Self::watermark_file(&wm).zip(size.filter(|s| s.0 > 0 && s.1 > 0)).map(|(p, (w, h))| {
                let tw = w.min(1280);
                let th = ((h as f64 * tw as f64 / w as f64 / 2.0).round() as u32 * 2).max(2);
                let lh = ((th as f64 * 0.075).round() as u32).max(12);
                let m = ((th as f64 * 0.025).round() as u32).max(4);
                format!("[0:v]scale={tw}:{th}[v];movie='{}':f=apng,reverse,format=rgba,scale=-1:{lh}[l];[v][l]overlay=W-w-{m}:H-h-{m}", ff_path(&p))
            });
            let ok = (logo.is_some() && still(logo)) || still(None);
            if ok { jpg = fs::read(&tmp).ok(); }
            let _ = fs::remove_file(&tmp);
        }
        if jpg.as_ref().map_or(true, |b| b.len() > 280_000) {
            jpg = fs::read(tdir.join(format!("{stem}.jpg"))).ok();
        }
        if let Some(b) = jpg.filter(|b| b.len() <= 280_000) {
            req["thumb"] = serde_json::json!(base64(&b));
        }
    }

    /// A link to the cloud copy (pCloud / MEGA / Filen public link); copied to the clipboard by the caller.
    /// `days`: this link's lifetime (None = the default from Settings → Sharing, 0 = never expires).
    fn links(&self) -> std::collections::BTreeMap<String, LinkEntry> {
        fs::read_to_string(self.thumbs_dir().join("links.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    fn save_links(&self, m: &std::collections::BTreeMap<String, LinkEntry>) {
        let _ = fs::create_dir_all(self.thumbs_dir());
        let _ = write_patient(&self.thumbs_dir().join("links.json"), &serde_json::to_string(m).unwrap_or_default());
    }
    /// The clip's link must not be handed out again (stopped sharing, deleted, moved, signed out…).
    pub fn forget_link(&self, rel: &str) {
        let mut m = self.links();
        if m.remove(rel).is_some() { self.save_links(&m); }
    }
    /// Size + change time of the clip (or its cloud copy): a link made for an older version isn't reused.
    fn link_sig(&self, rel: &str) -> String {
        let local = self.clips_dir().join(rel);
        if let Ok(md) = fs::metadata(&local) {
            let t = md.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            return format!("{}:{t}", md.len());
        }
        self.cloud_state().files.iter().find(|f| f.rel == rel).map(|f| format!("c{}", f.size)).unwrap_or_default()
    }
    fn remember_link(&self, rel: &str, url: &str, key: &str, days: u32, expires: u64, req: &serde_json::Value) {
        let mut meta = req.clone();
        if let Some(o) = meta.as_object_mut() { o.remove("thumb"); o.remove("path"); o.remove("days"); }
        let e = LinkEntry { url: url.into(), key: key.into(), days, expires, wm: share_wm(&self.settings()).to_string(), sig: self.link_sig(rel), meta };
        let mut m = self.links();
        // the share-link file names expire anyway; keep the list small
        let now = now_unix();
        m.retain(|_, v| v.expires == 0 || v.expires > now);
        m.insert(rel.to_string(), e);
        self.save_links(&m);
    }
    /// The link Echo made before, when it still works (same clip, same watermark, not expired or removed).
    /// Only the lifetime is different: Echo Cloud changes it on the same link.
    fn reuse_link(&self, rel: &str, days: u32) -> Result<String, bool> {
        let mut m = self.links();
        let Some(e) = m.get(rel).cloned() else { return Err(false) };
        let cfg = self.settings();
        let ok = (e.expires == 0 || e.expires > now_unix() + 3600) && e.sig == self.link_sig(rel) && e.wm == share_wm(&cfg)
            && (e.key.is_empty() || !cfg.ec_token.is_empty());
        // Echo Cloud links: ask the server whether the link still plays (taken down / removed elsewhere -> make a new one).
        // No internet: hand out the known link anyway.
        let alive = ok && (e.key.is_empty() || match crate::net::request("HEAD", &format!("{}.mp4", e.url), vec![], None, 8) {
            Ok((code, _)) => code < 400,
            Err(_) => true,
        });
        if !alive {
            m.remove(rel);
            self.save_links(&m);
            return Err(true);
        }
        if e.days == days {
            log_line(&format!("cloud: same link as before for {rel}"));
            return Ok(e.url);
        }
        if e.key.is_empty() { return Err(false); }
        let mut req = e.meta.clone();
        if !req.is_object() { req = serde_json::json!({}); }
        req["path"] = serde_json::json!(e.key);
        req["days"] = serde_json::json!(days);
        match ec_call(&ec_api(&cfg), "POST", "/share", &cfg.ec_token, Some(req.clone())) {
            Ok(j) if j["url"].as_str() == Some(e.url.as_str()) => {
                self.remember_link(rel, &e.url, &e.key, days, j["expires"].as_u64().unwrap_or(0), &req);
                log_line(&format!("cloud: same link for {rel}, new lifetime {days} days"));
                Ok(e.url)
            }
            _ => { self.forget_link(rel); Err(true) }
        }
    }

    pub fn cloud_link(&self, rel: &str, days: Option<u32>, progress: &dyn Fn(f64)) -> Result<String, String> {
        let days = days.map(|d| d.min(3650)).unwrap_or_else(|| self.settings().link_days);
        // pressed "Copy share link" before: the same link again (in well under a second)
        let stale = match self.reuse_link(rel, days) {
            Ok(u) => { progress(1.0); return Ok(u); }
            Err(stale) => stale,
        };
        let (url, key, exp, req) = self.cloud_link_new(rel, days, progress, stale)?;
        self.remember_link(rel, &url, &key, days, exp, &req);
        Ok(url)
    }

    /// Make the link (Echo Cloud: the same link comes back for the same file; other clouds: a new one).
    /// Returns (link, Echo Cloud object or "", expires, what the share page shows).
    fn cloud_link_new(&self, rel: &str, days: u32, progress: &dyn Fn(f64), stale: bool) -> Result<(String, String, u64, serde_json::Value), String> {
        let exp = format!("{days}d");
        let mut args = vec!["link"];
        if days > 0 {
            args.extend(["--expire", exp.as_str()]);
        }
        // the union remote can't make links: ask the account that holds the file
        let cp = self.cpath(rel);
        let mut r = remote(&cp);
        let cfg = self.settings();
        // share copy (an AV1 clip): an H.264 copy goes to Echo Cloud, so the link plays everywhere
        let has_ec = cfg.cloud_accounts.iter().any(|a| a.provider == "echocloud") && !cfg.ec_token.is_empty();
        let local = self.clips_dir().join(rel);
        if has_ec && local.exists() && is_av1(&local.to_string_lossy()) {
            match self.link_via_copy(rel, days, progress, !stale) {
                Ok(r) => return Ok(r),
                Err(e) if e.contains("full") => log_line(&format!("cloud: no room for a share copy ({e}) - linking the clip itself")),
                Err(e) => return Err(e),
            }
        }
        // Echo Cloud first and without a listing: the server checks the clip itself and answers in ~1 s
        // (looking the file up in e.g. a big Google Drive first took up to a minute)
        if has_ec {
            let key = format!("Echo/{}", cp.trim_start_matches('/'));
            let mut req = serde_json::json!({ "path": key, "days": days });
            self.share_preview(rel, &mut req);
            match ec_call(&ec_api(&cfg), "POST", "/share", &cfg.ec_token, Some(req.clone())) {
                Ok(j) => {
                    let url = j["url"].as_str().unwrap_or("").to_string();
                    if url.starts_with("http") {
                        log_line(&format!("cloud: echo cloud link for {rel}"));
                        return Ok((url, key, j["expires"].as_u64().unwrap_or(0), req));
                    }
                }
                // only on another cloud (e.g. Google Drive): copy it to Echo Cloud, so the link plays in Discord
                Err(e) if e.contains("isn't in Echo Cloud") => match self.link_via_copy(rel, days, progress, !stale) {
                    Ok(r) => return Ok(r),
                    Err(e2) => log_line(&format!("cloud: couldn't copy {rel} to Echo Cloud for a link ({e2}) - using the other cloud's link")),
                },
                Err(e) => return Err(format!("Couldn't make a link: {e}")),
            }
        }
        for a in cfg.cloud_accounts.iter().filter(|a| a.provider != "echocloud") {
            let p = acc_path(a, &format!("Echo/{}", cp.trim_start_matches('/')));
            if rclone_out(&["lsjson", &p]).map_or(false, |j| j.contains("\"Path\"")) {
                r = p;
                break;
            }
        }
        args.push(r.as_str());
        let url = rclone_out(&args).map_err(|e| format!("Couldn't make a link: {e}"))?.trim().lines().last().unwrap_or("").to_string();
        if !url.starts_with("http") {
            return Err("This cloud didn't return a link".into());
        }
        log_line(&format!("cloud: link for {rel}"));
        let exp = if days > 0 { now_unix() + days as u64 * 86400 } else { 0 };
        Ok((url, String::new(), exp, serde_json::json!({})))
    }

    /// Background sync, run every few minutes: upload new clips / favourites, download clips from other PCs.
    /// Returns true when the library changed.
    /// Once a week: remove EMPTY folders under Echo/ in the user's own clouds (Google Drive…) - left by failed moves
    /// and deleted clips (chat 27). rclone rmdirs never removes a folder that has a file in it; Echo Cloud has no
    /// real folders (its stray share copies are cleaned up by the server).
    fn tidy_cloud_folders(&self, cfg: &crate::settings::Settings) {
        let stamp = self.thumbs_dir().join("cloud_tidy.txt");
        let last: u64 = fs::read_to_string(&stamp).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        if now_unix().saturating_sub(last) < 7 * 86400 { return; }
        let _ = fs::write(&stamp, now_unix().to_string());
        for a in cfg.cloud_accounts.iter().filter(|a| a.provider != "echocloud").cloned() {
            std::thread::spawn(move || {
                let t = Instant::now();
                match rclone_out(&["rmdirs", "--leave-root", &acc_path(&a, "Echo")]) {
                    Ok(_) => log_line(&format!("cloud: tidied empty folders in {} ({} s)", cloud_name(&a.provider), t.elapsed().as_secs())),
                    Err(e) => log_line(&format!("cloud: tidying {} skipped: {e}", cloud_name(&a.provider))),
                }
            });
        }
    }

    pub fn cloud_sync(&self) -> bool {
        if !self.cloud_on() {
            return false;
        }
        let Ok(_one) = self.cloud_lock.try_lock() else { return false };
        // Echo Cloud keys renewed: a running streaming server gets them too (restarted on the same port)
        ec_ensure();
        if EC_NEW_KEYS.swap(false, Ordering::Relaxed) && self.cloud_serve.lock().unwrap().is_some() {
            self.stop_serve();
            let _ = self.cloud_stream_base();
        }
        let cfg = self.settings();
        let mut changed = false;
        let Ok(files) = self.cloud_refresh() else { return false };
        self.tidy_cloud_folders(&cfg);
        let in_cloud: std::collections::HashSet<String> = files.iter().map(|f| f.rel.clone()).collect();
        // 1) uploads
        if cfg.cloud_upload != "off" {
            let favs = self.favorites();
            let no_up = self.cloud_state().no_upload;
            // "game": like "all", but only once the game is closed (no upload traffic while playing)
            let game_running = self.last_game.lock().unwrap().as_ref().map_or(false, |(_, t)| t.elapsed().as_secs() < 15);
            let every = cfg.cloud_upload == "all" || (cfg.cloud_upload == "game" && !game_running);
            let todo: Vec<ClipInfo> = self.list_clips().into_iter().filter(|c| {
                let rel = if c.folder.is_empty() { format!("{}.mp4", c.name) } else { format!("{}/{}.mp4", c.folder, c.name) };
                !in_cloud.contains(&rel) && (favs.contains(&c.name) || (every && c.modified >= cfg.cloud_since))
                    && !UP_SKIPPED.lock().unwrap().contains(&rel) // cancelled by hand: not again on its own
                    && !no_up.contains(&rel) // its cloud copy was deleted on purpose (cloud space manager)
                    && now_unix().saturating_sub(c.modified) > 20 // not still being written
            }).collect();
            for (i, c) in todo.iter().enumerate() {
                self.set_busy(&format!("Uploading {} of {}…", i + 1, todo.len()));
                { let mut p = UP_PROGRESS.lock().unwrap(); p.idx = i as u32 + 1; p.count = todo.len() as u32; }
                if self.cloud_upload(&c.path).is_err() {
                    let mut p = UP_PROGRESS.lock().unwrap(); p.idx = 0; p.count = 0;
                    break;
                }
                if cfg.cloud_free_after && self.cloud_remove_local(&c.path).is_ok() {
                    changed = true;
                }
            }
        }
        // 2) downloads from other PCs
        if cfg.cloud_download {
            let st = self.cloud_state();
            let root = self.clips_dir();
            let todo: Vec<&CloudFile> = files.iter().filter(|f| {
                !root.join(&f.rel).exists() && !st.keep_remote.contains(&f.rel) && f.modified >= cfg.cloud_since
            }).collect();
            for (i, f) in todo.iter().enumerate() {
                self.set_busy(&format!("Downloading {} of {} from the cloud…", i + 1, todo.len()));
                if self.cloud_download(&f.rel).is_ok() {
                    changed = true;
                } else {
                    break;
                }
            }
        }
        self.set_busy("");
        changed
    }
}

/// Percent-encode one URL path segment.
fn url_part(s: &str) -> String {
    s.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        _ => format!("%{b:02X}"),
    }).collect()
}

/// A folder of another clipping app that Echo can import from.
#[derive(Serialize, Clone)]
pub struct ImportSource {
    pub app: String,
    pub path: String,
    pub count: usize,
    pub bytes: u64,
}

const AUDIO_EXTS: [&str; 8] = ["wav", "mp3", "ogg", "flac", "m4a", "aac", "opus", "wma"];

fn is_audio_file(p: &Path) -> bool {
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    AUDIO_EXTS.contains(&ext.as_str())
}

const VIDEO_EXTS: [&str; 12] = ["mp4", "mov", "m4v", "mkv", "webm", "avi", "ts", "flv", "wmv", "mpg", "mpeg", "3gp"];

fn is_video_file(p: &Path) -> bool {
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    VIDEO_EXTS.contains(&ext.as_str())
}

/// Medal keeps clips in <capture folder>\Clips next to Editor / Thumbnails: start there when it exists.
fn import_root(p: &Path) -> PathBuf {
    let c = p.join("Clips");
    if c.is_dir() { c } else { p.to_path_buf() }
}

fn collect_videos(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let n = e.file_name().to_string_lossy().to_lowercase();
        if p.is_dir() {
            // "Shared Clips" (SteelSeries) are extra copies made for sharing - skip them
            if n.starts_with('.') || ["thumbnails", "thumbs", "editor", "cache", "temp", "tmp", "screenshots", "logs", "shared clips"].contains(&n.as_str()) {
                continue;
            }
            collect_videos(&p, depth + 1, out);
        } else if is_video_file(&p) && !n.contains(".tmp") && !n.contains(".part") {
            out.push(p);
        }
    }
}

/// Folder paths (like "D:\\Clips") written inside small settings files of another app.
fn paths_in_settings(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if depth > 3 || out.len() > 40 {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            paths_in_settings(&p, depth + 1, out);
            continue;
        }
        let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !["json", "ini", "cfg", "config", "xml"].contains(&ext.as_str()) || fs::metadata(&p).map_or(true, |m| m.len() > 2_000_000) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&p) else { continue };
        let b = text.as_bytes();
        for i in 1..b.len().saturating_sub(2) {
            if b[i] == b':' && b[i + 1] == b'\\' && b[i - 1].is_ascii_alphabetic() && (i < 2 || !b[i - 2].is_ascii_alphanumeric()) {
                let rest = &text[i - 1..];
                let end = rest.find(|c: char| c == '"' || c == '\'' || c == '\n' || c == '\r' || c == '<' || c == ',').unwrap_or(rest.len());
                let raw = rest[..end].replace("\\\\", "\\").replace('/', "\\");
                let q = PathBuf::from(raw.trim_end_matches('\\'));
                if q.is_dir() && !out.contains(&q) {
                    out.push(q);
                }
            }
        }
    }
}

/// PresentMon (Intel's frame-time tool) for the FPS test, downloaded once to %APPDATA%\\Clipper\\presentmon.
pub fn presentmon_exe() -> Result<PathBuf, String> {
    let dir = app_dir().join("presentmon");
    let exe = dir.join("PresentMon.exe");
    if exe.is_file() {
        return Ok(exe);
    }
    fs::create_dir_all(&dir).map_err(err)?;
    // pinned version + fingerprint (chat 28)
    let url = "https://github.com/GameTechDev/PresentMon/releases/download/v2.4.1/PresentMon-2.4.1-x64.exe";
    download_checked(url, &exe, "sha256:d74183e7ae630f72cd3690be0373ecbfdc6cbb86578148aab8fa2a7166068f34")?;
    log_line(&format!("fps: downloaded {url}"));
    Ok(exe)
}

/// FPS of one app in a PresentMon CSV between two times (seconds since the capture started).
/// Returns (app, average FPS, 1% low FPS, frames).
pub fn fps_stats(csv: &Path, from: f64, to: f64) -> Option<(String, f64, f64, usize)> {
    let text = fs::read_to_string(csv).ok()?;
    let mut lines = text.lines();
    let head: Vec<String> = lines.next()?.split(',').map(|h| h.trim().to_lowercase()).collect();
    let col = |names: &[&str]| head.iter().position(|h| names.contains(&h.as_str()));
    let (ca, cms) = (col(&["application"])?, col(&["msbetweenpresents"])?);
    let ct = col(&["timeinseconds", "cpustarttime"]);
    let skip = ["dwm.exe", "echo.exe", "msedgewebview2.exe", "presentmon.exe", "<error>", "explorer.exe"];
    let mut per: std::collections::HashMap<String, Vec<(f64, f64)>> = Default::default();
    let mut clock: std::collections::HashMap<String, f64> = Default::default();
    for l in lines {
        let f: Vec<&str> = l.split(',').collect();
        let (Some(app), Some(ms)) = (f.get(ca), f.get(cms).and_then(|x| x.trim().parse::<f64>().ok())) else { continue };
        let app = app.trim().to_string();
        if skip.contains(&app.to_lowercase().as_str()) || ms <= 0.0 {
            continue;
        }
        // CPUStartTime (PresentMon 2) is in ms, TimeInSeconds (1.x / --v1_metrics) in s; else add up frame times
        let t = match ct.and_then(|c| f.get(c)).and_then(|x| x.trim().parse::<f64>().ok()) {
            Some(v) if head[ct.unwrap()] == "cpustarttime" => v / 1000.0,
            Some(v) => v,
            None => {
                let c = clock.entry(app.clone()).or_insert(0.0);
                *c += ms / 1000.0;
                *c
            }
        };
        per.entry(app).or_default().push((t, ms));
    }
    // the game = the app with the most frames
    let (app, frames) = per.into_iter().max_by_key(|(_, v)| v.len())?;
    let mut ms: Vec<f64> = frames.iter().filter(|(t, _)| *t >= from && *t < to).map(|(_, m)| *m).collect();
    if ms.len() < 30 {
        return None;
    }
    let total: f64 = ms.iter().sum();
    let avg = ms.len() as f64 * 1000.0 / total;
    ms.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let worst = &ms[..(ms.len() / 100).max(1)];
    let low = 1000.0 / (worst.iter().sum::<f64>() / worst.len() as f64);
    Some((app, avg, low, ms.len()))
}


/// whisper sometimes loops on music / silence ("4-4-4-4-4…" for 40 s, chat 30): a run of the same 1-4 word pattern
/// repeated more than 3 times keeps only its first 2 repeats
fn drop_repeats(words: &mut Vec<Word>) {
    // the loop can also come as ONE "word" without spaces ("1-2-3-4-4-4-4-…", -sow keeps it together): drop any word
    // whose text has a 2-6 character piece repeated more than 3 times in a row
    words.retain(|w| !looped(&w.text));
    let norm = |w: &Word| w.text.trim().trim_matches(|c: char| c.is_ascii_punctuation()).to_lowercase();
    let mut i = 0;
    while i < words.len() {
        let mut cut = None;
        for p in 1..=4 {
            if i + p * 4 > words.len() { break; }
            let pat: Vec<String> = words[i..i + p].iter().map(norm).collect();
            if pat.iter().all(|s| s.is_empty()) { continue; }
            let mut reps = 1;
            while i + (reps + 1) * p <= words.len() && (0..p).all(|k| norm(&words[i + reps * p + k]) == pat[k]) { reps += 1; }
            if reps > 3 { cut = Some((p, reps)); break; }
        }
        if let Some((p, reps)) = cut {
            words.drain(i + 2 * p..i + reps * p);
            i += 2 * p;
        } else {
            i += 1;
        }
    }
}

fn looped(text: &str) -> bool {
    let c: Vec<char> = text.trim().chars().collect();
    // units of 2-6 ("4-", "ab-"): a single repeated character ("1000000", "!!!!") is kept
    for p in 2..=6 {
        if c.len() < p * 4 { break; }
        for i in 0..=c.len() - p * 4 {
            if (1..4).all(|r| c[i + r * p..i + r * p + p] == c[i..i + p]) && c[i..i + p].iter().any(|ch| ch.is_alphanumeric()) {
                // "loooool", "hahaha" stay: letters repeated as a unit of 1-2 are normal speech
                if p <= 2 && c[i..i + p].iter().all(|ch| ch.is_alphabetic()) { continue; }
                return true;
            }
        }
    }
    false
}
