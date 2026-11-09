fn main() {
    let cfg = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/app.slint", cfg).unwrap();
    // Windows: Echo's icon + version info in the exe (Explorer, taskbar, Properties > Details) - same fields as the
    // Tauri build's EchoClip.exe. An exe without them looks more suspicious to antivirus heuristics.
    println!("cargo:rerun-if-env-changed=ECHO_VERSION");
    println!("cargo:rerun-if-changed=echo.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let ver = std::env::var("ECHO_VERSION").unwrap_or_else(|_| std::env::var("CARGO_PKG_VERSION").unwrap());
        let n: Vec<u64> = ver.split(['.', '-', '+']).take(3).map(|x| x.parse().unwrap_or(0)).collect();
        let packed = (n.first().copied().unwrap_or(0) << 48) | (n.get(1).copied().unwrap_or(0) << 32) | (n.get(2).copied().unwrap_or(0) << 16);
        let mut r = winresource::WindowsResource::new();
        r.set_icon("echo.ico")
            .set("ProductName", "Echo")
            .set("FileDescription", "Echo")
            .set("CompanyName", "Echo")
            .set("LegalCopyright", "© 2026 Echo")
            .set("OriginalFilename", "EchoClip.exe")
            .set("InternalName", "EchoClip")
            .set("FileVersion", &ver)
            .set("ProductVersion", &ver)
            .set_version_info(winresource::VersionInfo::FILEVERSION, packed)
            .set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed);
        if let Err(e) = r.compile() {
            // no resource compiler (rc.exe) found: still build, just without the icon / details
            println!("cargo:warning=exe icon / version info skipped: {e}");
        }
    }
}
