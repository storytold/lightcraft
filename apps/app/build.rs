//! Windows only: embed the app icon and version info (VERSIONINFO) into the desktop executable, so
//! Explorer, the taskbar, the Start menu and Alt-Tab show the lynx.
//!
//! On every other target this does nothing. A missing resource compiler is a warning, so a
//! cross-compile from macOS or Linux still links, unless `<PREFIX>_REQUIRE_WINRES=1` turns it
//! into an error (for release builds).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app-icon/app.ico");
    let require = lcc_brand::env_var("REQUIRE_WINRES");
    println!("cargo:rerun-if-env-changed={require}");
    for prefix in lcc_brand::LEGACY_ENV_PREFIXES {
        println!("cargo:rerun-if-env-changed={prefix}_REQUIRE_WINRES");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/app-icon/app.ico")
        .set("ProductName", lcc_brand::DISPLAY_NAME)
        .set("FileDescription", &format!("{}: {}", lcc_brand::DISPLAY_NAME, lcc_brand::TAGLINE))
        .set("CompanyName", lcc_brand::VENDOR)
        .set("LegalCopyright", &format!("Copyright (c) {}. MIT AND Apache-2.0.", lcc_brand::VENDOR))
        .set("OriginalFilename", &format!("{}.exe", lcc_brand::BINARY))
        .set("InternalName", lcc_brand::BINARY);
    if let Err(e) = res.compile() {
        if lcc_brand::env_is_set("REQUIRE_WINRES") {
            panic!("embedding Windows resources failed: {e}");
        }
        println!("cargo:warning={}.exe built without icon/version resources: {e}", lcc_brand::BINARY);
    }
}
