//! Develop ▸ Lens Corrections ▸ Vignetting Calibration: the buttons run the `vignettingCalibration.*` commands, the panel shows the
//! map, its reference and the notices, and the Modern dock's Optics group has the Amount pill only with a map.

use serde_json::json;

use crate::tests_classic::{T, click, demo, has, run};

/// A raw (a demo scene) made active in Develop with Lens Corrections open; returns its id.
fn raw_in_develop(h: &mut crate::headless::Headless, name: &str) -> lcc_catalog::PhotoId {
    // no reference library: the tests never read or write the settings folder's
    h.app.session.vignetting_calibration_dir_override = Some(None);
    let id = h.app.session.catalog.alloc_photo_id();
    let mut p = lcc_catalog::Photo::new(id, lcc_catalog::Source::Demo { scene: 0 }, name, "NEF", 300, 200, "2026-01-01T10:00:00");
    p.kind = lcc_catalog::MediaKind::Raw;
    h.app.session.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    run(h, "library.select", json!({"ids": [id.0]}));
    run(h, "module.develop", json!({}));
    if !h.app.ui.section_open("optics") {
        h.app.ui.toggle_section("optics");
    }
    h.step();
    h.step();
    id
}

fn a_map() -> lcc_develop::EmbeddedVignettingMap {
    let source = lcc_develop::VignettingMapOrigin {
        lens: Some("NIKKOR Z 85mm f/1.8 S".into()),
        focal_length: Some(85.0),
        aperture: Some(1.8),
        file_name: Some("REF_0001.NEF".into()),
        captured: Some("2021-03-27T10:00:00".into()),
        sha1: Some("a9993e364706816aba3e25717850c26c9cd0d89d".into()),
        ..Default::default()
    };
    lcc_develop::EmbeddedVignettingMap::from_ev([300, 200], [3, 2], None, source, &[0.4, 0.1, 0.4, 0.4, 0.1, 0.4]).unwrap()
}

/// Scrolls the right panel until widget `id` is wholly visible above its bottom bar.
fn scroll_to(h: &mut crate::headless::Headless, id: &str) {
    let rect = |h: &crate::headless::Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r);
    let panel = rect(h, "panel:right_panel").expect("the right panel is drawn");
    h.request("ui.move", json!({"x": panel.center().x, "y": panel.center().y}), T);
    for _ in 0..30 {
        let Some(target) = rect(h, id) else { panic!("{id} is laid out") };
        let floor = rect(h, "button:previousBar").map_or(panel.bottom(), |r| r.top());
        if target.bottom() <= floor && target.top() >= panel.top() {
            return;
        }
        h.request("ui.scroll", json!({"dy": -120.0}), T);
        for _ in 0..6 {
            h.step();
        }
    }
    panic!("{id} never came into view");
}

/// No calibration: Create and Apply; Apply is off (with a reason) while no reference fits. With one: its source, its
/// reference (gone here), the notices, Apply and Remove; Remove is one undo step and keeps the Amount.
#[test]
fn the_section_shows_create_apply_and_remove() {
    let mut h = demo();
    let id = raw_in_develop(&mut h, "SHOT_0001.NEF");
    assert!(has(&h, "button:vignettingCalibrationCreate") && has(&h, "button:vignettingCalibrationApply"), "the empty state");
    assert!(!has(&h, "button:vignettingCalibrationRemove") && !has(&h, "button:vignettingCalibrationChoose"), "no library list");
    assert!(!has(&h, "link:vignettingCalibrationGuide"), "the guide is in What's New, not the panel");
    // nothing fits a demo scene (no raw file to place): Apply does nothing when clicked
    scroll_to(&mut h, "button:vignettingCalibrationApply");
    click(&mut h, "button:vignettingCalibrationApply");
    h.step();
    assert!(h.app.session.develop_of(id).unwrap().optics.vignetting_calibration.is_none());
    // a calibration in the catalog on the photo: its source, its reference (gone), the Amount and Remove
    let map = a_map();
    let fid = lcc_develop::VignettingMapId::of(&map);
    let record = Box::new(lcc_catalog::VignettingMapRecord { map, photo: None, added: String::new(), withdrawn: false });
    h.app.session.catalog.apply(lcc_catalog::Op::AddVignettingMap { id: fid.clone(), record }).unwrap();
    run(&mut h, "develop.merge", json!({"settings": {"optics": {"vignetting_calibration": fid.as_str(), "lens_profile": true}}}));
    for _ in 0..3 {
        h.step();
    }
    for w in [
        "label:vignettingCalibrationSource",
        "label:vignettingCalibrationReference",
        "label:vignettingCalibrationReferenceGone",
        "label:vignettingCalibrationReplacesProfile",
        "button:vignettingCalibrationRemove",
        "button:vignettingCalibrationApply",
    ] {
        assert!(has(&h, w), "{w}");
    }
    // the Modern dock's Optics group has the Amount pill with a calibration only
    let pills =
        |h: &crate::headless::Headless| crate::modern_dock::specs(&h.app, crate::modern_dock::Group::Optics).iter().map(|s| s.id).collect::<Vec<_>>();
    assert!(pills(&h).contains(&"optics.vignettingCalibrationAmount"));
    // Remove: one undo step, the amount kept
    run(&mut h, "develop.set", json!({"values": {"optics.vignettingCalibrationAmount": 60}}));
    h.step();
    scroll_to(&mut h, "button:vignettingCalibrationRemove");
    click(&mut h, "button:vignettingCalibrationRemove");
    h.step();
    let d = h.app.session.develop_of(id).unwrap();
    assert_eq!((d.optics.vignetting_calibration.clone(), d.optics.vignetting_calibration_amount), (None, 60.0));
    assert!(!pills(&h).contains(&"optics.vignettingCalibrationAmount"));
    assert!(!has(&h, "button:vignettingCalibrationRemove"), "back to the empty state");
}

