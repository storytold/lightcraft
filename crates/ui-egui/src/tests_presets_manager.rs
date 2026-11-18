//! LR-PRE-MANAGE / LR-PRE-ONIMPORT / LR-PRESET-PICKER: the Presets Manager marks presets At
//! Import and hides them; hidden presets leave the Presets panel, At Import ones get their group.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(10);

fn widget(h: &mut Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

#[test]
fn the_manager_marks_at_import_hides_renames_groups_and_restores() {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(Duration::from_secs(60));
    let p = h.app.session.presets.first().unwrap().clone();
    assert_eq!(h.request("engine.execute", json!({"command": "dialog.presetsManager", "params": {"query": p.name}}), T)["ok"], true);
    h.step();
    h.step();
    let at_import = format!("icon:presetsManagerAtImport:{}", p.id);
    assert!(widget(&mut h, &at_import), "a search opens the group it finds");
    assert_eq!(h.request("ui.clickWidget", json!({"id": at_import}), T)["ok"], true);
    h.step();
    h.step();
    assert!(h.app.session.preset_at_import.contains(&p.id));
    let show = format!("icon:presetsManagerHide:{}", p.id);
    assert_eq!(h.request("ui.clickWidget", json!({"id": show}), T)["ok"], true);
    h.step();
    h.step();
    assert!(h.app.session.preset_hidden.contains(&p.id));
    // the Presets panel leaves a hidden preset out, the At Import group too
    h.app.ui.dialog = None;
    h.app.session.execute("preset.hide", &json!({"id": p.id, "hidden": false})).unwrap();
    h.app.run("panel.presets", json!({})).unwrap();
    h.step();
    h.step();
    assert!(widget(&mut h, "presetAtImport"), "At Import is a group of the Presets panel");
    h.app.session.execute("preset.hide", &json!({"id": p.id, "hidden": true})).unwrap();
    h.step();
    h.step();
    assert!(!widget(&mut h, "presetAtImport"), "a hidden preset leaves the panel");

    // renaming a built-in preset in place, hiding a whole group, a new group, a preset moved onto
    // it, and Restore Built-ins
    h.app.session.execute("preset.hide", &json!({"id": p.id, "hidden": false})).unwrap();
    let p = h.app.session.presets.iter().find(|p| p.builtin).unwrap().clone();
    assert_eq!(h.request("engine.execute", json!({"command": "dialog.presetsManager", "params": {"query": p.name}}), T)["ok"], true);
    h.step();
    h.step();
    click(&mut h, &format!("icon:presetsManagerRename:{}", p.id));
    let field = format!("field:presetsManagerName:{}", p.id);
    assert!(widget(&mut h, &field), "renamed in place");
    h.request("ui.text", json!({"text": " Renamed"}), T);
    h.request("ui.key", json!({"key": "Enter"}), T);
    h.step();
    h.step();
    let now = h.app.session.presets.iter().find(|q| q.id == p.id).unwrap().clone();
    assert!(now.name.ends_with("Renamed") && now.builtin, "a built-in renamed: {}", now.name);
    // hide the group whole (the search for the old name finds nothing now)
    if let Some(crate::state::Dialog::PresetsManager { query, .. }) = h.app.ui.dialog.as_mut() {
        query.clear();
    }
    h.step();
    h.step();
    click(&mut h, &format!("icon:presetsManagerGroupHide:{}", now.group));
    assert!(h.app.session.preset_hidden.contains(&p.id));
    // a new group, and the preset dragged onto it
    h.app.session.execute("preset.createGroup", &json!({"name": "Dropped Here"})).unwrap();
    if let Some(crate::state::Dialog::PresetsManager { query, .. }) = h.app.ui.dialog.as_mut() {
        query.clear();
    }
    h.step();
    h.step();
    assert!(widget(&mut h, "presetsManagerGroup:Dropped Here"), "an empty group of the user's is a place to drop");
    h.app.session.execute("preset.move", &json!({"id": p.id, "group": "Dropped Here"})).unwrap();
    click(&mut h, "button:presetsManagerRestore");
    click(&mut h, "button:presetsManagerRestoreConfirm");
    let back = h.app.session.presets.iter().find(|q| q.id == p.id).unwrap().clone();
    assert_eq!((back.name, back.group), (p.name.clone(), p.group.clone()), "Restore Built-ins");
    assert!(!h.app.session.preset_hidden.contains(&p.id));
}

fn has_prefix(h: &Headless, prefix: &str) -> usize {
    h.app.widgets.iter().filter(|(w, _)| w.starts_with(prefix)).count()
}

fn click(h: &mut Headless, id: &str) {
    assert_eq!(h.request("ui.clickWidget", json!({"id": id}), T)["ok"], true, "{id}");
    h.step();
    h.step();
}

/// LR-PRESET-PICKER: the picker's groups fold (the user's own first), a star toggles a favourite
/// in place, and resting on a preset shows it beside the list without waiting for the render.
#[test]
fn the_picker_folds_stars_and_previews() {
    let mut h = crate::tests_classic::demo();
    h.app.session.add_presets(vec![lcc_develop::Preset {
        id: "user.mine".into(),
        name: "Mine".into(),
        group: "My Looks".into(),
        settings: json!({"light": {"exposure": 1.0}}),
        favorite: false,
        builtin: false,
    }]);
    let id = h.app.session.visible_cloned()[0];
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0]}}), T);
    h.step();
    h.step();
    click(&mut h, "button:qd-preset");
    let groups: Vec<String> = h.app.widgets.iter().filter_map(|(w, _)| w.strip_prefix("presetPickerGroup:qd-preset:").map(str::to_string)).collect();
    assert!(!groups.is_empty(), "the picker lists groups");
    assert_eq!(groups.first().map(String::as_str), Some("preset:My Looks"), "the user's own group first: {groups:?}");
    assert_eq!(has_prefix(&h, "presetPick:qd-preset:"), 0, "groups start folded");
    click(&mut h, "presetPickerGroup:qd-preset:preset:My Looks");
    assert!(widget(&mut h, "presetPick:qd-preset:user.mine"), "an opened group shows its presets");
    click(&mut h, "presetPickStar:qd-preset:user.mine");
    assert!(h.app.session.presets.iter().any(|p| p.id == "user.mine" && p.favorite), "the star toggles in place");
    // (the picker stays open: Favorites now lists it too)
    assert!(widget(&mut h, "presetPick:qd-preset:user.mine"), "the picker stays open");
    assert_eq!(h.request("ui.hoverWidget", json!({"id": "presetPick:qd-preset:user.mine"}), T)["ok"], true);
    h.step();
    assert!(widget(&mut h, "presetPickerPreview"), "resting on a preset shows it");
}

