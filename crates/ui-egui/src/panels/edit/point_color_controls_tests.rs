//! In-memory SDK seam tests; no chooser, image decoding, renderer or filesystem access.
use super::*;
use crate::headless::HeadlessView;
use crate::widgets::{WidgetHit, take_registries};
use egui::{Event, Key, Modifiers, PointerButton, Rect};
use lightcraft_catalog::{Op, Photo, Source};
use lightcraft_develop::PointColor;
use std::sync::Arc;

type Registries = (Vec<(String, Rect)>, Vec<(String, WidgetHit)>);

fn fixture(samples: Vec<PointColor>) -> (egui::Context, LightcraftApp) {
    let mut session = lightcraft_engine::Session::new();
    let id = session.catalog.alloc_photo_id();
    let mut photo = Photo::new(id, Source::Demo { scene: 0 }, "memory.jpg", "JPEG", 8, 8, "2026-01-01T00:00:00");
    Arc::make_mut(&mut photo.develop).point_colors = samples;
    session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    session.execute("library.select", &json!({"ids": [id.0], "active": id.0})).unwrap();
    let app = LightcraftApp::new(session, crate::Services::default());
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    app.install_framework(&ctx);
    crate::theme::apply(&ctx);
    ctx.enable_accesskit();
    (ctx, app)
}

fn frame(ctx: &egui::Context, events: Vec<Event>, mut body: impl FnMut(&mut egui::Ui)) -> Registries {
    let time = ctx.input(|i| i.time) + 1.0 / 60.0;
    let input = HeadlessView::raw_input(vec2(440.0, 1000.0), 1.0, time, events);
    let mut out = ctx.run_ui(input, |ui| {
        take_registries(ui.ctx());
        body(ui);
    });
    out.textures_delta.clear();
    take_registries(ctx)
}

fn panel(ctx: &egui::Context, app: &mut LightcraftApp, events: Vec<Event>) -> Registries {
    frame(ctx, events, |ui| {
        let d = app.session.develop_of(app.session.active().unwrap()).unwrap();
        point_color(app, ui, &d);
    })
}

fn settle(ctx: &egui::Context, app: &mut LightcraftApp) -> Registries {
    let mut reg = (Vec::new(), Vec::new());
    for _ in 0..4 {
        reg = panel(ctx, app, Vec::new());
    }
    reg
}

fn hit(reg: &Registries, name: &str) -> WidgetHit {
    assert_eq!(reg.0.iter().filter(|(n, _)| n == name).count(), 1, "one rect for {name}");
    assert_eq!(reg.1.iter().filter(|(n, _)| n == name).count(), 1, "one real owner for {name}");
    reg.1.iter().find(|(n, _)| n == name).unwrap().1
}

fn click(ctx: &egui::Context, app: &mut LightcraftApp, reg: &Registries, name: &str) {
    let at = hit(reg, name).interact_rect.center();
    panel(ctx, app, vec![Event::PointerMoved(at)]);
    for pressed in [true, false] {
        panel(ctx, app, vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
    }
    panel(ctx, app, Vec::new());
}

/// The keyboard's owner: the framework's focus record (a group member's stop), else egui's focus.
fn focus(ctx: &egui::Context) -> Option<egui::Id> {
    craft::ui::runtime::focus::focused_rec(ctx).map(|record| record.stop.id).or_else(|| ctx.memory(|m| m.focused()))
}

/// Asks the framework for the focus of `id` (a swatch is a member of the samples' group, with no egui widget of its own).
fn request_focus(ctx: &egui::Context, id: egui::Id) {
    craft::ui::runtime::focus::request(ctx, craft::ui::runtime::focus::FocusTarget::Id(id));
}

fn key(pressed: bool, key: Key) -> Event {
    Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }
}

fn settings(app: &LightcraftApp) -> Arc<DevelopSettings> {
    app.session.develop_of(app.session.active().unwrap()).unwrap()
}

fn model(app: &LightcraftApp) -> (serde_json::Value, usize, usize, u64) {
    (serde_json::to_value(&*settings(app)).unwrap(), app.session.undo.len(), app.session.redo.len(), app.session.commits())
}

