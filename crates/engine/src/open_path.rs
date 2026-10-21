//! Whether a path the library holds may be handed to the operating system.
//!
//! A photo's `Source::File` path is not always something a user typed: it can come from an
//! imported Lightroom catalog, an `.lrcat` someone was sent, a relink or a synchronised folder.
//! Photo ▸ Edit In ▸ *Open Original in Other Editor* and the system default opener run whatever
//! the path names, so a record pointing at `payload.exe`, `thing.lnk`, `run.bat`, `page.hta`
//! (Windows) or `Evil.app`, `go.command` (macOS) would be executed on one click (issue #2135).
//!
//! [`openable`] is the one gate every such hand-over goes through: the path must be a regular
//! file whose extension is one LightCraft itself reads ([`crate::import::EXTENSIONS`], which
//! already covers the Edit In copy formats `tif`/`tiff`/`psd`), and what a link resolves to must
//! be one too, so a `photo.jpg` pointing at `/bin/sh` is refused as well. What comes back is the
//! absolute path to hand over; what comes back on refusal is the sentence to show the user.
//!
//! Nothing here opens, stats-for-content or reads the file: it only decides whether the path may
//! leave LightCraft. Catalog import applies the same test ([`crate::lightroom_catalog`]), so a
//! refused original never becomes a library record in the first place.

use std::path::Path;

/// The absolute path to hand to an opener or external editor, or the reason it is refused.
///
/// `label` names the thing in the error (a file name, a photo's name); an empty `label` uses
/// the path's own file name.
pub fn openable(path: &str, label: &str) -> Result<String, String> {
    let p = Path::new(path);
    let name = match label.trim() {
        "" => p.file_name().map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned()),
        l => l.to_string(),
    };
    if !is_media(p) {
        return Err(format!("{name} isn't one of the file types LightCraft reads, so it isn't handed to another application"));
    }
    match std::fs::metadata(p) {
        Ok(m) if m.is_file() => {}
        Ok(_) => return Err(format!("{name} isn't a file, so it isn't handed to another application")),
        Err(e) => return Err(format!("the original is missing: {path} ({e})")),
    }
    // a link may name a photo and point at a program; what runs is what it points at
    if let Ok(target) = p.canonicalize()
        && !is_media(&target)
    {
        return Err(format!("{name} points at {}, which isn't a file type LightCraft reads", target.display()));
    }
    let absolute = std::path::absolute(p).map_err(|e| format!("{name}: {e}"))?;
    Ok(absolute.to_string_lossy().into_owned())
}

/// Whether `path`'s extension is one LightCraft reads (the Edit In copy formats included).
pub fn is_media(path: &Path) -> bool {
    crate::import::is_supported(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str) -> String {
        let p = dir.join(name);
        std::fs::write(&p, b"x").unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn only_file_types_lightcraft_reads_are_handed_over() {
        let dir = std::env::temp_dir().join(format!("lc-openable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for good in ["DSC_1.NEF", "a.jpg", "copy-Edit.tif", "copy-Edit.psd"] {
            let path = write(&dir, good);
            assert_eq!(openable(&path, "").unwrap(), path, "{good} is a photo LightCraft reads");
        }
        for bad in ["payload.exe", "shortcut.lnk", "run.bat", "page.hta", "go.command", "no-extension"] {
            let path = write(&dir, bad);
            let e = openable(&path, "").unwrap_err();
            assert!(e.contains(bad) && e.contains("file types LightCraft reads"), "{bad}: {e}");
        }
        // a bundle (macOS `.app`) and any other folder are not files
        let bundle = dir.join("Evil.app");
        std::fs::create_dir_all(&bundle).unwrap();
        let e = openable(&bundle.to_string_lossy(), "").unwrap_err();
        assert!(e.contains("isn't one of the file types") || e.contains("isn't a file"), "{e}");
        let e = openable(&dir.join("gone.jpg").to_string_lossy(), "").unwrap_err();
        assert!(e.contains("missing"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A relative path is made absolute: the opener runs with its own working directory.
    #[test]
    fn the_path_handed_over_is_absolute() {
        let dir = std::env::temp_dir().join(format!("lc-openable-abs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = write(&dir, "a.jpg");
        let dotted = Path::new(&path).parent().unwrap().join(".").join("a.jpg");
        let out = openable(&dotted.to_string_lossy(), "").unwrap();
        assert!(Path::new(&out).is_absolute(), "{out}");
        assert!(out.ends_with("a.jpg"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A link named like a photo that points at a program is refused: what the opener runs is
    /// the target, not the name.
    #[cfg(unix)]
    #[test]
    fn a_link_to_a_program_is_refused() {
        let dir = std::env::temp_dir().join(format!("lc-openable-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = write(&dir, "payload.command");
        let link = dir.join("holiday.jpg");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let e = openable(&link.to_string_lossy(), "").unwrap_err();
        assert!(e.contains("points at") && e.contains("payload.command"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
