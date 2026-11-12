//! Small Windows helpers: which game is in front, and short feedback sounds
//! played from Rust (work while Clipper is hidden / a fullscreen game has focus).

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND, MAX_PATH};
use windows::Win32::Media::Audio::{PlaySoundW, SND_MEMORY, SND_NODEFAULT, SND_SYNC};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId};

/// (exe file stem, window title) of the foreground window.
pub fn foreground_app() -> Option<(String, String)> {
    unsafe {
        let hwnd: HWND = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut title = [0u16; 256];
        let n = GetWindowTextW(hwnd, &mut title);
        let title = String::from_utf16_lossy(&title[..n.max(0) as usize]);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; MAX_PATH as usize * 2];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let stem = std::path::Path::new(&path).file_stem()?.to_string_lossy().to_string();
        Some((stem, title))
    }
}

/// Play a mono 16-bit WAV held in memory; blocks, so call it from a worker thread.
pub fn play_wav(wav: &[u8]) {
    unsafe {
        let _ = PlaySoundW(PCWSTR(wav.as_ptr() as *const u16), None, SND_MEMORY | SND_SYNC | SND_NODEFAULT);
    }
}

/// True if another Clipper is already running (named mutex). The first instance keeps the mutex for its lifetime.
pub fn already_running() -> bool {
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    unsafe {
        match CreateMutexW(None, true, windows::core::w!("Local\\ClipperSingleInstance")) {
            Ok(h) => {
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    return true;
                }
                let _ = h; // never closed: held until Clipper exits
                false
            }
            Err(_) => false,
        }
    }
}

/// Bring the running Clipper's window to the front (it may be hidden in the tray).
pub fn show_existing() {
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW};
    unsafe {
        if let Ok(h) = FindWindowW(None, windows::core::w!("Echo")) {
            let _ = ShowWindow(h, SW_SHOW);
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE); // only un-minimise; keeps it maximised
            }
            let _ = SetForegroundWindow(h);
        }
    }
}

/// Seconds since the last keyboard/mouse input anywhere on the PC.
/// true when `pid` most likely runs as administrator and Echo doesn't: Windows then hides that window's keys
/// from Echo's keyboard hook. A token a normal program can't read counts as admin (or a protected process).
pub fn elevated_vs_echo(pid: u32) -> bool {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe fn elevated(p: HANDLE) -> Option<bool> {
        let mut t = HANDLE::default();
        OpenProcessToken(p, TOKEN_QUERY, &mut t).ok()?;
        let mut e = TOKEN_ELEVATION::default();
        let mut n = 0u32;
        let r = GetTokenInformation(t, TokenElevation, Some(&mut e as *mut TOKEN_ELEVATION as *mut core::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut n);
        let _ = CloseHandle(t);
        r.ok()?;
        Some(e.TokenIsElevated != 0)
    }
    unsafe {
        if elevated(GetCurrentProcess()) == Some(true) { return false; }
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let r = elevated(h);
        let _ = CloseHandle(h);
        r.unwrap_or(true)
    }
}

pub fn idle_seconds() -> u64 {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    unsafe {
        let mut li = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
        if GetLastInputInfo(&mut li).as_bool() {
            (GetTickCount().wrapping_sub(li.dwTime) / 1000) as u64
        } else {
            0
        }
    }
}

/// Make a window a non-activating, always-on-top tool window (the in-game "clip saved" popup):
/// showing it never takes focus away from the game.
pub fn make_overlay(hwnd: isize) {
    use windows::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        let h = HWND(hwnd as *mut core::ffi::c_void);
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        // without WS_EX_APPWINDOW (winit sets it on every top-level window): APPWINDOW wins over TOOLWINDOW and
        // kept the native popup in Alt+Tab as "Echo popup" (10 Nov 2026). Hidden while the style changes:
        // Windows only re-reads it for the taskbar / Alt+Tab list when the window is shown again (the caller shows it).
        let _ = ShowWindow(h, SW_HIDE);
        let ex = (ex & !(WS_EX_APPWINDOW.0 as isize)) | (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0) as isize;
        SetWindowLongPtrW(h, GWL_EXSTYLE, ex);
    }
}

/// Show without activating (keeps the game focused), or hide.
pub fn overlay_visible(hwnd: isize, on: bool) {
    use windows::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        let h = HWND(hwnd as *mut core::ffi::c_void);
        if on {
            let _ = SetWindowPos(h, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        } else {
            let _ = ShowWindow(h, SW_HIDE);
        }
    }
}

