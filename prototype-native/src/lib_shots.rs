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
}

fn shots_dir(clips: &Path) -> PathBuf {
    clips.join("Screenshots")
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
    let v = list(clips);
    let total: u64 = v.iter().map(|x| x.2).sum();
    let n = v.len();
    let s = a.global::<Shots>();
    s.set_count(format!("{n} screenshot{} · {}", if n == 1 { "" } else { "s" }, crate::library::fmt_size(total)).into());
    // same files as shown: keep the pictures, no flicker
    let same = LIST.with(|l| *l.borrow() == v.iter().map(|x| x.0.clone()).collect::<Vec<_>>());
    if same && s.get_items().row_count() == n {
        return;
    }
    LIST.with(|l| *l.borrow_mut() = v.iter().map(|x| x.0.clone()).collect());
    let rows: Vec<Shot> = v
        .iter()
        .map(|(p, t, b)| Shot {
            name: p.file_stem().unwrap_or_default().to_string_lossy().to_string().into(),
            when: format!("{} · {}", crate::library::ago(*t), crate::library::fmt_size(*b)).into(),
            img: Default::default(),
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
            let _ = std::process::Command::new("xdg-open").arg(&d).spawn();
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
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            let bin = if cfg!(windows) { "Recycle Bin" } else { "Trash" };
            let c = c.clone();
            crate::confirm_box(&a, "Delete screenshot?", &format!("\"{name}\" will be moved to the {bin}."), "Delete", move |a| {
                match crate::library::recycle_file(&p) {
                    Ok(()) => crate::toast(a, "Deleted"),
                    Err(e) => crate::toast(a, e),
                }
                refresh(a, &c);
            });
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
