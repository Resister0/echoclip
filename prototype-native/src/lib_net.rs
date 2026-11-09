//! Chat B: Echo's internet requests (src-tauri net.rs: reqwest blocking + rustls, same TLS stack), the Echo Cloud
//! server call (ec_call), the log (proto.log next to the exe), text clipboard, and Echo's secrets in settings.json
//! (Windows DPAPI "enc:<hex>" via crypt32.dll at run time; Linux "ks:" = desktop keyring via secret-tool).
//! Anyone may use these: crate::lib_net::request / ec_call / log_line / copy_text / secret_get.
use slint::ComponentHandle;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const EC_API_DEFAULT: &str = "https://echoclip.link";
const UA: &str = "Echo";

fn client(timeout: Option<Duration>) -> Result<reqwest::blocking::Client, String> {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    reqwest::blocking::Client::builder()
        .user_agent(UA)
        .connect_timeout(Duration::from_secs(15))
        .timeout(timeout)
        .build()
        .map_err(|e| format!("network setup failed: {e}"))
}

fn describe(e: reqwest::Error) -> String {
    use std::error::Error;
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(x) = src {
        s.push_str(": ");
        s.push_str(&x.to_string());
        src = x.source();
    }
    s
}

/// One request (blocking - worker threads only). Ok((status, body)) for any HTTP answer.
pub fn request(method: &str, url: &str, headers: Vec<(String, String)>, body: Option<String>, timeout_s: u64) -> Result<(u16, String), String> {
    let c = client(Some(Duration::from_secs(timeout_s)))?;
    let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let mut r = c.request(m, url);
    for (k, v) in headers {
        r = r.header(k, v);
    }
    if let Some(b) = body {
        r = r.body(b);
    }
    let resp = r.send().map_err(describe)?;
    let code = resp.status().as_u16();
    let text = resp.text().map_err(describe)?;
    Ok((code, text))
}

/// A small file into memory (profile pictures, share-page thumbnails); 15 s timeout, one try
pub fn get_bytes(url: &str) -> Result<Vec<u8>, String> {
    let c = client(Some(Duration::from_secs(15)))?;
    let resp = c.get(url).send().map_err(describe)?;
    if !resp.status().is_success() {
        return Err(format!("{url} answered {}", resp.status().as_u16()));
    }
    resp.bytes().map(|b| b.to_vec()).map_err(|e| e.to_string())
}

/// Download to a file (3 tries; a stalled one gives up after 60 s without data)
pub fn download(url: &str, to: &Path) -> Result<(), String> {
    let c = client(Some(Duration::from_secs(60)))?;
    let mut last = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_secs(2));
        }
        let r = (|| -> Result<(), String> {
            let mut resp = c.get(url).send().map_err(describe)?;
            if !resp.status().is_success() {
                return Err(format!("{url} answered {}", resp.status().as_u16()));
            }
            let mut f = std::fs::File::create(to).map_err(|e| e.to_string())?;
            std::io::copy(&mut resp, &mut f).map_err(|e| format!("download of {url} broke off: {e}"))?;
            Ok(())
        })();
        match r {
            Ok(()) => return Ok(()),
            Err(e) if e.contains(" answered 4") => return Err(e),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// The Echo Cloud server: ECHO_CLOUD_API, else settings "ec_api", else echoclip.link
pub fn ec_api() -> String {
    let cfg = crate::library::echo_settings();
    let a = std::env::var("ECHO_CLOUD_API")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| cfg.as_ref().and_then(|c| c.get("ec_api")).and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| EC_API_DEFAULT.to_string());
    a.trim_end_matches('/').to_string()
}

/// Echo's ec_call: JSON in and out; server errors ({"error": ...}) come back as Err(message). Blocking.
pub fn ec_call(method: &str, path: &str, token: &str, body: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
    let api = ec_api();
    let mut h = vec![("content-type".to_string(), "application/json".to_string())];
    if !token.is_empty() {
        h.push(("authorization".into(), format!("Bearer {token}")));
    }
    let (code, json) = match request(method, &format!("{api}{path}"), h, body.map(|b| b.to_string()), 30) {
        Ok(r) => r,
        Err(e) => {
            log_line(&format!("echo cloud: {api}{path} unreachable: {e}"));
            return Err("Couldn't reach Echo Cloud - check your internet".into());
        }
    };
    let v: serde_json::Value = serde_json::from_str(&json).unwrap_or(serde_json::Value::Null);
    if let Some(e) = v["error"].as_str() {
        return Err(e.to_string());
    }
    if code >= 300 {
        return Err(format!("Echo Cloud answered {code}"));
    }
    Ok(v)
}

/// ec_call on a worker thread, answer on the UI thread
pub fn call_ui(a: &crate::AppWindow, method: &str, path: &str, token: String, body: Option<serde_json::Value>, done: impl FnOnce(&crate::AppWindow, Result<serde_json::Value, String>) + 'static) {
    let (method, path) = (method.to_string(), path.to_string());
    let w = a.as_weak();
    bg(move || ec_call(&method, &path, &token, body), move |r| {
        if let Some(a) = w.upgrade() {
            done(&a, r)
        }
    });
}

