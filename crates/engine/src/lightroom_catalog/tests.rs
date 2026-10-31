use super::*;
use lightcraft_geom::CropGeometry;
fn row(v: Value) -> Row {
    serde_json::from_value(v).unwrap()
}
fn sample() -> CatalogImport {
    let mut photo = CatalogPhoto {
        source_id: 1,
        uuid: "test-master".into(),
        path: "/missing/test.ARW".into(),
        image: row(json!({"id_local":1,"rating":5,"pick":1,"fileFormat":"ARW","fileWidth":20,"fileHeight":10})),
        settings: "s = { Exposure2012 = 1.25, Contrast2012 = 17, CameraProfile = \"Example\" }".into(),
        xmp: String::new(),
        keywords: vec!["Travel|Goa".into()],
        history: vec![row(json!({"text":"s = { Exposure2012 = 0.5 }"}))],
        snapshots: Vec::new(),
        original: Original::Local,
    };
    let mut copy = photo.clone();
    copy.source_id = 2;
    copy.uuid = "test-copy".into();
    copy.image = row(json!({"id_local":2,"masterImage":1,"copyName":"Mono","rating":2,"pick":-1}));
    copy.settings = "s = { ConvertToGrayscale = true, Exposure2012 = -0.5 }".into();
    photo.image.insert("captureTime".into(), json!("2024-01-16T12:34:56"));
    CatalogImport {
        source: "test.lrcat".into(),
        photos: vec![copy, photo],
        collections: vec![
            row(json!({"id_local":9,"name":"Travel","creationId":"collection.group"})),
            row(json!({"id_local":10,"parent":9,"name":"Keepers","creationId":"collection"})),
        ],
        members: vec![
            row(json!({"collection":10,"image":2,"positionInCollection":"a"})),
            row(json!({"collection":10,"image":1,"positionInCollection":"b"})),
        ],
        collection_content: Vec::new(),
        warnings: Vec::new(),
        skipped: Vec::new(),
    }
}
/// A crafted `.lrcat` that names a program as a photo's original gets no record for it, so
/// Photo ▸ Edit In ▸ Open Original has nothing to hand the system opener (#2135). Its virtual
/// copies go with it; the catalog's real photos still import.
#[test]
fn a_catalog_original_that_is_not_a_photo_file_is_not_imported() {
    let mut data = sample();
    let mut exe = data.photos[1].clone();
    exe.source_id = 3;
    exe.uuid = "payload".into();
    exe.path = "/Users/sky/Downloads/payload.exe".into();
    exe.image = row(json!({"id_local":3,"fileFormat":"ARW","fileWidth":20,"fileHeight":10}));
    let mut exe_copy = exe.clone();
    exe_copy.source_id = 4;
    exe_copy.uuid = "payload-copy".into();
    exe_copy.path = "/Users/sky/Downloads/payload.exe".into();
    exe_copy.image = row(json!({"id_local":4,"masterImage":3,"copyName":"Mono"}));
    // the copy comes before its master, as a catalog's table order may have it
    data.photos.insert(0, exe_copy);
    data.photos.push(exe);
    for bad in [r"C:\Users\sky\start.lnk", "/Applications/Evil.app", "/Users/sky/go.command", "/Users/sky/run.bat"] {
        let mut one = data.clone();
        one.photos.retain(|p| p.source_id < 3);
        let mut planted = one.photos[1].clone();
        planted.source_id = 5;
        planted.uuid = "planted".into();
        planted.path = bad.into();
        planted.image = row(json!({"id_local":5,"fileFormat":"ARW"}));
        one.photos.push(planted);
        let mut s = crate::Session::new();
        let r = apply(&mut s, one, false).unwrap();
        assert_eq!(r["photos"], 2, "{bad} is not catalogued: {r}");
        assert!(s.catalog.photos().all(|p| !matches!(&p.source, Source::File { path } if path == bad)), "{bad} became a library record");
    }
    let mut s = crate::Session::new();
    let r = apply(&mut s, data, false).unwrap();
    assert_eq!(r["photos"], 2, "only the real master and its copy: {r}");
    assert!(s.catalog.photos().all(|p| !matches!(&p.source, Source::File { path } if path.ends_with(".exe"))), "the program was catalogued");
    let warnings = r["warnings"].to_string();
    assert!(warnings.contains("payload.exe") && warnings.contains("file types LightCraft reads"), "{warnings}");
    assert!(warnings.contains("image 4: not imported"), "the virtual copy of the refused original goes with it: {warnings}");
}

/// A refused original with id 0 (a malformed catalog) is left out alone: `masterImage` is 0
/// for every master, so the catalog's other photos still import.
#[test]
fn a_refused_original_with_id_zero_leaves_the_other_masters_alone() {
    let mut data = sample();
    let mut refused = data.photos[1].clone();
    refused.source_id = 0;
    refused.uuid = "refused".into();
    refused.path = "/missing/refused.ARW".into();
    refused.image = row(json!({"id_local":0,"fileFormat":"ARW","fileWidth":20,"fileHeight":10}));
    refused.original = Original::Refused("it is on another computer".into());
    data.photos.insert(0, refused);
    let mut s = crate::Session::new();
    let r = apply(&mut s, data, false).unwrap();
    assert_eq!(r["photos"], 2, "the master and its copy: {r}");
    assert!(s.catalog.photos().all(|p| !matches!(&p.source, Source::File { path } if path.ends_with("refused.ARW"))));
}

#[test]
fn offline_local_master_and_copy_join_library_with_one_undo_step() {
    let mut s = crate::Session::new();
    let data = sample();
    let master_id = s.catalog.alloc_photo_id();
    let mut master = Photo::new(master_id, Source::File { path: "/missing/test.ARW".into() }, "test.ARW", "ARW", 20, 10, "before");
    master.local = true;
    s.commit("Browse folder", Op::AddPhoto { photo: Box::new(master) }).unwrap();
    s.undo.clear();
    let before = s.catalog.photo(master_id).unwrap().clone();
    let mut index = ImportIndex::default();
    let missing_sources = [key("/missing/test.ARW")].into_iter().collect();
    let mut import = |s: &mut crate::Session| {
        let mut steps = ImportSteps::default();
        let mut apply = LightroomApply::new(s, data.source.clone(), false, index.clone(), Vec::new(), "now".into());
        let photos = data
            .photos
            .iter()
            .cloned()
            .map(|src| {
                let shape = Shape::of(&catalog_photo(&src, PhotoId(0), ""));
                MappedPhoto::new(src, shape)
            })
            .collect();
        steps.run(s, |s| apply.batch(s, photos, Default::default(), &missing_sources)).unwrap();
        let report = steps.run(s, |s| apply.finish(s, &data.collections, &data.members, true, None)).unwrap();
        index = apply.index.clone();
        report
    };
    let first = import(&mut s);
    assert_eq!(first["mapping"]["1"], master_id.0);
    assert_eq!(first["imported"], 2, "the promoted master and new copy both join All Photos");
    assert_eq!(first["missing"], json!(["/missing/test.ARW"]));
    assert_eq!(s.catalog.len(), 2);
    assert!(s.catalog.photos().all(|photo| !photo.local));
    assert_eq!(s.undo.len(), 1);
    let second = import(&mut s);
    assert_eq!(second["imported"], 0);
    assert_eq!(second["mapping"], first["mapping"]);
    s.undo_step().unwrap();
    s.undo_step().unwrap();
    assert_eq!(s.catalog.len(), 1);
    assert_eq!(s.catalog.photo(master_id), Some(&before));
    assert!(s.catalog.albums().next().is_none());
    let third = import(&mut s);
    assert_eq!(third["imported"], 2);
    assert!(s.catalog.photos().all(|photo| !photo.local));
}

#[test]
fn imports_copies_collections_ratings_and_mapped_edits_in_one_undo_step() {
    let mut s = crate::Session::new();
    let data = sample();
    let r = apply(&mut s, data.clone(), false).unwrap();
    assert_eq!(r["photos"], 2);
    assert_eq!(r["imported"], 2, "the offline master and virtual copy both count as added");
    assert_eq!(r["missing"].as_array().unwrap().len(), 1);
    let master = s.catalog.photo(PhotoId(r["mapping"]["1"].as_u64().unwrap())).unwrap();
    assert_eq!(master.rating, 5);
    assert_eq!(master.flag, Flag::Pick);
    assert_eq!(master.meta.keywords, vec!["Travel|Goa"]);
    assert_eq!(master.develop.light.exposure, 1.25);
    let copy = s.catalog.photos().find(|p| p.copy_of.is_some()).unwrap();
    assert_eq!(copy.copy_of, Some(master.id));
    assert_eq!(copy.rating, 2);
    assert_eq!(copy.flag, Flag::Reject);
    assert_eq!(copy.develop.light.exposure, -0.5);
    let album = s.catalog.albums().find(|a| a.name == "Keepers").unwrap();
    assert_eq!(album.photos, vec![copy.id, master.id]);
    assert!(album.parent.is_some());
    assert!(r["unmapped"]["1"].as_array().unwrap().iter().any(|v| v.as_str() == Some("CameraProfile")));
    assert_eq!(s.undo.len(), 1);
    s.undo_step().unwrap();
    assert!(s.catalog.is_empty());
    assert!(s.catalog.albums().next().is_none());
    assert!(data.photos[1].settings.contains("CameraProfile")); // unmapped source stays intact
}
/// A library photo at `path`, already committed.
/// `data`'s photos added the way the import adds a batch, whose file import reported `files`, then
/// its collections; `index` is the import index (a library on disk keeps it between imports).
fn apply_batch(s: &mut crate::Session, mut data: CatalogImport, files: crate::import::ImportReport, index: &mut ImportIndex) -> Value {
    refuse_non_media(&mut data);
    validate(&data).unwrap();
    let now = (s.clock)();
    let mut apply = LightroomApply::new(s, data.source.clone(), false, std::mem::take(index), data.warnings.clone(), now.clone());
    apply.skipped = data.skipped.clone();
    let mut probe = Photo::new(PhotoId(0), Source::File { path: String::new() }, "", "", 0, 0, &now);
    let photos = data
        .photos
        .into_iter()
        .map(|src| {
            probe.format = text(&src.image, "fileFormat").to_string();
            MappedPhoto::new(src, Shape::of(&probe))
        })
        .collect();
    apply.batch(s, photos, files, &HashSet::new()).unwrap();
    let report = apply.finish(s, &data.collections, &data.members, true, None).unwrap();
    *index = std::mem::take(&mut apply.index);
    report
}

