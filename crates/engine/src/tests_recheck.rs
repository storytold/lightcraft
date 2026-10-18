//! Issue #662: preview-only raws are rechecked when a library opens in another app version.

use std::path::PathBuf;

use lightcraft_catalog::{APP_VERSION, Op, PhotoId};
use serde_json::json;

use crate::Session;
use crate::cmd::recheck;
use crate::files::tests::cr3_with_preview;

/// A version other than this build's: the flag was set by an older (or newer) LightCraft.
const OTHER: &str = "0.5.0";

struct Lib {
    s: Session,
    dir: PathBuf,
    lib: PathBuf,
    file: PathBuf,
    id: PhotoId,
}

impl Drop for Lib {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A library with one preview-only CR3, its flag stamped by [`OTHER`] as a library saved by
/// another version would be.
fn library(name: &str) -> Lib {
    let dir = std::env::temp_dir().join(format!("lc-recheck-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (lib, files) = (dir.join("lib"), dir.join("files"));
    std::fs::create_dir_all(&files).unwrap();
    let file = files.join("IMG_0001.cr3");
    std::fs::write(&file, cr3_with_preview(96, 64)).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let r = s.execute("library.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
    let id = PhotoId(r["imported"][0].as_u64().unwrap());
    let p = s.catalog.photo(id).unwrap().clone();
    assert!(p.preview_only.is_some() && !p.preview_only_stale(), "just imported: decided by this version");
    s.apply_system(Op::SetContent {
        id,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: p.content_hash.clone(),
        preview_only: p.preview_only.clone(),
        preview_checked: OTHER.into(),
    })
    .unwrap();
    assert!(s.catalog.photo(id).unwrap().preview_only_stale());
    Lib { s, dir, lib, file, id }
}

fn decodable_now(x: &Lib) {
    std::fs::write(&x.file, crate::tests_xmp::synthetic_dng_with(None, lightcraft_meta::Metadata::default())).unwrap();
}

/// The acceptance test: a stale flag on a file that decodes now is cleared without a manual
/// Reload, adds no undo step, leaves develop settings and history alone, and survives a reopen.
#[test]
fn stale_flag_on_a_file_that_decodes_now_is_cleared() {
    let mut x = library("decodes");
    x.s.execute("develop.set", &json!({"ids": [x.id.0], "control": "light.exposure", "value": 0.5})).unwrap();
    let before = x.s.catalog.photo(x.id).unwrap().clone();
    let undo = x.s.undo.len();
    decodable_now(&x);
    let r = x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap();
    assert_eq!(r, json!({"checked": 1, "updated": [x.id.0], "cleared": [x.id.0]}));
    let p = x.s.catalog.photo(x.id).unwrap().clone();
    assert_eq!(p.preview_only, None);
    assert_eq!(p.preview_checked, "", "the stamp goes with the flag");
    assert!(p.develops_raw());
    assert_eq!(x.s.undo.len(), undo, "not an undo step");
    assert_eq!(*p.develop, *before.develop);
    assert_eq!(p.history.len(), before.history.len());
    // journaled: a reopen keeps it
    x.s.persist().unwrap();
    x.s = Session::new().with_fs();
    x.s.open_library(&x.lib, false).unwrap();
    assert_eq!(x.s.catalog.photo(x.id).unwrap().preview_only, None);
}

/// A file that still doesn't decode is stamped by this version (with the decoder's current
/// reason), so the next open doesn't probe it again.
#[test]
fn still_undecodable_file_gets_a_fresh_stamp_and_isnt_probed_again() {
    let mut x = library("still");
    let r = x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap();
    assert_eq!(r, json!({"checked": 1, "updated": [x.id.0], "cleared": []}));
    let p = x.s.catalog.photo(x.id).unwrap();
    assert!(p.preview_only.is_some());
    assert_eq!(p.preview_checked, APP_VERSION);
    assert_eq!(x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap()["checked"], 0);
    // `all` ignores stamps (testing a decoder build of the same version)
    assert_eq!(x.s.execute("library.recheckPreviewOnly", &json!({"all": true})).unwrap()["checked"], 1);
}

/// A file that can't be read (unplugged drive) changes nothing and is tried again next time.
#[test]
fn missing_file_keeps_its_flag_and_stamp_and_is_retried() {
    let mut x = library("missing");
    std::fs::remove_file(&x.file).unwrap();
    let r = x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap();
    assert_eq!(r, json!({"checked": 1, "updated": [], "cleared": []}));
    let p = x.s.catalog.photo(x.id).unwrap();
    assert!(p.preview_only.is_some());
    assert_eq!(p.preview_checked, OTHER);
    // back again, and decodable now
    decodable_now(&x);
    assert_eq!(x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap()["cleared"], json!([x.id.0]));
}

/// A photo that changed between the probe and applying it (here: relinked) is left alone.
#[test]
fn photo_changed_while_probing_is_skipped() {
    let mut x = library("changed");
    let listed = recheck::stale(&x.s.catalog, false);
    assert_eq!(listed.iter().map(|i| i.id).collect::<Vec<_>>(), vec![x.id]);
    let probed = recheck::probe(&recheck::file_probe(&x.s).unwrap(), listed);
    decodable_now(&x);
    let other = x.dir.join("files").join("IMG_0001-moved.cr3");
    std::fs::write(&other, cr3_with_preview(96, 64)).unwrap();
    x.s.execute("photo.relink", &json!({"id": x.id.0, "path": other.to_string_lossy()})).unwrap();
    let relinked = x.s.catalog.photo(x.id).unwrap().clone();
    assert_eq!(recheck::apply(&mut x.s, probed).unwrap(), recheck::Rechecked::default());
    assert_eq!(*x.s.catalog.photo(x.id).unwrap().clone(), *relinked);
}

/// Virtual copies share the file: they follow the photo they were copied from.
#[test]
fn virtual_copies_follow_their_photo() {
    let mut x = library("copies");
    let r = x.s.execute("photo.virtualCopy", &json!({"ids": [x.id.0]})).unwrap();
    let copy = PhotoId(r["ids"][0].as_u64().unwrap());
    assert!(x.s.catalog.photo(copy).unwrap().preview_only.is_some());
    assert_eq!(recheck::stale(&x.s.catalog, true).len(), 1, "copies aren't probed on their own");
    decodable_now(&x);
    let r = x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap();
    assert_eq!(r["cleared"], json!([x.id.0, copy.0]));
    assert!(x.s.catalog.photo(copy).unwrap().develops_raw());
}

/// Without file access every probe would "succeed" knowing nothing and clear every flag: refused.
#[test]
fn session_without_file_access_refuses() {
    let mut x = library("nofs");
    x.s.media.file_probe = None;
    let e = x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap_err().to_string();
    assert!(e.contains("no file access"), "{e}");
    assert!(x.s.catalog.photo(x.id).unwrap().preview_only.is_some());
}
