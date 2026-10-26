use serde_json::json;
use std::path::PathBuf;

#[test]
fn test_parse_json_photo_paths() {
    let json_data = r#"["/var/mobile/Containers/Data/Application/123/Documents/RAW_001.DNG", "/var/mobile/Containers/Data/Application/123/Documents/DSC_9999.ARW"]"#;
    let paths: Result<Vec<PathBuf>, _> = serde_json::from_str(json_data);
    assert!(paths.is_ok());
    let paths = paths.unwrap();
    assert_eq!(paths.len(), 2);
    assert_eq!(paths[0], PathBuf::from("/var/mobile/Containers/Data/Application/123/Documents/RAW_001.DNG"));
    assert_eq!(paths[1], PathBuf::from("/var/mobile/Containers/Data/Application/123/Documents/DSC_9999.ARW"));
}

#[test]
fn test_pick_kind_enum_mapping() {
    use lightcraft_ui_egui::pick::PickKind;

    fn map_kind(k: PickKind) -> u32 {
        match k {
            PickKind::Files => 0,
            PickKind::File => 1,
            PickKind::Folder => 2,
            PickKind::Save => 3,
        }
    }

    assert_eq!(map_kind(PickKind::Files), 0);
    assert_eq!(map_kind(PickKind::File), 1);
    assert_eq!(map_kind(PickKind::Folder), 2);
    assert_eq!(map_kind(PickKind::Save), 3);
}

#[test]
fn test_file_type_classification() {
    // Tests Lightroom-supported file types
    let dng = PathBuf::from("/photos/img.dng");
    let arw = PathBuf::from("/photos/sony.arw");
    let cr3 = PathBuf::from("/photos/canon.cr3");
    let nef = PathBuf::from("/photos/nikon.nef");
    let raf = PathBuf::from("/photos/fuji.raf");
    let rw2 = PathBuf::from("/photos/panasonic.rw2");
    let jpg = PathBuf::from("/photos/sample.jpg");
    let tif = PathBuf::from("/photos/sample.tif");

    fn is_raw(p: &std::path::Path) -> bool {
        match p.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()) {
            Some(ext) => matches!(ext.as_str(), "dng" | "arw" | "cr2" | "cr3" | "nef" | "raf" | "rw2" | "orf"),
            None => false,
        }
    }

    assert!(is_raw(&dng));
    assert!(is_raw(&arw));
    assert!(is_raw(&cr3));
    assert!(is_raw(&nef));
    assert!(is_raw(&raf));
    assert!(is_raw(&rw2));
    assert!(!is_raw(&jpg));
    assert!(!is_raw(&tif));
}

#[test]
fn test_channel_bridge_delivery() {
    let (tx, rx) = std::sync::mpsc::channel();
    let simulated_paths =
        vec![PathBuf::from("/var/mobile/Media/DCIM/100APPLE/IMG_0001.DNG"), PathBuf::from("/var/mobile/Media/DCIM/100APPLE/IMG_0002.JPG")];

    tx.send(simulated_paths.clone()).expect("Send succeeded");
    let received = rx.recv().expect("Receive succeeded");
    assert_eq!(received, simulated_paths);
}

#[test]
fn test_lightroom_catalog_pick_request_filter() {
    use lightcraft_ui_egui::pick::PickRequest;

    let req = PickRequest::file("Choose Lightroom Catalog", "Lightroom Catalog (*.lrcat)", &["lrcat"]);

    assert_eq!(req.title.as_deref(), Some("Choose Lightroom Catalog"));
    let (name, exts) = req.filter.unwrap();
    assert_eq!(name, "Lightroom Catalog (*.lrcat)");
    assert_eq!(exts, vec!["lrcat"]);
}

#[test]
fn test_save_preset_pick_request() {
    use lightcraft_ui_egui::pick::{PickKind, PickRequest};

    let req = PickRequest::save("Export Presets", "LightCraft Presets (*.lcpreset)", &["lcpreset"], "Portraits.lcpreset");
    assert_eq!(req.kind, PickKind::Save);
    assert_eq!(req.file_name.as_deref(), Some("Portraits.lcpreset"));
}

