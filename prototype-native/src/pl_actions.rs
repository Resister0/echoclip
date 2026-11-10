//! Chat C: the player's buttons that used to be look-only (Echo's recorder.rs frame_png / set_thumb / audio_file /
//! make_gif + Replace). Everything runs ffmpeg on a worker thread; the result comes back as a toast.
use crate::{confirm_box, prog_box, prog_done, prog_set, toast, AppWindow, Play, Player};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::rc::Rc;

thread_local! {
    /// the player, for the Replace job's callbacks (an Rc can't travel through worker threads)
    static PL: RefCell<Option<Rc<RefCell<Player>>>> = const { RefCell::new(None) };
    /// gaps-player: open clip `idx` in the player like a fresh one (main.rs: sheet cache dropped + play(row)); false =
    /// it isn't in the current view
    static REOPEN: RefCell<Option<Rc<dyn Fn(usize) -> bool>>> = RefCell::new(None);
}
/// the player is open on this file
fn showing(src: &Path) -> bool {
    PL.with(|p| {
        p.borrow().as_ref().is_some_and(|p| {
            let p = p.borrow();
            p.open && p.clip.and_then(|i| crate::LIB.with(|l| l.borrow().as_ref().map(|l| l.clip(i).path.clone()))).as_deref() == Some(src)
        })
    })
}
/// main.rs hands over its "open this clip" (after `play` exists)
pub fn set_reopen(f: Rc<dyn Fn(usize) -> bool>) {
    REOPEN.with(|r| *r.borrow_mut() = Some(f));
}
/// open clip `idx` like a fresh one (false = not possible)
pub fn reopen(idx: usize) -> bool {
    let f = REOPEN.with(|r| r.borrow().clone());
    f.is_some_and(|f| f(idx))
}
fn with_mpv(f: impl FnOnce(&crate::mpv::Mpv)) {
    PL.with(|p| {
        if let Some(p) = p.borrow().as_ref() {
            if let Some(m) = p.borrow().mpv.as_ref() {
                f(m);
            }
        }
    });
}

fn run_ff(ff: &Path, args: &[&str], input: &Path, tail: &[&str], out: &Path) -> Result<(), String> {
    let o = crate::edrender::hidden_cmd(ff).args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).arg("-i").arg(input).args(tail).arg(out)
        .stdin(Stdio::null()).stdout(Stdio::null()).output().map_err(|e| e.to_string())?;
    if o.status.success() && out.exists() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("ffmpeg failed").to_string())
    }
}

/// gaps-player (row 35): Echo's trim_clip encoders (recorder.rs): AMF -> NVENC -> QSV -> libx264, at about the
/// source bitrate (4-80 Mb/s), AAC 192k. Linux skips AMF / QSV, and NVENC without libcuda (recorder.rs skip_enc). ⚠ Linux
fn venc_chain(src: &Path, dur: f64) -> Vec<Vec<String>> {
    let size = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0) as f64;
    let kbps = if dur > 0.0 { ((size * 8.0 / dur / 1000.0) as u64).clamp(4000, 80000) } else { 20000 };
    let b = format!("{kbps}k");
    let cuda = cfg!(windows)
        || ["/usr/lib64/libcuda.so.1", "/usr/lib/x86_64-linux-gnu/libcuda.so.1", "/usr/lib/libcuda.so.1", "/usr/lib64/nvidia/libcuda.so.1"].iter().any(|p| Path::new(p).exists());
    let mut v: Vec<Vec<&str>> = vec![];
    if cfg!(windows) {
        v.push(vec!["-c:v", "h264_amf", "-quality", "quality", "-b:v", &b, "-maxrate", &b]);
    }
    if cuda {
        v.push(vec!["-c:v", "h264_nvenc", "-preset", "p5", "-b:v", &b, "-maxrate", &b]);
    }
    if cfg!(windows) {
        v.push(vec!["-c:v", "h264_qsv", "-b:v", &b, "-maxrate", &b]);
    }
    v.push(vec!["-c:v", "libx264", "-preset", "veryfast", "-crf", "18"]);
    v.into_iter().map(|e| e.into_iter().map(String::from).collect()).collect()
}

