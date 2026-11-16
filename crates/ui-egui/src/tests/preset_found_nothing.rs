//! Issue #4364: after an adaptive preset is applied, the Presets panel says on how many of the photos it found
//! nothing to select, under the preset's Amount slider, and Show narrows the view to those photos.
//! Issue #4368: it also says for how many it can't tell yet, with Check, and updates when they have been looked at.

use std::time::Duration;

use crate::{LightcraftApp, Services, headless::Headless};
use lightcraft_catalog::PhotoId;
use serde_json::json;

const T: Duration = Duration::from_secs(10);

fn rect(h: &Headless, id: &str) -> Option<egui::Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

/// The demo library with three photos selected and the Presets panel open, with `preset` starred so its row is in
/// the Favorites section at the top (the Adaptive group is the last one, below the fold).
fn app(preset: &str) -> (Headless, Vec<u64>) {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    let ids: Vec<u64> = h.app.session.catalog.photos().take(3).map(|p| p.id.0).collect();
    h.app.run("library.select", json!({"ids": ids, "active": ids[0]})).unwrap();
    h.app.run("panel.presets", json!({})).unwrap();
    h.app.run("preset.favorite", json!({"id": preset})).unwrap();
    for _ in 0..5 {
        h.step();
    }
    (h, ids)
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    for _ in 0..4 {
        h.step();
    }
}

/// Whiten Teeth on three photos without mattes: the panel says "3 of 3", and Show leaves exactly those photos in
/// the view.
#[test]
fn the_panel_says_where_a_preset_found_nothing_and_shows_those_photos() {
    let (mut h, ids) = app("lc.adaptive-whiten-teeth");
    assert!(rect(&h, "presetFoundNothing").is_none(), "nothing applied yet");
    click(&mut h, "presetFavorite:lc.adaptive-whiten-teeth");
    assert!(rect(&h, "presetFoundNothing").is_some(), "{:?}", h.painted_text());
    assert!(h.painted_text().iter().any(|t| t.contains("3 of 3")), "{:?}", h.painted_text());
    assert!(h.app.session.filter.only.is_empty());
    click(&mut h, "button:presetShowNothing");
    let only: Vec<PhotoId> = ids.iter().map(|i| PhotoId(*i)).collect();
    assert_eq!(h.app.session.filter.only, only);
    assert_eq!((h.app.session.selection.ids.clone(), h.app.session.active()), (only.clone(), only.first().copied()), "and selects them");
}

/// A preset that needs no matte says nothing of the kind.
#[test]
fn a_preset_that_finds_something_everywhere_adds_no_note() {
    let (mut h, _) = app("lc.adaptive-darken-background");
    click(&mut h, "presetFavorite:lc.adaptive-darken-background");
    assert_eq!(h.app.session.develop_of(h.app.session.active().unwrap()).unwrap().masks.len(), 1, "the preset was applied");
    assert!(rect(&h, "presetFoundNothing").is_none());
    assert!(rect(&h, "button:presetShowNothing").is_none());
}

/// Issue #4368: three DNGs whose mattes were never looked at. The panel can't tell for any of them and offers
/// Check. Once their mattes are recorded (what Check's Reload from Disk commits), the lines update by themselves:
/// one carries teeth, two don't. Undoing that step brings "can't tell" back; undoing the preset removes the lines.
#[test]
fn the_panel_says_where_it_cannot_tell_and_updates_when_the_photos_were_looked_at() {
    use lightcraft_catalog::Op;
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    let template = lightcraft_catalog::Photo::clone(h.app.session.catalog.photos().next().unwrap());
    let mut ids = Vec::new();
    for _ in 0..3 {
        let mut p = template.clone();
        p.id = h.app.session.catalog.alloc_photo_id();
        p.format = "DNG".into();
        p.meta.mattes = None;
        ids.push(p.id);
        h.app.session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let raw: Vec<u64> = ids.iter().map(|i| i.0).collect();
    h.app.run("library.select", json!({"ids": raw, "active": raw[0]})).unwrap();
    h.app.run("panel.presets", json!({})).unwrap();
    h.app.run("preset.favorite", json!({"id": "lc.adaptive-whiten-teeth"})).unwrap();
    for _ in 0..5 {
        h.step();
    }
    click(&mut h, "presetFavorite:lc.adaptive-whiten-teeth");
    assert!(rect(&h, "presetCantTell").is_some(), "{:?}", h.painted_text());
    assert!(rect(&h, "button:presetCheckUnknown").is_some());
    assert!(rect(&h, "presetFoundNothing").is_none(), "nothing is known yet");
    assert!(h.painted_text().iter().any(|t| t.contains("for 3 photos")), "{:?}", h.painted_text());

    // what Reload from Disk commits for them: the first carries teeth, the others only skin
    let ops = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let mut meta = h.app.session.catalog.photo(*id).unwrap().meta.clone();
            meta.mattes = Some(vec![if i == 0 { "teeth".to_string() } else { "skin".to_string() }]);
            Op::SetMeta { id: *id, meta: Box::new(meta) }
        })
        .collect();
    h.app.session.commit("Reload Changed Files", Op::Batch { ops }).unwrap();
    for _ in 0..3 {
        h.step();
    }
    assert!(rect(&h, "presetCantTell").is_none(), "{:?}", h.painted_text());
    assert!(rect(&h, "presetFoundNothing").is_some());
    assert!(h.painted_text().iter().any(|t| t.contains("2 of 3")), "{:?}", h.painted_text());

    h.app.run("edit.undo", json!({})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    assert!(rect(&h, "presetCantTell").is_some(), "the reload undone: not looked at again");
    h.app.run("edit.undo", json!({})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    assert!(rect(&h, "presetCantTell").is_none() && rect(&h, "presetFoundNothing").is_none(), "the preset undone: no lines");
}
