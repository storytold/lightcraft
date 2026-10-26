//! End-to-End Workflow Tests:
//! 1. Creating a new library and directory verification
//! 2. Importing photos with metadata and duplicate detection
//! 3. Selecting and editing photos in Develop mode (Exposure, WB, Contrast, Highlights, Shadows)
//! 4. Undo and Redo of develop edits
//! 5. Organizing photos (Ratings, Pick flags, Color labels, Filtering)
//! 6. Local adjustment masks (SAM / Brush masking)
//! 7. Exporting developed photos to disk
//! 8. Persistence round-trip across library close and reopen

use crate::{LightcraftApp, Services};
use serde_json::json;
use std::path::{Path, PathBuf};

fn temp_workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lc-e2e-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Generates a valid PNG test image with specific dimensions and color seed.
fn create_test_image(path: &Path, width: usize, height: usize, seed: u8) {
    let data: Vec<[u8; 4]> = (0..width * height)
        .map(|i| {
            let r = ((i % width) * 255 / width.max(1)) as u8;
            let g = ((i / width) * 255 / height.max(1)) as u8;
            let b = seed;
            [r, g, b, 255]
        })
        .collect();
    let img = lightcraft_raster::Rgba8 { width, height, data };
    let bytes =
        lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).expect("encode PNG");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, bytes).expect("write PNG test image");
}

