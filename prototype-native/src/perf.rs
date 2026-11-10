//! 9 Nov, release: how light Echo really is, measured on real PCs - for showing Echo is the best optimized
//! clipping app. A background thread samples once a minute: memory (Task Manager's "Memory" = private working set)
//! and CPU (share of the whole PC, like Task Manager) of Echo's process PLUS its child processes (the ffmpeg encoder
//! while recording - leaving it out would flatter the numbers). The averages / peaks ride along with the update
//! check Echo already makes every few hours (setup::check_updates) as query fields (cloud/src/index.js countPerf,
//! admin page "Performance"), with the PC's hardware model names + recording settings (specs.rs). Anonymous: no ID,
//! nothing about the person. Reset after each report. Off with Settings > About > "Send anonymous performance numbers".
//!
//! perf-states (10 Nov 2026): each minute counts under ONE state, so the tray, an open window and real use don't
//! mix (first report: 52 MB average, but the window-open first hour was 199 MB). The UI's 250 ms timer (main.rs,
//! runs while hidden too) tells `ui_tick` what it sees; the state seen most in that minute wins:
//! - tray   = window hidden (tray) or minimized
//! - idle   = window shown, no mouse / keyboard input in Echo's window for 60 s+ (e.g. left open behind a game)
//! - active = window shown with input in the last 60 s (home, library, settings...)
//! - player = window shown and a clip open in the player or the editor (input or not: watching is use)
//! Recording on / off is kept apart inside each state, like before.
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Default, Clone, Copy, PartialEq, Debug)]
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

impl Agg {
    fn add(&mut self, mem: f64, cpu: f64, rec: bool) {
        self.n += 1;
        self.mem_sum += mem;
        self.mem_peak = self.mem_peak.max(mem);
        self.cpu_sum += cpu;
        self.cpu_peak = self.cpu_peak.max(cpu);
        if rec {
            self.rn += 1;
            self.rmem_sum += mem;
            self.rcpu_sum += cpu;
        }
    }
    fn merge(&mut self, o: &Agg) {
        self.n += o.n;
        self.mem_sum += o.mem_sum;
        self.mem_peak = self.mem_peak.max(o.mem_peak);
        self.cpu_sum += o.cpu_sum;
        self.cpu_peak = self.cpu_peak.max(o.cpu_peak);
        self.rn += o.rn;
        self.rmem_sum += o.rmem_sum;
        self.rcpu_sum += o.rcpu_sum;
    }
    /// [avg MB, peak MB, avg CPU x10, peak CPU x10, rec avg MB, rec avg CPU x10] (recording ones 0 without recording)
    fn avgs(&self) -> [f64; 6] {
        let n = self.n.max(1) as f64;
        let rn = self.rn.max(1) as f64;
        let rec = self.rn > 0;
        [
            self.mem_sum / n,
            self.mem_peak,
            self.cpu_sum / n * 10.0,
            self.cpu_peak * 10.0,
            if rec { self.rmem_sum / rn } else { 0.0 },
            if rec { self.rcpu_sum / rn * 10.0 } else { 0.0 },
        ]
    }
}

/// what Echo was doing in a minute (index into the per-state numbers)
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum State {
    Tray = 0,
    Idle = 1,
    Active = 2,
    Player = 3,
}
const STATES: [State; 4] = [State::Tray, State::Idle, State::Active, State::Player];
/// URL field names (s_<name>=n,rn,m,mp,c,cp,rm,rc) - the server keeps these names as the state
const NAMES: [&str; 4] = ["tray", "idle", "active", "player"];
/// no input in Echo's window for this long = idle
const IDLE_AFTER: Duration = Duration::from_secs(60);

pub fn classify(visible: bool, player_open: bool, since_input: Duration) -> State {
    if !visible {
        State::Tray
    } else if player_open {
        State::Player
    } else if since_input >= IDLE_AFTER {
        State::Idle
    } else {
        State::Active
    }
}

/// UI ticks seen per state since the last sample
static TICKS: [AtomicU32; 4] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];

