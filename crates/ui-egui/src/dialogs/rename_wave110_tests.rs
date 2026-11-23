use crate::LightcraftApp;
use crate::headless::Headless;
use crate::state::{Dialog, ViewMode};
use serde_json::{Value, json};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(10);

fn app() -> Headless {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
    app.ui.view = ViewMode::PhotoGrid;
    Headless::new(app, [1400.0, 1000.0], 1.0)
}

fn ask(h: &mut Headless, method: &str, params: Value) -> Value {
    let response = h.request(method, params, WAIT);
    assert_eq!(response["ok"], true, "{method}: {response}");
    h.settle(WAIT);
    response
}

#[test]
fn rename_wave110_batch_preset_tags_and_confirm() {
    let mut h = app();
    let ids: Vec<_> = h.app.session.visible_cloned().iter().take(2).map(|id| id.0).collect();
    ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": ids}}));
    h.app.ui.dialog = Some(Dialog::Rename { template: "{name}".into(), start: 5 });
    h.settle(WAIT);
    ask(&mut h, "ui.clickWidget", json!({"id": "button:renamePreset-3"}));
    assert_eq!(h.app.ui.dialog, Some(Dialog::Rename { template: "Photo-{seq:3}".into(), start: 5 }));
    ask(&mut h, "ui.clickWidget", json!({"id": "button:renameTemplateTags"}));
    let widgets = ask(&mut h, "ui.widgets", json!({})).to_string();
    assert!(widgets.contains("button:renameTemplateTag-0"), "{widgets}");
    // Native shared tag-help still replaces the actual selection at the cursor.
    let id = egui::Id::unique("rename-template");
    let mut state = egui::text_edit::TextEditState::default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(0))));
    state.store(&h.view.ctx, id);
    ask(&mut h, "ui.clickWidget", json!({"id": "button:renameTemplateTag-0"}));
    assert_eq!(h.app.ui.dialog, Some(Dialog::Rename { template: "{name}Photo-{seq:3}".into(), start: 5 }));
    ask(&mut h, "ui.clickWidget", json!({"id": "button:renamePreset-3"}));
    ask(&mut h, "ui.dialog.confirm", json!({}));
    assert_eq!(h.app.ui.dialog, None);
    for (id, expected) in ids.iter().zip(["Photo-005.", "Photo-006."]) {
        let name = &h.app.session.catalog.photo(lightcraft_catalog::PhotoId(*id)).unwrap().file_name;
        assert!(name.starts_with(expected), "{name}");
    }
}

#[test]
fn rename_wave110_keyword_typing_replaces_name_and_return_applies() {
    let mut h = app();
    let id = h.app.session.visible_cloned()[0].0;
    ask(&mut h, "engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [id], "addKeywords": ["holiday"]}}));
    h.app.ui.dialog = Some(Dialog::RenameKeyword { from: "holiday".into(), to: "holiday".into() });
    h.settle(WAIT);
    ask(&mut h, "ui.text", json!({"text": "travel"}));
    assert_eq!(h.app.ui.dialog, Some(Dialog::RenameKeyword { from: "holiday".into(), to: "travel".into() }));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.ui.dialog, None);
    let keywords = &h.app.session.catalog.photo(lightcraft_catalog::PhotoId(id)).unwrap().meta.keywords;
    assert!(keywords.contains(&"travel".to_string()) && !keywords.contains(&"holiday".to_string()), "{keywords:?}");
}

#[test]
fn rename_wave110_person_merge_requires_checkbox_and_undo_restores() {
    let mut h = app();
    let ids: Vec<_> = h.app.session.catalog.photos().take(2).map(|p| p.id).collect();
    for (id, name) in ids.iter().zip(["Jane", "John"]) {
        let mut meta = h.app.session.catalog.photo(*id).unwrap().meta.clone();
        meta.regions = vec![lightcraft_meta::Region {
            rect: lightcraft_geom::Rect { x0: 0.2, y0: 0.2, x1: 0.5, y1: 0.6 },
            kind: lightcraft_meta::RegionKind::Face,
            name: Some(name.into()),
            description: None,
        }];
        h.app.session.commit("setup", lightcraft_catalog::Op::SetMeta { id: *id, meta: Box::new(meta) }).unwrap();
    }
    h.app.ui.dialog = Some(Dialog::RenamePerson { from: "Jane".into(), to: "Jane".into(), merge: false });
    h.settle(WAIT);
    ask(&mut h, "ui.text", json!({"text": "John"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.ui.dialog, Some(Dialog::RenamePerson { from: "Jane".into(), to: "John".into(), merge: false }));
    assert_eq!(h.app.session.catalog.photo(ids[0]).unwrap().meta.regions[0].name.as_deref(), Some("Jane"));
    ask(&mut h, "ui.clickWidget", json!({"id": "check:personMerge"}));
    assert_eq!(h.app.ui.dialog, Some(Dialog::RenamePerson { from: "Jane".into(), to: "John".into(), merge: true }));
    ask(&mut h, "ui.dialog.confirm", json!({}));
    assert_eq!(h.app.session.catalog.photo(ids[0]).unwrap().meta.regions[0].name.as_deref(), Some("John"));
    ask(&mut h, "engine.execute", json!({"command": "edit.undo"}));
    assert_eq!(h.app.session.catalog.photo(ids[0]).unwrap().meta.regions[0].name.as_deref(), Some("Jane"));
}

#[test]
fn rename_wave110_start_number_keyboard_entry() {
    let mut h = app();
    h.app.ui.dialog = Some(Dialog::Rename { template: "Trip-{seq:3}".into(), start: 1 });
    h.settle(WAIT);
    ask(&mut h, "ui.clickWidget", json!({"id": "field:renameStart"}));
    ask(&mut h, "ui.key", json!({"key": "A", "cmd": true}));
    ask(&mut h, "ui.text", json!({"text": "42"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(h.app.ui.dialog, Some(Dialog::Rename { template: "Trip-{seq:3}".into(), start: 42 }));
}

fn escape_reverts_then_cancels(dialog: Dialog) {
    let mut h = app();
    h.app.ui.dialog = Some(dialog.clone());
    h.settle(WAIT);
    let revision = h.app.session.catalog.revision;
    ask(&mut h, "ui.text", json!({"text": "Changed name"}));
    assert_ne!(h.app.ui.dialog, Some(dialog.clone()), "typing edits the dialog draft");
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    assert_eq!(h.app.ui.dialog, Some(dialog), "first Escape reverts the open field edit without cancelling the dialog");
    assert_eq!(h.app.session.catalog.revision, revision, "reverting a name draft does not rename anything");
    ask(&mut h, "ui.key", json!({"key": "Escape"}));
    assert_eq!(h.app.ui.dialog, None, "second Escape cancels the clean dialog");
    assert_eq!(h.app.session.catalog.revision, revision, "cancelling does not rename anything");
}

#[test]
fn rename_wave110_keyword_escape_reverts_then_cancels() {
    escape_reverts_then_cancels(Dialog::RenameKeyword { from: "holiday".into(), to: "holiday".into() });
}

#[test]
fn rename_wave110_person_escape_reverts_then_cancels() {
    escape_reverts_then_cancels(Dialog::RenamePerson { from: "Jane".into(), to: "Jane".into(), merge: false });
}
