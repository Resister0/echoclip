//! 11 Nov 2026: keep Echo's memory low while it sits hidden in the tray (Windows).
//! Task Manager's "Memory" is the private working set: pages Echo touched that Windows still keeps for it. On an
//! AMD PC (2.0.4, recording 1080p60) it read 216 MB, but most of it was touched only ONCE - graphics-driver set-up for
//! the capture, the encoder and the two OpenGL windows (main + popup), much of it just zero-filled buffers - and
//! never used again: one trim brought that recording Echo to ~13 MB, and it stayed there.
//! main.rs trims once, 20 s after the window was hidden. Things that happen LATER while hidden touch that memory
//! again and nothing gave it back, so Echo then sat at ~200 MB for hours: the recorder starting over when the game
//! window changes (driver set-up, ~30 MB each time), a clip-saved popup, a save - and, measured on that PC, another
//! program reading ALL of Echo's memory every 3-5 minutes while a game ran (most likely the game's anti-cheat looking
//! at the program that records it; a test process with the same capture + encoder was never read). Every page it
//! reads counts as Echo's again. So while hidden: every CHECK, if Echo grew by GROW_MB since it settled after the
//! last trim, trim again (at most every MIN_GAP). A trim only costs the pages really in use a quick soft fault -
//! recording keeps going (measured: no gap in the replay buffer).
use std::cell::Cell;
use std::time::{Duration, Instant};

const GROW_MB: f64 = 32.0;
const MIN_GAP: Duration = Duration::from_secs(90);
/// after a trim, the size it settles at (pages really in use come back within seconds)
const SETTLE: Duration = Duration::from_secs(30);
const CHECK: Duration = Duration::from_secs(30);

thread_local! {
    static LAST_TRIM: Cell<Option<Instant>> = const { Cell::new(None) };
    static SETTLED_MB: Cell<Option<f64>> = const { Cell::new(None) };
    static LAST_CHECK: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Task Manager's "Memory" of Echo's own process, MB (None off Windows)
fn private_mb() -> Option<f64> {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX2};
        let mut c: PROCESS_MEMORY_COUNTERS_EX2 = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
        if K32GetProcessMemoryInfo(windows_sys::Win32::System::Threading::GetCurrentProcess(), &mut c as *mut _ as *mut _, c.cb) != 0 {
            return Some(c.PrivateWorkingSetSize as f64 / 1048576.0);
        }
    }
    None
}

/// Give Windows back the pages Echo isn't using (they return on first use). Returns " (216 -> 7 MB)" for the log
/// ("" where it can't measure).
pub fn trim() -> String {
    let before = private_mb();
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet(windows_sys::Win32::System::Threading::GetCurrentProcess());
    }
    LAST_TRIM.with(|t| t.set(Some(Instant::now())));
    SETTLED_MB.with(|s| s.set(None));
    match (before, private_mb()) {
        (Some(b), Some(a)) => format!(" ({b:.0} -> {a:.0} MB)"),
        _ => String::new(),
    }
}

/// Window shown again: start over at the next hide.
pub fn shown() {
    LAST_TRIM.with(|t| t.set(None));
    SETTLED_MB.with(|s| s.set(None));
}

/// Call on every UI tick while hidden, after the first trim. Some(log line) when it trimmed again.
pub fn hidden_tick() -> Option<String> {
    let last = LAST_TRIM.with(|t| t.get())?;
    let now = Instant::now();
    if LAST_CHECK.with(|c| c.get()).is_some_and(|c| now - c < CHECK) {
        return None;
    }
    LAST_CHECK.with(|c| c.set(Some(now)));
    let mb = private_mb()?;
    let Some(settled) = SETTLED_MB.with(|s| s.get()) else {
        if now - last >= SETTLE {
            SETTLED_MB.with(|s| s.set(Some(mb)));
        }
        return None;
    };
    if mb < settled + GROW_MB || now - last < MIN_GAP {
        return None;
    }
    let t = trim();
    Some(format!("memory: grew from {settled:.0} MB while hidden - trimmed again{t}"))
}
