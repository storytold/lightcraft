//! Behavior tests for the SDK compact-icon seam. No renderer, GPU, services or filesystem IO.

use std::collections::HashMap;
use std::sync::Arc;

use crate::theme::Tokens;

use crate::headless::HeadlessView;
use crate::icons::Icon;
use crate::widgets::{WidgetHit, icon_button, take_registries};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, Ui, Vec2, vec2};

struct Frames {
    ctx: egui::Context,
    time: f64,
    size: Vec2,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, WidgetHit)>,
    nodes: HashMap<String, egui::accesskit::Node>,
    shapes: Vec<egui::epaint::ClippedShape>,
}

impl Frames {
    fn new() -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        ctx.enable_accesskit();
        let spec = craft::ui::InstallSpec::new(craft::host::target::ShellInfo::default(), Arc::new(craft::window::system::NoWindows));
        craft::ui::install(&ctx, spec).unwrap();
        Self { ctx, time: 0.0, size: vec2(900.0, 560.0), rects: vec![], hits: vec![], nodes: HashMap::new(), shapes: vec![] }
    }

    fn frame(&mut self, events: Vec<Event>, mut draw: impl FnMut(&mut Ui)) {
        let _ = take_registries(&self.ctx);
        self.time += 0.02;
        let mut raw = HeadlessView::raw_input(self.size, 1.0, self.time, events);
        let modifiers = raw
            .events
            .iter()
            .find_map(|e| match e {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        raw.events.insert(0, Event::ModifiersChanged(modifiers));
        let mut out = self.ctx.run_ui(raw, |ui| draw(ui));
        out.textures_delta.clear();
        if let Some(update) = out.platform_output.accesskit_update {
            for (_, node) in update.nodes {
                if let Some(label) = node.label() {
                    self.nodes.insert(label.to_owned(), node);
                }
            }
        }
        self.shapes = out.shapes;
        (self.rects, self.hits) = take_registries(&self.ctx);
    }

    fn settle(&mut self, mut draw: impl FnMut(&mut Ui)) {
        for _ in 0..3 {
            self.frame(vec![], &mut draw);
        }
    }

    /// Whether the last frame painted `text` as a line of text.
    fn painted(&self, text: &str) -> bool {
        fn found(shape: &egui::Shape, text: &str) -> bool {
            match shape {
                egui::Shape::Text(t) => t.galley.job.text == text,
                egui::Shape::Vec(shapes) => shapes.iter().any(|shape| found(shape, text)),
                _ => false,
            }
        }
        self.shapes.iter().any(|clipped| found(&clipped.shape, text))
    }

    fn rect(&self, id: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing {id}: {:?}", self.rects)).1
    }

    fn hit(&self, id: &str) -> WidgetHit {
        self.hits.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing hit {id}")).1
    }

    fn click(&mut self, at: Pos2, button: PointerButton, mut draw: impl FnMut(&mut Ui)) {
        self.frame(vec![Event::PointerMoved(at)], &mut draw);
        self.frame(vec![pointer(at, button, true)], &mut draw);
        self.frame(vec![pointer(at, button, false)], &mut draw);
    }
}

fn pointer(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE }
}

fn key(key: Key, shift: bool) -> Vec<Event> {
    let modifiers = Modifiers { shift, ..Modifiers::NONE };
    [true, false].map(|pressed| Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }).to_vec()
}

fn rect_shapes(shape: &egui::Shape, out: &mut Vec<egui::epaint::RectShape>) {
    match shape {
        egui::Shape::Rect(rect) => out.push(rect.clone()),
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                rect_shapes(shape, out);
            }
        }
        _ => {}
    }
}

