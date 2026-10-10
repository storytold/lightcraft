//! Photo Merge through the command layer: synthetic brackets / panorama views on disk → merged
//! DNG next to the sources → imported, selected, Auto Settings / auto crop applied.

use serde_json::json;

use crate::Session;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-merge-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn import(s: &mut Session, paths: Vec<String>) -> Vec<u64> {
    let r = s.execute("library.import", &json!({"paths": paths})).unwrap();
    r["imported"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect()
}

#[test]
fn hdr_merge_command_creates_and_imports_a_dng() {
    let dir = temp_dir("hdr");
    let mut paths = Vec::new();
    for (i, b) in lightcraft_merge::synth::bracket_dngs(480, 320, &[-2.0, 0.0, 2.0]).unwrap().into_iter().enumerate() {
        let p = dir.join(format!("IMG_{i}.dng"));
        std::fs::write(&p, b).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    assert_eq!(ids.len(), 3);
    // preview: rendered PNG, nothing added
    let png = dir.join("preview.png");
    let pv = s.execute("merge.hdr", &json!({"ids": ids, "deghost": "medium", "preview": true, "showOverlay": true, "previewPath": png})).unwrap();
    assert!(png.exists() && pv["previewSize"][0].as_u64().unwrap() > 100, "{pv}");
    assert_eq!(s.catalog.photos().count(), 3);
    let ev: Vec<f64> = pv["ev"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert!((ev[0] + 2.0).abs() < 0.1 && (ev[2] - 2.0).abs() < 0.1, "{ev:?}");
    // full merge
    let r = s.execute("merge.hdr", &json!({"ids": ids, "autoSettings": true, "stack": true})).unwrap();
    let path = r["path"].as_str().unwrap();
    assert!(path.ends_with("IMG_0-HDR.dng"), "{path}");
    let id = lightcraft_catalog::PhotoId(r["id"].as_u64().unwrap());
    let p = s.catalog.photo(id).unwrap().clone();
    assert_eq!((p.width, p.height), (480, 320));
    assert_eq!(p.kind, lightcraft_catalog::MediaKind::Raw);
    assert_eq!(s.selection.active, Some(id));
    let d = s.develop_of(id).unwrap();
    assert!(d.light.exposure != 0.0 || d.light.highlights != 0.0, "auto settings applied");
    let img = s.render_now(id, 256, 256).unwrap();
    assert_eq!(img.image.width, 256);
    // Create Stack: the result tops a collapsed stack of its sources, so the grid shows the result
    let st = s.catalog.stack_of(id).expect("merge result stacked").clone();
    assert_eq!(st.top(), id);
    assert_eq!(st.photos[1..].iter().map(|p| p.0).collect::<Vec<_>>(), ids);
    assert!(st.collapsed);
    assert_eq!(s.visible_cloned(), vec![id]);
    // A repeated deterministic merge reuses the existing content instead of publishing an
    // unimported uniquely named orphan.
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.73})).unwrap();
    let edited = s.develop_of(id).unwrap().as_ref().clone();
    let stack_before = s.catalog.stack_of(id).unwrap().clone();
    let undo_before = s.undo.len();
    let r2 = s.execute("merge.hdr", &json!({"ids": ids, "autoSettings": true, "stack": true})).unwrap();
    assert_eq!(r2["id"].as_u64(), Some(id.0));
    assert_eq!(r2["path"].as_str(), Some(path));
    assert!(!dir.join("IMG_0-HDR-2.dng").exists(), "duplicate merge left an orphan");
    assert_eq!(s.catalog.photos().count(), 4);
    assert_eq!(s.undo.len(), undo_before, "reusing merge content added an undo step");
    assert_eq!(s.develop_of(id).unwrap().as_ref(), &edited, "reusing merge content changed edits");
    assert_eq!(s.catalog.stack_of(id), Some(&stack_before), "reusing merge content changed stack");
    // the type filter finds merge results (and only them)
    s.execute("library.filter", &json!({"merged": "hdr"})).unwrap();
    let found: std::collections::HashSet<_> = s.catalog.query(&s.filter, &Default::default()).into_iter().collect();
    assert_eq!(found, [id].into_iter().collect());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hdr_merge_refuses_stale_result_before_writing() {
    let dir = temp_dir("hdr-stale");
    let mut paths = Vec::new();
    for (i, b) in lightcraft_merge::synth::bracket_dngs(240, 160, &[-2.0, 0.0, 2.0]).unwrap().into_iter().enumerate() {
        let p = dir.join(format!("IMG_{i}.dng"));
        std::fs::write(&p, b).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths.clone());
    let first = s.execute("merge.hdr", &json!({"ids": ids, "autoSettings": false})).unwrap();
    let id = lightcraft_catalog::PhotoId(first["id"].as_u64().unwrap());
    let path = first["path"].as_str().unwrap().to_string();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.73})).unwrap();
    let edited = s.develop_of(id).unwrap().as_ref().clone();
    let stack_before = s.catalog.stack_of(id).cloned();

    std::fs::remove_file(&path).unwrap();
    let undo_before = s.undo.len();
    let missing = s.execute("merge.hdr", &json!({"ids": ids, "autoSettings": true}));
    assert!(missing.as_ref().unwrap_err().to_string().contains("missing or changed"));
    assert!(!std::path::Path::new(&path).exists());
    assert!(!dir.join("IMG_0-HDR-2.dng").exists());
    assert_eq!(s.undo.len(), undo_before);
    assert_eq!(s.develop_of(id).unwrap().as_ref(), &edited, "repair changed edits");
    assert_eq!(s.catalog.stack_of(id).cloned(), stack_before);

    std::fs::write(&path, std::fs::read(&paths[0]).unwrap()).unwrap();
    let changed_bytes = std::fs::read(&path).unwrap();
    let changed = s.execute("merge.hdr", &json!({"ids": ids, "autoSettings": true}));
    assert!(changed.as_ref().unwrap_err().to_string().contains("missing or changed"));
    assert_eq!(std::fs::read(&path).unwrap(), changed_bytes);
    assert!(!dir.join("IMG_0-HDR-2.dng").exists());
    assert_eq!(s.undo.len(), undo_before);
    assert_eq!(s.develop_of(id).unwrap().as_ref(), &edited, "changed-source repair changed edits");
    assert_eq!(s.catalog.stack_of(id).cloned(), stack_before);

    s.execute("photo.delete", &json!({"ids": [id.0]})).unwrap();
    let deleted_bytes = std::fs::read(&path).unwrap();
    let undo_after_delete = s.undo.len();
    let failed = s.execute("merge.hdr", &json!({"ids": ids, "autoSettings": false}));
    assert!(failed.as_ref().unwrap_err().to_string().contains("deleted"));
    assert_eq!(std::fs::read(&path).unwrap(), deleted_bytes);
    assert!(!dir.join("IMG_0-HDR-3.dng").exists(), "failed import left an orphan");
    assert_eq!(s.undo.len(), undo_after_delete);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn panorama_command_with_auto_crop() {
    let dir = temp_dir("pano");
    let mut paths = Vec::new();
    for (i, v) in lightcraft_merge::synth::pano_views(480, 360, 420.0, &[-25.0, 0.0, 25.0]).unwrap().into_iter().enumerate() {
        let img = v.to_srgb8();
        let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        let p = dir.join(format!("P_{i}.png"));
        std::fs::write(&p, png).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    let r = s.execute("merge.panorama", &json!({"ids": ids, "projection": "cylindrical", "autoCrop": true, "autoSettings": false})).unwrap();
    assert_eq!(r["projection"], "cylindrical");
    assert_eq!(r["used"].as_array().unwrap().len(), 3);
    let id = lightcraft_catalog::PhotoId(r["id"].as_u64().unwrap());
    let p = s.catalog.photo(id).unwrap().clone();
    assert!(p.width > 800 && p.height > 300, "{}×{}", p.width, p.height);
    let crop = s.develop_of(id).unwrap().crop.geometry.rect;
    assert!(crop.width() < 1.0 && crop.width() * crop.height() > 0.5, "{crop:?}");
    // too few photos
    assert!(s.execute("merge.panorama", &json!({"ids": [ids[0]]})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
