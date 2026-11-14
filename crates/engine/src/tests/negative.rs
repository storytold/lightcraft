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

fn three_frames(s: &Session) -> Vec<u64> {
    s.catalog.photos().take(3).map(|p| p.id.0).collect()
}

#[test]
fn sync_roll_shares_one_locked_analysis_across_the_frames() {
    let mut s = Session::with_demo();
    let ids = three_frames(&s);
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.7, "ids": [ids[1]]})).unwrap();
    let r = s.execute("negative.syncRoll", &json!({"ids": ids, "wait": true})).unwrap();
    assert!(r["reset"].as_array().unwrap().contains(&json!("Light")), "{r}");
    let frames: Vec<_> = ids.iter().map(|id| s.develop_of(lightcraft_catalog::PhotoId(*id)).unwrap().negative).collect();
    for n in &frames {
        assert!(n.locked && n.film == FilmType::ColorNegative);
        let (a, b) = (n.analysis.unwrap(), frames[0].analysis.unwrap());
        assert_eq!((a.base, a.lo, a.hi, a.tones), (b.base, b.lo, b.hi, b.tones), "one analysis for the roll");
        assert!(a.region.is_some());
    }
    // one undo step
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(ids.iter().all(|id| !s.develop_of(lightcraft_catalog::PhotoId(*id)).unwrap().negative.converts()));
    s.execute("edit.redo", &json!({})).unwrap();
    // Re-analyze and Convert leave a locked frame's numbers alone; Unlock frees it
    let first = lightcraft_catalog::PhotoId(ids[0]);
    s.execute("library.select", &json!({"ids": [ids[0]], "active": ids[0]})).unwrap();
    let before = neg(&s).analysis;
    s.execute("negative.analyze", &json!({})).unwrap();
    s.execute("negative.convert", &json!({})).unwrap();
    assert_eq!(neg(&s).analysis, before);
    s.execute("negative.unlock", &json!({})).unwrap();
    assert!(!neg(&s).locked);
    s.execute("negative.analyze", &json!({})).unwrap();
    assert_ne!(neg(&s).analysis.map(|a| a.hi), before.map(|a| a.hi), "measured alone once unlocked");
    // a base picked on a frame makes it its own
    s.execute("negative.unlock", &json!({"ids": [ids[1]]})).unwrap();
    let _ = first;
}

#[test]
fn sync_roll_needs_two_frames_and_a_picked_base_unlocks() {
    let mut s = Session::with_demo();
    let ids = three_frames(&s);
    assert!(s.execute("negative.syncRoll", &json!({"ids": [ids[0]], "wait": true})).is_err());
    assert!(s.execute("negative.syncRoll", &json!({"ids": [ids[0], ids[0]], "wait": true})).is_err(), "duplicates are one frame");
    assert!(s.execute("negative.syncRoll", &json!({"ids": ids, "film": "kodachrome", "wait": true})).is_err());
    s.execute("negative.syncRoll", &json!({"ids": ids, "film": "bwNegative", "wait": true})).unwrap();
    s.execute("library.select", &json!({"ids": [ids[2]], "active": ids[2]})).unwrap();
    assert_eq!(neg(&s).film, FilmType::BwNegative);
    s.execute("negative.sampleBase", &json!({"rect": {"x0": 0.0, "y0": 0.0, "x1": 0.1, "y1": 0.05}})).unwrap();
    assert!(!neg(&s).locked, "a base picked on one frame makes it its own");
}

#[test]
fn converting_again_keeps_the_white_balance_set_on_the_positive() {
    let mut s = Session::with_demo();
    s.execute("negative.convert", &json!({})).unwrap();
    s.execute("develop.wb", &json!({"mode": "custom", "temp": 7200, "tint": 6})).unwrap();
    s.execute("negative.convert", &json!({"film": "colorNegative"})).unwrap();
    let wb = s.develop_of(s.active().unwrap()).unwrap().wb;
    assert_eq!((wb.temp, wb.tint), (7200.0, 6.0));
}

