use super::*;

/// A loupe job with a display fit returns its image shrunk to the drawn size, aspect kept;
/// a draft, and a render no larger than the drawn size, are left as rendered.
#[test]
fn a_display_fit_shrinks_the_loupe_render_to_the_drawn_size() {
    let mut s = crate::Session::with_demo();
    let id = s.active().unwrap();
    let plain = s.loupe_job(id, 1024, 1024, true).unwrap().run().rendered.unwrap().image;
    let long = plain.width.max(plain.height);
    assert!(long >= 600, "the demo renders at least 600 px at 1024: {long}");
    let fitted = s.loupe_job(id, 1024, 1024, true).unwrap().with_display_fit(400, 400).run().rendered.unwrap().image;
    assert_eq!(fitted.width.max(fitted.height), 400, "{}x{}", fitted.width, fitted.height);
    let aspect = |i: &Rgba8| i.width as f32 / i.height as f32;
    assert!((aspect(&fitted) - aspect(&plain)).abs() < 0.02, "aspect kept");
    let draft = s.loupe_job(id, 1024, 1024, true).unwrap().draft().with_display_fit(400, 400).run().rendered.unwrap().image;
    assert_eq!(draft.width.max(draft.height), long, "a draft is not shrunk");
    let same = s.loupe_job(id, 1024, 1024, true).unwrap().with_display_fit(plain.width, plain.height).run().rendered.unwrap().image;
    assert_eq!((same.width, same.height), (plain.width, plain.height), "no larger than drawn: left as rendered");
    let a = s.loupe_job(id, 1024, 1024, true).unwrap();
    let b = s.loupe_job(id, 1024, 1024, true).unwrap().with_display_fit(400, 400);
    assert_ne!(a.key, b.key, "the fit is part of the result key");
}

#[test]
fn renders_know_the_original_size_whichever_level_they_come_from() {
    // issue #960: noise reduction works in the original's pixels, so a grid cell drawn from the
    // 512-px thumbnail level denoises like a scaled-down export
    let p = Photo::new(PhotoId(1), Source::File { path: "/photos/a.ARW".into() }, "a.ARW", "ARW", 6000, 4000, "");
    assert_eq!(source_info(&p).native_long, Some(6000));
    let decoded = SourceInfo { raw: true, as_shot_temp: 4800.0, ..Default::default() };
    let thumb = DecodedSource::new(Arc::new(Rgb32f::new(8, 8)), Some(decoded.clone()));
    let info = thumb.info_or(source_info(&p));
    assert_eq!(info, SourceInfo { native_long: Some(6000), ..decoded }, "the decoder's facts, the catalog's size");
    let unknown = Photo::new(PhotoId(2), Source::File { path: "/photos/b.jpg".into() }, "b.jpg", "JPEG", 0, 0, "");
    assert_eq!(source_info(&unknown).native_long, None);
    assert_eq!(source_info(&Photo::new(PhotoId(3), Source::Demo { scene: 0 }, "d", "ARW", 600, 400, "")).native_long, None);
}

#[test]
fn denoise_mix_is_reserved_before_the_source_enters_the_cache() {
    let mut plain = Rgb32f::new(8, 8);
    plain.data.fill([1.0; 3]);
    let mut source = DecodedSource::new(Arc::new(plain), None);
    source.denoised = Some(Arc::new(Twin::new(Arc::new(Rgb32f::new(8, 8)))));
    let reserved = source.bytes();
    assert!(reserved >= 3 * 8 * 8 * 12);
    assert_eq!(source.image_for(0.5).data[0], [0.5; 3]);
    assert_eq!(source.bytes(), reserved, "creating a mix cannot evade the LRU's recorded cost");
    let _ = source.image_for(0.25);
    assert_eq!(source.bytes(), reserved, "changing Amount replaces the reserved mix");
}

