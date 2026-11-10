//! Echo's #notes: messages + progress boxes in ONE stack (theme.slint ToastLayer). Every popup goes through this
//! list, so they stack (newest at the bottom) instead of covering each other. Moved out of main.rs for 2.0.2:
//! - kinds: info (no icon), done (green tick), error (red, stays until clicked), running (spinner).
//!   `toast()` guesses the kind from the text; `say()` / `say_act()` set it.
//! - a button on a message (Undo, Show in folder, Open, Play): `say_act()`, `merge()`
//! - a finished job's box turns into its result message in place (`prog_done` + the message right after it)
//! - downloads / uploads: `rate()` = "4.2 MB/s · 12 s left"
//! - hovering a message holds its timer, a click puts it away, `merge()` adds repeats up ("3 clips deleted")
use crate::{AppWindow, Note, Notes};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Info = 0,
    Done = 1,
    Err = 2,
    Run = 3,
}

/// speed of a download / upload box (bytes per second, smoothed)
struct Rate {
    last: (Instant, u64),
    speed: f64,
}

struct NoteState {
    model: Rc<VecModel<Note>>,
    next: i32,
    gens: HashMap<i32, u32>, // message id -> timer generation (the same text again restarts it)
    btns: HashMap<i32, Box<dyn Fn()>>,
    /// message id -> how long it stays (ms); none = until clicked (errors) or a box
    hide: HashMap<i32, u64>,
    /// the message under the mouse: its timer waits
    hov: Option<i32>,
    /// merge key -> (message id, count so far)
    keys: HashMap<String, (i32, usize)>,
    rates: HashMap<i32, Rate>,
    /// a box that just finished (prog_done): a message shown right after it, in the same go, takes its place
    morph: Option<i32>,
}
thread_local! {
    static NOTES: RefCell<NoteState> = RefCell::new(NoteState {
        model: Rc::new(VecModel::default()),
        next: 1,
        gens: Default::default(),
        btns: Default::default(),
        hide: Default::default(),
        hov: None,
        keys: Default::default(),
        rates: Default::default(),
        morph: None,
    });
}

pub fn notes_init(a: &AppWindow) {
    NOTES.with(|n| a.global::<Notes>().set_items(ModelRc::from(n.borrow().model.clone())));
    let g = a.global::<Notes>();
    g.on_btn(|id| {
        let f = NOTES.with(|n| n.borrow_mut().btns.remove(&id));
        // a message's button does its thing once, then the message goes (a box's Cancel may relabel itself)
        let msg = NOTES.with(|n| note_row(&n.borrow().model, id).is_some_and(|(_, r)| !r.prog));
        if let Some(f) = f {
            f();
        }
        if msg {
            note_out(id);
        }
    });
    g.on_dismiss(note_out);
    g.on_hover(|id, on| {
        let again = NOTES.with(|n| {
            let mut n = n.borrow_mut();
            if on {
                n.hov = Some(id);
                *n.gens.entry(id).or_insert(0) += 1; // the timer that's running now does nothing
                None
            } else {
                if n.hov == Some(id) {
                    n.hov = None;
                }
                // a moment to read the rest, not the whole time again
                n.hide.get(&id).map(|ms| (*ms / 2).clamp(1500, 3000))
            }
        });
        if let Some(ms) = again {
            arm(id, ms);
        }
    });
}

fn note_row(m: &VecModel<Note>, id: i32) -> Option<(usize, Note)> {
    (0..m.row_count()).find_map(|i| m.row_data(i).filter(|n| n.id == id).map(|n| (i, n)))
}

/// fade a note out, then remove it
pub fn note_out(id: i32) {
    NOTES.with(|n| {
        let n = n.borrow();
        if let Some((i, mut r)) = note_row(&n.model, id) {
            if r.out {
                return;
            }
            r.out = true;
            n.model.set_row_data(i, r);
        }
    });
    slint::Timer::single_shot(Duration::from_millis(220), move || {
        NOTES.with(|n| {
            let mut n = n.borrow_mut();
            if let Some((i, _)) = note_row(&n.model, id) {
                n.model.remove(i);
            }
            n.gens.remove(&id);
            n.btns.remove(&id);
            n.hide.remove(&id);
            n.rates.remove(&id);
            n.keys.retain(|_, (k, _)| *k != id);
            if n.hov == Some(id) {
                n.hov = None;
            }
        })
    });
}

