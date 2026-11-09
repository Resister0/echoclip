//! The 60-picture "scrub sheet" of a clip (10 x 6 frames, Echo's <stem>.scrub.jpg): hover flipbook on the cards and
//! the seek-bar preview in the player. Echo's own sheet is used when it's there and newer than the clip; otherwise
//! the prototype makes one with ffmpeg (same command as recorder.rs scrub_sheet) in ITS OWN cache folder - it never
//! writes into Echo's .thumbs. Sheets are decoded on a worker thread and scaled to 320x180 per picture
//! (3200x1080 = ~10 MB as pixels instead of ~41 MB at full size).
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
    if let Ok(p) = std::env::var("ECHO_FFMPEG") {
        return Some(PathBuf::from(p));
    }
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
    let mut c = Command::new("ffmpeg");
    hide(&mut c);
    c.arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().ok().filter(|s| s.success()).map(|_| PathBuf::from("ffmpeg"))
}

fn hide(_c: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        _c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
}

fn own_cache(clip: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    clip.hash(&mut h);
    let dir = std::env::temp_dir().join("echo-proto-sheets");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("{:016x}.jpg", h.finish()))
}

fn make(job: &Job, out: &Path) -> Result<(), String> {
    if job.dur <= 0.0 {
        return Err("clip length unknown".into());
    }
    let ff = ffmpeg().ok_or("ffmpeg not found")?;
    let n = 60;
    let d = job.dur;
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

fn load(job: &Job) -> Done {
    let t0 = std::time::Instant::now();
    let fail = |note: String| Done { key: job.key, px: None, tw: 0, th: 0, note };
    let path = if fresh(&job.echo_sheet, &job.clip) {
        job.echo_sheet.clone()
    } else {
        let own = own_cache(&job.clip);
        if !fresh(&own, &job.clip) {
            if let Err(e) = make(job, &own) {
                return fail(format!("no seek pictures: {e}"));
            }
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
    Done { key: job.key, px: Some(px), tw, th, note: format!("seek pictures in {} ms", t0.elapsed().as_millis()) }
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
