//! Edit In round trips (RT.B): Lightroom-style stacking of versions, the xmpMM:DerivedFrom link
//! that rebuilds stacks on re-import, and `wait` with an editor that rewrites the file.

use std::path::{Path, PathBuf};

use lcc_catalog::PhotoId;
use serde_json::json;

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lcc-editrt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_png(path: &Path, seed: u8, w: usize, h: usize) {
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 6) as u8, (i / w * 9) as u8, seed, 255]).collect();
    let img = lcc_raster::Rgba8 { width: w, height: h, data };
    let bytes = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn session(dir: &Path) -> Session {
    let mut s = Session::new().with_fs();
    s.workflow = crate::cmd::actions::Workflow::in_dir(Some(dir.join("settings")));
    s
}

fn import(s: &mut Session, p: &Path) -> PhotoId {
    let r = s.execute("library.import", &json!({"paths": [p.to_string_lossy()]})).unwrap();
    PhotoId(r["imported"][0].as_u64().unwrap())
}

fn edit(s: &mut Session, p: serde_json::Value) -> (PhotoId, PathBuf) {
    let r = s.execute("photo.editIn", &p).unwrap();
    (PhotoId(r["id"].as_u64().unwrap()), PathBuf::from(r["path"].as_str().unwrap()))
}

/// Editing a version that is already in a stack puts the new version directly above it in the
/// same stack (Lightroom), expanded and selected; an Edit Original of the PSD adds no photo.
#[test]
fn versions_stack_directly_above_their_source() {
    let dir = temp_dir("stack");
    let (a, b) = (dir.join("a.png"), dir.join("b.png"));
    write_png(&a, 10, 24, 16);
    write_png(&b, 90, 24, 16);
    let mut s = session(&dir);
    let ia = import(&mut s, &a);
    let ib = import(&mut s, &b);
    s.execute("stack.group", &json!({"ids": [ia.0, ib.0], "top": ia.0, "collapsed": true})).unwrap();
    // b is second in the stack: its edit goes between a and b
    let (e1, p1) = edit(&mut s, json!({"id": ib.0, "preset": "PhotoCraft"}));
    let st = s.catalog.stack_of(ib).unwrap();
    assert_eq!(st.photos, vec![ia, e1, ib]);
    assert!(!st.collapsed, "expanded so the new version shows");
    assert_eq!(s.active(), Some(e1));
    // an edit of the edit (rendered TIFF) lands directly above it
    let (e2, p2) = edit(&mut s, json!({"id": e1.0}));
    assert!(p2.to_string_lossy().ends_with("b-Edit-Edit.tif"), "{}", p2.display());
    assert_eq!(s.catalog.stack_of(ib).unwrap().photos, vec![ia, e2, e1, ib]);
    // Edit Original on the PSD: the same photo, no new one
    let n = s.catalog.photos().count();
    let r = s.execute("photo.editIn", &json!({"id": e1.0, "mode": "original"})).unwrap();
    assert_eq!((r["id"].as_u64(), s.catalog.photos().count()), (Some(e1.0), n));
    assert_eq!(r["path"].as_str().unwrap(), p1.to_string_lossy());
    assert_eq!(crate::cmd::edit_in::suggested_mode("x-Edit.psd"), crate::cmd::edit_in::Mode::Original);
    assert_eq!(crate::cmd::edit_in::suggested_mode("x.CR3"), crate::cmd::edit_in::Mode::CopyWithAdjustments);
    assert_eq!(s.catalog.stacks().count(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An edit of a virtual copy renders the copy's settings and stacks with the copy and its master.
#[test]
fn virtual_copy_edits_stack_with_the_master() {
    let dir = temp_dir("vc");
    let a = dir.join("a.png");
    write_png(&a, 10, 24, 16);
    let mut s = session(&dir);
    let master = import(&mut s, &a);
    let r = s.execute("photo.virtualCopy", &json!({})).unwrap();
    let vc = s.catalog.photos().find(|p| p.copy_of == Some(master)).map(|p| p.id).unwrap_or_else(|| panic!("no virtual copy: {r}"));
    s.selection = crate::Selection::single(vc);
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 2.0})).unwrap();
    s.execute("stack.ungroup", &json!({"ids": [vc.0, master.0]})).ok();
    let (e, _) = edit(&mut s, json!({"id": vc.0}));
    let st = s.catalog.stack_of(e).unwrap();
    assert_eq!(st.top(), e);
    assert!(st.photos.contains(&vc) && st.photos.contains(&master), "{:?}", st.photos);
    let (m, v) = (s.render_now(master, 8, 8).unwrap(), s.render_now(e, 8, 8).unwrap());
    let sum = |r: &lcc_pipeline::Rendered| r.image.data.iter().map(|p| p[1] as u32).sum::<u32>();
    assert!(sum(&v) > sum(&m), "the copy's +2 EV is in the edit");
    let _ = std::fs::remove_dir_all(&dir);
}

