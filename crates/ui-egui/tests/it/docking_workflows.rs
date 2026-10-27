//! Real library/tool panel moves preserve the selected tool and saved arrangement.
use craft_ui::docking::{DockArea, Hit, Node, Side};
use egui::{Event, Modifiers, PointerButton, vec2};
use egui_kittest::{Harness, kittest::Queryable};
use lightcraft_ui_egui::{LightcraftApp, Services, UiState, docking::Panel, state::RightPanel};
use serde_json::json;
use std::time::{Duration, Instant};

fn harness(state: Option<UiState>, width: f32, scale: f32) -> Harness<'static, LightcraftApp> {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
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
        h.run_steps(2);
        if h.state().renderer.in_flight() == 0 && h.state().loupe_shown.is_some_and(|(_, source)| source == "render") {
            quiet += 1;
        } else {
            quiet = 0;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "the synthetic photo and panel thumbnails must finish rendering before capture");
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
