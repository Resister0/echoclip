// Same as src-tauri/build.rs (minus Tauri): Echo's Google Drive login secret from ECHO_GDRIVE_SECRET or
// src-tauri/gdrive.secret.txt; empty = rclone's own Google login.
fn main() {
    println!("cargo:rerun-if-env-changed=ECHO_GDRIVE_SECRET");
    println!("cargo:rerun-if-changed=../../src-tauri/gdrive.secret.txt");
    println!("cargo:rerun-if-changed=../../src-tauri/src");
    let secret = std::env::var("ECHO_GDRIVE_SECRET").ok().filter(|v| !v.trim().is_empty())
        .or_else(|| std::fs::read_to_string("../../src-tauri/gdrive.secret.txt").ok())
        .unwrap_or_default();
    println!("cargo:rustc-env=ECHO_GDRIVE_SECRET={}", secret.trim());
}