/// main.rs 250 ms timer: visible = shown and not minimized; player_open = a clip in the player / editor;
/// since_input = since the last mouse move / click / key in Echo's window
pub fn ui_tick(visible: bool, player_open: bool, since_input: Duration) {
    TICKS[classify(visible, player_open, since_input) as usize].fetch_add(1, Ordering::Relaxed);
}

/// the state seen most (a tie goes to the busier one); no ticks at all (UI thread stuck) = tray
fn dominant(t: [u32; 4]) -> State {
    let mut best = State::Tray;
    let mut most = 0;
    for s in STATES {
        if t[s as usize] > 0 && t[s as usize] >= most {
            most = t[s as usize];
            best = s;
        }
    }
    best
}

fn take_ticks() -> [u32; 4] {
    [0, 1, 2, 3].map(|i| TICKS[i].swap(0, Ordering::Relaxed))
}

static AGG: Mutex<Option<[Agg; 4]>> = Mutex::new(None);

fn record(state: State, mem: f64, cpu: f64, rec: bool) {
    let mut g = AGG.lock().unwrap();
    g.get_or_insert_with(Default::default)[state as usize].add(mem, cpu, rec);
}

/// start the sampler (once, at startup)
pub fn start() {
    std::thread::Builder::new()
        .name("perf".into())
        .spawn(|| {
            let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
            let mut last: Option<(Instant, f64)> = None;
            loop {
                std::thread::sleep(Duration::from_secs(60));
                let Some((mem, cpu_s)) = sample() else {
                    take_ticks();
                    continue;
                };
                let now = Instant::now();
                if let Some((t0, c0)) = last {
                    let wall = now.duration_since(t0).as_secs_f64();
                    // processes that ended in between (an encoder restart) lose their time: never below 0
                    let cpu = ((cpu_s - c0).max(0.0) / (wall * cores) * 100.0).min(100.0);
                    let rec = crate::rec_core::rec().is_some_and(|r| r.is_recording());
                    record(dominant(take_ticks()), mem, cpu, rec);
                } else {
                    take_ticks(); // the first minute isn't measured: its ticks don't count either
                }
                last = Some((now, cpu_s));
            }
        })
        .ok();
}

/// r2-perf: Settings > App Settings > About > "Send anonymous performance numbers" (on by default). Kept in
/// native_ui.json: settings.json is rewritten by the recorder's own Settings struct, which drops keys it doesn't know.
static OFF: AtomicBool = AtomicBool::new(false);
static OFF_READ: std::sync::Once = std::sync::Once::new();

pub fn enabled() -> bool {
    OFF_READ.call_once(|| OFF.store(crate::lib_fx::ui_get("send_perf").as_bool() == Some(false), Ordering::Relaxed));
    !OFF.load(Ordering::Relaxed)
}

/// S.set hook (main.rs): true = it was ours
pub fn set(key: &str, val: &str) -> bool {
    if key != "send_perf" {
        return false;
    }
    let on = val == "true";
    enabled(); // read the file first, so a later first read can't undo this
    OFF.store(!on, Ordering::Relaxed);
    crate::lib_fx::ui_set("send_perf", if on { serde_json::Value::Null } else { serde_json::json!(false) });
    true
}

/// "?m=..&mp=..&c=..&cp=..&rm=..&rc=..&n=..&rn=..&k=..&sv=1&s_tray=..&s_active=..&cpu=..&gpu=..&ram=..&os=..&res=..&fps=..&enc=.."
/// for the update check ("" = nothing measured yet, or the switch is off); resets.
/// m / mp / rm = MB, c / cp / rc = CPU % x10 (one decimal), n / rn = minutes measured / of those recording, k = cores
/// - these first ones are all states together (what servers before perf-states read).
/// sv=1 = per-state fields follow: s_<tray|idle|active|player>=n,rn,m,mp,c,cp,rm,rc (same units; only states that
/// had minutes). The rest = this PC's hardware model names + recording settings (specs.rs)
pub fn query() -> String {
    // taken either way: numbers from while the switch was off are never sent later
    let Some(st) = AGG.lock().ok().and_then(|mut g| g.take()) else { return String::new() };
    if !enabled() {
        return String::new();
    }
    let q = fields(&st);
    if q.is_empty() {
        return q;
    }
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    format!("{q}&k={cores}&sv=1{}", per_state(&st)) + &crate::specs::q()
}

