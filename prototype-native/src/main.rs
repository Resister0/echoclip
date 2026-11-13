#![recursion_limit = "512"]
// Echo native UI prototype (standalone - doesn't touch Echo's settings; writes only favorites.json + meta.json,
// the same way Echo does). Run: cargo run --release [clips folder]
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

slint::include_modules!();

mod icons;
mod import;
mod edrender;
mod editor;
mod ed_bench; // 11 Nov 2026: test hook ECHO_TL_BENCH - timeline scrolling, measured
mod edui;
mod library;
mod lib_cards;
mod lib_del; // gaps-library row 42: Echo's delete dialogs + progress
mod lib_prefs; // gaps-library rows 20 + 44: remembered sort / view / filters, "Longest" probes lengths
mod lib_thumbs; // 10 Nov: card pictures that were never made (black card)
mod lib_merge; // gaps-library row 29: folder right-click "Merge into …"
mod drop; // gaps-library row 1: drag and drop files onto the window (import / editor)
#[cfg(target_os = "linux")]
mod wl_dnd; // drag and drop on Linux Wayland (winit 0.30 has none there): feeds drop.rs
mod lib_shots;
mod lib_import;
mod tidy;
mod cleanup; // session cleanup: more Tidy up / Echo Cloud space categories
mod lib_pick;
mod lib_sound;
mod lib_net;
mod lib_set;
mod cloud;
mod cloud_live; // gaps-cloud: cloud player overlay, background upload progress, space warnings
mod setup;
mod lib_color;
mod lib_fx; // Chat B (night): Settings > Appearance > Background effect
mod lib_profile; // Chat B (night): Settings > Echo Profile editing
mod mpv;
mod osd;
mod pl_media;
mod pl_layer; // Chat C/A: editor video layers (PiP) in the preview
mod pl_sticker;
mod pl_actions;
mod pl_cuts; // 9 Nov: cut out parts on the player's strip (Shift+drag, X)
mod pl_seek; // 10 Nov: paced, non-blocking seeks while dragging on the strip / the trim ends
mod perf; // 9 Nov: memory / CPU averages for the admin panel (sent with the update check)
mod gpumem; // 12 Nov 2026: Echo's graphics-card memory for those numbers (Windows)
mod game_sync; // 12 Nov 2026: cloud-games - game folders + icons with clips in Echo Cloud show on every PC of the account
mod clip_thumbs; // 12 Nov 2026: cloud-only clips show their real card picture (kept on the Echo Cloud account)
mod specs; // r2-perf (10 Nov): the PC's hardware model names + recording settings, sent with those numbers
mod hover_vid; // 9 Nov: card hover preview = the real clip (was a flipbook)
mod discord; // 9 Nov: Echo's Discord status ("Playing Echo") - port of src-tauri/src/discord.rs
mod idle; // 9 Nov: efficiency mode for the UI side while hidden in the tray
mod memtrim; // 11 Nov 2026: trim memory again when it grows while hidden (recorder restarts touch driver set-up memory)
mod sheet_tiles; // 11 Nov 2026: the player's seek pictures as 60 small images (the whole sheet per picture = ~400 MB VRAM)
mod focus; // port leftovers L2: keys go back after a rename / overlay / dialog closes
mod home_inbox; // 11 Nov 2026: Home "Inbox" - the clip on show (new ones first), pager, details, Your games
mod hdr_home; // gaps (10 Nov): header chips (KDE keys), "On this day" + milestones, Record button, error toasts -> log

thread_local! {
    /// Echo's window is the one in front (winit Focused) - hover previews only then
    static WIN_FOCUSED: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}
mod copy;
mod dl_smooth; // 10 Nov: cloud download bars move evenly (rclone hands bytes over in 1 MiB lumps)
mod bgwork; // r2-threads: PORT_LEFTOVERS L3-L55, blocking work moved off the UI thread
// Chat A: Echo's real recorder (echo-core), in-game popup, tray
mod rec_core;
mod showkey; // 2.0.2: Show / hide Echo hotkey (tray <-> in front, over a game)
mod ownkeys; // 13 Nov 2026: hotkeys pressed inside Echo's own window (the keyboard hook misses those)
mod titlebar;
mod pl_zoom;
mod popup;
mod uiscale; // 11 Nov 2026: 1080p / 1440p look the same (Echo's own size on top of the system's scale)
mod tray;
#[cfg(target_os = "linux")]
mod linux; // gaps-linux: ffmpeg lookup, Trash fallback, DnD tip, Flatpak first start, menu entry, pickers
mod settings;
mod sheets;
mod undo; // 2.0.2: Undo on the notes (deleted clips back from the Trash, moved clips back)
mod notes; // 2.0.2: the notes at the bottom (kinds, Undo, finished-job buttons, speed + time left)
mod over; // r2-layout: menus over the video (L1), clicks that close a menu and go on (L39)
#[cfg(windows)]
mod winvideo;
#[cfg(windows)]
mod menuwin; // 11 Nov 2026: menus over the live video in a click-through window of their own

use slint::{ComponentHandle, GraphicsAPI, Model, ModelRc, RenderingState, Rgb8Pixel, SharedPixelBuffer, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void, CStr};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use glow::HasContext;

thread_local! {
    static LIB: RefCell<Option<Rc<library::LibModel>>> = const { RefCell::new(None) };
    static UI: RefCell<Option<slint::Weak<AppWindow>>> = const { RefCell::new(None) };
    static SHEETS: RefCell<SheetCache> = RefCell::new(SheetCache::default());
    static SCROLL: RefCell<Glide> = RefCell::new(Glide::default());
    /// the window's own mouse events (fullscreen controls: show on move, hide after 2.5 s still or when the mouse
    /// leaves the window; never while a button is held)
    static MOUSE: RefCell<MouseState> = RefCell::new(MouseState { last_move: Instant::now(), inside: true, pressed: false, y: 0.0 });
    static UI_SYNC: RefCell<Option<Rc<dyn Fn(bool)>>> = const { RefCell::new(None) };
    /// open the editor on a clip index (set once the player exists; native chat 5)
    static OPEN_EDITOR: RefCell<Option<Rc<dyn Fn(usize)>>> = const { RefCell::new(None) };
}

/// decoded seek-picture sheets, by clip index (at most 2: the hovered card + the playing clip)
#[derive(Default)]
struct SheetCache {
    items: Vec<(usize, slint::Image, i32, i32)>,
    /// the player's sheet as pixels (fullscreen controls drawn by mpv need them)
    player_px: Option<(usize, SharedPixelBuffer<Rgb8Pixel>, u32, u32)>,
    hover: Option<(usize, i32)>, // (clip index, view row) waiting for / showing the flipbook
    player: Option<usize>,       // clip index the player wants
    failed: Vec<usize>,
}

struct MouseState {
    last_move: Instant,
    inside: bool,
    pressed: bool,
    y: f32, // logical
}

/// wheel scrolling still to give out (logical px) - see the mouse wheel note in main()
#[derive(Default)]
struct Glide {
    left_x: f32,
    left_y: f32,
    pos: slint::LogicalPosition,
    last: Option<Instant>,
}

thread_local! {
    /// Ctrl / Shift as winit last reported them (= what Slint believes is held)
    static HELD_MODS: Cell<(bool, bool)> = const { Cell::new((false, false)) };
}

/// 9 Nov: "Ctrl seemed stuck" - clicking a card selected it instead of playing it until it "fixed itself".
/// When a key-up is missed (released while another window had focus), Slint keeps the modifier held. Before a click
/// and when the window gets focus again: if Windows says Ctrl / Shift aren't really down, tell Slint they were let go.
fn unstick_mods(win: &slint::Window) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_SHIFT};
        let down = |vk: u16| unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 };
        // It was seen again with this check limited to "winit says held": Slint keeps its OWN modifier state, which
        // can stay stuck while winit's is fine. So whenever Windows says the key is up, Slint is told so (a release of a
        // key that isn't held changes nothing; the Library's Ctrl mode just ends, which is right).
        let (ctrl, shift) = HELD_MODS.with(|x| x.get());
        if !down(VK_CONTROL) {
            win.dispatch_event(slint::platform::WindowEvent::KeyReleased { text: slint::platform::Key::Control.into() });
            if ctrl {
                echo_core::recorder::log_line("ui: Ctrl was stuck (key-up missed) - released");
            }
        }
        if !down(VK_SHIFT) {
            win.dispatch_event(slint::platform::WindowEvent::KeyReleased { text: slint::platform::Key::Shift.into() });
        }
        HELD_MODS.with(|x| x.set((ctrl && down(VK_CONTROL), shift && down(VK_SHIFT))));
    }
    #[cfg(not(windows))]
    let _ = win;
}

/// one frame's share of the wheel glide (called after each drawn frame)
fn glide_step(w: &slint::Weak<AppWindow>) {
    let step = SCROLL.with(|s| {
        let mut s = s.borrow_mut();
        if s.left_x == 0.0 && s.left_y == 0.0 {
            return None;
        }
        let now = Instant::now();
        let dt = s.last.map(|t| (now - t).as_secs_f32()).unwrap_or(1.0 / 120.0).clamp(0.001, 0.05);
        s.last = Some(now);
        // exponential ease-out, time based (same feel at 60 or 360 Hz): ~95 % done after 150 ms
        let k = 1.0 - (-dt / 0.05).exp();
        let take = |left: &mut f32| -> f32 {
            let t = if left.abs() < 0.75 { *left } else { *left * k };
            *left -= t;
            t
        };
        let (x, y) = (take(&mut s.left_x), take(&mut s.left_y));
        Some((x, y, s.pos))
    });
    if let Some((x, y, pos)) = step {
        let w = w.clone();
        // after the frame is done (not inside the renderer): move, which also asks for the next frame
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(a) = w.upgrade() {
                a.window().dispatch_event(slint::platform::WindowEvent::PointerScrolled { position: pos, delta_x: x, delta_y: y });
                a.window().request_redraw();
            }
        });
    }
}

#[derive(Default)]
struct Gpu {
    fbo: Option<glow::NativeFramebuffer>,
    tex: Option<glow::NativeTexture>,
    size: (i32, i32),
}

/// How the video is drawn. The player starts with the best one and falls back on its own:
/// Windows: Window (own Direct3D 11 child window, frames stay on the GPU) -> Texture (mpv draws into an OpenGL
/// texture inside Slint) -> Software (mpv draws on the CPU into memory). Linux starts at Texture.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Window,
    Texture,
    Software,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Window => "window (Direct3D 11)",
            Mode::Texture => "OpenGL texture",
            Mode::Software => "software (CPU)",
        }
    }
    fn next(self) -> Option<Mode> {
        match self {
            Mode::Window => Some(Mode::Texture),
            Mode::Texture => Some(Mode::Software),
            Mode::Software => None,
        }
    }
}

struct Player {
    mode: Mode,
    hwdec: String,
    #[cfg(windows)]
    vwin: Option<winvideo::VideoWindow>,
    /// normal view: the thumbnail in its own child window over mpv's while a clip opens
    #[cfg(windows)]
    poster: Option<winvideo::PosterWindow>,
    /// the opened clip's thumbnail pixels (for the poster window)
    poster_px: Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>>,
    mpv: Option<mpv::Mpv>,
    mpv_error: String,
    gpu: Gpu,
    open: bool,
    /// clip index + view row of what's open
    clip: Option<usize>,
    row: usize,
    /// a file to load once mpv is ready (path, start second)
    pending: Option<(PathBuf, f64)>,
    loaded_at: Option<Instant>,
    vo_checked: bool,
    /// Windows window mode: the new clip's first frame is on screen (the video window stays hidden until then -
    /// otherwise it flashed black with a piece of the previous clip)
    first_frame: bool,
    /// Windows flip model: the video window was already showing a clip when the next one was loaded (prev/next) -
    /// it stays, and the new clip's first picture simply replaces the old one
    swap: bool,
    /// this mode didn't work: switch to the next one at the next draw (needs the GL context for Texture)
    fail: Option<String>,
    /// switch drawing mode for fullscreen (Windows: window mode can't have controls drawn over the video, so
    /// fullscreen uses the OpenGL texture mode and goes back to window mode afterwards)
    switch_to: Option<Mode>,
    /// the mode used outside fullscreen (changes only when a mode fails)
    base_mode: Mode,
    /// the clip reopened by a mode switch was paused: keep it paused
    start_paused: bool,
    /// fullscreen controls drawn by mpv (Windows window mode)
    osd: osd::Osd,
    /// mpv not used for a while: shut it down at the next draw
    kill: bool,
    closed_at: Option<Instant>,
    frames: u32,
    fps_at: Instant,
    fps: f64,
    render_ms_max: f64,
    duration: f64,
    info_done: bool,
    log: Vec<String>,
    /// note typed in the side panel, saved after a short pause (clip index, text)
    note_dirty: Option<(usize, String)>,
}

fn log(p: &mut Player, line: String) {
    log_to(&mut p.log, line);
}

/// same as log(), for code that holds mpv borrowed from the Player (only the log list is borrowed here)
fn log_to(list: &mut Vec<String>, line: String) {
    eprintln!("{line}");
    // the prototype's log: proto.log next to the exe
    if let Some(d) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("proto.log"))) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(d) {
            let _ = writeln!(f, "{} {line}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        }
    }
    list.push(line);
    if list.len() > 3 {
        list.remove(0);
    }
}

/// Echo's CHANGELOG.md -> [(version, notes)] newest first ("## 1.0.70 - 7 Nov" + its "- " lines; "Next release" and
/// "(... internal build ...)" lines left out). ECHO_CHANGELOG=<file>, else Echo/CHANGELOG.md next to prototype-native.
fn changelog() -> Vec<(String, String)> {
    let mut cands: Vec<std::path::PathBuf> = vec![];
    if let Ok(f) = std::env::var("ECHO_CHANGELOG") {
        cands.push(f.into());
    }
    if let Some(d) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) {
        cands.push(d.join("..").join("CHANGELOG.md"));
        cands.push(d.join("../../..").join("CHANGELOG.md")); // target/release/
    }
    let Some(txt) = cands.iter().find_map(|p| std::fs::read_to_string(p).ok()) else { return vec![] };
    let mut out: Vec<(String, Vec<String>)> = vec![];
    for line in txt.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            let v = h.split_whitespace().next().unwrap_or("");
            if !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.') {
                out.push((v.to_string(), vec![]));
            } else {
                out.push((String::new(), vec![])); // "Next release": skipped below
            }
        } else if let (Some(b), Some(cur)) = (line.strip_prefix("- "), out.last_mut()) {
            if !b.starts_with('(') {
                cur.1.push(b.trim_end_matches('.').to_string());
            }
        }
    }
    out.into_iter().filter(|(v, n)| !v.is_empty() && !n.is_empty()).map(|(v, n)| (v, n.join(" · "))).collect()
}

fn ver_gt(a: &str, b: &str) -> bool {
    let p = |s: &str| s.split('.').map(|x| x.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>();
    let (x, y) = (p(a), p(b));
    for i in 0..3 {
        let (u, w) = (x.get(i).copied().unwrap_or(0), y.get(i).copied().unwrap_or(0));
        if u != w {
            return u > w;
        }
    }
    false
}

/// Echo's sidebar update card, look-only (the prototype has no version and doesn't update). Test hooks:
/// ECHO_WHATSNEW=<old version> = the card after an update ("Updated to Echo <newest>", "View N earlier updates you
/// missed"); ECHO_UPDATE=<your version> = "Update ready" before one ("+ N more updates included").
fn setup_updates(app: &AppWindow) {
    let (after, from) = match (std::env::var("ECHO_WHATSNEW"), std::env::var("ECHO_UPDATE")) {
        (Ok(v), _) => (true, v),
        (_, Ok(v)) => (false, v),
        _ => return,
    };
    let list = changelog();
    let Some((to, notes)) = list.first().cloned() else {
        toast(app, "No CHANGELOG.md found for the update card (ECHO_CHANGELOG=<file>)");
        return;
    };
    let u = app.global::<Upd>();
    // the versions in between (newest first), like Echo's showChanges()
    let earlier: Vec<(String, String)> = list.iter().skip(1).filter(|(v, _)| ver_gt(v, &from)).cloned().collect();
    if after {
        u.set_title(format!("Updated to Echo {to}").into());
        u.set_text(format!("What's new: {notes}").into());
        u.set_btn("Got it".into());
        u.set_primary(false);
    } else {
        u.set_title("Update ready".into());
        u.set_text(format!("Echo {to} is here (you have {from}). What's new: {notes}").into());
        u.set_btn("Restart Echo".into());
        u.set_primary(true);
    }
    let n = earlier.len();
    let s = if n > 1 { "s" } else { "" };
    u.set_more(match (n, after) {
        (0, _) => "".into(),
        (_, true) => format!("View {n} earlier update{s} you missed").into(),
        (_, false) => format!("+ {n} more update{s} included").into(),
    });
    // up to 8 shown; more than that = the newest 6 + a link to the full list
    let show: Vec<UpdOld> = earlier.iter().take(if n > 8 { 6 } else { 8 }).map(|(v, t)| UpdOld { v: v.into(), notes: t.into() }).collect();
    let rest = n - show.len();
    u.set_older(Rc::new(slint::VecModel::from(show)).into());
    u.set_all(if rest > 0 { format!("+ {rest} older update{} on echoclip.link", if rest > 1 { "s" } else { "" }) } else { "All changes on echoclip.link".into() }.into());
    u.set_on(true);
    // (the buttons are setup.rs's real ones - this test hook used to replace them with look-only toasts)
}

thread_local! {
    /// ECHO_DEBUG_OSD: when F was last pressed (frames for 3 s after it go to osd-debug.log)
    static FULL_AT: Cell<Option<Instant>> = const { Cell::new(None) };
    /// entering fullscreen: when F was pressed + the monitor's size. The fullscreen controls (mpv overlay) wait until
    /// Slint's window AND mpv's picture have the fullscreen size - the first frames after F still have the OLD
    /// window size, and a band drawn for it sat higher/narrower and then jumped (the "flicker in").
    static FS_ENTER: Cell<Option<(Instant, Option<(u32, u32)>)>> = const { Cell::new(None) };
}

/// ECHO_DEBUG_OSD=1: one line per frame for 3 s after entering/leaving fullscreen, into osd-debug.log next to the exe
/// (the Windows build has no console, so eprintln is lost there)
fn osd_dbg(line: &str) {
    let Some(t) = FULL_AT.with(|c| c.get()) else { return };
    let ms = t.elapsed().as_millis();
    if ms > 3000 {
        return;
    }
    if let Some(d) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("osd-debug.log"))) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(d) {
            let _ = writeln!(f, "+{ms:>4} ms  {line}");
        }
    }
}

fn simulated_fail(m: Mode) -> bool {
    // ECHO_FAIL=window,texture pretends those modes don't work (tests the fallback chain)
    std::env::var("ECHO_FAIL").map(|v| v.split(',').any(|x| x.trim() == format!("{m:?}").to_lowercase())).unwrap_or(false)
}

// ---- callbacks mpv calls ----
type Gpa<'a> = &'a dyn Fn(&CStr) -> *const c_void;
extern "C" fn gpa_trampoline(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    if ctx.is_null() || name.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let f = &*(ctx as *const Gpa);
        f(CStr::from_ptr(name)) as *mut c_void
    }
}
extern "C" fn on_mpv_update(ctx: *mut c_void) {
    // mpv's thread: a new frame is ready -> ask Slint to repaint
    let w = unsafe { &*(ctx as *const slint::Weak<AppWindow>) };
    let _ = w.upgrade_in_event_loop(|a| a.window().request_redraw());
}

/// header pills from settings.json's hotkeys ("Shift F11  Clip 3m")
fn hotkey_pills(settings: Option<&serde_json::Value>) -> Vec<HotkeyPill> {
    let mut pills = vec![];
    if let Some(hk) = settings.and_then(|s| s.get("hotkeys")).and_then(|v| v.as_array()) {
        for (i, h) in hk.iter().enumerate() {
            let keys = h.get("keys").and_then(|k| k.as_str()).unwrap_or("");
            let secs = h.get("seconds").and_then(|k| k.as_u64()).unwrap_or(0);
            if keys.is_empty() {
                continue;
            }
            // gaps (10 Nov): Linux with KDE's portal shows the key KDE really uses (hdr_home.rs)
            let kde = hdr_home::kde_show(keys);
            let parts: Vec<SharedString> = kde.clone().unwrap_or_else(|| settings::key_parts(keys)).into_iter().map(SharedString::from).collect();
            pills.push(HotkeyPill { keys: ModelRc::new(VecModel::from(parts)), label: library::fmt_len(secs).into(), idx: i as i32, secs: secs as i32, kde: kde.is_some() });
        }
    }
    pills
}

/// Echo's colour theme -> the T tokens (whole app follows)
fn apply_theme(a: &AppWindow, st: &settings::Store) {
    let p = settings::theme_vars(&st.s("theme"), &st.s("theme_accent"), &st.s("theme_bg"), &st.s("theme_hi"));
    let t = a.global::<T>();
    t.set_bg(p.bg);
    t.set_rail(p.rail);
    t.set_panel(p.panel);
    t.set_panel2(p.panel2);
    t.set_line(p.line);
    t.set_hover(p.hover);
    t.set_text(p.text);
    t.set_muted(p.muted);
    t.set_dim(p.dim);
    t.set_accent(p.accent);
    t.set_accent2(p.accent2);
    t.set_hi(p.hi);
    t.set_g0(p.grad[0]);
    t.set_g1(p.grad[1]);
    t.set_g2(p.grad[2]);
    t.set_on_accent(p.on_accent);
    t.set_on_hi(p.on_hi);
    t.set_hi_text(p.hi_text);
}

fn open_url(u: &str) {
    // r2-threads (L27): CREATE_NO_WINDOW - cmd's console no longer flashes up
    #[cfg(windows)]
    let _ = std::os::windows::process::CommandExt::creation_flags(std::process::Command::new("cmd").args(["/C", "start", "", u]), 0x0800_0000).spawn();
    #[cfg(not(windows))]
    let _ = crate::bgwork::spawn_reaped(std::process::Command::new("xdg-open").arg(u)); // r2-threads (L52): reaped
}

// ---------- Echo's #notes: messages + progress boxes in ONE stack -> notes.rs (2.0.2) ----------
use notes::{notes_init, prog_box, prog_btn, prog_done, prog_set, toast};

