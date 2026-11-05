fn library() -> crate::headless::Headless {
    use lightcraft_catalog::{Op, Photo, PhotoId, Source};
    let mut session = lightcraft_engine::Session::new();
    for id in 1..=4 {
        let photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/lightcraft-shortcuts/{id}.jpg") },
            &format!("{id}.jpg"),
            "JPEG",
            100,
            100,
            "2026-10-08",
        );
        session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    session.execute("library.sort", &json!({"key": "fileName", "ascending": true})).unwrap();
    session.execute("library.select", &json!({"ids": [1]})).unwrap();
    let app = LightcraftApp::new(session, crate::Services { png: None, ..Default::default() });
    let mut h = crate::headless::Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.ui.view = crate::state::ViewMode::PhotoGrid;
    for _ in 0..3 {
        h.step();
    }
    h
}

fn key(h: &mut crate::headless::Headless, key: &str, shift: bool) {
    let r = h.request("ui.key", json!({"key": key, "shift": shift}), std::time::Duration::from_secs(20));
    assert_eq!(r["ok"], true, "{r}");
}

/// Press the default Select Only Active Photo chord (the command key on every platform).
fn select_only_active_chord(h: &mut crate::headless::Headless) {
    let r = h.request("ui.key", json!({"key": "D", "cmd": true, "shift": true}), std::time::Duration::from_secs(20));
    assert_eq!(r["ok"], true, "{r}");
}

#[test]
fn select_only_active_keeps_the_active_photo_and_the_edit_undo_step() {
    use crate::state::ViewMode;
    use lightcraft_catalog::PhotoId;
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid, ViewMode::Detail] {
        let mut h = library();
        h.app.ui.view = view;
        h.app.session.execute("library.select", &json!({"ids": [1, 2, 3], "active": 2})).unwrap();
        h.app.session.execute("photo.rate", &json!({"rating": 4})).unwrap();
        select_only_active_chord(&mut h);
        assert_eq!(h.app.session.selection.ids, vec![PhotoId(2)]);
        assert_eq!(h.app.session.active(), Some(PhotoId(2)));
        // Narrowing the selection does not replace the last photo-edit undo step.
        h.app.session.execute("edit.undo", &json!({})).unwrap();
        for id in 1..=3 {
            assert_eq!(h.app.session.catalog.photo(PhotoId(id)).unwrap().rating, 0);
        }
    }
}

#[test]
fn select_only_active_yields_to_a_focused_text_field() {
    use lightcraft_catalog::PhotoId;
    let mut h = library();
    h.app.session.execute("library.select", &json!({"ids": [1, 2], "active": 2})).unwrap();
    let r = h.request("ui.clickWidget", json!({"id": "field:search"}), std::time::Duration::from_secs(20));
    assert_eq!(r["ok"], true, "{r}");
    select_only_active_chord(&mut h);
    assert_eq!(h.app.session.selection.ids, vec![PhotoId(1), PhotoId(2)]);
    assert_eq!(h.app.session.active(), Some(PhotoId(2)));
}

#[test]
fn select_only_active_follows_the_keymap() {
    use lightcraft_catalog::PhotoId;
    let mut h = library();
    let both = vec![PhotoId(1), PhotoId(2)];
    h.app.session.execute("library.select", &json!({"ids": [1, 2], "active": 2})).unwrap();
    assert_eq!(super::shortcut_of(&h.app.ui.settings.keymap, "library.selectOnlyActive"), Some("Cmd+Shift+D"));
    // unbound: the default chord does nothing
    super::assign(&mut h.app.ui.settings.keymap, "library.selectOnlyActive", None).unwrap();
    select_only_active_chord(&mut h);
    assert_eq!(h.app.session.selection.ids, both);
    // rebound: the new key narrows the selection and the old chord still does not
    super::assign(&mut h.app.ui.settings.keymap, "library.selectOnlyActive", Some("F9")).unwrap();
    select_only_active_chord(&mut h);
    assert_eq!(h.app.session.selection.ids, both);
    key(&mut h, "F9", false);
    assert_eq!(h.app.session.selection.ids, vec![PhotoId(2)]);
}