/// still on screen (not fading out)
pub fn alive(id: i32) -> bool {
    NOTES.with(|n| note_row(&n.borrow().model, id).is_some_and(|(_, r)| !r.out))
}

/// (re)start a message's timer: after `ms` it goes - unless the mouse is on it by then (it waits for the mouse to leave)
fn arm(id: i32, ms: u64) {
    let gen = NOTES.with(|n| {
        let mut n = n.borrow_mut();
        let g = n.gens.entry(id).or_insert(0);
        *g += 1;
        let g = *g;
        (n.hov != Some(id)).then_some(g)
    });
    let Some(gen) = gen else { return };
    slint::Timer::single_shot(Duration::from_millis(ms), move || {
        if NOTES.with(|n| n.borrow().gens.get(&id) == Some(&gen)) {
            note_out(id);
        }
    });
}

/// how long a message stays: longer text = longer; a button = a bit longer (time to press Undo); errors stay
fn stay_ms(kind: Kind, text: &str, btn: bool) -> Option<u64> {
    if kind == Kind::Err {
        return None;
    }
    let ms = (text.chars().count() as u64 * 55).clamp(2600, 7000);
    Some(if btn { (ms + 2500).max(6000).min(9000) } else { ms })
}

/// Echo's toast() guessed from the text: errors are red and stay, results get a tick, "…" spins
pub fn guess(s: &str) -> Kind {
    let l = s.to_lowercase();
    const BAD: [&str; 12] = ["couldn't", "could not", "can't", "cannot", "failed", "error", "wasn't found", "isn't there", "is open in", "didn't work", "not enough space", "went wrong"];
    if BAD.iter().any(|k| l.contains(k)) {
        return Kind::Err;
    }
    if s.ends_with('…') || s.ends_with("...") {
        return Kind::Run;
    }
    const GOOD: [&str; 22] = [
        "copied", "saved", "moved", "renamed", "uploaded", "deleted", "merged", "exported", "removed", "imported", "downloaded", "combined", "updated", "following", "added",
        "converted", "restored", "cleared", "done", "made", "picture", "thumbnail",
    ];
    let first = l.split(|c: char| !c.is_alphanumeric()).find(|w| !w.is_empty() && w.parse::<u64>().is_err()).unwrap_or("");
    let first_ok = GOOD.contains(&first) || (first == "clips" || first == "clip") && l.split(' ').nth(2).is_some_and(|w| GOOD.iter().any(|g| w.starts_with(g)));
    if first_ok || l.contains(" copied") || l.contains(" saved") || l.contains(" done") || l.starts_with("profile made") || l.starts_with("profile saved") {
        return Kind::Done;
    }
    Kind::Info
}

/// a message (its kind / button); the same text again (same kind) only restarts its timer; at most 3 messages
fn put(kind: Kind, s: SharedString, btn: Option<(&str, Box<dyn Fn()>)>) -> i32 {
    crate::hdr_home::log_toast(&s); // gaps (10 Nov): errors go into Echo's log too
    let has_btn = btn.is_some();
    let stay = stay_ms(kind, &s, has_btn);
    let (id, extra) = NOTES.with(|n| {
        let mut n = n.borrow_mut();
        let found = (0..n.model.row_count()).filter_map(|i| n.model.row_data(i)).find(|r| !r.prog && !r.out && r.title == s && r.kind == kind as i32);
        let morph = n.morph.take().and_then(|b| note_row(&n.model, b)).filter(|(_, r)| r.prog && !r.out);
        let id = match (found, morph) {
            (Some(r), _) => r.id,
            // the job's box turns into its result (no jump: the stack keeps its place)
            (None, Some((i, mut r))) => {
                r.prog = false;
                r.title = s.clone();
                r.text = "".into();
                r.pct = -1.0;
                r.btn = "".into();
                r.kind = kind as i32;
                n.model.set_row_data(i, r.clone());
                n.btns.remove(&r.id);
                n.rates.remove(&r.id);
                r.id
            }
            (None, None) => {
                let id = n.next;
                n.next += 1;
                n.model.push(Note { id, prog: false, title: s.clone(), text: "".into(), pct: -1.0, fin: false, btn: "".into(), out: false, kind: kind as i32 });
                id
            }
        };
        if let Some((label, f)) = btn {
            if let Some((i, mut r)) = note_row(&n.model, id) {
                r.btn = label.into();
                n.model.set_row_data(i, r);
            }
            n.btns.insert(id, f);
        }
        match stay {
            Some(ms) => n.hide.insert(id, ms),
            None => n.hide.remove(&id),
        };
        // more than 3 messages: the oldest goes
        let msgs: Vec<i32> = (0..n.model.row_count()).filter_map(|i| n.model.row_data(i)).filter(|r| !r.prog && !r.out).map(|r| r.id).collect();
        (id, (msgs.len() > 3).then(|| msgs[0]))
    });
    if let Some(e) = extra {
        note_out(e);
    }
    match stay {
        Some(ms) => arm(id, ms),
        None => NOTES.with(|n| *n.borrow_mut().gens.entry(id).or_insert(0) += 1), // was timed before: not any more
    }
    id
}