fn library_photo(s: &mut crate::Session, path: &str, edit: impl FnOnce(&mut Photo)) -> PhotoId {
    let id = s.catalog.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: path.into() }, "a.ARW", "ARW", 20, 10, "before");
    p.content_hash = Some("same-bytes".into());
    edit(&mut p);
    s.commit("Add", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}
/// A catalog photo whose file has the same bytes as a library photo in Recently Deleted (or a
/// virtual copy) is a new record of its own: it was counted as imported but landed in Recently
/// Deleted (or became a "copy" of nothing it was copied from), so a large import into a library
/// with a trash went missing photo by photo (#2589).
#[test]
fn a_content_duplicate_of_a_trashed_photo_or_virtual_copy_is_a_new_live_photo() {
    let trashed: fn(&mut Photo) = |p| p.deleted = true;
    let virtual_copy: fn(&mut Photo) = |p| {
        p.copy_of = Some(PhotoId(999));
        p.copy_name = Some("Copy 1".into());
    };
    for (case, edit) in [("trashed", trashed), ("virtual copy", virtual_copy)] {
        let mut s = crate::Session::new();
        let other = library_photo(&mut s, "/elsewhere/a.ARW", |p| {
            edit(p);
            // the other photo's own state, which the new record must not inherit
            let mut d = (*p.develop).clone();
            d.light.shadows = 77.0;
            p.develop = std::sync::Arc::new(d);
            p.edited = Some("earlier".into());
            p.history.push(lightcraft_catalog::HistoryStep { label: "Other photo's edit".into(), settings: p.develop.clone() });
            p.versions.push(lightcraft_catalog::Version {
                name: "Other photo's version".into(),
                created: "before".into(),
                settings: p.develop.clone(),
                auto: false,
            });
            p.meta.title = "Other photo's title".into();
            p.meta.keywords = vec!["other-photo".into()];
            p.local_baseline = Some(7);
            // what the bytes determine is shared
            p.meta.camera = "Same Camera".into();
            p.file_size = 4096;
        });
        let mut data = sample();
        data.photos.retain(|p| number(&p.image, "masterImage") == 0);
        let files = crate::import::ImportReport {
            duplicates: vec![crate::import::Duplicate {
                path: "/missing/test.ARW".into(),
                existing: Some(other.0),
                reason: "content",
                existing_deleted: case == "trashed",
                // what the file itself carries
                file: Some(Box::new(crate::import::FileFacts {
                    meta: lightcraft_catalog::Meta { camera: "Same Camera".into(), ..Default::default() },
                    captured: None,
                })),
            }],
            ..Default::default()
        };
        let r = apply_batch(&mut s, data, files, &mut ImportIndex::default());
        assert_eq!(r["imported"], 1, "{case}: {r}");
        assert_eq!(r["inRecentlyDeleted"], 0, "{case}: {r}");
        let new = s.catalog.photo(PhotoId(r["mapping"]["1"].as_u64().unwrap())).unwrap();
        assert_ne!(new.id, other, "{case}");
        assert!(!new.deleted, "{case}: the new record must not be in Recently Deleted");
        assert_eq!((new.copy_of, new.copy_name.as_deref()), (None, None), "{case}: the new record is no virtual copy");
        assert_ne!(new.imported, "before", "{case}: imported now, not when the other photo was");
        assert_eq!(new.rating, 5, "{case}: the catalog's own metadata applies");
        assert_ne!(new.develop.light.shadows, 77.0, "{case}: no edits from the other photo");
        assert!(new.history.iter().all(|h| h.label != "Other photo's edit"), "{case}: no history from the other photo");
        assert!(new.versions.is_empty(), "{case}: no versions from the other photo");
        assert_ne!(new.meta.title, "Other photo's title", "{case}");
        assert!(!new.meta.keywords.iter().any(|k| k == "other-photo"), "{case}");
        assert_eq!(new.local_baseline, None, "{case}");
        assert_eq!((new.meta.camera.as_str(), new.file_size), ("Same Camera", 4096), "{case}: what the bytes determine is kept");
        assert_eq!(new.content_hash.as_deref(), Some("same-bytes"), "{case}");
        assert_eq!(s.catalog.photo(other).unwrap().deleted, case == "trashed", "{case}: the other photo is left as it was");
    }
}
/// A content duplicate starts from the settings a file import gives a raw (its as-shot white
/// balance and embedded lens corrections), as its file would on its own, before the catalog's
/// settings go on top; not from the generic defaults (#2589).
#[test]
fn a_content_duplicate_raw_starts_from_its_camera_defaults() {
    let mut s = crate::Session::new();
    let other = library_photo(&mut s, "/elsewhere/a.DNG", |p| {
        p.deleted = true;
        p.format = "DNG".into();
        p.kind = lightcraft_catalog::MediaKind::Raw;
        p.as_shot_wb = Some((5200.0, 4.0));
        p.embedded_lens = Some(lightcraft_develop::EmbeddedLens::default());
    });
    let mut data = sample();
    data.photos.retain(|p| number(&p.image, "masterImage") == 0);
    // a catalog row with no develop settings of its own
    data.photos[0].settings.clear();
    let files = crate::import::ImportReport {
        duplicates: vec![crate::import::Duplicate {
            path: "/missing/test.ARW".into(),
            existing: Some(other.0),
            reason: "content",
            existing_deleted: true,
            file: None,
        }],
        ..Default::default()
    };
    let r = apply_batch(&mut s, data, files, &mut ImportIndex::default());
    let new = s.catalog.photo(PhotoId(r["mapping"]["1"].as_u64().unwrap())).unwrap();
    assert_eq!(*new.develop, new.camera_defaults(), "{r}");
    assert_eq!((new.develop.wb.temp, new.develop.wb.tint), (5200.0, 4.0), "the file's as-shot white balance");
    assert!(new.develop.optics.lens_profile, "the file's embedded lens corrections are on");
}
/// A content duplicate keeps the metadata its file carries (copyright, rights, creator) and the
/// capture time its file gives it, as a file import of it would, when the catalog doesn't restate
/// them; it reads the file's, not the other photo's, which a person may have edited (#2589).
#[test]
fn a_content_duplicate_keeps_the_rights_metadata_and_capture_time_in_its_file() {
    let dir = std::env::temp_dir().join(format!("lightcraft-lrcat-duplicate-rights-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Photos")).unwrap();
    let source = dir.join("source.lrcat");
    std::fs::write(dir.join("Photos/test.ARW"), b"same bytes").unwrap();
    std::fs::write(&source, native_catalog(&photo_tables((dir.to_str().unwrap(), "")))).unwrap();
    let mut s = crate::Session::new();
    let other = library_photo(&mut s, "/elsewhere/a.ARW", |p| {
        p.deleted = true;
        // edited on the other record after its import
        p.meta.copyright = "Other photo's notice".into();
        p.meta.creator = "Other photo's creator".into();
        p.meta.gps = Some((1.0, 2.0));
        p.captured = Some("1999-12-31T23:00:00".into());
    });
    let file_meta = lightcraft_catalog::Meta {
        camera: "Same Camera".into(),
        copyright: "© Jo Example".into(),
        copyright_status: lightcraft_catalog::CopyrightStatus::Copyrighted,
        usage_terms: "No reuse".into(),
        creator: "Jo Example".into(),
        ..Default::default()
    };
    let probed = file_meta.clone();
    s.media.file_probe = Some(Arc::new(move |_| {
        Ok(crate::media::ProbeInfo {
            format: "ARW".into(),
            width: 20,
            height: 10,
            content_hash: Some("same-bytes".into()),
            meta: probed.clone(),
            captured: Some("2024-05-06T07:08:09".into()),
            ..Default::default()
        })
    }));
    let r = run_job(&mut s, &source).report;
    let new = s.catalog.photos().find(|p| p.id != other).unwrap_or_else(|| panic!("no new record: {r}"));
    assert_eq!(new.content_hash.as_deref(), Some("same-bytes"), "{r}");
    assert_eq!(new.rating, 5, "the catalog's own metadata applies: {r}");
    let m = &new.meta;
    assert_eq!(
        (m.copyright.as_str(), m.copyright_status, m.usage_terms.as_str(), m.creator.as_str(), m.camera.as_str()),
        ("© Jo Example", file_meta.copyright_status, "No reuse", "Jo Example", "Same Camera"),
        "the file's own metadata: {r}"
    );
    assert_eq!(m.gps, None, "nothing from the other photo's record");
    // the catalog row has no capture time: the file's, not the other photo's edited one
    assert_eq!(new.captured.as_deref(), Some("2024-05-06T07:08:09"), "{r}");
    std::fs::remove_dir_all(dir).unwrap();
}
/// A catalog photo matched (by path) to a library photo in Recently Deleted leaves it there, as
/// the import was set up to do (a Lightroom import doesn't restore, see `OnDeleted`), and the report
/// counts it so the photos don't go missing unexplained (#2589). A new virtual copy of it is
/// imported, not trashed with its master.
#[test]
fn a_catalog_photo_matching_a_trashed_record_stays_there_and_is_counted() {
    let mut s = crate::Session::new();
    let trashed = library_photo(&mut s, "/missing/test.ARW", |p| p.deleted = true);
    let r = apply(&mut s, sample(), false).unwrap();
    assert_eq!(r["mapping"]["1"], trashed.0, "{r}");
    assert!(s.catalog.photo(trashed).unwrap().deleted, "the photo stays in Recently Deleted");
    assert_eq!(r["inRecentlyDeleted"], 1, "{r}");
    let copy = s.catalog.photos().find(|p| p.copy_of == Some(trashed)).unwrap();
    assert!(!copy.deleted, "the new virtual copy is imported, not trashed");
    assert_eq!(r["imported"], 1, "only the copy was added: {r}");
}
/// Importing the catalog again after its master and virtual copy were both moved to Recently
/// Deleted matches each through the import index: each is counted once, and neither is restored.
#[test]
fn a_reimport_counts_each_trashed_match_once() {
    let mut s = crate::Session::new();
    // one import index across both imports, as a library on disk keeps in Interop/
    let mut index = ImportIndex::default();
    let mut import = |s: &mut crate::Session| apply_batch(s, sample(), Default::default(), &mut index);
    let first = import(&mut s);
    let ids: Vec<PhotoId> = ["1", "2"].iter().map(|k| PhotoId(first["mapping"][k].as_u64().unwrap())).collect();
    for id in &ids {
        s.commit("Delete", Op::SetDeleted { id: *id, deleted: true }).unwrap();
    }
    let r = import(&mut s);
    assert_eq!(r["mapping"], first["mapping"], "{r}");
    assert_eq!(r["inRecentlyDeleted"], 2, "{r}");
    assert_eq!(r["imported"], 0, "{r}");
    assert!(ids.iter().all(|id| s.catalog.photo(*id).unwrap().deleted));
}
#[test]
fn preserves_existing_edits_and_rejects_invalid_structure_before_mutation() {
    let mut s = crate::Session::new();
    let data = sample();
    apply(&mut s, data.clone(), false).unwrap();
    let id = s.catalog.photos().find(|p| p.copy_of.is_none()).unwrap().id;
    s.set_develop(id, lightcraft_develop::DevelopSettings::default(), "Personal edit").unwrap();
    let r = apply(&mut s, data.clone(), false).unwrap();
    assert!(r["preservedExistingEdits"].as_u64().unwrap() > 0);
    assert_eq!(s.catalog.photo(id).unwrap().develop.light.exposure, 0.0);
    let before = s.catalog.to_snapshot();
    let mut cyclic = data;
    cyclic.collections[0].insert("parent".into(), json!(10));
    assert!(apply(&mut s, cyclic, false).is_err());
    assert_eq!(s.catalog.to_snapshot(), before);
    assert!(
        mapped_settings(
            &format!("{}0{}", "{".repeat(1000), "}".repeat(1000)),
            crate::crs::Target::RawAbsolute,
            1.0,
            Orientation::Normal,
            1.5,
            Orientation::Normal
        )
        .is_err()
    );
}
#[test]
fn compressed_xmp_is_bounded_and_settings_are_data_only() {
    let packet = b"<x:xmpmeta>example</x:xmpmeta>";
    let mut encoded = (packet.len() as u32).to_be_bytes().to_vec();
    encoded.extend(miniz_oxide::deflate::compress_to_vec_zlib(packet, 6));
    assert_eq!(xmp(Some(&json!(encoded))).unwrap(), String::from_utf8_lossy(packet));
    encoded[0] = 0xff;
    assert!(xmp(Some(&json!(encoded))).is_err());
    let (partial, _) = mapped_settings(
        "s = { Exposure2012 = 0.75, ToneCurvePV2012 = {0,0,128,150,255,255} }",
        crate::crs::Target::RawAbsolute,
        1.5,
        Orientation::Normal,
        1.5,
        Orientation::Normal,
    )
    .unwrap();
    assert_eq!(partial["light"]["exposure"], 0.75);
}