/// LR-PRE-CREATE: Update Preset's toast carries an Undo button that puts the old settings back.
#[test]
fn update_preset_toast_undoes() {
    let mut h = crate::tests_classic::demo();
    let id = h.app.session.visible_cloned()[0];
    h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0]}}), T);
    h.app.session.execute("preset.create", &json!({"name": "Mine", "group": "User Presets"})).unwrap();
    let pid = h.app.session.presets.iter().find(|p| p.name == "Mine").unwrap().id.clone();
    let before = h.app.session.presets.iter().find(|p| p.id == pid).unwrap().settings.clone();
    h.app.session.execute("develop.set", &json!({"values": {"light.exposure": 2.0}})).unwrap();
    assert_eq!(h.request("engine.execute", json!({"command": "dialog.updatePreset", "params": {"id": pid}}), T)["ok"], true);
    h.step();
    h.step();
    click(&mut h, "button:dialogOk");
    assert!(h.app.ui.toast_action.is_some(), "a toast with Undo");
    assert_ne!(h.app.session.presets.iter().find(|p| p.id == pid).unwrap().settings, before);
    click(&mut h, "button:toastAction");
    assert_eq!(h.app.session.presets.iter().find(|p| p.id == pid).unwrap().settings, before, "Undo puts the old settings back");
    assert!(h.app.ui.toast_action.is_none());
}