#[test]
fn select_only_active_without_active_photo_does_not_change_selection() {
    let mut h = library();
    // Deselect All keeps the active photo selected, so clear the whole selection state instead.
    h.app.session.selection = Default::default();
    assert!(!crate::menus::ui_enabled(&h.app, "library.selectOnlyActive"));
    assert!(h.app.run("library.selectOnlyActive", json!({})).is_err());
    select_only_active_chord(&mut h);
    assert!(h.app.session.selection.ids.is_empty());
}

#[test]
fn library_rating_labels_flags_and_undo() {
    use lightcraft_catalog::{ColorLabel, Flag, PhotoId};
    for view in [crate::state::ViewMode::PhotoGrid, crate::state::ViewMode::SquareGrid] {
        let mut h = library();
        h.app.ui.view = view;
        h.app.session.execute("library.select", &json!({"ids": [1, 2], "active": 1})).unwrap();
        for rating in [1, 2, 3, 4, 5, 0] {
            key(&mut h, &rating.to_string(), false);
            for id in [1, 2] {
                assert_eq!(h.app.session.catalog.photo(PhotoId(id)).unwrap().rating, rating);
            }
        }
        for (k, label) in [("6", ColorLabel::Red), ("7", ColorLabel::Yellow), ("8", ColorLabel::Green), ("9", ColorLabel::Blue)] {
            key(&mut h, k, false);
            assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().label, Some(label));
            assert_eq!(h.app.session.catalog.photo(PhotoId(2)).unwrap().label, Some(label));
        }
        for (k, flag) in [("P", Flag::Pick), ("X", Flag::Reject), ("U", Flag::None)] {
            key(&mut h, k, false);
            assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().flag, flag);
            assert_eq!(h.app.session.catalog.photo(PhotoId(2)).unwrap().flag, flag);
        }
        h.app.session.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().flag, Flag::Reject);
        assert_eq!(h.app.session.catalog.photo(PhotoId(2)).unwrap().flag, Flag::Reject);
        h.app.session.execute("edit.redo", &json!({})).unwrap();
        assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().flag, Flag::None);
        assert_eq!(h.app.session.active(), Some(PhotoId(1)));
    }
}

/// A saved keymap entry that gives a key to one command (kept from before another command
/// declared that key) wins: the press runs that command alone, and the other command's declared
/// key comes back once the entry is gone.
#[test]
fn a_saved_key_wins_over_another_commands_declared_key() {
    use lightcraft_catalog::{Flag, PhotoId};
    let mut h = library();
    h.app.ui.settings.keymap.insert("photo.increaseRating".into(), "P".into());
    assert_eq!(super::shortcut_of(&h.app.ui.settings.keymap, "photo.pick"), None);
    assert_eq!(super::conflicts(&h.app.ui.settings.keymap, "view.survey", "P"), vec!["photo.increaseRating"]);
    key(&mut h, "P", false);
    let photo = h.app.session.catalog.photo(PhotoId(1)).unwrap();
    assert_eq!(photo.rating, 1, "the saved key raised the rating");
    assert_eq!(photo.flag, Flag::None, "Flag as Pick's declared key yielded");
    // with the entry gone, P picks again
    h.app.ui.settings.keymap.remove("photo.increaseRating");
    assert_eq!(super::shortcut_of(&h.app.ui.settings.keymap, "photo.pick"), Some("P"));
    key(&mut h, "P", false);
    assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().flag, Flag::Pick);
    // a contextual partner's saved key does not silence the other side
    h.app.ui.settings.keymap.insert("crop.rotateAspect".into(), "X".into());
    assert_eq!(super::shortcut_of(&h.app.ui.settings.keymap, "photo.reject"), Some("X"));
    // nor does an entry for a command this build doesn't know
    h.app.ui.settings.keymap.insert("no.suchCommand".into(), "U".into());
    assert_eq!(super::shortcut_of(&h.app.ui.settings.keymap, "photo.unflag"), Some("U"));
}