#[test]
fn custom_white_balance_on_relative_wb_raws_is_not_read_as_kelvin() {
    // Issue #510: a catalog keeps no as-shot white for Custom photos, so on a raw developed
    // relative to its as-shot look the edit stays As Shot and is reported; a DNG keeps the Kelvin
    let settings =
        "s = { WhiteBalance = \"Custom\", Temperature = 3578, Tint = -5, CustomTemperature = 3578.95, CustomTint = -5, Exposure2012 = 0.5 }";
    let (partial, unknown) = mapped_settings(settings, crate::crs::Target::RawRelative, 1.5, Orientation::Normal, 1.5, Orientation::Normal).unwrap();
    assert_eq!(partial["wb"], json!({"mode": "asShot"}), "{partial}");
    assert_eq!(partial["light"]["exposure"], 0.5);
    assert!(unknown.iter().any(|k| k.starts_with("Temperature, Tint")), "{unknown:?}");
    let (partial, unknown) = mapped_settings(settings, crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, 1.5, Orientation::Normal).unwrap();
    assert_eq!(
        (partial["wb"]["mode"].as_str(), partial["wb"]["temp"].as_f64(), partial["wb"]["tint"].as_f64()),
        (Some("custom"), Some(3578.0), Some(-5.0))
    );
    assert!(!unknown.iter().any(|k| k.starts_with("Temperature")), "{unknown:?}");
}

#[test]
fn catalog_crops_without_has_crop_are_imported() {
    // Lightroom Classic catalogs store crop edges and angle but never `HasCrop`
    let (partial, unknown) = mapped_settings(
        "s = { CropConstrainAspectRatio = true, CropLeft = 0.042702, CropRight = 0.957298, CropTop = 0.1 }",
        crate::crs::Target::RawAbsolute,
        1.5,
        Orientation::Normal,
        1.5,
        Orientation::Normal,
    )
    .unwrap();
    let crop = &partial["crop"]["geometry"];
    assert_eq!(crop["rect"]["x0"], 0.042702);
    assert_eq!(crop["rect"]["y0"], 0.1);
    assert_eq!(crop["rect"]["x1"], 0.957298);
    assert_eq!(crop["rect"]["y1"], 1.0);
    assert_eq!(crop["angle"], 0.0);
    assert!(!unknown.iter().any(|k| k.starts_with("CropLeft")), "{unknown:?}");
    // an angle alone still means a crop (Lightroom's angle turns the other way to LightCraft's)
    let (partial, unknown) =
        mapped_settings("s = { CropAngle = -1.5 }", crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, 1.5, Orientation::Normal).unwrap();
    assert_eq!(partial["crop"]["geometry"]["angle"], 1.5);
    assert!(!unknown.iter().any(|k| k.starts_with("CropAngle")), "{unknown:?}");
    // no crop fields: the crop stays untouched
    let (partial, _) =
        mapped_settings("s = { Exposure2012 = 0.5 }", crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, 1.5, Orientation::Normal).unwrap();
    assert!(partial.get("crop").is_none());
}

#[test]
fn lightroom_orientation_codes() {
    // the three seen in a real catalog, checked against each file's EXIF orientation (1, 6, 8)
    assert_eq!(lightroom_orientation("AB"), Some(Orientation::Normal));
    assert_eq!(lightroom_orientation("BC"), Some(Orientation::Rotate90));
    assert_eq!(lightroom_orientation("DA"), Some(Orientation::Rotate270));
    assert_eq!(lightroom_orientation("CD"), Some(Orientation::Rotate180));
    assert_eq!(lightroom_orientation("BA"), Some(Orientation::FlipH));
    assert_eq!(lightroom_orientation("DC"), Some(Orientation::FlipV));
    assert_eq!(lightroom_orientation("AD"), Some(Orientation::Transpose));
    assert_eq!(lightroom_orientation("CB"), Some(Orientation::Transverse));
    for code in ["", "A", "ABC", "AC", "ab", "XY"] {
        assert_eq!(lightroom_orientation(code), None, "{code:?}");
    }
}

