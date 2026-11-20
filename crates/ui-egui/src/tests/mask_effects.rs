//! Headless tests of mask effect presets: the Effect menu (groups, search, keys, hover preview,
//! right-click actions), the New Effect Preset dialog and the Manage Effect Presets window.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn detail() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    exec(&mut h, "panel.masking", json!({}));
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

fn req(h: &mut Headless, method: &str, params: serde_json::Value) {
    let r = h.request(method, params, T);
    assert_eq!(r["ok"], true, "{method}: {r}");
}

fn click(h: &mut Headless, id: &str) {
    req(h, "ui.clickWidget", json!({"id": id}));
}

fn mask(h: &Headless) -> lightcraft_develop::Mask {
    let id = h.app.session.active().expect("active photo");
    let d = h.app.session.develop_of(id).unwrap_or_default();
    d.masks.iter().find(|m| Some(m.id) == h.app.session.active_mask).cloned().expect("a selected mask")
}

/// Three effects in two groups, one starred; a radial mask selected.
fn setup() -> Headless {
    let mut h = detail();
    exec(&mut h, "mask.add", json!({"kind": "radial"}));
    for (name, values, group) in [
        ("01 Dodge", json!({"exposure": 0.5}), ""),
        ("12 Sky", json!({"highlights": -60}), "Detail"),
        ("21 Skin", json!({"clarity": -40}), "Retouch"),
    ] {
        exec(&mut h, "mask.adjust", json!({"values": values}));
        exec(&mut h, "mask.saveEffect", json!({"name": name, "group": group}));
        let zero: serde_json::Map<String, serde_json::Value> =
            lightcraft_engine::cmd::mask_effects::FIELDS.iter().map(|k| (k.to_string(), json!(0))).collect();
        exec(&mut h, "mask.adjust", json!({"values": zero}));
    }
    exec(&mut h, "mask.favoriteEffect", json!({"name": "21 Skin"}));
    h.step();
    h
}

#[test]
fn effect_menu_groups_searches_and_applies_with_keys() {
    let mut h = setup();
    click(&mut h, "dropdown:maskEffect");
    h.step();
    let widgets = h.request("ui.widgets", json!({}), T);
    let ids: Vec<String> =
        widgets["result"].as_array().map(|a| a.iter().filter_map(|w| w["id"].as_str().map(str::to_string)).collect()).unwrap_or_default();
    for id in ["maskEffect:01 Dodge", "maskEffect:12 Sky", "maskEffect:21 Skin", "maskEffectFav:21 Skin", "maskEffectSearch", "maskEffectMenu:manage"]
    {
        assert!(ids.iter().any(|x| x == id), "{id} in {ids:?}");
    }
    // the pointer resting on an effect previews it on the photo
    req(&mut h, "ui.hoverWidget", json!({"id": "maskEffect:12 Sky"}));
    h.step();
    let preview = h.app.hover_preview.as_ref().expect("a hover preview");
    assert_eq!(preview.settings.masks.last().map(|m| m.adjust.highlights), Some(-60.0));
    assert_eq!(mask(&h).adjust.highlights, 0.0, "only previewed");
    // typing filters; ↓ and Return apply the first match
    req(&mut h, "ui.text", json!({"text": "skin"}));
    h.step();
    let widgets = h.request("ui.widgets", json!({}), T);
    let ids: Vec<String> =
        widgets["result"].as_array().map(|a| a.iter().filter_map(|w| w["id"].as_str().map(str::to_string)).collect()).unwrap_or_default();
    assert!(!ids.iter().any(|x| x == "maskEffect:12 Sky"), "filtered out: {ids:?}");
    req(&mut h, "ui.key", json!({"key": "ArrowDown"}));
    h.step();
    req(&mut h, "ui.key", json!({"key": "Enter"}));
    h.step();
    h.step();
    assert_eq!(mask(&h).adjust.clarity, -40.0);
    assert_eq!(exec(&mut h, "mask.effects", json!({}))["current"], "21 Skin");
    // a new radial mask now starts with it (the menu remembers per tool)
    exec(&mut h, "mask.add", json!({"kind": "radial"}));
    assert_eq!(mask(&h).adjust.clarity, -40.0);
}

#[test]
fn effect_menu_right_click_and_delete_asks_first() {
    let mut h = setup();
    click(&mut h, "dropdown:maskEffect");
    h.step();
    // the row's actions are commands; delete goes through a confirmation
    h.app.ui.dialog = Some(Dialog::DeleteEffects { names: vec!["01 Dodge".into()] });
    h.step();
    assert_eq!(h.app.session.mask_effects.len(), 3, "nothing deleted before confirming");
    click(&mut h, "button:dialogCancel");
    assert_eq!(h.app.session.mask_effects.len(), 3);
    h.app.ui.dialog = Some(Dialog::DeleteEffects { names: vec!["01 Dodge".into()] });
    h.step();
    req(&mut h, "ui.dialog.confirm", json!({}));
    assert_eq!(h.app.session.mask_effects.len(), 2);
    // Reset Sliders zeroes the mask and forgets the tool's effect
    exec(&mut h, "mask.applyEffect", json!({"name": "12 Sky"}));
    h.step();
    click(&mut h, "dropdown:maskEffect");
    h.step();
    click(&mut h, "maskEffectMenu:reset");
    h.step();
    assert_eq!(mask(&h).adjust.highlights, 0.0);
    assert!(h.app.session.mask_tool_effects.is_empty());
}

