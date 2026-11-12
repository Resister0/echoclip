//! 11 Nov 2026: menus over the live video (Windows window mode).
//!
//! The video is mpv's own Direct3D child window and Slint can't draw on top of it, so a menu reaching over the
//! video was hidden behind it. Until now the video window was parked while a menu was open and a still of its
//! picture showed in its place (over.rs) - that blinked and looked softer than the real video.
//!
//! Now: Slint still draws the menu as usual (also the part under the video window, which nobody sees), and right
//! after each frame the part of every open menu that lies over the video is read back from Slint's own frame and
//! shown in a small window of its own ABOVE the video: an owned, layered, click-through tool window (no taskbar
//! button, no Alt+Tab entry, never takes focus). Clicks and hover go straight through it to Slint, so the menu
//! works exactly as before; the video keeps playing at full quality underneath.
//! Pure black pixels (Slint's own background under the video, the menu's shadow over it) stay see-through, so the
//! rounded corners show the video and not a black square.
//!
//! If the read-back ever comes back empty (a driver that doesn't keep pixels under a child window), `broken()`
//! turns true and over.rs parks the video like before.
use crate::AppWindow;
use glow::HasContext;
use slint::ComponentHandle;
use std::cell::{Cell, RefCell};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match m {
        // click-through: never hit (WS_EX_TRANSPARENT already does this for layered windows)
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        _ => DefWindowProcW(h, m, w, l),
    }
}

struct Win {
    hwnd: HWND,
    shown: bool,
    /// the 32-bit DIB the pixels go into (re-made when the size changes)
    dib: Option<(HDC, HBITMAP, *mut u8, i32, i32)>,
}

thread_local! {
    static WIN: RefCell<Option<Win>> = const { RefCell::new(None) };
    /// the read-back gave nothing (all black) - over.rs parks the video instead
    static BROKEN: Cell<bool> = const { Cell::new(false) };
    /// frames in a row with a menu over the video but nothing read back
    static EMPTY: Cell<u32> = const { Cell::new(0) };
}

/// true = menus over the video can't be shown this way here: park the video like before (over.rs)
pub fn broken() -> bool {
    BROKEN.with(|b| b.get())
}

fn slint_hwnd(a: &AppWindow) -> Option<HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let h = a.window().window_handle();
    let h = h.window_handle().ok()?;
    match h.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as HWND),
        _ => None,
    }
}

fn make_win(owner: HWND) -> Option<Win> {
    unsafe {
        let inst = GetModuleHandleW(std::ptr::null());
        let cls: Vec<u16> = "EchoMenuOverVideo\0".encode_utf16().collect();
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: inst,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: cls.as_ptr(),
        };
        RegisterClassW(&wc);
        // owned popup (always above Echo's window, gone with it), layered + click-through, tool window (no taskbar /
        // Alt+Tab), never activated
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            cls.as_ptr(),
            std::ptr::null(),
            WS_POPUP | WS_DISABLED,
            0,
            0,
            1,
            1,
            owner,
            std::ptr::null_mut(),
            inst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return None;
        }
        Some(Win { hwnd, shown: false, dib: None })
    }
}

/// hide the menu window (menu closed, player closed, video window parked)
pub fn hide() {
    WIN.with(|w| {
        if let Some(w) = w.borrow_mut().as_mut().filter(|w| w.shown) {
            w.shown = false;
            unsafe { ShowWindow(w.hwnd, SW_HIDE) };
        }
    });
}