#[test]
fn original_auto_ids_hit_rectangles_faces_and_following_ids_survive_layout_variants() {
    for layout in [
        egui::Layout::top_down(egui::Align::Min),
        egui::Layout::right_to_left(egui::Align::Center),
        egui::Layout::top_down_justified(egui::Align::Center),
    ] {
        let mut before = Frames::new();
        let mut after = Frames::new();
        let mut expected = vec![];
        before.settle(|ui| {
            expected.clear();
            ui.with_layout(layout, |ui| {
                for size in [vec2(20.0, 20.0), vec2(24.0, 22.0), vec2(40.0, 32.0), vec2(48.0, 40.0)] {
                    let (face, response) = ui.allocate_exact_size(size, egui::Sense::click());
                    expected.push((face, response.rect, response.id, ui.next_auto_id()));
                }
            });
        });
        after.settle(|ui| {
            ui.with_layout(layout, |ui| {
                for (i, size) in [vec2(20.0, 20.0), vec2(24.0, 22.0), vec2(40.0, 32.0), vec2(48.0, 40.0)].into_iter().enumerate() {
                    let response = icon_button(ui, &i.to_string(), Icon::Eye, size, false, true, "Show overlay (O)");
                    let (_, hit, id, following) = expected[i];
                    assert_eq!(response.rect, hit);
                    assert_eq!(response.id, id, "the original allocation owns interaction");
                    assert_eq!(ui.next_auto_id(), following, "no phantom allocation");
                }
            });
        });
        for (i, (face, hit, _, _)) in expected.iter().enumerate() {
            assert_eq!(after.rect(&format!("icon:{i}")), *face);
            assert_eq!(after.hit(&format!("icon:{i}")).interact_rect, *hit);
        }
    }
}

#[test]
fn sizing_and_framework_measurement_publish_no_duplicate_target_or_activation() {
    let mut frames = Frames::new();
    frames.settle(|ui| {
        let ctx = ui.ctx().clone();
        let focus = ctx.memory(|m| m.focused());
        craft::ui::runtime::measure::run(&ctx, || {
            let mut child = ui.new_child(egui::UiBuilder::new().sizing_pass().max_rect(ui.max_rect()));
            assert!(!icon_button(&mut child, "measured", Icon::Eye, vec2(24.0, 22.0), true, false, "Show overlay (O)").clicked());
        });
        assert_eq!(focus, ctx.memory(|m| m.focused()));
        let out = icon_button(ui, "measured", Icon::Eye, vec2(24.0, 22.0), true, false, "Show overlay (O)");
        assert_eq!(out.rect.size(), vec2(24.0, 22.0));
    });
    assert_eq!(frames.rects.len(), 1);
    assert_eq!(frames.hits.len(), 1);
    let node = frames.nodes.get("Show overlay").unwrap();
    assert!(node.is_disabled());
    assert_eq!(node.is_selected(), Some(true));
    assert_eq!(node.description(), Some("O"));
}

#[test]
fn primary_release_secondary_click_and_cancel_have_distinct_results() {
    let mut frames = Frames::new();
    let mut activations = 0;
    let mut draw = |ui: &mut Ui| {
        let out = icon_button(ui, "action", Icon::Eye, vec2(30.0, 30.0), false, true, "Show overlay (O)").on_hover_text("Show overlay (O)");
        activations += usize::from(out.clicked());
    };
    frames.settle(&mut draw);
    let at = frames.rect("icon:action").center();
    frames.click(at, PointerButton::Primary, &mut draw);
    frames.click(at, PointerButton::Secondary, &mut draw);
    frames.frame(vec![pointer(at, PointerButton::Primary, true)], &mut draw);
    let outside = at + vec2(200.0, 100.0);
    frames.frame(vec![Event::PointerMoved(outside), pointer(outside, PointerButton::Primary, false)], &mut draw);
    frames.frame(vec![pointer(outside, PointerButton::Primary, true)], &mut draw);
    frames.frame(vec![Event::PointerMoved(at), pointer(at, PointerButton::Primary, false)], &mut draw);
    drop(draw);
    assert_eq!(activations, 1);
}

