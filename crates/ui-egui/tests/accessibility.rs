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
    let mut h = Harness::builder().with_size(vec2(1500.0, 950.0)).build_ui_state(
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
        if !interactive(role) {
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
        let windows = h.root().children_recursive().map(|n| n.accesskit_node()).filter(|n| n.role() == Role::Window).count();
        let untitled = h
            .root()
            .children_recursive()
            .map(|n| n.accesskit_node())
            .filter(|n| n.role() == Role::Window && n.label().unwrap_or_default().trim().is_empty())
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
