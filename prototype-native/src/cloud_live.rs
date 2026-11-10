//! gaps-cloud (10 Nov 2026, NATIVE_GAPS rows 5, 6, 24, 26): the cloud things that run by themselves, ported from
//! Echo's ui/index.html:
//! - the player while a cloud-only clip streams in: "Loading from the cloud…" / "Starting the stream…" /
//!   "<size> clip · buffering" with a bar; a broken stream or no picture after 15 s = "Couldn't stream this clip -
//!   downloading it instead", the download's progress inside the video, then the local copy plays (cloudLoading)
//! - background uploads (auto uploads, favourites, Upload to the cloud): the card's bar + "☁ Uploading 40%", one box
//!   at the bottom with ETA + Cancel, the player's "☁ Uploading 40% · 1 min left", the cloud badge when done
//!   (Echo's cloud_up_progress poll)
//! - "Echo Cloud is almost full" / "Echo Cloud is full" popups with Echo's timing rules (ecFullCheck) and the
//!   once-a-day "over your plan" reminder (a data-loss warning)
//! - the sidebar's extra health lines: GPU busy, cloud busy %, cloud usage (drawHealth)
use crate::cloud::{cloud_only, rel_of};
use crate::{AppWindow, Lib, Play};
use echo_core::recorder::{Recorder, UP_CANCEL, UP_PROGRESS};
use slint::ComponentHandle;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

fn lib() -> Option<Rc<crate::library::LibModel>> {
    crate::LIB.with(|l| l.borrow().clone())
}

/// "30 s" / "2 min" (Echo's ETA text)
fn left_text(secs: f64) -> String {
    if secs < 60.0 {
        format!("{} s", secs.round() as u64)
    } else {
        format!("{} min", (secs / 60.0).round() as u64)
    }
}

/// a cloud-only clip opening in the player
struct Stream {
    idx: usize,
    t0: Instant,
    /// the streaming address mpv was given (cloud::play_path) - mpv's "path" tells when THIS clip is loaded
    url: String,
    seen: bool,
    done: bool,
    fallback: bool,
}

#[derive(Default)]
struct UpState {
    last: String,
    t0: Option<Instant>,
    idle_at: Option<Instant>,
    boxid: Option<i32>,
    idx: Option<usize>,
    /// UpProgress.stage last seen: a new step restarts the "(N s)" count and the time-left guess
    stage: String,
}

thread_local! {
    /// main.rs's play(row): opens the player on a view row
    static PLAY: RefCell<Option<Rc<dyn Fn(i32)>>> = const { RefCell::new(None) };
    /// the clip open in the player (from main.rs's tick)
    static OPEN: Cell<Option<usize>> = const { Cell::new(None) };
    static PS: RefCell<Option<Stream>> = const { RefCell::new(None) };
    static UP: RefCell<UpState> = RefCell::new(UpState::default());
    /// (frames, late) at the last sidebar draw (GPU busy line)
    static FRAMES: Cell<Option<(u64, u64)>> = const { Cell::new(None) };
    static TIMERS: RefCell<Vec<slint::Timer>> = const { RefCell::new(vec![]) };
}

/// main.rs hands over its play(row) once
pub fn set_play(f: Rc<dyn Fn(i32)>) {
    PLAY.with(|p| *p.borrow_mut() = Some(f));
}

/// the clip open in the player right now (None = closed)
pub fn open_clip() -> Option<usize> {
    OPEN.with(|o| o.get())
}

/// open the player on this clip again (its file is on this PC now): its row in the library view, else on Home
fn replay(idx: usize) {
    let Some(l) = lib() else { return };
    let row = l.view_row_of(idx).or_else(|| (0..16).map(|k| crate::library::HOME_BASE + k).find(|&r| l.index_at(r) == Some(idx)));
    if let (Some(row), Some(f)) = (row, PLAY.with(|p| p.borrow().clone())) {
        f(row as i32);
    }
}

fn overlay(pl: &Play, title: &str, sub: &str, pct: f32) {
    pl.set_cl_title(title.into());
    pl.set_cl_sub(sub.into());
    pl.set_cl_pct(pct);
    if !pl.get_cl_on() {
        pl.set_cl_on(true);
    }
}

