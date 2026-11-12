//! Chat D - card actions that are real now: drag cards onto sidebar folders, inline rename (click the name / F2),
//! the card menu's "Move to" (Echo's cardDrag / moveClips / rename_clip).
//! 10 Nov 2026: the card's right-click menu is Echo's own (ui/library.slint CardMenu) + Echo's clip jobs: Convert to
//! AV1 / Make smaller, asked first, run one at a time with one progress box (Echo's queueJobs / runJobs).
use crate::library::{self, Clip, LibModel};
use crate::{AppWindow, Flip, Lib, Play};
use echo_core::recorder;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

thread_local! {
    /// clip path -> already AV1? (Echo's av1Of: asked once per clip when its menu opens, ffprobe is quick)
    static AV1_OF: RefCell<HashMap<PathBuf, bool>> = RefCell::new(HashMap::new());
    /// bumps on every menu: a late ffprobe answer for an older menu is dropped
    static MENU_GEN: Cell<u64> = const { Cell::new(0) };
    static JOBS: RefCell<Jobs> = RefCell::new(Jobs::default());
}

/// r2-threads (L4): the AV1 answer for this clip, if one was asked already (Copy skips its ffprobe then)
pub fn av1_known(p: &std::path::Path) -> Option<bool> {
    AV1_OF.with(|m| m.borrow().get(p).copied())
}

/// r2-threads (L4): remember an AV1 answer (Copy's own ffprobe)
pub fn av1_note(p: &std::path::Path, is: bool) {
    AV1_OF.with(|m| m.borrow_mut().insert(p.to_path_buf(), is));
}

struct Job {
    path: PathBuf,
    name: String,
    factor: f64,
    av1: bool,
}

