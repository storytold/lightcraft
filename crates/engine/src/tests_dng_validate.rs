//! Library ▸ Validate DNG Files (`dng.validate`) and Photo ▸ Reset Export Status (`photo.resetExportStatus`).

use std::sync::atomic::AtomicBool;

use serde_json::json;

use crate::Session;
use crate::dng_validate::{dng_targets, validate};
use crate::export::{Destination, ExportOptions, export_batch, mark_exported};
use crate::tests_xmp::{synthetic_dng_with, temp_dir};

/// The pixel bytes of the file's raw tile, to damage them without touching the structure.
fn flip_in_first_tile(bytes: &mut [u8]) {
    let tiff = lcc_tiff::Tiff::parse(bytes).unwrap();
    let ifd = tiff.all_ifds().into_iter().find(|i| i.contains(lcc_tiff::tags::TILE_OFFSETS)).unwrap();
    let off = ifd.u64s(lcc_tiff::tags::TILE_OFFSETS).unwrap()[0] as usize;
    let len = ifd.u64s(lcc_tiff::tags::TILE_BYTE_COUNTS).unwrap()[0] as usize;
    // (the 32 x 24 image sits in the corner of a 256 x 256 tile: its first rows are right after the JPEG header,
    // the rest of the tile is padding that is cropped away)
    assert!(len > 80, "tile of {len} bytes");
    for b in &mut bytes[off + 44..off + 52] {
        *b ^= 0x5A;
    }
}

#[test]
fn validate_dng_files_reports_the_damaged_ones() {
    let dir = temp_dir("validate-dng");
    let good = synthetic_dng_with(None, Default::default());
    // a second, different file (the library would take an identical one for a duplicate), damaged after it is in
    let other = synthetic_dng_with(None, lcc_meta::Metadata { make: Some("Other".into()), model: Some("Cam".into()), ..Default::default() });
    std::fs::write(dir.join("good.dng"), &good).unwrap();
    std::fs::write(dir.join("bad.dng"), &other).unwrap();
    std::fs::write(dir.join("cut.dng"), &good[..good.len() / 2]).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [dir.join("good.dng").to_string_lossy(), dir.join("bad.dng").to_string_lossy()]})).unwrap();
    let ids: Vec<u64> = s.catalog.photos().map(|p| p.id.0).collect();
    assert_eq!(ids.len(), 2);
    s.execute("library.select", &json!({"ids": ids})).unwrap();
    let mut bad = other.clone();
    flip_in_first_tile(&mut bad);
    std::fs::write(dir.join("bad.dng"), &bad).unwrap();

    let r = s.execute("dng.validate", &json!({})).unwrap();
    assert_eq!((r["total"].as_u64(), r["checked"].as_u64(), r["intact"].as_u64()), (Some(2), Some(2), Some(1)), "{r}");
    let damaged = r["damaged"].as_array().unwrap();
    assert_eq!(damaged.len(), 1, "{r}");
    assert_eq!(damaged[0]["file"], "bad.dng");
    assert!(r["unverifiable"].as_array().unwrap().is_empty());

    // a cancelled run stops before the next file and says so
    let targets = dng_targets(&s, &s.catalog.photos().map(|p| p.id).collect::<Vec<_>>());
    let stopped = validate(&targets, &AtomicBool::new(true), &|_| {});
    assert!((stopped.cancelled, stopped.checked, stopped.total) == (true, 0, 2));

    // a file that went missing or was cut short is damage too, never a panic
    let report = validate(
        &[
            crate::dng_validate::DngTarget {
                id: lcc_catalog::PhotoId(1),
                name: "cut.dng".into(),
                path: dir.join("cut.dng").to_string_lossy().into(),
            },
            crate::dng_validate::DngTarget {
                id: lcc_catalog::PhotoId(2),
                name: "gone.dng".into(),
                path: dir.join("gone.dng").to_string_lossy().into(),
            },
        ],
        &AtomicBool::new(false),
        &|_| {},
    );
    assert_eq!(report.damaged.len(), 2, "{report:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exported_photos_are_marked_and_reset_export_status_clears_them() {
    let dir = temp_dir("export-status");
    let src = dir.join("IMG_1.jpg");
    let img = lcc_raster::Rgba8::from_fn(48, 32, |x, y| [(x * 5) as u8, (y * 7) as u8, 120, 255]);
    std::fs::write(&src, crate::export::encode_image(&img, &ExportOptions::default()).unwrap()).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().exported, None, "never exported yet");
    assert!(dng_targets(&s, &[id]).is_empty(), "only DNG files are checked: a JPEG is not one");

    let out = dir.join("out");
    let to = Destination { dir: out.to_string_lossy().into(), exact: None };
    let mut written = Vec::new();
    let files = export_batch(
        &mut s,
        &[id],
        &ExportOptions::default(),
        &to,
        &mut |p, _| {
            written.push(p.to_string());
            Ok(())
        },
        &|_| false,
    )
    .unwrap();
    assert_eq!(written.len(), 1);
    // exporting does not itself change the catalog; the app calls `mark_exported` with what the batch wrote
    assert_eq!(s.catalog.photo(id).unwrap().exported, None);
    assert_eq!(mark_exported(&mut s, &files), 1);
    assert!(s.catalog.photo(id).unwrap().exported.is_some());
    // failures name their photo but wrote nothing: they are not marked
    assert_eq!(mark_exported(&mut s, &[json!({"photo": id.0, "file": "x.jpg", "error": "disk full"})]), 0);

    let r = s.execute("photo.resetExportStatus", &json!({})).unwrap();
    assert_eq!(r["changed"], 1);
    assert_eq!(s.catalog.photo(id).unwrap().exported, None);
    assert_eq!(s.execute("photo.resetExportStatus", &json!({})).unwrap()["changed"], 0, "nothing left to reset, and no undo step for it");
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.catalog.photo(id).unwrap().exported.is_some(), "undo brings the status back");
    let _ = std::fs::remove_dir_all(&dir);
}