/// TIFF and PSD versions carry a DerivedFrom link to their source; importing the folder into an
/// empty library stacks them again, each on the photo it came from.
#[test]
fn reimport_rebuilds_stacks_from_derived_from() {
    let dir = temp_dir("relink");
    let a = dir.join("a.png");
    write_png(&a, 10, 24, 16);
    let mut s = session(&dir);
    let ia = import(&mut s, &a);
    let (e1, p1) = edit(&mut s, json!({"id": ia.0, "bitDepth": 8}));
    let (_, p2) = edit(&mut s, json!({"id": e1.0, "preset": "PhotoCraft"}));
    for p in [&p1, &p2] {
        let x = crate::cmd::edit_in::link::file_xmp(p).unwrap_or_else(|| panic!("no XMP in {}", p.display()));
        let l = crate::cmd::edit_in::link::read_link(&x);
        assert!(l.document_id.is_some() && l.derived_document.is_some(), "{l:?}");
    }
    let l2 = crate::cmd::edit_in::link::read_link(&crate::cmd::edit_in::link::file_xmp(&p2).unwrap());
    let l1 = crate::cmd::edit_in::link::read_link(&crate::cmd::edit_in::link::file_xmp(&p1).unwrap());
    assert_eq!(l2.derived_document, l1.document_id, "the PSD names the TIFF's own document id");
    assert_eq!(l1.derived_path.as_deref(), Some("a.png"));
    // a fresh library: one import of the whole folder
    let mut t = session(&dir.join("other"));
    let r = t.execute("library.import", &json!({"paths": [dir.to_string_lossy()]})).unwrap();
    assert_eq!(r["imported"].as_array().unwrap().len(), 3, "{r}");
    let by_name = |t: &Session, n: &str| t.catalog.photos().find(|p| p.file_name == n).unwrap().id;
    let (na, n1, n2) = (by_name(&t, "a.png"), by_name(&t, "a-Edit.tif"), by_name(&t, "a-Edit-Edit.psd"));
    assert_eq!(t.catalog.stack_of(na).map(|s| s.photos.clone()), Some(vec![n2, n1, na]));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `launch` + `wait`: the editor (a script that overwrites the file and exits 0) runs to the end,
/// the activity stack shows it meanwhile, and the photo is reloaded with the new pixels.
#[cfg(unix)]
#[test]
fn wait_runs_the_editor_and_reloads() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir("wait");
    let a = dir.join("a.png");
    write_png(&a, 10, 24, 16);
    let other = dir.join("replacement.bin");
    write_png(&other, 250, 32, 20);
    let script = dir.join("editor.sh");
    // the last argument is the file; `--wait` comes first
    std::fs::write(&script, format!("#!/bin/sh\nfor f; do :; done\ncp '{}' \"$f\"\nexit 0\n", other.display())).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut s = session(&dir);
    // this test runs a real stand-in editor script
    s.desktop = std::sync::Arc::new(crate::desktop::System);
    let ia = import(&mut s, &a);
    let r = s
        .execute(
            "photo.editIn",
            &json!({"id": ia.0, "mode": "copy", "app": script.to_string_lossy(), "args": ["--wait"], "launch": true, "wait": true}),
        )
        .unwrap();
    assert_eq!(r["opened"]["waited"], true, "{r}");
    assert_eq!(r["opened"]["saved"], true, "{r}");
    let id = PhotoId(r["id"].as_u64().unwrap());
    assert_eq!(r["opened"]["reloaded"], json!([id.0]), "{r}");
    let ph = s.catalog.photo(id).unwrap();
    assert_eq!((ph.width, ph.height), (32, 20), "the editor's save is read back");
    assert!(s.activity.list().is_empty(), "the activity row is gone");
    let _ = std::fs::remove_dir_all(&dir);
}
