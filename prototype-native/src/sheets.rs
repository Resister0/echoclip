//! The 60-picture "scrub sheet" of a clip (10 x 6 frames, Echo's <stem>.scrub.jpg): hover flipbook on the cards and
//! the seek-bar preview in the player. The sheet in the clip's .thumbs is used when it's there and newer than the
//! clip; otherwise one is made with ffmpeg (same command as recorder.rs scrub_sheet) and kept THERE, Windows and Linux
//! alike (10 Nov 2026: it went to the temp folder before = /tmp on Linux, RAM, made again after every reboot).
//! An edited clip is newer than its sheet = made again; delete / rename take it along (lib_thumbs.rs). Cloud-only
//! clips (a stream) and folders Echo can't write to use a cache folder of their own. Sheets are decoded on a worker
//! thread and scaled to 320x180 per picture (3200x1080 = ~10 MB as pixels instead of ~41 MB at full size).
use slint::{Rgb8Pixel, SharedPixelBuffer};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::time::SystemTime;

pub const TILE_W: u32 = 320;

pub struct Job {
    pub key: usize, // clip index
    pub clip: PathBuf,
    pub echo_sheet: PathBuf,
    pub dur: f64,
}

pub struct Done {
    pub key: usize,
    pub px: Option<SharedPixelBuffer<Rgb8Pixel>>,
    /// Linux: the same pictures as RGBA, made here on the worker. Slint's OpenGL renderer uploads RGB with 1-byte
    /// row alignment = Mesa converts all ~10 MB on the CPU, on the UI thread: a visible hitch every time the
    /// seek-bar pictures appeared (9 Nov 2026, Nobara RX 9070 XT). RGBA takes the driver's fast path.
    pub rgba: Option<SharedPixelBuffer<slint::Rgba8Pixel>>,
    pub tw: u32,
    pub th: u32,
    pub note: String,
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn fresh(sheet: &Path, clip: &Path) -> bool {
    match (mtime(sheet), mtime(clip)) {
        (Some(a), Some(b)) => a >= b,
        _ => false,
    }
}

/// ffmpeg: ECHO_FFMPEG, then the copy Echo downloaded (%APPDATA%\Clipper\ffmpeg), then PATH
pub fn ffmpeg() -> Option<PathBuf> {
    // looked up ONCE: the PATH check runs 'ffmpeg -version' (~50 ms on Linux) and callers sit on the UI thread -
    // every clip open stuttered on Linux (9 Nov 2026, after the Windows-exe fix made Linux reach the PATH check)
    static FF: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    // only a FOUND ffmpeg is kept: on a first Windows run Echo may still be downloading it
    if let Some(f) = FF.get() {
        return f.clone();
    }
    // r2-threads (L53): "not found" is kept for 30 s too - every caller (hover video, player, editor) ran the
    // search + 'ffmpeg -version' again
    static MISS: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    if MISS.lock().map_or(false, |m| m.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(30))) {
        return None;
    }
    let f = find_ffmpeg();
    if f.is_some() {
        let _ = FF.set(f.clone());
    } else if let Ok(mut m) = MISS.lock() {
        *m = Some(std::time::Instant::now());
    }
    f
}

fn find_ffmpeg() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ECHO_FFMPEG") {
        return Some(PathBuf::from(p));
    }
    // Windows only: Echo's downloaded ffmpeg.exe. On Linux APPDATA = ~/.config, and a synced Clipper folder can hold
    // the Windows ffmpeg.exe -> 'Permission denied' on spawn = no seek pictures / editor renders (9 Nov 2026, Nobara)
    #[cfg(windows)]
    if let Ok(a) = std::env::var("APPDATA") {
        fn find(d: &Path) -> Option<PathBuf> {
            for e in std::fs::read_dir(d).ok()?.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if let Some(f) = find(&p) {
                        return Some(f);
                    }
                } else if p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("ffmpeg.exe")) {
                    return Some(p);
                }
            }
            None
        }
        if let Some(f) = find(&PathBuf::from(a).join("Clipper").join("ffmpeg")) {
            return Some(f);
        }
    }
    // Linux (gaps-linux row 38): next to Echo, PATH, then the usual folders a menu start leaves out of PATH
    #[cfg(target_os = "linux")]
    let found = crate::linux::find_ffmpeg();
    #[cfg(not(target_os = "linux"))]
    let found = {
        let mut c = Command::new("ffmpeg");
        hide(&mut c);
        c.arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().ok().filter(|s| s.success()).map(|_| PathBuf::from("ffmpeg"))
    };
    found
}

