//! Chat D - the Screenshots page (sidebar "Screenshots"): every picture in <clips>/Screenshots, newest first.
//! Small previews are made with ffmpeg into the prototype's own temp folder (%TEMP%\echo-proto-shots, by name +
//! file time) - Echo's .thumbs is never written. Click = open in the system viewer; Show in folder; Delete =
//! Recycle Bin / Trash after Echo's confirm.
use crate::{AppWindow, Shot, Shots};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

const EXTS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "bmp"];

thread_local! {
    static LIST: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(vec![]) };
    static GEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// gaps-library row 12: the selected screenshots (by file) and the page's search text
    static PICKED: std::cell::RefCell<std::collections::HashSet<PathBuf>> = std::cell::RefCell::new(Default::default());
    static QUERY: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

fn shots_dir(clips: &Path) -> PathBuf {
    clips.join("Screenshots")
}

/// gaps-library row 19: the sidebar's Screenshots count (Echo's n-shots)
pub fn count(clips: &Path) -> usize {
    std::fs::read_dir(shots_dir(clips))
        .map(|rd| rd.flatten().filter(|e| EXTS.contains(&e.path().extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default().as_str())).count())
        .unwrap_or(0)
}

fn list(clips: &Path) -> Vec<(PathBuf, SystemTime, u64)> {
    let mut v = vec![];
    if let Ok(rd) = std::fs::read_dir(shots_dir(clips)) {
        for e in rd.flatten() {
            let p = e.path();
            let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            if !EXTS.contains(&ext.as_str()) {
                continue;
            }
            if let Ok(m) = e.metadata() {
                if m.is_file() {
                    v.push((p, m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));
                }
            }
        }
    }
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

/// a 480 px JPEG preview in our temp folder (made once per file version)
fn preview(ff: &Option<PathBuf>, p: &Path, t: SystemTime) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join("echo-proto-shots");
    let _ = std::fs::create_dir_all(&dir);
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let out = dir.join(format!("{}-{secs}.jpg", p.file_stem()?.to_string_lossy()));
    if out.exists() {
        return Some(out);
    }
    let ff = ff.as_ref()?;
    let mut c = std::process::Command::new(ff);
    c.args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(p).args(["-frames:v", "1", "-vf", "scale=480:-2", "-q:v", "4"]).arg(&out);
    c.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c.status().ok().filter(|s| s.success())?;
    Some(out)
}

/// show the page and (re)load the list
pub fn show(a: &AppWindow, clips: &Path) {
    a.set_view("shots".into());
    a.set_filter("shots".into());
    refresh(a, clips);
}

pub fn refresh(a: &AppWindow, clips: &Path) {
    // gaps-library row 12: Echo's renderShots - the search box filters by name; files that went drop out of the selection
    let mut v = list(clips);
    PICKED.with(|p| p.borrow_mut().retain(|x| x.exists()));
    let q = QUERY.with(|q| q.borrow().trim().to_lowercase());
    if !q.is_empty() {
        v.retain(|x| x.0.file_stem().is_some_and(|s| s.to_string_lossy().to_lowercase().contains(&q)));
    }
    s_picks(a);
    let total: u64 = v.iter().map(|x| x.2).sum();
    let n = v.len();
    let s = a.global::<Shots>();
    s.set_count(format!("{n} screenshot{} · {}", if n == 1 { "" } else { "s" }, crate::library::fmt_size(total)).into());
    // same files as shown: keep the pictures, no flicker
    let same = LIST.with(|l| *l.borrow() == v.iter().map(|x| x.0.clone()).collect::<Vec<_>>());
    if same && s.get_items().row_count() == n {
        repaint_picks(a);
        return;
    }
    LIST.with(|l| *l.borrow_mut() = v.iter().map(|x| x.0.clone()).collect());
    let rows: Vec<Shot> = v
        .iter()
        .map(|(p, t, b)| Shot {
            name: p.file_stem().unwrap_or_default().to_string_lossy().to_string().into(),
            when: format!("{} · {}", crate::library::ago(*t), crate::library::fmt_size(*b)).into(),
            img: Default::default(),
            picked: PICKED.with(|k| k.borrow().contains(p)),
        })
        .collect();
    let model = Rc::new(VecModel::from(rows));
    s.set_items(ModelRc::from(model.clone()));
    // previews in the background, filled in as they come
    let gen = GEN.with(|g| {
        g.set(g.get() + 1);
        g.get()
    });
    let w = a.as_weak();
    std::thread::spawn(move || {
        let ff = crate::sheets::ffmpeg();
        for (i, (p, t, _)) in v.into_iter().enumerate() {
            let Some(px) = preview(&ff, &p, t).and_then(|j| crate::library::decode(&j)) else { continue };
            let w = w.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if GEN.with(|g| g.get()) != gen {
                    return;
                }
                if let Some(a) = w.upgrade() {
                    let m = a.global::<Shots>().get_items();
                    if let Some(mut r) = m.row_data(i) {
                        r.img = slint::Image::from_rgb8(px);
                        m.set_row_data(i, r);
                    }
                }
            });
        }
    });
}

