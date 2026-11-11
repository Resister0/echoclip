//! 11 Nov 2026 (1080p vs 1440p): Echo looks the same size on every screen. Slint draws in "logical" pixels = the
//! system's display scale (Windows 100 % -> 1 logical px = 1 screen px), so on a 2560x1440 screen at 100 % every
//! text, button and card came out 25 % smaller than on a 1920x1080 screen. Echo now adds its own size ON TOP of the
//! system's: Auto = the screen's size measured in the system's own scale, compared with 1920x1080 (1440p at 100 % ->
//! 133 %, 1440p at 125 % -> 1440 / 1.25 = 1152 -> 100 %, so nothing is scaled twice).
//! Auto then grows half of that (1440p -> 115 %: the full 133 % looked too big, 11 Nov 2026). Settings > Appearance > Size
//! can pick it by hand instead (native_ui.json "ui_scale": "auto" / "90" / "100" / "115" / "125" / "150").
//!
//! How: Slint's window keeps a scale factor (logical -> screen pixels). The winit hooks (main.rs for the main window,
//! `hook_popup` for the in-game popup) swallow winit's own ScaleFactorChanged and Echo hands Slint "system scale x
//! Echo's size" instead; everything Slint draws (text, paths, images) is drawn at that size, so it stays sharp.
//! Code that turns logical px into screen px reads `window().scale_factor()` (mpv's picture, thumbnails, the hover
//! video, the popup's window shape) and follows by itself. Re-checked when the window moves to another screen.
//! Test hook: ECHO_SCREEN=1920x1080 = pretend the window's screen has that size (Auto only).
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
use slint::ComponentHandle;
use std::cell::{Cell, RefCell};

/// Auto's steps (nearest one wins)
const STEPS: [f64; 7] = [1.0, 1.15, 1.25, 1.33, 1.5, 1.75, 2.0];
/// Settings > Appearance > Size
pub const PICKS: [(&str, &str); 6] = [("auto", "Auto"), ("90", "90 %"), ("100", "100 %"), ("115", "115 %"), ("125", "125 %"), ("150", "150 %")];

thread_local! {
    static PICK: RefCell<Option<String>> = const { RefCell::new(None) };
    static APP: RefCell<Option<slint::Weak<crate::AppWindow>>> = const { RefCell::new(None) };
    static POPUP: RefCell<Option<slint::Weak<crate::PopupWin>>> = const { RefCell::new(None) };
    /// the main window: (system scale, Echo's size wanted, scale handed to Slint) - the size wanted keeps the window's
    /// size on another screen; the scale handed over can be lower while the window is small (`capped`)
    static LAST: Cell<Option<(f64, f64, f32)>> = const { Cell::new(None) };
    /// the main window's screen (physical size) + system scale, for the Settings hint
    static SCREEN: Cell<Option<(u32, u32, f64)>> = const { Cell::new(None) };
    static QUEUED: Cell<bool> = const { Cell::new(false) };
    static POP_QUEUED: Cell<bool> = const { Cell::new(false) };
    /// the next fit may resize the window (another screen / first start). A Settings pick or a resize never does: the
    /// window stays, what's in it grows / shrinks
    static MAY_RESIZE: Cell<bool> = const { Cell::new(false) };
    /// the main window's first winit event came
    static SEEN: Cell<bool> = const { Cell::new(false) };
}

/// SLINT_SCALE_FACTOR set by Echo itself (Windows, before the first show) - not by the user
static OWN_ENV: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// the user set SLINT_SCALE_FACTOR: Slint's fixed scale wins, Echo adds nothing
fn user_fixed() -> bool {
    !OWN_ENV.load(std::sync::atomic::Ordering::Relaxed) && std::env::var_os("SLINT_SCALE_FACTOR").is_some()
}

pub fn pick() -> String {
    PICK.with(|p| {
        p.borrow_mut()
            .get_or_insert_with(|| {
                let v = crate::lib_fx::ui_get("ui_scale");
                let s = v.as_str().unwrap_or("auto");
                if PICKS.iter().any(|(k, _)| *k == s) { s.to_string() } else { "auto".into() }
            })
            .clone()
    })
}

/// Echo's size for a screen of `mon` physical px at system scale `os`
pub fn factor(pick: &str, mon: Option<(u32, u32)>, os: f64) -> f64 {
    match pick {
        "90" => 0.9,
        "100" => 1.0,
        "115" => 1.15,
        "125" => 1.25,
        "150" => 1.5,
        _ => auto(mon, os),
    }
}

