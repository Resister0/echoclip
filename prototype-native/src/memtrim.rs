//! 11 Nov 2026: keep Echo's memory low while it sits hidden in the tray (Windows; Linux since 26 Nov 2026, trim_linux).
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

/// Task Manager's "Memory" of Echo's own process, MB. Linux: the same thing from /proc (private pages in RAM,
/// what perf.rs reports). None where it can't be read.
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
    #[cfg(target_os = "linux")]
    {
        let r = std::fs::read_to_string("/proc/self/smaps_rollup").ok()?;
        let kb: f64 = r
            .lines()
            .filter(|l| l.starts_with("Private_Clean:") || l.starts_with("Private_Dirty:"))
            .filter_map(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
            .sum();
        return Some(kb / 1024.0);
    }
    #[allow(unreachable_code)]
    None
}

/// Linux (26 Nov 2026): the same idea as Windows' EmptyWorkingSet. Players' PCs showed Echo at ~187 MB on Linux
/// against ~67 MB on Windows; on the user's PC (tray, recording) Echo's own process held 127 MB, 111 MB of it plain
/// program memory (heap + anonymous) - driver set-up, buffers touched once - and Linux had no trim. Two steps:
/// 1. malloc_trim: glibc keeps freed heap memory for later; give the free parts back.
/// 2. MADV_PAGEOUT on Echo's own writable private memory: Linux moves those pages out to swap (zram on Fedora,
///    Nobara, Bazzite, Ubuntu ...: compressed in RAM - zero-filled set-up buffers take almost nothing). Pages in use
///    come back on first touch (a quick fault), exactly like after a Windows trim. Without any swap it changes
///    nothing (harmless). Only Echo's own process: trimming its helpers (gpu-screen-recorder, ffmpeg) would need
///    CAP_SYS_NICE (process_madvise), which an app doesn't have.
#[cfg(target_os = "linux")]
fn trim_linux() {
    extern "C" {
        fn madvise(addr: *mut std::ffi::c_void, len: usize, advice: i32) -> i32;
        fn malloc_trim(pad: usize) -> i32;
    }
    const MADV_PAGEOUT: i32 = 21;
    // SAFETY: malloc_trim only returns free heap memory to the system; madvise(MADV_PAGEOUT) is a hint that never
    // changes the contents of a mapping (pages fault back in unchanged), and a range it doesn't like just fails.
    unsafe {
        malloc_trim(0);
    }
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else { return };
    for (start, end) in pageout_ranges(&maps) {
        unsafe {
            madvise(start as *mut std::ffi::c_void, end - start, MADV_PAGEOUT);
        }
    }
}

/// From /proc/self/maps: the writable private memory that is Echo's own data - anonymous mappings and the heap.
/// Not the main thread's stack or the kernel's [vdso]/[vvar], not files (their data is small and read back from disk
/// anyway), not shared memory (other processes - the compositor, the graphics driver - use it too).
#[cfg(target_os = "linux")]
fn pageout_ranges(maps: &str) -> Vec<(usize, usize)> {
    maps.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let range = f.next()?;
            let perms = f.next()?;
            let path = f.nth(3).unwrap_or("");
            if !perms.starts_with("rw") || perms.as_bytes().get(3) != Some(&b'p') {
                return None;
            }
            if !(path.is_empty() || path == "[heap]" || path.starts_with("[anon:")) {
                return None;
            }
            let (a, b) = range.split_once('-')?;
            let (a, b) = (usize::from_str_radix(a, 16).ok()?, usize::from_str_radix(b, 16).ok()?);
            (b > a).then_some((a, b))
        })
        .collect()
}

/// Give the system back the pages Echo isn't using (they return on first use). Returns " (216 -> 7 MB)" for the log
/// ("" where it can't measure).
pub fn trim() -> String {
    let before = private_mb();
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet(windows_sys::Win32::System::Threading::GetCurrentProcess());
    }
    #[cfg(target_os = "linux")]
    trim_linux();
    LAST_TRIM.with(|t| t.set(Some(Instant::now())));
    SETTLED_MB.with(|s| s.set(None));
    match (before, private_mb()) {
        (Some(b), Some(a)) => format!(" ({b:.0} -> {a:.0} MB)"),
        _ => String::new(),
    }
}

