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
/// A photo whose profile is a measured look is decoded by the look loader, its thumbnails too (not by the reduced
/// loader), and a decoded source serves only the kind of profile it was decoded for: switching to or from a look
/// decodes the photo again, other edits don't.
#[test]
fn a_photo_with_a_look_is_decoded_for_it() {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    let table =
        lightcraft_raw::profile::HsvTable { hue_divisions: 1, sat_divisions: 2, val_divisions: 1, data: vec![[0.0, 1.0, 1.0]; 2], srgb_value: false };
    let info = move |deferred| SourceInfo {
        raw: true,
        camera_rgb: Some(Arc::new(lightcraft_pipeline::looks::CameraRgb {
            model: "X-T4".into(),
            matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            hue_sat: Some(Arc::new(table.clone())),
            deferred,
            exposure: 1.0,
        })),
        ..Default::default()
    };
    let mut s = session();
    let (plain, looks) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (p, r, l, i1, i3, i2) = (plain.clone(), plain.clone(), looks.clone(), info.clone(), info.clone(), info);
    s.media.file_loader = Some(Arc::new(move |_, _| {
        p.fetch_add(1, SeqCst);
        Ok((Rgb32f::new(8, 8), i1(false)))
    }));
    // what grid thumbnails load with: a plain decode (no reduced copy in this file)
    s.media.reduced_loader = Some(Arc::new(move |_, _| {
        r.fetch_add(1, SeqCst);
        Ok((Rgb32f::new(8, 8), i3(false), false))
    }));
    s.media.look_loader = Some(Arc::new(move |_, _| {
        l.fetch_add(1, SeqCst);
        Ok((Rgb32f::new(8, 8), i2(true)))
    }));
    let render = |s: &mut crate::Session| {
        let r = s.thumb_job(PhotoId(1), 256).unwrap().run();
        s.accept(&r);
    };
    let set = |s: &mut crate::Session, id: &str, exposure: f64| {
        let mut d = (*s.catalog.photo(PhotoId(1)).unwrap().develop).clone();
        d.profile.id = id.into();
        d.light.exposure = exposure;
        s.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(d), label: "edit".into(), edited: None }).unwrap();
    };
    let calls = || (plain.load(SeqCst), looks.load(SeqCst));
    render(&mut s);
    assert_eq!(calls(), (1, 0));
    set(&mut s, "look:x-t4/velvia", 0.0);
    render(&mut s);
    assert_eq!(calls(), (1, 1), "a look decodes the photo for it");
    set(&mut s, "look:x-t4/provia", 0.5);
    render(&mut s);
    assert_eq!(calls(), (1, 1), "another look or edit keeps that decode");
    set(&mut s, "lc.vivid", 0.0);
    render(&mut s);
    assert_eq!(calls(), (2, 1), "leaving the looks decodes it plainly again");
    // without a look loader (the web build) a look renders from the plain decode
    s.media.look_loader = None;
    set(&mut s, "look:x-t4/velvia", 0.0);
    render(&mut s);
    assert_eq!(calls(), (2, 1));
}

/// A photo with a denoised picture is decoded plainly (with its twin) even when its profile is a look, and that decode
/// keeps serving it: asking for the look decode would miss the cache on every render.
#[test]
fn a_denoised_photo_with_a_look_keeps_its_plain_decode() {
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    let table =
        lightcraft_raw::profile::HsvTable { hue_divisions: 1, sat_divisions: 2, val_divisions: 1, data: vec![[0.0, 1.0, 1.0]; 2], srgb_value: false };
    let info = SourceInfo {
        raw: true,
        camera_rgb: Some(Arc::new(lightcraft_pipeline::looks::CameraRgb {
            model: "X-T4".into(),
            matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            hue_sat: Some(Arc::new(table)),
            deferred: false,
            exposure: 1.0,
        })),
        ..Default::default()
    };
    let mut s = session();
    let (pairs, looks) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (p, l, i1, i2) = (pairs.clone(), looks.clone(), info.clone(), info);
    s.media.file_loader = Some(Arc::new(|_, _| Err("the pair loader decodes this photo".into())));
    s.media.look_loader = Some(Arc::new(move |_, _| {
        l.fetch_add(1, SeqCst);
        Ok((Rgb32f::new(8, 8), i1.clone()))
    }));
    s.media.denoise.loader = Some(Arc::new(move |_, _, _| {
        p.fetch_add(1, SeqCst);
        Ok((Rgb32f::new(8, 8), Some(Rgb32f::new(8, 8)), i2.clone()))
    }));
    s.media.denoise.set(PhotoId(1), DenoiseSpec { product: "denoised.bin".into(), key: "k".into(), make: None });
    // a look, then slider moves: each render needs the source again
    for i in 0..3 {
        let mut d = (*s.catalog.photo(PhotoId(1)).unwrap().develop).clone();
        d.profile.id = "look:x-t4/velvia".into();
        d.light.exposure = 0.1 * i as f64;
        s.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(d), label: "edit".into(), edited: None }).unwrap();
        let r = s.thumb_job(PhotoId(1), 256).unwrap().run();
        s.accept(&r);
    }
    assert_eq!((pairs.load(SeqCst), looks.load(SeqCst)), (1, 0), "one plain decode, kept");
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
