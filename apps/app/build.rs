//! Windows only: embed the app icon and version info (VERSIONINFO) into the desktop executable, so
//! Explorer, the taskbar, the Start menu and Alt-Tab show the lynx, and the manifest that puts the
//! app on the Segment Heap ([`MANIFEST`]).
//!
//! On every other target this does nothing. A missing resource compiler is a warning, so a
//! cross-compile from macOS or Linux still links, unless `<PREFIX>_REQUIRE_WINRES=1` turns it
//! into an error (for release builds).

/// The application manifest: the Segment Heap (Windows 10 2004 and later; older versions ignore
/// it). On the default NT heap every large allocation and free takes the one process-wide heap lock,
/// and while Build Previews or an export ran, the render workers' image buffers kept it busy: the
/// window's thread spent ~45 % of its time waiting on that lock (frames of 50–300 ms, the slider
/// handle trailing the pointer). The Segment Heap has no such lock for large blocks.
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <heapType xmlns="http://schemas.microsoft.com/SMI/2020/WindowsSettings">SegmentHeap</heapType>
    </windowsSettings>
  </application>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(target_os = "linux")]
    embed_desktop_entry();
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
        .set("InternalName", lcc_brand::BINARY)
        .set_manifest(MANIFEST);
    if let Err(e) = res.compile() {
        if lcc_brand::env_is_set("REQUIRE_WINRES") {
            panic!("embedding Windows resources failed: {e}");
        }
        println!("cargo:warning={}.exe built without icon/version resources: {e}", lcc_brand::BINARY);
    }
}

/// Linux AppImage self-integration (`desktop_entry`): the package's desktop entry with the brand values
/// filled in, and the hicolor icons, embedded in the executable. The entry is the same template the
/// package renders (`packaging/linux/{app_id}.desktop.in`), so the two cannot drift. Problems are cargo
/// warnings: the app then builds without an entry and does not register itself.
#[cfg(target_os = "linux")]
fn embed_desktop_entry() {
    use std::fmt::Write as _;

    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR").map(std::path::PathBuf::from).unwrap_or_default();
    let out_dir = std::env::var_os("OUT_DIR").map(std::path::PathBuf::from).unwrap_or_default();
    let root = manifest_dir.join("../..");
    println!("cargo:rerun-if-changed=../../packaging/linux/{{app_id}}.desktop.in");
    println!("cargo:rerun-if-changed=../../assets/app-icon/hicolor");
    let warn = |what: &str| println!("cargo:warning=the AppImage will not register a launcher entry ({what})");
    let none = |what: &str| {
        warn(what);
        write_assets(&out_dir, "", &root, &warn);
    };

    let template_path = root.join("packaging/linux/{app_id}.desktop.in");
    let Ok(template) = std::fs::read_to_string(&template_path) else {
        none(&format!("missing {}", template_path.display()));
        return;
    };
    let mut rendered = String::with_capacity(template.len() + 64);
    for line in template.lines() {
        let line = line
            .replace("{{display_name}}", lcc_brand::DISPLAY_NAME)
            .replace("{{tagline}}", lcc_brand::TAGLINE)
            .replace("{{app_id}}", lcc_brand::APP_ID)
            .replace("{{url_scheme}}", lcc_brand::URL_SCHEME);
        // `{{binary}}` is the package's program: here `{{EXEC}}` is replaced at run time by the path of
        // the running AppImage, escaped (`desktop_entry::entry_for`)
        if line.contains("{{") && !line.starts_with("Exec={{binary}}") {
            warn("a placeholder in the desktop entry template is not a brand value");
            write_assets(&out_dir, "", &root, &warn);
            return;
        }
        // (the value is the whole `Exec=`: the quoted path and the file arguments, as the package has them)
        let line = match line.strip_prefix("Exec={{binary}} ") {
            // the package's field codes for files and URLs; `entry_for` writes its own
            Some(codes) if !codes.is_empty() && codes.split_whitespace().all(|c| matches!(c, "%F" | "%U" | "%f" | "%u")) => {
                "Exec={{EXEC}}".to_string()
            }
            _ => line,
        };
        let _ = writeln!(rendered, "{line}");
    }
    write_assets(&out_dir, &rendered, &root, &warn);
}

/// Writes `OUT_DIR/desktop_assets.rs`: a macro that expands to `TEMPLATE` (the entry text) and
/// `ICONS: &[(&str, &[u8])]`, each icon's path under `hicolor/` and its bytes.
#[cfg(target_os = "linux")]
fn write_assets(out_dir: &std::path::Path, entry: &str, root: &std::path::Path, warn: &dyn Fn(&str)) {
    use std::fmt::Write as _;
    let mut data = String::from("macro_rules! embedded_desktop_assets {\n    () => {\n");
    let _ = writeln!(data, "        pub const TEMPLATE: &str = {entry:?};");
    let _ = data.write_str("        pub const ICONS: &[(&str, &[u8])] = &[\n");
    if !entry.is_empty() {
        let src = root.join("assets/app-icon/hicolor");
        let mut sizes = Vec::new();
        for size in std::fs::read_dir(&src).into_iter().flatten().flatten() {
            sizes.push(size.path());
        }
        sizes.sort();
        for size in sizes {
            let mut files = Vec::new();
            for f in std::fs::read_dir(size.join("apps")).into_iter().flatten().flatten() {
                files.push(f.path());
            }
            files.sort();
            for f in files {
                let is_icon = f.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e, "png" | "svg"));
                if is_icon {
                    let rel = f.strip_prefix(&src).map(|r| r.to_string_lossy().into_owned()).unwrap_or_default();
                    let _ = writeln!(data, "            ({rel:?}, include_bytes!({:?})),", f);
                }
            }
        }
    }
    let _ = data.write_str("        ];\n    };\n}\n");
    if std::fs::write(out_dir.join("desktop_assets.rs"), data).is_err() {
        warn(&format!("cannot write {}", out_dir.join("desktop_assets.rs").display()));
    }
}
