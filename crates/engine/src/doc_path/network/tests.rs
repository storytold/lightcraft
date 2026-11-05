//! Synthetic filesystem regressions: no user catalog or original is accessed.

use super::*;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) struct Fixture(pub(crate) PathBuf);

impl Fixture {
    /// Make disjoint selected and unselected directories containing distinguishable data.
    pub(crate) fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!("lc-unc-handles-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("selected")).unwrap();
        std::fs::create_dir(root.join("selected/safe")).unwrap();
        std::fs::create_dir(root.join("unselected")).unwrap();
        std::fs::write(root.join("selected/safe/photo.txt"), b"selected").unwrap();
        std::fs::write(root.join("unselected/photo.txt"), b"unselected").unwrap();
        Self(root)
    }

    /// Open only the test's explicitly selected root, preserving the production share flags.
    fn root(&self) -> File {
        std::fs::OpenOptions::new()
            .access_mode(READ_ATTRIBUTES | TRAVERSE)
            .share_mode(SHARE_READ_WRITE)
            .custom_flags(OPEN_REPARSE_POINT | BACKUP_SEMANTICS)
            .open(self.0.join("selected"))
            .unwrap()
    }

    /// Address this fixture through the machine's optional loopback administrative share.
    pub(crate) fn unc(&self) -> Option<PathBuf> {
        let drive_root = self.0.ancestors().last()?;
        let Some(Component::Prefix(prefix)) = drive_root.components().next() else { return None };
        let std::path::Prefix::Disk(drive) = prefix.kind() else { return None };
        Some(PathBuf::from(format!(r"\\localhost\{}$", char::from(drive))).join(self.0.strip_prefix(drive_root).ok()?))
    }
}