/// Echo's jobs map + jobBox: the clips still waiting, the one running, the totals for the last toast
#[derive(Default)]
struct Jobs {
    wait: VecDeque<Job>,
    running: Option<PathBuf>,
    run_name: String,
    run_av1: bool,
    done: usize,
    ok: usize,
    saved: i64,
    last: Option<(u64, u64)>,
    box_id: Option<i32>,
    /// 0..1000 of the running clip (the worker thread writes it)
    pct: Option<Arc<AtomicU32>>,
    poll: Option<slint::Timer>,
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// after a clip was replaced: the library, Home and sidebar follow (sizes, thumbnails)
fn lib_changed(a: &AppWindow) {
    if let Some(l) = crate::LIB.with(|l| l.borrow().clone()) {
        l.refresh();
        crate::home_fill(a, &l);
    }
    crate::UI_SYNC.with(|f| {
        if let Some(f) = f.borrow().as_ref() {
            f(true)
        }
    });
}

/// gaps-library row 2: what a clip's card shows on its picture (Echo's drawJob): ("Waiting…" | "Making smaller 42%",
/// percent, waiting). None = no job for it. Read by library.rs card_for.
pub fn job_of(path: &std::path::Path) -> Option<(String, f32, bool)> {
    JOBS.with(|j| {
        let j = j.borrow();
        if j.running.as_deref() == Some(path) {
            let f = j.pct.as_ref().map(|p| p.load(Ordering::Relaxed) as f32 / 10.0).unwrap_or(0.0);
            let label = if j.run_av1 { "Converting to AV1" } else { "Making smaller" };
            return Some((format!("{label} {}%", f.round() as u32), f, false));
        }
        j.wait.iter().any(|w| w.path == path).then(|| ("Waiting…".to_string(), 0.0, true))
    })
}

/// repaint the card(s) of this clip (library + Home)
fn touch(path: &std::path::Path) {
    if let Some(l) = crate::LIB.with(|l| l.borrow().clone()) {
        if let Some(i) = l.index_of_path(path) {
            l.changed(i);
        }
    }
}

/// Echo's queueJobs: adds clips (on this PC, not queued yet) and starts the queue if it isn't running
fn queue_jobs(a: &AppWindow, list: Vec<Rc<Clip>>, factor: f64, av1: bool) {
    let added = JOBS.with(|j| {
        let mut j = j.borrow_mut();
        let mut n = vec![];
        for c in list {
            if c.cloud.is_some() || j.running.as_ref() == Some(&c.path) || j.wait.iter().any(|w| w.path == c.path) {
                continue;
            }
            j.wait.push_back(Job { path: c.path.clone(), name: c.shown.clone(), factor, av1 });
            n.push(c.path.clone());
        }
        n
    });
    // their cards show "Waiting…" (outside the borrow: a repaint reads JOBS)
    added.iter().for_each(|p| touch(p));
    let added = added.len();
    let running = JOBS.with(|j| j.borrow().box_id.is_some());
    if added > 0 && running {
        crate::toast(a, if added > 1 { format!("{added} clips added to the queue") } else { "Added to the queue - it starts after the current one".to_string() });
        return;
    }
    if added > 1 {
        crate::toast(a, format!("{added} clips queued - each one shows its progress"));
    }
    if added == 0 {
        return;
    }
    // one box for the whole queue: "2 of 5: name · 40%", Cancel = drop the clips still waiting
    let id = crate::prog_box(if av1 { "Converting to AV1" } else { "Making clips smaller" });
    crate::prog_set(id, None, Some("Starting…"), Some(0.0));
    let w = a.as_weak();
    crate::prog_btn(id, "Cancel", move || {
        let n = JOBS.with(|j| {
            let mut j = j.borrow_mut();
            let n = j.wait.len();
            let gone: Vec<PathBuf> = j.wait.drain(..).map(|w| w.path).collect();
            drop(j);
            gone.iter().for_each(|p| touch(p));
            n
        });
        if let Some(a) = w.upgrade() {
            crate::toast(&a, if n > 0 { format!("Cancelled {n} waiting clip{} - the one in progress still finishes", plural(n)) } else { "The clip in progress still finishes".to_string() });
        }
    });
    JOBS.with(|j| {
        let mut j = j.borrow_mut();
        j.box_id = Some(id);
        j.done = 0;
        j.ok = 0;
        j.saved = 0;
        j.last = None;
        let t = slint::Timer::default();
        t.start(slint::TimerMode::Repeated, Duration::from_millis(400), || {
            JOBS.with(|j| {
                let j = j.borrow();
                let (Some(id), Some(p)) = (j.box_id, j.pct.as_ref()) else { return };
                let f = p.load(Ordering::Relaxed) as f32 / 1000.0;
                let all = j.done + 1 + j.wait.len();
                crate::prog_set(id, None, Some(&format!("{} of {all}: {} · {}%", j.done + 1, j.run_name, (f * 100.0) as u32)), Some((j.done as f32 + f) / all as f32 * 100.0));
                if let Some(p) = j.running.clone() {
                    drop(j);
                    touch(&p);
                }
            });
        });
        j.poll = Some(t);
    });
    next_job(a);
}

/// Echo's runJobs loop: the next waiting clip, or the end (toast + the box goes)
fn next_job(a: &AppWindow) {
    let Some(r) = crate::rec_core::rec() else { return };
    let job = JOBS.with(|j| {
        let mut j = j.borrow_mut();
        let job = j.wait.pop_front();
        j.running = job.as_ref().map(|x| x.path.clone());
        j.run_name = job.as_ref().map(|x| x.name.clone()).unwrap_or_default();
        j.run_av1 = job.as_ref().is_some_and(|x| x.av1);
        j.pct = job.as_ref().map(|_| Arc::new(AtomicU32::new(0)));
        job
    });
    let Some(job) = job else {
        let (id, ok, saved, last) = JOBS.with(|j| {
            let mut j = j.borrow_mut();
            j.poll = None;
            (j.box_id.take(), j.ok, j.saved, j.last)
        });
        match (ok, last) {
            (n, _) if n > 1 => crate::toast(a, format!("{n} clips done · {} saved", library::fmt_size(saved.max(0) as u64))),
            (1, Some((b, af))) => crate::toast(a, format!("{} → {}", library::fmt_size(b), library::fmt_size(af))),
            _ => {}
        }
        if let Some(id) = id {
            crate::prog_set(id, None, Some("Done"), Some(100.0));
            crate::prog_done(id, 1200);
        }
        return;
    };
    touch(&job.path);
    let pct = JOBS.with(|j| j.borrow().pct.clone()).unwrap_or_default();
    let (path, factor, av1, name) = (job.path.to_string_lossy().to_string(), job.factor, job.av1, job.name);
    let w = a.as_weak();
    crate::lib_net::bg(
        move || r.compress_clip(&path, factor, av1, &|f| pct.store((f.clamp(0.0, 1.0) * 1000.0) as u32, Ordering::Relaxed)),
        move |res| {
            let Some(a) = w.upgrade() else { return };
            let p = JOBS.with(|j| {
                let mut j = j.borrow_mut();
                j.done += 1;
                if let Ok((b, af)) = res {
                    j.ok += 1;
                    j.saved += b as i64 - af as i64;
                    j.last = Some((b, af));
                }
                j.running.take()
            });
            match res {
                Ok(_) if av1 => {
                    if let Some(p) = p {
                        AV1_OF.with(|m| m.borrow_mut().insert(p, true));
                    }
                }
                Ok(_) => {}
                Err(e) => crate::toast(&a, format!("{name}: {e}")),
            }
            lib_changed(&a);
            next_job(&a);
        },
    );
}

/// the player holds the file open (mpv): Windows then refuses to replace it - close it first (not the editor)
fn let_go(a: &AppWindow) -> u64 {
    crate::hover_vid::stop(a);
    a.global::<Flip>().set_row(-1);
    if a.get_player_open() && !a.global::<crate::Ed>().get_open() {
        a.global::<Play>().invoke_close();
        250
    } else {
        0
    }
}

/// the card menu's Convert to AV1 / Make smaller / Make much smaller: Echo asks first, then queues it
fn shrink_one(a: &AppWindow, c: Rc<Clip>, what: &str) {
    if c.cloud.is_some() {
        crate::toast(a, "This clip is only in the cloud - download it first");
        return;
    }
    let av1 = what == "av1";
    let factor = match what {
        "third" => 0.33,
        _ => 0.5,
    };
    let (title, msg, btn) = if av1 {
        (
            "Convert this clip to AV1?".to_string(),
            format!("\"{}\" is re-encoded with your graphics card's AV1 encoder and replaced: about half the size, same quality, all audio tracks kept. When you copy or share it, Echo sends an H.264 version so it plays everywhere.", c.shown),
            "Convert",
        )
    } else {
        (
            "Make this clip smaller?".to_string(),
            format!("\"{}\" is re-encoded at {}% of its quality budget and replaced. Same length, all audio tracks kept.", c.shown, (factor * 100.0f64).round()),
            "Make smaller",
        )
    };
    crate::ask(a, &title, &msg, &[("Cancel", "", ""), (btn, "ok", "delb")], None, "ok", move |a, v, _| {
        if v != "ok" {
            return;
        }
        let wait = let_go(a);
        let w = a.as_weak();
        slint::Timer::single_shot(Duration::from_millis(wait), move || {
            if let Some(a) = w.upgrade() {
                queue_jobs(&a, vec![c], factor, av1);
            }
        });
    });
}

/// gaps-library row 2: the old card action "smaller" / "smaller3" (lib_set::smaller) goes through the same confirm + queue
pub fn shrink_at(a: &AppWindow, idx: usize, factor: f64) {
    if let Some(l) = crate::LIB.with(|l| l.borrow().clone()) {
        if idx < l.len() {
            shrink_one(a, l.clip(idx), if factor < 0.4 { "third" } else { "half" });
        }
    }
}

/// Echo's #pickav1: the picked clips (on this PC) to AV1, asked once for all of them
fn av1_picked(a: &AppWindow, lib: &Rc<LibModel>) {
    let mut ids: Vec<usize> = lib.picked.borrow().iter().cloned().collect();
    ids.sort();
    let sel: Vec<Rc<Clip>> = ids.into_iter().filter(|&i| i < lib.len()).map(|i| lib.clip(i)).filter(|c| c.cloud.is_none() && !c.gone.get()).collect();
    if sel.is_empty() {
        crate::toast(a, "Pick clips that are on this PC");
        return;
    }
    let total: u64 = sel.iter().map(|c| c.size).sum();
    let n = sel.len();
    let msg = format!(
        "About {} becomes roughly {}. Each clip is replaced when it's done (same quality, all audio tracks kept); copies and share links still go out as H.264. They run one at a time - you can keep using Echo.",
        library::fmt_size(total),
        library::fmt_size(total / 2)
    );
    crate::ask(a, &format!("Convert {n} clip{} to AV1?", plural(n)), &msg, &[("Cancel", "", ""), ("Convert", "ok", "delb")], None, "ok", move |a, v, _| {
        if v != "ok" {
            return;
        }
        a.invoke_pick_clear();
        let wait = let_go(a);
        let w = a.as_weak();
        slint::Timer::single_shot(Duration::from_millis(wait), move || {
            if let Some(a) = w.upgrade() {
                queue_jobs(&a, sel, 0.5, true);
            }
        });
    });
}

/// the clips a card action works on: the whole selection when the card is part of it, else just that card
fn targets(lib: &LibModel, idx: usize) -> Vec<usize> {
    let p = lib.picked.borrow();
    if p.contains(&idx) && p.len() > 1 {
        let mut v: Vec<usize> = p.iter().cloned().collect();
        v.sort();
        v
    } else {
        vec![idx]
    }
}

/// the card menu's "Move to" list (sidebar order). Called whenever the sidebar is rebuilt.
pub fn fill_folders(a: &AppWindow, lib: &LibModel) {
    let v: Vec<SharedString> = lib.move_targets().into_iter().map(|(f, _)| SharedString::from(f)).collect();
    let l = a.global::<Lib>();
    let cur = l.get_folders();
    use slint::Model;
    if cur.row_count() == v.len() && (0..v.len()).all(|i| cur.row_data(i).as_ref() == Some(&v[i])) {
        return;
    }
    l.set_folders(ModelRc::new(VecModel::from(v)));
}

/// Echo's moveClips: stop previews / close the player on these clips first (Windows os error 32), then move.
pub fn move_ui(a: &AppWindow, lib: &Rc<LibModel>, ids: Vec<usize>, folder: String, sync: Rc<dyn Fn(bool)>) {
    let to = library::clean_folder(&folder);
    let todo: Vec<usize> = ids.into_iter().filter(|&i| i < lib.len() && !lib.clip(i).gone.get() && lib.clip(i).folder != to).collect();
    if todo.is_empty() {
        crate::toast(a, "Already in that folder");
        return;
    }
    // the hover flipbook doesn't hold the file, but the player (mpv) does: let go of it first
    a.global::<Flip>().set_row(-1);
    let wait = if a.get_player_open() && !a.global::<crate::Ed>().get_open() {
        a.global::<Play>().invoke_close();
        250
    } else {
        60
    };
    let (w, lib) = (a.as_weak(), lib.clone());
    let was = crate::undo::before_move(&lib, &todo); // 2.0.2: Undo moves them back
    // r2-threads (L13): the files move on a worker
    let l2 = lib.clone();
    slint::Timer::single_shot(Duration::from_millis(wait), move || l2.move_clips_bg(&todo, &to.clone(), move |n, err| {
        let Some(a) = w.upgrade() else { return };
        if n > 0 {
            lib.clear_picks();
            crate::undo::moved(&a, &lib, was, n, &to, err); // Echo's "Moved N clips to X" + Undo
        } else if let Some(e) = err {
            crate::toast(&a, e);
        }
        lib.refresh();
        sync(true);
    }));
}

/// test hook (11 Nov 2026): ECHO_CARD_SNAP=<folder> - every 1.5 s the window is drawn with take_snapshot (no screen
/// needed: works for a test copy kept off screen) into <folder>/snap.png; a file <folder>/move.txt with "x y" (logical
/// px, window coordinates) moves Slint's pointer there first (card hover without real input), "leave" = pointer out.
fn snap_hook(app: &AppWindow) {
    let Some(dir) = std::env::var_os("ECHO_CARD_SNAP").map(std::path::PathBuf::from) else { return };
    let w = app.as_weak();
    let t = Box::leak(Box::new(slint::Timer::default()));
    t.start(slint::TimerMode::Repeated, Duration::from_millis(1500), move || {
        let Some(a) = w.upgrade() else { return };
        let win = a.window();
        if let Ok(m) = std::fs::read_to_string(dir.join("move.txt")) {
            let _ = std::fs::remove_file(dir.join("move.txt"));
            let v: Vec<f32> = m.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            // 12 Nov 2026: "click x y" / "rclick x y" / "wheel x y dy" too (menus + scrolling without real input)
            let verb = m.split_whitespace().next().unwrap_or("");
            if let ("click" | "rclick", [x, y]) = (verb, &v[..]) {
                use slint::platform::{PointerEventButton as B, WindowEvent as E};
                let (position, button) = (slint::LogicalPosition::new(*x, *y), if verb == "click" { B::Left } else { B::Right });
                win.dispatch_event(E::PointerMoved { position });
                win.dispatch_event(E::PointerPressed { position, button });
                win.dispatch_event(E::PointerReleased { position, button });
            } else if let ("wheel", [x, y, dy]) = (verb, &v[..]) {
                let position = slint::LogicalPosition::new(*x, *y);
                win.dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
                win.dispatch_event(slint::platform::WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: *dy });
            } else if let [x, y] = v[..] {
                win.dispatch_event(slint::platform::WindowEvent::PointerMoved { position: slint::LogicalPosition::new(x, y) });
            } else if m.trim() == "leave" {
                win.dispatch_event(slint::platform::WindowEvent::PointerExited);
            }
            return; // draw on the next tick (the hover lift / preview have started)
        }
        if let Ok(b) = win.take_snapshot() {
            let tmp = dir.join("snap.tmp.png");
            if image::RgbaImage::from_raw(b.width(), b.height(), b.as_bytes().to_vec()).is_some_and(|im| im.save(&tmp).is_ok()) {
                let _ = std::fs::rename(&tmp, dir.join("snap.png"));
            }
        }
    });
}

