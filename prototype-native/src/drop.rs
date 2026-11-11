//! gaps-library row 1 (10 Nov 2026): Echo's drag & drop - videos / sounds dragged from Explorer, Discord or the
//! browser's downloads onto the window are imported as clips (Echo's tauri://drag-drop handler):
//! - while dragging: the overlay "Drop videos or sounds (.wav, .mp3…) to add them" (ui/drop.slint)
//! - the target folder = the sidebar folder under the mouse, else the folder being viewed, else the main folder
//! - toast "Importing N files into <folder>…", box "Adding N videos to <folder>" with "2 of 5: name · 40%" + Cancel
//! - "Added N clips to <folder>" / "Import cancelled - N added"; one clip opens in the player
//! - in the editor: pictures / GIFs become stickers, videos and sounds go onto the timeline
//!
//! The import itself is Echo's own (echo-core recorder.import_clips: converts what Echo can't play, sounds become
//! clips in "Sounds", dropped folders are imported whole, keeps the file dates).
//! winit 0.30 sends one HoveredFile / DroppedFile per file and no mouse position during the drag: the files are
//! batched, and on Windows the mouse is read from the OS (GetCursorPos). Linux: winit 0.30 has no drag and drop
//! on Wayland (X11 only, the position there is the last one the window saw): wl_dnd.rs listens itself and calls
//! wl_hover / wl_drop below.
use crate::library;
use crate::{AppWindow, DropG, Lib};
use slint::winit_030::winit;
use slint::ComponentHandle;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PICS: [&str; 6] = ["png", "jpg", "jpeg", "webp", "gif", "bmp"];

thread_local! {
    static PENDING: RefCell<Vec<PathBuf>> = const { RefCell::new(vec![]) };
    static FIRE: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
    static POLL: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
}

/// the mouse in window coordinates (logical px)
fn mouse(win: &winit::window::Window) -> (f32, f32) {
    #[cfg(windows)]
    {
        let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
        // SAFETY: plain Win32 call writing into a local POINT
        if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt) } != 0 {
            if let Ok(o) = win.inner_position() {
                let s = (win.scale_factor() * crate::uiscale::extra()) as f32; // winit's scale x Echo's size
                return ((pt.x - o.x) as f32 / s, (pt.y - o.y) as f32 / s);
            }
        }
    }
    #[cfg(not(windows))]
    let _ = win;
    crate::SCROLL.with(|s| {
        let p = s.borrow().pos;
        (p.x, p.y)
    })
}

fn set_mouse(a: &AppWindow, (x, y): (f32, f32)) {
    let d = a.global::<DropG>();
    d.set_px(x);
    d.set_py(y);
}

fn overlay(a: &AppWindow, on: bool) {
    let d = a.global::<DropG>();
    d.set_editor(a.get_player_open() && a.global::<crate::Ed>().get_open());
    if !on {
        d.set_hit(false);
        POLL.with(|p| *p.borrow_mut() = None);
    }
    d.set_on(on);
}

/// the winit hook (main.rs): HoveredFile / HoveredFileCancelled / DroppedFile
pub fn winit_event(wk: &slint::Weak<AppWindow>, win: &slint::Window, ev: &winit::event::WindowEvent) {
    use slint::winit_030::WinitWindowAccessor;
    let Some(a) = wk.upgrade() else { return };
    let here = || win.with_winit_window(mouse).unwrap_or((0.0, 0.0));
    match ev {
        winit::event::WindowEvent::HoveredFile(_) => {
            if a.global::<DropG>().get_on() {
                return; // one event per file: already showing
            }
            set_mouse(&a, here());
            overlay(&a, true);
            // follow the mouse while it drags (no mouse events reach the window during an OS drag on Windows)
            let w = wk.clone();
            let t = slint::Timer::default();
            t.start(slint::TimerMode::Repeated, Duration::from_millis(60), move || {
                if let Some(a) = w.upgrade() {
                    use slint::winit_030::WinitWindowAccessor;
                    if let Some(p) = a.window().with_winit_window(mouse) {
                        set_mouse(&a, p);
                    }
                }
            });
            POLL.with(|p| *p.borrow_mut() = Some(t));
        }
        winit::event::WindowEvent::HoveredFileCancelled => overlay(&a, false),
        winit::event::WindowEvent::DroppedFile(p) => {
            set_mouse(&a, here());
            files_in(&a, vec![p.clone()]);
        }
        _ => {}
    }
}

