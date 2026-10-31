//! Import previews must cover the tile when possible, retaining camera JPEGs as a safe fallback.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use crate::Session;
use crate::import::ImportCandidate;
use crate::media::QuickSource;

/// Synthetic raw, bounded decoder and camera JPEG, with reads counted independently of pixels.
fn fixture(preview_edge: Option<usize>, renderable: bool, preview_only: bool) -> (Session, ImportCandidate, Arc<AtomicUsize>) {
    let mut session = Session::new();
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    session.media.file_loader = Some(Arc::new(move |_, edge| {
        count.fetch_add(1, Ordering::SeqCst);
        if !renderable {
            return Err("synthetic raw cannot be rendered".into());
        }
        let width = edge.min(2048);
        let height = width * 3 / 4;
        Ok((lightcraft_raster::Rgb32f { width, height, data: vec![[0.1, 0.2, 0.3]; width * height] }, Default::default()))
    }));
    session.media.preview_loader = Some(Arc::new(move |_, requested| {
        let width = preview_edge?.min(requested);
        Some(lightcraft_raster::Rgba8 { width, height: width * 3 / 4, data: vec![[30, 60, 90, 255]; width * (width * 3 / 4)] })
    }));
    let candidate = ImportCandidate {
        path: "synthetic-import.nef".into(),
        name: "synthetic-import.nef".into(),
        format: "NEF".into(),
        kind: lightcraft_catalog::MediaKind::Raw,
        width: 1024,
        height: 768,
        preview_only: preview_only.then(|| "unsupported synthetic raw variant".into()),
        ..Default::default()
    };
    (session, candidate, reads)
}

#[test]
/// A renderable raw fills the requested tile when its embedded JPEG is too small.
fn import_preview_renders_when_the_camera_jpeg_cannot_cover_the_tile() {
    let (mut session, candidate, reads) = fixture(Some(64), true, false);
    let mut job = session.candidate_thumb_job(&candidate, 768, 0).unwrap();
    job.request_id = 42;
    let key = job.key;
    let result = job.run();
    assert_eq!(result.quick, Some(QuickSource::Small));
    let image = result.rendered.unwrap().image;
    assert_eq!((image.width, image.height), (768, 576));
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!((result.request_id, result.key), (42, key));
}

#[test]
/// A sufficient camera JPEG avoids decoding the raw, retaining the fast path.
fn import_preview_keeps_a_camera_jpeg_that_already_covers_the_tile() {
    let (mut session, candidate, reads) = fixture(Some(1024), true, false);
    let result = session.candidate_thumb_job(&candidate, 768, 0).unwrap().run();
    assert_eq!(result.quick, Some(QuickSource::Embedded));
    assert_eq!(result.rendered.unwrap().image.width, 768);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
}

#[test]
/// Preview-only raws never waste a render attempt behind their available camera JPEG.
fn import_preview_only_raw_uses_the_available_camera_jpeg_without_a_render() {
    let (mut session, candidate, reads) = fixture(Some(64), false, true);
    let result = session.candidate_thumb_job(&candidate, 768, 0).unwrap().run();
    assert_eq!(result.quick, Some(QuickSource::Embedded));
    assert_eq!(result.rendered.unwrap().image.width, 64);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
}

#[test]
/// A failed or absent render never discards the usable, if undersized, camera JPEG.
fn import_preview_retains_the_camera_jpeg_if_the_render_fails_or_is_absent() {
    for no_render in [false, true] {
        let (mut session, candidate, reads) = fixture(Some(64), false, false);
        let mut job = session.candidate_thumb_job(&candidate, 768, 0).unwrap();
        if no_render {
            job.small = None;
        }
        let result = job.run();
        assert_eq!(result.quick, Some(QuickSource::Embedded));
        assert_eq!(result.rendered.unwrap().image.width, 64);
        assert_eq!(reads.load(Ordering::SeqCst), usize::from(!no_render));
    }
}

#[test]
fn import_preview_supports_loupe_sizes_without_unbounded_requests() {
    let (mut session, mut candidate, reads) = fixture(None, true, false);
    candidate.width = 6000;
    candidate.height = 4000;
    assert_ne!(super::candidate_thumb_key(&candidate, 1024), super::candidate_thumb_key(&candidate, 1536));
    assert_eq!(super::candidate_thumb_key(&candidate, 2048), super::candidate_thumb_key(&candidate, usize::MAX));
    for (requested, expected) in [(1536, 1536), (2048, 2048), (usize::MAX, 2048)] {
        let result = session.candidate_thumb_job(&candidate, requested, 0).unwrap().run();
        let image = result.rendered.unwrap().image;
        assert_eq!(image.width, expected);
        assert_eq!(image.height, expected * 3 / 4);
    }
    assert_eq!(reads.load(Ordering::SeqCst), 3);
}
