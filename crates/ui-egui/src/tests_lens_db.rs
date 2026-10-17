//! Develop ▸ Optics says why Enable Lens Corrections has nothing to apply, and its "no lens database" link opens
//! Settings at the Lens profiles section (`app.settings {section}`).

use serde_json::json;

use crate::tests_classic::{T, click, demo, has, run};

/// A raw photo (rendered from a demo scene) whose metadata names `lens`, made active in Develop with Enable Lens
/// Corrections on and the Optics section open.
fn raw_in_develop(h: &mut crate::headless::Headless, lens: &str) -> lcc_catalog::PhotoId {
    let id = h.app.session.catalog.alloc_photo_id();
    let mut p = lcc_catalog::Photo::new(id, lcc_catalog::Source::Demo { scene: 0 }, "LENS0001.NEF", "NEF", 300, 200, "2026-01-01T10:00:00");
    p.kind = lcc_catalog::MediaKind::Raw;
    p.meta.lens = lens.into();
    h.app.session.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    run(h, "library.select", json!({"ids": [id.0]}));
    run(h, "module.develop", json!({}));
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
    if lcc_brand::env_os(lcc_engine::lens_profiles::ENV).is_some_and(|v| !v.is_empty()) {
        return;
    }
    let before = lcc_engine::lens_profiles::configured_dir();
    lcc_engine::lens_profiles::set_dir(None);
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
    lcc_engine::lens_profiles::set_dir(before);
    // (one test: the lens database is process-wide)
    optics_names_the_lens_the_database_lacks();
}

fn optics_names_the_lens_the_database_lacks() {
    if lcc_brand::env_os(lcc_engine::lens_profiles::ENV).is_some_and(|v| !v.is_empty()) {
        return;
    }
    let before = lcc_engine::lens_profiles::configured_dir();
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
    lcc_engine::lens_profiles::set_dir(before);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Develop ▸ Optics says what photos imported from now on start from and can make these settings it
/// under a name of the user's choosing (issue #148: the two checkboxes reset on every photo, so a
/// batch wants a default) — without touching the photo on screen or any photo already in the library.
#[test]
fn optics_can_become_the_default_for_new_photos() {
    let mut h = demo();
    let id = raw_in_develop(&mut h, "70-200mm F4");
    // the second of the pair, as a person would turn it on
    run(&mut h, "develop.merge", json!({"settings": {"optics": {"remove_ca": true}}, "label": "Remove CA"}));
    let photo_before = (*h.app.session.develop_of(id).expect("the photo has settings")).clone();
    assert!(has(&h, "button:opticsNewPhotoDefault"), "the Optics panel offers the action");

    // the action opens the naming dialog; nothing is saved yet
    click(&mut h, "button:opticsNewPhotoDefault");
    let Some(crate::state::Dialog::NewPhotoDefault { name }) = h.app.ui.dialog.clone() else {
        panic!("the naming dialog did not open: {:?}", h.app.ui.dialog);
    };
    assert!(name.is_empty(), "a first save starts from the engine's own name, not a stale one");
    assert!(h.app.session.import_defaults.raw_preset.is_none(), "nothing is set until it is named and saved");

    // and saving it under a name makes these settings the default for new photos
    h.app.ui.dialog = Some(crate::state::Dialog::NewPhotoDefault { name: "Birding optics".into() });
    h.step();
    click(&mut h, "button:dialogOk");

    let pid = h.app.session.import_defaults.raw_preset.clone().expect("new photos start from a preset now");
    let saved = h.app.session.presets.iter().find(|p| p.id == pid).cloned().expect("the preset was saved");
    assert_eq!(saved.name, "Birding optics");
    assert_eq!(saved.settings["optics"]["lens_profile"], json!(true));
    assert_eq!(saved.settings["optics"]["remove_ca"], json!(true));
    assert!(saved.settings.get("light").is_none(), "only the group asked for, never the whole edit");
    assert_eq!(*h.app.session.develop_of(id).expect("the photo has settings"), photo_before, "the photo on screen is untouched");
}