/// Keymap entries for commands this build doesn't know take no key: an alias, a rating key and
/// a declared shortcut on the same keys keep working.
#[test]
fn junk_keymap_entries_leave_aliases_ratings_and_declared_keys() {
    use lightcraft_catalog::{Flag, PhotoId};
    let mut h = library();
    for (id, sc) in [("no.suchCommand", "Shift+X"), ("no.otherCommand", "5"), ("no.thirdCommand", "P")] {
        h.app.ui.settings.keymap.insert(id.into(), sc.into());
    }
    key(&mut h, "5", false);
    assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().rating, 5, "5 still rates");
    key(&mut h, "P", false);
    assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().flag, Flag::Pick, "P still picks");
    key(&mut h, "X", true);
    assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().flag, Flag::Reject, "Shift+X still rejects");
}

#[test]
fn classic_f6_and_f7_toggle_the_filmstrip_and_left_panel() {
    let mut h = library();
    let (filmstrip, left) = (h.app.ui.filmstrip, h.app.ui.left_panel);
    key(&mut h, "F6", false);
    assert_eq!(h.app.ui.filmstrip, !filmstrip);
    assert_eq!(h.app.ui.left_panel, left);
    key(&mut h, "F7", false);
    assert_eq!(h.app.ui.left_panel, !left);
    key(&mut h, "F6", false);
    key(&mut h, "F7", false);
    assert_eq!((h.app.ui.filmstrip, h.app.ui.left_panel), (filmstrip, left));
}

/// Classic's F8 hides the right panel and shows the same one again; the other panels stay.
#[test]
fn classic_f8_hides_the_right_panel_and_brings_the_same_one_back() {
    use crate::state::RightPanel;
    let mut h = library();
    h.app.ui.view = crate::state::ViewMode::Detail;
    h.app.ui.right = RightPanel::Keywords;
    h.settle(std::time::Duration::from_secs(10));
    let (filmstrip, left) = (h.app.ui.filmstrip, h.app.ui.left_panel);
    key(&mut h, "F8", false);
    assert_eq!(h.app.ui.right, RightPanel::None);
    assert_eq!((h.app.ui.filmstrip, h.app.ui.left_panel), (filmstrip, left));
    key(&mut h, "F8", false);
    assert_eq!(h.app.ui.right, RightPanel::Keywords, "the panel F8 hid comes back");
    // a tool ends when F8 hides it; with nothing hidden before, F8 opens Edit
    h.app.ui.right = RightPanel::None;
    h.app.ui.right_hidden = RightPanel::None;
    key(&mut h, "F8", false);
    assert_eq!(h.app.ui.right, RightPanel::Edit);
}

/// F8 on a minimized Tools window opens it again in the mode it had.
#[test]
fn classic_f8_shows_a_minimized_tools_window_and_keeps_its_mode() {
    use crate::{docking::Panel, state::RightPanel};
    let mut h = library();
    h.app.ui.view = crate::state::ViewMode::Detail;
    h.app.ui.right = RightPanel::Edit;
    h.settle(std::time::Duration::from_secs(10));
    h.app.run("ui.dock", json!({"operation": "float", "panel": "tools", "rect": [400, 100, 320, 500]})).unwrap();
    h.app.run("ui.dock", json!({"action": {"SetMinimized": {"panel": "tools", "minimized": true}}})).unwrap();
    h.app.ui.right = RightPanel::Crop;
    h.step();
    assert!(!crate::docking::exposed(&h.app, Panel::Tools));
    key(&mut h, "F8", false);
    assert_eq!(h.app.ui.right, RightPanel::Crop);
    assert!(crate::docking::exposed(&h.app, Panel::Tools), "the window opens");
}

#[test]
fn shift_culling_keys_advance_exactly_once() {
    use lightcraft_catalog::{ColorLabel, Flag, PhotoId};
    for auto in [false, true] {
        for (k, rating, label, flag) in [
            ("0", 0, None, Flag::Pick),
            ("5", 5, None, Flag::Pick),
            ("6", 3, Some(ColorLabel::Red), Flag::Pick),
            ("7", 3, Some(ColorLabel::Yellow), Flag::Pick),
            ("8", 3, Some(ColorLabel::Green), Flag::Pick),
            ("9", 3, Some(ColorLabel::Blue), Flag::Pick),
            ("P", 3, None, Flag::Pick),
            ("X", 3, None, Flag::Reject),
            ("U", 3, None, Flag::None),
        ] {
            let mut h = library();
            h.app.ui.auto_advance = auto;
            h.app.session.execute("photo.rate", &json!({"rating": 3})).unwrap();
            h.app.session.execute("photo.pick", &json!({})).unwrap();
            key(&mut h, k, true);
            let p = h.app.session.catalog.photo(PhotoId(1)).unwrap();
            assert_eq!(p.rating, rating, "Shift+{k}");
            assert_eq!(p.label, label, "Shift+{k}");
            assert_eq!(p.flag, flag, "Shift+{k}");
            assert_eq!(h.app.session.active(), Some(PhotoId(2)), "Shift+{k}, auto={auto}");
        }
    }
    let mut h = library();
    h.app.session.execute("library.select", &json!({"ids": [4]})).unwrap();
    key(&mut h, "6", true);
    assert_eq!(h.app.session.active(), Some(PhotoId(4)), "last photo stays selected");
}

