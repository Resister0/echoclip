//! Windows: the video gets its own small window, and mpv draws into it with Direct3D 11 + d3d11va - decoded frames
//! never leave the GPU (the same path the normal mpv player uses). The window is disabled, so clicks and keys still
//! go to Slint (click = pause, Space, Esc all work).
//!
//! Two placements:
//! - normal: a CHILD window over the video area. Slint can't draw on top of a child window, so the seek preview is
//!   cut out of it (window region).
//! - fullscreen: a separate borderless window placed DIRECTLY BEHIND Slint's window, and Slint's window is clear
//!   (alpha 0) where the video is. Windows (DWM) blends Slint's controls over the video: real see-through
//!   controls, no OpenGL. (Slint's winit windows are created with transparency support.)
use std::cell::{Cell, RefCell};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, CombineRgn, CreateRectRgn, CreateRoundRectRgn, DeleteObject, EndPaint, FillRect, GetStockObject, InvalidateRect, SetBrushOrgEx, SetStretchBltMode, SetWindowRgn,
    StretchDIBits, UpdateWindow, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLACK_BRUSH, DIB_RGB_COLORS, HALFTONE, PAINTSTRUCT, RGN_DIFF, SRCCOPY,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    DefWindowProcW(h, m, w, l)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub struct VideoWindow {
    pub hwnd: HWND,
    parent: HWND,
    rect: RefCell<(i32, i32, i32, i32, Vec<(i32, i32, i32, i32)>)>,
    shown: Cell<bool>,
    behind: Cell<bool>,
    /// last requested placement (x, y, w, h, radius, holes) - re-applied when un-parking
    last: RefCell<(i32, i32, i32, i32, i32, Vec<(i32, i32, i32, i32)>)>,
    /// ShowWindow done once (the window then stays WS_VISIBLE; see show())
    visible: Cell<bool>,
}