fn overlay_off(pl: &Play) {
    if pl.get_cl_on() {
        pl.set_cl_on(false);
    }
}

// ---------- row 5: the player and cloud-only clips ----------
/// 4x a second from main.rs (after mpv's events are read): the cloud overlay, the stream fallback, the read-only title
pub fn player_tick(a: &AppWindow, m: Option<&crate::mpv::Mpv>, open: Option<usize>) {
    OPEN.with(|o| o.set(open));
    let pl = a.global::<Play>();
    let (Some(idx), Some(l)) = (open, lib()) else {
        overlay_off(&pl);
        PS.with(|s| *s.borrow_mut() = None);
        return;
    };
    if idx >= l.len() {
        overlay_off(&pl);
        return;
    }
    let c = l.clip(idx);
    let only = cloud_only(&c);
    if pl.get_cloud_only() != only {
        pl.set_cloud_only(only); // Echo: the side panel's Title is read-only for cloud clips
    }
    // a download of this clip is running (the fallback, or an action that needs the file): its progress in the video
    if let Some((f, txt)) = crate::cloud::DL_SHOWN.with(|d| d.borrow().get(&rel_of(&c).to_lowercase()).cloned()) {
        overlay(&pl, "Downloading from the cloud…", &txt, f);
        return;
    }
    if !only {
        overlay_off(&pl);
        PS.with(|s| *s.borrow_mut() = None);
        return;
    }
    crate::cloud::keep_stream_alive(); // r2-threads (L6): Echo's 2-minute ensureBase while it's open
    let fresh = PS.with(|s| s.borrow().as_ref().is_none_or(|x| x.idx != idx));
    if fresh {
        // r2-threads (L6): the stream address is worked out on a worker (rclone start-up) - until then this box
        let w = a.as_weak();
        if !crate::cloud::play_ready(&c.path, move || if let Some(a) = w.upgrade() { a.window().request_redraw() }) {
            overlay(&pl, "Loading from the cloud…", "Starting the stream…", -1.0);
            return;
        }
        let url = crate::cloud::play_path(&c.path).to_string_lossy().to_string();
        PS.with(|s| *s.borrow_mut() = Some(Stream { idx, t0: Instant::now(), url, seen: false, done: false, fallback: false }));
    }
    let (done, fallback) = PS.with(|s| s.borrow().as_ref().map(|x| (x.done, x.fallback)).unwrap_or((true, true)));
    if done || fallback {
        overlay_off(&pl);
        return;
    }
    let Some(m) = m else {
        overlay(&pl, "Loading from the cloud…", "Starting the stream…", -1.0);
        return;
    };
    // is mpv on THIS clip yet? (right after opening, its state still describes the previous clip)
    let (seen, elapsed) = PS.with(|s| {
        let mut s = s.borrow_mut();
        let x = s.as_mut().unwrap();
        if !x.seen && m.get("path").is_some_and(|p| p == x.url) {
            x.seen = true;
        }
        (x.seen, x.t0.elapsed())
    });
    // (a stream that fails at once may never show its path: after 1 s an error is this clip's - each load resets it)
    let failed = (seen || elapsed > Duration::from_secs(1)) && m.take_failed();
    let played = seen && m.has_played();
    if played && !failed && m.get("paused-for-cache").as_deref() != Some("yes") {
        PS.with(|s| s.borrow_mut().as_mut().map(|x| x.done = true));
        overlay_off(&pl);
        return;
    }
    // the stream broke, or no picture 15 s after the address was ready (the server already answers by then - Echo
    // waits for that - but the first look-up searches every connected cloud): download the clip instead and play
    // the local copy
    if failed || (!played && elapsed > Duration::from_secs(15)) {
        PS.with(|s| s.borrow_mut().as_mut().map(|x| x.fallback = true));
        m.command(&["stop"]);
        overlay(&pl, "Downloading from the cloud…", "Starting…", 0.0);
        crate::toast(a, "Couldn't stream this clip - downloading it instead");
        // later, not inside main.rs's tick (it holds the player)
        let w = a.as_weak();
        slint::Timer::single_shot(Duration::from_millis(1), move || {
            let Some(a) = w.upgrade() else { return };
            crate::cloud::ensure_local(&a, idx, |a, i| {
                // still this clip in the player: play the downloaded copy (closed meanwhile: just keep the download)
                if a.get_player_open() && open_clip() == Some(i) {
                    replay(i);
                }
            });
        });
        return;
    }
    let dur = m.get_f64("duration").filter(|d| *d > 0.0);
    match dur.filter(|_| seen) {
        Some(d) => {
            let pos = m.get_f64("time-pos").unwrap_or(0.0);
            let end = m.get_f64("demuxer-cache-time").unwrap_or(pos);
            let need = d.min(pos + 3.0).max(0.1);
            overlay(&pl, "Loading from the cloud…", &format!("{} clip · buffering", crate::library::fmt_size(c.size)), (end / need).clamp(0.0, 1.0) as f32);
        }
        None => overlay(&pl, "Loading from the cloud…", "Starting the stream…", -1.0),
    }
}