// ---------- Home ----------
thread_local! {
    /// Get started: (steps skipped, line hidden, loaded from native_ui.json yet) - saved per PC (lib_fx::ui_set)
    static GS: RefCell<(std::collections::HashSet<&'static str>, bool, bool)> = RefCell::new(Default::default());
    static HOME_COLS: std::cell::Cell<usize> = const { std::cell::Cell::new(4) };
    /// the sharp still on Home: (clip path, picture) - made once per latest clip
    static HERO: RefCell<(PathBuf, Option<slint::Image>)> = RefCell::new((PathBuf::new(), None));
    /// 12 Nov 2026: play() is loading Home's clip paused in Home's picture (not a real "play": no focus, no "watched")
    static HOME_QUIET: Cell<bool> = const { Cell::new(false) };
}

/// Echo's Get started steps: (id, title, text, button)
const GS_STEPS: [(&str, &str, &str, &str); 5] = [
    ("clip", "save your first clip", "press your clip hotkey in a game", ""),
    ("edit", "trim or edit a clip", "cut it down to the good part", "Open the editor"),
    ("account", "create a free Echo account", "profile page, link views, 10 GB backup", "Create account"),
    ("share", "share a clip", "copy one into Discord or make a link", "Copy for Discord"),
    ("backup", "back up your clips", "to Echo Cloud or your own storage", "Set up"),
];

fn gs_next(lib: &library::LibModel) -> (Vec<bool>, Option<usize>) {
    let s = library::echo_settings();
    let signed = s.as_ref().and_then(|v| v.get("cloud_accounts")).and_then(|a| a.as_array()).is_some_and(|a| a.iter().any(|x| x.get("provider").and_then(|p| p.as_str()) == Some("echocloud")));
    let cloud = s.as_ref().and_then(|v| v.get("cloud")).is_some_and(|c| !c.is_null() && c != &serde_json::Value::Bool(false) && c != "");
    let all = lib.all();
    // The line kept coming back - "share" was hard-coded false and Skip / Hide weren't saved. Now like
    // Echo: share = any share link or Copy used once; skips + hide kept per PC in native_ui.json
    // Echo's shared_clips (shared.json + links that still work) - the same set as Echo's Home step
    let shared = bgwork::shared_any(&lib.root()) || lib_fx::ui_get("gs_copied").as_bool().unwrap_or(false); // r2-threads (L29)
    let real = [!all.is_empty(), all.iter().any(|c| c.edited), signed, shared, cloud];
    GS.with(|g| {
        let mut g = g.borrow_mut();
        if !g.2 {
            g.2 = true;
            if let Some(a) = lib_fx::ui_get("gs_skip").as_array() {
                for s in a.iter().filter_map(|x| x.as_str()) {
                    if let Some(id) = GS_STEPS.iter().map(|s| s.0).find(|id| *id == s) {
                        g.0.insert(id);
                    }
                }
            }
            g.1 = lib_fx::ui_get("gs_hide").as_bool().unwrap_or(false);
        }
        let done: Vec<bool> = GS_STEPS.iter().zip(real).map(|((id, ..), r)| r || g.0.contains(id)).collect();
        let next = if g.1 { None } else { done.iter().position(|d| !d) };
        (done, next)
    })
}

/// fill Home from the library (cheap; the big still is made once per latest clip in the background)
fn home_fill(a: &AppWindow, lib: &Rc<library::LibModel>) {
    let h = a.global::<HomeState>();
    h.set_loading(lib.len() == 0 && lib.loading());
    // 11 Nov 2026: Home card 0 = the clip on show (home_inbox.rs), so the code below (hero still, meta) follows it
    let refill: Rc<dyn Fn()> = {
        let (w, l) = (a.as_weak(), lib.clone());
        Rc::new(move || {
            if let Some(a) = w.upgrade() {
                home_fill(&a, &l);
            }
        })
    };
    home_inbox::fill(a, lib, refill);
    h.set_extra(lib.home_extra());
    let (week, total, bytes, games, bars) = lib.home_stats();
    h.set_total(total.to_string().into());
    h.set_size(library::fmt_size(bytes).into());
    h.set_games(games.to_string().into());
    h.set_games_word(if games == 1 { "game" } else { "games" }.into());
    h.set_bars_max(bars.iter().copied().max().unwrap_or(1).max(1) as i32);
    h.set_spark(bars.iter().filter(|&&n| n > 0).count() >= 3);
    h.set_bars(ModelRc::new(VecModel::from(bars.iter().map(|&n| n as i32).collect::<Vec<_>>())));
    // 10 Nov: Home's first number box - clips per calendar day, last 7 days (today last) + day letters;
    // "clips this week" = the same 7 days, so the number and the bars always agree
    {
        use chrono::{Datelike, Local};
        let today = Local::now().date_naive();
        let mut days = [0i32; 7];
        for c in lib.all().iter() {
            let t: chrono::DateTime<Local> = c.mtime.into();
            let back = (today - t.date_naive()).num_days();
            if (0..7).contains(&back) {
                days[6 - back as usize] += 1;
            }
        }
        const L: [&str; 7] = ["M", "T", "W", "T", "F", "S", "S"];
        let letters: Vec<SharedString> = (0..7i64).map(|i| L[(today - chrono::Duration::days(6 - i)).weekday().num_days_from_monday() as usize].into()).collect();
        h.set_week(days.iter().sum::<i32>().to_string().into());
        h.set_days_max(days.iter().copied().max().unwrap_or(1).max(1));
        h.set_days(ModelRc::new(VecModel::from(days.to_vec())));
        h.set_day_l(ModelRc::new(VecModel::from(letters)));
    }
    let _ = week;
    let (done, next) = gs_next(lib);
    h.set_gs_done(ModelRc::new(VecModel::from(done)));
    h.set_gs_on(next.is_some());
    if let Some(i) = next {
        let (_, t, x, g) = GS_STEPS[i];
        h.set_gs_title(t.into());
        h.set_gs_text(x.into());
        h.set_gs_go(g.into());
    }
    let key = library::echo_settings()
        .and_then(|s| s.get("hotkeys").and_then(|v| v.get(0)).and_then(|v| v.get("keys")).and_then(|k| k.as_str()).map(String::from))
        .filter(|k| !k.is_empty());
    h.set_empty_key(key.map(|k| settings::key_parts(&k).join("+")).unwrap_or_else(|| "your clip hotkey".into()).into());
    hdr_home::charm(a, lib); // gaps (10 Nov): milestones + "On this day" follow the list
    // the latest clip: "1 hour ago · 58.8 MB · FiveM" + its picture
    let Some(idx) = lib.index_at(library::HOME_BASE) else { return };
    let c = lib.clip(idx);
    // (the line under the title, "FiveM · 32 min ago", is set by home_inbox::fill)
    let have = HERO.with(|x| {
        let x = x.borrow();
        (x.0 == c.path).then(|| x.1.clone()).flatten()
    });
    match have {
        Some(img) => h.set_hero(img),
        None => {
            // the card picture at once (small), a sharp 1600 px still from ffmpeg when it's ready (Echo's hero_frame)
            h.set_hero(lib.thumb_img(idx).unwrap_or_default());
            if HERO.with(|x| x.borrow().0 == c.path) {
                return; // already being made
            }
            let (path, dur, w) = (c.path.clone(), c.dur.unwrap_or(0.0), a.as_weak());
            let stamp = c.mtime.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let dir = std::env::temp_dir().join("echo-proto-hero");
            // "-start": the still is of the clip's start now (the older "-full" files were 15 % in)
            let out = dir.join(format!("{}-{stamp}-start.jpg", path.file_stem().unwrap_or_default().to_string_lossy()));
            // 11 Nov 2026 (William: "why is this image preview like 360p"): without ffmpeg yet (a first start still
            // downloads it) this used to mark the still as "being made" and stop - Home kept the small card picture
            // for good. Now a still already on disk is used without ffmpeg, and with neither, the next fill tries again.
            let ff = sheets::ffmpeg();
            if ff.is_none() && !out.exists() {
                return;
            }
            // clip-thumbs (12 Nov 2026): a cloud-only clip has no file here - its sharp still comes from the cloud copy
            // over the streaming server (Home's player streams it from there anyway); before, Home kept the small card
            // picture stretched big. Not ready yet: Home is filled again once the stream address is there.
            let input = if c.path.exists() || c.cloud.is_none() || out.exists() {
                c.path.clone()
            } else {
                let (w2, l2) = (a.as_weak(), lib.clone());
                if !cloud::play_ready(&c.path, move || if let Some(a) = w2.upgrade() { home_fill(&a, &l2) }) {
                    return;
                }
                let u = cloud::play_path(&c.path);
                if !u.to_string_lossy().starts_with("http") {
                    return; // not streamable (signed out, not in the cloud): the card picture stays
                }
                u
            };
            HERO.with(|x| *x.borrow_mut() = (c.path.clone(), None));
            std::thread::spawn(move || {
                let _ = std::fs::create_dir_all(&dir);
                if let (false, Some(ff)) = (out.exists(), ff) {
                    let mut cmd = std::process::Command::new(&ff);
                    #[cfg(windows)]
                    {
                        use std::os::windows::process::CommandExt;
                        cmd.creation_flags(0x0800_0000);
                    }
                    let at = home_still_at(dur);
                    let _ = cmd
                        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{at:.2}"), "-i"])
                        .arg(&input)
                        .args(["-frames:v", "1", "-vf", "scale=w='min(iw,1920)':h=-2:flags=lanczos", "-q:v", "2"]) // 11 Nov 2026: full clip width (was 1600 px)
                        .arg(&out)
                        .stdin(std::process::Stdio::null())
                        .output();
                }
                let Some(px) = library::decode(&out) else {
                    // no still came out: let the next fill try again (instead of "being made" for good)
                    let _ = slint::invoke_from_event_loop(move || {
                        HERO.with(|x| {
                            let mut x = x.borrow_mut();
                            if x.0 == path && x.1.is_none() {
                                *x = (PathBuf::new(), None);
                            }
                        });
                    });
                    return;
                };
                let _ = slint::invoke_from_event_loop(move || {
                    let img = slint::Image::from_rgb8(px);
                    let still_latest = HERO.with(|x| {
                        let mut x = x.borrow_mut();
                        if x.0 == path {
                            x.1 = Some(img.clone());
                            true
                        } else {
                            false
                        }
                    });
                    if let (true, Some(a)) = (still_latest, w.upgrade()) {
                        a.global::<HomeState>().set_hero(img);
                    }
                });
            });
        }
    }
}

/// 12 Nov 2026: Echo is the window in front right now (Windows: asked from the system - WIN_FOCUSED starts as true and
/// only follows focus events, which a window shown without activation never gets). Elsewhere: WIN_FOCUSED.
fn echo_in_front(win: &slint::Window) -> bool {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let h = win.window_handle();
        if let Ok(RawWindowHandle::Win32(h)) = h.window_handle().map(|h| h.as_raw()) {
            let fg = unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
            return fg as isize == h.hwnd.get();
        }
    }
    let _ = win;
    WIN_FOCUSED.with(|f| f.get())
}

/// the moment Home shows of a clip: its still, and where Home's paused clip waits - its start (0:00; half a second
/// in, the very first frame of a recording can be black). 12 Nov 2026 (William): coming back to Home shows 0:00, not
/// a spot further in (was 15 % of the clip, at most 5 s).
fn home_still_at(_dur: f64) -> f64 {
    0.5
}

// ---------- Echo's #confirm dialog (confirmBox / choiceBox / askText) ----------
type DlgCb = Box<dyn FnOnce(&AppWindow, String, String)>;
thread_local! {
    static DLG_CB: RefCell<Option<DlgCb>> = RefCell::new(None);
}

/// Open the dialog. `choices` = (label, value, kind "" / "primary" / "delb"), in the order shown (Cancel first, like
/// Echo). `input` = Some(placeholder) for askText. `enter` = the value Enter answers ("" = none, choiceBox).
/// `done(app, value, text)`: value "" = closed without choosing.
fn ask(a: &AppWindow, title: &str, msg: &str, choices: &[(&str, &str, &str)], input: Option<&str>, enter: &str, done: impl FnOnce(&AppWindow, String, String) + 'static) {
    let d = a.global::<Dlg>();
    d.set_title(title.into());
    d.set_msg(msg.into());
    d.set_choices(ModelRc::new(VecModel::from(
        choices.iter().map(|(l, v, k)| DChoice { label: (*l).into(), value: (*v).into(), kind: (*k).into() }).collect::<Vec<_>>(),
    )));
    d.set_input(input.is_some());
    d.set_placeholder(input.unwrap_or("").into());
    d.set_text("".into());
    d.set_enter(enter.into());
    DLG_CB.with(|c| *c.borrow_mut() = Some(Box::new(done)));
    d.set_open(true);
    // focus once the layer is shown (it isn't visible yet in this same event)
    let w = a.as_weak();
    slint::Timer::single_shot(Duration::from_millis(0), move || {
        if let Some(a) = w.upgrade() {
            a.invoke_focus_dialog();
        }
    });
}

/// Echo's confirmBox: [Cancel] [yes (red)], Enter = yes
fn confirm_box(a: &AppWindow, title: &str, msg: &str, yes: &str, done: impl FnOnce(&AppWindow) + 'static) {
    ask(a, title, msg, &[("Cancel", "", ""), (yes, "yes", "delb")], None, "yes", move |a, v, _| {
        if v == "yes" {
            done(a);
        }
    });
}

/// Delete clips (Chat B: real now - Recycle Bin / Trash). gaps-library row 42: Echo's dialogs (hidden-clips line,
/// choice order, Enter does nothing on the choice, progress box) are in lib_del.rs.
fn delete_clips_ui(a: &AppWindow, ids: Vec<usize>) {
    let Some(lib) = LIB.with(|l| l.borrow().clone()) else { return };
    lib_del::delete_ui(a, lib, ids);
}

fn open_editor(idx: usize) {
    if let Some(f) = OPEN_EDITOR.with(|o| o.borrow().clone()) {
        f(idx);
    }
}

fn show_in_folder(p: &std::path::Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", p.display())).spawn();
    }
    #[cfg(not(windows))]
    {
        // Chat 23: the file manager opens WITH the clip selected (org.freedesktop.FileManager1.ShowItems - Dolphin,
        // Nautilus, Nemo...); only the folder through xdg-open when that isn't there. Off the UI thread (dbus-send waits).
        let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        std::thread::spawn(move || {
            let uri: String = p.to_string_lossy().bytes().map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            }).collect();
            let ok = std::process::Command::new("dbus-send")
                .args(["--session", "--print-reply", "--reply-timeout=3000", "--dest=org.freedesktop.FileManager1", "/org/freedesktop/FileManager1", "org.freedesktop.FileManager1.ShowItems"])
                .arg(format!("array:string:file://{uri}")).arg("string:")
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success());
            if !ok {
                if let Some(d) = p.parent() {
                    let _ = crate::bgwork::spawn_reaped(std::process::Command::new("xdg-open").arg(d)); // r2-threads (L52): reaped
                }
            }
        });
    }
}

/// m:ss.d (trim times)
fn fmt1(s: f32) -> String {
    let t = (s.max(0.0) * 10.0).round() as u64;
    format!("{}:{:02}.{}", t / 600, t % 600 / 10, t % 10)
}

/// Echo's #sinfo: folder, size, date (+ resolution / codec once playing)
/// the side panel's detail chips: size, then 1080p60 + codec once playing, then the date ("11 Nov · 17:43")
fn info_chips(c: &library::Clip, video: Option<(String, String, f64)>) -> ModelRc<slint::SharedString> {
    let when: chrono::DateTime<chrono::Local> = c.mtime.into();
    let mut v: Vec<slint::SharedString> = vec![library::fmt_size(c.size).into()];
    if let Some((h, codec, fps)) = video {
        v.push(format!("{h}p{fps:.0}").into());
        if !codec.is_empty() {
            v.push(codec.to_uppercase().into());
        }
    }
    v.push(when.format("%-d %b · %H:%M").to_string().into());
    ModelRc::new(VecModel::from(v))
}
fn info_text(c: &library::Clip, video: Option<String>) -> String {
    let when: chrono::DateTime<chrono::Local> = c.mtime.into();
    let mut s = format!(
        "{}\n{}\n{}",
        if c.folder.is_empty() { "Main folder".to_string() } else { c.folder.clone() },
        library::fmt_size(c.size),
        when.format("%d/%m/%Y, %H.%M.%S")
    );
    if let Some(v) = video {
        s.push('\n');
        s.push_str(&v);
    }
    s
}

/// night 9 Nov: everything that used to say "isn't wired in the prototype yet" is real now; anything that still lands
/// here is a path nobody expected - it goes to the log (proto log), never to the user as "look-only"
fn unhandled(what: &str) {
    echo_core::recorder::log_line(&format!("ui: unhandled action {what}"));
}