#[test]
fn long_primary_release_keeps_sdk_result_through_tooltip_chains() {
    let mut frames = Frames::new();
    let mut activations = vec![];
    let mut draw = |ui: &mut Ui| {
        let out = icon_button(ui, "held", Icon::Eye, vec2(30.0, 30.0), false, true, "Show overlay (O)").on_hover_text("Show overlay (O)");
        if out.clicked() {
            activations.push(out.response.clicked());
        }
    };
    frames.settle(&mut draw);
    let at = frames.rect("icon:held").center();
    frames.frame(vec![Event::PointerMoved(at)], &mut draw);
    frames.frame(vec![pointer(at, PointerButton::Primary, true)], &mut draw);
    frames.time += 2.0;
    frames.frame(vec![], &mut draw);
    frames.frame(vec![pointer(at, PointerButton::Primary, false)], &mut draw);
    drop(draw);
    assert_eq!(activations, [false], "SDK primary-release activation is not egui's duration-limited click");
}

#[test]
fn explicit_disabled_scope_disabled_and_read_only_reject_activation_without_resizing() {
    for mode in 0..3 {
        let mut frames = Frames::new();
        let mut activations = 0;
        let mut draw = |ui: &mut Ui| {
            craft::ui::runtime::env::with_env(
                ui,
                |set| set.read_only = Some(mode == 2),
                |ui| {
                    ui.add_enabled_ui(mode != 1, |ui| {
                        let out = icon_button(ui, "off", Icon::Forward, vec2(30.0, 30.0), true, mode != 0, "Forward");
                        assert_eq!(out.rect.size(), vec2(30.0, 30.0));
                        activations += usize::from(out.clicked());
                    });
                },
            );
        };
        frames.settle(&mut draw);
        let target = frames.hit("icon:off");
        frames.click(frames.rect("icon:off").center(), PointerButton::Primary, &mut draw);
        frames.ctx.memory_mut(|m| m.request_focus(target.id));
        frames.frame(vec![], &mut draw);
        frames.frame(key(Key::Enter, false), &mut draw);
        frames.frame(key(Key::Space, false), &mut draw);
        drop(draw);
        assert_eq!(activations, 0, "mode={mode}");
        assert_eq!(frames.nodes.get("Forward").unwrap().is_disabled(), mode != 2);
    }
}

#[test]
fn selected_plate_and_keyboard_focus_remain_visible_in_every_theme() {
    let mut frames = Frames::new();
    for kind in crate::theme::ThemeKind::ALL {
        crate::theme::apply_kind(&frames.ctx, kind);
        let mut draw = |ui: &mut Ui| {
            let _ = icon_button(ui, "selected", Icon::Sliders, vec2(48.0, 40.0), true, true, "Edit (E)");
        };
        frames.settle(&mut draw);
        let hit = frames.hit("icon:selected");
        frames.ctx.memory_mut(|m| m.request_focus(hit.id));
        frames.frame(vec![], &mut draw);
        frames.frame(key(Key::Enter, false), &mut draw);
        let mut rects = vec![];
        for shape in &frames.shapes {
            rect_shapes(&shape.shape, &mut rects);
        }
        let t = Tokens::get(&frames.ctx);
        let plate = Rect::from_center_size(frames.rect("icon:selected").center(), vec2(32.0, 32.0));
        assert!(rects.iter().any(|r| r.rect == plate && r.fill == t.tool_active), "selected plate in {kind:?}");
        assert!(rects.iter().any(|r| r.stroke.width == 2.0 && r.stroke.color == t.accent), "keyboard focus in {kind:?}");
        assert_eq!(frames.nodes.get("Edit").unwrap().is_selected(), Some(true));
        assert_eq!(frames.nodes.get("Edit").unwrap().description(), Some("E"));
    }
}

#[test]
fn real_tool_strip_changes_panel_by_pointer_enter_and_space() {
    for activation in [None, Some(Key::Enter), Some(Key::Space)] {
        let mut frames = Frames::new();
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        app.ui.right = crate::state::RightPanel::Edit;
        let mut draw = |ui: &mut Ui| crate::panels::strip::show(&mut app, ui);
        frames.settle(&mut draw);
        let hit = frames.hit("icon:crop");
        if let Some(keycode) = activation {
            frames.ctx.memory_mut(|m| m.request_focus(hit.id));
            frames.frame(vec![], &mut draw);
            frames.frame(key(keycode, false), &mut draw);
        } else {
            frames.click(frames.rect("icon:crop").center(), PointerButton::Primary, &mut draw);
        }
        frames.frame(vec![], &mut draw);
        drop(draw);
        assert_eq!(app.ui.right, crate::state::RightPanel::Crop, "{activation:?}");
        assert_eq!(frames.nodes.get("Crop & Rotate").unwrap().is_selected(), Some(true));
    }
}

