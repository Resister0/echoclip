fn main() {
    // re-embed the UI whenever anything in ui/ changes
    println!("cargo:rerun-if-changed=../ui");
    tauri_build::build()
}