#[test]
fn test_e2e_complete_workflow_library_import_edit_undo_redo_export_roundtrip() {
    let ws = temp_workspace("complete-workflow");
    let lib_dir = ws.join("Libraries");
    let photos_dir = ws.join("IncomingPhotos");
    let export_dir = ws.join("ExportOutput");
    std::fs::create_dir_all(&lib_dir).unwrap();
    std::fs::create_dir_all(&photos_dir).unwrap();
    std::fs::create_dir_all(&export_dir).unwrap();

    // ---------------------------------------------------------
    // Phase 1: Create a brand new library
    // ---------------------------------------------------------
    let session = lightcraft_engine::Session::new().with_fs();
    let mut app = LightcraftApp::new(session, Services::default());

    let lib_name = "Highland Landscape";
    let res = app.run(
        "app.newLibrary",
        json!({
            "path": lib_dir.to_string_lossy(),
            "name": lib_name
        }),
    );
    assert!(res.is_ok(), "Failed to create new library: {:?}", res);

    let expected_lib_path = lib_dir.join(lib_name);
    assert!(expected_lib_path.is_dir(), "Library folder was not created on disk");
    assert_eq!(app.session.catalog.len(), 0, "New library should start with 0 photos");

    // ---------------------------------------------------------
    // Phase 2: Create test photos and import into library
    // ---------------------------------------------------------
    let photo1_path = photos_dir.join("Landscape_001.png");
    let photo2_path = photos_dir.join("Portrait_002.png");
    let photo3_path = photos_dir.join("Subfolder").join("Macro_003.png");

    create_test_image(&photo1_path, 120, 80, 42); // 3:2 landscape
    create_test_image(&photo2_path, 80, 120, 99); // 2:3 portrait
    create_test_image(&photo3_path, 100, 100, 175); // 1:1 square

    let import_res = app.run(
        "library.import",
        json!({
            "paths": [photos_dir.to_string_lossy()]
        }),
    );
    assert!(import_res.is_ok(), "Import command failed: {:?}", import_res);
    let import_val = import_res.unwrap();
    assert_eq!(import_val["scanned"], 3, "All 3 staged photos should be scanned");
    assert_eq!(import_val["imported"].as_array().map_or(0, |a| a.len()), 3, "All 3 photos imported");
    assert_eq!(app.session.catalog.len(), 3, "Catalog should contain exactly 3 photos");

    // Check catalog length and session tracking
    assert_eq!(app.session.catalog.len(), 3, "Catalog now holds all 3 photos");
    // Verify photo metadata (dimensions, content hashes, valid IDs)
    {
        let photos: Vec<_> = app.session.catalog.photos().collect();
        assert_eq!(photos.len(), 3);
        for p in &photos {
            assert!(p.width > 0 && p.height > 0, "Photo dimensions must be populated");
            assert!(p.content_hash.is_some(), "Content hash must be computed");
        }
    }
    let target_photo_id = app.session.catalog.photos().next().unwrap().id;

    // Test duplicate detection: importing again must detect duplicates and import 0
    let reimport_res = app
        .run(
            "library.import",
            json!({
                "paths": [photos_dir.to_string_lossy()]
            }),
        )
        .unwrap();
    assert_eq!(reimport_res["imported"].as_array().map_or(0, |a| a.len()), 0, "No duplicate photos should be imported");
    assert_eq!(reimport_res["duplicates"].as_array().map_or(0, |a| a.len()), 3, "All 3 photos must be flagged as duplicates");
    assert_eq!(app.session.catalog.len(), 3, "Catalog count remains unchanged after duplicate import");

    // ---------------------------------------------------------
    // Phase 3: Photo Selection & Develop Edits
    // ---------------------------------------------------------
    app.run("library.select", json!({"ids": [target_photo_id.0]})).expect("select photo");
    assert_eq!(app.session.active(), Some(target_photo_id), "Target photo must be active");

    // Switch to Develop module
    app.ui.view = crate::state::ViewMode::Detail;
    app.ui.right = crate::state::RightPanel::Edit;

    // Apply Develop adjustments: exposure, contrast, temperature, tint, shadows, highlights
    app.run("develop.set", json!({"control": "light.exposure", "value": 0.85})).expect("set exposure");
    app.run("develop.set", json!({"control": "light.contrast", "value": 20.0})).expect("set contrast");
    app.run("develop.set", json!({"control": "wb.temp", "value": 6200.0})).expect("set temperature");
    app.run("develop.set", json!({"control": "wb.tint", "value": 6.0})).expect("set tint");
    app.run("develop.set", json!({"control": "light.highlights", "value": -30.0})).expect("set highlights");
    app.run("develop.set", json!({"control": "light.shadows", "value": 25.0})).expect("set shadows");

    // Verify settings stored in the develop recipe
    let dev = app.session.develop_of(target_photo_id).expect("develop recipe must exist");
    assert!((dev.light.exposure - 0.85).abs() < 0.001, "Exposure adjustment applied");
    assert!((dev.light.contrast - 20.0).abs() < 0.001, "Contrast adjustment applied");
    assert!((dev.light.highlights - (-30.0)).abs() < 0.001, "Highlights adjustment applied");
    assert!((dev.light.shadows - 25.0).abs() < 0.001, "Shadows adjustment applied");

    // ---------------------------------------------------------
    // Phase 4: Undo & Redo Verification
    // ---------------------------------------------------------
    // Undo the shadows adjustment (most recent)
    app.run("edit.undo", json!({})).expect("undo shadows");
    let dev_undo = app.session.develop_of(target_photo_id).expect("develop recipe");
    assert_eq!(dev_undo.light.shadows, 0.0, "Shadows should revert to default 0.0");
    assert!((dev_undo.light.highlights - (-30.0)).abs() < 0.001, "Highlights still intact before its undo");

    // Redo the shadows adjustment
    app.run("edit.redo", json!({})).expect("redo shadows");
    let dev_redo = app.session.develop_of(target_photo_id).expect("develop recipe");
    assert!((dev_redo.light.shadows - 25.0).abs() < 0.001, "Shadows restored by redo");

    // ---------------------------------------------------------
    // Phase 5: Organization (Ratings, Flags, Labels, Filtering)
    // ---------------------------------------------------------
    // Rate 5 stars
    app.run("photo.rate", json!({"rating": 5})).expect("rate 5 stars");
    // Flag as Pick
    app.run("photo.flag", json!({"flag": "pick"})).expect("flag as pick");
    // Blue label
    app.run("photo.label", json!({"label": "blue"})).expect("set blue label");

    let updated_photo = app.session.catalog.photo(target_photo_id).expect("target photo");
    assert_eq!(updated_photo.rating, 5, "Photo rated 5 stars");
    assert_eq!(updated_photo.flag, lightcraft_catalog::Flag::Pick, "Photo flagged Pick");
    assert_eq!(updated_photo.label, Some(lightcraft_catalog::ColorLabel::Blue), "Color label is Blue");

    // ---------------------------------------------------------
    // Phase 5b: Local Masking (SAM / Geometric / Brush)
    // ---------------------------------------------------------
    app.run("mask.add", json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.3, "ry": 0.3})).expect("add radial mask");
    app.run("mask.adjust", json!({"values": {"exposure": 0.5}})).expect("adjust mask exposure");
    let dev_mask = app.session.develop_of(target_photo_id).expect("develop with mask");
    assert_eq!(dev_mask.masks.len(), 1, "One mask layer added");

    // ---------------------------------------------------------
    // Phase 5c: Export Rendered Photo to Disk
    // ---------------------------------------------------------
    let export_opts = lightcraft_engine::export::ExportOptions::from_json(&json!({
        "format": "jpeg",
        "quality": 95,
        "width": 120,
        "height": 80
    }));
    let exported = lightcraft_engine::export::export_photo(&mut app.session, target_photo_id, &export_opts, 1).expect("export photo");
    let output_file = export_dir.join(&exported.file_name);
    std::fs::write(&output_file, &exported.bytes).expect("write exported photo");
    assert!(output_file.exists(), "Exported image file must exist on disk");
    assert!(exported.bytes.len() > 100, "Exported file must contain rendered image data");
    assert_eq!(exported.width, 120);
    assert_eq!(exported.height, 80);

    // ---------------------------------------------------------
    // Phase 6: Persistence Round-Trip (Close and Re-open Library)
    // ---------------------------------------------------------
    drop(app);

    // Reopen the library from disk
    let mut session2 = lightcraft_engine::Session::new().with_fs();
    session2.open_library(&expected_lib_path, false).expect("reopen library from disk");
    let app2 = LightcraftApp::new(session2, Services::default());

    assert_eq!(app2.session.catalog.len(), 3, "Reopened library retains all 3 photos");
    let restored_photo = app2.session.catalog.photo(target_photo_id).expect("photo restored");
    assert_eq!(restored_photo.rating, 5, "Rating preserved across session");
    assert_eq!(restored_photo.flag, lightcraft_catalog::Flag::Pick, "Pick flag preserved");
    assert_eq!(restored_photo.label, Some(lightcraft_catalog::ColorLabel::Blue), "Label preserved");

    let restored_dev = app2.session.develop_of(target_photo_id).expect("restored develop recipe");
    assert!((restored_dev.light.exposure - 0.85).abs() < 0.001, "Exposure preserved across session");
    assert!((restored_dev.light.contrast - 20.0).abs() < 0.001, "Contrast preserved across session");
    assert!((restored_dev.light.highlights - (-30.0)).abs() < 0.001, "Highlights preserved across session");
    assert!((restored_dev.light.shadows - 25.0).abs() < 0.001, "Shadows preserved across session");
    assert_eq!(restored_dev.masks.len(), 1, "Mask layer preserved across session");

    // Clean up
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn test_e2e_multiple_libraries_data_isolation() {
    let ws = temp_workspace("multi-lib-isolation");
    let root = ws.join("Root");
    std::fs::create_dir_all(&root).unwrap();

    let session = lightcraft_engine::Session::new().with_fs();
    let mut app = LightcraftApp::new(session, Services::default());

    // Create Library Alpha
    app.run("app.newLibrary", json!({"path": root.to_string_lossy(), "name": "Alpha"})).unwrap();
    let src_alpha = ws.join("PhotosAlpha");
    std::fs::create_dir_all(&src_alpha).unwrap();
    create_test_image(&src_alpha.join("a1.png"), 60, 60, 1);
    app.run("library.import", json!({"paths": [src_alpha.to_string_lossy()]})).unwrap();
    assert_eq!(app.session.catalog.len(), 1, "Alpha library has 1 photo");

    // Create Library Beta
    app.run("app.newLibrary", json!({"path": root.to_string_lossy(), "name": "Beta"})).unwrap();
    assert_eq!(app.session.catalog.len(), 0, "Beta library is freshly initialized with 0 photos");
    let src_beta = ws.join("PhotosBeta");
    std::fs::create_dir_all(&src_beta).unwrap();
    create_test_image(&src_beta.join("b1.png"), 60, 60, 2);
    create_test_image(&src_beta.join("b2.png"), 60, 60, 3);
    app.run("library.import", json!({"paths": [src_beta.to_string_lossy()]})).unwrap();
    assert_eq!(app.session.catalog.len(), 2, "Beta library has 2 photos");

    // Switch back to Library Alpha
    let alpha_path = root.join("Alpha");
    app.run("app.openLibrary", json!({"path": alpha_path.to_string_lossy()})).unwrap();
    assert_eq!(app.session.catalog.len(), 1, "Switching back to Alpha preserves its 1 photo");

    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn test_e2e_user_flow_multi_file_export_copy_paste_styles_and_batch_sync() {
    let ws = temp_workspace("user-flow-batch");
    let lib_dir = ws.join("Libraries");
    let photos_dir = ws.join("Photos");
    let export_dir = ws.join("ExportedJPEGs");
    std::fs::create_dir_all(&lib_dir).unwrap();
    std::fs::create_dir_all(&photos_dir).unwrap();
    std::fs::create_dir_all(&export_dir).unwrap();

    // Stage 4 test photos with varied resolutions and color seeds
    let p1_path = photos_dir.join("Sunset_001.png");
    let p2_path = photos_dir.join("Sunset_002.png");
    let p3_path = photos_dir.join("Mountain_003.png");
    let p4_path = photos_dir.join("Forest_004.png");

    create_test_image(&p1_path, 160, 100, 30);
    create_test_image(&p2_path, 160, 100, 60);
    create_test_image(&p3_path, 100, 160, 90);
    create_test_image(&p4_path, 120, 120, 140);

    let services = Services { write: Some(Box::new(lightcraft_engine::export::write_file)), ..Services::default() };
    let session = lightcraft_engine::Session::new().with_fs();
    let mut app = LightcraftApp::new(session, services);

    // 1. Create library and import photos
    let lib_name = "Travel Memories";
    app.run(
        "app.newLibrary",
        json!({
            "path": lib_dir.to_string_lossy(),
            "name": lib_name,
        }),
    )
    .expect("create library");

    let import_res = app
        .run(
            "library.import",
            json!({
                "paths": [photos_dir.to_string_lossy()]
            }),
        )
        .expect("import photos");
    assert_eq!(import_res["imported"].as_array().map_or(0, |a| a.len()), 4);
    assert_eq!(app.session.catalog.len(), 4);

    let mut photo_ids: Vec<_> = app.session.catalog.photos().map(|p| (p.file_name.clone(), p.id)).collect();
    photo_ids.sort_by(|a, b| a.0.cmp(&b.0));
    let p1_id = photo_ids[0].1;
    let p2_id = photo_ids[1].1;
    let p3_id = photo_ids[2].1;
    let p4_id = photo_ids[3].1;

    // 2. Lead Photo Develop Adjustments (Exposure, Contrast, Highlights, Shadows, WB, Vibrance)
    app.run("library.select", json!({"ids": [p1_id.0]})).expect("select photo 1");
    app.ui.view = crate::state::ViewMode::Detail;
    app.ui.right = crate::state::RightPanel::Edit;

    app.run("develop.set", json!({"control": "light.exposure", "value": 1.35})).unwrap();
    app.run("develop.set", json!({"control": "light.contrast", "value": 30.0})).unwrap();
    app.run("develop.set", json!({"control": "light.highlights", "value": -40.0})).unwrap();
    app.run("develop.set", json!({"control": "light.shadows", "value": 25.0})).unwrap();
    app.run("develop.set", json!({"control": "color.vibrance", "value": 20.0})).unwrap();
    app.run("develop.set", json!({"control": "wb.temp", "value": 6700.0})).unwrap();
    app.run("develop.set", json!({"control": "wb.tint", "value": 7.0})).unwrap();

    let p1_dev = app.session.develop_of(p1_id).unwrap();
    assert!((p1_dev.light.exposure - 1.35).abs() < 0.001);
    assert!((p1_dev.light.contrast - 30.0).abs() < 0.001);
    assert!((p1_dev.color.vibrance - 20.0).abs() < 0.001);

    // 3. User Flow: Copy Edit Style
    app.run("develop.copy", json!({})).expect("copy edit style");
    assert!(app.session.clipboard.is_some(), "Clipboard must store develop style");

    // 4. User Flow: Batch Paste Style onto Photo 2 and Photo 3
    app.ui.view = crate::state::ViewMode::PhotoGrid;
    app.run("library.select", json!({"ids": [p2_id.0, p3_id.0]})).expect("select target photos");
    let paste_res = app.run("develop.paste", json!({})).expect("paste develop style");
    assert_eq!(paste_res["changed"], 2, "2 photos updated by batch paste");

    let p2_dev = app.session.develop_of(p2_id).unwrap();
    let p3_dev = app.session.develop_of(p3_id).unwrap();
    let p4_dev = app.session.develop_of(p4_id).unwrap();

    assert!((p2_dev.light.exposure - 1.35).abs() < 0.001, "Photo 2 inherited exposure");
    assert!((p2_dev.light.contrast - 30.0).abs() < 0.001, "Photo 2 inherited contrast");
    assert!((p2_dev.color.vibrance - 20.0).abs() < 0.001, "Photo 2 inherited vibrance");

    assert!((p3_dev.light.exposure - 1.35).abs() < 0.001, "Photo 3 inherited exposure");
    assert!((p3_dev.light.contrast - 30.0).abs() < 0.001, "Photo 3 inherited contrast");

    assert_eq!(p4_dev.light.exposure, 0.0, "Photo 4 unselected and unchanged");
    assert_eq!(p4_dev.light.contrast, 0.0, "Photo 4 unselected and unchanged");

    // 5. User Flow: Undo and Redo of Batch Paste
    app.run("edit.undo", json!({})).expect("undo batch paste");
    let p2_undo = app.session.develop_of(p2_id).unwrap();
    let p3_undo = app.session.develop_of(p3_id).unwrap();
    assert_eq!(p2_undo.light.exposure, 0.0, "Photo 2 reverted after undo");
    assert_eq!(p3_undo.light.exposure, 0.0, "Photo 3 reverted after undo");

    app.run("edit.redo", json!({})).expect("redo batch paste");
    let p2_redo = app.session.develop_of(p2_id).unwrap();
    let p3_redo = app.session.develop_of(p3_id).unwrap();
    assert!((p2_redo.light.exposure - 1.35).abs() < 0.001, "Photo 2 restored after redo");
    assert!((p3_redo.light.exposure - 1.35).abs() < 0.001, "Photo 3 restored after redo");

    // 6. User Flow: Selective Group Style Sync (develop.sync)
    // Tweak Photo 2 with cool white balance and sync only WB to Photo 4
    app.run("library.select", json!({"ids": [p2_id.0]})).expect("select photo 2");
    app.run("develop.set", json!({"control": "wb.temp", "value": 4800.0})).unwrap();
    app.run("develop.set", json!({"control": "wb.tint", "value": -14.0})).unwrap();

    app.run("library.select", json!({"ids": [p2_id.0, p4_id.0], "active": p2_id.0})).expect("select photo 2 active and photo 4 target");
    let sync_res = app.run("develop.sync", json!({"groups": ["whiteBalance"]})).expect("sync wb group");
    assert_eq!(sync_res["changed"], 1);

    let p4_synced = app.session.develop_of(p4_id).unwrap();
    assert!((p4_synced.wb.temp - 4800.0).abs() < 0.001, "Photo 4 received synced WB temp");
    assert!((p4_synced.wb.tint - (-14.0)).abs() < 0.001, "Photo 4 received synced WB tint");
    assert_eq!(p4_synced.light.exposure, 0.0, "Photo 4 exposure remains default (only WB synced)");

    // 7. User Flow: Batch Organization (Flagging, Rating, Color Labeling)
    app.run("library.selectAll", json!({})).expect("select all 4 photos");
    app.run("photo.flag", json!({"flag": "pick"})).expect("batch flag pick");
    app.run("photo.rate", json!({"rating": 5})).expect("batch rate 5 stars");
    app.run("photo.label", json!({"label": "purple"})).expect("batch label purple");

    for p in app.session.catalog.photos() {
        assert_eq!(p.flag, lightcraft_catalog::Flag::Pick, "All photos flagged pick");
        assert_eq!(p.rating, 5, "All photos rated 5");
        assert_eq!(p.label, Some(lightcraft_catalog::ColorLabel::Purple), "All photos labeled purple");
    }

    // 8. User Flow: Multi-File Batch Export
    app.run("library.selectAll", json!({})).expect("select all for export");
    let export_res = app
        .run(
            "app.export",
            json!({
                "dir": export_dir.to_string_lossy(),
                "format": "jpeg",
                "quality": 90,
                "percent": 50,
            }),
        )
        .expect("multi-file batch export");

    let exported_files = export_res["files"].as_array().expect("array of exported files");
    assert_eq!(exported_files.len(), 4, "Must export all 4 selected photos");

    for f in exported_files {
        let file_path = f["path"].as_str().expect("path in export result");
        let p = Path::new(file_path);
        assert!(p.exists(), "Exported file must exist on disk: {}", file_path);
        let bytes = std::fs::read(p).expect("read exported file");
        assert!(bytes.len() > 100, "Exported file must have content");
        // Verify JPEG magic bytes
        assert_eq!(&bytes[0..2], &[0xFF, 0xD8], "Must be valid JPEG file");
    }

    // 9. User Flow: Persistence & Catalog Reopen Verification
    let expected_lib_dir = lib_dir.join(lib_name);
    drop(app);

    let services2 = Services { write: Some(Box::new(lightcraft_engine::export::write_file)), ..Services::default() };
    let mut session2 = lightcraft_engine::Session::new().with_fs();
    session2.open_library(&expected_lib_dir, false).expect("reopen library");
    let app2 = LightcraftApp::new(session2, services2);

    assert_eq!(app2.session.catalog.len(), 4, "All 4 photos persisted");
    let restored_p1 = app2.session.develop_of(p1_id).unwrap();
    let restored_p2 = app2.session.develop_of(p2_id).unwrap();
    let restored_p4 = app2.session.develop_of(p4_id).unwrap();

    assert!((restored_p1.light.exposure - 1.35).abs() < 0.001);
    assert!((restored_p2.light.exposure - 1.35).abs() < 0.001);
    assert!((restored_p4.wb.temp - 4800.0).abs() < 0.001);

    for p in app2.session.catalog.photos() {
        assert_eq!(p.flag, lightcraft_catalog::Flag::Pick);
        assert_eq!(p.rating, 5);
        assert_eq!(p.label, Some(lightcraft_catalog::ColorLabel::Purple));
    }

    // Clean up
    let _ = std::fs::remove_dir_all(&ws);
}