#[test]
fn real_tool_strip_unavailable_photo_controls_do_not_activate() {
    let mut frames = Frames::new();
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
    app.ui.right = crate::state::RightPanel::Info;
    let mut draw = |ui: &mut Ui| crate::panels::strip::show(&mut app, ui);
    frames.settle(&mut draw);
    let hit = frames.hit("icon:crop");
    frames.click(frames.rect("icon:crop").center(), PointerButton::Primary, &mut draw);
    frames.ctx.memory_mut(|m| m.request_focus(hit.id));
    frames.frame(vec![], &mut draw);
    frames.frame(key(Key::Enter, false), &mut draw);
    drop(draw);
    assert_eq!(app.ui.right, crate::state::RightPanel::Info);
    assert!(frames.nodes.get("Crop & Rotate").unwrap().is_disabled());
}

#[test]
fn real_presets_menu_opens_on_long_release_and_keyboard_then_escape_dismisses() {
    for activation in [None, Some(Key::Enter), Some(Key::Space)] {
        let mut frames = Frames::new();
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        let mut draw = |ui: &mut Ui| crate::panels::presets::show(&mut app, ui);
        frames.settle(&mut draw);
        let hit = frames.hit("icon:presetMore");
        if let Some(keycode) = activation {
            frames.ctx.memory_mut(|m| m.request_focus(hit.id));
            frames.frame(vec![], &mut draw);
            frames.frame(key(keycode, false), &mut draw);
        } else {
            let at = frames.rect("icon:presetMore").center();
            frames.frame(vec![Event::PointerMoved(at)], &mut draw);
            frames.frame(vec![pointer(at, PointerButton::Primary, true)], &mut draw);
            frames.time += 2.0;
            frames.frame(vec![], &mut draw);
            frames.frame(vec![pointer(at, PointerButton::Primary, false)], &mut draw);
        }
        frames.frame(vec![], &mut draw);
        // The options menu is a framework menu layer, not an egui popup.
        assert!(craft::ui::runtime::overlay::any_open(&frames.ctx, craft::ui::runtime::overlay::OpenFilter::Any), "{activation:?}");
        assert!(frames.rects.iter().any(|(id, _)| id == "presetMenu:thumbnails"));
        frames.frame(key(Key::Escape, false), &mut draw);
        frames.frame(vec![], &mut draw);
        assert!(!craft::ui::runtime::overlay::any_open(&frames.ctx, craft::ui::runtime::overlay::OpenFilter::Any));
    }
}

#[test]
fn real_bottom_bar_sort_and_compact_view_options_open_and_dismiss() {
    for (width, target, content) in [(1600.0, "icon:sort", "Capture Date"), (390.0, "icon:viewOptions", "Photo Grid (G)")] {
        let mut frames = Frames::new();
        frames.size = vec2(width, 560.0);
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        let mut draw = |ui: &mut Ui| crate::panels::bottombar::show(&mut app, ui);
        frames.settle(&mut draw);
        assert!(!frames.nodes.contains_key(content), "popup content must not already be present: {target}");
        let at = frames.rect(target).center();
        frames.frame(vec![Event::PointerMoved(at)], &mut draw);
        frames.frame(vec![pointer(at, PointerButton::Primary, true)], &mut draw);
        frames.time += 2.0;
        frames.frame(vec![], &mut draw);
        frames.frame(vec![pointer(at, PointerButton::Primary, false)], &mut draw);
        frames.frame(vec![], &mut draw);
        // The bottom bar's menus are framework menus now, not egui popups: a menu is open while its rows are painted.
        assert!(frames.painted(content), "actual {target} menu content is painted");
        assert!(frames.nodes.contains_key(content), "actual {target} popup content: {:?}", frames.nodes.keys());
        frames.frame(key(Key::Escape, false), &mut draw);
        // (a closing menu fades out for a few frames)
        for _ in 0..12 {
            frames.frame(vec![], &mut draw);
        }
        assert!(!frames.painted(content), "Escape dismisses the {target} menu");
    }
}

