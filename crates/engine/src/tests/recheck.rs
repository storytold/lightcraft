//! Issue #662: preview-only raws are rechecked when a library opens with another app version.

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
    dir: super::TestDir,
    lib: PathBuf,
    file: PathBuf,
    id: PhotoId,
}

/// A library with one preview-only CR3, its flag stamped by [`OTHER`] as a library saved by
/// another version would be.
fn library(name: &str) -> Lib {
    let dir = super::TestDir::new(&format!("recheck-{name}"));
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
    let listed = recheck::stale(&mut x.s, false);
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
    assert_eq!(recheck::stale(&mut x.s, true).len(), 1, "copies aren't probed on their own");
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

#[test]
fn reload_without_a_file_reader_preserves_verified_facts() {
    use crate::cmd::reload;
    let mut x = library("reload-no-reader");
    let before = x.s.catalog.photo(x.id).unwrap().clone();
    x.s.media.file_probe = None;
    let options = json!({"id": x.id.0});
    let err = x.s.execute("photo.reload", &options).unwrap_err().to_string();
    assert!(err.contains("no file access"), "{err}");

    let items = reload::plan(&mut x.s, &options).unwrap();
    let results = reload::probe(None, &items, &std::sync::atomic::AtomicBool::new(false), &|_| {});
    assert!(results.iter().all(Result::is_err));
    assert!(reload::apply(&mut x.s, items, results).unwrap().checked.is_empty());
    assert_eq!(x.s.catalog.photo(x.id).unwrap(), &before);
}

#[test]
fn rejected_recheck_does_not_supersede_an_older_reload() {
    use crate::cmd::reload;
    let mut x = checked_orf("rejected-batch");
    let before = x.s.catalog.photo(x.id).unwrap().clone();
    let mut another = (*before).clone();
    another.id = x.s.catalog.alloc_photo_id();
    another.captured = None;
    another.source = lightcraft_catalog::Source::File { path: x.dir.join("other.orf").to_string_lossy().into() };
    let other_id = another.id;
    x.s.commit("Add", Op::AddPhoto { photo: Box::new(another) }).unwrap();

    let older = reload::plan(&mut x.s, &json!({"id": x.id.0})).unwrap();
    replace_checked_orf(&x.file);
    let old = reload::file_probe(&x.s).unwrap()(&x.file.to_string_lossy()).unwrap();
    let newer = recheck::stale(&mut x.s, true)
        .into_iter()
        .map(|item| {
            let mut info = old.clone();
            if item.id == other_id {
                info.captured = Some("invalid capture date".into());
            }
            (item, Ok(info))
        })
        .collect();
    assert!(recheck::apply(&mut x.s, newer).is_err());
    assert_eq!(x.s.catalog.photo(x.id).unwrap(), &before);
    assert_eq!(reload::apply(&mut x.s, older, vec![Ok(old)]).unwrap().reloaded, [x.id]);
}

/// Relinked copies must be checked independently, and must not receive their parent's file facts.
#[test]
fn relinked_copy_rechecks_its_own_file() {
    let mut x = library("relinked-copy");
    let r = x.s.execute("photo.virtualCopy", &json!({"ids": [x.id.0]})).unwrap();
    let copy = PhotoId(r["ids"][0].as_u64().unwrap());
    let other = x.dir.join("files/other.cr3");
    std::fs::write(&other, cr3_with_preview(120, 80)).unwrap();
    x.s.execute("photo.relink", &json!({"id": copy.0, "path": other.to_string_lossy()})).unwrap();
    let p = x.s.catalog.photo(copy).unwrap();
    x.s.apply_system(Op::SetContent {
        id: copy,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: p.content_hash.clone(),
        preview_only: p.preview_only.clone(),
        preview_checked: OTHER.into(),
    })
    .unwrap();
    let listed = recheck::stale(&mut x.s, false);
    assert_eq!(listed.iter().map(|p| p.id).collect::<Vec<_>>(), vec![x.id, copy]);
    let original = x.s.catalog.photo(x.id).unwrap().clone();
    std::fs::write(&other, crate::tests_xmp::synthetic_dng_with(None, lightcraft_meta::Metadata::default())).unwrap();
    let r = x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap();
    assert_eq!(r["checked"], 2);
    assert_eq!(r["cleared"], json!([copy.0]));
    let parent = x.s.catalog.photo(x.id).unwrap();
    assert_eq!(parent.content_hash, original.content_hash);
    assert_eq!(parent.preview_only, original.preview_only);
    assert_eq!(parent.preview_checked, APP_VERSION);
    assert!(x.s.catalog.photo(copy).unwrap().develops_raw());
}

/// A same-file copy still needs its own probe when its parent's stamp is already current.
#[test]
fn stale_copy_is_checked_when_its_parent_is_current() {
    let mut x = library("copy-only-stale");
    let r = x.s.execute("photo.virtualCopy", &json!({"ids": [x.id.0]})).unwrap();
    let copy = PhotoId(r["ids"][0].as_u64().unwrap());
    let p = x.s.catalog.photo(x.id).unwrap();
    x.s.apply_system(Op::SetContent {
        id: x.id,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: p.content_hash.clone(),
        preview_only: p.preview_only.clone(),
        preview_checked: APP_VERSION.into(),
    })
    .unwrap();
    assert_eq!(recheck::stale(&mut x.s, false).iter().map(|p| p.id).collect::<Vec<_>>(), vec![copy]);
    assert_eq!(recheck::stale(&mut x.s, true).iter().map(|p| p.id).collect::<Vec<_>>(), vec![x.id]);
    decodable_now(&x);
    let r = x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap();
    assert_eq!(r["checked"], 1);
    assert_eq!(r["cleared"], json!([copy.0, x.id.0]));
    assert!(x.s.catalog.photo(copy).unwrap().develops_raw());
    assert!(x.s.catalog.photo(x.id).unwrap().develops_raw());
}

#[test]
fn review_stale_copies_share_one_probe_when_the_parent_is_current() {
    let mut x = library("stale-siblings");
    let mut copies = Vec::new();
    for _ in 0..2 {
        copies.push(PhotoId(x.s.execute("photo.virtualCopy", &json!({"ids": [x.id.0]})).unwrap()["ids"][0].as_u64().unwrap()));
    }
    let p = x.s.catalog.photo(x.id).unwrap();
    x.s.apply_system(Op::SetContent {
        id: x.id,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: p.content_hash.clone(),
        preview_only: p.preview_only.clone(),
        preview_checked: APP_VERSION.into(),
    })
    .unwrap();
    decodable_now(&x);
    let file_probe = recheck::file_probe(&x.s).unwrap();
    x.s.media.file_probe = Some(std::sync::Arc::new(move |path| {
        let mut info = file_probe(path)?;
        info.meta.camera = "Camera from shared source".into();
        Ok(info)
    }));
    let r = x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap();
    assert_eq!(r["checked"], 1, "same-file siblings share a probe even without an eligible parent");
    for id in &copies {
        assert_eq!(x.s.catalog.photo(*id).unwrap().meta.camera, "Camera from shared source");
    }
    assert!(x.s.catalog.photo(x.id).unwrap().meta.camera.is_empty(), "the current parent was only an implicit target");
    let mut expected = vec![x.id.0];
    expected.extend(copies.iter().map(|id| id.0));
    for field in ["updated", "cleared"] {
        let mut ids: Vec<_> = r[field].as_array().unwrap().iter().map(|id| id.as_u64().unwrap()).collect();
        ids.sort();
        assert_eq!(ids, expected, "{field} reports each recovered photo once");
    }
}

#[test]
fn review_recheck_keeps_a_listed_copy_after_its_parent_moves() {
    for relink_copy in [false, true] {
        let mut x = library(&format!("parent-moves-{relink_copy}"));
        let copy = PhotoId(x.s.execute("photo.virtualCopy", &json!({"ids": [x.id.0]})).unwrap()["ids"][0].as_u64().unwrap());
        let listed = recheck::stale(&mut x.s, false);
        assert_eq!(listed.len(), 1);
        decodable_now(&x);
        let mut info = recheck::file_probe(&x.s).unwrap()(&x.file.to_string_lossy()).unwrap();
        info.meta.camera = "Camera from the listed copy’s source".into();
        let other = x.dir.join("other.cr3");
        std::fs::write(&other, cr3_with_preview(120, 80)).unwrap();
        x.s.execute("photo.relink", &json!({"id": x.id.0, "path": other.to_string_lossy()})).unwrap();
        if relink_copy {
            x.s.execute("photo.relink", &json!({"id": copy.0, "path": x.file.to_string_lossy()})).unwrap();
        }
        let parent = x.s.catalog.photo(x.id).unwrap().clone();
        let before = x.s.catalog.photo(copy).unwrap().clone();
        let out = recheck::apply(&mut x.s, listed.into_iter().map(|item| (item, Ok(info.clone()))).collect()).unwrap();
        assert_eq!(x.s.catalog.photo(x.id).unwrap(), &parent);
        if relink_copy {
            assert!(out.updated.is_empty());
            assert_eq!(x.s.catalog.photo(copy).unwrap(), &before);
        } else {
            assert_eq!(out.cleared, vec![copy]);
            let photo = x.s.catalog.photo(copy).unwrap();
            assert!(photo.develops_raw());
            assert_eq!(photo.meta.camera, "Camera from the listed copy’s source");
        }
    }
}

#[test]
fn review_same_path_relink_invalidates_older_reads_even_when_facts_match() {
    let mut x = checked_orf("same-path");
    let listed = recheck::stale(&mut x.s, true);
    let current = x.s.catalog.photo(x.id).unwrap().clone();
    replace_checked_orf(&x.file);
    let old = recheck::probe(&recheck::file_probe(&x.s).unwrap(), listed);
    std::fs::write(&x.file, crate::files::tests::sensor_orf()).unwrap();
    x.s.execute("photo.relink", &json!({"id": x.id.0, "path": x.file.to_string_lossy()})).unwrap();
    assert_eq!(x.s.catalog.photo(x.id).unwrap(), &current, "relink confirmed exactly the same facts");
    assert!(recheck::apply(&mut x.s, old).unwrap().updated.is_empty());
    assert_eq!(x.s.catalog.photo(x.id).unwrap(), &current);
    replace_checked_orf(&x.file);
    assert_eq!(x.s.execute("library.recheckPreviewOnly", &json!({"all": true})).unwrap()["updated"], json!([x.id.0]));
}

#[test]
fn recheck_refreshes_a_copy_created_during_the_probe() {
    let mut x = checked_orf("new-copy");
    let listed = recheck::stale(&mut x.s, true);
    replace_checked_orf(&x.file);
    let info = recheck::file_probe(&x.s).unwrap()(&x.file.to_string_lossy()).unwrap();
    let copy = PhotoId(x.s.execute("photo.virtualCopy", &json!({"id": x.id.0})).unwrap()["ids"][0].as_u64().unwrap());
    assert_ne!(x.s.catalog.photo(copy).unwrap().content_hash, info.content_hash);
    let out = recheck::apply(&mut x.s, listed.into_iter().map(|item| (item, Ok(info.clone()))).collect()).unwrap();
    assert!(out.updated.contains(&copy));
    assert_eq!(x.s.catalog.photo(copy).unwrap().content_hash, info.content_hash);
}

#[test]
fn recheck_does_not_overwrite_copies_created_after_its_listing() {
    let mut x = checked_orf("late-copy");
    let copy = PhotoId(x.s.execute("photo.virtualCopy", &json!({"id": x.id.0})).unwrap()["ids"][0].as_u64().unwrap());
    let listed = recheck::stale(&mut x.s, true);
    replace_checked_orf(&x.file);
    let old = recheck::file_probe(&x.s).unwrap()(&x.file.to_string_lossy()).unwrap();
    replace_checked_orf(&x.file);
    x.s.execute("photo.relink", &json!({"id": x.id.0, "path": x.file.to_string_lossy()})).unwrap();
    let late = PhotoId(x.s.execute("photo.virtualCopy", &json!({"id": x.id.0})).unwrap()["ids"][0].as_u64().unwrap());
    let current = x.s.catalog.photo(late).unwrap().clone();
    let out = recheck::apply(&mut x.s, listed.into_iter().map(|item| (item, Ok(old.clone()))).collect()).unwrap();
    assert_eq!(x.s.catalog.photo(late).unwrap(), &current, "a copy inheriting newer facts keeps them");
    assert_eq!(out.updated, [copy], "the untouched listed sibling still accepts its pending result");
    assert_eq!(x.s.catalog.photo(copy).unwrap().file_size, old.file_size);
}

fn checked_orf(name: &str) -> Lib {
    let dir = super::TestDir::new(&format!("recheck-checked-{name}"));
    let file = dir.join("original.orf");
    let mut x = Lib { s: Session::new().with_fs(), lib: dir.join("lib"), dir, file, id: PhotoId(0) };
    std::fs::write(&x.file, crate::files::tests::sensor_orf()).unwrap();
    let r = x.s.execute("library.import", &json!({"paths": [x.file.to_string_lossy()]})).unwrap();
    x.id = PhotoId(r["imported"][0].as_u64().unwrap());
    let photo = x.s.catalog.photo(x.id).unwrap();
    assert!(photo.preview_only.is_none() && !photo.preview_only_stale() && photo.format == "ORF");
    x
}

#[test]
fn review_recheck_after_relink_keeps_fresh_facts_and_the_untouched_copy() {
    let mut x = checked_orf("aba");
    let copy = PhotoId(x.s.execute("photo.virtualCopy", &json!({"ids": [x.id.0]})).unwrap()["ids"][0].as_u64().unwrap());
    let p = x.s.catalog.photo(copy).unwrap();
    x.s.catalog
        .apply(Op::SetContent {
            id: copy,
            width: p.width,
            height: p.height,
            file_size: 0,
            content_hash: p.content_hash.clone(),
            preview_only: None,
            preview_checked: p.preview_checked.clone(),
        })
        .unwrap();
    let p = x.s.catalog.photo(x.id).unwrap();
    x.s.catalog
        .apply(Op::SetContent {
            id: x.id,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: p.content_hash.clone(),
            preview_only: None,
            preview_checked: OTHER.into(),
        })
        .unwrap();
    let listed = recheck::stale(&mut x.s, false);
    assert_eq!(listed.len(), 1, "only the stale parent requests a probe");
    assert_eq!(listed[0].metadata_targets, [x.id]);
    let mut old = recheck::file_probe(&x.s).unwrap()(&x.file.to_string_lossy()).unwrap();
    old.meta.lens = "must not fill the unselected copy".into();
    let old_size = old.file_size;
    let other = x.dir.join("other.orf");
    std::fs::copy(&x.file, &other).unwrap();
    x.s.execute("photo.relink", &json!({"id": x.id.0, "path": other.to_string_lossy()})).unwrap();
    replace_checked_orf(&x.file);
    x.s.execute("photo.relink", &json!({"id": x.id.0, "path": x.file.to_string_lossy()})).unwrap();
    let current = x.s.catalog.photo(x.id).unwrap().clone();
    let out = recheck::apply(&mut x.s, listed.into_iter().map(|item| (item, Ok(old.clone()))).collect()).unwrap();
    assert_eq!(x.s.catalog.photo(x.id).unwrap(), &current);
    assert!(!out.updated.contains(&x.id) && out.updated.contains(&copy));
    let sibling = x.s.catalog.photo(copy).unwrap();
    assert_eq!(sibling.file_size, old_size);
    assert!(sibling.meta.lens.is_empty(), "automatic grouping does not select a copy's metadata");
}

#[test]
fn review_a_new_reload_supersedes_an_older_recheck() {
    overlapping_source_checks(true);
}

#[test]
fn an_older_reload_finishing_first_keeps_a_newer_pending_read() {
    use crate::cmd::reload;
    let mut x = checked_orf("older-first");
    let older = reload::plan(&mut x.s, &json!({"id": x.id.0})).unwrap();
    replace_checked_orf(&x.file);
    let old = reload::file_probe(&x.s).unwrap()(&x.file.to_string_lossy()).unwrap();
    let newer = reload::plan(&mut x.s, &json!({"id": x.id.0})).unwrap();
    replace_checked_orf(&x.file);
    let new = reload::file_probe(&x.s).unwrap()(&x.file.to_string_lossy()).unwrap();
    let expected = new.content_hash.clone();
    assert_eq!(reload::apply(&mut x.s, older, vec![Ok(old)]).unwrap().reloaded, [x.id]);
    assert_eq!(reload::apply(&mut x.s, newer, vec![Ok(new)]).unwrap().reloaded, [x.id]);
    assert_eq!(x.s.catalog.photo(x.id).unwrap().content_hash, expected);
}

#[test]
fn an_older_source_read_refreshing_the_stamp_keeps_a_newer_recheck() {
    use crate::cmd::reload;
    for older_reload in [false, true] {
        let mut x = library(&format!("older-stamp-{older_reload}"));
        let reloading = older_reload.then(|| reload::plan(&mut x.s, &json!({"id": x.id.0})).unwrap());
        let checking = (!older_reload).then(|| recheck::stale(&mut x.s, false));
        let probe = recheck::file_probe(&x.s).unwrap();
        let old = probe(&x.file.to_string_lossy()).unwrap();
        let newer = recheck::stale(&mut x.s, false);
        decodable_now(&x);
        let newer = recheck::probe(&probe, newer);
        if let Some(items) = reloading {
            reload::apply(&mut x.s, items, vec![Ok(old)]).unwrap();
        } else {
            recheck::apply(&mut x.s, checking.unwrap().into_iter().map(|item| (item, Ok(old.clone()))).collect()).unwrap();
        }
        assert!(!x.s.catalog.photo(x.id).unwrap().preview_only_stale(), "the older read refreshed the stamp");
        assert_eq!(recheck::apply(&mut x.s, newer).unwrap().cleared, [x.id]);
        assert!(x.s.catalog.photo(x.id).unwrap().develops_raw());
    }
}

#[test]
fn review_a_new_recheck_supersedes_an_older_reload() {
    overlapping_source_checks(false);
}

fn replace_checked_orf(file: &std::path::Path) {
    let mut bytes = std::fs::read(file).unwrap();
    bytes.push(42);
    std::fs::write(file, bytes).unwrap();
}

fn overlapping_source_checks(older_recheck: bool) {
    let mut x = checked_orf(if older_recheck { "old-recheck" } else { "old-reload" });
    let checking = older_recheck.then(|| recheck::stale(&mut x.s, true));
    let reloading = (!older_recheck).then(|| crate::cmd::reload::plan(&mut x.s, &json!({"ids": [x.id.0]})).unwrap());
    let old = recheck::file_probe(&x.s).unwrap()(&x.file.to_string_lossy()).unwrap();
    replace_checked_orf(&x.file);
    let replacement = Some(lightcraft_preview::hash_bytes(&std::fs::read(&x.file).unwrap()).to_string());
    assert_ne!(replacement, old.content_hash);
    let command = if older_recheck { "photo.reload" } else { "library.recheckPreviewOnly" };
    let result = x.s.execute(command, &json!({"ids": [x.id.0], "all": true})).unwrap();
    assert_eq!(result[if older_recheck { "reloaded" } else { "updated" }], json!([x.id.0]));
    let current = x.s.catalog.photo(x.id).unwrap().clone();
    assert_eq!(current.content_hash, replacement, "the newer check applied the replacement bytes");
    assert!(current.format == "ORF" && !current.preview_only_stale(), "current decoder facts still require ticket ordering");
    let updated = if let Some(items) = checking {
        recheck::apply(&mut x.s, items.into_iter().map(|item| (item, Ok(old.clone()))).collect()).unwrap().updated
    } else {
        crate::cmd::reload::apply(&mut x.s, reloading.unwrap(), vec![Ok(old)]).unwrap().reloaded
    };
    assert!(updated.is_empty());
    assert_eq!(x.s.catalog.photo(x.id).unwrap(), &current);
    assert_eq!(x.s.catalog.photo(x.id).unwrap().content_hash, replacement);
}