#[test]
fn hidden_grading_wheels_render_neutral_without_touching_the_photo() {
    let mut s = crate::Session::with_demo();
    let id = s.active().unwrap();
    s.execute("develop.set", &serde_json::json!({"values": {"grading.shadows.sat": 60, "grading.highlights.sat": 40}})).unwrap();
    let job = s.loupe_job(id, 64, 64, true).unwrap();
    let same = job.clone().with_hidden_grading([false; 3]);
    assert_eq!(same.key, job.key, "nothing hidden: the job is unchanged");
    assert!(same.view_cache.is_some());
    let hidden = job.clone().with_hidden_grading([true, false, false]);
    assert_eq!(hidden.settings.grading.shadows.sat, 0.0);
    assert_eq!(hidden.settings.grading.highlights.sat, 40.0);
    assert_ne!(hidden.key, job.key, "a hidden wheel gets its own result key");
    assert!(hidden.view_cache.is_none() && hidden.cache.is_none(), "never cached as the photo's preview");
    assert_eq!(s.catalog.photo(id).unwrap().develop.grading.shadows.sat, 60.0, "the edit is untouched");
}

#[test]
fn decoder_info_survives_render_jobs_cache_and_eviction() {
    let tone = lightcraft_pipeline::tone::CameraTone::new(std::array::from_fn(|i| {
        let x = 0.01 * (i + 1) as f32;
        [x, (x * 2.0).min(0.9)]
    }))
    .unwrap();
    let info = SourceInfo { raw: true, relative_wb: true, camera_tone: Some(tone), ..Default::default() };
    let mut s = crate::Session::with_demo();
    let id = s.active().unwrap();
    let mut job = s.render_job(id, 64, 64, false, true).unwrap();
    let loaded_info = info.clone();
    job.source = SourceRef::File {
        path: "synthetic.arw".into(),
        max_edge: 64,
        loader: Some(Arc::new(move |_, _| {
            let mut image = Rgb32f::new(64, 64);
            image.data.fill([0.1; 3]);
            Ok((image, loaded_info.clone()))
        })),
        fallback: None,
        denoise: None,
        reduced: None,
    };
    job.info = SourceInfo::default(); // Header facts cannot override decoder facts.
    job.settings = Arc::new(DevelopSettings::default());
    let r = job.clone().run();
    assert_eq!(r.loaded.as_ref().unwrap().info.as_ref(), Some(&info));
    let expected = lightcraft_pipeline::render(&r.loaded.as_ref().unwrap().image, &info, &job.settings, &job.request);
    assert_eq!(r.rendered.as_ref().unwrap().image.data, expected.image.data);
    s.accept(&r);
    job.source = s.media.source_ref(s.catalog.photo(id).unwrap(), job.level);
    let again = job.run();
    assert!(again.loaded.is_none());
    assert_eq!(r.rendered.unwrap().image.data, again.rendered.unwrap().image.data);
    s.media.forget(id);
    assert!(s.media.get_source(id, r.level).is_none());
}

