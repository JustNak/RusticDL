//! Embed Windows version resources so Task Manager / Explorer show "RusticDL".
//!
//! UAC / DPI manifests for the main app come from GPUI (`windows-manifest`).
//! The dedicated updater embeds its own asInvoker manifest — see
//! `apps/updater/build.rs` and `assets/windows/app.manifest`.

fn main() {
    // Nightly CI stamps a unique version via RUSTICDL_VERSION without rewriting Cargo.toml
    // (so cargo-packager / Windows ProductVersion stay on the stable x.y.z triple).
    let version = std::env::var("RUSTICDL_VERSION")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into()));
    println!("cargo:rerun-if-env-changed=RUSTICDL_VERSION");
    println!("cargo:rustc-env=RUSTICDL_VERSION={version}");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "RusticDL");
    res.set("FileDescription", "RusticDL");
    res.set("CompanyName", "JustNak");
    res.set("LegalCopyright", "Copyright (c) JustNak");
    res.set("InternalName", "rusticdl");
    res.set("OriginalFilename", "rusticdl.exe");
    res.set_icon("assets/brand/icon.ico");

    if let Err(error) = res.compile() {
        // Don't fail cross-tooling environments that lack a resource compiler.
        println!("cargo:warning=winresource failed to embed version info: {error}");
    }
}
