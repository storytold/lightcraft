use super::*;
use lcc_catalog::{PhotoId, Source};
use lcc_develop::VignettingMapOrigin;

struct Dir(PathBuf);
impl Dir {
    fn new(tag: &str) -> Dir {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("lcc-ffl-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A Z 7 map: 85 mm f/1.8 by default, an even radial fall-off (or a gradient when `gradient`).
fn map_with(f: impl FnOnce(&mut VignettingMapOrigin), gradient: bool) -> EmbeddedVignettingMap {
    let mut source = VignettingMapOrigin {
        model: Some("NIKON Z 7".into()),
        serial: Some("1234567".into()),
        lens: Some("NIKKOR Z 85mm f/1.8 S".into()),
        focal_length: Some(85.0),
        aperture: Some(1.8),
        captured: Some("2021-03-27T10:00:00".into()),
        ..Default::default()
    };
    f(&mut source);
    let grid = [24usize, 16];
    let ev: Vec<f64> = (0..grid[0] * grid[1])
        .map(|i| {
            let (u, v) = (((i % grid[0]) as f64 + 0.5) / grid[0] as f64, ((i / grid[0]) as f64 + 0.5) / grid[1] as f64);
            if gradient {
                1.5 * u
            } else {
                let d2 = ((u - 0.5).powi(2) + ((v - 0.5) * 0.66).powi(2)) / 0.36;
                1.3 * d2 * d2 + 0.2 * d2
            }
        })
        .collect();
    EmbeddedVignettingMap::from_ev([8256, 5504], grid, None, source, &ev).unwrap()
}

fn map() -> EmbeddedVignettingMap {
    map_with(|_| {}, false)
}

fn photo(model: &str, serial: &str, lens: &str, focal: f32, aperture: f32) -> Photo {
    let mut p = Photo::new(PhotoId(1), Source::File { path: "/x/SHOT_1.NEF".into() }, "SHOT_1.NEF", "NEF", 8256, 5504, "2026-01-01");
    (p.meta.camera_model, p.meta.camera_serial, p.meta.lens) = (model.into(), serial.into(), lens.into());
    (p.meta.focal_mm, p.meta.aperture) = (Some(focal), Some(aperture));
    p
}

fn placed(size: [usize; 2]) -> SourcePlacement {
    SourcePlacement { active_size: Some(size), ..SourcePlacement::new([0.0, 0.0, 1.0, 1.0], lcc_geom::Orientation::Normal) }
}

#[test]
fn a_map_is_saved_once_and_listed_and_loaded_by_its_key() {
    let d = Dir::new("rt");
    let m = map();
    let e = save(&d.0, &m).unwrap();
    assert_eq!(e.key, format!("1234567/NIKKOR-Z-85mm-f-1.8-S_85mm_f1.8_{}", VignettingMapId::of(&m).short()));
    assert_eq!(save(&d.0, &m).unwrap(), e, "the same map again: the same entry");
    let other = map_with(|s| s.aperture = Some(4.0), false);
    save(&d.0, &other).unwrap();
    let all = list(&d.0, None);
    assert_eq!(all.len(), 2);
    assert_eq!(list(&d.0, Some("1234567")).len(), 2);
    assert!(list(&d.0, Some("someone-else")).is_empty());
    assert_eq!(load(&d.0, &e.key).map(|x| x.map), Some(m.clone()));
    assert_eq!(std::fs::read_dir(d.0.join("1234567")).unwrap().count(), 2);
    // a body without a serial goes under its model
    let anon = map_with(|s| s.serial = None, false);
    assert!(save(&d.0, &anon).unwrap().key.starts_with("NIKON-Z-7/"));
}

#[test]
fn damaged_files_are_set_aside_and_the_rest_lists() {
    let d = Dir::new("bad");
    let good = save(&d.0, &map()).unwrap();
    let folder = d.0.join("1234567");
    let mut wrong =
        LibraryFile { version: VERSION, id: VignettingMapId::of(&map()), map: map_with(|s| s.aperture = Some(8.0), false), withdrawn: false };
    std::fs::write(folder.join("wrong-id.json"), serde_json::to_vec(&wrong).unwrap()).unwrap();
    wrong.version = 9;
    wrong.id = VignettingMapId::of(&wrong.map);
    std::fs::write(folder.join("future.json"), serde_json::to_vec(&wrong).unwrap()).unwrap();
    std::fs::write(folder.join("truncated.json"), b"{\"version\":1,\"id\":").unwrap();
    std::fs::write(folder.join("huge.json"), vec![b' '; (MAX_FILE + 1) as usize]).unwrap();
    let mut grid = serde_json::to_value(LibraryFile { version: VERSION, id: good.id.clone(), map: map(), withdrawn: false }).unwrap();
    grid["map"]["grid"] = serde_json::json!([300, 300]);
    std::fs::write(folder.join("grid.json"), serde_json::to_vec(&grid).unwrap()).unwrap();
    std::fs::write(folder.join("notes.txt"), b"not a map").unwrap();
    let all = list(&d.0, None);
    assert_eq!(all.iter().map(|e| e.key.clone()).collect::<Vec<_>>(), vec![good.key.clone()]);
    for f in ["wrong-id", "future", "truncated", "huge", "grid"] {
        assert!(folder.join(format!("{f}.json.bad")).is_file(), "{f} set aside");
    }
    assert!(folder.join("notes.txt").is_file(), "not a map file: left alone");
}

#[test]
fn a_key_never_leaves_the_library() {
    let d = Dir::new("keys");
    save(&d.0, &map()).unwrap();
    std::fs::write(d.0.join("outside.json"), b"{}").unwrap();
    for key in ["../outside", "1234567/../../outside", "1234567/..", "../1234567/x", "a\\b/c", "C:/x", "nobody", "/x", "1234567/", "x/y/z"] {
        assert!(load(&d.0, key).is_none(), "{key}");
    }
}

/// Design notes §10, *Matching a map to a photo*.
#[test]
fn the_matching_table() {
    let m = map();
    let p = photo("NIKON Z 7", "1234567", "NIKKOR Z 85mm f/1.8 S", 85.0, 1.8);
    let here = placed([8256, 5504]);
    assert_eq!(match_photo(&m, &p, Some(&here)), MatchReport { fits: true, refused: None, warnings: vec![], pending: false });
    // while the photo's header is still being read (the app's worker): everything but the image area is checked
    use crate::vignetting_calibration_placement::Lookup;
    let wait = match_lookup(&m, &p, &Lookup::Pending);
    assert_eq!((wait.fits, wait.refused.clone(), wait.pending), (true, None, true));
    assert_eq!(serde_json::to_value(&wait).unwrap()["pending"], true);
    assert!(serde_json::to_value(match_photo(&m, &p, Some(&here))).unwrap().get("pending").is_none(), "only said while it is");
    let z8 = photo("NIKON Z 8", "1234567", "NIKKOR Z 85mm f/1.8 S", 85.0, 1.8);
    assert_eq!(match_lookup(&m, &z8, &Lookup::Pending).refused, Some(Refusal::AnotherCamera));
    let short = match_lookup(&m, &p, &Lookup::Known(Some(placed([6912, 5504]))));
    assert_eq!((short.refused, short.pending), (Some(Refusal::ImageArea), false));
    assert_eq!(match_lookup(&m, &p, &Lookup::Known(None)).refused, Some(Refusal::NotRaw));
    // refused: not a raw, another camera, another image area
    assert_eq!(match_photo(&m, &p, None).refused, Some(Refusal::NotRaw));
    let z8 = photo("NIKON Z 8", "1234567", "NIKKOR Z 85mm f/1.8 S", 85.0, 1.8);
    assert_eq!(match_photo(&m, &z8, Some(&here)).refused, Some(Refusal::AnotherCamera));
    let r = match_photo(&m, &p, Some(&placed([6912, 5504])));
    assert_eq!((r.fits, r.refused), (false, Some(Refusal::ImageArea)));
    // another lens: refused (even while the image area is still being read)
    let other = photo("NIKON Z 7", "7654321", "NIKKOR Z 24-70mm f/4 S", 70.0, 5.6);
    let r = match_photo(&m, &other, Some(&here));
    assert_eq!((r.fits, r.refused), (false, Some(Refusal::AnotherLens)));
    assert_eq!(match_lookup(&m, &other, &Lookup::Pending).refused, Some(Refusal::AnotherLens));
    // another body doesn't count; another aperture is refused (a third of a stop is another aperture, rounding isn't)
    let other_body = photo("NIKON Z 7", "7654321", "NIKKOR Z 85mm f/1.8 S", 85.0, 1.8);
    assert_eq!(match_photo(&m, &other_body, Some(&here)), MatchReport { fits: true, refused: None, warnings: vec![], pending: false });
    let at_f = |f: f32| match_photo(&m, &photo("NIKON Z 7", "1234567", "NIKKOR Z 85mm f/1.8 S", 85.0, f), Some(&here));
    assert_eq!(at_f(5.6).refused, Some(Refusal::Aperture));
    assert_eq!(at_f(2.0).refused, Some(Refusal::Aperture), "a third of a stop from f/1.8");
    assert!(at_f(1.8).fits && at_f(1.78).fits, "rounding");
    // the aperture recorded on one side only: refused; on neither (an adapted lens): applies, with a note
    let no_aperture = |a: f32| {
        let mut p = photo("NIKON Z 7", "1234567", "NIKKOR Z 85mm f/1.8 S", 85.0, 1.8);
        p.meta.aperture = (a > 0.0).then_some(a);
        p
    };
    assert_eq!(match_photo(&m, &no_aperture(0.0), Some(&here)).refused, Some(Refusal::ApertureUnknown));
    let blind = map_with(|o| o.aperture = None, false);
    assert_eq!(match_photo(&blind, &no_aperture(1.8), Some(&here)).refused, Some(Refusal::ApertureUnknown));
    let r = match_photo(&blind, &no_aperture(0.0), Some(&here));
    assert!(r.fits);
    assert_eq!(r.warnings, vec![Warning::ApertureUnknown]);
    // case and spacing don't count
    let loose = photo("nikon z 7 ", "1234567", " nikkor z 85mm f/1.8 s", 85.2, 1.8);
    assert!(match_photo(&m, &loose, Some(&here)).warnings.is_empty());
    // a lens either side doesn't name can't be checked: it applies, with a note
    let blank = photo("", "", "", 85.0, 1.8);
    let r = match_photo(&m, &blank, Some(&here));
    assert!(r.fits);
    assert_eq!(r.warnings, vec![Warning::LensUnknown]);
    let unnamed = map_with(|o| o.lens = None, false);
    assert_eq!(match_photo(&unnamed, &p, Some(&here)).warnings, vec![Warning::LensUnknown]);
    // a zoom: near the same focal length (5 %, at least 2 mm) it applies with a note; further, it is refused
    let zoom = map_with(|o| (o.lens, o.focal_length) = (Some("NIKKOR Z 24-70mm f/4 S".into()), Some(50.0)), false);
    let at = |f: f32| match_photo(&zoom, &photo("NIKON Z 7", "1234567", "NIKKOR Z 24-70mm f/4 S", f, 1.8), Some(&here));
    assert!(at(50.0).fits && at(50.0).warnings.iter().all(|w| !matches!(w, Warning::FocalLength { .. })));
    assert!(at(52.0).fits && at(52.0).warnings.contains(&Warning::FocalLength { diff: 2.0 }));
    assert_eq!(at(55.0).refused, Some(Refusal::FocalLength));
    assert_eq!(at(24.0).refused, Some(Refusal::FocalLength));
    // which names are zooms
    for z in ["NIKKOR Z 24-70mm f/4 S", "NIKKOR Z 100-400mm f/4.5-5.6 VR S", "RF 70–200mm F2.8"] {
        assert!(is_zoom(z), "{z}");
    }
    for p in ["NIKKOR Z 85mm f/1.8 S", "NIKKOR Z MC 105mm f/2.8 VR S", "XF 23mm F1.4 R LM WR", "Z-mount 50mm"] {
        assert!(!is_zoom(p), "{p}");
    }
    // an uneven reference warns
    let g = map_with(|_| {}, true);
    assert!(match_photo(&g, &p, Some(&here)).warnings.contains(&Warning::Uneven));
}

#[test]
fn the_best_match_comes_first() {
    let p = photo("NIKON Z 7", "1234567", "NIKKOR Z 85mm f/1.8 S", 85.0, 2.8);
    let here = placed([8256, 5504]);
    let entry = |m: EmbeddedVignettingMap| {
        let r = match_photo(&m, &p, Some(&here));
        (Entry { key: VignettingMapId::of(&m).short().to_string(), id: VignettingMapId::of(&m), map: m, withdrawn: false }, r)
    };
    let f18 = map_with(|s| s.aperture = Some(1.8), false);
    let f28 = map_with(|s| s.aperture = Some(2.8), false);
    let f28_old = map_with(|s| (s.aperture, s.captured) = (Some(2.8), Some("2019-01-01T00:00:00".into())), false);
    let f4 = map_with(|s| s.aperture = Some(4.0), false);
    let other_lens = map_with(|s| (s.aperture, s.lens) = (Some(2.8), Some("NIKKOR Z 50mm f/1.8 S".into())), false);
    let unnamed = map_with(|s| (s.aperture, s.lens) = (Some(2.8), None), false);
    let z8 = map_with(|s| (s.aperture, s.model) = (Some(2.8), Some("NIKON Z 8".into())), false);
    let mut items = vec![
        entry(z8.clone()),
        entry(f4.clone()),
        entry(f28_old.clone()),
        entry(other_lens.clone()),
        entry(unnamed.clone()),
        entry(f18.clone()),
        entry(f28.clone()),
    ];
    sort_for(&mut items, &p);
    let order: Vec<EmbeddedVignettingMap> = items.into_iter().map(|(e, _)| e.map).collect();
    // the aperture must be the same: f/1.8 and f/4 are refused like another lens or camera; among those that fit, the
    // same lens named first; two at one combination (from before one per combination) the one shot last
    assert_eq!(&order[..3], &[f28, f28_old, unnamed], "fits, the same lens, newest");
    assert!(order[3..].iter().all(|m| [&f4, &f18, &z8, &other_lens].contains(&m)));
}

/// Only one reference per combination: camera model, lens, aperture (a sixth of a stop), a zoom's focal length and the
/// image area; the body doesn't count.
#[test]
fn a_combination_is_camera_lens_aperture_and_a_zooms_focal_length() {
    let base = map();
    let same = |f: &dyn Fn(&mut lcc_develop::VignettingMapOrigin)| same_combination(&base, &map_with(|o| f(o), false));
    assert!(same(&|o| o.serial = Some("7654321".into())), "another body: the same combination");
    assert!(same(&|o| o.captured = Some("2026-01-01T00:00:00".into())));
    assert!(same(&|o| o.aperture = o.aperture.map(|a| a * 1.01)), "rounding");
    assert!(!same(&|o| o.aperture = Some(2.0)), "a third of a stop");
    assert!(!same(&|o| o.aperture = None));
    assert!(!same(&|o| o.lens = Some("NIKKOR Z 50mm f/1.8 S".into())));
    assert!(!same(&|o| o.model = Some("NIKON Z 8".into())));
    let zoom = |f: f64| map_with(|o| (o.lens, o.focal_length) = (Some("NIKKOR Z 24-70mm f/4 S".into()), Some(f)), false);
    assert!(same_combination(&zoom(50.0), &zoom(51.0)));
    assert!(!same_combination(&zoom(50.0), &zoom(70.0)));
    let mut dx = map();
    dx.sensor = [5408, 3600];
    assert!(!same_combination(&base, &dx), "another image area: kept apart for now");
}

/// Image areas: a full-frame reference covers a crop on the same sensor, not the other way round; areas on another
/// sensor size or partly overlapping cover neither.
#[test]
fn a_full_frame_reference_covers_a_crop_not_the_other_way_round() {
    let with = |window: [usize; 6]| {
        let mut m = map();
        (m.sensor, m.window) = ([window[4], window[5]], Some(window));
        m
    };
    let fx = with([8256, 5504, 0, 0, 8256, 5504]);
    let dx = with([8256, 5504, 1424, 952, 5408, 3600]);
    let wide = with([8256, 5504, 0, 0, 8256, 2000]); // a strip along the top: it overlaps the DX area partly
    assert!(covers(&fx, &dx) && !covers(&dx, &fx));
    assert!(covers(&fx, &fx) && covers(&dx, &dx));
    assert!(!covers(&dx, &wide) && !covers(&wide, &dx), "partly overlapping");
    assert!(!covers(&fx, &with([8288, 5520, 0, 0, 8288, 5520])), "another sensor");
    assert!(same_setup(&fx, &dx) && !same_combination(&fx, &dx));
    // a map without a window covers its own size as a full sensor
    let bare = map();
    assert_eq!(coverage(&bare), (bare.sensor, [0, 0, bare.sensor[0], bare.sensor[1]]));
}
