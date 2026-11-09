//! Preset groups in the Presets panel: made from the ⋯ menu with no preset, renamed, deleted and
//! starred from a group's header.

use std::time::Duration;

use lightcraft_develop::Preset;
use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// A headless app on the demo library with the Presets panel open, holding three user presets in
/// group "AAA Mine" (first in the panel). Returns their ids.
fn app() -> (Headless, Vec<String>) {
    let mut session = lightcraft_engine::Session::with_demo();
    let presets = ["one", "two", "three"].map(|name| Preset {
        id: String::new(),
        name: name.into(),
        group: "AAA Mine".into(),
        settings: json!({"light": {"exposure": 0.5}}),
        favorite: false,
        builtin: false,
    });
    let ids = session.add_presets(presets.to_vec());
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.app.run("panel.presets", json!({})).unwrap();
    frames(&mut h);
    (h, ids)
}

fn frames(h: &mut Headless) {
    for _ in 0..5 {
        h.step();
    }
}

fn req(h: &mut Headless, method: &str, params: serde_json::Value) {
    let r = h.request(method, params, T);
    assert_eq!(r["ok"], true, "{method}: {r}");
    frames(h);
}

fn click(h: &mut Headless, id: &str, modifiers: serde_json::Value) {
    let mut params = json!({"id": id});
    if let (Some(p), Some(m)) = (params.as_object_mut(), modifiers.as_object()) {
        p.extend(m.clone());
    }
    req(h, "ui.clickWidget", params);
}

fn laid_out(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// A group is made from the ⋯ menu with no preset, and an empty one is deleted without asking.
#[test]
fn a_group_is_made_from_the_menu_and_deleted_from_its_header() {
    let (mut h, _) = app();
    let before = h.app.session.presets.len();
    click(&mut h, "icon:presetMore", json!({}));
    click(&mut h, "presetMenu:newGroup", json!({}));
    let Some(Dialog::TextPrompt { command, key, .. }) = h.app.ui.dialog.as_ref() else {
        panic!("the prompt opens");
    };
    assert_eq!((command.as_str(), key.as_str()), ("preset.createGroup", "name"));
    h.app.ui.dialog = None;

    h.app.run("preset.createGroup", json!({"name": "AAA Empty"})).unwrap();
    frames(&mut h);
    assert!(laid_out(&h, "presetGroup:AAA Empty"));
    assert_eq!(h.app.session.presets.len(), before, "no preset was made or moved");

    click(&mut h, "presetGroup:AAA Empty", json!({"button": "right"}));
    click(&mut h, "presetGroupMenu:delete", json!({}));
    assert!(h.app.ui.dialog.is_none(), "an empty group has nothing to lose");
    assert!(!laid_out(&h, "presetGroup:AAA Empty"));
}

/// An answer to a question asked in another library, or about fewer presets than the group holds
/// now, deletes nothing.
#[test]
fn a_stale_group_deletion_deletes_nothing() {
    let (mut h, ids) = app();
    let library = h.app.session.library_generation();
    for stale in [
        Dialog::DeletePresetGroup { name: "AAA Mine".into(), count: 3, library: library + 1 },
        Dialog::DeletePresetGroup { name: "AAA Mine".into(), count: 2, library },
    ] {
        assert!(crate::dialogs::confirm_dialog(&mut h.app, &stale).is_err());
        assert!(h.app.session.presets.iter().any(|p| p.id == ids[0]), "the presets stay");
    }
    let asked = Dialog::DeletePresetGroup { name: "AAA Mine".into(), count: 3, library };
    crate::dialogs::confirm_dialog(&mut h.app, &asked).unwrap();
    assert!(!h.app.session.presets.iter().any(|p| p.id == ids[0]));
}

/// The question reads right for a group of one preset.
#[test]
fn deleting_a_group_of_one_preset_says_so() {
    let (mut h, _) = app();
    let library = h.app.session.library_generation();
    h.app.ui.dialog = Some(Dialog::DeletePresetGroup { name: "AAA Mine".into(), count: 1, library });
    frames(&mut h);
    assert!(h.painted_text().iter().any(|t| t.contains("its only preset")), "{:?}", h.painted_text());
    assert!(!h.painted_text().iter().any(|t| t.contains("1 presets")));
}

/// A group holding presets asks before it goes, and a built-in one offers no Delete Group.
#[test]
fn deleting_a_group_with_presets_asks_and_built_in_groups_offer_no_delete() {
    let (mut h, _) = app();
    click(&mut h, "presetGroup:AAA Mine", json!({"button": "right"}));
    click(&mut h, "presetGroupMenu:delete", json!({}));
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::DeletePresetGroup { name, count: 3, .. }) if name == "AAA Mine"));
    h.app.ui.dialog = None;
    frames(&mut h);

    let builtin = h.app.session.presets.iter().find(|p| p.builtin).map(|p| p.group.clone()).unwrap();
    click(&mut h, &format!("presetGroup:{builtin}"), json!({"button": "right"}));
    assert!(laid_out(&h, "presetGroupMenu:favorite"));
    assert!(!laid_out(&h, "presetGroupMenu:delete"));
}

