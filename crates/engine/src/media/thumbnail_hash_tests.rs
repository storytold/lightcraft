use super::*;

fn session() -> crate::Session {
    let mut s = crate::Session::new();
    let p = Photo::new(PhotoId(1), Source::File { path: "hash-fixture.jpg".into() }, "fixture.jpg", "JPEG", 6000, 4000, "2026-01-01");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s
}
/// PERF-CULL: an unedited raw's camera JPEG is read once for the loupe and kept in memory for a
/// step back; the full-size one (zoomed in) is made at the size asked for and kept nowhere; an
/// edited raw or another file has none.
#[test]
fn camera_previews_are_kept_for_a_revisit_and_full_size_ones_are_not() {
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut s = crate::Session::new();
    let r = reads.clone();
    s.media.preview_loader = Some(Arc::new(move |_: &str, edge: usize| {
        r.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(Rgba8 { width: edge.min(64), height: 2, data: vec![[9, 8, 7, 255]; edge.min(64) * 2] })
    }));
    let mut raw = Photo::new(PhotoId(1), Source::File { path: "a.cr3".into() }, "a.cr3", "CR3", 6000, 4000, "2026-01-01");
    raw.kind = MediaKind::Raw;
    raw.develop = Arc::new(raw.camera_defaults());
    s.catalog.apply(Op::AddPhoto { photo: Box::new(raw) }).unwrap();
    let jpeg = Photo::new(PhotoId(2), Source::File { path: "b.jpg".into() }, "b.jpg", "JPEG", 600, 400, "2026-01-01");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(jpeg) }).unwrap();
    for _ in 0..3 {
        let q = s.quick_view_job(PhotoId(1), 48, true).unwrap();
        let r = q.run();
        assert_eq!(r.quick, Some(QuickSource::Embedded));
    }
    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1, "read once, then from memory");
    let full = s.embedded_full_job(PhotoId(1), 8192).unwrap();
    assert!(full.embedded_cache.is_none() && full.cached.is_empty() && full.small.is_none());
    assert_eq!(full.embedded.as_ref().map(|e| e.2), Some(8192));
    assert_ne!(full.key, s.quick_view_job(PhotoId(1), 48, true).unwrap().key);
    assert_eq!(full.run().quick, Some(QuickSource::Embedded));
    assert!(s.embedded_full_job(PhotoId(2), 8192).is_none(), "not a raw");
    let mut edited = (*s.catalog.photo(PhotoId(1)).unwrap().develop).clone();
    edited.light.exposure = 1.0;
    s.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(edited), label: "edit".into(), edited: None }).unwrap();
    assert!(s.embedded_full_job(PhotoId(1), 8192).is_none(), "an edit is shown rendered");
}

