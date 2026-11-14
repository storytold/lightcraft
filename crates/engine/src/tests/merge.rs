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

/// Two collapsed stacks plus loose photos plan one job per group via `plan_merges`, in the order
/// their first member appears, and `plan_merge` refuses such a multi-group selection instead of
/// silently dropping all but the first group.
#[test]
fn two_collapsed_stacks_and_loose_photos_plan_separate_jobs() {
    let dir = temp_dir("multi-stack");
    let mut paths = Vec::new();
    for (set, evs) in [("A", &[-2.0, 0.0, 2.0][..]), ("B", &[-1.5, 0.5, 2.5][..]), ("C", &[-1.0, 1.0][..])] {
        for (i, b) in lightcraft_merge::synth::bracket_dngs(96, 64, evs).unwrap().into_iter().enumerate() {
            let p = dir.join(format!("{set}_{i}.dng"));
            std::fs::write(&p, b).unwrap();
            paths.push(p.to_string_lossy().to_string());
        }
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    assert_eq!(ids.len(), 8);
    s.execute("stack.group", &json!({"ids": ids[..3]})).unwrap();
    s.execute("stack.group", &json!({"ids": ids[3..6]})).unwrap();
    let (kind, finish) = crate::merge::parse("merge.hdr", &json!({})).unwrap();
    // Interleave the groups: loose photos first, then a member of each stack.
    let mixed = [ids[6], ids[0], ids[3], ids[7], ids[1], ids[4]];
    let mixed_ids: Vec<lightcraft_catalog::PhotoId> = mixed.iter().map(|id| lightcraft_catalog::PhotoId(*id)).collect();
    let jobs = s.plan_merges(kind.clone(), finish.clone(), &mixed_ids, true).unwrap();
    assert_eq!(jobs.len(), 3, "one job per group");
    let members = |job: &crate::merge::MergeJob| job.sources.iter().map(|(p, _)| p.0).collect::<Vec<_>>();
    assert_eq!(members(&jobs[0]), vec![ids[6], ids[7]], "loose group keeps given order");
    assert_eq!(members(&jobs[1]), ids[..3].to_vec(), "first stack expands to its members");
    assert_eq!(members(&jobs[2]), ids[3..6].to_vec(), "second stack expands to its members");
    let err = s.plan_merge(kind, finish, &mixed_ids, true).err().unwrap().to_string();
    assert!(err.contains("more than one merge group"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two collapsed stacks merge via the command (through `merge_now`/`plan_merges`), and a valid stack
/// still merges when a loose photo alone is too few to merge: the undersized group is reported as
/// `skipped` instead of refusing the whole selection.
#[test]
fn two_stacks_merge_via_command_and_undersized_loose_is_skipped() {
    let dir = temp_dir("multi-stack-cmd");
    let mut paths = Vec::new();
    for (set, evs) in [("A", &[-2.0, 0.0, 2.0][..]), ("B", &[-1.5, 0.5, 2.5][..]), ("C", &[-1.0, 1.0][..])] {
        for (i, b) in lightcraft_merge::synth::bracket_dngs(96, 64, evs).unwrap().into_iter().enumerate() {
            let p = dir.join(format!("{set}_{i}.dng"));
            std::fs::write(&p, b).unwrap();
            paths.push(p.to_string_lossy().to_string());
        }
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    assert_eq!(ids.len(), 8);
    s.execute("stack.group", &json!({"ids": ids[..3]})).unwrap();
    s.execute("stack.group", &json!({"ids": ids[3..6]})).unwrap();
    // plan_merges skips the single loose photo and keeps the valid stack.
    let (kind, finish) = crate::merge::parse("merge.hdr", &json!({})).unwrap();
    let mixed: Vec<lightcraft_catalog::PhotoId> = [ids[6], ids[0]].iter().map(|id| lightcraft_catalog::PhotoId(*id)).collect();
    let jobs = s.plan_merges(kind.clone(), finish.clone(), &mixed, true).unwrap();
    assert_eq!(jobs.len(), 1, "undersized loose group is skipped, valid stack is kept");
    assert_eq!(jobs[0].sources.iter().map(|(p, _)| p.0).collect::<Vec<_>>(), ids[..3].to_vec());
    // The command merges the valid stack and reports the loose photo as skipped.
    let r = s.execute("merge.hdr", &json!({"ids": [ids[0], ids[6]], "autoSettings": false})).unwrap();
    assert_eq!(r["skipped"], json!([[ids[6]]]), "{r}");
    assert!(r["path"].as_str().unwrap().ends_with("A_0-HDR.dng"), "{r}");
    // Two collapsed stacks via the command produce two outputs.
    // The first stack already merged to an identical file, which import now skips as a duplicate, so this merge
    // uses other options to produce a different file.
    let r = s.execute("merge.hdr", &json!({"ids": [ids[0], ids[3]], "autoSettings": false, "deghost": "high"})).unwrap();
    let merged = r["merged"].as_array().unwrap();
    assert_eq!(merged.len(), 2, "{r}");
    let names: Vec<&str> = merged.iter().map(|m| m["path"].as_str().unwrap()).collect();
    assert!(names[0].ends_with("A_0-HDR-2.dng") && names[1].ends_with("B_0-HDR.dng"), "{names:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A collapsed stack contributes all its members to a merge, not just its visible top photo:
/// naming the top of a collapsed bracket stack plans the merge of its members.
#[test]
fn collapsed_stack_top_expands_to_stack_members() {
    let dir = temp_dir("collapsed-stack-top");
    let mut paths = Vec::new();
    for (i, b) in lightcraft_merge::synth::bracket_dngs(96, 64, &[-1.0, 0.0, 1.0]).unwrap().into_iter().enumerate() {
        let p = dir.join(format!("IMG_{i}.dng"));
        std::fs::write(&p, b).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    s.execute("stack.group", &json!({"ids": ids, "top": ids[0]})).unwrap();
    let top = lightcraft_catalog::PhotoId(ids[0]);
    let stack = s.catalog.stack_of(top).expect("the bracket stack").clone();
    assert!(stack.collapsed);
    assert_eq!(stack.top(), top);
    let (kind, finish) = crate::merge::parse("merge.hdr", &json!({})).unwrap();
    let planned = s.plan_merge(kind, finish, &[top], true);
    assert!(planned.is_ok(), "a collapsed stack top should expand to its members: {:?}", planned.err());
    let job = planned.unwrap();
    assert_eq!(job.sources.iter().map(|(p, _)| p.0).collect::<Vec<_>>(), stack.photos.iter().map(|p| p.0).collect::<Vec<_>>());
    let _ = std::fs::remove_dir_all(&dir);
}

/// When every merge job fails, `merge_now` is an error (not an empty `merged` list), so
/// `merge.hdrBatch` reports the set under `failed` instead of as a merged entry.
#[test]
fn a_merge_that_fails_every_job_is_an_error() {
    let mut s = Session::new();
    let mut ids = Vec::new();
    for i in 0..3 {
        let id = s.catalog.alloc_photo_id();
        let name = format!("IMG_{i}.dng");
        let p =
            lightcraft_catalog::Photo::new(id, lightcraft_catalog::Source::File { path: name.clone() }, &name, "DNG", 96, 64, "2026-01-01T00:00:00");
        s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
        ids.push(id);
    }
    let err = s.execute("merge.hdr", &json!({"ids": ids})).unwrap_err().to_string();
    assert!(err.contains("all merges failed"), "{err}");
}

/// An undersized group is skipped before its sources are validated: a demo photo alone is too
/// few to merge, so it must not reject a valid collapsed stack selected with it.
#[test]
fn undersized_group_is_skipped_before_source_validation() {
    let mut s = Session::new();
    let mut ids = Vec::new();
    for i in 0..3 {
        let id = s.catalog.alloc_photo_id();
        let name = format!("IMG_{i}.dng");
        let p =
            lightcraft_catalog::Photo::new(id, lightcraft_catalog::Source::File { path: name.clone() }, &name, "DNG", 96, 64, "2026-01-01T00:00:00");
        s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
        ids.push(id);
    }
    let demo = s.catalog.alloc_photo_id();
    let demo_photo =
        lightcraft_catalog::Photo::new(demo, lightcraft_catalog::Source::Demo { scene: 1 }, "demo.jpg", "JPEG", 96, 64, "2026-01-01T00:00:00");
    s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(demo_photo) }).unwrap();
    let op = s.catalog.group_ops(ids[0], &ids, true).unwrap();
    s.catalog.apply(op).unwrap();
    let (kind, finish) = crate::merge::parse("merge.hdr", &json!({})).unwrap();
    let selected = vec![ids[0], demo];
    let planned = s.plan_merges(kind, finish, &selected, true);
    assert!(planned.is_ok(), "undersized demo group should be skipped: {:?}", planned.err());
    let jobs = planned.unwrap();
    assert_eq!(jobs.len(), 1, "the valid stack is kept");
    assert_eq!(jobs[0].sources.iter().map(|(p, _)| *p).collect::<Vec<_>>(), ids);
}

/// A collapsed stack that contains an earlier merge result expands to its sources only:
/// the result (`*-HDR.dng`) is excluded by `merge_groups`, in capture order, for both
/// `plan_merge` and `plan_merges`, and a second merge via the command does not merge the result.
#[test]
fn collapsed_stack_excludes_earlier_merge_result() {
    let dir = temp_dir("stack-excludes-result");
    let mut paths = Vec::new();
    for (i, b) in lightcraft_merge::synth::bracket_dngs(96, 64, &[-1.0, 0.0, 1.0]).unwrap().into_iter().enumerate() {
        let p = dir.join(format!("IMG_{i}.dng"));
        std::fs::write(&p, b).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    let r = s.execute("merge.hdr", &json!({"ids": ids, "autoSettings": false, "stack": true})).unwrap();
    let result = lightcraft_catalog::PhotoId(r["id"].as_u64().unwrap());
    let stack = s.catalog.stack_of(result).expect("merge result stacked").clone();
    assert!(stack.photos.contains(&result));
    assert!(stack.photos[1..].iter().map(|p| p.0).collect::<Vec<_>>() == ids);
    let (kind, finish) = crate::merge::parse("merge.hdr", &json!({})).unwrap();
    let job = s.plan_merge(kind.clone(), finish.clone(), &[result], true).unwrap();
    assert_eq!(job.sources.iter().map(|(p, _)| p.0).collect::<Vec<_>>(), ids, "plan_merge excludes the merge result");
    let jobs = s.plan_merges(kind, finish, &[result], true).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].sources.iter().map(|(p, _)| p.0).collect::<Vec<_>>(), ids, "plan_merges excludes the merge result");
    let r2 = s.execute("merge.hdr", &json!({"ids": [result.0], "autoSettings": false})).unwrap();
    assert!(r2["path"].as_str().unwrap().ends_with("IMG_0-HDR-2.dng"), "{r2}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A middle group that fails does not stop later groups: `merge_now` reports both
/// `merged` (first and third) and `failed` (middle), and the same holds for previews.
#[test]
fn middle_group_failure_does_not_stop_later_groups() {
    let dir = temp_dir("middle-fails");
    let mut paths = Vec::new();
    for (set, evs) in [("A", &[-2.0, 0.0, 2.0][..]), ("C", &[-1.5, 0.5, 2.5][..])] {
        for (i, b) in lightcraft_merge::synth::bracket_dngs(96, 64, evs).unwrap().into_iter().enumerate() {
            let p = dir.join(format!("{set}_{i}.dng"));
            std::fs::write(&p, b).unwrap();
            paths.push(p.to_string_lossy().to_string());
        }
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    assert_eq!(ids.len(), 6);
    // Middle group: catalog photos whose files do not exist, so its merge fails.
    let mut missing = Vec::new();
    for i in 0..3 {
        let id = s.catalog.alloc_photo_id();
        let name = format!("MISSING_{i}.dng");
        let p =
            lightcraft_catalog::Photo::new(id, lightcraft_catalog::Source::File { path: name.clone() }, &name, "DNG", 96, 64, "2026-01-01T00:00:00");
        s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
        missing.push(id);
    }
    s.execute("stack.group", &json!({"ids": ids[..3]})).unwrap();
    s.execute("stack.group", &json!({"ids": ids[3..]})).unwrap();
    let op = s.catalog.group_ops(missing[0], &missing, true).unwrap();
    s.catalog.apply(op).unwrap();
    let selected = [ids[0], missing[0].0, ids[3]];
    // Preview: first and third previews are reported, middle under `failed`.
    let pv = s.execute("merge.hdr", &json!({"ids": selected, "preview": true})).unwrap();
    assert_eq!(pv["previews"].as_array().unwrap().len(), 2, "{pv}");
    let failed = pv["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 1, "{pv}");
    assert_eq!(failed[0]["photos"], json!(missing.iter().map(|p| p.0).collect::<Vec<_>>()), "{pv}");
    // Full merge: same per-group reporting, two outputs written.
    let r = s.execute("merge.hdr", &json!({"ids": selected, "autoSettings": false})).unwrap();
    let merged = r["merged"].as_array().unwrap();
    assert_eq!(merged.len(), 2, "{r}");
    let names: Vec<&str> = merged.iter().map(|m| m["path"].as_str().unwrap()).collect();
    assert!(names[0].ends_with("A_0-HDR.dng") && names[1].ends_with("C_0-HDR.dng"), "{names:?}");
    let failed = r["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 1, "{r}");
    assert_eq!(failed[0]["photos"], json!(missing.iter().map(|p| p.0).collect::<Vec<_>>()), "{r}");
    assert!(!failed[0]["error"].as_str().unwrap().is_empty(), "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A multi-group preview with `previewPath` is refused instead of silently dropping the path:
/// there is no single file to write for several groups.
#[test]
fn multi_group_preview_with_preview_path_is_an_error() {
    let dir = temp_dir("multi-preview-path");
    let mut paths = Vec::new();
    for (set, evs) in [("A", &[-2.0, 0.0, 2.0][..]), ("B", &[-1.5, 0.5, 2.5][..])] {
        for (i, b) in lightcraft_merge::synth::bracket_dngs(96, 64, evs).unwrap().into_iter().enumerate() {
            let p = dir.join(format!("{set}_{i}.dng"));
            std::fs::write(&p, b).unwrap();
            paths.push(p.to_string_lossy().to_string());
        }
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    assert_eq!(ids.len(), 6);
    s.execute("stack.group", &json!({"ids": ids[..3]})).unwrap();
    s.execute("stack.group", &json!({"ids": ids[3..]})).unwrap();
    let png = dir.join("preview.png");
    let err = s.execute("merge.hdr", &json!({"ids": [ids[0], ids[3]], "preview": true, "previewPath": png})).unwrap_err().to_string();
    assert!(err.contains("previewPath"), "{err}");
    assert!(!png.exists(), "no preview file is written for multiple groups");
    // Without previewPath the multi-group preview still works and reports each group.
    let r = s.execute("merge.hdr", &json!({"ids": [ids[0], ids[3]], "preview": true})).unwrap();
    assert_eq!(r["previews"].as_array().unwrap().len(), 2, "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A single selected collapsed stack enables all three merge actions: the expanded count
/// reaches each kind's minimum and planning expands the stack, while a single loose photo
/// does not.
#[test]
fn single_collapsed_stack_enables_all_three_merge_actions() {
    let mut s = Session::new();
    let mut ids = Vec::new();
    for i in 0..4 {
        let id = s.catalog.alloc_photo_id();
        let name = format!("IMG_{i}.dng");
        let p =
            lightcraft_catalog::Photo::new(id, lightcraft_catalog::Source::File { path: name.clone() }, &name, "DNG", 96, 64, "2026-01-01T00:00:00");
        s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
        ids.push(id);
    }
    let op = s.catalog.group_ops(ids[0], &ids, true).unwrap();
    s.catalog.apply(op).unwrap();
    let top = ids[0];
    let loose = s.catalog.alloc_photo_id();
    let loose_name = "LOOSE.dng".to_string();
    let loose_photo = lightcraft_catalog::Photo::new(
        loose,
        lightcraft_catalog::Source::File { path: loose_name.clone() },
        &loose_name,
        "DNG",
        96,
        64,
        "2026-01-01T00:00:00",
    );
    s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(loose_photo) }).unwrap();
    assert_eq!(s.expanded_merge_count(&[top]), 4, "one selected id represents the stack's members");
    assert_eq!(s.expanded_merge_count(&[loose]), 1, "a single loose photo still counts once");
    for command in ["merge.hdr", "merge.panorama", "merge.hdrPanorama"] {
        let (kind, finish) = crate::merge::parse(command, &json!({})).unwrap();
        assert!(s.merge_selection_ready(&kind, &[top]), "{command} should be enabled for the collapsed stack");
        let job = s.plan_merge(kind, finish, &[top], true).unwrap();
        assert_eq!(job.sources.iter().map(|(p, _)| *p).collect::<Vec<_>>(), ids, "{command} expands the stack");
    }
    let (kind, _) = crate::merge::parse("merge.hdr", &json!({})).unwrap();
    assert!(!s.merge_selection_ready(&kind, &[loose]), "a single loose photo does not enable a merge");
}

/// `plan_merge` refuses a selection holding more than one group even when one group is
/// undersized, instead of silently dropping it and merging only the valid group.
#[test]
fn plan_merge_refuses_undersized_plus_valid_group() {
    let mut s = Session::new();
    let mut ids = Vec::new();
    for i in 0..3 {
        let id = s.catalog.alloc_photo_id();
        let name = format!("IMG_{i}.dng");
        let p =
            lightcraft_catalog::Photo::new(id, lightcraft_catalog::Source::File { path: name.clone() }, &name, "DNG", 96, 64, "2026-01-01T00:00:00");
        s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
        ids.push(id);
    }
    let demo = s.catalog.alloc_photo_id();
    let demo_photo =
        lightcraft_catalog::Photo::new(demo, lightcraft_catalog::Source::Demo { scene: 1 }, "demo.jpg", "JPEG", 96, 64, "2026-01-01T00:00:00");
    s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(demo_photo) }).unwrap();
    let op = s.catalog.group_ops(ids[0], &ids, true).unwrap();
    s.catalog.apply(op).unwrap();
    let (kind, finish) = crate::merge::parse("merge.hdr", &json!({})).unwrap();
    let err = s.plan_merge(kind, finish, &[ids[0], demo], true).err().unwrap().to_string();
    assert!(err.contains("more than one merge group"), "{err}");
}

/// A preview reports an undersized group as `skipped`, like `merge_now` does, instead of
/// silently omitting it: a valid stack previews while a single loose photo is skipped.
#[test]
fn preview_reports_skipped_undersized_group() {
    let dir = temp_dir("preview-skipped");
    let mut paths = Vec::new();
    for (set, evs) in [("A", &[-2.0, 0.0, 2.0][..]), ("C", &[-1.0, 1.0][..])] {
        for (i, b) in lightcraft_merge::synth::bracket_dngs(96, 64, evs).unwrap().into_iter().enumerate() {
            let p = dir.join(format!("{set}_{i}.dng"));
            std::fs::write(&p, b).unwrap();
            paths.push(p.to_string_lossy().to_string());
        }
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    assert_eq!(ids.len(), 5);
    s.execute("stack.group", &json!({"ids": ids[..3]})).unwrap();
    let r = s.execute("merge.hdr", &json!({"ids": [ids[0], ids[3]], "preview": true})).unwrap();
    assert_eq!(r["skipped"], json!([[ids[3]]]), "{r}");
    assert_eq!(r["previewSize"].as_array().unwrap().len(), 2, "single-job preview returns its info directly: {r}");
    assert!(r["previews"].is_null(), "single-job preview has no previews array: {r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `merge_selection_ready` is per-group, not total: two undersized stacks whose total reaches
/// the minimum do not enable a merge, matching `plan_merge`/`plan_merges` which would skip or refuse them.
#[test]
fn merge_selection_ready_is_per_group_not_total() {
    let mut s = Session::new();
    let mut ids = Vec::new();
    for i in 0..4 {
        let id = s.catalog.alloc_photo_id();
        let name = format!("IMG_{i}.dng");
        let p =
            lightcraft_catalog::Photo::new(id, lightcraft_catalog::Source::File { path: name.clone() }, &name, "DNG", 96, 64, "2026-01-01T00:00:00");
        s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
        ids.push(id);
    }
    // Two stacks of two: each is enough for HDR (2) but neither is enough for HDR Panorama (4),
    // although together they total 4.
    let op = s.catalog.group_ops(ids[0], &ids[..2], true).unwrap();
    s.catalog.apply(op).unwrap();
    let op = s.catalog.group_ops(ids[2], &ids[2..], true).unwrap();
    s.catalog.apply(op).unwrap();
    let (pano_kind, _) = crate::merge::parse("merge.hdrPanorama", &json!({})).unwrap();
    assert_eq!(s.expanded_merge_count(&[ids[0], ids[2]]), 4, "total reaches the HDR panorama minimum");
    assert!(!s.merge_selection_ready(&pano_kind, &[ids[0], ids[2]]), "two undersized stacks do not enable a merge");
    // A single stack of two does enable HDR, but two stacks do not enable the single-group dialog merge.
    let (hdr_kind, _) = crate::merge::parse("merge.hdr", &json!({})).unwrap();
    assert!(s.merge_selection_ready(&hdr_kind, &[ids[0]]), "one stack of two enables HDR");
    assert!(!s.merge_selection_ready(&hdr_kind, &[ids[0], ids[2]]), "two groups do not enable the single-group merge");
    // A valid stack plus an undersized loose photo is also more than one group.
    let loose = s.catalog.alloc_photo_id();
    let loose_name = "LOOSE.dng".to_string();
    let loose_photo = lightcraft_catalog::Photo::new(
        loose,
        lightcraft_catalog::Source::File { path: loose_name.clone() },
        &loose_name,
        "DNG",
        96,
        64,
        "2026-01-01T00:00:00",
    );
    s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(loose_photo) }).unwrap();
    assert!(!s.merge_selection_ready(&hdr_kind, &[ids[0], loose]), "valid stack plus loose photo is more than one group");
}

/// `merge.hdrBatch` preserves the bracket set retained by `bracket_sets` when the stack
/// contains an earlier merge result: the result itself is not merged again, only its sources.
#[test]
fn hdr_batch_preserves_bracket_set_when_stack_contains_merge_result() {
    let dir = temp_dir("hdr-batch-result");
    let mut paths = Vec::new();
    for (i, b) in lightcraft_merge::synth::bracket_dngs(96, 64, &[-1.0, 0.0, 1.0]).unwrap().into_iter().enumerate() {
        let p = dir.join(format!("IMG_{i}.dng"));
        std::fs::write(&p, b).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    let r = s.execute("merge.hdr", &json!({"ids": ids, "autoSettings": false, "stack": true})).unwrap();
    let result = lightcraft_catalog::PhotoId(r["id"].as_u64().unwrap());
    // The stack now holds the merge result on top of its sources.
    let stack = s.catalog.stack_of(result).expect("merge result stacked").clone();
    assert!(stack.photos.contains(&result));
    // Batch on the result merges its sources only (3 frames), not the result itself.
    let batch = s.execute("merge.hdrBatch", &json!({"ids": [result.0], "autoSettings": false})).unwrap();
    let merged = batch["merged"].as_array().unwrap();
    assert_eq!(merged.len(), 1, "{batch}");
    assert!(batch["failed"].as_array().unwrap().is_empty(), "{batch}");
    let ev: Vec<f64> = merged[0]["ev"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert_eq!(ev.len(), 3, "only the three sources are merged: {merged:?}");
    assert!(merged[0]["path"].as_str().unwrap().ends_with("IMG_0-HDR-2.dng"), "{merged:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The desktop batch plans each bracket set as its explicit group: the ids are merged as given, in their order,
/// and the set is not expanded into the rest of its stack (here, the sources and the earlier merge result).
#[test]
fn explicit_bracket_set_is_merged_as_given() {
    let dir = temp_dir("explicit-group");
    let mut paths = Vec::new();
    for (i, b) in lightcraft_merge::synth::bracket_dngs(96, 64, &[-1.0, 0.0, 1.0]).unwrap().into_iter().enumerate() {
        let p = dir.join(format!("IMG_{i}.dng"));
        std::fs::write(&p, b).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut s = Session::new().with_fs();
    let ids = import(&mut s, paths);
    let r = s.execute("merge.hdr", &json!({"ids": ids, "autoSettings": false, "stack": true})).unwrap();
    let result = lightcraft_catalog::PhotoId(r["id"].as_u64().unwrap());
    let (kind, finish) = crate::merge::parse("merge.hdr", &json!({})).unwrap();
    let set = [lightcraft_catalog::PhotoId(ids[2]), lightcraft_catalog::PhotoId(ids[0]), lightcraft_catalog::PhotoId(ids[2])];
    let job = s.plan_merge_group(kind.clone(), finish.clone(), &set, false).unwrap();
    assert_eq!(job.sources.iter().map(|(p, _)| p.0).collect::<Vec<_>>(), vec![ids[2], ids[0]], "deduped, in the given order");
    assert!(!job.sources.iter().any(|(p, _)| *p == result), "the earlier merge result is not added back");
    let one = s.plan_merge_group(kind, finish, &set[..1], false);
    assert!(one.is_err(), "a set of one photo is not a merge");
    let _ = std::fs::remove_dir_all(&dir);
}