/// the open menus (Slint PopupWindows, not tooltips) in physical px of the window: (x, y, w, h)
fn popup_rects(a: &AppWindow, sf: f32) -> Vec<(i32, i32, i32, i32)> {
    use i_slint_core::window::PopupWindowLocation;
    use slint::private_unstable_api::re_exports::{vtable, WindowInner, WindowKind};
    WindowInner::from_pub(a.window())
        .active_popups()
        .iter()
        .filter(|p| !matches!(p.window_kind, WindowKind::ToolTip))
        .filter_map(|p| match &p.location {
            PopupWindowLocation::ChildWindow(pos) => {
                let g = vtable::VRc::borrow_pin(&p.component).as_ref().item_geometry(0);
                // a menu's box can reach a little past its popup's own rectangle (measured: ~10 px on the left), so
                // take a margin - the black around it stays see-through anyway
                const M: f32 = 24.0;
                let x0 = ((pos.x - M) * sf).floor() as i32;
                let y0 = ((pos.y - M) * sf).floor() as i32;
                let x1 = ((pos.x + g.size.width + M) * sf).ceil() as i32;
                let y1 = ((pos.y + g.size.height + M) * sf).ceil() as i32;
                (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
            }
            _ => None,
        })
        .collect()
}

/// Called after every frame Slint drew (the GL context is current, the frame is in the back buffer, not shown yet).
/// `video` = the video window's place in physical px when it is on screen, else None.
pub fn after_render(a: &AppWindow, gl: &glow::Context, video: Option<(i32, i32, i32, i32)>) {
    if broken() {
        return hide();
    }
    let Some((vx, vy, vw, vh)) = video else { return hide() };
    let sf = a.window().scale_factor();
    // the parts of the menus that lie over the video
    let parts: Vec<(i32, i32, i32, i32)> = popup_rects(a, sf)
        .into_iter()
        .filter_map(|(x, y, w, h)| {
            let (x0, y0) = (x.max(vx), y.max(vy));
            let (x1, y1) = ((x + w).min(vx + vw), (y + h).min(vy + vh));
            (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
        })
        .collect();
    if parts.is_empty() {
        EMPTY.with(|e| e.set(0));
        return hide();
    }
    // one window over all of them
    let bx = parts.iter().map(|p| p.0).min().unwrap_or(0);
    let by = parts.iter().map(|p| p.1).min().unwrap_or(0);
    let bw = parts.iter().map(|p| p.0 + p.2).max().unwrap_or(0) - bx;
    let bh = parts.iter().map(|p| p.1 + p.3).max().unwrap_or(0) - by;
    let fb_h = a.window().size().height as i32;
    if bw <= 0 || bh <= 0 || fb_h <= 0 {
        return hide();
    }
    // read the frame back (GL rows go bottom-up)
    let mut rgba = vec![0u8; (bw * bh * 4) as usize];
    unsafe {
        let prev = gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING);
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
        gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
        gl.read_pixels(bx, fb_h - (by + bh), bw, bh, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut rgba)));
        gl.bind_framebuffer(glow::READ_FRAMEBUFFER, std::num::NonZeroU32::new(prev as u32).map(glow::NativeFramebuffer));
    }
    let Some(owner) = slint_hwnd(a) else { return };
    WIN.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            *cell = make_win(owner);
        }
        let Some(win) = cell.as_mut() else {
            BROKEN.with(|b| b.set(true));
            return;
        };
        unsafe {
            // the DIB (top-down, premultiplied BGRA)
            if win.dib.as_ref().is_none_or(|d| (d.3, d.4) != (bw, bh)) {
                if let Some((dc, bmp, ..)) = win.dib.take() {
                    DeleteDC(dc);
                    DeleteObject(bmp);
                }
                let mut bi: BITMAPINFO = std::mem::zeroed();
                bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                bi.bmiHeader.biWidth = bw;
                bi.bmiHeader.biHeight = -bh;
                bi.bmiHeader.biPlanes = 1;
                bi.bmiHeader.biBitCount = 32;
                bi.bmiHeader.biCompression = BI_RGB;
                let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
                let bmp = CreateDIBSection(std::ptr::null_mut(), &bi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
                if bmp.is_null() || bits.is_null() {
                    BROKEN.with(|b| b.set(true));
                    return;
                }
                let dc = CreateCompatibleDC(std::ptr::null_mut());
                SelectObject(dc, bmp);
                win.dib = Some((dc, bmp, bits as *mut u8, bw, bh));
            }
            let Some((dc, _, bits, ..)) = win.dib else { return };
            let out = std::slice::from_raw_parts_mut(bits, (bw * bh * 4) as usize);
            let mut any = false;
            for row in 0..bh {
                let src_row = (bh - 1 - row) as usize; // flip GL's bottom-up rows
                let y = by + row;
                for col in 0..bw {
                    let x = bx + col;
                    let o = ((row * bw + col) * 4) as usize;
                    let s = src_row * bw as usize * 4 + col as usize * 4;
                    let (r, g, b) = (rgba[s], rgba[s + 1], rgba[s + 2]);
                    let inside = parts.iter().any(|&(px, py, pw, ph)| x >= px && x < px + pw && y >= py && y < py + ph);
                    if inside && (r | g | b) != 0 {
                        out[o] = b;
                        out[o + 1] = g;
                        out[o + 2] = r;
                        out[o + 3] = 255;
                        any = true;
                    } else {
                        out[o..o + 4].copy_from_slice(&[0, 0, 0, 0]);
                    }
                }
            }
            if !any {
                // nothing came back where a menu is: after a few frames, give up on this way (over.rs parks the video)
                let n = EMPTY.with(|e| {
                    e.set(e.get() + 1);
                    e.get()
                });
                if n >= 3 {
                    BROKEN.with(|b| b.set(true));
                    crate::lib_net::log_line("menus over the video: read-back empty - parking the video for menus instead");
                }
                if win.shown {
                    win.shown = false;
                    ShowWindow(win.hwnd, SW_HIDE);
                }
                return;
            }
            EMPTY.with(|e| e.set(0));
            let mut pt = POINT { x: bx, y: by };
            ClientToScreen(owner, &mut pt);
            let size = SIZE { cx: bw, cy: bh };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
            let sdc = GetDC(std::ptr::null_mut());
            UpdateLayeredWindow(win.hwnd, sdc, &pt, &size, dc, &src, 0, &blend, ULW_ALPHA);
            ReleaseDC(std::ptr::null_mut(), sdc);
            if !win.shown {
                win.shown = true;
                ShowWindow(win.hwnd, SW_SHOWNOACTIVATE);
            }
        }
    });
}