/// Save copy / Replace: [s0, s1] of `src` into `out`. Cuts (`keep` = the parts that stay) or Exact = re-encode through
/// the encoder chain above; otherwise a stream copy (starts at the keyframe before s0). `dur` = the whole clip's length.
pub fn encode_trim(ff: &Path, src: &Path, s0: f64, s1: f64, exact: bool, keep: &[(f64, f64)], has_cuts: bool, dur: f64, out: &Path) -> Result<(), String> {
    if let Some(d) = out.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let (pre, mid): (Vec<String>, Vec<String>) = if has_cuts {
        let ex = crate::pl_cuts::select_expr(keep);
        (vec![], ["-map", "0:v:0", "-map", "0:a?", "-vf", &format!("select='{ex}',setpts=N/FRAME_RATE/TB"), "-af", &format!("aselect='{ex}',asetpts=N/SR/TB")].iter().map(|s| s.to_string()).collect())
    } else {
        (vec!["-ss".into(), format!("{s0:.3}")], ["-t", &format!("{:.3}", s1 - s0), "-map", "0:v:0", "-map", "0:a?"].iter().map(|s| s.to_string()).collect())
    };
    let pre: Vec<&str> = pre.iter().map(String::as_str).collect();
    // +faststart only where the muxer has it (Echo's clips are .mp4; a .mkv / .webm import keeps its own container)
    let fast: &[&str] = if out.extension().is_some_and(|e| matches!(e.to_string_lossy().to_lowercase().as_str(), "mp4" | "mov" | "m4v")) { &["-movflags", "+faststart"] } else { &[] };
    if !has_cuts && !exact {
        let mut tail: Vec<&str> = mid.iter().map(String::as_str).collect();
        tail.extend(["-c", "copy", "-avoid_negative_ts", "make_zero"]);
        tail.extend(fast);
        return run_ff(ff, &pre, src, &tail, out).inspect_err(|_| {
            let _ = std::fs::remove_file(out);
        });
    }
    let mut last = String::from("ffmpeg failed");
    for enc in venc_chain(src, dur) {
        let mut tail: Vec<&str> = mid.iter().map(String::as_str).collect();
        tail.extend(enc.iter().map(String::as_str));
        tail.extend(["-c:a", "aac", "-b:a", "192k"]);
        tail.extend(fast);
        match run_ff(ff, &pre, src, &tail, out) {
            Ok(()) if std::fs::metadata(out).is_ok_and(|m| m.len() > 0) => {
                crate::lib_net::log_line(&format!("trim: {} ({:.1}s) with {}", out.display(), s1 - s0, enc[1]));
                return Ok(());
            }
            Ok(()) => {}
            Err(e) => last = e,
        }
    }
    let _ = std::fs::remove_file(out);
    Err(last)
}

/// the GIF on the clipboard + Echo's toast
fn gif_msg(out: &Path) -> String {
    match crate::copy::clipboard_file(out) {
        Ok(()) => "GIF copied - saved in GIFs folder".into(),
        Err(e) => format!("GIF saved in GIFs folder ({e})"),
    }
}

/// a free name in `dir`: "<stem>.<ext>", "<stem> (2).<ext>", ...
fn free_name(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    (1..).map(|n| dir.join(if n == 1 { format!("{stem}.{ext}") } else { format!("{stem} ({n}).{ext}") })).find(|p| !p.exists()).unwrap()
}

/// work on a thread, a toast with the result
fn job(a: &AppWindow, busy: &str, f: impl FnOnce() -> Result<String, String> + Send + 'static) {
    job_then(a, busy, f, |_| {});
}