fn path_at(i: i32) -> Option<PathBuf> {
    LIST.with(|l| l.borrow().get(i as usize).cloned())
}

// ---------- gaps-library row 12: selection (Echo's picked on the Screenshots page) ----------
/// the bar's "N screenshots selected"
fn s_picks(a: &AppWindow) {
    a.global::<Shots>().set_picked_n(PICKED.with(|p| p.borrow().len()) as i32);
}

/// the cards' ticks follow the selection (pictures stay)
fn repaint_picks(a: &AppWindow) {
    s_picks(a);
    let m = a.global::<Shots>().get_items();
    LIST.with(|l| {
        for (i, p) in l.borrow().iter().enumerate() {
            let on = PICKED.with(|k| k.borrow().contains(p));
            if let Some(mut r) = m.row_data(i) {
                if r.picked != on {
                    r.picked = on;
                    m.set_row_data(i, r);
                }
            }
        }
    });
}

fn bin() -> &'static str {
    if cfg!(windows) {
        "Recycle Bin"
    } else {
        "Trash"
    }
}

/// Echo's #pickdel on this page: "Delete N screenshots?" (+ how many aren't in the view), then the Recycle Bin
fn delete_picked(a: &AppWindow, clips: PathBuf) {
    let mut sel: Vec<PathBuf> = PICKED.with(|p| p.borrow().iter().cloned().collect());
    sel.sort();
    if sel.is_empty() {
        return;
    }
    let n = sel.len();
    let hidden = LIST.with(|l| sel.iter().filter(|p| !l.borrow().contains(p)).count());
    let hid_note = if hidden > 0 {
        format!(" {hidden} of them {} in the view you're looking at (other folders / filters).", if hidden == 1 { "isn't" } else { "aren't" })
    } else {
        String::new()
    };
    let title = format!("Delete {n} screenshot{}?", if n == 1 { "" } else { "s" });
    crate::confirm_box(a, &title, &format!("They will be moved to the {}.{hid_note}", bin()), "Delete", move |a| {
        // r2-threads (L5): the Recycle Bin / Trash runs on a worker
        let w = a.as_weak();
        crate::lib_net::bg(
            move || {
                let (mut k, mut err) = (0, None::<String>);
                for p in &sel {
                    match crate::library::recycle_file(p) {
                        Ok(()) => k += 1,
                        Err(e) => {
                            err.get_or_insert(e);
                        }
                    }
                }
                (k, err)
            },
            move |(k, err)| {
                let Some(a) = w.upgrade() else { return };
                let a = &a;
                PICKED.with(|p| p.borrow_mut().clear());
                match err {
                    Some(e) => crate::toast(a, e),
                    None => crate::toast(a, format!("Moved {k} screenshot{} to the {}", if k == 1 { "" } else { "s" }, bin())),
                }
                refresh(a, &clips);
                sidebar(a);
            },
        );
    });
}

/// the sidebar's Screenshots count follows (row 19)
fn sidebar(_a: &AppWindow) {
    crate::UI_SYNC.with(|f| {
        if let Some(f) = f.borrow().as_ref() {
            f(true)
        }
    });
}

