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
            // popup at its default spot, top left). Echo's spot, set right before every show: primary monitor's
            // right edge - 380, top + 24 (scaled).
            use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SetWindowPos, SM_CXSCREEN, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER};
            let sf = w.window().scale_factor();
            unsafe {
                let x = GetSystemMetrics(SM_CXSCREEN) - (380.0 * sf) as i32;
                SetWindowPos(hwnd as _, std::ptr::null_mut(), x, (24.0 * sf) as i32, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
        echo_core::winutil::overlay_visible(hwnd, on);
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
}

/// UI thread: the popup window, made on first use (hidden right after making on Windows).
fn ensure() -> Option<(crate::PopupWin, isize)> {
    if let Some(x) = WIN.with(|w| w.borrow().as_ref().map(|(w, h)| (w.clone_strong(), *h))) {
        return Some(x);
    }
    let w = crate::PopupWin::new().ok()?;
    #[cfg(windows)]
    RETRIED.with(|r| r.set(false));
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
            // top right of the main monitor (24 px from the top, like Echo)
            let (sw, sf) = unsafe {
                (windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows_sys::Win32::UI::WindowsAndMessaging::SM_CXSCREEN), w.window().scale_factor())
            };
            // Echo (make_toast): x = right edge - 380, y = 24 (was 14 here)
            w.window().set_position(slint::PhysicalPosition::new(sw - (380.0 * sf) as i32, (24.0 * sf) as i32));
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
                    echo_core::winutil::make_overlay(h);
                    echo_core::winutil::overlay_visible(h, false);
                    WIN.with(|x| {
                        if let Some((_, slot)) = x.borrow_mut().as_mut() {
                            if *slot == 0 {
                                *slot = h;
                            }
                        }
                    });
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