/// the least room Echo's pages get, logical px. 11 Nov 2026 (QA B10): 150 % in a 1500x900 window left 1000x600 -
/// Library's 2nd card column, the 2nd hotkey chip and the sidebar's last rows ran off the edges. Below this room the
/// size steps down (in 5 % steps, never under 100 %) until the window is bigger again.
const ROOM: (f64, f64) = (1280.0, 720.0);

/// Echo's size `k` as far as a `phys` window (physical px) at system scale `os` has room for it
fn capped(k: f64, phys: (u32, u32), os: f64) -> f64 {
    if k <= 1.0 || phys.0 == 0 || phys.1 == 0 || os <= 0.0 {
        return k;
    }
    let fit = (phys.0 as f64 / os / ROOM.0).min(phys.1 as f64 / os / ROOM.1);
    k.min(((fit * 20.0 + 1e-6).floor() / 20.0).max(1.0))
}

fn auto(mon: Option<(u32, u32)>, os: f64) -> f64 {
    let Some((w, h)) = fake_screen().or(mon) else { return 1.0 };
    if w == 0 || h == 0 || os <= 0.0 {
        return 1.0;
    }
    // both ways, so an ultrawide 3440x1440 counts as 1440 high and a 2560x1080 as 1080
    let r = (w as f64 / os / 1920.0).min(h as f64 / os / 1080.0);
    if r <= 1.0 {
        return 1.0; // smaller screens: as before (Echo was made for 1920x1080)
    }
    // 11 Nov 2026 (William): the full ratio (1440p -> 133 %) was too big, 100 % too small: grow half of it (1440p -> 115 %)
    let r = 1.0 + (r - 1.0) * 0.5;
    STEPS.iter().copied().min_by(|a, b| (a - r).abs().total_cmp(&(b - r).abs())).unwrap_or(1.0)
}

fn fake_screen() -> Option<(u32, u32)> {
    let v = std::env::var("ECHO_SCREEN").ok()?;
    let (w, h) = v.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// main(): remember the window; Windows: work out the first screen's size before the window exists, so it opens at
/// the right size straight away (no jump from small to big on every start)
pub fn install(app: &crate::AppWindow) {
    APP.with(|a| *a.borrow_mut() = Some(app.as_weak()));
    #[cfg(windows)]
    if !user_fixed() {
        if let Some((w, h, os)) = primary_screen() {
            let k = factor(&pick(), Some((w, h)), os);
            // the first size (app.slint preferred 1500 x 900) at a big pick can be taller than the screen (150 % on
            // 1080p = 1350 px): smaller then, same shape - and the size then as far as that window has room (capped)
            let (pw, ph) = (1500.0 * os * k, 900.0 * os * k);
            let fit = (w as f64 * 0.94 / pw).min((h as f64 - 48.0 * os) * 0.94 / ph);
            let ke = if fit < 1.0 {
                let (fw, fh) = ((pw * fit) as u32, (ph * fit) as u32);
                app.window().set_size(slint::PhysicalSize::new(fw, fh));
                capped(k, (fw, fh), os)
            } else {
                k
            };
            let sf = os * ke;
            LAST.with(|l| l.set(Some((os, k, sf as f32))));
            if (sf - os).abs() > 0.001 {
                OWN_ENV.store(true, std::sync::atomic::Ordering::Relaxed);
                // read by Slint only when it makes a window; taken away again at the main window's first event
                std::env::set_var("SLINT_SCALE_FACTOR", format!("{sf:.4}"));
            }
        }
    }
}

/// Windows: the main monitor (Windows opens new windows there): physical size + system scale
#[cfg(windows)]
fn primary_screen() -> Option<(u32, u32, f64)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
    use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI};
    // SAFETY: plain Win32 calls writing into locals. The DPI call is the same one winit makes (a second call fails
    // harmlessly); without it Windows would report a scaled-down screen and 96 dpi.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let m = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if m.is_null() || GetMonitorInfoW(m, &mut mi) == 0 {
            return None;
        }
        let (mut dx, mut dy) = (96u32, 96u32);
        if GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) != 0 {
            dx = 96;
        }
        let r = mi.rcMonitor;
        Some(((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32, dx as f64 / 96.0))
    }
}

