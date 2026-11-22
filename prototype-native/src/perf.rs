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
    // 12 Nov 2026: graphics-card memory (gpumem.rs) - minutes it could be read, sum, peak (Linux: never)
    gn: u32,
    vram_sum: f64,
    vram_peak: f64,
    // 17 Nov 2026: Echo's own process only (Windows: the zero-copy capture + encoder thread live in it; the rest is
    // ffmpeg = audio, noise filter, muxing) - all minutes / recording minutes. Tells where the CPU goes on PCs that use a lot.
    self_sum: f64,
    rself_sum: f64,
    // 21 Nov 2026: where the CPU goes (all minutes): the recorder's helpers (ffmpeg / gpu-screen-recorder), Echo's other
    // child processes (jobs: clip saves, upload copies, thumbnails...), and inside Echo's own process the window's
    // thread, the capture + encoder thread (Windows) and the audio mixer. Own minus those three = the graphics driver's
    // and the hardware encoder's own threads, the player (mpv), everything else.
    rec_sum: f64,
    job_sum: f64,
    ui_sum: f64,
    cap_sum: f64,
    aud_sum: f64,
}

/// one minute's CPU, % of the whole PC: everything, Echo's own process, and the parts (see Agg)
#[derive(Default, Clone, Copy, PartialEq, Debug)]
struct Split {
    cpu: f64,
    own: f64,
    rec: f64,
    job: f64,
    ui: f64,
    cap: f64,
    aud: f64,
}