#[test]
fn shift_pick_preserves_presets_outside_the_library_grids() {
    let mut h = library();
    h.app.ui.view = crate::state::ViewMode::SquareGrid;
    key(&mut h, "P", true);
    assert_eq!(h.app.session.active(), Some(lightcraft_catalog::PhotoId(2)));
    assert!(!h.app.ui.presets, "grid Shift+P must not open Presets");
    assert_eq!(h.app.ui.view, crate::state::ViewMode::SquareGrid);
    h.app.ui.view = crate::state::ViewMode::Detail;
    key(&mut h, "P", true);
    assert!(h.app.ui.presets);
    assert_eq!(h.app.session.active(), Some(lightcraft_catalog::PhotoId(2)));
}

#[test]
fn shift_label_targets_only_candidate_in_compare() {
    use lightcraft_catalog::{ColorLabel, PhotoId};
    let mut h = library();
    h.app.session.execute("library.select", &json!({"ids": [1, 2], "active": 1})).unwrap();
    crate::panels::compare::enter_compare(&mut h.app).unwrap();
    key(&mut h, "6", true);
    assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().label, None);
    assert_eq!(h.app.session.catalog.photo(PhotoId(2)).unwrap().label, Some(ColorLabel::Red));
    assert_eq!(h.app.session.active(), Some(PhotoId(3)));
}

#[test]
fn plain_culling_keys_auto_advance_once() {
    use lightcraft_catalog::PhotoId;
    for k in ["0", "5", "6", "9", "P", "X", "U"] {
        let mut h = library();
        h.app.ui.auto_advance = true;
        key(&mut h, k, false);
        assert_eq!(h.app.session.active(), Some(PhotoId(2)), "{k}");
    }
}

#[test]
fn culling_keys_go_on_from_a_photo_that_leaves_the_filter() {
    // #2312: the filter shows one-star photos; taking photo 2's star off makes photo 3 active, once, with or
    // without Shift and Auto Advance, and → goes on from there instead of from the first photo
    use lightcraft_catalog::PhotoId;
    let one_star = || {
        let mut h = library();
        h.app.session.execute("library.selectAll", &json!({})).unwrap();
        h.app.session.execute("photo.rate", &json!({"rating": 1})).unwrap();
        h.app.session.execute("library.filter", &json!({"rating": 1, "ratingOp": "exactly"})).unwrap();
        h.app.session.execute("library.select", &json!({"ids": [2]})).unwrap();
        h
    };
    for view in [crate::state::ViewMode::PhotoGrid, crate::state::ViewMode::Detail] {
        for (shift, auto) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut h = one_star();
            h.app.ui.view = view;
            h.app.ui.auto_advance = auto;
            key(&mut h, "0", shift);
            assert_eq!(h.app.session.catalog.photo(PhotoId(2)).unwrap().rating, 0);
            assert_eq!(h.app.session.active(), Some(PhotoId(3)), "{view:?} Shift={shift} Auto Advance={auto}");
            key(&mut h, "right", false);
            assert_eq!(h.app.session.active(), Some(PhotoId(4)), "{view:?} Shift={shift} Auto Advance={auto}");
        }
    }
    // the Photo menu with Auto Advance too
    let mut h = one_star();
    h.app.ui.auto_advance = true;
    crate::menubar::run_item(&mut h.app, "photo.rate", json!({"rating": 0})).unwrap();
    assert_eq!(h.app.session.active(), Some(PhotoId(3)));
}

