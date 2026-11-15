//! Never write over an original: exports, renders and other files the app writes for the user
//! must not land on a catalogued photo's own file (or its XMP sidecar), whatever the conflict
//! setting or exact path asked for. [`OriginalGuard`] is a snapshot of the library's originals
//! that answers "would writing here replace one?" by file identity (the same file under another
//! spelling, through `..`, a link or a case-insensitive volume counts), not by comparing strings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lcc_catalog::safe_file::same_file;
use lcc_catalog::{Catalog, Source};

/// The library's originals and their sidecars, looked up by file name.
#[derive(Debug, Default)]
pub struct OriginalGuard {
    /// lower-case file name → (path on disk, what it is: "the original of IMG_1.CR3", …)
    by_name: HashMap<String, Vec<(PathBuf, String)>>,
    /// The entries of `by_name` that are symlinks, by the lower-case name of the file each one
    /// resolves to, found on the first check that needs them from folder listings (one `read_dir`
    /// per folder, no per-file open: a `read_link` per original took minutes on Windows).
    links: std::sync::OnceLock<HashMap<String, Vec<(PathBuf, String)>>>,
}

impl OriginalGuard {
    /// The originals of every photo in `catalog`.
    pub fn new(catalog: &Catalog) -> Self {
        let mut g = Self::default();
        for p in catalog.photos() {
            if let Source::File { path } = &p.source {
                g.add(Path::new(path), &p.file_name);
            }
        }
        g
    }

    /// Protect `path` (an original named `label`) and its sidecars (`IMG_1.xmp`, `IMG_1.CR3.xmp`).
    pub fn add(&mut self, path: &Path, label: &str) {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else { return };
        self.links = std::sync::OnceLock::new();
        let mut put = |file: String, p: PathBuf, what: String| self.by_name.entry(file.to_lowercase()).or_default().push((p, what));
        put(name.clone(), path.to_path_buf(), format!("the original of {label}"));
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        put(format!("{stem}.xmp"), path.with_extension("xmp"), format!("the XMP sidecar of {label}"));
        put(format!("{name}.xmp"), PathBuf::from(format!("{}.xmp", path.to_string_lossy())), format!("the XMP sidecar of {label}"));
        // every other spelling the reader accepts (`.XMP`, `.Xmp`, a recased photo extension)
        for c in crate::sidecar::sidecar_candidates(&path.to_string_lossy(), crate::sidecar::SidecarNaming::Stem) {
            if let Some(file) = c.file_name().map(|n| n.to_string_lossy().to_string()) {
                put(file, c, format!("the XMP sidecar of {label}"));
            }
        }
    }

    /// `Err` (a message for the user) when writing `target` would replace a protected file, or
    /// create a protected sidecar that isn't there yet.
    pub fn check(&self, target: &Path) -> Result<(), String> {
        if std::fs::symlink_metadata(target).is_err() {
            return self.check_new(target); // nothing there to replace
        }
        let mut names: Vec<String> = target.file_name().map(|n| n.to_string_lossy().to_lowercase()).into_iter().collect();
        if let Some(n) = std::fs::canonicalize(target).ok().and_then(|c| c.file_name().map(|n| n.to_string_lossy().to_lowercase())) {
            names.push(n);
        }
        for n in &names {
            for (p, what) in self.by_name.get(n).into_iter().flatten() {
                if same_file(p, target) {
                    return Err(format!(
                        "{} is {what} in the library: The app never writes over an original (choose another folder or file name)",
                        target.display()
                    ));
                }
            }
        }
        // An imported symlink can have a different name from its original. A write to that
        // original would change what the library reads through the link. A symlink resolves to the
        // same file as the target, so it has the target's resolved name: look the target's own
        // names up in the links, found once per guard, not once per target.
        let links = self.links.get_or_init(|| self.resolve_links());
        for n in &names {
            for (p, what) in links.get(n).into_iter().flatten() {
                if same_file(p, target) {
                    return Err(format!(
                        "{} is {what} in the library: The app never writes over an original (choose another folder or file name)",
                        target.display()
                    ));
                }
            }
        }
        Ok(())
    }