// ---------- row 6: background upload progress ----------
/// av1-upload-fix (10 Nov 2026): what happens before the upload itself (UpProgress.stage), for the boxes.
/// `dest` = the cloud ("Google Drive"). None = no such step (uploading / getting ready).
pub fn stage_text(stage: &str, dest: &str) -> Option<String> {
    match stage {
        "h264" | "convert" => Some("Converting AV1 to H.264 so it plays everywhere".into()),
        "smaller" => Some("Making a smaller copy for Echo Cloud".into()),
        "fetch" => Some(format!("Downloading from {} to make a share copy", if dest.is_empty() { "the cloud" } else { dest })),
        _ => None,
    }
}

/// stage_text() for small places (card, player, sidebar)
fn stage_short(stage: &str) -> &'static str {
    match stage {
        "fetch" => "Downloading",
        "smaller" => "Making copy",
        _ => "Converting",
    }
}

fn up_tick(a: &AppWindow) {
    if !a.window().is_visible() || !crate::cloud::on() {
        return; // in the tray: nothing to draw
    }
    let idle_wait = UP.with(|u| {
        let u = u.borrow();
        u.last.is_empty() && u.idle_at.is_some_and(|t| t.elapsed() < Duration::from_secs(2))
    });
    if idle_wait {
        return; // nothing uploading: ask every 2 s instead of twice a second
    }
    let p = UP_PROGRESS.lock().map(|p| p.clone()).unwrap_or_default();
    let (finished_one, t0) = UP.with(|u| {
        let mut u = u.borrow_mut();
        if p.rel.is_empty() {
            u.idle_at = Some(Instant::now());
        }
        let mut fin = false;
        if p.rel != u.last {
            fin = !u.last.is_empty();
            u.last = p.rel.clone();
            u.t0 = Some(Instant::now());
            u.idx = None;
        }
        if p.stage != u.stage {
            u.stage = p.stage.clone();
            u.t0 = Some(Instant::now());
        }
        (fin, u.t0.unwrap_or_else(Instant::now))
    });
    // a clip finished (or the next one started): refresh, so it gets its cloud badge
    if finished_one {
        if let Some(l) = lib() {
            l.sync_disk(&l.root());
            l.refresh();
            crate::home_fill(a, &l);
        }
        crate::UI_SYNC.with(|f| {
            if let Some(f) = f.borrow().as_ref() {
                f(true)
            }
        });
    }
    let lb = a.global::<Lib>();
    let pl = a.global::<Play>();
    if p.rel.is_empty() {
        if let Some(id) = UP.with(|u| u.borrow_mut().boxid.take()) {
            crate::prog_set(id, None, None, Some(100.0));
            crate::prog_done(id, 600);
        }
        lb.set_up_row(-1);
        lb.set_up_home(-1);
        pl.set_up_on(false);
        return;
    }
    // av1-upload-fix (10 Nov 2026): while a copy is made the bar shows how far that is (it sat at 0 % for ~40 s
    // as "Getting ready (22 s)…", which looked like a slow server)
    let making = p.got == 0 && stage_text(&p.stage, "").is_some();
    let f = if making {
        (p.stage_f as f64).clamp(0.0, 1.0)
    } else if p.total > 0 {
        (p.got as f64 / p.total as f64).min(1.0)
    } else {
        0.0
    };
    let pc = (f * 100.0).round();
    let secs = t0.elapsed().as_secs_f64();
    let left = (f > 0.03).then(|| secs * (1.0 - f) / f).filter(|l| *l > 1.0);
    let name = p.rel.rsplit('/').next().unwrap_or(&p.rel);
    let name = name.strip_suffix(".mp4").or_else(|| name.strip_suffix(".MP4")).unwrap_or(name).to_string();
    let dest = if p.dest.is_empty() { "the cloud".to_string() } else { p.dest.clone() };
    let dest = dest.strip_prefix("the ").unwrap_or(&dest).to_string();
    let fsz = crate::library::fmt_size;
    let txt = format!(
        "{}{}",
        if p.count > 1 { format!("{} of {} · ", p.idx, p.count) } else { String::new() },
        if p.got > 0 && p.got >= p.total {
            format!("All sent - waiting for {dest} to finish…")
        } else if p.got > 0 {
            // 2.0.2: speed + time left on the box ("4.2 MB/s · 12 s left"); before that (or with no box) as it was
            let rate = UP.with(|u| u.borrow().boxid).and_then(|id| crate::notes::rate(id, p.got, p.total));
            format!("{} of {} · {}", fsz(p.got), fsz(p.total), rate.unwrap_or_else(|| format!("{pc:.0}%{}", left.map(|l| format!(" · about {} left", left_text(l))).unwrap_or_default())))
        } else if making {
            let what = stage_text(&p.stage, &dest).unwrap_or_default();
            let time = left.map(|l| format!(" · about {} left", left_text(l))).unwrap_or_default();
            format!("{what}… {}", if f > 0.0 { format!("{pc:.0}%{time}") } else { format!("({} s)", secs.round() as u64) })
        } else {
            let waited = if secs > 8.0 { format!(" ({} s)", secs.round() as u64) } else { String::new() };
            if p.stage == "send" {
                format!("Starting the upload to {dest}{waited}… · {}", fsz(p.total))
            } else {
                format!("Getting ready{waited}… · {}", fsz(p.total))
            }
        }
    );
    // the clip's card (library view and Home)
    let idx = UP.with(|u| u.borrow().idx).or_else(|| {
        let l = lib()?;
        let i = (0..l.len()).find(|&i| {
            let c = l.clip(i);
            !c.gone.get() && rel_of(&c).eq_ignore_ascii_case(&p.rel)
        })?;
        UP.with(|u| u.borrow_mut().idx = Some(i));
        Some(i)
    });
    let l = lib();
    let row = idx.zip(l.as_ref()).and_then(|(i, l)| l.view_row_of(i)).map(|r| r as i32).unwrap_or(-1);
    let home = idx
        .zip(l.as_ref())
        .and_then(|(i, l)| (0..16).map(|k| crate::library::HOME_BASE + k).find(|&r| l.index_at(r) == Some(i)))
        .map(|r| r as i32)
        .unwrap_or(-1);
    lb.set_up_row(row);
    lb.set_up_home(home);
    lb.set_up_pct(f as f32);
    lb.set_up_label(
        if p.got > 0 {
            format!("☁ Uploading {pc:.0}%")
        } else if making {
            format!("☁ {} {pc:.0}%", stage_short(&p.stage))
        } else {
            "☁ Getting ready…".to_string()
        }
        .into(),
    );
    // its own box at the bottom - not for a share link's upload (that one has its own box already)
    let linking = crate::cloud::LINKING.with(|s| s.borrow().contains(&p.rel.to_lowercase()));
    let boxid = UP.with(|u| u.borrow().boxid);
    match (boxid, linking) {
        (Some(id), true) => {
            crate::prog_done(id, 0);
            UP.with(|u| u.borrow_mut().boxid = None);
        }
        (None, false) => {
            let id = crate::prog_box("");
            let w = a.as_weak();
            crate::prog_btn(id, "Cancel", move || {
                crate::prog_btn(id, "Cancelling…", || {});
                if !UP_PROGRESS.lock().map(|p| p.rel.is_empty()).unwrap_or(true) {
                    UP_CANCEL.store(true, Ordering::Relaxed);
                }
                if let Some(a) = w.upgrade() {
                    crate::toast(&a, "Upload cancelled - the clip stays on this PC (upload it any time from its cloud menu)");
                }
            });
            UP.with(|u| u.borrow_mut().boxid = Some(id));
        }
        _ => {}
    }
    if let Some(id) = UP.with(|u| u.borrow().boxid) {
        let title = format!("Uploading {name}{}", if p.dest.is_empty() { String::new() } else { format!(" to {}", p.dest) });
        crate::prog_set(id, Some(&title), Some(&txt), Some(pc as f32));
    }
    // the player, when that clip is open
    let here = a.get_player_open() && idx.is_some() && open_clip() == idx;
    pl.set_up_on(here);
    if here {
        pl.set_up_pct(f as f32);
        pl.set_up_text(
            if making {
                format!("☁ {} {pc:.0}%", stage_short(&p.stage))
            } else {
                format!("☁ Uploading {pc:.0}%{}", left.map(|l| format!(" · {} left", left_text(l))).unwrap_or_default())
            }
            .into(),
        );
    }
}