#[test]
fn color_label_actions_show_feedback_only_after_success() {
    use lightcraft_catalog::PhotoId;
    let mut h = library();
    for shift in [false, true] {
        for (k, name) in [("6", "Red"), ("7", "Yellow"), ("8", "Green"), ("9", "Blue")] {
            h.app.session.execute("library.select", &json!({"ids": [1]})).unwrap();
            h.app.ui.toast = None;
            key(&mut h, k, shift);
            assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some(format!("{name} Label").as_str()));
            assert_eq!(h.app.ui.toast.as_ref().and_then(|t| t.2), h.app.session.catalog.photo(PhotoId(1)).unwrap().label);
            assert_eq!(h.app.session.active(), Some(PhotoId(if shift { 2 } else { 1 })));
        }
    }
    // Menus and the Info-panel swatches dispatch the same command, including purple/clear.
    h.app.run("photo.label", json!({"label": "purple"})).unwrap();
    assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("Purple Label"));
    h.app.run("photo.label", json!({"label": "none"})).unwrap();
    assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("Color label cleared"));
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.2.is_none()), "clear uses neutral feedback");
    h.app.session.execute("label.setNames", &json!({"names": {"red": "Needs review"}})).unwrap();
    key(&mut h, "6", false);
    assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("Needs review Label"));
    // Native menu callbacks run before logic() catches up to the current frame's clock.
    let now = h.view.ctx.input(|i| i.time);
    h.app.last_time = now - 10.0;
    h.app.run("photo.label", json!({"label": "blue"})).unwrap();
    assert!(h.app.ui.toast.as_ref().is_some_and(|t| t.1 > now), "menu feedback must survive an idle gap");
    h.app.ui.toast = None;
    assert!(h.app.run("photo.label", json!({"label": "invalid"})).is_err());
    assert!(h.app.ui.toast.is_none(), "failed actions must not announce success");
}

#[test]
fn shifted_number_punctuation_rates_and_advances() {
    use lightcraft_catalog::PhotoId;
    let mut h = library();
    // A real winit event on a layout with Shift+1 = !, rather than ui.key's logical Num1.
    for pressed in [true, false] {
        h.app.synthetic.push(egui::Event::Key {
            key: Key::Exclamationmark,
            physical_key: Some(Key::Num1),
            pressed,
            repeat: false,
            modifiers: Modifiers::SHIFT,
        });
    }
    h.step();
    assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().rating, 1);
    assert_eq!(h.app.session.active(), Some(PhotoId(2)));
}

#[test]
fn library_keys_yield_to_search_and_crop_context() {
    use lightcraft_catalog::{Flag, PhotoId};
    let mut h = library();
    let r = h.request("ui.clickWidget", json!({"id": "field:search"}), std::time::Duration::from_secs(20));
    assert_eq!(r["ok"], true, "{r}");
    for k in ["5", "6", "P", "X", "U"] {
        key(&mut h, k, false);
        key(&mut h, k, true);
    }
    let p = h.app.session.catalog.photo(PhotoId(1)).unwrap();
    assert_eq!(p.rating, 0);
    assert_eq!(p.label, None);
    assert_eq!(p.flag, Flag::None);
    assert_eq!(h.app.session.active(), Some(PhotoId(1)));

    let mut h = library();
    h.app.ui.view = crate::state::ViewMode::Detail;
    h.app.ui.right = crate::state::RightPanel::Crop;
    key(&mut h, "X", false);
    assert_eq!(h.app.session.catalog.photo(PhotoId(1)).unwrap().flag, Flag::None);
}

#[test]
fn delete_means_the_backspace_key() {
    assert_eq!(parse("Delete"), Some((Modifiers::NONE, Key::Backspace)));
    assert_eq!(parse("Cmd+Delete"), Some((Modifiers::COMMAND, Key::Backspace)));
}
use super::*;

