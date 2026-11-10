//! Echo's Discord Rich Presence - a port of src-tauri/src/discord.rs (that file needs Tauri's AppHandle, so it can't
//! go into echo-core unchanged). 9 Nov: the native app never showed "Playing Echo" at all.
//! "Playing Echo" + "Ready to clip" / "Clipping <game>" + "N clips saved", elapsed timer, Echo logo, "Get Echo"
//! button. Talks to the local Discord app over its IPC pipe (no crate, no login, no token):
//! Linux $XDG_RUNTIME_DIR/discord-ipc-N (+ Flatpak/Snap Discord paths), Windows \\.\pipe\discord-ipc-N.
//! Frame = op (u32 LE) + length (u32 LE) + JSON. op 0 = handshake, op 1 = command, op 2 = close.
//! Settings > App "Show in Discord" (discord_presence, on by default) + "Only while playing" (discord_game_only).
//! Nothing is sent when Discord isn't running; on quit Discord clears the status itself (pipe closes).
//! Started only by the Echo that records (rec_core::wire), so two copies never fight over the status.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Developer Portal app "Echo" (public id, not a secret). Art asset key "echo" = the logo.
const CLIENT_ID: &str = "1568425616208953395";

/// clips saved since Echo started (rec_core's save bumps it)
pub static CLIPS: AtomicU32 = AtomicU32::new(0);

#[cfg(unix)]
type Pipe = std::os::unix::net::UnixStream;
#[cfg(windows)]
type Pipe = std::fs::File;

