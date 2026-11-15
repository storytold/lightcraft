//! Hot / dead / stuck pixels and sensor dust through the engine: decode-time correction, the per-photo switch,
//! camera bodies' maps built by commands (with their activity rows), Remove Dust, and the settings round trip.

use serde_json::{Value, json};

use crate::Session;
use crate::tests_xmp::{synthetic_dng_of, temp_dir};

/// A smooth `w × h` RGGB mosaic (values in the DNG's range: black 256, white 16000; about 0.16 of white) with noise
/// `seed`, and `hot` samples raised by `add` (0..1 of the range).
fn mosaic(w: usize, h: usize, seed: u64, hot: &[(usize, usize, f32)]) -> Vec<u16> {
    let mut n = seed.wrapping_mul(2654435761).wrapping_add(1);
    let mut data: Vec<u16> = (0..w * h)
        .map(|i| {
            n = n.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let noise = ((n >> 40) % 41) as f32 - 20.0;
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            (256.0 + 2500.0 + 6.0 * x + 4.0 * y + noise) as u16
        })
        .collect();
    for &(x, y, add) in hot {
        data[y * w + x] = (f32::from(data[y * w + x]) + add * (16000.0 - 256.0)).min(16000.0) as u16;
    }
    data
}

fn meta(minute: u32, serial: Option<&str>) -> lcc_meta::Metadata {
    lcc_meta::Metadata {
        make: Some("Testmaker".into()),
        model: Some("Sensor Test 1".into()),
        serial_number: serial.map(str::to_string),
        capture_time: lcc_meta::DateTime::parse_iso(&format!("2026-09-27T{:02}:{:02}:00", 10 + minute / 60, minute % 60)),
        exposure_time: Some(0.01),
        iso: Some(400),
        f_number: Some(11.0),
        ..Default::default()
    }
}

fn green_at(img: &lcc_raster::Rgb32f, x: usize, y: usize) -> f32 {
    img.get(x, y)[1]
}

/// A clear hot pixel is corrected while a raw is decoded, unless the photo's switch is off; the decode reports it.
#[test]
fn decode_corrects_a_hot_pixel_unless_switched_off() {
    let _maps = crate::sensor_maps::change_generation();
    let (w, h) = (96usize, 64usize);
    let dng = synthetic_dng_of(w, h, "RGGB", mosaic(w, h, 1, &[(41, 30, 0.7)]), None, meta(0, None));
    let on = crate::media::LoadOptions { path: Some("_P1_0001.dng".into()), ..Default::default() };
    let off = crate::media::LoadOptions { pixel_fix: false, ..on.clone() };
    let (fixed, info) = crate::files::load_bytes_with(&dng, 4096, on.clone()).unwrap();
    let (plain, info_off) = crate::files::load_bytes_with(&dng, 4096, off).unwrap();
    let px = info.pixels.clone().unwrap();
    assert!(px.on && px.found >= 1, "{px:?}");
    assert_eq!(px.body.as_deref(), Some("file:P1"));
    assert_eq!(px.model, "Sensor Test 1");
    assert!(!info_off.pixels.unwrap().on);
    // the hot sample (an odd column on an even row: green) lit its neighbourhood in the plain decode only
    let (a, b) = (green_at(&plain, 41, 30), green_at(&fixed, 41, 30));
    assert!(a > 3.0 * b, "plain {a} fixed {b}");
    assert!((green_at(&fixed, 41, 30) - green_at(&fixed, 45, 30)).abs() < 0.05);
}

/// A faint hot pixel (below what one photo corrects) is confirmed by a body's map after three photos from
/// different times, and then corrected on every photo of that body; another body's photos are left alone.
#[test]
fn a_body_map_confirms_a_faint_hot_pixel_across_photos() {
    use lcc_raw::defects::{Thresholds, detect, refine};
    let _maps = crate::sensor_maps::change_generation();
    let (w, h) = (96usize, 64usize);
    // (above what a map learns, 0.04 here; under what one photo corrects, 0.056)
    let faint = (50usize, 20usize, 0.05f32);
    let decode = |seed: u64, minute: u32, serial: &str| {
        let dng = synthetic_dng_of(w, h, "RGGB", mosaic(w, h, seed, &[faint]), None, meta(minute, Some(serial)));
        lcc_raw::decode(&dng).unwrap()
    };
    // one photo alone doesn't correct it
    let raw = decode(10, 0, "SN-FAINT-1");
    let loose = detect(&raw, &Thresholds::LEARN);
    assert!(loose.defects.iter().any(|d| (d.x, d.y) == (50, 20)), "{loose:?}");
    assert!(!refine(&raw, &loose, &Thresholds::CORRECT).iter().any(|d| (d.x, d.y) == (50, 20)));
    let opts = crate::files::RawOptions::default();
    let body = raw.metadata.serial_number.clone();
    let mut reports = Vec::new();
    for (k, minute) in [0u32, 25, 50, 75].into_iter().enumerate() {
        let mut raw = decode(10 + k as u64, minute, "SN-FAINT-1");
        reports.push(crate::files::repair_pixels(&mut raw, &opts));
    }
    if body.is_some() {
        assert_eq!(reports[0].body.as_deref(), Some("SN-FAINT-1"));
    }
    let b = reports[0].body.clone().unwrap();
    // the third photo confirms it (and is corrected with it); the fourth gets it from the map
    assert_eq!(reports.iter().map(|r| r.from_map).collect::<Vec<_>>(), vec![0, 0, 1, 1], "{reports:?}");
    let m = crate::sensor_maps::get(&b).unwrap();
    assert_eq!(m.fixes().len(), 1);
    assert_eq!(m.photos, 4);
    // another body (another serial) has nothing
    let mut other = decode(30, 0, "SN-FAINT-2");
    assert_eq!(crate::files::repair_pixels(&mut other, &opts).from_map, 0);
    let _ = crate::sensor_maps::clear(&b, true, true);
}

