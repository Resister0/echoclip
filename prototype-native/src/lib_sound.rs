//! Chat B: Echo's feedback sounds (recorder.rs sound_wav / play_sound): synthesised WAVs, or the user's own .wav
//! from settings "sound_files". Windows: PlaySoundW from winmm.dll (loaded at run time); Linux: pw-play / paplay.
//! The recorder (Chat A) can call `play(kind, &settings_json, false)` for saved / failed / mark / shot / stopped.
use serde_json::Value;

/// "saved" | "failed" | "mark" | "shot" | "stopped"
pub const KINDS: [(&str, &str, &str); 5] = [
    ("saved", "sound_on_save", "Clip saved"),
    ("failed", "sound_on_fail", "Failed"),
    ("mark", "sound_on_mark", "Bookmark"),
    ("shot", "sound_on_shot", "Screenshot"),
    ("stopped", "sound_on_stop", "Recorder stopped"),
];

fn sound_wav(kind: &str, volume: f32) -> Vec<u8> {
    const RATE: u32 = 44100;
    let notes: &[(f32, f32, bool)] = match kind {
        "saved" => &[(880.0, 0.07, false), (1318.5, 0.13, false)],
        "failed" => &[(311.0, 0.12, true), (220.0, 0.22, true)],
        "mark" => &[(1318.5, 0.06, false)],
        "shot" => &[(1046.5, 0.05, false), (1568.0, 0.09, false)],
        _ => &[(659.3, 0.12, false), (523.3, 0.12, false), (392.0, 0.28, false)],
    };
    let mut pcm: Vec<i16> = Vec::new();
    for &(f, secs, buzzy) in notes {
        let n = (secs * RATE as f32) as usize;
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            let env = (t / 0.005).min(1.0) * (1.0 - i as f32 / n as f32).powf(1.6);
            let w = (t * f * std::f32::consts::TAU).sin();
            let w = if buzzy { (w * 3.0).tanh() * 0.6 } else { w };
            pcm.push((w * env * volume.clamp(0.0, 1.0) * 0.5 * i16::MAX as f32) as i16);
        }
    }
    let data_len = (pcm.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&RATE.to_le_bytes());
    v.extend_from_slice(&(RATE * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

/// Play `kind` if it's switched on in `cfg` (Echo's settings.json as JSON). force = Preview button.
pub fn play(kind: &str, cfg: &serde_json::Map<String, Value>, force: bool) {
    let Some((_, key, _)) = KINDS.iter().find(|k| k.0 == kind) else { return };
    let on = cfg.get(*key).and_then(|v| v.as_bool()).unwrap_or(true);
    let vol = cfg.get("sound_volume").and_then(|v| v.as_f64()).unwrap_or(0.6) as f32;
    if !(force || on) || vol <= 0.0 {
        return;
    }
    let custom = cfg
        .get("sound_files")
        .and_then(|m| m.get(kind))
        .and_then(|f| f.as_str())
        .filter(|f| !f.is_empty() && std::path::Path::new(f).is_file())
        .map(|s| s.to_string());
    let wav = sound_wav(kind, vol);
    std::thread::spawn(move || play_now(custom, wav));
}

#[cfg(windows)]
fn play_now(custom: Option<String>, wav: Vec<u8>) {
    // PlaySoundW(pszSound, hmod, fdwSound): SND_MEMORY 0x4, SND_FILENAME 0x20000, SND_NODEFAULT 0x2, SND_SYNC 0
    unsafe {
        let Ok(lib) = libloading::Library::new("winmm.dll") else { return };
        let Ok(f) = lib.get::<unsafe extern "system" fn(*const u16, isize, u32) -> i32>(b"PlaySoundW\0") else { return };
        match custom {
            Some(p) => {
                let w: Vec<u16> = p.encode_utf16().chain(std::iter::once(0)).collect();
                f(w.as_ptr(), 0, 0x20000 | 0x2);
            }
            None => {
                f(wav.as_ptr() as *const u16, 0, 0x4 | 0x2);
            }
        }
    }
}

#[cfg(not(windows))]
fn play_now(custom: Option<String>, wav: Vec<u8>) {
    use std::process::{Command, Stdio};
    let f = match custom {
        Some(f) => std::path::PathBuf::from(f),
        None => {
            let f = std::env::temp_dir().join(format!("echo_sound_{}_{}.wav", std::process::id(), wav.len()));
            if std::fs::write(&f, &wav).is_err() {
                return;
            }
            f
        }
    };
    let ok = Command::new("pw-play").arg(&f).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    if !ok {
        let _ = Command::new("paplay").arg(&f).stderr(Stdio::null()).status();
    }
}
