//! Chat C: editor stickers (Echo's stickerMenu) - an emoji drawn to a PNG, or a picture / GIF from the PC, as a box of
//! kind "image" (src = the file) over the video. Preview: mpv draws each sticker as its own bitmap overlay
//! (`overlay-add`, ids 10..), decoded once by ffmpeg (GIFs at 20 frames a second, looping), scaled with tiny-skia.
//! Export: edrender adds them as looping ffmpeg inputs (Echo's render_edit).
use crate::editor::{BoxFx, Editor};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

/// Echo's emoji list (+ 2 so the grid is 4 full rows of 9)
pub const EMOJI: [&str; 36] = ["😂", "🤣", "😭", "😱", "😎", "🔥", "💀", "👀", "💯", "🎯", "⚡", "💥", "🏆", "👑", "❤", "👍", "👎", "🤡", "🥶", "🤯", "😤", "🙏", "✨", "🎉", "🚀", "⭐", "❗", "❓", "💢", "🫡", "🗿", "🤝", "😅", "🤔", "😈", "🎮"];
const FIRST_ID: usize = 10;
const MAX_IDS: usize = 40;
const GIF_FPS: f64 = 20.0;

/// Echo's sticker folder (%APPDATA%\Clipper\stickers) - edits made in either app find the files
pub fn stickers_dir() -> PathBuf {
    std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir()).join("Clipper").join("stickers")
}

// ---------- emoji -> picture (swash: the system's colour emoji font) ----------
thread_local! {
    static EMOJI_FONT: Option<Rc<Vec<u8>>> = emoji_font_file().and_then(|p| std::fs::read(p).ok()).map(Rc::new);
}

fn emoji_font_file() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ECHO_EMOJI_FONT") {
        return Some(PathBuf::from(p));
    }
    let win = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let mut c = vec![PathBuf::from(&win).join("Fonts").join("seguiemj.ttf")];
    for d in ["/usr/share/fonts/truetype/noto", "/usr/share/fonts/google-noto-color-emoji-fonts", "/usr/share/fonts/noto-emoji", "/usr/share/fonts/noto", "/usr/share/fonts/TTF"] {
        c.push(Path::new(d).join("NotoColorEmoji.ttf"));
    }
    c.into_iter().find(|p| p.exists())
}

/// one emoji as straight RGBA, `px` square (None: no colour emoji font / not in it)
pub fn render_emoji(e: &str, px: u32) -> Option<Vec<u8>> {
    use swash::scale::{image::Content, Render, ScaleContext, Source, StrikeWith};
    let data = EMOJI_FONT.with(|f| f.clone())?;
    let font = swash::FontRef::from_index(&data, 0)?;
    let ch = e.chars().next()?;
    let gid = font.charmap().map(ch);
    if gid == 0 {
        return None;
    }
    let mut ctx = ScaleContext::new();
    let size = px as f32 * 0.82;
    let mut sc = ctx.builder(font).size(size).hint(false).build();
    let img = Render::new(&[Source::ColorOutline(0), Source::ColorBitmap(StrikeWith::BestFit), Source::Outline]).render(&mut sc, gid)?;
    let (iw, ih) = (img.placement.width as usize, img.placement.height as usize);
    if iw == 0 || ih == 0 {
        return None;
    }
    let rgba: Vec<u8> = match img.content {
        Content::Color => img.data.clone(),
        _ => img.data.iter().flat_map(|a| [255, 255, 255, *a]).collect(),
    };
    // bitmap strikes come at their own size: fit into px x px, centred
    let k = (px as f64 / iw.max(ih) as f64).min(if matches!(img.source, Source::ColorBitmap(_)) { 8.0 } else { 1.0 });
    let (dw, dh) = (((iw as f64 * k).round() as usize).max(1), ((ih as f64 * k).round() as usize).max(1));
    let (ox, oy) = ((px as usize).saturating_sub(dw) / 2, (px as usize).saturating_sub(dh) / 2);
    let mut out = vec![0u8; (px * px * 4) as usize];
    for y in 0..dh.min(px as usize) {
        for x in 0..dw.min(px as usize) {
            let (sx, sy) = (((x as f64 / k) as usize).min(iw - 1), ((y as f64 / k) as usize).min(ih - 1));
            let s = (sy * iw + sx) * 4;
            let d = ((oy + y) * px as usize + ox + x) * 4;
            if d + 4 <= out.len() && s + 4 <= rgba.len() {
                out[d..d + 4].copy_from_slice(&rgba[s..s + 4]);
            }
        }
    }
    Some(out)
}

