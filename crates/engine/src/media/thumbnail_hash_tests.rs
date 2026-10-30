use super::*;

fn session() -> crate::Session {
    let mut s = crate::Session::new();
    let p = Photo::new(PhotoId(1), Source::File { path: "hash-fixture.jpg".into() }, "fixture.jpg", "JPEG", 6000, 4000, "2026-01-01");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s
}
/// Settings saved by a newer LightCraft (a process number this build doesn't know) render with
/// this build's latest process, and a build that resolved the number to an older process
/// cached other pixels under the same settings hash: the persistent keys (thumbnails, the
/// loupe's view render, variants) name the process the settings render with, so those are
/// never served (review of 5a7016c8: a catalog saved with a newer process kept main's V2 thumbnails).
#[test]
fn persistent_keys_name_the_process_the_settings_render_with() {
    use lightcraft_develop::ProcessVersion;
    let mut s = session();
    let newer = DevelopSettings { process: ProcessVersion(ProcessVersion::LATEST.0 + 1), ..DevelopSettings::default() };
    s.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(newer.clone()), label: "newer".into(), edited: None }).unwrap();
    let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
    let unnamed = |kind: Option<&str>, size: u64| {
        let mut h = Hasher128::new();
        h.str(&content_key(&p));
        if let Some(k) = kind {
            h.str(k);
        }
        h.u64(newer.hash64()).u64(size).u64(RENDER_CACHE_VERSION).u64(crate::camera_profiles::cache_key()).finish()
    };
    let named = |kind: Option<&str>, size: u64, process: u64| {
        let mut h = Hasher128::new();
        h.str(&content_key(&p));
        if let Some(k) = kind {
            h.str(k);
        }
        h.u64(newer.hash64()).u64(size).u64(RENDER_CACHE_VERSION).u64(process).u64(crate::camera_profiles::cache_key()).finish()
    };
    let resolved = u64::from(lightcraft_develop::Process::LATEST.version().0);
    let keys = [
        (s.thumb_job(PhotoId(1), 256).unwrap().cache.unwrap().1, None, 256),
        (crate::Session::view_key(&p, true), Some("view"), 1),
        (crate::Session::variant_key(&p, &newer, 256), Some("variant"), 256),
    ];
    for (key, kind, size) in keys {
        assert_ne!(key, unnamed(kind, size), "old builds did not name their resolved process");
        assert_eq!(key, named(kind, size, resolved), "the key names the process used for rendering");
        assert_ne!(key, named(kind, size, u64::from(newer.process.0)), "the saved future number is not the resolved process");
    }
}

/// Photos on V1 and V2 keep the persistent keys builds before process V3 gave them (no process
/// in them), so their cached thumbnails, view renders and variants stay valid after upgrading
/// (review of 26cd2df1: every V1/V2 photo rendered again, and offline ones without smart
/// previews lost their thumbnails); V3 and later name their process.
#[test]
fn v1_and_v2_photos_keep_the_render_cache_keys_they_had() {
    use lightcraft_develop::Process;
    let mut s = session();
    for process in [Process::V1, Process::V2, Process::V3] {
        let settings = DevelopSettings { process: process.version(), ..DevelopSettings::default() };
        s.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(settings.clone()), label: "p".into(), edited: None }).unwrap();
        let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
        let before_v3 = |kind: Option<&str>, size: u64| {
            let mut h = Hasher128::new();
            h.str(&content_key(&p));
            if let Some(k) = kind {
                h.str(k);
            }
            h.u64(settings.hash64()).u64(size).u64(RENDER_CACHE_VERSION).u64(crate::camera_profiles::cache_key()).finish()
        };
        let keys = [
            s.thumb_job(PhotoId(1), 256).unwrap().cache.unwrap().1 == before_v3(None, 256),
            crate::Session::view_key(&p, true) == before_v3(Some("view"), 1),
            crate::Session::variant_key(&p, &settings, 256) == before_v3(Some("variant"), 256),
        ];
        let old = process <= Process::V2;
        assert_eq!(keys, [old; 3], "{process:?}: thumbnail, view and variant keys");
    }
}