#[test]
fn catalog_crops_follow_the_photo_orientation() {
    let crop = |o: Orientation| {
        let settings = "s = { CropLeft = 0.1, CropRight = 0.6, CropTop = 0.2, CropBottom = 0.9 }";
        let (partial, _) = mapped_settings(settings, crate::crs::Target::RawAbsolute, 1.5, o, 1.5, Orientation::Normal).unwrap();
        let g = &partial["crop"]["geometry"];
        let r = &g["rect"];
        [r["x0"].as_f64(), r["y0"].as_f64(), r["x1"].as_f64(), r["y1"].as_f64(), g["angle"].as_f64()].map(|v| (v.unwrap() * 1e9).round() / 1e9)
    };
    // (x, y) of the stored image → (1 − y, x) a quarter turn clockwise, (y, 1 − x) counter-clockwise
    assert_eq!(crop(Orientation::Normal), [0.1, 0.2, 0.6, 0.9, 0.0]);
    assert_eq!(crop(Orientation::Rotate90), [0.1, 0.1, 0.8, 0.6, 0.0]);
    assert_eq!(crop(Orientation::Rotate270), [0.2, 0.4, 0.9, 0.9, 0.0]);
    assert_eq!(crop(Orientation::Rotate180), [0.4, 0.1, 0.9, 0.8, 0.0]);
    assert_eq!(crop(Orientation::FlipH), [0.4, 0.2, 0.9, 0.9, 0.0]);
    // a mirror turns the straighten the other way
    let angle = |o: Orientation| {
        let (partial, _) = mapped_settings("s = { CropAngle = 2 }", crate::crs::Target::RawAbsolute, 1.5, o, 1.5, Orientation::Normal).unwrap();
        partial["crop"]["geometry"]["angle"].as_f64().unwrap()
    };
    assert_eq!(angle(Orientation::Rotate90), -2.0);
    assert_eq!(angle(Orientation::FlipH), 2.0);
    // no crop: nothing to orient
    let (partial, _) =
        mapped_settings("s = { Exposure2012 = 0.5 }", crate::crs::Target::RawAbsolute, 1.5, Orientation::Rotate90, 1.5, Orientation::Normal).unwrap();
    assert!(partial.get("crop").is_none());
}

#[test]
fn catalog_masks_follow_the_photo_orientation() {
    // #1806, EMY_9996 (EXIF 8, shown a quarter turn counter-clockwise): a horizon gradient along
    // the stored image's x axis runs along the shown photo's y axis
    let s = r#"s = { MaskGroupBasedCorrections = { { What = "Correction", CorrectionAmount = 1, LocalExposure2012 = -0.1, CorrectionMasks = {
        { What = "Mask/Gradient", MaskValue = 1, ZeroX = 0.362544, ZeroY = 0.481093, FullX = 0.451639, FullY = 0.480645 } } } } }"#;
    let (partial, _) = mapped_settings(s, crate::crs::Target::RawAbsolute, 1.5, Orientation::Rotate270, 1.5, Orientation::Normal).unwrap();
    let g = &partial["masks"][0]["components"][0]["shape"];
    let close = |v: &Value, want: f64| (v.as_f64().unwrap() - want).abs() < 1e-9;
    assert!(close(&g["start"]["x"], 0.480645) && close(&g["start"]["y"], 1.0 - 0.451639), "{g}");
    assert!(close(&g["end"]["x"], 0.481093) && close(&g["end"]["y"], 1.0 - 0.362544), "{g}");

    let masks = |o: Orientation| {
        let s = r#"s = {
            MaskGroupBasedCorrections = { { What = "Correction", CorrectionAmount = 1, LocalExposure2012 = 0.1, CorrectionMasks = {
                { What = "Mask/Gradient", FullX = 0.1, FullY = 0.2, ZeroX = 0.3, ZeroY = 0.6 },
                { What = "Mask/CircularGradient", Left = 0, Right = 0.2, Top = 0.1, Bottom = 0.3, Angle = 10, Feather = 50, Version = 2, Flipped = true },
                { What = "Mask/Paint", Radius = 0.05, MaskValue = 1, Dabs = { "d 0.1 0.2", "d 0.3 0.6" } } } } } }"#;
        let (partial, _) = mapped_settings(s, crate::crs::Target::RawAbsolute, 1.5, o, 1.5, Orientation::Normal).unwrap();
        let c = &partial["masks"][0]["components"];
        let (linear, radial, brush) = (&c[0]["shape"], &c[1]["shape"], &c[2]["shape"]["strokes"][0]["points"]);
        [
            &linear["start"]["x"],
            &linear["start"]["y"],
            &linear["end"]["x"],
            &linear["end"]["y"],
            &radial["center"]["x"],
            &radial["center"]["y"],
            &radial["rx"],
            &radial["ry"],
            &radial["angle"],
            &brush[0]["x"],
            &brush[0]["y"],
            &brush[1]["x"],
            &brush[1]["y"],
        ]
        .map(|v| (v.as_f64().unwrap() * 1e9).round() / 1e9)
    };
    // (x, y) of the stored image → (1 − y, x) a quarter turn clockwise, (y, 1 − x) counter-clockwise;
    // a radial's radii (in long-edge units) swap with the axes, and a mirror turns its angle the
    // other way
    let r = (0.1, (0.1_f64 / 1.5 * 1e9).round() / 1e9);
    assert_eq!(masks(Orientation::Normal), [0.1, 0.2, 0.3, 0.6, 0.1, 0.2, r.0, r.1, 10.0, 0.1, 0.2, 0.3, 0.6]);
    assert_eq!(masks(Orientation::Rotate90), [0.8, 0.1, 0.4, 0.3, 0.8, 0.1, r.1, r.0, 10.0, 0.8, 0.1, 0.4, 0.3]);
    assert_eq!(masks(Orientation::Rotate270), [0.2, 0.9, 0.6, 0.7, 0.2, 0.9, r.1, r.0, 10.0, 0.2, 0.9, 0.6, 0.7]);
    assert_eq!(masks(Orientation::Rotate180), [0.9, 0.8, 0.7, 0.4, 0.9, 0.8, r.0, r.1, 10.0, 0.9, 0.8, 0.7, 0.4]);
    assert_eq!(masks(Orientation::FlipH), [0.9, 0.2, 0.7, 0.6, 0.9, 0.2, r.0, r.1, -10.0, 0.9, 0.2, 0.7, 0.6]);
    assert_eq!(masks(Orientation::Transpose), [0.2, 0.1, 0.6, 0.3, 0.2, 0.1, r.1, r.0, -10.0, 0.2, 0.1, 0.6, 0.3]);
    // a photo already in the library has its shown size (portrait here), but Lightroom's edges are
    // fractions of the stored image: a circle on a 3:2 stored image stays a circle
    let s = r#"s = { MaskGroupBasedCorrections = { { What = "Correction", CorrectionAmount = 1, LocalExposure2012 = 0.1, CorrectionMasks = {
        { What = "Mask/CircularGradient", Left = 0.3, Right = 0.5, Top = 0.3, Bottom = 0.6, Version = 2, Flipped = true } } } } }"#;
    let (partial, _) = mapped_settings(s, crate::crs::Target::RawAbsolute, 1.0 / 1.5, Orientation::Rotate90, 1.5, Orientation::Normal).unwrap();
    let radial = &partial["masks"][0]["components"][0]["shape"];
    assert!(close(&radial["rx"], 0.1) && close(&radial["ry"], 0.1), "{radial}");
}

/// The `crs` corners and angle Lightroom would store for LightCraft crop `g` on a stored image
/// of aspect `w` (height 1) shown with orientation `o`.
fn lightroom_crop(g: CropGeometry, o: Orientation, w: f64) -> String {
    let (ow, oh) = if o.swaps_axes() { (1.0, w) } else { (w, 1.0) };
    let to_source = Affine::rotate_about(-g.angle.to_radians(), Point::new(ow / 2.0, oh / 2.0));
    let stored = |x: f64, y: f64| {
        let p = to_source.apply(Point::new(x * ow, y * oh));
        o.inverse().map(p.x, p.y, ow, oh)
    };
    // the rectangle's corners that land top-left and bottom-right on the stored image
    let corners = [stored(g.rect.x0, g.rect.y0), stored(g.rect.x1, g.rect.y0), stored(g.rect.x1, g.rect.y1), stored(g.rect.x0, g.rect.y1)];
    let (tl, br) = (0..4).map(|i| (corners[i], corners[(i + 2) % 4])).find(|(a, b)| a.0 < b.0 && a.1 < b.1).unwrap();
    let angle = if o.to_parts().0 { g.angle } else { -g.angle };
    format!("s = {{ CropLeft = {}, CropTop = {}, CropRight = {}, CropBottom = {}, CropAngle = {angle} }}", tl.0 / w, tl.1, br.0 / w, br.1)
}

#[test]
fn straightened_catalog_crops_round_trip() {
    let g = CropGeometry { rect: Rect::new(0.2, 0.15, 0.7, 0.8), angle: 6.5 };
    for o in [Orientation::Normal, Orientation::Rotate90, Orientation::Rotate270, Orientation::Rotate180, Orientation::FlipH] {
        for angle in [6.5, -6.5] {
            let g = CropGeometry { angle, ..g };
            let (partial, _) =
                mapped_settings(&lightroom_crop(g, o, 1.5), crate::crs::Target::RawAbsolute, 1.5, o, 1.5, Orientation::Normal).unwrap();
            let got: CropGeometry = serde_json::from_value(partial["crop"]["geometry"].clone()).unwrap();
            let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
            assert!(
                close(got.angle, g.angle)
                    && [(got.rect.x0, g.rect.x0), (got.rect.y0, g.rect.y0), (got.rect.x1, g.rect.x1), (got.rect.y1, g.rect.y1)]
                        .iter()
                        .all(|&(a, b)| close(a, b)),
                "{o:?} {angle}: {got:?}"
            );
        }
    }
}

