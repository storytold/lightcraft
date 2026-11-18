//! Accessibility audit: walks the AccessKit tree egui exposes to screen readers (VoiceOver,
//! Narrator, AT-SPI) in each main view and Develop tool, headless, and fails on an interactive
//! node with no name, a slider without its value and range, or a canvas tool no key can reach.
use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::{Harness, kittest::NodeT};
use lcc_ui_egui::{
    LccApp, Services,
    state::{RightPanel, ViewMode},
};

fn harness(view: ViewMode, right: RightPanel) -> Harness<'static, LccApp> {
    lcc_engine::scene_disk::use_build_dir();
    let mut app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.left_panel = true;
    app.ui.view = view;
    app.ui.right = right;
    // the editing tools are Develop's (in the fork, Library's right column is its Classic panels)
    if matches!(right, RightPanel::Edit | RightPanel::Profiles | RightPanel::Crop | RightPanel::Remove | RightPanel::Masking | RightPanel::RedEye) {
        app.ui.module = lcc_ui_egui::module::ModuleId::Develop;
    }
    let mut h = Harness::builder().with_size(vec2(1500.0, 950.0)).build_ui_state(
        |ui, app: &mut LccApp| {
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
fn audit(h: &Harness<'_, LccApp>, place: &str) -> Vec<String> {
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
    let mut problems = Vec::new();
    // (one app per module, switched from view to view and panel to panel: a demo session per place cost 30 of them)
    let mut h = harness(ViewMode::PhotoGrid, RightPanel::None);
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid, ViewMode::Detail, ViewMode::Compare, ViewMode::Survey, ViewMode::Reference] {
        h.state_mut().ui.view = view;
        h.run_steps(10);
        problems.extend(audit(&h, &format!("{view:?}")));
    }
    h.state_mut().ui.view = ViewMode::Detail;
    for right in [RightPanel::Versions, RightPanel::Activity, RightPanel::Keywords, RightPanel::Info] {
        h.state_mut().ui.right = right;
        h.run_steps(10);
        problems.extend(audit(&h, &format!("Detail/{right:?}")));
    }
    let mut develop = harness(ViewMode::Detail, RightPanel::Edit);
    for right in [RightPanel::Edit, RightPanel::Profiles, RightPanel::Crop, RightPanel::Remove, RightPanel::Masking, RightPanel::RedEye] {
        develop.state_mut().ui.right = right;
        develop.run_steps(10);
        problems.extend(audit(&develop, &format!("Detail/{right:?}")));
    }
    drop(develop);
    h.state_mut().ui.view = ViewMode::PhotoGrid;
    h.state_mut().ui.right = RightPanel::None;
    h.run_steps(4);
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
        "dialog.watermark",
    ] {
        h.state_mut().ui.dialog = None;
        h.run_steps(2);
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

fn slider_value(h: &Harness<'_, LccApp>, name: &str) -> Option<f64> {
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
/// Tab alone, and the arrow keys then adjust them. In the Alternative keymap set: the Classic set
/// gives Tab to Hide/Show Side Panels, as Lightroom does.
#[test]
fn canvas_handles_are_named_and_tool_sliders_are_reachable_with_tab() {
    for right in [RightPanel::Crop, RightPanel::Remove] {
        let mut h = harness(ViewMode::Detail, right);
        // the canvas handles are in the tree by name, with their position as the value
        if right == RightPanel::Crop {
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
        } else {
            assert!(h.root().children_recursive().any(|n| n.accesskit_node().label().is_some_and(|l| l.starts_with("New spot"))));
        }
        h.state_mut().ui.settings.keymap_set = lcc_ui_egui::shortcuts::KeymapSet::Alternative;
        h.run_steps(2);
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

/// Tab walks each right panel from top to bottom (rows left to right): the order a sighted user
/// reads it in. Each step down may not jump back up by more than a row.
#[test]
fn tab_walks_the_right_panels_top_to_bottom() {
    for right in [RightPanel::Edit, RightPanel::Crop, RightPanel::Remove, RightPanel::Masking] {
        let mut h = harness(ViewMode::Detail, right);
        // the Classic keymap's Tab hides the side panels (and never moves the focus in the app): walk
        // with the Alternative set, where Tab is focus navigation
        h.state_mut().ui.settings.keymap_set = lcc_ui_egui::shortcuts::KeymapSet::Alternative;
        h.run_steps(2);
        // the panel's edges as drawn, not derived from the window width (it differs in the fork, which
        // has no tool strip beside the panel: the right panel folds from the top bar)
        let panel = h.state().widgets.iter().find(|(w, _)| w == "panel:right_panel").map(|(_, r)| *r).expect("a right panel");
        let strip = panel.right() + 1.0;
        let panel_left = panel.left() - 1.0;
        let mut seen: Vec<(String, f32)> = Vec::new();
        for _ in 0..120 {
            h.key_press(egui::Key::Tab);
            // (a control the focus scrolls into view is drawn there on the pass after)
            h.run_steps(2);
            let Some(n) = h.root().children_recursive().map(|n| n.accesskit_node()).find(|n| n.is_focused()) else { continue };
            let Some(b) = n.bounding_box() else { continue };
            if (b.x0 as f32) < panel_left || (b.x0 as f32) >= strip - 1.0 {
                continue;
            }
            let name = n.label().unwrap_or_default();
            // the focus is never left on a control scrolled out of view (the top bar above is not the panel's)
            let in_panel = b.y0 as f32 >= panel.top() - 1.0;
            assert!(!in_panel || b.y1 as f32 <= panel.bottom() + 1.0, "{right:?}: {name:?} has the focus out of view at {b:?}");
            // a tall control (a colour wheel) scrolls the panel to fit when it takes the focus, which moves every
            // position after it: it is checked for being in view above, but not for its place in the order
            if (b.y1 - b.y0) as f32 > 60.0 {
                continue;
            }
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