/// Window shown again (and in front): start over at the next hide.
pub fn shown() {
    LAST_TRIM.with(|t| t.set(None));
    SETTLED_MB.with(|s| s.set(None));
    BEHIND_SINCE.with(|b| b.set(None));
}

/// 11 Nov 2026 (2.0.5, window open on Home behind a game): Task Manager showed 150-300 MB. Every player start
/// (libmpv + its Direct3D 11 device + the AMD driver) touches ~100 MB of set-up memory; mpv is shut down after a
/// minute unused and frees it, but the pages stay counted to Echo (a trim on hide: 118 -> 3 MB). Window open but
/// NOT in front for BEHIND: trim once, then like hidden (trim again when it grows). Nothing is drawn while Echo is
/// behind, so the soft faults land on the next look at Echo (a few ms).
const BEHIND: Duration = Duration::from_secs(60);
thread_local! {
    static BEHIND_SINCE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Call on every UI tick while the window is open but another window is in front (and no clip is playing).
/// Some(log line) when it trimmed.
pub fn behind_tick() -> Option<String> {
    let since = BEHIND_SINCE.with(|b| {
        let s = b.get().unwrap_or_else(Instant::now);
        b.set(Some(s));
        s
    });
    if since.elapsed() < BEHIND {
        return None;
    }
    if LAST_TRIM.with(|t| t.get()).is_none() {
        let t = trim();
        return Some(format!("memory: Echo behind another window 60 s - trimmed{t}"));
    }
    hidden_tick().map(|m| m.replace("while hidden", "while behind"))
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn picks_only_own_writable_private_memory() {
        let maps = "\
55d0a000-55d0b000 r--p 00000000 103:02 123 /usr/bin/echo-clips
55d0b000-55d0c000 rw-p 00001000 103:02 123 /usr/bin/echo-clips
55d0c000-55d2c000 rw-p 00000000 00:00 0 [heap]
7f0000000000-7f0000100000 rw-p 00000000 00:00 0
7f0000100000-7f0000101000 ---p 00000000 00:00 0
7f0000200000-7f0000300000 rw-s 00000000 00:05 9 /memfd:wayland-shm (deleted)
7f0000300000-7f0000301000 rw-p 00000000 00:00 0 [anon:glibc]
7ffd00000000-7ffd00021000 rw-p 00000000 00:00 0 [stack]
7ffd00030000-7ffd00032000 r-xp 00000000 00:00 0 [vdso]";
        assert_eq!(
            pageout_ranges(maps),
            vec![(0x55d0c000, 0x55d2c000), (0x7f0000000000, 0x7f0000100000), (0x7f0000300000, 0x7f0000301000)]
        );
    }

    /// Fills 64 MB, trims, and checks Echo's own number drops - only where the PC has swap (zram on Fedora / Nobara /
    /// Bazzite / Ubuntu); without swap Linux can't move the pages out and the trim rightly changes nothing.
    #[test]
    fn trim_gives_back_untouched_memory() {
        let swap = std::fs::read_to_string("/proc/swaps").map(|s| s.lines().count() > 1).unwrap_or(false);
        let mut big = vec![0u8; 64 << 20];
        for (i, b) in big.iter_mut().enumerate().step_by(512) {
            *b = (i % 251) as u8 + 1;
        }
        let before = private_mb().expect("smaps_rollup");
        trim_linux();
        let after = private_mb().expect("smaps_rollup");
        // the pages are still there and unchanged (they come back on touch)
        assert!(big.iter().enumerate().step_by(512).all(|(i, b)| *b == (i % 251) as u8 + 1));
        eprintln!("trim: {before:.0} -> {after:.0} MB (swap: {swap})");
        if swap {
            assert!(after < before - 40.0, "expected most of the 64 MB to leave RAM: {before:.0} -> {after:.0} MB");
        }
    }
}
