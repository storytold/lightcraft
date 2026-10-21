//! Develop ▸ Optics says why Enable Lens Corrections has nothing to apply, and its "no lens database" link opens
//! Settings at the Lens profiles section (`app.settings {section}`).

use serde_json::json;

use std::time::Duration;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.settle(Duration::from_secs(120));
    h
}

fn run(h: &mut Headless, id: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": id, "params": params}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    h.step();
    r["result"].clone()
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
    h.step();
}

/// A raw photo (rendered from a demo scene) whose metadata names `lens`, made active in Develop with Enable Lens
/// Corrections on and the Optics section open.
fn raw_in_develop(h: &mut crate::headless::Headless, lens: &str) -> lightcraft_catalog::PhotoId {
    let id = h.app.session.catalog.alloc_photo_id();
    let mut p =
        lightcraft_catalog::Photo::new(id, lightcraft_catalog::Source::Demo { scene: 0 }, "LENS0001.NEF", "NEF", 300, 200, "2026-01-01T10:00:00");
    p.kind = lightcraft_catalog::MediaKind::Raw;
    p.meta.lens = lens.into();
    h.app.session.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    run(h, "library.select", json!({"ids": [id.0]}));
    h.app.ui.view = crate::state::ViewMode::Detail;
    h.app.ui.right = crate::state::RightPanel::Edit;
    run(h, "develop.merge", json!({"settings": {"optics": {"lens_profile": true}}, "label": "Lens Corrections"}));
    h.app.ui.open_sections = vec!["optics".into()];
    if !h.app.ui.section_open("optics") {
        h.app.ui.toggle_section("optics");
    }
    h.step();
    h.step();
    id
}

#[test]
fn optics_link_opens_settings_at_lens_profiles() {
    if std::env::var_os(lightcraft_engine::lens_profiles::ENV).is_some_and(|v| !v.is_empty()) {
        return;
    }
    let before = lightcraft_engine::lens_profiles::configured_dir();
    lightcraft_engine::lens_profiles::set_dir(None);
    let mut h = demo();
    raw_in_develop(&mut h, "70-200mm F4");
    assert!(has(&h, "link:lensDbSetup"), "no database: the Optics note is a link");
    click(&mut h, "link:lensDbSetup");
    let Some(crate::state::Dialog::Settings { tab }) = h.app.ui.dialog.clone() else { panic!("Settings did not open: {:?}", h.app.ui.dialog) };
    assert_eq!(tab, "import");
    h.step();
    assert!(has(&h, "section:lensProfiles"), "the Lens profiles section is shown");
    assert!(h.app.ui.settings_section.is_none(), "brought into view once");
    assert!(has(&h, "button:lensDbFolder"));
    // an unknown section is an error that lists the known ones
    let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"section": "nope"}}), T);
    assert!(r["error"].as_str().is_some_and(|e| e.contains("lensProfiles")), "{r}");
    lightcraft_engine::lens_profiles::set_dir(before);
    // (one test: the lens database is process-wide)
    optics_names_the_lens_the_database_lacks();
}

fn optics_names_the_lens_the_database_lacks() {
    if std::env::var_os(lightcraft_engine::lens_profiles::ENV).is_some_and(|v| !v.is_empty()) {
        return;
    }
    let before = lightcraft_engine::lens_profiles::configured_dir();
    let dir = std::env::temp_dir().join(format!("lens-ui-db-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("t.xml"),
        r#"<lensdatabase><lens><maker>Testcam</maker><model>Testcam 18-55mm f/3.5-5.6</model><mount>T</mount><cropfactor>1.5</cropfactor>
        <calibration><distortion model="poly3" focal="18" k1="-0.03" /></calibration></lens></lensdatabase>"#,
    )
    .unwrap();
    let mut h = demo();
    run(&mut h, "app.lensDbFolder", json!({"dir": dir.to_string_lossy()}));
    let id = raw_in_develop(&mut h, "70-200mm F4");
    assert!(!has(&h, "link:lensDbSetup"));
    assert!(has(&h, "label:lensNote"));
    let st = h.app.session.execute("lens.db.photo", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(st, json!({"state": "notFound", "lens": "70-200mm F4"}));
    lightcraft_engine::lens_profiles::set_dir(before);
    let _ = std::fs::remove_dir_all(&dir);
}
