use super::*;

fn session() -> crate::Session {
    let mut s = crate::Session::new();
    let p = Photo::new(PhotoId(1), Source::File { path: "hash-fixture.jpg".into() }, "fixture.jpg", "JPEG", 6000, 4000, "2026-01-01");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s
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
/// The photo's Camera colour option is part of the decode: a source decoded with another option is decoded again
/// (and a late render of the old one is not taken back into the cache); one the option doesn't change is reused.
#[test]
fn camera_colour_decodes_again_only_when_it_changes_the_source() {
    use lightcraft_develop::CameraColour;
    let decodes = |colorimetric: bool| {
        let seen = Arc::new(std::sync::Mutex::new(Vec::<LoadOptions>::new()));
        let log = seen.clone();
        let loader: FileLoader = Arc::new(move |_, _, options| {
            log.lock().unwrap().push(options);
            Ok((Rgb32f::new(8, 8), SourceInfo { raw: true, colorimetric, ..SourceInfo::default() }))
        });
        (loader, seen)
    };
    let set = |s: &mut crate::Session, colour: CameraColour| {
        let mut d = (*s.catalog.photo(PhotoId(1)).unwrap().develop).clone();
        d.profile.camera_colour = colour;
        s.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(d), label: "edit".into(), edited: None }).unwrap();
    };
    let render = |s: &mut crate::Session| {
        let r = s.render_job(PhotoId(1), 64, 64, false, true).unwrap().run();
        s.accept(&r);
    };

    let mut s = session();
    let (loader, seen) = decodes(true);
    s.media.file_loader = Some(loader);
    render(&mut s);
    // a job made before the option changes, finishing after it
    let late = s.render_job(PhotoId(1), 64, 64, false, true).unwrap();
    render(&mut s);
    assert_eq!(seen.lock().unwrap().len(), 1, "the decoded source is reused while the option stays");
    set(&mut s, CameraColour::Colorimetric);
    render(&mut s);
    assert_eq!(seen.lock().unwrap().len(), 2, "a new option decodes again");
    assert_eq!(seen.lock().unwrap()[1].colour, CameraColour::Colorimetric, "with the option");
    render(&mut s);
    assert_eq!(seen.lock().unwrap().len(), 2, "and the new source is reused");
    // the late job re-decodes with the old option (its source was the cached one: decode it from the file)
    let mut late = late;
    late.source = s.media.origin_ref(&Source::File { path: "hash-fixture.jpg".into() }, 64);
    let late = late.run();
    assert_eq!(late.loaded.as_ref().map(|l| l.options.colour), Some(CameraColour::Auto));
    s.accept(&late);
    let n = seen.lock().unwrap().len();
    render(&mut s);
    assert_eq!(seen.lock().unwrap().len(), n, "the Colorimetric source stayed in the cache");

    // a preset's preview with the other option is decoded with it, and its source doesn't replace the photo's own
    let mut auto = (*s.catalog.photo(PhotoId(1)).unwrap().develop).clone();
    auto.profile.camera_colour = CameraColour::Auto;
    let preview = s.variant_job(PhotoId(1), &auto, 64).unwrap().run();
    assert_eq!(preview.loaded.as_ref().map(|l| l.options.colour), Some(CameraColour::Auto), "the preview's own option");
    s.accept(&preview);
    let n = seen.lock().unwrap().len();
    render(&mut s);
    assert_eq!(seen.lock().unwrap().len(), n, "the photo's Colorimetric source is still the cached one");

    // a camera without measured colour: the option changes nothing, so the source is kept
    let mut s = session();
    let (loader, seen) = decodes(false);
    s.media.file_loader = Some(loader);
    render(&mut s);
    set(&mut s, CameraColour::Colorimetric);
    render(&mut s);
    assert_eq!(seen.lock().unwrap().len(), 1, "no second decode where the option has no effect");
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
        minimum_embedded_edge: None,
        small: None,
    };
    let r = quick.run();
    assert_eq!((r.request_id, r.key), (7, 9));
    assert_eq!(r.rendered.err().as_deref(), Some("the render failed unexpectedly: preview reader bug"));
}
