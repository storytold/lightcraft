//! Accessibility audit: walks the AccessKit tree egui exposes to screen readers (VoiceOver,
//! Narrator, AT-SPI) in each main view and Develop tool, headless, and fails on an interactive
//! node with no name, a slider without its value and range, or a canvas tool no key can reach.
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::{Harness, kittest::NodeT};
use lightcraft_ui_egui::{
    LightcraftApp, Services,
    state::{RightPanel, ViewMode},
};

fn harness(view: ViewMode, right: RightPanel) -> Harness<'static, LightcraftApp> {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.left_panel = true;
    app.ui.view = view;
    app.ui.right = right;
    let mut builder = Harness::builder().with_size(vec2(1500.0, 950.0));
    // text such as "▸" in the Reference view's hint has its glyph only in the craft-fonts faces (releases
    // always have them): without them it is drawn as a box, which this audit of names doesn't need
    if lightcraft_engine::CRAFT_FONTS.is_empty() {
        builder = builder.allow_missing_glyphs();
    }
    let mut h = builder.build_ui_state(
        |ui, app: &mut LightcraftApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(10);
    h
}

fn interactive(role: Role) -> bool {
    matches!(
        role,
        Role::Button
            | Role::CheckBox
            | Role::Switch
            | Role::RadioButton
            | Role::Slider
            | Role::SpinButton
            | Role::TextInput
            | Role::MultilineTextInput
            | Role::SearchInput
            | Role::ComboBox
            | Role::EditableComboBox
            | Role::Tab
            | Role::Link
            | Role::MenuItem
            | Role::MenuItemCheckBox
            | Role::MenuItemRadio
            | Role::ListBoxOption
    )
}

/// Every problem found in the harness's current tree, one line each.
fn audit(h: &Harness<'_, LightcraftApp>, place: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let mut count = 0;
    for node in h.root().children_recursive() {
        let n = node.accesskit_node();
        let role = n.role();
        // egui's own scroll bars are announced by their role, as native ones are
        if role == Role::ScrollBar {
            continue;
        }
        // a widget that takes the focus or a click is interactive whatever its role
        let acts = |a| n.data().supports_action(a);
        if !interactive(role) && !acts(egui::accesskit::Action::Focus) && !acts(egui::accesskit::Action::Click) {
            continue;
        }
        count += 1;
        let name = n.label().unwrap_or_default();
        let name = if name.trim().is_empty() { n.value().unwrap_or_default() } else { name };
        if name.trim().is_empty() {
            problems.push(format!("{place}: unnamed {role:?} at {:?}", n.bounding_box()));
        }
        if role == Role::Slider && (n.numeric_value().is_none() || n.min_numeric_value().is_none() || n.max_numeric_value().is_none()) {
            problems.push(format!("{place}: slider {name:?} lacks value/range"));
        }
    }
    assert!(count > 5, "{place}: the tree has interactive nodes ({count})");
    problems
}

#[test]
fn every_interactive_node_has_a_name_in_every_view_and_tool() {
    // said once per audit, not once per harness
    if lightcraft_engine::CRAFT_FONTS.is_empty() {
        lightcraft_testkit::skip!(Fonts, "built without CRAFT_FONTS_DIR: glyphs only craft-fonts has are drawn as boxes, not checked");
    }
    let mut problems = Vec::new();
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid, ViewMode::Detail, ViewMode::Compare, ViewMode::Survey, ViewMode::Reference] {
        let h = harness(view, RightPanel::None);
        problems.extend(audit(&h, &format!("{view:?}")));
    }
    for right in [
        RightPanel::Edit,
        RightPanel::Profiles,
        RightPanel::Crop,
        RightPanel::Remove,
        RightPanel::Masking,
        RightPanel::RedEye,
        RightPanel::Versions,
        RightPanel::Activity,
        RightPanel::Keywords,
        RightPanel::Info,
    ] {
        let h = harness(ViewMode::Detail, right);
        problems.extend(audit(&h, &format!("Detail/{right:?}")));
    }
    // the dialogs: each a window with a title, every control in it named
    for dialog in [
        "dialog.export",
        "dialog.copySettings",
        "dialog.pasteSettings",
        "dialog.createPreset",
        "dialog.rename",
        "dialog.newAlbum",
        "dialog.newSmartAlbum",
        "dialog.captureTime",
        "dialog.labelNames",
        "dialog.allMetadata",
        "dialog.saveMetadataPreset",
        "dialog.autoStack",
        "dialog.contactSheet",
    ] {
        let mut h = harness(ViewMode::PhotoGrid, RightPanel::None);
        if let Err(e) = h.state_mut().run(dialog, serde_json::json!({})) {
            problems.push(format!("{dialog}: does not open ({e})"));
            continue;
        }
        h.run_steps(6);
        problems.extend(audit(&h, dialog));
        let is_window = |role| matches!(role, Role::Window | Role::Dialog | Role::AlertDialog);
        let windows = h.root().children_recursive().map(|n| n.accesskit_node()).filter(|n| is_window(n.role())).count();
        let untitled = h
            .root()
            .children_recursive()
            .map(|n| n.accesskit_node())
            .filter(|n| is_window(n.role()) && n.label().unwrap_or_default().trim().is_empty())
            .count();
        if untitled > 0 {
            problems.push(format!("{dialog}: {untitled} of {windows} windows have no title"));
        }
    }
    problems.sort();
    problems.dedup();
    assert!(problems.is_empty(), "{} accessibility problems:\n{}", problems.len(), problems.join("\n"));
}

