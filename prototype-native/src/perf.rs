//! William (9 Nov, release): how light Echo really is, measured on real PCs - for showing Echo is the best optimized
//! clipping app. A background thread samples once a minute: memory (Task Manager's "Memory" = private working set)
//! and CPU (share of the whole PC, like Task Manager) of Echo's process PLUS its child processes (the ffmpeg encoder
//! while recording - leaving it out would flatter the numbers). The averages / peaks ride along with the update
//! check Echo already makes every few hours (setup::check_updates) as query fields - the server ignores them until
//! the admin side is built (cloud/src/index.js matches only the path). Anonymous: numbers only, nothing about the
//! PC or the person. Reset after each report.
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Agg {
    n: u32,
    mem_sum: f64,
    mem_peak: f64,
    cpu_sum: f64,
    cpu_peak: f64,
    // the same while recording (the number that matters next to a game)
    rn: u32,
    rmem_sum: f64,
    rcpu_sum: f64,
}

static AGG: Mutex<Option<Agg>> = Mutex::new(None);

/// start the sampler (once, at startup)
pub fn start() {
    std::thread::Builder::new()
        .name("perf".into())
        .spawn(|| {
            let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
            let mut last: Option<(Instant, f64)> = None;
            loop {
                std::thread::sleep(Duration::from_secs(60));
                let Some((mem, cpu_s)) = sample() else { continue };
                let now = Instant::now();
                if let Some((t0, c0)) = last {
                    let wall = now.duration_since(t0).as_secs_f64();
                    // processes that ended in between (an encoder restart) lose their time: never below 0
                    let cpu = ((cpu_s - c0).max(0.0) / (wall * cores) * 100.0).min(100.0);
                    let rec = crate::rec_core::rec().is_some_and(|r| r.is_recording());
                    let mut g = AGG.lock().unwrap();
                    let a = g.get_or_insert_with(Agg::default);
                    a.n += 1;
                    a.mem_sum += mem;
                    a.mem_peak = a.mem_peak.max(mem);
                    a.cpu_sum += cpu;
                    a.cpu_peak = a.cpu_peak.max(cpu);
                    if rec {
                        a.rn += 1;
                        a.rmem_sum += mem;
                        a.rcpu_sum += cpu;
                    }
                }
                last = Some((now, cpu_s));
            }
        })
        .ok();
}

/// "?m=..&mp=..&c=..&cp=..&rm=..&rc=..&n=..&rn=..&k=.." for the update check ("" = nothing measured yet); resets.
/// m / mp / rm = MB, c / cp / rc = CPU % x10 (one decimal), n / rn = minutes measured / of those recording, k = cores
pub fn query() -> String {
    let Some(a) = AGG.lock().ok().and_then(|mut g| g.take()) else { return String::new() };
    if a.n == 0 {
        return String::new();
    }
    let n = a.n as f64;
    let rn = a.rn.max(1) as f64;
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    format!(
        "?m={:.0}&mp={:.0}&c={:.0}&cp={:.0}&rm={:.0}&rc={:.0}&n={}&rn={}&k={cores}",
        a.mem_sum / n,
        a.mem_peak,
        a.cpu_sum / n * 10.0,
        a.cpu_peak * 10.0,
        if a.rn > 0 { a.rmem_sum / rn } else { 0.0 },
        if a.rn > 0 { a.rcpu_sum / rn * 10.0 } else { 0.0 },
        a.n,
        a.rn
    )
}

/// (private memory MB, CPU seconds used so far) of this process + its children
#[cfg(windows)]
fn sample() -> Option<(f64, f64)> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX2};
    use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe fn one(pid: u32) -> Option<(f64, f64)> {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut c: PROCESS_MEMORY_COUNTERS_EX2 = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
        let mem = if K32GetProcessMemoryInfo(h, &mut c as *mut _ as *mut _, c.cb) != 0 { c.PrivateWorkingSetSize as f64 / 1048576.0 } else { 0.0 };
        let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut cr, mut ex, mut k, mut u) = (z, z, z, z);
        let ft = |f: &FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as f64 / 1e7;
        let cpu = if GetProcessTimes(h, &mut cr, &mut ex, &mut k, &mut u) != 0 { ft(&k) + ft(&u) } else { 0.0 };
        CloseHandle(h);
        Some((mem, cpu))
    }
    unsafe {
        let me = GetCurrentProcessId();
        let (mut mem, mut cpu) = one(me)?;
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap != INVALID_HANDLE_VALUE {
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            if Process32FirstW(snap, &mut e) != 0 {
                loop {
                    if e.th32ParentProcessID == me && e.th32ProcessID != me {
                        if let Some((m, c)) = one(e.th32ProcessID) {
                            mem += m;
                            cpu += c;
                        }
                    }
                    if Process32NextW(snap, &mut e) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snap);
        }
        Some((mem, cpu))
    }
}

/// Linux: /proc - own RSS + children's (ffmpeg), CPU from utime + stime (clock ticks, 100 on every desktop distro)
#[cfg(not(windows))]
fn sample() -> Option<(f64, f64)> {
    fn one(pid: &str) -> Option<(f64, f64)> {
        let st = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = &st[st.rfind(')')? + 2..];
        let f: Vec<&str> = rest.split_whitespace().collect();
        let cpu = (f.get(11)?.parse::<f64>().ok()? + f.get(12)?.parse::<f64>().ok()?) / 100.0;
        let s = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
        let rss = s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        Some((rss / 1024.0, cpu))
    }
    let me = std::process::id().to_string();
    let (mut mem, mut cpu) = one(&me)?;
    for e in std::fs::read_dir("/proc").ok()?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name == me || !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let ppid = std::fs::read_to_string(format!("/proc/{name}/stat")).ok().and_then(|st| st.rfind(')').and_then(|i| st[i + 2..].split_whitespace().nth(1).map(|x| x.to_string())));
        if ppid.as_deref() == Some(me.as_str()) {
            if let Some((m, c)) = one(&name) {
                mem += m;
                cpu += c;
            }
        }
    }
    Some((mem, cpu))
}
