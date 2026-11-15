use super::*;

fn row(v: Value) -> Row {
    serde_json::from_value(v).unwrap()
}

fn layers(adjustment: Value, default: Value) -> Layers {
    Layers { adjustment: row(adjustment), default: row(default), ..Default::default() }
}

fn round(v: &Value) -> f64 {
    (v.as_f64().unwrap() * 1e9).round() / 1e9
}

fn rect(p: &Value) -> [f64; 4] {
    let r = &p["crop"]["geometry"]["rect"];
    [round(&r["x0"]), round(&r["y0"]), round(&r["x1"]), round(&r["y1"])]
}

#[test]
fn columns_belong_to_the_tool_with_the_longest_prefix() {
    assert_eq!(tool_of("ZLENSLCCLIGHTFALLOFFAMOUNT"), Some("LCC"));
    assert_eq!(tool_of("ZLENSLIGHTFALLOFF"), Some("Lens Correction"));
    assert_eq!(tool_of("ZGRADATIONCURVEY"), Some("Curve"));
    assert_eq!(tool_of("ZBWLIGHTTONEHUE"), Some("Black & White"));
    assert_eq!(tool_of("ZCLARITYSTRUCTURE"), Some("Clarity"));
    // imported, computed by Capture One, bookkeeping, the ICC profile: never reported
    for column in ["ZEXPOSURE", "ZCROP", "ZCALCULATEDDEHAZECOLOR", "ZLENSDEFAULTCHROMABR", "ZNAME", "ZICCPROFILE", "ZBRUSHSIZE"] {
        assert_eq!(tool_of(column), None, "{column}");
    }
}

#[test]
fn stored_values_compare_as_numbers() {
    assert!(same(&json!("1.852;1.000;1.426"), &json!("1.852000;1.000000;1.426000")));
    assert!(!same(&json!("1.852;1.000;1.426"), &json!("1.852;1.000;1.500")));
    assert!(same(&json!(0.0), &json!(0)));
    assert!(same(&json!("ExampleX1-Auto.fcrv"), &json!("ExampleX1-Auto.fcrv")));
    assert!(!same(&json!("1;2"), &json!("1;2;3")));
    // hostile lists are no numbers, never a panic
    for bad in ["", ";;;", "nan;1", "inf", "1e999", "x,y"] {
        assert_eq!(numbers(bad), None, "{bad}");
    }
    assert_eq!(numbers(&"1;".repeat(10_000)), None, "a huge list is no list");
}

#[test]
fn only_what_differs_from_the_default_is_the_variants() {
    let l = layers(
        json!({"ZEXPOSURE": 0.5, "ZBRIGHTNESS": 0, "ZCONTRAST": 10, "ZFLIP": 0, "ZROTATION": null}),
        json!({"ZEXPOSURE": 0, "ZBRIGHTNESS": 0, "ZCONTRAST": 0, "ZFLIP": 0, "ZROTATION": 90}),
    );
    let d = develop_of(&l, [6000.0, 4000.0], ("", ""));
    assert_eq!(d.exposure, Some(0.5));
    assert_eq!(d.flip, None);
    // a portrait the user didn't turn: the camera's orientation, nothing to do
    assert_eq!((d.rotation, d.camera_rotation), (90.0, 90.0));
    // Brightness set back to its default is not used; Contrast is
    assert_eq!(d.not_imported, vec!["Contrast"]);
    assert!(!d.is_empty());
    let untouched = develop_of(&layers(json!({"ZEXPOSURE": 0}), json!({"ZEXPOSURE": 0, "ZROTATION": 90})), [6000.0, 4000.0], ("", ""));
    assert!(untouched.is_empty() && untouched.not_imported.is_empty());
}