/// `job` + `done` on the UI thread when it ends (L14: the GIF clears Play.gif-busy)
fn job_then(a: &AppWindow, busy: &str, f: impl FnOnce() -> Result<String, String> + Send + 'static, done: impl FnOnce(&AppWindow) + Send + 'static) {
    let pb = prog_box(busy);
    prog_set(pb, None, None, Some(-1.0));
    let w = a.as_weak();
    std::thread::spawn(move || {
        let r = f();
        let _ = slint::invoke_from_event_loop(move || {
            prog_done(pb, 300);
            if let Some(a) = w.upgrade() {
                done(&a);
                match r {
                    Ok(m) | Err(m) => toast(&a, m),
                }
            }
        });
    });
}

/// the player's act(): true = handled here. `clip` = the open clip, `thumbs` = <clips>/.thumbs
pub fn act(a: &AppWindow, p: &Rc<RefCell<Player>>, clip: &Path, thumbs: &Path, what: &str) -> bool {
    let Some(ff) = crate::sheets::ffmpeg() else {
        if matches!(what, "frame" | "thumb" | "audio" | "wav" | "gif" | "replace") {
            toast(a, "This needs ffmpeg (Echo's copy wasn't found)");
            return true;
        }
        return false;
    };
    let pos = p.borrow().mpv.as_ref().and_then(|m| m.get_f64("time-pos")).unwrap_or(0.0).max(0.0);
    let dur = p.borrow().duration.max(0.0);
    let pl = a.global::<Play>();
    let (s0, s1) = if pl.get_trimming() && dur > 0.0 { (pl.get_t0() as f64 * dur, pl.get_t1() as f64 * dur) } else { (0.0, dur) };
    let root = thumbs.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = clip.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let src = clip.to_path_buf();
    match what {
        // P: the frame as a PNG in <clips>/Screenshots, also on the clipboard (Echo's frame_png command)
        "frame" => job(a, "Saving the frame…", move || {
            let dir = root.join("Screenshots");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let out = dir.join(format!("{stem} @{pos:.1}s.png"));
            run_ff(&ff, &["-ss", &format!("{pos:.3}")], &src, &["-frames:v", "1", "-update", "1"], &out).map_err(|_| "Couldn't grab this frame".to_string())?;
            // Echo: "Frame copied - saved in Screenshots"
            match crate::copy::clipboard_file(&out) {
                Ok(()) => Ok("Frame copied - saved in Screenshots".into()),
                Err(e) => Ok(format!("Frame saved in Screenshots ({e})")),
            }
        }),
        // Use as thumbnail: <stem>.jpg + <stem>.thumbt (the moment, for share previews) in .thumbs (Echo's set_thumb)
        // chat 23 (Linux): the files were written but the card kept its cached picture -> "does nothing". Now the
        // card (+ its 720 px copy) and the player's poster are reloaded, and an Echo Cloud share page gets the new still.
        "thumb" => {
            let th = thumbs.to_path_buf();
            // a cloud-only clip: grab the frame over Echo's streaming server (UI thread: play_path reads the library)
            let input = crate::cloud::play_path(&src);
            let rel = src.strip_prefix(&root).map(|r| r.to_string_lossy().replace('\\', "/")).unwrap_or_default();
            let pb = prog_box("Setting the thumbnail…");
            prog_set(pb, None, None, Some(-1.0));
            let w = a.as_weak();
            let clip = src.clone();
            std::thread::spawn(move || {
                let jpg = th.join(format!("{stem}.jpg"));
                let _ = std::fs::create_dir_all(&th);
                let r = run_ff(&ff, &["-ss", &format!("{pos:.3}")], &input, &["-frames:v", "1", "-vf", "scale=480:-2", "-q:v", "4"], &jpg)
                    .map_err(|_| "Couldn't grab that frame".to_string());
                let mut px = None;
                let mut cloud_msg = String::new();
                if r.is_ok() {
                    let _ = std::fs::write(th.join(format!("{stem}.thumbt")), format!("{pos:.3}"));
                    let hi = crate::library::card_hi_path(&jpg);
                    let _ = std::fs::remove_file(&hi);
                    let hi_ok = crate::library::make_card_hi(&input, &jpg, &hi);
                    px = crate::library::decode(if hi_ok { &hi } else { &jpg });
                    cloud_msg = cloud_share_thumb(&ff, &input, &th, &rel, pos);
                }
                let _ = slint::invoke_from_event_loop(move || {
                    prog_done(pb, 300);
                    let Some(a) = w.upgrade() else { return };
                    if let Some(px) = px {
                        let img = slint::Image::from_rgb8(px);
                        if let Some(l) = crate::LIB.with(|l| l.borrow().clone()) {
                            if let Some(idx) = l.index_of_path(&clip) {
                                l.thumb_ready(idx, img.clone());
                            }
                        }
                        a.global::<Play>().set_poster(img);
                    }
                    match r {
                        Ok(()) => toast(&a, format!("Thumbnail updated{cloud_msg}")),
                        Err(e) => toast(&a, e),
                    }
                });
            });
        }
        // MP3 (Echo: <clips>/Audio/<name>.mp3; WAV = 16-bit 48 kHz for sound effects)
        "audio" | "wav" => {
            let wav = what == "wav";
            // gaps-player (row 13): Echo's texts, and the file goes on the clipboard too (audio_mp3 / audio_wav)
            job(a, "Exporting sound…", move || {
                let dir = root.join("Audio");
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let out = dir.join(format!("{stem}.{}", if wav { "wav" } else { "mp3" }));
                let enc: &[&str] = if wav { &["-map", "0:a:0", "-vn", "-c:a", "pcm_s16le", "-ar", "48000"] } else { &["-map", "0:a:0", "-vn", "-c:a", "libmp3lame", "-b:a", "192k"] };
                run_ff(&ff, &[], &src, enc, &out).map_err(|_| "Couldn't save the sound (does this clip have any?)".to_string())?;
                let what = if wav { "WAV" } else { "MP3" };
                match crate::copy::clipboard_file(&out) {
                    Ok(()) => Ok(format!("{what} copied - saved in the Audio folder")),
                    Err(e) => Ok(format!("{what} saved in the Audio folder ({e})")),
                }
            });
        }
        // GIF of the trim selection (or the first 30 s) into <clips>/GIFs, shrinking until it's under ~10 MB
        // Echo: a selection over 30 s says so first; the result is "GIF copied - saved in GIFs folder"
        // L14: Echo's trim.busy - not while a GIF / Save copy / Replace runs; GIF greyed out while it works
        "gif" if pl.get_gif_busy() || pl.get_busy() => {}
        "gif" => {
            pl.set_gif_busy(true);
            job_then(a, if s1 - s0 > 30.0 { "GIFs are limited to 30 s - using the first 30 s" } else { "Making GIF…" }, move || {
            let len = (s1 - s0).min(30.0);
            if len < 0.3 {
                return Err("The selection is too short for a GIF".into());
            }
            // William (9 Nov): Echo's OWN make_gif (same sizes / palette) - it adds the Echo watermark like the old app
            if let Some(rec) = crate::rec_core::rec() {
                let out = rec.make_gif(&src.to_string_lossy(), s0, s1).map_err(|e| format!("Couldn't make a GIF: {e}"))?;
                return Ok(gif_msg(Path::new(&out)));
            }
            let dir = root.join("GIFs");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let out = free_name(&dir, &stem, "gif");
            let tmp = dir.join(format!("{stem}.giftmp"));
            for (w, fps) in [(640, 20), (480, 15), (360, 12), (270, 10)] {
                let vf = format!("[0:v]fps={fps},scale={w}:-2:flags=lanczos,split[a][b];[a]palettegen=max_colors=192:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle");
                if run_ff(&ff, &["-ss", &format!("{s0:.3}"), "-t", &format!("{len:.3}")], &src, &["-filter_complex", &vf, "-loop", "0", "-f", "gif"], &tmp).is_err() {
                    continue;
                }
                let size = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
                if size > 0 && (size <= 10 * 1024 * 1024 || w == 270) {
                    std::fs::rename(&tmp, &out).map_err(|e| e.to_string())?;
                    return Ok(gif_msg(&out));
                }
            }
            let _ = std::fs::remove_file(&tmp);
            Err("Couldn't make a GIF of this clip".into())
        }, |a| a.global::<Play>().set_gif_busy(false));
        }
        "replace" => {
            // L14: Echo's trim.busy - Replace would overwrite the file a Save copy / GIF is reading
            if pl.get_busy() || pl.get_gif_busy() {
                return true;
            }
            if !pl.get_trimming() || s1 - s0 < 0.3 {
                toast(a, "Drag the yellow ends first, then Replace keeps only that part");
                return true;
            }
            let name = stem.clone();
            let (p2, th) = (p.clone(), thumbs.to_path_buf());
            confirm_box(a, "Replace original?", &format!("\"{name}\" will be overwritten with the trimmed part. This can't be undone."), "Replace", move |a| {
                replace(a, &p2, ff, src, th, s0, s1);
            });
        }
        _ => return false,
    }
    true
}

