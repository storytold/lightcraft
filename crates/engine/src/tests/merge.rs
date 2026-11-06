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

/// Issue #1455: a panorama keeps its photos' resolution, held to what fits in the merge's memory
/// beside them (down to its floor), and never above a size asked for.
#[test]
fn panorama_size_is_held_to_memory() {
    use crate::merge::fit_in_memory;
    let full = lightcraft_merge::PanoOptions::default();
    let gb = |g: f64| g * (1u64 << 30) as f64;
    let photos = 5.0 * 51e6; // five 51 MP photos
    let mp = |o: std::result::Result<lightcraft_merge::PanoOptions, String>| o.unwrap().max_megapixels;
    assert_eq!(mp(fit_in_memory(&full, photos, gb(512.0))), lightcraft_merge::pano::MAX_MEGAPIXELS, "plenty of memory: the full size");
    let m10 = mp(fit_in_memory(&full, photos, gb(10.5)));
    assert!(m10 > 40.0 && m10 < 512.0, "10.5 GB: {m10} MP");
    assert!(mp(fit_in_memory(&full, photos, gb(22.5))) > m10, "more memory, more pixels");
    let asked = lightcraft_merge::PanoOptions { max_megapixels: 20.0, ..full.clone() };
    assert_eq!(mp(fit_in_memory(&asked, photos, gb(10.5))), 20.0, "a size asked for is kept when it fits");
}

/// Issue #3708: a panorama is made smaller to fit its memory down to its floor (40 MP, or a
/// smaller size asked for, or its photos' own resolution when that is less), and says memory held
/// it; a merge whose memory can't hold the floor is refused, naming the floor and the memory it
/// needs and has, rather than made quietly far below it.
#[test]
fn a_panorama_is_made_no_smaller_than_its_floor() {
    use crate::merge::{MIN_PANORAMA_MEGAPIXELS, fit_in_memory, panorama_floor, size_limit};
    assert_eq!(MIN_PANORAMA_MEGAPIXELS, 40.0);
    let full = lightcraft_merge::PanoOptions::default();
    let photos = 5.0 * 12e6; // five 12 MP photos: the blend (68 B/px) decides
    let fit = |pano: &lightcraft_merge::PanoOptions, photos: f64, room: f64| fit_in_memory(pano, photos, room).map(|o| o.max_megapixels);
    let mp = |room: f64| fit(&full, photos, room);
    // the floor and a little more fit: the size memory allows, between the floor and the full size
    assert_eq!(mp(40e6 * 68.0), Ok(40.0));
    assert_eq!(mp(50e6 * 68.0), Ok(50.0));
    assert_eq!(size_limit(lightcraft_merge::pano::MAX_MEGAPIXELS, 50.0, 55.0, 50.0), Some("memory"), "the result says memory held it");
    // 2 GB (a 4 GB computer, or a small memory limit): about 29 MP, under the floor: refused
    let e = mp(2e9).unwrap_err();
    assert!(e.starts_with("not enough memory for this panorama: at 40 MP, the smallest it is made,"), "{e}");
    assert!(e.contains("needs about 2594 MB") && e.contains("the merge's budget is 1907 MB (three quarters of this computer's memory"), "{e}");
    assert!(
        e.ends_with("merge fewer or smaller photos, or set maxMegapixels to 29.4 or less (command line or MCP)"),
        "the largest size that fits: {e}"
    );
    assert!(!e.contains("close other apps"), "the budget doesn't grow when other apps close: {e}");
    let asked = lightcraft_merge::PanoOptions { max_megapixels: 29.4, ..full.clone() };
    assert_eq!(fit(&asked, photos, 2e9), Ok(29.4), "the size the refusal names fits");
    // three 1 MP photos: their own resolution is the floor
    assert_eq!(panorama_floor(full.max_megapixels, 3e6), 3.0);
    assert_eq!(fit(&full, 3e6, 3.5e6 * 68.0), Ok(3.5));
    let e = fit(&full, 3e6, 2e6 * 68.0).unwrap_err();
    assert!(e.contains("at 3 MP") && e.contains("budget is 129 MB"), "{e}");
    assert!(e.ends_with("; merge fewer or smaller photos"), "a floor of the photos' own pixels: no smaller maxMegapixels to name: {e}");
    // a size asked for below 40 MP is the floor: room for 0.6 MP beside the photos holds 0.5 MP
    let room = photos * 12.0 + 0.6e6 * 36.0;
    let asked = lightcraft_merge::PanoOptions { max_megapixels: 0.5, ..full.clone() };
    assert_eq!(fit(&asked, photos, room), Ok(0.5));
    assert!(fit(&lightcraft_merge::PanoOptions { max_megapixels: 0.7, ..full.clone() }, photos, room).unwrap_err().contains("at 0.7 MP"));
    assert!(mp(room).unwrap_err().contains("at 40 MP"), "the full size: refused");
    assert_eq!(panorama_floor(f64::NAN, 1e9), 40.0, "a NaN size asked for is ignored");
    assert_eq!(panorama_floor(0.01, 1e9), 0.1, "a size asked for under the stitch's least cap is held to it");
    for room in [0.0, -1.0, f64::NAN] {
        let e = mp(room).unwrap_err();
        assert!(e.contains("budget is 0 MB") && e.ends_with("; merge fewer or smaller photos"), "{room}: {e}");
    }
}