/// The foreground window handle (0 = none) and whether a window handle is still valid.
pub fn foreground_hwnd() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}
pub fn window_alive(hwnd: isize) -> bool {
    unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(HWND(hwnd as _)).as_bool() }
}

/// (pid, exe file stem, window title) of the foreground window.
pub fn foreground_pid() -> Option<(u32, String, String)> {
    unsafe {
        let hwnd: HWND = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let (stem, title) = foreground_app()?;
        Some((pid, stem, title))
    }
}

/// All running processes: (pid, parent pid, lowercase exe file name like "discord.exe").
pub fn processes() -> Vec<(u32, u32, String)> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            out.push((e.th32ProcessID, e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..n]).to_lowercase()));
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// VR (Quest Link / Air Link, SteamVR, Virtual Desktop): the game draws into the headset and the monitor
/// shows the desktop, but the game's own PC window shows the player's view (that's what Moments records).
/// When a VR runtime runs: the biggest visible window of a process that loaded a VR library.
/// Returns (pid, window, exe stem, window title). Cheap when no VR runtime runs (one process list).
pub fn vr_game() -> Option<(u32, isize, String, String)> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowRect, IsIconic, IsWindowVisible};
    const RUNTIMES: &[&str] = &["vrserver.exe", "vrcompositor.exe", "ovrserver_x64.exe", "virtualdesktop.streamer.exe"];
    // runtimes, their helpers and launchers are never "the game"
    const SKIP: &[&str] = &["vrserver.exe", "vrcompositor.exe", "vrmonitor.exe", "vrdashboard.exe", "vrwebhelper.exe",
        "vrstartup.exe", "vrserverhelper.exe", "vrprismhost.exe", "ovrserver_x64.exe", "ovrserverhelper_x64.exe",
        "oculusclient.exe", "oculusdash.exe", "oculusmirror.exe", "ovrredir.exe", "virtualdesktop.streamer.exe",
        "virtualdesktop.server.exe", "steam.exe", "steamwebhelper.exe", "echo.exe", "explorer.exe"];
    let procs = processes();
    if !procs.iter().any(|(_, _, n)| RUNTIMES.contains(&n.as_str())) {
        return None;
    }
    let names: HashMap<u32, String> = procs.iter().map(|(p, _, n)| (*p, n.clone())).collect();
    unsafe extern "system" fn each(h: HWND, l: LPARAM) -> BOOL {
        let v = &mut *(l.0 as *mut Vec<(isize, u32, i64)>);
        if IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool() {
            let mut r = RECT::default();
            if GetWindowRect(h, &mut r).is_ok() {
                let area = (r.right - r.left) as i64 * (r.bottom - r.top) as i64;
                if area >= 320 * 240 {
                    let mut pid = 0u32;
                    GetWindowThreadProcessId(h, Some(&mut pid));
                    v.push((h.0 as isize, pid, area));
                }
            }
        }
        BOOL(1)
    }
    let mut wins: Vec<(isize, u32, i64)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut wins as *mut _ as isize));
    }
    // pid -> (uses VR, checked at); "no" is checked again after 20 s (a game loads its VR library a bit after start)
    static CACHE: Mutex<Option<HashMap<u32, (bool, Instant)>>> = Mutex::new(None);
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(HashMap::new);
    cache.retain(|p, _| names.contains_key(p));
    let me = std::process::id();
    let mut best: Option<(i64, u32, isize)> = None;
    for (h, pid, area) in wins {
        if pid == me {
            continue;
        }
        let Some(n) = names.get(&pid) else { continue };
        if SKIP.contains(&n.as_str()) {
            continue;
        }
        let e = cache.entry(pid).or_insert_with(|| (uses_vr(pid), Instant::now()));
        if !e.0 && e.1.elapsed() > Duration::from_secs(20) {
            *e = (uses_vr(pid), Instant::now());
        }
        if e.0 && best.map_or(true, |b| area > b.0) {
            best = Some((area, pid, h));
        }
    }
    drop(guard);
    let (_, pid, h) = best?;
    let mut title = [0u16; 256];
    let n = unsafe { GetWindowTextW(HWND(h as _), &mut title) };
    let title = String::from_utf16_lossy(&title[..n.max(0) as usize]);
    let stem = names.get(&pid).map(|n| n.trim_end_matches(".exe").to_string()).unwrap_or_default();
    Some((pid, h, stem, title))
}

