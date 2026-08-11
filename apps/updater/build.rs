//! Embed Windows version resources and an asInvoker UAC manifest.
//!
//! Without `requestedExecutionLevel=asInvoker`, Windows Installer Detection
//! treats filenames containing "update" as requiring elevation. CreateProcess
//! then fails with ERROR_ELEVATION_REQUIRED (740) when the main app is not
//! elevated — which is how self-update used to break.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let icon = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/brand/icon.ico");
    let manifest =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/windows/app.manifest");

    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-changed={}", manifest.display());

    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "RusticDL");
    res.set("FileDescription", "RusticDL Updater");
    res.set("CompanyName", "JustNak");
    res.set("LegalCopyright", "Copyright (c) JustNak");
    res.set("InternalName", "rusticdl-updater");
    res.set("OriginalFilename", "rusticdl-updater.exe");
    if icon.is_file() {
        res.set_icon(icon.to_str().expect("icon path is valid UTF-8"));
    }
    if manifest.is_file() {
        res.set_manifest_file(manifest.to_str().expect("manifest path is valid UTF-8"));
    }

    if let Err(error) = res.compile() {
        println!("cargo:warning=winresource failed to embed version info: {error}");
    }
}