/// files are in: act once they are all in (winit sends one DroppedFile per file) and the sidebar row under the
/// mouse has answered
fn files_in(a: &AppWindow, paths: Vec<PathBuf>) {
    PENDING.with(|v| v.borrow_mut().extend(paths));
    let w = a.as_weak();
    let t = slint::Timer::default();
    t.start(slint::TimerMode::SingleShot, Duration::from_millis(120), move || {
        if let Some(a) = w.upgrade() {
            dropped(&a);
        }
    });
    FIRE.with(|f| *f.borrow_mut() = Some(t));
}

/// Linux Wayland (wl_dnd.rs): a drag with files over the window at `p` (logical px), None = it left / was cancelled
#[cfg(target_os = "linux")]
pub fn wl_hover(a: &AppWindow, p: Option<(f32, f32)>) {
    match p {
        Some(p) => {
            set_mouse(a, p);
            if !a.global::<DropG>().get_on() {
                overlay(a, true);
            }
        }
        None => overlay(a, false),
    }
}

/// Linux Wayland (wl_dnd.rs): the files dropped at `p`
#[cfg(target_os = "linux")]
pub fn wl_drop(a: &AppWindow, paths: Vec<PathBuf>, p: (f32, f32)) {
    set_mouse(a, p);
    if paths.is_empty() {
        overlay(a, false);
        crate::toast(a, "Only files from this PC can be dropped here");
        return;
    }
    files_in(a, paths);
}

fn dropped(a: &AppWindow) {
    let paths: Vec<PathBuf> = PENDING.with(|v| std::mem::take(&mut *v.borrow_mut()));
    let d = a.global::<DropG>();
    let (hit, row_folder) = (d.get_hit(), d.get_folder().to_string());
    overlay(a, false);
    if paths.is_empty() {
        return;
    }
    // the editor: into the timeline (not the library); pictures / GIFs become stickers
    if a.get_player_open() && a.global::<crate::Ed>().get_open() {
        for p in paths {
            let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            if PICS.contains(&ext.as_str()) {
                crate::edui::add_sticker(a, p);
            } else {
                crate::edui::add_media_file(a, p);
            }
        }
        return;
    }
    let Some(lib) = crate::LIB.with(|l| l.borrow().clone()) else { return };
    // Echo (chat 30): the sidebar folder under the mouse, else the folder being viewed, else none
    let folder = if hit { row_folder } else { lib.nav.borrow().strip_prefix("game:").unwrap_or("").to_string() };
    import(a, paths, folder);
}

