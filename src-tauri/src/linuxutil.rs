//! Linux versions of the small Windows helpers in winutil.rs.

use std::io::{Read, Write};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::sync::OnceLock;

static LISTENER: OnceLock<UnixListener> = OnceLock::new();

fn addr() -> Option<SocketAddr> {
    // abstract socket (no file): per user, gone when Echo exits
    let user = std::env::var("USER").unwrap_or_default();
    SocketAddr::from_abstract_name(format!("echo-clips-{user}").as_bytes()).ok()
}

/// Single instance: true if Echo already runs (it was asked to show its window, so just exit).
/// Otherwise this process now owns the socket.
pub fn already_running() -> bool {
    let Some(a) = addr() else { return false };
    if let Ok(mut s) = UnixStream::connect_addr(&a) {
        let _ = s.write_all(b"show");
        return true;
    }
    if let Ok(l) = UnixListener::bind_addr(&a) {
        let _ = LISTENER.set(l);
    }
    false
}

/// Calls `cb` whenever a second launch asks the running Echo to show itself.
pub fn listen_show(cb: impl Fn() + Send + 'static) {
    let Some(l) = LISTENER.get() else { return };
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut b = [0u8; 8];
            let mut s = s;
            let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(1)));
            if s.read(&mut b).map_or(false, |n| &b[..n] == b"show") {
                cb();
            }
        }
    });
}

/// Seconds since the last keyboard / mouse input (KDE / freedesktop screensaver D-Bus; 0 if unknown).
pub fn idle_seconds() -> u64 {
    use dbus::blocking::Connection;
    let Ok(c) = Connection::new_session() else { return 0 };
    let p = c.with_proxy("org.freedesktop.ScreenSaver", "/org/freedesktop/ScreenSaver", std::time::Duration::from_secs(2));
    let r: Result<(u32,), _> = p.method_call("org.freedesktop.ScreenSaver", "GetSessionIdleTime", ());
    r.map(|(ms,)| ms as u64 / 1000).unwrap_or(0)
}

/// Free bytes on the drive that holds `dir`.
pub fn disk_free(dir: &str) -> u64 {
    let o = std::process::Command::new("df").args(["--output=avail", "-B1", dir]).output();
    o.ok().and_then(|o| String::from_utf8_lossy(&o.stdout).lines().nth(1).and_then(|l| l.trim().parse().ok())).unwrap_or(0)
}

/// Desktop notification (shows over games in KDE's corner; the Windows popup window can't on Wayland).
pub fn notify(title: &str, sub: &str, ok: bool) {
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "Echo", "-i", "echo-clips", "-t", "2500", "-u", if ok { "low" } else { "normal" }, "-h", "string:x-canonical-private-synchronous:echo", title, sub])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

pub fn pid_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}
