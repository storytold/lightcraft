//! Photo Merge through the command layer: synthetic brackets / panorama views on disk → merged
//! DNG next to the sources → imported, selected, Auto Settings / auto crop applied.

use serde_json::json;

use crate::Session;

#[test]
fn hdr_merge_rejects_over_budget_dimensions_before_reading_sources() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let mut s = Session::new();
    let mut ids = Vec::new();
    for i in 0..32 {
        let id = s.catalog.alloc_photo_id();
        let path = format!("synthetic-{i}.dng");
        let photo =
            lcc_catalog::Photo::new(id, lcc_catalog::Source::File { path: path.clone() }, &path, "DNG", 65_535, 65_535, "2026-10-16T00:00:00Z");
        s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        ids.push(id.0);
    }

    let reads = std::sync::Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    s.media.file_bytes = Some(std::sync::Arc::new(move |_| {
        count.fetch_add(1, Ordering::Relaxed);
        Err("source reader should not be called".into())
    }));

    let error = s.execute("merge.hdr", &json!({"ids": ids})).unwrap_err().to_string();
    assert!(error.to_lowercase().contains("memory budget"), "{error}");
    assert_eq!(reads.load(Ordering::Relaxed), 0, "Photo Merge read source bytes before rejecting its memory estimate");
}

#[test]
fn hdr_batch_checks_each_bracket_set_before_reading_its_sources() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let mut s = Session::new();
    let mut ids = Vec::new();
    for (i, (width, height)) in [(65_535, 65_535), (65_535, 65_535), (320, 240), (320, 240)].into_iter().enumerate() {
        let id = s.catalog.alloc_photo_id();
        let path = format!("bracket-{i}.dng");
        let photo =
            lcc_catalog::Photo::new(id, lcc_catalog::Source::File { path: path.clone() }, &path, "DNG", width, height, "2026-10-16T00:00:00Z");
        s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        ids.push(id);
    }
    for pair in [(&ids[0], &ids[1]), (&ids[2], &ids[3])] {
        let op = s.catalog.group_ops(*pair.0, std::slice::from_ref(pair.1), true).unwrap();
        s.catalog.apply(op).unwrap();
    }

    let reads = std::sync::Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    s.media.file_bytes = Some(std::sync::Arc::new(move |_| {
        count.fetch_add(1, Ordering::Relaxed);
        Err("synthetic source has no bytes".into())
    }));

    let result = s.execute("merge.hdrBatch", &json!({"ids": ids.iter().map(|id| id.0).collect::<Vec<_>>()})).unwrap();
    let failed = result["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 2, "{result}");
    assert!(failed[0]["error"].as_str().unwrap().contains("memory budget"), "{result}");
    assert!(failed[1]["error"].as_str().unwrap().contains("synthetic source has no bytes"), "{result}");
    assert_eq!(reads.load(Ordering::Relaxed), 1, "the oversized bracket was read, or the small bracket read more than once");
}

#[test]
fn merge_memory_fallback_is_representable_on_32_bit_targets() {
    use crate::merge::{merge_memory_bytes, unknown_ram_fallback};

    let simulated_32_bit_max = u32::MAX as usize;
    let fallback = unknown_ram_fallback(simulated_32_bit_max);
    assert_eq!(fallback, simulated_32_bit_max, "8 GiB must clamp to the largest representable 32-bit byte count");
    assert!(merge_memory_bytes(Some(fallback), 0) > 0);
    assert!(merge_memory_bytes(None, 0) > 0, "the native unknown-RAM fallback must stay nonzero");
}

