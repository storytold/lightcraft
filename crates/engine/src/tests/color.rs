//! Sampling commands: Point Color, the targeted adjustment tool, red eye.

use serde_json::json;

use crate::Session;

fn demo() -> Session {
    Session::with_demo()
}

fn active_dev(s: &Session) -> lightcraft_develop::DevelopSettings {
    (*s.develop_of(s.active().unwrap()).unwrap()).clone()
}

/// Spread of the channels (a saturation proxy) at normalized `(x, y)`.
fn chroma_at(img: &lightcraft_raster::Rgba8, x: f64, y: f64) -> i32 {
    let p = img.data[(img.height as f64 * y) as usize * img.width + (img.width as f64 * x) as usize];
    p[0].max(p[1]).max(p[2]) as i32 - p[0].min(p[1]).min(p[2]) as i32
}

#[test]
fn point_color_pick_and_adjust() {
    let mut s = demo();
    let id = s.active().unwrap();
    let before = s.render_now(id, 384, 384).unwrap().image;
    let r = s.execute("pointColor.pick", &json!({"x": 0.5, "y": 0.08})).unwrap();
    assert_eq!(r["index"], 0);
    let d = active_dev(&s);
    assert_eq!(d.point_colors.len(), 1);
    assert!(d.point_colors[0].chroma > 0.01, "the sky has colour: {r}");
    // the sample's sliders are develop controls (and listed as such)
    s.execute("develop.set", &json!({"control": "pointColor.0.satShift", "value": -100})).unwrap();
    assert_eq!(active_dev(&s).point_colors[0].sat_shift, -100.0);
    let ctl = s.execute("develop.controls", &json!({"section": "pointColor"})).unwrap();
    assert!(ctl.as_array().unwrap().iter().any(|c| c["id"] == "pointColor.0.satShift" && c["value"] == -100.0), "{ctl}");
    // the picked colour is desaturated in the render
    let after = s.render_now(id, 384, 384).unwrap().image;
    assert!(
        chroma_at(&after, 0.5, 0.08) + 8 < chroma_at(&before, 0.5, 0.08),
        "{} vs {}",
        chroma_at(&after, 0.5, 0.08),
        chroma_at(&before, 0.5, 0.08)
    );
    // at most 8 samples; delete
    for _ in 0..7 {
        s.execute("pointColor.pick", &json!({"x": 0.3, "y": 0.5})).unwrap();
    }
    assert!(s.execute("pointColor.pick", &json!({"x": 0.3, "y": 0.5})).is_err());
    s.execute("pointColor.delete", &json!({"index": 3})).unwrap();
    assert_eq!(active_dev(&s).point_colors.len(), 7);
    // survives the settings JSON (what the catalog and XMP sidecars store)
    let d = active_dev(&s);
    assert_eq!(lightcraft_develop::DevelopSettings::from_json(&d.to_json()).unwrap(), d);
}

#[test]
fn red_eye_commands_and_controls() {
    let mut s = demo();
    let r = s.execute("redeye.add", &json!({"center": [0.3, 0.4], "rx": 0.03, "ry": 0.02})).unwrap();
    assert_eq!(r["index"], 0);
    s.execute("redeye.add", &json!({"center": [0.6, 0.4], "rx": 0.03, "ry": 0.02, "darken": 80})).unwrap();
    let d = active_dev(&s);
    assert_eq!(d.red_eye.len(), 2);
    assert_eq!((d.red_eye[0].pupil_size, d.red_eye[0].darken, d.red_eye[1].darken), (50.0, 50.0, 80.0));
    // per-eye sliders are develop controls
    s.execute("develop.set", &json!({"values": {"redEye.1.pupilSize": 70, "redEye.0.darken": 140}})).unwrap();
    let d = active_dev(&s);
    assert_eq!((d.red_eye[1].pupil_size, d.red_eye[0].darken), (70.0, 100.0));
    let ctl = s.execute("develop.controls", &json!({"section": "redEye"})).unwrap();
    assert_eq!(ctl.as_array().unwrap().len(), 4, "{ctl}");
    // the render runs with eyes (detection falls back gracefully on a photo without red pupils)
    let id = s.active().unwrap();
    assert!(s.render_now(id, 200, 200).is_ok());
    s.execute("redeye.delete", &json!({"index": 0})).unwrap();
    assert_eq!(active_dev(&s).red_eye.len(), 1);
    assert!(s.execute("redeye.delete", &json!({"index": 5})).is_err());
    assert!(s.execute("redeye.add", &json!({"rx": 0.03})).is_err());
    let d = active_dev(&s);
    assert_eq!(lightcraft_develop::DevelopSettings::from_json(&d.to_json()).unwrap(), d);
}