/// Issue #730: an ILCE-7M3 ARW from an older catalog (white balance relative to the as-shot look, 6500 / 0 as
/// its as-shot white, a Custom value on that scale) renders pixel for pixel as it did once the camera's matrices
/// give it a Kelvin scale; the decode teaches the catalog the real as-shot white (bookkeeping, not an undo step),
/// and the panel then reads As Shot in Kelvin while the stored settings keep their look.
#[test]
fn old_relative_white_balance_on_a_kelvin_camera_keeps_its_look_and_learns_the_as_shot_white() {
    use lightcraft_develop::{WbMode, WbScale};
    let mut s = crate::Session::new();
    let id = s.catalog.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: "/photos/DSC00001.ARW".into() }, "DSC00001.ARW", "ARW", 64, 64, "2026-01-01T00:00:00");
    p.kind = MediaKind::Raw;
    p.meta.camera = "SONY ILCE-7M3".into();
    p.as_shot_wb = Some((6500.0, 0.0));
    let mut d = lightcraft_develop::DevelopSettings::for_raw(6500.0, 0.0);
    (d.wb.mode, d.wb.temp, d.wb.tint) = (WbMode::Custom, 5800.0, 3.0);
    assert_eq!(d.wb.scale, WbScale::Legacy);
    p.develop = Arc::new(d.clone());
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s.selection.ids = vec![id];
    s.selection.active = Some(id);
    // the catalog's view of the source before the file is read: Kelvin scale, as-shot white not yet known
    let header = s.source_info(id);
    assert!(header.raw && !header.relative_wb && header.legacy_relative_wb);
    assert_eq!((header.as_shot_temp, header.as_shot_tint), (6500.0, 0.0));
    // the decode: what `files::load_bytes` reports for this camera (the as-shot white through its matrices)
    let decoded = SourceInfo { raw: true, legacy_relative_wb: true, as_shot_temp: 4986.0, as_shot_tint: -2.0, ..Default::default() };
    let mut pixels = Rgb32f::new(64, 64);
    for (i, px) in pixels.data.iter_mut().enumerate() {
        let v = (i % 64) as f32 / 64.0;
        *px = [0.05 + 0.6 * v, 0.04 + 0.4 * v, 0.03 + 0.3 * v];
    }
    let mut job = s.render_job(id, 64, 64, false, true).unwrap();
    let (loaded_info, loaded_pixels) = (decoded.clone(), pixels.clone());
    job.source = SourceRef::File {
        path: "/photos/DSC00001.ARW".into(),
        max_edge: 64,
        loader: Some(Arc::new(move |_, _| Ok((loaded_pixels.clone(), loaded_info.clone())))),
        fallback: None,
        denoise: None,
        reduced: None,
    };
    let undo_before = s.undo.len();
    let request = job.request;
    let r = job.run();
    let new = r.rendered.as_ref().unwrap().image.data.clone();
    // the old pipeline: the same settings against the relative-scale source facts of an older build
    let old_info = SourceInfo { raw: true, relative_wb: true, as_shot_temp: 6500.0, as_shot_tint: 0.0, ..Default::default() };
    let old = lightcraft_pipeline::render(&pixels, &old_info, &d, &request).image.data;
    assert_eq!(new, old, "the look of an existing edit changed");
    assert_ne!(new, lightcraft_pipeline::render(&pixels, &decoded, &lightcraft_develop::DevelopSettings::for_raw(4986.0, -2.0), &request).image.data);
    // accepting the result records the as-shot white, journaled but not undoable, and the settings are untouched
    s.accept(&r);
    let p = s.catalog.photo(id).unwrap();
    assert_eq!(p.as_shot_wb, Some((4986.0, -2.0)));
    assert_eq!(*p.develop, d);
    assert_eq!(s.undo.len(), undo_before);
    assert!(s.pending_log.iter().any(|op| matches!(op, Op::SetAsShotWb { id: i, wb: Some((4986.0, -2.0)) } if *i == id)));
    assert!(!p.is_edited() || p.develop.wb.mode == WbMode::Custom);
    // the panel reads the white balance in effect in Kelvin: the value the relative edit renders as
    let info = s.source_info(id);
    assert_eq!((info.as_shot_temp, info.as_shot_tint), (4986.0, -2.0));
    let shown = lightcraft_pipeline::local::effective_wb(&info, &d);
    assert_eq!(shown, lightcraft_pipeline::local::legacy_to_kelvin(5800.0, 3.0, (4986.0, -2.0)));
    assert!(shown.0 < 4986.0 && shown.0 > 4000.0, "{shown:?}");
    // accepting again changes nothing more
    let log = s.pending_log.len();
    s.accept(&r);
    assert_eq!(s.pending_log.len(), log);
    // a catalog-only (proxy) render of the same settings before the decode gave the same pixels too
    let proxy = lightcraft_pipeline::render(&pixels, &header, &d, &request).image.data;
    assert_eq!(proxy, old, "a smart preview of an existing edit changed");
}

