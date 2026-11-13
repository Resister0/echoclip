//! Chat A: in-game "Clip saved" popup = Echo's toast()/toast_keeper()/make_toast() (src-tauri main.rs) as a Slint
//! window (ui/popup.slint). Windows: never takes focus (WS_EX_NOACTIVATE + TOOLWINDOW + TOPMOST from Echo's
//! winutil::make_overlay; first show with SW_SHOWNOACTIVATE via the winit "active: false" attribute), top right of
//! the main monitor, ~2.6 s. Kept alive while a game ran < 30 s ago or a popup < 60 s ago, else destroyed (memory).
//! Linux: desktop notification (Echo's linuxutil::notify) - ECHO_POPUP=window shows this window instead (cloud test).
use crate::rec_core::rec;
use slint::ComponentHandle;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// read by the winit attributes hook (main.rs select_backend): the next window made is the popup
pub static POPUP_NEXT: AtomicBool = AtomicBool::new(false);
static GEN: AtomicU64 = AtomicU64::new(0);
/// when the last popup was asked for (unix seconds)
static AT: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static WIN: RefCell<Option<(crate::PopupWin, isize)>> = const { RefCell::new(None) };
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
fn use_window() -> bool {
    cfg!(windows) || std::env::var("ECHO_POPUP").is_ok_and(|v| v == "window")
}

/// Pick the winit backend with one hook: the popup window is made inactive (never steals focus from a game) and
/// see-through. Every other window keeps its attributes untouched. Call first thing in main().
pub fn select_backend() {
    // test hook: ECHO_PLAIN_BACKEND=1 = Slint's default selection (to compare video behaviour)
    if std::env::var("ECHO_PLAIN_BACKEND").is_ok() {
        return;
    }
    let r = slint::BackendSelector::new()
        .with_winit_window_attributes_hook(|attrs| {
            if POPUP_NEXT.swap(false, Ordering::SeqCst) {
                attrs.with_active(false).with_transparent(true).with_decorations(false).with_resizable(false)
            } else {
                // Linux: Wayland app_id / X11 WM_CLASS "link.echoclip.Echo" = the desktop file name (linux.rs, Flatpak id). KWin's Alt+Tab
                // only finds the icon through a desktop file named exactly like the app_id (10 Nov 2026: was "echo" - taskbar OK, Alt+Tab blank)
                // was: "echo" = matches ~/.local/share/applications/echo.desktop
                // (build.sh, Icon=echo-clips). Without it KDE showed the generic Wayland "W" in the taskbar (9 Nov 2026)
                #[cfg(target_os = "linux")]
                let attrs = {
                    use slint::winit_030::winit::platform::{wayland::WindowAttributesExtWayland, x11::WindowAttributesExtX11};
                    let a = WindowAttributesExtWayland::with_name(attrs, "link.echoclip.Echo", "echo");
                    WindowAttributesExtX11::with_name(a, "link.echoclip.Echo", "echo")
                };
                // test hook (13 Nov 2026): ECHO_OFFSCREEN=<w>x<h> = the window opens far off screen, inactive and
                // without a taskbar button (snapshots via ECHO_CARD_SNAP while a game runs; size: rec_core::run)
                #[cfg(windows)]
                let attrs = if std::env::var_os("ECHO_OFFSCREEN").is_some() {
                    use slint::winit_030::winit::{dpi::PhysicalPosition, platform::windows::WindowAttributesExtWindows};
                    attrs.with_active(false).with_skip_taskbar(true).with_position(PhysicalPosition::new(-20000, -20000))
                } else {
                    attrs
                };
                attrs
            }
        })
        .select();
    if let Err(e) = r {
        eprintln!("backend: {e}");
    }
}

/// Any thread. `icon` = data:image/png;base64 (the game's icon) or "" (tick / cross).
pub fn show(title: &str, sub: &str, ok: bool, icon: &str) {
    if !rec().is_some_and(|r| r.settings().show_overlay) {
        return;
    }
    AT.store(now(), Ordering::Relaxed);
    if !use_window() {
        #[cfg(target_os = "linux")]
        echo_core::linuxutil::notify(title, sub, ok);
        return;
    }
    show_window(title, sub, ok, icon);
}