impl VideoWindow {
    pub fn new(parent: HWND) -> Option<VideoWindow> {
        unsafe {
            let inst = GetModuleHandleW(std::ptr::null());
            let cls = wide("EchoProtoVideo");
            let wc = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: inst,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: GetStockObject(BLACK_BRUSH) as _,
                lpszMenuName: std::ptr::null(),
                lpszClassName: cls.as_ptr(),
            };
            RegisterClassW(&wc);
            // Slint's window must not paint over its child
            let st = GetWindowLongPtrW(parent, GWL_STYLE);
            SetWindowLongPtrW(parent, GWL_STYLE, st | WS_CLIPCHILDREN as isize);
            let hwnd = CreateWindowExW(0, cls.as_ptr(), std::ptr::null(), WS_CHILD | WS_CLIPSIBLINGS | WS_DISABLED, PARK, PARK, 16, 16, parent, std::ptr::null_mut(), inst, std::ptr::null());
            if hwnd.is_null() {
                return None;
            }
            Some(VideoWindow {
                hwnd,
                parent,
                rect: RefCell::new((i32::MIN, 0, 0, 0, vec![])),
                shown: Cell::new(false),
                behind: Cell::new(false),
                last: RefCell::new((0, 0, 0, 0, 0, vec![])),
                visible: Cell::new(false),
            })
        }
    }

    /// true = fullscreen placement: own borderless window right behind Slint's (Slint draws see-through over it)
    #[allow(dead_code)] // kept on purpose: the fullscreen-behind path (FRAGILE section of the handoff), unused today
    pub fn set_behind(&self, on: bool) {
        if self.behind.get() == on {
            return;
        }
        self.behind.set(on);
        let was_shown = self.shown.get();
        self.show(false);
        // the window stays WS_VISIBLE (parked, see show()): keep that bit when the style is replaced
        let vis = if self.visible.get() { WS_VISIBLE } else { 0 };
        unsafe {
            if on {
                SetWindowLongPtrW(self.hwnd, GWL_STYLE, (WS_POPUP | WS_DISABLED | vis) as isize);
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, (WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) as isize);
                SetParent(self.hwnd, std::ptr::null_mut());
            } else {
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, 0);
                SetWindowLongPtrW(self.hwnd, GWL_STYLE, (WS_CHILD | WS_CLIPSIBLINGS | WS_DISABLED | vis) as isize);
                SetParent(self.hwnd, self.parent);
            }
            SetWindowPos(self.hwnd, std::ptr::null_mut(), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
            SetWindowRgn(self.hwnd, std::ptr::null_mut(), 1);
        }
        // force the next place() to apply everything again
        *self.rect.borrow_mut() = (i32::MIN, 0, 0, 0, vec![]);
        self.show(was_shown);
    }

    /// physical pixels inside Slint's window. `holes` (relative to the video) = areas Slint draws over the video
    /// (the seek preview): cut out of the child window so Slint's drawing shows there (blit model only - the flip
    /// model ignores regions). Not used behind. While "hidden" (parked) the size/region are kept up to date but the
    /// window stays outside the visible area.
    pub fn place(&self, x: i32, y: i32, w: i32, h: i32, radius: i32, holes: Vec<(i32, i32, i32, i32)>) {
        let parked = !self.shown.get();
        let (px, py) = if parked { (PARK, PARK) } else { (x, y) };
        unsafe {
            if self.behind.get() {
                // screen position of Slint's client area; directly BELOW Slint's window in the z-order (every
                // frame: if something else came in between, the video goes back right behind Echo)
                let mut p = POINT { x: px, y: py };
                if !parked {
                    ClientToScreen(self.parent, &mut p);
                }
                SetWindowPos(self.hwnd, self.parent, p.x, p.y, w, h, SWP_NOACTIVATE);
                *self.rect.borrow_mut() = (x, y, w, h, vec![]);
                *self.last.borrow_mut() = (x, y, w, h, radius, vec![]);
                return;
            }
        }
        *self.last.borrow_mut() = (x, y, w, h, radius, holes.clone());
        let now = (px, py, w, h, holes);
        if *self.rect.borrow() == now {
            return;
        }
        let (moved, resized) = {
            let r = self.rect.borrow();
            ((r.0, r.1, r.2, r.3) != (px, py, w, h), (r.2, r.3) != (w, h) || r.4 != now.4)
        };
        unsafe {
            if moved {
                // bottom of Slint's child windows: the poster window (below) sits on top of it while a clip opens
                SetWindowPos(self.hwnd, HWND_BOTTOM, px, py, w, h, SWP_NOACTIVATE);
                // William (9 Nov, measured with ECHO_DEBUG_OSD): mpv draws into its OWN window inside this one and
                // only noticed a new size ~340 ms later (going fullscreen showed its old small picture in the big
                // window - the "black box"). Resize mpv's window right away, so it gets its WM_SIZE now.
                let inner = GetWindow(self.hwnd, GW_CHILD);
                if !inner.is_null() && resized {
                    SetWindowPos(inner, std::ptr::null_mut(), 0, 0, w, h, SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOMOVE);
                }
            }
            if resized || self.rect.borrow().0 == i32::MIN {
                // rounded corners like the rest of Echo
                let rgn = if radius > 0 { CreateRoundRectRgn(0, 0, w + 1, h + 1, radius * 2, radius * 2) } else { CreateRectRgn(0, 0, w, h) };
                for &(hx, hy, hw, hh) in &now.4 {
                    let cut = CreateRectRgn(hx, hy, hx + hw, hy + hh);
                    CombineRgn(rgn, rgn, cut, RGN_DIFF);
                    DeleteObject(cut);
                }
                // the window owns the region from here on (no DeleteObject for rgn)
                SetWindowRgn(self.hwnd, rgn, 1);
            }
        }
        *self.rect.borrow_mut() = now;
    }

    /// Visible or not. The window is NEVER really hidden (ShowWindow SW_HIDE): a hidden Direct3D window comes back
    /// without its picture and a hidden child leaves a stale/see-through hole in Slint's OpenGL window. "Hidden" =
    /// parked far outside the visible area (it keeps its last picture, and Slint's window under it is drawn normally).
    pub fn show(&self, on: bool) {
        if !self.visible.get() {
            self.visible.set(true);
            unsafe { ShowWindow(self.hwnd, SW_SHOWNA) };
        }
        if self.shown.get() == on {
            return;
        }
        self.shown.set(on);
        let (x, y, w, h, r, holes) = self.last.borrow().clone();
        if w > 0 && h > 0 {
            self.place(x, y, w, h, r, holes);
        } else if !on {
            unsafe { SetWindowPos(self.hwnd, std::ptr::null_mut(), PARK, PARK, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE) };
        }
    }

    pub fn is_shown(&self) -> bool {
        self.shown.get()
    }
}

/// "hidden" position (outside any monitor / Slint's client area)
const PARK: i32 = -30000;

// ---------- poster: the clip's thumbnail while it opens (normal view) ----------
// mpv's child window stays SHOWN while a clip opens (hiding it showed the desktop for a moment: Slint's OpenGL
// window had never drawn under the child, so that area was see-through until Slint drew it a few frames later).
// Instead a second child window ON TOP of it shows the thumbnail with plain GDI (painted at once, no swap chain),
// and hides when mpv has this clip's first picture on screen.

thread_local! {
    /// BGRA pixels (top-down), width, height
    static POSTER_PX: RefCell<Option<(Vec<u8>, i32, i32)>> = const { RefCell::new(None) };
}