#[test]
fn straightened_catalog_crops_match_lightroom_exports() {
    // Settings and pixel sizes from a real Lightroom Classic catalog (9504 × 6336 stored) and the
    // JPEGs Lightroom exported from it at full size.
    for (settings, o, export) in [
        (
            "s = { CropAngle = -9.34444, CropBottom = 0.802034, CropLeft = 0.05499, CropRight = 0.94501, CropTop = 0.197966 }",
            Orientation::Normal,
            (7725.0, 5150.0),
        ),
        (
            "s = { CropAngle = -1.48964, CropBottom = 0.962457, CropLeft = 0.010429, CropRight = 0.989571, CropTop = 0.037543 }",
            Orientation::Rotate90,
            (6100.0, 9150.0),
        ),
    ] {
        let (partial, _) = mapped_settings(settings, crate::crs::Target::RawAbsolute, 1.5, o, 9504.0 / 6336.0, Orientation::Normal).unwrap();
        let r: Rect = serde_json::from_value(partial["crop"]["geometry"]["rect"].clone()).unwrap();
        let (ow, oh) = if o.swaps_axes() { (6336.0, 9504.0) } else { (9504.0, 6336.0) };
        let (w, h) = (r.width() * ow, r.height() * oh);
        assert!((w - export.0).abs() <= 3.0 && (h - export.1).abs() <= 3.0, "{o:?}: {w:.1} × {h:.1}, Lightroom exported {export:?}");
    }
}

#[test]
fn imported_crops_use_the_catalog_orientation() {
    let mut data = sample();
    data.photos.truncate(1);
    data.photos[0].image.remove("masterImage");
    data.photos[0].image.insert("orientation".into(), json!("BC"));
    data.photos[0].settings = "s = { CropLeft = 0.1, CropRight = 0.6, CropTop = 0.2, CropBottom = 0.9 }".into();
    data.photos[0].xmp = String::new();
    let mut s = crate::Session::new();
    apply(&mut s, data, false).unwrap();
    let g = s.catalog.photos().next().unwrap().develop.crop.geometry;
    let r = [g.rect.x0, g.rect.y0, g.rect.x1, g.rect.y1].map(|v| (v * 1e9).round() / 1e9);
    assert_eq!(r, [0.1, 0.1, 0.8, 0.6]);
}

/// Masks only the catalog's XMP has (the develop row has none to replace them) are on the stored
/// image too: on a vertical shot (`DA`) they turn like the row's, and a radial keeps its shape on
/// the 2:1 stored image, also for a photo already in the library at its shown 10 × 20.
#[test]
fn catalog_xmp_masks_follow_the_photo_orientation() {
    let mut data = sample();
    data.photos.truncate(1);
    data.photos[0].image.remove("masterImage");
    data.photos[0].image.insert("orientation".into(), json!("DA"));
    data.photos[0].image.insert("fileWidth".into(), json!(20));
    data.photos[0].image.insert("fileHeight".into(), json!(10));
    data.photos[0].settings = "s = { Exposure2012 = 0.5 }".into();
    data.photos[0].xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/">
 <crs:MaskGroupBasedCorrections><rdf:Seq>
  <rdf:li><rdf:Description crs:What="Correction" crs:CorrectionAmount="1" crs:LocalExposure2012="-0.25">
   <crs:CorrectionMasks><rdf:Seq>
    <rdf:li crs:What="Mask/Gradient" crs:FullX="0.1" crs:FullY="0.2" crs:ZeroX="0.3" crs:ZeroY="0.6"/>
    <rdf:li crs:What="Mask/CircularGradient" crs:Left="0.3" crs:Right="0.5" crs:Top="0.3" crs:Bottom="0.6" crs:Flipped="true" crs:Version="2"/>
   </rdf:Seq></crs:CorrectionMasks>
  </rdf:Description></rdf:li>
 </rdf:Seq></crs:MaskGroupBasedCorrections>
</rdf:Description></rdf:RDF></x:xmpmeta>"#
        .into();
    let mut s = crate::Session::new();
    let id = s.catalog.alloc_photo_id();
    let shown = Photo::new(id, Source::File { path: "/missing/test.ARW".into() }, "test.ARW", "ARW", 10, 20, "before");
    s.commit("Browse folder", Op::AddPhoto { photo: Box::new(shown) }).unwrap();
    apply(&mut s, data, true).unwrap();
    assert_eq!(s.catalog.len(), 1, "the catalog's photo is the one already in the library");
    let p = s.catalog.photo(id).unwrap();
    assert_eq!(p.develop.light.exposure, 0.5, "the develop row is applied too");
    let c = &serde_json::to_value(&p.develop.masks).unwrap()[0]["components"];
    let (linear, radial) = (&c[0]["shape"], &c[1]["shape"]);
    let r = [&linear["start"]["x"], &linear["start"]["y"], &linear["end"]["x"], &linear["end"]["y"], &radial["center"]["x"], &radial["center"]["y"]]
        .map(|v| (v.as_f64().unwrap() * 1e9).round() / 1e9);
    // (x, y) → (y, 1 − x), a quarter turn counter-clockwise
    assert_eq!(r, [0.2, 0.9, 0.6, 0.7, 0.45, 0.6], "{c}");
    // 4 × 3 px of a 20 × 10 stored image, in long-edge units, with the axes swapped
    let radii = [&radial["rx"], &radial["ry"]].map(|v| (v.as_f64().unwrap() * 1e9).round() / 1e9);
    assert_eq!(radii, [0.075, 0.1], "{radial}");
}

/// A photo already in the library keeps its Rotate Left / Right on update, so the catalog's masks
/// go through it too: an upright (`AB`) photo turned right in LightCraft gets its gradient turned.
#[test]
fn updated_masks_follow_the_rotation_kept_from_the_library() {
    let mut data = sample();
    data.photos.truncate(1);
    data.photos[0].image.remove("masterImage");
    data.photos[0].image.insert("orientation".into(), json!("AB"));
    data.photos[0].settings =
        r#"s = { MaskGroupBasedCorrections = { { What = "Correction", CorrectionAmount = 1, LocalExposure2012 = 0.1, CorrectionMasks = {
        { What = "Mask/Gradient", FullX = 0.1, FullY = 0.2, ZeroX = 0.3, ZeroY = 0.6 } } } } }"#
            .into();
    data.photos[0].xmp = String::new();
    let mut s = crate::Session::new();
    apply(&mut s, data.clone(), false).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    let turned = lightcraft_develop::DevelopSettings { orientation: Orientation::Rotate90, ..Default::default() };
    s.set_develop(id, turned, "Rotate Right").unwrap();
    apply(&mut s, data, true).unwrap();
    let p = s.catalog.photo(id).unwrap();
    assert_eq!(p.develop.orientation, Orientation::Rotate90, "the update keeps the photo's rotation");
    let g = &serde_json::to_value(&p.develop.masks).unwrap()[0]["components"][0]["shape"];
    let r = [&g["start"]["x"], &g["start"]["y"], &g["end"]["x"], &g["end"]["y"]].map(|v| (v.as_f64().unwrap() * 1e9).round() / 1e9);
    // (x, y) → (1 − y, x), a quarter turn clockwise
    assert_eq!(r, [0.8, 0.1, 0.4, 0.3], "{g}");
    // the catalog's orientation first, then the photo's own, for every pair
    for a in 1..=8 {
        for b in 1..=8 {
            let (a, b) = (Orientation::from_exif(a), Orientation::from_exif(b));
            for (x, y) in [(0.1, 0.2), (0.7, 0.4)] {
                let (u, v) = a.map(x, y, 1.0, 1.0);
                let ((p, q), (want_p, want_q)) = (in_photo_frame(a, b).map(x, y, 1.0, 1.0), b.map(u, v, 1.0, 1.0));
                assert!((p - want_p).abs() < 1e-9 && (q - want_q).abs() < 1e-9, "{a:?} then {b:?}: ({p}, {q}) for ({want_p}, {want_q})");
            }
        }
    }
}

#[test]
fn deferred_auto_tone_sentinels_never_become_real_slider_values() {
    let mut data = sample();
    data.photos.truncate(1);
    data.photos[0].image.remove("masterImage");
    data.photos[0].settings = "s = { AutoTone = true, Exposure2012 = -999999, Contrast2012 = -999999, Shadows2012 = 65, Temperature = 6150 }".into();
    data.photos[0].xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="-999999" crs:Contrast2012="-999999" crs:Blacks2012="-22"/></rdf:RDF></x:xmpmeta>"#.into();
    let mut s = crate::Session::new();
    let report = apply(&mut s, data, false).unwrap();
    let p = s.catalog.photos().next().unwrap();
    assert_eq!(p.develop.light.exposure, 0.0);
    assert_eq!(p.develop.light.contrast, 0.0);
    assert_eq!(p.develop.light.shadows, 65.0);
    assert_eq!(p.develop.light.blacks, -22.0);
    assert!(report["unmapped"]["2"].as_array().unwrap().iter().any(|v| v.as_str().is_some_and(|v| v.contains("Deferred"))));
    let (partial, _) = mapped_settings(
        "s = { Exposure2012 = -5, Contrast2012 = -100 }",
        crate::crs::Target::RawAbsolute,
        1.0,
        Orientation::Normal,
        1.5,
        Orientation::Normal,
    )
    .unwrap();
    assert_eq!(partial["light"]["exposure"], -5.0);
    assert_eq!(partial["light"]["contrast"], -100.0);
}

