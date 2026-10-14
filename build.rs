//! Embeds the Windows resources: icon, VERSIONINFO (product, version, credits) and a manifest.
//! On the GNU target winresource needs `windres` / `ar` (see .toolchain/env.sh, `WINDRES` and `AR`).

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=assets/winstats.ico");
    println!("cargo:rerun-if-changed=assets/WindowsStats.manifest");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=WINDRES");

    let version = env!("CARGO_PKG_VERSION");
    // build year for the copyright line, without pulling in a date crate
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let year = 1970 + (secs as f64 / 86400.0 / 365.2425) as u64;
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/winstats.ico")
        .set_manifest_file("assets/WindowsStats.manifest")
        .set("ProductName", "Windows Stats")
        .set("FileDescription", "Windows Stats - sistem izleme")
        .set("ProductVersion", version)
        .set("FileVersion", version)
        .set("CompanyName", "Windows Stats")
        .set(
            "LegalCopyright",
            &format!("Copyright \u{00A9} {year} Windows Stats contributors. Based on winstats, Copyright \u{00A9} 2026 Oleksandr Zhabotynskyi (MIT)"),
        )
        .set("OriginalFilename", "WindowsStats.exe")
        .set("InternalName", "WindowsStats");
    if let Err(e) = res.compile() {
        println!("cargo:warning=windows resources not embedded: {e}");
    }
}
