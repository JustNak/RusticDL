//! Embed Windows version resources so Task Manager / Explorer show "RusticDL".

fn main() {
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
