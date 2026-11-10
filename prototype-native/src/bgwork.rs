//! r2-threads (PORT_LEFTOVERS L3-L55): work the original did off the UI thread, done on a worker here too.
//! The blocking part runs on a thread (`lib_net::bg` or a plain thread), the result is applied on the UI thread.
use crate::{AppWindow, Ed, Play};
use slint::ComponentHandle;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Work on a thread that reports several results as it goes (a clip at a time): `item` runs on the UI thread for
/// each one, `end` once the thread is done and every item was handled.
pub fn stream<T: Send + 'static>(work: impl FnOnce(std::sync::mpsc::Sender<T>) + Send + 'static, mut item: impl FnMut(T) + 'static, end: impl FnOnce() + 'static) {
    let (tx, rx) = std::sync::mpsc::channel::<T>();
    std::thread::spawn(move || work(tx));
    let end = std::cell::RefCell::new(Some(end));
    let holder = std::rc::Rc::new(std::cell::RefCell::new(None::<slint::Timer>));
    let h2 = holder.clone();
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_millis(50), move || loop {
        match rx.try_recv() {
            Ok(v) => item(v),
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                let x = h2.borrow_mut().take();
                slint::Timer::single_shot(Duration::from_millis(0), move || drop(x));
                if let Some(f) = end.borrow_mut().take() {
                    f();
                }
                return;
            }
        }
    });
    *holder.borrow_mut() = Some(t);
}

/// L12: the sidebar health line's slow parts (the buffer's newest segment, free space - Linux runs `df`), measured on
/// a worker at most every 4 s like Echo's get_status; (buffer age s, free bytes, when measured)
static HEALTH: std::sync::Mutex<(Option<f64>, u64, Option<std::time::Instant>)> = std::sync::Mutex::new((None, 0, None));
static HEALTH_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// L12: (buffer age, free bytes on the clips drive; 0 = unknown) from the last measurement - never waits. A new one
/// starts on a worker when the last is 4 s old.
pub fn health(r: &'static echo_core::recorder::Recorder, clips: &Path) -> (Option<f64>, u64) {
    use std::sync::atomic::Ordering;
    let (age, free, at) = *HEALTH.lock().unwrap_or_else(|e| e.into_inner());
    if at.is_none_or(|t| t.elapsed() >= Duration::from_secs(4)) && !HEALTH_BUSY.swap(true, Ordering::Relaxed) {
        let clips = clips.to_path_buf();
        std::thread::spawn(move || {
            let age = r.buffer_age();
            #[cfg(windows)]
            let free = echo_core::winutil::disk_free(&clips.to_string_lossy());
            #[cfg(not(windows))]
            let free = echo_core::linuxutil::disk_free(&clips.to_string_lossy());
            *HEALTH.lock().unwrap_or_else(|e| e.into_inner()) = (age, free, Some(std::time::Instant::now()));
            HEALTH_BUSY.store(false, Ordering::Relaxed);
        });
    }
    (age, free)
}

/// L52: start a program that ends by itself (xdg-open, a file manager call…) and wait for it on a thread, so it
/// doesn't stay behind as a zombie process
#[allow(dead_code)] // the callers are Linux-only
pub fn spawn_reaped(c: &mut std::process::Command) {
    if let Ok(mut ch) = c.spawn() {
        std::thread::spawn(move || {
            let _ = ch.wait();
        });
    }
}

/// L29: Echo's shared_clips() is not empty (shared.json + links that still work) - asked again only when one of the
/// two files changed or a minute went by (links expire), not on every Home fill
pub fn shared_any(clips: &Path) -> bool {
    type Stamp = Option<(Option<std::time::SystemTime>, u64)>;
    thread_local! {
        static SHARED: std::cell::RefCell<Option<([Stamp; 2], std::time::Instant, bool)>> = const { std::cell::RefCell::new(None) };
    }
    let th = clips.join(".thumbs");
    let stamp = |n: &str| std::fs::metadata(th.join(n)).ok().map(|m| (m.modified().ok(), m.len()));
    let key = [stamp("shared.json"), stamp("links.json")];
    if let Some(v) = SHARED.with(|s| s.borrow().as_ref().filter(|x| x.0 == key && x.1.elapsed() < Duration::from_secs(60)).map(|x| x.2)) {
        return v;
    }
    let v = crate::rec_core::rec().is_some_and(|r| !r.shared_clips().is_empty());
    SHARED.with(|s| *s.borrow_mut() = Some((key, std::time::Instant::now(), v)));
    v
}