#[test]
fn parses_shortcuts() {
    let (m, k) = parse("Cmd+Shift+Z").unwrap();
    assert!(m.command && m.shift && !m.alt);
    assert_eq!(k, Key::Z);
    assert_eq!(parse("Right").unwrap().1, Key::ArrowRight);
    assert_eq!(parse("\\").unwrap().1, Key::Backslash);
    // every declared shortcut parses
    for (id, _, sc, _) in crate::menus::ui_commands() {
        if let Some(sc) = sc {
            assert!(parse(sc).is_some(), "{id}: {sc}");
        }
    }
    for c in lightcraft_engine::command_specs() {
        if let Some(sc) = c.shortcut {
            assert!(parse(sc).is_some(), "{}: {sc}", c.id);
        }
    }
}

#[test]
fn aliases_parse_and_target_existing_commands() {
    let ui: Vec<&str> = crate::menus::ui_commands().map(|c| c.0).collect();
    for (sc, id, params) in ALIASES {
        assert!(parse(sc).is_some(), "{id}: {sc}");
        assert!(ui.contains(id) || lightcraft_engine::find_command(id).is_some(), "alias {sc} → unknown command {id}");
        assert!(serde_json::from_str::<serde_json::Value>(params).is_ok(), "alias {sc}: bad params");
    }
}

#[test]
fn key_presses_format_to_shortcuts_that_parse_back() {
    for k in Key::ALL {
        for m in [Modifiers::NONE, Modifiers::SHIFT, Modifiers::COMMAND, Modifiers::ALT | Modifiers::SHIFT] {
            if let Some(sc) = format(m, *k) {
                let want = if *k == Key::Delete { Key::Backspace } else { *k };
                assert_eq!(parse(&sc).map(|p| p.1), Some(want), "{sc}");
            }
        }
    }
    assert_eq!(format(Modifiers::COMMAND | Modifiers::SHIFT, Key::K).as_deref(), Some("Cmd+Shift+K"));
    assert_eq!(format(Modifiers::NONE, Key::Slash).as_deref(), Some("/"));
    assert_eq!(format(Modifiers::NONE, Key::Backspace).as_deref(), Some("Delete"));
}

#[test]
fn assigning_a_key_moves_it_and_reset_restores_it() {
    let mut keymap = Keymap::new();
    assert_eq!(shortcut_of(&keymap, "view.survey"), Some("N"));
    // D belongs to Detail: Survey takes it, Detail loses it
    let lost = assign(&mut keymap, "view.survey", Some("D")).unwrap();
    assert_eq!(lost, vec!["view.detail"]);
    assert_eq!(shortcut_of(&keymap, "view.survey"), Some("D"));
    assert_eq!(shortcut_of(&keymap, "view.detail"), None);
    // restoring Detail takes D back
    let lost = reset(&mut keymap, "view.detail").unwrap();
    assert_eq!(lost, vec!["view.survey"]);
    assert_eq!(shortcut_of(&keymap, "view.detail"), Some("D"));
    reset(&mut keymap, "view.survey").unwrap();
    assert!(keymap.is_empty(), "declared shortcuts aren't stored: {keymap:?}");
    // a command without a declared shortcut can get one, and lose it again
    assign(&mut keymap, "view.photoGrid", Some("cmd+shift+1")).unwrap_err();
    assign(&mut keymap, "view.photoGrid", Some("Cmd+Shift+1")).unwrap();
    assert_eq!(shortcut_of(&keymap, "view.photoGrid"), Some("Cmd+Shift+1"));
    assign(&mut keymap, "view.photoGrid", None).unwrap();
    assert!(keymap.is_empty());
}

#[test]
fn bad_assignments_are_errors() {
    let mut keymap = Keymap::new();
    assert!(assign(&mut keymap, "no.suchCommand", Some("K")).is_err());
    assert!(assign(&mut keymap, "view.survey", Some("Cmd+Nonsense")).is_err());
    assert!(assign(&mut keymap, "view.survey", Some("Cmd+Q")).is_err());
    assert!(assign(&mut keymap, "view.survey", Some("Cmd+,")).is_err());
    assert!(keymap.is_empty());
}