fn main() {
    rec_core::early_env(); // Chat A
    popup::select_backend(); // Chat A: popup window never takes focus
    if popup::test_hook() {
        return; // ECHO_POPUP_TEST=1: only the in-game popup, nothing else (popup.rs)
    }
    let app = AppWindow::new().expect("window");
    uiscale::install(&app); // 1080p vs 1440p: same size on every screen (before the window is shown)
    notes_init(&app);
    // test hook: ECHO_NOTES_TEST=1 shows two progress boxes + messages at start (the stack must never overlap)
    if std::env::var("ECHO_NOTES_TEST").is_ok() {
        let shrink = prog_box("Making it fit Discord (10 MB)");
        prog_set(shrink, None, Some("42%"), Some(42.0));
        prog_btn(shrink, "Cancel", move || prog_done(shrink, 0));
        let link = prog_box("Making link");
        prog_set(link, None, Some("Uploading 18%"), Some(18.0));
        let w = app.as_weak();
        slint::Timer::single_shot(Duration::from_millis(3500), move || {
            if let Some(a) = w.upgrade() {
                toast(&a, "Copied - paste it into Discord or anywhere");
                toast(&a, "Added to favourites");
                // 2.0.2: every kind + a button (the error stays until clicked)
                notes::say_act(notes::Kind::Done, "Clip deleted", "Undo", || {});
                notes::say(notes::Kind::Err, "Couldn't move it to the Trash, so nothing was deleted (test)");
                prog_done(link, 0);
                notes::say_act(notes::Kind::Done, "Link copied - expires in 7 days", "Open", || {});
            }
        });
    }
    UI.with(|u| *u.borrow_mut() = Some(app.as_weak()));

    // ---------- library ----------
    let clips_dir = library::find_clips_dir();
    let t0 = Instant::now();
    // r2-threads (L37): read on a worker; a library that takes longer than 200 ms comes in after the window opens
    let (clips, late_scan) = bgwork::scan_start(&clips_dir, Duration::from_millis(200));
    let scan_ms = t0.elapsed().as_millis();
    let (tx, rx) = std::sync::mpsc::channel::<(usize, PathBuf, Option<PathBuf>)>();
    std::thread::spawn(move || {
        let send = |idx: usize, px: slint::SharedPixelBuffer<slint::Rgb8Pixel>| {
            let _ = slint::invoke_from_event_loop(move || {
                LIB.with(|l| {
                    if let Some(m) = l.borrow().as_ref() {
                        m.thumb_ready(idx, slint::Image::from_rgb8(px));
                    }
                })
            });
        };
        while let Ok((idx, path, clip)) = rx.recv() {
            // 9 Nov: sharper card pictures - Echo's .thumbs/<name>.jpg is 480 px, stretched to ~600 px on a
            // 1440p screen. A 720 px still of the SAME moment (Echo's .thumbt, else 0.5 s) is made once per clip as
            // <name>.card720.jpg next to it (remade when Echo's picture changes, e.g. Use as thumbnail).
            let hi = library::card_hi_path(&path);
            if library::card_hi_fresh(&path, &hi) {
                if let Some(px) = library::decode(&hi) {
                    send(idx, px);
                    continue;
                }
            }
            if let Some(px) = library::decode(&path) {
                send(idx, px);
            }
            if let Some(clip) = clip {
                if library::make_card_hi(&clip, &path, &hi) {
                    if let Some(px) = library::decode(&hi) {
                        send(idx, px);
                    }
                }
            }
        }
    });
    let lib = Rc::new(library::LibModel::new(clips, clips_dir.join(".thumbs"), tx));
    lib.set_loading(late_scan.is_some()); // r2-threads (L37)
    LIB.with(|l| *l.borrow_mut() = Some(lib.clone()));
    app.set_cards(ModelRc::from(lib.clone()));
    app.set_rows(ModelRc::from(lib.rows.clone()));
    eprintln!("library: {} clips in {} scanned in {scan_ms} ms", lib.len(), clips_dir.display());

    // header + sidebar meter from Echo's settings.json (look-only: the prototype doesn't record)
    let settings = library::echo_settings();
    let cap_gb = settings.as_ref().and_then(|s| s.get("max_storage_gb")).and_then(|v| v.as_u64()).unwrap_or(0);
    app.set_hotkeys(ModelRc::new(VecModel::from(hotkey_pills(settings.as_ref()))));

    // everything that follows a change of view, favourites, tags or selection
    let sync = {
        let (w, lib) = (app.as_weak(), lib.clone());
        Rc::new(move |nav_too: bool| {
            let Some(a) = w.upgrade() else { return };
            let n = lib.view_len();
            let bytes: u64 = (0..n).filter_map(|r| lib.clip_at(r)).map(|c| c.size).sum();
            a.set_count_text(format!("{n} clip{} · {}", if n == 1 { "" } else { "s" }, library::fmt_size(bytes)).into());
            if lib.len() == 0 && lib.loading() {
                // r2-threads (L37): the clips folder is still being read - no "No clips yet" meanwhile
                a.set_empty_title("".into());
                a.set_empty_text("".into());
            } else if lib.len() == 0 {
                a.set_empty_title("No clips yet".into());
                a.set_empty_text("Press your hotkey in game to save the last moments.".into());
            } else {
                a.set_empty_title("Nothing here".into());
                a.set_empty_text("No clips match this search or filter.".into());
            }
            let total: u64 = lib.all().iter().map(|c| c.size).sum();
            a.set_storage_used(format!("{} used", library::fmt_size(total)).into());
            a.set_storage_cap(if cap_gb > 0 { format!("of {}", library::fmt_size(cap_gb * 1_000_000_000)) } else { "no limit".into() }.into());
            // 10 Nov: the status card's hover panel
            a.set_local_limit(if cap_gb > 0 { library::fmt_size(cap_gb * 1_000_000_000) } else { "No limit".into() }.into());
            a.set_storage_frac(if cap_gb > 0 { total as f32 / (cap_gb as f32 * 1e9) } else { (total as f32 / 50e9).min(1.0) });
            a.set_picked_n(lib.picked.borrow().len() as i32);
            a.global::<Lib>().set_picking(!lib.picked.borrow().is_empty());
            a.set_factive(ModelRc::new(VecModel::from(lib.factive())));
            a.set_flines(ModelRc::new(VecModel::from(lib.filter_lines())));
            if nav_too {
                a.set_nav(ModelRc::new(VecModel::from(lib.nav_items())));
                lib_cards::fill_folders(&a, &lib);
            }
            // 9 Nov, sketch E: the library's toolbar names the list (the sidebar entry) - it opens the filter
            let f = a.get_filter();
            let name = a.get_nav().iter().find(|n| n.id == f).map(|n| n.label.to_string()).unwrap_or_else(|| "All clips".into());
            a.set_list_name(name.into());
            home_fill(&a, &lib);
        })
    };
    app.global::<Play>().set_show_stats(std::env::var("ECHO_STATS").is_ok());
    app.global::<HomeState>().set_cards(ModelRc::new(library::HomeCards(lib.clone())));
    lib.refresh();
    sync(true);
    if let Some(rx) = late_scan {
        bgwork::scan_finish(&app, &lib, rx, sync.clone()); // r2-threads (L37)
    }
    {
        let (w, lib) = (app.as_weak(), lib.clone());
        app.global::<HomeState>().on_cols(move |n| {
            HOME_COLS.with(|c| c.set(n.max(1) as usize));
            if let Some(a) = w.upgrade() {
                home_fill(&a, &lib);
            }
        });
    }
    UI_SYNC.with(|f| *f.borrow_mut() = Some(sync.clone()));
    lib_cards::wire(&app, lib.clone(), sync.clone());
    lib_prefs::wire(&app, &lib, sync.clone());
    tidy::wire(&app, lib.clone());
    lib_shots::wire(&app, clips_dir.clone());

    {
        let (w, lib, sync) = (app.as_weak(), lib.clone(), sync.clone());
        app.on_pick_filter(move |id| {
            if let Some(a) = w.upgrade() {
                if id == "shots" {
                    lib_shots::show(&a, &lib.root());
                    return;
                }
                // Chat B: "cloud" = the library filtered to clips in the cloud (cloud-only ones included)
                a.set_view("library".into());
                a.set_filter(id.clone());
                // the same list again (disk sync, rename, upload, trim, Free up space call this to refresh): keep the
                // scroll place - only another folder / filter goes to the top
                let other = *lib.nav.borrow() != id.as_str();
                *lib.nav.borrow_mut() = id.to_string();
                lib.refresh();
                sync(false);
                if !other {
                    return;
                }
                // 10 Nov 2026: the card list to the top now that the new rows are in, and again once the ListView
                // has measured them (it kept the old folder's scroll position: a one-clip folder sat half out of view)
                a.set_lib_top_seq(a.get_lib_top_seq() + 1);
                let w2 = a.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(120), move || {
                    if let Some(a) = w2.upgrade() {
                        a.set_lib_top_seq(a.get_lib_top_seq() + 1);
                    }
                });
            }
        });
    }
    {
        let (lib, sync) = (lib.clone(), sync.clone());
        app.on_search(move |t| {
            *lib.search.borrow_mut() = t.to_string();
            lib.refresh();
            sync(false);
        });
    }
    {
        let lib = lib.clone();
        app.on_set_cols(move |n| lib.set_cols(n.max(1) as usize));
    }
    over::init(&app); // r2-layout (L39)
    {
        let lib = lib.clone();
        app.global::<Over>().on_grid_card_w(move |w| lib.set_card_w(w)); // r2-layout (L9): tag lines
    }
    {
        let (w, lib, sync) = (app.as_weak(), lib.clone(), sync.clone());
        app.on_set_sort(move |s| {
            if let Some(a) = w.upgrade() {
                a.set_sort(s.clone());
            }
            *lib.sort.borrow_mut() = s.to_string();
            lib.refresh();
            sync(false);
            lib_prefs::save_sort(&s);
            if let (Some(a), true) = (w.upgrade(), s == "long") {
                lib_prefs::probe_long(&a, &lib, sync.clone());
            }
        });
    }
    {
        let w = app.as_weak();
        app.on_set_view(move |list| {
            if let Some(a) = w.upgrade() {
                a.set_list_view(list);
            }
            lib_prefs::save_view(list);
        });
    }
    {
        let (lib, sync) = (lib.clone(), sync.clone());
        app.on_ftoggle(move |g, k| {
            lib.ftoggle(&g, &k);
            lib_prefs::save_filters(&lib);
            lib.refresh();
            sync(false);
        });
    }
    {
        let (lib, sync) = (lib.clone(), sync.clone());
        app.on_fclear(move || {
            lib.fsel.borrow_mut().clear();
            lib_prefs::save_filters(&lib);
            lib.refresh();
            sync(false);
        });
    }
    {
        let (lib, sync) = (lib.clone(), sync.clone());
        app.on_pick_all(move || {
            lib.pick_all();
            sync(false);
        });
    }
    {
        let (lib, sync) = (lib.clone(), sync.clone());
        app.on_pick_clear(move || {
            lib.clear_picks();
            sync(false);
        });
    }
    {
        let (w, lib, sync) = (app.as_weak(), lib.clone(), sync.clone());
        app.on_pick_fav(move || {
            let Some(a) = w.upgrade() else { return };
            let names: Vec<String> = lib.picked.borrow().iter().map(|&i| lib.clip(i).name.clone()).collect();
            let on = names.iter().any(|n| !lib.favs.borrow().contains(n));
            match lib.set_favs(&names, on) {
                Ok(()) => toast(&a, if on { format!("{} added to favourites", names.len()) } else { format!("{} removed from favourites", names.len()) }),
                Err(e) => toast(&a, format!("Couldn't save favourites: {e}")),
            }
            lib.clear_picks();
            if lib.nav.borrow().as_str() == "fav" || lib.fsel.borrow().get("type").is_some_and(|v| v.iter().any(|k| k == "fav")) {
                lib.refresh();
            }
            sync(true);
        });
    }
    {
        let (w, lib) = (app.as_weak(), lib.clone());
        app.on_pick_act(move |what| {
            if let Some(a) = w.upgrade() {
                hover_vid::stop(&a);
                let n = lib.picked.borrow().len();
                let many = format!("{n} clip{}", if n == 1 { "" } else { "s" });
                if what == "delete" {
                    // Echo's #pickdel
                    let mut ids: Vec<usize> = lib.picked.borrow().iter().cloned().collect();
                    ids.sort();
                    delete_clips_ui(&a, ids);
                    return;
                }
                if what == "combine" {
                    // Chat A: Echo's montage (recorder.rs combine_clips), oldest first like Echo
                    let mut ids: Vec<usize> = lib.picked.borrow().iter().cloned().collect();
                    ids.sort();
                    let mut cl: Vec<_> = ids.iter().map(|i| lib.clip(*i)).collect();
                    cl.sort_by_key(|c| std::fs::metadata(&c.path).and_then(|m| m.modified()).ok());
                    rec_core::combine_ui(&a, cl.iter().map(|c| c.path.to_string_lossy().to_string()).collect());
                    return;
                }
                let verb = match what.as_str() {
                    "move" => "Move",
                    _ => "Combine",
                };
                unhandled(&format!("pick {verb} {many}"));
            }
        });
    }
    // ---------- folders (Chat B): the selection bar's Move menu + dragging folders in the sidebar ----------
    {
        let (w, lib, sync) = (app.as_weak(), lib.clone(), sync.clone());
        app.on_move_picked(move |folder| {
            let Some(a) = w.upgrade() else { return };
            let mut ids: Vec<usize> = lib.picked.borrow().iter().cloned().collect();
            ids.sort();
            // r2-threads (L13): the files move on a worker
            let (w, l2, sync, folder) = (a.as_weak(), lib.clone(), sync.clone(), folder.to_string());
            let was = undo::before_move(&lib, &ids); // 2.0.2: Undo moves them back
            lib.move_clips_bg(&ids, &folder.clone(), move |n, err| {
                let Some(a) = w.upgrade() else { return };
                let lib = l2;
                // Echo's moveClips toast: no quotes round the folder, the last error after it (r2-texts L48) + Undo
                if n > 0 {
                    lib.clear_picks();
                    undo::moved(&a, &lib, was, n, &library::clean_folder(&folder), err);
                } else {
                    toast(&a, err.unwrap_or_else(|| "Nothing moved".into()));
                }
                lib.refresh();
                sync(true);
            });
        });
    }
    {
        let (w, lib) = (app.as_weak(), lib.clone());
        app.on_folder_drag_start(move |src| {
            let Some(a) = w.upgrade() else { return };
            let nav = a.get_nav();
            for i in 0..nav.row_count() {
                if let Some(mut n) = nav.row_data(i) {
                    let d = lib.drop_mode(&src, &n.folder);
                    if n.drop != d {
                        n.drop = d.into();
                        nav.set_row_data(i, n);
                    }
                }
            }
        });
    }
    {
        let (w, lib, sync) = (app.as_weak(), lib.clone(), sync.clone());
        app.on_folder_drop(move |src, target, zone| {
            let Some(a) = w.upgrade() else { return };
            let (src, target) = (src.to_string(), target.to_string());
            let after = {
                let (lib, sync) = (lib.clone(), sync.clone());
                move |a: &AppWindow| {
                    lib.refresh();
                    a.set_filter(lib.nav.borrow().as_str().into());
                    sync(true);
                }
            };
            if zone == "before" || zone == "after" {
                if let Err(e) = lib.reorder_folder(&src, &target, zone == "after") {
                    toast(&a, format!("Couldn't save the order: {e}"));
                }
                after(&a);
                return;
            }
            let leaf = src.rsplit('/').next().unwrap_or(&src).to_string();
            let new = if zone == "top" { leaf.clone() } else { format!("{target}/{leaf}") };
            let go = {
                let (lib, src, new, target, leaf) = (lib.clone(), src.clone(), new.clone(), target.clone(), leaf.clone());
                move |a: &AppWindow, merge: bool| {
                    // r2-threads (L13): the files move on a worker
                    let (w, src, new, target, leaf, after) = (a.as_weak(), src.clone(), new.clone(), target.clone(), leaf.clone(), after.clone());
                    lib.move_folder_bg(&src.clone(), &new.clone(), move |r| {
                        let Some(a) = w.upgrade() else { return };
                        let a = &a;
                        match r {
                            Ok(_) if merge => toast(a, format!("Merged \"{src}\" into \"{new}\"")),
                            Ok(_) if target.is_empty() => toast(a, format!("\"{leaf}\" is a top folder now")),
                            Ok(_) => toast(a, format!("Moved \"{leaf}\" into {target}")),
                            Err(e) => toast(a, e),
                        }
                        after(a);
                    });
                }
            };
            match lib.folder_exists(&new) {
                Some(hit) if !hit.eq_ignore_ascii_case(&src) => {
                    let msg = format!("{} already has a folder called \"{leaf}\". All clips in \"{src}\" move into it. New {leaf} clips will go there too.", if target.is_empty() { "The top level".to_string() } else { format!("\"{target}\"") });
                    confirm_box(&a, "Merge folders?", &msg, "Merge", move |a| go(a, true));
                }
                _ => go(&a, false),
            }
        });
    }
    {
        let (w, lib, sync) = (app.as_weak(), lib.clone(), sync.clone());
        app.on_folder_new(move |parent, name| {
            let Some(a) = w.upgrade() else { return };
            let full = if parent.is_empty() { name.to_string() } else { format!("{parent}/{name}") };
            match lib.create_folder(&full) {
                Ok(f) => toast(&a, format!("Made folder \"{f}\" - drag clips onto it")),
                Err(e) => toast(&a, e),
            }
            sync(true);
        });
    }
    {
        let (w, lib, sync) = (app.as_weak(), lib.clone(), sync.clone());
        app.on_folder_rename(move |folder, name| {
            let Some(a) = w.upgrade() else { return };
            let (folder, leaf) = (folder.to_string(), library::clean_folder(&name.replace('/', " ").replace('\\', " ")));
            let old_leaf = folder.rsplit('/').next().unwrap_or(&folder).to_string();
            if leaf.is_empty() || leaf == old_leaf {
                return;
            }
            let par = library::parent_of(&folder).to_string();
            let new = if par.is_empty() { leaf.clone() } else { format!("{par}/{leaf}") };
            let go = {
                let (lib, sync, folder, new, leaf) = (lib.clone(), sync.clone(), folder.clone(), new.clone(), leaf.clone());
                move |a: &AppWindow, merge: bool| {
                    // r2-threads (L13): the files move on a worker
                    let (w, l2, sync, folder, new, leaf) = (a.as_weak(), lib.clone(), sync.clone(), folder.clone(), new.clone(), leaf.clone());
                    lib.move_folder_bg(&folder.clone(), &new.clone(), move |r| {
                        let Some(a) = w.upgrade() else { return };
                        let (a, lib) = (&a, l2);
                        match r {
                            Ok(_) if merge => toast(a, format!("Merged \"{folder}\" into \"{new}\"")),
                            Ok(_) => toast(a, format!("Renamed to \"{leaf}\"")),
                            Err(e) => toast(a, e),
                        }
                        lib.refresh();
                        a.set_filter(lib.nav.borrow().as_str().into());
                        sync(true);
                    });
                }
            };
            match lib.folder_exists(&new) {
                Some(hit) if !hit.eq_ignore_ascii_case(&folder) => {
                    let msg = format!("There's already a folder called \"{hit}\". All clips in \"{folder}\" move into it. New {old_leaf} clips will go there too.");
                    confirm_box(&a, "Merge folders?", &msg, "Merge", move |a| go(a, true));
                }
                _ => go(&a, false),
            }
        });
    }
    {
        let (w, lib, sync, root) = (app.as_weak(), lib.clone(), sync.clone(), clips_dir.clone());
        app.on_folder_act(move |folder, what| {
            let Some(a) = w.upgrade() else { return };
            if let Some(to) = what.strip_prefix("merge:") {
                lib_merge::merge_ui(&a, lib.clone(), folder.to_string(), to.to_string(), sync.clone());
                return;
            }
            match what.as_str() {
                "notreal" => toast(&a, format!("\"{folder}\" isn't a folder yet - its clips are in the main folder. Drag clips onto it to make one.")),
                "open" => {
                    let d = root.join(folder.as_str());
                    #[cfg(windows)]
                    let _ = std::process::Command::new("explorer").arg(&d).spawn();
                    #[cfg(not(windows))]
                    let _ = crate::bgwork::spawn_reaped(std::process::Command::new("xdg-open").arg(&d)); // r2-threads (L52): reaped
                }
                "delete" => {
                    match lib.delete_folder(&folder) {
                        Ok(()) => {
                            if a.get_filter().as_str() == format!("game:{folder}") {
                                *lib.nav.borrow_mut() = "all".into();
                                a.set_filter("all".into());
                                lib.refresh();
                            }
                            toast(&a, format!("Deleted \"{folder}\""));
                        }
                        Err(e) => toast(&a, e),
                    }
                    sync(true);
                }
                _ => {}
            }
        });
    }
    // ---------- the dialog's answers ----------
    {
        let w = app.as_weak();
        app.global::<Dlg>().on_answer(move |v| {
            let Some(a) = w.upgrade() else { return };
            let d = a.global::<Dlg>();
            if !d.get_open() {
                return;
            }
            d.set_open(false);
            let text = d.get_text().to_string();
            // keys go back where they were (L2: editor / Tidy first - the editor keeps a hidden player "open")
            focus::back(&a);
            cloud::space_refocus(&a); // r2-texts L17: the Echo Cloud space manager under the dialog gets the keys back
            if let Some(cb) = DLG_CB.with(|c| c.borrow_mut().take()) {
                cb(&a, v.to_string(), text);
            }
            // the answer may have closed the editor / Tidy or left Settings: hand the keys on again (a new dialog
            // from the answer focuses itself)
            if !a.global::<Dlg>().get_open() {
                focus::back(&a);
            }
        });
    }
    // ---------- settings page ----------
    let store = Rc::new(RefCell::new(settings::Store::load()));
    {
        let mut st = store.borrow_mut();
        // games = the library's game folders; usage = size per game (+ main folder), biggest first
        let mut per: std::collections::BTreeMap<String, u64> = Default::default();
        let mut games: Vec<String> = vec![];
        for c in lib.all() {
            let g = c.folder.split('/').next().unwrap_or("").to_string();
            if !g.is_empty() && g != "Screenshots" && g != "Exported Clips" && !games.contains(&g) {
                games.push(g.clone());
            }
            *per.entry(if c.folder.is_empty() { "Main folder".to_string() } else { c.folder.split('/').next().unwrap_or("").to_string() }).or_default() += c.size;
        }
        games.sort_by_key(|g| g.to_lowercase());
        let mut usage: Vec<(String, u64)> = per.into_iter().collect();
        usage.sort_by(|a, b| b.1.cmp(&a.1));
        if usage.len() > 8 {
            let rest: u64 = usage[7..].iter().map(|x| x.1).sum();
            usage.truncate(7);
            usage.push(("Other".into(), rest));
        }
        st.total = usage.iter().map(|x| x.1).sum();
        st.usage = usage;
        st.games = games;
        st.monitors = settings::monitors();
        apply_theme(&app, &st);
    }
    // text weight boost (theme.slint T.fw): Windows default 50 (Slint's text renders thinner than WebView2's)
    let fw = std::env::var("ECHO_FONT_BOOST").ok().and_then(|v| v.parse::<i32>().ok()).unwrap_or(if cfg!(windows) { 50 } else { 0 });
    app.global::<T>().set_fw(fw.clamp(-200, 300));
    app.global::<S>().set_nav(settings::nav());
    let sbox_model = Rc::new(VecModel::<SBox>::default());
    app.global::<S>().set_boxes(ModelRc::from(sbox_model.clone()));
    let sbox_inner: Rc<RefCell<Vec<Rc<VecModel<SRow>>>>> = Default::default();
    let srefresh = {
        let (w, store, m, inner) = (app.as_weak(), store.clone(), sbox_model.clone(), sbox_inner.clone());
        Rc::new(move || {
            let Some(a) = w.upgrade() else { return };
            let st = store.borrow();
            let (title, intro, boxes, note) = st.page();
            let s = a.global::<S>();
            s.set_sec(st.sec.clone().into());
            s.set_title(title.into());
            s.set_intro(intro.into());
            s.set_note(note.into());
            s.set_listening(!st.listening.is_empty());
            // audit (9 Nov): Echo's hotkeys_paused - while "Press keys…" waits, the global save keys don't fire
            // (pressing your current save key to re-pick it saved a clip). Only on a change: the hook is shared.
            thread_local!(static PAUSED: Cell<bool> = const { Cell::new(false) });
            let want = !st.listening.is_empty();
            if PAUSED.with(|p| p.replace(want)) != want {
                rec_core::hotkeys_paused(want);
            }
            s.set_hero(st.sec == "quality" && st.query.trim().is_empty());
            let (big, sub, qmin, qclip, lbl) = st.hero();
            s.set_qbig(big.into());
            s.set_qsub(sub.into());
            s.set_qmin(qmin.into());
            s.set_qclip(qclip.into());
            s.set_qcliplbl(lbl.into());
            s.set_themes(settings::theme_cards(&st.s("theme"), &st.s("theme_accent"), &st.s("theme_bg"), &st.s("theme_hi")));
            settings::apply(&m, &mut inner.borrow_mut(), boxes);
            setup::refresh(&a, &st);
            // the header pills follow the hotkeys
            a.set_hotkeys(ModelRc::new(VecModel::from(hotkey_pills(Some(&serde_json::Value::Object(st.v.clone()))))));
            apply_theme(&a, &st);
        })
    };
    hdr_home::wire(&app, srefresh.clone()); // gaps (10 Nov): KDE keys re-read, "On this day" buttons
    {
        let (store, r) = (store.clone(), srefresh.clone());
        app.global::<S>().on_pick(move |id| {
            {
                let mut st = store.borrow_mut();
                st.sec = id.to_string();
                st.listening.clear();
            }
            r();
            if id == "cloud" || id == "profile" {
                UI.with(|u| {
                    if let Some(a) = u.borrow().as_ref().and_then(|w| w.upgrade()) {
                        cloud::load_info(&a); // Chat B: space per cloud
                    }
                });
            }
            if id == "profile" {
                UI.with(|u| {
                    if let Some(a) = u.borrow().as_ref().and_then(|w| w.upgrade()) {
                        lib_profile::open(&a); // Chat B (night): load the profile
                    }
                });
            }
        });
    }
    {
        let (store, r) = (store.clone(), srefresh.clone());
        app.global::<S>().on_search(move |q| {
            store.borrow_mut().query = q.to_string();
            r();
        });
    }
    {
        let (w, store, r) = (app.as_weak(), store.clone(), srefresh.clone());
        app.global::<S>().on_set(move |k, v| {
            // Chat B (night): the background effect lives in native_ui.json, not settings.json
            // (r2-perf: "Send anonymous performance numbers" too - perf.rs)
            if lib_fx::set(&k, &v) || perf::set(&k, &v) || uiscale::set(&k, &v) {
                r();
                return;
            }
            if w.upgrade().is_some_and(|a| cloud::own_pick(&a, &k, &v)) {
                return; // r2-texts L42: the "Own server…" menu pick, not a setting
            }
            let msg = store.borrow_mut().set(&k, &v);
            r();
            if let Some(a) = w.upgrade() {
                if msg.is_none() {
                    lib_set::after_set(&a, &store, &k);
                }
                if let Some(m) = msg {
                    toast(&a, m);
                }
            }
        });
    }
    {
        let (w, store, r) = (app.as_weak(), store.clone(), srefresh.clone());
        app.global::<S>().on_listen(move |id| {
            // gaps (10 Nov): KDE decides the keys (Linux portal) - say where to change them instead
            if w.upgrade().is_some_and(|a| hdr_home::kde_blocks(&a, &store.borrow(), &id)) {
                return;
            }
            {
                let mut st = store.borrow_mut();
                st.listening = if st.listening == id.as_str() { String::new() } else { id.to_string() };
            }
            r();
        });
    }
    {
        let (store, r) = (store.clone(), srefresh.clone());
        app.global::<S>().on_cancel_listen(move || {
            store.borrow_mut().listening.clear();
            r();
        });
    }
    {
        let (w, store, r) = (app.as_weak(), store.clone(), srefresh.clone());
        app.global::<S>().on_key(move |text, ctrl, alt, shift, meta| {
            let single = store.borrow().listening == "ptt";
            let Some(c) = settings::combo(&text, ctrl, alt, shift, meta, single) else { return };
            let msg = store.borrow_mut().key(&c);
            r();
            if let (Some(m), Some(a)) = (msg, w.upgrade()) {
                toast(&a, m);
            }
        });
    }
    {
        let (w, store, r) = (app.as_weak(), store.clone(), srefresh.clone());
        app.global::<S>().on_act(move |act| {
            let Some(a) = w.upgrade() else { return };
            let act = act.as_str();
            // Chat B: real Settings actions (pickers, clean up, sounds, feedback, log, ...)
            {
                let rr: Rc<dyn Fn()> = r.clone();
                if lib_set::act(&a, &store, &rr, act) {
                    return;
                }
            }
            // Chat D: Import is real (lib_import.rs)
            if act == "import" || act.starts_with("import:") {
                let clips = std::path::PathBuf::from(store.borrow().s("clips_dir"));
                if let Some(k) = act.strip_prefix("import:").and_then(|k| k.parse::<usize>().ok()) {
                    if let Some(src) = import::sources(&clips).get(k) {
                        lib_import::start(&a, src.path.clone(), clips);
                    }
                } else {
                    ask(&a, "Import old clips", "Paste the folder of the other app (Medal, SteelSeries Moments, Outplayed…) or any folder with videos. Sub-folders become game folders.", &[("Cancel", "", ""), ("Import", "ok", "primary")], Some("D:\\Medal\\Clips"), "ok", move |a, v, text| {
                        let t = text.trim().trim_matches('"').to_string();
                        if v == "ok" && !t.is_empty() {
                            lib_import::start(a, std::path::PathBuf::from(t), clips);
                        }
                    });
                }
                return;
            }
            // audit (9 Nov): Settings > Games Remove = Echo's two choices + Echo's own forget_game (games.json, its icon,
            // the per-game rule; "never" also adds it to not_games) - the native version only wrote not_games
            if let Some(g) = act.strip_prefix("gameremove:") {
                let g = g.to_string();
                let (store, r) = (store.clone(), r.clone());
                ask(
                    &a,
                    &format!("Remove {g}?"),
                    "Remove = off the list (it comes back if Echo sees it as a game again). Not a game = never detected again.",
                    &[("Cancel", "", ""), ("Remove", "list", ""), ("Not a game", "never", "primary")],
                    None,
                    "",
                    move |a, v, _| {
                        if v.is_empty() {
                            return;
                        }
                        let never = v == "never";
                        if let Some(rc) = rec_core::rec() {
                            rc.forget_game(&g, never);
                        }
                        {
                            let mut st = store.borrow_mut();
                            st.reload();
                            st.games.retain(|n| !n.eq_ignore_ascii_case(&g));
                        }
                        r();
                        toast(a, if never { format!("{g} won't be treated as a game anymore") } else { format!("Removed {g} (it comes back if Echo sees it as a game again)") });
                    },
                );
                return;
            }
            let mut msg: Option<String> = None;
            {
                let mut st = store.borrow_mut();
                if act == "hkadd" {
                    msg = st.set("hk_add", "");
                } else if let Some(i) = act.strip_prefix("hkdel:") {
                    msg = st.set(&format!("hk_del:{i}"), "");
                } else if let Some(id) = act.strip_prefix("hkoff:") {
                    let k = match id {
                        "shot" => "screenshot_key",
                        "mark" => "bookmark_key",
                        "long" => "session_key",
                        "mute" => "mic_mute_key",
                        "show" => "show_key",
                        _ => "ptt_key",
                    };
                    msg = st.set(k, "");
                } else if act == "padoff" {
                    msg = st.set("pad_buttons", "0");
                } else if let Some(g) = act.strip_prefix("gameremove:") {
                    // Chat D: real - Echo's not_games
                    msg = st.set("not_game", g).or_else(|| Some(format!("\"{g}\" isn't treated as a game any more")));
                } else if act == "openclips" {
                    let d = st.s("clips_dir");
                    #[cfg(windows)]
                    let _ = std::process::Command::new("explorer").arg(&d).spawn();
                    #[cfg(not(windows))]
                    let _ = crate::bgwork::spawn_reaped(std::process::Command::new("xdg-open").arg(&d)); // r2-threads (L52): reaped
                } else if act == "discord" {
                    open_url("https://discord.gg/JPVewEfUQs");
                } else if act == "changelog" {
                    // the page (/changes is the JSON the update card reads - raw data was seen)
                    open_url("https://echoclip.link/changelog");
                } else {
                    let what = match act {
                        a if a.starts_with("gameremove:") => "Removing a game".to_string(),
                        a if a.starts_with("cloud:") => "Adding a cloud".to_string(),
                        a if a.starts_with("sound:") => "Sound previews".to_string(),
                        a if a.starts_with("import") => "Importing".to_string(),
                        "padset" => "Setting a controller combo".to_string(),
                        _ => "This button".to_string(),
                    };
                    unhandled(&what);
                }
            }
            r();
            if let Some(m) = msg {
                toast(&a, m);
            }
        });
    }
    lib_set::wire(&app, store.clone());
    // (night 9 Nov: setup::wire + cloud::wire were called twice here after a merge - every cloud timer ran double)
    {
        let rr: Rc<dyn Fn()> = srefresh.clone();
        setup::wire(&app, store.clone(), rr.clone());
        cloud::wire(&app, rr.clone());
        cloud_live::wire(&app); // gaps-cloud (rows 6, 24)
        lib_color::wire(&app, store.clone(), rr);
        lib_fx::wire(&app);
        lib_profile::wire(&app);
        game_sync::start(); // cloud-games: the account's game icons
        clip_thumbs::start(); // clip-thumbs: card pictures of clips in Echo Cloud
    }
    lib_set::wire(&app, store.clone());
    // Settings > Clips & Storage open: other apps' clip counts follow their folders (Chat B, chat 5)
    let import_timer = slint::Timer::default();
    import::set_refresh(srefresh.clone()); // r2-threads (L28): the scan runs on a worker, the page follows
    {
        let (w, store, r) = (app.as_weak(), store.clone(), srefresh.clone());
        import_timer.start(slint::TimerMode::Repeated, Duration::from_secs(3), move || {
            let Some(a) = w.upgrade() else { return };
            let dir = {
                let st = store.borrow();
                if a.get_view() != "settings" || st.sec != "storage" || !a.window().is_visible() {
                    return;
                }
                std::path::PathBuf::from(st.s("clips_dir"))
            };
            if import::changed(&dir) {
                r();
            }
        });
    }
    {
        let (w, sref) = (app.as_weak(), srefresh.clone());
        app.on_page(move |p| {
            if let Some(a) = w.upgrade() {
                if p == "Tidy up" {
                    if let Some(lib) = LIB.with(|l| l.borrow().clone()) {
                        tidy::open(&a, &lib);
                        a.invoke_focus_tidy();
                    }
                    return;
                }
                if p == "Home" {
                    a.set_view("home".into());
                    if let Some(lib) = LIB.with(|l| l.borrow().clone()) {
                        home_fill(&a, &lib);
                    }
                    a.invoke_focus_library();
                    return;
                }
                if p == "Settings" {
                    a.set_view("settings".into());
                    sref();
                    a.invoke_focus_settings();
                    return;
                }
                toast(&a, match p.as_str() {
                    "Record" | "Save clip" => format!("{p} isn't available right now"),
                    _ => { unhandled(&format!("page {p}")); return; }
                });
            }
        });
    }
    {
        let dir = clips_dir.clone();
        app.on_open_folder(move || {
            #[cfg(windows)]
            let _ = std::process::Command::new("explorer").arg(&dir).spawn();
            #[cfg(not(windows))]
            let _ = crate::bgwork::spawn_reaped(std::process::Command::new("xdg-open").arg(&dir)); // r2-threads (L52): reaped
        });
    }

    // card callbacks (global Lib)
    {
        let (lib, sync) = (lib.clone(), sync.clone());
        app.global::<Lib>().on_pick(move |row| {
            lib.toggle_pick(row as usize);
            sync(false);
        });
    }
    let toggle_fav = {
        let (w, lib, sync) = (app.as_weak(), lib.clone(), sync.clone());
        Rc::new(move |idx: usize| -> bool {
            let Some(a) = w.upgrade() else { return false };
            let name = lib.clip(idx).name.clone();
            let on = !lib.favs.borrow().contains(&name);
            match lib.set_favs(&[name], on) {
                Ok(()) => toast(&a, if on { "Added to favourites" } else { "Removed from favourites" }),
                Err(e) => toast(&a, format!("Couldn't save favourites: {e}")),
            }
            lib.changed(idx);
            sync(true);
            on
        })
    };
    {
        let (lib, tf) = (lib.clone(), toggle_fav.clone());
        app.global::<Lib>().on_fav(move |row| {
            if let Some(i) = lib.index_at(row as usize) {
                tf(i);
            }
        });
    }
    {
        let (w, lib) = (app.as_weak(), lib.clone());
        app.global::<Lib>().on_act(move |row, what| {
            let (Some(a), Some(c)) = (w.upgrade(), lib.clip_at(row as usize)) else { return };
            if row as usize >= library::HOME_BASE && what.as_str() != "fav" {
                home_inbox::touched(&lib); // 11 Nov 2026: the buttons on Home's picture count as looking at it
            }
            if cloud::card_needs_local(&a, row, what.as_str()) {
                return; // gaps-cloud (row 4): a cloud-only clip is downloaded first, then this runs again
            }
            hover_vid::stop(&a); // the preview keeps the file open (rename / move / delete would fail)
            if let Some(k) = what.as_str().strip_prefix("copy") {
                copy::copy_clip_ui(&a, c.path.clone(), c.thumbs.clone(), k.trim_start_matches(':')); // Chat C
                return;
            }
            match what.as_str() {
                "folder" => show_in_folder(&c.path),
                "rename" => a.global::<Lib>().set_renaming(row), // Chat D: inline rename (lib_cards.rs)
                "move" => unhandled("card move"),
                "smaller" | "smaller3" => {
                    if let Some(i) = lib.index_at(row as usize) {
                        lib_set::smaller(&a, i, if what.as_str() == "smaller3" { 0.33 } else { 0.5 }); // Chat B
                    }
                }
                "delete" => {
                    let _ = &c;
                    if let Some(i) = lib.index_at(row as usize) {
                        delete_clips_ui(&a, vec![i]);
                    }
                }
                "edit" => {
                    if let Some(i) = lib.index_at(row as usize) {
                        open_editor(i);
                    }
                }
                "cloud" | "link" => {
                    // Chat B: the cloud menu / L = copy the share link (cloud.rs)
                    if let Some(i) = lib.index_at(row as usize) {
                        // gaps-cloud (row 43): L opens the cloud menu (link + expiry chips) like Echo, not a link at once
                        cloud::menu(&a, i, false);
                    }
                }
                w if w.starts_with("copy") => unhandled(w),
                _ => {}
            }
        });
    }

    // ---------- seek-picture sheets (hover flipbook + player seek preview) ----------
    let sheet_tx = sheets::spawn(|d| {
        let _ = slint::invoke_from_event_loop(move || {
            let Some(a) = UI.with(|u| u.borrow().as_ref().and_then(|w| w.upgrade())) else { return };
            SHEETS.with(|s| {
                let mut s = s.borrow_mut();
                let Some(px) = d.px else {
                    eprintln!("sheet {}: {}", d.key, d.note);
                    s.failed.push(d.key);
                    return;
                };
                // "sheet: reused <stem>.scrub.jpg in 85 ms" / "made …" (ffmpeg ran) - in proto.log too
                log_to(&mut Vec::new(), format!("sheet: {}", d.note));
                if s.player == Some(d.key) {
                    s.player_px = Some((d.key, px.clone(), d.tw, d.th));
                }
                let img = match d.rgba {
                    Some(b) => slint::Image::from_rgba8(b),
                    None => slint::Image::from_rgb8(px),
                };
                s.items.retain(|x| x.0 != d.key);
                s.items.push((d.key, img.clone(), d.tw as i32, d.th as i32));
                // keep the hovered card's and the player's sheet only
                let keep: Vec<usize> = [s.hover.map(|h| h.0), s.player].into_iter().flatten().collect();
                while s.items.len() > 2 {
                    match s.items.iter().position(|x| !keep.contains(&x.0)) {
                        Some(p) => {
                            s.items.remove(p);
                        }
                        None => {
                            s.items.remove(0);
                        }
                    }
                }
                if let Some((k, row)) = s.hover {
                    if k == d.key {
                        let f = a.global::<Flip>();
                        f.set_sheet(img.clone());
                        f.set_tw(d.tw as i32);
                        f.set_th(d.th as i32);
                        f.set_row(row);
                    }
                }
                if s.player == Some(d.key) {
                    sheet_tiles::set(&a.global::<Play>(), img, d.tw as i32, d.th as i32);
                }
            });
        });
    });
    let ask_sheet = {
        let (lib, tx) = (lib.clone(), sheet_tx.clone());
        Rc::new(move |idx: usize| -> Option<(slint::Image, i32, i32)> {
            let hit = SHEETS.with(|s| {
                let s = s.borrow();
                if s.failed.contains(&idx) {
                    return Some(None);
                }
                s.items.iter().find(|x| x.0 == idx).map(|x| Some((x.1.clone(), x.2, x.3)))
            });
            match hit {
                Some(r) => {
                    // 11 Nov 2026: fullscreen on Windows (osd.rs) draws the filmstrip from the player's PIXELS
                    // (player_px), which are only kept when a sheet arrives for the open player. A sheet already in
                    // memory as a picture (e.g. from hovering the card first) left them empty -> an empty grey
                    // filmstrip in fullscreen. Read it again for the player (the .scrub.jpg on disk, ~0.1 s).
                    let need_px = r.is_some()
                        && SHEETS.with(|s| {
                            let s = s.borrow();
                            s.player == Some(idx) && !s.player_px.as_ref().is_some_and(|x| x.0 == idx)
                        });
                    if need_px {
                        let c = lib.clip(idx);
                        if cloud::play_ready(&c.path, || {}) {
                            let _ = tx.send(sheets::Job { key: idx, clip: cloud::play_path(&c.path), echo_sheet: c.scrub(), dur: c.dur.unwrap_or(0.0) });
                        }
                    }
                    r
                }
                None => {
                    let c = lib.clip(idx);
                    // gaps-cloud (row 5): a cloud-only clip's seek pictures come from its stream - for the player only
                    if cloud::cloud_only(&c) && SHEETS.with(|s| s.borrow().player) != Some(idx) {
                        return None;
                    }
                    if !cloud::play_ready(&c.path, || {}) {
                        return None; // r2-threads (L6): its stream address is being worked out (worker)
                    }
                    let _ = tx.send(sheets::Job { key: idx, clip: cloud::play_path(&c.path), echo_sheet: c.scrub(), dur: c.dur.unwrap_or(0.0) });
                    None
                }
            }
        })
    };
    let hover_timer = Rc::new(slint::Timer::default());
    let flip_stop = Rc::new(slint::Timer::default());
    {
        let (w, lib, ask, ht, fs) = (app.as_weak(), lib.clone(), ask_sheet.clone(), hover_timer.clone(), flip_stop.clone());
        app.global::<Lib>().on_hover(move |row, on| {
            let Some(a) = w.upgrade() else { return };
            let Some(idx) = lib.index_at(row as usize) else { return };
            if !on {
                let mine = SHEETS.with(|s| {
                    let mut s = s.borrow_mut();
                    let m = s.hover.map(|h| h.1) == Some(row);
                    if m {
                        s.hover = None;
                    }
                    m
                });
                if mine {
                    hover_vid::stop(&a);
                }
                return;
            }
            SHEETS.with(|s| s.borrow_mut().hover = Some((idx, row)));
            // like Echo: start after a short rest on the card, stop after 20 s
            let (w2, ask2, fs2, lib2) = (w.clone(), ask.clone(), fs.clone(), lib.clone());
            ht.start(slint::TimerMode::SingleShot, Duration::from_millis(450), move || {
                let Some(a) = w2.upgrade() else { return };
                if SHEETS.with(|s| s.borrow().hover) != Some((idx, row)) || !WIN_FOCUSED.with(|f| f.get()) {
                    return;
                }
                // the seek-bar pictures of a clip that's likely to be opened next (Echo: "filmstrip" on hover)
                let _ = ask2(idx);
                // 9 Nov: the real clip, like Echo on Windows (was the 60-picture flipbook)
                let c = lib2.clip(idx);
                if c.cloud.is_none() {
                    hover_vid::start(&a, row, c.path.clone(), hover_vid::size_for(&a, row));
                }
                let w3 = w2.clone();
                fs2.start(slint::TimerMode::SingleShot, Duration::from_secs(20), move || {
                    if let Some(a) = w3.upgrade() {
                        hover_vid::stop(&a);
                    }
                });
            });
        });
    }

    // ---------- player ----------
    let want = std::env::var("ECHO_VIDEO").unwrap_or_default();
    let mode = match want.as_str() {
        "texture" => Mode::Texture,
        "software" => Mode::Software,
        "window" if cfg!(windows) => Mode::Window,
        _ if cfg!(windows) => Mode::Window,
        _ => Mode::Texture,
    };
    let hwdec = std::env::var("ECHO_HWDEC").unwrap_or_else(|_| "auto-safe".into());
    let player = Rc::new(RefCell::new(Player {
        mode,
        hwdec,
        #[cfg(windows)]
        vwin: None,
        #[cfg(windows)]
        poster: None,
        poster_px: None,
        mpv: None,
        mpv_error: "starts with the first clip".into(),
        gpu: Gpu::default(),
        open: false,
        clip: None,
        row: 0,
        pending: None,
        loaded_at: None,
        vo_checked: false,
        first_frame: false,
        swap: false,
        fail: None,
        switch_to: None,
        base_mode: mode,
        start_paused: false,
        osd: osd::Osd::new(),
        kill: false,
        closed_at: None,
        frames: 0,
        fps_at: Instant::now(),
        fps: 0.0,
        render_ms_max: 0.0,
        duration: 0.0,
        info_done: false,
        log: vec![],
        note_dirty: None,
    }));

    let save_note = {
        let (w, lib, sync) = (app.as_weak(), lib.clone(), sync.clone());
        Rc::new(move |p: &mut Player| {
            if let Some((idx, text)) = p.note_dirty.take() {
                let m = lib.meta_of(&lib.clip(idx).name);
                if m.note.trim() != text.trim() {
                    if let Err(e) = lib.set_meta(&lib.clip(idx).name, m.tags, text) {
                        if let Some(a) = w.upgrade() {
                            toast(&a, format!("Couldn't save the note: {e}"));
                        }
                    }
                    lib.changed(idx);
                    sync(false);
                }
            }
        })
    };

    let play = {
        let (w, p, lib, ask, sn) = (app.as_weak(), player.clone(), lib.clone(), ask_sheet.clone(), save_note.clone());
        Rc::new(move |row: i32| {
            let Some(a) = w.upgrade() else { return };
            let Some(idx) = lib.index_at(row as usize) else { return };
            // a Home card: play it at its spot in the library view (prev / next walk the view)
            let row = lib.view_row_of(idx).map(|r| r as i32).unwrap_or(row);
            let c = lib.clip(idx);
            let quiet = HOME_QUIET.with(|q| q.get());
            if !quiet {
                cleanup::opened(&lib.thumbs_dir, &c.name); // cleanup: "Not watched in 90+ days" needs the last play
            }
            let mut p = p.borrow_mut();
            sn(&mut p);
            p.open = true;
            p.clip = Some(idx);
            p.row = row as usize;
            p.pending = Some((c.path.clone(), 0.0));
            p.closed_at = None;
            p.kill = false;
            p.duration = c.dur.unwrap_or(0.0);
            p.render_ms_max = 0.0;
            p.info_done = false;
            let pl = a.global::<Play>();
            pl.set_title(c.shown.as_str().into());
            // 12 Nov 2026: in Home's picture its sharp still (the same frame mpv waits on), not the small card picture
            let sharp = match HERO.with(|x| x.borrow().clone()) {
                (hp, Some(img)) if pl.get_inline() && hp == c.path => Some(img),
                _ => None,
            };
            let thumb = sharp.clone().or_else(|| lib.thumb_img(idx));
            p.poster_px = thumb.as_ref().and_then(|i| i.to_rgb8());
            pl.set_poster(if sharp.is_some() || std::env::var("ECHO_POSTER").is_ok() { thumb.unwrap_or_default() } else { slint::Image::default() });
            pl.set_poster_on(true);
            pl.set_snap(true);
            pl.set_progress(0.0);
            pl.set_paused(false);
            pl.set_duration(p.duration as f32);
            pl.set_time_text("0:00".into());
            pl.set_dur_text(library::fmt_dur(p.duration).into());
            pl.set_stats("Opening…".into());
            pl.set_fav(lib.favs.borrow().contains(&c.name));
            let m = lib.meta_of(&c.name);
            pl.set_tags(ModelRc::new(VecModel::from(m.tags.iter().map(|t| SharedString::from(t.as_str())).collect::<Vec<_>>())));
            pl.set_note(m.note.into());
            // Chat A: bookmarks + zoom back to 1x for every clip
            pl.set_marks(ModelRc::new(VecModel::from(pl_zoom::marks(&c.path, &c.thumbs))));
            pl.set_zoomed(false);
            pl_zoom::reset(p.mpv.as_ref());
            pl.set_info(info_text(&c, None).into());
            pl.set_info_chips(info_chips(&c, None));
            pl.set_size_text(library::fmt_size(c.size).into());
            // a new clip: no trim selection, no cuts / highlight
            pl.set_trimming(false);
            pl.set_t0(0.0);
            pl.set_t1(1.0);
            pl_cuts::reset(&pl);
            pl.set_busy(false);
            pl.set_has_prev(row > 0);
            pl.set_has_next((row as usize) + 1 < lib.view_len());
            pl.set_sheet_ok(false);
            SHEETS.with(|s| s.borrow_mut().player = Some(idx));
            // the same clip again, its pictures still in memory: show them now, no 400 ms wait (player_px = the same
            // pictures for the fullscreen controls mpv draws on Windows - only when they're this clip's too)
            let again = SHEETS.with(|s| {
                let s = s.borrow();
                s.player_px.as_ref().filter(|x| x.0 == idx).and_then(|_| s.items.iter().find(|x| x.0 == idx).map(|x| (x.1.clone(), x.2, x.3)))
            });
            if let Some((img, tw, th)) = again {
                eprintln!("sheet: reused from memory");
                sheet_tiles::set(&pl, img, tw, th);
            }
            a.set_player_open(true);
            // also inside Home's picture: the player's keys (S cuts, T / I / O trim, J / L, K, ...); ← / → there
            // are Home's other clips (player.slint). Only loaded there, paused: Home keeps its keys.
            if quiet {
                a.invoke_focus_library();
            } else {
                a.invoke_focus_player();
            }
            a.window().request_redraw(); // mpv starts / loads the file in the render hook
            // extras AFTER the video starts: opening stays instant
            let (w2, ask2) = (w.clone(), ask.clone());
            // (gaps-cloud: a cloud clip after 6 s - its seek pictures read the stream too, the video goes first)
            let soon = if c.cloud.is_some() { 6000 } else if a.global::<Play>().get_inline() { 0 } else { 400 }; // Home: at once
            slint::Timer::single_shot(Duration::from_millis(soon), move || {
                let Some(a) = w2.upgrade() else { return };
                if SHEETS.with(|s| s.borrow().player) != Some(idx) {
                    return;
                }
                if let Some((img, tw, th)) = ask2(idx) {
                    sheet_tiles::set(&a.global::<Play>(), img, tw, th);
                }
            });
        })
    };
    // 12 Nov 2026 (William: "diff between playing and not playing, i want both the same" + "the player opened one is
    // significantly sharper, just the real freeze frame"): Home's clip sits LOADED and PAUSED in Home's picture, at the
    // moment Home's still shows - video + filmstrip + bar, the real frame at full size; play starts it right there,
    // nothing moves. `start` = play it now (Home's play), else only load it paused.
    let home_load: Rc<dyn Fn(&AppWindow, bool)> = {
        let (p, lib, play) = (player.clone(), lib.clone(), play.clone());
        Rc::new(move |a: &AppWindow, start: bool| {
            let Some(idx) = lib.index_at(library::HOME_BASE) else { return };
            let c = lib.clip(idx);
            HOME_QUIET.with(|q| q.set(!start));
            a.global::<Play>().set_inline(true);
            a.global::<Play>().set_inline_active(start);
            play(library::HOME_BASE as i32);
            HOME_QUIET.with(|q| q.set(false));
            if !start {
                // 12 Nov 2026 (William: back on Home it showed 0:00 but kept the old spot): always a fresh load from the
                // start - mpv itself is there; the time and the playhead only come from mpv (play() starts them at 0)
                let mut pb = p.borrow_mut();
                pb.pending = Some((c.path.clone(), home_still_at(c.dur.unwrap_or(0.0))));
                pb.start_paused = true;
                a.global::<Play>().set_paused(true);
            }
        })
    };
    // 12 Nov 2026 (William): Home's player in FULLSCREEN, ← at the very start / → at the very end = Home's previous / next
    // clip (the same as Home's ‹ ›), loaded in the same player: it stays fullscreen, starts at 0:00, playing or paused as
    // before. Nothing at the first / last clip.
    let home_edge: Rc<dyn Fn(&AppWindow, bool)> = {
        let (p, lib, play) = (player.clone(), lib.clone(), play.clone());
        Rc::new(move |a: &AppWindow, forward: bool| {
            let hs = a.global::<HomeState>();
            if (forward && !hs.get_can_next()) || (!forward && !hs.get_can_prev()) {
                return;
            }
            let ps = a.global::<Play>();
            let was_paused = ps.get_paused();
            if home_inbox::act(a, &lib, if forward { "next" } else { "prev" }) {
                home_fill(a, &lib);
            }
            let Some(idx) = lib.index_at(library::HOME_BASE) else { return };
            let path = lib.clip(idx).path.clone();
            ps.set_inline(true);
            ps.set_inline_active(true);
            play(library::HOME_BASE as i32);
            if was_paused {
                let mut pb = p.borrow_mut();
                pb.pending = Some((path, 0.0));
                pb.start_paused = true;
                ps.set_paused(true);
            }
        })
    };
    // ... loaded while Home shows a clip (a local file) in a visible window, let go when Home is left / hidden, a
    // dialog or the editor opens; a new clip on show (pager, a new recording) is loaded instead unless it's playing
    let home_tick: Rc<dyn Fn(&AppWindow)> = {
        let (p, lib, hl) = (player.clone(), lib.clone(), home_load.clone());
        Rc::new(move |a: &AppWindow| {
            let a = a.clone_strong();
            let ps = a.global::<Play>();
            let ed = a.global::<Ed>();
            let idx = lib.index_at(library::HOME_BASE);
            let minimized = {
                use slint::winit_030::WinitWindowAccessor;
                a.window().with_winit_window(|w| w.is_minimized() == Some(true)).unwrap_or(false)
            };
            let want = a.get_view() == "home"
                && a.window().is_visible()
                && !minimized // minimized: let mpv go (nothing drawn, nothing kept)
                && !ed.get_open()
                && !ed.get_opening()
                && !a.global::<Dlg>().get_open()
                && idx.is_some_and(|i| lib.clip(i).path.is_file());
            let (open, clip) = {
                let pb = p.borrow();
                (pb.open, pb.clip)
            };
            if !open {
                if want {
                    hl(&a, false);
                }
            } else if ps.get_inline() {
                if !want {
                    if !ps.get_full() {
                        ps.invoke_close();
                    }
                } else if clip != idx && !ps.get_inline_active() {
                    hl(&a, false);
                }
            }
        })
    };
    {
        let (w, tick) = (app.as_weak(), home_tick.clone());
        let t = Box::leak(Box::new(slint::Timer::default()));
        t.start(slint::TimerMode::Repeated, Duration::from_millis(400), move || {
            if let Some(a) = w.upgrade() {
                tick(&a);
            }
        });
    }
    // gaps-player: Save copy / Replace open a clip like a fresh one (Replace: its old seek pictures are dropped first)
    {
        let (lib, play) = (lib.clone(), play.clone());
        pl_actions::set_reopen(Rc::new(move |idx| {
            SHEETS.with(|s| {
                let mut s = s.borrow_mut();
                s.items.retain(|x| x.0 != idx);
                s.failed.retain(|&i| i != idx);
                if s.player_px.as_ref().is_some_and(|x| x.0 == idx) {
                    s.player_px = None;
                }
            });
            match lib.view_row_of(idx) {
                Some(r) => {
                    play(r as i32);
                    true
                }
                None => false,
            }
        }));
    }
    cloud_live::set_play(play.clone()); // gaps-cloud (row 5): play the downloaded copy after a stream fallback
    {
        let (lib, play, p) = (lib.clone(), play.clone(), player.clone());
        let sync = sync.clone();
        app.global::<Lib>().on_open(move |row, _| {
            if !lib.picked.borrow().is_empty() {
                lib.toggle_pick(row as usize);
                sync(false);
                return;
            }
            let _ = &p;
            // the player takes over: no second decoder on the same clip
            UI.with(|u| u.borrow().as_ref().and_then(|w| w.upgrade())).inspect(hover_vid::stop);
            // 11 Nov 2026: a clip played anywhere (Home, Library) is not new on Home any more (QA B36)
            if let Some(i) = lib.index_at(row as usize) {
                home_inbox::seen_idx(&lib, i);
                if let Some(a) = UI.with(|u| u.borrow().as_ref().and_then(|w| w.upgrade())) {
                    home_fill(&a, &lib);
                }
            }
            if let Some(a) = UI.with(|u| u.borrow().as_ref().and_then(|w| w.upgrade())) {
                // 11 Nov 2026: Home's big picture plays right there; any other card opens the normal player
                a.global::<Play>().set_inline(row as usize == library::HOME_BASE && a.get_view() == "home");
            }
            play(row);
        });
    }

    // ---------- Home's buttons ----------
    {
        let (w, lib, tf, hl, pp, tick) = (app.as_weak(), lib.clone(), toggle_fav.clone(), home_load.clone(), player.clone(), home_tick.clone());
        app.global::<HomeState>().on_act(move |what| {
            let Some(a) = w.upgrade() else { return };
            // 11 Nov 2026 (Home Inbox): sending / editing / playing the clip on show = it isn't new any more
            if matches!(what.as_str(), "play" | "copy" | "copym" | "link" | "edit" | "cap") || what.starts_with("copy:") {
                home_inbox::touched(&lib);
            }
            // 11 Nov 2026: the clip plays inside Home's picture - another clip, Delete, the editor or the library
            // stop it first
            let inline_on = a.global::<Play>().get_inline() && a.get_player_open();
            if inline_on && (matches!(what.as_str(), "prev" | "next" | "keep" | "del" | "library" | "edit" | "cap" | "gs-go") || what.starts_with("shelf:")) {
                a.global::<Play>().invoke_close();
            }
            match what.as_str() {
                "play" => {
                    hover_vid::stop(&a); // the player takes over: no second decoder on the same clip
                    // already loaded (paused) in the picture: just play it there; else load it playing
                    let here = a.get_player_open() && a.global::<Play>().get_inline() && pp.borrow().clip == lib.index_at(library::HOME_BASE);
                    if here {
                        if a.global::<Play>().get_paused() {
                            a.global::<Play>().invoke_toggle_pause();
                        }
                        a.global::<Play>().set_inline_active(true);
                    } else {
                        hl(&a, true);
                    }
                }
                "library" => a.invoke_pick_filter("all".into()),
                // gaps (10 Nov): "copy:<size>" = a size from the ▾ menu (home.slint CopyMenuBody)
                w if w == "copy" || w == "copym" || w.starts_with("copy:") => {
                    if cloud::home_needs_local(&a, w) {
                        return; // gaps-cloud (row 4): download first, then copy
                    }
                    if let Some(c) = lib.index_at(library::HOME_BASE).map(|i| lib.clip(i)) {
                        copy::copy_clip_ui(&a, c.path.clone(), c.thumbs.clone(), w.strip_prefix("copy:").unwrap_or("")); // Chat C
                    }
                }
                "link" => {
                    if let Some(i) = lib.index_at(library::HOME_BASE) {
                        cloud::quick_link(&a, i); // Chat B
                    }
                }
                "edit" | "cap" => {
                    if cloud::home_needs_local(&a, what.as_str()) {
                        return; // gaps-cloud (row 4): download first, then the editor
                    }
                    if let Some(i) = lib.index_at(library::HOME_BASE) {
                        open_editor(i);
                        if what.as_str() == "cap" {
                            edui::act_when_open(&a, "cap"); // gaps-editor: waits for the editor (it may ask first)
                        }
                    }
                }
                "gs-go" => {
                    let (_, next) = gs_next(&lib);
                    match next.map(|i| GS_STEPS[i].0) {
                        Some("edit") => {
                            // a cloud-only clip downloads first, then Home's "edit" opens the editor
                            if cloud::home_needs_local(&a, "edit") {
                                return;
                            }
                            if let Some(i) = lib.index_at(library::HOME_BASE) {
                                open_editor(i);
                            }
                        }
                        // gaps (10 Nov): Echo's step 4 button "Copy for Discord" copies the latest clip (was a link)
                        Some("share") => {
                            if let Some(i) = lib.index_at(library::HOME_BASE) {
                                // gaps-cloud (row 4): a cloud-only clip downloads first, then Home's "copy" runs
                                if !cloud::home_needs_local(&a, "copy") {
                                    let c = lib.clip(i);
                                    copy::copy_clip_ui(&a, c.path.clone(), c.thumbs.clone(), "");
                                    home_fill(&a, &lib); // the step ticks itself off
                                }
                            }
                        }
                        Some("account") | Some("backup") => {
                            a.invoke_page("Settings".into());
                            a.global::<S>().invoke_pick("cloud".into());
                        }
                        _ => {}
                    }
                }
                "gs-skip" => {
                    let (_, next) = gs_next(&lib);
                    if let Some(i) = next {
                        let all: Vec<&str> = GS.with(|g| {
                            let mut g = g.borrow_mut();
                            g.0.insert(GS_STEPS[i].0);
                            g.0.iter().copied().collect()
                        });
                        lib_fx::ui_set("gs_skip", serde_json::json!(all));
                    }
                    home_fill(&a, &lib);
                }
                "gs-hide" => {
                    GS.with(|g| g.borrow_mut().1 = true);
                    lib_fx::ui_set("gs_hide", serde_json::json!(true));
                    home_fill(&a, &lib);
                    toast(&a, "Hidden - bring it back under Settings → App Settings");
                }
                // 11 Nov 2026 (Home Inbox): Keep = star it (if it isn't yet) + the next clip; the pager; Delete; a shelf
                "keep" => {
                    if let Some(i) = home_inbox::current(&lib) {
                        if !lib.favs.borrow().contains(&lib.clip(i).name) {
                            tf(i);
                        }
                    }
                    if home_inbox::act(&a, &lib, "keep") {
                        home_fill(&a, &lib);
                    }
                }
                w if w.starts_with("shelf:") => home_inbox::open_shelf(&a, &w["shelf:".len()..]),
                "show" => {} // Home came on screen: the check below loads its clip
                w => {
                    if home_inbox::act(&a, &lib, w) {
                        home_fill(&a, &lib);
                    }
                }
            }
            // Home's player ready at once after entering Home, the pager, Keep or Delete (not on the next 400 ms check)
            tick(&a);
        });
    }
    // 10 Nov: the status card's hover panel - "Manage cloud storage" = Settings > Cloud + its space manager
    {
        let w = app.as_weak();
        app.on_manage_cloud(move || {
            let Some(a) = w.upgrade() else { return };
            a.invoke_page("Settings".into());
            a.global::<S>().invoke_pick("cloud".into());
            cloud::space_open(&a);
        });
    }
    // sidebar statcard's Echo Profile row (Echo's #acctrow): click = Settings > Cloud (goCloudSettings), x = hide
    {
        let w = app.as_weak();
        app.on_acct_act(move |k| {
            let Some(a) = w.upgrade() else { return };
            if k == "hide" {
                rec_core::set_acct_hidden(true);
                a.set_acct_on(false);
                toast(&a, "Hidden - sign in any time under Settings → Cloud");
            } else {
                a.invoke_page("Settings".into());
                a.global::<S>().invoke_pick("cloud".into());
            }
        });
    }
    // ---------- editor (native chat 5): edui.rs ----------
    {
        let play_dyn: Rc<dyn Fn(i32)> = play.clone();
        edui::setup(&app, player.clone(), play_dyn.clone(), clips_dir.clone());
        focus::setup(&app);
        let (w, p) = (app.as_weak(), player.clone());
        OPEN_EDITOR.with(|o| {
            *o.borrow_mut() = Some(Rc::new(move |idx| {
                if let Some(a) = w.upgrade() {
                    edui::open(&a, &p, idx, &play_dyn, true);
                }
            }))
        });
    }
    let pl = app.global::<Play>();
    {
        let (w, p, sn) = (app.as_weak(), player.clone(), save_note.clone());
        pl.on_close(move || {
            let mut p = p.borrow_mut();
            sn(&mut p);
            let p = &mut *p;
            if let Some(m) = p.mpv.as_ref() {
                p.osd.hide_preview(m);
                m.command(&["stop"]);
            }
            p.open = false;
            p.pending = None;
            p.closed_at = Some(Instant::now());
            SHEETS.with(|s| {
                let mut s = s.borrow_mut();
                // keep the closed clip's pictures too (at most 2 are kept anyway): opening it again shows them
                // at once instead of decoding the sheet again (10 Nov 2026: looked like "made on every open")
                let keep = [s.hover.map(|h| h.0), s.player.take()];
                s.items.retain(|x| keep.contains(&Some(x.0)));
            });
            if let Some(a) = w.upgrade() {
                a.set_player_open(false);
                over::player_closed(&a);
                a.invoke_focus_library();
                a.global::<Play>().set_video_window(false);
                if a.global::<Play>().get_full() {
                    a.global::<Play>().set_full(false);
                    a.window().set_fullscreen(false);
                }
                let pl = a.global::<Play>();
                pl.set_inline(false);
                pl.set_frame(slint::Image::default());
                sheet_tiles::clear(&pl); // sheet + its 60 pictures (their textures go with them)
                a.window().request_redraw(); // lets the render hook free the video texture
            }
        });
    }
    {
        let (w, p) = (app.as_weak(), player.clone());
        pl.on_toggle_pause(move || {
            let p = p.borrow();
            if let Some(m) = p.mpv.as_ref() {
                // Chat A: replay at the end - Space / click on an ended clip starts it again (trim start when trimming)
                if m.get("eof-reached").as_deref() == Some("yes") {
                    if let Some(a) = w.upgrade() {
                        let pl = a.global::<Play>();
                        let d = m.get_f64("duration").unwrap_or(p.duration);
                        let t = if pl.get_trimming() { pl.get_t0() as f64 * d } else { 0.0 };
                        m.command(&["seek", &format!("{t:.3}"), "absolute+exact"]);
                        m.set("pause", "no");
                        pl.set_paused(false);
                        if pl.get_inline() {
                            pl.set_inline_active(true);
                        }
                        pl.set_snap(true);
                        pl.set_progress(if d > 0.0 { (t / d) as f32 } else { 0.0 });
                    }
                    return;
                }
                let paused = m.get("pause").as_deref() == Some("yes");
                m.set("pause", if paused { "no" } else { "yes" });
                if let Some(a) = w.upgrade() {
                    let pl = a.global::<Play>();
                    pl.set_paused(!paused);
                    // 12 Nov 2026: Home's paused clip started in its picture: the player's keys from now on
                    if paused && pl.get_inline() {
                        pl.set_inline_active(true);
                    }
                    // stop gliding at once: show where it really is
                    let pos = m.get_f64("time-pos").unwrap_or(0.0);
                    if p.duration > 0.0 {
                        pl.set_snap(true);
                        pl.set_progress((pos / p.duration).clamp(0.0, 1.0) as f32);
                    }
                }
            }
        });
    }
    // keyframe seeks while dragging on the strip / a trim end: paced + never waiting for mpv (pl_seek.rs)
    let drag_seek = {
        let p = player.clone();
        Rc::new(pl_seek::DragSeek::new(move |t| {
            if let Some(m) = p.borrow().mpv.as_ref() {
                m.command_async(&["seek", &format!("{t:.3}"), "absolute+keyframes"]);
            }
        }))
    };
    {
        let (w, p, ds) = (app.as_weak(), player.clone(), drag_seek.clone());
        pl.on_seek(move |frac, dragging| {
            let p = p.borrow();
            if let Some(m) = p.mpv.as_ref() {
                let d = if p.duration > 0.0 { p.duration } else { m.get_f64("duration").unwrap_or(0.0) };
                // keyframe seeks while dragging (fast), one exact seek on release
                if dragging {
                    ds.drag(frac as f64 * d);
                } else {
                    ds.end();
                    m.command_async(&["seek", &format!("{:.3}", frac as f64 * d), "absolute+exact"]);
                }
            }
            if let Some(a) = w.upgrade() {
                let pl = a.global::<Play>();
                pl.set_snap(true);
                pl.set_progress(frac);
            }
        });
    }
    {
        let (w, p, edge) = (app.as_weak(), player.clone(), home_edge.clone());
        pl.on_seek_rel(move |s| {
            // Home's player in fullscreen: a press at the edge changes clip; one that would cross it lands on it first
            // (so the next press is the one that changes clip - no skipping by accident)
            if let Some(a) = w.upgrade().filter(|a| a.global::<Play>().get_full() && a.global::<Play>().get_inline()) {
                let step = {
                    let pb = p.borrow();
                    pb.mpv.as_ref().map(|m| {
                        let pos = m.get_f64("time-pos").unwrap_or(0.0);
                        let dur = m.get_f64("duration").unwrap_or(pb.duration);
                        let eof = m.get("eof-reached").as_deref() == Some("yes");
                        (pos, dur, eof)
                    })
                };
                if let Some((pos, dur, eof)) = step {
                    if s > 0.0 && (eof || pos >= dur - 0.1) {
                        return edge(&a, true);
                    }
                    if s < 0.0 && pos <= 0.1 {
                        return edge(&a, false);
                    }
                    let to = (pos + s as f64).clamp(0.0, (dur - 0.05).max(0.0));
                    let pb = p.borrow();
                    if let Some(m) = pb.mpv.as_ref() {
                        m.command(&["seek", &format!("{to:.3}"), "absolute+exact"]);
                    }
                    if dur > 0.0 {
                        let pl = a.global::<Play>();
                        pl.set_snap(true);
                        pl.set_progress((to / dur) as f32);
                    }
                    return;
                }
            }
            let p = p.borrow();
            if let Some(m) = p.mpv.as_ref() {
                m.command(&["seek", &format!("{s}"), "relative+exact"]);
                let pos = (m.get_f64("time-pos").unwrap_or(0.0) + s as f64).clamp(0.0, p.duration.max(0.0));
                if let (Some(a), true) = (w.upgrade(), p.duration > 0.0) {
                    let pl = a.global::<Play>();
                    pl.set_snap(true);
                    pl.set_progress((pos / p.duration) as f32);
                }
            }
        });
    }
    // Chat A: wheel zoom + pan on the video (pl_zoom.rs)
    {
        let (w, p) = (app.as_weak(), player.clone());
        pl.on_zoom(move |dir, fx, fy| {
            if let (Some(m), Some(a)) = (p.borrow().mpv.as_ref(), w.upgrade()) {
                a.global::<Play>().set_zoomed(pl_zoom::wheel(m, dir, fx, fy));
            }
        });
        let p = player.clone();
        pl.on_pan(move |dx, dy| {
            if let Some(m) = p.borrow().mpv.as_ref() {
                pl_zoom::pan(m, dx, dy);
            }
        });
    }
    {
        let (p, play) = (player.clone(), play.clone());
        pl.on_prev(move || {
            if let Some(a) = UI.with(|u| u.borrow().as_ref().and_then(|w| w.upgrade())).filter(|a| a.global::<Play>().get_inline()) {
                a.global::<HomeState>().invoke_act("prev".into());
                return;
            }
            let r = p.borrow().row;
            if r > 0 {
                play(r as i32 - 1);
            }
        });
    }
    {
        let (p, play, lib) = (player.clone(), play.clone(), lib.clone());
        pl.on_next(move || {
            if let Some(a) = UI.with(|u| u.borrow().as_ref().and_then(|w| w.upgrade())).filter(|a| a.global::<Play>().get_inline()) {
                a.global::<HomeState>().invoke_act("next".into());
                return;
            }
            let r = p.borrow().row;
            if r + 1 < lib.view_len() {
                play(r as i32 + 1);
            }
        });
    }
    {
        let (w, p, tf) = (app.as_weak(), player.clone(), toggle_fav.clone());
        pl.on_toggle_fav(move || {
            let Some(idx) = p.borrow().clip else { return };
            let on = tf(idx);
            if let Some(a) = w.upgrade() {
                a.global::<Play>().set_fav(on);
            }
        });
    }
    let set_tags = {
        let (w, p, lib, sync) = (app.as_weak(), player.clone(), lib.clone(), sync.clone());
        Rc::new(move |f: &dyn Fn(&mut Vec<String>)| {
            let Some(a) = w.upgrade() else { return };
            let mut p = p.borrow_mut();
            let Some(idx) = p.clip else { return };
            let name = lib.clip(idx).name.clone();
            let mut m = lib.meta_of(&name);
            f(&mut m.tags);
            let note = p.note_dirty.take().map(|d| d.1).unwrap_or(m.note);
            match lib.set_meta(&name, m.tags, note) {
                Ok(()) => {}
                Err(e) => toast(&a, format!("Couldn't save the tags: {e}")),
            }
            let m = lib.meta_of(&name);
            a.global::<Play>().set_tags(ModelRc::new(VecModel::from(m.tags.iter().map(|t| SharedString::from(t.as_str())).collect::<Vec<_>>())));
            lib.changed(idx);
            sync(true);
        })
    };
    {
        let st = set_tags.clone();
        pl.on_add_tag(move |t| {
            let t = t.trim().trim_start_matches('#').trim().to_string();
            if t.is_empty() {
                return;
            }
            st(&|tags: &mut Vec<String>| {
                if !tags.contains(&t) {
                    tags.push(t.clone())
                }
            });
        });
    }
    {
        let st = set_tags.clone();
        pl.on_remove_tag(move |t| {
            let t = t.to_string();
            st(&|tags: &mut Vec<String>| tags.retain(|x| x != &t));
        });
    }
    let note_timer = Rc::new(slint::Timer::default());
    {
        let (p, nt, sn) = (player.clone(), note_timer.clone(), save_note.clone());
        pl.on_note_edited(move |t| {
            let Some(idx) = p.borrow().clip else { return };
            p.borrow_mut().note_dirty = Some((idx, t.to_string()));
            let (p2, sn2) = (p.clone(), sn.clone());
            nt.start(slint::TimerMode::SingleShot, Duration::from_millis(700), move || sn2(&mut p2.borrow_mut()));
        });
    }
    {
        let (w, p, lib) = (app.as_weak(), player.clone(), lib.clone());
        pl.on_act(move |what| {
            let Some(a) = w.upgrade() else { return };
            let Some(idx) = p.borrow().clip else { return };
            // night 9 Nov: the side panel's Title renames the clip (Echo's sideRename): mpv lets go of the file, the
            // rename retries ~4 s (Windows keeps it locked a moment), then the clip plays on from the same spot
            if let Some(name) = what.as_str().strip_prefix("rename:") {
                let c = lib.clip(idx);
                let v = name.split_whitespace().collect::<Vec<_>>().join(" ");
                let pl = a.global::<Play>();
                if v.is_empty() || v == c.name || v == c.shown {
                    pl.set_title(c.shown.as_str().into());
                    return;
                }
                if c.cloud.is_some() {
                    pl.set_title(c.shown.as_str().into());
                    toast(&a, "Download the clip first to rename it");
                    return;
                }
                // 12 Nov 2026: its share link / upload is sending the file right now: renamed once that's done
                let rel = cloud::rel_of(&c);
                if cloud::clip_busy(&rel) {
                    let (what, p, lib) = (what.to_string(), p.clone(), lib.clone());
                    let msg = format!("Renaming \"{}\" once its upload is done - the share link keeps working", c.shown);
                    cloud::when_free(&a, vec![rel], &msg, move |a| {
                        if a.get_player_open() && p.borrow().clip == Some(idx) {
                            a.global::<Play>().invoke_act(what.as_str().into()); // still open: the player's rename
                        } else {
                            let w = a.as_weak();
                            lib.clone().rename_clip_bg(idx, &v, move |res| {
                                let Some(a) = w.upgrade() else { return };
                                match res {
                                    Ok(_) => toast(&a, "Renamed"),
                                    Err(e) => toast(&a, e),
                                }
                                lib.refresh();
                                a.invoke_pick_filter(a.get_filter());
                            });
                        }
                    });
                    return;
                }
                let (at, paused) = match p.borrow().mpv.as_ref() {
                    Some(m) => {
                        let at = m.get_f64("time-pos").unwrap_or(0.0);
                        let paused = m.get("pause").as_deref() == Some("yes");
                        m.command(&["stop"]);
                        (at, paused)
                    }
                    None => (0.0, true),
                };
                let (w, p, lib) = (a.as_weak(), p.clone(), lib.clone());
                // r2-threads (L7): the file rename (and its ~4 s retry while mpv lets go) runs on a worker
                let l2 = lib.clone();
                slint::Timer::single_shot(Duration::from_millis(150), move || l2.rename_clip_bg(idx, &v, move |res| {
                    let Some(a) = w.upgrade() else { return };
                    let c = lib.clip(idx);
                    let pl = a.global::<Play>();
                    pl.set_title(c.shown.as_str().into());
                    match res {
                        Ok(_) => toast(&a, "Renamed"),
                        Err(e) => toast(&a, e),
                    }
                    lib.refresh();
                    a.invoke_pick_filter(a.get_filter());
                    // still this clip in the player: play on from the same spot
                    if a.get_player_open() && p.borrow().clip == Some(idx) {
                        if let Some(m) = p.borrow().mpv.as_ref() {
                            m.command(&["loadfile", &c.path.to_string_lossy(), "replace", "-1", &format!("start={at:.3},pause={}", if paused { "yes" } else { "no" })]);
                        }
                    }
                }));
                return;
            }
            if cloud::player_needs_local(&a, idx, what.as_str()) {
                return; // gaps-cloud (row 4): download first, then this button again
            }
            {
                // Chat C: frame / thumbnail / MP3 / WAV / GIF / Replace + Copy sizes (pl_actions.rs, copy.rs)
                let c = lib.clip(idx);
                if let Some(k) = what.as_str().strip_prefix("copy") {
                    copy::copy_clip_ui(&a, c.path.clone(), c.thumbs.clone(), k.trim_start_matches(':'));
                    return;
                }
                if pl_actions::act(&a, &p, &c.path, &c.thumbs, what.as_str()) {
                    return;
                }
            }
            // 12 Nov 2026: Esc / a click outside the clip playing in Home's picture: back to Home's paused frame
            if what.as_str() == "inline-stop" {
                {
                    let pb = p.borrow();
                    if let Some(m) = pb.mpv.as_ref() {
                        m.set("pause", "yes");
                        m.command(&["seek", &format!("{:.3}", home_still_at(pb.duration)), "absolute+exact"]);
                    }
                }
                let ps = a.global::<Play>();
                ps.set_paused(true);
                ps.set_inline_active(false);
                a.invoke_focus_library();
                return;
            }
            // the bar's SPEED menu (11 Nov 2026)
            if let Some(sp) = what.as_str().strip_prefix("speed:") {
                if let Some(m) = p.borrow().mpv.as_ref() {
                    m.set("speed", sp);
                }
                return;
            }
            match what.as_str() {
                "folder" => show_in_folder(&lib.clip(idx).path),
                "edit" => {
                    // 9 Nov: the editor opens where the player was, with the trim + cuts already made
                    let carry = pl_cuts::take_for_editor(&a, &p);
                    open_editor(idx);
                    if let Some(cv) = carry {
                        edui::carry_in(&a, &p, &lib.clip(idx).path, cv);
                    }
                }
                "delete" => delete_clips_ui(&a, vec![idx]),
                "share" => cloud::menu(&a, idx, true), // Chat B
                "cloud" => cloud::menu(&a, idx, false),
                w if w.starts_with("copy") => unhandled(w),
                _ => {}
            }
        });
    }

    // ---------- quick trim (the yellow ends of the strip), volume, fullscreen ----------
    let last_trim = Rc::new(std::cell::Cell::new((0.0f32, 1.0f32)));
    pl_cuts::wire(&app, player.clone());
    pl_actions::wire_tags(&app, player.clone()); // gaps-player: Tags box suggestions + add on blur
    {
        let (w, p, lt, ds) = (app.as_weak(), player.clone(), last_trim.clone(), drag_seek.clone());
        pl.on_trim_changed(move |dragging| {
            let Some(a) = w.upgrade() else { return };
            let pl = a.global::<Play>();
            let p = p.borrow();
            let Some(m) = p.mpv.as_ref() else { return };
            // p.duration while dragging: reading mpv's property waits for its core (busy with the last seek)
            let d = if p.duration > 0.0 && dragging { p.duration } else { m.get_f64("duration").unwrap_or(p.duration) };
            let (t0, t1) = (pl.get_t0(), pl.get_t1());
            let (l0, l1) = lt.get();
            lt.set((t0, t1));
            if dragging {
                // show the frame under the handle that moves (fast keyframe seek, paced: pl_seek.rs)
                let f = if (t1 - l1).abs() > (t0 - l0).abs() { t1 } else { t0 };
                ds.drag(f as f64 * d);
            } else {
                ds.end();
                // play the selection on a loop (mpv's A-B loop), from its start
                m.set("ab-loop-a", &format!("{:.3}", t0 as f64 * d));
                m.set("ab-loop-b", &format!("{:.3}", t1 as f64 * d));
                let pos = m.get_f64("time-pos").unwrap_or(0.0);
                if pos < t0 as f64 * d || pos > t1 as f64 * d - 0.05 || (t0 - l0).abs() > 0.0 {
                    m.command(&["seek", &format!("{:.3}", t0 as f64 * d), "absolute+exact"]);
                }
            }
        });
    }
    {
        let (w, p, lt) = (app.as_weak(), player.clone(), last_trim.clone());
        pl.on_trim_cancel(move || {
            let Some(a) = w.upgrade() else { return };
            let pl = a.global::<Play>();
            pl.set_trimming(false);
            pl.set_t0(0.0);
            pl.set_t1(1.0);
            pl_cuts::reset(&pl);
            lt.set((0.0, 1.0));
            if let Some(m) = p.borrow().mpv.as_ref() {
                m.set("ab-loop-a", "no");
                m.set("ab-loop-b", "no");
            }
        });
    }
    {
        // Save copy: a NEW file next to the clip ("<name> (trim).mp4") - the original is never touched
        let (w, p, lib, root) = (app.as_weak(), player.clone(), lib.clone(), clips_dir.clone());
        pl.on_trim_save(move || {
            let Some(a) = w.upgrade() else { return };
            let pl = a.global::<Play>();
            if pl.get_busy() || pl.get_gif_busy() {
                return; // L14: Echo's trim.busy (a key could start a 2nd Save copy, or one during a GIF)
            }
            let Some(idx) = p.borrow().clip else { return };
            if cloud::player_needs_local(&a, idx, "trim") {
                return; // gaps-cloud (row 4): a cloud-only clip is downloaded first, then Save copy runs again
            }
            let c = lib.clip(idx);
            let d = p.borrow().duration;
            let (s0, s1) = (pl.get_t0() as f64 * d, pl.get_t1() as f64 * d);
            if d <= 0.0 || s1 - s0 < 0.3 {
                return;
            }
            // cuts made on the strip (pl_cuts): the parts that stay, joined (re-encode; no cuts = plain copy as before)
            let keep = pl_cuts::kept(s0, s1, &pl_cuts::cuts_secs(&pl, d));
            let has_cuts = keep.len() != 1 || (keep[0].0 - s0).abs() > 1e-3 || (keep[0].1 - s1).abs() > 1e-3;
            if keep.is_empty() {
                toast(&a, "Nothing left - the cuts cover the whole selection");
                return;
            }
            let Some(ff) = sheets::ffmpeg() else {
                toast(&a, "Save copy needs ffmpeg (Echo's copy wasn't found)");
                return;
            };
            let ext = c.path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_else(|| "mp4".into());
            let dir = c.path.parent().map(|d| d.to_path_buf()).unwrap_or_else(|| root.clone());
            let base = c.name.clone();
            let mut out = dir.join(format!("{base} (trim).{ext}"));
            let mut k = 2;
            while out.exists() {
                out = dir.join(format!("{base} (trim {k}).{ext}"));
                k += 1;
            }
            // made in .thumbs (a dot folder the library skips), then moved next to the clip: no half-made card
            let tmp = root.join(".thumbs").join(format!("{base}.saving.{ext}"));
            let exact = pl.get_exact();
            pl.set_busy(true);
            // its own progress box in the notes stack (spinner while ffmpeg runs)
            let pb = prog_box(if has_cuts || exact { "Exporting (re-encoding)…" } else { "Exporting…" }); // Echo's texts
            prog_set(pb, None, None, Some(-1.0));
            let (src, w2, root2) = (c.path.clone(), w.clone(), root.clone());
            let tdir = root.join(".thumbs");
            let job = move || -> Result<(PathBuf, PathBuf, Option<f64>), String> {
                // gaps-player (row 35): Exact / cuts re-encode with Echo's chain (AMF -> NVENC -> QSV -> libx264)
                pl_actions::encode_trim(&ff, &src, s0, s1, exact, &keep, has_cuts, d, &tmp)?;
                if let Err(e) = std::fs::rename(&tmp, &out) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(e.to_string());
                }
                // its card picture: <clips>/.thumbs/<name>.jpg like every clip (10 Nov: it went to a temp folder and
                // was lost after a restart = a black card, e.g. "transfer summary"); its length below -> <name>.dur
                let _ = std::fs::create_dir_all(&tdir);
                let thumb = tdir.join(format!("{}.jpg", out.file_stem().unwrap_or_default().to_string_lossy()));
                let mut t = std::process::Command::new(&ff);
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    t.creation_flags(0x0800_0000);
                }
                let _ = t
                    .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", "0.5", "-i"])
                    .arg(&out)
                    .args(["-frames:v", "1", "-vf", "scale=480:-2", "-q:v", "4"])
                    .arg(&thumb)
                    .stdin(std::process::Stdio::null())
                    .output();
                // its real length (a copy without "Exact" starts at the keyframe before the selection)
                let probe = edrender::ffprobe(&ff); // r2-threads (L55): next to ffmpeg (ECHO_FFMPEG too), else PATH
                let mut pc = std::process::Command::new(probe);
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    pc.creation_flags(0x0800_0000);
                }
                let len = pc
                    .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
                    .arg(&out)
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok());
                if let Some(d) = len.filter(|d| *d > 0.0) {
                    let _ = std::fs::write(tdir.join(format!("{}.dur", out.file_stem().unwrap_or_default().to_string_lossy())), d.to_string());
                }
                Ok((out, thumb, len))
            };
            std::thread::spawn(move || {
                let r = job();
                let _ = slint::invoke_from_event_loop(move || {
                    prog_done(pb, 0);
                    let Some(a) = w2.upgrade() else { return };
                    a.global::<Play>().set_busy(false);
                    let Some(lib) = LIB.with(|l| l.borrow().clone()) else { return };
                    match r {
                        Ok((out, thumb, len)) => {
                            let mut new_idx = lib.index_of_path(&out);
                            if new_idx.is_none() {
                                if let Some(mut c) = library::clip_from(&out, &root2) {
                                    c.dur = len.or(Some(s1 - s0));
                                    if thumb.exists() {
                                        c.own_thumb = Some(thumb);
                                    }
                                    new_idx = Some(lib.add(c));
                                    lib.refresh();
                                }
                            }
                            toast(&a, "Saved trimmed clip"); // Echo's text
                            a.invoke_pick_filter(a.get_filter());
                            UI_SYNC.with(|f| {
                                if let Some(f) = f.borrow().as_ref() {
                                    f(true)
                                }
                            });
                            // gaps-player (row 35): Echo opens the NEW clip in the player
                            if let Some(i) = new_idx.filter(|_| a.get_player_open()) {
                                pl_actions::reopen(i);
                            }
                        }
                        Err(e) => toast(&a, format!("Couldn't save the copy: {e}")),
                    }
                });
            });
        });
    }
    {
        let p = player.clone();
        pl.on_set_volume(move |v, muted| {
            if let Some(m) = p.borrow().mpv.as_ref() {
                m.set("volume", &format!("{:.0}", (v * 100.0).clamp(0.0, 100.0)));
                m.set("mute", if muted { "yes" } else { "no" });
            }
        });
    }
    {
        let w = app.as_weak();
        pl.on_toggle_full(move || {
            let Some(a) = w.upgrade() else { return };
            let pl = a.global::<Play>();
            let on = !pl.get_full();
            if std::env::var("ECHO_DEBUG_OSD").is_ok() {
                FULL_AT.with(|c| c.set(Some(Instant::now())));
                let sz = a.window().size();
                osd_dbg(&format!("==== F pressed: {} fullscreen, window {}x{}", if on { "INTO" } else { "OUT OF" }, sz.width, sz.height));
            }
            pl.set_full(on);
            pl.set_ui_on(true);
            {
                use slint::winit_030::WinitWindowAccessor;
                let mon = if on { a.window().with_winit_window(|w| w.current_monitor().map(|m| (m.size().width, m.size().height))).flatten() } else { None };
                FS_ENTER.with(|c| c.set(if on { Some((Instant::now(), mon)) } else { None }));
            }
            a.window().set_fullscreen(on);
        });
    }

    // ---------- render hook: mpv starts, draws, falls back and shuts down here (the GL context is current) ----------
    {
        let (w, p) = (app.as_weak(), player.clone());
        let upd_ctx = Box::into_raw(Box::new(app.as_weak())) as *mut c_void;
        let gl_slot: Rc<RefCell<Option<glow::Context>>> = Rc::new(RefCell::new(None));
        let res = app.window().set_rendering_notifier(move |state, api| {
            let mut guard = p.borrow_mut();
            let pl = &mut *guard;
            match state {
                RenderingState::RenderingSetup => {
                    if let GraphicsAPI::NativeOpenGL { get_proc_address } = api {
                        *gl_slot.borrow_mut() = Some(unsafe { glow::Context::from_loader_function_cstr(|s| get_proc_address(s)) });
                    }
                    #[cfg(windows)]
                    if pl.mode == Mode::Window {
                        let hwnd = w.upgrade().and_then(|a| {
                            match raw_window_handle::HasWindowHandle::window_handle(&a.window().window_handle()).ok()?.as_raw() {
                                raw_window_handle::RawWindowHandle::Win32(h) => Some(h.hwnd.get() as windows_sys::Win32::Foundation::HWND),
                                _ => None,
                            }
                        });
                        pl.vwin = hwnd.and_then(winvideo::VideoWindow::new);
                        pl.poster = hwnd.and_then(winvideo::PosterWindow::new);
                    }
                }
                RenderingState::BeforeRendering => {
                    let Some(a) = w.upgrade() else { return };
                    let gl_ref = gl_slot.borrow();
                    let gl = gl_ref.as_ref();
                    // 1. a mode didn't work: drop it, try the next one (same clip, same position)
                    // 0. fullscreen <-> normal: change drawing mode, same clip, same position, same pause state
                    if let Some(target) = pl.switch_to.take() {
                        if target != pl.mode {
                            let pos = pl.mpv.as_ref().and_then(|m| m.get_f64("time-pos")).unwrap_or(0.0);
                            let paused = pl.mpv.as_ref().map(|m| m.get("pause").as_deref() == Some("yes")).unwrap_or(false);
                            let path = pl.clip.and_then(|i| LIB.with(|l| l.borrow().as_ref().map(|l| l.clip(i).path.clone())));
                            pl.mpv = None;
                            free_gpu(gl, &mut pl.gpu);
                            #[cfg(windows)]
                            if let Some(vw) = pl.vwin.as_ref() {
                                vw.show(false);
                            }
                            let line = format!("player: {} -> {} (fullscreen {})", pl.mode.name(), target.name(), if target == pl.base_mode { "off" } else { "on" });
                            log(pl, line);
                            pl.mode = target;
                            if pl.open {
                                pl.pending = path.map(|p| (p, pos));
                                pl.start_paused = paused;
                            }
                            a.global::<Play>().set_frame(slint::Image::default());
                        }
                    }
                    if let Some(why) = pl.fail.take() {
                        let pos = pl.mpv.as_ref().and_then(|m| m.get_f64("time-pos")).unwrap_or(0.0);
                        let path = pl.clip.and_then(|i| LIB.with(|l| l.borrow().as_ref().map(|l| l.clip(i).path.clone())));
                        pl.mpv = None;
                        free_gpu(gl, &mut pl.gpu);
                        #[cfg(windows)]
                        if let Some(vw) = pl.vwin.as_ref() {
                            vw.show(false);
                        }
                        match pl.mode.next() {
                            Some(n) => {
                                let line = format!("player: {} didn't work ({why}) - using {}", pl.mode.name(), n.name());
                                log(pl, line);
                                pl.mode = n;
                                pl.base_mode = n;
                                if pl.open {
                                    pl.pending = path.map(|p| (p, pos));
                                }
                            }
                            None => {
                                let line = format!("player: {} didn't work ({why}) - nothing left to try", pl.mode.name());
                                log(pl, line);
                                pl.mpv_error = why;
                                pl.pending = None;
                                a.global::<Play>().set_stats(format!("Can't play: {}", pl.mpv_error).into());
                                // gaps-player (row 40): that line is hidden - say it where it shows
                                toast(&a, format!("Can't play this clip: {}", pl.mpv_error));
                            }
                        }
                        a.global::<Play>().set_frame(slint::Image::default());
                    }
                    // 2. shut mpv down after a minute unused (less memory in the tray)
                    if pl.kill {
                        pl.kill = false;
                        if !pl.open && pl.mpv.is_some() {
                            pl.mpv = None;
                            pl.mpv_error = "stopped - starts again with the next clip".into();
                            // 11 Nov 2026: give back the pages mpv's start touched (D3D11 + driver, ~100 MB) - memtrim.rs
                            let t = memtrim::trim();
                            log(pl, format!("player: mpv shut down (player closed and not used for a while), memory trimmed{t}"));
                        }
                    }
                    if !pl.open {
                        // texture mode: the video texture + FBO stay while mpv runs (freed when mpv shuts down after
                        // a minute unused). Deleting them on close and making new ones on the next open gave a BLACK
                        // video now and then (time running, mpv drawing - cloud llvmpipe, native chat 4: 3 of 5 reopens
                        // black, 0 of 6 with the texture kept). ECHO_FREE_TEX=1 = old behaviour, for comparison.
                        if pl.mpv.is_none() || std::env::var("ECHO_FREE_TEX").is_ok() {
                            free_gpu(gl, &mut pl.gpu);
                        }
                        #[cfg(windows)]
                        if let Some(vw) = pl.vwin.as_ref() {
                            vw.show(false);
                        }
                        #[cfg(windows)]
                        if let Some(po) = pl.poster.as_ref() {
                            po.show(false);
                        }
                        pl.poster_px = None;
                        return;
                    }
                    // 3. start mpv for the current mode (lazily: only when a clip is opened)
                    if pl.mpv.is_none() && pl.pending.is_some() {
                        let r = start_mpv(pl, &a, api, upd_ctx);
                        match r {
                            Ok(()) => {
                                pl.mpv_error.clear();
                                pl.osd.forget();
                                let line = format!("player: started mpv - {}, hwdec {}", pl.mode.name(), if pl.mode == Mode::Software { "no" } else { pl.hwdec.as_str() });
                                log(pl, line);
                            }
                            Err(e) => {
                                pl.mpv = None;
                                pl.fail = Some(e);
                                a.window().request_redraw();
                                return;
                            }
                        }
                    }
                    // 4. load a waiting file
                    // r2-threads (L6): a cloud-only clip waits here until its stream address is ready (worker)
                    let aw = a.as_weak();
                    let ready = pl.pending.as_ref().is_none_or(|(p, _)| cloud::play_ready(p, move || if let Some(a) = aw.upgrade() { a.window().request_redraw() }));
                    if let (Some(m), Some((path, at))) = (pl.mpv.as_ref(), pl.pending.take_if(|_| ready)) {
                        let path = cloud::play_path(&path); // Chat B: cloud-only clips stream from Echo's rclone server
                        m.arm_first_frame();
                        if at > 0.5 {
                            m.command(&["loadfile", &path.to_string_lossy(), "replace", "-1", &format!("start={at:.3}")]);
                        } else {
                            m.command(&["loadfile", &path.to_string_lossy()]);
                        }
                        m.set("pause", if std::mem::take(&mut pl.start_paused) { "yes" } else { "no" });
                        m.set("ab-loop-a", "no");
                        m.set("ab-loop-b", "no");
                        let ps = a.global::<Play>();
                        m.set("volume", &format!("{:.0}", (ps.get_volume() * 100.0).clamp(0.0, 100.0)));
                        m.set("mute", if ps.get_muted() { "yes" } else { "no" });
                        pl.loaded_at = Some(Instant::now());
                        pl.first_frame = false;
                        // until the first picture of THIS clip is on screen: its thumbnail instead of black / the old clip.
                        // Windows window mode: a GDI child window over mpv's (Slint can't draw over mpv's window, and
                        // hiding mpv's window showed the desktop for a moment); other modes: Slint draws it.
                        a.global::<Play>().set_poster_on(pl.mode != Mode::Window);
                        #[cfg(windows)]
                        if let Some(vw) = pl.vwin.as_ref() {
                            let full = a.global::<Play>().get_full();
                            match pl.poster.as_ref() {
                                // flip model (default): prev/next while showing = the new clip replaces the old picture
                                // directly; opened from the library = stays parked until its first picture (Slint's
                                // black video area meanwhile)
                                _ if mpv::d3d_flip() && !full => pl.swap = vw.is_shown(),
                                Some(po) if pl.mode == Mode::Window && !full => {
                                    // plain black until the first picture (the video is there at once, the thumbnail
                                    // step only got in the way). ECHO_POSTER=1 = show the thumbnail on a fresh open again.
                                    let px = pl.poster_px.as_ref().filter(|_| at <= 0.5 && std::env::var("ECHO_POSTER").is_ok());
                                    po.set_picture(px.map(|b| (b.as_bytes(), b.width(), b.height())));
                                    let sf = a.window().scale_factor();
                                    let r = |v: f32| (v * sf).round() as i32;
                                    po.place(r(a.get_video_x()), r(a.get_video_y()), r(a.get_video_w()), r(a.get_video_h()), r(11.0));
                                    po.show(true);
                                }
                                // fullscreen (video window behind Slint's) / no poster window: hidden until the first picture
                                _ => vw.show(false),
                            }
                        }
                        pl.vo_checked = false;
                    }
                    let Some(m) = pl.mpv.as_ref() else { return };
                    // gaps-player (row 40): mpv couldn't open the file - say so instead of an endless black picture
                    // (cloud-only clips: left to the cloud's own streaming fallback)
                    if let Some(why) = m.take_load_error() {
                        let cloud = pl.clip.is_some_and(|i| LIB.with(|l| l.borrow().as_ref().is_some_and(|l| l.clip(i).cloud.is_some())));
                        // (never the streaming server's secret path: logs get shared)
                        let detail = m.last_error().replace(echo_core::recorder::serve_token_pub(), "…");
                        let detail = if detail.is_empty() { String::new() } else { format!(" - {detail}") };
                        log_to(&mut pl.log, format!("player: mpv couldn't open the clip ({why}){detail}"));
                        if !cloud {
                            toast(&a, format!("Can't play this clip ({why}) - the file may be damaged"));
                        }
                    }
                    let Some(m) = pl.mpv.as_ref() else { return };
                    // first picture of THIS clip on screen? (mpv's own START_FILE + PLAYBACK_RESTART events; or 1.5 s
                    // passed - never stay on the poster). Window mode keeps the video window hidden until then.
                    let mut just_shown = false;
                    if !pl.first_frame {
                        let shown = m.first_frame_shown();
                        if shown || pl.loaded_at.is_some_and(|t| t.elapsed() > Duration::from_millis(1500)) {
                            pl.first_frame = true;
                            just_shown = true;
                            if std::env::var("ECHO_DEBUG_OPEN").is_ok() || !shown {
                                let ms = pl.loaded_at.map(|t| t.elapsed().as_millis()).unwrap_or(0);
                                let line = if shown { format!("player: first picture after {ms} ms") } else { "player: no first-picture signal from mpv after 1.5 s - showing the video anyway".to_string() };
                                log_to(&mut pl.log, line);
                            }
                        } else {
                            a.window().request_redraw(); // look again next frame
                        }
                    }
                    if just_shown {
                        a.global::<Play>().set_poster_on(false);
                    }
                    // can the controls go over the video? Windows window mode only via mpv-drawn controls (osd.rs);
                    // ECHO_OSD=0 = keep them in a band below the video there
                    a.global::<Play>().set_overlay_ok(pl.mode != Mode::Window || std::env::var("ECHO_OSD").map(|v| v != "0").unwrap_or(true));
                    // flip model: mpv draws the seek preview over the video -> Slint keeps the preview inside the video
                    a.global::<Play>().set_pv_mpv(pl.mode == Mode::Window && mpv::d3d_flip());
                    match pl.mode {
                        Mode::Window => {
                            #[cfg(windows)]
                            if let Some(vw) = pl.vwin.as_ref() {
                                let sf = a.window().scale_factor();
                                let px = |v: f32| (v * sf).round() as i32;
                                // the seek preview sits partly over the video: cut it out of the video window
                                let full = a.global::<Play>().get_full();
                                let flip = mpv::d3d_flip();
                                let mut holes = vec![];
                                // blit model only: cut the seek preview out of the video window (flip model ignores
                                // regions - mpv draws the preview itself, below)
                                if a.get_preview_on() && !flip {
                                    holes.push((px(a.get_preview_x() - a.get_video_x()), px(a.get_preview_y() - a.get_video_y()), px(a.get_preview_w()), px(a.get_preview_h())));
                                }
                                // 9 Nov: going fullscreen showed the video at the OLD window size for ~80 ms
                                // (Windows grows the window a few frames after F) and mpv drew that size for another
                                // ~170 ms - the "half fullscreen" black box. Measured with ECHO_DEBUG_OSD (FRAGILE
                                // rule 1). While the window is still catching up, the video window gets its FINAL size
                                // (the whole monitor) at once: the part past the old window edge is just cut off for
                                // those frames, and mpv renders at the right size from the start.
                                let target = FS_ENTER.with(|c| c.get()).and_then(|(_, mon)| mon).filter(|&(mw, mh)| {
                                    let sz = a.window().size();
                                    full && (sz.width, sz.height) != (mw, mh)
                                });
                                match target {
                                    Some((mw, mh)) => vw.place(0, 0, mw as i32, mh as i32, 0, vec![]),
                                    None => vw.place(px(a.get_video_x()), px(a.get_video_y()), px(a.get_video_w()), px(a.get_video_h()), if full { 0 } else { px(11.0) }, holes),
                                }
                                let poster_up = pl.poster.as_ref().is_some_and(|po| po.is_shown());
                                if let Some(po) = pl.poster.as_ref().filter(|_| poster_up) {
                                    if full || pl.first_frame {
                                        // mpv's window shows THIS clip now: take the poster away
                                        po.show(false);
                                    } else {
                                        po.place(px(a.get_video_x()), px(a.get_video_y()), px(a.get_video_w()), px(a.get_video_h()), px(11.0));
                                    }
                                }
                                // with the poster on top, mpv's window can stay shown (no see-through gap); flip model:
                                // always shown in normal view (a new clip simply replaces the picture)
                                // a dialog is open (Leave editor? / Edit the project? / Delete?): the video window would cover
                                // it (Slint can't draw over it) - park it meanwhile; parked, it keeps its picture
                                let dlg = a.global::<Dlg>().get_open();
                                // gaps-cloud (row 5): also parked while the cloud overlay ("Loading from the cloud…") shows
                                let dlg = dlg || a.global::<Play>().get_cl_on();
                                // 11 Nov 2026: Edit from Home / Library loads the clip in the hidden player - no video over the page
                                let dlg = dlg || a.global::<Ed>().get_opening();
                                // 12 Nov 2026: Home scrolled down (small window): the picture is partly under the top bar - parked
                                let dlg = dlg || (a.global::<Play>().get_inline() && !full && a.global::<HomeState>().get_scrolled());
                                // r2-layout (L1): a menu reaching over the video - parked too, a still of its picture shows (over.rs)
                                let dlg = over::park_for_menu(&a, m, vw.is_shown()) || dlg;
                                vw.show(!dlg && (pl.first_frame || (poster_up && !full) || (flip && !full && pl.swap)));
                                over::set_video_win(&a, vw.is_shown());
                                // flip model, normal view: mpv draws the part of the seek preview that lies over the video
                                let pv = if flip && !full && a.get_preview_on() {
                                    let f = a.get_osd_hover();
                                    let d = a.global::<Play>().get_duration();
                                    Some((a.get_preview_x() - a.get_video_x(), a.get_preview_y() - a.get_video_y(), f, library::fmt_dur((f * d) as f64)))
                                } else {
                                    None
                                };
                                let sheet = SHEETS.with(|s| s.borrow().player_px.clone());
                                pl.osd.update_preview(m, pv, sheet.as_ref(), sf, px(a.get_video_w()), px(a.get_video_h()));
                                if just_shown {
                                    // a window that was hidden may not have taken mpv's last picture: if paused, ask
                                    // mpv to draw it again (playing = the next frame comes anyway)
                                    if m.get("pause").as_deref() == Some("yes") {
                                        m.command(&["seek", "0", "relative+exact"]);
                                    }
                                }

                            }
                        }
                        Mode::Texture => {
                            let Some(gl) = gl else { return };
                            let flags = m.update();
                            let sf = a.window().scale_factor();
                            let want = (((a.get_video_w() * sf).round() as i32).max(16), ((a.get_video_h() * sf).round() as i32).max(16));
                            let new_tex = unsafe { ensure_tex(gl, &mut pl.gpu, want) };
                            if new_tex || flags & mpv::UPDATE_FRAME != 0 {
                                let ms = unsafe { draw_mpv(gl, m, &pl.gpu, want) };
                                if ms > pl.render_ms_max {
                                    pl.render_ms_max = ms;
                                }
                                pl.frames += 1;
                                // same texture, new content: Slint can't see that, so flip a property to repaint it
                                let ps = a.global::<Play>();
                                ps.set_tick(!ps.get_tick());
                            }
                            // also when the texture was kept but the player cleared its picture (closed + reopened)
                            if new_tex || a.global::<Play>().get_frame().size().width == 0 {
                                let tex = pl.gpu.tex.unwrap();
                                let img = unsafe { slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(tex.0, [want.0 as u32, want.1 as u32].into()).build() };
                                a.global::<Play>().set_frame(img);
                            }
                        }
                        Mode::Software => {
                            let flags = m.update();
                            if flags & mpv::UPDATE_FRAME != 0 {
                                // CPU drawing: at most 1280 wide (Slint scales it up)
                                let sf = a.window().scale_factor();
                                let vw = ((a.get_video_w() * sf) as i32).clamp(64, 1280) & !15;
                                let vh = (vw * 9 / 16).max(36);
                                let t = Instant::now();
                                let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(vw as u32, vh as u32);
                                let bytes = buf.make_mut_bytes();
                                if m.render_sw(vw, vh, vw as usize * 4, bytes) {
                                    for px in bytes.chunks_exact_mut(4) {
                                        px[3] = 255;
                                    }
                                    a.global::<Play>().set_frame(slint::Image::from_rgba8(buf));
                                    pl.frames += 1;
                                }
                                let ms = t.elapsed().as_secs_f64() * 1000.0;
                                if ms > pl.render_ms_max {
                                    pl.render_ms_max = ms;
                                }
                            }
                        }
                    }
                    // editor: texts, captions, selection boxes, gaps + fades over the video = one ASS overlay mpv draws
                    // (all modes; nothing when the editor is closed). Not part of the fullscreen code below.
                    if let Some(m) = pl.mpv.as_ref() {
                        edui::overlay_frame(&a, m);
                    }
                    // fullscreen controls drawn by mpv (Windows window mode; ECHO_OSD=1 also in the other modes, for testing)
                    let osd_env = std::env::var("ECHO_OSD").unwrap_or_default();
                    let ps = a.global::<Play>();
                    let osd_on = osd_env != "0" && ps.get_full() && (pl.mode == Mode::Window || osd_env == "1");
                    ps.set_mpv_osd(osd_on);
                    if let Some(m) = pl.mpv.as_ref() {
                        // FRAGILE (fullscreen flicker, fixed native chat 4 after a failed try in chat 3): read the
                        // "FRAGILE" section of the handoff before changing anything here, in toggle_full or in osd.rs.
                        // Rule: never hand mpv an overlay drawn for a size that isn't on screen yet. Check with
                        // ECHO_DEBUG_OSD=1 (osd-debug.log, one line per frame for 3 s after F) before AND after a change.
                        // just entered fullscreen: wait (max 600 ms) until the window has the monitor's size and
                        // mpv's picture has the video window's size - then the band is drawn once, at its real spot
                        let mut waiting = false;
                        if osd_on {
                            if let Some((t, mon)) = FS_ENTER.with(|c| c.get()) {
                                let sz = a.window().size();
                                let sf = a.window().scale_factor();
                                let want = ((a.get_video_w() * sf).round() as i64, (a.get_video_h() * sf).round() as i64);
                                let mpv_sz = (m.get("osd-width").and_then(|v| v.parse::<i64>().ok()), m.get("osd-height").and_then(|v| v.parse::<i64>().ok()));
                                let win_ok = mon.map_or(true, |(w, h)| (sz.width, sz.height) == (w, h));
                                let mpv_ok = mpv_sz == (Some(want.0), Some(want.1));
                                if (win_ok && mpv_ok) || t.elapsed().as_millis() > 600 {
                                    FS_ENTER.with(|c| c.set(None));
                                } else {
                                    waiting = true;
                                    if pl.osd.on_screen {
                                        pl.osd.hide(m);
                                    }
                                    pl.osd.dbg = format!("waiting (window ok {win_ok}, mpv ok {mpv_ok})");
                                    a.window().request_redraw();
                                }
                            }
                        }
                        if osd_on && !waiting {
                            if let Some((k, px, tw, th)) = SHEETS.with(|s| s.borrow().player_px.clone()) {
                                pl.osd.set_sheet(k, &px, tw, th);
                            }
                            let sf = a.window().scale_factor();
                            let (vx, vy, vw, vh) = (a.get_video_x(), a.get_video_y(), a.get_video_w(), a.get_video_h());
                            let d = ps.get_duration();
                            let band_top = vy + vh - osd::BAND;
                            let st = osd::State {
                                w: vw,
                                sf,
                                show: ps.get_ui_on(),
                                progress: a.get_osd_progress(),
                                paused: ps.get_paused(),
                                time: format!("{} / {}", ps.get_time_text(), ps.get_dur_text()),
                                trimming: ps.get_trimming(),
                                t0: ps.get_t0(),
                                t1: ps.get_t1(),
                                trim_text: format!("{} – {}", fmt1(ps.get_t0() * d), fmt1(ps.get_t1() * d)),
                                trim_len: format!("· {:.1}s", (ps.get_t1() - ps.get_t0() - ps.get_cut_sum()).max(0.0) * d),
                                volume: ps.get_volume(),
                                muted: ps.get_muted(),
                                speed: ps.get_speed(),
                                accent: {
                                    let k = a.global::<T>().get_accent();
                                    (k.red(), k.green(), k.blue())
                                },
                                hi: {
                                    let k = a.global::<T>().get_hi();
                                    (k.red(), k.green(), k.blue())
                                },
                                cuts: pl_cuts::spans(&ps).into_iter().map(|(x, y)| (x as f32, y as f32)).collect(),
                                hl: (ps.get_h0() >= 0.0).then(|| (ps.get_h0(), ps.get_h1())),
                                preview: if a.get_preview_on() {
                                    let f = a.get_osd_hover();
                                    Some((a.get_preview_x() - vx, a.get_preview_y() - band_top, f, library::fmt_dur((f * d) as f64)))
                                } else {
                                    None
                                },
                                hover_strip: a.get_preview_on(),
                            };
                            let more = pl.osd.update(m, &st, (vh * sf).round() as u32);
                            if more {
                                a.window().request_redraw();
                            }
                        } else if waiting {
                        } else if pl.osd.on_screen {
                            pl.osd.hide(m);
                            pl.osd.drop_sheet(); // ~14 MB of pixels only while fullscreen
                            pl.osd.dbg = "hidden".into();
                        } else {
                            pl.osd.dbg = "-".into();
                        }
                        if std::env::var("ECHO_DEBUG_OSD").is_ok() {
                            let sf = a.window().scale_factor();
                            let sz = a.window().size();
                            let px = |v: f32| (v * sf).round() as i32;
                            osd_dbg(&format!(
                                "full={} win={}x{} video={},{} {}x{} mpv-osd-size={}x{} osd_on={} show={} band: {}",
                                ps.get_full() as u8,
                                sz.width,
                                sz.height,
                                px(a.get_video_x()),
                                px(a.get_video_y()),
                                px(a.get_video_w()),
                                px(a.get_video_h()),
                                m.get("osd-width").unwrap_or_default(),
                                m.get("osd-height").unwrap_or_default(),
                                osd_on as u8,
                                ps.get_ui_on() as u8,
                                pl.osd.dbg
                            ));
                        }
                    }
                    let el = pl.fps_at.elapsed().as_secs_f64();
                    if el >= 1.0 {
                        pl.fps = pl.frames as f64 / el;
                        pl.frames = 0;
                        pl.fps_at = Instant::now();
                    }
                }
                RenderingState::AfterRendering => {
                    glide_step(&w);
                    // 11 Nov 2026: the parts of open menus that lie over the video window go into a window above it
                    #[cfg(windows)]
                    if let Some(a) = w.upgrade() {
                        let shown = pl.mode == Mode::Window && pl.vwin.as_ref().is_some_and(|v| v.is_shown());
                        match gl_slot.borrow().as_ref() {
                            Some(gl) if shown => {
                                let sf = a.window().scale_factor();
                                let px = |v: f32| (v * sf).round() as i32;
                                menuwin::after_render(&a, gl, Some((px(a.get_video_x()), px(a.get_video_y()), px(a.get_video_w()), px(a.get_video_h()))));
                            }
                            _ => menuwin::hide(),
                        }
                    }
                    if pl.open && pl.mode == Mode::Texture {
                        if let Some(m) = pl.mpv.as_ref() {
                            m.report_swap();
                        }
                    }
                }
                RenderingState::RenderingTeardown => {
                    pl.mpv = None;
                    let gl = gl_slot.borrow_mut().take();
                    free_gpu(gl.as_ref(), &mut pl.gpu);
                }
                _ => {}
            }
        });
        if res.is_err() {
            player.borrow_mut().mpv_error = "this Slint renderer has no rendering hook".into();
        }
    }

    // ---------- 4x a second: player state + stats ----------
    let timer = slint::Timer::default();
    // ECHO_IDLE_SECS = test knob for the 60 s shutdown
    let idle_after = Duration::from_secs(std::env::var("ECHO_IDLE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(60));
    {
        let (w, p, lib, ds) = (app.as_weak(), player.clone(), lib.clone(), drag_seek.clone());
        let root = clips_dir.clone();
        let mut tick = 0u32;
        let mut hidden_since: Option<Instant> = None;
        let mut idle_since: Option<Instant> = None;
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(250), move || {
            let Some(a) = w.upgrade() else { return };
            tick += 1;
            let mut p = p.borrow_mut();
            if let Some(m) = p.mpv.as_ref() {
                m.drain_events();
            }
            cloud_live::player_tick(&a, p.mpv.as_ref(), p.clip.filter(|_| p.open)); // gaps-cloud (row 5)
            // the "Saving clip…" card never stays longer than 90 s (a save that never reports back)
            if tick % 12 == 6 && lib.saving_expire() {
                lib.rebuild_rows();
            }
            // every 3 s: clips deleted / added outside Echo (file manager) leave / join the library (chat 5)
            // r2-threads (L3): the walk runs on a worker (bgwork.rs), the library follows on the UI thread
            if tick % 12 == 6 && a.window().is_visible() && !a.window().is_minimized() {
                bgwork::disk_tick(&a, &lib, &root);
            }
            // 9 Nov: hidden (tray / minimized) for 30 s = efficiency mode for the UI side (idle.rs: fast
            // timers paused, UI thread EcoQoS + lower priority); the recorder's threads are never touched
            let hidden = a.window().is_minimized() || !a.window().is_visible();
            // perf-states: tray / idle / active / player for this tick (perf.rs: each minute counts under one state)
            perf::ui_tick(!hidden, a.get_player_open(), MOUSE.with(|m| m.borrow().last_move.elapsed()));
            if hidden {
                if idle_since.get_or_insert_with(Instant::now).elapsed() > Duration::from_secs(30) {
                    idle::sleep();
                }
            } else {
                idle_since = None;
                idle::wake();
            }
            // minimized for 20 s (and no clip open): let go of thumbnails + seek sheets, trim memory (like Echo's
            // low-memory mode in the tray). Everything comes back when the window is shown again.
            if hidden && !p.open {
                let since = *hidden_since.get_or_insert_with(Instant::now);
                if since.elapsed() > Duration::from_secs(20) && !lib.is_suspended() {
                    lib.suspend();
                    SHEETS.with(|s| s.borrow_mut().items.clear());
                    a.global::<Flip>().set_sheet(slint::Image::default());
                    hover_vid::stop(&a);
                    a.global::<Flip>().set_row(-1);
                    let t = memtrim::trim();
                    log(&mut p, format!("memory: minimized 20 s - thumbnails let go, memory trimmed{t}"));
                } else if let Some(m) = memtrim::hidden_tick() {
                    log(&mut p, m); // grew again while hidden (recorder restart, popup, save): trim again
                }
            } else {
                hidden_since = None;
                if p.open || WIN_FOCUSED.with(|f| f.get()) {
                    memtrim::shown();
                } else if let Some(m) = memtrim::behind_tick() {
                    log(&mut p, m); // open behind a game for 60 s: trim (memtrim.rs)
                }
                if lib.is_suspended() {
                    lib.resume();
                    log(&mut p, "memory: window back - thumbnails reload".into());
                }
            }
            // fullscreen controls: hide after 2.5 s without mouse movement (not while paused, while a button is
            // held, or while the mouse rests on the controls band)
            {
                let ps = a.global::<Play>();
                if ps.get_full() && ps.get_ui_on() && !ps.get_paused() {
                    let (idle, pressed, y) = MOUSE.with(|m| {
                        let m = m.borrow();
                        (m.last_move.elapsed(), m.pressed, m.y)
                    });
                    let on_controls = y >= a.get_video_y() + a.get_video_h() - 123.0;
                    if !pressed && !on_controls && idle > Duration::from_millis(2500) {
                        ps.set_ui_on(false);
                    }
                }
            }
            // mpv unused for 60 s: shut it down (the render hook does it, with the GL context current)
            if !p.open && p.mpv.is_some() && p.closed_at.is_some_and(|t| t.elapsed() > idle_after) {
                p.kill = true;
                p.closed_at = None;
                a.window().request_redraw();
            }
            if !p.open {
                return;
            }
            let p = &mut *p;
            let Some(m) = p.mpv.as_ref() else { return };
            // no picture 3 s after loading: this drawing mode doesn't work here -> next one
            if !p.vo_checked && p.loaded_at.is_some_and(|t| t.elapsed() > Duration::from_secs(3)) {
                p.vo_checked = true;
                let has_video = m.get("video-codec").is_some_and(|v| !v.is_empty());
                let vo_ok = m.get("vo-configured").as_deref() == Some("yes");
                if (has_video && !vo_ok) || simulated_fail(p.mode) {
                    p.fail = Some(if simulated_fail(p.mode) { "ECHO_FAIL test".into() } else { "no picture after 3 s".into() });
                    a.window().request_redraw();
                    return;
                }
            }
            // dragging on the strip / a trim end: no reads (each one waits for mpv's core, busy seeking)
            if ds.busy() {
                return;
            }
            let pos = m.get_f64("time-pos").unwrap_or(0.0);
            let dur = m.get_f64("duration").unwrap_or(p.duration);
            let paused = m.get("pause").as_deref() == Some("yes") || m.get("eof-reached").as_deref() == Some("yes");
            if std::env::var("ECHO_DEBUG_STATE").is_ok() {
                eprintln!("state: pos {pos:.2} pause {:?} eof {:?} idle {:?} seeking {:?} ab {:?}-{:?} paused-for-cache {:?}", m.get("pause"), m.get("eof-reached"), m.get("core-idle"), m.get("seeking"), m.get("ab-loop-a"), m.get("ab-loop-b"), m.get("paused-for-cache"));
            }
            let pl = a.global::<Play>();
            if dur > 0.0 {
                p.duration = dur;
                // where it will be at the next update: the bar glides there in 250 ms (smooth, no steps)
                let ahead = if paused || m.get("seeking").as_deref() == Some("yes") { 0.0 } else { 0.25 * m.get_f64("speed").unwrap_or(1.0) };
                pl.set_snap(paused);
                pl.set_progress(((pos + ahead) / dur).clamp(0.0, 1.0) as f32);
                pl.set_duration(dur as f32);
                pl.set_dur_text(library::fmt_dur(dur).into());
            }
            pl.set_time_text(library::fmt_dur(pos).into());
            pl.set_paused(paused);
            let hw = m.get("hwdec-current").unwrap_or_default();
            let codec = m.get("video-format").unwrap_or_default();
            let (vw, vh) = (m.get("video-params/w").unwrap_or_default(), m.get("video-params/h").unwrap_or_default());
            let vfps = m.get_f64("container-fps").unwrap_or(0.0);
            if !p.info_done && !vw.is_empty() {
                p.info_done = true;
                if let Some(idx) = p.clip {
                    let c = lib.clip(idx);
                    pl.set_info(info_text(&c, Some(format!("{vw}×{vh} · {codec} · {vfps:.0} fps"))).into());
                    pl.set_info_chips(info_chips(&c, Some((vh.clone(), codec.clone(), vfps))));
                }
            }
            let drops = m.get("frame-drop-count").unwrap_or_else(|| "0".into());
            let ddrops = m.get("decoder-frame-drop-count").unwrap_or_else(|| "0".into());
            let decoder = if hw.is_empty() || hw == "no" {
                "CPU decode".to_string()
            } else if hw.ends_with("-copy") {
                format!("GPU decode + copy ({hw})")
            } else {
                format!("GPU only ({hw})")
            };
            if p.mode == Mode::Window {
                // mpv shows the frames itself here: ask it how many it put on screen
                p.fps = m.get_f64("estimated-vf-fps").unwrap_or(0.0);
            }
            pl.set_stats(
                format!(
                    "{codec} {vw}x{vh} @ {vfps:.0} · {decoder} · {} · drawn {:.0} fps · dropped {drops}/{ddrops} · slowest draw {:.1} ms",
                    match p.mode {
                        Mode::Window => "D3D11 window",
                        Mode::Texture => "GL texture",
                        Mode::Software => "software",
                    },
                    p.fps,
                    p.render_ms_max
                )
                .into(),
            );
        });
    }

    // test hook: ECHO_PAGE=settings:<section> (or settings:?<search>) opens the Settings page at start
    if let Ok(pg) = std::env::var("ECHO_PAGE") {
        if let Some(sec) = pg.strip_prefix("settings:") {
            {
                let mut st = store.borrow_mut();
                if let Some(q) = sec.strip_prefix('?') {
                    st.query = q.into();
                    app.global::<S>().set_query(q.into());
                } else {
                    st.sec = sec.into();
                }
            }
            app.set_view("settings".into());
            srefresh();
        } else if pg == "library" {
            app.set_view("library".into()); // ECHO_PAGE=library (uiscale screenshots)
        }
    }
    // test hooks: ECHO_AUTOPLAY=<card number> opens that clip 1.5 s after start,
    // ECHO_HOVER=<card number> starts that card's flipbook, ECHO_PICK=<n> selects the first n cards
    if let Some(row) = std::env::var("ECHO_AUTOPLAY").ok().and_then(|v| v.parse::<i32>().ok()) {
        let play = play.clone();
        slint::Timer::single_shot(Duration::from_millis(1500), move || play(row));
    }
    // ECHO_EDIT=<card number> opens that clip in the editor 1.5 s after start (native chat 5)
    if let Some(row) = std::env::var("ECHO_EDIT").ok().and_then(|v| v.parse::<usize>().ok()) {
        let lib = lib.clone();
        slint::Timer::single_shot(Duration::from_millis(1500), move || {
            if let Some(i) = lib.index_at(row) {
                open_editor(i);
            }
        });
    }
    if let Some(row) = std::env::var("ECHO_HOVER").ok().and_then(|v| v.parse::<i32>().ok()) {
        let w = app.as_weak();
        slint::Timer::single_shot(Duration::from_millis(1200), move || {
            if let Some(a) = w.upgrade() {
                a.global::<Lib>().invoke_hover(row, true);
            }
        });
    }
    if let Some(n) = std::env::var("ECHO_PICK").ok().and_then(|v| v.parse::<usize>().ok()) {
        for r in 0..n {
            lib.toggle_pick(r);
        }
        sync(false);
    }
    // mouse wheel: Slint scrolls 60 px per wheel step in one jump - with ~300 px card rows that "doesn't do much".
    // ECHO_SCROLL px per step (default 160, ~ a browser; 0 = Slint's own scrolling) is given out
    // ONCE PER DRAWN FRAME (the render hook's AfterRendering), easing out over ~150 ms - so it glides at the
    // monitor's refresh rate (240 Hz and up), not at a timer's pace. Touchpad pixel scrolling is left alone.
    let step: f32 = std::env::var("ECHO_SCROLL").ok().and_then(|v| v.parse().ok()).unwrap_or(160.0);
    // 9 Nov: mouse side buttons, browser-style. Back = one step back (leave the editor, close the player,
    // or from Settings / Screenshots back to the page before); Forward = undo that Back (reopen the clip / the page).
    let nav: Rc<dyn Fn(&AppWindow, bool)> = {
        let (play, p, lib) = (play.clone(), player.clone(), lib.clone());
        Rc::new(move |a: &AppWindow, back: bool| {
            thread_local!(static FWD: RefCell<Option<(String, usize)>> = const { RefCell::new(None) });
            let go = |a: &AppWindow, v: &str| match v {
                "home" => a.invoke_page("Home".into()),
                "settings" => a.invoke_page("Settings".into()),
                "shots" => a.invoke_pick_filter("shots".into()),
                _ => a.invoke_pick_filter(if a.get_filter() == "shots" { "all".into() } else { a.get_filter() }),
            };
            if a.global::<Dlg>().get_open() {
                return;
            }
            if back {
                if a.global::<Shots>().get_view_open() {
                    a.global::<Shots>().set_view_open(false);
                } else if a.global::<Ed>().get_open() {
                    a.global::<Ed>().invoke_act("back".into());
                } else if a.get_player_open() {
                    let idx = p.borrow().clip;
                    a.global::<Play>().invoke_close();
                    FWD.with(|f| *f.borrow_mut() = idx.map(|i| ("clip".to_string(), i)));
                } else {
                    let (cur, prev) = (a.get_view().to_string(), a.get_prev_view().to_string());
                    if cur != prev {
                        go(a, &prev);
                        FWD.with(|f| *f.borrow_mut() = Some((cur, 0)));
                    }
                }
            } else if let Some((what, idx)) = FWD.with(|f| f.borrow_mut().take()) {
                if what == "clip" {
                    if let Some(row) = lib.view_row_of(idx) {
                        play(row as i32);
                    }
                } else {
                    go(a, &what);
                }
            }
        })
    };
    {
        use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
        let wk = app.as_weak();
        let nav = nav.clone();
        app.window().on_winit_window_event(move |win, ev| {
            ownkeys::winit_event(ev); // Echo's hotkeys while its own window has the keyboard
            if let Some(r) = uiscale::main_event(win, ev) {
                return r; // Echo's own size on top of the system's scale (uiscale.rs)
            }
            #[cfg(target_os = "linux")]
            wl_dnd::start(&wk, win); // first event only: Wayland drag and drop listener (wl_dnd.rs)
            // fullscreen controls: wake on any movement inside the window
            let wake = |w: &slint::Weak<AppWindow>| {
                if let Some(a) = w.upgrade() {
                    let pl = a.global::<Play>();
                    if pl.get_full() && !pl.get_ui_on() {
                        pl.set_ui_on(true);
                    }
                }
            };
            match ev {
                winit::event::WindowEvent::CursorMoved { position, .. } => {
                    let p = position.to_logical::<f32>(win.scale_factor() as f64);
                    SCROLL.with(|s| s.borrow_mut().pos = slint::LogicalPosition::new(p.x, p.y));
                    MOUSE.with(|m| {
                        let mut m = m.borrow_mut();
                        m.last_move = Instant::now();
                        m.inside = true;
                        m.y = p.y;
                    });
                    wake(&wk);
                    EventResult::Propagate
                }
                winit::event::WindowEvent::Focused(false) => {
                    WIN_FOCUSED.with(|f| f.set(false));
                    // Ctrl's key-up never arrives when the window loses focus (Alt+Tab with Ctrl held): Echo's blur
                    if let Some(a) = wk.upgrade() {
                        let l = a.global::<Lib>();
                        l.set_ctrl_held(false);
                        l.set_ctrl_inst(false);
                        // Echo: no hover preview while Echo isn't in front (the mouse never "leaves" the card then)
                        hover_vid::stop(&a);
                    }
                    EventResult::Propagate
                }
                // gaps-library row 1: files dragged onto the window (drop.rs)
                winit::event::WindowEvent::HoveredFile(_) | winit::event::WindowEvent::HoveredFileCancelled | winit::event::WindowEvent::DroppedFile(_) => {
                    drop::winit_event(&wk, win, ev);
                    EventResult::Propagate
                }
                winit::event::WindowEvent::CursorEntered { .. } => {
                    MOUSE.with(|m| m.borrow_mut().inside = true);
                    EventResult::Propagate
                }
                winit::event::WindowEvent::CursorLeft { .. } => {
                    MOUSE.with(|m| m.borrow_mut().inside = false);
                    if let Some(a) = wk.upgrade() {
                        let pl = a.global::<Play>();
                        if pl.get_full() && !MOUSE.with(|m| m.borrow().pressed) {
                            pl.set_ui_on(false);
                        }
                    }
                    EventResult::Propagate
                }
                winit::event::WindowEvent::ModifiersChanged(m) => {
                    HELD_MODS.with(|x| x.set((m.state().control_key(), m.state().shift_key())));
                    EventResult::Propagate
                }
                winit::event::WindowEvent::Focused(true) => {
                    WIN_FOCUSED.with(|f| f.set(true));
                    unstick_mods(win);
                    setup::focused(); // gaps-settings: Echo's updSoon (update re-check, at most every 5 min)
                    hdr_home::on_focus(); // gaps (10 Nov): KDE may have changed Echo's keys
                    EventResult::Propagate
                }
                winit::event::WindowEvent::MouseInput { state: winit::event::ElementState::Pressed, button: b @ (winit::event::MouseButton::Back | winit::event::MouseButton::Forward), .. } => {
                    // side buttons: after this event (not inside winit's handler)
                    let back = *b == winit::event::MouseButton::Back;
                    let (wk2, nav) = (wk.clone(), nav.clone());
                    slint::Timer::single_shot(Duration::from_millis(0), move || {
                        if let Some(a) = wk2.upgrade() {
                            nav(&a, back);
                        }
                    });
                    EventResult::PreventDefault
                }
                // L21: Echo's keydown wakes the fullscreen controls too (Space to pause kept them hidden)
                winit::event::WindowEvent::KeyboardInput { event, .. } if event.state == winit::event::ElementState::Pressed => {
                    MOUSE.with(|m| m.borrow_mut().last_move = Instant::now());
                    wake(&wk);
                    EventResult::Propagate
                }
                winit::event::WindowEvent::MouseInput { state, .. } => {
                    if *state == winit::event::ElementState::Pressed {
                        unstick_mods(win);
                    }
                    MOUSE.with(|m| {
                        let mut m = m.borrow_mut();
                        m.pressed = *state == winit::event::ElementState::Pressed;
                        m.last_move = Instant::now();
                    });
                    wake(&wk);
                    EventResult::Propagate
                }
                // 12 Nov 2026 (William: no scrolling work while minimized / in the tray): a wheel turn does nothing while
                // the window is minimized or hidden, and on Home also while Echo isn't the focused window (Windows'
                // "scroll inactive windows" sends the wheel to whatever window is under the mouse, e.g. Echo open behind
                // a game) - no glide, no relayout, nothing loaded
                winit::event::WindowEvent::MouseWheel { .. }
                    if !win.is_visible()
                        || win.is_minimized()
                        || (!echo_in_front(win) && wk.upgrade().is_some_and(|a| a.get_view() == "home")) =>
                {
                    EventResult::PreventDefault
                }
                winit::event::WindowEvent::MouseWheel { delta: winit::event::MouseScrollDelta::LineDelta(dx, dy), .. } if step > 0.0 => {
                    SCROLL.with(|s| {
                        let mut s = s.borrow_mut();
                        // a turn the other way stops the glide at once
                        if s.left_y * dy < 0.0 {
                            s.left_y = 0.0;
                        }
                        if s.left_x * dx < 0.0 {
                            s.left_x = 0.0;
                        }
                        if s.left_x == 0.0 && s.left_y == 0.0 {
                            s.last = None;
                        }
                        s.left_x += dx * step;
                        s.left_y += dy * step;
                    });
                    win.request_redraw();
                    EventResult::PreventDefault
                }
                _ => EventResult::Propagate,
            }
        });
    }
    setup_updates(&app);
    rec_core::wire(&app); // Chat A: recorder, hotkeys, tray, popup
    perf::start(); // memory / CPU averages, sent with the update check (perf.rs)
    rec_core::run(&app); // Chat A: was app.run() - the window can hide into the tray
    // close mpv before the window goes away
    player.borrow_mut().mpv = None;
}