#[test]
fn budget_evicts_least_recently_used_across_caches() {
    let img = |w: usize| Arc::new(Rgb32f::new(w, w));
    let mut m = MediaCache::default();
    let mb = 1 << 20;
    m.set_budget(30 * mb);
    // 3 previews of 12 MB (1000² × 12 B) don't fit 30 MB: the least recently used goes
    m.insert(PhotoId(1), SourceLevel::Preview, img(1000));
    m.insert(PhotoId(2), SourceLevel::Preview, img(1000));
    assert!(m.get(PhotoId(1), SourceLevel::Preview).is_some()); // 1 is now more recent than 2
    m.insert(PhotoId(3), SourceLevel::Preview, img(1000));
    assert!(m.get(PhotoId(2), SourceLevel::Preview).is_none());
    assert!(m.get(PhotoId(1), SourceLevel::Preview).is_some() && m.get(PhotoId(3), SourceLevel::Preview).is_some());
    // a thumbnail source and a rendered preview compete in the same budget
    m.insert(PhotoId(4), SourceLevel::Thumb, img(500));
    m.rendered.put(Hash128(7), Arc::new(lightcraft_raster::Rgba8::new(1000, 1000)));
    assert!(m.held() <= 30 * mb, "{}", m.held());
    // the newest preview (the one on screen) is never evicted, even alone over budget
    m.set_budget(mb);
    assert!(m.get(PhotoId(3), SourceLevel::Preview).is_some());
    assert_eq!(m.usage().1.count, 1);
    assert_eq!(m.usage().0.count, 0);
    assert_eq!(m.rendered.mem_usage().0, 0);
}

// Given Compare zoomed to 1:1 (two photos' originals), both stay decoded; back to one view,
// the least recently used goes, and the budget still evicts them like any source
#[test]
fn full_size_originals_are_kept_up_to_the_capacity() {
    let img = |w: usize| Arc::new(Rgb32f::new(w, w));
    let mut m = MediaCache::default();
    let mb = 1 << 20;
    m.set_budget(100 * mb);
    m.insert(PhotoId(1), SourceLevel::Full, img(1000));
    m.insert(PhotoId(2), SourceLevel::Full, img(1000));
    assert!(!m.has_source(PhotoId(1), SourceLevel::Full), "one original by default");
    m.set_full_capacity(2);
    m.insert(PhotoId(1), SourceLevel::Full, img(1000));
    assert!(m.has_source(PhotoId(1), SourceLevel::Full) && m.has_source(PhotoId(2), SourceLevel::Full));
    assert_eq!(m.usage().2.count, 2);
    // re-inserting a held photo replaces it instead of taking a second place
    m.insert(PhotoId(2), SourceLevel::Full, img(1000));
    assert_eq!(m.usage().2.count, 2);
    // 1 is the least recently used now
    m.set_full_capacity(1);
    assert!(!m.has_source(PhotoId(1), SourceLevel::Full) && m.has_source(PhotoId(2), SourceLevel::Full));
    m.set_full_capacity(0);
    assert_eq!(m.usage().2.count, 1, "never less than one");
    m.set_full_capacity(2);
    m.insert(PhotoId(3), SourceLevel::Full, img(1000));
    m.set_budget(15 * mb);
    assert_eq!(m.usage().2.count, 1, "the budget evicts the older original");
    assert!(m.has_source(PhotoId(3), SourceLevel::Full));
}