#[test]
fn a_style_counts_where_the_adjustment_layer_is_empty() {
    let mut l = layers(json!({"ZEXPOSURE": 1.0}), json!({"ZEXPOSURE": 0, "ZLENSLCC": null}));
    l.styles = vec![(1, row(json!({"ZEXPOSURE": -1.0, "ZLENSLCC": "0123abcd"})))];
    l.local = 2;
    let d = develop_of(&l, [6000.0, 4000.0], ("", ""));
    assert_eq!(d.exposure, Some(1.0), "the adjustment layer wins over a style");
    assert_eq!(d.not_imported, vec!["LCC", "Local adjustment layers"]);
}

#[test]
fn a_crop_without_a_turn_is_its_rectangle() {
    let d =
        CaptureOneDevelop { crop: Some([2760.0, 1840.0, 5520.0, 3105.0]), size: [5520.0, 3680.0], aspect: Some("16x9".into()), ..Default::default() };
    let (p, skipped) = partial(&d, crate::crs::Target::RawAbsolute, None);
    assert!(skipped.is_empty(), "{skipped:?}");
    let top = (3680.0 - 3105.0) / 2.0 / 3680.0;
    assert_eq!(rect(&p), [0.0, round(&json!(top)), 1.0, round(&json!(1.0 - top))]);
    assert_eq!(p["crop"]["aspect"], json!([16, 9]));
    assert_eq!(p["crop"]["geometry"]["angle"], json!(0.0));
    assert!(p.get("orientation").is_none());
}

/// A quarter turn the user made becomes the photo's orientation; the camera's own (a portrait) is the file's and
/// adds nothing. Either way the crop's centre is on the stored image and its size in the shown frame.
#[test]
fn quarter_turns_and_portraits() {
    let crop = Some([4500.0, 1000.0, 1000.0, 3000.0]);
    let size = [6000.0, 4000.0];
    // turned a quarter by the user: (x, y) on the stored image → (h − y, x) shown, 4000 × 6000
    let user = CaptureOneDevelop { rotation: 90.0 * ROTATION_SIGN, crop, size, aspect: Some("1x3".into()), ..Default::default() };
    let (p, _) = partial(&user, crate::crs::Target::RawAbsolute, None);
    assert_eq!(p["orientation"], json!("Rotate90"));
    let (u, v) = (4000.0 - 1000.0, 4500.0);
    assert_eq!(rect(&p), [(u - 500.0) / 4000.0, (v - 1500.0) / 6000.0, (u + 500.0) / 4000.0, (v + 1500.0) / 6000.0].map(|x| round(&json!(x))));
    assert_eq!(p["crop"]["aspect"], json!([1, 3]), "a portrait crop keeps a portrait aspect");
    // a portrait camera orientation: the same crop, no orientation of the user's
    let portrait = CaptureOneDevelop { camera_rotation: 90.0 * ROTATION_SIGN, ..user.clone() };
    let (q, _) = partial(&portrait, crate::crs::Target::RawAbsolute, None);
    assert!(q.get("orientation").is_none(), "{q}");
    assert_eq!(rect(&q), rect(&p));
    // a whole turn the other way round, and half a turn
    let back = CaptureOneDevelop { rotation: -90.0 * ROTATION_SIGN, crop: None, ..user.clone() };
    assert_eq!(partial(&back, crate::crs::Target::RawAbsolute, None).0["orientation"], json!("Rotate270"));
    let half = CaptureOneDevelop { rotation: 180.0, crop: None, ..user };
    assert_eq!(partial(&half, crate::crs::Target::RawAbsolute, None).0["orientation"], json!("Rotate180"));
}

