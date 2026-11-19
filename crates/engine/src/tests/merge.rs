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
    // Auto Settings: the brightness is in the DNG (BaselineExposure), every slider at 0 with its whole range free
    let d = s.develop_of(id).unwrap();
    assert_eq!((d.light.exposure, d.light.contrast, d.light.highlights, d.light.shadows), (0.0, 0.0, 0.0, 0.0), "sliders untouched");
    let baseline = |path: &str| lightcraft_raw::decode(&std::fs::read(path).unwrap()).unwrap().color.baseline_exposure;
    let auto_baseline = baseline(path);
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
    // without Auto Settings the DNG keeps the merge's own brightness
    assert!((auto_baseline - baseline(r2["path"].as_str().unwrap())).abs() > 0.05, "auto exposure baked in");
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
    // the auto crop is the DNG's default crop: the photo is that part, and no crop edit
    let p = s.catalog.photo(id).unwrap().clone();
    let (full_w, full_h) = (r["width"].as_u64().unwrap() as f64, r["height"].as_u64().unwrap() as f64);
    assert!(full_w > 800.0 && full_h > 300.0, "{full_w}×{full_h}");
    assert!((p.width as f64) < full_w && (p.width * p.height) as f64 > 0.5 * full_w * full_h, "{}×{} of {full_w}×{full_h}", p.width, p.height);
    let d = s.develop_of(id).unwrap();
    assert!(d.crop.geometry.is_identity() && !d.crop.full_frame, "{:?}", d.crop);
    let aspect = |s: &mut Session| {
        let img = s.render_now(id, 1200, 1200).unwrap().image;
        img.width as f64 / img.height as f64
    };
    let cropped = aspect(&mut s);
    assert!((cropped - p.width as f64 / p.height as f64).abs() < 0.02, "{cropped}");
    // Show Full Image: the whole merged canvas, past the default crop; and back
    s.execute("crop.fullFrame", &json!({"on": true})).unwrap();
    assert!(s.develop_of(id).unwrap().crop.full_frame);
    let full = aspect(&mut s);
    assert!((full - full_w / full_h).abs() < 0.02, "{full} vs {}", full_w / full_h);
    s.execute("crop.fullFrame", &json!({})).unwrap();
    assert!((aspect(&mut s) - cropped).abs() < 1e-6);
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

/// A 100×60 linear DNG with a 40×40 default crop at (20, 10), imported and selected (PR #2030 review).
fn dng_with_default_crop(tag: &str) -> (std::path::PathBuf, Session, lightcraft_catalog::PhotoId) {
    let dir = temp_dir(tag);
    let image = lightcraft_raster::Rgb32f::from_fn(100, 60, |x, y| [0.1 + x as f32 / 200.0, 0.2 + y as f32 / 200.0, 0.3]);
    let to_xyz_d50 = lightcraft_color::bradford(lightcraft_color::D65, lightcraft_color::D50).mul(&lightcraft_color::SRGB.to_xyz());
    let color = lightcraft_merge::FrameColor::Linear { to_xyz_d50 };
    let dng = lightcraft_merge::write_linear_dng(
        &image,
        &color,
        lightcraft_geom::Orientation::Normal,
        &Default::default(),
        0.0,
        lightcraft_merge::DngSamples::Half,
        &[],
    )
    .unwrap();
    let mut raw = lightcraft_raw::decode(&dng).unwrap();
    raw.crop = lightcraft_raw::Rect::new(20, 10, 40, 40);
    let path = dir.join("cropped.dng");
    std::fs::write(&path, lightcraft_raw::write_dng(&raw, &Default::default()).unwrap()).unwrap();
    let mut s = Session::new().with_fs();
    let id = lightcraft_catalog::PhotoId(import(&mut s, vec![path.to_string_lossy().to_string()])[0]);
    s.selection = crate::Selection::single(id);
    let p = s.catalog.photo(id).unwrap();
    assert_eq!((p.width, p.height), (40, 40), "the photo is its default crop");
    (dir, s, id)
}

