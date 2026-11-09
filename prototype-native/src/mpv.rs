//! Minimal libmpv binding, loaded at runtime (libmpv-2.dll next to the exe on Windows, libmpv.so.2 on Linux).
//! mpv does the decoding (GPU when it can), audio output and A/V sync; it draws each video frame into an
//! OpenGL texture that Slint then shows like any other image.
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::cell::Cell;
use std::ptr;

#[repr(C)]
pub struct RenderParam {
    pub kind: c_int,
    pub data: *mut c_void,
}
#[repr(C)]
pub struct OpenGlInitParams {
    pub get_proc_address: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    pub get_proc_address_ctx: *mut c_void,
}
#[repr(C)]
pub struct OpenGlFbo {
    pub fbo: c_int,
    pub w: c_int,
    pub h: c_int,
    pub internal_format: c_int,
}
#[repr(C)]
struct Event {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

pub const P_INVALID: c_int = 0;
pub const P_API_TYPE: c_int = 1;
pub const P_OPENGL_INIT_PARAMS: c_int = 2;
pub const P_OPENGL_FBO: c_int = 3;
pub const P_FLIP_Y: c_int = 4;
pub const P_X11_DISPLAY: c_int = 8;
pub const P_WL_DISPLAY: c_int = 9;
pub const P_BLOCK_FOR_TARGET_TIME: c_int = 12;
pub const P_SW_SIZE: c_int = 17;
pub const P_SW_FORMAT: c_int = 18;
pub const P_SW_STRIDE: c_int = 19;
pub const P_SW_POINTER: c_int = 20;
pub const UPDATE_FRAME: u64 = 1;

type Handle = c_void;
type RenderCtx = c_void;

struct Api {
    _lib: libloading::Library,
    create: unsafe extern "C" fn() -> *mut Handle,
    initialize: unsafe extern "C" fn(*mut Handle) -> c_int,
    set_option_string: unsafe extern "C" fn(*mut Handle, *const c_char, *const c_char) -> c_int,
    set_property_string: unsafe extern "C" fn(*mut Handle, *const c_char, *const c_char) -> c_int,
    get_property_string: unsafe extern "C" fn(*mut Handle, *const c_char) -> *mut c_char,
    command: unsafe extern "C" fn(*mut Handle, *const *const c_char) -> c_int,
    free: unsafe extern "C" fn(*mut c_void),
    wait_event: unsafe extern "C" fn(*mut Handle, f64) -> *mut Event,
    error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    terminate_destroy: unsafe extern "C" fn(*mut Handle),
    rc_create: unsafe extern "C" fn(*mut *mut RenderCtx, *mut Handle, *mut RenderParam) -> c_int,
    rc_set_update_callback: unsafe extern "C" fn(*mut RenderCtx, Option<extern "C" fn(*mut c_void)>, *mut c_void),
    rc_update: unsafe extern "C" fn(*mut RenderCtx) -> u64,
    rc_render: unsafe extern "C" fn(*mut RenderCtx, *mut RenderParam) -> c_int,
    rc_report_swap: unsafe extern "C" fn(*mut RenderCtx),
    rc_free: unsafe extern "C" fn(*mut RenderCtx),
}

fn load_api() -> Result<Api, String> {
    let names: &[&str] = if cfg!(windows) { &["libmpv-2.dll", "mpv-2.dll", "mpv-1.dll"] } else { &["libmpv.so.2", "libmpv.so.1", "libmpv.so"] };
    let mut dirs: Vec<std::path::PathBuf> = vec![];
    if let Some(d) = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())) {
        dirs.push(d.clone());
        // cargo run puts the exe in target/release: also look in the project folder
        if let Some(p) = d.parent().and_then(|p| p.parent()) {
            dirs.push(p.to_path_buf());
        }
    }
    let mut tried = vec![];
    let mut lib = None;
    'outer: for n in names {
        for d in &dirs {
            let p = d.join(n);
            if p.exists() {
                if let Ok(l) = unsafe { libloading::Library::new(&p) } {
                    lib = Some(l);
                    break 'outer;
                }
            }
        }
        match unsafe { libloading::Library::new(n) } {
            Ok(l) => {
                lib = Some(l);
                break 'outer;
            }
            Err(e) => tried.push(format!("{n}: {e}")),
        }
    }
    let lib = lib.ok_or_else(|| format!("libmpv not found ({})", tried.join("; ")))?;
    unsafe {
        macro_rules! f {
            ($n:literal) => {
                *lib.get(concat!($n, "\0").as_bytes()).map_err(|e| format!("{}: {e}", $n))?
            };
        }
        Ok(Api {
            create: f!("mpv_create"),
            initialize: f!("mpv_initialize"),
            set_option_string: f!("mpv_set_option_string"),
            set_property_string: f!("mpv_set_property_string"),
            get_property_string: f!("mpv_get_property_string"),
            command: f!("mpv_command"),
            free: f!("mpv_free"),
            wait_event: f!("mpv_wait_event"),
            error_string: f!("mpv_error_string"),
            terminate_destroy: f!("mpv_terminate_destroy"),
            rc_create: f!("mpv_render_context_create"),
            rc_set_update_callback: f!("mpv_render_context_set_update_callback"),
            rc_update: f!("mpv_render_context_update"),
            rc_render: f!("mpv_render_context_render"),
            rc_report_swap: f!("mpv_render_context_report_swap"),
            rc_free: f!("mpv_render_context_free"),
            _lib: lib,
        })
    }
}