fn hide(_c: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        _c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
}

/// gaps-cloud (row 5): a cloud-only clip comes in as its streaming address (cloud::play_path)
fn remote(clip: &Path) -> bool {
    clip.to_string_lossy().starts_with("http")
}

/// a stream's length from ffmpeg's "Duration: 00:00:31.53" line (cloud clips whose length isn't known yet)
fn probe_dur(clip: &Path) -> f64 {
    let Some(ff) = ffmpeg() else { return 0.0 };
    let mut c = Command::new(ff);
    hide(&mut c);
    let Ok(o) = c.args(["-hide_banner", "-i"]).arg(clip).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).output() else { return 0.0 };
    let err = String::from_utf8_lossy(&o.stderr);
    let Some(t) = err.split("Duration: ").nth(1).and_then(|s| s.split(',').next()) else { return 0.0 };
    t.trim().split(':').filter_map(|x| x.parse::<f64>().ok()).fold(0.0, |acc, x| acc * 60.0 + x)
}

/// the cache folder for sheets that can't go into .thumbs: Windows %TEMP% (kept), Linux ~/.cache (not /tmp = RAM)
fn own_dir() -> PathBuf {
    #[cfg(not(windows))]
    {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")));
        if let Some(b) = base {
            return b.join("echo").join("sheets");
        }
    }
    std::env::temp_dir().join("echo-proto-sheets")
}

fn own_cache(clip: &Path) -> PathBuf {
    own_cache_in(&own_dir(), clip)
}

fn own_cache_in(dir: &Path, clip: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    // a stream: the clip's part of the address only (the streaming server's port changes between runs)
    let s = clip.to_string_lossy();
    if remote(clip) {
        s.splitn(4, '/').nth(3).unwrap_or(&s).hash(&mut h);
    } else {
        clip.hash(&mut h);
    }
    let _ = std::fs::create_dir_all(dir);
    dir.join(format!("{:016x}.jpg", h.finish()))
}

fn make(job: &Job, out: &Path) -> Result<(), String> {
    let d = if job.dur <= 0.0 && remote(&job.clip) { probe_dur(&job.clip) } else { job.dur };
    if d <= 0.0 {
        return Err("clip length unknown".into());
    }
    let ff = ffmpeg().ok_or("ffmpeg not found")?;
    let n = 60;
    let mut c = Command::new(ff);
    hide(&mut c);
    c.args(["-hide_banner", "-loglevel", "error", "-y", "-threads", "4"]);
    if d > 90.0 {
        c.args(["-discard", "nokey", "-skip_frame", "nokey"]);
    }
    let vf = format!("tpad=stop_mode=clone:stop_duration=5,fps={n}/{d:.3}:start_time=0,scale=640:360:force_original_aspect_ratio=decrease:flags=bilinear,pad=640:360:-1:-1,setsar=1,tile=10x6");
    let o = c
        .args(["-an", "-sn", "-dn", "-i"])
        .arg(&job.clip)
        .args(["-map", "0:v:0", "-vf", &vf, "-frames:v", "1", "-q:v", "4"])
        .arg(out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| e.to_string())?;
    if !o.status.success() || !out.exists() {
        let _ = std::fs::remove_file(out);
        return Err(String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("ffmpeg failed").to_string());
    }
    Ok(())
}