/// The switch is a develop control, kept out of settings while on (so older settings keep their hash), and the
/// settings with it off, an auto-dust spot and an Even Out spot survive JSON and an XMP sidecar.
#[test]
fn sensor_switch_and_auto_dust_spots_round_trip() {
    use lcc_develop::{AutoSpot, AutoSpotKind, DevelopSettings, Spot, SpotMode};
    let _maps = crate::sensor_maps::change_generation();
    let d = DevelopSettings::default();
    assert!(!d.to_json().to_string().contains("sensor"), "default settings JSON unchanged");
    assert_eq!(lcc_develop::controls::get(&d, "sensor.pixelFix"), Some(1.0));
    let mut s = d.clone();
    assert!(lcc_develop::controls::set(&mut s, "sensor.pixelFix", 0.0));
    assert!(!s.sensor.fixes_pixels());
    s.spots.push(Spot {
        mode: SpotMode::Flat,
        points: vec![lcc_geom::Point::new(0.3, 0.4)],
        size: 0.01,
        auto: Some(AutoSpot { kind: AutoSpotKind::Dust, confirmed: true, confidence: 0.9 }),
        ..Default::default()
    });
    let back = DevelopSettings::from_json(&s.to_json()).unwrap();
    assert_eq!(back, s);
    // through a sidecar in the app's own namespace
    let dir = temp_dir("sensor-xmp");
    let png = dir.join("p.png");
    let img = lcc_raster::Rgba8 { width: 40, height: 24, data: vec![[90, 100, 110, 255]; 960] };
    std::fs::write(&png, lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap()).unwrap();
    let mut ses = Session::new().with_fs();
    ses.execute("library.import", &json!({"paths": [png.to_string_lossy()]})).unwrap();
    let id = ses.active().unwrap();
    ses.set_develop(id, s.clone(), "test").unwrap();
    ses.execute("photo.saveMetadataToFile", &json!({})).unwrap();
    let xmp = std::fs::read_to_string(dir.join("p.xmp")).unwrap();
    let parsed = crate::sidecar::parse_sidecar(&xmp, crate::crs::Target::Rendered).unwrap();
    match parsed.develop {
        Some(crate::sidecar::DevelopPatch::Full(f)) => {
            assert_eq!(f.sensor, s.sensor);
            assert_eq!(f.spots, s.spots);
        }
        other => panic!("no lc:settings: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Turning the switch off decodes the photo again without corrections, and on again with them.
#[test]
fn the_switch_reloads_the_photo() {
    let _maps = crate::sensor_maps::change_generation();
    let dir = temp_dir("sensor-switch");
    let (w, h) = (96usize, 64usize);
    let path = dir.join("_P2_0001.dng");
    std::fs::write(&path, synthetic_dng_of(w, h, "RGGB", mosaic(w, h, 2, &[(41, 30, 0.7)]), None, meta(0, None))).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    let pixels = |s: &mut Session| {
        s.render_now(id, 96, 96).unwrap();
        s.source_info(id).pixels.map(|p| p.on)
    };
    assert_eq!(pixels(&mut s), Some(true));
    s.execute("develop.set", &json!({"control": "sensor.pixelFix", "value": 0})).unwrap();
    assert_eq!(pixels(&mut s), Some(false));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(pixels(&mut s), Some(true));
    let r = s.execute("photo.pixelDefects", &json!({})).unwrap();
    assert!(r["found"].as_array().is_some_and(|a| a.iter().any(|d| d["x"] == 41 && d["y"] == 30)), "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

fn wait_job(s: &mut Session) -> Value {
    let t0 = std::time::Instant::now();
    loop {
        let st = s.execute("camera.pixelMap.status", &json!({"body": "all"})).unwrap();
        if st["job"]["running"] == false {
            return st;
        }
        assert!(t0.elapsed().as_secs() < 120, "still running: {st}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// `camera.pixelMap.rebuild` reads a body's raws in the background with an activity row, confirms what several
/// photos agree on, and `status` / `clear` show and forget it.
#[test]
fn pixel_map_commands_build_show_and_clear_a_body_map() {
    let _maps = crate::sensor_maps::change_generation();
    let dir = temp_dir("sensor-rebuild");
    let (w, h) = (128usize, 96usize);
    let mut paths = Vec::new();
    for k in 0..8u64 {
        let p = dir.join(format!("_R7_{k:04}.dng"));
        // a faint hot pixel in every photo; another only in one (a one-off is never confirmed)
        let mut hot = vec![(60usize, 40usize, 0.05f32)];
        if k == 3 {
            hot.push((20, 70, 0.05));
        }
        std::fs::write(&p, synthetic_dng_of(w, h, "RGGB", mosaic(w, h, 100 + k, &hot), None, meta(k as u32 * 30, None))).unwrap();
        paths.push(p.to_string_lossy().into_owned());
    }
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": paths})).unwrap();
    let ids: Vec<u64> = s.catalog.photos().map(|p| p.id.0).collect();
    let r = s.execute("camera.pixelMap.rebuild", &json!({"ids": ids, "body": "file:R7"})).unwrap();
    assert_eq!(r["total"], 8, "{r}");
    let rows = s.activity.list();
    assert!(rows.iter().any(|t| t.kind == "sensorMaps" && t.label == "Mapping hot pixels" && t.cancellable) || r["running"] == false, "{rows:?}");
    // only one build at a time
    let again = s.execute("camera.pixelMap.rebuild", &json!({"ids": ids}));
    let st = wait_job(&mut s);
    if r["running"] == true {
        assert!(again.is_err() || st["job"]["total"] == 8);
    }
    assert!(s.activity.list().iter().all(|t| t.kind != "sensorMaps"), "the row goes when it ends");
    let st = s.execute("camera.pixelMap.status", &json!({"body": "file:R7"})).unwrap();
    let b = &st["bodies"][0];
    assert_eq!(b["photos"], 8, "{st}");
    assert_eq!(b["pixels"]["confirmed"], 1, "{st}");
    assert_eq!(b["label"], "R7");
    // a rebuild starts afresh (the same photos again: the same answer, not doubled counts)
    s.execute("camera.pixelMap.rebuild", &json!({"ids": ids, "body": "file:R7", "wait": true})).unwrap();
    let st = s.execute("camera.pixelMap.status", &json!({"body": "file:R7", "list": true})).unwrap();
    assert_eq!(st["bodies"][0]["photos"], 8, "{st}");
    assert_eq!(st["bodies"][0]["list"][0]["x"], 60);
    // the photos now render with it corrected, the map's file is in the folder
    assert!(crate::sensor_maps::dir().unwrap().join("file_R7.json").exists());
    s.execute("camera.pixelMap.clear", &json!({"body": "file:R7"})).unwrap();
    let st = s.execute("camera.pixelMap.status", &json!({"body": "file:R7"})).unwrap();
    assert_eq!(st["bodies"][0]["pixels"]["confirmed"], 0, "{st}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Dark frames confirm their hot pixels at once and keep their noise figures.
#[test]
fn dark_frame_calibration_confirms_hot_pixels() {
    let _maps = crate::sensor_maps::change_generation();
    let dir = temp_dir("sensor-dark");
    let (w, h) = (96usize, 64usize);
    let mut n = 5u64;
    let mut data: Vec<u16> = (0..w * h)
        .map(|_| {
            n = n.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            256 + 60 + ((n >> 40) % 21) as u16
        })
        .collect();
    for (x, y) in [(10usize, 10usize), (70, 40)] {
        data[y * w + x] = 3000;
    }
    let path = dir.join("_D1_dark.dng");
    std::fs::write(&path, synthetic_dng_of(w, h, "RGGB", data, None, meta(0, None))).unwrap();
    let mut s = Session::new().with_fs();
    let r = s.execute("camera.pixelMap.calibrate", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    let f = &r["frames"][0];
    assert_eq!((f["body"].as_str(), f["hot"].as_u64()), (Some("file:D1"), Some(2)), "{r}");
    assert!(f["noise"][1].as_f64().unwrap() > 0.0);
    assert!(f["saved"].as_str().is_some_and(|p| std::path::Path::new(p).exists()), "{r}");
    let st = s.execute("camera.pixelMap.status", &json!({"body": "file:D1"})).unwrap();
    assert_eq!(st["bodies"][0]["pixels"]["fromDarkFrames"], 2, "{st}");
    // a lit photo is refused with a reason
    let lit = dir.join("_D1_lit.dng");
    std::fs::write(&lit, synthetic_dng_of(w, h, "RGGB", mosaic(w, h, 3, &[]), None, meta(0, None))).unwrap();
    let r = s.execute("camera.pixelMap.calibrate", &json!({"paths": [lit.to_string_lossy()]})).unwrap();
    assert!(r["frames"][0]["error"].as_str().is_some_and(|e| e.contains("dark frame")), "{r}");
    let _ = crate::sensor_maps::clear("file:D1", true, true);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A sky photo (PNG, no camera body) with two dust spots: Remove Dust heals both, tags them auto dust, and running
/// it again replaces them instead of adding more; moving one makes it the user's own.
#[test]
fn remove_dust_tags_its_spots_and_replaces_them_when_run_again() {
    let _maps = crate::sensor_maps::change_generation();
    let dir = temp_dir("sensor-dust");
    let (w, h) = (900usize, 600usize);
    let spots = [(180.0f32, 140.0f32), (600.0, 300.0)];
    let data: Vec<[u8; 4]> = (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            let mut v = 0.5 + 0.3 * (y / h as f32);
            for (cx, cy) in spots {
                v -= 0.07 * (-((x - cx).powi(2) + (y - cy).powi(2)) / (2.0 * 25.0)).exp();
            }
            let b = (v * 255.0) as u8;
            [b, b, (b as f32 * 1.15).min(255.0) as u8, 255]
        })
        .collect();
    let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&lcc_raster::Rgba8 { width: w, height: h, data }), &Default::default()).unwrap();
    std::fs::write(dir.join("sky.png"), png).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [dir.join("sky.png").to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    let r = s.execute("spot.findDust", &json!({})).unwrap();
    assert_eq!(r["added"], 2, "{r}");
    let d = s.develop_of(id).unwrap();
    assert!(d.spots.iter().all(|sp| sp.auto.is_some_and(|a| a.kind == lcc_develop::AutoSpotKind::Dust && !a.confirmed)));
    // again: still two
    s.execute("spot.findDust", &json!({})).unwrap();
    assert_eq!(s.develop_of(id).unwrap().spots.len(), 2);
    // a moved or resized spot is the user's: it stays when Remove Dust runs again (which may add its own for that
    // dust when the user's spot doesn't remove it from the render the finder looks at)
    s.execute("spot.select", &json!({"index": 0})).unwrap();
    s.execute("spot.update", &json!({"size": 0.03})).unwrap();
    assert!(s.develop_of(id).unwrap().spots[0].auto.is_none(), "resized: no longer automatic");
    s.execute("spot.findDust", &json!({})).unwrap();
    let after = s.develop_of(id).unwrap();
    assert!((2..=3).contains(&after.spots.len()), "{:?}", after.spots);
    assert_eq!(after.spots.iter().filter(|sp| sp.auto.is_none()).count(), 1, "the user's spot stays");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Dust on a body's sensor: photos taken over time with the same spot build a confirmed map entry; a later photo
/// whose spot is too faint to be found alone gets it from the map; a photo turned to portrait gets it at the
/// right place; a selection runs in the background and `dust.pump` applies it.
#[test]
fn a_body_dust_map_finds_faint_dust_on_later_photos() {
    let _maps = crate::sensor_maps::change_generation();
    let dir = temp_dir("sensor-dustmap");
    let (w, h) = (600usize, 400usize);
    // a smooth bright mosaic with a soft dark spot at (cx, cy) on the sensor, `depth` deep
    let photo = |seed: u64, depth: f32| {
        let mut n = seed.wrapping_add(7);
        (0..w * h)
            .map(|i| {
                n = n.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                let d2 = (x - 420.0).powi(2) + (y - 130.0).powi(2);
                let t = 1.0 - depth * (-d2 / (2.0 * 5.0 * 5.0)).exp();
                (256.0 + (2600.0 + 2.0 * y) * t + ((n >> 40) % 9) as f32) as u16
            })
            .collect::<Vec<u16>>()
    };
    let mut paths = Vec::new();
    for k in 0..4u32 {
        let p = dir.join(format!("_U5_{k:04}.dng"));
        std::fs::write(&p, synthetic_dng_of(w, h, "RGGB", photo(k as u64, 0.16), None, meta(k * 40, None))).unwrap();
        paths.push(p);
    }
    // a later photo where the dust is faint (a wider aperture)
    let faint = dir.join("_U5_0100.dng");
    std::fs::write(&faint, synthetic_dng_of(w, h, "RGGB", photo(50, 0.03), None, meta(300, None))).unwrap();
    let mut s = Session::new().with_fs();
    let all: Vec<String> = paths.iter().chain([&faint]).map(|p| p.to_string_lossy().into_owned()).collect();
    s.execute("library.import", &json!({"paths": all})).unwrap();
    let id_of = |s: &Session, name: &str| s.catalog.photos().find(|p| p.file_name == name).unwrap().id;
    let learn: Vec<u64> = (0..4).map(|k| id_of(&s, &format!("_U5_{k:04}.dng")).0).collect();
    s.execute("camera.dustMap.rebuild", &json!({"ids": learn, "wait": true})).unwrap();
    let st = s.execute("camera.dustMap.status", &json!({"body": "file:U5", "list": true})).unwrap();
    assert_eq!(st["bodies"][0]["dust"]["confirmed"], 1, "{st}");
    // the faint photo: found from the map (it is too faint to be found alone at this sensitivity)
    let fid = id_of(&s, "_U5_0100.dng");
    s.execute("library.select", &json!({"ids": [fid.0]})).unwrap();
    let alone = s.execute("spot.findDust", &json!({"useMap": false, "add": false})).unwrap();
    let r = s.execute("spot.findDust", &json!({})).unwrap();
    assert!(alone["found"].as_u64() < Some(1) || r["confirmed"].as_u64() >= Some(1), "{alone} {r}");
    assert_eq!(r["added"], 1, "{r}");
    let sp = &r["spots"][0];
    assert!((sp["x"].as_f64().unwrap() - 420.0 / w as f64).abs() < 0.01 && (sp["y"].as_f64().unwrap() - 130.0 / h as f64).abs() < 0.015, "{r}");
    assert_eq!(sp["confirmed"], true);
    // turned to portrait (Rotate Right): the spot follows the sensor position
    s.execute("photo.rotateRight", &json!({"ids": [fid.0]})).unwrap();
    let o = s.develop_of(fid).unwrap().orientation;
    let r = s.execute("spot.findDust", &json!({})).unwrap();
    let (ex, ey) = o.map(420.0 / w as f64, 130.0 / h as f64, 1.0, 1.0);
    let sp = &r["spots"][0];
    assert!((sp["x"].as_f64().unwrap() - ex).abs() < 0.015 && (sp["y"].as_f64().unwrap() - ey).abs() < 0.015, "{o:?} {r}");
    // a selection: in the background, applied by dust.pump
    let sel: Vec<u64> = learn.iter().copied().chain([fid.0]).collect();
    let job = s.execute("spot.findDust", &json!({"ids": sel})).unwrap();
    assert_eq!(job["total"], 5, "{job}");
    let t0 = std::time::Instant::now();
    loop {
        let p = s.execute("dust.pump", &json!({})).unwrap();
        if p["job"]["running"] == false {
            s.execute("dust.pump", &json!({})).unwrap();
            break;
        }
        assert!(t0.elapsed().as_secs() < 120);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    for id in &learn {
        let d = s.develop_of(lcc_catalog::PhotoId(*id)).unwrap();
        assert!(d.spots.iter().any(|sp| sp.auto.is_some_and(|a| a.confirmed)), "{id}: {:?}", d.spots);
    }
    let _ = crate::sensor_maps::clear("file:U5", true, true);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Settings → Import → Remove dust on import: an import starts Remove Dust on the new raws in the background (off
/// by default; a PNG is never included).
#[test]
fn remove_dust_on_import_runs_on_new_raws() {
    let _maps = crate::sensor_maps::change_generation();
    let dir = temp_dir("sensor-import");
    let (w, h) = (96usize, 64usize);
    let raw = dir.join("_I1_0001.dng");
    std::fs::write(&raw, synthetic_dng_of(w, h, "RGGB", mosaic(w, h, 9, &[]), None, meta(0, None))).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [raw.to_string_lossy()]})).unwrap();
    assert!(s.sensor_job.is_none(), "off by default");
    s.execute("library.preferences", &json!({"import": {"removeDust": true}})).unwrap();
    assert_eq!(s.execute("library.preferences", &json!({})).unwrap()["import"]["removeDust"], true);
    let raw2 = dir.join("_I1_0002.dng");
    std::fs::write(&raw2, synthetic_dng_of(w, h, "RGGB", mosaic(w, h, 10, &[]), None, meta(5, None))).unwrap();
    s.execute("library.import", &json!({"paths": [raw2.to_string_lossy()]})).unwrap();
    let job = s.sensor_job.clone().expect("Remove Dust started");
    assert_eq!((job.what, job.total), ("removeDust", 1));
    let t0 = std::time::Instant::now();
    while !job.finished.load(std::sync::atomic::Ordering::Relaxed) {
        assert!(t0.elapsed().as_secs() < 60);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    s.execute("dust.pump", &json!({})).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Confirm a pixel on `body`'s map: the same sample found in photos of three capture periods.
fn confirm_pixel(body: &str, x: u32) {
    use lcc_raw::defects::{Defect, DefectKind};
    let found = [Defect { x, y: 7, s: 1, kind: DefectKind::Hot, level: 0.2 }];
    let mut changed = false;
    for k in 0..3u32 {
        let shot = crate::sensor_maps::Shot {
            fingerprint: 0x5e7_0000 + u64::from(x) * 16 + u64::from(k),
            captured: Some(format!("2026-09-27T1{k}:00:00")),
            exposure: Some(0.01),
            iso: Some(400),
            f_number: Some(8.0),
        };
        changed |= crate::sensor_maps::record_pixels(body, "Testmaker", "Sensor Test 1", [96, 64], &shot, &found) == Some(true);
    }
    assert!(changed, "{body}: the pixel is confirmed");
}

/// Confirming a hot pixel on one camera body changes the preview keys of that body's photos and of no other body's:
/// the rest of a library keeps its thumbnails, loupe and 1:1 previews.
#[test]
fn a_body_map_changes_only_that_bodys_preview_keys() {
    let dir = temp_dir("sensor-keys");
    let (w, h) = (96usize, 64usize);
    let mut s = Session::new().with_fs();
    // two bodies with a serial number, and two cameras that write none and are known by their file names
    let mut photo = |name: &str, serial: Option<&str>| {
        let path = dir.join(name);
        // (different pixels: an identical file would be skipped as a duplicate)
        let seed = name.bytes().map(u64::from).sum::<u64>();
        std::fs::write(&path, synthetic_dng_of(w, h, "RGGB", mosaic(w, h, seed, &[]), None, meta(0, serial))).unwrap();
        s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
        s.catalog.photos().find(|p| p.file_name == name).unwrap().id
    };
    let serial_a = photo("a_0001.dng", Some("SN-KEYS-A"));
    let serial_b = photo("b_0001.dng", Some("SN-KEYS-B"));
    let named_a = photo("_K1_0001.dng", None);
    let named_b = photo("_K2_0001.dng", None);
    // (each file-named body has a map, as it does once one of its raws has been decoded)
    for b in ["file:K1", "file:K2"] {
        crate::sensor_maps::update(b, "Testmaker", "Sensor Test 1", [96, 64], |_| ()).unwrap();
    }
    let keys = |s: &mut Session, id| {
        let p = s.catalog.photo(id).unwrap().clone();
        let thumb = s.thumb_job(id, 256).unwrap().cache.unwrap().1;
        (thumb, Session::full_view_key(&p), Session::variant_key(&p, &p.develop, 128))
    };
    let all = |s: &mut Session| [serial_a, serial_b, named_a, named_b].map(|id| keys(s, id));
    let differ = |a: &(lcc_preview::Hash128, lcc_preview::Hash128, lcc_preview::Hash128),
                  b: &(lcc_preview::Hash128, lcc_preview::Hash128, lcc_preview::Hash128)| { a.0 != b.0 && a.1 != b.1 && a.2 != b.2 };

    let before = all(&mut s);
    confirm_pixel("SN-KEYS-A", 11);
    let after = all(&mut s);
    assert!(differ(&before[0], &after[0]), "the body whose map changed gets new keys");
    assert_eq!(before[1], after[1], "another body's keys stay");
    assert_eq!(before[2], after[2], "a file-named body's keys stay");
    assert_eq!(before[3], after[3], "a file-named body's keys stay");

    let before = after;
    confirm_pixel("file:K1", 13);
    let after = all(&mut s);
    assert_eq!(before[0], after[0]);
    assert_eq!(before[1], after[1]);
    assert!(differ(&before[2], &after[2]), "a body known by its file names gets new keys too");
    assert_eq!(before[3], after[3]);

    for b in ["SN-KEYS-A", "file:K1", "file:K2"] {
        let _ = crate::sensor_maps::clear(b, true, true);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Confirming a pixel on one body makes that body's decoded raws out of date, and no other photo's.
#[test]
fn a_body_map_change_drops_only_that_bodys_decoded_sources() {
    use crate::media::{DecodedSource, SourceLevel};
    use lcc_catalog::PhotoId;
    let mut s = Session::new();
    let source = |body: Option<&str>| {
        let pixels = body.map(|b| lcc_pipeline::PixelRepair { on: true, body: Some(b.into()), ..Default::default() });
        let info = lcc_pipeline::SourceInfo { pixels, ..Default::default() };
        DecodedSource::new(std::sync::Arc::new(lcc_raster::Rgb32f::from_fn(8, 8, |_, _| [0.5; 3])), Some(info))
    };
    let (a, b, none) = (PhotoId(901), PhotoId(902), PhotoId(903));
    s.media.get(a, SourceLevel::Preview);
    s.media.insert_source(a, SourceLevel::Preview, source(Some("SN-SRC-A")));
    s.media.insert_source(b, SourceLevel::Preview, source(Some("SN-SRC-B")));
    s.media.insert_source(none, SourceLevel::Preview, source(None));
    confirm_pixel("SN-SRC-A", 17);
    assert!(s.media.get(a, SourceLevel::Preview).is_none(), "decoded before its body's map changed");
    assert!(s.media.get(b, SourceLevel::Preview).is_some(), "another body's decode is still right");
    assert!(s.media.get(none, SourceLevel::Preview).is_some(), "no map went into this one");
    let _ = crate::sensor_maps::clear("SN-SRC-A", true, true);
}

/// A film scanner's RGBI TIFF: a flat mid-grey picture (nothing to find in it) whose infrared
/// pass has one dark speck and one dark hair; Find Film Dust heals both from the pass, and says
/// it did; without the pass it finds nothing, and asked for the pass on a plain photo it refuses.
#[test]
fn film_dust_is_healed_from_a_scanners_infrared_pass() {
    let dir = temp_dir("film-ir");
    let (w, h) = (320u32, 240u32);
    let px = |x: u32, y: u32| {
        let speck = (x as i64 - 80).pow(2) + (y as i64 - 60).pow(2) <= 4;
        let hair = (120..260).contains(&x) && y as i64 == 150 + (x as i64 - 120) / 7;
        // dust blocks light: dark in the colour too, and in the pass
        if speck || hair { vec![4000u16, 3000, 2000, 3000] } else { vec![30000, 25000, 20000, 42000] }
    };
    let path = dir.join("scan.tif");
    std::fs::write(&path, lcc_testkit::tiff16(w, h, 4, "VueScan 9 x64 (9.8.52)", None, px)).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    // decoded as an opaque picture, the pass beside it
    let info = s.source_info(id);
    let _ = s.source_now(id, crate::media::SourceLevel::Preview).unwrap();
    let info = if info.infrared.is_none() { s.source_info(id) } else { info };
    assert!(info.infrared.is_some(), "the infrared pass is kept with the source");
    assert!(info.alpha.is_none(), "and is not transparency");

    let r = s.execute("spot.findFilmDust", &json!({})).unwrap();
    assert_eq!(r["infrared"], true);
    let spots = r["spots"].as_array().unwrap();
    let near = |p: &Value, x: f64, y: f64| (p[0].as_f64().unwrap() - x).abs() < 0.02 && (p[1].as_f64().unwrap() - y).abs() < 0.02;
    assert!(
        spots.iter().any(|sp| sp["points"].as_array().unwrap().len() == 1 && near(&sp["points"][0], 80.0 / 320.0, 60.0 / 240.0)),
        "speck: {spots:?}"
    );
    assert!(spots.iter().any(|sp| sp["points"].as_array().unwrap().len() >= 3), "hair as a stroke: {spots:?}");
    let d = s.develop_of(id).unwrap();
    assert_eq!(d.spots.len(), spots.len());
    assert!(
        d.spots.iter().all(|sp| sp.mode == lcc_develop::SpotMode::Fill && sp.source_offset.is_none()),
        "rebuilt from the surroundings by default"
    );
    assert!(
        d.spots.iter().all(|sp| sp.auto.is_some_and(|a| a.kind == lcc_develop::AutoSpotKind::FilmDust && a.confirmed)),
        "tagged as film dust from the pass"
    );
    // heal mode copies from a source instead; expand grows each mark's spot
    let r = s.execute("spot.findFilmDust", &json!({"mode": "heal", "expand": 0})).unwrap();
    let speck_size = |r: &Value| {
        r["spots"].as_array().unwrap().iter().find(|sp| sp["points"].as_array().unwrap().len() == 1).map(|sp| sp["size"].as_f64().unwrap())
    };
    let small = speck_size(&r).unwrap();
    let d = s.develop_of(id).unwrap();
    assert!(d.spots.iter().all(|sp| sp.mode == lcc_develop::SpotMode::Heal && sp.source_offset.is_some()));
    let grown = s.develop_of(id).unwrap().spots.iter().map(|sp| sp.size).fold(0.0, f64::max);
    s.execute("spot.findFilmDust", &json!({"expand": 40})).unwrap();
    let grown_more = s.develop_of(id).unwrap().spots.iter().map(|sp| sp.size).fold(0.0, f64::max);
    assert!(grown_more > grown + 30.0 / 2400.0, "expanded: {grown_more} vs {grown} (speck {small})");
    assert!(s.execute("spot.findFilmDust", &json!({"mode": "magic"})).is_err());
    // a run at another sensitivity replaces its own spots (the user's stay), so the strength can be adjusted
    s.execute("spot.add", &json!({"mode": "heal", "points": [[0.9, 0.9]], "size": 0.01})).unwrap();
    let r = s.execute("spot.findFilmDust", &json!({"sensitivity": 80})).unwrap();
    assert_eq!(r["replaced"].as_u64(), Some(spots.len() as u64));
    let d = s.develop_of(id).unwrap();
    assert_eq!(d.spots.iter().filter(|sp| sp.auto.is_none()).count(), 1, "the user's spot stays");
    assert_eq!(d.spots.iter().filter(|sp| sp.auto.is_some()).count(), r["added"].as_u64().unwrap() as usize);
    assert!(
        s.execute("spot.findFilmDust", &json!({"sensitivity": f64::NAN, "expand": f64::INFINITY})).is_ok(),
        "a bad sensitivity is clamped, not a crash"
    );

    // the marks as a mask: a stored segmentation named for it, selected, replaced on a re-run
    let r = s.execute("spot.filmDustMask", &json!({"expand": 2})).unwrap();
    let mid = r["mask"].as_u64().unwrap() as u32;
    assert!(r["coverage"].as_f64().unwrap() > 0.0 && r["coverage"].as_f64().unwrap() < 0.1, "{r}");
    assert_eq!(s.active_mask, Some(mid));
    let d = s.develop_of(id).unwrap();
    let m = d.masks.iter().find(|m| m.id == mid).unwrap();
    assert_eq!(m.name, crate::cmd::film_dust::MASK_NAME);
    assert!(matches!(m.components.first().map(|c| &c.shape), Some(lcc_develop::MaskShape::Object { seg: Some(_), .. })));
    let r2 = s.execute("spot.filmDustMask", &json!({"expand": 40})).unwrap();
    assert!(r2["coverage"].as_f64().unwrap() > r["coverage"].as_f64().unwrap(), "expanded: {r2} vs {r}");
    let d = s.develop_of(id).unwrap();
    assert_eq!(d.masks.iter().filter(|m| m.name == crate::cmd::film_dust::MASK_NAME).count(), 1, "the earlier mask is replaced");
    // and it can be filled like any mask
    let n = d.spots.len();
    let r = s.execute("spot.fromMask", &json!({"mode": "fill"})).unwrap();
    assert!(r["dabs"].as_u64().unwrap() > 0);
    assert_eq!(s.develop_of(id).unwrap().spots.len(), n + 1);

    // the picture alone shows nothing (it is flat), and asking for the pass on a plain photo is refused
    let r = s.execute("spot.findFilmDust", &json!({"infrared": "never", "add": false})).unwrap();
    assert_eq!((r["infrared"].as_bool(), r["added"].as_u64()), (Some(false), Some(0)));
    let plain = dir.join("plain.png");
    let img = lcc_raster::Rgba8 { width: 80, height: 60, data: vec![[90, 100, 110, 255]; 4800] };
    std::fs::write(&plain, lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap()).unwrap();
    s.execute("library.import", &json!({"paths": [plain.to_string_lossy()]})).unwrap();
    let err = s.execute("spot.findFilmDust", &json!({"infrared": "only"})).unwrap_err().to_string();
    assert!(err.contains("infrared"), "{err}");
    assert!(s.execute("spot.filmDustMask", &json!({})).unwrap_err().to_string().contains("infrared"));
    assert!(s.execute("spot.findFilmDust", &json!({"infrared": "sometimes"})).is_err());
}

/// Remove Film Dust (`filmDust.remove`, a develop control) fills the pass's marks in before the
/// picture is converted: the speck renders like its surroundings, off it stays dark; a photo
/// without the pass renders the same with it on; the setting is not written while default.
#[test]
fn remove_film_dust_fills_the_marks_in_the_render() {
    let dir = temp_dir("film-ir-remove");
    let (w, h) = (320u32, 240u32);
    let px = |x: u32, y: u32| {
        let speck = (x as i64 - 80).pow(2) + (y as i64 - 60).pow(2) <= 9;
        if speck { vec![4000u16, 3000, 2000, 3000] } else { vec![30000, 25000, 20000, 42000] }
    };
    let path = dir.join("scan.tif");
    std::fs::write(&path, lcc_testkit::tiff16(w, h, 4, "VueScan 9 x64 (9.8.52)", None, px)).unwrap();
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    let at = |s: &mut Session, x: usize, y: usize| -> [u8; 4] {
        let img = s.render_now(id, 320, 240).unwrap().image;
        img.data[y * img.width + x]
    };
    let dark = at(&mut s, 80, 60);
    let clean = at(&mut s, 100, 60);
    assert!(dark[1] + 40 < clean[1], "the speck is dark before: {dark:?} vs {clean:?}");
    s.execute("develop.set", &json!({"control": "filmDust.remove", "value": 1})).unwrap();
    let filled = at(&mut s, 80, 60);
    assert!((i32::from(filled[1]) - i32::from(clean[1])).abs() <= 6, "filled: {filled:?} vs {clean:?}");
    assert_eq!(at(&mut s, 100, 60), clean, "the rest is untouched");
    // the setting rides along in the settings JSON only when it is not the default
    let d = s.develop_of(id).unwrap();
    assert!(serde_json::to_string(&*d).unwrap().contains("film_dust"));
    assert!(!serde_json::to_string(&lcc_develop::DevelopSettings::default()).unwrap().contains("film_dust"));
    // sensitivity and expand are develop controls
    s.execute("develop.set", &json!({"control": "filmDust.expand", "value": 40})).unwrap();
    assert_eq!(s.develop_of(id).unwrap().film_dust.expand, 40.0);
    s.execute("develop.set", &json!({"control": "filmDust.remove", "value": 0})).unwrap();
    assert_eq!(at(&mut s, 80, 60), dark, "off again: the speck is back");
    // no pass: nothing changes with it on
    let plain = dir.join("plain.png");
    let img = lcc_raster::Rgba8 { width: 80, height: 60, data: vec![[90, 100, 110, 255]; 4800] };
    std::fs::write(&plain, lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &Default::default()).unwrap()).unwrap();
    s.execute("library.import", &json!({"paths": [plain.to_string_lossy()]})).unwrap();
    let pid = s.active().unwrap();
    let before = s.render_now(pid, 80, 60).unwrap().image;
    s.execute("develop.set", &json!({"control": "filmDust.remove", "value": 1})).unwrap();
    assert_eq!(s.render_now(pid, 80, 60).unwrap().image.data, before.data);
}
