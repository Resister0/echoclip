//! Smooth download bars (download-bar session, 10 Nov 2026). Cloud downloads come through Echo's rclone streaming
//! server, which hands the bytes over in 1 MiB lumps (measured: 309 of 345 bursts on Google Drive, 54 of 57 on Echo
//! Cloud) with 0.3-0.4 s stalls now and then. Below ~10 MB/s the 100 ms progress tick saw "+1 MB, 0, +1 MB, 0…", so
//! the bar moved, stopped, moved several times a second ("choppy"). Echo itself drew a steady 60 fps
//! (measured), and the Slint animation tweaks couldn't help: the input was lumpy.
//! So the bar shows this value instead of the raw bytes: it moves at the average speed of the last ~1.5 s, never goes
//! back, never gets more than one lump ahead of the bytes really received (it waits there in a long stall), and
//! catches up when it falls more than a lump behind. The caller snaps it to 100 % when the download is done.
use std::time::Instant;

/// rclone's read-ahead buffer: the bytes arrive in pieces of this size
const LUMP: f64 = 1_048_576.0;
/// the speed is averaged over about this many seconds
const TAU: f64 = 1.5;
/// a gap of more than one lump is made up in about this many seconds
const CATCH: f64 = 0.3;

pub struct Smooth {
    start: Instant,
    last: Option<(Instant, u64)>,
    shown: f64,
    speed: f64,
}

impl Smooth {
    pub fn new(start: Instant) -> Self {
        Smooth { start, last: None, shown: 0.0, speed: 0.0 }
    }

    /// bytes really received so far (+ the total) -> the bytes the bar and its text show now
    pub fn step(&mut self, real: u64, total: u64, now: Instant) -> u64 {
        let Some((t, r)) = self.last else {
            self.last = Some((now, real));
            return self.shown as u64;
        };
        let dt = now.saturating_duration_since(t).as_secs_f64().min(1.0);
        if dt <= 0.0 {
            return self.shown as u64;
        }
        let real = real.max(r); // a late (smaller) answer counts as no news
        if self.speed == 0.0 && real > 0 {
            // the first bytes: a first guess from the time so far, the average takes over from here
            self.speed = real as f64 / now.saturating_duration_since(self.start).as_secs_f64().max(0.05);
        } else {
            let inst = (real - r) as f64 / dt;
            self.speed += (inst - self.speed) * (1.0 - (-dt / TAU).exp());
        }
        let realf = real as f64;
        let v = self.speed + (realf - self.shown - LUMP).max(0.0) / CATCH;
        let cap = (realf + LUMP).min(if total > 0 { total as f64 } else { f64::MAX });
        self.shown = (self.shown + v * dt).min(cap).max(self.shown);
        self.last = Some((now, real));
        self.shown as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// ticks every 50 ms over a byte source; returns (t, real, shown) per tick
    fn run(total: u64, secs: f64, src: impl Fn(f64) -> u64) -> Vec<(f64, u64, u64)> {
        let t0 = Instant::now();
        let mut s = Smooth::new(t0);
        let mut out = vec![];
        let mut k = 0;
        loop {
            let t = k as f64 * 0.05;
            if t > secs {
                return out;
            }
            let real = src(t).min(total);
            out.push((t, real, s.step(real, total, t0 + Duration::from_secs_f64(t))));
            k += 1;
        }
    }

    /// 1 MiB lumps every 250 ms (4 MB/s): the old bar moved on 2 of 5 ticks; this one moves every tick, evenly
    #[test]
    fn lumps_move_evenly() {
        let mib = 1_048_576u64;
        let total = 200 * mib;
        let v = run(total, 20.0, |t| (t / 0.25).floor() as u64 * mib);
        let mut prev = 0;
        let mut steps = vec![];
        for &(t, real, shown) in &v {
            assert!(shown >= prev, "went back at {t}");
            assert!(shown <= real + mib, "ran ahead at {t}");
            if t > 3.0 {
                steps.push((shown - prev) as f64);
            }
            prev = shown;
        }
        let mean = steps.iter().sum::<f64>() / steps.len() as f64;
        assert!(steps.iter().all(|&d| d > mean * 0.7 && d < mean * 1.3), "uneven steps: {steps:?}");
    }

    /// a 2 s stall: waits at most one lump ahead, then catches up (within 6 MB of 400 after 1.5 s)
    #[test]
    fn stall_waits_then_catches_up() {
        let mb = 1_000_000.0;
        let total = 400_000_000;
        let src = |t: f64| (if t < 3.0 { t * 20.0 * mb } else if t < 5.0 { 60.0 * mb } else { 60.0 * mb + (t - 5.0) * 20.0 * mb }) as u64;
        let v = run(total, 10.0, src);
        let at = |x: f64| v.iter().find(|e| e.0 >= x).unwrap();
        let (_, real, shown) = *at(4.9);
        assert!(shown <= real + 1_048_576 && shown + 3_000_000 > real, "stall: real {real} shown {shown}");
        let (_, real, shown) = *at(6.5);
        assert!(real - shown.min(real) < 6_000_000, "no catch-up: real {real} shown {shown}");
    }

    /// a fast download (60 MB in 1 s) is followed at its speed (~8 MB behind), reaches the end, never passes the total
    #[test]
    fn fast_and_capped() {
        let total = 60_000_000;
        let v = run(total, 1.5, |t| ((t - 0.2).max(0.0) * 60e6) as u64);
        let (_, real, shown) = *v.iter().find(|e| e.0 >= 0.8).unwrap();
        assert!(real - shown.min(real) < 10_000_000, "lags: real {real} shown {shown}");
        assert!(v.iter().all(|e| e.2 <= total));
        assert_eq!(v.last().unwrap().2, total, "didn't reach the end");
    }
}
