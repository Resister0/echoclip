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

/// a free name in `dir`: "<stem>.<ext>", "<stem> (2).<ext>", ...
fn free_name(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    (1..).map(|n| dir.join(if n == 1 { format!("{stem}.{ext}") } else { format!("{stem} ({n}).{ext}") })).find(|p| !p.exists()).unwrap()
}

/// work on a thread, a toast with the result
fn job(a: &AppWindow, busy: &str, f: impl FnOnce() -> Result<String, String> + Send + 'static) {
    let pb = prog_box(busy);
    prog_set(pb, None, None, Some(-1.0));
    let w = a.as_weak();
    std::thread::spawn(move || {
        let r = f();
        let _ = slint::invoke_from_event_loop(move || {
            prog_done(pb, 300);
            if let Some(a) = w.upgrade() {
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
            let copied = crate::copy::clipboard_file(&out).is_ok();
            Ok(format!("Frame saved to Screenshots{}", if copied { " and copied" } else { "" }))
        }),
        // Use as thumbnail: <stem>.jpg + <stem>.thumbt (the moment, for share previews) in .thumbs (Echo's set_thumb)
        "thumb" => {
            let th = thumbs.to_path_buf();
            job(a, "Setting the thumbnail…", move || {
                let _ = std::fs::create_dir_all(&th);
                run_ff(&ff, &["-ss", &format!("{pos:.3}")], &src, &["-frames:v", "1", "-vf", "scale=480:-2", "-q:v", "4"], &th.join(format!("{stem}.jpg")))
                    .map_err(|_| "Couldn't grab that frame".to_string())?;
                let _ = std::fs::write(th.join(format!("{stem}.thumbt")), format!("{pos:.3}"));
                Ok("Thumbnail set to this moment".into())
            });
        }
        // MP3 (Echo: <clips>/Audio/<name>.mp3; WAV = 16-bit 48 kHz for sound effects)
        "audio" | "wav" => {
            let wav = what == "wav";
            job(a, if wav { "Saving the sound as WAV…" } else { "Saving the sound as MP3…" }, move || {
                let dir = root.join("Audio");
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let out = dir.join(format!("{stem}.{}", if wav { "wav" } else { "mp3" }));
                let enc: &[&str] = if wav { &["-map", "0:a:0", "-vn", "-c:a", "pcm_s16le", "-ar", "48000"] } else { &["-map", "0:a:0", "-vn", "-c:a", "libmp3lame", "-b:a", "192k"] };
                run_ff(&ff, &[], &src, enc, &out).map_err(|_| "Couldn't save the sound (does this clip have any?)".to_string())?;
                Ok(format!("Saved to Audio as {}", out.file_name().unwrap_or_default().to_string_lossy()))
            });
        }
        // GIF of the trim selection (or the first 30 s) into <clips>/GIFs, shrinking until it's under ~10 MB
        "gif" => job(a, "Making a GIF…", move || {
            let len = (s1 - s0).min(30.0);
            if len < 0.3 {
                return Err("The selection is too short for a GIF".into());
            }
            // William (9 Nov): Echo's OWN make_gif (same sizes / palette) - it adds the Echo watermark like the old app
            if let Some(rec) = crate::rec_core::rec() {
                let out = rec.make_gif(&src.to_string_lossy(), s0, s1).map_err(|e| format!("Couldn't make a GIF: {e}"))?;
                let copied = crate::copy::clipboard_file(Path::new(&out)).is_ok();
                return Ok(format!("GIF saved to GIFs{}", if copied { " and copied" } else { "" }));
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
                    let copied = crate::copy::clipboard_file(&out).is_ok();
                    return Ok(format!("GIF saved to GIFs{}", if copied { " and copied" } else { "" }));
                }
            }
            let _ = std::fs::remove_file(&tmp);
            Err("Couldn't make a GIF of this clip".into())
        }),
        "replace" => {
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

/// Replace: the trimmed part over the original. mpv lets go of the file first (Windows keeps it locked otherwise),
/// the card picture + length are made again, the player reopens the NEW file (no stale picture - checklist W22).
fn replace(a: &AppWindow, p: &Rc<RefCell<Player>>, ff: PathBuf, src: PathBuf, thumbs: PathBuf, s0: f64, s1: f64) {
    let exact = a.global::<Play>().get_exact();
    let pl = a.global::<Play>();
    pl.set_busy(true);
    let ext = src.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_else(|| "mp4".into());
    let tmp = src.with_extension(format!("replace.{ext}"));
    let pb = prog_box("Replacing the clip with the trimmed part…");
    prog_set(pb, None, None, Some(-1.0));
    let w = a.as_weak();
    PL.with(|x| *x.borrow_mut() = Some(p.clone()));
    std::thread::spawn(move || {
        let tail: Vec<String> = if exact {
            ["-t", &format!("{:.3}", s1 - s0), "-map", "0:v:0", "-map", "0:a?", "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-c:a", "copy"].iter().map(|s| s.to_string()).collect()
        } else {
            ["-t", &format!("{:.3}", s1 - s0), "-map", "0:v:0", "-map", "0:a?", "-c", "copy", "-avoid_negative_ts", "make_zero"].iter().map(|s| s.to_string()).collect()
        };
        let tail_ref: Vec<&str> = tail.iter().map(|s| s.as_str()).collect();
        let r = run_ff(&ff, &["-ss", &format!("{s0:.3}")], &src, &tail_ref, &tmp);
        let _ = slint::invoke_from_event_loop(move || {
            let Some(a) = w.upgrade() else { return };
            if let Err(e) = r {
                let _ = std::fs::remove_file(&tmp);
                prog_done(pb, 300);
                a.global::<Play>().set_busy(false);
                toast(&a, format!("Replace failed: {e}"));
                return;
            }
            // mpv lets go of the old file
            with_mpv(|m| m.command(&["stop"]));
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
                if res.is_ok() {
                    let stem = src.file_stem().unwrap_or_default().to_string_lossy().to_string();
                    let _ = run_ff(&ff, &["-ss", "0.5"], &src, &["-frames:v", "1", "-vf", "scale=480:-2", "-q:v", "4"], &thumbs.join(format!("{stem}.jpg")));
                    if let Some(d) = crate::pl_media::duration(&ff, &src) {
                        let _ = std::fs::write(thumbs.join(format!("{stem}.dur")), format!("{d:.3}"));
                    }
                    for x in ["strip.jpg", "scrub.jpg", "marks.json"] {
                        let _ = std::fs::remove_file(thumbs.join(format!("{stem}.{x}")));
                    }
                } else {
                    let _ = std::fs::remove_file(&tmp);
                }
                let _ = slint::invoke_from_event_loop(move || {
                    prog_done(pb, 300);
                    let Some(a) = w2.upgrade() else { return };
                    let pl = a.global::<Play>();
                    pl.set_busy(false);
                    pl.set_trimming(false);
                    pl.set_t0(0.0);
                    pl.set_t1(1.0);
                    with_mpv(|m| {
                        m.set("ab-loop-a", "no");
                        m.set("ab-loop-b", "no");
                        m.command(&["loadfile", &src.to_string_lossy(), "replace", "-1", "pause=no"]);
                    });
                    match res {
                        Ok(()) => toast(&a, "Replaced - the clip is now just the trimmed part"),
                        Err(e) => toast(&a, format!("Couldn't replace the clip (it's open in another program?) {e}")),
                    }
                });
            });
        });
    });
}
