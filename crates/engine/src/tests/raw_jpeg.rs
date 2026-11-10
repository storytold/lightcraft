//! RAW+JPEG pairs: `library.import`'s `pairRawJpeg`, the `import.pairRawJpeg` preference and
//! `stack.rawJpegPairs` (tiny generated files: a synthetic DNG and a small JPEG).

use std::path::{Path, PathBuf};

use lightcraft_catalog::{MediaKind, PhotoId};
use serde_json::{Value, json};

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-rawjpeg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_raw(path: &Path) {
    write_raw_n(path, 0);
}

/// A tiny DNG; `n` makes its bytes differ, so several aren't duplicates of each other.
fn write_raw_n(path: &Path, n: u8) {
    let mut bytes = crate::tests::xmp::synthetic_dng_with(None, lightcraft_meta::Metadata::default());
    bytes.push(n);
    std::fs::write(path, bytes).unwrap();
}

fn write_jpeg(path: &Path, seed: u8) {
    let (w, h) = (32usize, 24usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 7) as u8, (i / w * 9) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_jpeg(
        &lightcraft_codecs::EncodeImage::rgba8(&img),
        90,
        lightcraft_codecs::ChromaSubsampling::default(),
        &lightcraft_codecs::EncodeMeta::default(),
    )
    .unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// `IMG_0001.DNG` + `img_0001.jpg` (a pair), `IMG_0002.JPG` (alone), and `sub/IMG_0001.JPG` (an
/// export in a subfolder: not the camera's).
fn shoot(tag: &str) -> PathBuf {
    let d = temp_dir(tag);
    write_raw(&d.join("IMG_0001.DNG"));
    write_jpeg(&d.join("img_0001.jpg"), 1);
    write_jpeg(&d.join("IMG_0002.JPG"), 2);
    std::fs::create_dir_all(d.join("sub")).unwrap();
    write_jpeg(&d.join("sub").join("IMG_0001.JPG"), 3);
    d
}

fn id_named(s: &Session, folder: &Path, name: &str) -> PhotoId {
    let want = folder.join(name).to_string_lossy().to_string();
    s.catalog.photos().find(|p| matches!(&p.source, lightcraft_catalog::Source::File { path } if *path == want)).map(|p| p.id).unwrap()
}

fn import(s: &mut Session, dir: &Path, extra: Value) -> Value {
    let mut p = json!({"paths": [dir.to_string_lossy()]});
    if let (Some(p), Some(e)) = (p.as_object_mut(), extra.as_object()) {
        p.extend(e.clone());
    }
    s.execute("library.import", &p).unwrap()
}

#[test]
fn import_with_pairing_stacks_each_pair_as_one_photo_in_one_undo_step() {
    let dir = shoot("import");
    let mut s = Session::new().with_fs();
    let r = import(&mut s, &dir, json!({"pairRawJpeg": true}));
    assert_eq!(r["imported"].as_array().unwrap().len(), 4);
    assert_eq!(r["paired"], 1);
    let raw = id_named(&s, &dir, "IMG_0001.DNG");
    let jpg = id_named(&s, &dir, "img_0001.jpg");
    assert_eq!(s.catalog.photo(raw).unwrap().kind, MediaKind::Raw);
    let st = s.catalog.stack_of(raw).unwrap();
    assert_eq!(st.photos, vec![raw, jpg], "raw on top");
    assert!(st.collapsed);
    assert_eq!(s.catalog.stacks().count(), 1, "the lone JPEG and the subfolder export stay single");
    assert_eq!(s.visible_cloned().len(), 3, "the pair shows as one photo");
    // one undo step takes the whole import back, the stack with it
    assert_eq!(s.undo.len(), 1);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.len(), 0);
    assert_eq!(s.catalog.stacks().count(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_without_pairing_keeps_both_files_as_photos() {
    let dir = shoot("off");
    let mut s = Session::new().with_fs();
    let r = import(&mut s, &dir, json!({}));
    assert!(r.get("paired").is_none());
    assert_eq!(s.catalog.stacks().count(), 0, "off by default: nothing changes for existing users");
    assert_eq!(s.visible_cloned().len(), 4);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_library_preference_is_the_imports_default() {
    let dir = shoot("pref");
    let mut s = Session::new().with_fs();
    let prefs = s.execute("library.preferences", &json!({"import": {"pairRawJpeg": true}})).unwrap();
    assert_eq!(prefs["import"]["pairRawJpeg"], true);
    let r = import(&mut s, &dir, json!({}));
    assert_eq!(r["paired"], 1);
    // an explicit param wins over the preference
    let dir2 = shoot("pref-off");
    let r = import(&mut s, &dir2, json!({"pairRawJpeg": false}));
    assert!(r.get("paired").is_none());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

#[test]
fn a_jpeg_imported_after_its_raw_joins_it() {
    let dir = temp_dir("later");
    write_raw(&dir.join("DSC_0001.DNG"));
    let mut s = Session::new().with_fs();
    import(&mut s, &dir.join("DSC_0001.DNG"), json!({"pairRawJpeg": true}));
    write_jpeg(&dir.join("DSC_0001.JPG"), 4);
    let r = import(&mut s, &dir.join("DSC_0001.JPG"), json!({"pairRawJpeg": true}));
    assert_eq!(r["paired"], 1);
    let raw = id_named(&s, &dir, "DSC_0001.DNG");
    assert_eq!(s.catalog.stack_of(raw).unwrap().photos.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stack_raw_jpeg_pairs_fixes_a_library_imported_before() {
    let dir = shoot("command");
    let mut s = Session::new().with_fs();
    import(&mut s, &dir, json!({}));
    let raw = id_named(&s, &dir, "IMG_0001.DNG");
    let jpg = id_named(&s, &dir, "img_0001.jpg");
    let undo0 = s.undo.len();
    let pre = s.execute("stack.rawJpegPairs", &json!({"all": true, "preview": true})).unwrap();
    assert_eq!(pre, json!({"stacks": 1, "photos": 2}));
    assert_eq!(s.catalog.stacks().count(), 0, "preview changes nothing");
    let r = s.execute("stack.rawJpegPairs", &json!({"all": true})).unwrap();
    assert_eq!(r["stacks"], 1);
    assert_eq!(s.catalog.stack_of(jpg).unwrap().photos, vec![raw, jpg]);
    assert_eq!(s.undo.len(), undo0 + 1, "one undo step");
    // running it again finds nothing new
    assert_eq!(s.execute("stack.rawJpegPairs", &json!({"all": true})).unwrap()["stacks"], 0);
    assert_eq!(s.undo.len(), undo0 + 1);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.stacks().count(), 0);
    // by id: only the named photos' pairs
    let lone = id_named(&s, &dir, "IMG_0002.JPG");
    assert_eq!(s.execute("stack.rawJpegPairs", &json!({"ids": [lone.0]})).unwrap()["stacks"], 0);
    assert_eq!(s.execute("stack.rawJpegPairs", &json!({"ids": [jpg.0]})).unwrap()["stacks"], 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_preview_only_raw_puts_its_jpeg_on_top() {
    let dir = shoot("preview-only");
    let mut s = Session::new().with_fs();
    import(&mut s, &dir, json!({}));
    let raw = id_named(&s, &dir, "IMG_0001.DNG");
    let jpg = id_named(&s, &dir, "img_0001.jpg");
    // as a Nikon High Efficiency NEF this build cannot decode is recorded
    let mut p = lightcraft_catalog::Photo::clone(s.catalog.photo(raw).unwrap());
    p.set_preview_only(Some("High Efficiency NEF".into()));
    s.catalog.apply(lightcraft_catalog::Op::RemovePhoto { id: raw }).unwrap();
    s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s.execute("stack.rawJpegPairs", &json!({"all": true})).unwrap();
    assert_eq!(s.catalog.stack_of(raw).unwrap().photos, vec![jpg, raw]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A raw container this build can't decode (a CRW: its magic, then a JPEG), so it imports as
/// preview only, as a Nikon High Efficiency NEF does.
fn write_undecodable_raw(path: &Path) {
    use lightcraft_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
    let px: Vec<u8> = (0..40 * 30).flat_map(|i| [(i % 251) as u8, 128, 200]).collect();
    let jpeg = encode_jpeg(&EncodeImage::new(40, 30, 3, Samples::U8(&px)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
    std::fs::write(path, [&b"II\x1a\0\0\0HEAPCCDR"[..], &[0x11u8; 64][..], &jpeg].concat()).unwrap();
}

#[test]
fn importing_an_undecodable_raw_puts_its_jpeg_on_top() {
    let dir = temp_dir("undecodable");
    write_undecodable_raw(&dir.join("CRW_0001.CRW"));
    write_jpeg(&dir.join("CRW_0001.JPG"), 5);
    let mut s = Session::new().with_fs();
    let r = import(&mut s, &dir, json!({"pairRawJpeg": true}));
    assert_eq!(r["paired"], 1, "{r}");
    let raw = id_named(&s, &dir, "CRW_0001.CRW");
    let jpg = id_named(&s, &dir, "CRW_0001.JPG");
    let p = s.catalog.photo(raw).unwrap();
    assert!(p.kind == MediaKind::Raw && p.preview_only.is_some(), "{p:?}");
    assert_eq!(s.catalog.stack_of(raw).unwrap().photos, vec![jpg, raw], "the camera JPEG is the photo shown");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn batches_of_one_import_pair_across_batches_and_merge_into_one_undo_step() {
    let dir = temp_dir("batches");
    for i in 1..=3u8 {
        write_raw_n(&dir.join(format!("DSC_000{i}.DNG")), i);
        write_jpeg(&dir.join(format!("DSC_000{i}.JPG")), i);
    }
    let mut s = Session::new().with_fs();
    let undo0 = s.undo.len();
    let opts = crate::import::ImportOptions { pair_raw_jpeg: true, ..Default::default() };
    let mut job = crate::import::ImportJob::new(&mut s, opts).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    // the app's import task: a batch at a time, each raw and its JPEG in different batches
    let p = |n: &str| dir.join(n).to_string_lossy().to_string();
    let batches =
        [vec![p("DSC_0001.DNG"), p("DSC_0002.DNG")], vec![p("DSC_0001.JPG"), p("DSC_0003.DNG")], vec![p("DSC_0002.JPG"), p("DSC_0003.JPG")]];
    let mut paired = 0;
    for b in batches {
        let prepared = job.prepare_files(b, &cancel);
        paired += crate::import::commit_prepared(&mut s, &job.opts, job.now(), prepared).unwrap().paired;
    }
    assert_eq!(paired, 3);
    assert_eq!(s.catalog.stacks().count(), 3);
    s.merge_undo(s.undo.len() - undo0, "Add 6 Photos");
    assert_eq!(s.undo.len(), undo0 + 1);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!((s.catalog.len(), s.catalog.stacks().count()), (0, 0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn synchronize_folder_follows_the_preference() {
    let dir = temp_dir("sync");
    write_raw(&dir.join("DSC_0001.DNG"));
    let mut s = Session::new().with_fs();
    import(&mut s, &dir, json!({}));
    write_jpeg(&dir.join("DSC_0001.JPG"), 6);
    s.execute("library.preferences", &json!({"import": {"pairRawJpeg": true}})).unwrap();
    let r = s.execute("folder.synchronize", &json!({"path": dir.to_string_lossy()})).unwrap();
    assert_eq!((r["imported"].as_u64(), r["paired"].as_u64()), (Some(1), Some(1)), "{r}");
    let raw = id_named(&s, &dir, "DSC_0001.DNG");
    assert_eq!(s.catalog.stack_of(raw).unwrap().photos.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn copy_import_pairs_when_names_are_kept_and_not_when_a_sequence_renames_them() {
    let base = temp_dir("copy");
    let src = base.join("card");
    std::fs::create_dir_all(&src).unwrap();
    write_raw(&src.join("DSC_0001.DNG"));
    write_jpeg(&src.join("DSC_0001.JPG"), 7);
    let mut s = Session::new().with_fs();
    s.open_library(base.join("library"), false).unwrap();
    let copy = |s: &mut Session, dest: &str, rename: Option<&str>| {
        let mut p = json!({"mode": "copy", "destination": base.join(dest).to_string_lossy(), "organize": "flat", "pairRawJpeg": true});
        if let Some(r) = rename {
            p["rename"] = json!(r);
        }
        import(s, &src, p)
    };
    // names kept (also with a template that keeps {name}): the copies pair
    assert_eq!(copy(&mut s, "kept", None)["paired"], 1);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(copy(&mut s, "named", Some("trip_{name}"))["paired"], 1);
    s.execute("edit.undo", &json!({})).unwrap();
    // a numbering template gives the two files different names: they stay apart (documented)
    let r = copy(&mut s, "numbered", Some("trip_{seq:3}"));
    assert!(r.get("paired").is_none(), "{r}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn reload_puts_the_right_photo_on_top_of_a_pair() {
    let dir = temp_dir("recheck");
    write_raw(&dir.join("DSC_0001.DNG"));
    write_jpeg(&dir.join("DSC_0001.JPG"), 8);
    let mut s = Session::new().with_fs();
    import(&mut s, &dir, json!({}));
    let raw = id_named(&s, &dir, "DSC_0001.DNG");
    let jpg = id_named(&s, &dir, "DSC_0001.JPG");
    // the library thought the raw was preview only when the pair was stacked
    let p = lightcraft_catalog::Photo::clone(s.catalog.photo(raw).unwrap());
    s.catalog
        .apply(lightcraft_catalog::Op::SetContent {
            id: raw,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: p.content_hash.clone(),
            preview_only: Some("not decoded by an older build".into()),
            preview_checked: "0.0.1".into(),
        })
        .unwrap();
    s.execute("stack.rawJpegPairs", &json!({"all": true})).unwrap();
    assert_eq!(s.catalog.stack_of(raw).unwrap().photos, vec![jpg, raw]);
    // the recheck after an update finds the raw decodes: the raw goes on top
    let r = s.execute("library.recheckPreviewOnly", &json!({"all": true})).unwrap();
    assert_eq!(r["cleared"], json!([raw.0]), "{r}");
    assert_eq!(s.catalog.stack_of(raw).unwrap().photos, vec![raw, jpg]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A collapsed pair with the raw marked preview only, stacked with the JPEG on top.
fn pair_with_preview_only_raw(tag: &str) -> (Session, PathBuf, PhotoId, PhotoId) {
    let dir = temp_dir(tag);
    write_raw(&dir.join("DSC_0001.DNG"));
    write_jpeg(&dir.join("DSC_0001.JPG"), 9);
    let mut s = Session::new().with_fs();
    import(&mut s, &dir, json!({}));
    let raw = id_named(&s, &dir, "DSC_0001.DNG");
    let jpg = id_named(&s, &dir, "DSC_0001.JPG");
    let p = lightcraft_catalog::Photo::clone(s.catalog.photo(raw).unwrap());
    s.catalog
        .apply(lightcraft_catalog::Op::SetContent {
            id: raw,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: p.content_hash.clone(),
            preview_only: Some("not decoded by an older build".into()),
            preview_checked: "0.0.1".into(),
        })
        .unwrap();
    s.execute("stack.rawJpegPairs", &json!({"all": true})).unwrap();
    assert_eq!(s.catalog.stack_of(raw).unwrap().photos, vec![jpg, raw]);
    (s, dir, raw, jpg)
}

#[test]
fn a_retop_moves_the_active_photo_to_the_new_top() {
    let (mut s, dir, raw, jpg) = pair_with_preview_only_raw("retop-active");
    s.selection = crate::Selection::single(jpg);
    s.execute("library.recheckPreviewOnly", &json!({"all": true})).unwrap();
    assert_eq!(s.catalog.stack_of(raw).unwrap().photos, vec![raw, jpg]);
    // the JPEG is hidden in the collapsed stack now: the active photo is the one the grid shows
    assert_eq!(s.selection.active, Some(raw));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_reload_that_only_changes_the_file_keeps_the_top_the_user_chose() {
    let dir = temp_dir("keep-top");
    write_raw(&dir.join("DSC_0001.DNG"));
    write_jpeg(&dir.join("DSC_0001.JPG"), 10);
    let mut s = Session::new().with_fs();
    import(&mut s, &dir, json!({"pairRawJpeg": true}));
    let raw = id_named(&s, &dir, "DSC_0001.DNG");
    let jpg = id_named(&s, &dir, "DSC_0001.JPG");
    assert_eq!(s.catalog.stack_of(raw).unwrap().photos, vec![raw, jpg]);
    s.selection = crate::Selection::single(jpg);
    s.execute("stack.setTop", &json!({"id": jpg.0})).unwrap();
    assert_eq!(s.catalog.stack_of(raw).unwrap().photos, vec![jpg, raw]);
    // the raw is saved again (new bytes) but still decodes: the user's top stays
    write_raw_n(&dir.join("DSC_0001.DNG"), 9);
    let r = s.execute("photo.reload", &json!({"ids": [raw.0]})).unwrap();
    assert_eq!(r["reloaded"], json!([raw.0]), "{r}");
    assert_eq!(s.catalog.stack_of(raw).unwrap().photos, vec![jpg, raw]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stacking_a_selected_pair_leaves_only_the_visible_photo_selected() {
    let dir = temp_dir("pair-selection");
    write_raw(&dir.join("DSC_0001.DNG"));
    write_jpeg(&dir.join("DSC_0001.JPG"), 11);
    write_jpeg(&dir.join("DSC_0002.JPG"), 12);
    let mut s = Session::new().with_fs();
    import(&mut s, &dir, json!({}));
    let raw = id_named(&s, &dir, "DSC_0001.DNG");
    let jpg = id_named(&s, &dir, "DSC_0001.JPG");
    let other = id_named(&s, &dir, "DSC_0002.JPG");
    s.selection = crate::Selection { ids: vec![jpg, raw, other], active: Some(jpg) };
    assert_eq!(s.execute("stack.rawJpegPairs", &json!({})).unwrap()["stacks"], 1);
    // the JPEG is hidden under the raw now: an edit with Auto Sync must not reach it
    assert_eq!(s.selection.ids, vec![raw, other]);
    assert_eq!(s.selection.active, Some(raw));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_raw_synchronized_next_to_the_active_jpeg_takes_its_place_in_the_selection() {
    let dir = temp_dir("sync-selection");
    write_jpeg(&dir.join("DSC_0001.JPG"), 13);
    let mut s = Session::new().with_fs();
    import(&mut s, &dir, json!({}));
    let jpg = id_named(&s, &dir, "DSC_0001.JPG");
    s.selection = crate::Selection::single(jpg);
    write_raw(&dir.join("DSC_0001.DNG"));
    s.execute("library.preferences", &json!({"import": {"pairRawJpeg": true}})).unwrap();
    let r = s.execute("folder.synchronize", &json!({"path": dir.to_string_lossy()})).unwrap();
    assert_eq!(r["paired"], 1, "{r}");
    let raw = id_named(&s, &dir, "DSC_0001.DNG");
    assert_eq!(s.catalog.stack_of(raw).unwrap().photos, vec![raw, jpg]);
    // the JPEG is hidden under the raw: edits go to the photo the grid shows
    assert_eq!((s.selection.ids.clone(), s.selection.active), (vec![raw], Some(raw)));
    let _ = std::fs::remove_dir_all(&dir);
}