#[test]
fn point_controls_equal_colors_have_distinct_sdk_ids_and_real_response_owners() {
    let p = PointColor::default();
    let (ctx, mut app) = fixture(vec![p, p, PointColor { hue: 235.0, ..p }]);
    app.ui.point_color = 1;
    let mut reg = settle(&ctx, &mut app);
    let mut observed = None;
    for _ in 0..4 {
        reg = frame(&ctx, Vec::new(), |ui| {
            let d = settings(&app);
            let (_, recorded) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("point-controls"), |ui| point_color(&mut app, ui, &d));
            observed = recorded;
        });
    }
    let record = observed.expect("real SDK region");
    let mut ids = Vec::new();
    for i in 0..3 {
        let name = format!("pointColor:{i}");
        let label = format!("{} {}", crate::i18n::tr("Point Color"), i + 1);
        let owner = hit(&reg, &name);
        let e = record
            .elements
            .iter()
            // a member of the samples' radio group: a radio button that still carries its colour
            .find(|e| e.facts.as_ref().is_some_and(|f| f.name.as_deref() == Some(label.as_str()) && f.role == "RadioButton"))
            .expect("SDK swatch radio");
        assert_eq!(owner.id, e.id, "not a hover automation probe");
        // A group member has no egui widget of its own: its sensing area is the framework's, recorded for it.
        let facts = e.facts.as_ref().unwrap();
        assert!(facts.interactive && facts.actions.contains(&"click") && facts.actions.contains(&"focus"), "really clickable and focusable");
        let areas: Vec<_> = record.hits.iter().filter(|area| area.owner == owner.id && area.part.is_none()).collect();
        assert_eq!(areas.len(), 1, "one actual SDK sensing area");
        assert_eq!(areas[0].rect, owner.interact_rect, "the locator is the real sensing area");
        assert_eq!(
            e.facts.as_ref().unwrap().value.as_deref(),
            Some(point_color_srgb(oklch_color(p.lum, p.chroma, if i == 2 { 235.0 } else { p.hue })).to_hex().as_str())
        );
        ids.push(owner.id);
    }
    assert_ne!(ids[0], ids[1], "equal-color samples must remain different controls");
    assert_ne!(ids[1], ids[2]);
    let checkbox = hit(&reg, "pointColorVisualize");
    let e = record
        .elements
        .iter()
        .find(|e| e.facts.as_ref().is_some_and(|f| f.role == "CheckBox" && f.name.as_deref() == Some(crate::i18n::tr("Visualize range"))))
        .expect("SDK Checkbox");
    assert_eq!(checkbox.id, e.id);
}

#[test]
fn point_controls_pointer_and_keyboard_change_only_ui_state_never_history() {
    let p = PointColor::default();
    let (ctx, mut app) = fixture(vec![p, p]);
    let before = model(&app);
    let mut reg = settle(&ctx, &mut app);
    for index in [1, 0, 1] {
        click(&ctx, &mut app, &reg, &format!("pointColor:{index}"));
        assert_eq!(app.ui.point_color, index);
        assert_eq!(model(&app), before);
        assert!(app.ui.tool.is_empty(), "a sample must not activate the photo picker");
        assert!(!egui::Popup::is_any_open(&ctx));
        reg = settle(&ctx, &mut app);
    }
    for expected in [true, false] {
        click(&ctx, &mut app, &reg, "pointColorVisualize");
        assert_eq!(app.ui.point_color_visualize, expected);
        assert_eq!(model(&app), before);
        reg = settle(&ctx, &mut app);
    }
    // Start from the actual focus after the pointer interactions, not a forced focus request. The samples are one
    // radio group, one Tab stop (the chosen sample): the arrows move between them.
    for (name, travel, key_value) in [
        ("pointColor:0", Some(Key::ArrowLeft), Key::Enter),
        ("pointColor:1", Some(Key::ArrowRight), Key::Space),
        ("pointColorVisualize", None, Key::Space),
    ] {
        let owner = hit(&reg, name);
        // Tab reaches the control (for a sample: the group's stop)
        let stops: Vec<egui::Id> =
            if travel.is_some() { ["pointColor:0", "pointColor:1"].iter().map(|n| hit(&reg, n).id).collect() } else { vec![owner.id] };
        let mut visited = std::collections::HashSet::new();
        for _ in 0..=reg.1.len() + 2 {
            if focus(&ctx).is_some_and(|id| stops.contains(&id)) {
                break;
            }
            for pressed in [true, false] {
                panel(&ctx, &mut app, vec![key(pressed, Key::Tab)]);
            }
            if let Some(id) = focus(&ctx) {
                if stops.contains(&id) {
                    break;
                }
                assert!(visited.insert(id), "Tab cycled before reaching {name}");
            }
        }
        // then the arrow moves to the sample, unless the keyboard is on it already
        if let Some(arrow) = travel
            && focus(&ctx) != Some(owner.id)
        {
            for pressed in [true, false] {
                panel(&ctx, &mut app, vec![key(pressed, arrow)]);
            }
        }
        assert_eq!(focus(&ctx), Some(owner.id), "actual keyboard owner for {name}");
        panel(&ctx, &mut app, vec![key(true, key_value)]);
        panel(&ctx, &mut app, vec![key(false, key_value)]);
        assert_eq!(model(&app), before);
        reg = settle(&ctx, &mut app);
        match name {
            "pointColor:0" => assert_eq!(app.ui.point_color, 0),
            "pointColor:1" => assert_eq!(app.ui.point_color, 1),
            _ => assert!(app.ui.point_color_visualize),
        }
    }
}