/// Replace: the trimmed part (minus cuts made on the strip) over the original, re-encoded with Echo's encoder chain
/// when Exact is on or there are cuts. mpv lets go of the file first (Windows keeps it locked otherwise), Echo's
/// caches for the old file go, the card is reloaded, and the clip reopens like a fresh one (new length, new seek
/// pictures, no stale picture - checklist W22). Echo's toast: "Clip trimmed".
fn replace(a: &AppWindow, p: &Rc<RefCell<Player>>, ff: PathBuf, src: PathBuf, thumbs: PathBuf, s0: f64, s1: f64) {
    let pl = a.global::<Play>();
    if pl.get_busy() || pl.get_gif_busy() {
        return; // L14
    }
    let exact = pl.get_exact();
    let dur = p.borrow().duration.max(0.0);
    let keep = crate::pl_cuts::kept(s0, s1, &crate::pl_cuts::cuts_secs(&pl, dur));
    let has_cuts = keep.len() != 1 || (keep[0].0 - s0).abs() > 1e-3 || (keep[0].1 - s1).abs() > 1e-3;
    if keep.is_empty() {
        toast(a, "Nothing left - the cuts cover the whole selection");
        return;
    }
    pl.set_busy(true);
    let ext = src.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_else(|| "mp4".into());
    let stem = src.file_stem().unwrap_or_default().to_string_lossy().to_string();
    // in .thumbs (a dot folder): the library never sees the half-made file as a new clip; same drive = plain rename
    let _ = std::fs::create_dir_all(&thumbs);
    let tmp = thumbs.join(format!("{stem}.replace.{ext}"));
    let pb = prog_box(if exact || has_cuts { "Exporting (re-encoding)…" } else { "Exporting…" });
    prog_set(pb, None, None, Some(-1.0));
    let w = a.as_weak();
    PL.with(|x| *x.borrow_mut() = Some(p.clone()));
    std::thread::spawn(move || {
        let r = encode_trim(&ff, &src, s0, s1, exact, &keep, has_cuts, dur, &tmp);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(a) = w.upgrade() else { return };
            if let Err(e) = r {
                let _ = std::fs::remove_file(&tmp);
                prog_done(pb, 300);
                a.global::<Play>().set_busy(false);
                toast(&a, format!("Replace failed: {e}"));
                return;
            }
            // mpv lets go of the old file (only when the player still shows it - it may be closed or on another clip now)
            if showing(&src) {
                with_mpv(|m| m.command(&["stop"]));
            }
            let w2 = a.as_weak();
            std::thread::spawn(move || {
                // Windows: the file can stay locked a moment after mpv closes it (os error 32) - retry ~4 s
                let mut res = Err(String::new());
                for _ in 0..20 {
                    res = std::fs::rename(&tmp, &src).map_err(|e| e.to_string());
                    if res.is_ok() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                let mut px = None;
                if res.is_ok() {
                    // Echo's trim_clip(replace) drops these; the card picture + length are made again right away
                    for x in ["strip.jpg", "strip2.jpg", "strip3.jpg", "scrub.jpg", "marks.json", "thumbt", "a1.m4a", "a2.m4a"] {
                        let _ = std::fs::remove_file(thumbs.join(format!("{stem}.{x}")));
                    }
                    let jpg = thumbs.join(format!("{stem}.jpg"));
                    let _ = run_ff(&ff, &["-ss", "0.5"], &src, &["-frames:v", "1", "-vf", "scale=480:-2", "-q:v", "4"], &jpg);
                    if let Some(d) = crate::pl_media::duration(&ff, &src) {
                        let _ = std::fs::write(thumbs.join(format!("{stem}.dur")), format!("{d:.3}"));
                    }
                    let hi = crate::library::card_hi_path(&jpg);
                    let _ = std::fs::remove_file(&hi);
                    let hi_ok = crate::library::make_card_hi(&src, &jpg, &hi);
                    px = crate::library::decode(if hi_ok { &hi } else { &jpg });
                } else {
                    let _ = std::fs::remove_file(&tmp);
                }
                let _ = slint::invoke_from_event_loop(move || {
                    prog_done(pb, 300);
                    let Some(a) = w2.upgrade() else { return };
                    a.global::<Play>().set_busy(false);
                    let lib = crate::LIB.with(|l| l.borrow().clone());
                    let idx = lib.as_ref().and_then(|l| l.index_of_path(&src));
                    if let (Some(l), Some(idx)) = (lib.as_ref(), idx) {
                        if res.is_ok() {
                            l.reload(idx, thumbs.parent().unwrap_or(&thumbs));
                            l.refresh();
                        }
                        if let Some(px) = px {
                            l.thumb_ready(idx, slint::Image::from_rgb8(px));
                        }
                    }
                    // open it again like a fresh clip (resets trim / cuts, new seek pictures); else just reload mpv
                    let reopened = !showing(&src) || idx.is_some_and(reopen);
                    if !reopened {
                        let pl = a.global::<Play>();
                        pl.set_trimming(false);
                        pl.set_t0(0.0);
                        pl.set_t1(1.0);
                        crate::pl_cuts::reset(&pl);
                        with_mpv(|m| {
                            m.set("ab-loop-a", "no");
                            m.set("ab-loop-b", "no");
                            m.command(&["loadfile", &src.to_string_lossy(), "replace", "-1", "pause=no"]);
                        });
                    }
                    match res {
                        Ok(()) => toast(&a, "Clip trimmed"),
                        Err(e) => toast(&a, format!("Couldn't replace the clip (it's open in another program?) {e}")),
                    }
                });
            });
        });
    });
}

