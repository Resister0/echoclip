//! Seeks while dragging on the player's seek strip or its yellow trim ends (10 Nov 2026: dragging a trim end made
//! the whole UI lag until the mouse stopped). A mouse sends 125-1000 moves a second; each one was a blocking mpv
//! seek on the UI thread. Now: at most one keyframe seek every GAP (the newest position wins, the rest are dropped),
//! sent without waiting for mpv (mpv itself also only runs the newest of the seeks it has queued). The exact seek on
//! release goes out right away.
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// ~40 seeks a second at most while dragging
const GAP: Duration = Duration::from_millis(25);

pub struct DragSeek {
    inner: Rc<Inner>,
}

struct Inner {
    last: Cell<Option<Instant>>,
    pending: Cell<Option<f64>>,
    timer: slint::Timer,
    send: Box<dyn Fn(f64)>,
}

impl Inner {
    fn flush(&self) {
        if let Some(t) = self.pending.take() {
            self.last.set(Some(Instant::now()));
            (self.send)(t);
        }
    }
}

impl DragSeek {
    /// `send(t)` = a fast seek to t seconds (called on the UI thread, never more than once per GAP)
    pub fn new(send: impl Fn(f64) + 'static) -> Self {
        DragSeek { inner: Rc::new(Inner { last: Cell::new(None), pending: Cell::new(None), timer: slint::Timer::default(), send: Box::new(send) }) }
    }

    /// the drag is at t seconds now
    pub fn drag(&self, t: f64) {
        let i = &self.inner;
        let now = Instant::now();
        match i.last.get().map(|l| now.duration_since(l)) {
            Some(e) if e < GAP => {
                i.pending.set(Some(t));
                if !i.timer.running() {
                    let w = Rc::downgrade(&self.inner);
                    i.timer.start(slint::TimerMode::SingleShot, GAP - e, move || {
                        if let Some(i) = w.upgrade() {
                            i.flush();
                        }
                    });
                }
            }
            _ => {
                i.pending.set(None);
                i.last.set(Some(now));
                (i.send)(t);
            }
        }
    }

    /// a drag sent a seek in the last 300 ms (the 250 ms player tick leaves mpv alone then)
    pub fn busy(&self) -> bool {
        self.inner.last.get().is_some_and(|l| l.elapsed() < Duration::from_millis(300))
    }

    /// the drag is over: forget a seek that hasn't gone out yet (the caller sends the exact one)
    pub fn end(&self) {
        self.inner.timer.stop();
        self.inner.pending.set(None);
        self.inner.last.set(None);
    }
}
