fn main() {
    // re-embed the UI whenever anything in ui/ changes
    println!("cargo:rerun-if-changed=../ui");
    // Echo's Google Drive login secret (chat 29): kept out of the code, because GitHub's push protection blocks
    // Google client secrets in the public repo. From the env var ECHO_GDRIVE_SECRET (public build: repo secret) or
    // src-tauri/gdrive.secret.txt (private repo only - "*.secret.txt" is in ci/public-exclude.txt). Neither = empty,
    // and Echo uses rclone's own Google login.
    println!("cargo:rerun-if-env-changed=ECHO_GDRIVE_SECRET");
    println!("cargo:rerun-if-changed=gdrive.secret.txt");
    let secret = std::env::var("ECHO_GDRIVE_SECRET").ok().filter(|v| !v.trim().is_empty())
        .or_else(|| std::fs::read_to_string("gdrive.secret.txt").ok())
        .unwrap_or_default();
    let secret = secret.trim();
    if secret.is_empty() { println!("cargo:warning=no Google Drive login secret (ECHO_GDRIVE_SECRET / gdrive.secret.txt) - this build uses rclone's"); }
    println!("cargo:rustc-env=ECHO_GDRIVE_SECRET={secret}");
    tauri_build::build()
}
