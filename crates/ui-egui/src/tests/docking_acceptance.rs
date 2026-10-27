//! Dock changes exercise the same real catalog and command handlers as the application.
use crate::{LightcraftApp, Services, docking::Panel, headless::Headless, state::RightPanel};
use egui::{Event, Modifiers, PointerButton};
use serde_json::json;

fn app() -> Headless {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
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
fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(key, _)| key == id).unwrap().1
}

#[test]
fn the_gaps_between_dock_panes_show_chrome_not_the_dark_clear_colour() {
    let mut h = app();
    let image = h.snapshot(std::time::Duration::from_secs(5));
    let pixel = |p: egui::Pos2| image.pixels[p.y as usize * image.size[0] + p.x as usize];
    // each panel's own chrome just inside its edge stroke, and the dock's gap just outside it
    let right = rect(&h, "panel:right_panel");
    let left = rect(&h, "panel:left_panel");
    for (panel, inside, gap) in [
        ("Tools", egui::pos2(right.left() + 3.0, right.center().y), egui::pos2(right.left() - 2.0, right.center().y)),
        ("Library", egui::pos2(left.right() - 3.0, left.center().y), egui::pos2(left.right() + 2.0, left.center().y)),
    ] {
        assert_eq!(pixel(gap), pixel(inside), "the gap beside {panel} at {gap:?} is not chrome");
    }
}
fn release(h: &mut Headless, pos: egui::Pos2) {
    h.app.synthetic.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    h.app.synthetic.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
}

#[test]
fn window_menu_reopens_closed_panels_in_their_dock_columns() {
    use craft_ui::docking::Side;
    let mut h = app();
    let columns = |h: &Headless, side| {
        crate::docking::columns(h.app.ui.docking.as_ref().unwrap(), side).into_iter().map(|(_, groups)| groups).collect::<Vec<_>>()
    };
    let end = vec![vec![vec![Panel::Tools]], vec![vec![Panel::Presets]]];
    h.app.run("panel.presets", json!({})).unwrap();
    h.step();
    assert_eq!(columns(&h, Side::End), end, "Presets opens on Tools' photo side");
    dock(&mut h, json!({"operation":"close","panel":"tools"}));
    assert_eq!(h.app.ui.right, RightPanel::None);
    h.app.run("panel.edit", json!({})).unwrap();
    h.step();
    assert_eq!(columns(&h, Side::End), end, "Tools reopens at the window edge");
    let width = crate::docking::columns(h.app.ui.docking.as_ref().unwrap(), Side::Start).first().unwrap().0;
    dock(&mut h, json!({"operation":"close","panel":"library"}));
    assert!(!h.app.ui.left_panel);
    h.app.run("view.leftPanel", json!({})).unwrap();
    h.step();
    assert_eq!(crate::docking::columns(h.app.ui.docking.as_ref().unwrap(), Side::Start), [(width, vec![vec![Panel::Library]])]);
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
    h.app.ui.presets = true;
    h.step();
    dock(&mut h, json!({"operation":"move","panel":"presets","target":"library","zone":"center"}));
    dock(&mut h, json!({"operation":"activate","panel":"library"}));
    h.app.run("panel.presets", json!({})).unwrap();
    assert!(crate::docking::exposed_in(h.app.ui.docking.as_ref().unwrap(), Panel::Presets));
    dock(&mut h, json!({"operation":"activate","panel":"library"}));
    dock(&mut h, json!({"operation":"open","panel":"presets"}));
    assert!(crate::docking::exposed_in(h.app.ui.docking.as_ref().unwrap(), Panel::Presets));
}

#[test]
fn crop_canvas_double_click_reveals_edit_in_actual_custom_workspace() {
    let mut h = app();
    h.app.ui.view = crate::state::ViewMode::Detail;
    h.app.run("panel.crop", json!({})).unwrap();
    assert!(h.settle(std::time::Duration::from_secs(120)), "the demo photo settles before pointer input");
    h.app.ui.presets = true;
    h.step();
    dock(&mut h, json!({"operation":"move","panel":"tools","target":"presets","zone":"center"}));
    dock(&mut h, json!({"operation":"activate","panel":"presets"}));
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
    assert_eq!(workspace.layout.location(&Panel::Tools).unwrap().anchor, Some(Panel::Presets));
    assert_eq!(workspace.layout.location(&Panel::Library).unwrap(), library);
    assert_eq!(h.app.session.active(), Some(id));
    assert_eq!(h.app.session.undo.len(), undo);
    assert_eq!(serde_json::to_value(h.app.session.catalog.photo(id).unwrap().as_ref()).unwrap(), photo);
}