/// A group of the user's is renamed from its header, and a stock group offers no Rename.
#[test]
fn a_group_is_renamed_from_its_header() {
    let (mut h, ids) = app();
    click(&mut h, "presetGroup:AAA Mine", json!({"button": "right"}));
    click(&mut h, "presetGroupMenu:rename", json!({}));
    let Some(Dialog::TextPrompt { command, key, value, params, .. }) = h.app.ui.dialog.as_ref() else {
        panic!("the prompt opens");
    };
    assert_eq!((command.as_str(), key.as_str(), value.as_str()), ("preset.renameGroup", "to", "AAA Mine"));
    assert_eq!(params["name"], "AAA Mine");
    h.app.ui.dialog = None;
    h.app.run("preset.renameGroup", json!({"name": "AAA Mine", "to": "AAA Ours"})).unwrap();
    frames(&mut h);
    assert!(laid_out(&h, "presetGroup:AAA Ours"));
    assert!(!laid_out(&h, "presetGroup:AAA Mine"));
    assert!(h.app.session.presets.iter().filter(|p| ids.contains(&p.id)).all(|p| p.group == "AAA Ours"));

    let builtin = h.app.session.presets.iter().find(|p| p.builtin).map(|p| p.group.clone()).unwrap();
    click(&mut h, &format!("presetGroup:{builtin}"), json!({"button": "right"}));
    assert!(!laid_out(&h, "presetGroupMenu:rename"));
}

/// A group starred from its header's menu moves above the others, a built-in one too, with its star
/// away it goes back in its place, and the rows it holds move with it.
#[test]
fn a_starred_group_rises_to_the_top_and_comes_back() {
    let (mut h, ids) = app();
    let top = |h: &Headless, id: &str| h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| r.top());
    let builtin = h.app.session.presets.iter().find(|p| p.builtin).map(|p| p.group.clone()).unwrap();
    let header = format!("presetGroup:{builtin}");
    assert!(top(&h, "presetGroup:AAA Mine").unwrap() < top(&h, &header).unwrap(), "by name at first");

    click(&mut h, &header, json!({"button": "right"}));
    click(&mut h, "presetGroupMenu:favorite", json!({}));
    assert!(top(&h, &header).unwrap() < top(&h, "presetGroup:AAA Mine").unwrap(), "a starred group comes first");
    assert!(h.app.session.presets.favorite_groups().contains(&builtin));
    assert!(top(&h, &format!("preset:{}", ids[0])).unwrap() > top(&h, "presetGroup:AAA Mine").unwrap(), "its rows stay under its header");

    click(&mut h, &header, json!({"button": "right"}));
    click(&mut h, "presetGroupMenu:favorite", json!({}));
    assert!(top(&h, "presetGroup:AAA Mine").unwrap() < top(&h, &header).unwrap(), "and goes back");
}