/// chat 23: the clip has an Echo Cloud share link (.thumbs/links.json, key "Echo/…"): send the new still to its share
/// page (Discord / browser preview). POST /share again with the same object + what the page showed, keeping the
/// link's remaining lifetime. Returns "" or " (+ share page)" / a short note for the toast. Worker thread.
fn cloud_share_thumb(ff: &Path, input: &Path, thumbs: &Path, rel: &str, pos: f64) -> String {
    use base64::Engine;
    if rel.is_empty() {
        return String::new();
    }
    let lp = thumbs.join("links.json");
    let Some(mut links) = std::fs::read_to_string(&lp).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) else { return String::new() };
    let Some(k) = links.as_object().and_then(|m| m.keys().find(|k| k.replace('\\', "/").eq_ignore_ascii_case(rel)).cloned()) else { return String::new() };
    let e = links[&k].clone();
    let key = e["key"].as_str().unwrap_or("").to_string();
    if key.is_empty() {
        return String::new(); // another cloud's link: no share page of ours to update
    }
    let token = crate::cloud::token();
    if token.is_empty() {
        return String::new();
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let exp = e["expires"].as_u64().unwrap_or(0);
    if exp != 0 && exp <= now {
        return String::new(); // the link is gone anyway
    }
    let days = if exp == 0 { 0 } else { (exp - now).div_ceil(86400).max(1) };
    let tmp = std::env::temp_dir().join(format!("echo_thumb_{}.jpg", std::process::id()));
    let mut b64 = None;
    for (vf, q) in [("scale='min(1280,iw)':-2", "5"), ("scale='min(640,iw)':-2", "6")] {
        if run_ff(ff, &["-ss", &format!("{pos:.3}")], input, &["-frames:v", "1", "-vf", vf, "-q:v", q], &tmp).is_ok() {
            if let Ok(b) = std::fs::read(&tmp) {
                let s = base64::engine::general_purpose::STANDARD.encode(&b);
                if s.len() < 400_000 {
                    b64 = Some(s);
                    break;
                }
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    let Some(b64) = b64 else { return " (share page not updated)".into() };
    let mut req = if e["meta"].is_object() { e["meta"].clone() } else { serde_json::json!({}) };
    req["path"] = serde_json::json!(key);
    req["days"] = serde_json::json!(days);
    req["thumb"] = serde_json::json!(b64);
    match crate::lib_net::ec_call("POST", "/share", &token, Some(req)) {
        Ok(j) => {
            if let Some(x) = j["expires"].as_u64() {
                links[&k]["expires"] = serde_json::json!(x);
                if let Ok(s) = serde_json::to_string(&links) {
                    let _ = std::fs::write(&lp, s);
                }
            }
            crate::lib_net::log_line(&format!("cloud: share page thumbnail updated for {rel}"));
            " (+ share page)".into()
        }
        Err(err) => {
            crate::lib_net::log_line(&format!("cloud: share page thumbnail for {rel} failed: {err}"));
            " (share page not updated)".into()
        }
    }
}

/// gaps-player (row 45): the side panel's Tags box suggests existing tags (Echo's datalist) and adds the typed tag
/// when it loses focus (Echo's onblur). Slint runs `changed has-focus` a moment later, when the player may already
/// show the next clip: the blur only adds when it's still the clip the text was typed for.
pub fn wire_tags(a: &AppWindow, p: Rc<RefCell<Player>>) {
    let typed_for: Rc<std::cell::Cell<Option<usize>>> = Rc::default();
    let pl = a.global::<Play>();
    {
        let (w, p, tf) = (a.as_weak(), p.clone(), typed_for.clone());
        pl.on_tag_typed(move |t| {
            let Some(a) = w.upgrade() else { return };
            let clip = p.borrow().clip;
            tf.set(clip);
            let q = t.trim().trim_start_matches('#').trim().to_lowercase();
            let mut hints: Vec<String> = vec![];
            if let (false, Some(idx), Some(lib)) = (q.is_empty(), clip, crate::LIB.with(|l| l.borrow().clone())) {
                let mine = lib.meta_of(&lib.clip(idx).name).tags;
                let all: std::collections::BTreeSet<String> = lib.meta.borrow().values().flat_map(|m| m.tags.iter().cloned()).collect();
                // starts-with first, then the rest that contain it
                let (mut a1, mut a2): (Vec<String>, Vec<String>) = all
                    .into_iter()
                    .filter(|x| !mine.contains(x) && x.to_lowercase().contains(&q) && x.to_lowercase() != q)
                    .partition(|x| x.to_lowercase().starts_with(&q));
                a1.append(&mut a2);
                hints = a1.into_iter().take(6).collect();
            }
            let m: Vec<slint::SharedString> = hints.iter().map(|s| s.as_str().into()).collect();
            a.global::<Play>().set_tag_hints(slint::ModelRc::new(slint::VecModel::from(m)));
        });
    }
    {
        let (w, tf) = (a.as_weak(), typed_for);
        pl.on_tag_blur(move |t| {
            let Some(a) = w.upgrade() else { return };
            let same = tf.take().is_some_and(|c| p.borrow().clip == Some(c));
            if same && !t.trim().is_empty() {
                a.global::<Play>().invoke_add_tag(t);
            }
        });
    }
}