#[test]
fn wal_checksums_cover_commits_without_materializing_historical_databases() {
    let mut main = vec![0; 512];
    main[16..18].copy_from_slice(&512u16.to_be_bytes());
    assert_eq!(wal_checksum(&[1, 0, 0, 0, 2, 0, 0, 0], false, (0, 0)), (1, 3));
    for big in [false, true] {
        let mut wal = vec![0; 32];
        wal[0..4].copy_from_slice(&(if big { 0x377f0683u32 } else { 0x377f0682u32 }).to_be_bytes());
        wal[4..8].copy_from_slice(&3_007_000u32.to_be_bytes());
        wal[8..12].copy_from_slice(&512u32.to_be_bytes());
        wal[16..20].copy_from_slice(&123u32.to_be_bytes());
        let mut sum = wal_checksum(&wal[..24], big, (0, 0));
        wal[24..28].copy_from_slice(&sum.0.to_be_bytes());
        wal[28..32].copy_from_slice(&sum.1.to_be_bytes());
        for _ in 0..128 {
            let mut frame = vec![0; 536];
            frame[0..4].copy_from_slice(&1u32.to_be_bytes());
            frame[4..8].copy_from_slice(&1u32.to_be_bytes());
            frame[8..12].copy_from_slice(&123u32.to_be_bytes());
            sum = wal_checksum(&frame[..8], big, sum);
            sum = wal_checksum(&frame[24..], big, sum);
            frame[16..20].copy_from_slice(&sum.0.to_be_bytes());
            frame[20..24].copy_from_slice(&sum.1.to_be_bytes());
            wal.extend(frame);
        }
        assert_eq!(committed_wal_len(&main, &wal).unwrap(), wal.len());
        let end = wal.len();
        wal[end - 1] ^= 1;
        assert_eq!(committed_wal_len(&main, &wal).unwrap(), end - 536);
        wal[24] ^= 1;
        assert!(committed_wal_len(&main, &wal).is_err());
        main[17] = 0;
        assert!(committed_wal_len(&main, &wal).is_err());
        main[16..18].copy_from_slice(&512u16.to_be_bytes());
    }
}