/// Echo's toast(): a message, its kind guessed from the text
pub fn toast(_a: &AppWindow, s: impl Into<SharedString>) {
    let s: SharedString = s.into();
    put(guess(&s), s, None);
}

/// a message of this kind
pub fn say(kind: Kind, s: impl Into<SharedString>) -> i32 {
    put(kind, s.into(), None)
}

/// a message with one button ("Undo", "Show in folder", "Open", "Play"); pressing it also puts the message away
pub fn say_act(kind: Kind, s: impl Into<SharedString>, label: &str, f: impl Fn() + 'static) -> i32 {
    put(kind, s.into(), Some((label, Box::new(f))))
}

/// repeats add up: while the last `key` message is still on screen it becomes `text(total)` (and its button is
/// replaced), else a new one starts at `add`. Returns (message id, total).
pub fn merge(key: &str, kind: Kind, add: usize, text: impl Fn(usize) -> String, btn: Option<(&str, Box<dyn Fn()>)>) -> (i32, usize) {
    let prev = NOTES.with(|n| n.borrow().keys.get(key).copied()).filter(|(id, _)| alive(*id));
    let total = prev.map_or(add, |(_, c)| c + add);
    let s: SharedString = text(total).into();
    let id = match prev {
        Some((id, _)) => {
            NOTES.with(|n| {
                let mut n = n.borrow_mut();
                if let Some((i, mut r)) = note_row(&n.model, id) {
                    r.title = s.clone();
                    r.kind = kind as i32;
                    r.btn = btn.as_ref().map(|b| b.0.into()).unwrap_or_default();
                    n.model.set_row_data(i, r);
                }
                n.btns.remove(&id);
                let has = btn.is_some();
                if let Some((_, f)) = btn {
                    n.btns.insert(id, f);
                }
                if let Some(ms) = stay_ms(kind, &s, has) {
                    n.hide.insert(id, ms);
                }
            });
            if let Some(ms) = NOTES.with(|n| n.borrow().hide.get(&id).copied()) {
                arm(id, ms);
            }
            id
        }
        None => put(kind, s, btn),
    };
    NOTES.with(|n| n.borrow_mut().keys.insert(key.to_string(), (id, total)));
    (id, total)
}

/// Echo's progBox(): its own box per job. Returns the id for prog_set / prog_btn / prog_done / prog_finish.
pub fn prog_box(title: &str) -> i32 {
    NOTES.with(|n| {
        let mut n = n.borrow_mut();
        let id = n.next;
        n.next += 1;
        n.model.push(Note { id, prog: true, title: title.into(), text: "".into(), pct: 0.0, fin: false, btn: "".into(), out: false, kind: Kind::Run as i32 });
        id
    })
}

/// change a box: title / text (None = keep), pct (None = keep, < 0 = no bar)
pub fn prog_set(id: i32, title: Option<&str>, text: Option<&str>, pct: Option<f32>) {
    NOTES.with(|n| {
        let n = n.borrow();
        if let Some((i, mut r)) = note_row(&n.model, id) {
            if !r.prog {
                return; // already finished into a message (a late progress tick)
            }
            if let Some(t) = title {
                r.title = t.into();
            }
            if let Some(t) = text {
                r.text = t.into();
            }
            if let Some(p) = pct {
                r.pct = p;
            }
            n.model.set_row_data(i, r);
        }
    });
}

