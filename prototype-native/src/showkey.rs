//! 2.0.2: the Show / hide Echo hotkey (Settings > Hotkeys, `show_key`, off by default).
//! One key toggles: Echo hidden in the tray / minimized / behind other windows -> shown, raised and focused;
//! Echo already in front -> back to the tray (like the window's close button with "close to tray").
//! - showing goes through rec_core::show_window (the tray's Open does the same), so the low-memory tray mode
//!   comes back the same way (main.rs's tick resumes the library once the window is visible)
//! - shown with no clip / editor open: Echo opens on Home, which shows the newest clip big
//! - Windows: SetForegroundWindow only works for the foreground process; when it didn't take, the
//!   attach-thread-input trick (once, small)
//! - Linux X11: winit's focus_window (_NET_ACTIVE_WINDOW)
//! - Linux Wayland: xdg_activation_v1 on the window's own wl_display. KWin's focus stealing prevention only
//!   lets a window come forward with a valid token: the KDE shortcut portal sends one with the key press
//!   (hotkey_linux.rs activation_token(), newer portals only). Without one (evdev keys, older portal) Echo asks
//!   for its own token; KWin usually turns that down and only flashes Echo's taskbar entry - a window that was
//!   hidden in the tray still opens (a new window), but may open behind the game.
use crate::AppWindow;
use slint::ComponentHandle;

/// hotkey id (beyond seconds, next to rec_core's BOOKMARK / SESSION / MIC_MUTE)
pub const SHOW: u32 = u32::MAX - 3;

/// The key was pressed (hotkey thread): toggle on the UI thread.
pub fn pressed() {
    #[cfg(target_os = "linux")]
    let tok = echo_core::hotkey::activation_token();
    #[cfg(not(target_os = "linux"))]
    let tok: Option<String> = None;
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(a) = crate::UI.with(|u| u.borrow().as_ref().and_then(|w| w.upgrade())) {
            toggle(&a, tok);
        }
    });
}

/// What the key does now (pure, for the tests): true = hide, false = show.
fn wants_hide(visible: bool, minimized: bool, focused: bool) -> bool {
    visible && !minimized && focused
}

fn toggle(a: &AppWindow, tok: Option<String>) {
    let w = a.window();
    if wants_hide(w.is_visible(), w.is_minimized(), focused(a)) {
        hide(a);
    } else {
        show(a, tok);
    }
}

/// Echo's window has the keyboard (= the user looks at it)
fn focused(a: &AppWindow) -> bool {
    #[cfg(windows)]
    {
        hwnd(a).is_some_and(|h| unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() } == h)
    }
    #[cfg(not(windows))]
    {
        use slint::winit_030::WinitWindowAccessor;
        a.window().with_winit_window(|w| w.has_focus()).unwrap_or(false)
    }
}

fn hide(a: &AppWindow) {
    if crate::tray::exists() {
        // the close button's tray path (rec_core::close_requested), without its "quit when close to tray is off":
        // a hotkey never quits Echo
        crate::rec_core::hide_hook(a);
        let _ = a.hide();
    } else {
        // no tray icon to come back from (GNOME without the AppIndicator extension): minimize instead
        a.window().set_minimized(true);
    }
}

fn show(a: &AppWindow, tok: Option<String>) {
    let busy = a.get_player_open() || a.global::<crate::Ed>().get_open();
    crate::rec_core::show_window(a);
    if a.window().is_minimized() {
        a.window().set_minimized(false);
    }
    if !busy {
        a.invoke_page("Home".into()); // Home = the newest clip, big (main.rs's on_page fills it)
    }
    raise(a, tok);
}

#[cfg(windows)]
fn hwnd(a: &AppWindow) -> Option<windows_sys::Win32::Foundation::HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let h = a.window().window_handle();
    let h = h.window_handle().ok()?;
    match h.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as windows_sys::Win32::Foundation::HWND),
        _ => None,
    }
}

/// show_window already called SetForegroundWindow; Windows ignores it when another app (the game) is in front
/// and Echo didn't get the last input. Then: join the front window's input queue for a moment and try again.
#[cfg(windows)]
fn raise(a: &AppWindow, _tok: Option<String>) {
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::WindowsAndMessaging::{BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow};
    let Some(h) = hwnd(a) else { return };
    unsafe {
        let fg = GetForegroundWindow();
        if fg == h {
            return;
        }
        let them = GetWindowThreadProcessId(fg, std::ptr::null_mut());
        let me = GetCurrentThreadId();
        let joined = them != 0 && them != me && AttachThreadInput(me, them, 1) != 0;
        BringWindowToTop(h);
        SetForegroundWindow(h);
        if joined {
            AttachThreadInput(me, them, 0);
        }
        if GetForegroundWindow() != h {
            echo_core::recorder::log_line("show key: Windows kept another window in front (Echo's taskbar button flashes)");
        }
    }
}