/// With several photos selected Apply is on (each photo is checked by itself) and says how many photos it left as
/// they were; Create on a photo without a raw file on disk says why at once and leaves nothing measuring.
#[test]
fn apply_on_a_selection_reports_and_create_says_why_it_cant() {
    let mut h = demo();
    let other = raw_in_develop(&mut h, "SHOT_0002.NEF");
    let id = raw_in_develop(&mut h, "SHOT_0003.NEF");
    run(&mut h, "library.select", json!({"ids": [id.0, other.0], "active": id.0}));
    for _ in 0..3 {
        h.step();
    }
    scroll_to(&mut h, "button:vignettingCalibrationApply");
    click(&mut h, "button:vignettingCalibrationApply");
    h.step();
    let toast = h.app.ui.toast().map(|t| t.0.clone()).unwrap_or_default();
    assert!(toast.contains('2'), "both photos without a reference are counted: {toast}");
    scroll_to(&mut h, "button:vignettingCalibrationCreate");
    click(&mut h, "button:vignettingCalibrationCreate");
    h.step();
    assert!(h.app.ui.vignetting_calibration_creating.is_none(), "nothing left measuring");
    assert!(h.app.ui.toast().is_some(), "a demo scene has no raw file: said at once");
}

/// The reference photo says what it is and offers Stop Using as Reference (one undo step). Withdrawn, it is a normal
/// photo to the panel: nothing about the reference, no button to restore it (Create makes it a reference again). The
/// panel follows an undo and a restore by command (no settings change).
#[test]
fn a_reference_photo_can_be_withdrawn() {
    let mut h = demo();
    let id = raw_in_develop(&mut h, "REF_0001.NEF");
    let map = a_map();
    let fid = lcc_develop::VignettingMapId::of(&map);
    h.app.session.catalog.apply(lcc_catalog::Op::SetSha1 { id, sha1: map.source.sha1.clone() }).unwrap();
    let record = Box::new(lcc_catalog::VignettingMapRecord { map, photo: Some(id), added: String::new(), withdrawn: false });
    h.app.session.catalog.apply(lcc_catalog::Op::AddVignettingMap { id: fid.clone(), record }).unwrap();
    for _ in 0..3 {
        h.step();
    }
    let withdrawn = |h: &crate::headless::Headless| h.app.session.catalog.vignetting_map(&fid).map(|r| r.withdrawn);
    assert!(has(&h, "label:vignettingCalibrationIsReference0"), "the photo says it is a reference");
    scroll_to(&mut h, "button:vignettingCalibrationWithdraw0");
    click(&mut h, "button:vignettingCalibrationWithdraw0");
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(withdrawn(&h), Some(true));
    let reference_shown = |h: &crate::headless::Headless| {
        h.app.widgets.iter().any(|(w, _)| w.starts_with("label:vignettingCalibrationIsReference") || w.contains("vignettingCalibrationRestore"))
    };
    assert!(!reference_shown(&h) && !has(&h, "button:vignettingCalibrationWithdraw0"), "a normal photo again");
    // undo: in use again, and the panel follows
    run(&mut h, "edit.undo", json!({}));
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(withdrawn(&h), Some(false));
    assert!(has(&h, "label:vignettingCalibrationIsReference0") && has(&h, "button:vignettingCalibrationWithdraw0"));
    // withdrawn and restored by command (the CLI, MCP): the panel follows too
    run(&mut h, "vignettingCalibration.withdraw", json!({"map": fid.as_str()}));
    for _ in 0..3 {
        h.step();
    }
    assert!(!reference_shown(&h));
    run(&mut h, "vignettingCalibration.restore", json!({"map": fid.as_str()}));
    for _ in 0..3 {
        h.step();
    }
    assert!(has(&h, "label:vignettingCalibrationIsReference0"));
}