impl Drop for Fixture {
    /// Delete only this fixture; unlink junctions before removing its ordinary directories.
    fn drop(&mut self) {
        for name in ["selected/redirect", "selected/safe"] {
            let link = self.0.join(name);
            if junction::exists(&link).unwrap_or(false) {
                let _ = junction::delete(&link);
            }
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Both intermediate and final junctions are rejected before any target bytes can be read.
#[test]
fn directory_and_leaf_reparse_points_are_refused() {
    let f = Fixture::new();
    junction::create(f.0.join("unselected"), f.0.join("selected/redirect")).unwrap();
    for name in ["redirect/photo.txt", "redirect"] {
        let parts: Vec<_> = Path::new(name).components().collect();
        let mut visited = Vec::new();
        let error = descend(f.root(), &parts, true, |index, _| visited.push(index)).unwrap_err();
        assert!(super::super::is_network_redirect(&error), "{error}");
        assert!(visited.is_empty(), "a rejected entry must never authorize a descendant");
    }
}

/// A replacement attempted between the directory check and child open cannot change the read.
#[test]
fn directory_replacement_during_handle_walk_is_blocked() {
    let f = Fixture::new();
    let parts: Vec<_> = Path::new("safe/photo.txt").components().collect();
    let mut attempted = false;
    let mut file = descend(f.root(), &parts, true, |index, _| {
        if index == 0 {
            attempted = true;
            assert!(std::fs::rename(f.0.join("selected/safe"), f.0.join("selected/moved")).is_err());
        }
    })
    .unwrap();
    assert!(attempted);
    let mut bytes = String::new();
    file.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "selected");
    drop(file);
    std::fs::rename(f.0.join("selected/safe"), f.0.join("selected/moved")).unwrap();
}

/// Once opened, a read uses that handle rather than resolving the file's name again.
#[test]
fn leaf_replacement_cannot_change_an_open_read() {
    let f = Fixture::new();
    let parts: Vec<_> = Path::new("safe/photo.txt").components().collect();
    let mut file = descend(f.root(), &parts, true, |_, _| {}).unwrap();
    let original = f.0.join("selected/safe/photo.txt");
    let moved = f.0.join("selected/safe/moved.txt");
    assert!(std::fs::rename(&original, &moved).is_err());
    let mut bytes = String::new();
    file.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "selected");
    drop(file);
    std::fs::rename(original, moved).unwrap();
}

/// The production UNC entry point reads ordinary files and refuses redirects over real SMB.
#[test]
fn loopback_smb_reads_only_non_redirected_originals() {
    let f = Fixture::new();
    let Some(unc) = f.unc().filter(|p| std::fs::metadata(p).is_ok()) else {
        lightcraft_testkit::skip!(System, "loopback administrative share unavailable");
        return;
    };
    let ordinary = unc.join("selected/safe/photo.txt");
    assert_eq!(super::super::read_original(&ordinary).unwrap(), b"selected");
    assert!(super::super::original_metadata(&ordinary).unwrap().is_file());
    junction::create(f.0.join("unselected"), f.0.join("selected/redirect")).unwrap();
    for path in [unc.join("selected/redirect/photo.txt"), unc.join("selected/redirect")] {
        assert!(super::super::is_network_redirect(&open(&path, true).unwrap_err()));
        assert!(super::super::is_network_redirect(&open(&path, false).unwrap_err()));
    }
}

/// Explicit symlinks are covered when Windows grants the test process link-creation privileges.
#[test]
fn loopback_smb_refuses_directory_and_file_symlinks() {
    let f = Fixture::new();
    let Some(unc) = f.unc().filter(|p| std::fs::metadata(p).is_ok()) else {
        lightcraft_testkit::skip!(System, "loopback administrative share unavailable");
        return;
    };
    let directory = f.0.join("selected/symlink");
    if let Err(error) = std::os::windows::fs::symlink_dir(r"\\localhost\lightcraft-unselected", &directory) {
        lightcraft_testkit::skip!(System, "Windows symlink creation unavailable: {error}");
        return;
    }
    let leaf = f.0.join("selected/leaf.txt");
    std::os::windows::fs::symlink_file(r"\\localhost\lightcraft-unselected\photo.txt", &leaf).unwrap();
    for path in [unc.join("selected/symlink/photo.txt"), unc.join("selected/leaf.txt")] {
        assert!(super::super::is_network_redirect(&open(&path, true).unwrap_err()));
        assert!(super::super::is_network_redirect(&open(&path, false).unwrap_err()));
    }
}

/// Non-Unicode UNC names are refused before the unchecked local-path fallback can run.
#[test]
fn non_unicode_unc_does_not_bypass_the_handle_walk() {
    use std::os::windows::ffi::OsStringExt;
    let mut wide: Vec<_> = r"\\localhost\lightcraft-unselected\".encode_utf16().collect();
    wide.push(0xd800);
    let path = PathBuf::from(std::ffi::OsString::from_wide(&wide));
    assert!(is_unc(&path));
    assert_eq!(super::super::open_original(&path).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    assert_eq!(super::super::original_metadata(&path).unwrap_err().kind(), io::ErrorKind::InvalidInput);
}

/// Catalog-driven probes and XMP reads use safe handles even when a caller supplied a path-based hook.
#[test]
fn unc_import_reads_original_and_sidecar_without_an_unchecked_probe() {
    use crate::import::{ImportJob, ImportMode, ImportOptions};
    use lightcraft_codecs::{EncodeImage, EncodeMeta, Samples};
    use std::sync::{Arc, atomic::AtomicBool};
    let f = Fixture::new();
    let Some(unc) = f.unc().filter(|p| std::fs::metadata(p).is_ok()) else {
        lightcraft_testkit::skip!(System, "loopback administrative share unavailable");
        return;
    };
    let png = lightcraft_codecs::encode_png(&EncodeImage::new(1, 1, 4, Samples::U8(&[20, 40, 80, 255])), &EncodeMeta::default()).unwrap();
    let photo = f.0.join("selected/safe/photo.png");
    std::fs::write(&photo, &png).unwrap();
    let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="4"/></rdf:RDF></x:xmpmeta>"#;
    std::fs::write(f.0.join("selected/safe/photo.xmp"), xmp).unwrap();
    let called = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&called);
    let mut session = crate::Session::new().with_fs();
    session.media.file_probe = Some(Arc::new(move |_| {
        seen.store(true, Ordering::Relaxed);
        Err("unchecked path hook must not run for a catalog UNC original".into())
    }));
    let options = ImportOptions { mode: ImportMode::Add, ..Default::default() };
    let mut job = ImportJob::new(&mut session, options.clone()).unwrap();
    job.check_network_links();
    let prepared = job.prepare_files(vec![unc.join("selected/safe/photo.png").to_string_lossy().into_owned()], &AtomicBool::new(false));
    let report = crate::import::commit_prepared(&mut session, &options, "2026-01-01T00:00:00", prepared).unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(report.imported.len(), 1);
    assert_eq!(report.sidecars, 1);
    assert_eq!(session.catalog.photo(lightcraft_catalog::PhotoId(report.imported[0])).unwrap().rating, 4);
    assert!(!called.load(Ordering::Relaxed));
    assert_eq!(std::fs::read(photo).unwrap(), png);
}

/// A region packet already read from a sidecar cannot authorize a redirected orientation reread.
#[test]
fn region_orientation_reread_refuses_replaced_directory() {
    use crate::sidecar::{SidecarLookup, SidecarNaming};
    use lightcraft_catalog::MediaKind;
    use lightcraft_meta::Orientation;
    let f = Fixture::new();
    let Some(unc) = f.unc().filter(|p| std::fs::metadata(p).is_ok()) else {
        lightcraft_testkit::skip!(System, "loopback administrative share unavailable");
        return;
    };
    for (folder, orientation) in [("selected/safe", Orientation::Rotate90), ("unselected", Orientation::Rotate180)] {
        let image = lightcraft_raster::Rgba8::from_fn(32, 24, |_, _| [20, 40, 80, 255]);
        let exif = lightcraft_meta::write_exif(&lightcraft_meta::Metadata { orientation: Some(orientation), ..Default::default() });
        let bytes = lightcraft_codecs::encode_jpeg(
            &lightcraft_codecs::EncodeImage::rgba8(&image),
            90,
            Default::default(),
            &lightcraft_codecs::EncodeMeta { exif: Some(&exif), ..Default::default() },
        )
        .unwrap();
        std::fs::write(f.0.join(folder).join("photo.jpg"), bytes).unwrap();
    }
    let packet = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
      <rdf:Description xmlns:mwg-rs="http://www.metadataworkinggroup.com/schemas/regions/"
        xmlns:stDim="http://ns.adobe.com/xap/1.0/sType/Dimensions#" xmlns:stArea="http://ns.adobe.com/xmp/sType/Area#">
        <mwg-rs:Regions rdf:parseType="Resource"><mwg-rs:AppliedToDimensions rdf:parseType="Resource" stDim:w="32" stDim:h="24" stDim:unit="pixel"/>
        <mwg-rs:RegionList><rdf:Bag><rdf:li rdf:parseType="Resource" mwg-rs:Name="Example" mwg-rs:Type="Face">
        <mwg-rs:Area rdf:parseType="Resource" stArea:x="0.25" stArea:y="0.4" stArea:w="0.1" stArea:h="0.15" stArea:unit="normalized"/>
        </rdf:li></rdf:Bag></mwg-rs:RegionList></mwg-rs:Regions></rdf:Description></rdf:RDF></x:xmpmeta>"#;
    std::fs::write(f.0.join("selected/safe/photo.xmp"), packet).unwrap();
    let original = unc.join("selected/safe/photo.jpg").to_string_lossy().into_owned();
    let lookup = SidecarLookup { naming: SidecarNaming::Stem, owns_stem: true };
    let (packet, _) = crate::sidecar::read_packet(&original, MediaKind::Image, lookup).unwrap();
    let sc = crate::sidecar::parse_sidecar(&packet, crate::crs::Target::Rendered).unwrap();
    assert!(sc.regions_drawn_on.is_some());
    assert_eq!(sc.clone().with_file_orientation(&original).orientation, Some(Orientation::Rotate90));
    std::fs::rename(f.0.join("selected/safe"), f.0.join("selected/before")).unwrap();
    junction::create(f.0.join("unselected"), f.0.join("selected/safe")).unwrap();
    assert_eq!(sc.with_file_orientation(&original).orientation, None, "must not read the redirected original's orientation");
}