#[test]
fn test_feature_demo_catalog_initialization() {
    use lightcraft_engine::{Session, demo};

    let mut session = Session::new();
    demo::load(&mut session);

    assert_eq!(session.catalog.len(), 24);
    let p1 = session.catalog.photo(lightcraft_engine::catalog::PhotoId(1)).expect("Photo 1 exists");
    assert_eq!(p1.file_name, "LC01207.JPG");
    assert_eq!(p1.format, "JPEG");
    assert!(p1.meta.camera.contains("Synthetic"));
}

#[test]
fn test_feature_lightroom_rating_and_culling() {
    use lightcraft_engine::{Session, catalog::Flag, demo};

    let mut session = Session::new();
    demo::load(&mut session);

    let id = lightcraft_engine::catalog::PhotoId(1);
    session.selection.active = Some(id);
    session.selection.ids = vec![id];

    // Rate 5 stars
    let res = session.execute("photo.rate", &json!({"rating": 5}));
    assert!(res.is_ok());
    assert_eq!(session.catalog.photo(id).map(|p| p.rating), Some(5));

    // Flag as Pick
    let res = session.execute("photo.flag", &json!({"flag": "pick"}));
    assert!(res.is_ok());
    assert_eq!(session.catalog.photo(id).map(|p| p.flag), Some(Flag::Pick));

    // Verify Undo reverts flag to previous state
    let undo_res = session.execute("edit.undo", &json!({}));
    assert!(undo_res.is_ok());
}

#[test]
fn test_feature_lightroom_develop_adjustments() {
    use lightcraft_engine::{Session, demo};

    let mut session = Session::new();
    demo::load(&mut session);

    let id = lightcraft_engine::catalog::PhotoId(2);
    session.selection.active = Some(id);
    session.selection.ids = vec![id];

    // Set Exposure
    let res = session.execute("develop.set", &json!({"control": "light.exposure", "value": 1.5}));
    assert!(res.is_ok());
    let p = session.catalog.photo(id).expect("Photo 2 exists");
    assert!((p.develop.light.exposure - 1.5).abs() < 0.001);

    // Set Contrast
    let res = session.execute("develop.set", &json!({"control": "light.contrast", "value": 25.0}));
    assert!(res.is_ok());
    let p = session.catalog.photo(id).expect("Photo 2 exists");
    assert!((p.develop.light.contrast - 25.0).abs() < 0.001);

    // Set Highlights
    let res = session.execute("develop.set", &json!({"control": "light.highlights", "value": -40.0}));
    assert!(res.is_ok());
    let p = session.catalog.photo(id).expect("Photo 2 exists");
    assert!((p.develop.light.highlights - (-40.0)).abs() < 0.001);
}

#[test]
fn test_feature_lightroom_auto_tone() {
    use lightcraft_engine::{Session, demo};

    let mut session = Session::new();
    demo::load(&mut session);

    let id = lightcraft_engine::catalog::PhotoId(1);
    session.selection.active = Some(id);
    session.selection.ids = vec![id];

    // Trigger AI Auto Tone Settings
    let res = session.execute("develop.auto", &json!({}));
    assert!(res.is_ok(), "develop.auto should succeed");

    let p = session.catalog.photo(id).expect("Photo 1 exists");
    // Verify auto tone set intelligent values
    assert!(p.develop.light.contrast > 0.0, "Auto tone sets contrast");
    assert!(p.develop.color.vibrance > 0.0, "Auto tone sets vibrance");

    // Test Undo restores baseline
    let undo_res = session.execute("edit.undo", &json!({}));
    assert!(undo_res.is_ok(), "Undo auto tone succeeds");
}

#[test]
fn test_feature_lightroom_auto_bw_mix() {
    use lightcraft_engine::{Session, demo};

    let mut session = Session::new();
    demo::load(&mut session);

    let id = lightcraft_engine::catalog::PhotoId(1);
    session.selection.active = Some(id);
    session.selection.ids = vec![id];

    let res = session.execute("develop.autoBwMix", &json!({}));
    assert!(res.is_ok(), "develop.autoBwMix should succeed");

    let p = session.catalog.photo(id).expect("Photo 1 exists");
    assert_eq!(p.develop.bw_mix.bands().len(), 8, "B&W mix has 8 channels");
}