/// create mpv for the player's current mode (called in the render hook, GL context current)
fn start_mpv(pl: &mut Player, a: &AppWindow, api: &GraphicsAPI, upd_ctx: *mut c_void) -> Result<(), String> {
    if simulated_fail(pl.mode) && pl.mode != Mode::Software && std::env::var("ECHO_FAIL_LATE").is_err() {
        return Err("ECHO_FAIL test".into());
    }
    match pl.mode {
        Mode::Window => {
            #[cfg(windows)]
            {
                let vw = pl.vwin.as_ref().ok_or("couldn't make the video window")?;
                let hw = if std::env::var("ECHO_HWDEC").is_err() { "d3d11va".to_string() } else { pl.hwdec.clone() };
                pl.mpv = Some(mpv::Mpv::new(&hw, Some(vw.hwnd as i64))?);
                Ok(())
            }
            #[cfg(not(windows))]
            Err("window mode is Windows only".into())
        }
        Mode::Texture => {
            let GraphicsAPI::NativeOpenGL { get_proc_address } = api else {
                return Err("Slint isn't drawing with OpenGL".into());
            };
            let mut m = mpv::Mpv::new(&pl.hwdec, None)?;
            // window system display for GPU-decoded frames (VA-API on Linux)
            let mut extra = vec![];
            if let Ok(dh) = raw_window_handle::HasDisplayHandle::display_handle(&a.window().window_handle()) {
                match dh.as_raw() {
                    raw_window_handle::RawDisplayHandle::Wayland(h) => extra.push(mpv::RenderParam { kind: mpv::P_WL_DISPLAY, data: h.display.as_ptr() }),
                    raw_window_handle::RawDisplayHandle::Xlib(h) => {
                        if let Some(d) = h.display {
                            extra.push(mpv::RenderParam { kind: mpv::P_X11_DISPLAY, data: d.as_ptr() })
                        }
                    }
                    _ => {}
                }
            }
            let gpa: Gpa = get_proc_address;
            let ctx = &gpa as *const Gpa as *mut c_void;
            // (not wrapped in keep_gl_state: mpv then drew black - it has to see the context as Slint leaves it)
            m.create_render_context(gpa_trampoline, ctx, extra, on_mpv_update, upd_ctx)?;
            pl.mpv = Some(m);
            Ok(())
        }
        Mode::Software => {
            let mut m = mpv::Mpv::new("no", None)?;
            m.create_sw_render_context(on_mpv_update, upd_ctx)?;
            pl.mpv = Some(m);
            Ok(())
        }
    }
}