/// The process loaded OpenVR / OpenXR / the Oculus runtime (= it's a VR app).
fn uses_vr(pid: u32) -> bool {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, MODULEENTRY32W, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32,
    };
    const LIBS: &[&str] = &["openvr_api.dll", "openxr_loader.dll", "libovrrt64_1.dll", "libovrrt32_1.dll", "vrclient_x64.dll", "vrclient.dll"];
    unsafe {
        // fails for protected (anti-cheat) processes -> not detected, fine
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid) else { return false };
        let mut e = MODULEENTRY32W { dwSize: std::mem::size_of::<MODULEENTRY32W>() as u32, ..Default::default() };
        let mut ok = Module32FirstW(snap, &mut e).is_ok();
        let mut found = false;
        while ok {
            let n = e.szModule.iter().position(|&c| c == 0).unwrap_or(e.szModule.len());
            if LIBS.contains(&String::from_utf16_lossy(&e.szModule[..n]).to_lowercase().as_str()) {
                found = true;
                break;
            }
            ok = Module32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
        found
    }
}

/// Per-app audio (process loopback, Windows 10 2004+): pushes the sound of `pid` and its child
/// processes into `push` as f32 interleaved at `rate`/`ch` until `stop` is set.
pub fn app_loopback(pid: u32, rate: u32, ch: u16, stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
                    mut push: impl FnMut(&[f32])) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    use wasapi::*;
    let _ = initialize_mta();
    let fmt = WaveFormat::new(32, 32, &SampleType::Float, rate as usize, ch as usize, None);
    let mut client = AudioClient::new_application_loopback_client(pid, true).map_err(|e| e.to_string())?;
    client
        .initialize_client(&fmt, &Direction::Capture, &StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 1_000_000 })
        .map_err(|e| e.to_string())?;
    let ev = client.set_get_eventhandle().map_err(|e| e.to_string())?;
    let cap = client.get_audiocaptureclient().map_err(|e| e.to_string())?;
    client.start_stream().map_err(|e| e.to_string())?;
    let mut bytes: std::collections::VecDeque<u8> = std::collections::VecDeque::new();
    let mut samples: Vec<f32> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        let _ = ev.wait_for_event(200); // no event while the app is silent - that's fine
        loop {
            match cap.get_next_packet_size() {
                Ok(Some(n)) if n > 0 => {}
                Ok(_) => break,
                Err(e) => {
                    let _ = client.stop_stream();
                    return Err(e.to_string());
                }
            }
            bytes.clear();
            let info = cap.read_from_device_to_deque(&mut bytes).map_err(|e| e.to_string())?;
            samples.clear();
            if info.flags.silent {
                samples.resize(bytes.len() / 4, 0.0);
            } else {
                let (a, b) = bytes.as_slices();
                let mut all = Vec::with_capacity(a.len() + b.len());
                all.extend_from_slice(a);
                all.extend_from_slice(b);
                samples.extend(all.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])));
            }
            push(&samples);
        }
    }
    let _ = client.stop_stream();
    Ok(())
}

/// Full exe path of a process.
pub fn exe_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; MAX_PATH as usize * 2];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// The exe's version info says it is Echo (ProductName "Echo", set by prototype-native/build.rs) - QA E19, 11 Nov 2026:
/// a copy of Echo under another file name (a test copy, a renamed portable exe) was taken for a game when maximised.
pub fn exe_is_echo(exe: &str) -> bool {
    use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
    let w: Vec<u16> = exe.encode_utf16().chain(std::iter::once(0)).collect();
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    unsafe {
        let n = GetFileVersionInfoSizeW(PCWSTR(w.as_ptr()), None);
        if n == 0 {
            return false;
        }
        let mut buf = vec![0u8; n as usize];
        if GetFileVersionInfoW(PCWSTR(w.as_ptr()), 0, n, buf.as_mut_ptr() as *mut _).is_err() {
            return false;
        }
        let (mut p, mut len): (*mut std::ffi::c_void, u32) = (std::ptr::null_mut(), 0);
        let q = wide("\\VarFileInfo\\Translation");
        let mut langs: Vec<(u16, u16)> = vec![];
        if VerQueryValueW(buf.as_ptr() as *const _, PCWSTR(q.as_ptr()), &mut p, &mut len).as_bool() && !p.is_null() {
            let words = std::slice::from_raw_parts(p as *const u16, (len / 2) as usize);
            langs = words.chunks_exact(2).map(|c| (c[0], c[1])).collect();
        }
        if langs.is_empty() {
            langs.push((0x0409, 0x04b0));
        }
        for (l, c) in langs {
            let key = wide(&format!("\\StringFileInfo\\{l:04x}{c:04x}\\ProductName"));
            if VerQueryValueW(buf.as_ptr() as *const _, PCWSTR(key.as_ptr()), &mut p, &mut len).as_bool() && !p.is_null() && len > 0 {
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(p as *const u16, len as usize));
                if s.trim_end_matches('\0').trim() == "Echo" {
                    return true;
                }
            }
        }
        false
    }
}

