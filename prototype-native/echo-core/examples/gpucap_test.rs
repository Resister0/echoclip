//! Headless end-to-end test of the Windows recorder's GPU encoder loop (gpucap::encode, the code that ships) -
//! no window, no screen capture: raw NV12 frames from stdin stand in for the screen.
//!
//!   ffmpeg -i clip.mkv -f rawvideo -pix_fmt nv12 - | gpucap_test <w> <h> <out.h264> [refuse|keep] [max-mbps]
//!
//! `refuse` = the encoder "refuses" a lower max QP (the dark-scene boost must switch itself off and recording go on).
//! `keep` = normal (a placeholder so [max-mbps] can follow).
//! Settings as the default smart bitrate: 60 fps, max 20 Mbps (average 12; or [max-mbps]), quality 72, H.264, 2 s keyframes.

#[cfg(windows)]
fn main() {
    use echo_core::gpucap;
    use std::io::Write;
    use std::sync::atomic::Ordering;

    fn log(s: &str) {
        eprintln!("[{}] {s}", chrono::Local::now().format("%H:%M:%S%.3f"));
    }
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        eprintln!("usage: gpucap_test <w> <h> <out.h264> [refuse|keep] [max-mbps]   (raw NV12 frames on stdin)");
        std::process::exit(2);
    }
    let (w, h): (u32, u32) = (a[1].parse().expect("w"), a[2].parse().expect("h"));
    let out_path = a[3].clone();
    if a.get(4).map(|s| s.as_str()) == Some("refuse") {
        gpucap::TEST_REFUSE_MAX_QP.store(true, Ordering::Relaxed);
        log("test: the encoder will refuse max QP");
    }
    let name = format!(r"\\.\pipe\echo_gpucap_test_{}", std::process::id());
    let pipe = gpucap::create_pipe(&name).expect("pipe");
    // the reader (ffmpeg's place in the recorder): copy the stream into the file until the encoder closes the pipe
    let reader = std::thread::spawn(move || {
        let mut f = std::fs::File::create(&out_path).expect("out file");
        let mut p = loop {
            match std::fs::OpenOptions::new().read(true).open(&name) {
                Ok(p) => break p,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        };
        let n = std::io::copy(&mut p, &mut f).unwrap_or(0);
        f.flush().ok();
        n
    });
    let o = gpucap::Opts {
        fps: 60,
        bitrate_mbps: a.get(5).and_then(|s| s.parse().ok()).unwrap_or(20),
        gop: 120,
        target_h: 0,
        monitor: String::new(),
        codec: "h264".into(),
        smart_bitrate: true,
        smart_quality: 72,
        window: 0,
        cursor: false,
    };
    let t0 = std::time::Instant::now();
    let r = gpucap::encode_test(Box::new(std::io::stdin()), w, h, pipe, o, log);
    let bytes = reader.join().unwrap_or(0);
    log(&format!("test: {:?} after {:.1}s, {bytes} bytes, frames fed {}, late {}", r, t0.elapsed().as_secs_f64(),
        gpucap::FRAMES.load(Ordering::Relaxed), gpucap::LATE.load(Ordering::Relaxed)));
    std::process::exit(if r.is_ok() && bytes > 0 { 0 } else { 1 });
}

#[cfg(not(windows))]
fn main() {
    eprintln!("gpucap_test: Windows only");
}