fn free_gpu(gl: Option<&glow::Context>, g: &mut Gpu) {
    let Some(gl) = gl else {
        g.tex = None;
        g.fbo = None;
        return;
    };
    unsafe {
        if let Some(t) = g.tex.take() {
            gl.delete_texture(t);
        }
        if let Some(f) = g.fbo.take() {
            gl.delete_framebuffer(f);
        }
    }
    g.size = (0, 0);
}

/// (re)create the video texture + framebuffer at `want` pixels; true = new texture
unsafe fn ensure_tex(gl: &glow::Context, g: &mut Gpu, want: (i32, i32)) -> bool {
    if g.size == want && g.tex.is_some() {
        return false;
    }
    free_gpu(Some(gl), g);
    let prev_fb = gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING);
    let prev_tex = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D);
    let tex = gl.create_texture().unwrap();
    gl.bind_texture(glow::TEXTURE_2D, Some(tex));
    gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, want.0, want.1, 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
    let fbo = gl.create_framebuffer().unwrap();
    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
    gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
    let sc = gl.is_enabled(glow::SCISSOR_TEST);
    gl.disable(glow::SCISSOR_TEST);
    gl.clear_color(0.0, 0.0, 0.0, 1.0);
    gl.clear(glow::COLOR_BUFFER_BIT);
    if sc {
        gl.enable(glow::SCISSOR_TEST)
    }
    gl.bind_framebuffer(glow::FRAMEBUFFER, NonZeroU32::new(prev_fb as u32).map(glow::NativeFramebuffer));
    gl.bind_texture(glow::TEXTURE_2D, NonZeroU32::new(prev_tex as u32).map(glow::NativeTexture));
    if std::env::var("ECHO_DEBUG_PIX").is_ok() {
        eprintln!("debug: new video texture {:?} fbo {:?} size {want:?}", tex, fbo);
    }
    g.tex = Some(tex);
    g.fbo = Some(fbo);
    g.size = want;
    true
}