    /// The indexed entries that are symlinks, keyed by the lower-case name of the file each one
    /// resolves to. Each folder is listed once and only the entries the listing reports as
    /// symlinks are resolved. A folder that is there but can't be listed has each of its entries
    /// asked instead; a folder that is gone, and a dangling link, have nothing to replace.
    fn resolve_links(&self) -> HashMap<String, Vec<(PathBuf, String)>> {
        let mut by_folder: HashMap<&Path, Vec<(String, &PathBuf, &String)>> = HashMap::new();
        for (p, what) in self.by_name.values().flatten() {
            if let Some(name) = p.file_name() {
                by_folder.entry(folder_of(p)).or_default().push((name.to_string_lossy().to_lowercase(), p, what));
            }
        }
        let mut out: HashMap<String, Vec<(PathBuf, String)>> = HashMap::new();
        for (folder, entries) in by_folder {
            let listed = std::fs::read_dir(folder).and_then(|listing| {
                let mut links = std::collections::HashSet::new();
                for e in listing {
                    let e = e?;
                    // an entry whose type can't be read is resolved below, like a link
                    if e.file_type().map_or(true, |t| t.is_symlink()) {
                        links.insert(e.file_name().to_string_lossy().to_lowercase());
                    }
                }
                Ok(links)
            });
            let links = match listed {
                Ok(links) if links.is_empty() => continue,
                Ok(links) => Some(links),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => None,
            };
            for (name, p, what) in entries {
                let is_link = match &links {
                    Some(links) => links.contains(&name),
                    None => std::fs::read_link(p).is_ok(),
                };
                if !is_link {
                    continue;
                }
                let Some(resolved) = std::fs::canonicalize(p).ok().and_then(|c| c.file_name().map(|n| n.to_string_lossy().to_lowercase())) else {
                    continue;
                };
                out.entry(resolved).or_default().push((p.clone(), what.clone()));
            }
        }
        out
    }

    /// A new file with a sidecar's name in that sidecar's folder would be read back as the
    /// photo's edits. The folders are compared by identity, so `sub/../` or a link counts.
    fn check_new(&self, target: &Path) -> Result<(), String> {
        if !is_xmp(target) {
            return Ok(());
        }
        let Some(name) = target.file_name().map(|n| n.to_string_lossy().to_lowercase()) else { return Ok(()) };
        for (p, what) in self.by_name.get(&name).into_iter().flatten() {
            if same_file(folder_of(p), folder_of(target)) {
                return Err(format!("{} would become {what} in the library (choose another folder or file name)", target.display()));
            }
        }
        Ok(())
    }
}

fn is_xmp(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("xmp"))
}