fn slider_value(h: &Harness<'_, LightcraftApp>, name: &str) -> Option<f64> {
    h.root()
        .children_recursive()
        .map(|n| n.accesskit_node())
        .find(|n| n.role() == Role::Slider && n.label().as_deref() == Some(name))
        .and_then(|n| n.numeric_value())
}

/// A screen reader's Increment on a develop slider moves it one step, as an arrow key does.
#[test]
fn screen_reader_increment_moves_a_slider() {
    let mut h = harness(ViewMode::Detail, RightPanel::Edit);
    let before = slider_value(&h, "Exposure").expect("Exposure is in the tree");
    let target = h
        .root()
        .children_recursive()
        .find(|n| n.accesskit_node().role() == Role::Slider && n.accesskit_node().label().as_deref() == Some("Exposure"))
        .unwrap()
        .accesskit_node()
        .locate();
    h.event(egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
        action: egui::accesskit::Action::Increment,
        target_node: target.0,
        target_tree: target.1,
        data: None,
    }));
    h.run_steps(4);
    let after = slider_value(&h, "Exposure").unwrap();
    assert!(after > before, "{before} -> {after}");
}

/// The canvas tools' settings (crop angle and offsets, spot size, mask amount) are reachable with
/// Tab alone, and the arrow keys then adjust them.
#[test]
fn canvas_tool_sliders_are_reachable_with_tab() {
    for right in [RightPanel::Crop, RightPanel::Remove] {
        let mut h = harness(ViewMode::Detail, right);
        let mut reached = None;
        for _ in 0..300 {
            h.key_press(egui::Key::Tab);
            h.run_steps(1);
            let focused = h.root().children_recursive().map(|n| n.accesskit_node()).find(|n| n.is_focused() && n.role() == Role::Slider);
            if let Some(n) = focused {
                reached = Some((n.label().unwrap_or_default(), n.numeric_value()));
                break;
            }
        }
        let (name, before) = reached.unwrap_or_else(|| panic!("{right:?}: no slider takes the focus with Tab"));
        h.key_press(egui::Key::ArrowUp);
        h.run_steps(4);
        let after = slider_value(&h, &name);
        assert_ne!(before, after, "{right:?}: ArrowUp on the Tab-focused {name:?}");
    }
}

/// The canvas handles are in the tree by name, with their position as the value.
#[test]
fn canvas_handles_are_named_with_their_position() {
    let h = harness(ViewMode::Detail, RightPanel::Crop);
    let found: Vec<(String, String)> = h
        .root()
        .children_recursive()
        .map(|n| n.accesskit_node())
        .filter(|n| n.role() == Role::Button)
        .map(|n| (n.label().unwrap_or_default(), n.value().unwrap_or_default()))
        .collect();
    for name in ["Crop top-left corner", "Crop right edge", "Crop box"] {
        assert!(found.iter().any(|(l, v)| l == name && v.contains('%')), "{name}: {found:?}");
    }
    let h = harness(ViewMode::Detail, RightPanel::Remove);
    assert!(h.root().children_recursive().any(|n| n.accesskit_node().label().is_some_and(|l| l.starts_with("New spot"))));
}