#[test]
fn floating_panel_blocks_photo_album_drop_then_uncovered_target_adds_exact_selection() {
    let mut h = app();
    let album = h.app.run("album.create", json!({"name":"A Dock Target", "addSelected":false})).unwrap()["id"].as_u64().unwrap();
    h.step();
    h.step();
    let row = rect(&h, &format!("source:album:{album}"));
    let ids: Vec<_> = h.app.session.visible_cloned().iter().take(2).map(|id| id.0).collect();
    dock(&mut h, json!({"operation":"float","panel":"tools","rect":[row.left()-10.0,row.top()-30.0,330,500]}));
    let undo = h.app.session.undo.len();
    h.app.ui.dragging_photos = Some(ids.clone());
    release(&mut h, row.center());
    assert!(h.app.session.catalog.album(lightcraft_catalog::AlbumId(album)).unwrap().photos.is_empty());
    assert_eq!(h.app.session.undo.len(), undo);
    dock(&mut h, json!({"operation":"moveFloating","panel":"tools","rect":[800,100,330,500]}));
    h.app.ui.dragging_photos = Some(ids.clone());
    release(&mut h, row.center());
    assert_eq!(
        h.app.session.catalog.album(lightcraft_catalog::AlbumId(album)).unwrap().photos,
        ids.into_iter().map(lightcraft_catalog::PhotoId).collect::<Vec<_>>()
    );
    assert_eq!(h.app.session.undo.len(), undo + 1);
}