/// L37: the clips folder is read on a worker at start. Small libraries are back within `wait` (the window then
/// shows them at once, as before); a big one (HDD / network folder) no longer holds the window up - it opens with
/// an empty list and the clips come in when read (`scan_finish`).
pub fn scan_start(dir: &Path, wait: Duration) -> (Vec<crate::library::Clip>, Option<std::sync::mpsc::Receiver<Vec<crate::library::Clip>>>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let d = dir.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(crate::library::scan(&d));
    });
    match rx.recv_timeout(wait) {
        Ok(v) => (v, None),
        Err(_) => (vec![], Some(rx)),
    }
}

/// L37: the start-up read came back late: the library takes the clips and every view follows (`sync` = main.rs's)
pub fn scan_finish(a: &AppWindow, lib: &std::rc::Rc<crate::library::LibModel>, rx: std::sync::mpsc::Receiver<Vec<crate::library::Clip>>, sync: std::rc::Rc<dyn Fn(bool)>) {
    lib.set_loading(true);
    let (w, lib) = (a.as_weak(), lib.clone());
    let holder = std::rc::Rc::new(std::cell::RefCell::new(None::<slint::Timer>));
    let h2 = holder.clone();
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_millis(50), move || {
        let clips = match rx.try_recv() {
            Ok(v) => v,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(_) => vec![],
        };
        let x = h2.borrow_mut().take();
        slint::Timer::single_shot(Duration::from_millis(0), move || drop(x));
        let n = clips.len();
        lib.load_scanned(clips);
        lib.refresh();
        sync(true);
        if let Some(a) = w.upgrade() {
            if a.get_view() == "library" {
                a.invoke_pick_filter(a.get_filter());
            }
        }
        eprintln!("library: {n} clips read after the window opened");
    });
    *holder.borrow_mut() = Some(t);
}

thread_local! {
    /// a disk sync is running on its worker (the next 3 s tick skips instead of stacking a second walk)
    static SYNC_BUSY: Cell<bool> = const { Cell::new(false) };
}

/// L3: every 3 s, clips deleted / added outside Echo (file manager) leave / join the library. The clips tree walk
/// and Echo's cloud.json run on a worker; the library follows on the UI thread.
pub fn disk_tick(a: &AppWindow, lib: &std::rc::Rc<crate::library::LibModel>, root: &Path) {
    if lib.loading() || SYNC_BUSY.with(|b| b.replace(true)) {
        return;
    }
    let known = lib.known_paths();
    let r2: PathBuf = root.to_path_buf();
    let root = root.to_path_buf();
    let (w, lib) = (a.as_weak(), lib.clone());
    crate::lib_net::bg(
        move || crate::library::disk_scan(&r2, known),
        move |scan| {
            SYNC_BUSY.with(|b| b.set(false));
            let Some(a) = w.upgrade() else { return };
            if !lib.sync_apply(&root, scan) {
                return;
            }
            lib.refresh();
            crate::home_fill(&a, &lib);
            // Chat D: only in the library - pick_filter switches the view, it pulled Home / Settings away
            if a.get_view() == "library" {
                a.invoke_pick_filter(a.get_filter());
            }
            crate::UI_SYNC.with(|f| {
                if let Some(f) = f.borrow().as_ref() {
                    f(true)
                }
            });
            // the open clip was deleted: close the player (not while editing - the editor keeps its copy in memory)
            if a.get_player_open() && !a.global::<Ed>().get_open() && crate::cloud_live::open_clip().is_some_and(|i| i < lib.len() && lib.clip(i).gone.get()) {
                let w2 = a.as_weak();
                slint::Timer::single_shot(Duration::from_millis(0), move || {
                    if let Some(a) = w2.upgrade() {
                        a.global::<Play>().invoke_close();
                    }
                });
            }
        },
    );
}