// Given the on-screen original alone over the budget, the photo's other sources and its neighbour's stay
// (counted, it evicted them all: each edit decoded the raw again for the thumbnail and the whole-frame render)
#[test]
fn the_on_screen_original_does_not_evict_the_other_sources() {
    let img = |w: usize| Arc::new(Rgb32f::new(w, w));
    let mut m = MediaCache::default();
    let mb = 1 << 20;
    m.set_budget(20 * mb);
    m.set_on_screen(Some(PhotoId(1)));
    m.insert(PhotoId(1), SourceLevel::Preview, img(500));
    m.insert(PhotoId(1), SourceLevel::Thumb, img(200));
    m.insert(PhotoId(1), SourceLevel::Full, img(1500)); // 27 MB
    m.insert(PhotoId(2), SourceLevel::Preview, img(500)); // a prefetched neighbour
    for (id, level) in [(1, SourceLevel::Preview), (1, SourceLevel::Thumb), (1, SourceLevel::Full), (2, SourceLevel::Preview)] {
        assert!(m.has_source(PhotoId(id), level), "{id} {level:?}");
    }
}

// A 64 MP original (797 MB as linear float) is bigger than the default cache share (768 MiB):
// evicted on insert, every pan of a zoomed view decoded the raw again. The original of the photo
// on screen stays, whatever is decoded after it (prefetched neighbours), until another is shown.
#[test]
fn the_original_on_screen_stays_even_over_budget() {
    let img = |w: usize| Arc::new(Rgb32f::new(w, w));
    let mut m = MediaCache::default();
    let mb = 1 << 20;
    m.set_budget(20 * mb);
    m.set_on_screen(Some(PhotoId(1)));
    m.insert(PhotoId(1), SourceLevel::Preview, img(500));
    m.insert(PhotoId(1), SourceLevel::Full, img(1500)); // 27 MB
    assert!(m.has_source(PhotoId(1), SourceLevel::Full));
    m.insert(PhotoId(2), SourceLevel::Preview, img(500)); // a prefetched neighbour
    m.insert(PhotoId(3), SourceLevel::Thumb, img(100));
    assert!(m.has_source(PhotoId(1), SourceLevel::Full));
    m.set_on_screen(Some(PhotoId(2)));
    assert!(!m.has_source(PhotoId(1), SourceLevel::Full));
    assert!(m.held() <= 20 * mb, "{}", m.held());
}

#[test]
fn work_gate_waits_for_room() {
    use crate::memory::WorkGate;
    let g = Arc::new(WorkGate::new(100));
    let a = g.acquire(80);
    // more than fits waits until `a` is released; urgent work never waits
    let urgent = g.acquire_urgent(500);
    assert_eq!(g.usage().0, 580);
    drop(urgent);
    let g2 = g.clone();
    let t = std::thread::spawn(move || {
        let _b = g2.acquire(50);
        g2.usage().0
    });
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(g.usage().0, 80, "the second holder is still waiting");
    drop(a);
    assert_eq!(t.join().unwrap(), 50);
    assert_eq!(g.usage().0, 0);
    // a single holder may exceed the limit
    drop(g.acquire(1000));
}

#[test]
fn levels_cover_sizes_and_large_exports_use_the_original() {
    assert_eq!(SourceLevel::for_size(256), SourceLevel::Thumb);
    assert_eq!(SourceLevel::for_size(2560), SourceLevel::Preview);
    assert_eq!(SourceLevel::for_size(2561), SourceLevel::Full);
    let mut s = crate::Session::with_demo();
    let p = s.catalog.photos().next().unwrap().clone();
    let long = p.width.max(p.height) as usize;
    assert!(long > 2560, "demo photos are camera-sized");
    // a full-size export reads the source at its native size, not an upscaled preview
    let job = s.render_job(p.id, long, long, false, true).unwrap();
    assert_eq!(job.level, SourceLevel::Full);
    match &job.source {
        SourceRef::Demo { max_edge, .. } => assert_eq!(*max_edge, long),
        _ => panic!("demo source expected"),
    }
    let loupe = s.render_job(p.id, 1600, 1600, false, true).unwrap();
    assert_eq!(loupe.level, SourceLevel::Preview);
    assert_ne!(loupe.key, s.render_job(p.id, 1600, 1600, false, true).unwrap().draft().key);
    // two photos with identical settings and size never share a result key
    let plain: Vec<_> = s.catalog.photos().filter(|p| !p.is_edited()).map(|p| p.id).take(2).collect();
    let (a, b) = (s.render_job(plain[0], 1600, 1600, false, true).unwrap(), s.render_job(plain[1], 1600, 1600, false, true).unwrap());
    assert_ne!(a.key, b.key);
}