/// run `f` (mpv drawing / starting) without disturbing the GL state Slint's renderer relies on
unsafe fn keep_gl_state<R>(gl: &glow::Context, f: impl FnOnce() -> R) -> R {
    let prev_fb = gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING);
    let mut vp = [0i32; 4];
    gl.get_parameter_i32_slice(glow::VIEWPORT, &mut vp);
    let prev_prog = gl.get_parameter_i32(glow::CURRENT_PROGRAM);
    let prev_vao = gl.get_parameter_i32(glow::VERTEX_ARRAY_BINDING);
    let prev_buf = gl.get_parameter_i32(glow::ARRAY_BUFFER_BINDING);
    let prev_active = gl.get_parameter_i32(glow::ACTIVE_TEXTURE);
    let prev_tex = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D);
    let blend = gl.is_enabled(glow::BLEND);
    let scissor = gl.is_enabled(glow::SCISSOR_TEST);
    let stencil = gl.is_enabled(glow::STENCIL_TEST);
    let depth = gl.is_enabled(glow::DEPTH_TEST);
    let cull = gl.is_enabled(glow::CULL_FACE);
    // Slint's renderer clips with scissor/stencil: those would cut mpv's picture
    gl.disable(glow::SCISSOR_TEST);
    gl.disable(glow::STENCIL_TEST);
    gl.disable(glow::DEPTH_TEST);
    gl.disable(glow::CULL_FACE);
    gl.color_mask(true, true, true, true);
    let r = f();
    gl.bind_framebuffer(glow::FRAMEBUFFER, NonZeroU32::new(prev_fb as u32).map(glow::NativeFramebuffer));
    gl.viewport(vp[0], vp[1], vp[2], vp[3]);
    gl.use_program(NonZeroU32::new(prev_prog as u32).map(glow::NativeProgram));
    gl.bind_vertex_array(NonZeroU32::new(prev_vao as u32).map(glow::NativeVertexArray));
    gl.bind_buffer(glow::ARRAY_BUFFER, NonZeroU32::new(prev_buf as u32).map(glow::NativeBuffer));
    gl.active_texture(prev_active as u32);
    gl.bind_texture(glow::TEXTURE_2D, NonZeroU32::new(prev_tex as u32).map(glow::NativeTexture));
    if blend {
        gl.enable(glow::BLEND)
    } else {
        gl.disable(glow::BLEND)
    }
    if scissor {
        gl.enable(glow::SCISSOR_TEST)
    } else {
        gl.disable(glow::SCISSOR_TEST)
    }
    if stencil {
        gl.enable(glow::STENCIL_TEST)
    }
    if depth {
        gl.enable(glow::DEPTH_TEST)
    }
    if cull {
        gl.enable(glow::CULL_FACE)
    }
    r
}

/// mpv draws the current frame into our texture; returns how long it took (ms)
unsafe fn draw_mpv(gl: &glow::Context, m: &mpv::Mpv, g: &Gpu, want: (i32, i32)) -> f64 {
    keep_gl_state(gl, || {
        let t = Instant::now();
        m.render(g.fbo.unwrap().0.get() as i32, want.0, want.1, false);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if std::env::var("ECHO_DEBUG_PIX").is_ok() {
            gl.bind_framebuffer(glow::FRAMEBUFFER, g.fbo);
            let mut px = [0u8; 4];
            gl.read_pixels(want.0 / 2, want.1 / 2, 1, 1, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut px)));
            let st = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            let is_tex = g.tex.is_some_and(|t| gl.is_texture(t));
            eprintln!("debug: centre pixel {px:?} err {:x} size {want:?} tex {:?} alive {is_tex} fbo status {st:x}", gl.get_error(), g.tex);
        }
        ms
    })
}

