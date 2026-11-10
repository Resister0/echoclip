//! Helper processes that must not outlive Echo (download-bar session, 10 Nov 2026). The cloud streaming server
//! (`rclone serve http`, ~40-80 MB RAM) was left running after Echo quit or was restarted: three of them from
//! 16:31-16:47 were still alive at 18:45 (parent = systemd), and each Echo start added one more.
//! - Linux: the child gets PR_SET_PDEATHSIG = SIGTERM, so the kernel ends it as soon as Echo is gone (quit, crash,
//!   kill -9). That signal follows the THREAD that started the child, and Echo starts rclone on short-lived worker
//!   threads - so every tied child is started from one long-lived spawner thread here.
//! - Windows: the child goes into a Job Object with KILL_ON_JOB_CLOSE; Windows closes Echo's handle to the job when
//!   Echo ends in any way, which ends the child.
//! - Earlier Echo versions left servers behind: `reap_orphans` stops the ones that use Echo's own rclone config and
//!   whose Echo is gone (Linux; on Windows the job makes new leftovers impossible).
use std::process::{Child, Command};

/// start `c` so it ends together with Echo
pub fn spawn(c: Command) -> std::io::Result<Child> {
    #[cfg(target_os = "linux")]
    {
        linux::spawn(c)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut c = c;
        let child = c.spawn()?;
        #[cfg(windows)]
        win::tie(&child);
        Ok(child)
    }
}

/// Stop `rclone serve` processes that an earlier Echo started with this config file and left behind (their Echo is
/// gone: parent = init / the user's systemd). A server of a running Echo is never touched. Returns how many.
pub fn reap_orphans(config: &std::path::Path) -> usize {
    #[cfg(target_os = "linux")]
    {
        linux::reap(config)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = config;
        0
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::{c_int, c_ulong};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};
    use std::sync::{mpsc, Mutex, OnceLock};

    extern "C" {
        fn prctl(option: c_int, ...) -> c_int;
        fn getppid() -> c_int;
        fn kill(pid: c_int, sig: c_int) -> c_int;
    }
    const PR_SET_PDEATHSIG: c_int = 1;
    const SIGTERM: c_int = 15;

    type Job = (Command, mpsc::Sender<std::io::Result<Child>>);

    pub fn spawn(mut c: Command) -> std::io::Result<Child> {
        let echo = std::process::id() as c_int;
        // SAFETY: runs in the child between fork and exec; prctl / getppid are async-signal-safe
        unsafe {
            c.pre_exec(move || {
                prctl(PR_SET_PDEATHSIG, SIGTERM as c_ulong);
                // Echo already gone before prctl took effect: don't start at all
                if getppid() != echo {
                    return Err(std::io::Error::other("Echo is gone"));
                }
                Ok(())
            });
        }
        static TX: OnceLock<Mutex<mpsc::Sender<Job>>> = OnceLock::new();
        let tx = TX.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Job>();
            // lives as long as Echo: the "parent" the death signal is tied to
            let _ = std::thread::Builder::new().name("echo-spawner".into()).spawn(move || {
                for (mut c, back) in rx {
                    let _ = back.send(c.spawn());
                }
            });
            Mutex::new(tx)
        });
        let (back, answer) = mpsc::channel();
        tx.lock().map_err(|_| std::io::Error::other("spawner"))?.send((c, back)).map_err(|_| std::io::Error::other("spawner gone"))?;
        answer.recv().map_err(|_| std::io::Error::other("spawner gone"))?
    }

    /// (pid, parent pid, arguments) of every process we may look at
    fn procs() -> Vec<(i32, i32, Vec<String>)> {
        let mut out = vec![];
        let Ok(rd) = std::fs::read_dir("/proc") else { return out };
        for e in rd.flatten() {
            let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() else { continue };
            let Ok(cmd) = std::fs::read(e.path().join("cmdline")) else { continue };
            let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else { continue };
            // "pid (comm) state ppid ..." - comm may hold spaces / ')', so read after the LAST ')'
            let ppid = stat.rsplit_once(')').and_then(|(_, r)| r.split_whitespace().nth(1)).and_then(|p| p.parse().ok()).unwrap_or(0);
            let args = cmd.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
            out.push((pid, ppid, args));
        }
        out
    }

    /// an rclone streaming server started by Echo with this config
    pub(super) fn is_echo_serve(args: &[String], config: &str) -> bool {
        let exe = args.first().map(|a| a.rsplit('/').next().unwrap_or(a)).unwrap_or("");
        exe.starts_with("rclone")
            && args.windows(2).any(|w| w[0] == "serve" && w[1] == "http")
            && args.windows(2).any(|w| w[0] == "--config" && w[1] == config)
    }

    /// its Echo is gone: the process was handed to init or to the user's service manager (a "subreaper")
    pub(super) fn orphaned(ppid: i32, parent_comm: Option<&str>) -> bool {
        ppid <= 1 || matches!(parent_comm, None | Some("systemd") | Some("init"))
    }

    pub fn reap(config: &std::path::Path) -> usize {
        let config = config.to_string_lossy();
        let me = std::process::id() as i32;
        let mut n = 0;
        for (pid, ppid, args) in procs() {
            if pid == me || !is_echo_serve(&args, &config) {
                continue;
            }
            let comm = std::fs::read_to_string(format!("/proc/{ppid}/comm")).ok();
            if orphaned(ppid, comm.as_deref().map(str::trim)) {
                // SAFETY: plain kill(2) on a process we checked above
                if unsafe { kill(pid, SIGTERM) } == 0 {
                    n += 1;
                }
            }
        }
        n
    }
}