#[test]
fn persistent_reimport_keeps_copy_and_collection_ids_and_capture_time() {
    let dir = std::env::temp_dir().join(format!("lightcraft-lrcat-reimport-{}", std::process::id()));
    let mut s = crate::Session::new();
    s.open_library(&dir, false).unwrap();
    let mut data = sample();
    data.collections.reverse(); // Parent must be committed before its child regardless of row order.
    let first = apply(&mut s, data.clone(), false).unwrap();
    s.close_library().unwrap();
    s.open_library(&dir, false).unwrap();
    let id = PhotoId(first["mapping"]["1"].as_u64().unwrap());
    s.commit("Changed time", Op::SetCaptured { id, captured: None }).unwrap();
    let second = apply(&mut s, data, true).unwrap();
    assert_eq!(first["mapping"], second["mapping"]);
    assert_eq!(first["imported"], 2);
    assert_eq!(second["imported"], 0, "reimport updates records instead of adding them");
    assert_eq!(second["missing"].as_array().unwrap().len(), 1, "offline originals are reported on reimport too");
    assert_eq!(second["library"], dir.to_string_lossy().as_ref());
    assert_eq!(s.catalog.len(), 2);
    assert_eq!(s.catalog.albums().count(), 2);
    assert_eq!(s.catalog.photo(id).unwrap().captured.as_deref(), Some("2024-01-16T12:34:56"));
    s.close_library().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

// Construct a small SQLite fixture from public file-format records, never private media.
fn varint(mut n: u64) -> Vec<u8> {
    let mut out = vec![(n & 127) as u8];
    n >>= 7;
    while n > 0 {
        out.push((n & 127) as u8 | 128);
        n >>= 7;
    }
    out.reverse();
    out
}
fn record(values: &[SqlValue]) -> Vec<u8> {
    let mut header = Vec::new();
    let mut body = Vec::new();
    for value in values {
        let serial = match value {
            SqlValue::Null => 0,
            SqlValue::Integer(n) => {
                body.extend(n.to_be_bytes());
                6
            }
            SqlValue::Text(s) => {
                body.extend(s.as_bytes());
                13 + 2 * s.len() as u64
            }
            _ => unreachable!(),
        };
        header.extend(varint(serial));
    }
    let mut out = varint(1 + header.len() as u64);
    out.extend(header);
    out.extend(body);
    out
}
fn leaf(page: &mut [u8], offset: usize, rows: &[Vec<SqlValue>]) {
    page[offset] = 13;
    page[offset + 3..offset + 5].copy_from_slice(&(rows.len() as u16).to_be_bytes());
    let mut end = page.len();
    for (i, values) in rows.iter().enumerate() {
        let payload = record(values);
        let mut cell = varint(payload.len() as u64);
        cell.extend(varint(i as u64 + 1));
        cell.extend(payload);
        end -= cell.len();
        page[end..end + cell.len()].copy_from_slice(&cell);
        page[offset + 8 + i * 2..offset + 10 + i * 2].copy_from_slice(&(end as u16).to_be_bytes());
    }
    page[offset + 5..offset + 7].copy_from_slice(&(end as u16).to_be_bytes());
}
/// A one-row-per-table SQLite file: `(name, columns, row)` each.
fn native_catalog(tables: &[(&str, &str, Vec<SqlValue>)]) -> Vec<u8> {
    native_catalog_with_last_table_page(tables, None)
}
/// [`native_catalog`] whose last table claims `last_page` as its root page.
fn native_catalog_with_last_table_page(tables: &[(&str, &str, Vec<SqlValue>)], last_page: Option<i64>) -> Vec<u8> {
    let tables: Vec<_> = tables.iter().map(|(name, columns, row)| (*name, *columns, vec![row.clone()])).collect();
    native_catalog_rows(&tables, last_page)
}
/// A SQLite file with one leaf page per table: `(name, columns, rows)` each. `last_page` overrides the last
/// table's root page.
fn native_catalog_rows(tables: &[(&str, &str, Vec<Vec<SqlValue>>)], last_page: Option<i64>) -> Vec<u8> {
    use SqlValue::{Integer as I, Text as T};
    let mut bytes = vec![0u8; 5 * 2048];
    bytes[..16].copy_from_slice(b"SQLite format 3\0");
    bytes[16..18].copy_from_slice(&2048u16.to_be_bytes());
    bytes[18] = 1;
    bytes[19] = 1;
    bytes[21] = 64;
    bytes[22] = 32;
    bytes[23] = 32;
    bytes[28..32].copy_from_slice(&5u32.to_be_bytes());
    bytes[44..48].copy_from_slice(&4u32.to_be_bytes());
    bytes[56..60].copy_from_slice(&1u32.to_be_bytes());
    let mut schema = Vec::new();
    for (i, (name, columns, rows)) in tables.iter().enumerate() {
        schema.push(vec![
            T("table".into()),
            T((*name).into()),
            T((*name).into()),
            I(if i + 1 == tables.len() { last_page.unwrap_or(i as i64 + 2) } else { i as i64 + 2 }),
            T(format!("CREATE TABLE {name} ({columns})")),
        ]);
        leaf(&mut bytes[(i + 1) * 2048..(i + 2) * 2048], 0, rows);
    }
    leaf(&mut bytes[..2048], 100, &schema);
    bytes
}

fn photo_tables(root: (&str, &str)) -> Vec<(&'static str, &'static str, Vec<SqlValue>)> {
    use SqlValue::{Integer as I, Null as N, Text as T};
    vec![
        (
            "AgLibraryRootFolder",
            "id_local INTEGER PRIMARY KEY, absolutePath, name, relativePathFromCatalog",
            vec![N, T(root.0.into()), T("Photos".into()), T(root.1.into())],
        ),
        ("AgLibraryFolder", "id_local INTEGER PRIMARY KEY, rootFolder, pathFromRoot", vec![N, I(1), T("Photos/".into())]),
        ("AgLibraryFile", "id_local INTEGER PRIMARY KEY, folder, idx_filename", vec![N, I(1), T("test.ARW".into())]),
        (
            "Adobe_images",
            "id_local INTEGER PRIMARY KEY, id_global, rootFile, fileFormat, fileWidth, fileHeight, rating, pick",
            vec![N, T("fixture-image".into()), I(1), T("ARW".into()), I(20), I(10), I(5), I(1)],
        ),
    ]
}

#[test]
fn native_video_import_is_visible_in_video_filter() {
    let dir = std::env::temp_dir().join(format!("lightcraft-lrcat-video-filter-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("videos.lrcat");
    let mut tables = photo_tables((dir.to_str().unwrap(), ""));
    tables[2].2[2] = SqlValue::Text("clip.MOV".into());
    tables[3].2[3] = SqlValue::Text("VIDEO".into());
    std::fs::write(&path, native_catalog(&tables)).unwrap();
    let mut s = crate::Session::new();
    let imported = s.execute("library.importLightroom", &json!({"path": path})).unwrap();
    let id = PhotoId(imported["mapping"]["1"].as_u64().unwrap());
    assert_eq!(s.catalog.photo(id).unwrap().kind, lightcraft_catalog::MediaKind::Video);
    s.execute("library.filter", &json!({"kind": "video"})).unwrap();
    assert_eq!(s.visible(), &[id]);
    s.execute("library.filter", &json!({"kind": "image"})).unwrap();
    assert!(s.visible().is_empty());
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.catalog.is_empty());
    s.execute("edit.redo", &json!({})).unwrap();
    s.execute("library.filter", &json!({"kind": "video"})).unwrap();
    assert_eq!(s.visible(), &[id]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn native_video_reimport_repairs_kind_without_losing_edits_or_memberships() {
    let dir = std::env::temp_dir().join(format!("lightcraft-lrcat-video-reimport-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("videos.lrcat");
    let mut tables = photo_tables((dir.to_str().unwrap(), ""));
    tables[2].2[2] = SqlValue::Text("clip.MOV".into());
    tables[3].2[3] = SqlValue::Text("VIDEO".into());
    std::fs::write(&path, native_catalog(&tables)).unwrap();
    let mut s = crate::Session::new();
    let id = s.catalog.alloc_photo_id();
    let mut photo =
        Photo::new(id, Source::File { path: dir.join("Photos/clip.MOV").to_string_lossy().into() }, "clip.MOV", "VIDEO", 1920, 1080, "before");
    photo.rating = 5;
    photo.flag = Flag::Pick;
    photo.file_size = 1234;
    photo.content_hash = Some("existing-content".into());
    s.commit("Previous import", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    let mut settings = lightcraft_develop::DevelopSettings::default();
    settings.light.exposure = 0.75;
    s.set_develop(id, settings, "Personal edit").unwrap();
    let peer_id = s.catalog.alloc_photo_id();
    let peer = Photo::new(peer_id, Source::File { path: "/missing/peer.jpg".into() }, "peer.jpg", "JPEG", 20, 10, "before");
    s.commit("Peer", Op::AddPhoto { photo: Box::new(peer) }).unwrap();
    let mut album = Album::new(s.catalog.alloc_album_id(), "Personal album");
    album.photos = vec![id, peer_id];
    s.commit("Album", Op::AddAlbum { album: album.clone() }).unwrap();
    let stack = lightcraft_catalog::Stack { id: s.catalog.alloc_stack_id(), photos: vec![id, peer_id], collapsed: false };
    s.commit("Stack", Op::AddStack { stack: stack.clone() }).unwrap();
    let before = s.catalog.to_snapshot();
    let mut expected = (**s.catalog.photo(id).unwrap()).clone();
    expected.kind = lightcraft_catalog::MediaKind::Video;
    s.execute("library.filter", &json!({"kind": "video"})).unwrap();
    assert!(s.visible().is_empty());
    let result = s.execute("library.importLightroom", &json!({"path": path})).unwrap();
    assert_eq!(result["mapping"]["1"], id.0);
    assert_eq!(result["imported"], 0);
    assert_eq!(result["preservedExistingEdits"], 1);
    assert_eq!(s.catalog.photo(id).unwrap().as_ref(), &expected);
    assert_eq!(s.visible(), &[id]);
    let after = s.catalog.to_snapshot();
    assert_eq!(s.catalog.album(album.id), Some(&album));
    assert_eq!(s.catalog.stack(stack.id), Some(&stack));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.to_snapshot(), before);
    assert!(s.visible().is_empty());
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(s.catalog.to_snapshot(), after);
    assert_eq!(s.visible(), &[id]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn reads_native_sqlite_tables_without_lightroom_or_external_processes() {
    let bytes = native_catalog(&photo_tables(("/catalog/", "")));
    let path = std::env::temp_dir().join(format!("lightcraft-native-catalog-{}.lrcat", std::process::id()));
    std::fs::write(&path, &bytes).unwrap();
    let catalog = read(&path).unwrap();
    assert_eq!(catalog.photos.len(), 1);
    assert_eq!(catalog.photos[0].uuid, "fixture-image");
    assert_eq!(number(&catalog.photos[0].image, "rating"), 5);
    assert!(catalog.photos[0].path.ends_with("test.ARW"));
    // Put the required image table beyond the old 1 GiB cutoff without allocating it.
    // A main-file-sized allocation would defeat the purpose of the page-backed reader.
    // The large fixture relies on Unix sparse-file behavior; small parity runs everywhere.
    #[cfg(unix)]
    {
        let far_offset = 1u64 << 30;
        let bytes = native_catalog_with_last_table_page(&photo_tables(("/catalog/", "")), Some((far_offset / 2048 + 1) as i64));
        use std::io::{Seek, SeekFrom, Write};
        let mut sparse = std::fs::OpenOptions::new().write(true).truncate(true).open(&path).unwrap();
        sparse.write_all(&bytes).unwrap();
        sparse.set_len(far_offset + 2048).unwrap();
        sparse.seek(SeekFrom::Start(far_offset)).unwrap();
        sparse.write_all(&bytes[4 * 2048..5 * 2048]).unwrap();
        drop(sparse);
        let large = read(&path).unwrap();
        assert_eq!(serde_json::to_value(&large.photos).unwrap(), serde_json::to_value(&catalog.photos).unwrap());
        let mut session = crate::Session::new();
        session.execute("library.importLightroom", &json!({"path": path.to_string_lossy()})).unwrap();
        assert_eq!(session.catalog.len(), 1);
        assert_eq!(session.catalog.photos().next().unwrap().rating, 5);
        session.execute("edit.undo", &json!({})).unwrap();
        assert!(session.catalog.is_empty());
    }
    std::fs::write(&path, b"bad catalog").unwrap();
    assert!(read(&path).is_err());
    std::fs::remove_file(path).unwrap();
}

/// A one-photo catalog (its original, `/catalog/Photos/test.ARW`, is missing).
fn fixture_catalog() -> Vec<u8> {
    native_catalog(&photo_tables(("/catalog/", "")))
}

/// The synchronous import (CLI, MCP): read, batches, collections, one undo step, the index.
#[test]
fn import_reads_a_catalog_end_to_end() {
    let dir = std::env::temp_dir().join(format!("lightcraft-lr-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = crate::Session::new().with_fs();
    s.open_library(dir.join("Library"), false).unwrap();
    let catalog = dir.join("fixture.lrcat");
    std::fs::write(&catalog, fixture_catalog()).unwrap();
    let undo0 = s.undo.len();
    let report = import(&mut s, &catalog, false).unwrap();
    assert_eq!(report["photos"], 1, "{report}");
    assert_eq!(report["missing"].as_array().map(Vec::len), Some(1), "the original is missing: kept for relinking");
    assert!(report.get("cancelled").is_none());
    let photo = s.catalog.photos().next().unwrap();
    assert_eq!((photo.rating, photo.flag), (5, Flag::Pick));
    assert_eq!(s.undo.len(), undo0 + 1, "one undo step");
    let index = load_index(Some(&dir.join("Library/Interop/lightroom-index.json"))).unwrap();
    assert_eq!(index.photos.get("fixture-image"), Some(&photo.id.0));
    // importing again maps the same photo
    let again = import(&mut s, &catalog, false).unwrap();
    assert_eq!(again["mapping"], report["mapping"]);
    assert_eq!(s.catalog.len(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

/// The import as a host runs it: the catalog read, every batch added, then the collections.
fn run_job(s: &mut crate::Session, source: &Path) -> crate::lightroom_job::LightroomCompletion {
    let cancel = AtomicBool::new(false);
    let mut job = crate::lightroom_job::LightroomJob::new(s, source.to_path_buf(), false).unwrap();
    let start = job.start(&cancel).unwrap();
    let mut import = crate::lightroom_job::LightroomImport::begin(s, start).unwrap();
    while let Some(batch) = job.next_batch(&cancel) {
        import.commit(s, batch.unwrap()).unwrap();
    }
    import.finish(s, true).unwrap()
}

/// Reads `tables` as a catalog file and returns what the reader made of it.
fn read_tables(tables: &[(&str, &str, Vec<Vec<SqlValue>>)], name: &str) -> CatalogImport {
    let path = std::env::temp_dir().join(format!("lightcraft-{name}-{}.lrcat", std::process::id()));
    std::fs::write(&path, native_catalog_rows(tables, None)).unwrap();
    let catalog = read(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    catalog
}

/// `tables` with each row as a one-row table, for [`native_catalog_rows`].
fn one_row(tables: Vec<(&'static str, &'static str, Vec<SqlValue>)>) -> Vec<(&'static str, &'static str, Vec<Vec<SqlValue>>)> {
    tables.into_iter().map(|(name, columns, row)| (name, columns, vec![row])).collect()
}

#[test]
fn images_with_incomplete_records_are_skipped_not_fatal() {
    use SqlValue::{Integer as I, Text as T};
    // (table, column, value): the record that breaks, and the reason the import gives
    let cases = [
        (3, 2, I(2), SkipReason::MissingFile),                // Adobe_images.rootFile → no such file
        (2, 1, I(2), SkipReason::MissingFolder),              // AgLibraryFile.folder → no such folder
        (1, 1, I(2), SkipReason::MissingRoot),                // AgLibraryFolder.rootFolder → no such root
        (2, 2, T(String::new()), SkipReason::IncompletePath), // AgLibraryFile.idx_filename empty
    ];
    for (table, column, value, reason) in cases {
        let mut tables = one_row(photo_tables(("/catalog/", "")));
        tables[table].2[0][column] = value;
        let catalog = read_tables(&tables, &format!("skipped-{reason:?}"));
        assert!(catalog.photos.is_empty(), "{reason:?}");
        assert_eq!(catalog.skipped, [SkippedImage { image: 1, reason }]);
        assert!(catalog.warnings.is_empty(), "{reason:?}: {:?}", catalog.warnings);
    }
}

#[test]
fn a_virtual_copy_of_a_skipped_master_is_skipped_too() {
    use SqlValue::{Integer as I, Null as N, Text as T};
    let mut tables = one_row(photo_tables(("/catalog/", "")));
    tables[3] = (
        "Adobe_images",
        "id_local INTEGER PRIMARY KEY, id_global, rootFile, masterImage, fileFormat, fileWidth, fileHeight, rating, pick",
        vec![
            // the master's file record is gone, so it is skipped …
            vec![N, T("master".into()), I(2), N, T("ARW".into()), I(20), I(10), I(5), I(1)],
            // … and its virtual copy goes with it, although its own file record is there
            vec![N, T("copy".into()), I(1), I(1), T("ARW".into()), I(20), I(10), I(3), I(0)],
        ],
    );
    let catalog = read_tables(&tables, "skipped-copy");
    assert!(catalog.photos.is_empty());
    assert_eq!(
        catalog.skipped,
        [SkippedImage { image: 1, reason: SkipReason::MissingFile }, SkippedImage { image: 2, reason: SkipReason::MasterSkipped }]
    );
    validate(&catalog).unwrap();
}

/// The batched import (CLI, MCP and the desktop app) reports the reader's skipped images too.
#[test]
fn skipped_images_reach_the_batched_import_report() {
    use SqlValue::Integer as I;
    let mut tables = one_row(photo_tables(("/catalog/", "")));
    tables[3].2[0][2] = I(2); // Adobe_images.rootFile → no such file
    let path = std::env::temp_dir().join(format!("lightcraft-skipped-batched-{}.lrcat", std::process::id()));
    std::fs::write(&path, native_catalog_rows(&tables, None)).unwrap();
    let mut s = crate::Session::new();
    let report = import(&mut s, &path, false).unwrap();
    std::fs::remove_file(path).unwrap();
    assert_eq!(report["skipped"], json!([{"image": 1, "reason": "missingFile"}]), "{report}");
    assert!(s.catalog.is_empty());
}

#[test]
fn skipped_images_are_in_the_import_report() {
    let mut data = sample();
    data.photos.retain(|p| p.source_id != 1);
    drop_copies_of(&mut data, &HashSet::from([1]));
    data.skipped.insert(0, SkippedImage { image: 1, reason: SkipReason::MissingFile });
    let mut s = crate::Session::new();
    let report = apply(&mut s, data, false).unwrap();
    assert_eq!(report["photos"], 0);
    assert_eq!(report["skipped"], json!([{"image": 1, "reason": "missingFile"}, {"image": 2, "reason": "masterSkipped"}]));
    assert_eq!(report["warnings"], json!([]));
}

#[test]
fn preparation_skips_missing_original_probes_but_keeps_catalog_records() {
    let dir = std::env::temp_dir().join(format!("lightcraft-lrcat-offline-probes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source.lrcat");
    let root = dir.join("offline");
    let tables = photo_tables((root.to_str().unwrap(), ""));
    std::fs::write(&source, native_catalog(&tables)).unwrap();
    let mut s = crate::Session::new();
    let probes = Arc::new(AtomicUsize::new(0));
    let calls = probes.clone();
    s.media.file_probe = Some(Arc::new(move |_| {
        calls.fetch_add(1, Ordering::Relaxed);
        Err("a known-missing original must not be probed".into())
    }));
    let result = run_job(&mut s, &source);
    assert_eq!(probes.load(Ordering::Relaxed), 0);
    assert_eq!(result.report["imported"], 1);
    assert_eq!(result.report["failed"], json!([]));
    assert_eq!(result.report["missing"].as_array().unwrap().len(), 1);
    let photo = s.catalog.photos().next().unwrap();
    assert_eq!(photo.rating, 5);
    assert_eq!((photo.width, photo.height), (20, 10));
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.catalog.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn probed_original_is_counted_once_and_reimport_adds_nothing() {
    let dir = std::env::temp_dir().join(format!("lightcraft-lrcat-present-probes-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("Photos")).unwrap();
    let source = dir.join("source.lrcat");
    std::fs::write(dir.join("Photos/test.ARW"), b"original").unwrap();
    std::fs::write(&source, native_catalog(&photo_tables((dir.to_str().unwrap(), "")))).unwrap();
    let mut s = crate::Session::new();
    let probes = Arc::new(AtomicUsize::new(0));
    let calls = probes.clone();
    s.media.file_probe = Some(Arc::new(move |_| {
        calls.fetch_add(1, Ordering::Relaxed);
        Ok(crate::media::ProbeInfo { format: "ARW".into(), width: 20, height: 10, ..Default::default() })
    }));
    for expected in [1, 0] {
        let completion = run_job(&mut s, &source);
        assert_eq!(completion.report["imported"], expected);
        assert_eq!(completion.report["missing"], json!([]));
        assert_eq!(completion.report["failed"], json!([]));
        assert_eq!(s.catalog.len(), 1);
    }
    assert_eq!(probes.load(Ordering::Relaxed), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_root_missing_here_is_found_relative_to_the_catalog() {
    let dir = std::env::temp_dir().join(format!("lightcraft-relocated-catalog-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Lightroom")).unwrap();
    std::fs::create_dir_all(dir.join("RAW")).unwrap();
    let path = dir.join("Lightroom").join("Lightroom.lrcat");
    std::fs::write(&path, native_catalog(&photo_tables(("C:/Users/me/Project/RAW/", "../RAW/")))).unwrap();
    let catalog = read(&path).unwrap();
    assert_eq!(Path::new(&catalog.photos[0].path), dir.join("RAW").join("Photos").join("test.ARW"));
    assert!(catalog.warnings.iter().any(|w| w.contains("relative to the catalog")), "{:?}", catalog.warnings);
    // a relative path that leads nowhere keeps the recorded one
    std::fs::write(&path, native_catalog(&photo_tables(("C:/Users/me/Project/RAW/", "../Elsewhere/")))).unwrap();
    let catalog = read(&path).unwrap();
    assert!(Path::new(&catalog.photos[0].path).starts_with("C:/Users/me/Project/RAW/"), "{}", catalog.photos[0].path);
    assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn originals_on_other_computers_are_never_looked_at_or_added() {
    let dir = std::env::temp_dir().join(format!("lightcraft-lrcat-remote-roots-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Photos")).unwrap();
    std::fs::write(dir.join("Photos/test.ARW"), b"original").unwrap();
    let source = dir.join("source.lrcat");
    let mut roots = vec![r"\\192.0.2.1\share\".to_string(), "file://192.0.2.1/s/".to_string()];
    if cfg!(unix) {
        // `//<local folder>/` is a local folder to Linux, but a network path to the resolver.
        roots.push(format!("/{}/", dir.display()));
    }
    for root in &roots {
        std::fs::write(&source, native_catalog(&photo_tables((root, "")))).unwrap();
        let catalog = read(&source).unwrap();
        assert!(matches!(catalog.photos[0].original, Original::Refused(_)), "{root}: {:?}", catalog.photos[0]);
        assert!(catalog.warnings.iter().any(|w| w.contains("not opened")), "{root}: {:?}", catalog.warnings);
        let mut s = crate::Session::new();
        let probes = Arc::new(AtomicUsize::new(0));
        let calls = probes.clone();
        s.media.file_probe = Some(Arc::new(move |_| {
            calls.fetch_add(1, Ordering::Relaxed);
            Err("a refused original must not be probed".into())
        }));
        let report = import(&mut s, &source, false).unwrap();
        assert_eq!(probes.load(Ordering::Relaxed), 0, "{root}");
        assert_eq!(report["missing"].as_array().map(Vec::len), Some(1), "{root}: {report}");
        assert!(s.catalog.is_empty(), "{root}");
    }
    // An absolute folder path would replace the root.
    let mut tables = photo_tables((dir.to_str().unwrap(), ""));
    tables[1].2[2] = SqlValue::Text(format!("{}/Photos/", dir.display()));
    std::fs::write(&source, native_catalog(&tables)).unwrap();
    assert!(matches!(read(&source).unwrap().photos[0].original, Original::Refused(_)));
    // A root beside the catalog is followed.
    std::fs::write(&source, native_catalog(&photo_tables((dir.to_str().unwrap(), "")))).unwrap();
    let catalog = read(&source).unwrap();
    assert_eq!(catalog.photos[0].original, Original::Local);
    assert!(catalog.photos[0].original_is_file());
    // A root that moved is found through its path relative to the catalog.
    std::fs::write(&source, native_catalog(&photo_tables((r"\\192.0.2.1\share\", "./")))).unwrap();
    let catalog = read(&source).unwrap();
    assert!(catalog.photos[0].original_is_file(), "{:?}", catalog.photos[0]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn oversized_optional_table_warns_and_is_skipped() {
    let mut main = vec![0u8; 2 * 512];
    main[..16].copy_from_slice(b"SQLite format 3\0");
    main[16..18].copy_from_slice(&512u16.to_be_bytes());
    main[18] = 1;
    main[19] = 1;
    main[21] = 64;
    main[22] = 32;
    main[23] = 32;
    main[28..32].copy_from_slice(&2u32.to_be_bytes());
    main[44..48].copy_from_slice(&4u32.to_be_bytes());
    main[56..60].copy_from_slice(&1u32.to_be_bytes());
    main[100] = 0x0d;
    let page = &mut main[512..];
    page[0] = 0x0d;
    page[3..5].copy_from_slice(&1u16.to_be_bytes());
    let mut cell = varint((64 * 1024 * 1024 + 1) as u64);
    cell.extend(varint(1));
    let start = 500 - cell.len();
    page[start..start + cell.len()].copy_from_slice(&cell);
    page[8..10].copy_from_slice(&u16::try_from(start).unwrap_or(0).to_be_bytes());
    page[5..7].copy_from_slice(&u16::try_from(start).unwrap_or(0).to_be_bytes());
    let db = Database::open_with_wal(main, &[]).unwrap();
    let reader = Reader { db, tables: vec![LiveTable { name: "optional_history".into(), rootpage: 2, column_names: Some(vec!["payload".into()]) }] };
    let mut warnings = Vec::new();
    let rows = reader.optional_table("optional_history", &mut warnings).unwrap();
    assert!(rows.is_empty());
    assert!(warnings.iter().any(|warning| warning.contains("optional_history")));
}