/// Photos with fewer pixels than the stitch's least cap (0.1 MP) make a panorama of their own
/// size, never larger (the stitch doesn't scale them up): that is its floor, so a memory that holds
/// it isn't refused for a 0.1 MP panorama the photos can't make (#3708).
#[test]
fn a_panorama_of_photos_under_the_least_cap_is_floored_at_their_size() {
    use crate::merge::{fit_in_memory, panorama_floor};
    let full = lightcraft_merge::PanoOptions::default();
    let photos = 5e4; // two 250×100 photos, 0.05 MP
    assert_eq!(panorama_floor(full.max_megapixels, photos), 0.05);
    assert_eq!(panorama_floor(0.1, photos), 0.05, "a 0.1 MP cap too");
    // 5 MB holds a 0.07 MP blend (68 B/px) but not 0.1 MP: made at what fits, not refused
    let fit = fit_in_memory(&full, photos, 5e6).map(|o| o.max_megapixels);
    assert!(fit.as_ref().is_ok_and(|mp| (*mp - 5e6 / 68.0 / 1e6).abs() < 1e-9), "{fit:?}");
    // 3 MB holds less than the photos: refused, naming their size as the smallest it is made
    let e = fit_in_memory(&full, photos, 3e6).unwrap_err();
    assert!(e.contains("at 0.05 MP, the smallest it is made") && e.contains("needs about 4 MB"), "{e}");
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

/// `bytes` of memory the merge may use, this computer's memory known.
fn known(bytes: f64) -> crate::merge::Memory {
    crate::merge::Memory { bytes, known: true }
}

/// `n` 24 MP photos (6000 × 4000, 30 MB files), as the catalog knows them.
fn photos_24mp(n: usize) -> Vec<crate::merge::PhotoSize> {
    vec![crate::merge::PhotoSize::new(6000, 4000, 30_000_000).unwrap(); n]
}

fn panorama() -> crate::merge::MergeKind {
    crate::merge::MergeKind::Panorama { pano: lightcraft_merge::PanoOptions::default() }
}

fn hdr() -> crate::merge::MergeKind {
    crate::merge::MergeKind::Hdr { hdr: lightcraft_merge::HdrOptions::default() }
}

fn hdr_panorama(bracket: usize) -> crate::merge::MergeKind {
    crate::merge::MergeKind::HdrPanorama { hdr: lightcraft_merge::HdrOptions::default(), pano: lightcraft_merge::PanoOptions::default(), bracket }
}

/// Issue #3708: a panorama is planned before its photos are decoded. Five 24 MP photos decoded one
/// at a time peak at ~2.05 GB (four frames held, 12 bytes a pixel, beside the fifth decoding: 36
/// bytes a pixel and its file), and their stitch at its 40 MP floor needs 2.72 GB (68 bytes a
/// pixel), or their 120 MP of frames and 36 bytes a pixel of it (2.88 GB): with 2.9 GB they run at
/// full size; with 2.8 GB they are decoded a little smaller, so the stitch still reaches its floor;
/// with 2.0 GB or 400 MB not even the floor fits, and the merge is refused with the memory it needs
/// and has, rather than made smaller than its floor.
#[test]
fn a_panorama_is_planned_before_its_photos_are_decoded() {
    use crate::merge::plan_memory;
    let photos = photos_24mp(5);
    assert_eq!(plan_memory(&panorama(), &photos, 2.9e9), Ok(None), "fits: full size, as before");
    assert_eq!(plan_memory(&panorama(), &photos, 1e12), Ok(None));
    let edge = plan_memory(&panorama(), &photos, 2.8e9).unwrap().expect("made smaller");
    // ~110 MP of frames leave room for the stitch as large as memory allows (41 MP): 6000 × √(110 / 120)
    assert!((5600..6000).contains(&edge), "{edge}");
    for room in [2.0e9, 0.4e9] {
        let e = plan_memory(&panorama(), &photos, room).unwrap_err();
        assert!(e.starts_with("not enough memory for this panorama"), "{e}");
        // made no smaller than keeps 80 MP of frames for a 40 MP panorama: 6000 × √(80 / 120)
        assert!(e.contains("made 4899 px on the long side and the panorama 40 MP, the smallest it is made,"), "{e}");
        assert!(e.contains("needs about 2594 MB"), "{e}");
    }
    // photos no larger than the smallest frames can't be made smaller: refused at the stitch's
    // floor, their own 2.4 MP
    let small = vec![crate::merge::PhotoSize::new(1000, 800, 1_000_000).unwrap(); 3];
    assert_eq!(plan_memory(&panorama(), &small, 1e9), Ok(None));
    let e = plan_memory(&panorama(), &small, 20e6).unwrap_err();
    assert!(
        e.contains("even with the panorama 2.4 MP, the smallest it is made,")
            && e.contains("needs about 156 MB")
            && e.contains("the merge's budget is 19 MB"),
        "{e}"
    );
    for room in [0.0, -1.0, f64::NAN] {
        assert!(plan_memory(&panorama(), &photos, room).unwrap_err().contains("budget is 0 MB"), "{room}");
    }
}

/// Photos under the stitch's least cap (two of 250 × 100 px, 0.05 MP) are planned against their own
/// size as the floor, which they can make, not 0.1 MP, which they can't: 5 MB holds their stitch
/// (68 bytes a pixel, 3.4 MB) but not a 0.1 MP one (6.8 MB), and 3 MB is refused naming 0.05 MP.
#[test]
fn photos_under_the_least_cap_are_planned_at_their_own_size() {
    use crate::merge::plan_memory;
    let tiny = vec![crate::merge::PhotoSize::new(250, 100, 20_000).unwrap(); 2];
    assert_eq!(plan_memory(&panorama(), &tiny, 5e6), Ok(None));
    let e = plan_memory(&panorama(), &tiny, 3e6).unwrap_err();
    assert!(e.contains("the panorama 0.05 MP, the smallest it is made,") && e.contains("needs about 4 MB"), "{e}");
}

/// A panorama's photos are decoded smaller only while its stitch still reaches its floor, which
/// follows the photos' own size (smaller frames don't lower it); one that would have to go below it
/// is refused before any photo is read.
#[test]
fn a_panorama_is_decoded_smaller_only_while_it_reaches_its_floor() {
    use crate::merge::{fit_with_floor, plan_memory};
    use std::sync::atomic::Ordering;
    let photos = photos_24mp(5);
    let pano = lightcraft_merge::PanoOptions::default();
    let edge = plan_memory(&panorama(), &photos, 2.8e9).unwrap().expect("made smaller");
    let frames = 5.0 * photos[0].decoded(Some(edge));
    assert!(fit_with_floor(&pano, frames, 40.0, 2.8e9).unwrap().max_megapixels >= 40.0, "the stitch reaches its floor");
    // smaller frames could hold a smaller stitch, but not one of 40 MP: refused, nothing read
    let (job, reads) = counted_job(panorama(), photos.into_iter().map(Some).collect(), None, 2.0e9);
    let e = job.run(&|_, _| true).err().unwrap_or_default();
    assert!(e.starts_with("not enough memory for this panorama") && e.contains("40 MP, the smallest it is made"), "{e}");
    assert_eq!(reads.load(Ordering::SeqCst), 0, "no photo read");
}

/// An HDR is planned before its photos are decoded too, and keeps their resolution. Three 24 MP
/// photos peak in the merge (three frames, 5 more bytes a pixel of each, a frame of radiance:
/// ~1.51 GB): they run with 1.6 GB. Deghosting adds its 2048 px maps (~0.25 GB): refused with
/// 1.6 GB, runs with 1.8 GB.
#[test]
fn an_hdr_is_planned_before_its_photos_are_decoded_and_never_made_smaller() {
    use crate::merge::plan_memory;
    let photos = photos_24mp(3);
    assert_eq!(plan_memory(&hdr(), &photos, 1.6e9), Ok(None));
    let e = plan_memory(&hdr(), &photos, 1.4e9).unwrap_err();
    assert!(e.starts_with("not enough memory for this HDR merge: with its 3 photos at full size,"), "{e}");
    assert!(e.contains("needs about 1442 MB") && e.contains("budget is 1335 MB"), "{e}");
    let deghost =
        crate::merge::MergeKind::Hdr { hdr: lightcraft_merge::HdrOptions { deghost: lightcraft_merge::Deghost::High, ..Default::default() } };
    assert!(plan_memory(&deghost, &photos, 1.6e9).is_err());
    assert_eq!(plan_memory(&deghost, &photos, 1.8e9), Ok(None));
}

/// An HDR panorama holds every bracket's photos before it merges them: twelve 24 MP photos peak at
/// ~4.1 GB merging the first bracket of three beside the others' photos. With 4.0 GB they are
/// decoded smaller (each bracket's merge is a stitch frame, and the four keep enough pixels for
/// the stitch to reach its 40 MP floor); with 2.0 GB the floor doesn't fit and the merge is
/// refused. A bracket left to EXIF is planned as every size it could be (2, 3, 4 or 6 here): in
/// brackets of six they make two 24 MP frames, which can't be made smaller and still reach the
/// floor, so with 4.0 GB that merge is refused.
#[test]
fn an_hdr_panorama_is_planned_before_its_photos_are_decoded() {
    use crate::merge::plan_memory;
    let photos = photos_24mp(12);
    for bracket in [0, 2, 3] {
        assert_eq!(plan_memory(&hdr_panorama(bracket), &photos, 1e12), Ok(None), "{bracket}");
        let e = plan_memory(&hdr_panorama(bracket), &photos, 2.0e9).unwrap_err();
        assert!(
            e.starts_with("not enough memory for this HDR panorama") && e.contains("the panorama 40 MP") && e.contains("budget is 1907 MB"),
            "{bracket}: {e}"
        );
    }
    assert_eq!(plan_memory(&hdr_panorama(3), &photos, 4.2e9), Ok(None));
    // pairs make more stitch frames than threes: the frames are made no larger
    let pairs = plan_memory(&hdr_panorama(2), &photos, 4.0e9).unwrap().expect("made smaller");
    let threes = plan_memory(&hdr_panorama(3), &photos, 4.0e9).unwrap().expect("made smaller");
    assert!((1024..6000).contains(&threes) && pairs <= threes, "{pairs} {threes}");
    let e = plan_memory(&hdr_panorama(0), &photos, 4.0e9).unwrap_err();
    assert!(e.contains("even with the panorama 40 MP, the smallest it is made,"), "{e}");
}

/// A preview's photos are decoded at full size before they are made small: several at once when
/// that fits, one at a time when only that does, and refused when not even one decode fits.
#[test]
fn a_merge_preview_is_planned_before_its_photos_are_decoded() {
    use crate::merge::plan_preview;
    // each decode: 24 MP × 48 bytes and its file (1.18 GB), beside six 1024 × 683 frames
    let photos = photos_24mp(6);
    assert_eq!(plan_preview(&panorama(), &photos, 5e9, 4), Ok(true));
    assert_eq!(plan_preview(&panorama(), &photos, 2e9, 4), Ok(false), "one at a time");
    assert_eq!(plan_preview(&panorama(), &photos, 2e9, 1), Ok(true), "one thread is one at a time");
    let e = plan_preview(&panorama(), &photos, 1e9, 4).unwrap_err();
    assert!(e.starts_with("not enough memory for this merge preview: even decoding one photo at a time,") && e.contains("budget is 953 MB"), "{e}");
}

/// A preview is planned through the merge after its decode too: twenty 1024 px photos decode one
/// at a time in ~290 MB, but their HDR merge (saturation maps beside every frame) needs ~420 MB.
#[test]
fn a_merge_preview_is_planned_through_the_merge_after_its_decode() {
    use crate::merge::plan_preview;
    let photos = vec![crate::merge::PhotoSize::new(1024, 1024, 1_000_000).unwrap(); 20];
    let e = plan_preview(&hdr(), &photos, 3.5e8, 4).unwrap_err();
    assert!(e.starts_with("not enough memory for this merge preview: even decoding one photo at a time,") && e.contains("needs about 400 MB"), "{e}");
    // their 21 MP panorama would need 1.4 GB at its floor; its preview is made down to 1 MP
    assert_eq!(plan_preview(&panorama(), &photos, 3.5e8, 1), Ok(true), "a panorama of them stitches in less");
    assert!(crate::merge::plan_memory(&panorama(), &photos, 3.5e8).is_err(), "the panorama itself is refused");
    assert_eq!(plan_preview(&hdr(), &photos, 5e8, 4), Ok(true));
}

/// A raw's sensor can hold far more samples than its picture has pixels (masked borders, a small
/// default crop): the catalog keeps only the picture's size, so the header read just before the
/// decode is what the plan is checked with.
#[test]
fn a_raw_sensor_larger_than_its_picture_is_planned_with_its_samples() {
    use crate::merge::{PhotoSize, holds, plan_memory};
    let catalog = vec![PhotoSize::new(1000, 1000, 200_000_000).unwrap(); 2];
    let header = vec![PhotoSize { samples: 100_000_000, ..catalog[0] }; 2];
    assert_eq!(plan_memory(&hdr(), &catalog, 4e8), Ok(None));
    assert_eq!(holds(&hdr(), &catalog, 4e8, None, None), Ok(()));
    // 100 M samples × 8 bytes beside the 200 MB file
    let e = holds(&hdr(), &header, 4e8, None, None).unwrap_err();
    assert!(e.starts_with("not enough memory for this HDR merge: with its 2 photos at full size,") && e.contains("needs about 977 MB"), "{e}");
}

/// A photo replaced by one of the same pixels and file size but another shape is held to a larger
/// frame at the planned edge (8000 × 3000 at 1024 px is 1024 × 384; 5000 × 4800 is 1024 × 983):
/// the check before its decode sees it, as it sees a larger one.
#[test]
fn a_photo_of_another_shape_is_checked_at_the_planned_edge() {
    use crate::merge::{PhotoSize, holds, plan_memory};
    let wide = vec![PhotoSize::new(8000, 3000, 24_000_000).unwrap(); 200];
    let edge = plan_memory(&panorama(), &wide, 3e9).unwrap().expect("made smaller");
    assert!((1024..8000).contains(&edge), "{edge}");
    assert_eq!(holds(&panorama(), &wide, 3e9, Some(edge), None), Ok(()));
    let squarer = vec![PhotoSize::new(5000, 4800, 24_000_000).unwrap(); 200];
    let e = holds(&panorama(), &squarer, 3e9, Some(edge), None).unwrap_err();
    assert!(e.starts_with(&format!("not enough memory for this panorama: with its photos made {edge} px on the long side,")), "{e}");
}

/// A photo of unknown size among known ones is planned as the largest of them (a merge's photos
/// come from one camera, as a rule); with none known, the merge is unplanned.
#[test]
fn photos_of_unknown_size_are_planned_as_the_largest_known() {
    use crate::merge::{PhotoSize, fill_unknown};
    let (a, b) = (PhotoSize::new(4000, 3000, 9).unwrap(), PhotoSize::new(6000, 4000, 5).unwrap());
    assert_eq!(fill_unknown(&[Some(a), None, Some(b)]), Some(vec![a, PhotoSize { bytes: 9, ..b }, b]));
    assert_eq!(fill_unknown(&[None, None]), None);
    assert_eq!(PhotoSize::new(1, 1, 10), None, "a size of a pixel is unknown");
    assert_eq!(PhotoSize::new(10, 10, 0), None, "so is one without its file size");
}

/// A merge job whose photos are `sizes` (the catalog's), with `room` bytes, reading through a
/// counter: a merge the plan refuses reads (so decodes) no photo.
fn counted_job(
    kind: crate::merge::MergeKind,
    sizes: Vec<Option<crate::merge::PhotoSize>>,
    preview: Option<usize>,
    room: f64,
) -> (crate::merge::MergeJob, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = reads.clone();
    let read: crate::merge::ByteReader = Arc::new(move |path: &str| {
        counter.fetch_add(1, Ordering::SeqCst);
        Err(format!("{path}: read"))
    });
    let sources = (0..sizes.len()).map(|i| (lightcraft_catalog::PhotoId(i as u64 + 1), format!("/photos/{i}.ARW"))).collect();
    let job = crate::merge::MergeJob { kind, finish: Default::default(), sources, preview, read, sizes, memory: Some(known(room)) };
    (job, reads)
}

/// No photo is read, let alone decoded, when the plan refuses the merge or its preview (issue
/// #3708: the decode ran first and could run out of memory before the merge was sized).
#[test]
fn a_refused_merge_decodes_nothing() {
    use std::sync::atomic::Ordering;
    let known = || photos_24mp(6).into_iter().map(Some).collect::<Vec<_>>();
    for kind in [panorama(), hdr(), hdr_panorama(3)] {
        for preview in [None, Some(crate::merge::PREVIEW_EDGE)] {
            let (job, reads) = counted_job(kind.clone(), known(), preview, 0.3e9);
            let e = job.run(&|_, _| true).err().unwrap_or_default();
            assert!(e.starts_with("not enough memory"), "{kind:?} {preview:?}: {e}");
            assert_eq!(reads.load(Ordering::SeqCst), 0, "{kind:?} {preview:?}: no photo read");
        }
        // with the memory, the photos are read (and here can't be)
        let (job, reads) = counted_job(kind.clone(), known(), None, 1e12);
        assert_eq!(job.run(&|_, _| true).err().as_deref(), Some("/photos/0.ARW: read"), "{kind:?}");
        assert_eq!(reads.load(Ordering::SeqCst), 1, "{kind:?}");
    }
}

/// What the options alone rule out is refused before any photo is read: brackets of 4 that don't
/// divide six photos into positions, and (a `MergeKind` built directly, not through `parse`) a
/// panorama cap below 0.1 MP, the smallest the stitch keeps.
#[test]
fn options_that_cant_work_are_refused_before_any_photo_is_read() {
    use std::sync::atomic::Ordering;
    let known = || photos_24mp(6).into_iter().map(Some).collect::<Vec<_>>();
    let (job, reads) = counted_job(hdr_panorama(4), known(), None, 1e12);
    let e = job.run(&|_, _| true).err().unwrap_or_default();
    assert!(e.starts_with("6 photos can't be split into brackets of 4: an HDR panorama needs two or more brackets of the same size, so a multiple of 4 photos, at least 8"), "{e}");
    assert_eq!(reads.load(Ordering::SeqCst), 0, "no photo read");
    let tiny = lightcraft_merge::PanoOptions { max_megapixels: 0.05, ..Default::default() };
    for kind in [
        crate::merge::MergeKind::Panorama { pano: tiny.clone() },
        crate::merge::MergeKind::HdrPanorama { hdr: lightcraft_merge::HdrOptions::default(), pano: tiny.clone(), bracket: 3 },
    ] {
        let (job, reads) = counted_job(kind.clone(), known(), None, 1e12);
        let e = job.run(&|_, _| true).err().unwrap_or_default();
        assert!(e.contains("below 0.1 MP, the smallest size limit the stitch keeps"), "{kind:?}: {e}");
        assert_eq!(reads.load(Ordering::SeqCst), 0, "{kind:?}: no photo read");
    }
    // a bracket left to EXIF: five photos can't be divided into two positions by any bracket
    let five = photos_24mp(5).into_iter().map(Some).collect::<Vec<_>>();
    let (job, reads) = counted_job(hdr_panorama(0), five, None, 1e12);
    let e = job.run(&|_, _| true).err().unwrap_or_default();
    assert!(e.starts_with("5 photos can't be split into two or more brackets of the same size"), "{e}");
    assert_eq!(reads.load(Ordering::SeqCst), 0, "no photo read");
    // brackets of 3 divide six photos into two positions: on to reading them (which can't here)
    let (job, _) = counted_job(hdr_panorama(3), known(), None, 1e12);
    assert_eq!(job.run(&|_, _| true).err().as_deref(), Some("/photos/0.ARW: read"));
}

/// Cancelled while its photos are read, a merge reads no more of them.
#[test]
fn a_merge_cancelled_while_reading_reads_no_more_photos() {
    use std::sync::atomic::Ordering;
    let (dir, job) = bracketed_dngs("cancel-reading");
    let (job, reads) = counted(&job, job.sizes.clone(), 1e12, false);
    let e = job.run(&|f, s| !(s == "Reading photos" && f > 0.0)).err();
    assert_eq!(e.as_deref(), Some("cancelled"));
    assert_eq!(reads.load(Ordering::SeqCst), 1, "only the photo being read when it was cancelled");
    let _ = std::fs::remove_dir_all(dir);
}

/// Three 480 × 320 bracketed DNGs in a library, and a merge of them planned by the session.
fn bracketed_dngs(tag: &str) -> (std::path::PathBuf, crate::merge::MergeJob) {
    let dir = temp_dir(tag);
    let mut paths = Vec::new();
    for (i, b) in lightcraft_merge::synth::bracket_dngs(480, 320, &[-2.0, 0.0, 2.0]).unwrap().into_iter().enumerate() {
        let p = dir.join(format!("IMG_{i}.dng"));
        std::fs::write(&p, b).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids: Vec<_> = import(&mut s, paths).into_iter().map(lightcraft_catalog::PhotoId).collect();
    let job = s.plan_merge(hdr(), Default::default(), &ids, false).unwrap();
    (dir, job)
}

/// `job` reading through a counter, with `sizes` and `room`; `garbage`: every file reads as junk.
fn counted(
    job: &crate::merge::MergeJob,
    sizes: Vec<Option<crate::merge::PhotoSize>>,
    room: f64,
    garbage: bool,
) -> (crate::merge::MergeJob, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let reads = Arc::new(AtomicUsize::new(0));
    let (inner, counter) = (job.read.clone(), reads.clone());
    let read: crate::merge::ByteReader = Arc::new(move |path: &str| {
        counter.fetch_add(1, Ordering::SeqCst);
        if garbage { Ok(b"not a photo".to_vec()) } else { inner(path) }
    });
    (crate::merge::MergeJob { read, sizes, memory: Some(known(room)), ..job.clone() }, reads)
}

/// A photo the catalog has no size for is sized from its file's header, read but not decoded, and
/// the merge is planned with it; when no header can be read the merge is unplanned, decoded as
/// before (where such a file fails).
#[test]
fn a_photo_of_unknown_size_is_sized_from_its_header() {
    use std::sync::atomic::Ordering;
    let (dir, job) = bracketed_dngs("unknown-size");
    assert!(job.sizes.iter().all(|s| s.is_some_and(|s| (s.width, s.height) == (480, 320) && s.bytes > 0)), "the catalog's sizes: {:?}", job.sizes);
    let unknown = vec![
        None,
        Some(crate::merge::PhotoSize { width: 0, height: 0, bytes: 9, samples: 0, active: 0, whole: false }),
        Some(crate::merge::PhotoSize { width: 1, height: 1, bytes: 9, samples: 1, active: 1, whole: false }),
    ];
    // three 480 × 320 photos need several MB: refused with 4 MB after reading only the headers
    let (small, reads) = counted(&job, unknown.clone(), 4e6, false);
    let e = small.run(&|_, _| true).err().unwrap_or_default();
    assert!(e.starts_with("not enough memory for this HDR merge: with its 3 photos") && e.contains("budget is 3 MB"), "{e}");
    assert_eq!(reads.load(Ordering::SeqCst), 3, "headers only");
    // with the memory: headers, then the photos
    let (fits, reads) = counted(&job, unknown.clone(), 1e9, false);
    assert!(fits.run(&|_, _| true).is_ok());
    assert_eq!(reads.load(Ordering::SeqCst), 6);
    // no header: unplanned, decoded as before, and that fails as before
    let (unreadable, reads) = counted(&job, unknown, 4e6, true);
    let e = unreadable.run(&|_, _| true).err().unwrap_or_default();
    assert!(!e.contains("not enough memory") && e.contains("IMG_0.dng"), "{e}");
    assert_eq!(reads.load(Ordering::SeqCst), 4, "the three headers, then the first decode");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file larger than the catalog says (changed since it was imported) is planned again before it
/// is decoded: refused, naming it, when the plan no longer holds; merged when it still does.
#[test]
fn a_photo_larger_than_the_catalog_says_is_planned_again_before_it_is_decoded() {
    use std::sync::atomic::Ordering;
    let (dir, job) = bracketed_dngs("stale-size");
    let stale = vec![crate::merge::PhotoSize::new(48, 32, 10); 3];
    let (small, reads) = counted(&job, stale.clone(), 4e6, false);
    let e = small.run(&|_, _| true).err().unwrap_or_default();
    assert!(
        e.contains("IMG_0.dng needs more memory to decode than expected: not enough memory for this HDR merge")
            && e.ends_with(". It looks changed since it was imported: synchronize its folder, then merge again"),
        "{e}"
    );
    assert!(e.contains("not enough memory for this HDR merge"), "{e}");
    assert_eq!(reads.load(Ordering::SeqCst), 1, "the first file read, not decoded");
    let (fits, _) = counted(&job, stale, 1e9, false);
    assert!(fits.run(&|_, _| true).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A panorama whose photos don't fit at full size is stitched from smaller frames while it still
/// reaches its floor (here a 0.5 MP size asked for): the result names the photos' own resolution.
/// On a guessed memory too small for even that, it runs as before merges were planned: at full
/// size, the stitch at its floor (the photos' own 9.6 MP).
#[test]
fn a_panorama_of_photos_too_large_for_memory_is_made_from_smaller_frames() {
    let dir = temp_dir("pano-reduced");
    let mut paths = Vec::new();
    for (i, v) in lightcraft_merge::synth::pano_views(1600, 1200, 1400.0, &[-40.0, -20.0, 0.0, 20.0, 40.0]).unwrap().into_iter().enumerate() {
        let png =
            lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&v.to_srgb8()), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        let p = dir.join(format!("P_{i}.png"));
        std::fs::write(&p, png).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids: Vec<_> = import(&mut s, paths).into_iter().map(lightcraft_catalog::PhotoId).collect();
    let small = crate::merge::MergeKind::Panorama { pano: lightcraft_merge::PanoOptions { max_megapixels: 0.5, ..Default::default() } };
    let mut job = s.plan_merge(small, Default::default(), &ids, false).unwrap();
    let sizes: Vec<_> = job.sizes.iter().map(|s| s.unwrap()).collect();
    let file = sizes.iter().map(|s| s.bytes).max().unwrap() as f64;
    // five 1.9 MP photos peak at ~161 MB and a file at full size (four frames beside the fifth
    // decoding), ~130 MB and a file made 1024 px long (the fifth then resampled too)
    let room = 145e6 + file;
    let edge = crate::merge::plan_memory(&job.kind, &sizes, room).unwrap().expect("made smaller");
    assert!((1024..1600).contains(&edge), "{edge}");
    job.memory = Some(known(room));
    let r = job.run(&|_, _| true).unwrap().info;
    assert_eq!(r["used"].as_array().map(Vec::len), Some(5), "{r}");
    assert_eq!(r["limit"], "asked", "{r}");
    let (mp, native) = (r["megapixels"].as_f64().unwrap(), r["nativeMegapixels"].as_f64().unwrap());
    // the size asked for (whole pixels aside); the photos' own resolution, many times that
    assert!(mp <= 0.5 * 1.01 && native > 4.0 * mp, "{r}");
    // the full size on a guessed 1 MB: not refused
    let mut job = s.plan_merge(panorama(), Default::default(), &ids, false).unwrap();
    job.memory = Some(crate::merge::Memory { bytes: 1e6, known: false });
    let r = job.run(&|_, _| true).unwrap().info;
    assert_eq!(r["used"].as_array().map(Vec::len), Some(5), "{r}");
    assert!(r["megapixels"].as_f64().unwrap() > 2.0, "at its floor, the photos' own size: {r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A computer whose memory can't be read (Windows, for now) has a guessed allowance (8 GB of RAM):
/// the plan follows it, but a merge it would refuse runs as it did before merges were planned
/// (full size, one photo at a time), since HDR merges like five 67 MP raws (measured at ~5.5 GB)
/// ran there before and may well fit. No check before a decode refuses on the guess either.
#[test]
fn a_guessed_memory_never_refuses_a_merge() {
    use crate::merge::{Memory, PhotoSize, plan_memory};
    use std::sync::atomic::Ordering;
    let guess = |bytes: f64| Memory { bytes, known: false };
    let raws = vec![PhotoSize::new(10_000, 6_700, 80_000_000).unwrap(); 5];
    assert!(plan_memory(&hdr(), &raws, 5e9).is_err(), "refused when 5 GB is known");
    let (mut job, reads) = counted_job(hdr(), raws.into_iter().map(Some).collect(), None, 5e9);
    job.memory = Some(guess(5e9));
    assert_eq!(job.plan(guess(5e9)).map(|(l, _)| (l.edge, l.parallel)), Ok((None, false)));
    assert_eq!(job.run(&|_, _| true).err().as_deref(), Some("/photos/0.ARW: read"), "it runs: the photos are read");
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    for kind in [panorama(), hdr(), hdr_panorama(3)] {
        for preview in [None, Some(crate::merge::PREVIEW_EDGE)] {
            let (job, _) = counted_job(kind.clone(), photos_24mp(6).into_iter().map(Some).collect(), preview, 0.3e9);
            let (load, _) = job.plan(guess(0.3e9)).unwrap();
            assert_eq!((load.edge, load.parallel), (preview, false), "{kind:?} {preview:?}: as before, one at a time");
            assert!(job.plan(known(0.3e9)).is_err(), "{kind:?} {preview:?}: refused when the memory is known");
        }
    }
    // what the guess allows is still planned: smaller frames, previews in parallel
    let (job, _) = counted_job(panorama(), photos_24mp(5).into_iter().map(Some).collect(), None, 2.8e9);
    assert_eq!(job.plan(guess(2.8e9)).map(|(l, _)| l.edge), job.plan(known(2.8e9)).map(|(l, _)| l.edge));
    assert!(job.plan(guess(2.8e9)).unwrap().0.edge.is_some());
    let (job, _) = counted_job(panorama(), photos_24mp(6).into_iter().map(Some).collect(), Some(crate::merge::PREVIEW_EDGE), 1e12);
    assert!(job.plan(guess(1e12)).unwrap().0.parallel);
}

/// A raw with a default crop much smaller than its active area (an in-camera aspect or crop mode)
/// demosaics the whole active area, then crops it, unless it is a Bayer mosaic without an
/// `OpcodeList3` (`lightcraft_raw::RawImage::develop`): its decode is counted at least the
/// demosaiced active area (12 bytes a pixel) beside its frame, not just its picture. A preview's
/// (bilinear) decode always demosaics the whole area.
#[test]
fn a_small_crop_of_a_large_sensor_is_counted_with_its_whole_demosaic() {
    use crate::merge::{PhotoSize, holds};
    let catalog = PhotoSize::new(3000, 2000, 30_000_000).unwrap();
    let sensor = 6000 * 4000;
    let whole = PhotoSize { samples: sensor, active: sensor, whole: true, ..catalog };
    let windowed = PhotoSize { whole: false, ..whole };
    let (frame, demosaic) = (6e6 * 12.0, sensor as f64 * 12.0);
    assert!(whole.decoding(None, false) >= demosaic + frame, "{}", whole.decoding(None, false));
    assert_eq!(whole.decoding(None, false) - windowed.decoding(None, false), demosaic, "a Bayer AHD decode demosaics just its crop");
    assert!(windowed.decoding(Some(1024), true) >= demosaic + frame, "a preview demosaics all of it");
    // two of them: planned from the catalog they fit in 600 MB; with the header read before the
    // decode the whole demosaic (288 MB a photo, ~654 MB at the second decode) doesn't
    assert_eq!(holds(&hdr(), &[catalog; 2], 6e8, None, None), Ok(()));
    assert!(holds(&hdr(), &[whole; 2], 6e8, None, None).is_err());
    // the header's active area: a linear DNG (three samples a pixel) "demosaics" all of it, which
    // here is its picture, so it holds nothing more
    let dng = lightcraft_merge::synth::bracket_dngs(480, 320, &[0.0]).unwrap().remove(0);
    let header = PhotoSize::from_header(&dng).unwrap();
    assert_eq!((header.width, header.height, header.active, header.whole), (480, 320, 480 * 320, true), "{header:?}");
    assert_eq!(header.decoding(None, false), PhotoSize { whole: false, ..header }.decoding(None, false));
}

/// A JPEG XL the catalog has no size for is sized from its headers before the merge decodes it
/// (its `read_header` is a small decode), so it is planned and checked like any photo. A preview
/// none of whose photos has a size known runs unplanned, and then decodes one photo at a time.
#[test]
fn a_jpeg_xl_is_planned_from_its_headers_and_an_unplanned_preview_decodes_one_at_a_time() {
    use lightcraft_codecs::{EncodeImage, EncodeMeta, JxlOptions, encode_jxl};
    use std::sync::Arc;
    let img = lightcraft_raster::Rgba8::from_fn(600, 400, |x, y| [(x % 256) as u8, (y % 256) as u8, 90, 255]);
    let jxl = encode_jxl(&EncodeImage::rgba8(&img), &JxlOptions::default(), &EncodeMeta::default()).unwrap();
    let size = crate::merge::PhotoSize::from_header(&jxl).expect("sized from its headers");
    assert_eq!((size.width, size.height, size.bytes), (600, 400, jxl.len() as u64));
    let job = |bytes: Vec<u8>| crate::merge::MergeJob {
        kind: panorama(),
        finish: Default::default(),
        sources: (1..=3).map(|i| (lightcraft_catalog::PhotoId(i), format!("/photos/{i}.jxl"))).collect(),
        preview: Some(crate::merge::PREVIEW_EDGE),
        read: Arc::new(move |_: &str| Ok(bytes.clone())),
        sizes: vec![None; 3],
        memory: None,
    };
    let (load, planned) = job(jxl).plan(known(1e12)).unwrap();
    assert_eq!(planned, vec![size; 3]);
    assert!(load.parallel, "planned: in parallel when that fits");
    let (load, planned) = job(b"not a photo".to_vec()).plan(known(1e12)).unwrap();
    assert!(planned.is_empty() && !load.parallel, "unplanned: one at a time");
}

/// A panorama whose decode is just over its memory is decoded only as much smaller as it must be:
/// ten 24 MP photos peak at ~3.77 GB, so with 3.7 GB they are made smaller, but not halved (the
/// plan used to keep all frames under the blend's memory, 3.7 GB / 68 bytes = 54 MP: ~2850 px).
/// Frames up to ~145 MP still leave the stitch as large as memory allows (3.7 GB / 68 bytes);
/// larger ones would leave less room for its projected tiles and make the panorama smaller.
#[test]
fn a_panorama_just_over_its_memory_is_made_only_a_little_smaller() {
    use crate::merge::{fit_with_floor, plan_memory};
    let (photos, room) = (photos_24mp(10), 3.7e9);
    let edge = plan_memory(&panorama(), &photos, room).unwrap().expect("made smaller");
    assert!((4300..6000).contains(&edge), "{edge}");
    let frames = |edge: usize| 10.0 * photos[0].decoded(Some(edge));
    let pano = lightcraft_merge::PanoOptions::default();
    let stitch = |edge: usize| fit_with_floor(&pano, frames(edge), 40.0, room).unwrap().max_megapixels;
    assert!((stitch(edge) - room / 68e6).abs() < 1e-9, "as large as memory allows: {}", stitch(edge));
    assert!(stitch(edge + 100) < stitch(edge), "larger frames make it smaller");
}

/// The check before a decode says a file may have changed since import only when its header
/// disagrees with the catalog in size or file size, not when it is a portrait photo the catalog
/// keeps upright and its raw header doesn't.
#[test]
fn a_portrait_photo_is_not_taken_as_changed_since_import() {
    use crate::merge::{PhotoSize, changed_since_import};
    let catalog = PhotoSize::new(4000, 6000, 30_000_000).unwrap();
    assert!(!changed_since_import(catalog, PhotoSize::new(6000, 4000, 30_000_000).unwrap()), "turned");
    assert!(!changed_since_import(catalog, PhotoSize { samples: 30_000_000, ..catalog }), "its sensor");
    assert!(changed_since_import(catalog, PhotoSize::new(6000, 4500, 30_000_000).unwrap()));
    assert!(changed_since_import(catalog, PhotoSize::new(4000, 6000, 31_000_000).unwrap()));
}

/// `nativeMegapixels` comes from the photos' own sizes: a photo the plan sized as the largest
/// known (its own unknown) counts as its frame, not as that stand-in.
#[test]
fn native_megapixels_come_only_from_real_sizes() {
    use crate::merge::{PhotoSize, photo_areas};
    let (big, small) = (PhotoSize::new(6000, 4000, 9), PhotoSize::new(3000, 2000, 9));
    assert_eq!(photo_areas(&[big, None, small], &[1e6, 2e6, 3e6], 1), vec![(24e6, 1e6), (2e6, 2e6), (6e6, 3e6)]);
    // an HDR panorama's brackets: the largest photo of each, or its frame when one is unknown
    assert_eq!(photo_areas(&[big, small, small, None], &[1e6, 2e6], 2), vec![(24e6, 1e6), (2e6, 2e6)]);
}

/// Frames made smaller for memory keep enough pixels for the panorama to reach its floor: three
/// 100 MP photos with 5 GB would fit only as ~12 MP of frames (one photo's decode alone holds
/// ~4.9 GB), far under a 40 MP panorama, so the merge is refused rather than stitched below it.
/// With 6 GB they are decoded smaller, to at least 80 MP of frames (the floor's pixels over the
/// half a panorama is taken to keep of its frames). After the stitch, a panorama of smaller frames
/// still under its floor (their overlap known only then) is refused too.
#[test]
fn frames_made_smaller_keep_enough_pixels_for_the_floor() {
    use crate::merge::{PhotoSize, below_floor, plan_memory};
    let photos = vec![PhotoSize::new(10_000, 10_000, 100_000_000).unwrap(); 3];
    assert_eq!(plan_memory(&panorama(), &photos, 8e9), Ok(None));
    let edge = plan_memory(&panorama(), &photos, 6e9).unwrap().expect("made smaller");
    assert!(3.0 * photos[0].decoded(Some(edge)) >= 80e6, "{edge}");
    let e = plan_memory(&panorama(), &photos, 5e9).unwrap_err();
    assert!(e.starts_with("not enough memory for this panorama") && e.contains("the panorama 40 MP"), "{e}");
    // after the stitch: 30 MP from frames four times smaller than the photos is under 40 MP
    assert!(below_floor(30.0, 4.0, 40.0));
    assert!(!below_floor(40.0, 4.0, 40.0) && !below_floor(45.0, 4.0, 40.0));
    assert!(!below_floor(30.0, 1.0, 40.0), "not made smaller: its own size");
    // small photos whose own size is the floor: any panorama of smaller frames is under it
    assert!(below_floor(2.39, 1.2, 3.0) && !below_floor(2.9, 1.01, 3.0));
}