// ---------- row 24: Echo Cloud space warnings ----------
fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Echo's front-and-quiet test: Echo is in front and nothing else is open (player, editor, a dialog, the manager)
fn quiet(a: &AppWindow) -> bool {
    a.window().is_visible()
        && !a.window().is_minimized()
        && crate::WIN_FOCUSED.with(|f| f.get())
        && !a.get_player_open()
        && !a.global::<crate::Ed>().get_open()
        && !a.global::<crate::Dlg>().get_open()
        && !a.global::<crate::Ecm>().get_open()
}

/// Echo's ecFullCheck: 90 % = almost full, 100 % = full. Again only when it gets fuller, or after 3 days (full: 1 day).
fn full_check(a: &AppWindow) {
    let Some(r) = crate::rec_core::rec() else { return };
    if !crate::cloud::has_ec() || !quiet(a) {
        return;
    }
    let w = a.as_weak();
    crate::lib_net::bg(move || r.echocloud_me().ok(), move |me| {
        let (Some(a), Some(me)) = (w.upgrade(), me) else { return };
        let (used, quota) = (me["used"].as_u64().unwrap_or(0), me["quota"].as_u64().unwrap_or(0));
        if quota == 0 {
            return;
        }
        let p = used as f64 / quota as f64;
        let level = if p >= 1.0 { 2 } else if p >= 0.9 { 1 } else { 0 };
        let seen = crate::lib_fx::ui_get("ecfull");
        if level == 0 {
            if !seen.is_null() {
                crate::lib_fx::ui_set("ecfull", serde_json::Value::Null);
            }
            return;
        }
        let wait = if level == 2 { 1 } else { 3 } * 86_400_000u64;
        if seen["level"].as_u64().unwrap_or(0) >= level && now_ms().saturating_sub(seen["at"].as_u64().unwrap_or(0)) < wait {
            return;
        }
        if !quiet(&a) {
            return; // something opened while asking the server: next time
        }
        crate::lib_fx::ui_set("ecfull", serde_json::json!({ "level": level, "at": now_ms() }));
        let full = level == 2;
        let fsz = crate::library::fmt_size;
        crate::ask(
            &a,
            if full { "Echo Cloud is full" } else { "Echo Cloud is almost full" },
            &format!(
                "{} of {} used. {} See everything in Echo Cloud, download what you want to keep to this PC, and delete the rest from the cloud.",
                fsz(used),
                fsz(quota),
                if full { "New clips can't be uploaded until you make room." } else { "New uploads stop once it's full." }
            ),
            &[("Not now", "", ""), ("Manage space", "go", "primary")],
            None,
            "", // L47: Echo's choiceBox - Enter does nothing on purpose
            |a, v, _| {
                if v == "go" {
                    crate::cloud::open_settings(a);
                    crate::cloud::space_open(a);
                }
            },
        );
    });
}