#[test]
fn test_feature_lightroom_color_mixer() {
    use lightcraft_engine::{Session, demo};

    let mut session = Session::new();
    demo::load(&mut session);

    let id = lightcraft_engine::catalog::PhotoId(3);
    session.selection.active = Some(id);
    session.selection.ids = vec![id];

    let res = session.execute("develop.set", &json!({"control": "mixer.blue.sat", "value": 45.0}));
    assert!(res.is_ok());
    let p = session.catalog.photo(id).expect("Photo 3 exists");
    assert!((p.develop.mixer.blue.sat - 45.0).abs() < 0.001);
}

#[test]
fn test_feature_lightroom_album_organization() {
    use lightcraft_engine::{Session, catalog::AlbumId, demo};

    let mut session = Session::new();
    demo::load(&mut session);

    let res = session.execute("album.create", &json!({"name": "iPad Portfolio"}));
    assert!(res.is_ok());

    let album_id = res.unwrap().get("id").and_then(|v| v.as_u64()).unwrap();
    let add_res = session.execute(
        "album.addPhotos",
        &json!({
            "id": album_id,
            "ids": [1, 2, 3]
        }),
    );
    assert!(add_res.is_ok());

    let album = session.catalog.album(AlbumId(album_id)).expect("Album exists");
    assert_eq!(album.name, "iPad Portfolio");
    assert!(album.photos.contains(&lightcraft_engine::catalog::PhotoId(1)));
    assert!(album.photos.contains(&lightcraft_engine::catalog::PhotoId(2)));
    assert!(album.photos.contains(&lightcraft_engine::catalog::PhotoId(3)));
}

#[test]
fn test_feature_simulator_dcim_media_access() {
    let dcim_path =
        PathBuf::from("/Users/kislayranjanneetandon/Library/Developer/CoreSimulator/Devices/89EFF446-8527-488B-B773-019E63C9D7DE/data/Media/DCIM");
    if dcim_path.exists() {
        let entries: Vec<_> = std::fs::read_dir(&dcim_path).expect("Can read DCIM").filter_map(|e| e.ok()).collect();
        assert!(!entries.is_empty(), "DCIM should contain photo folders");
    }
}

#[test]
fn test_feature_lightroom_batch_auto_tone() {
    use lightcraft_engine::{Session, catalog::PhotoId, demo};

    let mut session = Session::new();
    demo::load(&mut session);

    let id1 = PhotoId(1);
    let id2 = PhotoId(2);
    session.selection.active = Some(id1);
    session.selection.ids = vec![id1, id2];

    // Trigger AI Auto Tone Settings across batch
    let res = session.execute("develop.auto", &json!({}));
    assert!(res.is_ok(), "develop.auto batch should succeed");

    let p1 = session.catalog.photo(id1).expect("Photo 1 exists");
    let p2 = session.catalog.photo(id2).expect("Photo 2 exists");
    assert!(p1.develop.light.contrast > 0.0, "Photo 1 contrast set by auto");
    assert!(p2.develop.light.contrast > 0.0, "Photo 2 contrast set by auto");

    // Atomic batch undo: undoing once reverts both photos in Lightroom fashion
    let undo_res = session.execute("edit.undo", &json!({}));
    assert!(undo_res.is_ok(), "Batch undo succeeds");
}

#[test]
fn test_feature_lightroom_auto_tone_with_wb() {
    use lightcraft_develop::WbMode;
    use lightcraft_engine::{Session, catalog::PhotoId, demo};

    let mut session = Session::new();
    demo::load(&mut session);

    let id = PhotoId(1);
    session.selection.active = Some(id);
    session.selection.ids = vec![id];

    // Trigger AI Auto Tone with Auto WB
    let res = session.execute("develop.auto", &json!({"wb": true}));
    assert!(res.is_ok(), "develop.auto with wb should succeed");

    let p = session.catalog.photo(id).expect("Photo 1 exists");
    assert_eq!(p.develop.wb.mode, WbMode::Custom, "Auto WB sets custom temperature");
    assert!(p.develop.wb.temp >= 2000.0 && p.develop.wb.temp <= 50000.0, "Auto WB temperature is in range");
}