// Issue #323: a window of a 1:1 view is its own job: the window's pixels, at the zoom scale,
// from the original, with a key of its own
#[test]
fn region_jobs_render_a_window_of_the_zoomed_frame() {
    use lightcraft_pipeline::PixelWindow;
    let mut s = crate::Session::with_demo();
    let p = s.catalog.photos().next().unwrap().clone();
    let (w, h) = (p.width as usize, p.height as usize);
    assert!(w.max(h) > 2560, "demo photos are camera-sized");
    let win = PixelWindow { x: w / 3, y: h / 3, w: 300, h: 200 };
    let job = s.region_job(p.id, w, h, win, true).unwrap();
    assert_eq!(job.level, SourceLevel::Full, "a 1:1 window reads the original");
    let img = job.run().rendered.unwrap().image;
    assert_eq!((img.width, img.height), (300, 200));
    // other windows, and the whole-frame job, never share a key
    let other = s.region_job(p.id, w, h, PixelWindow { x: win.x + 64, ..win }, true).unwrap();
    let whole = s.render_job(p.id, w, h, false, true).unwrap();
    let again = s.region_job(p.id, w, h, win, true).unwrap();
    assert_ne!(other.key, again.key);
    assert_ne!(whole.key, again.key);
    assert_eq!(s.region_job(p.id, w, h, win, true).unwrap().key, again.key, "the same window keeps its key");
    // a window never writes the photo's view preview or the thumbnail cache
    assert!(again.view_cache.is_none() && again.cache.is_none());
}

// Issue #323: a tight crop shown at its own pixels needs the original, not the 2560 px preview
#[test]
fn a_tight_crop_reads_the_source_level_its_pixels_need() {
    let mut s = crate::Session::with_demo();
    let p = s.catalog.photos().next().unwrap().clone();
    let long = p.width.max(p.height) as usize;
    assert!(long > 2560);
    let id = p.id;
    let mut d = (*s.develop_of(id).unwrap()).clone();
    // the uncropped photo at 1600 px: the preview is plenty
    assert_eq!(s.render_job(id, 1600, 1600, false, true).unwrap().level, SourceLevel::Preview);
    // a crop to 30 % of each side shown 1600 px long needs 1600 / 0.3 px of the source
    d.crop.geometry.rect = lightcraft_geom::Rect { x0: 0.2, y0: 0.2, x1: 0.5, y1: 0.5 };
    s.set_develop(id, d, "Crop").unwrap();
    assert_eq!(s.render_job(id, 1600, 1600, false, true).unwrap().level, SourceLevel::Full);
    // …but with the crop tool open (the whole photo shown) it does not
    assert_eq!(s.render_job(id, 1600, 1600, false, false).unwrap().level, SourceLevel::Preview);
    // and a crop shown small enough still reads the preview
    assert_eq!(s.render_job(id, 600, 600, false, true).unwrap().level, SourceLevel::Preview);
}

// a window whose spot reads from further away than a render can hold is refused
#[test]
fn a_region_job_is_refused_when_a_spot_reads_beyond_what_fits() {
    use lightcraft_pipeline::PixelWindow;
    let mut s = crate::Session::with_demo();
    let id = s.active().unwrap();
    let p = s.catalog.photo(id).unwrap().clone();
    let (w, h) = (p.width as usize * 8, p.height as usize * 8);
    let mut d = (*s.develop_of(id).unwrap()).clone();
    d.spots.push(lightcraft_develop::Spot {
        points: vec![lightcraft_geom::Point::new(0.2, 0.5)],
        size: 0.01,
        source_offset: Some(lightcraft_geom::Point::new(0.6, 0.0)),
        ..Default::default()
    });
    s.set_develop(id, d, "Spot").unwrap();
    let win = PixelWindow { x: (w as f64 * 0.2) as usize - 100, y: h / 2 - 100, w: 400, h: 300 };
    assert!(s.region_job(id, w, h, win, true).is_none());
    // a window elsewhere is fine
    assert!(s.region_job(id, w, h, PixelWindow { x: 5000, y: 5000, w: 400, h: 300 }, true).is_some());
}