/// Show Full Image exports the whole picture at its own size, not scaled into the default crop's size.
#[test]
fn full_image_exports_at_the_whole_pictures_size() {
    let (dir, mut s, id) = dng_with_default_crop("full-export");
    s.execute("crop.fullFrame", &json!({"on": true})).unwrap();
    let view = s.render_now(id, 100, 100).unwrap().image;
    assert_eq!((view.width, view.height), (100, 60));
    assert_eq!(s.canvas_size(id), Some((100, 60)));
    let options = crate::export::ExportOptions { format: crate::export::ExportFormat::Png, resize: None, ..Default::default() };
    let exported = crate::export::export_photo(&mut s, id, &options, 1).unwrap();
    assert_eq!((exported.width, exported.height), (100, 60));
    let decoded = lightcraft_codecs::decode(&exported.bytes, lightcraft_codecs::DecodeOptions::default()).unwrap();
    assert_eq!((decoded.image.width, decoded.image.height), (100, 60));
    // and back: the default crop's size
    s.execute("crop.fullFrame", &json!({"on": false})).unwrap();
    assert_eq!(s.canvas_size(id), Some((40, 40)));
    let exported = crate::export::export_photo(&mut s, id, &options, 1).unwrap();
    assert_eq!((exported.width, exported.height), (40, 40));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A grid thumbnail of a photo shown whole is the whole picture too, not the default crop that a thumbnail's
/// reduced-copy loader decodes.
#[test]
fn a_thumbnail_with_show_full_image_is_the_whole_picture() {
    let (dir, mut s, id) = dng_with_default_crop("full-thumb");
    s.execute("crop.fullFrame", &json!({"on": true})).unwrap();
    s.media.clear_sources();
    let thumb = s.thumb_job(id, 128).unwrap().run();
    let image = &thumb.rendered.as_ref().unwrap().image;
    assert!(image.width.abs_diff(image.height * 100 / 60) <= 1, "the whole picture's aspect: {}×{}", image.width, image.height);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A decode planned before Show Full Image was toggled that finishes after it doesn't put the old picture back.
#[test]
fn a_decode_from_before_full_image_is_not_kept() {
    let (dir, mut s, id) = dng_with_default_crop("full-late");
    let old = s.render_job(id, 100, 100, false, true).unwrap();
    s.execute("crop.fullFrame", &json!({"on": true})).unwrap();
    let late = old.run();
    assert!(late.rendered.is_ok() && late.loaded.is_some());
    s.accept(&late);
    let full = s.render_now(id, 100, 100).unwrap().image;
    assert_eq!((full.width, full.height), (100, 60), "the whole picture, not the late default crop");
    // the other way: a whole-picture decode that finishes after the toggle is undone
    let old = s.render_job(id, 100, 100, false, true).unwrap();
    s.media.forget(id);
    let old = old.run();
    s.undo_step().unwrap();
    s.accept(&old);
    let cropped = s.render_now(id, 100, 100).unwrap().image;
    assert_eq!((cropped.width, cropped.height), (100, 100));
    s.redo_step().unwrap();
    let full = s.render_now(id, 100, 100).unwrap().image;
    assert_eq!((full.width, full.height), (100, 60));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Show Full Image toggled inside an interaction (previewed without an undo step) drops the decoded picture like a
/// committed toggle does, so a cancelled interaction doesn't keep showing the other picture (PR #2030 review).
#[test]
fn a_cancelled_full_image_preview_drops_its_picture() {
    let (dir, mut s, id) = dng_with_default_crop("full-cancel");
    let cropped = s.render_now(id, 100, 100).unwrap().image;
    assert_eq!((cropped.width, cropped.height), (100, 100));
    let set_full = |s: &mut Session, on: bool| {
        let mut d = (*s.develop_of(id).unwrap()).clone();
        d.crop.full_frame = on;
        s.set_develop(id, d, "Show Full Image").unwrap();
    };
    s.begin_interaction("Show Full Image").unwrap();
    set_full(&mut s, true);
    let full = s.render_now(id, 100, 100).unwrap().image;
    assert_eq!((full.width, full.height), (100, 60), "the preview shows the whole picture");
    s.cancel_interaction().unwrap();
    let back = s.render_now(id, 100, 100).unwrap().image;
    assert_eq!((back.width, back.height), (100, 100), "cancelled: the default crop again, not the cached whole picture");
    // the other way: a preview turning it off, cancelled, shows the whole picture again
    s.execute("crop.fullFrame", &json!({"on": true})).unwrap();
    assert_eq!(s.render_now(id, 100, 100).unwrap().image.height, 60);
    s.begin_interaction("Show Full Image").unwrap();
    set_full(&mut s, false);
    assert_eq!(s.render_now(id, 100, 100).unwrap().image.height, 100, "the preview shows the default crop");
    s.cancel_interaction().unwrap();
    assert_eq!(s.render_now(id, 100, 100).unwrap().image.height, 60, "cancelled: the whole picture again");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Show Full Image is refused for a photo that has no picture past its crop (a PNG), and its crop is kept (PR #2030
/// review); turning it off is still allowed.
#[test]
fn full_image_is_refused_for_a_rendered_photo() {
    let dir = temp_dir("full-png");
    let img = lightcraft_raster::Rgba8::from_fn(64, 48, |x, y| [x as u8 * 3, y as u8 * 4, 90, 255]);
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    let path = dir.join("flat.png");
    std::fs::write(&path, png).unwrap();
    let mut s = Session::new().with_fs();
    let id = lightcraft_catalog::PhotoId(import(&mut s, vec![path.to_string_lossy().to_string()])[0]);
    s.selection = crate::Selection::single(id);
    s.execute("develop.set", &json!({"control": "crop.angle", "value": 5.0})).unwrap();
    let before = s.develop_of(id).unwrap().crop;
    assert!(!before.geometry.is_identity());
    let e = s.execute("crop.fullFrame", &json!({"on": true})).unwrap_err().to_string();
    assert!(e.contains("raw"), "{e}");
    assert_eq!(s.develop_of(id).unwrap().crop, before, "the crop is untouched");
    assert!(s.execute("crop.fullFrame", &json!({})).is_err(), "a toggle from off is turning it on");
    s.execute("crop.fullFrame", &json!({"on": false})).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The UI's whole-picture size never reads the file on the calling thread: the first ask gives the default crop's size
/// and reads the headers in the background, with a row in the activity stack; once read, the size is the whole
/// picture's (PR #2030 review).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_ui_reads_the_whole_pictures_size_in_the_background() {
    let (dir, mut s, id) = dng_with_default_crop("full-probe");
    s.execute("crop.fullFrame", &json!({"on": true})).unwrap();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let wait = std::sync::Mutex::new(wait);
    s.media.file_bytes = Some(std::sync::Arc::new(move |path: &str| {
        // held until the test lets go (or a few seconds, so a read on the calling thread fails the test, not hangs it)
        let _ = wait.lock().unwrap().recv_timeout(std::time::Duration::from_secs(3));
        std::fs::read(path).map_err(|e| e.to_string())
    }));
    let started = std::time::Instant::now();
    assert_eq!(s.canvas_size_soon(id), Some((40, 40)), "not read yet: the default crop's size");
    assert!(started.elapsed() < std::time::Duration::from_secs(2), "the file was read on the calling thread");
    assert_eq!(s.canvas_size_soon(id), Some((40, 40)), "asking again doesn't start a second read");
    let rows: Vec<_> = s.activity.list().into_iter().filter(|t| t.kind == "probe").collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(!rows[0].cancellable);
    release.send(()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while s.canvas_size_soon(id) != Some((100, 60)) {
        assert!(std::time::Instant::now() < deadline, "the size never arrived");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    while s.activity.list().iter().any(|t| t.kind == "probe") {
        assert!(std::time::Instant::now() < deadline, "the row stayed");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // the export's synchronous size finds it too
    assert_eq!(s.canvas_size(id), Some((100, 60)));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A whole-picture size read that fails isn't started again on every ask (the watermark preview asks every frame):
/// it waits [`crate::media::SIZE_RETRY`] before reading the file again (PR #2030 review).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_failed_size_read_is_not_retried_on_every_ask() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (dir, mut s, id) = dng_with_default_crop("full-probe-fail");
    s.execute("crop.fullFrame", &json!({"on": true})).unwrap();
    let reads = std::sync::Arc::new(AtomicUsize::new(0));
    let counted = reads.clone();
    s.media.file_bytes = Some(std::sync::Arc::new(move |_: &str| {
        counted.fetch_add(1, Ordering::SeqCst);
        Err("the drive is offline".to_string())
    }));
    let settle = |s: &Session| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while s.activity.list().iter().any(|t| t.kind == "probe") {
            assert!(std::time::Instant::now() < deadline, "the read never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    assert_eq!(s.canvas_size_soon(id), Some((40, 40)));
    settle(&s);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    for _ in 0..20 {
        assert_eq!(s.canvas_size_soon(id), Some((40, 40)), "still the default crop's size");
        settle(&s);
    }
    assert_eq!(reads.load(Ordering::SeqCst), 1, "a failed read was started again on every ask");
    // once the retry interval has passed, the file is read again (it may be back)
    for at in crate::media::lock_sizes(&s.media.canvas_sizes).failed.values_mut() {
        *at = at.checked_sub(crate::media::SIZE_RETRY).unwrap();
    }
    assert_eq!(s.canvas_size_soon(id), Some((40, 40)));
    settle(&s);
    assert_eq!(reads.load(Ordering::SeqCst), 2, "never read again");
    let _ = std::fs::remove_dir_all(&dir);
}