#[test]
fn a_straighten_angle_turns_the_crop_about_the_centre() {
    let size = [6000.0, 4000.0];
    // a centred crop stays centred whatever the angle
    let d = CaptureOneDevelop { rotation: 2.0, crop: Some([3000.0, 2000.0, 5000.0, 3000.0]), size, ..Default::default() };
    let (p, _) = partial(&d, crate::crs::Target::RawAbsolute, None);
    assert_eq!(round(&p["crop"]["geometry"]["angle"]), 2.0 * ROTATION_SIGN);
    assert_eq!(rect(&p), [500.0 / 6000.0, 0.125, 5500.0 / 6000.0, 0.875].map(|x| round(&json!(x))));
    // an off-centre one moves round it
    let off = CaptureOneDevelop { crop: Some([4000.0, 2000.0, 1000.0, 1000.0]), ..d.clone() };
    let (q, _) = partial(&off, crate::crs::Target::RawAbsolute, None);
    let [x0, y0, x1, y1] = rect(&q);
    let (cx, cy) = ((x0 + x1) / 2.0 * 6000.0, (y0 + y1) / 2.0 * 4000.0);
    let a = (2.0 * ROTATION_SIGN).to_radians();
    assert!((cx - (3000.0 + 1000.0 * a.cos())).abs() < 1e-6 && (cy - (2000.0 + 1000.0 * a.sin())).abs() < 1e-6, "{cx} {cy}");
    // 91.5° is a quarter turn and 1.5°
    let turned = CaptureOneDevelop { rotation: 91.5, crop: None, ..d };
    let (t, _) = partial(&turned, crate::crs::Target::RawAbsolute, None);
    assert_eq!(round(&t["crop"]["geometry"]["angle"]), 1.5 * ROTATION_SIGN);
    assert_eq!(t["crop"]["geometry"]["rect"], json!({"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}));
}

#[test]
fn flips_and_crops_that_are_not_ones() {
    let d = |flip: i64| CaptureOneDevelop { flip: Some(flip), ..Default::default() };
    assert_eq!(partial(&d(1), crate::crs::Target::RawAbsolute, None).0["crop"]["flip_h"], json!(true));
    assert_eq!(partial(&d(2), crate::crs::Target::RawAbsolute, None).0["crop"]["flip_v"], json!(true));
    assert_eq!(partial(&d(7), crate::crs::Target::RawAbsolute, None).1, vec!["Flip"]);
    // a crop wholly outside the image is reported, not a zero-size crop
    let outside = CaptureOneDevelop { crop: Some([-5000.0, -5000.0, 100.0, 100.0]), size: [6000.0, 4000.0], ..Default::default() };
    let (p, skipped) = partial(&outside, crate::crs::Target::RawAbsolute, None);
    assert!(p.get("crop").is_none() && skipped == vec!["Crop (outside the image)"], "{p} {skipped:?}");
    for bad in [json!("1;2;3"), json!("1;2;0;5"), json!("a;b;c;d"), json!(5), json!("nan;1;2;3")] {
        assert_eq!(crop_of(&bad), None, "{bad}");
    }
    // an image of no size has no crop to place
    let no_size = CaptureOneDevelop { crop: Some([1.0, 1.0, 1.0, 1.0]), ..Default::default() };
    assert!(partial(&no_size, crate::crs::Target::RawAbsolute, None).0.get("crop").is_none());
}

#[test]
fn aspect_names() {
    assert_eq!(aspect_of("16x9", false), Some((16, 9)));
    assert_eq!(aspect_of("16x9", true), Some((9, 16)));
    assert_eq!(aspect_of("4x5", false), Some((5, 4)));
    assert_eq!(aspect_of("1x1 (Square)", true), Some((1, 1)));
    for none in ["Original", "Unconstrained", "", "0x5", "99999x1", "x"] {
        assert_eq!(aspect_of(none, false), None, "{none}");
    }
}

/// A camera the app has matrices for (any one: the conversion is the same for all).
fn camera() -> lcc_camdb::Entry {
    lcc_camdb::entries().into_iter().find(|e| lcc_raw::color::has_matrix(&e.color_data())).expect("a camera with matrices")
}

#[test]
fn white_balance_is_capture_ones_change_from_as_shot() {
    let entry = camera();
    let as_shot = kelvin(Some(&entry), [2.0, 1.0, 1.4]).unwrap();
    let warmer = kelvin(Some(&entry), [2.2, 1.0, 1.2]).unwrap();
    assert!(warmer.0 > as_shot.0, "less blue gain is a warmer white balance: {warmer:?} {as_shot:?}");
    let w = White { temp: warmer.0, tint: warmer.1, as_shot_temp: as_shot.0, as_shot_tint: as_shot.1 };
    let d = CaptureOneDevelop { white: Some(w), ..Default::default() };
    // without the file's as-shot white: the Kelvin through the camera's matrices
    let (p, _) = partial(&d, crate::crs::Target::RawAbsolute, None);
    assert_eq!((p["wb"]["mode"].as_str(), p["wb"]["scale"].as_str()), (Some("custom"), Some("kelvin")));
    assert!((p["wb"]["temp"].as_f64().unwrap() - warmer.0).abs() < 1e-6);
    // with it: the same change from the file's white (a white left as shot stays exactly as shot)
    let file = (5000.0, 3.0);
    let (q, _) = partial(&d, crate::crs::Target::RawAbsolute, Some(file));
    let expected = 1e6 / (1e6 / file.0 + 1e6 / warmer.0 - 1e6 / as_shot.0);
    assert!((q["wb"]["temp"].as_f64().unwrap() - expected).abs() < 1e-6, "{q}");
    assert!((q["wb"]["tint"].as_f64().unwrap() - (3.0 + warmer.1 - as_shot.1)).abs() < 1e-9);
    let unchanged = CaptureOneDevelop { white: Some(White { temp: as_shot.0, tint: as_shot.1, ..w }), ..Default::default() };
    let (u, _) = partial(&unchanged, crate::crs::Target::RawAbsolute, Some(file));
    assert!((u["wb"]["temp"].as_f64().unwrap() - file.0).abs() < 1e-6 && (u["wb"]["tint"].as_f64().unwrap() - file.1).abs() < 1e-9);
    // a raw edited relative to its as-shot look gets the relative scale; a rendered file none
    let (r, _) = partial(&d, crate::crs::Target::RawRelative, None);
    assert!(r["wb"]["temp"].as_f64().unwrap() > 6500.0 && r["wb"].get("scale").is_none(), "{r}");
    let (none, skipped) = partial(&d, crate::crs::Target::Rendered, None);
    assert!(none.get("wb").is_none() && skipped == vec!["White Balance (not a raw file)"]);
}

/// Gains are read as the app reads a raw's own: through the camera's matrices when it has them, else as a raw
/// without colour tags. A rendered file (Capture One names its format, not a camera) has no gains to read.
#[test]
fn white_balance_reads_as_the_app_reads_a_raws_gains() {
    let entry = camera();
    let l = layers(json!({"ZWHITEBALANCE": "2.200;1.000;1.200"}), json!({"ZWHITEBALANCE": "2.000000;1.000000;1.400000"}));
    let d = develop_of(&l, [6000.0, 4000.0], (&entry.make, &entry.model));
    let w = d.white.expect("a white balance");
    assert!(w.temp > w.as_shot_temp, "{w:?}");
    assert_eq!((w.temp, w.tint), kelvin(Some(&entry), [2.2, 1.0, 1.2]).unwrap());
    assert!(d.not_imported.is_empty());
    // a camera without matrices: the white a raw without colour tags gives, as the app reads its own gains
    let unknown = develop_of(&l, [6000.0, 4000.0], ("NO SUCH MAKER", "NO SUCH CAMERA 9"));
    let none = lcc_color::cct::xy_to_temp_tint(lcc_raw::color::as_shot_white_of(&lcc_raw::ColorData::default(), Some([2.2, 1.0, 1.2])));
    assert_eq!(unknown.white.map(|w| (w.temp, w.tint)), Some(none));
    assert!(unknown.not_imported.is_empty());
    let rendered = develop_of(&l, [6000.0, 4000.0], ("", ""));
    assert_eq!((rendered.white, rendered.not_imported), (None, vec!["White Balance (not a raw file)".to_string()]));
    // gains that aren't gains are no white balance
    for bad in ["0;1;1", "-1;1;1", "1;1", "1e9;1;1", "x;y;z"] {
        assert_eq!(gains(&json!(bad)), None, "{bad}");
    }
}