/// Junk in a hand-edited `ui.json` keymap is ignored (the declared shortcut stays), never a panic.
#[test]
fn junk_keymap_entries_mean_no_shortcut() {
    let mut keymap = Keymap::new();
    keymap.insert("view.survey".into(), "Hyper+☃".into());
    keymap.insert("no.suchCommand".into(), "K".into());
    keymap.insert("view.detail".into(), "Cmd+SuperLeft".into());
    assert_eq!(shortcut_of(&keymap, "view.survey"), Some("N"));
    assert_eq!(shortcut_of(&keymap, "view.detail"), Some("D"));
    assert!(conflicts(&keymap, "view.compare", "N").contains(&"view.survey"));
    assert_eq!(parse("Hyper+K"), None);
}

/// Contextual partners keep sharing their key when one is reassigned.
#[test]
fn contextual_partners_are_not_conflicts() {
    let keymap = Keymap::new();
    assert!(conflicts(&keymap, "photo.reject", "X").is_empty());
}

/// No key fires two different actions (a UI command may shadow the engine command it wraps).
#[test]
fn no_conflicting_bindings() {
    let mut ui: Vec<((Modifiers, Key), String)> = Vec::new();
    for (id, _, sc, _) in crate::menus::ui_commands() {
        if let Some(k) = sc.and_then(parse) {
            ui.push((k, id.to_string()));
        }
    }
    for (sc, id, _) in ALIASES {
        ui.push((parse(sc).unwrap(), format!("alias {id}")));
    }
    let mut engine: Vec<((Modifiers, Key), &str)> = Vec::new();
    for c in lightcraft_engine::command_specs() {
        if let Some(k) = c.shortcut.and_then(parse) {
            engine.push((k, c.id));
        }
    }
    for (sc, id, _) in ALIASES {
        let k = parse(sc).unwrap();
        assert!(!engine.iter().any(|(k2, _)| *k2 == k), "alias {sc} ({id}) shadows an engine shortcut");
    }
    for (i, (k, a)) in ui.iter().enumerate() {
        for (k2, b) in &ui[i + 1..] {
            let contextual = CONTEXTUAL.iter().any(|(x, y)| (x == a && y == b) || (x == b && y == a));
            assert!(k != k2 || contextual, "{a} and {b} share a key");
        }
    }
    for (i, (k, a)) in engine.iter().enumerate() {
        for (k2, b) in &engine[i + 1..] {
            let contextual = CONTEXTUAL.iter().any(|(x, y)| (x == a && y == b) || (x == b && y == a));
            assert!(k != k2 || contextual, "{a} and {b} share a key");
        }
    }
}
mod issue1771 {
    use super::*;
    use crate::state::{Dialog, ViewMode};
    use lightcraft_catalog::PhotoId;

    fn grid(view: ViewMode) -> crate::headless::Headless {
        let mut h = library();
        h.app.ui.view = view;
        h.app.session.execute("library.select", &json!({"ids": [1, 2, 3], "active": 2})).unwrap();
        for _ in 0..3 {
            h.step();
        }
        h
    }

    #[test]
    fn issue1771_photo_grid_enter_opens_active_preserving_multiselection() {
        opens(ViewMode::PhotoGrid);
    }

    #[test]
    fn issue1771_square_grid_enter_opens_active_preserving_multiselection() {
        opens(ViewMode::SquareGrid);
    }

    fn opens(view: ViewMode) {
        let mut h = grid(view);
        key(&mut h, "Enter", false);
        assert_eq!(h.app.ui.view, ViewMode::Detail, "Enter from {view:?}");
        assert_eq!(h.app.session.active(), Some(PhotoId(2)));
        assert_eq!(h.app.session.selection.ids, vec![PhotoId(1), PhotoId(2), PhotoId(3)]);
        key(&mut h, "Enter", false);
        assert_eq!(h.app.ui.view, ViewMode::Detail, "second Enter remains in Detail");
        assert_eq!(h.app.session.active(), Some(PhotoId(2)));
    }

