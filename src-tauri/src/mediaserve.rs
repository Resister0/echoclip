//! Tiny localhost HTTP server for clips. WebKitGTK (Linux) can't play videos from Tauri's
//! asset:// protocol, but plays them fine over http with Range requests.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::OnceLock;

static BASE: OnceLock<String> = OnceLock::new();
/// folders (and single files the user picked) the server may hand out - nothing else on the PC (chat 28)
static ROOTS: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());
static FILES: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());

fn canon(p: &str) -> Option<std::path::PathBuf> { std::fs::canonicalize(p).ok() }

pub fn allow_root(dir: &str) {
    if let Some(c) = canon(dir) { let mut r = ROOTS.lock().unwrap(); if !r.contains(&c) { r.push(c); } }
}
pub fn allow_file(file: &str) {
    if let Some(c) = canon(file) { let mut f = FILES.lock().unwrap(); if !f.contains(&c) { f.push(c); } }
}
fn allowed(file: &str) -> bool {
    let Some(c) = canon(file) else { return false };
    FILES.lock().unwrap().contains(&c) || ROOTS.lock().unwrap().iter().any(|r| c.starts_with(r))
}
/// only Echo's own window may read the bytes from a page (canvas / fetch); other web pages get no CORS header
fn cors(origin: &Option<String>) -> String {
    match origin.as_deref() {
        Some(o @ ("tauri://localhost" | "http://tauri.localhost" | "https://tauri.localhost")) => format!("Access-Control-Allow-Origin: {o}\r\nVary: Origin\r\n"),
        _ => String::new(),
    }
}

/// "http://127.0.0.1:PORT/TOKEN/" or "" if the server is not running.
pub fn base() -> String {
    BASE.get().cloned().unwrap_or_default()
}

pub fn start() {
    let l = match TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => { crate::recorder::log_line(&format!("media: server failed: {e}")); return; }
    };
    let port = l.local_addr().map(|a| a.port()).unwrap_or(0);
    // secret part of every address: web pages (or other programs) can't guess it
    let token = crate::recorder::random_token();
    let _ = BASE.set(format!("http://127.0.0.1:{port}/{token}/"));
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let t = token.clone();
            std::thread::spawn(move || { let _ = handle(s, &t); });
        }
    });
}

fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) { out.push(v); i += 3; continue; }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn mime(p: &str) -> Option<&'static str> {
    let e = p.rsplit('.').next()?.to_ascii_lowercase();
    Some(match e.as_str() {
        "mp4" | "m4v" => "video/mp4", "mov" => "video/quicktime", "mkv" => "video/x-matroska",
        "webm" => "video/webm", "m4a" => "audio/mp4", "mp3" => "audio/mpeg", "wav" => "audio/wav",
        "ogg" | "opus" => "audio/ogg", "flac" => "audio/flac", "aac" => "audio/aac",
        _ => return None,
    })
}