#[test]
fn save_dialog_picks_sliders_amount_and_brush() {
    let mut h = setup();
    exec(&mut h, "mask.adjust", json!({"values": {"shadows": 30, "texture": 15, "amount": 70}}));
    h.step();
    click(&mut h, "dropdown:maskEffect");
    h.step();
    click(&mut h, "maskEffectMenu:save");
    h.step();
    assert!(matches!(h.app.ui.dialog, Some(Dialog::SaveEffect { .. })), "{:?}", h.app.ui.dialog);
    // nothing named: an error, the dialog stays
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], false, "{r}");
    click(&mut h, "effectInclude:changed");
    h.step();
    click(&mut h, "check:effectAmount");
    h.step();
    req(&mut h, "ui.clickWidget", json!({"id": "field:effectName"}));
    req(&mut h, "ui.text", json!({"text": "Lift"}));
    h.step();
    let Some(Dialog::SaveEffect { fields, amount, .. }) = &h.app.ui.dialog else { panic!("{:?}", h.app.ui.dialog) };
    assert_eq!((fields.clone(), *amount), (vec!["shadows".to_string(), "texture".into()], true));
    req(&mut h, "ui.dialog.confirm", json!({}));
    let e = h.app.session.mask_effects.iter().find(|e| e.name == "Lift").expect("saved");
    assert_eq!((e.fields.clone(), e.with_amount, e.adjust.amount), (vec!["shadows".to_string(), "texture".into()], true, 70.0));
    assert!(e.brush.is_none(), "not the brush tool");
}

#[test]
fn manage_window_renames_groups_stars_duplicates_and_deletes() {
    let mut h = setup();
    exec(&mut h, "dialog.manageMaskEffects", json!({}));
    h.step();
    assert!(matches!(h.app.ui.dialog, Some(Dialog::ManageEffects { .. })));
    // select and rename inline
    click(&mut h, "effectRow:12 Sky");
    h.step();
    click(&mut h, "button:effectRename");
    h.step();
    req(&mut h, "ui.key", json!({"key": "A", "modifiers": {"command": true}}));
    req(&mut h, "ui.text", json!({"text": "12 Sky Darken"}));
    req(&mut h, "ui.key", json!({"key": "Enter"}));
    h.step();
    h.step();
    assert!(
        h.app.session.mask_effects.iter().any(|e| e.name == "12 Sky Darken"),
        "{:?}",
        h.app.session.mask_effects.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
    // duplicate selects the copy; star it; apply it to the mask
    click(&mut h, "button:effectDuplicate");
    h.step();
    let Some(Dialog::ManageEffects { selected, .. }) = &h.app.ui.dialog else { panic!() };
    assert_eq!(selected, &vec!["12 Sky Darken copy".to_string()]);
    click(&mut h, "effectStar:12 Sky Darken copy");
    h.step();
    assert!(h.app.session.mask_effects.iter().any(|e| e.name == "12 Sky Darken copy" && e.favorite));
    click(&mut h, "button:effectApply");
    h.step();
    assert_eq!(mask(&h).adjust.highlights, -60.0);
    // search narrows the list; a group heading selects the group
    req(&mut h, "ui.clickWidget", json!({"id": "field:effectSearch"}));
    req(&mut h, "ui.text", json!({"text": "retouch"}));
    h.step();
    click(&mut h, "effectGroup:Retouch");
    h.step();
    let Some(Dialog::ManageEffects { selected, .. }) = &h.app.ui.dialog else { panic!() };
    assert_eq!(selected, &vec!["21 Skin".to_string()]);
    // delete asks first, inside the window
    click(&mut h, "button:effectDelete");
    h.step();
    assert_eq!(h.app.session.mask_effects.len(), 4);
    click(&mut h, "button:effectDeleteConfirm");
    h.step();
    assert_eq!(h.app.session.mask_effects.len(), 3);
    assert!(matches!(h.app.ui.dialog, Some(Dialog::ManageEffects { .. })), "stays open");
}

#[test]
fn imported_effects_toast_reports_groups_and_what_was_kept() {
    let v = json!({"effects": [
        {"name": "a", "group": "Detail", "unmapped": ["Grain"]},
        {"name": "b", "group": "Retouch", "unmapped": ["Point Color", "Grain"]},
        {"name": "c", "group": "", "unmapped": []},
    ]});
    let msg = crate::panels::mask_effects::import_toast(&v);
    assert!(msg.contains("3 mask effects") && msg.contains("2 groups") && msg.contains("Grain, Point Color"), "{msg}");
}
