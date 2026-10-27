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
        let undo0 = s.undo.len();
        apply_prepared(
            s,
            data.clone(),
            ApplyPrepared {
                update_existing: false,
                files: Default::default(),
                index: &mut index,
                archive_path: None,
                missing_sources: &missing_sources,
                undo0,
            },
        )
        .unwrap()
        .report
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
        mapped_settings(&format!("{}0{}", "{".repeat(1000), "}".repeat(1000)), crate::crs::Target::RawAbsolute, 1.0, Orientation::Normal, 1.5)
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
    let (partial, unknown) = mapped_settings(settings, crate::crs::Target::RawRelative, 1.5, Orientation::Normal, 1.5).unwrap();
    assert_eq!(partial["wb"], json!({"mode": "asShot"}), "{partial}");
    assert_eq!(partial["light"]["exposure"], 0.5);
    assert!(unknown.iter().any(|k| k.starts_with("Temperature, Tint")), "{unknown:?}");
    let (partial, unknown) = mapped_settings(settings, crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, 1.5).unwrap();
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
    let (partial, unknown) = mapped_settings("s = { CropAngle = -1.5 }", crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, 1.5).unwrap();
    assert_eq!(partial["crop"]["geometry"]["angle"], 1.5);
    assert!(!unknown.iter().any(|k| k.starts_with("CropAngle")), "{unknown:?}");
    // no crop fields: the crop stays untouched
    let (partial, _) = mapped_settings("s = { Exposure2012 = 0.5 }", crate::crs::Target::RawAbsolute, 1.5, Orientation::Normal, 1.5).unwrap();
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
        let (partial, _) = mapped_settings(settings, crate::crs::Target::RawAbsolute, 1.5, o, 1.5).unwrap();
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
        let (partial, _) = mapped_settings("s = { CropAngle = 2 }", crate::crs::Target::RawAbsolute, 1.5, o, 1.5).unwrap();
        partial["crop"]["geometry"]["angle"].as_f64().unwrap()
    };
    assert_eq!(angle(Orientation::Rotate90), -2.0);
    assert_eq!(angle(Orientation::FlipH), 2.0);
    // no crop: nothing to orient
    let (partial, _) = mapped_settings("s = { Exposure2012 = 0.5 }", crate::crs::Target::RawAbsolute, 1.5, Orientation::Rotate90, 1.5).unwrap();
    assert!(partial.get("crop").is_none());
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
            let (partial, _) = mapped_settings(&lightroom_crop(g, o, 1.5), crate::crs::Target::RawAbsolute, 1.5, o, 1.5).unwrap();
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
        let (partial, _) = mapped_settings(settings, crate::crs::Target::RawAbsolute, 1.5, o, 9504.0 / 6336.0).unwrap();
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
    let (partial, _) =
        mapped_settings("s = { Exposure2012 = -5, Contrast2012 = -100 }", crate::crs::Target::RawAbsolute, 1.0, Orientation::Normal, 1.5).unwrap();
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
    let mut job = crate::lightroom_job::LightroomJob::new(&mut s, source, false).unwrap();
    let prepared = job.prepare(&AtomicBool::new(false)).unwrap();
    assert_eq!(probes.load(Ordering::Relaxed), 0);
    let result = crate::lightroom_job::commit_prepared(&mut s, prepared).unwrap();
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
        let mut job = crate::lightroom_job::LightroomJob::new(&mut s, source.clone(), false).unwrap();
        let prepared = job.prepare(&AtomicBool::new(false)).unwrap();
        let completion = crate::lightroom_job::commit_prepared(&mut s, prepared).unwrap();
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