fn handle(s: TcpStream, token: &str) -> std::io::Result<()> {
    let mut r = BufReader::new(s.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut range: Option<(u64, Option<u64>)> = None;
    let mut raw_range: Option<String> = None;
    let mut origin: Option<String> = None;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h.trim().is_empty() { break; }
        let lh = h.to_ascii_lowercase();
        if lh.starts_with("origin:") { origin = Some(h[7..].trim().to_string()); }
        if lh.starts_with("range:") { raw_range = Some(h[6..].trim().to_string()); }
        if let Some(v) = lh.strip_prefix("range:") {
            if let Some(v) = v.trim().strip_prefix("bytes=") {
                let mut it = v.splitn(2, '-');
                let a = it.next().and_then(|x| x.trim().parse().ok());
                let b = it.next().and_then(|x| x.trim().parse().ok());
                if let Some(a) = a { range = Some((a, b)); }
            }
        }
    }
    let mut w = s;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    let path = target.split('?').next().unwrap_or("");
    let want = format!("/{token}/");
    let file = match path.strip_prefix(&want) { Some(p) => unescape(p), None => String::new() };
    // a cloud clip: passed through from the cloud streaming server (rclone serve http on this PC)
    if file.starts_with("http://127.0.0.1:") {
        // only to rclone's cloud streaming server (its secret path), never to other programs on this PC
        let rest = &file["http://127.0.0.1:".len()..];
        let port_ok = rest.split('/').next().map_or(false, |p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
        if !port_ok || !rest.contains(&format!("/{}/", crate::recorder::serve_token_pub())) {
            return w.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
        return proxy(&file, raw_range, method == "HEAD", w, &cors(&origin));
    }
    let ct = mime(&file);
    let f = if !file.is_empty() && ct.is_some() && allowed(&file) { std::fs::File::open(&file).ok() } else { None };
    let Some(mut f) = f else {
        return w.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    };
    let len = f.metadata()?.len();
    let fix = btrt_fix(&file, &mut f, len);
    let (start, end, status) = match range {
        Some((a, b)) if a < len => (a, b.unwrap_or(len - 1).min(len - 1), "206 Partial Content"),
        Some(_) => return w.write_all(format!("HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{len}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()),
        None => (0, len.saturating_sub(1), "200 OK"),
    };
    let n = if len == 0 { 0 } else { end - start + 1 };
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Type: {}\r\nAccept-Ranges: bytes\r\nContent-Length: {n}\r\n{}Cache-Control: no-cache\r\nConnection: close\r\n", ct.unwrap(), cors(&origin));
    if range.is_some() { head += &format!("Content-Range: bytes {start}-{end}/{len}\r\n"); }
    head += "\r\n";
    w.write_all(head.as_bytes())?;
    if method == "HEAD" || n == 0 { return Ok(()); }
    f.seek(SeekFrom::Start(start))?;
    let mut left = n;
    let mut buf = vec![0u8; 256 * 1024];
    while left > 0 {
        let m = left.min(buf.len() as u64) as usize;
        let k = f.read(&mut buf[..m])?;
        if k == 0 { break; }
        // patch the bytes of a 0 average bitrate on their way out (see btrt_fix)
        let at = start + (n - left);
        for (off, val) in &fix {
            for (j, b) in val.iter().enumerate() {
                let o = off + j as u64;
                if o >= at && o < at + k as u64 { buf[(o - at) as usize] = *b; }
            }
        }
        w.write_all(&buf[..k])?;
        left -= k as u64;
    }
    Ok(())
}

/// MP4 files whose video "btrt" box says average bitrate 0 (Echo's own exports: libx264 CRF + maxrate) make
/// WebKitGTK's player (GStreamer) stop after the first frames - for some files every time (chat 24: "(edit 2)"
/// froze, the same file with only that field filled in played). The file on disk is not touched: the server
/// hands out the maximum bitrate in that field instead. Returns (file offset, 4 bytes) to patch; cached per file.
fn btrt_fix(path: &str, f: &mut std::fs::File, len: u64) -> Vec<(u64, [u8; 4])> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static CACHE: OnceLock<Mutex<HashMap<String, (u64, u64, Vec<(u64, [u8; 4])>)>>> = OnceLock::new();
    let mtime = f.metadata().ok().and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((l, m, v)) = cache.lock().unwrap().get(path) {
        if *l == len && *m == mtime { return v.clone(); }
    }
    let mut out = Vec::new();
    // top-level boxes: find moov (small, a few hundred KB at most for clips)
    let mut pos = 0u64;
    let mut hdr = [0u8; 16];
    while pos + 8 <= len {
        if f.seek(SeekFrom::Start(pos)).is_err() || f.read_exact(&mut hdr[..8]).is_err() { break; }
        let mut size = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let kind = [hdr[4], hdr[5], hdr[6], hdr[7]];
        let mut h = 8u64;
        if size == 1 {
            if f.read_exact(&mut hdr[8..16]).is_err() { break; }
            size = u64::from_be_bytes(hdr[8..16].try_into().unwrap()); h = 16;
        } else if size == 0 { size = len - pos; }
        if size < h { break; }
        if &kind == b"moov" {
            if size <= 32 << 20 {
                let mut m = vec![0u8; (size - h) as usize];
                if f.read_exact(&mut m).is_ok() {
                    // btrt = size 20, "btrt", bufferSizeDB, maxBitrate, avgBitrate
                    let mut i = 4;
                    while i + 16 <= m.len() {
                        if &m[i..i + 4] == b"btrt" && u32::from_be_bytes(m[i - 4..i].try_into().unwrap()) == 20 {
                            let max = [m[i + 8], m[i + 9], m[i + 10], m[i + 11]];
                            let avg = u32::from_be_bytes(m[i + 12..i + 16].try_into().unwrap());
                            if avg == 0 && max != [0; 4] {
                                out.push((pos + h + (i + 12) as u64, max));
                            }
                            i += 16;
                        } else { i += 1; }
                    }
                }
            }
            break;
        }
        pos += size;
    }
    if !out.is_empty() {
        crate::recorder::log_line(&format!("media: {} - average bitrate 0 in the file, the player gets the maximum instead", 
            std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    }
    cache.lock().unwrap().insert(path.to_string(), (len, mtime, out.clone()));
    let _ = f.seek(SeekFrom::Start(0));
    out
}

/// Linux cloud clips: WebKitGTK plays them from here instead of straight from rclone's server (streaming straight
/// from there froze the whole window, chat 23). Ranges are passed through; the start of the file gets the same
/// average-bitrate fix as local clips (btrt avg 0 -> max, see btrt_fix).
fn proxy(url: &str, range: Option<String>, head: bool, mut w: TcpStream, cors: &str) -> std::io::Result<()> {
    let rest = url.trim_start_matches("http://");
    let (host, path) = match rest.find('/') { Some(i) => (&rest[..i], &rest[i..]), None => (rest, "/") };
    let t0 = std::time::Instant::now();
    let mut up = loop {
        match TcpStream::connect(host) {
            Ok(c) => break c,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused && t0.elapsed().as_secs() < 20 => std::thread::sleep(std::time::Duration::from_millis(250)),
            Err(e) => {
                crate::recorder::log_line(&format!("media: cloud stream unreachable: {e}"));
                return w.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
        }
    };
    let mut req = format!("{} {path} HTTP/1.0\r\nHost: {host}\r\n", if head { "HEAD" } else { "GET" });
    if let Some(r) = &range { req += &format!("Range: {r}\r\n"); }
    req += "\r\n";
    up.write_all(req.as_bytes())?;
    let mut r = BufReader::with_capacity(1 << 20, up);
    let mut status = String::new();
    r.read_line(&mut status)?;
    let code = status.split_whitespace().nth(1).unwrap_or("502").to_string();
    let reason = status.splitn(3, ' ').nth(2).unwrap_or("").trim().to_string();
    let mut keep = String::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h.trim().is_empty() { break; }
        let l = h.to_ascii_lowercase();
        if ["content-length:", "content-range:", "accept-ranges:", "last-modified:"].iter().any(|k| l.starts_with(k)) { keep += h.trim_end(); keep += "\r\n"; }
    }
    if code != "200" && code != "206" {
        crate::recorder::log_line(&format!("media: cloud stream answered {code} {reason}"));
    }
    w.write_all(format!("HTTP/1.1 {code} {reason}\r\nContent-Type: video/mp4\r\n{keep}{cors}Cache-Control: no-cache\r\nConnection: close\r\n\r\n").as_bytes())?;
    if head { return Ok(()); }
    // the first MB (the header of a faststart MP4 sits there): fix a 0 average bitrate, then pass the rest through
    let mut first = Vec::with_capacity(1 << 20);
    let mut buf = vec![0u8; 256 * 1024];
    while first.len() < 1 << 20 {
        let k = r.read(&mut buf)?;
        if k == 0 { break; }
        first.extend_from_slice(&buf[..k]);
    }
    let mut i = 4;
    while i + 16 <= first.len() {
        if &first[i..i + 4] == b"btrt" && first[i - 4..i] == [0, 0, 0, 20] && first[i + 12..i + 16] == [0; 4] && first[i + 8..i + 12] != [0; 4] {
            let max = [first[i + 8], first[i + 9], first[i + 10], first[i + 11]];
            first[i + 12..i + 16].copy_from_slice(&max);
            i += 16;
        } else { i += 1; }
    }
    w.write_all(&first)?;
    std::io::copy(&mut r, &mut w)?;
    Ok(())
}