/// A user preset `user.{i}` named `name` in `group`.
fn preset(i: usize, name: &str, group: &str) -> lcc_develop::Preset {
    lcc_develop::Preset {
        id: format!("user.{i}"),
        name: name.into(),
        group: group.into(),
        settings: json!({"light": {"exposure": 0.1}}),
        favorite: false,
        builtin: false,
    }
}

fn manager(size: [f32; 2], presets: Vec<lcc_develop::Preset>) -> Headless {
    let mut session = lcc_engine::Session::with_demo();
    session.add_presets(presets);
    let app = LccApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, size, 1.0);
    h.settle(Duration::from_secs(60));
    assert_eq!(h.request("engine.execute", json!({"command": "dialog.presetsManager", "params": {}}), T)["ok"], true);
    for _ in 0..4 {
        h.step();
    }
    h
}

fn window(h: &Headless) -> egui::Rect {
    h.app.widgets.iter().rev().find(|(w, _)| w == "dialog:window").map(|(_, r)| *r).expect("the manager's window")
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn drag(h: &mut Headless, from: egui::Pos2, to: egui::Pos2) {
    assert_eq!(h.request("ui.drag", json!({"x": from.x, "y": from.y, "toX": to.x, "toY": to.y}), T)["ok"], true);
    for _ in 0..20 {
        h.step();
    }
}

/// LR-PRE-MANAGE: the window opens inside the screen and keeps its size: frame after frame, and
/// after drags in its list (the manager used to widen itself past a 4K screen).
#[test]
fn the_manager_fits_the_screen_and_keeps_its_size() {
    let mut list: Vec<lcc_develop::Preset> =
        (0..300).map(|i| preset(i, &format!("A rather long preset name number {i} for the width"), &format!("Group {}", i % 6))).collect();
    list.push(preset(999, &"Very long name ".repeat(40), "Group 0"));
    for screen in [[1600.0, 1000.0], [900.0, 640.0]] {
        let mut h = manager(screen, list.clone());
        h.app.ui.group_open.insert("presetsManager:Group 0".into(), true);
        for _ in 0..3 {
            h.step();
        }
        let first = window(&h);
        assert!(
            first.left() >= 0.0 && first.right() <= screen[0] && first.top() >= 0.0 && first.bottom() <= screen[1],
            "{first:?} inside {screen:?}"
        );
        for _ in 0..30 {
            h.step();
        }
        assert_eq!(window(&h).size(), first.size(), "no growth frame after frame");
        // stray drags over the list and its headers
        let row = rect(&h, "presetsManagerRow:user.0").center();
        for d in [egui::vec2(300.0, 0.0), egui::vec2(-200.0, 40.0), egui::vec2(600.0, -30.0)] {
            drag(&mut h, row, row + d);
        }
        let head = rect(&h, "presetsManagerGroup:Group 0").center();
        drag(&mut h, head, head + egui::vec2(500.0, 0.0));
        let after = window(&h);
        assert!((after.width() - first.width()).abs() < 1.0, "stray drags don't widen it: {first:?} → {after:?}");
        assert!(after.right() <= screen[0] && after.left() >= 0.0, "{after:?}");
        assert!(h.app.ui.presets_manager_size.is_some(), "its size is remembered");
    }
}

/// LR-PRE-MANAGE: the size the user leaves the window at is the size it opens at next time.
#[test]
fn the_manager_opens_at_the_size_it_was_left_at() {
    let mut session = lcc_engine::Session::with_demo();
    session.add_presets(vec![preset(1, "One", "Mine")]);
    let mut app = LccApp::new(session, Services { png: None, ..Default::default() });
    app.ui.presets_manager_size = Some([700.0, 500.0]);
    let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
    h.settle(Duration::from_secs(60));
    assert_eq!(h.request("engine.execute", json!({"command": "dialog.presetsManager", "params": {}}), T)["ok"], true);
    for _ in 0..6 {
        h.step();
    }
    let w = window(&h);
    // (the window as it was left)
    assert!((w.width() - 700.0).abs() < 2.0 && w.height() > 440.0 && w.height() <= 500.5, "{w:?}");
    assert_eq!(h.app.ui.presets_manager_size, Some([700.0, 500.0]));
}

/// LR-PRE-MANAGE: a group's eye hides every preset in it and shows them again; dragging a preset
/// onto a group's header moves it into that group, several selected presets together; a group's
/// Delete asks at the group, and Cancel keeps it.
#[test]
fn a_group_hides_takes_dragged_presets_and_asks_before_deleting() {
    let mut h =
        manager([1600.0, 1000.0], vec![preset(1, "One", "Alpha"), preset(2, "Two", "Alpha"), preset(3, "Three", "Beta"), preset(4, "Four", "Gamma")]);
    // a group's eye hides every preset in it, and shows them again
    {
        click(&mut h, "icon:presetsManagerGroupHide:Alpha");
        let hidden = |h: &Headless| ["user.1", "user.2", "user.3"].map(|id| h.app.session.preset_hidden.iter().any(|x| x == id));
        assert_eq!(hidden(&h), [true, true, false], "the group hidden, the other not");
        click(&mut h, "icon:presetsManagerGroupHide:Alpha");
        assert_eq!(hidden(&h), [false, false, false], "and shown again");
    }
    // dragging a preset onto a group's header moves it into that group; several selected presets go
    // together
    h.app.ui.group_open.insert("presetsManager:Alpha".into(), true);
    h.step();
    h.step();
    let from = rect(&h, "presetsManagerRow:user.1").center();
    let to = rect(&h, "presetsManagerGroup:Beta").center();
    drag(&mut h, from, to);
    let group = |h: &Headless, id: &str| h.app.session.presets.iter().find(|p| p.id == id).map(|p| p.group.clone());
    assert_eq!(group(&h, "user.1").as_deref(), Some("Beta"), "dropped on Beta's header");
    assert_eq!(group(&h, "user.2").as_deref(), Some("Alpha"), "the others stay");
    // Ctrl-click picks a second one; both go together
    h.app.ui.group_open.insert("presetsManager:Beta".into(), true);
    h.step();
    h.step();
    click(&mut h, "presetsManagerRow:user.1");
    assert_eq!(h.request("ui.clickWidget", json!({"id": "presetsManagerRow:user.3", "ctrl": true}), T)["ok"], true);
    h.step();
    h.step();
    let from = rect(&h, "presetsManagerRow:user.3").center();
    let to = rect(&h, "presetsManagerGroup:Alpha").center();
    drag(&mut h, from, to);
    assert_eq!((group(&h, "user.1").as_deref(), group(&h, "user.3").as_deref()), (Some("Alpha"), Some("Alpha")), "both selected presets moved");
    // a group's Delete asks at the group, and Cancel keeps it (all three are in Alpha by now)
    click(&mut h, "icon:presetsManagerGroupDelete:Alpha");
    let ask = rect(&h, "label:presetsManagerGroupDeleteAsk:Alpha");
    let head = rect(&h, "presetsManagerGroup:Alpha");
    assert!(ask.top() >= head.bottom() - 1.0 && ask.top() < head.bottom() + 60.0, "asked under the group's header: {head:?} {ask:?}");
    click(&mut h, "button:presetsManagerGroupDeleteCancel:Alpha");
    assert!(!widget(&mut h, "label:presetsManagerGroupDeleteAsk:Alpha"), "Cancel closes it");
    assert_eq!(h.app.session.presets.iter().filter(|p| p.group == "Alpha").count(), 3);
    click(&mut h, "icon:presetsManagerGroupDelete:Alpha");
    click(&mut h, "button:presetsManagerGroupDeleteConfirm:Alpha");
    assert!(!h.app.session.presets.iter().any(|p| p.group == "Alpha"), "the group and its presets are gone");
    assert!(h.app.session.presets.iter().any(|p| p.id == "user.4"), "another group's preset stays");
}

/// LR-PRE-MANAGE: with 10 000 presets in 40 groups, all open, a frame lays out only the rows in
/// view, and stays quick.
#[test]
fn ten_thousand_presets_lay_out_only_the_rows_in_view() {
    let list: Vec<lcc_develop::Preset> = (0..10_000).map(|i| preset(i, &format!("Look {i}"), &format!("Collection {}", i % 40))).collect();
    let mut h = manager([1600.0, 1000.0], list);
    for g in 0..40 {
        h.app.ui.group_open.insert(format!("presetsManager:Collection {g}"), true);
    }
    for _ in 0..4 {
        h.step();
    }
    let (first, end) = crate::panels::presets_manager::drawn_rows(&h.view.ctx).expect("the list was drawn");
    assert!(end > first && end - first < 80, "only the rows in view: {first}..{end}");
    let rows = h.app.widgets.iter().filter(|(w, _)| w.starts_with("presetsManagerRow:")).count();
    assert!(rows > 5 && rows < 80, "{rows} preset rows laid out");
    // frames with nothing changed reuse the rows
    let t0 = std::time::Instant::now();
    for _ in 0..20 {
        h.step();
    }
    let per = t0.elapsed() / 20;
    eprintln!("presets manager, 10 000 presets: {per:?} a frame (headless, the whole app)");
    // scrolled to the end, still only the rows in view
    let at = rect(&h, "presetsManagerGroup:Collection 0").center() + egui::vec2(0.0, 200.0);
    h.request("ui.move", json!({"x": at.x, "y": at.y}), T);
    h.step();
    assert_eq!(h.request("ui.scroll", json!({"dy": -400000.0}), T)["ok"], true);
    for _ in 0..40 {
        h.step();
    }
    let (first, end) = crate::panels::presets_manager::drawn_rows(&h.view.ctx).expect("the list was drawn");
    assert!(first > 9_000 && end - first < 80, "scrolled: {first}..{end}");
}

/// Pictures of the manager for a review (`PRESETS_MANAGER_SNAPSHOTS=<folder>`, run with
/// `--ignored`): 10 000 presets in 40 groups, folded and opened, and a group's Delete asking.
#[test]
#[ignore = "writes pictures for a review"]
fn presets_manager_snapshots() {
    let Ok(dir) = std::env::var("PRESETS_MANAGER_SNAPSHOTS") else { return };
    let dir = std::path::PathBuf::from(dir);
    let mut list: Vec<lcc_develop::Preset> =
        (0..10_000).map(|i| preset(i, &format!("Look {i:05} soft film warm"), &format!("Collection_{}", i % 40))).collect();
    list.push(preset(20_000, &"A very long preset name that goes on and on ".repeat(6), "Collection_1"));
    let mut h = manager([1920.0, 1080.0], list);
    for i in (0..10_000).step_by(3) {
        h.app.session.preset_hidden.push(format!("user.{i}"));
    }
    h.app.session.presets.bump();
    let save = |h: &mut Headless, name: &str| {
        let img = h.snapshot(Duration::from_secs(20));
        let [w, hh] = img.size;
        let raw: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
        image::RgbaImage::from_raw(w as u32, hh as u32, raw).unwrap().save(dir.join(name)).unwrap();
    };
    save(&mut h, "01-folded.png");
    h.app.ui.group_open.insert("presetsManager:Collection_1".into(), true);
    h.step();
    h.step();
    click(&mut h, "presetsManagerRow:user.1");
    save(&mut h, "02-open.png");
    click(&mut h, "icon:presetsManagerGroupDelete:Collection_0");
    save(&mut h, "03-group-delete.png");
}