#[test]
fn stock_presets_set_a_look_and_keep_the_frame_s_numbers() {
    let mut s = Session::with_demo();
    s.execute("negative.convert", &json!({})).unwrap();
    let measured = neg(&s).analysis;
    let stocks: Vec<_> = s.presets.iter().filter(|p| p.group == "Film Stocks").cloned().collect();
    assert!(stocks.len() >= 10);
    for p in &stocks {
        assert!(p.settings["negative"].get("analysis").is_none() && p.settings["negative"].get("locked").is_none(), "{}", p.id);
        s.execute("preset.apply", &json!({"id": p.id})).unwrap();
        let n = neg(&s);
        assert!(n.converts(), "{}", p.id);
        assert_eq!(n.analysis, measured, "{} kept the measured base and range", p.id);
        let d = s.develop_of(s.active().unwrap()).unwrap();
        assert_eq!(d.treatment == lightcraft_develop::Treatment::Bw, n.film == FilmType::BwNegative, "{}: B&W film, B&W treatment", p.id);
        assert!(d.grain.amount > 0.0, "{} sets a grain", p.id);
    }
}

#[test]
fn mixed_scans_still_sync_but_warn() {
    use lightcraft_pipeline::negative::{FrameMeasure, base_spread};
    let frame = |b: [f32; 3], src: lightcraft_develop::BaseSource| FrameMeasure {
        px: vec![[0.1; 3]; 4],
        base: b,
        base_source: src,
        base_rect: None,
        region: lightcraft_geom::Rect::UNIT,
    };
    use lightcraft_develop::BaseSource::{Detected, Estimated};
    assert_eq!(base_spread(&[frame([0.6, 0.5, 0.3], Detected)]), 1.0);
    assert!(base_spread(&[frame([0.6, 0.5, 0.3], Detected), frame([0.62, 0.51, 0.31], Detected)]) < 1.15, "one setup");
    assert!(base_spread(&[frame([0.67, 0.65, 0.57], Detected), frame([0.31, 0.30, 0.24], Detected)]) > 2.0, "two setups");
    assert_eq!(base_spread(&[frame([0.6, 0.5, 0.3], Detected), frame([0.2, 0.2, 0.2], Estimated)]), 1.0, "estimates don't count");
    // the demo frames carry no film edge: no warning
    let mut s = Session::with_demo();
    let ids = three_frames(&s);
    let r = s.execute("negative.syncRoll", &json!({"ids": ids, "wait": true})).unwrap();
    assert!(r.get("warning").is_none(), "{r}");
}

#[test]
fn copy_before_to_after_keeps_the_conversion() {
    let mut s = Session::with_demo();
    s.execute("negative.convert", &json!({})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.4})).unwrap();
    s.execute("beforeAfter.copyBeforeToAfter", &json!({})).unwrap();
    let d = s.develop_of(s.active().unwrap()).unwrap();
    assert!(d.negative.converts() && d.light.exposure == 0.0, "the before state, still converted");
    s.execute("beforeAfter.swap", &json!({})).unwrap();
    assert!(neg(&s).converts());
}

#[test]
fn a_frame_edited_while_the_roll_is_read_keeps_its_edit() {
    use crate::cmd::negative::{roll_apply, roll_measure, roll_plan};
    let mut s = Session::with_demo();
    let ids = three_frames(&s);
    let plan = roll_plan(&mut s, &json!({"ids": ids})).unwrap();
    let measured = roll_measure(&plan, &std::sync::atomic::AtomicBool::new(false), &|_| ()).unwrap();
    // meanwhile frame 2 is cropped
    let edited = lightcraft_catalog::PhotoId(ids[1]);
    s.execute(
        "develop.merge",
        &json!({"ids": [ids[1]], "settings": {"crop": {"geometry": {"angle": 0.0, "rect": {"x0": 0.2, "y0": 0.2, "x1": 0.8, "y1": 0.8}}}}}),
    )
    .ok();
    if s.develop_of(edited).unwrap().crop.geometry.rect.x0 < 0.1 {
        let mut d = (*s.develop_of(edited).unwrap()).clone();
        d.crop.geometry.rect = lightcraft_geom::Rect::new(0.2, 0.2, 0.8, 0.8);
        s.set_develop(edited, d, "crop").unwrap();
    }
    let r = roll_apply(&mut s, plan, measured).unwrap();
    assert!(r["skipped"].as_array().unwrap().iter().any(|v| v["id"] == ids[1]), "{r}");
    assert!(!s.develop_of(edited).unwrap().negative.locked, "the edited frame isn't overwritten");
    assert!(s.develop_of(lightcraft_catalog::PhotoId(ids[0])).unwrap().negative.locked);
}

