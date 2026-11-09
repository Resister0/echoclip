//! William (9 Nov): "efficiency mode" when Echo sits hidden in the tray. The old Echo got it for free: WebView2 put
//! its separate UI processes into Windows' efficiency mode. Here the UI and the recorder share ONE process, so the
//! whole process must NOT go into efficiency mode (EcoQoS + idle priority would slow the capture / audio threads
//! while a game runs - dropped frames, choppy sound). Instead, 30 s after the window was hidden:
//! - the always-on fast timers (editor playhead 16 ms, strip cuts 30 ms, RGB theme + background effect 33 ms) stop -
//!   each woke the UI thread 30-60 times a second for nothing while hidden;
//! - the UI thread alone gets EcoQoS (thread power throttling) + below-normal priority.
//! Shown again: the timers restart and the UI thread is back to normal. (Thumbnails / memory: main.rs, after 20 s.)
use std::cell::{Cell, RefCell};

thread_local! {
    static SLEEPERS: RefCell<Vec<slint::Timer>> = const { RefCell::new(Vec::new()) };
    static ASLEEP: Cell<bool> = const { Cell::new(false) };
}

/// keep a repeating timer alive for the whole session; it pauses while Echo is hidden (replaces mem::forget)
pub fn keep(t: slint::Timer) {
    if ASLEEP.with(|a| a.get()) {
        t.stop();
    }
    SLEEPERS.with(|s| s.borrow_mut().push(t));
}

/// hidden for 30 s
pub fn sleep() {
    if ASLEEP.with(|a| a.replace(true)) {
        return;
    }
    SLEEPERS.with(|s| s.borrow().iter().for_each(|t| t.stop()));
    ui_thread_eco(true);
    echo_core::recorder::log_line("idle: hidden 30 s - UI timers paused, UI thread in efficiency mode");
}

/// shown again
pub fn wake() {
    if !ASLEEP.with(|a| a.replace(false)) {
        return;
    }
    SLEEPERS.with(|s| s.borrow().iter().for_each(|t| t.restart()));
    ui_thread_eco(false);
    echo_core::recorder::log_line("idle: window back - UI timers running, UI thread normal");
}

/// EcoQoS + below-normal priority for the CALLING thread (the UI thread) only
#[cfg(windows)]
fn ui_thread_eco(on: bool) {
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadInformation, SetThreadPriority, ThreadPowerThrottling, THREAD_POWER_THROTTLING_CURRENT_VERSION,
        THREAD_POWER_THROTTLING_EXECUTION_SPEED, THREAD_POWER_THROTTLING_STATE, THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_NORMAL,
    };
    let st = THREAD_POWER_THROTTLING_STATE {
        Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: if on { THREAD_POWER_THROTTLING_EXECUTION_SPEED } else { 0 },
    };
    unsafe {
        let t = GetCurrentThread();
        SetThreadInformation(t, ThreadPowerThrottling, &st as *const _ as *const _, std::mem::size_of::<THREAD_POWER_THROTTLING_STATE>() as u32);
        SetThreadPriority(t, if on { THREAD_PRIORITY_BELOW_NORMAL } else { THREAD_PRIORITY_NORMAL });
    }
}
#[cfg(not(windows))]
fn ui_thread_eco(_on: bool) {}
