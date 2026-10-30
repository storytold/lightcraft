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
            .find(|n| n.label().as_deref() == Some("Edit (E)"))
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

/// What a screen reader is told about the Settings tab strip: a tab list holding one tab per
/// section, the open one selected, and the open tab controlling the page under it. VoiceOver
/// read the tabs as check boxes (a button with a toggled state) before.
#[test]
fn the_settings_tab_strip_is_a_tab_list_of_tabs_that_control_their_page() {
    let tabs = lightcraft_ui_egui::panels::settings::TABS;
    let mut h = harness(ViewMode::PhotoGrid, RightPanel::None);
    h.state_mut().run("app.settings", serde_json::json!({"tab": "slideshow"})).expect("Settings opens");
    h.run_steps(8);
    let nodes: Vec<_> = h.root().children_recursive().map(|n| n.accesskit_node()).collect();
    // each node with its ancestors (id, role), nearest first
    let with_ancestors: Vec<_> = nodes
        .iter()
        .map(|n| {
            let mut up = n.parent();
            let mut chain = Vec::new();
            while let Some(p) = up {
                chain.push((p.id(), p.role()));
                up = p.parent();
            }
            (n, chain)
        })
        .collect();
    let lists: Vec<_> = nodes.iter().filter(|n| n.role() == Role::TabList).collect();
    assert_eq!(lists.len(), 1, "one tab list: {:?}", lists.iter().map(|n| n.label()).collect::<Vec<_>>());
    let list = lists[0];
    assert!(list.label().is_some_and(|l| !l.trim().is_empty()), "the tab list has a name");
    // one tab per section, in order, each under the list through nothing but plain containers
    // (macOS lists the tabs from the list's children, looking through those alone)
    let found: Vec<_> = with_ancestors
        .iter()
        .filter(|(n, chain)| {
            n.role() == Role::Tab
                && chain.iter().position(|(id, _)| *id == list.id()).is_some_and(|at| chain[..at].iter().all(|(_, r)| *r == Role::GenericContainer))
        })
        .map(|(n, _)| n)
        .collect();
    let names: Vec<String> = found.iter().map(|n| n.label().unwrap_or_default()).collect();
    let want: Vec<String> = tabs.iter().map(|(_, l)| lightcraft_ui_egui::i18n::tr(l).to_string()).collect();
    assert_eq!(names, want, "one tab per section");
    // the open one, and only it, is selected, and it is no toggle (a check box to VoiceOver)
    for (tab, (id, _)) in found.iter().zip(tabs) {
        assert_eq!(tab.is_selected(), Some(*id == "slideshow"), "{:?} selected", tab.label());
        assert!(tab.toggled().is_none(), "{:?} is no toggle", tab.label());
    }
    // the open tab, and no other, controls a tab panel named for it
    let open = found[tabs.iter().position(|(id, _)| *id == "slideshow").expect("a Slideshow tab")];
    let panel: Vec<_> = open.controls().collect();
    assert_eq!(panel.len(), 1, "the open tab controls its page");
    assert_eq!(panel[0].role(), Role::TabPanel);
    assert_eq!(panel[0].label(), open.label());
    assert!(found.iter().filter(|t| t.controls().next().is_some()).count() == 1, "only the open tab controls a page");
    // the page is in the panel: its controls, and Slideshow's slide preview pinned above them
    let in_panel = |name: &str| {
        with_ancestors
            .iter()
            .any(|(n, chain)| n.label().as_deref() == Some(name) && n.role() != Role::TabPanel && chain.iter().any(|(id, _)| *id == panel[0].id()))
    };
    assert!(in_panel("Intro"), "the preview's Photo / Intro / Ending choice is in the page's panel");
    assert!(
        with_ancestors.iter().filter(|(n, chain)| n.role() == Role::CheckBox && chain.iter().any(|(id, _)| *id == panel[0].id())).count() > 0,
        "the page's own controls are in the panel"
    );
}