/// Echo Cloud over the plan (a paid plan ended): the server removes the oldest clips on delete_at - say so once a day
fn over_plan(a: &AppWindow) {
    let Some(r) = crate::rec_core::rec() else { return };
    if !crate::cloud::has_ec() {
        return;
    }
    let w = a.as_weak();
    crate::lib_net::bg(move || r.echocloud_me().ok(), move |me| {
        let (Some(a), Some(me)) = (w.upgrade(), me) else { return };
        let Some(at) = me["delete_at"].as_i64().filter(|t| *t > 0) else { return };
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        if crate::lib_fx::ui_get("overwarn").as_str() == Some(today.as_str()) {
            return;
        }
        crate::lib_fx::ui_set("overwarn", serde_json::json!(today));
        let Some(d) = chrono::DateTime::from_timestamp(at, 0) else { return };
        let d = d.with_timezone(&chrono::Local).format("%-d %B").to_string();
        crate::toast(&a, format!("Echo Cloud is over your plan - oldest clips are removed on {d}. See Settings → Cloud."));
    });
}

// ---------- row 26: the sidebar's extra health lines ----------
/// (kind 0 ok / 1 warn, text): "GPU busy - N% of frames late …", "☁ <busy> NN%", "☁ 3.1 GB of 10 GB in the cloud"
pub fn health_lines(a: &AppWindow, r: &Recorder) -> Vec<(i32, String)> {
    let mut out = vec![];
    // late frames since the last draw (Windows GPU capture counts them; Linux has no counter, like Echo)
    #[cfg(windows)]
    let now = (echo_core::gpucap::FRAMES.load(Ordering::Relaxed), echo_core::gpucap::LATE.load(Ordering::Relaxed));
    #[cfg(not(windows))]
    let now = (0u64, 0u64);
    if let Some((f0, l0)) = FRAMES.with(|p| p.get()) {
        if now.0 > f0 {
            let pct = now.1.saturating_sub(l0) as f64 / (now.0 - f0) as f64 * 100.0;
            if pct >= 5.0 {
                out.push((1, format!("GPU busy - {}% of frames late (try a lower resolution / FPS)", pct.round())));
            }
        }
    }
    FRAMES.with(|p| p.set(Some(now)));
    let busy = r.cloud_busy.lock().map(|b| b.clone()).unwrap_or_default();
    if !busy.is_empty() {
        let p = UP_PROGRESS.lock().map(|p| p.clone()).unwrap_or_default();
        // av1-upload-fix: while the copy is made, how far that is (it said 0 %)
        let pct = if !p.rel.is_empty() && p.got == 0 && stage_text(&p.stage, "").is_some() {
            format!(" - {} {}%", stage_short(&p.stage).to_lowercase(), (p.stage_f * 100.0).round())
        } else if !p.rel.is_empty() && p.total > 0 {
            format!(" {}%", (p.got as f64 / p.total as f64 * 100.0).round())
        } else {
            String::new()
        };
        out.push((0, format!("☁ {busy}{pct}")));
    }
    // 10 Nov: the usage is the status card's bar now - a line only for a cloud that's almost full, each
    // connected cloud on its own (they were added together)
    for (name, used, total, _) in crate::cloud::usages_for_sidebar(a).unwrap_or_default() {
        if total > 0 && used as f64 / total as f64 > 0.9 {
            let fsz = crate::library::fmt_size;
            out.push((1, format!("☁ {name} almost full - {} of {}", fsz(used), fsz(total))));
        }
    }
    out
}

pub fn wire(app: &AppWindow) {
    let up = slint::Timer::default();
    {
        let w = app.as_weak();
        up.start(slint::TimerMode::Repeated, Duration::from_millis(500), move || {
            if let Some(a) = w.upgrade() {
                up_tick(&a);
            }
        });
    }
    // full / almost full: 25 s after start, then every 15 min
    let full = slint::Timer::default();
    {
        let w = app.as_weak();
        full.start(slint::TimerMode::Repeated, Duration::from_secs(15 * 60), move || {
            if let Some(a) = w.upgrade() {
                full_check(&a);
            }
        });
    }
    {
        let w = app.as_weak();
        slint::Timer::single_shot(Duration::from_secs(25), move || {
            if let Some(a) = w.upgrade() {
                full_check(&a);
            }
        });
    }
    {
        let w = app.as_weak();
        slint::Timer::single_shot(Duration::from_secs(40), move || {
            if let Some(a) = w.upgrade() {
                over_plan(&a);
            }
        });
    }
    TIMERS.with(|t| t.borrow_mut().extend([up, full]));
}