/// test hook: ECHO_POPUP_TEST=1 runs ONLY the popup (no main window, recorder, hotkeys or tray - safe next to a
/// running Echo): three popups 3.5 s apart (short, "Saving clip…", long text), then quits. Set TEMP to a scratch
/// folder: the log lines go to %TEMP%\clipper_ffmpeg.log. true = it ran (main() returns).
pub fn test_hook() -> bool {
    if std::env::var("ECHO_POPUP_TEST").is_err() {
        return false;
    }
    let shows: [(&str, &str, bool); 3] =
        [("Clip saved", "Last 30 s", true), ("Saving clip…", "Last 30 s", true), ("Couldn't save the clip", "The recorder isn't running - a much longer line here", false)];
    for (i, (t, s, ok)) in shows.into_iter().enumerate() {
        slint::Timer::single_shot(Duration::from_millis(800 + 3500 * i as u64), move || show_window(t, s, ok, ""));
    }
    slint::Timer::single_shot(Duration::from_millis(800 + 3500 * 3), || {
        let _ = slint::quit_event_loop();
    });
    let _ = slint::run_event_loop_until_quit();
    true
}

fn show_window(title: &str, sub: &str, ok: bool, icon: &str) {
    let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let (title, sub, icon) = (title.to_string(), sub.to_string(), icon.to_string());
    let png = icon.strip_prefix("data:image/png;base64,").and_then(echo_core::recorder::unb64);
    let _ = slint::invoke_from_event_loop(move || {
        let Some((w, hwnd)) = ensure() else { return };
        w.set_head(title.into());
        w.set_sub(sub.into());
        w.set_ok(ok);
        let img = png.and_then(|b| image::load_from_memory_with_format(&b, image::ImageFormat::Png).ok()).map(|i| {
            let i = i.to_rgba8();
            slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(i.as_raw(), i.width(), i.height()))
        });
        w.set_has_icon(ok && img.is_some());
        if let Some(i) = img {
            w.set_game_icon(i);
        }
        if let Some((a, b)) = crate::rec_core::accent() {
            w.set_grad_a(a);
            w.set_grad_b(b);
        }
        w.set_on(false);
        visible(&w, hwnd, true);
        let w2 = w.as_weak();
        slint::Timer::single_shot(Duration::from_millis(30), move || {
            if let Some(w) = w2.upgrade() {
                w.set_on(true);
            }
        });
        let w3 = w.as_weak();
        slint::Timer::single_shot(Duration::from_millis(2300), move || {
            if GEN.load(Ordering::SeqCst) == gen {
                if let Some(w) = w3.upgrade() {
                    w.set_on(false);
                }
            }
        });
        let w4 = w.as_weak();
        slint::Timer::single_shot(Duration::from_millis(2600), move || {
            if GEN.load(Ordering::SeqCst) == gen {
                if let Some(w) = w4.upgrade() {
                    visible(&w, hwnd, false);
                }
            }
        });
    });
}

