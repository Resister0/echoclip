//! Chat B: the system's folder picker (Windows' modern IFileOpenDialog with FOS_PICKFOLDERS, loaded from ole32.dll
//! at run time - no new Cargo features; Linux: kdialog, else zenity, like Echo's picker). Files go through Chat C's
//! pl_media::pick_file. Blocking: call from a worker thread (`pick_folder_ui` does that and answers on the UI thread).
//! Test hook: ECHO_PICK_DIR=<folder> returns that folder without a dialog (the cloud has none).
use std::path::PathBuf;

pub fn pick_folder(title: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ECHO_PICK_DIR") {
        return (!p.is_empty()).then(|| PathBuf::from(p));
    }
    #[cfg(windows)]
    {
        win::pick_folder(title)
    }
    #[cfg(not(windows))]
    {
        // kdialog, zenity, else the desktop's FileChooser portal (always the portal in the Flatpak)
        crate::linux::pick(title, None, true)
    }
}

/// Folder dialog on a worker thread; `done` runs on the UI thread with the picked folder (not called on Cancel).
pub fn pick_folder_ui(title: &str, done: impl FnOnce(PathBuf) + 'static) {
    let title = title.to_string();
    let done = std::cell::RefCell::new(Some(done));
    let (tx, rx) = std::sync::mpsc::channel::<Option<PathBuf>>();
    std::thread::spawn(move || {
        let _ = tx.send(pick_folder(&title));
    });
    poll(rx, done);
}

/// File dialog (pl_media's) on a worker thread; `done` gets the file on the UI thread.
pub fn pick_file_ui(title: &str, filter: &str, exts: &'static [&'static str], done: impl FnOnce(PathBuf) + 'static) {
    let (title, filter) = (title.to_string(), filter.to_string());
    let done = std::cell::RefCell::new(Some(done));
    let (tx, rx) = std::sync::mpsc::channel::<Option<PathBuf>>();
    std::thread::spawn(move || {
        let _ = tx.send(crate::pl_media::pick_file(&title, &filter, exts));
    });
    poll(rx, done);
}

fn poll<F: FnOnce(PathBuf) + 'static>(rx: std::sync::mpsc::Receiver<Option<PathBuf>>, done: std::cell::RefCell<Option<F>>) {
    let t = slint::Timer::default();
    let rc = std::rc::Rc::new(std::cell::RefCell::new(None::<slint::Timer>));
    let rc2 = rc.clone();
    t.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(120), move || match rx.try_recv() {
        Ok(r) => {
            let t = rc2.borrow_mut().take();
            slint::Timer::single_shot(std::time::Duration::from_millis(0), move || drop(t));
            if let (Some(p), Some(f)) = (r, done.borrow_mut().take()) {
                f(p);
            }
        }
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            let t = rc2.borrow_mut().take();
            slint::Timer::single_shot(std::time::Duration::from_millis(0), move || drop(t));
        }
        Err(_) => {}
    });
    *rc.borrow_mut() = Some(t);
}

#[cfg(windows)]
mod win {
    //! IFileOpenDialog by hand: CoCreateInstance + the COM vtable (IUnknown 0-2, IModalWindow::Show 3,
    //! IFileDialog: SetOptions 9, GetOptions 10, SetTitle 17, GetResult 20; IShellItem::GetDisplayName 5).
    //! ⚠ untested on Windows (written in the cloud; layout checked against ShObjIdl_core.h).
    use std::ffi::c_void;
    use std::path::PathBuf;

    #[repr(C)]
    struct Guid(u32, u16, u16, [u8; 8]);
    const CLSID_FILE_OPEN_DIALOG: Guid = Guid(0xDC1C5A9C, 0xE88A, 0x4DDE, [0xA5, 0xA1, 0x60, 0xF8, 0x2A, 0x20, 0xAE, 0xF7]);
    const IID_IFILE_OPEN_DIALOG: Guid = Guid(0xD57C7288, 0xD4AD, 0x4768, [0xBE, 0x02, 0x9D, 0x96, 0x95, 0x32, 0xD9, 0x60]);
    const FOS_PICKFOLDERS: u32 = 0x20;
    const FOS_FORCEFILESYSTEM: u32 = 0x40;
    const SIGDN_FILESYSPATH: i32 = 0x80058000u32 as i32;

    type VT = *const *const c_void;
    unsafe fn slot(obj: *mut c_void, i: usize) -> *const c_void {
        let vt: VT = *(obj as *const VT);
        *vt.add(i)
    }
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn pick_folder(title: &str) -> Option<PathBuf> {
        unsafe {
            let ole = libloading::Library::new("ole32.dll").ok()?;
            let init: libloading::Symbol<unsafe extern "system" fn(*mut c_void, u32) -> i32> = ole.get(b"CoInitializeEx\0").ok()?;
            let create: libloading::Symbol<unsafe extern "system" fn(*const Guid, *mut c_void, u32, *const Guid, *mut *mut c_void) -> i32> =
                ole.get(b"CoCreateInstance\0").ok()?;
            let free: libloading::Symbol<unsafe extern "system" fn(*mut c_void)> = ole.get(b"CoTaskMemFree\0").ok()?;
            let uninit: libloading::Symbol<unsafe extern "system" fn()> = ole.get(b"CoUninitialize\0").ok()?;
            let hr = init(std::ptr::null_mut(), 0x2); // COINIT_APARTMENTTHREADED
            let mut d: *mut c_void = std::ptr::null_mut();
            let mut out = None;
            if create(&CLSID_FILE_OPEN_DIALOG, std::ptr::null_mut(), 1, &IID_IFILE_OPEN_DIALOG, &mut d) >= 0 && !d.is_null() {
                let get_opts: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32 = std::mem::transmute(slot(d, 10));
                let set_opts: unsafe extern "system" fn(*mut c_void, u32) -> i32 = std::mem::transmute(slot(d, 9));
                let set_title: unsafe extern "system" fn(*mut c_void, *const u16) -> i32 = std::mem::transmute(slot(d, 17));
                let show: unsafe extern "system" fn(*mut c_void, isize) -> i32 = std::mem::transmute(slot(d, 3));
                let result: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32 = std::mem::transmute(slot(d, 20));
                let release: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(slot(d, 2));
                let mut o = 0u32;
                get_opts(d, &mut o);
                set_opts(d, o | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);
                let t = wide(title);
                set_title(d, t.as_ptr());
                if show(d, 0) >= 0 {
                    let mut item: *mut c_void = std::ptr::null_mut();
                    if result(d, &mut item) >= 0 && !item.is_null() {
                        let name: unsafe extern "system" fn(*mut c_void, i32, *mut *mut u16) -> i32 = std::mem::transmute(slot(item, 5));
                        let irel: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(slot(item, 2));
                        let mut p: *mut u16 = std::ptr::null_mut();
                        if name(item, SIGDN_FILESYSPATH, &mut p) >= 0 && !p.is_null() {
                            let mut n = 0;
                            while *p.add(n) != 0 {
                                n += 1;
                            }
                            out = Some(PathBuf::from(String::from_utf16_lossy(std::slice::from_raw_parts(p, n))));
                            free(p as *mut c_void);
                        }
                        irel(item);
                    }
                }
                release(d);
            }
            if hr >= 0 {
                uninit();
            }
            out
        }
    }
}
