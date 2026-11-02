//! Tiny localhost HTTP server for clips. WebKitGTK (Linux) can't play videos from Tauri's
//! asset:// protocol, but plays them fine over http with Range requests.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::OnceLock;

static BASE: OnceLock<String> = OnceLock::new();

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
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h.trim().is_empty() { break; }
        let lh = h.to_ascii_lowercase();
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
    let ct = mime(&file);
    let f = if !file.is_empty() && ct.is_some() { std::fs::File::open(&file).ok() } else { None };
    let Some(mut f) = f else {
        return w.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    };
    let len = f.metadata()?.len();
    let (start, end, status) = match range {
        Some((a, b)) if a < len => (a, b.unwrap_or(len - 1).min(len - 1), "206 Partial Content"),
        Some(_) => return w.write_all(format!("HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{len}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()),
        None => (0, len.saturating_sub(1), "200 OK"),
    };
    let n = if len == 0 { 0 } else { end - start + 1 };
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Type: {}\r\nAccept-Ranges: bytes\r\nContent-Length: {n}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-cache\r\nConnection: close\r\n", ct.unwrap());
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
        w.write_all(&buf[..k])?;
        left -= k as u64;
    }
    Ok(())
}
