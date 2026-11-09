//! Real library/tool panel moves preserve the selected tool and saved arrangement.
use craft_ui::docking::{DockArea, Hit, Node, Side};
use egui::{Event, Modifiers, PointerButton, vec2};
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use lightcraft_ui_egui::{LightcraftApp, Services, UiState, docking::Panel, state::RightPanel};
use serde_json::json;
use std::time::{Duration, Instant};

fn harness(state: Option<UiState>, width: f32, scale: f32) -> Harness<'static, LightcraftApp> {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    // a headless host never gains the first-run tour: its modal would take the pointer the drags aim at
    app.headless_host = true;
    if let Some(state) = state {
        app.ui = state;
    } else {
        app.ui.left_panel = true;
        app.ui.right = RightPanel::Edit;
    }
    let mut h = Harness::builder().with_size(vec2(width, 900.0)).with_pixels_per_point(scale).build_ui_state(
        |ui, app: &mut LightcraftApp| {
            app.logic(&ui.ctx().clone());
            app.ui(ui);
        },
        app,
    );
    h.run_steps(8);
    h
}

fn drag(h: &mut Harness<'_, LightcraftApp>, from: egui::Pos2, to: egui::Pos2) {
    h.event(Event::PointerMoved(from));
    h.run_steps(2);
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(2);
    for step in 1..=8 {
        h.event(Event::PointerMoved(from.lerp(to, step as f32 / 8.0)));
        h.run_steps(1);
    }
    h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(4);
}