#[test]
fn pet_eye_catchlight_command() {
    let mut s = demo();
    s.execute("redeye.add", &json!({"center": [0.3, 0.4], "rx": 0.03, "ry": 0.03})).unwrap();
    s.execute("redeye.add", &json!({"center": [0.6, 0.4], "rx": 0.03, "ry": 0.03, "pet": true})).unwrap();
    assert!(s.execute("redeye.catchlight", &json!({"index": 0})).is_err(), "red eyes have no catchlight");
    s.execute("redeye.catchlight", &json!({"index": 1})).unwrap();
    assert_eq!(active_dev(&s).red_eye[1].catchlight, Some(lightcraft_geom::Point::new(-0.35, -0.35)));
    s.execute("redeye.catchlight", &json!({"index": 1, "offset": [0.2, -3.0]})).unwrap();
    assert_eq!(active_dev(&s).red_eye[1].catchlight, Some(lightcraft_geom::Point::new(0.2, -1.0)));
    s.execute("redeye.catchlight", &json!({"index": 1, "on": false})).unwrap();
    assert_eq!(active_dev(&s).red_eye[1].catchlight, None);
}

#[test]
fn targeted_adjustment_on_curve_and_mixer() {
    let mut s = demo();
    // the bright sky lies in the upper regions of the parametric curve
    let r = s.execute("develop.targeted", &json!({"target": "curve", "x": 0.5, "y": 0.35, "delta": 20})).unwrap();
    let (k, v) = r.as_object().unwrap().iter().next().map(|(k, v)| (k.clone(), v.clone())).unwrap();
    assert!(k == "curve.lights" || k == "curve.highlights", "{r}");
    assert_eq!(v, 20.0);
    assert_eq!(lightcraft_develop::controls::get(&active_dev(&s), &k), Some(20.0));
    // the dark trees in the lower ones
    let r = s.execute("develop.targeted", &json!({"target": "curve", "x": 0.4, "y": 0.62, "delta": -10})).unwrap();
    assert!(r.get("curve.shadows").is_some() || r.get("curve.darks").is_some(), "{r}");
    // point curve: a point appears at the sampled input, raised by delta levels
    s.execute("develop.targeted", &json!({"target": "curve", "channel": "master", "x": 0.5, "y": 0.35, "delta": 12.75})).unwrap();
    let m = active_dev(&s).curve.master;
    assert_eq!(m.len(), 3);
    assert!((m[1].y - m[1].x - 0.05).abs() < 1e-6, "{m:?}");
    // a second step moves the same point
    s.execute("develop.targeted", &json!({"target": "curve", "channel": "master", "x": 0.5, "y": 0.35, "delta": 12.75})).unwrap();
    assert_eq!(active_dev(&s).curve.master.len(), 3);
    // colour mixer: the purple sky's bands lose saturation, the dominant one by the full delta
    let r = s.execute("develop.targeted", &json!({"target": "sat", "x": 0.5, "y": 0.1, "delta": -30})).unwrap();
    let o = r.as_object().unwrap();
    assert!(!o.is_empty() && o.keys().all(|k| k.starts_with("mixer.") && k.ends_with(".sat")), "{r}");
    assert!(o.values().any(|v| v.as_f64() == Some(-30.0)), "{r}");
    assert!(o.values().all(|v| v.as_f64().unwrap() < 0.0));
    assert!(s.execute("develop.targeted", &json!({"target": "nope", "x": 0.5, "y": 0.5, "delta": 1})).is_err());
}

