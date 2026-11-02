//! Echo's own internet requests. These used to go through hidden curl.exe processes, which antivirus
//! programs treat as suspicious (a hidden downloader). Same TLS stack the auto-updater already uses.
use std::{path::Path, time::Duration};

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

/// The blocking client must not run on an async thread (most Tauri commands are async), so every
/// request runs on a short-lived thread of its own.
fn off_thread<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    std::thread::spawn(f).join().unwrap_or_else(|_| Err("network request crashed".into()))
}

/// One request. Ok((status, body)) for any HTTP answer; Err(reason) when the server couldn't be reached.
pub fn request(method: &str, url: &str, headers: Vec<(String, String)>, body: Option<String>, timeout_s: u64) -> Result<(u16, String), String> {
    let (method, url) = (method.to_string(), url.to_string());
    off_thread(move || {
        let c = client(Some(Duration::from_secs(timeout_s)))?;
        let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
        let mut r = c.request(m, &url);
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
    })
}

/// GET a JSON document (GitHub release info).
pub fn get_json(url: &str) -> Result<serde_json::Value, String> {
    let (code, text) = request("GET", url, vec![("accept".into(), "application/json".into())], None, 30)?;
    if code >= 300 {
        return Err(format!("{url} answered {code}"));
    }
    serde_json::from_str(&text).map_err(|e| format!("bad JSON from {url}: {e}"))
}

/// Download a file (follows redirects, up to 3 tries). The blocking client's timeout applies to each
/// read, so a big download may take long, but a stalled one gives up after 60 s without data.
pub fn download(url: &str, to: &Path) -> Result<(), String> {
    let (url, to) = (url.to_string(), to.to_path_buf());
    off_thread(move || {
        let c = client(Some(Duration::from_secs(60)))?;
        let mut last = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_secs(2));
            }
            match fetch(&c, &url, &to) {
                Ok(()) => return Ok(()),
                Err(e) if e.contains(" answered 4") => return Err(e),   // 404 etc: retrying won't help
                Err(e) => last = e,
            }
        }
        Err(last)
    })
}

fn fetch(c: &reqwest::blocking::Client, url: &str, to: &Path) -> Result<(), String> {
    let resp = c.get(url).send().map_err(describe)?;
    if !resp.status().is_success() {
        return Err(format!("{url} answered {}", resp.status().as_u16()));
    }
    let mut f = std::fs::File::create(to).map_err(|e| e.to_string())?;
    let mut resp = resp;
    std::io::copy(&mut resp, &mut f).map_err(|e| format!("download of {url} broke off: {e}"))?;
    Ok(())
}

/// Short reason for the log (DNS, TLS, timeout…).
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