#[test]
fn point_controls_both_sizing_modes_publish_no_targets_or_actions_even_with_focused_input() {
    let (ctx, _app) = fixture(Vec::new());
    let (mut visualize, mut clicked) = (false, false);
    let mut reg = (Vec::new(), Vec::new());
    for _ in 0..4 {
        reg = frame(&ctx, Vec::new(), |ui| {
            clicked |= point_color_swatches(ui, &[Color32::RED], 0).is_some();
            point_color_visualize(ui, &mut visualize);
        });
    }
    for native in [true, false] {
        for name in ["pointColor:0", "pointColorVisualize"] {
            request_focus(&ctx, hit(&reg, name).id);
            for pressed in [true, false] {
                frame(&ctx, vec![key(pressed, Key::Space)], |ui| {
                    take_registries(&ctx);
                    let mut body = |ui: &mut egui::Ui| {
                        clicked |= point_color_swatches(ui, &[Color32::RED], 0).is_some();
                        point_color_visualize(ui, &mut visualize);
                    };
                    if native {
                        let mut child = ui.new_child(egui::UiBuilder::new().sizing_pass().invisible());
                        assert!(child.is_sizing_pass());
                        body(&mut child);
                    } else {
                        craft::ui::runtime::measure::run(&ctx, || body(ui));
                    }
                    let (rects, hits) = take_registries(&ctx);
                    assert!(rects.is_empty() && hits.is_empty());
                });
                assert!(!clicked && !visualize, "no speculative action native={native}");
            }
        }
    }
}

#[test]
fn point_controls_disabled_components_ignore_pointer_and_keys() {
    let (ctx, _app) = fixture(Vec::new());
    let (mut visualize, mut clicked) = (false, false);
    let mut draw = |ui: &mut egui::Ui| {
        ui.add_enabled_ui(false, |ui| {
            clicked |= point_color_swatches(ui, &[Color32::RED], 0).is_some();
            point_color_visualize(ui, &mut visualize);
        });
    };
    let mut reg = (Vec::new(), Vec::new());
    for _ in 0..4 {
        reg = frame(&ctx, Vec::new(), &mut draw);
    }
    for name in ["pointColor:0", "pointColorVisualize"] {
        let h = hit(&reg, name);
        let at = h.interact_rect.center();
        frame(&ctx, vec![Event::PointerMoved(at)], &mut draw);
        for pressed in [true, false] {
            frame(&ctx, vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }], &mut draw);
        }
        request_focus(&ctx, h.id);
        for pressed in [true, false] {
            frame(&ctx, vec![key(pressed, Key::Space)], &mut draw);
        }
    }
    drop(draw);
    assert!(!clicked && !visualize);
}

#[test]
fn point_controls_srgb_adapter_preserves_every_encoded_byte_and_oklch_result() {
    for byte in 0..=255 {
        let c = Color32::from_rgb(byte, 255 - byte, byte / 2);
        assert_eq!(point_color_srgb(c).to_rgba().0, [c.r(), c.g(), c.b(), 255]);
    }
    for (l, c, h) in [(0.0, 0.0, 0.0), (1.0, 0.0, 0.0), (0.5, 0.1, 0.0), (0.7, 0.37, 235.0), (0.6, 0.25, 360.0), (0.4, 0.4, -45.0)] {
        let old = oklch_color(l, c, h);
        assert_eq!(point_color_srgb(old).to_rgba().0, [old.r(), old.g(), old.b(), 255]);
    }
}

