//! Film negative conversion through the commands (`docs/negative-conversion.md`): Convert stores a measured
//! analysis, presets change the look but keep each frame's measured base, Before stays converted, the options and
//! Reset work, and bad arguments are errors, not panics.

use lightcraft_develop::{BaseSource, FilmColorModel, FilmTone, FilmType};
use serde_json::json;

use crate::Session;

fn neg(s: &Session) -> lightcraft_develop::Negative {
    s.develop_of(s.active().unwrap()).unwrap().negative
}

#[test]
fn convert_measures_and_stores_the_analysis() {
    let mut s = Session::with_demo();
    s.execute("negative.convert", &json!({})).unwrap();
    let n = neg(&s);
    assert_eq!(n.film, FilmType::ColorNegative);
    let a = n.analysis.expect("analysis stored");
    assert!(a.is_valid());
    let d = s.develop_of(s.active().unwrap()).unwrap();
    assert_eq!((d.wb.temp, d.wb.tint), (6500.0, 0.0), "white balance starts as converted");
    // one undo step back to the scan
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(neg(&s).film, FilmType::Off);
}

#[test]
fn presets_keep_the_measured_base() {
    let mut s = Session::with_demo();
    s.execute("negative.convert", &json!({})).unwrap();
    let before = neg(&s).analysis;
    s.execute("preset.apply", &json!({"id": "lc.neg-vivid"})).unwrap();
    let n = neg(&s);
    assert_eq!(n.tone, FilmTone::Hard);
    assert_eq!(n.analysis, before, "a conversion preset never replaces the measured base and range");
    for p in s.presets.iter().filter(|p| p.group == "Film Conversion") {
        assert!(p.settings["negative"].get("analysis").is_none(), "{} carries an analysis", p.id);
    }
}

#[test]
fn before_stays_converted() {
    let mut s = Session::with_demo();
    s.execute("negative.convert", &json!({})).unwrap();
    let id = s.active().unwrap();
    let p = s.catalog.photo(id).unwrap().clone();
    assert_eq!(s.before_settings(&p).negative.film, FilmType::ColorNegative);
    assert!(lightcraft_pipeline::before_settings(&p.develop).negative_active());
}

#[test]
fn options_sample_and_reset() {
    let mut s = Session::with_demo();
    s.execute("negative.set", &json!({"film": "colorNegative", "tone": "soft", "colorModel": "cool"})).unwrap();
    let n = neg(&s);
    assert_eq!((n.film, n.tone, n.color_model), (FilmType::ColorNegative, FilmTone::Soft, FilmColorModel::Cool));
    s.execute("negative.sampleBase", &json!({"rect": {"x0": 0.0, "y0": 0.0, "x1": 0.2, "y1": 0.05}})).unwrap();
    assert_eq!(neg(&s).analysis.unwrap().base_source, BaseSource::Sampled);
    // re-analysing keeps a sampled base
    s.execute("negative.analyze", &json!({})).unwrap();
    assert_eq!(neg(&s).analysis.unwrap().base_source, BaseSource::Sampled);
    s.execute("develop.set", &json!({"control": "negative.contrast", "value": 250})).unwrap();
    assert_eq!(neg(&s).contrast, 100.0, "sliders clamp");
    s.execute("negative.reset", &json!({})).unwrap();
    assert!(neg(&s).is_default());
}

#[test]
fn bad_arguments_are_errors() {
    let mut s = Session::with_demo();
    for (c, p) in [
        ("negative.convert", json!({"film": "kodachrome"})),
        ("negative.convert", json!({"film": "off"})),
        ("negative.convert", json!({"preset": "nope"})),
        ("negative.analyze", json!({})),
        ("negative.sampleBase", json!({})),
        ("negative.sampleBase", json!({"rect": {"x0": 2.0, "y0": 2.0, "x1": 3.0, "y1": 3.0}})),
        ("negative.sampleBase", json!({"rect": {"x0": "a"}})),
        ("negative.set", json!({})),
        ("negative.set", json!({"tone": 7})),
        ("negative.reset", json!({"ids": []})),
        ("negative.reset", json!({"ids": [999_999]})),
    ] {
        assert!(s.execute(c, &p).is_err(), "{c} {p}");
    }
}

#[test]
fn converting_resets_the_edits_made_on_the_negative_and_keeps_the_rest() {
    let mut s = Session::with_demo();
    s.execute("develop.set", &json!({"values": {"light.exposure": 1.2, "light.contrast": 30, "color.saturation": 20, "effects.dehaze": 15, "grain.amount": 25, "detail.sharpenAmount": 60}})).unwrap();
    s.execute("develop.wb", &json!({"mode": "custom", "temp": 4200, "tint": 12})).unwrap();
    s.execute("develop.merge", &json!({"settings": {"crop": {"geometry": {"angle": 0.0, "rect": {"x0": 0.1, "y0": 0.1, "x1": 0.9, "y1": 0.9}}}}}))
        .unwrap();
    let r = s.execute("negative.conflicts", &json!({})).unwrap();
    assert_eq!(r["reset"], json!(["White Balance", "Light", "Color", "Effects"]));
    let r = s.execute("negative.convert", &json!({})).unwrap();
    assert_eq!(r["reset"], json!(["White Balance", "Light", "Color", "Effects"]));
    let d = s.develop_of(s.active().unwrap()).unwrap();
    assert_eq!((d.light.exposure, d.light.contrast, d.color.saturation, d.effects.dehaze), (0.0, 0.0, 0.0, 0.0));
    assert_eq!(d.wb.mode, lightcraft_develop::WbMode::AsShot);
    // polarity-free edits stay
    assert_eq!((d.grain.amount, d.detail.sharpen_amount), (25.0, 60.0));
    assert!((d.crop.geometry.rect.x0 - 0.1).abs() < 1e-9);
    // one undo step brings everything back
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.develop_of(s.active().unwrap()).unwrap().light.exposure, 1.2);
    // edits on a converted photo are made on the positive: converting again keeps them
    s.execute("edit.redo", &json!({})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5})).unwrap();
    assert_eq!(s.execute("negative.conflicts", &json!({})).unwrap()["reset"], json!([]));
    s.execute("negative.convert", &json!({"film": "colorNegative"})).unwrap();
    assert_eq!(s.develop_of(s.active().unwrap()).unwrap().light.exposure, 0.5);
}

#[test]
fn keep_edits_converts_without_resetting() {
    let mut s = Session::with_demo();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.8})).unwrap();
    let r = s.execute("negative.convert", &json!({"keepEdits": true})).unwrap();
    assert_eq!(r["reset"], json!([]));
    assert_eq!(s.develop_of(s.active().unwrap()).unwrap().light.exposure, 0.8);
}
