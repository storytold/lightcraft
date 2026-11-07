//! Compressed ORF withdrawal: public import/recheck and render identities stay consistent.

use std::path::PathBuf;
use std::sync::Arc;

use lightcraft_catalog::{APP_VERSION, MediaKind, Op, Photo, PhotoId, Source};
use lightcraft_engine::{Session, cmd::recheck, media::content_key};
use lightcraft_pipeline::SourceInfo;
use lightcraft_preview::Hash128;
use lightcraft_raster::Rgb32f;
use serde_json::json;

struct OrfLibrary {
    s: Session,
    dir: PathBuf,
    lib: PathBuf,
    id: PhotoId,
}

impl Drop for OrfLibrary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A fixed original and settings isolate decoder status from every other render-key input.
fn decoder_session(format: &str) -> Session {
    let mut s = Session::new();
    let mut p = Photo::new(PhotoId(1), Source::File { path: "same-original.orf".into() }, "same-original.orf", format, 6000, 4000, "2026-01-01");
    p.kind = MediaKind::Raw;
    p.content_hash = Some("unchanged-file-bytes".into());
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s
}

/// Include the frontend's ordinary, hover and quick-view identities as well as all persistent
/// render families. The original-file identity must not change along with any of these.
fn decoder_keys(s: &mut Session) -> (Vec<u64>, Vec<Hash128>, String) {
    let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
    let thumb = s.thumb_job(p.id, 256).unwrap();
    let render = s.render_job(p.id, 640, 480, false, true).unwrap();
    let hover = s.preview_job(p.id, 640, 480, true, &p.develop).unwrap();
    let quick = s.quick_view_job(p.id, 640, true).unwrap();
    (
        vec![thumb.key, render.key, hover.key, quick.key],
        vec![
            thumb.cache.unwrap().1,
            s.loupe_job(p.id, 640, 480, true).unwrap().view_cache.unwrap().1,
            Session::variant_key(&p, &p.develop, 256, 0),
            render.source_key.unwrap(),
        ],
        content_key(&p),
    )
}

/// Old ORF renders cannot stand in after withdrawal; other formats and original-file identities stay stable.
#[test]
fn withdrawn_orf_decoder_has_distinct_render_keys_without_changing_file_identity() {
    // Before the withdrawal, format was not part of any of these keys: an otherwise identical
    // non-ORF photo gives the old keys, preserved for every other camera and ordinary image.
    let legacy = decoder_keys(&mut decoder_session("DNG"));
    assert_eq!(legacy, decoder_keys(&mut decoder_session("JPEG")));
    let orf = decoder_keys(&mut decoder_session("ORF"));
    for (old, new) in legacy.0.iter().zip(&orf.0) {
        assert_ne!(old, new, "old ORF frontend render must not be reused");
    }
    for (old, new) in legacy.1.iter().zip(&orf.1) {
        assert_ne!(old, new, "old ORF persistent render or decoded source must not be reused");
    }
    assert_eq!(legacy.2, orf.2, "smart previews and denoise products still identify the original file");
    assert_eq!(orf, decoder_keys(&mut decoder_session("orf")));
}

/// Apply the same-byte capability transition that Reload or the background recheck records.
fn mark_orf_preview_only(s: &mut Session) {
    let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
    s.catalog
        .apply(Op::SetContent {
            id: p.id,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: p.content_hash.clone(),
            preview_only: Some("Olympus compressed ORF".into()),
            preview_checked: lightcraft_catalog::APP_VERSION.into(),
        })
        .unwrap();
    s.media.forget(p.id);
}

/// A support-status change invalidates every render family, even when a resize happens at the same time.
#[test]
fn orf_preview_transition_changes_render_keys_with_identical_bytes_and_settings() {
    let mut s = decoder_session("ORF");
    let before = decoder_keys(&mut s);
    mark_orf_preview_only(&mut s);
    let after = decoder_keys(&mut s);
    for (old, new) in before.0.iter().zip(&after.0) {
        assert_ne!(old, new, "Reload changes frontend render identity");
    }
    for (old, new) in before.1.iter().zip(&after.1) {
        assert_ne!(old, new, "Reload changes persistent render and decoded source identity");
    }
    assert_eq!(before.2, after.2, "Reload does not change the original file's identity");
    let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
    assert_ne!(before.0[1], s.render_job(p.id, 648, 480, false, true).unwrap().key, "a resize must not cancel the status salt");
    assert_ne!(before.0[2], s.preview_job(p.id, 648, 480, true, &p.develop).unwrap().key, "a hover resize must not cancel the status salt");
}