/// a local clip without a fresh sheet: make it (or take the one an older Echo left in the temp folder) and keep it in
/// .thumbs as <stem>.scrub.jpg. None = .thumbs can't be written (read-only folder): the caller uses its own cache.
/// Ok((path, how)): how = "made" (ffmpeg ran) / "moved" (the temp-folder copy).
fn into_thumbs(job: &Job) -> Option<Result<(PathBuf, &'static str), String>> {
    let out = &job.echo_sheet;
    std::fs::create_dir_all(out.parent()?).ok()?;
    // written under another name first: a half-written sheet must never look fresh
    let tmp = out.with_extension("part.jpg");
    std::fs::File::create(&tmp).ok()?;
    let old = own_cache_in(&std::env::temp_dir().join("echo-proto-sheets"), &job.clip);
    let moved = fresh(&old, &job.clip) && std::fs::copy(&old, &tmp).is_ok();
    let made = if moved { Ok(()) } else { make(job, &tmp) };
    let r = made.and_then(|_| std::fs::rename(&tmp, out).map_err(|e| e.to_string()));
    let _ = std::fs::remove_file(&tmp);
    if r.is_ok() {
        let _ = std::fs::remove_file(&old); // the temp-folder copy isn't needed any more
        // a clip dated in the future (clock / synced folder): give the sheet the clip's date, or it's never "fresh"
        // and made again on every open
        if !fresh(out, &job.clip) {
            if let (Some(t), Ok(f)) = (mtime(&job.clip), std::fs::File::options().write(true).open(out)) {
                let _ = f.set_modified(t);
            }
        }
    }
    Some(r.map(|_| (out.clone(), if moved { "moved" } else { "made" })))
}

fn load(job: &Job) -> Done {
    let t0 = std::time::Instant::now();
    let fail = |note: String| Done { key: job.key, px: None, rgba: None, tw: 0, th: 0, note };
    // a stream has no file date: a sheet made before (Echo's, from when the clip was on this PC, or ours) is used as is
    let far = remote(&job.clip);
    let mut how = "reused";
    let path = if fresh(&job.echo_sheet, &job.clip) || (far && job.echo_sheet.exists()) {
        job.echo_sheet.clone()
    } else if let Some(r) = (!far).then(|| into_thumbs(job)).flatten() {
        match r {
            Ok((p, h)) => {
                how = h;
                p
            }
            Err(e) => return fail(format!("no seek pictures: {e}")),
        }
    } else {
        let own = own_cache(&job.clip);
        if !(fresh(&own, &job.clip) || (far && own.exists())) {
            if let Err(e) = make(job, &own) {
                return fail(format!("no seek pictures: {e}"));
            }
            how = "made";
        }
        own
    };
    let Ok(img) = image::open(&path) else { return fail("seek pictures unreadable".into()) };
    let (w, h) = (img.width(), img.height());
    let tw0 = w / 10;
    let th0 = h / 6;
    if tw0 == 0 || th0 == 0 {
        return fail("seek pictures: bad size".into());
    }
    let tw = TILE_W.min(tw0);
    let th = (th0 * tw / tw0).max(1);
    let small = image::imageops::resize(&img.to_rgb8(), tw * 10, th * 6, image::imageops::FilterType::Triangle);
    let px = SharedPixelBuffer::<Rgb8Pixel>::clone_from_slice(small.as_raw(), small.width(), small.height());
    let rgba = if cfg!(windows) {
        None
    } else {
        let mut b = SharedPixelBuffer::<slint::Rgba8Pixel>::new(small.width(), small.height());
        for (d, s) in b.make_mut_bytes().chunks_exact_mut(4).zip(small.as_raw().chunks_exact(3)) {
            d[..3].copy_from_slice(s);
            d[3] = 255;
        }
        Some(b)
    };
    Done { key: job.key, px: Some(px), rgba, tw, th, note: format!("{how} {} in {} ms", path.file_name().unwrap_or_default().to_string_lossy(), t0.elapsed().as_millis()) }
}

/// one worker: newest request first (older hover requests are dropped)
pub fn spawn(on_done: impl Fn(Done) + Send + 'static) -> Sender<Job> {
    let (tx, rx): (Sender<Job>, Receiver<Job>) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(mut job) = rx.recv() {
            // skip to the newest waiting request
            while let Ok(j) = rx.try_recv() {
                job = j;
            }
            on_done(load(&job));
        }
    });
    tx
}