fn settle_photo(h: &mut Harness<'_, LightcraftApp>) {
    let started = Instant::now();
    let mut quiet = 0;
    while quiet < 4 {
        if h.state().renderer.in_flight_without_prefetch() > 0 {
            h.state_mut().renderer.wait_for_worker(Duration::from_millis(250));
        }
        h.run_steps(1);
        // Neighbouring-photo prefetch does not contribute pixels to this capture.
        if h.state().renderer.in_flight_without_prefetch() == 0 && h.state().loupe_shown.is_some_and(|(_, source)| source == "render") {
            quiet += 1;
        } else {
            quiet = 0;
        }
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "the synthetic photo and panel thumbnails must finish rendering before capture: loupe {:?}, pending {:?}",
            h.state().loupe_shown,
            h.state().renderer.pending_slots()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn real_drag_float_close_reopen_and_state_reload_keep_photo_tool_mode() {
    let mut h = harness(None, 1400.0, 1.0);
    let source = h.get_by_role_and_label(egui::accesskit::Role::Tab, "Tools").rect().center();
    let target = h.get_by_role_and_label(egui::accesskit::Role::Tab, "Library").rect().center();
    drag(&mut h, source, target);
    assert_eq!(h.state().ui.docking.as_ref().unwrap().layout.location(&Panel::Tools).unwrap().anchor, Some(Panel::Library));
    h.get_by_role_and_label(egui::accesskit::Role::Tab, "Tools").click_button(PointerButton::Secondary);
    h.run_steps(3);
    h.get_by_label("Float panel").click();
    h.run_steps(5);
    let rect = h.state().ui.docking.as_ref().unwrap().layout.floating.iter().find(|g| g.contains(&Panel::Tools)).unwrap().rect;
    h.get_by_role_and_label(egui::accesskit::Role::Tab, "Tools").click_button(PointerButton::Secondary);
    h.run_steps(3);
    h.get_by_label("Close panel").click();
    h.run_steps(4);
    assert_eq!(h.state().ui.right, RightPanel::None);
    h.state_mut().ui.right = RightPanel::Crop;
    h.run_steps(4);
    assert_eq!(h.state().ui.docking.as_ref().unwrap().layout.floating.iter().find(|g| g.contains(&Panel::Tools)).unwrap().rect, rect);
    let saved = serde_json::to_string(&h.state().ui).unwrap();
    let expected = serde_json::to_value(&h.state().ui.docking).unwrap();
    let mut reloaded = harness(Some(serde_json::from_str(&saved).unwrap()), 1400.0, 1.0);
    assert_eq!(serde_json::to_value(&reloaded.state().ui.docking).unwrap(), expected);
    assert_eq!(reloaded.state().ui.right, RightPanel::Crop);
    reloaded.state_mut().run("ui.dock", json!({"operation":"move","panel":"tools","target":"library","zone":"center"})).unwrap();
    reloaded.run_steps(4);
    assert_eq!(reloaded.state().ui.right, RightPanel::Crop);
    reloaded.state_mut().run("ui.dock", json!({"operation":"close","panel":"library"})).unwrap();
    reloaded.run_steps(3);
    assert!(!reloaded.state().ui.left_panel);
    assert_eq!(reloaded.state().ui.right, RightPanel::Crop);
}

#[test]
fn dragging_a_group_by_its_bar_moves_the_whole_group() {
    let mut h = harness(None, 1400.0, 1.0);
    h.state_mut().ui.presets = true;
    h.run_steps(4);
    h.state_mut().run("ui.dock", json!({"operation":"move","panel":"presets","target":"tools","zone":"center"})).unwrap();
    h.run_steps(6);
    let bar = DockArea::<Panel>::hit_rects(&h.ctx, egui::Id::unique("lightcraft-docking"))
        .into_iter()
        .find_map(|(hit, rect)| matches!(hit, Hit::DragBar(Panel::Tools | Panel::Presets)).then_some(rect))
        .expect("the Tools and Presets group draws its drag bar");
    let target = h.get_by_role_and_label(egui::accesskit::Role::Tab, "Library").rect().center();
    drag(&mut h, bar.center(), target);
    let mut library = start_dock(&h).into_iter().flatten().find(|panels| panels.contains(&Panel::Library)).unwrap();
    library.sort_by_key(|panel| panel.id());
    assert_eq!(library, [Panel::Library, Panel::Presets, Panel::Tools], "both panels of the dragged group joined Library");
    assert_eq!(h.state().ui.right, RightPanel::Edit);
}

/// The start dock's columns from the window edge inwards, each as its groups' panels.
fn start_dock(h: &Harness<'_, LightcraftApp>) -> Vec<Vec<Vec<Panel>>> {
    let layout = &h.state().ui.docking.as_ref().unwrap().layout;
    let mut nodes: Vec<_> = layout.root.iter().collect();
    while let Some(node) = nodes.pop() {
        match node {
            Node::Split { first, second, .. } => nodes.extend([&**first, &**second]),
            Node::Dock { side: Side::Start, columns } => {
                return columns.iter().map(|column| column.groups.iter().map(|group| group.panels.clone()).collect()).collect();
            }
            _ => {}
        }
    }
    Vec::new()
}

/// In a dock column, a group's top third stacks above it, its photo-side third adds a column
/// there and its middle joins its tabs.
#[test]
fn a_tab_dropped_on_a_dock_group_stacks_adds_a_column_or_joins_by_where_it_lands() {
    let cases: [(fn(egui::Rect) -> egui::Pos2, Vec<Vec<Vec<Panel>>>); 3] = [
        (|r| egui::pos2(r.center().x, r.top() + r.height() / 6.0), vec![vec![vec![Panel::Tools], vec![Panel::Library]]]),
        (|r| egui::pos2(r.right() - r.width() / 6.0, r.center().y), vec![vec![vec![Panel::Library]], vec![vec![Panel::Tools]]]),
        (|r| r.center(), vec![vec![vec![Panel::Library, Panel::Tools]]]),
    ];
    for (at, expected) in cases {
        let mut h = harness(None, 1400.0, 1.0);
        let library = h.state().widgets.iter().find(|(id, _)| id == "panel:left_panel").unwrap().1;
        let source = h.get_by_role_and_label(egui::accesskit::Role::Tab, "Tools").rect().center();
        drag(&mut h, source, at(library));
        assert_eq!(start_dock(&h), expected, "dropped at {:?} of {library:?}", at(library));
        lightcraft_ui_egui::docking::validate(h.state().ui.docking.as_ref().unwrap()).unwrap();
        assert_eq!(h.state().ui.right, RightPanel::Edit);
    }
}

#[test]
fn invalid_docking_requests_preserve_layout_and_selected_tool() {
    let mut h = harness(None, 1400.0, 1.0);
    let before = serde_json::to_value(&h.state().ui.docking).unwrap();
    for params in [
        json!({"operation":"close","panel":"canvas"}),
        json!({"operation":"move","panel":"tools","target":"canvas","zone":"center"}),
        json!({"operation":"float","panel":"tools","rect":[0,0,-1,100]}),
        json!({"operation":"move","panel":"unknown","target":"library","zone":"center"}),
    ] {
        assert!(h.state_mut().run("ui.dock", params).is_err());
        assert_eq!(serde_json::to_value(&h.state().ui.docking).unwrap(), before);
        assert_eq!(h.state().ui.right, RightPanel::Edit);
    }
}

#[test]
fn docking_renders_real_photo_panels_default_floating_and_redocked() {
    for width in [900.0, 1400.0] {
        for scale in [1.0, 2.0] {
            let mut h = harness(None, width, scale);
            for state in ["default", "floating", "redocked"] {
                if state == "floating" {
                    h.state_mut().run("ui.dock", json!({"operation":"float","panel":"tools","rect":[120,90,320,600]})).unwrap();
                } else if state == "redocked" {
                    h.state_mut().run("ui.dock", json!({"operation":"move","panel":"tools","target":"library","zone":"center"})).unwrap();
                }
                h.run_steps(8);
                settle_photo(&mut h);
                lightcraft_ui_egui::docking::validate(h.state().ui.docking.as_ref().unwrap()).unwrap();
                if let Ok(directory) = std::env::var("CRAFT_UI_DOCKING_DIR") {
                    std::fs::create_dir_all(&directory).unwrap();
                    h.render().unwrap().save(format!("{directory}/light-{width}-{scale}-{state}.png")).unwrap();
                }
            }
        }
    }
}

#[test]
fn tool_rail_menu_closes_a_collapsed_panel_without_closing_presets() {
    let mut h = harness(None, 1400.0, 1.0);
    h.state_mut().ui.presets = true;
    h.run_steps(4);
    h.state_mut().run("ui.dock", json!({"action": {"SetIconic": {"panel": "tools", "iconic": true}}})).unwrap();
    h.run_steps(4);
    let before = serde_json::to_value(h.state().session.catalog.photos().map(|p| p.as_ref()).collect::<Vec<_>>()).unwrap();
    let undo = h.state().session.undo.len();
    h.get_by_role_and_label(egui::accesskit::Role::Toolbar, "Tool strip")
        .get_by_role_and_label(egui::accesskit::Role::Button, "Edit")
        .click_button(PointerButton::Secondary);
    h.run_steps(3);
    h.get_by_label("Close panel").click();
    h.run_steps(5);
    assert_eq!(h.state().ui.right, RightPanel::None);
    assert!(h.state().ui.presets);
    assert!(!h.state().ui.docking.as_ref().unwrap().layout.contains(&Panel::Tools));
    assert_eq!(h.state().session.undo.len(), undo);
    assert_eq!(serde_json::to_value(h.state().session.catalog.photos().map(|p| p.as_ref()).collect::<Vec<_>>()).unwrap(), before);
}

#[test]
fn tool_rail_menu_groups_panels_and_close_keeps_the_other_tab_across_reload() {
    let mut h = harness(None, 1400.0, 1.0);
    h.state_mut().ui.presets = true;
    h.run_steps(4);
    h.get_by_role_and_label(egui::accesskit::Role::Toolbar, "Tool strip")
        .get_by_role_and_label(egui::accesskit::Role::Button, "Edit")
        .click_button(PointerButton::Secondary);
    h.run_steps(3);
    h.get_by_label("Move to group     ").hover();
    h.run_steps(5);
    h.get_by_label("Move to group     ")
        .get_by_role(egui::accesskit::Role::Menu)
        .get_by_role_and_label(egui::accesskit::Role::Button, "Presets")
        .click();
    h.run_steps(5);
    let workspace = h.state().ui.docking.as_ref().unwrap();
    assert_eq!(workspace.layout.location(&Panel::Tools).unwrap().anchor, Some(Panel::Presets));
    assert_eq!(h.state().ui.right, RightPanel::Edit);
    h.get_by_role_and_label(egui::accesskit::Role::Toolbar, "Tool strip")
        .get_by_role_and_label(egui::accesskit::Role::Button, "Presets")
        .click_button(PointerButton::Secondary);
    h.run_steps(3);
    h.get_by_label("Close panel").click();
    h.run_steps(5);
    assert!(!h.state().ui.presets);
    assert_eq!(h.state().ui.right, RightPanel::Edit);
    let state: UiState = serde_json::from_str(&serde_json::to_string(&h.state().ui).unwrap()).unwrap();
    let mut reloaded = harness(Some(state), 1400.0, 1.0);
    reloaded.state_mut().run("panel.presets", json!({})).unwrap();
    reloaded.run_steps(4);
    assert_eq!(reloaded.state().ui.docking.as_ref().unwrap().layout.location(&Panel::Presets).unwrap().anchor, Some(Panel::Tools));
}

#[test]
fn tool_rail_menu_respects_locked_workspaces_and_missing_group_targets() {
    for (source, label) in [(Panel::Tools, "Edit"), (Panel::Presets, "Presets")] {
        for (target_open, locked) in [(true, false), (true, true), (false, false)] {
            let mut h = harness(None, 1400.0, 1.0);
            h.state_mut().ui.presets = source == Panel::Presets || target_open;
            h.state_mut().ui.right = if source == Panel::Tools || target_open { RightPanel::Edit } else { RightPanel::None };
            h.run_steps(4);
            h.state_mut().ui.docking.as_mut().unwrap().locked = locked;
            h.run_steps(3);
            let before = serde_json::to_value(&h.state().ui.docking).unwrap();
            h.get_by_role_and_label(egui::accesskit::Role::Toolbar, "Tool strip")
                .get_by_role_and_label(egui::accesskit::Role::Button, label)
                .click_button(PointerButton::Secondary);
            h.run_steps(3);
            assert_eq!(h.get_by_label("Close panel").accesskit_node().is_disabled(), locked, "{source:?}, target open={target_open}");
            assert_eq!(
                h.get_by_label("Move to group     ").accesskit_node().is_disabled(),
                locked || !target_open,
                "{source:?}, target open={target_open}, locked={locked}"
            );
            assert_eq!(serde_json::to_value(&h.state().ui.docking).unwrap(), before);
        }
    }
}

#[test]
fn tool_rail_menu_moves_presets_into_tools_and_reopens_it_there_after_reload() {
    let mut h = harness(None, 1400.0, 1.0);
    h.state_mut().ui.presets = true;
    h.run_steps(4);
    h.get_by_role_and_label(egui::accesskit::Role::Toolbar, "Tool strip")
        .get_by_role_and_label(egui::accesskit::Role::Button, "Presets")
        .click_button(PointerButton::Secondary);
    h.run_steps(3);
    h.get_by_label("Move to group     ").hover();
    h.run_steps(5);
    h.get_by_label("Move to group     ")
        .get_by_role(egui::accesskit::Role::Menu)
        .get_by_role_and_label(egui::accesskit::Role::Button, "Tools")
        .click();
    h.run_steps(5);
    assert_eq!(h.state().ui.docking.as_ref().unwrap().layout.location(&Panel::Presets).unwrap().anchor, Some(Panel::Tools));
    assert!(h.state().ui.presets);
    assert_eq!(h.state().ui.right, RightPanel::Edit);
    let expected = serde_json::to_value(&h.state().ui.docking).unwrap();
    let state: UiState = serde_json::from_str(&serde_json::to_string(&h.state().ui).unwrap()).unwrap();
    let mut reloaded = harness(Some(state), 1400.0, 1.0);
    assert_eq!(serde_json::to_value(&reloaded.state().ui.docking).unwrap(), expected);
    reloaded
        .get_by_role_and_label(egui::accesskit::Role::Toolbar, "Tool strip")
        .get_by_role_and_label(egui::accesskit::Role::Button, "Presets")
        .click_button(PointerButton::Secondary);
    reloaded.run_steps(3);
    reloaded.get_by_label("Close panel").click();
    reloaded.run_steps(4);
    assert!(!reloaded.state().ui.presets);
    assert_eq!(reloaded.state().ui.right, RightPanel::Edit);
    reloaded
        .get_by_role_and_label(egui::accesskit::Role::Toolbar, "Tool strip")
        .get_by_role_and_label(egui::accesskit::Role::Button, "Presets")
        .click();
    reloaded.run_steps(4);
    assert!(reloaded.state().ui.presets);
    assert_eq!(reloaded.state().ui.right, RightPanel::Edit);
    assert_eq!(reloaded.state().ui.docking.as_ref().unwrap().layout.location(&Panel::Presets).unwrap().anchor, Some(Panel::Tools));
}

#[test]
fn tool_rail_menu_does_not_dispatch_close_for_either_closed_panel() {
    for (panel, label) in [(Panel::Tools, "Edit"), (Panel::Presets, "Presets")] {
        let mut h = harness(None, 1400.0, 1.0);
        h.state_mut().ui.presets = true;
        h.run_steps(4);
        let close = json!({"operation": "close", "panel": panel});
        h.state_mut().run("ui.dock", close.clone()).unwrap();
        h.run_steps(4);
        // A dispatched close of this absent panel is an error, which panel_menu would toast.
        // Together with unchanged layout, no toast detects a dispatch even when it cannot close anything.
        assert!(h.state_mut().run("ui.dock", close).is_err());
        h.state_mut().ui.toast = None;
        let workspace = serde_json::to_value(&h.state().ui.docking).unwrap();
        let modes = (h.state().ui.right, h.state().ui.presets);
        let photos = serde_json::to_value(h.state().session.catalog.photos().map(|p| p.as_ref()).collect::<Vec<_>>()).unwrap();
        let undo = h.state().session.undo.len();
        h.get_by_role_and_label(egui::accesskit::Role::Toolbar, "Tool strip")
            .get_by_role_and_label(egui::accesskit::Role::Button, label)
            .click_button(PointerButton::Secondary);
        h.run_steps(3);
        assert!(h.get_by_label("Close panel").accesskit_node().is_disabled());
        assert!(h.get_by_label("Move to group     ").accesskit_node().is_disabled());
        h.get_by_label("Close panel").click();
        h.run_steps(4);
        assert_eq!(serde_json::to_value(&h.state().ui.docking).unwrap(), workspace);
        assert_eq!((h.state().ui.right, h.state().ui.presets), modes);
        assert_eq!(serde_json::to_value(h.state().session.catalog.photos().map(|p| p.as_ref()).collect::<Vec<_>>()).unwrap(), photos);
        assert_eq!(h.state().session.undo.len(), undo);
        assert!(h.state().ui.toast.is_none(), "{panel:?}: disabled Close must not dispatch ui.dock");
    }
}

#[test]
fn tool_rail_menu_can_close_open_presets_without_a_selected_photo() {
    let mut h = harness(None, 1400.0, 1.0);
    h.state_mut().ui.presets = true;
    h.state_mut().run("library.select", json!({"ids": []})).unwrap();
    h.run_steps(4);
    assert!(h.state().session.active().is_none());
    h.get_by_role_and_label(egui::accesskit::Role::Toolbar, "Tool strip")
        .get_by_role_and_label(egui::accesskit::Role::Button, "Presets")
        .click_button(PointerButton::Secondary);
    h.run_steps(3);
    h.get_by_label("Close panel").click();
    h.run_steps(4);
    assert!(!h.state().ui.presets);
    assert_eq!(h.state().ui.right, RightPanel::Edit);
    h.get_by_role_and_label(egui::accesskit::Role::Toolbar, "Tool strip")
        .get_by_role_and_label(egui::accesskit::Role::Button, "Edit")
        .click_button(PointerButton::Secondary);
    h.run_steps(3);
    h.get_by_label("Close panel").click();
    h.run_steps(4);
    assert_eq!(h.state().ui.right, RightPanel::None);
}

fn primary_click_harness(mode: RightPanel, selected: bool, locked: bool) -> Harness<'static, LightcraftApp> {
    let mut h = harness(None, 1400.0, 1.0);
    h.state_mut().ui.presets = true;
    h.state_mut().ui.right = mode;
    h.state_mut().run("library.select", json!({"ids": if selected { vec![21] } else { vec![] }})).unwrap();
    h.run_steps(4);
    h.state_mut().run("ui.dock", json!({"operation": "float", "panel": "library", "rect": [80.0, 90.0, 320.0, 500.0]})).unwrap();
    let workspace = h.state_mut().ui.docking.as_mut().unwrap();
    workspace.saved = Some(workspace.layout.clone());
    workspace.locked = locked;
    h.run_steps(3);
    h.state_mut().ui.toast = None;
    h
}

#[test]
fn tool_rail_primary_click_presets_preserves_a_locked_workspace() {
    for selected in [false, true] {
        for locked in [true, false] {
            let mut h = primary_click_harness(RightPanel::Edit, selected, locked);
            let before = h.state().ui.docking.clone();
            h.get_by_role_and_label(egui::accesskit::Role::Toolbar, "Tool strip")
                .get_by_role_and_label(egui::accesskit::Role::Button, "Presets")
                .click();
            h.run_steps(4);
            assert_eq!(h.state().ui.presets, locked, "selected={selected}, locked={locked}");
            assert_eq!(h.state().ui.right, RightPanel::Edit);
            if locked {
                assert_eq!(h.state().ui.docking, before, "a refused close must not reset the saved arrangement or unlock it");
                assert!(h.state().ui.toast.is_none(), "no layout-recovery error");
            }
        }
    }
}

#[test]
fn tool_rail_primary_click_tools_preserves_a_locked_workspace() {
    for (mode, label) in [
        (RightPanel::Edit, "Edit"),
        (RightPanel::Crop, "Crop & Rotate"),
        (RightPanel::Remove, "Remove"),
        (RightPanel::Masking, "Masking"),
        (RightPanel::RedEye, "Red Eye"),
        (RightPanel::Versions, "Versions"),
        (RightPanel::Activity, "History & Activity"),
        (RightPanel::Keywords, "Keywords"),
        (RightPanel::Info, "Info"),
    ] {
        for selected in [false, true] {
            for locked in [true, false] {
                let mut h = primary_click_harness(mode, selected, locked);
                let before = h.state().ui.docking.clone();
                let photos = serde_json::to_value(h.state().session.catalog.photos().map(|p| p.as_ref()).collect::<Vec<_>>()).unwrap();
                let undo = h.state().session.undo.len();
                h.get_by_role_and_label(egui::accesskit::Role::Toolbar, "Tool strip")
                    .get_by_role_and_label(egui::accesskit::Role::Button, label)
                    .click();
                h.run_steps(4);
                assert_eq!(h.state().ui.right, if locked { mode } else { RightPanel::None }, "{mode:?}, selected={selected}, locked={locked}");
                assert!(h.state().ui.presets);
                if locked {
                    assert_eq!(h.state().ui.docking, before, "{mode:?}: preserve the arrangement and lock");
                    assert!(h.state().ui.toast.is_none(), "{mode:?}: no layout-recovery error or false Off toast");
                    assert_eq!(h.state().session.undo.len(), undo);
                    assert_eq!(serde_json::to_value(h.state().session.catalog.photos().map(|p| p.as_ref()).collect::<Vec<_>>()).unwrap(), photos);
                }
            }
        }
    }
}

#[test]
fn tool_rail_primary_click_can_open_and_activate_panels_while_locked() {
    for (panel, label) in [(Panel::Presets, "Presets"), (Panel::Tools, "Edit")] {
        let mut h = primary_click_harness(RightPanel::Edit, true, false);
        let mut expected = h.state().ui.docking.clone().unwrap();
        expected.locked = true;
        h.state_mut().run("ui.dock", json!({"operation": "close", "panel": panel})).unwrap();
        h.state_mut().ui.docking.as_mut().unwrap().locked = true;
        h.run_steps(3);
        h.get_by_role_and_label(egui::accesskit::Role::Toolbar, "Tool strip").get_by_role_and_label(egui::accesskit::Role::Button, label).click();
        h.run_steps(4);
        assert_eq!(h.state().ui.docking.as_ref().unwrap(), &expected, "{panel:?}: opening is allowed while locked");
    }
    let mut h = primary_click_harness(RightPanel::Edit, true, false);
    h.state_mut().run("ui.dock", json!({"operation": "move", "panel": "presets", "target": "tools", "zone": "center"})).unwrap();
    h.state_mut().ui.docking.as_mut().unwrap().locked = true;
    h.run_steps(3);
    for (label, panel) in [("Edit", Panel::Tools), ("Presets", Panel::Presets)] {
        h.get_by_role_and_label(egui::accesskit::Role::Toolbar, "Tool strip").get_by_role_and_label(egui::accesskit::Role::Button, label).click();
        h.run_steps(4);
        let workspace = h.state().ui.docking.as_ref().unwrap();
        assert!(workspace.locked);
        assert!(lightcraft_ui_egui::docking::exposed_in(workspace, panel));
        assert_eq!(workspace.layout.location(&Panel::Presets).unwrap().anchor, Some(Panel::Tools));
        assert_eq!(workspace.layout.floating.len(), 1);
        assert!(h.state().ui.presets);
        assert_eq!(h.state().ui.right, RightPanel::Edit);
    }
}

#[test]
fn tool_rail_primary_click_can_change_the_tool_while_locked() {
    for selected in [false, true] {
        let mut h = primary_click_harness(RightPanel::Profiles, selected, true);
        let before = h.state().ui.docking.clone();
        // Edit is highlighted for Profiles too, but this click changes mode instead of closing Tools.
        h.get_by_role_and_label(egui::accesskit::Role::Toolbar, "Tool strip").get_by_role_and_label(egui::accesskit::Role::Button, "Edit").click();
        h.run_steps(4);
        assert_eq!(h.state().ui.right, RightPanel::Edit);
        assert_eq!(h.state().ui.docking, before);
        if selected {
            h.get_by_role_and_label(egui::accesskit::Role::Toolbar, "Tool strip")
                .get_by_role_and_label(egui::accesskit::Role::Button, "Crop & Rotate")
                .click();
            h.run_steps(4);
            assert_eq!(h.state().ui.right, RightPanel::Crop);
            assert_eq!(h.state().ui.docking, before);
        }
    }
}