unsafe extern "system" fn poster_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match m {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(h, &mut ps);
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(h, &mut rc);
            FillRect(hdc, &rc, GetStockObject(BLACK_BRUSH) as _);
            POSTER_PX.with(|p| {
                if let Some((px, iw, ih)) = p.borrow().as_ref() {
                    let (cw, ch) = (rc.right - rc.left, rc.bottom - rc.top);
                    if cw > 0 && ch > 0 && *iw > 0 && *ih > 0 {
                        // fit inside (like image-fit: contain)
                        let s = (cw as f32 / *iw as f32).min(ch as f32 / *ih as f32);
                        let (dw, dh) = ((*iw as f32 * s).round() as i32, (*ih as f32 * s).round() as i32);
                        let (dx, dy) = ((cw - dw) / 2, (ch - dh) / 2);
                        let mut bmi: BITMAPINFO = std::mem::zeroed();
                        bmi.bmiHeader = BITMAPINFOHEADER {
                            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                            biWidth: *iw,
                            biHeight: -*ih, // top-down
                            biPlanes: 1,
                            biBitCount: 32,
                            biCompression: BI_RGB,
                            biSizeImage: 0,
                            biXPelsPerMeter: 0,
                            biYPelsPerMeter: 0,
                            biClrUsed: 0,
                            biClrImportant: 0,
                        };
                        SetStretchBltMode(hdc, HALFTONE);
                        SetBrushOrgEx(hdc, 0, 0, std::ptr::null_mut());
                        StretchDIBits(hdc, dx, dy, dw, dh, 0, 0, *iw, *ih, px.as_ptr() as _, &bmi, DIB_RGB_COLORS, SRCCOPY);
                    }
                }
            });
            EndPaint(h, &ps);
            0
        }
        _ => DefWindowProcW(h, m, w, l),
    }
}

pub struct PosterWindow {
    hwnd: HWND,
    rect: Cell<(i32, i32, i32, i32, i32)>,
    shown: Cell<bool>,
}

impl PosterWindow {
    pub fn new(parent: HWND) -> Option<PosterWindow> {
        unsafe {
            let inst = GetModuleHandleW(std::ptr::null());
            let cls = wide("EchoProtoPoster");
            let wc = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(poster_proc),
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
            let hwnd = CreateWindowExW(0, cls.as_ptr(), std::ptr::null(), WS_CHILD | WS_CLIPSIBLINGS | WS_DISABLED, PARK, PARK, 16, 16, parent, std::ptr::null_mut(), inst, std::ptr::null());
            if hwnd.is_null() {
                return None;
            }
            Some(PosterWindow { hwnd, rect: Cell::new((i32::MIN, 0, 0, 0, 0)), shown: Cell::new(false) })
        }
    }

    /// picture to show (RGB8, as decoded for the cards); None = plain black
    pub fn set_picture(&self, rgb: Option<(&[u8], u32, u32)>) {
        let v = rgb.map(|(px, w, h)| {
            let mut out = Vec::with_capacity((w * h * 4) as usize);
            for c in px.chunks_exact(3) {
                out.extend_from_slice(&[c[2], c[1], c[0], 255]);
            }
            (out, w as i32, h as i32)
        });
        POSTER_PX.with(|p| *p.borrow_mut() = v);
        unsafe { InvalidateRect(self.hwnd, std::ptr::null(), 0) };
    }

    /// physical pixels inside Slint's window, on top of the video window, same rounded corners
    pub fn place(&self, x: i32, y: i32, w: i32, h: i32, radius: i32) {
        unsafe {
            SetWindowPos(self.hwnd, HWND_TOP, x, y, w, h, SWP_NOACTIVATE);
            if self.rect.get() != (x, y, w, h, radius) {
                self.rect.set((x, y, w, h, radius));
                let rgn = if radius > 0 { CreateRoundRectRgn(0, 0, w + 1, h + 1, radius * 2, radius * 2) } else { CreateRectRgn(0, 0, w, h) };
                SetWindowRgn(self.hwnd, rgn, 1);
                InvalidateRect(self.hwnd, std::ptr::null(), 0);
            }
        }
    }

    pub fn show(&self, on: bool) {
        if self.shown.get() == on {
            return;
        }
        self.shown.set(on);
        unsafe {
            ShowWindow(self.hwnd, if on { SW_SHOWNA } else { SW_HIDE });
            if on {
                // paint now, before mpv's window (with the previous clip's last picture) can be seen
                UpdateWindow(self.hwnd);
            } else {
                POSTER_PX.with(|p| *p.borrow_mut() = None);
            }
        }
    }

    pub fn is_shown(&self) -> bool {
        self.shown.get()
    }
}