#[cfg(target_os = "linux")]
fn raise(a: &AppWindow, tok: Option<String>) {
    use slint::winit_030::WinitWindowAccessor;
    // X11: _NET_ACTIVE_WINDOW (on Wayland winit does nothing here)
    a.window().with_winit_window(|w| w.focus_window());
    // a window that was hidden in the tray is made again on show: give it a moment to be on screen
    let wk = a.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(150), move || {
        if let Some(a) = wk.upgrade() {
            match wl::activate(&a, tok) {
                Ok(how) => echo_core::recorder::log_line(&format!("show key: window raised ({how})")),
                Err(e) => echo_core::recorder::log_line(&format!("show key: couldn't raise the window: {e}")),
            }
        }
    });
}

#[cfg(not(any(windows, target_os = "linux")))]
fn raise(_: &AppWindow, _: Option<String>) {}

/// Wayland: xdg_activation_v1.activate(token, Echo's surface), on winit's own connection (like wl_dnd.rs).
#[cfg(target_os = "linux")]
mod wl {
    use crate::AppWindow;
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
    use slint::ComponentHandle;
    use wayland_client::globals::{registry_queue_init, GlobalListContents};
    use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
    use wayland_protocols::xdg::activation::v1::client::xdg_activation_token_v1::{self, XdgActivationTokenV1};
    use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1::XdgActivationV1;

    #[derive(Default)]
    struct St {
        token: Option<String>,
    }

    /// Ok("portal token" / "own token" / "X11"), or why not
    pub fn activate(a: &AppWindow, tok: Option<String>) -> Result<&'static str, String> {
        let wh = a.window().window_handle();
        let dh = wh.display_handle().map_err(|e| e.to_string())?;
        let RawDisplayHandle::Wayland(d) = dh.as_raw() else { return Ok("X11") };
        let h = wh.window_handle().map_err(|e| e.to_string())?;
        let RawWindowHandle::Wayland(w) = h.as_raw() else { return Err("no Wayland surface".into()) };
        // SAFETY: winit's wl_display + the window's wl_surface, both alive while the window is shown; "guest"
        // backend: it never closes the display, and the surface proxy is only used right here
        let backend = unsafe { wayland_backend::client::Backend::from_foreign_display(d.display.as_ptr() as *mut _) };
        let conn = Connection::from_backend(backend);
        let sid = unsafe { wayland_backend::client::ObjectId::from_ptr(WlSurface::interface(), w.surface.as_ptr() as *mut _) }
            .map_err(|e| format!("surface: {e:?}"))?;
        let surface = WlSurface::from_id(&conn, sid).map_err(|e| format!("surface: {e:?}"))?;
        let (globals, mut q) = registry_queue_init::<St>(&conn).map_err(|e| e.to_string())?;
        let qh = q.handle();
        let act: XdgActivationV1 = globals.bind(&qh, 1..=1, ()).map_err(|e| format!("no xdg_activation_v1: {e}"))?;
        let mut st = St::default();
        let (token, how) = match tok {
            Some(t) => (t, "portal token"),
            None => {
                // our own token: KWin only grants it to the window in front, else it flashes the taskbar entry
                let t = act.get_activation_token(&qh, ());
                t.set_surface(&surface);
                t.set_app_id("echo".into());
                t.commit();
                for _ in 0..10 {
                    if st.token.is_some() {
                        break;
                    }
                    q.roundtrip(&mut st).map_err(|e| e.to_string())?;
                }
                t.destroy();
                (st.token.take().ok_or("no token from the compositor")?, "own token")
            }
        };
        act.activate(token, &surface);
        act.destroy();
        conn.flush().map_err(|e| e.to_string())?;
        Ok(how)
    }

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for St {
        fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
    }
    impl Dispatch<XdgActivationV1, ()> for St {
        fn event(_: &mut Self, _: &XdgActivationV1, _: <XdgActivationV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    }
    impl Dispatch<XdgActivationTokenV1, ()> for St {
        fn event(st: &mut Self, _: &XdgActivationTokenV1, ev: xdg_activation_token_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
            if let xdg_activation_token_v1::Event::Done { token } = ev {
                st.token = Some(token);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggles() {
        assert!(wants_hide(true, false, true)); // in front -> tray
        assert!(!wants_hide(false, false, false)); // in the tray -> show
        assert!(!wants_hide(true, true, false)); // minimized -> show
        assert!(!wants_hide(true, false, false)); // behind the game -> show
    }

    #[test]
    fn id_is_free() {
        // the other ids beyond seconds (rec_core BOOKMARK / SESSION / MIC_MUTE) and real clip lengths
        assert!(![u32::MAX, u32::MAX - 1, u32::MAX - 2].contains(&SHOW));
        assert!(SHOW > 100_000);
    }
}
