//! William (9 Nov): cut out middle parts in the player's strip, without opening the editor. Shift+drag on the
//! strip highlights a part (Play.h0 / h1), X cuts it. Cuts are fractions of the clip in Play.cuts
//! ([a0, b0, a1, b1, ...], sorted, merged). Playing jumps over them, Save copy leaves them out (re-encode), and
//! E / Edit carries them + the trim + the play head into the editor (edui::carry_in). Click the x on a cut = back.
use crate::{toast, AppWindow, Ed, Play, Player};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration, time::Instant};

/// what the editor gets from the player: parts to take out (seconds of the clip) + where the play head was
pub struct Carry {
    pub removed: Vec<(f64, f64)>,
    pub pos: f64,
    pub cuts: usize,
}

pub fn reset(pl: &Play) {
    pl.set_cuts(ModelRc::new(VecModel::from(Vec::<f32>::new())));
    pl.set_cut_sum(0.0);
    pl.set_h0(-1.0);
    pl.set_h1(-1.0);
}

/// the cuts as (a, b) fractions
pub fn spans(pl: &Play) -> Vec<(f64, f64)> {
    let v: Vec<f32> = pl.get_cuts().iter().collect();
    v.chunks_exact(2).map(|c| (c[0] as f64, c[1] as f64)).collect()
}

fn set(pl: &Play, mut s: Vec<(f64, f64)>) {
    s.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut m: Vec<(f64, f64)> = vec![];
    for (a, b) in s {
        match m.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => m.push((a, b)),
        }
    }
    let flat: Vec<f32> = m.iter().flat_map(|(a, b)| [*a as f32, *b as f32]).collect();
    pl.set_cuts(ModelRc::new(VecModel::from(flat)));
    pl.set_cut_sum(m.iter().map(|(a, b)| b - a).sum::<f64>() as f32);
}

/// [s0, s1] (seconds) without the cuts (seconds): the parts that stay
pub fn kept(s0: f64, s1: f64, cuts: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = vec![];
    let mut at = s0;
    for &(a, b) in cuts {
        if b <= at || a >= s1 {
            continue;
        }
        if a > at {
            out.push((at, a));
        }
        at = at.max(b);
    }
    if s1 > at {
        out.push((at, s1));
    }
    out.into_iter().filter(|(a, b)| b - a >= 0.05).collect()
}

/// ffmpeg select / aselect expression keeping these parts (single quotes protect the commas in the filtergraph)
pub fn select_expr(kept: &[(f64, f64)]) -> String {
    kept.iter().map(|(a, b)| format!("between(t,{a:.3},{b:.3})")).collect::<Vec<_>>().join("+")
}

/// the cuts in seconds
pub fn cuts_secs(pl: &Play, d: f64) -> Vec<(f64, f64)> {
    spans(pl).into_iter().map(|(a, b)| (a * d, b * d)).collect()
}

/// E / Edit from the player: what to take into the editor (None = nothing to carry). Also ends the A-B loop of the
/// trim, so the editor's own playback isn't held inside it.
pub fn take_for_editor(a: &AppWindow, p: &Rc<RefCell<Player>>) -> Option<Carry> {
    let pl = a.global::<Play>();
    let pb = p.borrow();
    let m = pb.mpv.as_ref()?;
    let d = m.get_f64("duration").unwrap_or(pb.duration);
    if d <= 0.0 {
        return None;
    }
    let pos = m.get_f64("time-pos").unwrap_or(0.0);
    m.set("ab-loop-a", "no");
    m.set("ab-loop-b", "no");
    let mut removed = vec![];
    if pl.get_trimming() && pl.get_t0() > 0.0 {
        removed.push((0.0, pl.get_t0() as f64 * d));
    }
    let cuts = cuts_secs(&pl, d);
    let n = cuts.len();
    removed.extend(cuts);
    if pl.get_trimming() && pl.get_t1() < 1.0 {
        removed.push((pl.get_t1() as f64 * d, d));
    }
    Some(Carry { removed, pos, cuts: n })
}

pub fn wire(app: &AppWindow, player: Rc<RefCell<Player>>) {
    let pl = app.global::<Play>();
    {
        let (w, p) = (app.as_weak(), player.clone());
        pl.on_cut(move || {
            let Some(a) = w.upgrade() else { return };
            let pl = a.global::<Play>();
            let (h0, h1) = (pl.get_h0().min(pl.get_h1()) as f64, pl.get_h0().max(pl.get_h1()) as f64);
            pl.set_h0(-1.0);
            pl.set_h1(-1.0);
            if h0 < 0.0 || h1 - h0 < 0.002 {
                return;
            }
            let mut s = spans(&pl);
            s.push((h0, h1));
            set(&pl, s);
            // the Save copy bar shows (trim mode); the kept part stays the whole clip unless the ends were moved
            pl.set_trimming(true);
            let d = p.borrow().duration;
            toast(&a, format!("Cut {:.1} s - skipped while playing. Save copy leaves it out, E takes it into the editor", (h1 - h0) * d));
        });
    }
    {
        let w = app.as_weak();
        pl.on_uncut(move |k| {
            let Some(a) = w.upgrade() else { return };
            let pl = a.global::<Play>();
            let mut s = spans(&pl);
            if (k as usize) < s.len() {
                s.remove(k as usize);
                set(&pl, s);
            }
        });
    }
    // playing: jump over the cuts (checked every 30 ms; not while paused, so scrubbing into a cut still shows it)
    let last: Rc<std::cell::Cell<Option<(f64, Instant)>>> = Rc::new(std::cell::Cell::new(None));
    let t = slint::Timer::default();
    let (w, p) = (app.as_weak(), player);
    t.start(slint::TimerMode::Repeated, Duration::from_millis(30), move || {
        let Some(a) = w.upgrade() else { return };
        if !a.get_player_open() || a.global::<Ed>().get_open() {
            return;
        }
        let pl = a.global::<Play>();
        if pl.get_cuts().row_count() < 2 {
            return;
        }
        let Ok(pb) = p.try_borrow() else { return };
        let Some(m) = pb.mpv.as_ref() else { return };
        if m.get("pause").as_deref() == Some("yes") {
            return;
        }
        let d = m.get_f64("duration").unwrap_or(pb.duration);
        let Some(pos) = m.get_f64("time-pos") else { return };
        if d <= 0.0 {
            return;
        }
        for (ca, cb) in cuts_secs(&pl, d) {
            if pos >= ca && pos < cb - 0.02 {
                // don't send the same jump again while mpv is still getting there
                if let Some((tgt, at)) = last.get() {
                    if (tgt - cb).abs() < 1e-3 && at.elapsed() < Duration::from_millis(400) {
                        return;
                    }
                }
                last.set(Some((cb, Instant::now())));
                m.command(&["seek", &format!("{:.3}", cb.min(d)), "absolute+exact"]);
                return;
            }
        }
    });
    crate::idle::keep(t); // pauses while Echo is hidden in the tray
}