/// main.rs's winit hook, first thing: Some(result) = handled here
pub fn main_event(win: &slint::Window, ev: &winit::event::WindowEvent) -> Option<EventResult> {
    if user_fixed() {
        return None;
    }
    if OWN_ENV.swap(false, std::sync::atomic::Ordering::Relaxed) {
        // the main window exists now: later windows (the popup) start from the system's scale and hook_popup fits them
        std::env::remove_var("SLINT_SCALE_FACTOR");
    }
    match ev {
        // the system's scale changed (other screen / Windows setting): Echo's own value instead of winit's
        winit::event::WindowEvent::ScaleFactorChanged { .. } => {
            queue_main(true);
            Some(EventResult::PreventDefault)
        }
        winit::event::WindowEvent::Moved(_) => {
            queue_main(true);
            None
        }
        // a smaller / bigger window: the size may step down / back up (capped), the window itself stays
        winit::event::WindowEvent::Resized(_) => {
            queue_main(false);
            None
        }
        _ => {
            // first event, or Slint holds winit's own value again (Wayland makes the window anew on every show)
            if !SEEN.with(|s| s.replace(true)) || LAST.with(|l| l.get()).is_none_or(|(_, _, sf)| (sf - win.scale_factor()).abs() > 0.0005) {
                queue_main(true);
            }
            None
        }
    }
}

/// after this event: never change sizes from inside winit's callback. `may_resize`: see MAY_RESIZE
fn queue_main(may_resize: bool) {
    if may_resize {
        MAY_RESIZE.with(|m| m.set(true));
    }
    if QUEUED.with(|q| q.replace(true)) {
        return;
    }
    slint::Timer::single_shot(std::time::Duration::ZERO, || {
        QUEUED.with(|q| q.set(false));
        if let Some(a) = APP.with(|a| a.borrow().as_ref().and_then(|w| w.upgrade())) {
            fit_main(&a);
        }
    });
}

struct Screen {
    os: f64,
    mon: Option<(u32, u32)>,
    /// monitor rectangle (x, y, w, h), physical px
    rect: Option<(i32, i32, u32, u32)>,
    /// window outer position, maximized / fullscreen
    pos: Option<(i32, i32)>,
    fixed: bool,
}

fn screen_of(win: &slint::Window) -> Option<Screen> {
    win.with_winit_window(|w| {
        let m = w.current_monitor();
        Screen {
            os: w.scale_factor(),
            mon: m.as_ref().map(|m| (m.size().width, m.size().height)),
            rect: m.as_ref().map(|m| (m.position().x, m.position().y, m.size().width, m.size().height)),
            pos: w.outer_position().ok().map(|p| (p.x, p.y)),
            fixed: w.is_maximized() || w.fullscreen().is_some(),
        }
    })
}