/// The folder `path` is in (`.` for a bare file name).
fn folder_of(path: &Path) -> &Path {
    path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

impl crate::Session {
    /// A guard over this library's originals (see [`OriginalGuard`]).
    pub fn original_guard(&self) -> OriginalGuard {
        OriginalGuard::new(&self.catalog)
    }

    /// `Err` when writing `path` would replace a catalogued photo's original or sidecar, or
    /// create one of its sidecars. A free path that isn't an `.xmp` (the usual case) is answered
    /// without building the guard over the whole library.
    pub fn check_write_target(&self, path: &str) -> Result<(), String> {
        let path = Path::new(path);
        if std::fs::symlink_metadata(path).is_err() {
            // `missing/../IMG_1.jpg` names an existing file once the writer creates `missing`, so a `..` that cannot
            // be resolved now cannot be checked: refuse it.
            if path.components().any(|c| c == std::path::Component::ParentDir) && std::fs::canonicalize(folder_of(path)).is_err() {
                return Err(format!("{}: the folder part has `..` components that cannot be resolved; use a path without them", path.display()));
            }
            if !is_xmp(path) {
                return Ok(()); // nothing there to replace, and no sidecar to create
            }
        }
        self.original_guard().check(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcc_catalog::{Op, Photo, PhotoId};

    #[test]
    fn originals_and_their_sidecars_are_protected_by_identity() {
        let d = std::env::temp_dir().join(format!("lc-originals-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let orig = d.join("IMG_1.jpg");
        std::fs::write(&orig, b"original").unwrap();
        std::fs::write(d.join("IMG_1.xmp"), b"<x/>").unwrap();
        std::fs::write(d.join("other.jpg"), b"other").unwrap();
        let mut c = Catalog::default();
        let p = Photo::new(PhotoId(1), Source::File { path: orig.to_string_lossy().to_string() }, "IMG_1.jpg", "JPEG", 10, 10, "2026-01-01");
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        let g = OriginalGuard::new(&c);
        assert!(g.check(&orig).unwrap_err().contains("original of IMG_1.jpg"));
        assert!(g.check(&d.join("sub/../IMG_1.jpg")).is_err(), "another spelling of the same file");
        assert!(g.check(&d.join("IMG_1.xmp")).unwrap_err().contains("sidecar"));
        assert!(g.check(&d.join("other.jpg")).is_ok(), "an ordinary existing file");
        assert!(g.check(&d.join("new.jpg")).is_ok(), "a free name");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An export overwrite found symlinked originals with a `read_link` per original (minutes in a
    /// large Windows library). They now come from folder listings, and a original symlinked under
    /// another name is still refused.
    #[cfg(unix)]
    #[test]
    fn an_original_symlinked_under_another_name_is_found_from_folder_listings() {
        let d = std::env::temp_dir().join(format!("lc-originals-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("lib")).unwrap();
        std::fs::create_dir_all(d.join("out")).unwrap();
        let real = d.join("out/export.jpg");
        std::fs::write(&real, b"pixels").unwrap();
        std::fs::write(d.join("out/other.jpg"), b"other").unwrap();
        let link = d.join("lib/IMG_1.jpg");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mut c = Catalog::default();
        let p = Photo::new(PhotoId(1), Source::File { path: link.to_string_lossy().to_string() }, "IMG_1.jpg", "JPEG", 10, 10, "2026-01-01");
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        // originals in folders that aren't there have nothing to replace
        for i in 0..200 {
            let p = Photo::new(
                PhotoId(i + 2),
                Source::File { path: d.join(format!("gone/{}/IMG_{i}.jpg", i % 5)).to_string_lossy().to_string() },
                &format!("IMG_{i}.jpg"),
                "JPEG",
                10,
                10,
                "2026-01-01",
            );
            c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        }
        let g = OriginalGuard::new(&c);
        assert!(g.check(&real).unwrap_err().contains("original of IMG_1.jpg"), "the file the library reads through the link");
        assert!(g.check(&d.join("out/other.jpg")).is_ok(), "an ordinary existing file");
        let links = g.links.get().unwrap_or(&HashMap::new()).values().flatten().count();
        assert_eq!(links, 1, "only the symlink is resolved");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The symlink index is built by the first check; an original added after it must still be
    /// found, so `add` drops the index and the next check builds it again.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_original_added_after_a_check_is_found_by_the_next_one() {
        let d = std::env::temp_dir().join(format!("lc-originals-symlink-late-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("lib")).unwrap();
        std::fs::create_dir_all(d.join("out")).unwrap();
        let real = d.join("out/export.jpg");
        std::fs::write(&real, b"pixels").unwrap();
        let plain = d.join("lib/IMG_0.jpg");
        std::fs::write(&plain, b"plain").unwrap();
        let link = d.join("lib/IMG_1.jpg");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mut g = OriginalGuard::default();
        g.add(&plain, "IMG_0.jpg");
        assert!(g.check(&real).is_ok(), "nothing in the library reads this file yet");
        assert!(g.links.get().is_some(), "the first check built the index");
        g.add(&link, "IMG_1.jpg");
        assert!(g.links.get().is_none(), "adding an original drops the index");
        assert!(g.check(&real).unwrap_err().contains("original of IMG_1.jpg"), "the index built again sees the new link");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A symlinked original whose file is gone has nothing a write could replace through it: an
    /// export over an ordinary file next to where it pointed goes ahead, and nothing panics.
    #[cfg(unix)]
    #[test]
    fn a_dangling_symlinked_original_refuses_nothing() {
        let d = std::env::temp_dir().join(format!("lc-originals-dangling-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("lib")).unwrap();
        std::fs::create_dir_all(d.join("out")).unwrap();
        let other = d.join("out/other.jpg");
        std::fs::write(&other, b"other").unwrap();
        let link = d.join("lib/IMG_1.jpg");
        std::os::unix::fs::symlink(d.join("out/missing.jpg"), &link).unwrap();
        let mut g = OriginalGuard::default();
        g.add(&link, "IMG_1.jpg");
        assert!(g.check(&other).is_ok(), "an ordinary existing file");
        assert!(g.check(&d.join("out/missing.jpg")).is_ok(), "the link's missing file is a free name");
        let links = g.links.get().expect("built by the first existing target");
        assert!(links.is_empty(), "a dangling link resolves to no file: {links:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A hard link with another name shares no name with its original, and the write check looks
    /// up only the target's own names (it never compares the target with every original), so it
    /// lets such a link through; the original itself is still refused. This guard has no
    /// identity check that scans every original.
    #[cfg(unix)]
    #[test]
    fn a_hard_link_under_another_name_is_not_scanned_for_by_the_write_check() {
        let d = std::env::temp_dir().join(format!("lc-originals-hard-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("lib")).unwrap();
        std::fs::create_dir_all(d.join("out")).unwrap();
        let original = d.join("lib/IMG_1.jpg");
        std::fs::write(&original, b"original").unwrap();
        let linked = d.join("out/export.jpg");
        std::fs::hard_link(&original, &linked).unwrap();
        let mut g = OriginalGuard::default();
        g.add(&original, "IMG_1.jpg");
        assert!(g.check(&linked).is_ok(), "an export overwrite looks up only the target's own names");
        assert!(g.check(&original).unwrap_err().contains("original of IMG_1.jpg"), "the original itself is still refused");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sidecars_the_reader_accepts_in_other_cases_are_protected() {
        let d = std::env::temp_dir().join(format!("lc-originals-case-sidecar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let orig = d.join("IMG_1.jpg");
        std::fs::write(&orig, b"original").unwrap();
        let mut g = OriginalGuard::default();
        g.add(&orig, "IMG_1.jpg");
        for name in ["IMG_1.XMP", "IMG_1.Xmp", "IMG_1.JPG.xmp", "IMG_1.jpg.XMP"] {
            std::fs::write(d.join(name), b"<x/>").unwrap();
            assert!(g.check(&d.join(name)).unwrap_err().contains("sidecar"), "{name}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_sidecar_that_isnt_there_yet_is_protected_too() {
        let d = std::env::temp_dir().join(format!("lc-originals-new-sidecar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let orig = d.join("IMG_1.jpg");
        std::fs::write(&orig, b"original").unwrap();
        let mut c = Catalog::default();
        let p = Photo::new(PhotoId(1), Source::File { path: orig.to_string_lossy().to_string() }, "IMG_1.jpg", "JPEG", 10, 10, "2026-01-01");
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        let g = OriginalGuard::new(&c);
        assert!(g.check(&d.join("IMG_1.xmp")).unwrap_err().contains("XMP sidecar of IMG_1.jpg"));
        assert!(g.check(&d.join("IMG_1.jpg.xmp")).is_err(), "the appended spelling");
        assert!(g.check(&d.join("sub/../IMG_1.xmp")).is_err(), "another spelling of the folder");
        assert!(g.check(&d.join("sub/IMG_1.xmp")).is_ok(), "the same name in another folder");
        assert!(g.check(&d.join("other.xmp")).is_ok(), "no photo of that name");
        assert!(g.check(&d.join("missing/IMG_1.xmp")).is_ok(), "a folder that isn't there");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// `..` through a folder that does not exist yet could reach an original once the writer creates it.
    #[test]
    fn dot_dot_through_a_missing_folder_is_refused() {
        let d = std::env::temp_dir().join(format!("lc-originals-dotdot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let orig = d.join("IMG_1.jpg");
        std::fs::write(&orig, b"original").unwrap();
        let mut s = crate::Session::new();
        let p = Photo::new(PhotoId(1), Source::File { path: orig.to_string_lossy().to_string() }, "IMG_1.jpg", "JPEG", 10, 10, "2026-01-01");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        let error = s.check_write_target(&d.join("missing/../IMG_1.jpg").to_string_lossy()).unwrap_err();
        // Windows resolves `..` lexically and finds the original; Unix cannot resolve it and refuses
        assert!(error.contains("cannot be resolved") || error.contains("never writes over an original"), "{error}");
        assert!(s.check_write_target(&d.join("missing/new.jpg").to_string_lossy()).is_ok(), "a free name in a missing folder");
        assert!(!d.join("missing").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