    #[test]
    fn issue1771_no_selection_and_modifier_chords_do_not_open() {
        for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
            let mut h = grid(view);
            for modifiers in [json!({"shift":true}), json!({"cmd":true}), json!({"alt":true}), json!({"ctrl":true})] {
                let mut args = modifiers;
                args["key"] = json!("Enter");
                assert_eq!(h.request("ui.key", args, std::time::Duration::from_secs(20))["ok"], true);
                assert_eq!(h.app.ui.view, view);
            }
            h.app.session.selection = Default::default();
            key(&mut h, "Enter", false);
            assert_eq!(h.app.ui.view, view);
            assert!(h.app.session.selection.ids.is_empty());
            assert_eq!(h.app.session.active(), None);
        }
    }

    #[test]
    fn issue1771_search_dialog_and_shortcut_recording_keep_enter() {
        for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
            let mut h = grid(view);
            assert_eq!(h.request("ui.clickWidget", json!({"id":"field:search"}), std::time::Duration::from_secs(20))["ok"], true);
            key(&mut h, "Enter", false);
            assert_eq!(h.app.ui.view, view, "focused search");
            h.app.run("app.shortcuts", json!({})).unwrap();
            h.step();
            h.step();
            key(&mut h, "Enter", false);
            assert_eq!(h.app.ui.view, view, "shortcut dialog");
            assert!(matches!(h.app.ui.dialog, Some(Dialog::Shortcuts)));
            h.app.recording_shortcut = Some("view.survey".into());
            key(&mut h, "Enter", false);
            assert_eq!(h.app.ui.view, view, "recording owns Enter");
            assert_eq!(super::super::shortcut_of(&h.app.ui.settings.keymap, "view.survey"), Some("Enter"));
        }
    }

    #[test]
    fn issue1771_custom_binding_and_native_menu_keep_enter() {
        for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
            let mut h = grid(view);
            h.app.run("app.setShortcut", json!({"id":"view.survey", "shortcut":"Enter"})).unwrap();
            key(&mut h, "Enter", false);
            assert_eq!(h.app.ui.view, ViewMode::Survey, "custom binding wins");
            assert_eq!(h.app.session.active(), Some(PhotoId(2)));
            assert_eq!(h.app.session.selection.ids.len(), 3);
            let mut h = grid(view);
            h.app.native_shortcuts.insert("Enter".into());
            key(&mut h, "Enter", false);
            assert_eq!(h.app.ui.view, view, "native menu owns Enter");
        }
    }

    #[test]
    fn issue1771_popup_keeps_enter() {
        for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
            let mut h = grid(view);
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ui| {
                    let ctx = ui.ctx();
                    egui::Popup::open_id(ctx, egui::Id::unique("issue1771-menu"));
                    assert!(egui::Popup::is_any_open(ctx));
                    super::super::handle(&mut h.app, ctx);
                },
            );
            output.textures_delta.clear();
            assert_eq!(h.app.ui.view, view);
        }
    }

    #[test]
    fn issue1771_detail_enter_keeps_active_and_multiselection() {
        let mut h = grid(ViewMode::Detail);
        key(&mut h, "Enter", false);
        assert_eq!(h.app.ui.view, ViewMode::Detail);
        assert_eq!(h.app.session.active(), Some(PhotoId(2)));
        assert_eq!(h.app.session.selection.ids, vec![PhotoId(1), PhotoId(2), PhotoId(3)]);
    }

    #[test]
    fn issue1771_open_file_menu_keeps_enter() {
        for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
            let mut h = grid(view);
            assert_eq!(h.request("ui.clickWidget", json!({"id":"menu:File"}), std::time::Duration::from_secs(20))["ok"], true);
            h.step();
            assert!(egui::Popup::is_any_open(&h.view.ctx), "File menu open before Enter");
            key(&mut h, "Enter", false);
            assert_eq!(h.app.ui.view, view);
            assert_eq!(h.app.session.active(), Some(PhotoId(2)));
            assert_eq!(h.app.session.selection.ids.len(), 3);
        }
    }

    #[test]
    fn issue1771_repeat_event_opens_once_and_detail_keeps_selection() {
        let mut h = grid(ViewMode::PhotoGrid);
        h.events.push(egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: true, modifiers: egui::Modifiers::NONE });
        h.step();
        h.step();
        assert_eq!(h.app.ui.view, ViewMode::Detail);
        assert_eq!(h.app.session.active(), Some(PhotoId(2)));
        assert_eq!(h.app.session.selection.ids.len(), 3);
        h.events.push(egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: true, modifiers: egui::Modifiers::NONE });
        h.step();
        h.step();
        assert_eq!(h.app.ui.view, ViewMode::Detail);
        assert_eq!(h.app.session.selection.ids.len(), 3);
    }
}