#[test]
fn point_controls_empty_state_and_post_delete_clamping_are_unchanged() {
    let (ctx, mut app) = fixture(Vec::new());
    let before = model(&app);
    let reg = settle(&ctx, &mut app);
    assert!(!reg.0.iter().any(|(n, _)| n.starts_with("pointColor:") || n == "pointColorVisualize"));
    assert_eq!(model(&app), before);
    let p = PointColor::default();
    let (ctx, mut app) = fixture(vec![p, p, p]);
    app.ui.point_color = 2;
    app.run("pointColor.delete", json!({"index": 2})).unwrap();
    let before = model(&app);
    let reg = settle(&ctx, &mut app);
    assert_eq!(app.ui.point_color, 1);
    assert_eq!(model(&app), before, "render clamping adds no edit/history entry");
    hit(&reg, "pointColor:1");
    assert!(!reg.0.iter().any(|(n, _)| n == "pointColor:2"));
}

#[test]
fn point_controls_selected_cue_moves_between_equal_color_samples() {
    let p = PointColor::default();
    let (ctx, mut app) = fixture(vec![p, p]);
    let mut nodes = std::collections::HashMap::new();
    for selected in [0, 1, 0] {
        app.ui.point_color = selected;
        for _ in 0..4 {
            let raw = HeadlessView::raw_input(vec2(440.0, 1000.0), 1.0, ctx.input(|i| i.time) + 0.1, vec![]);
            let mut output = ctx.run_ui(raw, |ui| {
                let d = settings(&app);
                point_color(&mut app, ui, &d);
            });
            output.textures_delta.clear();
            if let Some(update) = output.platform_output.accesskit_update {
                for (_, node) in update.nodes {
                    if let Some(label) = node.label() {
                        nodes.insert(label.to_owned(), node);
                    }
                }
            }
            take_registries(&ctx);
        }
        for i in 0..2 {
            let label = format!("{} {}", crate::i18n::tr("Point Color"), i + 1);
            // a radio reports its check as toggled
            let on = nodes.get(&label).expect("named swatch").toggled() == Some(egui::accesskit::Toggled::True);
            assert_eq!(on, i == selected);
        }
    }
}

#[test]
fn point_controls_production_sizing_preserves_ui_and_model_and_hides_only_migrated_targets() {
    let p = PointColor::default();
    let (ctx, mut app) = fixture(vec![p, p]);
    app.ui.point_color = 3; // stale index after the sample list contracted
    app.ui.point_color_visualize = true;
    let before = model(&app);
    frame(&ctx, Vec::new(), |ui| {
        for native in [true, false] {
            take_registries(&ctx);
            let d = settings(&app);
            if native {
                let mut child = ui.new_child(egui::UiBuilder::new().sizing_pass().invisible());
                point_color(&mut app, &mut child, &d);
            } else {
                craft::ui::runtime::measure::run(&ctx, || point_color(&mut app, ui, &d));
            }
            let (rects, hits) = take_registries(&ctx);
            for name in ["pointColor:0", "pointColor:1", "pointColorVisualize"] {
                assert!(!rects.iter().any(|(n, _)| n == name));
                assert!(!hits.iter().any(|(n, _)| n == name));
            }
            assert_eq!(app.ui.point_color, 3, "speculative layout must not commit recovery");
            assert!(app.ui.point_color_visualize);
            assert_eq!(model(&app), before);
        }
    });
    let recovered = settle(&ctx, &mut app);
    hit(&recovered, "pointColor:1");
    assert_eq!(app.ui.point_color, 1);
    assert!(app.ui.point_color_visualize);
    assert_eq!(model(&app), before);
}