fn fit_main(a: &crate::AppWindow) {
    let win = a.window();
    let may_resize = MAY_RESIZE.with(|m| m.replace(false));
    let Some(s) = screen_of(win) else { return };
    if let Some((w, h)) = s.mon {
        SCREEN.with(|x| x.set(Some((w, h, s.os))));
    }
    let k = factor(&pick(), s.mon, s.os);
    let have = win.scale_factor();
    let first = LAST.with(|l| l.get()).is_none();
    // Echo's size wanted before this: the window keeps its size in Echo's pixels when it changes (another screen)
    let k_old = LAST.with(|l| l.get()).map_or(have as f64 / s.os, |(_, k, _)| k);
    let mut phys = win.size();
    if may_resize && (k - k_old).abs() > 0.001 && !s.fixed && phys.width > 0 && phys.height > 0 {
        // another screen: same size in Echo's pixels as before (like Windows does for its own scale), but never
        // larger than the screen
        let r = k / k_old;
        let (mw, mh) = s.mon.unwrap_or((u32::MAX, u32::MAX));
        let nw = ((phys.width as f64 * r).round() as u32).min((mw as f64 * 0.96) as u32).max(200);
        let nh = ((phys.height as f64 * r).round() as u32).min((mh as f64 * 0.92) as u32).max(150);
        if nw != phys.width || nh != phys.height {
            win.set_size(slint::PhysicalSize::new(nw, nh));
            if let (Some((mx, my, mw, mh)), Some((x, y))) = (s.rect, s.pos) {
                let to = if let Some((cx, cy)) = dragging() {
                    // dragged onto another screen: grow / shrink around the mouse, so the bar stays under it
                    let f = |c: i32, p: i32, old: u32, new: u32| c - ((c - p) as f64 * new as f64 / old.max(1) as f64).round() as i32;
                    Some((f(cx, x, phys.width, nw), f(cy, y, phys.height, nh)))
                } else if first {
                    // first start (Linux: the window grew after it opened): past the right / bottom edge -> back in
                    Some((x.min(mx + mw as i32 - nw as i32 - 8).max(mx), y.min(my + mh as i32 - nh as i32 - 48).max(my)))
                } else {
                    None
                };
                if let Some((nx, ny)) = to.filter(|p| *p != (x, y)) {
                    win.set_position(slint::PhysicalPosition::new(nx, ny));
                }
            }
            phys = slint::PhysicalSize::new(nw, nh);
        }
    }
    // as far as the window has room for it (a small window steps the size down, see ROOM)
    let ke = capped(k, (phys.width, phys.height), s.os);
    let want = (s.os * ke) as f32;
    LAST.with(|l| l.set(Some((s.os, k, want))));
    if (want - have).abs() < 0.0005 {
        return;
    }
    echo_core::recorder::log_line(&format!(
        "ui scale: screen {:?} at {:.0} %, Echo size {:.0} % ({}), window {}x{} -> scale {want:.3} (was {have:.3})",
        s.mon,
        s.os * 100.0,
        k * 100.0,
        pick(),
        phys.width,
        phys.height
    ));
    win.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: want });
    win.dispatch_event(slint::platform::WindowEvent::Resized { size: win.size().to_logical(want) });
    win.request_redraw();
}

/// Windows: the left mouse button is held (the window is being dragged): the mouse, screen px
fn dragging() -> Option<(i32, i32)> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
        use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut pt = POINT { x: 0, y: 0 };
        // SAFETY: plain Win32 calls writing into a local POINT
        if unsafe { GetAsyncKeyState(VK_LBUTTON as i32) } as u16 & 0x8000 != 0 && unsafe { GetCursorPos(&mut pt) } != 0 {
            return Some((pt.x, pt.y));
        }
    }
    None
}

/// the main window's Echo size right now (for code that reads winit's scale instead of Slint's)
pub fn extra() -> f64 {
    LAST.with(|l| l.get()).map_or(1.0, |(os, _, sf)| sf as f64 / os)
}

/// Settings > Appearance > Size: true = it was this setting
pub fn set(key: &str, val: &str) -> bool {
    if key != "ui_scale" {
        return false;
    }
    let v = if PICKS.iter().any(|(k, _)| *k == val) { val } else { "auto" };
    PICK.with(|p| *p.borrow_mut() = Some(v.to_string()));
    crate::lib_fx::ui_set("ui_scale", if v == "auto" { serde_json::Value::Null } else { serde_json::json!(v) });
    queue_main(false); // like a zoom: the window stays, what's in it changes size
    queue_popup();
    true
}

/// the Settings row's hint: what Auto (or the pick) gives on this screen
pub fn hint() -> String {
    let p = pick();
    // the window's screen right now (the page can be made before the first fit)
    let live = APP.with(|a| a.borrow().as_ref().and_then(|w| w.upgrade())).and_then(|a| screen_of(a.window())).and_then(|s| s.mon.map(|(w, h)| (w, h, s.os)));
    let scr = live.or(SCREEN.with(|x| x.get()));
    #[cfg(windows)]
    let scr = scr.or_else(primary_screen);
    let (mon, os) = scr.map_or((None, 1.0), |(w, h, os)| (Some((w, h)), os));
    let k = factor(&p, mon, os);
    let pct = (k * 100.0).round();
    let screen = match (fake_screen().or(mon), os) {
        (Some((w, h)), os) if (os - 1.0).abs() < 0.01 => format!("{w} x {h}"),
        (Some((w, h)), os) => format!("{w} x {h} at {:.0} %", os * 100.0),
        _ => "this screen".into(),
    };
    let mut s = if p == "auto" {
        format!("Echo grows a bit on big screens so it isn't tiny. On this one ({screen}): {pct:.0} %.")
    } else {
        format!("{pct:.0} % on every screen. Auto would pick {:.0} % here ({screen}).", (factor("auto", mon, os) * 100.0).round())
    };
    // QA B10: a small window steps the size down so everything still fits - say so
    let win = APP.with(|a| a.borrow().as_ref().and_then(|w| w.upgrade())).map(|a| a.window().size());
    if let Some(w) = win.filter(|w| w.width > 0) {
        let ke = capped(k, (w.width, w.height), os);
        if ke < k - 0.001 {
            // no number: this text is made when the page is, not again on every resize
            s.push_str(" This window is small for that, so Echo is a bit smaller until the window is bigger.");
        }
    }
    s
}