/// Create on a Borderline reference asks first: Create Anyway keeps the map measured meanwhile (no second measurement)
/// and applies it to the photo.
#[test]
fn create_anyway_keeps_the_borderline_reference() {
    let mut h = demo();
    let id = raw_in_develop(&mut h, "REF_0001.NEF");
    let map = a_map();
    let fid = lcc_develop::VignettingMapId::of(&map);
    let mut plausibility = lcc_raw::vignetting_plausibility::Plausibility::judge(lcc_raw::vignetting_plausibility::Metrics {
        level: 0.3,
        falloff: 1.0,
        detail_max: 0.07,
        ..Default::default()
    });
    assert_eq!(plausibility.verdict, lcc_raw::vignetting_plausibility::Verdict::Borderline);
    plausibility.reasons = vec![lcc_raw::vignetting_plausibility::Reason::Detail];
    let measured = lcc_engine::cmd::vignetting_calibration::Measured::for_tests(map, plausibility);
    h.app.ui.vignetting_calibration_pending = Some((id, Box::new(measured)));
    h.app.ui.dialog = Some(crate::state::Dialog::VignettingCreateAnyway { photo: id.0, reasons: "It has detail in it.".into() });
    h.step();
    h.request("ui.dialog.confirm", json!({}), T);
    h.step();
    assert_eq!(h.app.session.develop_of(id).unwrap().optics.vignetting_calibration.as_ref(), Some(&fid), "created and applied");
    assert!(h.app.ui.vignetting_calibration_pending.is_none() && h.app.ui.dialog.is_none());
    assert_eq!(h.app.session.catalog.vignetting_map(&fid).map(|r| r.withdrawn), Some(false));
}

/// Create of a reference for a combination that has one in use asks first; Replace keeps the new one and withdraws
/// the old one in one undo step; the report says when VR was on.
#[test]
fn create_asks_before_replacing_the_reference_in_use() {
    let mut h = demo();
    let id = raw_in_develop(&mut h, "REF_0002.NEF");
    // the reference in use: the same camera, lens and aperture, another shot
    let old = a_map();
    let old_id = lcc_develop::VignettingMapId::of(&old);
    let record = Box::new(lcc_catalog::VignettingMapRecord { map: old.clone(), photo: None, added: String::new(), withdrawn: false });
    h.app.session.catalog.apply(lcc_catalog::Op::AddVignettingMap { id: old_id.clone(), record }).unwrap();
    let mut source = old.source.clone();
    (source.file_name, source.sha1) = (Some("REF_0002.NEF".into()), Some("84983e441c3bd26ebaae4aa1f95129e5e54670f1".into()));
    let new = lcc_develop::EmbeddedVignettingMap::from_ev([300, 200], [3, 2], None, source, &[0.5, 0.1, 0.5, 0.5, 0.1, 0.5]).unwrap();
    let new_id = lcc_develop::VignettingMapId::of(&new);
    let pass = lcc_raw::vignetting_plausibility::Plausibility::judge(lcc_raw::vignetting_plausibility::Metrics {
        level: 0.3,
        falloff: 1.0,
        ..Default::default()
    });
    let mut measured = lcc_engine::cmd::vignetting_calibration::Measured::for_tests(new, pass);
    measured.vr = Some(true);
    crate::panels::edit::continue_vignetting_calibration(&mut h.app, id, measured, false).unwrap();
    let Some(crate::state::Dialog::VignettingReplace { question, .. }) = h.app.ui.dialog.clone() else { panic!("asks first: {:?}", h.app.ui.dialog) };
    assert!(question.contains("NIKKOR Z 85mm f/1.8 S") && question.contains("f/1.8") && question.contains("REF_0001.NEF"), "{question}");
    assert!(h.app.session.catalog.vignetting_map(&new_id).is_none(), "nothing created yet");
    h.step();
    h.request("ui.dialog.confirm", json!({}), T);
    for _ in 0..3 {
        h.step();
    }
    let state = |h: &crate::headless::Headless, m: &lcc_develop::VignettingMapId| h.app.session.catalog.vignetting_map(m).map(|r| r.withdrawn);
    assert_eq!((state(&h, &old_id), state(&h, &new_id)), (Some(true), Some(false)));
    assert_eq!(h.app.session.develop_of(id).unwrap().optics.vignetting_calibration.as_ref(), Some(&new_id));
    // the report's VR line
    scroll_to(&mut h, "label:vignettingCalibrationExposure0");
    let lines = h.app.widgets.iter().filter(|(w, _)| w.starts_with("label:vignettingCalibrationExposure")).count();
    assert_eq!(lines, 2, "the exposure line and the VR line");
    // one undo step: the old one back in use
    run(&mut h, "edit.undo", json!({}));
    h.step();
    assert_eq!((state(&h, &old_id), state(&h, &new_id)), (Some(false), Some(true)));
}