/// the emoji grid of the Sticker menu
pub fn emoji_images() -> Vec<slint::Image> {
    EMOJI.iter().map(|e| {
        let px = 64;
        let mut b = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(px, px);
        if let Some(rgba) = render_emoji(e, px) {
            b.make_mut_bytes().copy_from_slice(&rgba);
        }
        slint::Image::from_rgba8(b)
    }).collect()
}

/// emoji -> PNG in the sticker folder (Echo: emoji_<hash>.png)
pub fn save_emoji(e: &str) -> Result<PathBuf, String> {
    let rgba = render_emoji(e, 256).ok_or("No colour emoji font found on this PC")?;
    let png = png_encode(256, 256, &rgba);
    let mut h: u64 = 0xcbf29ce484222325;
    for b in &png {
        h = (h ^ *b as u64).wrapping_mul(0x100000001b3);
    }
    let dir = stickers_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let p = dir.join(format!("emoji_{h:016x}.png"));
    std::fs::write(&p, png).map_err(|e| e.to_string())?;
    Ok(p)
}

/// a picture from the PC is copied next to the emoji stickers so the edit keeps working (Echo's pick_image)
pub fn copy_picture(src: &Path) -> Result<PathBuf, String> {
    let dir = stickers_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = src.file_name().ok_or("bad file")?.to_string_lossy().to_string();
    let len = |p: &Path| std::fs::metadata(p).map(|m| m.len()).ok();
    let mut dst = dir.join(&name);
    let mut n = 2;
    while dst.exists() && len(&dst) != len(src) {
        dst = dir.join(format!("{n}_{name}"));
        n += 1;
    }
    std::fs::copy(src, &dst).map_err(|e| e.to_string())?;
    Ok(dst)
}

