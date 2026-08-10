//! Embed Windows version resources so Task Manager shows "RusticDL Backend".

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let icon = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/brand/icon.ico");

    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "RusticDL");
    // Background / bridge process name (Startup lists, Task Manager overflow).
    res.set("FileDescription", "RusticDL Backend");
    res.set("CompanyName", "JustNak");
    res.set("LegalCopyright", "Copyright (c) JustNak");
    res.set("InternalName", "rusticdl-native-host");
    res.set("OriginalFilename", "rusticdl-native-host.exe");
    if icon.is_file() {
        res.set_icon(icon.to_str().expect("icon path is valid UTF-8"));
    }

    if let Err(error) = res.compile() {
        println!("cargo:warning=winresource failed to embed version info: {error}");
    }
}