/// Is this process still running?
pub fn pid_alive(pid: u32) -> bool {
    use windows::Win32::System::Threading::GetExitCodeProcess;
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code).is_ok();
        let _ = CloseHandle(h);
        ok && code == 259 // STILL_ACTIVE
    }
}

/// Play a .wav file (custom sounds); blocks, so call it from a worker thread.
pub fn play_file(path: &str) {
    use windows::Win32::Media::Audio::SND_FILENAME;
    let w: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = PlaySoundW(PCWSTR(w.as_ptr()), None, SND_FILENAME | SND_SYNC | SND_NODEFAULT);
    }
}

/// Free bytes on the drive that holds `dir` (0 if unknown).
pub fn disk_free(dir: &str) -> u64 {
    let w: Vec<u16> = dir.encode_utf16().chain(std::iter::once(0)).collect();
    let mut free = 0u64;
    unsafe {
        let _ = windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(PCWSTR(w.as_ptr()), Some(&mut free), None, None);
    }
    free
}

/// Size of a window in pixels (0,0 if it's gone).
pub fn window_size(hwnd: isize) -> (i32, i32) {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    unsafe {
        let mut r = RECT::default();
        if hwnd == 0 || GetWindowRect(HWND(hwnd as *mut core::ffi::c_void), &mut r).is_err() {
            return (0, 0);
        }
        (r.right - r.left, r.bottom - r.top)
    }
}

/// Does the foreground window cover its whole monitor (fullscreen / borderless game)?
pub fn foreground_fullscreen() -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return false;
        }
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return false;
        }
        let m = mi.rcMonitor;
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
    }
}

/// Second launch -> ask the running Echo to show itself (through Tauri, so its window state stays right).
/// Returns false if no Echo is listening (older version): then fall back to show_existing().
pub fn signal_show() -> bool {
    use windows::Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE};
    unsafe {
        match OpenEventW(EVENT_MODIFY_STATE, false, windows::core::w!("Local\\EchoShowWindow")) {
            Ok(h) => {
                let ok = SetEvent(h).is_ok();
                let _ = windows::Win32::Foundation::CloseHandle(h);
                ok
            }
            Err(_) => false,
        }
    }
}

/// Running Echo: call `cb` whenever another launch asks to show the window.
pub fn listen_show(cb: impl Fn() + Send + 'static) {
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};
    std::thread::spawn(move || unsafe {
        let Ok(h) = CreateEventW(None, false, false, windows::core::w!("Local\\EchoShowWindow")) else { return };
        loop {
            if WaitForSingleObject(h, INFINITE) != windows::Win32::Foundation::WAIT_OBJECT_0 {
                return;
            }
            cb();
        }
    });
}

// ---------- native replacements for the old PowerShell / reg.exe calls ----------
// (PowerShell one-liners that kill processes, elevate and edit the registry look like malware to
// antivirus heuristics; these do the same through normal Windows functions.)

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Move a file to the Recycle Bin (no dialogs). true = gone.
pub fn recycle(path: &str) -> bool {
    use windows::Win32::UI::Shell::{SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW};
    // pFrom must end with two NULs
    let mut from: Vec<u16> = path.encode_utf16().collect();
    from.extend([0, 0]);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT).0 as u16,
        ..Default::default()
    };
    let r = unsafe { SHFileOperationW(&mut op) };
    r == 0 && !op.fAnyOperationsAborted.as_bool() && !std::path::Path::new(path).exists()
}