/// A virtual copy shares its master's file and, unedited, its settings; its loupe stand-ins
/// are its own all the same (CodeRabbit on #3080).
#[test]
fn a_virtual_copy_has_its_own_stand_in_keys() {
    let mut s = crate::Session::new();
    s.media.preview_loader = Some(Arc::new(|_: &str, _: usize| None::<Rgba8>));
    for id in [1, 2] {
        let mut raw = Photo::new(PhotoId(id), Source::File { path: "a.cr3".into() }, "a.cr3", "CR3", 6000, 4000, "2026-01-01");
        raw.kind = MediaKind::Raw;
        raw.develop = Arc::new(raw.camera_defaults());
        s.catalog.apply(Op::AddPhoto { photo: Box::new(raw) }).unwrap();
    }
    let quick = |s: &mut crate::Session, id: u64| s.quick_view_job(PhotoId(id), 48, true).unwrap().key;
    assert_ne!(quick(&mut s, 1), quick(&mut s, 2));
    let full = |s: &mut crate::Session, id: u64| s.embedded_full_job(PhotoId(id), 8192).unwrap().key;
    assert_ne!(full(&mut s, 1), full(&mut s, 2));
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
        (s.view_key(&p, true), Some("view"), 1),
        (crate::Session::variant_key(&p, &newer, 256, 0), Some("variant"), 256),
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
            s.view_key(&p, true) == before_v3(Some("view"), 1),
            crate::Session::variant_key(&p, &settings, 256, 0) == before_v3(Some("variant"), 256),
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
    s.media.file_loader = Some(Arc::new(|_, _, _| Ok((Rgb32f::new(8, 8), SourceInfo::default()))));
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
/// A decode that started before the photo's database lens profile changed (a lens database arriving, looked up again
/// by `cmd::lens_refresh`, which drops the photo's sources) never puts its source back in the cache: the source's
/// colour was fitted through the old profile.
#[test]
fn source_from_before_a_lens_profile_change_is_not_accepted() {
    let mut s = session();
    s.media.file_loader = Some(Arc::new(|_, _, _| Ok((Rgb32f::new(8, 8), SourceInfo::default()))));
    let old = s.thumb_job(PhotoId(1), 256).unwrap().run();
    assert!(old.loaded.is_some());
    let lens = lightcraft_develop::EmbeddedLens { source: lightcraft_develop::LensSource::Database, ..Default::default() };
    s.catalog.apply(Op::SetEmbeddedLens { id: PhotoId(1), lens: Some(Box::new(lens)) }).unwrap();
    s.media.forget(PhotoId(1));
    s.accept(&old);
    assert_eq!(s.media.source_usage().0, 0, "the old decode is dropped");
    // a decode made with the new profile is kept
    let new = s.thumb_job(PhotoId(1), 256).unwrap().run();
    s.accept(&new);
    assert_ne!(s.media.source_usage().0, 0);
}
/// The loupe's hover preview (a preset or profile under the pointer) is cached by its key, so a database lens
/// profile looked up again gives it a new key and the hover renders afresh instead of showing the old correction.
#[test]
fn a_hover_preview_key_follows_the_lens_profile() {
    let mut s = session();
    let settings = s.catalog.photo(PhotoId(1)).unwrap().develop.clone();
    let before = s.preview_job(PhotoId(1), 640, 480, true, &settings).unwrap().key;
    let lens = lightcraft_develop::EmbeddedLens { source: lightcraft_develop::LensSource::Database, ..Default::default() };
    s.catalog.apply(Op::SetEmbeddedLens { id: PhotoId(1), lens: Some(Box::new(lens)) }).unwrap();
    let after = s.preview_job(PhotoId(1), 640, 480, true, &settings).unwrap().key;
    assert_ne!(before, after);
    // one database profile looked up again as another (a newer database): the key follows the profile's contents,
    // not only whether there is one
    let vignette = lightcraft_develop::EmbeddedVignette { k: [-0.3, 0.0, 0.0, 0.0, 0.0], radius: 1.0, ..Default::default() };
    let other = lightcraft_develop::EmbeddedLens { source: lightcraft_develop::LensSource::Database, vignette: Some(vignette), ..Default::default() };
    s.catalog.apply(Op::SetEmbeddedLens { id: PhotoId(1), lens: Some(Box::new(other)) }).unwrap();
    let again = s.preview_job(PhotoId(1), 640, 480, true, &settings).unwrap().key;
    assert_ne!(after, again, "a different database profile");
}
/// A panic while rendering (here in the decoder) used to end the pool's worker thread without
/// an answer, so the view that asked showed "Rendering…" forever. It is the job's failure now,
/// for renders and for stand-ins, answering the request it came from.
#[test]
fn a_render_that_panics_fails_instead_of_never_answering() {
    let mut s = session();
    s.media.file_loader = Some(Arc::new(|_, _, _| panic!("decoder bug")));
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
        embedded_cache: None,
        embedded_on_disk: false,
        minimum_embedded_edge: None,
        small: None,
    };
    let r = quick.run();
    assert_eq!((r.request_id, r.key), (7, 9));
    assert_eq!(r.rendered.err().as_deref(), Some("the render failed unexpectedly: preview reader bug"));
}

/// CodeRabbit on #3544: a stand-in job decoding a camera JPEG when Discard Preview Cache clears the
/// cache doesn't put the picture back: it writes for the generation it was made in (memory and
/// disk alike), as render jobs do.
#[test]
fn a_camera_jpeg_decoded_across_a_cache_clear_is_not_kept() {
    for on_disk in [false, true] {
        let cache = Arc::new(PreviewCache::memory(64 << 20));
        let made_in = cache.generation();
        let key = Hash128(5);
        let quick = QuickJob {
            request_id: 1,
            photo: PhotoId(1),
            key: 2,
            cached: Vec::new(),
            embedded: Some(("x.arw".into(), Arc::new(|_: &str, _: usize| Some(Rgba8::new(16, 16))), 256)),
            embedded_cache: Some((cache.clone(), key, made_in)),
            embedded_on_disk: on_disk,
            minimum_embedded_edge: None,
            small: None,
        };
        // the user discards the cache while the job is on its way
        cache.clear();
        assert!(quick.run().rendered.is_ok(), "the stand-in is still shown");
        assert!(cache.get_memory(key).is_none(), "on disk {on_disk}: nothing kept in the cleared cache");
        // a job made after the clear keeps it as before
        let quick = QuickJob {
            request_id: 1,
            photo: PhotoId(1),
            key: 2,
            cached: Vec::new(),
            embedded: Some(("x.arw".into(), Arc::new(|_: &str, _: usize| Some(Rgba8::new(16, 16))), 256)),
            embedded_cache: Some((cache.clone(), key, cache.generation())),
            embedded_on_disk: on_disk,
            minimum_embedded_edge: None,
            small: None,
        };
        assert!(quick.run().rendered.is_ok());
        assert!(cache.get_memory(key).is_some(), "on disk {on_disk}: kept when made after the clear");
    }
}