#[test]
fn real_calendar_opens_by_enter_escape_preserves_value_and_pick_returns_changed_response() {
    use crate::widgets::date_picker::{DatePicker, PickedDate};
    let mut frames = Frames::new();
    let original = PickedDate::parse("2026-08-14").unwrap();
    let mut value = Some(original);
    let mut changed = false;
    let mut draw = |ui: &mut Ui| {
        changed |= DatePicker::new("fixture", &mut value).show(ui).changed();
    };
    frames.settle(&mut draw);
    let hit = frames.hit("icon:datePicker:fixture");
    frames.ctx.memory_mut(|m| m.request_focus(hit.id));
    frames.frame(vec![], &mut draw);
    frames.frame(key(Key::Enter, false), &mut draw);
    frames.frame(vec![], &mut draw);
    assert!(crate::dialogs::popup_open(&frames.ctx));
    assert!(frames.rects.iter().any(|(id, _)| id == "datePickerCell:2026-08-15:fixture"));
    frames.frame(key(Key::Escape, false), &mut draw);
    frames.frame(vec![], &mut draw);
    drop(draw);
    assert_eq!(value, Some(original));
    assert!(!changed);
    assert!(!crate::dialogs::popup_open(&frames.ctx));
    let mut draw = |ui: &mut Ui| {
        changed |= DatePicker::new("fixture", &mut value).show(ui).changed();
    };
    frames.click(frames.rect("icon:datePicker:fixture").center(), PointerButton::Primary, &mut draw);
    frames.frame(vec![], &mut draw);
    frames.click(frames.rect("datePickerCell:2026-08-15:fixture").center(), PointerButton::Primary, &mut draw);
    frames.frame(vec![], &mut draw);
    drop(draw);
    assert_eq!(value, PickedDate::parse("2026-08-15"));
    assert!(changed, "the public DatePicker Response remains changed when a date is picked");
    assert!(!crate::dialogs::popup_open(&frames.ctx));
}

#[test]
fn actual_app_letter_shortcut_still_routes_when_no_control_owns_focus() {
    let mut frames = Frames::new();
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
    app.ui.right = crate::state::RightPanel::Crop;
    let mut draw = |ui: &mut Ui| {
        crate::panels::strip::show(&mut app, ui);
        crate::shortcuts::handle(&mut app, ui.ctx());
    };
    frames.settle(&mut draw);
    frames.frame(key(Key::E, false), &mut draw);
    drop(draw);
    assert_eq!(app.ui.right, crate::state::RightPanel::Edit);
}

#[test]
fn focused_leaf_does_not_consume_letter_shortcuts_or_add_duplicate_tab_stops() {
    let mut frames = Frames::new();
    let mut saw_letter = false;
    let mut clicked = false;
    let mut draw = |ui: &mut Ui| {
        ui.horizontal(|ui| {
            clicked |= icon_button(ui, "first", Icon::Sliders, vec2(30.0, 30.0), true, true, "Edit (E)").clicked();
            clicked |= icon_button(ui, "second", Icon::Crop, vec2(30.0, 30.0), false, true, "Crop & Rotate (C)").clicked();
        });
        saw_letter |= ui.input(|i| i.key_pressed(Key::E));
    };
    frames.settle(&mut draw);
    let first = frames.hit("icon:first").id;
    let second = frames.hit("icon:second").id;
    frames.ctx.memory_mut(|m| m.request_focus(first));
    frames.frame(vec![], &mut draw);
    frames.frame(key(Key::Tab, false), &mut draw);
    assert_eq!(frames.ctx.memory(|m| m.focused()), Some(second));
    frames.frame(key(Key::Tab, true), &mut draw);
    assert_eq!(frames.ctx.memory(|m| m.focused()), Some(first));
    frames.frame(key(Key::E, false), &mut draw);
    drop(draw);
    assert!(saw_letter);
    assert!(!clicked, "the tooltip's E hint is not a new activation binding");
}