#[test]
fn hashes_match_uncached_keys_and_are_not_recomputed_for_sizes_metadata_or_before() {
    let mut s = session();
    for edge in [256, 384, 512, 256] {
        let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
        let b = thumb_bucket(edge);
        // Independent original formula: never asks the new hash cache for expected values.
        let settings = p.develop.hash64();
        let content = Hasher128::new().str(&content_key(&p)).finish().0 as u64;
        let expected = settings ^ ((b as u64) << 40) ^ ((b as u64) << 20) ^ 1 ^ p.id.0.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ content.rotate_left(17);
        let disk = Hasher128::new()
            .str(&content_key(&p))
            .u64(settings)
            .u64(b as u64)
            .u64(RENDER_CACHE_VERSION)
            .u64(u64::from(p.develop.process.process().version().0))
            .u64(crate::camera_profiles::cache_key())
            .finish();
        let job = s.thumb_job(p.id, edge).unwrap();
        assert_eq!(job.key, expected);
        assert_eq!(job.cache.unwrap().1, disk);
    }
    s.catalog.apply(Op::SetRating { id: PhotoId(1), rating: 4 }).unwrap();
    s.thumb_job(PhotoId(1), 256).unwrap();
    s.render_job(PhotoId(1), 256, 256, true, true).unwrap();
    s.thumb_job(PhotoId(1), 256).unwrap();
    assert_eq!(s.media.settings_hashes.computations, 1);
    let mut edit = DevelopSettings::default();
    edit.light.exposure = 1.0;
    let undo = s.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edit), label: "edit".into(), edited: None }).unwrap();
    s.thumb_job(PhotoId(1), 256).unwrap();
    s.thumb_job(PhotoId(1), 384).unwrap();
    assert_eq!(s.media.settings_hashes.computations, 2);
    s.catalog.apply(undo).unwrap();
    s.thumb_job(PhotoId(1), 256).unwrap();
    assert_eq!(s.media.settings_hashes.computations, 3);
}
#[test]
fn hash_cache_is_bounded_weak_and_shared_settings_remain_independent() {
    let mut hashes = SettingsHashes::default();
    let shared = Arc::new(DevelopSettings::default());
    for id in 1..=SETTINGS_HASH_ENTRIES as u64 * 3 {
        assert_eq!(hashes.get(PhotoId(id), &shared), shared.hash64());
    }
    assert_eq!(hashes.entries.len(), SETTINGS_HASH_ENTRIES);
    assert_eq!(hashes.order.len(), SETTINGS_HASH_ENTRIES);
    assert_eq!(Arc::strong_count(&shared), 1);
    let mut edit = (*shared).clone();
    edit.light.exposure = 1.0;
    let edit = Arc::new(edit);
    let a = PhotoId(10000);
    let b = PhotoId(10001);
    let initial = hashes.get(a, &shared);
    assert_eq!(hashes.get(b, &shared), initial);
    assert_ne!(hashes.get(a, &edit), initial);
    assert_eq!(hashes.get(b, &shared), initial);
    let mut s = session();
    s.thumb_job(PhotoId(1), 256).unwrap();
    s.media.clear_sources();
    assert!(s.media.settings_hashes.entries.is_empty());
    assert!(s.media.settings_hashes.order.is_empty());
}
#[test]
fn source_from_before_content_reload_is_not_accepted() {
    let mut s = session();
    s.media.file_loader = Some(Arc::new(|_, _| Ok((Rgb32f::new(8, 8), SourceInfo::default()))));
    let old = s.thumb_job(PhotoId(1), 256).unwrap().run();
    assert!(old.loaded.is_some());
    s.catalog
        .apply(Op::SetContent {
            id: PhotoId(1),
            width: 6000,
            height: 4000,
            file_size: 99,
            content_hash: Some("replacement".into()),
            preview_only: None,
            preview_checked: String::new(),
        })
        .unwrap();
    s.media.forget(PhotoId(1));
    s.accept(&old);
    assert_eq!(s.media.source_usage().0, 0);
}
/// A panic while rendering (here in the decoder) used to end the pool's worker thread without
/// an answer, so the view that asked showed "Rendering…" forever. It is the job's failure now,
/// for renders and for stand-ins, answering the request it came from.
#[test]
fn a_render_that_panics_fails_instead_of_never_answering() {
    let mut s = session();
    s.media.file_loader = Some(Arc::new(|_, _| panic!("decoder bug")));
    let mut job = s.thumb_job(PhotoId(1), 256).unwrap();
    job.request_id = 42;
    let (key, level, source_key) = (job.key, job.level, job.source_key);
    let r = job.run();
    assert_eq!((r.request_id, r.photo, r.key, r.level, r.source_key), (42, PhotoId(1), key, level, source_key));
    assert_eq!(r.rendered.err().as_deref(), Some("the render failed unexpectedly: decoder bug"));
    let quick = QuickJob {
        request_id: 7,
        photo: PhotoId(1),
        key: 9,
        cached: Vec::new(),
        embedded: Some(("x.arw".into(), Arc::new(|_: &str, _: usize| -> Option<Rgba8> { panic!("preview reader bug") }), 256)),
        small: None,
    };
    let r = quick.run();
    assert_eq!((r.request_id, r.key), (7, 9));
    assert_eq!(r.rendered.err().as_deref(), Some("the render failed unexpectedly: preview reader bug"));
}
