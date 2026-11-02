//! Save a clip with a game controller: polls XInput (Xbox and most PC pads) for a button combo.
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::time::{Duration, Instant};

/// Wanted combo (XInput button bits; LT = 0x0400, RT = 0x0800). 0 = off.
static COMBO: AtomicU16 = AtomicU16::new(0);
/// Settings "Set" button: record the next combo the user presses.
static CAPTURE: AtomicBool = AtomicBool::new(false);
static CAPTURED: AtomicU16 = AtomicU16::new(0);

pub fn set_combo(bits: u16) {
    COMBO.store(bits, Ordering::Relaxed);
}

/// Button names for the UI, e.g. "Back + RB".
pub fn names(bits: u16) -> String {
    const N: [(u16, &str); 16] = [
        (0x0020, "Back"), (0x0010, "Start"), (0x0100, "LB"), (0x0200, "RB"), (0x0400, "LT"), (0x0800, "RT"),
        (0x1000, "A"), (0x2000, "B"), (0x4000, "X"), (0x8000, "Y"), (0x0040, "L-stick"), (0x0080, "R-stick"),
        (0x0001, "Up"), (0x0002, "Down"), (0x0004, "Left"), (0x0008, "Right"),
    ];
    N.iter().filter(|(b, _)| bits & b != 0).map(|(_, n)| *n).collect::<Vec<_>>().join(" + ")
}

/// Waits (max ~10 s) until the user presses and releases buttons; returns the combo held (0 = nothing).
pub fn capture() -> u16 {
    CAPTURED.store(0, Ordering::Relaxed);
    CAPTURE.store(true, Ordering::Relaxed);
    let t = Instant::now();
    while CAPTURE.load(Ordering::Relaxed) && t.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    CAPTURE.store(false, Ordering::Relaxed);
    CAPTURED.load(Ordering::Relaxed)
}

#[cfg(windows)]
fn read(slot: u32) -> Option<u16> {
    use windows::Win32::UI::Input::XboxController::{XInputGetState, XINPUT_STATE};
    let mut st = XINPUT_STATE::default();
    if unsafe { XInputGetState(slot, &mut st) } != 0 {
        return None;
    }
    let g = st.Gamepad;
    let mut b = g.wButtons.0 & !0x0C00;
    if g.bLeftTrigger > 128 {
        b |= 0x0400;
    }
    if g.bRightTrigger > 128 {
        b |= 0x0800;
    }
    Some(b)
}
#[cfg(not(windows))]
fn read(_: u32) -> Option<u16> {
    None
}

/// Background thread: calls `on_combo` when the saved combo is pressed (once per press).
pub fn start(on_combo: Box<dyn Fn() + Send>) {
    std::thread::spawn(move || {
        let mut connected = [false; 4];
        let mut last_scan = Instant::now() - Duration::from_secs(10);
        let (mut was, mut best) = (false, 0u16);
        let mut last_fire = Instant::now() - Duration::from_secs(10);
        loop {
            let capturing = CAPTURE.load(Ordering::Relaxed);
            let combo = COMBO.load(Ordering::Relaxed);
            if combo == 0 && !capturing {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
            // empty slots are slow to query: only look for newly plugged-in pads every 2 s
            let rescan = last_scan.elapsed() > Duration::from_secs(2);
            if rescan {
                last_scan = Instant::now();
            }
            let mut held = 0u16;
            for i in 0..4u32 {
                if connected[i as usize] || rescan {
                    match read(i) {
                        Some(b) => {
                            connected[i as usize] = true;
                            held |= b;
                        }
                        None => connected[i as usize] = false,
                    }
                }
            }
            if capturing {
                if held != 0 {
                    best |= held;
                } else if best != 0 {
                    CAPTURED.store(best, Ordering::Relaxed);
                    CAPTURE.store(false, Ordering::Relaxed);
                    best = 0;
                }
            } else {
                best = 0;
                let on = combo != 0 && held & combo == combo;
                if on && !was && last_fire.elapsed() > Duration::from_secs(1) {
                    last_fire = Instant::now();
                    on_combo();
                }
                was = on;
            }
            let idle = !connected.iter().any(|c| *c);
            std::thread::sleep(Duration::from_millis(if idle { 500 } else { 25 }));
        }
    });
}