/// the all-states fields "?m=..&n=..&rn=.." ("" = no minutes)
fn fields(st: &[Agg; 4]) -> String {
    let mut a = Agg::default();
    st.iter().for_each(|s| a.merge(s));
    if a.n == 0 {
        return String::new();
    }
    let [m, mp, c, cp, rm, rc] = a.avgs();
    format!("?m={m:.0}&mp={mp:.0}&c={c:.0}&cp={cp:.0}&rm={rm:.0}&rc={rc:.0}&n={}&rn={}", a.n, a.rn)
}

/// "&s_tray=n,rn,m,mp,c,cp,rm,rc..." for each state with minutes
fn per_state(st: &[Agg; 4]) -> String {
    let mut o = String::new();
    for s in STATES {
        let a = &st[s as usize];
        if a.n == 0 {
            continue;
        }
        let [m, mp, c, cp, rm, rc] = a.avgs();
        o += &format!("&s_{}={},{},{m:.0},{mp:.0},{c:.0},{cp:.0},{rm:.0},{rc:.0}", NAMES[s as usize], a.n, a.rn);
    }
    o
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

/// Linux: /proc - own PRIVATE memory + children's (recorder / ffmpeg), CPU from utime + stime (clock ticks, 100 on every
/// desktop distro). Private = Private_Clean + Private_Dirty from smaps_rollup = what Windows reports (Task Manager's private
/// working set). Until 2.0.3 Linux sent VmRSS, which also counts shared libraries (graphics driver, fonts, libmpv) in full
/// and read 2-5x higher than Windows (10 Nov 2026). VmRSS only as a fallback when smaps_rollup can't be read.
#[cfg(not(windows))]
fn private_kb(rollup: &str) -> Option<f64> {
    let mut sum = None;
    for l in rollup.lines() {
        if l.starts_with("Private_Clean:") || l.starts_with("Private_Dirty:") {
            let v = l.split_whitespace().nth(1)?.parse::<f64>().ok()?;
            sum = Some(sum.unwrap_or(0.0) + v);
        }
    }
    sum
}

#[cfg(not(windows))]
fn sample() -> Option<(f64, f64)> {
    fn one(pid: &str) -> Option<(f64, f64)> {
        let st = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = &st[st.rfind(')')? + 2..];
        let f: Vec<&str> = rest.split_whitespace().collect();
        let cpu = (f.get(11)?.parse::<f64>().ok()? + f.get(12)?.parse::<f64>().ok()?) / 100.0;
        let kb = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok().and_then(|r| private_kb(&r)).unwrap_or_else(|| {
            let s = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
            s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0)
        });
        Some((kb / 1024.0, cpu))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn agg(n: u32, mem_sum: f64, mem_peak: f64, cpu_sum: f64, cpu_peak: f64, rn: u32, rmem_sum: f64, rcpu_sum: f64) -> Agg {
        Agg { n, mem_sum, mem_peak, cpu_sum, cpu_peak, rn, rmem_sum, rcpu_sum }
    }
    #[cfg(not(windows))]
    #[test]
    fn private_memory_from_smaps_rollup() {
        let r = "Rss:              250000 kB\nPss:              120000 kB\nShared_Clean:     150000 kB\nPrivate_Clean:     20000 kB\nPrivate_Dirty:     60000 kB\n";
        assert_eq!(private_kb(r), Some(80000.0));
        assert_eq!(private_kb("Rss: 1 kB\n"), None);
        // this test process can read its own rollup and gets a sane number
        let me = std::fs::read_to_string("/proc/self/smaps_rollup").ok().and_then(|r| private_kb(&r));
        assert!(me.is_some_and(|kb| kb > 100.0));
    }

    fn put() {
        let mut st = [Agg::default(); 4];
        st[State::Tray as usize] = agg(2, 100.0, 60.0, 1.0, 0.6, 0, 0.0, 0.0);
        st[State::Active as usize] = agg(1, 500.0, 250.0, 2.0, 2.5, 1, 240.0, 1.5);
        *AGG.lock().unwrap() = Some(st);
    }

    /// numbers + specs in one string, reset after sending, nothing at all while the switch is off
    #[test]
    fn query_and_switch() {
        let dir = std::env::temp_dir().join("echo_perf_switch_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("ECHO_SETTINGS", dir.join("settings.json"));
        assert!(enabled());
        put();
        let q = query();
        println!("query: {q}");
        // all states together first (old servers), then per state; peaks are the highest of all states
        assert!(q.starts_with("?m=200&mp=250&c=10&cp=25&rm=240&rc=15&n=3&rn=1&k="), "{q}");
        assert!(q.contains("&sv=1&s_tray=2,0,50,60,5,6,0,0&s_active=1,1,500,250,20,25,240,15&cpu="), "{q}");
        assert!(!q.contains("s_idle") && !q.contains("s_player") && q.contains("&os="));
        assert_eq!(query(), "");
        assert!(set("send_perf", "false"));
        assert!(std::fs::read_to_string(dir.join("native_ui.json")).unwrap().contains("\"send_perf\": false"));
        put();
        assert_eq!(query(), "");
        set("send_perf", "true");
        put();
        assert!(query().starts_with("?m="));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn classify_states() {
        let s = Duration::from_secs;
        // hidden / minimized = tray, whatever else is going on
        assert_eq!(classify(false, false, s(0)), State::Tray);
        assert_eq!(classify(false, true, s(0)), State::Tray);
        assert_eq!(classify(false, false, s(600)), State::Tray);
        // shown: a clip open = player, even with no input (watching)
        assert_eq!(classify(true, true, s(0)), State::Player);
        assert_eq!(classify(true, true, s(600)), State::Player);
        // shown, no clip: input in the last 60 s = active, else idle
        assert_eq!(classify(true, false, s(0)), State::Active);
        assert_eq!(classify(true, false, s(59)), State::Active);
        assert_eq!(classify(true, false, s(60)), State::Idle);
        assert_eq!(classify(true, false, s(3600)), State::Idle);
    }

    #[test]
    fn dominant_state() {
        assert_eq!(dominant([0, 0, 0, 0]), State::Tray); // UI thread stuck
        assert_eq!(dominant([240, 0, 0, 0]), State::Tray);
        assert_eq!(dominant([200, 0, 40, 0]), State::Tray); // opened for 10 s of the minute
        assert_eq!(dominant([100, 0, 140, 0]), State::Active);
        assert_eq!(dominant([0, 120, 0, 120]), State::Player); // a tie goes to the busier state
        assert_eq!(dominant([0, 0, 1, 0]), State::Active);
    }

    /// each sample lands in its own state; recording minutes stay apart inside it
    #[test]
    fn aggregate_per_state() {
        let mut st = [Agg::default(); 4];
        st[State::Idle as usize].add(100.0, 1.0, false);
        st[State::Idle as usize].add(300.0, 3.0, true);
        st[State::Player as usize].add(200.0, 5.0, false);
        let i = st[State::Idle as usize];
        assert_eq!((i.n, i.rn, i.mem_peak, i.cpu_peak), (2, 1, 300.0, 3.0));
        assert_eq!(i.avgs(), [200.0, 300.0, 20.0, 30.0, 300.0, 30.0]);
        assert_eq!(st[State::Tray as usize], Agg::default());
        assert_eq!(per_state(&st), "&s_idle=2,1,200,300,20,30,300,30&s_player=1,0,200,200,50,50,0,0");
        assert_eq!(fields(&st), "?m=200&mp=300&c=30&cp=50&rm=300&rc=30&n=3&rn=1");
        assert_eq!(fields(&[Agg::default(); 4]), "");
    }
}