/// Issue #730: on a raw whose camera has measured colour matrices the white-balance commands write Kelvin, mark
/// the scale, resolve the presets to their real temperatures, and turn a value still on the old relative scale into
/// the Custom Kelvin value that renders the same before applying an edit on top of it.
#[test]
fn white_balance_commands_write_kelvin_on_raws_whose_camera_has_matrices() {
    use lightcraft_catalog::{MediaKind, Op, Photo, Source};
    use lightcraft_develop::{DevelopSettings, WbMode, WbScale};
    let mut s = Session::new();
    let add = |s: &mut Session, name: &str, camera: &str, as_shot: (f64, f64)| {
        let id = s.catalog.alloc_photo_id();
        let mut p = Photo::new(id, Source::File { path: format!("/photos/{name}") }, name, "ARW", 64, 64, "2026-01-01T00:00:00");
        p.kind = MediaKind::Raw;
        p.meta.camera = camera.into();
        p.as_shot_wb = Some(as_shot);
        p.develop = std::sync::Arc::new(p.camera_defaults());
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id
    };
    let kelvin = add(&mut s, "a.arw", "SONY ILCE-7M3", (4986.0, -2.0));
    let relative = add(&mut s, "b.arw", "SONY ILCE-7M2", (6500.0, 0.0));
    let dev = |s: &Session, id| (*s.develop_of(id).unwrap()).clone();
    assert_eq!((dev(&s, kelvin).wb.scale, dev(&s, relative).wb.scale), (WbScale::Kelvin, WbScale::Legacy));

    // presets are real temperatures on the Kelvin raw, scaled around 6500 K = as shot on the other
    s.execute("library.select", &json!({"ids": [kelvin.0], "active": kelvin.0})).unwrap();
    let r = s.execute("develop.wb", &json!({"mode": "cloudy"})).unwrap();
    assert_eq!((r["temp"].as_f64(), r["tint"].as_f64()), (Some(6500.0), Some(10.0)));
    let d = dev(&s, kelvin);
    assert_eq!((d.wb.mode, d.wb.temp, d.wb.tint, d.wb.scale), (WbMode::Cloudy, 6500.0, 10.0, WbScale::Kelvin));
    s.execute("develop.wb", &json!({"mode": "tungsten"})).unwrap();
    assert_eq!(dev(&s, kelvin).wb.temp, 2850.0);
    s.execute("library.select", &json!({"ids": [relative.0], "active": relative.0})).unwrap();
    let r = s.execute("develop.wb", &json!({"mode": "cloudy"})).unwrap();
    assert!((r["temp"].as_f64().unwrap() - 6500.0 * 6500.0 / 5500.0).abs() < 1e-6, "{r}");
    assert_eq!(dev(&s, relative).wb.scale, WbScale::Legacy);

    // a slider edit on the Kelvin raw is Kelvin; the stored value says so
    s.execute("library.select", &json!({"ids": [kelvin.0], "active": kelvin.0})).unwrap();
    s.execute("develop.wb", &json!({"mode": "asShot"})).unwrap();
    s.execute("develop.set", &json!({"control": "wb.temp", "value": 5200})).unwrap();
    let d = dev(&s, kelvin);
    assert_eq!((d.wb.mode, d.wb.temp, d.wb.tint, d.wb.scale), (WbMode::Custom, 5200.0, -2.0, WbScale::Kelvin));
    let info = s.source_info(kelvin);
    assert_eq!(lightcraft_engine_effective(&info, &d), (5200.0, -2.0));

    // settings from an older catalog on the Kelvin raw: a relative Custom value (6500 = as shot) with no scale
    let mut old = DevelopSettings::for_raw(6500.0, 0.0);
    (old.wb.mode, old.wb.temp, old.wb.tint) = (WbMode::Custom, 5800.0, 3.0);
    s.set_develop(kelvin, old.clone(), "old").unwrap();
    let shown = lightcraft_engine_effective(&info, &old);
    let expect = lightcraft_pipeline::local::legacy_to_kelvin(5800.0, 3.0, (4986.0, -2.0));
    assert_eq!(shown, expect);
    assert!(shown.0 < 4986.0, "a warm relative shift is a lower Kelvin than as shot: {shown:?}");
    // nudging the tint converts the temperature to the Kelvin it rendered as, then applies the nudge
    s.execute("develop.adjust", &json!({"control": "wb.tint", "delta": 5})).unwrap();
    let d = dev(&s, kelvin);
    assert_eq!((d.wb.mode, d.wb.scale), (WbMode::Custom, WbScale::Kelvin));
    assert!((d.wb.temp - expect.0).abs() < 1e-6 && (d.wb.tint - (expect.1 + 5.0)).abs() < 1e-6, "{:?} vs {expect:?}", d.wb);
    // a relative preset value from an older catalog becomes a Custom value with its old look, not the preset's Kelvin
    let mut old = DevelopSettings::for_raw(6500.0, 0.0);
    (old.wb.mode, old.wb.temp, old.wb.tint) = (WbMode::Cloudy, 6500.0 * 6500.0 / 5500.0, 10.0);
    s.set_develop(kelvin, old.clone(), "old preset").unwrap();
    assert_eq!(lightcraft_engine_effective(&info, &old), lightcraft_pipeline::local::legacy_to_kelvin(old.wb.temp, 10.0, (4986.0, -2.0)));
    s.execute("develop.set", &json!({"control": "wb.tint", "value": 0})).unwrap();
    let d = dev(&s, kelvin);
    assert_eq!((d.wb.mode, d.wb.tint, d.wb.scale), (WbMode::Custom, 0.0, WbScale::Kelvin));
    assert!(d.wb.temp != 6500.0 && (d.wb.temp - lightcraft_pipeline::local::legacy_to_kelvin(old.wb.temp, 10.0, (4986.0, -2.0)).0).abs() < 1e-6);
    // a partial without a scale (an old preset, `develop.merge`) keeps the relative meaning on the Kelvin raw
    s.execute("develop.merge", &json!({"settings": {"wb": {"mode": "custom", "temp": 6500.0, "tint": 0.0}}})).unwrap();
    let d = dev(&s, kelvin);
    assert_eq!(d.wb.scale, WbScale::Legacy);
    assert_eq!(lightcraft_engine_effective(&info, &d), (4986.0, -2.0), "6500 / 0 without a scale is as shot");
    // Quick Develop on old settings: As Shot from an older catalog (no scale) nudges the Kelvin as-shot white, a
    // relative Custom value is converted to the Kelvin it rendered as first (never a Kelvin number read as relative)
    let mut old = DevelopSettings::for_raw(6500.0, 0.0);
    old.wb.mode = WbMode::AsShot;
    s.set_develop(kelvin, old, "old as shot").unwrap();
    s.execute("develop.quickAdjust", &json!({"control": "wb.temp", "delta": 100})).unwrap();
    let d = dev(&s, kelvin);
    assert_eq!((d.wb.mode, d.wb.temp, d.wb.tint, d.wb.scale), (WbMode::Custom, 5086.0, -2.0, WbScale::Kelvin));
    let mut old = DevelopSettings::for_raw(6500.0, 0.0);
    (old.wb.mode, old.wb.temp, old.wb.tint) = (WbMode::Custom, 5800.0, 3.0);
    s.set_develop(kelvin, old, "old custom").unwrap();
    s.execute("develop.quickAdjust", &json!({"control": "wb.tint", "delta": 5})).unwrap();
    let d = dev(&s, kelvin);
    assert_eq!((d.wb.mode, d.wb.scale), (WbMode::Custom, WbScale::Kelvin));
    assert!((d.wb.temp - expect.0).abs() < 1e-6 && (d.wb.tint - (expect.1 + 5.0)).abs() < 1e-6, "{:?} vs {expect:?}", d.wb);

    // reset: As Shot, on the Kelvin scale
    s.execute("develop.reset", &json!({})).unwrap();
    let d = dev(&s, kelvin);
    assert_eq!((d.wb.mode, d.wb.scale), (WbMode::AsShot, WbScale::Kelvin));
    assert!(!s.catalog.photo(kelvin).unwrap().is_edited());
}

fn lightcraft_engine_effective(info: &lightcraft_pipeline::SourceInfo, d: &lightcraft_develop::DevelopSettings) -> (f64, f64) {
    lightcraft_pipeline::local::effective_wb(info, d)
}
