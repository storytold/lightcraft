//! Real library/tool panel moves preserve the selected tool and saved arrangement.
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
    let rect = h.state().ui.docking.as_ref().unwrap().layout.floating.iter().find(|g| g.panels.contains(&Panel::Tools)).unwrap().rect;
    h.get_by_role_and_label(egui::accesskit::Role::Tab, "Tools").click_button(PointerButton::Secondary);
    h.run_steps(3);
    h.get_by_label("Close panel").click();
    h.run_steps(4);
    assert_eq!(h.state().ui.right, RightPanel::None);
    h.state_mut().ui.right = RightPanel::Crop;
    h.run_steps(4);
    assert_eq!(h.state().ui.docking.as_ref().unwrap().layout.floating.iter().find(|g| g.panels.contains(&Panel::Tools)).unwrap().rect, rect);
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
                h.state().ui.docking.as_ref().unwrap().validate().unwrap();
                if let Ok(directory) = std::env::var("CRAFT_UI_DOCKING_DIR") {
                    std::fs::create_dir_all(&directory).unwrap();
                    h.render().unwrap().save(format!("{directory}/light-{width}-{scale}-{state}.png")).unwrap();
                }
            }
        }
    }
}
