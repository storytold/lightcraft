//! `vignettingCalibration.*`: making a map from a reference raw, listing, applying, removing, and what the Optics panel reads,
//! on synthetic diffuser shots (Bayer DNGs with a known fall-off).

use std::path::PathBuf;

use lcc_color::Mat3;
use lcc_geom::Orientation;
use lcc_meta::Metadata;
use lcc_raw::{BlackLevel, Cfa, ColorData, DngWriteOptions, OpcodeLists, RawData, RawFormat, RawImage, Rect};
use serde_json::{Value, json};

use crate::Session;

const W: usize = 960;
const H: usize = 640;

fn temp_dir(tag: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("lcc-ffc-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// EV darker than the centre at `(x, y)`.
fn falloff(x: f64, y: f64) -> f64 {
    let (dx, dy) = (x / W as f64 - 0.5, (y / H as f64 - 0.5) * H as f64 / W as f64);
    let d2 = (dx * dx + dy * dy) / (0.25 + 0.25 * (H as f64 / W as f64).powi(2));
    1.1 * d2 * d2 + 0.25 * d2
}

/// A Bayer DNG of an even grey field seen through the fall-off, at `level` of white in the centre (`seed` varies
/// the noise so two shots differ).
fn diffuser_dng(level: f64, seed: u64) -> Vec<u8> {
    shot_dng(level, seed, |_, _| 0.0)
}

/// [`diffuser_dng`] darker by `extra(x, y)` EV on top of the fall-off (something in front of the lens).
fn shot_dng(level: f64, seed: u64, extra: impl Fn(f64, f64) -> f64) -> Vec<u8> {
    let cfa = Cfa::bayer("RGGB").unwrap();
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    let data = (0..W * H)
        .map(|i| {
            let (x, y) = (i % W, i / W);
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let noise = ((state >> 40) as f64 / (1u64 << 24) as f64 - 0.5) * 0.004;
            let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
            let v = level * (-(falloff(fx, fy) + extra(fx, fy))).exp2() + noise;
            (v.clamp(0.0, 1.0) * 4000.0 + 64.0).min(4095.0) as u16
        })
        .collect();
    let _ = cfa.color_at(0, 0);
    let raw = RawImage {
        format: RawFormat::Dng,
        width: W,
        height: H,
        cpp: 1,
        data: RawData::U16(data),
        cfa: Some(cfa),
        bits: 12,
        black: BlackLevel::uniform(64.0),
        white: vec![4095.0],
        active_area: Rect::new(0, 0, W, H),
        crop: Rect::new(0, 0, W, H),
        orientation: Orientation::Normal,
        color: ColorData {
            illuminant: [21, 0],
            color_matrix: [Some(Mat3::IDENTITY), None],
            as_shot_neutral: Some([1.0, 1.0, 1.0]),
            ..Default::default()
        },
        wb_multipliers: None,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: OpcodeLists::default(),
        metadata: Metadata { make: Some("Test".into()), model: Some("Flat Cam".into()), ..Default::default() },
        lens_metadata: Default::default(),
    };
    lcc_raw::write_dng(&raw, &DngWriteOptions::default()).unwrap()
}

/// A session over a fresh folder of shots: the reference (`REF.dng`), three photos and a JPEG-like non-raw stand-in,
/// with the flat-field library in `library` (or none).
struct Bench {
    s: Session,
    dir: PathBuf,
    reference: u64,
    photos: Vec<u64>,
}

fn bench(tag: &str, library: bool) -> Bench {
    let dir = temp_dir(tag);
    let mut files = vec![("REF.dng", diffuser_dng(0.5, 1))];
    for (i, n) in ["A.dng", "B.dng", "C.dng"].into_iter().enumerate() {
        files.push((n, diffuser_dng(0.3, 10 + i as u64)));
    }
    for (n, b) in &files {
        std::fs::write(dir.join(n), b).unwrap();
    }
    let mut s = Session::new().with_fs();
    s.vignetting_calibration_dir_override = Some(library.then(|| dir.join("library")));
    let mut ids = Vec::new();
    for (n, _) in &files {
        let r = s.execute("library.import", &json!({"paths": [dir.join(n).to_string_lossy()]})).unwrap();
        ids.push(r["imported"][0].as_u64().unwrap());
    }
    Bench { s, dir, reference: ids[0], photos: ids[1..].to_vec() }
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn select(s: &mut Session, ids: &[u64]) {
    s.execute("library.select", &json!({"ids": ids, "active": ids[0]})).unwrap();
}

fn vignetting_calibration(s: &Session, id: u64) -> Option<String> {
    s.catalog.photo(lcc_catalog::PhotoId(id)).and_then(|p| p.develop.optics.vignetting_calibration.as_ref().map(|i| i.to_string()))
}

#[test]
fn create_measures_the_reference_keeps_the_map_and_applies_it() {
    let mut b = bench("create", true);
    select(&mut b.s, &[b.reference]);
    let r = b.s.execute("vignettingCalibration.create", &json!({})).unwrap();
    let id = r["map"]["id"].as_str().unwrap().to_string();
    assert_eq!(r["map"]["fileName"], json!("REF.dng"));
    assert!((r["falloff"].as_f64().unwrap() - falloff(0.0, 0.0)).abs() < 0.15, "{r}");
    assert_eq!(r["uneven"], json!(false));
    // the exposure report: the middle at 0.5 of white (green), the darkest corner by the fall-off below it, nothing
    // clipped, no note
    let e = &r["exposure"];
    assert!((e["centre"][1].as_f64().unwrap() - 0.5).abs() < 0.03, "{e}");
    let darkest = 0.5 * (-falloff(0.0, 0.0)).exp2();
    assert!((e["darkest"].as_f64().unwrap() - darkest).abs() < 0.05, "{e} vs {darkest}");
    assert_eq!((e["clipped"].as_f64(), e["verdict"].as_str()), (Some(0.0), Some("ok")));
    // on the reference itself, at 100, in the catalog linked to it, and in the library
    assert_eq!(vignetting_calibration(&b.s, b.reference).as_deref(), Some(id.as_str()));
    let fid = lcc_develop::VignettingMapId::parse(&id).unwrap();
    assert_eq!(b.s.catalog.vignetting_map(&fid).and_then(|x| x.photo).map(|p| p.0), Some(b.reference));
    assert_eq!(lcc_catalog::reference_photo(&b.s.catalog, &fid).map(|p| p.0), Some(b.reference));
    let key = r["key"].as_str().unwrap();
    assert!(crate::vignetting_calibration_library::load(&b.dir.join("library"), key).is_some(), "{key}");
    // one undo step takes the settings back; the map stays in the catalog
    b.s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(vignetting_calibration(&b.s, b.reference), None);
    assert!(b.s.catalog.vignetting_map(&fid).is_some());
}

#[test]
fn a_clipped_or_dark_reference_is_refused_and_changes_nothing() {
    let mut b = bench("refused", true);
    for (name, level, says) in [("CLIP.dng", 3.0, "clipped"), ("DARK.dng", 0.004, "too dark")] {
        std::fs::write(b.dir.join(name), diffuser_dng(level, 5)).unwrap();
        let r = b.s.execute("library.import", &json!({"paths": [b.dir.join(name).to_string_lossy()]})).unwrap();
        let id = r["imported"][0].as_u64().unwrap();
        select(&mut b.s, &[id]);
        let e = b.s.execute("vignettingCalibration.create", &json!({})).unwrap_err().to_string();
        assert!(e.contains(says), "{name}: {e}");
        assert_eq!(vignetting_calibration(&b.s, id), None);
    }
    assert_eq!(b.s.catalog.vignetting_maps().count(), 0);
    assert!(!b.dir.join("library").exists() || crate::vignetting_calibration_library::list(&b.dir.join("library"), None).is_empty());
}

#[test]
fn list_apply_amount_and_remove() {
    let mut b = bench("apply", true);
    select(&mut b.s, &[b.reference]);
    let made = b.s.execute("vignettingCalibration.create", &json!({})).unwrap();
    let id = made["map"]["id"].as_str().unwrap().to_string();
    let target = b.photos[0];
    let listed = b.s.execute("vignettingCalibration.list", &json!({"id": target})).unwrap();
    let first = &listed["maps"][0];
    assert_eq!((first["id"].as_str(), first["inCatalog"].as_bool(), first["match"]["fits"].as_bool()), (Some(id.as_str()), Some(true), Some(true)));
    // on a selection, one undo step
    select(&mut b.s, &b.photos.clone());
    let r = b.s.execute("vignettingCalibration.apply", &json!({"map": id})).unwrap();
    assert_eq!(r["applied"].as_array().map(Vec::len), Some(3), "{r}");
    assert!(b.photos.iter().all(|p| vignetting_calibration(&b.s, *p).as_deref() == Some(id.as_str())));
    b.s.execute("edit.undo", &json!({})).unwrap();
    assert!(b.photos.iter().all(|p| vignetting_calibration(&b.s, *p).is_none()), "one step for all three");
    // by explicit ids, by library key, without a selection
    b.s.execute("library.select", &json!({"ids": []})).unwrap();
    let key = made["key"].as_str().unwrap();
    b.s.execute("vignettingCalibration.apply", &json!({"key": key, "ids": [target]})).unwrap();
    assert_eq!(vignetting_calibration(&b.s, target).as_deref(), Some(id.as_str()));
    // the amount, then Remove keeps it
    select(&mut b.s, &[target]);
    b.s.execute("develop.set", &json!({"values": {"optics.vignettingCalibrationAmount": 70}})).unwrap();
    b.s.execute("vignettingCalibration.remove", &json!({"ids": [target]})).unwrap();
    let d = b.s.catalog.photo(lcc_catalog::PhotoId(target)).unwrap().develop.clone();
    assert_eq!((d.optics.vignetting_calibration.clone(), d.optics.vignetting_calibration_amount), (None, 70.0));
}

#[test]
fn the_panel_reads_the_map_its_reference_and_what_differs() {
    let mut b = bench("photo", true);
    select(&mut b.s, &[b.reference]);
    let made = b.s.execute("vignettingCalibration.create", &json!({})).unwrap();
    let id = made["map"]["id"].as_str().unwrap().to_string();
    let target = b.photos[1];
    b.s.execute("vignettingCalibration.apply", &json!({"map": id, "ids": [target]})).unwrap();
    let v = b.s.execute("vignettingCalibration.photo", &json!({"id": target})).unwrap();
    assert_eq!(v["map"]["id"], json!(id));
    assert_eq!((v["missing"].as_bool(), v["match"]["fits"].as_bool()), (Some(false), Some(true)));
    assert_eq!(v["reference"]["photo"], json!(b.reference));
    assert_eq!(v["profileVignettingReplaced"], json!(false));
    // Show Reference makes it the active photo
    let shown = b.s.execute("vignettingCalibration.showReference", &json!({"id": target})).unwrap();
    assert_eq!(shown["id"], json!(b.reference));
    assert_eq!(b.s.active().map(|p| p.0), Some(b.reference));
    // the reference photo removed: the map still applies and the panel says the reference is gone
    b.s.execute("photo.delete", &json!({"ids": [b.reference]})).unwrap();
    let v = b.s.execute("vignettingCalibration.photo", &json!({"id": target})).unwrap();
    assert_eq!((v["reference"]["inCatalog"].as_bool(), v["match"]["fits"].as_bool()), (Some(false), Some(true)));
    assert!(b.s.execute("vignettingCalibration.showReference", &json!({"id": target})).is_err());
    assert!(b.s.job_info(b.s.catalog.photo(lcc_catalog::PhotoId(target)).unwrap()).vignetting_maps.is_some());
}

#[test]
fn without_a_library_folder_the_catalogs_maps_are_listed() {
    let mut b = bench("nolib", false);
    select(&mut b.s, &[b.reference]);
    let made = b.s.execute("vignettingCalibration.create", &json!({})).unwrap();
    assert_eq!(made["key"], Value::Null);
    let listed = b.s.execute("vignettingCalibration.list", &json!({"id": b.photos[0]})).unwrap();
    assert_eq!(listed["maps"][0]["id"], made["map"]["id"]);
    assert!(b.s.execute("vignettingCalibration.apply", &json!({"key": "a/b", "ids": [b.photos[0]]})).is_err(), "no library to take a key from");
}

#[test]
fn hostile_arguments_are_refused() {
    let mut b = bench("hostile", true);
    let p = b.photos[0];
    for (c, args) in [
        ("vignettingCalibration.apply", json!({"map": "not an id", "ids": [p]})),
        ("vignettingCalibration.apply", json!({"map": "0".repeat(64), "ids": [p]})),
        ("vignettingCalibration.apply", json!({"map": {"grid": [4, 3]}, "ids": [p]})),
        ("vignettingCalibration.apply", json!({"key": "../../etc/passwd", "ids": [p]})),
        ("vignettingCalibration.create", json!({"id": 999_999})),
        ("vignettingCalibration.create", json!({"id": "x"})),
        ("vignettingCalibration.photo", json!({"id": -1})),
    ] {
        assert!(b.s.execute(c, &args).is_err(), "{c} {args}");
    }
    // without a map Apply picks one per photo: here there is none, so nothing changes and the photo is listed
    let r = b.s.execute("vignettingCalibration.apply", &json!({"ids": [p]})).unwrap();
    assert_eq!(r["skipped"], json!([{"id": p, "reason": "noReference"}]), "{r}");
    assert_eq!(b.s.catalog.vignetting_maps().count(), 0);
}

/// Photo `id`'s lens name as its metadata gives it, shot at f/1.8 like [`lens_map`] (the synthetic DNGs name neither).
fn set_lens(s: &mut Session, id: u64, lens: &str) {
    let pid = lcc_catalog::PhotoId(id);
    let mut p = (**s.catalog.photo(pid).unwrap()).clone();
    (p.meta.lens, p.meta.aperture) = (lens.into(), Some(1.8));
    s.catalog.apply(lcc_catalog::Op::RemovePhoto { id: pid }).unwrap();
    s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
}

/// A map of this bench's camera made with `lens`, in portable form (as an agent or a preset gives it).
fn lens_map(lens: Option<&str>) -> lcc_develop::EmbeddedVignettingMap {
    let source = lcc_develop::VignettingMapOrigin {
        model: Some("Flat Cam".into()),
        lens: lens.map(str::to_string),
        focal_length: Some(85.0),
        aperture: Some(1.8),
        ..Default::default()
    };
    let grid = [24usize, 16];
    let ev: Vec<f64> = (0..grid[0] * grid[1])
        .map(|i| falloff(((i % grid[0]) as f64 + 0.5) * W as f64 / grid[0] as f64, ((i / grid[0]) as f64 + 0.5) * H as f64 / grid[1] as f64))
        .map(|e| e - falloff(W as f64 / 2.0, H as f64 / 2.0))
        .collect();
    lcc_develop::EmbeddedVignettingMap::from_ev([W, H], grid, None, source, &ev).unwrap()
}

/// Apply to Selected Photos, Sync and Paste put a map only on photos of its lens: another lens is skipped and
/// reported; a photo that names no lens can't be checked and takes it.
#[test]
fn another_lens_is_skipped_by_apply_sync_and_paste() {
    let mut b = bench("lens-skip", false);
    let [a, other, unnamed] = [b.photos[0], b.photos[1], b.photos[2]];
    set_lens(&mut b.s, a, "NIKKOR Z 85mm f/1.8 S");
    set_lens(&mut b.s, other, "NIKKOR Z 50mm f/1.8 S");
    set_lens(&mut b.s, unnamed, "");
    let map = lens_map(Some("NIKKOR Z 85mm f/1.8 S"));
    let id = lcc_develop::VignettingMapId::of(&map).to_string();
    // Apply: the 50 mm photo is skipped and listed
    let r = b.s.execute("vignettingCalibration.apply", &json!({"map": serde_json::to_value(&map).unwrap(), "ids": [a, other, unnamed]})).unwrap();
    assert_eq!(r["applied"].as_array().map(Vec::len), Some(2), "{r}");
    assert_eq!(r["skipped"], json!([{"id": other, "reason": "anotherLens"}]));
    assert_eq!(vignetting_calibration(&b.s, a).as_deref(), Some(id.as_str()));
    assert_eq!(vignetting_calibration(&b.s, other), None);
    assert_eq!(vignetting_calibration(&b.s, unnamed).as_deref(), Some(id.as_str()), "a photo without a lens name takes it");
    // the panel's list for the 50 mm photo: the map is refused there
    let listed = b.s.execute("vignettingCalibration.list", &json!({"id": other})).unwrap();
    assert_eq!(listed["maps"][0]["match"]["refused"], json!("anotherLens"));
    b.s.execute("vignettingCalibration.remove", &json!({"ids": [unnamed]})).unwrap();
    // Sync from the 85 mm photo: the map stays off the 50 mm photo, the rest of the settings go
    select(&mut b.s, &[a, other, unnamed]);
    b.s.execute("develop.set", &json!({"values": {"light.exposure": 0.5}, "ids": [a]})).unwrap();
    let r = b.s.execute("develop.sync", &json!({"groups": ["light", "optics"]})).unwrap();
    assert_eq!(r["vignettingCalibrationSkipped"], json!([{"id": other, "reason": "anotherLens"}]), "{r}");
    assert_eq!(vignetting_calibration(&b.s, other), None);
    assert_eq!(vignetting_calibration(&b.s, unnamed).as_deref(), Some(id.as_str()));
    assert_eq!(b.s.develop_of(lcc_catalog::PhotoId(other)).unwrap().light.exposure, 0.5, "the other settings were synced");
    // Paste: the same
    b.s.execute("edit.undo", &json!({})).unwrap();
    select(&mut b.s, &[a]);
    b.s.execute("develop.copy", &json!({})).unwrap();
    let r = b.s.execute("develop.paste", &json!({"ids": [other, unnamed]})).unwrap();
    assert_eq!(r["vignettingCalibrationSkipped"], json!([{"id": other, "reason": "anotherLens"}]), "{r}");
    assert_eq!(vignetting_calibration(&b.s, other), None);
}

/// Create says how the reference was exposed: well under the histogram's middle, or close to clipping, it still makes
/// the map but says so. (Both are of one combination: the second replaces the first.)
#[test]
fn create_reports_an_underexposed_or_nearly_clipped_reference() {
    let mut b = bench("exposure", false);
    for (name, level, verdict) in [("LOW.dng", 0.15, "under"), ("HIGH.dng", 0.9, "nearClip")] {
        std::fs::write(b.dir.join(name), diffuser_dng(level, 7)).unwrap();
        let r = b.s.execute("library.import", &json!({"paths": [b.dir.join(name).to_string_lossy()]})).unwrap();
        let id = r["imported"][0].as_u64().unwrap();
        select(&mut b.s, &[id]);
        let r = b.s.execute("vignettingCalibration.create", &json!({"replace": true})).unwrap();
        assert_eq!(r["exposure"]["verdict"], json!(verdict), "{name}: {r}");
        assert!(vignetting_calibration(&b.s, id).is_some(), "{name}: the map is made all the same");
    }
}

/// A map of this bench's camera: `lens`, `aperture`, made on body `serial` at `captured`.
fn map_of(lens: Option<&str>, aperture: f64, serial: &str, captured: &str) -> lcc_develop::EmbeddedVignettingMap {
    let mut m = lens_map(lens);
    m.source.aperture = Some(aperture);
    m.source.serial = Some(serial.into());
    m.source.captured = Some(captured.into());
    m
}

/// Photo `id`'s lens, aperture and body as its metadata gives them.
fn set_meta(s: &mut Session, id: u64, lens: &str, aperture: f32, serial: &str) {
    let pid = lcc_catalog::PhotoId(id);
    let mut p = (**s.catalog.photo(pid).unwrap()).clone();
    (p.meta.lens, p.meta.aperture, p.meta.camera_serial, p.meta.focal_mm) = (lens.into(), Some(aperture), serial.into(), Some(85.0));
    s.catalog.apply(lcc_catalog::Op::RemovePhoto { id: pid }).unwrap();
    s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
}

/// Apply without a map picks each photo's reference by itself: the same camera and lens (named on both) and the same
/// aperture, from any body of the model; nothing at another aperture (said as such) or for another lens, or a photo
/// that names no lens.
#[test]
fn apply_picks_each_photos_reference() {
    let mut b = bench("auto", false);
    let z85 = "NIKKOR Z 85mm f/1.8 S";
    let maps = [
        ("f/1.8", map_of(Some(z85), 1.8, "2", "2024-01-01T00:00:00")),
        ("f/2.8", map_of(Some(z85), 2.8, "1", "2025-01-01T00:00:00")),
        ("50 mm", map_of(Some("NIKKOR Z 50mm f/1.8 S"), 1.8, "1", "2026-06-01T00:00:00")),
        ("no lens named", map_of(None, 1.8, "1", "2026-07-01T00:00:00")),
    ];
    let mut id_of = std::collections::BTreeMap::new();
    for (name, m) in &maps {
        let id = lcc_develop::VignettingMapId::of(m);
        let record = Box::new(lcc_catalog::VignettingMapRecord { map: m.clone(), photo: None, added: String::new(), withdrawn: false });
        b.s.catalog.apply(lcc_catalog::Op::AddVignettingMap { id: id.clone(), record }).unwrap();
        id_of.insert(id.to_string(), *name);
    }
    let p = b.photos[0];
    let pick = |s: &mut Session| -> String {
        let r = s.execute("vignettingCalibration.photo", &json!({"id": p})).unwrap();
        r["auto"]["id"].as_str().map(|i| id_of.get(i).copied().unwrap_or("?").to_string()).unwrap_or_else(|| format!("none: {}", r["auto"]["none"]))
    };
    for (lens, aperture, serial, want) in [
        (z85, 1.8, "1", "f/1.8"), // made on body 2: the body doesn't count
        (z85, 2.8, "3", "f/2.8"),
        (z85, 2.0, "1", "none: \"noReferenceAtAperture\""), // a third of a stop from f/1.8: another aperture
        (z85, 13.0, "1", "none: \"noReferenceAtAperture\""),
        ("NIKKOR Z 50mm f/1.8 S", 1.8, "1", "50 mm"),
        ("NIKKOR Z 24-70mm f/4 S", 4.0, "1", "none: \"noReference\""),
        ("", 1.8, "1", "none: \"noReference\""), // the lens can't be checked: not picked by itself
    ] {
        set_meta(&mut b.s, p, lens, aperture, serial);
        assert_eq!(pick(&mut b.s), want, "{lens} f/{aperture} body {serial}");
    }
    // the panel's reason carries the photo's aperture
    set_meta(&mut b.s, p, z85, 13.0, "1");
    let r = b.s.execute("vignettingCalibration.photo", &json!({"id": p})).unwrap();
    assert_eq!(r["auto"]["aperture"].as_f64().map(|a| (a * 10.0).round()), Some(130.0));
    // Apply on a selection: each photo its own reference, the others listed with why
    let [a, c] = [b.photos[1], b.photos[2]];
    set_meta(&mut b.s, a, z85, 2.8, "1");
    set_meta(&mut b.s, c, "NIKKOR Z 24-70mm f/4 S", 4.0, "1");
    set_meta(&mut b.s, p, z85, 1.8, "1");
    std::fs::write(b.dir.join("D.dng"), diffuser_dng(0.3, 40)).unwrap();
    let d = b.s.execute("library.import", &json!({"paths": [b.dir.join("D.dng").to_string_lossy()]})).unwrap()["imported"][0].as_u64().unwrap();
    set_meta(&mut b.s, d, z85, 4.0, "1");
    select(&mut b.s, &[p, a, c, d]);
    let r = b.s.execute("vignettingCalibration.apply", &json!({})).unwrap();
    let applied: Vec<(u64, &str)> =
        r["applied"].as_array().unwrap().iter().map(|x| (x["id"].as_u64().unwrap(), id_of[x["map"].as_str().unwrap()])).collect();
    assert_eq!(applied, vec![(p, "f/1.8"), (a, "f/2.8")], "{r}");
    assert_eq!(r["skipped"], json!([{"id": c, "reason": "noReference"}, {"id": d, "reason": "noReferenceAtAperture"}]));
    // one undo step for all of it
    b.s.execute("edit.undo", &json!({})).unwrap();
    assert!([p, a].iter().all(|x| vignetting_calibration(&b.s, *x).is_none()));
}

/// A withdrawn reference isn't picked by Apply (the one before it is), the photos that use it keep it, and withdrawing
/// is one undo step; `references` lists both with their state.
#[test]
fn a_withdrawn_reference_is_skipped_by_apply_and_kept_by_its_photos() {
    let mut b = bench("withdraw", false);
    let z85 = "NIKKOR Z 85mm f/1.8 S";
    let older = map_of(Some(z85), 1.8, "1", "2024-01-01T00:00:00");
    let newer = map_of(Some(z85), 1.8, "1", "2025-01-01T00:00:00");
    for m in [&older, &newer] {
        let record = Box::new(lcc_catalog::VignettingMapRecord { map: m.clone(), photo: None, added: String::new(), withdrawn: false });
        b.s.catalog.apply(lcc_catalog::Op::AddVignettingMap { id: lcc_develop::VignettingMapId::of(m), record }).unwrap();
    }
    let (older, newer) = (lcc_develop::VignettingMapId::of(&older).to_string(), lcc_develop::VignettingMapId::of(&newer).to_string());
    let [p, q] = [b.photos[0], b.photos[1]];
    set_meta(&mut b.s, p, z85, 1.8, "1");
    set_meta(&mut b.s, q, z85, 1.8, "1");
    let auto = |s: &mut Session, id: u64| s.execute("vignettingCalibration.photo", &json!({"id": id})).unwrap()["auto"]["id"].clone();
    assert_eq!(auto(&mut b.s, p), json!(newer));
    select(&mut b.s, &[p]);
    b.s.execute("vignettingCalibration.apply", &json!({})).unwrap();
    assert_eq!(vignetting_calibration(&b.s, p).as_deref(), Some(newer.as_str()));
    let r = b.s.execute("vignettingCalibration.withdraw", &json!({"map": newer})).unwrap();
    assert_eq!(r["changed"], json!([newer]));
    assert_eq!(auto(&mut b.s, q), json!(older), "the reference before it");
    assert_eq!(vignetting_calibration(&b.s, p).as_deref(), Some(newer.as_str()), "the photo keeps its map");
    let photo = b.s.execute("vignettingCalibration.photo", &json!({"id": p})).unwrap();
    assert_eq!(photo["map"]["withdrawn"], json!(true));
    // listed with their state
    let all = b.s.execute("vignettingCalibration.references", &json!({})).unwrap();
    let states: Vec<(Value, Value)> = all["references"].as_array().unwrap().iter().map(|r| (r["id"].clone(), r["withdrawn"].clone())).collect();
    assert_eq!(states, vec![(json!(newer), json!(true)), (json!(older), json!(false))]);
    let withdrawn = b.s.execute("vignettingCalibration.references", &json!({"withdrawn": true})).unwrap();
    assert_eq!(withdrawn["references"].as_array().map(Vec::len), Some(1));
    // once more: nothing to change; then undo puts it back in use
    assert_eq!(b.s.execute("vignettingCalibration.withdraw", &json!({"map": newer})).unwrap()["changed"], json!([]));
    b.s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(auto(&mut b.s, q), json!(newer));
    // restore on one in use changes nothing; a photo that is no reference is refused
    assert_eq!(b.s.execute("vignettingCalibration.restore", &json!({"map": newer})).unwrap()["changed"], json!([]));
    assert!(b.s.execute("vignettingCalibration.withdraw", &json!({"id": p})).is_err());
    assert!(b.s.execute("vignettingCalibration.references", &json!({"withdrawn": "yes"})).is_err());
}

/// Undoing a Create withdraws its reference (the map stays for the photos that use it), in the catalog and in the
/// library file; redo puts it back. The reference photo says what it is, and the photo's own commands withdraw and
/// restore every map measured from it.
#[test]
fn undoing_a_create_withdraws_the_reference_and_the_library_follows() {
    let mut b = bench("undocreate", true);
    select(&mut b.s, &[b.reference]);
    let r = b.s.execute("vignettingCalibration.create", &json!({})).unwrap();
    let (id, key) = (r["map"]["id"].as_str().unwrap().to_string(), r["key"].as_str().unwrap().to_string());
    let fid = lcc_develop::VignettingMapId::parse(&id).unwrap();
    let lib_dir = b.dir.join("library");
    let library = |key: &str| crate::vignetting_calibration_library::load(&lib_dir, key).map(|e| e.withdrawn);
    let state = |s: &Session| s.catalog.vignetting_map(&fid).map(|r| r.withdrawn);
    assert_eq!((state(&b.s), library(&key)), (Some(false), Some(false)));
    let reference = lcc_catalog::PhotoId(b.reference);
    assert_eq!(b.s.catalog.vignetting_reference_state(reference), Some(lcc_catalog::ReferenceState::Reference));
    let photo = b.s.execute("vignettingCalibration.photo", &json!({})).unwrap();
    assert_eq!(
        photo["referenceOf"].as_array().map(|a| a.iter().map(|m| (m["id"].clone(), m["withdrawn"].clone())).collect::<Vec<_>>()),
        Some(vec![(json!(id), json!(false))])
    );
    b.s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!((state(&b.s), library(&key)), (Some(true), Some(true)), "undo withdrew it");
    assert_eq!(b.s.catalog.vignetting_reference_state(reference), Some(lcc_catalog::ReferenceState::Withdrawn));
    b.s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!((state(&b.s), library(&key)), (Some(false), Some(false)), "redo put it back");
    // the reference photo's own commands
    assert_eq!(b.s.execute("vignettingCalibration.withdraw", &json!({})).unwrap()["changed"], json!([id]));
    assert_eq!((state(&b.s), library(&key)), (Some(true), Some(true)));
    assert_eq!(b.s.execute("vignettingCalibration.restore", &json!({})).unwrap()["changed"], json!([id]));
    assert_eq!((state(&b.s), library(&key)), (Some(false), Some(false)));
    // a map only in the library: its file
    let mut other = lens_map(Some("NIKKOR Z 50mm f/1.8 S"));
    other.source.captured = Some("2020-01-01T00:00:00".into());
    let entry = crate::vignetting_calibration_library::save(&lib_dir, &other).unwrap();
    let r = b.s.execute("vignettingCalibration.withdraw", &json!({"map": entry.id.as_str()})).unwrap();
    assert_eq!(r["library"], json!(true));
    assert_eq!(library(&entry.key), Some(true));
    // a Create of the same reference again puts a withdrawn map back in use
    b.s.execute("vignettingCalibration.withdraw", &json!({})).unwrap();
    b.s.execute("vignettingCalibration.create", &json!({})).unwrap();
    assert_eq!((state(&b.s), library(&key)), (Some(false), Some(false)));
}

/// Create checks that the raw looks like a flat-field reference: something in the frame (a bird against a sky) is
/// refused with the reason and changes nothing; a faint spot (dust on a screen) is Borderline: refused without
/// `force`, created with it; a clean reference passes and says so.
#[test]
fn create_refuses_what_isnt_a_flat_field_and_asks_first_when_unsure() {
    let mut b = bench("plausible", true);
    let bird = b.dir.join("BIRD.dng");
    std::fs::write(&bird, shot_dng(0.4, 7, |x, y| if (600.0..630.0).contains(&x) && (200.0..220.0).contains(&y) { 2.0 } else { 0.0 })).unwrap();
    let dust = b.dir.join("DUST.dng");
    std::fs::write(&dust, shot_dng(0.4, 8, |x, y| if (300.0..330.0).contains(&x) && (300.0..320.0).contains(&y) { 0.08 } else { 0.0 })).unwrap();
    let mut import =
        |p: &std::path::Path| b.s.execute("library.import", &json!({"paths": [p.to_string_lossy()]})).unwrap()["imported"][0].as_u64().unwrap();
    let (bird, dust) = (import(&bird), import(&dust));
    let before = b.s.catalog.vignetting_maps().count();
    let e = b.s.execute("vignettingCalibration.create", &json!({"id": bird})).unwrap_err().to_string();
    assert!(e.contains("doesn't look like a flat-field reference") && e.contains("detail"), "{e}");
    assert_eq!(b.s.catalog.vignetting_maps().count(), before, "nothing kept");
    assert_eq!(vignetting_calibration(&b.s, bird), None);
    let e = b.s.execute("vignettingCalibration.create", &json!({"id": dust})).unwrap_err().to_string();
    assert!(e.contains("force"), "{e}");
    assert_eq!(b.s.catalog.vignetting_maps().count(), before);
    let r = b.s.execute("vignettingCalibration.create", &json!({"id": dust, "force": true})).unwrap();
    assert_eq!(r["plausibility"]["verdict"], json!("borderline"));
    assert_eq!(r["plausibility"]["reasons"], json!(["detail"]));
    assert!(vignetting_calibration(&b.s, dust).is_some());
    assert!(b.s.execute("vignettingCalibration.create", &json!({"id": dust, "force": "yes"})).is_err());
    // the clean reference is of the same combination as the dusty one: replaced only when asked
    let e = b.s.execute("vignettingCalibration.create", &json!({"id": b.reference})).unwrap_err().to_string();
    assert!(e.contains("already a reference") && e.contains("DUST.dng") && e.contains("replace: true"), "{e}");
    let r = b.s.execute("vignettingCalibration.create", &json!({"id": b.reference, "replace": true})).unwrap();
    assert_eq!(r["plausibility"]["verdict"], json!("pass"), "{r}");
    assert!(r["plausibility"]["metrics"]["detailMax"].as_f64().unwrap() < 0.05);
}

/// One reference in use per combination: Create of another one for the same camera, lens and aperture asks first;
/// Replace withdraws the old one in the same undo step (undo puts it back), photos that use it keep it, and Apply then
/// uses the new one. One only in the library (from another catalog) is withdrawn there too.
#[test]
fn create_replaces_the_reference_of_the_same_combination() {
    let mut b = bench("replace", true);
    let lib_dir = b.dir.join("library");
    let library = |key: &str| crate::vignetting_calibration_library::load(&lib_dir, key).map(|e| e.withdrawn);
    let first = b.s.execute("vignettingCalibration.create", &json!({"id": b.reference})).unwrap();
    let (old, old_key) =
        (lcc_develop::VignettingMapId::parse(first["map"]["id"].as_str().unwrap()).unwrap(), first["key"].as_str().unwrap().to_string());
    // a photo uses the old one
    let user = b.photos[0];
    b.s.execute("vignettingCalibration.apply", &json!({"map": old.as_str(), "ids": [user]})).unwrap();
    // a second shot of the same setup
    let again = b.dir.join("REF_0002.dng");
    std::fs::write(&again, diffuser_dng(0.5, 99)).unwrap();
    let second = b.s.execute("library.import", &json!({"paths": [again.to_string_lossy()]})).unwrap()["imported"][0].as_u64().unwrap();
    let before = b.s.catalog.vignetting_maps().count();
    let e = b.s.execute("vignettingCalibration.create", &json!({"id": second})).unwrap_err().to_string();
    assert!(e.contains("already a reference") && e.contains("REF.dng"), "{e}");
    assert_eq!(b.s.catalog.vignetting_maps().count(), before, "Cancel creates nothing");
    let r = b.s.execute("vignettingCalibration.create", &json!({"id": second, "replace": true})).unwrap();
    let new = lcc_develop::VignettingMapId::parse(r["map"]["id"].as_str().unwrap()).unwrap();
    assert_eq!(r["replaced"], json!([old.as_str()]));
    let state = |s: &Session, id: &lcc_develop::VignettingMapId| s.catalog.vignetting_map(id).map(|r| r.withdrawn);
    assert_eq!((state(&b.s, &old), state(&b.s, &new), library(&old_key)), (Some(true), Some(false), Some(true)));
    assert_eq!(vignetting_calibration(&b.s, user).as_deref(), Some(old.as_str()), "the photo keeps the old map");
    // undo: the old one is back in use, the new one withdrawn (its map stays)
    b.s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!((state(&b.s, &old), state(&b.s, &new), library(&old_key)), (Some(false), Some(true), Some(false)));
    b.s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!((state(&b.s, &old), state(&b.s, &new)), (Some(true), Some(false)));
}

/// A withdrawn reference is a normal photo again; Create on it measures the same map (the same id) and puts it back in
/// use, through the same plausibility check.
#[test]
fn create_again_puts_a_withdrawn_reference_back_in_use() {
    let mut b = bench("recreate", true);
    let r = b.s.execute("vignettingCalibration.create", &json!({"id": b.reference})).unwrap();
    let id = lcc_develop::VignettingMapId::parse(r["map"]["id"].as_str().unwrap()).unwrap();
    b.s.execute("vignettingCalibration.withdraw", &json!({"id": b.reference})).unwrap();
    assert!(!b.s.catalog.is_vignetting_reference(lcc_catalog::PhotoId(b.reference)));
    let again = b.s.execute("vignettingCalibration.create", &json!({"id": b.reference})).unwrap();
    assert_eq!(again["map"]["id"], json!(id.as_str()), "the same map");
    assert_eq!(again["plausibility"]["verdict"], json!("pass"));
    assert_eq!(b.s.catalog.vignetting_map(&id).map(|r| r.withdrawn), Some(false));
    assert!(b.s.catalog.is_vignetting_reference(lcc_catalog::PhotoId(b.reference)));
    assert_eq!(b.s.catalog.vignetting_maps().count(), 1, "no second map");
    // undoing that Create withdraws it again
    b.s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(b.s.catalog.vignetting_map(&id).map(|r| r.withdrawn), Some(true));
}

/// A reference of this bench's camera, 85 mm at f/1.8, covering `area` (x, y, width, height) of a W × H sensor.
fn area_map(area: [usize; 4], seed: f64) -> lcc_develop::EmbeddedVignettingMap {
    let source = lcc_develop::VignettingMapOrigin {
        model: Some("Flat Cam".into()),
        lens: Some("NIKKOR Z 85mm f/1.8 S".into()),
        focal_length: Some(85.0),
        aperture: Some(1.8),
        file_name: Some(format!("AREA_{}x{}.dng", area[2], area[3])),
        ..Default::default()
    };
    let ev: Vec<f64> = (0..24).map(|i| seed * f64::from(i % 6) / 5.0).collect();
    let [x, y, w, h] = area;
    lcc_develop::EmbeddedVignettingMap::from_ev([w, h], [6, 4], Some([W, H, x, y, w, h]), source, &ev).unwrap()
}

/// Replacing across image areas: a new reference replaces those it covers (a full-frame one replaces a crop one, after
/// asking) and keeps those it doesn't (a new crop reference leaves the full-frame one for full-frame shots).
#[test]
fn a_reference_replaces_only_the_image_areas_it_covers() {
    use crate::cmd::vignetting_calibration::{Measured, commit_measured, conflicts, kept_beside};
    let mut b = bench("areas", false);
    let pass = lcc_raw::vignetting_plausibility::Plausibility::judge(lcc_raw::vignetting_plausibility::Metrics {
        level: 0.3,
        falloff: 1.0,
        ..Default::default()
    });
    let (fx, dx) = (area_map([0, 0, W, H], 1.0), area_map([W / 4, H / 4, W / 2, H / 2], 1.2));
    let id = |m: &lcc_develop::EmbeddedVignettingMap| lcc_develop::VignettingMapId::of(m);
    let state = |s: &Session, m: &lcc_develop::EmbeddedVignettingMap| s.catalog.vignetting_map(&id(m)).map(|r| r.withdrawn);
    let reference = lcc_catalog::PhotoId(b.reference);
    // a crop reference first, then a full-frame one: it covers the crop's area, so it asks, and Replace withdraws it
    commit_measured(&mut b.s, reference, Measured::for_tests(dx.clone(), pass.clone()), false, false).unwrap();
    let wide = Measured::for_tests(fx.clone(), pass.clone());
    assert_eq!(conflicts(&b.s, &wide).iter().map(|e| e.id.clone()).collect::<Vec<_>>(), vec![id(&dx)]);
    let e = commit_measured(&mut b.s, reference, wide.clone(), false, false).unwrap_err().to_string();
    assert!(e.contains("covers its image area too"), "{e}");
    let r = commit_measured(&mut b.s, reference, wide, false, true).unwrap();
    assert_eq!(r["replaced"], json!([id(&dx).as_str()]));
    assert_eq!((state(&b.s, &dx), state(&b.s, &fx)), (Some(true), Some(false)));
    // a new crop reference with the full-frame one in use: no question, both in use, each for its own area
    let dx2 = area_map([W / 4, H / 4, W / 2, H / 2], 1.4);
    let narrow = Measured::for_tests(dx2.clone(), pass);
    assert!(conflicts(&b.s, &narrow).is_empty());
    assert_eq!(kept_beside(&b.s, &narrow).iter().map(|e| e.id.clone()).collect::<Vec<_>>(), vec![id(&fx)]);
    let r = commit_measured(&mut b.s, reference, narrow, false, false).unwrap();
    assert_eq!((r["replaced"].clone(), r["keptForOtherAreas"].clone()), (json!([]), json!([id(&fx).as_str()])));
    assert_eq!((state(&b.s, &fx), state(&b.s, &dx2)), (Some(false), Some(false)));
}
