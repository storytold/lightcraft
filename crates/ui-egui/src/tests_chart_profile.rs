//! The Create Camera Profile dialog (ported from OneSlips, LR-PROF-CREATE): it opens on a chart photo,
//! reads it in the background with a row in the activity stack, finds the chart, suggests a name that
//! follows the light and look until typed, and refuses the same light twice. Procedural DNGs only;
//! nothing is built here (building files the profile in the user's settings folder; the engine's
//! tests cover it in a temporary one).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(60);

fn opts(h: &Headless) -> crate::chart_profile::ChartProfileDialog {
    match &h.app.ui.dialog {
        Some(Dialog::ChartProfile { opts }) => opts.clone(),
        _ => panic!("the chart dialog is open"),
    }
}

fn write(dir: &std::path::Path, name: &str, light: &str) -> String {
    let corners = lcc_engine::chart_profiling::camprof::ChartCorners::from_rect(90.0, 60.0, 180.0, 120.0);
    let p = dir.join(name);
    std::fs::write(&p, lcc_engine::chart_profiling::demo_chart_dng_of_body(360, 240, &corners, light, true, Some("ABC71")).unwrap()).unwrap();
    p.to_string_lossy().to_string()
}

#[test]
fn the_dialog_reads_the_chart_photo_and_suggests_a_name() {
    let dir = lcc_testkit::TempDir::new("lcc-ui-chart");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let paths = [write(&dir, "C_D55.dng", "D55"), write(&dir, "C_D55b.dng", "D55")];
    let app = LccApp::new(lcc_engine::Session::new(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    let r = h.app.session.execute("library.import", &json!({"paths": paths})).unwrap();
    let ids: Vec<u64> = r["imported"].as_array().unwrap().iter().filter_map(|v| v.as_u64()).collect();
    h.app.session.execute("library.select", &json!({"ids": [ids[0]], "active": ids[0]})).unwrap();
    crate::menus::run_ui_command(&mut h.app, "dialog.cameraProfile", &json!({})).unwrap().unwrap();
    assert!(h.step_until(T, |h| h.app.chart_profile.shots[0].as_ref().is_some_and(|s| s.prepared.is_some())));
    h.settle(T);
    let o = opts(&h);
    assert!(o.corners[0].is_some(), "the chart was found");
    assert!(o.name.contains("71 · Daylight · Photo"), "{}", o.name);
    assert!(crate::chart_profile::can_build(&h.app, &o));
    assert!(h.app.session.activity.list().is_empty(), "the reading row is gone");
    // the same light twice is refused before anything is built
    crate::menus::run_ui_command(&mut h.app, "cameraProfile.second", &json!({"id": ids[1]})).unwrap().unwrap();
    assert!(h.step_until(T, |h| h.app.chart_profile.shots[1].as_ref().is_some_and(|s| s.prepared.is_some())));
    h.settle(T);
    let mut o = opts(&h);
    o.lights = ["D55".into(), "D55".into()];
    assert!(crate::chart_profile::start_build(&mut h.app, &o).is_err());
    // bad UI-command arguments are errors, not panics
    for (cmd, p) in [("cameraProfile.zoom", json!({"mode": "sideways"})), ("cameraProfile.resolve", json!({"choice": "replace"}))] {
        assert!(crate::menus::run_ui_command(&mut h.app, cmd, &p).unwrap().is_err(), "{cmd} {p}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
