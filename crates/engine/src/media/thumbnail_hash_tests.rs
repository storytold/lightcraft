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
fn learning_a_hash_drops_no_source_in_flight() {
    let mut s = session();
    s.media.file_loader = Some(Arc::new(|_, _, _| Ok((Rgb32f::new(8, 8), SourceInfo::default()))));
    let old = s.thumb_job(PhotoId(1), 256).unwrap().run();
    let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
    s.apply_system(Op::SetContent {
        id: p.id,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: Some("learned".into()),
        preview_only: None,
        preview_checked: String::new(),
    })
    .unwrap();
    s.accept(&old);
    assert!(s.media.has_source(p.id, SourceLevel::Thumb));
}

#[test]
fn a_thumbnail_cached_before_the_hash_is_found_after_it() {
    let mut s = session();
    s.media.file_loader = Some(Arc::new(|_, _, _| Ok((Rgb32f::new(8, 8), SourceInfo::default()))));
    let old = s.thumb_job(PhotoId(1), 256).unwrap().run();
    assert!(old.rendered.is_ok());
    let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
    s.apply_system(Op::SetContent {
        id: p.id,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: Some("learned".into()),
        preview_only: None,
        preview_checked: String::new(),
    })
    .unwrap();
    s.media.file_loader = Some(Arc::new(|_, _, _| Err("must use cached thumbnail".into())));
    let r = s.thumb_job(p.id, 256).unwrap().run();
    assert!(r.rendered.is_ok(), "{:?}", r.rendered.err());
    assert!(r.loaded.is_none());
}