/// a small button on a box (e.g. "Cancel"), once per box
pub fn prog_btn(id: i32, label: &str, f: impl Fn() + 'static) {
    NOTES.with(|n| {
        let mut n = n.borrow_mut();
        if let Some((i, mut r)) = note_row(&n.model, id) {
            r.btn = label.into();
            n.model.set_row_data(i, r);
            n.btns.insert(id, Box::new(f));
        }
    });
}

/// the job ended: the spinner stops; the box goes after `ms` - or, 2.0.2, the message the job shows right after
/// this (its result: "Copied", an error...) takes the box's place, so the box never just vanishes
pub fn prog_done(id: i32, ms: u64) {
    NOTES.with(|n| {
        let mut n = n.borrow_mut();
        n.rates.remove(&id);
        if let Some((i, mut r)) = note_row(&n.model, id) {
            r.fin = true;
            n.model.set_row_data(i, r);
            n.morph = Some(id);
        }
    });
    // only a message in this same go (the done step) - one shown later is its own note
    slint::Timer::single_shot(Duration::ZERO, move || {
        NOTES.with(|n| {
            let mut n = n.borrow_mut();
            if n.morph == Some(id) {
                n.morph = None;
            }
        })
    });
    // a box that became a message keeps that message's own timer
    slint::Timer::single_shot(Duration::from_millis(ms), move || {
        if NOTES.with(|n| note_row(&n.borrow().model, id).is_some_and(|(_, r)| r.prog)) {
            note_out(id);
        }
    });
}

/// a download / upload box's speed + time left: "4.2 MB/s · 12 s left" (None until there's a fair guess)
pub fn rate(id: i32, got: u64, total: u64) -> Option<String> {
    let now = Instant::now();
    let speed = NOTES.with(|n| {
        let mut n = n.borrow_mut();
        let r = n.rates.entry(id).or_insert(Rate { last: (now, got), speed: 0.0 });
        if got < r.last.1 {
            r.last = (now, got); // the next file of a batch started: keep the speed, count from here
        }
        let dt = now.duration_since(r.last.0).as_secs_f64();
        if dt >= 0.25 {
            let inst = got.saturating_sub(r.last.1) as f64 / dt;
            // smoothed, so the numbers don't jump about every tick
            r.speed = if r.speed <= 0.0 { inst } else { r.speed * 0.75 + inst * 0.25 };
            r.last = (now, got);
        }
        r.speed
    });
    if speed < 1.0 {
        return None;
    }
    let spd = if speed >= 1024.0 * 1024.0 { format!("{:.1} MB/s", speed / 1048576.0) } else { format!("{:.0} KB/s", (speed / 1024.0).max(1.0)) };
    if total <= got {
        return Some(spd);
    }
    Some(format!("{spd} · {} left", left((total - got) as f64 / speed)))
}

/// "12 s", "3 min", "1 h 5 min"
pub fn left(secs: f64) -> String {
    let s = secs.max(1.0).round() as u64;
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => {
            let (m, r) = (s / 60, s % 60);
            if m < 5 && r >= 10 {
                format!("{m} min {r} s")
            } else {
                format!("{} min", (s + 30) / 60)
            }
        }
        _ => format!("{} h {} min", s / 3600, (s % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kinds() {
        assert_eq!(guess("Couldn't move it to the Trash, so nothing was deleted (x)"), Kind::Err);
        assert_eq!(guess("Copied (4.1 MB) - paste it into Discord or anywhere"), Kind::Done);
        assert_eq!(guess("Moved 3 clips to Valorant"), Kind::Done);
        assert_eq!(guess("3 clips done · 120 MB saved"), Kind::Done);
        assert_eq!(guess("Saving recording…"), Kind::Run);
        assert_eq!(guess("Already in that folder"), Kind::Info);
        assert_eq!(guess("Pick clips that are on this PC"), Kind::Info);
        assert_eq!(guess("Renamed"), Kind::Done);
        assert_eq!(left(12.2), "12 s");
        assert_eq!(left(150.0), "2 min 30 s");
        assert_eq!(left(3725.0), "1 h 2 min");
    }
}