/// Put data on the clipboard (several formats at once). Retries a few times if another app holds it.
fn set_clipboard(items: Vec<(u32, Vec<u8>)>) -> bool {
    use windows::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    unsafe {
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(HWND::default()).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        if !opened {
            return false;
        }
        let _ = EmptyClipboard();
        let mut ok = true;
        for (fmt, data) in items {
            let Ok(h): Result<HGLOBAL, _> = GlobalAlloc(GMEM_MOVEABLE, data.len()) else { ok = false; continue };
            let p = GlobalLock(h) as *mut u8;
            if p.is_null() {
                ok = false;
                continue;
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
            let _ = GlobalUnlock(h);
            ok &= SetClipboardData(fmt, HANDLE(h.0)).is_ok();
        }
        let _ = CloseClipboard();
        ok
    }
}

/// Copy files to the clipboard like Explorer's Ctrl+C (paste into Discord, folders…).
pub fn clipboard_files(paths: &[&str]) -> bool {
    use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
    use windows::Win32::System::Ole::CF_HDROP;
    // DROPFILES header (20 bytes): offset of the file list, pt x/y, fNC, fWide = 1; then NUL-separated
    // UTF-16 paths ending with an extra NUL
    let mut d: Vec<u8> = Vec::new();
    d.extend(20u32.to_le_bytes());
    d.extend(0i32.to_le_bytes());
    d.extend(0i32.to_le_bytes());
    d.extend(0u32.to_le_bytes());
    d.extend(1u32.to_le_bytes());
    for p in paths {
        for c in p.encode_utf16().chain(std::iter::once(0)) {
            d.extend(c.to_le_bytes());
        }
    }
    d.extend(0u16.to_le_bytes());
    let effect = unsafe { RegisterClipboardFormatW(PCWSTR(wide("Preferred DropEffect").as_ptr())) };
    set_clipboard(vec![(CF_HDROP.0 as u32, d), (effect, 1u32.to_le_bytes().to_vec())]) // 1 = copy
}

/// Put text on the clipboard.
pub fn clipboard_text(text: &str) -> bool {
    use windows::Win32::System::Ole::CF_UNICODETEXT;
    let d: Vec<u8> = text.encode_utf16().chain(std::iter::once(0)).flat_map(|c| c.to_le_bytes()).collect();
    set_clipboard(vec![(CF_UNICODETEXT.0 as u32, d)])
}

/// Open a link in the default browser the normal Windows way (ShellExecute "open"), instead of
/// starting rundll32 - antivirus heuristics treat rundll32 launches from unsigned apps as suspicious.
pub fn open_url(url: &str) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let (verb, file) = (wide("open"), wide(url));
    let h = unsafe { ShellExecuteW(HWND::default(), PCWSTR(verb.as_ptr()), PCWSTR(file.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    h.0 as isize > 32
}

/// Start a program with admin rights (Windows shows its permission prompt).
/// wait = true: waits and returns the exit code; false: returns Some(0) once it started. None = declined / failed.
pub fn run_elevated(exe: &str, args: &str, wait: bool) -> Option<i32> {
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    let (verb, file, params) = (wide("runas"), wide(exe), wide(args));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info).ok()?;
        if info.hProcess.is_invalid() {
            return Some(0);
        }
        let mut code = 0u32;
        if wait {
            WaitForSingleObject(info.hProcess, INFINITE);
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
        }
        let _ = CloseHandle(info.hProcess);
        Some(code as i32)
    }
}

/// HKCU\...\Run entry: Some(command) sets it, None removes it.
pub fn set_run_entry(name: &str, command: Option<&str>) -> bool {
    use windows::Win32::System::Registry::{RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ};
    unsafe {
        let mut key = HKEY::default();
        let sub = wide(r"Software\Microsoft\Windows\CurrentVersion\Run");
        if RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr()), 0, KEY_SET_VALUE, &mut key).is_err() {
            return false;
        }
        let n = wide(name);
        let ok = match command {
            Some(c) => {
                let data: Vec<u8> = wide(c).iter().flat_map(|w| w.to_le_bytes()).collect();
                RegSetValueExW(key, PCWSTR(n.as_ptr()), 0, REG_SZ, Some(&data)).is_ok()
            }
            None => {
                let _ = RegDeleteValueW(key, PCWSTR(n.as_ptr()));
                true
            }
        };
        let _ = RegCloseKey(key);
        ok
    }
}