#[test]
fn panorama_memory_estimate_allows_projected_canvas_larger_than_source_area() {
    use crate::merge::{MergeKind, estimate_merge_working_bytes_with_budget};
    use lcc_merge::{Frame, PanoOptions, Projection};

    let frames: Vec<Frame> = lcc_merge::synth::pano_views(240, 180, 210.0, &[-35.0, 0.0, 35.0])
        .unwrap()
        .into_iter()
        .map(|image| Frame::from_linear_rec2020(image, None))
        .collect();
    let source_pixels: usize = frames.iter().map(|frame| frame.width() * frame.height()).sum();
    let options = PanoOptions { projection: Projection::Perspective, max_megapixels: 4.0, ..Default::default() };
    let stitched = lcc_merge::stitch(frames, &options, &|_, _| true).unwrap();
    let output_pixels = stitched.image.width.checked_mul(stitched.image.height).unwrap();
    assert!(output_pixels > source_pixels, "fixture must reproduce output larger than source area");

    let bounded_frames: Vec<Frame> = lcc_merge::synth::pano_views(240, 180, 210.0, &[-35.0, 0.0, 35.0])
        .unwrap()
        .into_iter()
        .map(|image| Frame::from_linear_rec2020(image, None))
        .collect();
    let bounded = lcc_merge::pano::stitch_with_output_pixel_limit(bounded_frames, &options, source_pixels, &|_, _| true).unwrap();
    let bounded_pixels = bounded.image.width.checked_mul(bounded.image.height).unwrap();
    assert!(bounded_pixels <= source_pixels, "bounded canvas uses {bounded_pixels} pixels above its {source_pixels}-pixel limit");
    assert!(bounded_pixels < output_pixels, "the native canvas should be scaled down to fit the pixel limit");

    let estimated = estimate_merge_working_bytes_with_budget(&MergeKind::Panorama { pano: options }, &[(240, 180); 3], None, usize::MAX).unwrap();
    assert!(
        estimated >= output_pixels.checked_mul(68).unwrap(),
        "estimated panorama working set {estimated} must cover at least the output blend buffer for {output_pixels} pixels"
    );
}

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
    for (i, b) in lcc_merge::synth::bracket_dngs(480, 320, &[-2.0, 0.0, 2.0]).unwrap().into_iter().enumerate() {
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
    assert_eq!(r["reused"].as_bool(), Some(false));
    let id = lcc_catalog::PhotoId(r["id"].as_u64().unwrap());
    let p = s.catalog.photo(id).unwrap().clone();
    assert_eq!((p.width, p.height), (480, 320));
    assert_eq!(p.kind, lcc_catalog::MediaKind::Raw);
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
    assert_eq!(r2["reused"].as_bool(), Some(true));
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

/// An HDR merge keeps the reference frame's lens corrections. The sources carry an off-centre `WarpRectilinear` and
/// `FixVignetteRadial` for their active area and have a default crop; the merged DNG must give "Enable Profile
/// Corrections" the same correction as the reference photo, upright and for a vertical bracket (whose DNG keeps the
/// pixels unturned).
#[test]
fn hdr_merge_keeps_the_reference_frames_lens_corrections() {
    use lcc_geom::{Orientation, Point};
    use lcc_pipeline::optics::{embedded_vignette_gain, embedded_warp};
    use lcc_raw::Opcode;
    for orientation in [Orientation::Normal, Orientation::Rotate90] {
        let dir = temp_dir(&format!("hdr-lens-{orientation:?}"));
        let mut paths = Vec::new();
        for (i, b) in lcc_merge::synth::bracket_dngs(480, 320, &[-2.0, 0.0, 2.0]).unwrap().into_iter().enumerate() {
            let mut raw = lcc_raw::decode(&b).unwrap();
            raw.crop = lcc_raw::Rect::new(10, 6, 452, 304);
            raw.orientation = orientation;
            raw.metadata.orientation = Some(orientation);
            raw.opcodes.list3 = vec![
                Opcode::WarpRectilinear { planes: vec![[0.96, 0.05, -0.012, 0.002, 0.001, -0.0005]], center: [0.48, 0.53] },
                Opcode::FixVignetteRadial { k: [0.35, -0.05, 0.01, 0.0, 0.0], center: [0.48, 0.53] },
            ];
            let p = dir.join(format!("IMG_{i}.dng"));
            std::fs::write(&p, lcc_raw::write_dng(&raw, &Default::default()).unwrap()).unwrap();
            paths.push(p.to_string_lossy().to_string());
        }
        let mut s = Session::new().with_fs();
        let ids = import(&mut s, paths);
        let r = s.execute("merge.hdr", &json!({"ids": ids})).unwrap();
        assert_eq!(r["reference"].as_u64(), Some(ids[1]), "{r}");
        let merged = s.catalog.photo(lcc_catalog::PhotoId(r["id"].as_u64().unwrap())).unwrap().clone();
        let reference = s.catalog.photo(lcc_catalog::PhotoId(ids[1])).unwrap().clone();
        let expect = if orientation == Orientation::Normal { (452, 304) } else { (304, 452) };
        assert_eq!(((merged.width, merged.height), (reference.width, reference.height)), (expect, expect));
        let a = reference.embedded_lens.clone().expect("the source's correction");
        let b = merged.embedded_lens.clone().expect("the merged DNG carries the reference frame's correction");
        let (wa, wb) = (a.warp.unwrap(), b.warp.unwrap());
        let (va, vb) = (a.vignette.unwrap(), b.vignette.unwrap());
        let (w, h) = (expect.0 as f64, expect.1 as f64);
        for (x, y) in [(0.0, 0.0), (w, h), (w, 0.0), (0.0, h), (w * 0.3, h * 0.7), (w * 0.5, h * 0.5)] {
            let p = Point::new(x, y);
            for ch in 0..3 {
                let (pa, pb) = (embedded_warp(&wa, p, ch, w, h), embedded_warp(&wb, p, ch, w, h));
                assert!((pa.x - pb.x).abs() < 1e-6 && (pa.y - pb.y).abs() < 1e-6, "{orientation:?} {p:?}: {pa:?} vs {pb:?}");
            }
            let (ga, gb) = (embedded_vignette_gain(&va, p, w, h), embedded_vignette_gain(&vb, p, w, h));
            assert!((ga - gb).abs() < 1e-9, "{orientation:?} {p:?}: {ga} vs {gb}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Raws without colour matrices of their own (CR2, NEF, ARW…) merge into a DNG with the colour they open with,
/// not with no colour at all (issue #965: an HDR merge of CR2s rendered green and dark).
#[test]
fn a_merge_of_raws_without_colour_matrices_keeps_their_colour() {
    let dir = temp_dir("hdr-colour");
    let neutral = [0.5, 1.0, 0.7];
    let mut paths = Vec::new();
    for (i, b) in lcc_merge::synth::bracket_dngs(240, 160, &[-1.0, 0.0, 1.0]).unwrap().into_iter().enumerate() {
        // the same pixels with a white balance but no colour matrices
        let mut raw = lcc_raw::decode(&b).unwrap();
        raw.color = lcc_raw::ColorData { as_shot_neutral: Some(neutral), ..Default::default() };
        let p = dir.join(format!("IMG_{i}.dng"));
        std::fs::write(&p, lcc_raw::write_dng(&raw, &Default::default()).unwrap()).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    let r = s.execute("merge.hdr", &json!({"ids": ids, "autoSettings": false})).unwrap();
    let merged = lcc_raw::decode(&std::fs::read(r["path"].as_str().unwrap()).unwrap()).unwrap();
    assert!(lcc_raw::color::has_matrix(&merged.color), "{:?}", merged.color);
    let got = merged.color.as_shot_neutral.expect("the sources' white balance");
    assert!(got.iter().zip(neutral).all(|(a, b)| (a - b).abs() < 1e-3), "{got:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hdr_merge_refuses_stale_result_before_writing() {
    let dir = temp_dir("hdr-stale");
    let mut paths = Vec::new();
    for (i, b) in lcc_merge::synth::bracket_dngs(240, 160, &[-2.0, 0.0, 2.0]).unwrap().into_iter().enumerate() {
        let p = dir.join(format!("IMG_{i}.dng"));
        std::fs::write(&p, b).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths.clone());
    let first = s.execute("merge.hdr", &json!({"ids": ids, "autoSettings": false})).unwrap();
    let id = lcc_catalog::PhotoId(first["id"].as_u64().unwrap());
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
    for (i, v) in lcc_merge::synth::pano_views(480, 360, 420.0, &[-25.0, 0.0, 25.0]).unwrap().into_iter().enumerate() {
        let img = v.to_srgb8();
        let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
        let p = dir.join(format!("P_{i}.png"));
        std::fs::write(&p, png).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    let r = s.execute("merge.panorama", &json!({"ids": ids, "projection": "cylindrical", "autoCrop": true, "autoSettings": false})).unwrap();
    assert_eq!(r["projection"], "cylindrical");
    assert_eq!(r["used"].as_array().unwrap().len(), 3);
    let id = lcc_catalog::PhotoId(r["id"].as_u64().unwrap());
    let p = s.catalog.photo(id).unwrap().clone();
    assert!(p.width > 800 && p.height > 300, "{}×{}", p.width, p.height);
    let crop = s.develop_of(id).unwrap().crop.geometry.rect;
    assert!(crop.width() < 1.0 && crop.width() * crop.height() > 0.5, "{crop:?}");
    // the photos' own resolution, so nothing held it back (issue #1455)
    let (mp, native) = (r["megapixels"].as_f64().unwrap(), r["nativeMegapixels"].as_f64().unwrap());
    assert!((mp / native - 1.0).abs() < 0.02 && r["limit"].is_null(), "{r}");
    // a size asked for says so
    let small =
        s.execute("merge.panorama", &json!({"ids": ids, "projection": "cylindrical", "autoSettings": false, "maxMegapixels": native / 2.0})).unwrap();
    assert_eq!(small["limit"], "asked", "{small}");
    assert!(small["megapixels"].as_f64().unwrap() <= native / 2.0 * 1.02, "{small}");
    // too few photos
    assert!(s.execute("merge.panorama", &json!({"ids": [ids[0]]})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// How strongly coloured a linear picture is: the mean spread between its channels, per unit of brightness.
fn colourfulness(img: &lcc_raster::Rgb32f) -> f32 {
    let (mut spread, mut level) = (0.0, 0.0);
    for p in img.data.iter().filter(|p| p.iter().any(|v| *v > 0.0)) {
        spread += p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]);
        level += (p[0] + p[1] + p[2]) / 3.0;
    }
    spread / level
}

/// Issue #752: a merge of JPEGs holds their display RGB in the DNG, so the profile of the camera in their metadata
/// (made for that camera's sensor RGB) must not develop it: OM-1 JPEGs came out oversaturated once the OM-1 had one.
#[test]
fn a_merge_of_jpegs_from_a_profiled_camera_keeps_their_colour() {
    crate::camera_profiles::install_dcps();
    let (make, model) = ("OM Digital Solutions", "OM-1");
    assert!(lcc_raw::dcp::resolve("", Some(make), model).is_some(), "the camera needs a profile for this to test anything");
    let meta = lcc_meta::Metadata { make: Some(make.into()), model: Some(model.into()), ..Default::default() };
    let xmp = lcc_meta::write_xmp(&meta, None);
    let dir = temp_dir("jpeg-colour");
    let encode = |v: &lcc_raster::Rgb32f| {
        let img = v.to_srgb8();
        let encoded = lcc_codecs::EncodeMeta { xmp: Some(&xmp), ..Default::default() };
        lcc_codecs::encode_jpeg(&lcc_codecs::EncodeImage::rgba8(&img), 95, lcc_codecs::ChromaSubsampling::S444, &encoded).unwrap()
    };
    let (sharp, views) = lcc_merge::synth::focus_bracket(480, 320, 3).unwrap();
    let mut paths = Vec::new();
    for (i, v) in views.iter().enumerate() {
        let p = dir.join(format!("F_{i}.jpg"));
        std::fs::write(&p, encode(v)).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    let r = s.execute("merge.focusStack", &json!({"ids": ids, "autoSettings": false})).unwrap();
    let dng = std::fs::read(r["path"].as_str().unwrap()).unwrap();
    assert_eq!(lcc_raw::decode(&dng).unwrap().metadata.model.as_deref(), Some(model));
    let (merged, _) = crate::files::load_bytes(&dng, 1024).unwrap();
    let (source, _) = crate::files::load_bytes(&encode(&sharp), 1024).unwrap();
    let (got, want) = (colourfulness(&merged), colourfulness(&source));
    assert!((got / want - 1.0).abs() < 0.05, "merged colourfulness {got}, source {want}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// HDR for Each Bracket Set: two stacks of brackets → two merged DNGs, each next to its first photo; photos that form
/// no set are refused.
#[test]
fn hdr_batch_merges_each_stack() {
    let dir = temp_dir("hdr-batch");
    let mut paths = Vec::new();
    // different exposures, or the import would take set B for duplicates of set A
    for (set, evs) in [("A", [-2.0, 0.0, 2.0]), ("B", [-1.5, 0.5, 2.5])] {
        for (i, b) in lcc_merge::synth::bracket_dngs(240, 160, &evs).unwrap().into_iter().enumerate() {
            let p = dir.join(format!("{set}_{i}.dng"));
            std::fs::write(&p, b).unwrap();
            paths.push(p.to_string_lossy().to_string());
        }
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    assert_eq!(ids.len(), 6);
    let err = s.execute("merge.hdrBatch", &json!({"ids": [ids[0]]})).unwrap_err().to_string();
    assert!(err.contains("no bracket sets"), "{err}");
    s.execute("stack.group", &json!({"ids": ids[..3]})).unwrap();
    s.execute("stack.group", &json!({"ids": ids[3..]})).unwrap();
    let r = s.execute("merge.hdrBatch", &json!({"ids": [ids[0], ids[3]], "autoSettings": false})).unwrap();
    let merged = r["merged"].as_array().unwrap();
    assert_eq!(merged.len(), 2, "{r}");
    assert!(r["failed"].as_array().unwrap().is_empty(), "{r}");
    let names: Vec<&str> = merged.iter().map(|m| m["path"].as_str().unwrap()).collect();
    assert!(names[0].ends_with("A_0-HDR.dng") && names[1].ends_with("B_0-HDR.dng"), "{names:?}");
    assert_eq!(s.catalog.photos().count(), 8);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #1455: a panorama keeps its photos' resolution, held to what fits in memory beside them
/// (never below the 40 MP every panorama used to have), and never above a size asked for.
#[test]
fn panorama_size_is_held_to_memory() {
    use crate::merge::MIN_FITTED_MEGAPIXELS;
    // beside the app's old 1.5 GiB budget, whatever this process's is (it follows the computer and
    // `{ENV_PREFIX}_MEMORY_MB`)
    let fit_in_memory = |o: &lcc_merge::PanoOptions, px: f64, ram: Option<usize>| crate::merge::fit_in_memory_beside(o, px, ram, 3 << 29);
    let full = lcc_merge::PanoOptions::default();
    let gb = |g: usize| Some(g << 30);
    let photos = 5.0 * 51e6; // five 51 MP photos
    let mp = |o: lcc_merge::PanoOptions| o.max_megapixels;
    assert_eq!(mp(fit_in_memory(&full, photos, gb(512))), lcc_merge::pano::MAX_MEGAPIXELS, "plenty of memory: the full size");
    let m16 = mp(fit_in_memory(&full, photos, gb(16)));
    assert!(m16 > MIN_FITTED_MEGAPIXELS && m16 < 512.0, "16 GB: {m16} MP");
    assert!(mp(fit_in_memory(&full, photos, gb(32))) > m16, "more memory, more pixels");
    assert_eq!(mp(fit_in_memory(&full, photos, gb(4))), MIN_FITTED_MEGAPIXELS, "never below 40 MP");
    let asked = lcc_merge::PanoOptions { max_megapixels: 20.0, ..full.clone() };
    assert_eq!(mp(fit_in_memory(&asked, photos, gb(16))), 20.0, "a size asked for is kept when it fits");
    assert_eq!(fit_in_memory(&full, 1e6, None), fit_in_memory(&full, 1e6, gb(8)), "memory unknown: taken as 8 GB");
}

/// A photo named twice is merged once (issue #1947): `[a, a, a]` is one photo, too few to merge, and `[a, b, a, b]`
/// merges two frames rather than holding four in memory.
#[test]
fn a_photo_named_twice_is_merged_once() {
    let dir = temp_dir("dup-ids");
    let mut paths = Vec::new();
    for (i, b) in lcc_merge::synth::bracket_dngs(96, 64, &[-1.0, 1.0]).unwrap().into_iter().enumerate() {
        let p = dir.join(format!("IMG_{i}.dng"));
        std::fs::write(&p, b).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    let (a, b) = (ids[0], ids[1]);
    for cmd in ["merge.hdr", "merge.panorama"] {
        let e = s.execute(cmd, &json!({"ids": [a, a, a], "preview": true})).unwrap_err();
        assert!(e.to_string().contains("at least 2"), "{cmd}: {e}");
    }
    let (kind, finish) = crate::merge::parse("merge.hdr", &json!({})).unwrap();
    let job = s.plan_merge(kind, finish, &[a, b, a, b, a].map(lcc_catalog::PhotoId), true).unwrap();
    assert_eq!(job.sources.iter().map(|(p, _)| p.0).collect::<Vec<_>>(), vec![a, b]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn focus_stack_command_creates_and_imports_a_dng() {
    let dir = temp_dir("focus");
    let mut paths = Vec::new();
    let (_, frames) = lcc_merge::synth::focus_bracket(480, 320, 4).unwrap();
    for (i, v) in frames.into_iter().enumerate() {
        let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&v.to_srgb8()), &lcc_codecs::EncodeMeta::default()).unwrap();
        let p = dir.join(format!("F_{i}.png"));
        std::fs::write(&p, png).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    // preview: nothing added
    let png = dir.join("preview.png");
    let pv = s.execute("merge.focusStack", &json!({"ids": ids, "preview": true, "previewPath": png})).unwrap();
    assert!(png.exists() && pv["previewSize"][0].as_u64().unwrap() > 100, "{pv}");
    assert_eq!(s.catalog.photos().count(), 4);
    // picked in any order, the photos stack in shooting order (capture time, then file name): each is
    // aligned to its neighbour
    let picked = [ids[2], ids[0], ids[3], ids[1]];
    let r = s.execute("merge.focusStack", &json!({"ids": picked, "autoSettings": false, "stack": true})).unwrap();
    assert!(r["path"].as_str().unwrap().ends_with("F_0-Focus.dng"), "{r}");
    // the most magnified (last) frame is the reference
    assert_eq!(r["reference"], ids[3]);
    let models: Vec<&str> = r["alignment"].as_array().unwrap().iter().map(|a| a["model"].as_str().unwrap()).collect();
    assert_eq!(models, ["similarity", "similarity", "similarity", "reference"], "{r}");
    assert_eq!(r["skipped"], json!([]));
    assert!(r["ev"].as_array().unwrap().iter().all(|v| v.as_f64().unwrap() == 0.0), "{r}");
    let id = lcc_catalog::PhotoId(r["id"].as_u64().unwrap());
    let p = s.catalog.photo(id).unwrap().clone();
    assert_eq!((p.width, p.height), (480, 320));
    assert_eq!(s.catalog.stack_of(id).expect("stacked on its sources").top(), id);
    s.execute("library.filter", &json!({"merged": "focusStack"})).unwrap();
    assert_eq!(s.catalog.query(&s.filter, &Default::default()), vec![id]);
    assert!(s.execute("merge.focusStack", &json!({"ids": [ids[0]]})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A preview whose options change reads and decodes its photos once: the second (another deghost amount) takes
/// them from the session's frame cache.
#[test]
fn a_changed_preview_reuses_its_decoded_photos() {
    let dir = temp_dir("preview-cache");
    let mut paths = Vec::new();
    for (i, b) in lcc_merge::synth::bracket_dngs(320, 240, &[-2.0, 0.0, 2.0]).unwrap().into_iter().enumerate() {
        let p = dir.join(format!("IMG_{i}.dng"));
        std::fs::write(&p, b).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let r = reads.clone();
    s.media.file_bytes = Some(std::sync::Arc::new(move |path: &str| {
        r.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::fs::read(path).map_err(|e| e.to_string())
    }));
    let count = || reads.load(std::sync::atomic::Ordering::Relaxed);
    s.execute("merge.hdr", &json!({"ids": ids, "preview": true})).unwrap();
    assert_eq!(count(), 3);
    assert_eq!(s.media.merge_frames.len(), 3);
    s.execute("merge.hdr", &json!({"ids": ids, "preview": true, "deghost": "high"})).unwrap();
    assert_eq!(count(), 3, "the second preview read its photos again");
    // the full-size merge doesn't use (or fill) the preview cache
    s.execute("merge.hdr", &json!({"ids": ids, "autoSettings": false})).unwrap();
    assert_eq!(count(), 6);
    assert_eq!(s.media.merge_frames.len(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merge_memory_estimate_is_checked_and_uses_a_controlled_budget() {
    use crate::merge::{MergeKind, check_merge_memory_budget, estimate_merge_working_bytes, merge_memory_bytes};

    let kind = MergeKind::Hdr { hdr: Default::default() };
    let small = estimate_merge_working_bytes(&kind, &[(100, 100), (100, 100)], None).unwrap();
    let larger = estimate_merge_working_bytes(&kind, &[(200, 100), (200, 100)], None).unwrap();
    assert!(larger > small, "larger inputs must estimate more working memory: {small} → {larger}");

    let required = estimate_merge_working_bytes(&kind, &[(5_000, 5_000), (5_000, 5_000)], None).unwrap();
    let error = check_merge_memory_budget(required, 256 << 20).unwrap_err();
    assert!(error.contains("merge memory budget") && error.contains("lower-resolution"), "{error}");
    assert_eq!(merge_memory_bytes(Some(8 << 30), 1 << 30), 5 << 30, "75% of RAM less the cache budget");

    let kinds = [
        kind.clone(),
        MergeKind::Panorama { pano: Default::default() },
        MergeKind::HdrPanorama { hdr: Default::default(), pano: Default::default(), bracket: 3 },
    ];
    for kind in kinds {
        let needed = estimate_merge_working_bytes(&kind, &[(5_000, 5_000), (5_000, 5_000), (5_000, 5_000), (5_000, 5_000)], None).unwrap();
        assert!(check_merge_memory_budget(needed, 256 << 20).is_err(), "{kind:?} bypassed the controlled budget");
    }

    assert!(estimate_merge_working_bytes(&kind, &[(0, 100), (100, 100)], None).is_err(), "zero dimensions must be refused");
    assert!(estimate_merge_working_bytes(&kind, &[(u32::MAX, u32::MAX), (1, 1)], None).is_err(), "overflow must be refused");
}

/// Merge to Layers (#769): `layers: true` also writes a layered PSD of the aligned frames, named with their exposure
/// steps, with the merged result at the bottom when asked, added to the library and stacked under the DNG.
#[test]
fn hdr_merge_writes_aligned_layers() {
    let dir = temp_dir("hdr-layers");
    let mut paths = Vec::new();
    for (i, b) in lcc_merge::synth::bracket_dngs(240, 160, &[-2.0, 0.0, 2.0]).unwrap().into_iter().enumerate() {
        let p = dir.join(format!("IMG_{i}.dng"));
        std::fs::write(&p, b).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    let r = s
        .execute("merge.hdr", &json!({"ids": ids, "autoSettings": false, "stack": true, "layers": true, "includeMerged": true, "bitDepth": 8}))
        .unwrap();
    let lpath = r["layers"]["path"].as_str().expect("layers written").to_string();
    assert!(lpath.ends_with("IMG_0-HDR-Layers.psd"), "{lpath}");
    let f = lcc_psd::PsdFile::from_bytes(&std::fs::read(&lpath).unwrap()).unwrap();
    assert_eq!((f.header.width, f.header.height, f.header.depth), (240, 160, 8));
    let names: Vec<String> = (0..4).map(|i| f.layer(i).unwrap().name().to_string()).collect();
    assert_eq!(names[0], "Merged", "{names:?}");
    // the first source on top, each named with its exposure step
    assert!(names[3].starts_with("IMG_0 (EV \u{2212}2"), "{names:?}");
    assert!(names[2].starts_with("IMG_1 (EV 0"), "{names:?}");
    // imported and stacked under the merged DNG with the sources
    let dng = lcc_catalog::PhotoId(r["id"].as_u64().unwrap());
    let lid = lcc_catalog::PhotoId(r["layers"]["id"].as_u64().unwrap());
    let st = s.catalog.stack_of(dng).expect("stacked").clone();
    assert_eq!(st.top(), dng);
    assert!(st.photos.contains(&lid) && ids.iter().all(|i| st.photos.contains(&lcc_catalog::PhotoId(*i))));
    // a preview never writes layers; non-HDR merges refuse them
    s.execute("merge.hdr", &json!({"ids": ids, "preview": true, "layers": true})).unwrap();
    assert!(!dir.join("IMG_0-HDR-Layers-2.psd").exists());
    assert!(s.execute("merge.panorama", &json!({"ids": ids, "layers": true})).is_err());
    assert!(s.execute("merge.hdr", &json!({"ids": ids, "layers": true, "layersFormat": "tiff"})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
