//! Dock changes exercise the same real catalog and command handlers as the application.
use crate::{LccApp, Services, docking::Panel, headless::Headless, state::RightPanel};
use egui::{Event, Modifiers, PointerButton};
use serde_json::json;

fn app() -> Headless {
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.left_panel = true;
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    for _ in 0..5 {
        h.step();
    }
    h
}
fn dock(h: &mut Headless, p: serde_json::Value) {
    h.app.run("ui.dock", p).unwrap();
    for _ in 0..4 {
        h.step();
    }
}

fn release(h: &mut Headless, pos: egui::Pos2) {
    h.app.synthetic.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    h.app.synthetic.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
}

#[test]
fn inactive_same_mode_commands_reveal_without_closing_or_editing() {
    let mut h = app();
    dock(&mut h, json!({"operation":"move","panel":"tools","target":"library","zone":"center"}));
    for command in ["panel.crop", "panel.crop", "tool.brush", "section.light", "panel.profiles"] {
        dock(&mut h, json!({"operation":"activate","panel":"library"}));
        let undo = h.app.session.undo.len();
        let photos = serde_json::to_value(h.app.session.catalog.photos().map(|photo| photo.as_ref()).collect::<Vec<_>>()).unwrap();
        h.app.run(command, json!({})).unwrap();
        assert!(crate::docking::exposed_in(h.app.ui.docking.as_ref().unwrap(), Panel::Tools), "{command}");
        assert_ne!(h.app.ui.right, RightPanel::None);
        assert_eq!(h.app.session.undo.len(), undo);
        assert_eq!(serde_json::to_value(h.app.session.catalog.photos().map(|photo| photo.as_ref()).collect::<Vec<_>>()).unwrap(), photos);
        h.step();
    }
    // the presets are a panel of the Library column: their command exposes that column, and
    // never opens a Presets dock panel
    // they start unfolded and the command toggles them: fold first so the command shows them
    crate::panels::presets::set_shown(&mut h.app, false);
    h.app.run("panel.presets", json!({})).unwrap();
    let workspace = h.app.ui.docking.as_ref().unwrap();
    assert!(crate::docking::exposed_in(workspace, Panel::Library));
    assert!(!workspace.layout.contains(&Panel::Presets));
}

#[test]
fn crop_canvas_double_click_reveals_edit_in_actual_custom_workspace() {
    let mut h = app();
    h.app.ui.view = crate::state::ViewMode::Detail;
    h.app.run("panel.crop", json!({})).unwrap();
    assert!(h.settle(std::time::Duration::from_secs(120)), "the demo photo settles before pointer input");
    dock(&mut h, json!({"operation":"move","panel":"tools","target":"library","zone":"center"}));
    dock(&mut h, json!({"operation":"activate","panel":"library"}));
    assert_eq!(h.app.ui.right, RightPanel::Crop);
    assert!(!crate::docking::exposed_in(h.app.ui.docking.as_ref().unwrap(), Panel::Tools));
    let library = h.app.ui.docking.as_ref().unwrap().layout.location(&Panel::Library).unwrap();
    let id = h.app.session.active().unwrap();
    let photo = serde_json::to_value(h.app.session.catalog.photo(id).unwrap().as_ref()).unwrap();
    let undo = h.app.session.undo.len();
    let center = h.app.image_rect.expect("crop photo is rendered").center();
    release(&mut h, center);
    release(&mut h, center);
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(h.app.ui.right, RightPanel::Edit, "the real Canvas double-click completes Crop");
    let workspace = h.app.ui.docking.as_ref().unwrap();
    assert!(crate::docking::exposed_in(workspace, Panel::Tools), "tool.done reveals Tools after the Canvas callback");
    assert_eq!(workspace.layout.location(&Panel::Tools).unwrap().anchor, Some(Panel::Library));
    assert_eq!(workspace.layout.location(&Panel::Library).unwrap(), library);
    assert_eq!(h.app.session.active(), Some(id));
    assert_eq!(h.app.session.undo.len(), undo);
    assert_eq!(serde_json::to_value(h.app.session.catalog.photo(id).unwrap().as_ref()).unwrap(), photo);
}

#[test]
fn preset_hover_survives_dock_changes_without_mutating_photo() {
    let mut h = app();
    // (preset groups start folded)
    let groups: Vec<String> = h.app.session.presets.iter().map(|p| format!("preset:{}", p.group)).collect();
    h.app.ui.group_open.extend(groups.into_iter().map(|g| (g, true)));
    // they start unfolded and the command toggles them: fold first so the command shows them
    crate::panels::presets::set_shown(&mut h.app, false);
    h.app.run("panel.presets", json!({})).unwrap();
    dock(&mut h, json!({"operation":"float","panel":"tools","rect":[500,100,300,700]}));
    let undo = h.app.session.undo.len();
    let photos = serde_json::to_value(h.app.session.catalog.photos().map(|photo| photo.as_ref()).collect::<Vec<_>>()).unwrap();
    for x in [500, 650] {
        dock(&mut h, json!({"operation":"moveFloating","panel":"tools","rect":[x,100,300,700]}));
        let row = h.app.widgets.iter().find(|(id, _)| id.starts_with("preset:")).unwrap().1;
        h.app.synthetic.push(Event::PointerMoved(row.center()));
        h.step();
        h.step();
        assert!(h.app.hover_preview.is_some());
        assert_eq!(h.app.session.undo.len(), undo);
        assert_eq!(serde_json::to_value(h.app.session.catalog.photos().map(|photo| photo.as_ref()).collect::<Vec<_>>()).unwrap(), photos);
    }
    h.app.synthetic.push(Event::PointerMoved(egui::pos2(5.0, 5.0)));
    h.step();
    h.step();
    assert!(h.app.hover_preview.is_none());
}

fn key(h: &mut Headless, key: egui::Key) {
    h.app.synthetic.push(Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: Modifiers::NONE });
    h.step();
    h.app.synthetic.push(Event::Key { key, physical_key: Some(key), pressed: false, repeat: false, modifiers: Modifiers::NONE });
    h.step();
}

#[test]
fn content_text_edit_retains_command_keys_and_typed_text() {
    let mut h = app();
    h.app.run("app.setShortcut", json!({"id":"panel.crop","shortcut":"K"})).unwrap();
    h.app.run("panel.keywords", json!({})).unwrap();
    h.step();
    h.step();
    let result = h.request("ui.clickWidget", json!({"id":"field:keywordFilter"}), std::time::Duration::from_secs(20));
    assert_eq!(result["ok"], true, "{result}");
    key(&mut h, egui::Key::K);
    h.app.synthetic.push(Event::Text("dock search".into()));
    h.step();
    assert_eq!(h.app.ui.right, RightPanel::Keywords);
    assert_eq!(h.view.ctx.data(|data| data.get_temp::<String>(egui::Id::new("keyword-list-filter"))).as_deref(), Some("dock search"));
}