#[test]
fn a_locked_frame_switched_to_another_film_type_is_measured_again() {
    let mut s = Session::with_demo();
    let ids = three_frames(&s);
    s.execute("negative.syncRoll", &json!({"ids": ids, "wait": true})).unwrap();
    s.execute("library.select", &json!({"ids": [ids[0]], "active": ids[0]})).unwrap();
    // a preset switches the type without going through negative.set
    s.execute("develop.merge", &json!({"settings": {"negative": {"film": "slide"}}})).unwrap();
    s.execute("negative.analyze", &json!({})).unwrap();
    let n = neg(&s);
    assert_eq!(n.analysis.unwrap().film, Some(FilmType::Slide));
    assert!(!n.locked, "its own analysis now, not the roll's");
}

#[test]
fn a_new_film_type_from_negative_set_is_measured_and_stored() {
    let mut s = Session::with_demo();
    s.execute("negative.convert", &json!({})).unwrap();
    s.execute("negative.set", &json!({"film": "slide"})).unwrap();
    let a = neg(&s).analysis.expect("stored, not left to the render");
    assert_eq!(a.film, Some(FilmType::Slide));
}

#[test]
fn switching_an_unconverted_photo_on_resets_its_negative_edits_and_picking_a_base_needs_a_conversion() {
    let mut s = Session::with_demo();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.9})).unwrap();
    assert!(s.execute("negative.sampleBase", &json!({"rect": {"x0": 0.0, "y0": 0.0, "x1": 0.1, "y1": 0.1}})).is_err(), "convert first");
    let r = s.execute("negative.set", &json!({"film": "colorNegative"})).unwrap();
    assert!(r["reset"].as_array().unwrap().contains(&json!("Light")), "{r}");
    assert_eq!(s.develop_of(s.active().unwrap()).unwrap().light.exposure, 0.0);
}

#[test]
fn convert_with_a_film_preset_and_kept_edits_still_starts_the_white_balance_as_converted() {
    let mut s = Session::with_demo();
    s.execute("develop.wb", &json!({"mode": "custom", "temp": 4100, "tint": 15})).unwrap();
    s.execute("negative.convert", &json!({"preset": "lc.neg-standard", "keepEdits": true})).unwrap();
    let wb = s.develop_of(s.active().unwrap()).unwrap().wb;
    assert_eq!((wb.mode, wb.temp, wb.tint), (lightcraft_develop::WbMode::AsShot, 6500.0, 0.0));
}

#[test]
fn a_roll_takes_its_film_type_from_its_own_frames() {
    let mut s = Session::with_demo();
    let ids: Vec<u64> = s.catalog.photos().take(4).map(|p| p.id.0).collect();
    // frames 1 and 2 are B&W negatives; the active photo (frame 3) is an unrelated colour negative outside the roll
    s.execute("negative.set", &json!({"ids": [ids[0], ids[1]], "film": "bwNegative"})).unwrap();
    s.execute("library.select", &json!({"ids": [ids[3]], "active": ids[3]})).unwrap();
    s.execute("negative.convert", &json!({})).unwrap();
    s.execute("negative.syncRoll", &json!({"ids": [ids[0], ids[1]], "wait": true})).unwrap();
    for id in [ids[0], ids[1]] {
        assert_eq!(s.develop_of(lightcraft_catalog::PhotoId(id)).unwrap().negative.film, FilmType::BwNegative);
    }
}

#[test]
fn a_preset_made_from_a_converted_photo_carries_no_film_base_or_lock() {
    let mut s = Session::with_demo();
    let ids = three_frames(&s);
    s.execute("negative.syncRoll", &json!({"ids": ids, "wait": true})).unwrap();
    let d = s.develop_of(lightcraft_catalog::PhotoId(ids[0])).unwrap();
    let p = lightcraft_develop::Preset::from_settings("x", "X", "User", &d, &[lightcraft_develop::SettingsGroup::Negative]);
    let n = &p.settings["negative"];
    assert!(n.get("analysis").is_none() && n.get("locked").is_none() && n["film"] == "colorNegative", "{n}");
}