/// run `work` on a thread, `done` with its result on the UI thread
pub fn bg<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(T) + 'static) {
    let (tx, rx) = std::sync::mpsc::channel::<T>();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    let done = std::cell::RefCell::new(Some(done));
    let holder = std::rc::Rc::new(std::cell::RefCell::new(None::<slint::Timer>));
    let h2 = holder.clone();
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_millis(80), move || {
        let r = match rx.try_recv() {
            Ok(v) => Some(v),
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(_) => None,
        };
        let x = h2.borrow_mut().take();
        slint::Timer::single_shot(Duration::from_millis(0), move || drop(x));
        if let (Some(v), Some(f)) = (r, done.borrow_mut().take()) {
            f(v);
        }
    });
    *holder.borrow_mut() = Some(t);
}

// ---------- log ----------
pub fn log_path() -> PathBuf {
    std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("proto.log"))).unwrap_or_else(|| std::env::temp_dir().join("echo-native.log"))
}

pub fn log_line(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = writeln!(f, "[{}] {msg}", chrono::Local::now().format("%H:%M:%S"));
    }
}

/// the last `lines` lines, at most `max` bytes (newest part)
pub fn log_tail(lines: usize, max: usize) -> String {
    let t = std::fs::read_to_string(log_path()).unwrap_or_default();
    let l: Vec<&str> = t.lines().collect();
    let mut tail = l[l.len().saturating_sub(lines)..].join("\n");
    if tail.len() > max {
        let mut cut = tail.len() - max;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail = tail[cut..].to_string();
    }
    tail
}

/// Echo's redact_emails: name@domain.tld -> [email]
pub fn redact_emails(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let local = |ch: char| ch.is_alphanumeric() || "._%+-".contains(ch);
    let dom = |ch: char| ch.is_alphanumeric() || ".-".contains(ch);
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        if c[i] == '@' {
            let tail: Vec<char> = out.chars().collect();
            let mut k = tail.len();
            while k > 0 && local(tail[k - 1]) {
                k -= 1;
            }
            let mut j = i + 1;
            while j < c.len() && dom(c[j]) {
                j += 1;
            }
            let d: String = c[i + 1..j].iter().collect();
            let d = d.trim_end_matches('.');
            if k < tail.len() && d.contains('.') {
                let a = tail[..k].iter().collect::<String>().len();
                out.truncate(a);
                out.push_str("[email]");
                i += 1 + d.chars().count();
                continue;
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

// ---------- clipboard (text) ----------
pub fn copy_text(text: &str) -> Result<(), String> {
    if let Ok(f) = std::env::var("ECHO_CLIP_OUT") {
        return std::fs::write(f, text).map_err(|e| e.to_string());
    }
    #[cfg(windows)]
    unsafe {
        // OpenClipboard / EmptyClipboard / GlobalAlloc / SetClipboardData(CF_UNICODETEXT) ⚠ untested
        let u = libloading::Library::new("user32.dll").map_err(|e| e.to_string())?;
        let k = libloading::Library::new("kernel32.dll").map_err(|e| e.to_string())?;
        let open: libloading::Symbol<unsafe extern "system" fn(isize) -> i32> = u.get(b"OpenClipboard\0").map_err(|e| e.to_string())?;
        let empty: libloading::Symbol<unsafe extern "system" fn() -> i32> = u.get(b"EmptyClipboard\0").map_err(|e| e.to_string())?;
        let set: libloading::Symbol<unsafe extern "system" fn(u32, isize) -> isize> = u.get(b"SetClipboardData\0").map_err(|e| e.to_string())?;
        let close: libloading::Symbol<unsafe extern "system" fn() -> i32> = u.get(b"CloseClipboard\0").map_err(|e| e.to_string())?;
        let alloc: libloading::Symbol<unsafe extern "system" fn(u32, usize) -> isize> = k.get(b"GlobalAlloc\0").map_err(|e| e.to_string())?;
        let lock: libloading::Symbol<unsafe extern "system" fn(isize) -> *mut u16> = k.get(b"GlobalLock\0").map_err(|e| e.to_string())?;
        let unlock: libloading::Symbol<unsafe extern "system" fn(isize) -> i32> = k.get(b"GlobalUnlock\0").map_err(|e| e.to_string())?;
        let w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let mut ok = false;
        for _ in 0..10 {
            if open(0) != 0 {
                empty();
                let h = alloc(0x0002, w.len() * 2);
                if h != 0 {
                    let p = lock(h);
                    if !p.is_null() {
                        std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                        unlock(h);
                        ok = set(13, h) != 0;
                    }
                }
                close();
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        if ok {
            Ok(())
        } else {
            Err("copy failed".into())
        }
    }
    #[cfg(not(windows))]
    {
        use std::io::Write;
        use std::process::{Command, Stdio};
        for (cmd, args) in [("wl-copy", vec![]), ("xclip", vec!["-selection", "clipboard"]), ("xsel", vec!["-b", "-i"])] {
            if let Ok(mut c) = Command::new(cmd).args(&args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
                if let Some(mut i) = c.stdin.take() {
                    let _ = i.write_all(text.as_bytes());
                }
                // wl-copy / xclip keep running to serve the clipboard: don't wait for them
                std::thread::spawn(move || {
                    let _ = c.wait();
                });
                return Ok(());
            }
        }
        Err("no clipboard tool (wl-copy / xclip)".into())
    }
}