#[test]
fn preset_hover_survives_float_movement_without_mutating_photo() {
    let mut h = app();
    h.app.run("panel.presets", json!({})).unwrap();
    dock(&mut h, json!({"operation":"float","panel":"presets","rect":[500,100,300,700]}));
    let undo = h.app.session.undo.len();
    let photos = serde_json::to_value(h.app.session.catalog.photos().map(|photo| photo.as_ref()).collect::<Vec<_>>()).unwrap();
    for x in [500, 650] {
        dock(&mut h, json!({"operation":"moveFloating","panel":"presets","rect":[x,100,300,700]}));
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
fn dock_tab_keeps_activation_keys_and_global_command_remains_reachable() {
    let mut h = app();
    h.app.run("app.setShortcut", json!({"id":"panel.crop","shortcut":"K"})).unwrap();
    dock(&mut h, json!({"operation":"move","panel":"tools","target":"library","zone":"center"}));
    h.app.run("panel.crop", json!({})).unwrap();
    h.step();
    // The test chooses the renderer's actual registered tab, rather than an arbitrary focus owner.
    let id = egui::Id::unique("lightcraft-docking").with(("tab", Panel::Tools));
    h.view.ctx.memory_mut(|memory| memory.request_focus(id));
    h.step();
    key(&mut h, egui::Key::Enter);
    assert_eq!(h.app.ui.right, RightPanel::Crop, "Enter activates the tab without tool.done");
    key(&mut h, egui::Key::K);
    assert_eq!(h.app.ui.right, RightPanel::None, "the global crop toggle is reachable from tab focus");
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
    assert_eq!(h.view.ctx.data(|data| data.get_temp::<String>(egui::Id::unique("keyword-list-filter"))).as_deref(), Some("dock search"));
}

#[test]
fn floating_presets_block_keyword_photo_drop_then_uncovered_target_changes_exact_metadata() {
    let mut h = app();
    h.app.run("panel.keywords", json!({})).unwrap();
    h.app.run("keyword.create", json!({"name":"A Dock Keyword"})).unwrap();
    h.step();
    h.step();
    let row = rect(&h, "keywordRow:A Dock Keyword");
    h.app.ui.presets = true;
    h.step();
    let ids: Vec<_> = h.app.session.visible_cloned().iter().take(2).map(|id| id.0).collect();
    dock(&mut h, json!({"operation":"float","panel":"presets","rect":[row.left()-10.0,row.top()-30.0,300,500]}));
    let undo = h.app.session.undo.len();
    let before: Vec<_> = ids.iter().map(|id| h.app.session.catalog.photo(lightcraft_catalog::PhotoId(*id)).unwrap().meta.keywords.clone()).collect();
    h.app.ui.dragging_photos = Some(ids.clone());
    release(&mut h, row.center());
    for (id, expected) in ids.iter().zip(&before) {
        assert_eq!(&h.app.session.catalog.photo(lightcraft_catalog::PhotoId(*id)).unwrap().meta.keywords, expected);
    }
    assert_eq!(h.app.session.undo.len(), undo);
    dock(&mut h, json!({"operation":"moveFloating","panel":"presets","rect":[400,100,300,500]}));
    h.app.ui.dragging_photos = Some(ids.clone());
    release(&mut h, row.center());
    for (id, mut expected) in ids.iter().zip(before) {
        expected.push("A Dock Keyword".into());
        let mut actual = h.app.session.catalog.photo(lightcraft_catalog::PhotoId(*id)).unwrap().meta.keywords.clone();
        expected.sort();
        actual.sort();
        assert_eq!(actual, expected);
    }
    assert_eq!(h.app.session.undo.len(), undo + 1);
}

#[test]
fn covered_folder_rejects_album_move_and_uncovered_folder_nests_exact_album() {
    let mut h = app();
    let folder = h.app.run("album.create", json!({"name":"A Dock Folder","folder":true})).unwrap()["id"].as_u64().unwrap();
    let child = h.app.run("album.create", json!({"name":"B Dock Album","addSelected":false})).unwrap()["id"].as_u64().unwrap();
    h.step();
    h.step();
    let row = rect(&h, &format!("source:folder:{folder}"));
    dock(&mut h, json!({"operation":"float","panel":"tools","rect":[row.left()-10.0,row.top()-30.0,330,500]}));
    let undo = h.app.session.undo.len();
    h.app.ui.dragging_album = Some(child);
    release(&mut h, row.center());
    assert_eq!(h.app.session.catalog.album(lightcraft_catalog::AlbumId(child)).unwrap().parent, None);
    assert_eq!(h.app.session.undo.len(), undo);
    dock(&mut h, json!({"operation":"moveFloating","panel":"tools","rect":[800,100,330,500]}));
    h.app.ui.dragging_album = Some(child);
    release(&mut h, row.center());
    assert_eq!(h.app.session.catalog.album(lightcraft_catalog::AlbumId(child)).unwrap().parent, Some(lightcraft_catalog::AlbumId(folder)));
    assert_eq!(h.app.session.undo.len(), undo + 1);
}

#[test]
fn covered_keyword_rejects_hierarchy_move_and_uncovered_target_moves_exact_keyword() {
    let mut h = app();
    h.app.run("panel.keywords", json!({})).unwrap();
    h.app.run("keyword.create", json!({"name":"A Dock Parent"})).unwrap();
    h.app.run("keyword.create", json!({"name":"B Dock Child"})).unwrap();
    h.app.ui.presets = true;
    h.step();
    h.step();
    let row = rect(&h, "keywordRow:A Dock Parent");
    dock(&mut h, json!({"operation":"float","panel":"presets","rect":[row.left()-10.0,row.top()-30.0,300,500]}));
    let undo = h.app.session.undo.len();
    h.app.ui.dragging_keyword = Some("B Dock Child".into());
    release(&mut h, row.center());
    assert!(h.app.session.catalog.has_keyword("B Dock Child"));
    assert!(!h.app.session.catalog.has_keyword("A Dock Parent|B Dock Child"));
    assert_eq!(h.app.session.undo.len(), undo);
    dock(&mut h, json!({"operation":"moveFloating","panel":"presets","rect":[400,100,300,500]}));
    h.app.ui.dragging_keyword = Some("B Dock Child".into());
    release(&mut h, row.center());
    assert!(!h.app.session.catalog.has_keyword("B Dock Child"));
    assert!(h.app.session.catalog.has_keyword("A Dock Parent|B Dock Child"));
    assert_eq!(h.app.session.undo.len(), undo + 1);
}

#[test]
fn reset_during_held_divider_drag_keeps_replacement_layout_on_later_movement() {
    let mut h = app();
    let divider = egui::Id::unique("lightcraft-docking").with(("column-resize", Some(Panel::Library)));
    let from = h.view.ctx.read_response(divider).expect("the Library column's divider is rendered").rect.center();
    let before = h.app.ui.docking.as_ref().unwrap().layout.clone();
    h.app.synthetic.push(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    h.app.synthetic.push(Event::PointerMoved(from + egui::vec2(40.0, 0.0)));
    h.step();
    assert_ne!(h.app.ui.docking.as_ref().unwrap().layout, before, "fixture performs a real divider resize");
    assert_eq!(h.view.ctx.dragged_id(), Some(divider));
    let resized = h.app.ui.docking.as_ref().unwrap().layout.clone();
    h.app.synthetic.push(Event::PointerMoved(from + egui::vec2(55.0, 0.0)));
    h.step();
    assert_ne!(h.app.ui.docking.as_ref().unwrap().layout, resized, "renderer-generated actions preserve continuous dragging");
    let undo = h.app.session.undo.len();
    h.app.run("ui.dock.reset", json!({})).unwrap();
    let reset = serde_json::to_value(&h.app.ui.docking).unwrap();
    h.app.synthetic.push(Event::PointerMoved(from + egui::vec2(90.0, 0.0)));
    h.step();
    assert_eq!(serde_json::to_value(&h.app.ui.docking).unwrap(), reset, "a held old divider cannot resize the replacement tree");
    h.app.synthetic.push(Event::PointerButton {
        pos: from + egui::vec2(90.0, 0.0),
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.step();
    assert_eq!(serde_json::to_value(&h.app.ui.docking).unwrap(), reset);
    assert_eq!(h.app.session.undo.len(), undo);
}

fn held_divider_replacement_route(route: &str) {
    let mut h = app();
    let divider = egui::Id::unique("lightcraft-docking").with(("column-resize", Some(Panel::Library)));
    let from = h.view.ctx.read_response(divider).unwrap().rect.center();
    let photos = serde_json::to_value(h.app.session.catalog.photos().map(|photo| photo.as_ref()).collect::<Vec<_>>()).unwrap();
    h.app.synthetic.push(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    h.app.synthetic.push(Event::PointerMoved(from + egui::vec2(40.0, 0.0)));
    h.step();
    assert_eq!(h.view.ctx.dragged_id(), Some(divider));
    let undo = h.app.session.undo.len();
    match route {
        "ui.set" => {
            let response = h.request("ui.set", json!({"rightWidth":410.0}), std::time::Duration::from_secs(20));
            assert_eq!(response["ok"], true, "{response}");
            assert_eq!(h.app.ui.right_width, 410.0);
        }
        "panel.close" => {
            h.app.run("panel.close", json!({})).unwrap();
            h.step();
        }
        "public-action" => {
            let action = craft_ui::docking::Action::Float { panel: Panel::Tools, rect: [800.0, 100.0, 330.0, 500.0] };
            h.app.run("ui.dock", json!({"action":action})).unwrap();
            h.step();
        }
        _ => panic!("unknown test route"),
    }
    let replacement = serde_json::to_value(&h.app.ui.docking).unwrap();
    h.app.synthetic.push(Event::PointerMoved(from + egui::vec2(90.0, 0.0)));
    h.step();
    assert_eq!(serde_json::to_value(&h.app.ui.docking).unwrap(), replacement, "{route} cancels the previous divider gesture");
    assert_eq!(h.app.session.undo.len(), undo);
    assert_eq!(serde_json::to_value(h.app.session.catalog.photos().map(|photo| photo.as_ref()).collect::<Vec<_>>()).unwrap(), photos);
}
#[test]
fn actual_ui_set_width_replacement_cancels_held_divider() {
    held_divider_replacement_route("ui.set");
}
#[test]
fn panel_close_sync_replacement_cancels_held_divider() {
    held_divider_replacement_route("panel.close");
}
#[test]
fn externally_serialized_action_replacement_cancels_held_divider() {
    held_divider_replacement_route("public-action");
}
