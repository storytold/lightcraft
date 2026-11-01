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
    // the second merge doesn't overwrite the first
    let r2 = s.execute("merge.hdr", &json!({"ids": ids, "autoSettings": false})).unwrap();
    assert!(r2["path"].as_str().unwrap().ends_with("IMG_0-HDR-2.dng"));
    let id2 = lightcraft_catalog::PhotoId(r2["id"].as_u64().unwrap());
    assert!(s.catalog.stack_of(id2).is_none(), "no stack without the option");
    // the type filter finds merge results (and only them)
    s.execute("library.filter", &json!({"merged": "hdr"})).unwrap();
    let found: std::collections::HashSet<_> = s.catalog.query(&s.filter, &Default::default()).into_iter().collect();
    assert_eq!(found, [id, id2].into_iter().collect());
    let _ = std::fs::remove_dir_all(&dir);
}

/// An HDR merge keeps the reference frame's lens corrections. The sources carry an off-centre `WarpRectilinear` and
/// `FixVignetteRadial` for their active area and have a default crop; the merged DNG must give "Enable Profile
/// Corrections" the same correction as the reference photo, upright and for a vertical bracket (whose DNG keeps the
/// pixels unturned).
#[test]
fn hdr_merge_keeps_the_reference_frames_lens_corrections() {
    use lightcraft_geom::{Orientation, Point};
    use lightcraft_pipeline::optics::{embedded_vignette_gain, embedded_warp};
    use lightcraft_raw::Opcode;
    for orientation in [Orientation::Normal, Orientation::Rotate90] {
        let dir = temp_dir(&format!("hdr-lens-{orientation:?}"));
        let mut paths = Vec::new();
        for (i, b) in lightcraft_merge::synth::bracket_dngs(480, 320, &[-2.0, 0.0, 2.0]).unwrap().into_iter().enumerate() {
            let mut raw = lightcraft_raw::decode(&b).unwrap();
            raw.crop = lightcraft_raw::Rect::new(10, 6, 452, 304);
            raw.orientation = orientation;
            raw.metadata.orientation = Some(orientation);
            raw.opcodes.list3 = vec![
                Opcode::WarpRectilinear { planes: vec![[0.96, 0.05, -0.012, 0.002, 0.001, -0.0005]], center: [0.48, 0.53] },
                Opcode::FixVignetteRadial { k: [0.35, -0.05, 0.01, 0.0, 0.0], center: [0.48, 0.53] },
            ];
            let p = dir.join(format!("IMG_{i}.dng"));
            std::fs::write(&p, lightcraft_raw::write_dng(&raw, &Default::default()).unwrap()).unwrap();
            paths.push(p.to_string_lossy().to_string());
        }
        let mut s = Session::new().with_fs();
        let ids = import(&mut s, paths);
        let r = s.execute("merge.hdr", &json!({"ids": ids})).unwrap();
        assert_eq!(r["reference"].as_u64(), Some(ids[1]), "{r}");
        let merged = s.catalog.photo(lightcraft_catalog::PhotoId(r["id"].as_u64().unwrap())).unwrap().clone();
        let reference = s.catalog.photo(lightcraft_catalog::PhotoId(ids[1])).unwrap().clone();
        let expect = if orientation == Orientation::Normal { (452, 304) } else { (304, 452) };
        assert_eq!(((merged.width, merged.height), (reference.width, reference.height)), (expect, expect));
        let a = reference.embedded_lens.expect("the source's correction");
        let b = merged.embedded_lens.expect("the merged DNG carries the reference frame's correction");
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
    for (i, b) in lightcraft_merge::synth::bracket_dngs(240, 160, &[-1.0, 0.0, 1.0]).unwrap().into_iter().enumerate() {
        // the same pixels with a white balance but no colour matrices
        let mut raw = lightcraft_raw::decode(&b).unwrap();
        raw.color = lightcraft_raw::ColorData { as_shot_neutral: Some(neutral), ..Default::default() };
        let p = dir.join(format!("IMG_{i}.dng"));
        std::fs::write(&p, lightcraft_raw::write_dng(&raw, &Default::default()).unwrap()).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    let r = s.execute("merge.hdr", &json!({"ids": ids, "autoSettings": false})).unwrap();
    let merged = lightcraft_raw::decode(&std::fs::read(r["path"].as_str().unwrap()).unwrap()).unwrap();
    assert!(lightcraft_raw::color::has_matrix(&merged.color), "{:?}", merged.color);
    let got = merged.color.as_shot_neutral.expect("the sources' white balance");
    assert!(got.iter().zip(neutral).all(|(a, b)| (a - b).abs() < 1e-3), "{got:?}");
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

/// HDR for Each Bracket Set: two stacks of brackets → two merged DNGs, each next to its first photo; photos that form
/// no set are refused.
#[test]
fn hdr_batch_merges_each_stack() {
    let dir = temp_dir("hdr-batch");
    let mut paths = Vec::new();
    // different exposures, or the import would take set B for duplicates of set A
    for (set, evs) in [("A", [-2.0, 0.0, 2.0]), ("B", [-1.5, 0.5, 2.5])] {
        for (i, b) in lightcraft_merge::synth::bracket_dngs(240, 160, &evs).unwrap().into_iter().enumerate() {
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
    use crate::merge::{MIN_FITTED_MEGAPIXELS, fit_in_memory};
    let full = lightcraft_merge::PanoOptions::default();
    let gb = |g: usize| Some(g << 30);
    let photos = 5.0 * 51e6; // five 51 MP photos
    let mp = |o: lightcraft_merge::PanoOptions| o.max_megapixels;
    assert_eq!(mp(fit_in_memory(&full, photos, gb(512))), lightcraft_merge::pano::MAX_MEGAPIXELS, "plenty of memory: the full size");
    let m16 = mp(fit_in_memory(&full, photos, gb(16)));
    assert!(m16 > MIN_FITTED_MEGAPIXELS && m16 < 512.0, "16 GB: {m16} MP");
    assert!(mp(fit_in_memory(&full, photos, gb(32))) > m16, "more memory, more pixels");
    assert_eq!(mp(fit_in_memory(&full, photos, gb(4))), MIN_FITTED_MEGAPIXELS, "never below 40 MP");
    let asked = lightcraft_merge::PanoOptions { max_megapixels: 20.0, ..full.clone() };
    assert_eq!(mp(fit_in_memory(&asked, photos, gb(16))), 20.0, "a size asked for is kept when it fits");
    assert_eq!(fit_in_memory(&full, 1e6, None), fit_in_memory(&full, 1e6, gb(8)), "memory unknown: taken as 8 GB");
}

/// A photo named twice is merged once (issue #1947): `[a, a, a]` is one photo, too few to merge, and `[a, b, a, b]`
/// merges two frames rather than holding four in memory.
#[test]
fn a_photo_named_twice_is_merged_once() {
    let dir = temp_dir("dup-ids");
    let mut paths = Vec::new();
    for (i, b) in lightcraft_merge::synth::bracket_dngs(96, 64, &[-1.0, 1.0]).unwrap().into_iter().enumerate() {
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
    let job = s.plan_merge(kind, finish, &[a, b, a, b, a].map(lightcraft_catalog::PhotoId), true).unwrap();
    assert_eq!(job.sources.iter().map(|(p, _)| p.0).collect::<Vec<_>>(), vec![a, b]);
    let _ = std::fs::remove_dir_all(&dir);
}