// Issue #323: the Before side of a Before/After view at 1:1 is a window too, of the photo
// without its edits (the crop is kept), with a key of its own
#[test]
fn a_before_window_shows_the_unedited_look() {
    use lightcraft_pipeline::PixelWindow;
    let mut s = crate::Session::with_demo();
    let id = s.active().unwrap();
    let p = s.catalog.photo(id).unwrap().clone();
    let (w, h) = (p.width as usize, p.height as usize);
    let mut d = (*s.develop_of(id).unwrap()).clone();
    d.light.exposure = 2.0;
    s.set_develop(id, d, "Exposure").unwrap();
    let win = PixelWindow { x: w / 3, y: h / 3, w: 200, h: 150 };
    let after = s.region_job(id, w, h, win, true).unwrap();
    let before = s.region_job_before(id, w, h, win, true).unwrap();
    assert_ne!(after.key, before.key);
    let (a, b) = (after.run().rendered.unwrap().image, before.run().rendered.unwrap().image);
    assert_eq!((a.width, a.height), (b.width, b.height));
    let mean = |i: &Rgba8| i.data.iter().map(|p| p[1] as f64).sum::<f64>() / i.data.len() as f64;
    assert!(mean(&a) > mean(&b) + 10.0, "two stops brighter after: {} vs {}", mean(&a), mean(&b));
}

// LR-VIEW-ZOOM: what the loupe shows at 1:1 is what an export at full size writes. A window of
// the zoomed frame (the photo's own pixels) is pixel-for-pixel the export, at the centre and
// at a corner, with tone, colour and detail edits on a camera-sized photo
#[test]
fn one_to_one_window_pixels_are_export_pixels() {
    use lightcraft_pipeline::{OutputDepth, OutputSpace, PixelWindow};
    let mut s = crate::Session::with_demo();
    let id = s.active().unwrap();
    let p = s.catalog.photo(id).unwrap().clone();
    let (w, h) = (p.width as usize, p.height as usize);
    assert!(w.max(h) > 2560, "larger than the preview cap: {w}×{h}");
    let mut d = (*s.develop_of(id).unwrap()).clone();
    d.light.exposure = 0.7;
    d.light.contrast = 25.0;
    d.color.saturation = 20.0;
    s.set_develop(id, d, "Edits").unwrap();
    let export = s.render_export(id, w, h, OutputSpace::Srgb, OutputDepth::U8).unwrap().image;
    assert_eq!((export.width, export.height), (w, h), "a full-size export");
    for win in [PixelWindow { x: w / 2 - 256, y: h / 2 - 256, w: 512, h: 384 }, PixelWindow { x: 0, y: h - 300, w: 400, h: 300 }] {
        let tile = s.region_job(id, w, h, win, true).unwrap().run().rendered.unwrap().image;
        assert_eq!((tile.width, tile.height), (win.w, win.h));
        let mut worst = 0u8;
        for y in 0..win.h {
            for x in 0..win.w {
                let (a, b) = (tile.data[y * win.w + x], export.data[(win.y + y) * w + win.x + x]);
                for k in 0..3 {
                    worst = worst.max(a[k].abs_diff(b[k]));
                }
            }
        }
        assert!(worst <= 1, "window {win:?}: the 1:1 view differs from the export by up to {worst} levels");
    }
}

