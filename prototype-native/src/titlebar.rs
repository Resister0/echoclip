//! Chat A: Echo's own title bar (William's pick "B": title bar + header in one 52 px row). No Windows frame:
//! empty bar space drags the window (native move = Windows snap / Aero shake still work), double-click maximizes,
//! own minimize / maximize / close (close = rec_core's close-to-tray rule), 5 px invisible edges resize.
//! ECHO_NATIVE_FRAME=1 brings the normal frame back (fallback if something misbehaves on a PC).
use crate::AppWindow;
use slint::winit_030::{winit, WinitWindowAccessor};
use slint::ComponentHandle;

/// Windows: a frameless window keeps its invisible 8 px resize border, so MAXIMIZED it hangs 8 px past every screen
/// edge (the bottom 8 px under the taskbar, the bar's top cut). Standard fix: while maximized, WM_NCCALCSIZE makes the
/// client area exactly the monitor's work area (the overhang becomes non-client, off screen). Subclass = runs before
/// winit's own window procedure; everything else is passed on unchanged.
#[cfg(windows)]
fn fix_maximized_overhang(app: &AppWindow) {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowLongW, IsZoomed, GWL_STYLE, NCCALCSIZE_PARAMS, WM_NCCALCSIZE, WS_THICKFRAME};
    unsafe extern "system" fn proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> LRESULT {
        // only a normal maximized window (still resizable): player fullscreen drops WS_THICKFRAME and must keep
        // the whole monitor (no taskbar gap)
        let sizable = (GetWindowLongW(h, GWL_STYLE) as u32 & WS_THICKFRAME) != 0;
        if msg == WM_NCCALCSIZE && wp != 0 && sizable && IsZoomed(h) != 0 {
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            let p = lp as *mut NCCALCSIZE_PARAMS;
            let r = (*p).rgrc[0];
            // going fullscreen: Windows still says "maximized" for a moment while the proposed rect is already the
            // whole monitor - leave it alone (clamping it to the work area showed the video at the maximized size
            // for ~80 ms - William's "half fullscreen" frame)
            let fullscreen = |m: RECT| r.left == m.left && r.top == m.top && r.right == m.right && r.bottom == m.bottom;
            if GetMonitorInfoW(MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST), &mut mi) != 0 && !fullscreen(mi.rcMonitor) {
                (*p).rgrc[0] = RECT { left: mi.rcWork.left, top: mi.rcWork.top, right: mi.rcWork.right, bottom: mi.rcWork.bottom };
                return 0;
            }
        }
        DefSubclassProc(h, msg, wp, lp)
    }
    let Some(hwnd) = hwnd_of(app).map(|h| h as HWND) else { return };
    unsafe {
        SetWindowSubclass(hwnd, Some(proc), 0xEC40, 0);
    }
}

/// Windows: Slint's window is created see-through (winit: DWM blur-behind with an empty region), so every spot Slint
/// hasn't painted yet shows the DESKTOP - e.g. for ~80 ms where the video window was when E opens the editor (measured
/// with ui.ps1 burst, William: "flickers, shows my desktop background briefly"). Nothing needs see-through any more
/// (winvideo set_behind is unused), so it's switched off: unpainted spots are black for a frame instead.
/// ECHO_SEE_THROUGH=1 keeps the old behaviour.
#[cfg(windows)]
fn no_see_through(app: &AppWindow) {
    use windows_sys::Win32::Graphics::Dwm::{DwmEnableBlurBehindWindow, DWM_BB_ENABLE, DWM_BLURBEHIND};
    if std::env::var("ECHO_SEE_THROUGH").is_ok() {
        return;
    }
    let Some(hwnd) = hwnd_of(app).map(|h| h as windows_sys::Win32::Foundation::HWND) else { return };
    let bb = DWM_BLURBEHIND { dwFlags: DWM_BB_ENABLE, fEnable: 0, hRgnBlur: std::ptr::null_mut(), fTransitionOnMaximized: 0 };
    let hr = unsafe { DwmEnableBlurBehindWindow(hwnd, &bb) };
    echo_core::recorder::log_line(&format!("window: see-through off (hwnd {:?}, hr {hr:#x})", hwnd));
}

#[cfg(windows)]
thread_local! {
    static HOOK_TIMER: slint::Timer = slint::Timer::default();
}

/// William (9 Nov): the taskbar showed Windows' blank icon - winit only sets the window's SMALL icon (Slint's
/// `icon:`), the taskbar uses the BIG one. It comes from the exe's own icon (build.rs embeds echo.ico as icon 1).
#[cfg(windows)]
fn big_icon(app: &AppWindow) {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{LoadImageW, SendMessageW, ICON_BIG, IMAGE_ICON, LR_DEFAULTSIZE, WM_SETICON};
    let Some(hwnd) = hwnd_of(app).map(|h| h as windows_sys::Win32::Foundation::HWND) else { return };
    unsafe {
        let h = LoadImageW(GetModuleHandleW(std::ptr::null()), 1 as _, IMAGE_ICON, 0, 0, LR_DEFAULTSIZE);
        if !h.is_null() {
            SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, h as isize);
        } else {
            echo_core::recorder::log_line("window: no icon in the exe (built without build.rs resources?)");
        }
    }
}