/// Tab walks each right panel from top to bottom (rows left to right): the order a sighted user
/// reads it in. Each step down may not jump back up by more than a row.
#[test]
fn tab_walks_the_right_panels_top_to_bottom() {
    for right in [RightPanel::Edit, RightPanel::Crop, RightPanel::Remove, RightPanel::Masking] {
        let mut h = harness(ViewMode::Detail, right);
        let panel_left = 1500.0 - h.state().ui.right_width - 10.0;
        // the tool strip beside the panel is a column of its own, walked before it
        let strip = h
            .root()
            .children_recursive()
            .map(|n| n.accesskit_node())
            .find(|n| n.label().as_deref() == Some("Edit") && n.description().as_deref() == Some("E"))
            .and_then(|n| n.bounding_box())
            .map(|b| b.x0 as f32)
            .unwrap_or(f32::INFINITY);
        let mut seen: Vec<(String, f32)> = Vec::new();
        for _ in 0..120 {
            h.key_press(egui::Key::Tab);
            h.run_steps(1);
            let Some(n) = h.root().children_recursive().map(|n| n.accesskit_node()).find(|n| n.is_focused()) else { continue };
            let Some(b) = n.bounding_box() else { continue };
            if (b.x0 as f32) < panel_left || (b.x0 as f32) >= strip - 1.0 {
                continue;
            }
            let name = n.label().unwrap_or_default();
            if seen.iter().any(|(s, y)| *s == name && (*y - b.y0 as f32).abs() < 1.0) && !name.is_empty() {
                break;
            }
            seen.push((name, b.y0 as f32));
        }
        assert!(seen.len() > 3, "{right:?}: Tab reaches the panel: {seen:?}");
        for w in seen.windows(2) {
            assert!(w[1].1 >= w[0].1 - 30.0, "{right:?}: Tab jumps up from {:?} to {:?}\n{seen:?}", w[0], w[1]);
        }
    }
}

/// A slider's value field is "<slider> value" (the instruction to click, type or drag is its
/// description, not part of the name VoiceOver reads on every visit).
#[test]
fn a_slider_value_field_is_named_value_and_describes_how_to_use_it() {
    const INSTRUCTION: &str = "Click to type a value, drag to change it";
    let h = harness(ViewMode::Detail, RightPanel::Edit);
    let buttons: Vec<_> = h.root().children_recursive().filter(|n| n.accesskit_node().role() == Role::Button).collect();
    let names: Vec<String> = buttons.iter().filter_map(|n| n.accesskit_node().label()).collect();
    let field = buttons
        .iter()
        .find(|n| n.accesskit_node().label().as_deref() == Some("Exposure value"))
        .unwrap_or_else(|| panic!("a value field named \"Exposure value\": {names:?}"));
    assert_eq!(field.accesskit_node().description().as_deref(), Some(INSTRUCTION), "the instruction is its description");
    assert!(!names.iter().any(|n| n.contains(INSTRUCTION)), "no name carries the instruction: {names:?}");
}

/// Whether a button named `name` has `help` as its accessible description (the menu bar has an
/// "Edit" menu too, so the name alone doesn't pick the tool-strip button).
fn button_described(h: &Harness<'_, LightcraftApp>, name: &str, help: &str) -> bool {
    h.root()
        .children_recursive()
        .map(|n| n.accesskit_node())
        .any(|n| n.role() == Role::Button && n.label().as_deref() == Some(name) && n.description().as_deref() == Some(help))
}

/// A screen reader reads a control's name every time it lands on it: the shortcut belongs in the
/// description (macOS AXHelp, read after a pause), not in the name.
#[test]
fn tool_strip_and_toolbar_buttons_are_named_by_label_with_the_shortcut_as_description() {
    let h = harness(ViewMode::Detail, RightPanel::Edit);
    for (name, help) in [
        ("Presets", "Shift+P"),
        ("Edit", "E"),
        ("Crop & Rotate", "C"),
        ("Masking", "M"),
        ("Versions", "Shift+V"),
        ("History & Activity", "Y"),
        ("Keywords", "K"),
        ("Info", "I"),
        ("Photo Grid", "G"),
        ("Square Grid", "G toggles"),
        ("Detail", "D"),
        ("Compare", "Shift+C"),
        ("Survey", "N"),
        ("Before/After", "Y"),
        ("Filmstrip", "/"),
    ] {
        assert!(button_described(&h, name, help), "a button named {name:?} describing {help:?}");
    }
    let leaked: Vec<String> = h
        .root()
        .children_recursive()
        .map(|n| n.accesskit_node())
        .filter(|n| n.role() == Role::Button)
        .filter_map(|n| n.label())
        .filter(|l| ["Edit (E)", "Presets (Shift+P)", "Photo Grid (G)", "Filmstrip (/)"].contains(&l.as_str()))
        .collect();
    assert!(leaked.is_empty(), "shortcuts left in names: {leaked:?}");
}

/// The top bar's Import button carries the key it is bound to now, written for this platform.
#[test]
fn import_button_describes_its_shortcut_as_this_platform_writes_it() {
    let h = harness(ViewMode::PhotoGrid, RightPanel::Edit);
    let mac = h.ctx.os() == egui::os::OperatingSystem::Mac;
    let sc = lightcraft_ui_egui::shortcuts::shortcut_of(&h.state().ui.settings.keymap, "file.addPhotos").expect("Import Photos… has a shortcut");
    let help = lightcraft_ui_egui::menubar::shortcut_text(sc, mac);
    assert!(button_described(&h, "Import Photos…", &help), "a button named \"Import Photos…\" describing {help:?}");
}