/// popup.rs: the in-game popup gets the same size (from its own screen); its winit hook swallows winit's scale
pub fn hook_popup(w: &crate::PopupWin) {
    POPUP.with(|p| *p.borrow_mut() = Some(w.as_weak()));
    if user_fixed() {
        return;
    }
    w.window().on_winit_window_event(|_, ev| match ev {
        winit::event::WindowEvent::ScaleFactorChanged { .. } => {
            queue_popup();
            EventResult::PreventDefault
        }
        _ => EventResult::Propagate,
    });
}

fn queue_popup() {
    if POP_QUEUED.with(|q| q.replace(true)) {
        return;
    }
    slint::Timer::single_shot(std::time::Duration::ZERO, || {
        POP_QUEUED.with(|q| q.set(false));
        if let Some(w) = POPUP.with(|p| p.borrow().as_ref().and_then(|w| w.upgrade())) {
            fit_popup(&w);
        }
    });
}

/// right now (popup.rs place(), before it cuts the window to the card): the popup's scale = system x Echo's size.
/// Its window has a fixed logical size (340 x 58), so Slint sizes the window itself from the new scale.
pub fn fit_popup(w: &crate::PopupWin) {
    if user_fixed() {
        return;
    }
    let win = w.window();
    let Some(s) = screen_of(win) else { return };
    let want = (s.os * factor(&pick(), s.mon, s.os)) as f32;
    let have = win.scale_factor();
    if (want - have).abs() < 0.0005 {
        return;
    }
    win.dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: want });
    // the window's own size: 340 x 58 logical px (popup.slint) at the new scale
    let size = slint::LogicalSize::new(340.0, 58.0).to_physical(want);
    win.set_size(size);
    win.dispatch_event(slint::platform::WindowEvent::Resized { size: size.to_logical(want) });
    win.request_redraw();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_sizes() {
        let a = |w, h, os| factor("auto", Some((w, h)), os);
        assert_eq!(a(1920, 1080, 1.0), 1.0);
        assert_eq!(a(2560, 1440, 1.0), 1.15);
        assert_eq!(a(2560, 1440, 1.25), 1.0); // Windows already scales it: not twice
        assert_eq!(a(3840, 2160, 1.5), 1.15);
        assert_eq!(a(3840, 2160, 1.0), 1.5);
        assert_eq!(a(3440, 1440, 1.0), 1.15); // ultrawide: by height
        assert_eq!(a(2560, 1080, 1.0), 1.0);
        assert_eq!(a(1366, 768, 1.0), 1.0); // small laptop: as before
        assert_eq!(a(2560, 1600, 1.0), 1.15); // 16:10: 2560 / 1920 = 1.33 wide -> half of it
        assert_eq!(factor("auto", None, 1.0), 1.0);
        assert_eq!(factor("150", Some((1920, 1080)), 1.0), 1.5);
        assert_eq!(factor("90", Some((2560, 1440)), 1.0), 0.9);
    }

    #[test]
    fn small_window_steps_down() {
        assert_eq!(capped(1.5, (1500, 900), 1.0), 1.15); // QA B10: was 1000x600 of room, now 1304x783
        assert_eq!(capped(1.5, (1920, 1080), 1.0), 1.5); // full 1080p screen: 1280x720 of room
        assert_eq!(capped(1.25, (1280, 720), 1.0), 1.0); // never under 100 %
        assert_eq!(capped(1.33, (1996, 1198), 1.0), 1.33); // 1440p Auto, Echo's first window
        assert_eq!(capped(1.33, (2560, 1392), 1.0), 1.33); // 1440p maximized
        assert_eq!(capped(0.9, (800, 600), 1.0), 0.9); // 90 % stays
        assert_eq!(capped(1.33, (2000, 1200), 1.25), 1.25); // system 125 %: room counted in its own scale
    }
}