#[test]
fn a_changed_file_of_the_same_size_does_not_fall_back_to_old_data() {
    let dir = crate::tests_xmp::temp_dir("content-alias-replaced");
    let mut s = session();
    s.media.attach_disk_cache(&dir.join("thumbs"), 1 << 20);
    s.media.file_loader = Some(Arc::new(|_, _, _| Ok((Rgb32f::new(8, 8), SourceInfo::default()))));
    let old_job = s.thumb_job(PhotoId(1), 256).unwrap();
    let old_key = old_job.cache.as_ref().unwrap().1;
    let old = old_job.clone().run();
    assert!(old.rendered.is_ok());
    let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
    s.apply_system(Op::SetContent {
        id: p.id,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: Some("before".into()),
        preview_only: None,
        preview_checked: String::new(),
    })
    .unwrap();
    s.commit(
        "Replace",
        Op::SetContent {
            id: p.id,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: Some("after".into()),
            preview_only: None,
            preview_checked: String::new(),
        },
    )
    .unwrap();
    s.accept(&old);
    assert!(!s.media.has_source(p.id, SourceLevel::Thumb));
    let _late = old_job.run();
    s.media.rendered.put(old_key, Arc::new(Rgba8::new(8, 8)));
    let changed = (**s.catalog.photo(p.id).unwrap()).clone();
    let mut reopened = crate::Session::new();
    reopened.catalog.apply(Op::AddPhoto { photo: Box::new(changed) }).unwrap();
    reopened.media.attach_disk_cache(&dir.join("thumbs"), 1 << 20);
    reopened.media.file_loader = Some(Arc::new(|_, _, _| Err("replacement is offline".into())));
    let result = reopened.thumb_job(p.id, 256).unwrap().run();
    assert!(result.rendered.is_err(), "must not show the cached old pixels after reopening");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn f_repair_copy_fallbacks_use_the_masters_physical_size() {
    let mut s = session();
    let master = s.catalog.photo(PhotoId(1)).unwrap().clone();
    s.apply_system(Op::SetContent {
        id: master.id,
        width: master.width,
        height: master.height,
        file_size: master.file_size,
        content_hash: Some("learned".into()),
        preview_only: master.preview_only.clone(),
        preview_checked: master.preview_checked.clone(),
    })
    .unwrap();
    let mut copy = (**s.catalog.photo(master.id).unwrap()).clone();
    copy.id = PhotoId(2);
    copy.copy_of = Some(master.id);
    copy.file_size = master.file_size.saturating_add(1);
    s.catalog.apply(Op::AddPhoto { photo: Box::new(copy.clone()) }).unwrap();
    let resolved = s.cache_photo(copy.id).unwrap();
    assert_eq!(resolved.file_size, master.file_size);
    assert_eq!(s.media.content_keys(&resolved), s.media.content_keys(s.catalog.photo(master.id).unwrap()));
    assert!(!s.media.content_keys(&resolved).contains(&crate::cache_identity::path_key(&copy).unwrap()));
}

#[test]
fn f_repair_failed_marker_write_drops_populated_sources() {
    for fault_stage in 0..if cfg!(unix) { 6 } else { 2 } {
        let sentinel_failure = fault_stage == 1;
        let dir = crate::tests_xmp::temp_dir(&format!("failure-populated-{fault_stage}"));
        let mut s = session();
        s.media.attach_disk_cache(&dir.join("thumbs"), 1 << 20);
        let id = PhotoId(1);
        let p = s.catalog.photo(id).unwrap().clone();
        let content = |hash: &str| Op::SetContent {
            id,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: Some(hash.into()),
            preview_only: p.preview_only.clone(),
            preview_checked: p.preview_checked.clone(),
        };
        s.apply_system(content("before")).unwrap();
        s.media.file_loader = Some(Arc::new(|_, _, _| Ok((Rgb32f::new(8, 8), SourceInfo::default()))));
        let old_job = s.thumb_job(id, 256).unwrap();
        let old_key = old_job.cache.as_ref().unwrap().1;
        let old = old_job.run();
        assert!(old.rendered.is_ok());
        s.accept(&old);
        s.media.smart_dir = Some(dir.join("smart"));
        let stale_paths = s.smart_preview_paths(s.catalog.photo(id).unwrap());
        let proxy = crate::smart::encode(&Rgb32f::new(8, 8), None).unwrap();
        let mut denoised_source = DecodedSource::new(Arc::new(Rgb32f::new(8, 8)), None);
        denoised_source.denoised = Some(Arc::new(Twin::new(Arc::new(Rgb32f::new(8, 8)))));
        s.media.insert_source(id, SourceLevel::Full, denoised_source);
        s.media.insert(id, SourceLevel::Thumb, Arc::new(Rgb32f::new(8, 8)));
        s.media.insert(id, SourceLevel::Preview, Arc::new(Rgb32f::new(8, 8)));
        s.media.denoise.set(id, DenoiseSpec { product: dir.join("old.lcdn"), key: "before".into(), make: None });
        s.faces.scanned.insert(id.0);
        let ticket = s.media.identities.ticket(&p);
        let fault = if sentinel_failure {
            std::fs::create_dir(dir.join("source-invalidations-enabled")).unwrap();
            None
        } else if fault_stage == 0 {
            Some(lightcraft_catalog::safe_file::fail_writes_after(0))
        } else {
            None
        };
        #[cfg(unix)]
        let directory_fault = (fault_stage >= 2).then(|| crate::cache_identity::fail_directory_after(fault_stage - 2));
        assert!(s.commit("Replace", content("after")).is_err());
        drop(fault);
        #[cfg(unix)]
        drop(directory_fault);
        assert_eq!(s.catalog.photo(id).unwrap().content_hash.as_deref(), Some("before"));
        assert!(!ticket.current());
        assert!(!s.media.has_source(id, SourceLevel::Thumb), "old thumbnail survived failure");
        assert!(!s.media.has_source(id, SourceLevel::Preview), "old preview survived failure");
        assert!(s.media.denoise.spec(id).is_none());
        assert!(!s.media.has_source(id, SourceLevel::Full));
        assert!(!s.faces.scanned.contains(id.0));
        assert!(!s.media.content_keys(s.catalog.photo(id).unwrap()).contains(&"before".to_string()));
        s.media.rendered.put(old_key, Arc::new(Rgba8::new(8, 8)));
        for path in &stale_paths {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, &proxy).unwrap();
        }
        s.media.file_loader = Some(Arc::new(|_, _, _| Err("replaced source offline".into())));
        assert!(s.thumb_job(id, 256).unwrap().run().rendered.is_err());
        let smart = s.execute("library.smartPreviews", &serde_json::json!({"ids":[id.0]})).unwrap();
        assert_eq!(smart["built"], 0);
        assert_eq!(smart["failed"].as_array().unwrap().len(), 1);
        if sentinel_failure {
            std::fs::remove_dir(dir.join("source-invalidations-enabled")).unwrap();
        }
        s.commit("Retry", content("after")).unwrap();
        let reopened = crate::cache_identity::CacheIdentity::open(&dir.join("source-invalidations"));
        assert!(!reopened.keys(&p).contains(&crate::cache_identity::path_key(&p).unwrap()));
        let current = s.catalog.photo(id).unwrap().clone();
        let mut s = crate::Session::new();
        s.catalog.apply(Op::AddPhoto { photo: Box::new((*current).clone()) }).unwrap();
        s.media.attach_disk_cache(&dir.join("thumbs"), 1 << 20);
        s.media.smart_dir = Some(dir.join("smart"));
        s.media.file_loader = Some(Arc::new(|_, _, _| Err("still offline after recovery and reopen".into())));
        assert!(s.thumb_job(id, 256).unwrap().run().rendered.is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn f_repair_system_replacement_retires_sources_without_undo() {
    let mut s = session();
    let id = PhotoId(1);
    let p = s.catalog.photo(id).unwrap().clone();
    let content = |hash: &str| Op::SetContent {
        id,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: Some(hash.into()),
        preview_only: p.preview_only.clone(),
        preview_checked: p.preview_checked.clone(),
    };
    s.apply_system(content("before")).unwrap();
    s.media.insert(id, SourceLevel::Preview, Arc::new(Rgb32f::new(8, 8)));
    s.faces.scanned.insert(id.0);
    let ticket = s.media.identities.ticket(&p);
    let stacks = (s.undo.len(), s.redo.len());
    s.apply_system(content("after")).unwrap();
    assert!(!ticket.current());
    assert!(!s.media.has_source(id, SourceLevel::Preview));
    assert!(!s.faces.scanned.contains(id.0));
    assert_eq!((s.undo.len(), s.redo.len()), stacks);
}

#[test]
fn failed_alias_invalidation_refuses_replacement_and_blocks_the_old_source() {
    let dir = crate::tests_xmp::temp_dir("content-alias-failed");
    let mut s = session();
    s.media.attach_disk_cache(&dir.join("thumbs"), 1 << 20);
    std::fs::write(dir.join("source-invalidations"), b"not a directory").unwrap();
    let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
    s.apply_system(Op::SetContent {
        id: p.id,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: Some("before".into()),
        preview_only: None,
        preview_checked: String::new(),
    })
    .unwrap();
    s.media.insert(p.id, SourceLevel::Preview, Arc::new(Rgb32f::new(8, 8)));
    s.media.file_loader = Some(Arc::new(|_, _, _| Ok((Rgb32f::new(8, 8), SourceInfo::default()))));
    assert!(s.thumb_job(p.id, 256).unwrap().run().rendered.is_ok());
    let result = s.commit(
        "Replace",
        Op::SetContent {
            id: p.id,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: Some("after".into()),
            preview_only: None,
            preview_checked: String::new(),
        },
    );
    assert!(result.is_err());
    let kept = s.catalog.photo(p.id).unwrap().clone();
    assert_eq!(kept.content_hash.as_deref(), Some("before"));
    assert!(!s.media.has_source(p.id, SourceLevel::Preview), "the old decoded source is dropped");
    assert!(!s.media.content_keys(&kept).contains(&"before".to_string()), "the old content's cached work is fenced");
    s.media.file_loader = Some(Arc::new(|_, _, _| Err("must not reuse the old thumbnail".into())));
    assert!(s.thumb_job(p.id, 256).unwrap().run().rendered.is_err());
    let _ = std::fs::remove_dir_all(dir);
}
#[test]
fn an_old_library_source_cannot_enter_a_reopened_library() {
    let mut s = session();
    s.media.file_loader = Some(Arc::new(|_, _, _| Ok((Rgb32f::new(8, 8), SourceInfo::default()))));
    let old = s.thumb_job(PhotoId(1), 256).unwrap().run();
    let mut reopened = session();
    reopened.accept(&old);
    assert!(!reopened.media.has_source(PhotoId(1), SourceLevel::Thumb));
}

#[test]
fn learning_through_a_batch_keeps_decoded_sources_and_faces() {
    let mut s = session();
    let id = PhotoId(1);
    s.media.insert(id, SourceLevel::Preview, Arc::new(Rgb32f::new(8, 8)));
    s.faces.scanned.insert(id.0);
    let p = s.catalog.photo(id).unwrap().clone();
    s.commit(
        "Learn",
        Op::Batch {
            ops: vec![Op::Batch {
                ops: vec![Op::SetContent {
                    id,
                    width: p.width,
                    height: p.height,
                    file_size: p.file_size,
                    content_hash: Some("learned".into()),
                    preview_only: p.preview_only.clone(),
                    preview_checked: p.preview_checked.clone(),
                }],
            }],
        },
    )
    .unwrap();
    assert!(s.media.has_source(id, SourceLevel::Preview));
    assert!(s.faces.scanned.contains(id.0));
    s.undo_step().unwrap();
    assert!(s.media.has_source(id, SourceLevel::Preview), "undo learning dropped unchanged source");
    assert!(s.faces.scanned.contains(id.0));
    s.redo_step().unwrap();
    assert!(s.media.has_source(id, SourceLevel::Preview));
    assert!(s.faces.scanned.contains(id.0));
}

#[test]
fn f_inventory_a_virtual_copy_finds_the_masters_smart_preview_after_learning() {
    let dir = crate::tests_xmp::temp_dir("virtual-content-alias");
    let mut s = session();
    s.media.smart_dir = Some(dir.clone());
    let master = (**s.catalog.photo(PhotoId(1)).unwrap()).clone();
    let mut copy = master.clone();
    copy.id = PhotoId(2);
    copy.copy_of = Some(master.id);
    s.catalog.apply(Op::AddPhoto { photo: Box::new(copy) }).unwrap();
    let path = crate::smart::sharded_file_path(&dir, &master);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, crate::smart::encode(&Rgb32f::new(8, 8), None).unwrap()).unwrap();
    s.apply_system(Op::SetContent {
        id: master.id,
        width: master.width,
        height: master.height,
        file_size: master.file_size,
        content_hash: Some("learned".into()),
        preview_only: None,
        preview_checked: String::new(),
    })
    .unwrap();
    s.media.file_loader = Some(Arc::new(|_, _, _| Err("offline".into())));
    assert!(s.render_now(PhotoId(2), 16, 16).is_ok());
    assert!(crate::cmd::previews::has_smart_preview(&s, master.id));
    assert!(crate::cmd::previews::has_smart_preview(&s, PhotoId(2)));
    assert!(s.catalog.photo(PhotoId(2)).unwrap().content_hash.is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn f_inventory_replacement_retires_old_and_new_size_aliases() {
    let dir = crate::tests_xmp::temp_dir("changed-size-alias");
    let mut s = session();
    s.media.smart_dir = Some(dir.clone());
    let mut replacement = (**s.catalog.photo(PhotoId(1)).unwrap()).clone();
    replacement.file_size = 100;
    let stale = crate::smart::sharded_file_path(&dir, &replacement);
    std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
    std::fs::write(&stale, crate::smart::encode(&Rgb32f::new(8, 8), None).unwrap()).unwrap();
    s.media.file_loader = Some(Arc::new(|_, _, _| Err("offline".into())));
    s.commit(
        "Reload",
        Op::SetContent {
            id: replacement.id,
            width: replacement.width,
            height: replacement.height,
            file_size: replacement.file_size,
            content_hash: None,
            preview_only: None,
            preview_checked: String::new(),
        },
    )
    .unwrap();
    assert!(s.render_now(replacement.id, 16, 16).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn f_inventory_memory_library_switch_retires_rendered_aliases_and_writers() {
    let mut s = session();
    let photo = s.catalog.photo(PhotoId(1)).unwrap().clone();
    s.media.file_loader = Some(Arc::new(|_, _, _| Ok((Rgb32f::new(8, 8), SourceInfo::default()))));
    assert!(s.thumb_job(photo.id, 256).unwrap().run().rendered.is_ok());
    let late = s.thumb_job(photo.id, 256).unwrap();
    s.open_library_in(
        crate::library::LibraryStores {
            dir: "isolated-memory".into(),
            catalog: Box::new(lightcraft_catalog::MemStore::new()),
            files: Box::new(lightcraft_catalog::MemStore::new()),
            on_disk: false,
        },
        false,
    )
    .unwrap();
    s.catalog.apply(Op::AddPhoto { photo: Box::new((*photo).clone()) }).unwrap();
    assert!(late.run().rendered.is_err(), "the retired library ticket rejects loading before it can publish");
    s.media.file_loader = Some(Arc::new(|_, _, _| Ok((Rgb32f::from_fn(8, 8, |_, _| [1.0; 3]), SourceInfo::default()))));
    let current = s.thumb_job(photo.id, 256).unwrap().run();
    assert!(current.loaded.is_some(), "must decode this library's source instead of accepting the old rendered alias");
    assert_eq!(current.loaded.unwrap().image.data.first(), Some(&[1.0; 3]));
}

#[test]
fn f_inventory_learning_keeps_valid_work_from_the_current_retired_alias_epoch() {
    let mut s = session();
    let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
    s.media.identities.invalidate(&p, None, || ()).unwrap();
    s.media.file_loader = Some(Arc::new(|_, _, _| Ok((Rgb32f::new(8, 8), SourceInfo::default()))));
    let result = s.thumb_job(p.id, 256).unwrap().run();
    assert!(result.rendered.is_ok());
    s.apply_system(Op::SetContent {
        id: p.id,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: Some("learned".into()),
        preview_only: None,
        preview_checked: String::new(),
    })
    .unwrap();
    s.accept(&result);
    assert!(s.media.has_source(p.id, SourceLevel::Thumb));
    s.media.forget(p.id);
    s.media.file_loader = Some(Arc::new(|_, _, _| Err("must reuse the rendered cache".into())));
    let cached = s.thumb_job(p.id, 256).unwrap().run();
    assert!(cached.rendered.is_ok());
    assert!(cached.loaded.is_none());
}

#[test]
fn f_inventory_offline_and_build_use_a_valid_flat_alias_after_a_bad_primary() {
    let dir = crate::tests_xmp::temp_dir("smart-flat-fallback");
    let mut s = session();
    s.media.smart_dir = Some(dir.clone());
    let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
    let alias = dir.join(crate::smart::file_name(&p));
    std::fs::write(&alias, crate::smart::encode(&Rgb32f::new(8, 8), None).unwrap()).unwrap();
    s.apply_system(Op::SetContent {
        id: p.id,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: Some("learned".into()),
        preview_only: None,
        preview_checked: String::new(),
    })
    .unwrap();
    let primary = crate::smart::sharded_file_path(&dir, s.catalog.photo(p.id).unwrap());
    std::fs::create_dir_all(primary.parent().unwrap()).unwrap();
    std::fs::write(&primary, b"damaged").unwrap();
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = reads.clone();
    s.media.file_loader = Some(Arc::new(move |_, _, _| {
        count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Err("offline".into())
    }));
    assert!(s.source_now(p.id, SourceLevel::Preview).is_ok());
    let report = s.execute("library.smartPreviews", &serde_json::json!({"ids":[p.id.0]})).unwrap();
    assert_eq!(report["repaired"], 0, "{report}");
    assert_eq!(report["failed"], serde_json::json!([]));
    assert_eq!(reads.load(std::sync::atomic::Ordering::Relaxed), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn f_inventory_invalidated_hashless_sources_share_no_cache_alias() {
    let mut s = session();
    let first = s.catalog.photo(PhotoId(1)).unwrap().clone();
    let mut second = (*first).clone();
    second.id = PhotoId(2);
    second.source = Source::File { path: "distinct-source.jpg".into() };
    s.catalog.apply(Op::AddPhoto { photo: Box::new(second.clone()) }).unwrap();
    s.media.identities.invalidate(&first, None, || ()).unwrap();
    s.media.identities.invalidate(&second, None, || ()).unwrap();
    let a = s.media.content_keys(&s.cache_photo(first.id).unwrap());
    let b = s.media.content_keys(&s.cache_photo(second.id).unwrap());
    assert!(a.iter().all(|key| !b.contains(key)), "different unknown contents must not share an alias: {a:?} / {b:?}");
}

#[test]
fn f_inventory_pixel_statistics_find_the_masters_hash_only_proxy() {
    let dir = crate::tests_xmp::temp_dir("pixel-stats-virtual-proxy");
    let mut s = session();
    s.media.smart_dir = Some(dir.clone());
    let mut master = (**s.catalog.photo(PhotoId(1)).unwrap()).clone();
    master.content_hash = Some("learned".into());
    let mut copy = master.clone();
    copy.id = PhotoId(2);
    copy.copy_of = Some(master.id);
    copy.content_hash = None;
    s.catalog.apply(Op::RemovePhoto { id: master.id }).unwrap();
    s.catalog.apply(Op::AddPhoto { photo: Box::new(master.clone()) }).unwrap();
    s.catalog.apply(Op::AddPhoto { photo: Box::new(copy.clone()) }).unwrap();
    let proxy = crate::smart::sharded_file_path(&dir, &master);
    std::fs::create_dir_all(proxy.parent().unwrap()).unwrap();
    std::fs::write(proxy, crate::smart::encode(&Rgb32f::new(8, 8), None).unwrap()).unwrap();
    s.media.file_loader = Some(Arc::new(|_, _, _| Err("offline".into())));
    assert!(s.source_now(copy.id, SourceLevel::Preview).is_ok());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn f_inventory_loupe_writes_the_guarded_view_cache_identity() {
    let mut s = session();
    let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
    s.media.identities.invalidate(&p, None, || ()).unwrap();
    s.media.file_loader = Some(Arc::new(|_, _, _| Ok((Rgb32f::new(8, 8), SourceInfo::default()))));
    assert!(s.loupe_job(p.id, 16, 16, true).unwrap().run().rendered.is_ok());
    let result = s.quick_view_job(p.id, 16, true).unwrap().run();
    assert_eq!(result.quick, Some(QuickSource::Cached));
}

#[test]
fn f_inventory_renamed_source_rejects_an_old_path_job_with_the_same_hash() {
    let mut s = session();
    let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
    s.apply_system(Op::SetContent {
        id: p.id,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: Some("known".into()),
        preview_only: None,
        preview_checked: String::new(),
    })
    .unwrap();
    s.media.file_loader = Some(Arc::new(|_, _, _| Ok((Rgb32f::new(8, 8), SourceInfo::default()))));
    let result = s.thumb_job(p.id, 256).unwrap().run();
    s.commit("Rename", Op::SetFile { id: p.id, file_name: "other.jpg".into(), source: Source::File { path: "other.jpg".into() } }).unwrap();
    s.accept(&result);
    assert!(!s.media.has_source(p.id, SourceLevel::Thumb));
}

#[test]
fn f_inventory_relink_cannot_revive_a_destination_path_alias() {
    let dir = crate::tests_xmp::temp_dir("relink-cache-alias");
    let mut s = session();
    s.media.smart_dir = Some(dir.clone());
    let mut destination = (**s.catalog.photo(PhotoId(1)).unwrap()).clone();
    destination.source = Source::File { path: "other.jpg".into() };
    let old_proxy = crate::smart::sharded_file_path(&dir, &destination);
    std::fs::create_dir_all(old_proxy.parent().unwrap()).unwrap();
    std::fs::write(&old_proxy, crate::smart::encode(&Rgb32f::new(8, 8), None).unwrap()).unwrap();
    s.media.file_loader = Some(Arc::new(|_, _, _| Err("offline".into())));
    s.commit("Relink", Op::Relink { id: destination.id, file_name: "other.jpg".into(), source: destination.source.clone(), format: None }).unwrap();
    assert!(s.render_now(destination.id, 16, 16).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn reloading_a_virtual_copy_updates_its_shared_physical_identity() {
    let mut s = session();
    let mut master = (**s.catalog.photo(PhotoId(1)).unwrap()).clone();
    master.content_hash = Some("before".into());
    let mut copy = master.clone();
    copy.id = PhotoId(2);
    copy.copy_of = Some(master.id);
    copy.content_hash = None;
    s.catalog.apply(Op::RemovePhoto { id: master.id }).unwrap();
    s.catalog.apply(Op::AddPhoto { photo: Box::new(master.clone()) }).unwrap();
    s.catalog.apply(Op::AddPhoto { photo: Box::new(copy) }).unwrap();
    let info = ProbeInfo {
        width: master.width,
        height: master.height,
        file_size: master.file_size,
        content_hash: Some("after".into()),
        ..Default::default()
    };
    let (mut ops, mut changed) = (Vec::new(), Vec::new());
    crate::cmd::convert::reload_ops(&s.catalog, PhotoId(2), &info, &mut ops, &mut changed);
    s.commit("Reload", Op::Batch { ops }).unwrap();
    assert_eq!(s.cache_photo(PhotoId(2)).unwrap().content_hash.as_deref(), Some("after"));
    assert_eq!(s.catalog.photo(master.id).unwrap().content_hash.as_deref(), Some("after"));
}

#[test]
fn reloading_unchanged_duplicate_bytes_keeps_the_suffix_and_cached_work() {
    let mut s = session();
    let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
    s.apply_system(Op::SetContent {
        id: p.id,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: Some("0123456789abcdef0123456789abcdef:dup1".into()),
        preview_only: None,
        preview_checked: String::new(),
    })
    .unwrap();
    s.media.insert(p.id, SourceLevel::Preview, Arc::new(Rgb32f::new(8, 8)));
    s.faces.scanned.insert(p.id.0);
    let info = ProbeInfo {
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: Some("0123456789abcdef0123456789abcdef".into()),
        ..Default::default()
    };
    let (mut ops, mut changed) = (Vec::new(), Vec::new());
    crate::cmd::convert::reload_ops(&s.catalog, p.id, &info, &mut ops, &mut changed);
    assert!(ops.is_empty());
    assert!(s.catalog.photo(p.id).unwrap().content_hash.as_deref().unwrap().ends_with(":dup1"));
    assert!(s.media.has_source(p.id, SourceLevel::Preview));
    assert!(s.faces.scanned.contains(p.id.0));
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
        embedded_fallback: Vec::new(),
        embedded_on_disk: false,
        minimum_embedded_edge: None,
        small: None,
    };
    let r = quick.run();
    assert_eq!((r.request_id, r.key), (7, 9));
    assert_eq!(r.rendered.err().as_deref(), Some("the render failed unexpectedly: preview reader bug"));
}

/// A raw photo's smart preview has no mosaic left: it stands in with the box on or off, so an offline raw keeps
/// reusing the decoded proxy instead of reading the file again for every render.
#[test]
fn a_smart_preview_fits_either_hot_pixel_setting() {
    let raw = SourceInfo { raw: true, ..Default::default() };
    let decoded = DecodedSource::new(Arc::new(Rgb32f::new(8, 8)), Some(raw));
    assert!(decoded.fits_outliers(false) && !decoded.fits_outliers(true), "a raw decode must match the box");
    let proxy = DecodedSource { from_proxy: true, ..decoded };
    assert!(proxy.fits_outliers(false) && proxy.fits_outliers(true));
}

/// The Remove Hot Pixels box decides how a raw is decoded (#1345): the loader is told, and a source decoded with
/// the other setting is not reused. A rendered file has no mosaic, so its source stays.
#[test]
fn hot_pixel_box_decodes_the_raw_again() {
    let mut s = session();
    let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = asked.clone();
    let raw = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let is_raw = raw.clone();
    s.media.file_loader = Some(Arc::new(move |_, _, with: crate::media::RawDecode<'_>| {
        log.lock().unwrap().push(with.outliers);
        Ok((Rgb32f::new(8, 8), SourceInfo { raw: is_raw.load(std::sync::atomic::Ordering::Relaxed), ..Default::default() }))
    }));
    let set = |s: &mut crate::Session, on: bool| {
        let mut d = (*s.catalog.photo(PhotoId(1)).unwrap().develop).clone();
        d.detail.nr_outliers = on;
        // a new edit each time, so a render cached for earlier settings is not simply shown again
        d.light.exposure += 0.1;
        s.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(d), label: "box".into(), edited: None }).unwrap();
    };
    let render = |s: &mut crate::Session| {
        let r = s.thumb_job(PhotoId(1), 256).unwrap().run();
        s.accept(&r);
    };
    render(&mut s);
    render(&mut s);
    assert_eq!(*asked.lock().unwrap(), [false], "decoded once, then cached");
    set(&mut s, true);
    render(&mut s);
    render(&mut s);
    assert_eq!(*asked.lock().unwrap(), [false, true], "switched on: decoded again with the filter, then cached");
    set(&mut s, false);
    render(&mut s);
    assert_eq!(*asked.lock().unwrap(), [false, true, false]);
    // not a raw: the box changes nothing, the cached source is kept
    raw.store(false, std::sync::atomic::Ordering::Relaxed);
    s.media.forget(PhotoId(1));
    set(&mut s, false);
    render(&mut s);
    set(&mut s, true);
    render(&mut s);
    assert_eq!(*asked.lock().unwrap(), [false, true, false, false]);
}

/// With the Detail section's eye off, Remove Hot Pixels contributes nothing either: the raw is decoded without it.
#[test]
fn detail_eye_off_decodes_the_raw_without_hot_pixel_removal() {
    let mut s = session();
    let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = asked.clone();
    s.media.file_loader = Some(Arc::new(move |_, _, with: crate::media::RawDecode<'_>| {
        log.lock().unwrap().push(with.outliers);
        Ok((Rgb32f::new(8, 8), SourceInfo { raw: true, ..Default::default() }))
    }));
    for eye in [true, false] {
        let mut d = (*s.catalog.photo(PhotoId(1)).unwrap().develop).clone();
        d.detail.nr_outliers = true;
        d.set_section_enabled("detail", eye);
        d.light.exposure += 0.1;
        s.catalog.apply(Op::SetDevelop { id: PhotoId(1), settings: Arc::new(d), label: "eye".into(), edited: None }).unwrap();
        let r = s.thumb_job(PhotoId(1), 256).unwrap().run();
        s.accept(&r);
    }
    assert_eq!(*asked.lock().unwrap(), [true, false]);
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
            embedded_fallback: Vec::new(),
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
            embedded_fallback: Vec::new(),
            embedded_on_disk: on_disk,
            minimum_embedded_edge: None,
            small: None,
        };
        assert!(quick.run().rendered.is_ok());
        assert!(cache.get_memory(key).is_some(), "on disk {on_disk}: kept when made after the clear");
    }
}

#[test]
fn a_folder_the_app_moves_keeps_its_photos_cached_work_both_ways() {
    let dir = crate::tests_xmp::temp_dir("folder-move-keeps-caches");
    let trip = dir.join("trip");
    std::fs::create_dir_all(&trip).unwrap();
    let mut s = crate::Session::new();
    s.media.attach_disk_cache(&dir.join("library/thumbs"), 1 << 20);
    for i in 0..40u64 {
        let path = trip.join(format!("{i}.jpg")).to_string_lossy().to_string();
        let mut p = Photo::new(PhotoId(100 + i), Source::File { path }, &format!("{i}.jpg"), "JPEG", 60, 40, "2026-01-01");
        p.content_hash = Some(format!("hash{i}"));
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    s.faces.scanned.insert(100);
    let syncs = lightcraft_catalog::safe_file::syncs_on_this_thread();
    s.execute("folder.rename", &serde_json::json!({"path": trip, "name": "holiday"})).unwrap();
    assert!(lightcraft_catalog::safe_file::syncs_on_this_thread() - syncs < 10, "one fence write for the whole folder");
    assert!(s.catalog.photos().all(|p| !s.media.identities.needs_validation(p)), "moved photos keep their cache keys");
    assert!(s.faces.scanned.contains(100));
    s.execute("edit.undo", &serde_json::json!({})).unwrap();
    assert!(s.catalog.photos().all(|p| p.source == Source::File { path: trip.join(p.file_name.as_str()).to_string_lossy().to_string() }));
    assert!(s.catalog.photos().all(|p| !s.media.identities.needs_validation(p)), "and keep them when undo moves the folder back");
    assert!(s.faces.scanned.contains(100));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_replacement_retires_every_record_of_the_same_file() {
    let mut s = session();
    let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
    s.apply_system(Op::SetContent {
        id: p.id,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: Some("old".into()),
        preview_only: None,
        preview_checked: String::new(),
    })
    .unwrap();
    let mut other = (**s.catalog.photo(p.id).unwrap()).clone();
    other.id = PhotoId(2);
    other.file_size = p.file_size + 7;
    s.catalog.apply(Op::AddPhoto { photo: Box::new(other) }).unwrap();
    assert!(s.media.content_keys(s.catalog.photo(PhotoId(2)).unwrap()).contains(&"old".to_string()));
    s.commit(
        "Reload",
        Op::SetContent {
            id: p.id,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: Some("new".into()),
            preview_only: None,
            preview_checked: String::new(),
        },
    )
    .unwrap();
    assert!(!s.media.content_keys(s.catalog.photo(PhotoId(2)).unwrap()).contains(&"old".to_string()));
}

#[test]
fn a_damaged_smart_preview_gives_way_to_the_next_candidate() {
    let dir = crate::tests_xmp::temp_dir("smart-candidates");
    std::fs::create_dir_all(&dir).unwrap();
    let good = crate::smart::encode(&Rgb32f::new(8, 8), None).unwrap();
    let body = good.windows(2).position(|w| w == [0xFF, 0xD8]).unwrap();
    let mut damaged = good[..body].to_vec();
    damaged.extend([0xFF, 0xD8, 0, 0, 0, 0, 0xFF, 0xD9]);
    let (first, second) = (dir.join("first.lcsp"), dir.join("second.lcsp"));
    std::fs::write(&first, &damaged).unwrap();
    std::fs::write(&second, &good).unwrap();
    assert!(crate::smart::is_valid(&first) && crate::smart::load(&first).is_err());
    let paths = vec![first, second];
    assert!(SourceRef::Smart { paths: paths.clone() }.load_source().is_ok());
    let offline = SourceRef::File {
        path: dir.join("offline.jpg").to_string_lossy().to_string(),
        max_edge: 64,
        loader: Some(Arc::new(|_, _, _| Err("offline".into()))),
        fallback: Some(paths),
        denoise: None,
        ticket: None,
        camera_profile: None,
        outliers: false,
        reduced: None,
    };
    assert!(offline.load_source().is_ok());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_offline_raw_keeps_its_camera_thumbnail_after_learning_its_hash() {
    let mut s = crate::Session::new();
    s.media.preview_loader = Some(Arc::new(|_: &str, edge: usize| Some(Rgba8::new(edge.min(64), 2))));
    let mut raw = Photo::new(PhotoId(1), Source::File { path: "offline.cr3".into() }, "offline.cr3", "CR3", 6000, 4000, "2026-01-01");
    raw.kind = MediaKind::Raw;
    raw.develop = Arc::new(raw.camera_defaults());
    s.catalog.apply(Op::AddPhoto { photo: Box::new(raw) }).unwrap();
    let job = s.thumb_job(PhotoId(1), 256).unwrap();
    assert_eq!(s.quick_thumb_job(&job).unwrap().run().quick, Some(QuickSource::Embedded));
    let p = s.catalog.photo(PhotoId(1)).unwrap().clone();
    s.apply_system(Op::SetContent {
        id: p.id,
        width: p.width,
        height: p.height,
        file_size: p.file_size,
        content_hash: Some("learned".into()),
        preview_only: None,
        preview_checked: String::new(),
    })
    .unwrap();
    s.media.preview_loader = Some(Arc::new(|_: &str, _: usize| None::<Rgba8>));
    s.media.file_loader = Some(Arc::new(|_, _, _| Err("offline".into())));
    let job = s.thumb_job(PhotoId(1), 256).unwrap();
    let r = s.quick_thumb_job(&job).unwrap().run();
    assert!(r.rendered.is_ok(), "{:?}", r.rendered.err());
    assert_eq!(r.quick, Some(QuickSource::Embedded));
}