/// smallest valid PNG writer (stored deflate blocks) - no image crate features needed
fn png_encode(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for b in data {
            c ^= *b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend_from_slice(data);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc(&c).to_be_bytes());
    }
    let mut raw = Vec::with_capacity(((w * 4 + 1) * h) as usize);
    for y in 0..h as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * w as usize * 4..(y + 1) * w as usize * 4]);
    }
    let mut z = vec![0x78, 0x01];
    for (i, blk) in raw.chunks(65535).enumerate() {
        let last = (i + 1) * 65535 >= raw.len();
        z.push(last as u8);
        z.extend_from_slice(&(blk.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(blk.len() as u16)).to_le_bytes());
        z.extend_from_slice(blk);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for x in &raw {
        a = (a + *x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = w.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

pub fn is_gif(p: &str) -> bool {
    p.to_lowercase().ends_with(".gif")
}

/// (width, height) of a picture (first frame)
pub fn picture_size(ff: &Path, p: &Path) -> (u32, u32) {
    let (w, h, _, _) = crate::edrender::probe(ff, p);
    (w.max(1), h.max(1))
}

// ---------- preview: decoded pictures + mpv bitmap overlays ----------
struct Decoded {
    w: u32,
    h: u32,
    /// premultiplied RGBA frames (GIFs: GIF_FPS a second)
    frames: Vec<tiny_skia::Pixmap>,
}

thread_local! {
    /// file -> decoded (None = being decoded / failed)
    static CACHE: RefCell<HashMap<String, Option<Rc<Decoded>>>> = RefCell::new(HashMap::new());
    /// overlay id -> (what it shows, its pixels - must stay alive while mpv shows them)
    static SHOWN: RefCell<HashMap<usize, (String, Vec<u8>)>> = RefCell::new(HashMap::new());
}

fn decode(ff: PathBuf, src: String) -> Option<(u32, u32, Vec<Vec<u8>>)> {
    let (w0, h0) = picture_size(&ff, Path::new(&src));
    let k = (512.0 / w0.max(h0) as f64).min(1.0);
    let (w, h) = (((w0 as f64 * k).round() as u32).max(1), ((h0 as f64 * k).round() as u32).max(1));
    let mut c = crate::edrender::hidden_cmd(&ff);
    c.args(["-v", "error", "-i", &src, "-vf", &format!("{}scale={w}:{h}:flags=bicubic,format=rgba", if is_gif(&src) { format!("fps={GIF_FPS}:round=down,") } else { String::new() })]);
    c.args(["-frames:v", if is_gif(&src) { "200" } else { "1" }, "-f", "rawvideo", "-"]);
    let o = c.stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
    let fsz = (w * h * 4) as usize;
    let frames: Vec<Vec<u8>> = o.stdout.chunks_exact(fsz).map(|f| f.to_vec()).collect();
    (!frames.is_empty()).then_some((w, h, frames))
}

fn get(src: &str) -> Option<Rc<Decoded>> {
    if let Some(v) = CACHE.with(|c| c.borrow().get(src).cloned()) {
        return v;
    }
    CACHE.with(|c| c.borrow_mut().insert(src.to_string(), None));
    let ff = crate::sheets::ffmpeg()?;
    let s = src.to_string();
    std::thread::spawn(move || {
        let r = decode(ff, s.clone()).map(Arc::new);
        let _ = slint::invoke_from_event_loop(move || {
            let d = r.and_then(|r| {
                let frames: Vec<tiny_skia::Pixmap> = r.2.iter().filter_map(|f| {
                    let mut pm = tiny_skia::Pixmap::new(r.0, r.1)?;
                    for (d, s) in pm.data_mut().chunks_exact_mut(4).zip(f.chunks_exact(4)) {
                        let a = s[3] as u32;
                        d.copy_from_slice(&[(s[0] as u32 * a / 255) as u8, (s[1] as u32 * a / 255) as u8, (s[2] as u32 * a / 255) as u8, s[3]]);
                    }
                    Some(pm)
                }).collect();
                (!frames.is_empty()).then(|| Rc::new(Decoded { w: r.0, h: r.1, frames }))
            });
            CACHE.with(|c| c.borrow_mut().insert(s, d));
        });
    });
    None
}

fn src_of(b: &BoxFx) -> Option<&str> {
    (b.kind == "image").then(|| b.extra.get("src").and_then(|x| x.as_str())).flatten().filter(|x| !x.is_empty())
}

/// every frame the editor is open (render hook): stickers showing at the playhead are handed to mpv
pub fn overlay(e: &Editor, m: &crate::mpv::Mpv) {
    let g = e.geo;
    let mut want: HashMap<usize, (String, i32, i32, u32, u32, Rc<Decoded>, usize, f64)> = HashMap::new();
    let mut n = 0;
    for b in e.p.boxes.iter() {
        let Some(src) = src_of(b) else { continue };
        if e.t < b.start || e.t > b.end || n >= MAX_IDS {
            continue;
        }
        let Some(d) = get(src) else { continue };
        let (x, y, w, h) = e.box_rect(b);
        let (w, h) = (w.round().max(2.0) as u32, h.round().max(2.0) as u32);
        // keep it inside mpv's OSD
        if w > 4096 || h > 4096 || g.ow < 2.0 {
            continue;
        }
        let fi = if d.frames.len() > 1 { (((e.t - b.start) * GIF_FPS) as usize) % d.frames.len() } else { 0 };
        let op = b.opacity.clamp(0.0, 1.0);
        let key = format!("{src}|{fi}|{:.0},{:.0},{w}x{h}|{op:.2}", x, y);
        want.insert(FIRST_ID + n, (key, x.round() as i32, y.round() as i32, w, h, d, fi, op));
        n += 1;
    }
    SHOWN.with(|s| {
        let mut s = s.borrow_mut();
        let gone: Vec<usize> = s.keys().filter(|k| !want.contains_key(k)).copied().collect();
        for id in gone {
            m.command(&["overlay-remove", &id.to_string()]);
            s.remove(&id);
        }
        for (id, (key, x, y, w, h, d, fi, op)) in want {
            if s.get(&id).is_some_and(|v| v.0 == key) {
                continue;
            }
            // mpv refuses a bitmap that starts outside the OSD: crop the part that sticks out on the left / top
            let (cx, cy) = ((-x).max(0) as u32, (-y).max(0) as u32);
            if cx >= w || cy >= h {
                continue;
            }
            let (vw, vh) = (w - cx, h - cy);
            let Some(mut pm) = tiny_skia::Pixmap::new(vw, vh) else { continue };
            let tr = tiny_skia::Transform::from_scale(w as f32 / d.w as f32, h as f32 / d.h as f32).post_translate(-(cx as f32), -(cy as f32));
            let paint = tiny_skia::PixmapPaint { opacity: op as f32, quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() };
            pm.draw_pixmap(0, 0, d.frames[fi].as_ref(), &paint, tr, None);
            // tiny-skia is RGBA, mpv wants BGRA (both premultiplied)
            let mut buf = pm.take();
            for p in buf.chunks_exact_mut(4) {
                p.swap(0, 2);
            }
            let addr = buf.as_ptr() as usize;
            m.command(&["overlay-add", &id.to_string(), &x.max(0).to_string(), &y.max(0).to_string(), &format!("&{addr}"), "0", "bgra", &vw.to_string(), &vh.to_string(), &(vw * 4).to_string()]);
            // the old pixels may only go once mpv has the new ones
            s.insert(id, (key, buf));
        }
    });
}

/// editor closed: sticker bitmaps off the video
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
}