fn s(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// "2 of 5: name · 40%", with what the import is doing (r2-import, 10 Nov 2026 - the box sat on a silent 0% while
/// the video was checked): "2 of 5: name · Checking the video…", then "2 of 5: name · Converting… 40%" / "Copying… 40%"
fn progress_text(i: usize, total: usize, name: &str, stage: &str, f: f64) -> String {
    let pct = (f * 100.0).floor() as u32;
    match stage {
        "" => format!("{} of {total}: {name} · {pct}%", i + 1),
        s if s.starts_with("Checking") => format!("{} of {total}: {name} · {s}", i + 1),
        s => format!("{} of {total}: {name} · {s} {pct}%", i + 1),
    }
}

/// Echo's import_clips with its progress box
fn import(a: &AppWindow, paths: Vec<PathBuf>, folder: String) {
    let Some(r) = crate::rec_core::rec() else {
        crate::toast(a, "Importing needs Echo's recorder - it isn't running");
        return;
    };
    let n = paths.len();
    let into = if folder.is_empty() { String::new() } else { format!(" into {folder}") };
    crate::toast(a, format!("Importing {n} file{}{into}…", s(n)));
    let id = crate::prog_box(&format!("Adding {n} video{}{}", s(n), if folder.is_empty() { String::new() } else { format!(" to {folder}") }));
    crate::prog_set(id, None, Some("Starting…"), Some(0.0));
    crate::prog_btn(id, "Cancel", move || r.import_cancel());
    // (fraction of all files, file index, total, name) - the worker writes it, a timer shows it
    let prog: Arc<Mutex<Option<(f64, usize, usize, String)>>> = Arc::default();
    let poll = slint::Timer::default();
    {
        let prog = prog.clone();
        poll.start(slint::TimerMode::Repeated, Duration::from_millis(250), move || {
            if let Some((f, i, total, name)) = prog.lock().ok().and_then(|g| g.clone()) {
                let text = if i < total { progress_text(i, total, &name, r.import_stage(), f) } else { "Done".to_string() };
                crate::prog_set(id, None, Some(&text), Some((f * 100.0) as f32));
            }
        });
    }
    let list: Vec<String> = paths.iter().map(|p| p.to_string_lossy().to_string()).collect();
    let (w, folder2) = (a.as_weak(), folder.clone());
    crate::lib_net::bg(
        move || {
            r.import_clips(&list, &folder2, &|f, i, total, name| {
                if let Ok(mut g) = prog.lock() {
                    *g = Some((f, i, total, name.to_string()));
                }
            })
        },
        move |(ok, mut bad): (Vec<String>, Vec<String>)| {
            drop(poll);
            crate::prog_done(id, 800);
            let Some(a) = w.upgrade() else { return };
            let cancelled = bad.iter().any(|b| b == "cancelled");
            bad.retain(|b| b != "cancelled");
            let first = added(&a, &ok);
            if cancelled {
                crate::toast(&a, format!("Import cancelled{}", if ok.is_empty() { String::new() } else { format!(" - {} added", ok.len()) }));
                return;
            }
            let to = if folder.is_empty() { String::new() } else { format!(" to {folder}") };
            crate::toast(
                &a,
                if !ok.is_empty() {
                    format!("Added {} clip{}{to}{}", ok.len(), s(ok.len()), if bad.is_empty() { String::new() } else { format!(" - {} failed: {}", bad.len(), bad[0]) })
                } else {
                    bad.first().cloned().unwrap_or_else(|| "Nothing imported".into())
                },
            );
            if ok.len() == 1 {
                if let Some(idx) = first {
                    open(&a, idx);
                }
            }
        },
    );
}

/// the new clips join the library (a sound imported just now isn't "settled" for sync_disk yet: added here)
fn added(a: &AppWindow, ok: &[String]) -> Option<usize> {
    let lib = crate::LIB.with(|l| l.borrow().clone())?;
    let root = lib.root();
    lib.sync_disk(&root);
    let mut first = None;
    for p in ok.iter().map(PathBuf::from) {
        let idx = match lib.index_of_path(&p) {
            Some(i) => Some(i),
            None => library::clip_from(&p, &root).map(|c| lib.add(c)),
        };
        first = first.or(idx);
    }
    lib.refresh();
    crate::home_fill(a, &lib);
    crate::UI_SYNC.with(|f| {
        if let Some(f) = f.borrow().as_ref() {
            f(true)
        }
    });
    first
}

/// one clip imported: it opens in the player (shown in "All clips" when the current view doesn't have it)
fn open(a: &AppWindow, idx: usize) {
    let Some(lib) = crate::LIB.with(|l| l.borrow().clone()) else { return };
    if lib.view_row_of(idx).is_none() {
        a.invoke_pick_filter("all".into());
    }
    if let Some(row) = lib.view_row_of(idx) {
        a.global::<Lib>().invoke_open(row as i32, false);
    }
}
