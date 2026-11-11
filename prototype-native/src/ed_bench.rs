//! Test hook ECHO_TL_BENCH=<pps>,<pps>,... (11 Nov 2026): measures scrolling the editor's timeline. Once the editor is
//! open (e.g. with ECHO_EDIT), it sets each zoom (pixels per second) in turn and does 150 scroll steps the way the
//! mouse-wheel glide does (one real scroll event over the timeline per step, 32 px of wheel delta = 16 px of timeline;
//! main.rs glide_step), turning at the ends. Each step: the event is handled (Slint's handler + Echo's Rust, timed),
//! then the whole window is drawn with take_snapshot (timed - it doesn't need a screen, so it works with the display
//! off; it includes reading the picture back, the same for every run). Logged per zoom ("tl-bench:" in the proto log).
//! Zoom suffixes: "150s" = draw without scrolling, "150n" = scroll with the filmstrip hidden. ECHO_TL_SNAP=<x.png> saves
//! the last frame of each zoom as x-<n>.png.
use crate::AppWindow;
use slint::ComponentHandle;
use std::cell::RefCell;
use std::time::{Duration, Instant};

const STEPS: usize = 150;

#[derive(Default)]
struct Bench {
    /// (pixels per second, mode): ' ' = scroll, 's' = draw without scrolling, 'n' = scroll with the filmstrip hidden
    zooms: Vec<(f64, char)>,
    phase: usize,
    step: usize,
    ev_ms: Vec<f64>,
    draw_ms: Vec<f64>,
    dir: f32,
    /// where the wheel goes (window y, logical px): found once by trying spots until the timeline moves
    y: f32,
    /// timeline px scrolled in this zoom's run
    moved: f64,
}

thread_local! {
    static BENCH: RefCell<Option<Bench>> = const { RefCell::new(None) };
}

fn log(line: String) {
    crate::log_to(&mut Vec::new(), line);
}

/// called when the editor has opened: starts the run 1.5 s later if ECHO_TL_BENCH is set
pub fn arm(a: &AppWindow) {
    let Ok(v) = std::env::var("ECHO_TL_BENCH") else { return };
    let zooms: Vec<(f64, char)> = v
        .split(',')
        .filter_map(|x| {
            let x = x.trim();
            let (n, m) = match x.chars().last() {
                Some(c) if c.is_ascii_alphabetic() => (&x[..x.len() - 1], c),
                _ => (x, ' '),
            };
            n.parse().ok().map(|p| (p, m))
        })
        .collect();
    if zooms.is_empty() || BENCH.with(|b| b.borrow().is_some()) {
        return;
    }
    let w = a.as_weak();
    slint::Timer::single_shot(Duration::from_millis(1500), move || {
        let Some(a) = w.upgrade() else { return };
        let Some(y) = find_spot(&a) else {
            log("tl-bench: no spot where the wheel scrolls the timeline".into());
            return;
        };
        log(format!("tl-bench: wheel at y {y:.0}"));
        BENCH.with(|b| *b.borrow_mut() = Some(Bench { zooms, dir: -1.0, y, ..Default::default() }));
        start_phase(&a);
    });
}

fn start_phase(a: &AppWindow) {
    let pps = BENCH.with(|b| b.borrow().as_ref().and_then(|b| b.zooms.get(b.phase).copied()));
    let Some((pps, mode)) = pps else {
        a.global::<crate::Play>().set_sheet_ok(true);
        BENCH.with(|b| *b.borrow_mut() = None);
        log("tl-bench: done".into());
        return;
    };
    crate::edui::bench_zoom(a, pps);
    a.global::<crate::Play>().set_sheet_ok(mode != 'n');
    BENCH.with(|b| {
        if let Some(b) = b.borrow_mut().as_mut() {
            b.step = 0;
            b.ev_ms.clear();
            b.draw_ms.clear();
            b.moved = 0.0;
        }
    });
    next(a.as_weak());
}

