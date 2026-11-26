//! Windows only: embed the app icon and version info (VERSIONINFO) into `lightcraft.exe`, so
//! Explorer, the taskbar, the Start menu and Alt-Tab show the lynx.
//!
//! On every other target this does nothing. A missing resource compiler is a warning, so a
//! cross-compile from macOS or Linux still links, unless `LIGHTCRAFT_REQUIRE_WINRES=1` turns it
//! into an error (for release and Nightly builds).

#[path = "src/build_channel.rs"]
mod build_channel;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=LIGHTCRAFT_BUILD_CHANNEL");
    println!("cargo:rustc-check-cfg=cfg(lightcraft_nightly_v1)");
    // Only the Nightly producer sets this; anything else is a build error, never a silent fallback.
    let nightly = match std::env::var("LIGHTCRAFT_BUILD_CHANNEL").as_deref() {
        Ok("nightly-v1") => {
            println!("cargo:rustc-cfg=lightcraft_nightly_v1");
            true
        }
        // An empty value (an unset CI input) means the default stable build.
        Ok("stable" | "") | Err(std::env::VarError::NotPresent) => false,
        _ => {
            eprintln!("unsupported LIGHTCRAFT_BUILD_CHANNEL (expected stable or nightly-v1)");
            std::process::exit(1);
        }
    };
    println!("cargo:rerun-if-changed=../../assets/app-icon/lightcraft.ico");
    println!("cargo:rerun-if-changed=../../assets/app-icon/lightcraft-nightly.ico");
    println!("cargo:rerun-if-env-changed=LIGHTCRAFT_REQUIRE_WINRES");
    println!("cargo:rerun-if-env-changed=LIGHTCRAFT_VERSION");
    println!("cargo:rerun-if-env-changed=LIGHTCRAFT_OVERRIDE_VERSION");
    let built_version = if nightly {
        // The feed identity: LIGHTCRAFT_VERSION, or the packaging scripts' LIGHTCRAFT_OVERRIDE_VERSION.
        match std::env::var("LIGHTCRAFT_VERSION").or_else(|_| std::env::var("LIGHTCRAFT_OVERRIDE_VERSION")) {
            Ok(version) if build_channel::is_nightly_version(&version) => version,
            _ => {
                eprintln!("nightly-v1 requires an explicit LIGHTCRAFT_VERSION feed identity");
                std::process::exit(1);
            }
        }
    } else {
        std::env::var("CARGO_PKG_VERSION").unwrap_or_default()
    };
    println!("cargo:rustc-env=LIGHTCRAFT_BUILT_VERSION={built_version}");
    println!("cargo:rerun-if-env-changed=LIGHTCRAFT_BUILD_SHA");
    let built_commit = std::env::var("LIGHTCRAFT_BUILD_SHA").unwrap_or_default();
    if nightly && !(built_commit.len() == 40 && built_commit.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))) {
        eprintln!("nightly-v1 requires the exact LIGHTCRAFT_BUILD_SHA source commit");
        std::process::exit(1);
    }
    println!("cargo:rustc-env=LIGHTCRAFT_BUILD_COMMIT={built_commit}");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon(build_channel::windows_icon_path(nightly))
        .set("ProductName", if nightly { "LightCraft Nightly" } else { "LightCraft" })
        .set(
            "FileDescription",
            if nightly { "LightCraft Nightly photo library and raw developer" } else { "LightCraft photo library and raw developer" },
        )
        .set("LegalCopyright", "Copyright (c) the LightCraft authors. MIT OR Apache-2.0.")
        .set("OriginalFilename", "lightcraft.exe")
        .set("InternalName", "lightcraft");
    if nightly {
        // The Launcher verifies the PE ProductVersion against the exact feed identity, not FileVersion.
        res.set("ProductVersion", &built_version);
    }
    if let Err(e) = res.compile() {
        if std::env::var_os("LIGHTCRAFT_REQUIRE_WINRES").is_some() {
            panic!("embedding Windows resources failed: {e}");
        }
        println!("cargo:warning=lightcraft.exe built without icon/version resources: {e}");
    }
}