/// An in-flight sensor decode must not refill a source cache cleared by the preview-only transition.
#[test]
fn source_from_before_orf_preview_transition_is_not_accepted() {
    let mut s = decoder_session("ORF");
    s.media.file_loader = Some(Arc::new(|_, _, _| Ok((Rgb32f::new(8, 8), SourceInfo::default()))));
    let old = s.thumb_job(PhotoId(1), 256).unwrap().run();
    assert!(old.loaded.is_some());
    mark_orf_preview_only(&mut s);
    s.accept(&old);
    assert_eq!(s.media.source_usage().0, 0, "a sensor decode finishing after Reload cannot repopulate its source cache");
}

/// A legacy ORF record and a controlled probe let the upgrade tests exercise catalog migration
/// without downloading camera files. The raw crate separately tests the actual ORF probe.
fn legacy_orf(name: &str, preview_only: Option<String>) -> OrfLibrary {
    let dir = std::env::temp_dir().join(format!("lc-orf-recheck-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    let file = dir.join("legacy.orf");
    let mut s = Session::new();
    s.open_library(&lib, false).unwrap();
    let id = s.catalog.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: file.to_string_lossy().into() }, "legacy.orf", "ORF", 96, 64, "2026-01-01");
    p.kind = MediaKind::Raw;
    p.file_size = 1024;
    p.content_hash = Some("same-file".into());
    let info = lightcraft_engine::media::ProbeInfo {
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: p.content_hash.clone(),
        format: p.format.clone(),
        kind: p.kind,
        preview_only,
        ..Default::default()
    };
    s.commit("Legacy ORF", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s.selection = lightcraft_engine::Selection::single(id);
    s.media.file_probe = Some(Arc::new(move |_| Ok(info.clone())));
    OrfLibrary { s, dir, lib, id }
}

/// Previously supported compressed ORFs and their copies migrate without losing edits, history or persistence.
#[test]
fn legacy_compressed_orf_and_copies_become_preview_only_without_losing_edits() {
    let mut x = legacy_orf("compressed", Some("Olympus compressed ORF".into()));
    x.s.execute("develop.set", &json!({"ids": [x.id.0], "control": "light.exposure", "value": 0.5})).unwrap();
    let r = x.s.execute("photo.virtualCopy", &json!({"ids": [x.id.0]})).unwrap();
    let copy = PhotoId(r["ids"][0].as_u64().unwrap());
    let before = x.s.catalog.photo(x.id).unwrap().clone();
    let undo = x.s.undo.len();
    let r = x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap();
    assert_eq!(r, json!({"checked": 1, "updated": [x.id.0, copy.0], "cleared": []}));
    for id in [x.id, copy] {
        let p = x.s.catalog.photo(id).unwrap();
        assert_eq!(p.preview_only.as_deref(), Some("Olympus compressed ORF"));
        assert_eq!(p.preview_checked, APP_VERSION);
        assert!(!p.develops_raw());
        assert_eq!(*p.develop, *before.develop);
    }
    assert_eq!(x.s.catalog.photo(x.id).unwrap().history, before.history);
    assert_eq!(x.s.undo.len(), undo, "bookkeeping is not an undo step");
    x.s.persist().unwrap();
    x.s = Session::new();
    x.s.open_library(&x.lib, false).unwrap();
    assert!(x.s.catalog.photo(x.id).unwrap().preview_only.is_some());
    assert!(recheck::stale(&x.s.catalog, false).is_empty());
}

/// Supported uncompressed ORF is checked once and never reported as newly recovered sensor support.
#[test]
fn legacy_uncompressed_orf_is_stamped_once_without_a_false_recovery_notice() {
    let mut x = legacy_orf("supported", None);
    let r = x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap();
    assert_eq!(r, json!({"checked": 1, "updated": [x.id.0], "cleared": []}));
    let p = x.s.catalog.photo(x.id).unwrap();
    assert!(p.develops_raw());
    assert_eq!(p.preview_checked, APP_VERSION);
    assert!(recheck::stale(&x.s.catalog, false).is_empty());
    assert_eq!(recheck::stale(&x.s.catalog, true).len(), 1);
    x.s.persist().unwrap();
    x.s = Session::new();
    x.s.open_library(&x.lib, false).unwrap();
    assert!(recheck::stale(&x.s.catalog, false).is_empty());
    assert!(x.s.catalog.photo(x.id).unwrap().develops_raw());
}

/// An offline original keeps its catalog state and remains eligible for a later recheck.
#[test]
fn unavailable_legacy_orf_is_unchanged_and_retried() {
    let mut x = legacy_orf("offline", None);
    x.s.media.file_probe = Some(std::sync::Arc::new(|_| Err("offline".into())));
    let before = x.s.catalog.photo(x.id).unwrap().clone();
    let r = x.s.execute("library.recheckPreviewOnly", &json!({})).unwrap();
    assert_eq!(r, json!({"checked": 1, "updated": [], "cleared": []}));
    assert_eq!(x.s.catalog.photo(x.id).unwrap(), &before);
    assert_eq!(recheck::stale(&x.s.catalog, false).len(), 1);
}
