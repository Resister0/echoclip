//! echo-core: Echo's REAL backend for the native app (decision Chat A, 9 Nov).
//! The modules below are Echo's own files from src-tauri/src, compiled UNCHANGED via #[path]: one source of truth,
//! so every recorder / settings / hotkey / cloud fix made for Echo is in the native app too. None of them use Tauri.
//! When the Tauri app is retired, move these files here and drop the #[path] lines.
#![allow(dead_code, unused_imports, unused_variables, unused_mut, clippy::all)]

#[path = "../../../src-tauri/src/net.rs"]
pub mod net;
#[path = "../../../src-tauri/src/recorder.rs"]
pub mod recorder;
#[path = "../../../src-tauri/src/settings.rs"]
pub mod settings;
#[path = "../../../src-tauri/src/pad.rs"]
pub mod pad;
#[cfg(target_os = "linux")]
#[path = "../../../src-tauri/src/keyring_linux.rs"]
pub mod keyring_linux;
#[cfg(target_os = "linux")]
#[path = "../../../src-tauri/src/linuxutil.rs"]
pub mod linuxutil;
#[cfg(windows)]
#[path = "../../../src-tauri/src/gpucap.rs"]
pub mod gpucap;
#[cfg(windows)]
#[path = "../../../src-tauri/src/winutil.rs"]
pub mod winutil;
#[cfg(windows)]
#[path = "../../../src-tauri/src/hotkey.rs"]
pub mod hotkey;
#[cfg(target_os = "linux")]
#[path = "../../../src-tauri/src/hotkey_linux.rs"]
pub mod hotkey;