/// show / hide without activating (Windows: Echo's overlay_visible = SetWindowPos NOACTIVATE / SW_HIDE)
fn visible(w: &crate::PopupWin, hwnd: isize, on: bool) {
    // the handle often isn't there yet when ensure() runs (winit makes the window a moment after show()), which
    // skipped the overlay styles AND the position (reported: "notifs appear in wrong spots" = Windows' default
    // spot, top left). Fetch it now; the first time, make it Echo's no-focus overlay and remember it.
    #[cfg(windows)]
    SHOWING.with(|s| s.set(on));
    #[cfg(windows)]
    let hwnd = if hwnd != 0 {
        hwnd
    } else {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let h = match w.window().window_handle().window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
            _ => 0,
        };
        if h != 0 {
            echo_core::winutil::make_overlay(h);
            WIN.with(|x| {
                if let Some((_, slot)) = x.borrow_mut().as_mut() {
                    *slot = h;
                }
            });
        }
        h
    };
    #[cfg(windows)]
    if hwnd != 0 {
        if on {
            // reported: "notifs appear in wrong spots" - Slint's set_position at creation didn't stick (Windows put the
            // popup at its default spot, top left). Place it right before every show, cut to this text's card width.
            place(w, hwnd);
        }
        echo_core::winutil::overlay_visible(hwnd, on);
        if on {
            // once more after the first frames: moving to the main monitor can change the DPI (other scale), and
            // Slint only picks up the new scale factor on a later event-loop turn
            let ww = w.as_weak();
            slint::Timer::single_shot(Duration::from_millis(150), move || {
                if let Some(w) = ww.upgrade() {
                    place(&w, hwnd);
                }
            });
        }
        return;
    }
    // Windows, very first popup: the native window isn't made yet - try again on the next event-loop turn (once)
    #[cfg(windows)]
    if on && !RETRIED.with(|r| r.replace(true)) {
        let ww = w.as_weak();
        slint::Timer::single_shot(Duration::from_millis(1), move || {
            if let Some(w) = ww.upgrade() {
                visible(&w, 0, true);
            }
        });
        return;
    }
    let _ = hwnd;
    if on {
        let _ = w.show();
    } else {
        let _ = w.hide();
    }
}