/// one step, then the next one from the event loop (so timers / other events still run in between)
fn next(w: slint::Weak<AppWindow>) {
    slint::Timer::single_shot(Duration::from_millis(1), move || {
        let Some(a) = w.upgrade() else { return };
        let done = BENCH.with(|b| b.borrow().as_ref().map(|b| b.step >= STEPS)).unwrap_or(true);
        if done {
            report();
            BENCH.with(|b| {
                if let Some(b) = b.borrow_mut().as_mut() {
                    b.phase += 1;
                }
            });
            start_phase(&a);
            return;
        }
        // turn at the ends
        let (at, max) = crate::edui::bench_scroll();
        let dir = BENCH.with(|b| {
            let mut g = b.borrow_mut();
            let b = g.as_mut()?;
            if at >= max - 1.0 {
                b.dir = 1.0;
            } else if at <= 0.5 {
                b.dir = -1.0;
            }
            Some(if b.zooms.get(b.phase).map(|z| z.1) == Some('s') { 0.0 } else { b.dir })
        });
        let Some(dir) = dir else { return };
        let win = a.window();
        let sz = win.size().to_logical(win.scale_factor());
        let y = BENCH.with(|b| b.borrow().as_ref().map(|b| b.y)).unwrap_or(sz.height - 60.0);
        let pos = slint::LogicalPosition::new(sz.width / 2.0, y);
        win.dispatch_event(slint::platform::WindowEvent::PointerMoved { position: pos });
        let t = Instant::now();
        win.dispatch_event(slint::platform::WindowEvent::PointerScrolled { position: pos, delta_x: 0.0, delta_y: dir * 32.0 });
        let ev = t.elapsed().as_secs_f64() * 1000.0;
        let t = Instant::now();
        let moved = (crate::edui::bench_scroll().0 - at).abs();
        let snap = win.take_snapshot();
        let ok = snap.is_ok();
        // ECHO_TL_SNAP=<png>: keep the last frame of each zoom (to look at the timeline afterwards)
        if let (Ok(buf), Ok(path)) = (&snap, std::env::var("ECHO_TL_SNAP")) {
            if BENCH.with(|b| b.borrow().as_ref().is_some_and(|b| b.step + 1 == STEPS)) {
                let phase = BENCH.with(|b| b.borrow().as_ref().map(|b| b.phase).unwrap_or(0));
                let out = path.replace(".png", &format!("-{phase}.png"));
                let _ = image::RgbaImage::from_raw(buf.width(), buf.height(), buf.as_bytes().to_vec()).map(|im| im.save(out));
            }
        }
        let draw = t.elapsed().as_secs_f64() * 1000.0;
        BENCH.with(|b| {
            if let Some(b) = b.borrow_mut().as_mut() {
                b.ev_ms.push(ev);
                if ok {
                    b.draw_ms.push(draw);
                }
                b.moved += moved;
                b.step += 1;
            }
        });
        next(w);
    });
}

fn stats(v: &[f64]) -> String {
    if v.is_empty() {
        return "-".into();
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let avg = s.iter().sum::<f64>() / s.len() as f64;
    let p95 = s[((s.len() as f64 * 0.95) as usize).min(s.len() - 1)];
    format!("avg {avg:.2} / p95 {p95:.2} / max {:.2} ms", s[s.len() - 1])
}

fn report() {
    BENCH.with(|b| {
        if let Some(b) = b.borrow().as_ref() {
            let (pps, mode) = b.zooms.get(b.phase).copied().unwrap_or((0.0, ' '));
            let what = match mode { 's' => " (no scrolling)", 'n' => " (filmstrip hidden)", _ => "" };
            log(format!("tl-bench: zoom {pps} px/s{what}, {} steps, {:.0} px scrolled | scroll event {} | draw (take_snapshot) {}", b.ev_ms.len(), b.moved, stats(&b.ev_ms), stats(&b.draw_ms)));
        }
    });
}

/// a window y where a wheel step really scrolls the timeline (tries from the bottom up; puts the scroll back)
fn find_spot(a: &AppWindow) -> Option<f32> {
    crate::edui::bench_zoom(a, 150.0);
    let win = a.window();
    let sz = win.size().to_logical(win.scale_factor());
    let mut y = sz.height - 20.0;
    while y > sz.height * 0.4 {
        let before = crate::edui::bench_scroll().0;
        win.dispatch_event(slint::platform::WindowEvent::PointerMoved { position: slint::LogicalPosition::new(sz.width / 2.0, y) });
        win.dispatch_event(slint::platform::WindowEvent::PointerScrolled { position: slint::LogicalPosition::new(sz.width / 2.0, y), delta_x: 0.0, delta_y: -64.0 });
        if (crate::edui::bench_scroll().0 - before).abs() > 0.5 {
            crate::edui::bench_zoom(a, 150.0);
            return Some(y);
        }
        y -= 15.0;
    }
    None
}