/// the native window handle, once winit has made the window (None before that)
#[cfg(windows)]
fn hwnd_of(app: &AppWindow) -> Option<isize> {
    match raw_window_handle::HasWindowHandle::window_handle(&app.window().window_handle()).ok()?.as_raw() {
        raw_window_handle::RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// Linux (Wayland/X11): after drag_window / drag_resize_window the compositor runs the move and the button release
/// never reaches the app, so Slint kept the pressed TouchArea grabbed and routed EVERY later click to it -> the whole
/// window acted as the title bar (9 Nov 2026, Nobara KDE Wayland). Hand Slint the missing release. Windows: no-op.
fn end_grab(_a: &AppWindow) {
    #[cfg(not(windows))]
    {
        let w = _a.as_weak();
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            if let Some(a) = w.upgrade() {
                a.window().dispatch_event(slint::platform::WindowEvent::PointerReleased {
                    position: slint::LogicalPosition::new(1.0, 1.0),
                    button: slint::platform::PointerEventButton::Left,
                });
            }
        });
    }
}

pub fn wire(app: &AppWindow) {
    let custom = std::env::var("ECHO_NATIVE_FRAME").is_err();
    app.set_custom_frame(custom);
    app.set_drag_on_move(cfg!(not(windows)));
    // Windows hooks need the native window, which winit makes only after the event loop shows it (a 0 ms timer was
    // too early: both hooks silently did nothing). Try every 100 ms until it exists, then once.
    #[cfg(windows)]
    {
        let w = app.as_weak();
        let tries = std::cell::Cell::new(0u32);
        HOOK_TIMER.with(|t| {
            t.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(100), move || {
                let Some(a) = w.upgrade() else { return };
                tries.set(tries.get() + 1);
                if hwnd_of(&a).is_none() && tries.get() < 100 {
                    return;
                }
                no_see_through(&a);
                if custom {
                    fix_maximized_overhang(&a);
                }
                big_icon(&a);
                echo_core::recorder::log_line(&format!("window: hooks done after {} tries", tries.get()));
                // run once - and stop waking up every 100 ms (it used to keep ticking for the whole session)
                HOOK_TIMER.with(|t| t.stop());
            });
        });
    }
    let w = app.as_weak();
    app.on_win_act(move |what| {
        let Some(a) = w.upgrade() else { return };
        let win = a.window();
        if std::env::var("ECHO_DEBUG_BAR").is_ok() {
            eprintln!("titlebar: {what}");
        }
        match what.as_str() {
            "min" => win.set_minimized(true),
            "max" => {
                win.set_maximized(!win.is_maximized());
                a.set_win_max(win.is_maximized());
            }
            "close" => {
                let _ = crate::rec_core::close_requested(&a);
            }
            "drag" => {
                win.with_winit_window(|ww| {
                    let _ = ww.drag_window();
                });
                // Linux: compositor keeps the button release -> see end_grab()
                end_grab(&a);
            }
            r if r.starts_with("resize:") => {
                use winit::window::ResizeDirection as D;
                let d = match &r[7..] {
                    "n" => D::North,
                    "s" => D::South,
                    "e" => D::East,
                    "w" => D::West,
                    "ne" => D::NorthEast,
                    "nw" => D::NorthWest,
                    "se" => D::SouthEast,
                    _ => D::SouthWest,
                };
                win.with_winit_window(|ww| {
                    let _ = ww.drag_resize_window(d);
                });
                end_grab(&a);
            }
            _ => {}
        }
    });
    // maximized by Windows itself (snap to the top, Win+Up, double-click handled by us): keep the button icon right
    let t = Box::leak(Box::new(slint::Timer::default()));
    let w = app.as_weak();
    // window height for the update card + Home (win-hh): set from here, because reading root.height inside the
    // layout made a binding loop (Slint: "deprecated, may panic at runtime")
    fn win_h(a: &AppWindow) {
        let h = a.window().size().to_logical(a.window().scale_factor()).height;
        if h > 0.0 && (a.get_win_hh() - h).abs() > 0.5 {
            a.set_win_hh(h);
        }
    }
    win_h(app);
    t.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(400), move || {
        if let Some(a) = w.upgrade() {
            let m = a.window().is_maximized();
            if a.get_win_max() != m {
                a.set_win_max(m);
            }
            win_h(&a);
        }
    });
}