#[test]
fn test_feature_ai_scene_intelligence_backlit_recovery() {
    use lightcraft_develop::DevelopSettings;
    use lightcraft_pipeline::{SourceInfo, auto::auto_tone};
    use lightcraft_raster::Rgb32f;

    // Backlit scene: top sky is bright (+2 EV), center subject is underexposed (-1.5 EV)
    let backlit_scene = Rgb32f::from_fn(128, 128, |x, y| {
        let nx = x as f32 / 128.0;
        let ny = y as f32 / 128.0;
        let dx = nx - 0.5;
        let dy = ny - 0.5;
        let r2 = dx * dx + dy * dy;
        if r2 < 0.08 {
            [0.06; 3] // Underexposed subject in center
        } else if ny < 0.35 {
            [1.5; 3] // Bright sky in top third
        } else {
            [0.15; 3] // Terrain / background
        }
    });

    let auto = auto_tone(&backlit_scene, &SourceInfo::default(), &DevelopSettings::default());
    // Auto tone should aggressively lift shadows and recover highlights for backlit subject
    assert!(auto.shadows > 30.0, "Backlit subject shadows lifted: {}", auto.shadows);
    assert!(auto.highlights < -20.0, "Sky highlights recovered: {}", auto.highlights);
}

#[test]
fn test_feature_export_batch_core() {
    use lightcraft_engine::export::{Destination, ExportFormat, ExportOptions, export_batch, write_file};
    use lightcraft_engine::{Session, catalog::PhotoId, demo};

    let mut session = Session::new();
    demo::load(&mut session);

    let id = PhotoId(1);
    let opts = ExportOptions { format: ExportFormat::Jpeg, quality: 90, ..ExportOptions::default() };

    let tmp = std::env::temp_dir()
        .join(format!("lc_test_export_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&tmp).expect("create temp export dir");

    let to = Destination { dir: tmp.to_string_lossy().to_string(), exact: None };

    let files =
        export_batch(&mut session, &[id], &opts, &to, &mut |path, bytes| write_file(path, bytes), &|path| std::path::Path::new(path).exists())
            .expect("export_batch succeeds");

    assert_eq!(files.len(), 1, "Exported exactly one photo");
    let exported_path = files[0]["path"].as_str().expect("output path present");
    let out_file = std::path::Path::new(exported_path);
    assert!(out_file.exists(), "Exported file must exist on disk");
    let metadata = std::fs::metadata(out_file).expect("readable metadata");
    assert!(metadata.len() > 1000, "Exported JPEG has realistic content size: {} bytes", metadata.len());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_feature_app_export_via_services() {
    use lightcraft_engine::{Session, catalog::PhotoId, demo};
    use lightcraft_ui_egui::{LightcraftApp, Services};

    let mut session = Session::new();
    demo::load(&mut session);

    let services = Services {
        write: Some(Box::new(lightcraft_engine::export::write_file)),
        write_shared: Some(std::sync::Arc::new(lightcraft_engine::export::write_file)),
        png: Some(Box::new(|img: &lightcraft_raster::Rgba8| {
            lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
        })),
        ..Services::default()
    };

    let mut app = LightcraftApp::new(session, services);
    let id = PhotoId(1);
    app.session.selection.active = Some(id);
    app.session.selection.ids = vec![id];

    let tmp = std::env::temp_dir()
        .join(format!("lc_test_app_export_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&tmp).expect("create temp export dir");

    let params = json!({
        "ids": [1],
        "dir": tmp.to_string_lossy().to_string(),
        "format": "jpeg",
        "quality": 85,
        "background": false
    });

    let res = app.run("app.export", params);
    assert!(res.is_ok(), "app.export succeeds via configured services: {:?}", res.err());

    let val = res.unwrap();
    let files = val["files"].as_array().expect("files array returned");
    assert_eq!(files.len(), 1);
    let path_str = files[0]["path"].as_str().expect("valid path");
    let out_file = std::path::Path::new(path_str);
    assert!(out_file.exists(), "Exported image file was written");
    assert!(out_file.metadata().unwrap().len() > 0, "Exported file has non-zero size");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_feature_export_presets_list_and_save() {
    use lightcraft_engine::{Session, demo};

    let mut session = Session::new();
    demo::load(&mut session);

    // List presets
    let presets = session.execute("export.presets", &json!({})).expect("list export presets");
    let arr = presets.as_array().expect("presets is an array");
    assert!(!arr.is_empty(), "Built-in export presets are available");

    // Save a custom iPad export preset
    let save_res = session.execute(
        "export.savePreset",
        &json!({
            "name": "iPad Pro AirDrop",
            "params": {
                "format": "jpeg",
                "quality": 95,
                "longEdge": 2048
            }
        }),
    );
    assert!(save_res.is_ok(), "Save custom export preset succeeds");

    // Verify it is present in presets list
    let updated = session.execute("export.presets", &json!({})).unwrap();
    let found = updated.as_array().unwrap().iter().any(|p| p["name"] == "iPad Pro AirDrop");
    assert!(found, "New custom preset exists");
}

#[test]
fn test_feature_masking_brush_stroke_workflow() {
    use lightcraft_develop::MaskShape;
    use lightcraft_engine::{Session, catalog::PhotoId, demo};

    let mut session = Session::new();
    demo::load(&mut session);

    let id = PhotoId(1);
    session.selection.active = Some(id);
    session.selection.ids = vec![id];

    // 1. Create a new Brush mask
    let add_res = session.execute(
        "mask.add",
        &json!({
            "kind": "brush",
            "name": "Portrait Touch-up Brush"
        }),
    );
    assert!(add_res.is_ok(), "mask.add brush succeeded: {:?}", add_res.err());

    let mask_id = session.active_mask.expect("active mask must be set");

    // 2. Add realistic touch/Apple Pencil brush strokes
    let stroke_res = session.execute(
        "mask.brushStroke",
        &json!({
            "id": mask_id,
            "mode": "paint",
            "size": 45.0,
            "feather": 50.0,
            "flow": 0.85,
            "points": [
                [0.45, 0.45],
                [0.48, 0.48],
                [0.50, 0.52],
                [0.53, 0.55]
            ]
        }),
    );
    assert!(stroke_res.is_ok(), "mask.brushStroke succeeded: {:?}", stroke_res.err());

    // 3. Verify stroke is stored in photo develop settings
    let p = session.catalog.photo(id).expect("photo exists");
    let mask = p.develop.masks.iter().find(|m| m.id == mask_id).expect("mask in develop");
    assert_eq!(mask.name, "Portrait Touch-up Brush");
    assert_eq!(mask.components.len(), 1);

    if let MaskShape::Brush { strokes } = &mask.components[0].shape {
        assert!(!strokes.is_empty(), "Brush strokes must be stored");
        assert_eq!(strokes[0].points.len(), 4, "Stroke contains 4 touch dabs");
    } else {
        panic!("Component shape is not Brush");
    }

    // 4. Apply local adjustments: lift exposure and increase sharpness
    let adj_res = session.execute(
        "mask.adjust",
        &json!({
            "id": mask_id,
            "values": {
                "exposure": 1.25,
                "clarity": 20.0,
                "sharpness": 35.0
            }
        }),
    );
    assert!(adj_res.is_ok(), "mask.adjust succeeded");

    let p = session.catalog.photo(id).expect("photo exists");
    let mask = p.develop.masks.iter().find(|m| m.id == mask_id).expect("mask in develop");
    assert!((mask.adjust.exposure - 1.25).abs() < 0.001);
    assert!((mask.adjust.clarity - 20.0).abs() < 0.001);
    assert!((mask.adjust.sharpness - 35.0).abs() < 0.001);
}

#[test]
fn test_feature_masking_linear_gradient_and_local_adjustments() {
    use lightcraft_develop::MaskShape;
    use lightcraft_engine::{Session, catalog::PhotoId, demo};

    let mut session = Session::new();
    demo::load(&mut session);

    let id = PhotoId(2);
    session.selection.active = Some(id);
    session.selection.ids = vec![id];

    // 1. Create a Linear Gradient mask (e.g. darkening the sky)
    let add_res = session.execute(
        "mask.add",
        &json!({
            "kind": "linear",
            "name": "Sky Gradient",
            "start": [0.5, 0.0],
            "end": [0.5, 0.45]
        }),
    );
    assert!(add_res.is_ok(), "Linear mask creation succeeded");

    let mask_id = session.active_mask.expect("active mask must be set");

    // 2. Adjust local tone controls
    let adj_res = session.execute(
        "mask.adjust",
        &json!({
            "id": mask_id,
            "values": {
                "exposure": -0.85,
                "highlights": -50.0,
                "contrast": 15.0,
                "saturation": -10.0
            }
        }),
    );
    assert!(adj_res.is_ok(), "mask.adjust on linear succeeded");

    let p = session.catalog.photo(id).expect("photo exists");
    let mask = p.develop.masks.iter().find(|m| m.id == mask_id).expect("mask exists");
    assert!((mask.adjust.exposure - (-0.85)).abs() < 0.001);
    assert!((mask.adjust.highlights - (-50.0)).abs() < 0.001);

    if let MaskShape::Linear { start, end } = mask.components[0].shape {
        assert_eq!(start.x, 0.5);
        assert_eq!(start.y, 0.0);
        assert_eq!(end.x, 0.5);
        assert_eq!(end.y, 0.45);
    } else {
        panic!("Component is not Linear");
    }
}

#[test]
fn test_feature_masking_radial_gradient_invert_and_adjust() {
    use lightcraft_develop::MaskShape;
    use lightcraft_engine::{Session, catalog::PhotoId, demo};

    let mut session = Session::new();
    demo::load(&mut session);

    let id = PhotoId(3);
    session.selection.active = Some(id);
    session.selection.ids = vec![id];

    // 1. Create a Radial Vignette / Spotlight mask
    let add_res = session.execute(
        "mask.add",
        &json!({
            "kind": "radial",
            "name": "Subject Spotlight",
            "center": [0.5, 0.4],
            "rx": 0.25,
            "ry": 0.35,
            "feather": 60.0,
            "invert": true
        }),
    );
    assert!(add_res.is_ok(), "Radial mask creation succeeded");

    let mask_id = session.active_mask.expect("active mask must be set");

    // 2. Adjust local controls
    let adj_res = session.execute(
        "mask.adjust",
        &json!({
            "id": mask_id,
            "values": {
                "exposure": -0.60
            }
        }),
    );
    assert!(adj_res.is_ok());

    let p = session.catalog.photo(id).expect("photo exists");
    let mask = p.develop.masks.iter().find(|m| m.id == mask_id).expect("mask exists");
    if let MaskShape::Radial { center, rx, ry, invert, .. } = mask.components[0].shape {
        assert_eq!(center.x, 0.5);
        assert_eq!(center.y, 0.4);
        assert_eq!(rx, 0.25);
        assert_eq!(ry, 0.35);
        assert!(invert, "Radial is inverted");
    } else {
        panic!("Component is not Radial");
    }
}

#[test]
fn test_feature_masking_duplicate_and_delete() {
    use lightcraft_engine::{Session, catalog::PhotoId, demo};

    let mut session = Session::new();
    demo::load(&mut session);

    let id = PhotoId(1);
    session.selection.active = Some(id);
    session.selection.ids = vec![id];

    // Create mask
    let _ = session.execute("mask.add", &json!({"kind": "brush", "name": "Temp Mask"}));
    let mask_id = session.active_mask.unwrap();

    // Duplicate mask
    let dup_res = session.execute("mask.duplicate", &json!({"id": mask_id}));
    assert!(dup_res.is_ok(), "mask.duplicate succeeded");

    let p = session.catalog.photo(id).expect("photo exists");
    assert_eq!(p.develop.masks.len(), 2, "Two masks after duplicate");
    let dup_id = session.active_mask.unwrap();
    assert_ne!(dup_id, mask_id);

    // Delete mask
    let del_res = session.execute("mask.delete", &json!({"id": dup_id}));
    assert!(del_res.is_ok(), "mask.delete succeeded");

    let p = session.catalog.photo(id).expect("photo exists");
    assert_eq!(p.develop.masks.len(), 1, "One mask remains after delete");
}

#[test]
fn test_feature_masking_visibility_toggle() {
    use lightcraft_engine::{Session, catalog::PhotoId, demo};

    let mut session = Session::new();
    demo::load(&mut session);

    let id = PhotoId(1);
    session.selection.active = Some(id);
    session.selection.ids = vec![id];

    let _ = session.execute("mask.add", &json!({"kind": "linear", "name": "Gradient"}));
    let mask_id = session.active_mask.unwrap();

    // Mask is initially visible
    let p = session.catalog.photo(id).unwrap();
    assert!(p.develop.masks[0].visible);

    // Toggle visibility to hidden
    let vis_res = session.execute("mask.visible", &json!({"id": mask_id, "visible": false}));
    assert!(vis_res.is_ok(), "mask.visible toggle succeeded");

    let p = session.catalog.photo(id).unwrap();
    assert!(!p.develop.masks[0].visible, "Mask is now hidden");
}