fn kill_pid(pid: u32) {
    use windows::Win32::System::Threading::{TerminateProcess, PROCESS_TERMINATE};
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) {
            let _ = TerminateProcess(h, 1);
            let _ = CloseHandle(h);
        }
    }
}

/// Stops helper programs (ffmpeg.exe, rclone.exe…) left running by an earlier Echo that crashed or was
/// killed: their parent is gone, or is another Echo / Clipper process.
pub fn kill_orphans(names: &[&str]) {
    let me = std::process::id();
    let all = processes();
    for (pid, parent, name) in &all {
        if !names.contains(&name.as_str()) || *parent == me {
            continue;
        }
        let parent_name = all.iter().find(|p| p.0 == *parent).map(|p| p.2.as_str());
        let orphan = match parent_name {
            None => true,
            Some(n) => (n == "echo.exe" || n == "clipper.exe") && *parent != me,
        };
        if orphan || !pid_alive(*parent) {
            kill_pid(*pid);
        }
    }
}

/// Stops this Echo's own helper programs with these names (e.g. a thumbnail ffmpeg still reading a clip),
/// except the ones listed in `keep`.
pub fn kill_children(names: &[&str], keep: &[u32]) {
    let me = std::process::id();
    for (pid, parent, name) in processes() {
        if parent == me && names.contains(&name.as_str()) && !keep.contains(&pid) {
            kill_pid(pid);
        }
    }
}

/// An exe's icon as `size`x`size` BGRA pixels (top row first).
pub fn exe_icon_bgra(exe: &str, size: i32) -> Option<Vec<u8>> {
    use windows::Win32::Graphics::Gdi::{DeleteObject, GetDC, GetDIBits, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, PrivateExtractIconsW, HICON, ICONINFO};
    let mut name = [0u16; 260];
    for (i, c) in exe.encode_utf16().take(259).enumerate() {
        name[i] = c;
    }
    unsafe {
        let mut icons = [HICON::default()];
        let n = PrivateExtractIconsW(&name, 0, size, size, Some(&mut icons), None, 0);
        if n == 0 || n == u32::MAX || icons[0].is_invalid() {
            return None;
        }
        let mut ii = ICONINFO::default();
        let got = GetIconInfo(icons[0], &mut ii).is_ok();
        let mut px = vec![0u8; (size * size * 4) as usize];
        let mut ok = false;
        if got && !ii.hbmColor.is_invalid() {
            let mut bi = BITMAPINFO::default();
            bi.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            };
            let dc = GetDC(HWND::default());
            ok = GetDIBits(dc, ii.hbmColor, 0, size as u32, Some(px.as_mut_ptr() as _), &mut bi, DIB_RGB_COLORS) > 0;
            ReleaseDC(HWND::default(), dc);
            // old-style icons have no alpha: make them opaque
            if ok && px.chunks(4).all(|p| p[3] == 0) {
                px.chunks_mut(4).for_each(|p| p[3] = 255);
            }
        }
        if got {
            let _ = DeleteObject(ii.hbmColor);
            let _ = DeleteObject(ii.hbmMask);
        }
        let _ = DestroyIcon(icons[0]);
        ok.then_some(px)
    }
}

/// Windows file / folder picker. filter = (label, "*.wav;*.mp3"); folder = true picks a folder.
pub fn pick_path(title: &str, filter: Option<(&str, &str)>, folder: bool) -> Option<String> {
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, FOS_PICKFOLDERS, SIGDN_FILESYSPATH};
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let res = (|| -> Option<String> {
            let d: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
            let t = wide(title);
            let _ = d.SetTitle(PCWSTR(t.as_ptr()));
            if folder {
                let o = d.GetOptions().ok()?;
                d.SetOptions(o | FOS_PICKFOLDERS).ok()?;
            }
            let (lab, pat) = filter.map(|(a, b)| (wide(a), wide(b))).unwrap_or_default();
            if filter.is_some() {
                let spec = [COMDLG_FILTERSPEC { pszName: PCWSTR(lab.as_ptr()), pszSpec: PCWSTR(pat.as_ptr()) }];
                let _ = d.SetFileTypes(&spec);
            }
            d.Show(HWND::default()).ok()?;
            let item = d.GetResult().ok()?;
            let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            let s = p.to_string().ok();
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
            s
        })();
        if init {
            CoUninitialize();
        }
        res
    }
}