pub fn wire(app: &AppWindow, clips: PathBuf) {
    // William (9 Nov): open IN the app like Echo's openShot (was: the system viewer - Paint on his PC). The full-size
    // picture is decoded on a worker thread; the viewer (lib_shots.slint ShotViewer) zooms / pans it.
    let w = app.as_weak();
    app.global::<Shots>().on_open(move |i| {
        let Some(p) = path_at(i) else { return };
        let w = w.clone();
        crate::lib_net::bg(
            move || image::open(&p).ok().map(|i| i.to_rgba8()).map(|i| (i.width(), i.height(), i.into_raw())),
            move |r| {
                let Some(a) = w.upgrade() else { return };
                let Some((iw, ih, px)) = r else {
                    crate::toast(&a, "Couldn't open that picture");
                    return;
                };
                let s = a.global::<Shots>();
                s.set_view_img(slint::Image::from_rgba8(slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&px, iw, ih)));
                s.set_view_w(iw as f32);
                s.set_view_h(ih as f32);
                s.set_view_open(true);
            },
        );
    });
    {
        let c = clips.clone();
        app.global::<Shots>().on_open_folder(move || {
            let d = shots_dir(&c);
            let _ = std::fs::create_dir_all(&d);
            #[cfg(windows)]
            let _ = std::process::Command::new("explorer").arg(&d).spawn();
            #[cfg(not(windows))]
            let _ = crate::bgwork::spawn_reaped(std::process::Command::new("xdg-open").arg(&d)); // r2-threads (L52): reaped
        });
    }
    {
        let (w, c) = (app.as_weak(), clips.clone());
        app.global::<Shots>().on_act(move |i, what| {
            let (Some(a), Some(p)) = (w.upgrade(), path_at(i)) else { return };
            if what == "folder" {
                crate::show_in_folder(&p);
                return;
            }
            // gaps-library row 12: Echo's Copy (copy_clip: the file goes on the clipboard, Discord takes it as a picture)
            if what == "copy" {
                match crate::copy::clipboard_file(&p) {
                    Ok(()) => crate::toast(&a, "Screenshot copied - paste it into Discord or anywhere"),
                    Err(e) => crate::toast(&a, e),
                }
                return;
            }
            let name = p.file_stem().unwrap_or_default().to_string_lossy().to_string();
            let c = c.clone();
            crate::confirm_box(&a, "Delete screenshot?", &format!("\"{name}\" goes to the {}.", bin()), "Delete", move |a| {
                match crate::library::recycle_file(&p) {
                    Ok(()) => crate::toast(a, format!("Moved to {}", bin())),
                    Err(e) => crate::toast(a, e),
                }
                PICKED.with(|k| k.borrow_mut().remove(&p));
                refresh(a, &c);
                sidebar(a);
            });
        });
    }
    // gaps-library row 12: select (tick / Ctrl+Shift+click / Ctrl+A), Clear / Esc, batch Delete, search, Grid / List
    {
        let w = app.as_weak();
        app.global::<Shots>().on_pick(move |i| {
            let (Some(a), Some(p)) = (w.upgrade(), path_at(i)) else { return };
            PICKED.with(|k| {
                let mut k = k.borrow_mut();
                if !k.remove(&p) {
                    k.insert(p);
                }
            });
            repaint_picks(&a);
        });
    }
    {
        let w = app.as_weak();
        app.global::<Shots>().on_pick_all(move || {
            let Some(a) = w.upgrade() else { return };
            // what the page shows (search), like Echo's shotView
            LIST.with(|l| PICKED.with(|k| k.borrow_mut().extend(l.borrow().iter().cloned())));
            repaint_picks(&a);
        });
    }
    {
        let w = app.as_weak();
        app.global::<Shots>().on_pick_clear(move || {
            let Some(a) = w.upgrade() else { return };
            PICKED.with(|k| k.borrow_mut().clear());
            repaint_picks(&a);
        });
    }
    {
        let (w, c) = (app.as_weak(), clips.clone());
        app.global::<Shots>().on_del_picked(move || {
            if let Some(a) = w.upgrade() {
                delete_picked(&a, c.clone());
            }
        });
    }
    {
        let (w, c) = (app.as_weak(), clips.clone());
        app.global::<Shots>().on_search(move |q| {
            QUERY.with(|x| *x.borrow_mut() = q.to_string());
            if let Some(a) = w.upgrade() {
                refresh(&a, &c);
            }
        });
    }
    {
        let w = app.as_weak();
        app.global::<Shots>().on_set_view(move |list| {
            if let Some(a) = w.upgrade() {
                a.invoke_set_view(list); // the library's Grid / List (remembered, lib_prefs)
            }
        });
    }
    // new screenshots show up while the page is open (every 3 s)
    let t = slint::Timer::default();
    let w = app.as_weak();
    t.start(slint::TimerMode::Repeated, std::time::Duration::from_secs(3), move || {
        if let Some(a) = w.upgrade() {
            if a.get_view() == "shots" && a.window().is_visible() {
                refresh(&a, &clips);
            }
        }
    });
    std::mem::forget(t);
}