fn connect() -> Option<Pipe> {
    #[cfg(unix)]
    {
        let base = std::env::var("XDG_RUNTIME_DIR").or_else(|_| std::env::var("TMPDIR")).unwrap_or_else(|_| "/tmp".into());
        // inside a Flatpak XDG_RUNTIME_DIR is the sandbox's; the host's is /run/user/<uid>
        let host = format!("/run/user/{}", unsafe { libc_getuid() });
        for dir in [base.clone(), host] {
            for sub in ["", "app/com.discordapp.Discord/", "app/dev.vencord.Vesktop/", "snap.discord/", ".flatpak/com.discordapp.Discord/xdg-run/"] {
                for i in 0..10 {
                    let p = format!("{dir}/{sub}discord-ipc-{i}");
                    if let Ok(s) = Pipe::connect(&p) {
                        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                        let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
                        return Some(s);
                    }
                }
            }
        }
        None
    }
    #[cfg(windows)]
    {
        for i in 0..10 {
            if let Ok(f) = std::fs::OpenOptions::new().read(true).write(true).open(format!(r"\\.\pipe\discord-ipc-{i}")) {
                return Some(f);
            }
        }
        None
    }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

fn send(p: &mut Pipe, op: u32, body: &serde_json::Value) -> std::io::Result<serde_json::Value> {
    let data = body.to_string();
    let mut frame = Vec::with_capacity(8 + data.len());
    frame.extend_from_slice(&op.to_le_bytes());
    frame.extend_from_slice(&(data.len() as u32).to_le_bytes());
    frame.extend_from_slice(data.as_bytes());
    p.write_all(&frame)?;
    // every frame gets an answer; read it so the pipe never fills up
    let mut head = [0u8; 8];
    p.read_exact(&mut head)?;
    let rop = u32::from_le_bytes(head[0..4].try_into().unwrap_or([0; 4]));
    let len = u32::from_le_bytes(head[4..8].try_into().unwrap_or([0; 4])) as usize;
    if len > 1 << 20 {
        return Err(std::io::Error::other("answer too big"));
    }
    let mut buf = vec![0u8; len];
    p.read_exact(&mut buf)?;
    let v: serde_json::Value = serde_json::from_slice(&buf).unwrap_or_default();
    if rop == 2 {
        return Err(std::io::Error::other(format!("Discord closed the connection: {v}")));
    }
    if v["evt"] == "ERROR" {
        return Err(std::io::Error::other(format!("Discord said: {}", v["data"])));
    }
    Ok(v)
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// what Discord shows
fn activity(game: Option<&str>, since: u64) -> serde_json::Value {
    let n = CLIPS.load(Ordering::Relaxed);
    let mut a = serde_json::json!({
        "type": 0,
        "timestamps": { "start": since },
        "assets": { "large_image": "echo", "large_text": "Echo - instant replays and clips" },
        "buttons": [ { "label": "Get Echo", "url": "https://echoclip.link" } ],
    });
    match game {
        Some(g) => {
            a["details"] = format!("Clipping {g}").into();
            // member list shows "Clipping <game>" instead of "Echo"
            a["status_display_type"] = 2.into();
        }
        None => a["details"] = "Ready to clip".into(),
    }
    if n > 0 {
        a["state"] = format!("{n} clip{} saved", if n == 1 { "" } else { "s" }).into();
    }
    a
}

pub fn start() {
    let log = echo_core::recorder::log_line;
    std::thread::Builder::new()
        .name("discord".into())
        .spawn(move || {
            let started = now_ms();
            let mut pipe: Option<Pipe> = None;
            let mut last_sent = String::new();
            let mut last_try = Instant::now() - Duration::from_secs(60);
            let mut last_send = Instant::now() - Duration::from_secs(60);
            let mut cur_game: Option<String> = None;
            let mut game_since = started;
            let mut logged_missing = false;
            loop {
                std::thread::sleep(Duration::from_secs(2));
                let Some(rec) = crate::rec_core::rec() else { continue };
                let cfg = rec.settings();
                let game = rec.last_game.lock().ok().and_then(|g| g.as_ref().filter(|(_, t)| t.elapsed().as_secs() < 20).map(|(g, _)| g.clone()));
                // Settings > App "Only while playing": hide the status when no game is detected
                let on = cfg.discord_presence && (!cfg.discord_game_only || game.is_some());
                if game != cur_game {
                    game_since = if game.is_some() { now_ms() } else { started };
                    cur_game = game.clone();
                }
                if !on {
                    if let Some(mut p) = pipe.take() {
                        let _ = send(&mut p, 1, &serde_json::json!({ "cmd": "SET_ACTIVITY", "args": { "pid": std::process::id() }, "nonce": now_ms().to_string() }));
                        log("discord: status cleared");
                    }
                    last_sent.clear();
                    continue;
                }
                if pipe.is_none() {
                    if last_try.elapsed() < Duration::from_secs(20) {
                        continue;
                    }
                    last_try = Instant::now();
                    let Some(mut p) = connect() else {
                        if !logged_missing {
                            log("discord: Discord isn't running (no status shown)");
                            logged_missing = true;
                        }
                        continue;
                    };
                    match send(&mut p, 0, &serde_json::json!({ "v": 1, "client_id": CLIENT_ID })) {
                        Ok(v) => {
                            log(&format!("discord: connected (user {})", v["data"]["user"]["username"].as_str().unwrap_or("?")));
                            logged_missing = false;
                            pipe = Some(p);
                            last_sent.clear();
                        }
                        Err(e) => {
                            log(&format!("discord: handshake failed ({e})"));
                            continue;
                        }
                    }
                }
                let a = activity(cur_game.as_deref(), game_since);
                let key = a.to_string();
                // Discord allows ~5 updates / 20 s
                if key == last_sent || last_send.elapsed() < Duration::from_secs(5) {
                    continue;
                }
                let msg = serde_json::json!({ "cmd": "SET_ACTIVITY", "args": { "pid": std::process::id(), "activity": a }, "nonce": now_ms().to_string() });
                last_send = Instant::now();
                let Some(p) = pipe.as_mut() else { continue };
                match send(p, 1, &msg) {
                    Ok(_) => last_sent = key,
                    Err(e) => {
                        log(&format!("discord: update failed ({e}) - reconnecting"));
                        pipe = None;
                    }
                }
            }
        })
        .ok();
}