#[test]
fn point_color_select_equal_swatch_preview_delete_target_and_undo_on_one_panel() {
    let first = PointColor::default();
    // Same displayed sample; distinct edit data proves which one Delete removed.
    let second = PointColor { hue_shift: 19.0, ..first };
    let (ctx, mut app) = fixture(vec![first, second]);
    let before = model(&app);
    let active = app.session.active().unwrap();
    let selection = app.session.selection.clone();
    let initial_journal = app.session.journal.clone();
    let history = app.session.catalog.photo(active).unwrap().history.len();
    let mut reg = settle(&ctx, &mut app);
    click(&ctx, &mut app, &reg, "pointColor:1");
    assert_eq!(app.ui.point_color, 1);
    reg = settle(&ctx, &mut app);
    click(&ctx, &mut app, &reg, "pointColorVisualize");
    assert!(app.ui.point_color_visualize);
    assert_eq!(model(&app), before);
    assert_eq!(app.session.journal, initial_journal);
    reg = settle(&ctx, &mut app);
    click(&ctx, &mut app, &reg, "button:pointColorDelete");
    assert_eq!(settings(&app).point_colors, vec![first]);
    assert_eq!(app.ui.point_color, 0);
    assert!(app.ui.point_color_visualize);
    assert_eq!(app.session.journal.len(), initial_journal.len() + 1);
    assert_eq!(app.session.journal.last(), Some(&("pointColor.delete".into(), json!({"index":1}))));
    assert_eq!(app.session.catalog.photo(active).unwrap().history.len(), history + 1);
    assert_eq!(app.session.undo.len(), before.1 + 1);
    app.run("edit.undo", json!({})).unwrap();
    let recovered = settle(&ctx, &mut app);
    hit(&recovered, "pointColor:1");
    assert_eq!(model(&app).0, before.0);
    assert_eq!(app.session.selection, selection);
    assert_eq!(app.session.undo.len(), before.1);
    assert!(app.ui.point_color_visualize);
    assert_eq!(app.renderer.in_flight(), 0);
    assert_eq!(app.renderer.live_workers(), 0);
}

fn point_measured_release(native: bool, visual: bool) {
    let p = PointColor::default();
    let (ctx, mut app) = fixture(vec![p, p]);
    let reg = settle(&ctx, &mut app);
    let key = if visual { "pointColorVisualize" } else { "pointColor:1" };
    let owner = hit(&reg, key);
    let at = owner.interact_rect.center();
    let state = |app: &LightcraftApp| {
        let id = app.session.active().unwrap();
        json!({"model":model(app),"photo":&app.session.catalog.photo(id).unwrap(),"revision":app.session.catalog.revision,"journal":app.session.journal,"selection":app.session.selection,"index":app.ui.point_color,"visual":app.ui.point_color_visualize,"tool":app.ui.tool})
    };
    let before = state(&app);
    panel(&ctx, &mut app, vec![Event::PointerMoved(at)]);
    let press = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    let mut presses = 0;
    frame(&ctx, vec![press(true)], |ui| {
        let d = settings(&app);
        point_color(&mut app, ui, &d);
        let reg = take_registries(&ctx);
        let current = hit(&reg, key);
        assert_eq!(current.id, owner.id);
        if visual {
            assert!(ctx.read_response(current.id).unwrap().is_pointer_button_down_on());
        } else {
            // a group member has no egui widget of its own: the press is down on its real sensing area
            assert!(ctx.input(|i| i.pointer.primary_down()) && current.interact_rect.contains(at), "pressed on the sample");
        }
        presses += 1;
        assert_eq!(state(&app), before);
    });
    assert!(presses > 0);
    let mut callbacks = 0;
    frame(&ctx, vec![press(false)], |ui| {
        let mut draw = |ui: &mut egui::Ui| {
            callbacks += 1;
            assert_eq!(ui.is_sizing_pass(), native);
            assert_eq!(craft::ui::runtime::measure::active(&ctx), !native);
            take_registries(&ctx);
            let d = settings(&app);
            point_color(&mut app, ui, &d);
            let (rects, hits) = take_registries(&ctx);
            let guarded = |n: &str| n.starts_with("pointColor:") || n == "pointColorVisualize";
            assert!(rects.iter().all(|(n, _)| !guarded(n)) && hits.iter().all(|(n, _)| !guarded(n)));
            assert_eq!(state(&app), before);
        };
        if native {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
            draw(&mut child);
        } else {
            craft::ui::runtime::measure::run(&ctx, || draw(ui));
        }
    });
    assert!(callbacks > 0);
    let recovered = settle(&ctx, &mut app);
    assert_eq!(state(&app), before);
    assert_eq!(hit(&recovered, key).id, owner.id);
    click(&ctx, &mut app, &recovered, key);
    let mut expected = before;
    if visual {
        expected["visual"] = json!(true);
    } else {
        expected["index"] = json!(1);
    }
    assert_eq!(state(&app), expected);
}
#[test]
fn point_native_sizing_release_recovers_same_swatch_and_visualize_owner() {
    for visual in [false, true] {
        point_measured_release(true, visual);
    }
}
#[test]
fn point_sdk_measure_release_recovers_same_swatch_and_visualize_owner() {
    for visual in [false, true] {
        point_measured_release(false, visual);
    }
}