#[cfg(windows)]
mod win {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub fn tie(child: &std::process::Child) {
        // one job for Echo's whole life; its handle is never closed by Echo itself (Windows closes it at exit)
        static JOB: OnceLock<usize> = OnceLock::new();
        let job = *JOB.get_or_init(|| unsafe {
            let Ok(j) = CreateJobObjectW(None, windows::core::PCWSTR::null()) else { return 0 };
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                j,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok.is_err() {
                return 0;
            }
            j.0 as usize
        });
        if job != 0 {
            unsafe {
                let _ = AssignProcessToJobObject(HANDLE(job as _), HANDLE(child.as_raw_handle() as _));
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::linux::{is_echo_serve, orphaned};

    fn a(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    #[test]
    fn matches_only_echos_server() {
        let conf = "/home/u/.config/Clipper/rclone/echo.conf";
        assert!(is_echo_serve(&a("/home/u/.config/Clipper/rclone/rclone-v1.75.2-linux-amd64/rclone --config /home/u/.config/Clipper/rclone/echo.conf --ask-password=false serve http echo:Echo --read-only"), conf));
        assert!(!is_echo_serve(&a("/usr/bin/rclone --config /home/u/.config/rclone/rclone.conf serve http remote:"), conf));
        assert!(!is_echo_serve(&a("/home/u/.config/Clipper/rclone/rclone --config /home/u/.config/Clipper/rclone/echo.conf copyto a b"), conf));
        assert!(!is_echo_serve(&a("/usr/bin/python3 serve http --config /home/u/.config/Clipper/rclone/echo.conf"), conf));
    }

    #[test]
    fn orphan_rules() {
        assert!(orphaned(1, Some("systemd")));
        assert!(orphaned(2157, Some("systemd")));
        assert!(orphaned(4242, None));
        assert!(!orphaned(401018, Some("Echo")));
        assert!(!orphaned(401018, Some("echo-native-pro")));
    }

    /// a tied child is started from the spawner thread and still runs after the calling thread ended
    #[test]
    fn tied_child_survives_its_worker_thread() {
        let mut child = std::thread::spawn(|| {
            let mut c = std::process::Command::new("sleep");
            c.arg("5");
            super::spawn(c).unwrap()
        })
        .join()
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(matches!(child.try_wait(), Ok(None)), "the child died with the worker thread");
        let _ = child.kill();
        let _ = child.wait();
    }

    /// helper for the test below (does nothing in a normal test run): start a tied `sleep`, print its pid, exit
    #[test]
    fn tie_helper() {
        if std::env::var("ECHO_TIE_HELPER").is_err() {
            return;
        }
        let mut c = std::process::Command::new("sleep");
        c.arg("30");
        let child = super::spawn(c).unwrap();
        println!("TIEPID {}", child.id());
        std::process::exit(0); // "Echo" ends without stopping its child
    }

    /// the tied child ends when the process that started it ends (no Drop, no cleanup - like a crash)
    #[test]
    fn tied_child_dies_with_echo() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "childtie::tests::tie_helper", "--nocapture", "--test-threads=1"])
            .env("ECHO_TIE_HELPER", "1")
            .output()
            .unwrap();
        let txt = String::from_utf8_lossy(&out.stdout);
        let pid: u32 = txt.split("TIEPID ").nth(1).and_then(|r| r.split_whitespace().next()).and_then(|p| p.parse().ok()).expect("helper pid");
        let alive = || std::fs::read_to_string(format!("/proc/{pid}/stat")).map(|s| !s.rsplit_once(')').is_some_and(|(_, r)| r.trim_start().starts_with('Z'))).unwrap_or(false);
        for _ in 0..50 {
            if !alive() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the tied child {pid} outlived its parent");
    }
}