#[cfg(windows)]
thread_local! {
    static RETRIED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// a popup is meant to be on screen right now (visible(on)); ensure()'s handle timer must not hide it
    static SHOWING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
#[cfg(windows)]
static GL_LOGGED: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static PLACE_LOGGED: AtomicBool = AtomicBool::new(false);

/// Windows: put the popup top right of the main monitor and cut the window to the card's rounded rectangle.
/// 11 Nov 2026: on a friend's PC (AMD, Echo 2.0.4) the see-through part of the popup drew BLACK - a box around and
/// left of the card (the window was 360x84 for a 220-340 x 58 card). See-through on Windows = winit's DWM blur-behind
/// + an OpenGL surface with alpha; whether DWM honours the alpha depends on the GPU driver's OpenGL present path.
/// With the window region (SetWindowRgn, physical pixels) nothing outside the card is drawn at all, see-through or not.
#[cfg(windows)]
fn place(w: &crate::PopupWin, hwnd: isize) {
    use windows_sys::Win32::Foundation::{POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{ClientToScreen, CreateRoundRectRgn, SetWindowRgn};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetClientRect, GetSystemMetrics, GetWindowRect, SetWindowPos, SM_CXSCREEN, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER};
    let h = hwnd as windows_sys::Win32::Foundation::HWND;
    // 1080p vs 1440p: the popup's scale = system x Echo's size (uiscale.rs), before the window is cut to the card
    crate::uiscale::fit_popup(w);
    let sf = w.window().scale_factor();
    let px = |v: f32| (v * sf).round() as i32;
    // window rect, client size, and where the client area starts inside the window (0,0 for Echo's frameless popup)
    let rects = || unsafe {
        let (mut r, mut c, mut p): (RECT, RECT, POINT) = (std::mem::zeroed(), std::mem::zeroed(), POINT { x: 0, y: 0 });
        GetWindowRect(h, &mut r);
        GetClientRect(h, &mut c);
        ClientToScreen(h, &mut p);
        (r, c.right - c.left, c.bottom - c.top, p.x - r.left, p.y - r.top)
    };
    let (_, cw, _, ox, oy) = rects();
    // the card's right edge 30 px from the main monitor's right edge, its top 34 px down (where it was before:
    // window at right - 380 / top 24 with the old 10 px margin)
    unsafe {
        SetWindowPos(h, std::ptr::null_mut(), GetSystemMetrics(SM_CXSCREEN) - px(30.0) - (ox + cw), px(34.0) - oy, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
    // moving can change the DPI and with it the size: read again
    let (r, cw, ch, ox, oy) = rects();
    let card = px(w.get_card_w()).clamp(1, cw.max(1));
    let d = px(28.0) + 2; // 2 x the card's 14 px corner radius
    // 1 px extra all round so the card's anti-aliased border isn't cut (CreateRoundRectRgn leaves out right / bottom)
    let ok = unsafe {
        let rgn = CreateRoundRectRgn(ox + cw - card - 1, oy - 1, ox + cw + 2, oy + ch + 2, d, d);
        !rgn.is_null() && SetWindowRgn(h, rgn, 1) != 0 // the system owns the region from here
    };
    if !PLACE_LOGGED.swap(true, Ordering::Relaxed) {
        let (os, gpu) = crate::specs::os_gpu();
        echo_core::recorder::log_line(&format!(
            "popup: {os}, GPU '{gpu}', scale {sf}, window {}x{} px (client {cw}x{ch} at +{ox}+{oy}), card {card}x{ch} px, cut to the card: {}, {}",
            r.right - r.left,
            r.bottom - r.top,
            if ok { "yes" } else { "FAILED" },
            see_through_info(h)
        ));
    }
}

/// what decides whether the popup's see-through part works: the window's pixel format (alpha bits, DWM composition),
/// DWM blur-behind (what winit uses for transparent windows) and the "Transparency effects" setting
#[cfg(windows)]
fn see_through_info(h: windows_sys::Win32::Foundation::HWND) -> String {
    use windows_sys::Win32::Graphics::Dwm::{DwmEnableBlurBehindWindow, DwmIsCompositionEnabled, DWM_BB_BLURREGION, DWM_BB_ENABLE, DWM_BLURBEHIND};
    use windows_sys::Win32::Graphics::Gdi::{CreateRectRgn, DeleteObject, GetDC, ReleaseDC};
    use windows_sys::Win32::Graphics::OpenGL::{DescribePixelFormat, GetPixelFormat, PFD_SUPPORT_COMPOSITION, PIXELFORMATDESCRIPTOR};
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    unsafe {
        let dc = GetDC(h);
        let n = GetPixelFormat(dc);
        let mut pfd: PIXELFORMATDESCRIPTOR = std::mem::zeroed();
        let pf = if n > 0 && DescribePixelFormat(dc, n, std::mem::size_of::<PIXELFORMATDESCRIPTOR>() as u32, &mut pfd) != 0 {
            format!("pixel format {n}: {} alpha bits, composition {}", pfd.cAlphaBits, if pfd.dwFlags & PFD_SUPPORT_COMPOSITION != 0 { "yes" } else { "no" })
        } else {
            format!("pixel format {n} (unknown)")
        };
        ReleaseDC(h, dc);
        let mut comp = 0;
        DwmIsCompositionEnabled(&mut comp);
        // the same call winit makes for a transparent window (empty blur region = see-through where alpha < 1);
        // made again here only to learn its result, which winit doesn't show
        let rgn = CreateRectRgn(0, 0, -1, -1);
        let bb = DWM_BLURBEHIND { dwFlags: DWM_BB_ENABLE | DWM_BB_BLURREGION, fEnable: 1, hRgnBlur: rgn, fTransitionOnMaximized: 0 };
        let hr = DwmEnableBlurBehindWindow(h, &bb);
        DeleteObject(rgn);
        let w = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let (k, v) = (w(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"), w("EnableTransparency"));
        let (mut val, mut size) = (0u32, 4u32);
        let te = match RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), v.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), &mut val as *mut u32 as *mut _, &mut size) {
            0 if val == 0 => "off",
            0 => "on",
            _ => "unknown",
        };
        format!("{pf}, DWM composition {}, blur-behind hr {hr:#x}, Transparency effects {te}", if comp != 0 { "on" } else { "off" })
    }
}

/// UI thread: the popup window, made on first use (hidden right after making on Windows).
fn ensure() -> Option<(crate::PopupWin, isize)> {
    if let Some(x) = WIN.with(|w| w.borrow().as_ref().map(|(w, h)| (w.clone_strong(), *h))) {
        return Some(x);
    }
    let w = crate::PopupWin::new().ok()?;
    crate::uiscale::hook_popup(&w);
    #[cfg(windows)]
    RETRIED.with(|r| r.set(false));
    // log which OpenGL driver draws the popup (once per run): see-through depends on it
    #[cfg(windows)]
    let _ = w.window().set_rendering_notifier(|state, api| {
        if let (slint::RenderingState::RenderingSetup, slint::GraphicsAPI::NativeOpenGL { get_proc_address }) = (state, api) {
            if !GL_LOGGED.swap(true, Ordering::Relaxed) {
                use glow::HasContext;
                let gl = unsafe { glow::Context::from_loader_function_cstr(|s| get_proc_address(s)) };
                let (r, v) = unsafe { (gl.get_parameter_string(glow::RENDERER), gl.get_parameter_string(glow::VERSION)) };
                echo_core::recorder::log_line(&format!("popup: drawn with OpenGL '{r}' ({v})"));
            }
        }
    });
    POPUP_NEXT.store(true, Ordering::SeqCst);
    let _ = w.show();
    POPUP_NEXT.store(false, Ordering::SeqCst);
    #[allow(unused_mut)]
    let mut hwnd = 0isize;
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(h) = w.window().window_handle().window_handle() {
            if let RawWindowHandle::Win32(h) = h.as_raw() {
                hwnd = h.hwnd.get();
            }
        }
        if hwnd != 0 {
            echo_core::winutil::make_overlay(hwnd);
            // top right of the main monitor, cut to the card (see place)
            place(&w, hwnd);
            echo_core::winutil::overlay_visible(hwnd, false);
        } else {
            // the handle often isn't there yet right after show(): the empty, see-through popup then stayed shown -
            // in Alt+Tab as "Echo popup" from startup until the first real popup (10 Nov 2026). Look again
            // every 50 ms (up to 5 s); once it's there: no-focus tool window (out of Alt+Tab / taskbar) + hidden.
            let ww = w.as_weak();
            let tries = std::rc::Rc::new(std::cell::Cell::new(0u32));
            let t = std::rc::Rc::new(slint::Timer::default());
            let t2 = t.clone();
            t.start(slint::TimerMode::Repeated, Duration::from_millis(50), move || {
                tries.set(tries.get() + 1);
                let Some(w) = ww.upgrade() else { t2.stop(); return };
                use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                let h = match w.window().window_handle().window_handle().map(|h| h.as_raw()) {
                    Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
                    _ => 0,
                };
                if h != 0 {
                    // 11 Nov 2026: this hid the window even while the very first popup was showing (made on first use,
                    // e.g. the first save outside a game: that popup never appeared). visible() may have set it up
                    // already; else set it up here and keep it shown if a popup is up right now.
                    let known = WIN.with(|x| x.borrow().as_ref().is_some_and(|(_, slot)| *slot != 0));
                    if !known {
                        echo_core::winutil::make_overlay(h);
                        place(&w, h);
                        echo_core::winutil::overlay_visible(h, SHOWING.with(|s| s.get()));
                        WIN.with(|x| {
                            if let Some((_, slot)) = x.borrow_mut().as_mut() {
                                *slot = h;
                            }
                        });
                    }
                    t2.stop();
                } else if tries.get() >= 100 {
                    let _ = w.hide();
                    t2.stop();
                }
            });
        }
    }
    #[cfg(not(windows))]
    {
        let _ = w.hide();
        w.window().set_position(slint::LogicalPosition::new(1500.0, 14.0));
    }
    WIN.with(|x| *x.borrow_mut() = Some((w.clone_strong(), hwnd)));
    Some((w, hwnd))
}

/// Echo's toast_keeper: ready while a game runs (so "Clip saved" is instant in games), gone a minute after.
pub fn keeper() {
    if !use_window() {
        return;
    }
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_secs(5));
        let Some(r) = rec() else { return };
        let game = r.last_game.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|(_, t)| t.elapsed().as_secs() < 30);
        let recent = now().saturating_sub(AT.load(Ordering::Relaxed)) < 60;
        let want = r.settings().show_overlay && (game || recent);
        let _ = slint::invoke_from_event_loop(move || {
            let has = WIN.with(|w| w.borrow().is_some());
            if want && !has {
                let _ = ensure();
            } else if !want && has {
                if let Some((w, _)) = WIN.with(|w| w.borrow_mut().take()) {
                    let _ = w.hide();
                }
            }
        });
    });
}