/// The thumbnail of a photo whose preview-level source is decoded is cut from it: the file (here
/// one nothing can decode) is not read again.
#[test]
fn a_thumbnail_is_cut_from_the_decoded_preview() {
    let mut s = crate::Session::new();
    let p = Photo::new(PhotoId(1), Source::File { path: "/nowhere/a.ARW".into() }, "a.ARW", "ARW", 2400, 1600, "");
    s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(p.clone()) }).unwrap();
    s.media.insert(p.id, SourceLevel::Preview, Arc::new(Rgb32f::from_fn(2400, 1600, |x, _| [x as f32 / 2400.0, 0.5, 0.25])));
    let job = s.thumb_job(p.id, 512).unwrap();
    assert!(matches!(job.source, SourceRef::Fit { .. }));
    let r = job.run();
    let thumb = r.rendered.as_ref().unwrap();
    assert_eq!((thumb.image.width, thumb.image.height), (512, 341));
    s.accept(&r);
    let cached = s.media.get(p.id, SourceLevel::Thumb).unwrap();
    assert_eq!((cached.width, cached.height), (512, 341), "a thumbnail-sized source is kept, not the preview");
}

#[test]
fn has_source_follows_what_the_session_accepted() {
    let mut s = crate::Session::with_demo();
    let id = s.active().unwrap();
    let p = s.catalog.photo(id).unwrap().clone();
    let long = p.width.max(p.height) as usize;
    assert!(!s.media.has_source(id, SourceLevel::Full));
    let r = s.render_job(id, long, long, false, true).unwrap().run();
    s.accept(&r);
    assert!(s.media.has_source(id, SourceLevel::Full));
    assert!(!s.media.has_source(id, SourceLevel::Preview));
    assert!(!s.media.has_source(PhotoId(id.0 + 1), SourceLevel::Full));
}

#[test]
fn variant_jobs_have_their_own_keys_and_hit_the_cache() {
    let mut s = crate::Session::with_demo();
    let id = s.active().unwrap();
    let mine = (*s.develop_of(id).unwrap()).clone();
    let with = |pid: &str| {
        let mut d = mine.clone();
        d.profile.id = pid.into();
        d
    };
    let (a, b) = (with("lc.vivid"), with("lc.bw.sepia"));
    let ja = s.variant_job(id, &a, 128).unwrap();
    let jb = s.variant_job(id, &b, 128).unwrap();
    assert_ne!(ja.key, jb.key, "keys differ per settings");
    assert_eq!(ja.key, s.variant_job(id, &a, 128).unwrap().key, "stable");
    assert_ne!(ja.key, s.variant_job(id, &a, 256).unwrap().key, "and per size");
    assert_ne!(ja.key, s.thumb_job(id, 128).unwrap().key, "not the photo's own thumbnail");
    assert_eq!(ja.level, SourceLevel::Thumb);
    let (cache, ck) = ja.cache.clone().unwrap();
    assert!(cache.get(ck).is_none());
    let r = ja.run();
    s.accept(&r);
    let first = r.rendered.unwrap().image;
    assert!(first.width.max(first.height) <= 128);
    assert!(cache.get(ck).is_some(), "rendered variants are cached");
    assert!(cache.get(jb.cache.as_ref().unwrap().1).is_none());
    // a second job for the same variant is served from the cache, pixel for pixel
    let again = s.variant_job(id, &a, 128).unwrap().run();
    assert!(again.loaded.is_none());
    assert_eq!(again.rendered.unwrap().image.as_bytes(), first.as_bytes());
    let other = jb.run().rendered.unwrap().image;
    assert_ne!(other.as_bytes(), first.as_bytes());
    // the photo's own settings never changed
    assert_eq!(*s.develop_of(id).unwrap(), mine);
    // hover previews: a loupe-sized job with other settings, its own key
    let pv = s.preview_job(id, 800, 600, true, &a).unwrap();
    assert_ne!(pv.key, s.render_job(id, 800, 600, false, true).unwrap().key);
    assert_eq!(*pv.settings, a);
}