impl Agg {
    /// a minute where all the CPU counts as Echo's own (tests)
    #[cfg(test)]
    fn add(&mut self, mem: f64, cpu: f64, vram: Option<f64>, rec: bool) {
        self.add_split(mem, Split { cpu, own: cpu, ..Default::default() }, vram, rec);
    }
    fn add_split(&mut self, mem: f64, x: Split, vram: Option<f64>, rec: bool) {
        let (cpu, cpu_self) = (x.cpu, x.own);
        self.rec_sum += x.rec;
        self.job_sum += x.job;
        self.ui_sum += x.ui;
        self.cap_sum += x.cap;
        self.aud_sum += x.aud;
        self.self_sum += cpu_self;
        if rec {
            self.rself_sum += cpu_self;
        }
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
        if let Some(v) = vram {
            self.gn += 1;
            self.vram_sum += v;
            self.vram_peak = self.vram_peak.max(v);
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
        self.gn += o.gn;
        self.vram_sum += o.vram_sum;
        self.vram_peak = self.vram_peak.max(o.vram_peak);
        self.self_sum += o.self_sum;
        self.rself_sum += o.rself_sum;
        self.rec_sum += o.rec_sum;
        self.job_sum += o.job_sum;
        self.ui_sum += o.ui_sum;
        self.cap_sum += o.cap_sum;
        self.aud_sum += o.aud_sum;
    }
    /// (avg VRAM MB, peak VRAM MB) over the minutes it was read; None = never read (Linux, old Windows)
    fn vram(&self) -> Option<(f64, f64)> {
        (self.gn > 0).then(|| (self.vram_sum / self.gn as f64, self.vram_peak))
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

fn record(state: State, mem: f64, x: Split, vram: Option<f64>, rec: bool) {
    let mut g = AGG.lock().unwrap();
    g.get_or_insert_with(Default::default)[state as usize].add_split(mem, x, vram, rec);
}

/// whose process it is
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Own,
    /// the recorder's ffmpeg / gpu-screen-recorder
    Rec,
    /// any other child process (clip save, upload copy, thumbnails...)
    Job,
}

/// One reading of Echo + its child processes and a few of Echo's own threads. CPU times only ever grow, so a minute's
/// use is the sum of each process' increase - 21 Nov 2026: it used to be (sum now - sum before), and when a busy
/// child (a clip save's ffmpeg) ended in between, that went below 0 and the WHOLE minute counted as 0 % (the admin
/// page showed minutes of 0 % between minutes of 20 %+ on one PC).
#[derive(Default, Clone, Debug)]
struct Reading {
    /// when it was taken, in process start-time ticks (TICKS_PER_S)
    at: u64,
    mem: f64,
    pids: Vec<u32>,
    /// (pid, start time in ticks, kind, CPU seconds so far)
    procs: Vec<(u32, u64, Kind, f64)>,
    /// Echo's own threads [window, capture + encoder, audio mixer]: (OS thread id, CPU seconds so far); id 0 = none
    threads: [(u32, f64); 3],
}

/// CPU seconds used between two readings, by part
fn used(prev: &Reading, now: &Reading) -> Split {
    let mut u = Split::default();
    for &(pid, start, kind, cpu) in &now.procs {
        let d = match prev.procs.iter().find(|p| p.0 == pid && p.1 == start) {
            Some(p) => (cpu - p.3).max(0.0),
            // started since the last reading: all of its time is new. An older one that couldn't be read then counts
            // from the next minute on (else its whole life would land in this minute).
            None if start + TICKS_PER_S >= prev.at => cpu,
            None => 0.0,
        };
        u.cpu += d;
        match kind {
            Kind::Own => u.own += d,
            Kind::Rec => u.rec += d,
            Kind::Job => u.job += d,
        }
    }
    let th = |i: usize| -> f64 {
        let ((tid, cpu), (ptid, pcpu)) = (now.threads[i], prev.threads[i]);
        match tid {
            0 => 0.0,
            // the same thread: its increase; a new one (recorder restarted) started after the last reading
            t if t == ptid => (cpu - pcpu).max(0.0),
            _ => cpu,
        }
    };
    // never more than the process they belong to
    u.ui = th(0).min(u.own);
    u.cap = th(1).min(u.own - u.ui);
    u.aud = th(2).min(u.own - u.ui - u.cap);
    u
}

/// the window's thread (perf::start runs on it)
static UI_TID: AtomicU32 = AtomicU32::new(0);

/// the threads Reading::threads follows
fn followed_threads() -> [u32; 3] {
    use echo_core::recorder::PERF_TIDS;
    [UI_TID.load(Ordering::Relaxed), PERF_TIDS[0].load(Ordering::Relaxed), PERF_TIDS[1].load(Ordering::Relaxed)]
}

/// a child's kind from the recorder's process ids
fn kind_of(pid: u32) -> Kind {
    use echo_core::recorder::PERF_PIDS;
    if PERF_PIDS.iter().any(|p| p.load(Ordering::Relaxed) == pid) {
        Kind::Rec
    } else {
        Kind::Job
    }
}

/// start the sampler (once, at startup; on the window's thread)
pub fn start() {
    UI_TID.store(echo_core::recorder::os_thread_id(), Ordering::Relaxed);
    std::thread::Builder::new()
        .name("perf".into())
        .spawn(|| {
            let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
            let mut last: Option<(Instant, Reading)> = None;
            loop {
                std::thread::sleep(Duration::from_secs(60));
                let Some(r) = sample() else {
                    take_ticks();
                    continue;
                };
                let now = Instant::now();
                if let Some((t0, prev)) = &last {
                    let wall = now.duration_since(*t0).as_secs_f64();
                    let pct = |s: f64| s / (wall * cores) * 100.0;
                    let u = used(prev, &r);
                    let cpu = pct(u.cpu).min(100.0);
                    let own = pct(u.own).min(cpu);
                    let x = Split { cpu, own, rec: pct(u.rec).min(cpu), job: pct(u.job).min(cpu), ui: pct(u.ui).min(own), cap: pct(u.cap).min(own), aud: pct(u.aud).min(own) };
                    let rec = crate::rec_core::rec().is_some_and(|r| r.is_recording());
                    // the same processes' graphics-card memory (Echo + the recorder's ffmpeg, like Task Manager's group)
                    let t = Instant::now();
                    let vram = crate::gpumem::vram_mb(&r.pids);
                    let state = dominant(take_ticks());
                    // ECHO_PERF_LOG=1 (testing): each minute's sample in the log, with what the VRAM read cost
                    if std::env::var_os("ECHO_PERF_LOG").is_some() {
                        echo_core::recorder::log_line(&format!(
                            "perf: {} - memory {:.0} MB, CPU {cpu:.2} % (Echo {own:.2}: window {:.2}, capture {:.2}, audio {:.2}; recorder {:.2}, jobs {:.2}), VRAM {} ({} processes, read in {} us)",
                            NAMES[state as usize],
                            r.mem,
                            x.ui,
                            x.cap,
                            x.aud,
                            x.rec,
                            x.job,
                            vram.map_or("-".into(), |v| format!("{v:.0} MB")),
                            r.pids.len(),
                            t.elapsed().as_micros()
                        ));
                    }
                    record(state, r.mem, x, vram, rec);
                } else {
                    take_ticks(); // the first minute isn't measured: its ticks don't count either
                }
                last = Some((now, r));
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

/// "?m=..&mp=..&c=..&cp=..&rm=..&rc=..&n=..&rn=..[&g=..&gp=..]&k=..&sv=1&s_tray=..&s_active=..&cpu=..&gpu=..&ram=..&os=..&res=..&fps=..&enc=.."
/// for the update check ("" = nothing measured yet, or the switch is off); resets.
/// m / mp / rm = MB, c / cp / rc = CPU % x10 (one decimal), n / rn = minutes measured / of those recording, k = cores,
/// g / gp = graphics-card memory MB, average / peak (12 Nov 2026; left out when it couldn't be read - Linux)
/// - these first ones are all states together (what servers before perf-states read).
/// sv=1 = per-state fields follow: s_<tray|idle|active|player>=n,rn,m,mp,c,cp,rm,rc (same units; only states that
/// had minutes) + v_<state>=g,gp when VRAM was read. The rest = this PC's hardware + recording settings (specs.rs)
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
    format!("{q}&k={cores}&sv=1{}", per_state(&st)) + &crate::specs::q() + &self_fields(&st) + &part_fields(&st)
}

/// 21 Nov 2026: "&x_<state>=rec,job,ui,cap,aud" - the parts of that state's CPU, % x10 averaged over its minutes (see
/// Agg). Own process minus ui / cap / aud = drivers, hardware encoder threads, player, the rest. Last in the query:
/// servers before it ignore it.
fn part_fields(st: &[Agg; 4]) -> String {
    let mut o = String::new();
    for s in STATES {
        let a = &st[s as usize];
        if a.n == 0 {
            continue;
        }
        let v = |x: f64| x / a.n as f64 * 10.0;
        o += &format!("&x_{}={:.0},{:.0},{:.0},{:.0},{:.0}", NAMES[s as usize], v(a.rec_sum), v(a.job_sum), v(a.ui_sum), v(a.cap_sum), v(a.aud_sum));
    }
    o
}

/// 17 Nov 2026: "&ce=..&rce=.." = Echo's own process CPU % x10, all minutes / recording minutes (the rest of c / rc is its
/// ffmpeg). Last in the query: servers before it ignore fields they don't know.
/// 18 Nov 2026: + "&e_<state>=ce,rce" per state with minutes (rce 0 without recording minutes), so the admin page's
/// per-state split is that state's own (it used to put the all-states share under every state).
fn self_fields(st: &[Agg; 4]) -> String {
    let mut a = Agg::default();
    st.iter().for_each(|s| a.merge(s));
    if a.n == 0 {
        return String::new();
    }
    let mut o = format!("&ce={:.0}", a.self_sum / a.n as f64 * 10.0);
    if a.rn > 0 {
        o += &format!("&rce={:.0}", a.rself_sum / a.rn as f64 * 10.0);
    }
    for s in STATES {
        let x = &st[s as usize];
        if x.n == 0 {
            continue;
        }
        let rce = if x.rn > 0 { x.rself_sum / x.rn as f64 * 10.0 } else { 0.0 };
        o += &format!("&e_{}={:.0},{rce:.0}", NAMES[s as usize], x.self_sum / x.n as f64 * 10.0);
    }
    o
}

/// the all-states fields "?m=..&n=..&rn=.." ("" = no minutes)
fn fields(st: &[Agg; 4]) -> String {
    let mut a = Agg::default();
    st.iter().for_each(|s| a.merge(s));
    if a.n == 0 {
        return String::new();
    }
    let [m, mp, c, cp, rm, rc] = a.avgs();
    let g = a.vram().map(|(g, gp)| format!("&g={g:.0}&gp={gp:.0}")).unwrap_or_default();
    format!("?m={m:.0}&mp={mp:.0}&c={c:.0}&cp={cp:.0}&rm={rm:.0}&rc={rc:.0}&n={}&rn={}{g}", a.n, a.rn)
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
        // VRAM in a field of its own: servers before it want exactly 8 numbers in s_<state> and would drop the state
        if let Some((g, gp)) = a.vram() {
            o += &format!("&v_{}={g:.0},{gp:.0}", NAMES[s as usize]);
        }
    }
    o
}

/// process start times / Reading::at: Windows FILETIME (100 ns), Linux clock ticks since boot (100 a second on every
/// desktop distro)
#[cfg(windows)]
const TICKS_PER_S: u64 = 10_000_000;
#[cfg(not(windows))]
const TICKS_PER_S: u64 = 100;

/// Echo's process + its children (memory, CPU so far, start time) and Echo's followed threads.
/// 21 Nov 2026: a "child" must have started after Echo. Windows keeps a process' parent id after the parent is gone and
/// reuses ids, so a process whose long-dead parent had Echo's id counted as Echo's (Linux re-parents orphans: no issue).
#[cfg(windows)]
fn sample() -> Option<Reading> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX2};
    use windows_sys::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, GetProcessIdOfThread, GetProcessTimes, GetThreadTimes, OpenProcess, OpenThread, PROCESS_QUERY_LIMITED_INFORMATION, THREAD_QUERY_LIMITED_INFORMATION,
    };
    let ticks = |f: &FILETIME| (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64;
    let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    // (private memory MB, CPU seconds so far, start time)
    let one = |pid: u32| -> Option<(f64, f64, u64)> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let mut c: PROCESS_MEMORY_COUNTERS_EX2 = std::mem::zeroed();
            c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
            let mem = if K32GetProcessMemoryInfo(h, &mut c as *mut _ as *mut _, c.cb) != 0 { c.PrivateWorkingSetSize as f64 / 1048576.0 } else { 0.0 };
            let (mut cr, mut ex, mut k, mut u) = (z, z, z, z);
            let ok = GetProcessTimes(h, &mut cr, &mut ex, &mut k, &mut u) != 0;
            CloseHandle(h);
            ok.then(|| (mem, (ticks(&k) + ticks(&u)) as f64 / 1e7, ticks(&cr)))
        }
    };
    let me = unsafe { GetCurrentProcessId() };
    let thread = |tid: u32| -> f64 {
        if tid == 0 {
            return 0.0;
        }
        unsafe {
            let h = OpenThread(THREAD_QUERY_LIMITED_INFORMATION, 0, tid);
            if h.is_null() {
                return 0.0;
            }
            let (mut cr, mut ex, mut k, mut u) = (z, z, z, z);
            // a thread id is reused once its thread ended: only a thread of Echo's own process counts
            let cpu = if GetProcessIdOfThread(h) == me && GetThreadTimes(h, &mut cr, &mut ex, &mut k, &mut u) != 0 { (ticks(&k) + ticks(&u)) as f64 / 1e7 } else { 0.0 };
            CloseHandle(h);
            cpu
        }
    };
    let mut at = z;
    unsafe { GetSystemTimeAsFileTime(&mut at) };
    let (mem, cpu, born) = one(me)?;
    let mut r = Reading { at: ticks(&at), mem, pids: vec![me], procs: vec![(me, born, Kind::Own, cpu)], threads: Default::default() };
    for (i, tid) in followed_threads().into_iter().enumerate() {
        r.threads[i] = (tid, thread(tid));
    }
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap != INVALID_HANDLE_VALUE {
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            if Process32FirstW(snap, &mut e) != 0 {
                loop {
                    if e.th32ParentProcessID == me && e.th32ProcessID != me {
                        if let Some((m, c, start)) = one(e.th32ProcessID).filter(|x| x.2 >= born) {
                            r.mem += m;
                            r.pids.push(e.th32ProcessID);
                            r.procs.push((e.th32ProcessID, start, kind_of(e.th32ProcessID), c));
                        }
                    }
                    if Process32NextW(snap, &mut e) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snap);
        }
    }
    Some(r)
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

/// (CPU seconds so far, start time in ticks) from a /proc/<pid>/stat or /proc/<pid>/task/<tid>/stat line
#[cfg(not(windows))]
fn stat_times(st: &str) -> Option<(f64, u64)> {
    let f: Vec<&str> = st[st.rfind(')')? + 2..].split_whitespace().collect();
    let cpu = (f.get(11)?.parse::<f64>().ok()? + f.get(12)?.parse::<f64>().ok()?) / TICKS_PER_S as f64;
    Some((cpu, f.get(19)?.parse().ok()?))
}

#[cfg(not(windows))]
fn sample() -> Option<Reading> {
    // (private memory MB, CPU seconds so far, start time)
    fn one(pid: &str) -> Option<(f64, f64, u64)> {
        let (cpu, start) = stat_times(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)?;
        let kb = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok().and_then(|r| private_kb(&r)).unwrap_or_else(|| {
            let s = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
            s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0)
        });
        Some((kb / 1024.0, cpu, start))
    }
    let me = std::process::id().to_string();
    let up = std::fs::read_to_string("/proc/uptime").ok()?;
    let at = (up.split_whitespace().next()?.parse::<f64>().ok()? * TICKS_PER_S as f64) as u64;
    let (mem, cpu, born) = one(&me)?;
    let mut r = Reading { at, mem, pids: vec![std::process::id()], procs: vec![(std::process::id(), born, Kind::Own, cpu)], threads: Default::default() };
    for (i, tid) in followed_threads().into_iter().enumerate() {
        let cpu = if tid == 0 { None } else { std::fs::read_to_string(format!("/proc/self/task/{tid}/stat")).ok().and_then(|s| stat_times(&s)) };
        r.threads[i] = (tid, cpu.map_or(0.0, |c| c.0));
    }
    for e in std::fs::read_dir("/proc").ok()?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name == me || !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let ppid = std::fs::read_to_string(format!("/proc/{name}/stat")).ok().and_then(|st| st.rfind(')').and_then(|i| st[i + 2..].split_whitespace().nth(1).map(|x| x.to_string())));
        if ppid.as_deref() == Some(me.as_str()) {
            if let (Some((m, c, start)), Ok(pid)) = (one(&name), name.parse::<u32>()) {
                r.mem += m;
                r.pids.push(pid);
                r.procs.push((pid, start, kind_of(pid), c));
            }
        }
    }
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agg(n: u32, mem_sum: f64, mem_peak: f64, cpu_sum: f64, cpu_peak: f64, rn: u32, rmem_sum: f64, rcpu_sum: f64) -> Agg {
        Agg { n, mem_sum, mem_peak, cpu_sum, cpu_peak, rn, rmem_sum, rcpu_sum, ..Default::default() }
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

    /// 17 Nov 2026: Echo's own CPU next to the total (the rest = ffmpeg), all minutes and recording minutes
    #[test]
    fn own_process_cpu_fields() {
        let mut st = [Agg::default(); 4];
        let sp = |cpu: f64, own: f64| Split { cpu, own, ..Default::default() };
        st[State::Tray as usize].add_split(100.0, sp(4.0, 3.0), None, true);
        st[State::Tray as usize].add_split(100.0, sp(2.0, 1.0), None, false);
        assert_eq!(self_fields(&st), "&ce=20&rce=30&e_tray=20,30");
        // 18 Nov 2026: each state gets its own share too (no recording minutes = rce 0)
        st[State::Idle as usize].add_split(100.0, sp(5.0, 5.0), None, false);
        assert_eq!(self_fields(&st), "&ce=30&rce=30&e_tray=20,30&e_idle=50,0");
        assert_eq!(self_fields(&[Agg::default(); 4]), "");
        // the old add() counts everything as Echo's own (nothing split)
        let mut a = Agg::default();
        a.add(50.0, 1.5, None, false);
        assert_eq!(a.self_sum, 1.5);
    }

    fn rd(at: u64, procs: &[(u32, u64, Kind, f64)], threads: [(u32, f64); 3]) -> Reading {
        Reading { at, procs: procs.to_vec(), threads, ..Default::default() }
    }

    /// 21 Nov 2026: each process counts its own increase - a busy child that ended no longer zeroes the minute
    #[test]
    fn used_per_process() {
        let s = TICKS_PER_S;
        let t0 = [(1, 10.0), (7, 4.0), (0, 0.0)];
        let prev = rd(100 * s, &[(1, 0, Kind::Own, 50.0), (20, 90 * s, Kind::Rec, 30.0), (21, 95 * s, Kind::Job, 400.0)], t0);
        // the job (400 s so far) ended; the recorder used 2 s, Echo 3 s (window 1, capture 1.5)
        let now = rd(160 * s, &[(1, 0, Kind::Own, 53.0), (20, 90 * s, Kind::Rec, 32.0)], [(1, 11.0), (7, 5.5), (0, 0.0)]);
        let u = used(&prev, &now);
        assert_eq!((u.cpu, u.own, u.rec, u.job, u.ui, u.cap, u.aud), (5.0, 3.0, 2.0, 0.0, 1.0, 1.5, 0.0));
        // a job that started in this minute counts in full; one older than the last reading (couldn't be read then)
        // only from the next minute on; the same pid with another start time is another process
        let now = rd(160 * s, &[(1, 0, Kind::Own, 53.0), (20, 90 * s, Kind::Rec, 32.0), (30, 130 * s, Kind::Job, 6.0), (31, 10 * s, Kind::Job, 900.0), (21, 150 * s, Kind::Job, 1.0)], t0);
        let u = used(&prev, &now);
        assert_eq!((u.cpu, u.job), (12.0, 7.0));
        // the capture thread restarted (new id): its whole time is new; thread parts never exceed Echo's own process
        let now = rd(160 * s, &[(1, 0, Kind::Own, 51.0)], [(1, 12.0), (9, 0.8), (0, 0.0)]);
        let u = used(&prev, &now);
        assert_eq!((u.own, u.ui, u.cap), (1.0, 1.0, 0.0));
        let now = rd(160 * s, &[(1, 0, Kind::Own, 53.0)], [(1, 10.5), (9, 0.8), (3, 0.2)]);
        let u = used(&prev, &now);
        assert_eq!((u.ui, u.cap, u.aud), (0.5, 0.8, 0.2));
    }

    /// the parts per state, % x10 averaged over that state's minutes
    #[test]
    fn part_fields_per_state() {
        let mut st = [Agg::default(); 4];
        st[State::Tray as usize].add_split(100.0, Split { cpu: 3.0, own: 1.0, rec: 0.6, job: 1.6, ui: 0.1, cap: 0.6, aud: 0.2 }, None, true);
        st[State::Tray as usize].add_split(100.0, Split { cpu: 1.0, own: 0.8, rec: 0.2, job: 0.0, ui: 0.1, cap: 0.4, aud: 0.2 }, None, true);
        st[State::Player as usize].add_split(100.0, Split { cpu: 9.0, own: 9.0, ui: 4.0, ..Default::default() }, None, false);
        assert_eq!(part_fields(&st), "&x_tray=4,8,1,5,2&x_player=0,0,40,0,0");
        assert_eq!(part_fields(&[Agg::default(); 4]), "");
    }

    /// this PC: a real reading sees Echo's own process (this test) and its main thread
    #[test]
    fn reading_this_process() {
        UI_TID.store(echo_core::recorder::os_thread_id(), Ordering::Relaxed);
        let r = sample().unwrap();
        assert!(r.at > 0 && r.mem > 0.0);
        assert_eq!(r.procs[0].0, std::process::id());
        assert_eq!(r.procs[0].2, Kind::Own);
        assert!(r.threads[0].0 != 0);
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
        st[State::Idle as usize].add(100.0, 1.0, None, false);
        st[State::Idle as usize].add(300.0, 3.0, None, true);
        st[State::Player as usize].add(200.0, 5.0, None, false);
        let i = st[State::Idle as usize];
        assert_eq!((i.n, i.rn, i.mem_peak, i.cpu_peak), (2, 1, 300.0, 3.0));
        assert_eq!(i.avgs(), [200.0, 300.0, 20.0, 30.0, 300.0, 30.0]);
        assert_eq!(st[State::Tray as usize], Agg::default());
        assert_eq!(per_state(&st), "&s_idle=2,1,200,300,20,30,300,30&s_player=1,0,200,200,50,50,0,0");
        assert_eq!(fields(&st), "?m=200&mp=300&c=30&cp=50&rm=300&rc=30&n=3&rn=1");
        assert_eq!(fields(&[Agg::default(); 4]), "");
    }

    /// graphics-card memory: averaged over the minutes it was read, peak kept, sent only when read at all
    #[test]
    fn vram_per_state() {
        let mut st = [Agg::default(); 4];
        st[State::Tray as usize].add(20.0, 0.2, Some(200.0), true);
        st[State::Tray as usize].add(20.0, 0.2, None, true); // a minute it couldn't be read: not counted as 0
        st[State::Tray as usize].add(20.0, 0.2, Some(220.0), true);
        st[State::Player as usize].add(250.0, 4.0, Some(310.0), false);
        assert_eq!(st[State::Tray as usize].vram(), Some((210.0, 220.0)));
        assert_eq!(per_state(&st), "&s_tray=3,3,20,20,2,2,20,2&v_tray=210,220&s_player=1,0,250,250,40,40,0,0&v_player=310,310");
        assert_eq!(fields(&st), "?m=78&mp=250&c=12&cp=40&rm=20&rc=2&n=4&rn=3&g=243&gp=310");
        // Linux / never read: exactly the old fields, no v_ / g / gp at all
        let mut l = [Agg::default(); 4];
        l[State::Idle as usize].add(100.0, 1.0, None, false);
        assert_eq!(per_state(&l), "&s_idle=1,0,100,100,10,10,0,0");
        assert!(!fields(&l).contains("&g="));
    }

    /// this PC: the reader finds the GPU memory of a process that drew something (a plain process may hold none)
    #[cfg(windows)]
    #[test]
    fn vram_reader_runs() {
        let me = std::process::id();
        // never panics; on a PC with a GPU the answer is a sane number or None for "this process never used it"
        if let Some(v) = crate::gpumem::vram_mb(&[me]) {
            assert!((0.0..100_000.0).contains(&v), "{v}");
        }
        assert_eq!(crate::gpumem::vram_mb(&[]), None);
    }
}