#[test]
fn updating_a_preset_from_a_converted_photo_keeps_out_its_film_base_and_lock() {
    let mut s = Session::with_demo();
    let ids = three_frames(&s);
    s.execute("negative.syncRoll", &json!({"ids": ids, "wait": true})).unwrap();
    s.execute("library.select", &json!({"ids": [ids[0]], "active": ids[0]})).unwrap();
    let r = s.execute("preset.create", &json!({"name": "Mine", "groups": ["negative"]})).unwrap();
    let pid = r["id"].as_str().unwrap_or_default().to_string();
    let pid = if pid.is_empty() { s.presets.iter().find(|p| p.name == "Mine").unwrap().id.clone() } else { pid };
    for p in [json!({"id": pid}), json!({"id": pid, "groups": ["negative"]})] {
        s.execute("preset.update", &p).unwrap();
        let n = &s.presets.iter().find(|x| x.id == pid).unwrap().settings["negative"];
        assert!(n.get("analysis").is_none() && n.get("locked").is_none(), "{p}: {n}");
        assert_eq!(n["film"], "colorNegative", "{p}: the group's options are kept");
    }
}

#[test]
fn a_b_and_w_switch_keeps_a_sampled_base() {
    let mut s = Session::with_demo();
    s.execute("negative.convert", &json!({})).unwrap();
    s.execute("negative.sampleBase", &json!({"rect": {"x0": 0.0, "y0": 0.0, "x1": 0.1, "y1": 0.05}})).unwrap();
    s.execute("negative.set", &json!({"film": "bwNegative"})).unwrap();
    let a = neg(&s).analysis.unwrap();
    assert_eq!((a.base_source, a.film), (lightcraft_develop::BaseSource::Sampled, Some(FilmType::BwNegative)));
}

#[test]
fn a_frame_left_out_of_a_roll_doesn_t_count_in_its_numbers() {
    use crate::cmd::negative::{roll_apply, roll_measure, roll_plan};
    let mut s = Session::with_demo();
    let ids = three_frames(&s);
    let plan = roll_plan(&mut s, &json!({"ids": ids})).unwrap();
    let measured = roll_measure(&plan, &std::sync::atomic::AtomicBool::new(false), &|_| ()).unwrap();
    // meanwhile frame 2 gets an edit the sync would reset
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.8, "ids": [ids[1]]})).unwrap();
    let r = roll_apply(&mut s, plan, measured).unwrap();
    assert!(r["skipped"].as_array().unwrap().iter().any(|v| v["id"] == ids[1]), "{r}");
    assert_eq!(s.develop_of(lightcraft_catalog::PhotoId(ids[1])).unwrap().light.exposure, 0.8, "its edit is kept");
    // the others carry the roll of frames 1 and 3 alone
    let written = s.develop_of(lightcraft_catalog::PhotoId(ids[0])).unwrap().negative.analysis.unwrap();
    let mut fresh = Session::with_demo();
    fresh.execute("negative.syncRoll", &json!({"ids": [ids[0], ids[2]], "wait": true})).unwrap();
    let alone = fresh.develop_of(lightcraft_catalog::PhotoId(ids[0])).unwrap().negative.analysis.unwrap();
    assert_eq!((written.lo, written.hi, written.tones), (alone.lo, alone.hi, alone.tones));
}

#[test]
fn a_white_balance_preset_that_switches_the_conversion_off_changes_nothing_at_zero() {
    let mut s = Session::with_demo();
    s.execute("negative.convert", &json!({})).unwrap();
    s.presets.push(lightcraft_develop::Preset {
        id: "user.wb-off".into(),
        name: "WB, conversion off".into(),
        group: "User".into(),
        settings: json!({"negative": {"film": "off"}, "wb": {"temp": 5000.0}}),
        favorite: false,
        builtin: false,
    });
    s.execute("preset.apply", &json!({"id": "user.wb-off", "amount": 0})).unwrap();
    assert!(neg(&s).converts(), "0 % leaves the photo converted");
    s.execute("preset.apply", &json!({"id": "user.wb-off", "amount": 100})).unwrap();
    assert!(!neg(&s).converts(), "100 % applies it");
}

#[test]
fn updating_a_preset_from_an_unconverted_photo_keeps_its_negative_group() {
    let mut s = Session::with_demo();
    s.execute("negative.convert", &json!({})).unwrap();
    s.execute("preset.create", &json!({"name": "Conv", "groups": ["negative"]})).unwrap();
    let pid = s.presets.iter().find(|p| p.name == "Conv").unwrap().id.clone();
    s.execute("negative.reset", &json!({})).unwrap();
    s.execute("preset.update", &json!({"id": pid})).unwrap();
    let n = &s.presets.iter().find(|x| x.id == pid).unwrap().settings["negative"];
    assert_eq!(n["film"], "off", "the group stays, now off: {n}");
}