pub fn wire(app: &AppWindow, lib: Rc<LibModel>, sync: Rc<dyn Fn(bool)>) {
    snap_hook(app);
    // Echo's av1_support, 15 s after start like Echo: does this PC's graphics card encode AV1? (a tiny test encode,
    // once) - only then the menus offer "Convert to AV1". rec_core sets AV1_PLAYS (mpv decodes it in hardware).
    {
        let w = app.as_weak();
        slint::Timer::single_shot(Duration::from_secs(15), move || {
            crate::lib_net::bg(recorder::av1_ready, move |ok| {
                if let Some(a) = w.upgrade() {
                    a.global::<Lib>().set_av1_ok(ok);
                }
            });
        });
    }
    {
        // the menu is about to show: how many clips it works on + whether this one is AV1 already
        let (w, lib) = (app.as_weak(), lib.clone());
        app.global::<Lib>().on_menu_open(move |row| {
            let Some(a) = w.upgrade() else { return };
            let l = a.global::<Lib>();
            let gen = MENU_GEN.with(|g| {
                g.set(g.get() + 1);
                g.get()
            });
            let Some(idx) = lib.index_at(row as usize) else {
                l.set_menu_n(1);
                l.set_menu_av1(false);
                return;
            };
            l.set_menu_n(targets(&lib, idx).len() as i32);
            let c = lib.clip(idx);
            let known = AV1_OF.with(|m| m.borrow().get(&c.path).copied());
            l.set_menu_av1(known.unwrap_or(false));
            if known.is_none() && c.cloud.is_none() {
                let (w, p) = (a.as_weak(), c.path.clone());
                let p2 = p.clone();
                crate::lib_net::bg(
                    move || recorder::is_av1(&p2.to_string_lossy()),
                    move |is| {
                        AV1_OF.with(|m| m.borrow_mut().insert(p, is));
                        if let Some(a) = w.upgrade() {
                            if MENU_GEN.with(|g| g.get()) == gen {
                                a.global::<Lib>().set_menu_av1(is);
                            }
                        }
                    },
                );
            }
        });
    }
    {
        let (w, lib) = (app.as_weak(), lib.clone());
        app.global::<Lib>().on_shrink(move |row, what| {
            let (Some(a), Some(c)) = (w.upgrade(), lib.clip_at(row as usize)) else { return };
            shrink_one(&a, c, what.as_str());
        });
    }
    {
        // several clips picked: the selection bar's actions (Echo's .pickbar), from the right-click menu
        let (w, lib) = (app.as_weak(), lib.clone());
        app.global::<Lib>().on_sel_act(move |what| {
            let Some(a) = w.upgrade() else { return };
            match what.as_str() {
                "combine" | "delete" => a.invoke_pick_act(what),
                "fav" => a.invoke_pick_fav(),
                "clear" => a.invoke_pick_clear(),
                "av1" => av1_picked(&a, &lib),
                _ => {}
            }
        });
    }
    {
        let (w, lib) = (app.as_weak(), lib.clone());
        app.global::<Lib>().on_drag_start(move |row| {
            let Some(a) = w.upgrade() else { return };
            // a hover preview keeps the file open - Windows then refuses rename / move / delete (Echo: stopPreviews)
            crate::hover_vid::stop(&a);
            let Some(idx) = lib.index_at(row as usize) else { return };
            let ids = targets(&lib, idx);
            let label = if ids.len() > 1 { format!("{} clips", ids.len()) } else { lib.clip(idx).shown.clone() };
            a.global::<crate::CDrag>().set_label(label.into());
            // Echo: the hover preview keeps the file open - Windows then refuses the move
            a.global::<Flip>().set_row(-1);
            a.invoke_ctrl_off();
        });
    }
    {
        let (w, lib, sync) = (app.as_weak(), lib.clone(), sync.clone());
        app.global::<Lib>().on_drop_to(move |row, folder| {
            let Some(a) = w.upgrade() else { return };
            // a hover preview keeps the file open - Windows then refuses rename / move / delete (Echo: stopPreviews)
            crate::hover_vid::stop(&a);
            let Some(idx) = lib.index_at(row as usize) else { return };
            move_ui(&a, &lib, targets(&lib, idx), folder.to_string(), sync.clone());
        });
    }
    {
        let (w, lib, sync) = (app.as_weak(), lib.clone(), sync.clone());
        app.global::<Lib>().on_move_to(move |row, folder| {
            let Some(a) = w.upgrade() else { return };
            // a hover preview keeps the file open - Windows then refuses rename / move / delete (Echo: stopPreviews)
            crate::hover_vid::stop(&a);
            let Some(idx) = lib.index_at(row as usize) else { return };
            let ids = targets(&lib, idx);
            if folder == "?" {
                let n = ids.len();
                let title = if n == 1 { "Move to a new folder".to_string() } else { format!("Move {n} clips to a new folder") };
                let (lib, sync) = (lib.clone(), sync.clone());
                crate::ask(&a, &title, "A new folder, or a sub-folder like FiveM/Heists.", &[("Cancel", "", ""), ("Move", "ok", "primary")], Some("Name or Folder/Sub"), "ok", move |a, v, text| {
                    if v == "ok" && !library::clean_folder(&text).is_empty() {
                        move_ui(a, &lib, ids, text, sync);
                    }
                });
                return;
            }
            move_ui(&a, &lib, ids, folder.to_string(), sync.clone());
        });
    }
    {
        let (w, lib, sync) = (app.as_weak(), lib.clone(), sync.clone());
        app.global::<Lib>().on_rename(move |row, name| {
            let Some(a) = w.upgrade() else { return };
            // a hover preview keeps the file open - Windows then refuses rename / move / delete (Echo: stopPreviews)
            crate::hover_vid::stop(&a);
            let Some(idx) = lib.index_at(row as usize) else { return };
            let c = lib.clip(idx);
            let v = name.trim();
            if v.is_empty() || v == c.name || v == c.shown {
                return;
            }
            // the player / editor holds the file: close the player first (not the editor - it has its own project)
            if a.get_player_open() && a.global::<crate::Ed>().get_open() {
                crate::toast(&a, "Close the editor first, then rename the clip");
                return;
            }
            let reopen = a.get_player_open();
            if reopen {
                a.global::<Play>().invoke_close();
            }
            let (w, lib, sync, v) = (a.as_weak(), lib.clone(), sync.clone(), v.to_string());
            // r2-threads (L13): the file rename (it waits up to ~4 s on a locked file) runs on a worker
            let l2 = lib.clone();
            slint::Timer::single_shot(Duration::from_millis(if reopen { 250 } else { 0 }), move || l2.rename_clip_bg(idx, &v, move |res| {
                let Some(a) = w.upgrade() else { return };
                match res {
                    Ok(_) => crate::toast(&a, "Renamed"),
                    Err(e) => crate::toast(&a, e),
                }
                lib.refresh();
                sync(false);
            }));
        });
    }
}