fn cs(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

pub struct Mpv {
    api: Api,
    h: *mut Handle,
    rc: *mut RenderCtx,
    /// since the last arm_first_frame(): mpv began the new file (START_FILE) / showed its first picture
    /// (PLAYBACK_RESTART after START_FILE). Set while reading events - any drain_events() call updates them.
    started: Cell<bool>,
    restarted: Cell<bool>,
}

const EV_START_FILE: c_int = 6;
const EV_PLAYBACK_RESTART: c_int = 21;

/// Windows window mode: mpv's flip-model swap chain (default) or the old blit model (ECHO_D3D_FLIP=0)
pub fn d3d_flip() -> bool {
    std::env::var("ECHO_D3D_FLIP").map(|v| v != "0").unwrap_or(true)
}

impl Mpv {
    /// `wid` = a native window to draw into (Windows window mode: Direct3D 11, frames stay on the GPU);
    /// None = draw through the OpenGL render API into Slint's texture.
    pub fn new(hwdec: &str, wid: Option<i64>) -> Result<Mpv, String> {
        let api = load_api()?;
        let h = unsafe { (api.create)() };
        if h.is_null() {
            return Err("mpv_create failed".into());
        }
        let m = Mpv { api, h, rc: ptr::null_mut(), started: Cell::new(false), restarted: Cell::new(false) };
        match wid {
            Some(w) => {
                m.set_option("vo", "gpu-next");
                m.set_option("gpu-api", "d3d11");
                m.set_option("wid", &w.to_string());
                // flip-model presentation (mpv's default) = new clips appear instantly. The older "blit" model
                // respected the window region (rounded corners, seek-preview hole) but opened clips with a black flash
                // + a piece of the previous clip. ECHO_D3D_FLIP=0 = blit model again.
                if !d3d_flip() {
                    m.set_option("d3d11-flip", "no");
                }
                m.set_option("background-color", "#000000");
                // keep the video output between files: prev/next swaps pictures directly (no re-init = no black), and
                // after "stop" the window holds plain black instead of the last clip
                m.set_option("force-window", "yes");
            }
            None => m.set_option("vo", "libmpv"),
        }
        for (k, v) in [
            ("hwdec", hwdec),
            ("idle", "yes"),
            ("keep-open", "yes"),
            ("terminal", "no"),
            ("input-default-bindings", "no"),
            ("input-vo-keyboard", "no"),
            ("osc", "no"),
            ("audio-client-name", "Echo"),
            ("hr-seek", "yes"),
            // small demuxer cache: these are local files
            ("demuxer-max-bytes", "16MiB"),
            ("demuxer-max-back-bytes", "8MiB"),
        ] {
            m.set_option(k, v);
        }
        // ECHO_MPV_LOG=<file> writes mpv's own log (for debugging playback)
        if let Ok(f) = std::env::var("ECHO_MPV_LOG") {
            m.set_option("log-file", &f);
        }
        // ECHO_MPV_OPTS="key=value;key=value" sets any extra mpv option (experiments)
        if let Ok(o) = std::env::var("ECHO_MPV_OPTS") {
            for kv in o.split(';') {
                if let Some((k, v)) = kv.split_once('=') {
                    m.set_option(k.trim(), v.trim());
                }
            }
        }
        let r = unsafe { (m.api.initialize)(m.h) };
        if r < 0 {
            return Err(format!("mpv_initialize: {}", m.err(r)));
        }
        Ok(m)
    }

    fn err(&self, r: c_int) -> String {
        unsafe { CStr::from_ptr((self.api.error_string)(r)).to_string_lossy().into_owned() }
    }

    pub fn set_option(&self, k: &str, v: &str) {
        unsafe { (self.api.set_option_string)(self.h, cs(k).as_ptr(), cs(v).as_ptr()) };
    }

    pub fn set(&self, k: &str, v: &str) {
        unsafe { (self.api.set_property_string)(self.h, cs(k).as_ptr(), cs(v).as_ptr()) };
    }

    pub fn get(&self, k: &str) -> Option<String> {
        unsafe {
            let p = (self.api.get_property_string)(self.h, cs(k).as_ptr());
            if p.is_null() {
                return None;
            }
            let s = CStr::from_ptr(p).to_string_lossy().into_owned();
            (self.api.free)(p as *mut c_void);
            Some(s)
        }
    }

    pub fn get_f64(&self, k: &str) -> Option<f64> {
        self.get(k).and_then(|s| s.parse().ok())
    }

    pub fn command(&self, args: &[&str]) {
        let c: Vec<CString> = args.iter().map(|a| cs(a)).collect();
        let mut p: Vec<*const c_char> = c.iter().map(|a| a.as_ptr()).collect();
        p.push(ptr::null());
        unsafe { (self.api.command)(self.h, p.as_ptr()) };
    }

    /// Empty mpv's event queue (state is read by polling properties, except the "new file's first picture is on
    /// screen" moment below).
    pub fn drain_events(&self) {
        unsafe {
            loop {
                let e = (self.api.wait_event)(self.h, 0.0);
                if e.is_null() || (*e).event_id == 0 {
                    break;
                }
                match (*e).event_id {
                    EV_START_FILE => {
                        self.started.set(true);
                        self.restarted.set(false);
                    }
                    EV_PLAYBACK_RESTART if self.started.get() => self.restarted.set(true),
                    _ => {}
                }
            }
        }
    }

    /// Call right BEFORE loadfile: forget old events, then wait for this file's own START_FILE + PLAYBACK_RESTART.
    /// (Properties like vo-configured / estimated-frame-number still describe the PREVIOUS clip for a moment after
    /// loadfile - checking them showed the video window too early: black flash + a piece of the old clip.)
    pub fn arm_first_frame(&self) {
        self.drain_events();
        self.started.set(false);
        self.restarted.set(false);
    }

    /// The file loaded after arm_first_frame() has its first picture on screen.
    pub fn first_frame_shown(&self) -> bool {
        self.drain_events();
        self.restarted.get()
    }

    /// Must be called with Slint's OpenGL context current. `params` are the extra render params (display handles).
    pub fn create_render_context(
        &mut self,
        gpa: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
        gpa_ctx: *mut c_void,
        mut extra: Vec<RenderParam>,
        on_update: extern "C" fn(*mut c_void),
        update_ctx: *mut c_void,
    ) -> Result<(), String> {
        let api_type = cs("opengl");
        let mut init = OpenGlInitParams { get_proc_address: gpa, get_proc_address_ctx: gpa_ctx };
        let mut params = vec![
            RenderParam { kind: P_API_TYPE, data: api_type.as_ptr() as *mut c_void },
            RenderParam { kind: P_OPENGL_INIT_PARAMS, data: &mut init as *mut _ as *mut c_void },
        ];
        params.append(&mut extra);
        params.push(RenderParam { kind: P_INVALID, data: ptr::null_mut() });
        let mut rc = ptr::null_mut();
        let r = unsafe { (self.api.rc_create)(&mut rc, self.h, params.as_mut_ptr()) };
        if r < 0 {
            return Err(format!("mpv_render_context_create: {}", self.err(r)));
        }
        self.rc = rc;
        unsafe { (self.api.rc_set_update_callback)(rc, Some(on_update), update_ctx) };
        Ok(())
    }

    /// Software renderer (last fallback: no GPU drawing at all - VMs, remote desktop, broken drivers):
    /// mpv converts + scales each frame on the CPU into memory we hand it.
    pub fn create_sw_render_context(&mut self, on_update: extern "C" fn(*mut c_void), update_ctx: *mut c_void) -> Result<(), String> {
        let api_type = cs("sw");
        let mut params = [
            RenderParam { kind: P_API_TYPE, data: api_type.as_ptr() as *mut c_void },
            RenderParam { kind: P_INVALID, data: ptr::null_mut() },
        ];
        let mut rc = ptr::null_mut();
        let r = unsafe { (self.api.rc_create)(&mut rc, self.h, params.as_mut_ptr()) };
        if r < 0 {
            return Err(format!("mpv software renderer: {}", self.err(r)));
        }
        self.rc = rc;
        unsafe { (self.api.rc_set_update_callback)(rc, Some(on_update), update_ctx) };
        Ok(())
    }

    /// draw the current frame as RGB0 (4 bytes per pixel) into `buf` (w*h*4 bytes, `stride` bytes per line)
    pub fn render_sw(&self, w: i32, h: i32, stride: usize, buf: &mut [u8]) -> bool {
        if self.rc.is_null() || buf.len() < stride * h as usize {
            return false;
        }
        let mut size: [c_int; 2] = [w, h];
        let fmt = cs("rgb0");
        let mut stride = stride;
        let mut params = [
            RenderParam { kind: P_SW_SIZE, data: size.as_mut_ptr() as *mut c_void },
            RenderParam { kind: P_SW_FORMAT, data: fmt.as_ptr() as *mut c_void },
            RenderParam { kind: P_SW_STRIDE, data: &mut stride as *mut usize as *mut c_void },
            RenderParam { kind: P_SW_POINTER, data: buf.as_mut_ptr() as *mut c_void },
            RenderParam { kind: P_INVALID, data: ptr::null_mut() },
        ];
        unsafe { (self.api.rc_render)(self.rc, params.as_mut_ptr()) >= 0 }
    }

    #[allow(dead_code)]
    pub fn has_render_context(&self) -> bool {
        !self.rc.is_null()
    }

    pub fn update(&self) -> u64 {
        if self.rc.is_null() {
            return 0;
        }
        unsafe { (self.api.rc_update)(self.rc) }
    }

    pub fn render(&self, fbo: i32, w: i32, h: i32, flip: bool) {
        if self.rc.is_null() {
            return;
        }
        let mut f = OpenGlFbo { fbo, w, h, internal_format: 0 };
        let mut flip_y: c_int = flip as c_int;
        // never let mpv sleep on Slint's UI thread waiting for the frame's display time
        let mut block: c_int = 0;
        let mut params = [
            RenderParam { kind: P_OPENGL_FBO, data: &mut f as *mut _ as *mut c_void },
            RenderParam { kind: P_FLIP_Y, data: &mut flip_y as *mut _ as *mut c_void },
            RenderParam { kind: P_BLOCK_FOR_TARGET_TIME, data: &mut block as *mut _ as *mut c_void },
            RenderParam { kind: P_INVALID, data: ptr::null_mut() },
        ];
        unsafe { (self.api.rc_render)(self.rc, params.as_mut_ptr()) };
    }

    pub fn report_swap(&self) {
        if !self.rc.is_null() {
            unsafe { (self.api.rc_report_swap)(self.rc) };
        }
    }

    /// Free the render context (needs the GL context current).
    pub fn free_render_context(&mut self) {
        if !self.rc.is_null() {
            unsafe { (self.api.rc_free)(self.rc) };
            self.rc = ptr::null_mut();
        }
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        self.free_render_context();
        unsafe { (self.api.terminate_destroy)(self.h) };
    }
}
