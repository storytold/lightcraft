//! The Import dialog's camera profile rows (ported from OneSlips, LR-PROF-IMPORT-APPLY): one row
//! per camera body on the card, each choosing its profile for this import.

use std::time::Duration;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(60);

#[test]
fn each_body_on_the_card_gets_a_row_and_its_choice_goes_with_the_import() {
    let dir = lcc_testkit::TempDir::new("lcc-ui-importbodies");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let corners = lcc_engine::chart_profiling::camprof::ChartCorners::from_rect(10.0, 10.0, 40.0, 30.0);
    for (name, serial, light) in [("SA0001.dng", "SN1", "D55"), ("SA0002.dng", "SN1", "A"), ("SB0001.dng", "SN2", "D65")] {
        let bytes = lcc_engine::chart_profiling::demo_chart_dng_of_body(64, 48, &corners, light, true, Some(serial)).unwrap();
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    let app = LccApp::new(lcc_engine::Session::new().with_fs(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
    let candidates = lcc_engine::import::scan(&mut h.app.session, &[dir.to_string_lossy().to_string()]);
    let rows = lcc_engine::profile_bodies::plan(&h.app.session, &candidates, &Default::default());
    assert_eq!(rows.len(), 2, "two bodies: {rows:?}");
    assert_eq!(rows.iter().map(|r| r.photos).sum::<usize>(), 3);
    let mut d = crate::import::ImportDialog::new(candidates);
    d.per_body.insert(rows[0].body.clone(), "lc".into());
    h.app.ui.dialog = Some(crate::state::Dialog::Import { opts: Box::new(d) });
    h.settle(T);
    for r in &rows {
        assert!(h.app.widgets.iter().any(|(w, _)| *w == format!("combo:importBodyProfile:{}", r.body)), "a row for {}", r.body);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