/// Convert to another film type unlocks a roll-synced frame, even one whose analysis predates the recorded film type
/// (it would otherwise "fit" any type and keep the roll's numbers for the old one).
#[test]
fn convert_to_another_film_type_unlocks_a_roll_frame() {
    let mut s = Session::with_demo();
    let ids = three_frames(&s);
    s.execute("negative.syncRoll", &json!({"ids": ids, "wait": true})).unwrap();
    s.execute("library.select", &json!({"ids": [ids[0]], "active": ids[0]})).unwrap();
    s.execute("develop.merge", &json!({"settings": {"negative": {"analysis": {"film": null}}}})).unwrap();
    assert!(neg(&s).locked && neg(&s).analysis.unwrap().film.is_none());
    s.execute("negative.convert", &json!({"film": "bwNegative"})).unwrap();
    let n = neg(&s);
    assert_eq!(n.film, FilmType::BwNegative);
    assert!(!n.locked, "measured on its own for the new type");
    assert_eq!(n.analysis.unwrap().film, Some(FilmType::BwNegative));
}

/// A `film` of null names no film: an already converted photo keeps its type.
#[test]
fn convert_with_a_null_film_keeps_the_film_type() {
    let mut s = Session::with_demo();
    s.execute("negative.convert", &json!({"film": "slide"})).unwrap();
    s.execute("negative.convert", &json!({"film": null})).unwrap();
    assert_eq!(neg(&s).film, FilmType::Slide);
}

/// An option name the command doesn't know is an error, not the option's fallback.
#[test]
fn a_misspelled_option_is_an_error() {
    let mut s = Session::with_demo();
    s.execute("negative.convert", &json!({})).unwrap();
    for p in [json!({"tone": "sof"}), json!({"colorModel": "warmish"}), json!({"film": "negative"})] {
        assert!(s.execute("negative.set", &p).is_err(), "{p}");
    }
    assert_eq!(neg(&s).tone, FilmTone::Standard);
    s.execute("negative.set", &json!({"tone": "soft"})).unwrap();
    assert_eq!(neg(&s).tone, FilmTone::Soft);
}

/// Setting the film type to off on a photo that isn't converted resets nothing: only switching on does.
#[test]
fn switching_an_unconverted_photo_off_keeps_its_edits() {
    let mut s = Session::with_demo();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.7})).unwrap();
    s.execute("develop.merge", &json!({"settings": {"negative": {"film": "someFutureType"}}})).unwrap();
    assert_eq!(neg(&s).film, FilmType::Unknown);
    s.execute("negative.set", &json!({"film": "off"})).unwrap();
    let d = s.develop_of(s.active().unwrap()).unwrap();
    assert_eq!(d.light.exposure, 0.7);
}

/// A photo no longer converted gets its source's own white balance back: the relative values set on the positive
/// don't carry over as the raw's.
#[test]
fn switching_the_conversion_off_restores_the_source_white_balance() {
    for off in [json!(["negative.reset", {}]), json!(["negative.set", {"film": "off"}])] {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        let original = s.develop_of(id).unwrap().wb;
        s.execute("negative.convert", &json!({})).unwrap();
        s.execute("develop.set", &json!({"control": "wb.temp", "value": 9000})).unwrap();
        s.execute(off[0].as_str().unwrap(), &off[1]).unwrap();
        let d = s.develop_of(id).unwrap();
        assert!(!d.negative.converts());
        assert_eq!((d.wb.mode, d.wb.temp, d.wb.tint), (original.mode, original.temp, original.tint), "{off}");
    }
}

/// Slide film has no base to pick: the Film Base Selector is an error there, not a stored base the render ignores.
#[test]
fn a_slide_has_no_film_base_to_sample() {
    let mut s = Session::with_demo();
    s.execute("negative.convert", &json!({"film": "slide"})).unwrap();
    assert!(s.execute("negative.sampleBase", &json!({"rect": {"x0": 0.0, "y0": 0.0, "x1": 0.2, "y1": 0.05}})).is_err());
    assert_ne!(neg(&s).analysis.unwrap().base_source, BaseSource::Sampled);
}
