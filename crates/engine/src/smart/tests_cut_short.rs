//! A Compact proxy cut short (by a version before atomic writes, issue #106, or an interrupted
//! copy or sync) still decodes as JPEG, with part of the picture wrong. It must not count as a
//! smart preview or render offline, Build Smart Previews must repair it, and a folder move must
//! never keep it over the complete copy.

use super::*;
use crate::{Session, media};
use lightcraft_preview::{Hash128, PreviewCache};
use serde_json::json;

/// A float TIFF original whose Compact proxy has a realistic size.
fn write_original(path: &Path, w: usize, h: usize) {
    let mut image = Rgb32f::new(w, h);
    for (i, p) in image.data.iter_mut().enumerate() {
        let x = (i % w) as f32 / w as f32;
        let y = (i / w) as f32 / h as f32;
        *p = [x * 0.8 + y * 0.2, (x * 9.0).sin() * 0.3 + 0.4, y * 0.9 - x * 0.1 + 0.1];
    }
    let encoded = lightcraft_codecs::EncodeImage::new(w as u32, h as u32, 3, lightcraft_codecs::Samples::F32(image.data.as_flattened()));
    std::fs::write(path, lightcraft_codecs::encode_tiff(&encoded, lightcraft_codecs::TiffCompression::Deflate, &Default::default()).unwrap())
        .unwrap();
}

#[test]
fn regression709_cut_short_compact_proxies_are_repaired_and_never_kept() {
    let root = super::tests::temp("cut-short-compact");
    let original = root.join("original.tif");
    write_original(&original, 1200, 800);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    s.execute("library.import", &json!({"paths": [original]})).unwrap();
    let id = s.catalog.photos().next().unwrap().id;
    s.execute("library.smartPreviews", &json!({"format": "compact", "ids": [id.0]})).unwrap();
    let photo = s.catalog.photo(id).unwrap().clone();
    let file = file_path(s.media.smart_dir.as_ref().unwrap(), &photo);
    let full = std::fs::read(&file).unwrap();
    assert_eq!(header(&file).unwrap().0, Format::Compact);
    assert!(full.len() > 20_000, "a realistic proxy: {} bytes", full.len());
    // The render path once the original is known to be offline (`SourceRef::Smart`).
    let offline_render = |s: &mut Session, path: &Path| {
        let mut job = s.thumb_job(id, 256).unwrap();
        job.source = media::SourceRef::Smart { path: path.to_path_buf() };
        job.cache = Some((Arc::new(PreviewCache::memory(1_000_000)), Hash128(0)));
        job.run().rendered.map(|r| r.image)
    };
    let intact = offline_render(&mut s, &file).unwrap();
    let mut problems = Vec::new();
    for percent in [50, 75, 90, 99] {
        std::fs::write(&file, &full[..full.len() * percent / 100]).unwrap();
        if s.execute("photo.smartPreview", &json!({"id": id.0})).unwrap()["smartPreview"] != json!(false) {
            problems.push(format!("{percent}%: Info reports a smart preview"));
        }
        if let Ok(picture) = offline_render(&mut s, &file) {
            let wrong = picture.data.iter().zip(&intact.data).filter(|(a, b)| a != b).count();
            problems.push(format!("{percent}%: rendered offline, {wrong} of {} pixels wrong", picture.data.len()));
        }
        let r = s.execute("library.smartPreviews", &json!({"ids": [id.0]})).unwrap();
        if r["repaired"] != json!(1) || std::fs::read(&file).unwrap() != full {
            problems.push(format!("{percent}%: Build kept it ({r})"));
            std::fs::write(&file, &full).unwrap();
        }
    }
    // A move into a folder that already holds a cut-short copy (an earlier interrupted copy).
    let to = root.join("moved");
    let copy = sharded_file_path(&to, &photo);
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::write(&copy, &full[..full.len() * 9 / 10]).unwrap();
    s.execute("library.smartPreviewsLocation", &json!({"path": to, "existing": "move"})).unwrap();
    assert_eq!(s.media.smart_dir.as_ref(), Some(&to));
    let active = std::fs::read(file_path(&to, &photo)).unwrap();
    if active != full {
        problems.push(format!(
            "the move kept the {}-byte cut copy and removed the complete {}-byte proxy (old exists={})",
            active.len(),
            full.len(),
            file.exists()
        ));
    }
    assert!(problems.is_empty(), "{problems:#?}");
    assert_eq!(offline_render(&mut s, &file_path(&to, &photo)).unwrap().data, intact.data);
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}

/// A proxy that cannot be verified at the new place (here cut short, with its original offline,
/// so Build cannot repair it) stays in the old folder and is named; the rest of the folder moves.
#[test]
fn regression709_a_move_leaves_a_cut_short_proxy_behind_and_moves_the_rest() {
    let root = super::tests::temp("cut-short-move");
    let (cut, intact) = (root.join("cut.tif"), root.join("intact.tif"));
    write_original(&cut, 1200, 800);
    write_original(&intact, 400, 300);
    let mut s = Session::new().with_fs();
    s.open_library(root.join("library"), false).unwrap();
    s.execute("library.import", &json!({"paths": [&cut, &intact]})).unwrap();
    let photo = |s: &Session, name: &str| {
        s.catalog.photos().find(|p| matches!(&p.source, lightcraft_catalog::Source::File { path } if path.ends_with(name))).unwrap().clone()
    };
    let (cut_photo, intact_photo) = (photo(&s, "cut.tif"), photo(&s, "intact.tif"));
    s.execute("library.smartPreviews", &json!({"format": "compact", "ids": [cut_photo.id.0, intact_photo.id.0]})).unwrap();
    let from = s.media.smart_dir.clone().unwrap();
    let cut_file = file_path(&from, &cut_photo);
    let full = std::fs::read(&cut_file).unwrap();
    std::fs::write(&cut_file, &full[..full.len() * 9 / 10]).unwrap();
    std::fs::rename(&cut, cut.with_extension("offline")).unwrap();
    let intact_bytes = std::fs::read(file_path(&from, &intact_photo)).unwrap();
    let to = root.join("moved");
    let r = s.execute("library.smartPreviewsLocation", &json!({"path": to, "existing": "move"}));
    let r = r.unwrap_or_else(|e| panic!("one cut-short proxy refused the whole move: {e}"));
    assert_eq!(s.media.smart_dir.as_ref(), Some(&to), "{r}");
    assert_eq!(r["handled"], json!(1), "{r}");
    assert_eq!(std::fs::read(file_path(&to, &intact_photo)).unwrap(), intact_bytes, "the intact proxy moved");
    assert!(!file_path(&from, &intact_photo).exists());
    assert_eq!(std::fs::read(&cut_file).unwrap().len(), full.len() * 9 / 10, "the cut-short proxy stays in the old folder");
    assert!(!file_path(&to, &cut_photo).exists());
    let failed: Vec<&str> = r["failed"].as_array().unwrap().iter().filter_map(serde_json::Value::as_str).collect();
    let name = cut_file.file_name().unwrap().to_string_lossy().to_string();
    assert!(failed.len() == 1 && failed[0].contains(&name), "the result names the file left behind: {failed:?}");
    drop(s);
    std::fs::remove_dir_all(root).unwrap();
}
