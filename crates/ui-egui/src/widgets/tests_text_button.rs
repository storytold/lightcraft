//! Behavior tests for the SDK dense-button seam. No renderer, GPU, services or filesystem IO.

use std::collections::HashMap;
use std::sync::Arc;

use crate::theme::Tokens;
use craft::ui::contract::{Props as _, Show as _};

use crate::headless::HeadlessView;
use crate::widgets::{WidgetHit, take_registries, text_button, text_button_size};
use egui::{Event, Id, Key, Modifiers, PointerButton, Pos2, Rect, Ui, Vec2, vec2};

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
fn measurement_is_read_only_and_matches_selected_disabled_and_translated_buttons() {
    let mut frames = Frames::new();
    let mut widths = vec![];
    frames.settle(|ui| {
        let next = ui.next_auto_id();
        let cursor = ui.cursor();
        let focus = ui.ctx().memory(|m| m.focused());
        let before = ui.ctx().data(|d| d.get_temp::<crate::widgets::Registry>(Id::unique("lc-registry"))).map_or(0, |r| r.rects.len());
        for _ in 0..3 {
            for label in ["Auto", "B&W", "AI Denoise", "Reset layout"] {
                assert!(text_button_size(ui, label) >= 37.0);
            }
        }
        assert_eq!(next, ui.next_auto_id());
        assert_eq!(cursor, ui.cursor());
        assert_eq!(focus, ui.ctx().memory(|m| m.focused()));
        let after = ui.ctx().data(|d| d.get_temp::<crate::widgets::Registry>(Id::unique("lc-registry"))).map_or(0, |r| r.rects.len());
        assert_eq!(before, after, "measurement must not register automation targets");
        widths.clear();
        for (i, (enabled, active)) in [(true, false), (true, true), (false, false), (false, true)].into_iter().enumerate() {
            let measured = text_button_size(ui, "Auto");
            let out = ui.add_enabled_ui(enabled, |ui| text_button(ui, &format!("width-{i}"), "Auto", active)).inner;
            assert!((out.rect.width() - measured).abs() < 0.01);
            assert_eq!(out.rect.height(), 24.0);
            widths.push(out.rect.width());
        }
    });
    assert!(widths.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(frames.rects.len(), 4);
    for i in 0..4 {
        let name = format!("button:width-{i}");
        assert_eq!(frames.rects.iter().filter(|(id, _)| id == &name).count(), 1);
        assert_eq!(frames.hits.iter().filter(|(id, _)| id == &name).count(), 1);
        assert_eq!(frames.hit(&name).interact_rect, frames.rect(&name));
    }
}

#[test]
fn invisible_measurement_runs_never_register_a_second_target() {
    let mut frames = Frames::new();
    frames.settle(|ui| {
        let ctx = ui.ctx().clone();
        craft::ui::runtime::measure::run(&ctx, || {
            let mut child = ui.new_child(egui::UiBuilder::new().sizing_pass().max_rect(ui.max_rect()));
            let _ = text_button(&mut child, "measure", "Auto", true);
        });
        let out = text_button(ui, "measure", "Auto", true);
        assert!(!out.clicked());
    });
    assert_eq!(frames.rects.len(), 1);
    assert_eq!(frames.hits.len(), 1);
    assert_eq!(frames.rects[0].0, "button:measure");
}

#[test]
fn pointer_activation_survives_tooltips_but_secondary_and_cancellation_do_not_activate() {
    let mut frames = Frames::new();
    let mut clicks = 0;
    let mut draw = |ui: &mut Ui| {
        if text_button(ui, "action", "Reset layout", false).on_hover_text("Restore the layout").clicked() {
            clicks += 1;
        }
    };
    frames.settle(&mut draw);
    let at = frames.rect("button:action").center();
    frames.click(at, PointerButton::Primary, &mut draw);
    frames.click(at, PointerButton::Secondary, &mut draw);
    frames.frame(vec![Event::PointerMoved(at), pointer(at, PointerButton::Primary, true)], &mut draw);
    let outside = at + vec2(200.0, 100.0);
    frames.frame(vec![Event::PointerMoved(outside)], &mut draw);
    frames.frame(vec![pointer(outside, PointerButton::Primary, false)], &mut draw);
    frames.frame(vec![pointer(outside, PointerButton::Primary, true)], &mut draw);
    frames.frame(vec![Event::PointerMoved(at), pointer(at, PointerButton::Primary, false)], &mut draw);
    assert_eq!(clicks, 1, "only the complete primary click activates");
}

#[test]
fn long_primary_release_uses_framework_click_and_tooltip_keeps_the_output() {
    let mut frames = Frames::new();
    let mut activations = vec![];
    let mut draw = |ui: &mut Ui| {
        let out = text_button(ui, "held", "Apply", false).on_hover_text("Apply changes");
        if out.clicked() {
            activations.push(out.response.clicked());
        }
    };
    frames.settle(&mut draw);
    let at = frames.rect("button:held").center();
    frames.frame(vec![Event::PointerMoved(at)], &mut draw);
    frames.frame(vec![pointer(at, PointerButton::Primary, true)], &mut draw);
    frames.time += 2.0;
    frames.frame(vec![], &mut draw);
    frames.frame(vec![pointer(at, PointerButton::Primary, false)], &mut draw);
    assert_eq!(activations, [false], "SDK activation must not be silently narrowed to egui's click duration");
}

#[test]
fn settings_choices_change_by_pointer_enter_and_space_with_a_visible_focus_ring() {
    for activation in [None, Some(Key::Enter), Some(Key::Space)] {
        let mut frames = Frames::new();
        let mut value = 0;
        let mut draw = |ui: &mut Ui| {
            ui.horizontal_wrapped(|ui| {
                crate::dialogs::settings::choices(ui, "settingsPreview", &[(0, "Standard"), (1, "Large"), (2, "Maximum")], &mut value);
            });
        };
        frames.settle(&mut draw);
        let target = frames.hit("button:settingsPreview-1");
        if let Some(keycode) = activation {
            frames.ctx.memory_mut(|m| m.request_focus(target.id));
            frames.frame(vec![], &mut draw);
            frames.frame(key(keycode, false), &mut draw);
        } else {
            frames.click(frames.rect("button:settingsPreview-1").center(), PointerButton::Primary, &mut draw);
        }
        frames.frame(vec![], &mut draw);
        drop(draw);
        assert_eq!(value, 1, "actual Settings choices caller: {activation:?}");
        assert_eq!(frames.nodes.get("Large").unwrap().toggled(), Some(egui::accesskit::Toggled::True));
        let mut rects = vec![];
        for clipped in &frames.shapes {
            rect_shapes(&clipped.shape, &mut rects);
        }
        let t = Tokens::get(&frames.ctx);
        let focus_visible = rects.iter().any(|r| r.stroke.width == 2.0 && r.stroke.color == t.accent);
        assert_eq!(focus_visible, activation.is_some(), "keyboard focus is visible; pointer focus does not add a ring");
    }
}

#[test]
fn tab_and_shift_tab_visit_the_real_buttons_once_and_leave_letter_shortcuts_available() {
    let mut frames = Frames::new();
    let mut clicked = false;
    let mut saw_shortcut = false;
    let mut draw = |ui: &mut Ui| {
        ui.horizontal(|ui| {
            clicked |= text_button(ui, "first", "Auto", false).clicked();
            ui.add_enabled_ui(false, |ui| {
                let _ = text_button(ui, "disabled", "B&W", true);
            });
            clicked |= text_button(ui, "second", "HDR", true).clicked();
        });
        saw_shortcut |= ui.input(|i| i.key_pressed(Key::X));
    };
    frames.settle(&mut draw);
    let first = frames.hit("button:first").id;
    let second = frames.hit("button:second").id;
    frames.ctx.memory_mut(|m| m.request_focus(first));
    frames.frame(vec![], &mut draw);
    frames.frame(key(Key::Tab, false), &mut draw);
    assert_eq!(frames.ctx.memory(|m| m.focused()), Some(second), "no phantom allocation/probe focus stop");
    frames.frame(key(Key::Tab, true), &mut draw);
    assert_eq!(frames.ctx.memory(|m| m.focused()), Some(first));
    frames.frame(key(Key::X, false), &mut draw);
    drop(draw);
    assert!(!clicked);
    assert!(saw_shortcut, "the app's X shortcut remains its own, not a button activation");
}

#[test]
fn disabled_and_read_only_buttons_reject_pointer_and_keyboard_activation_without_resizing() {
    for disabled in [true, false] {
        let mut frames = Frames::new();
        let mut clicked = false;
        let mut draw = |ui: &mut Ui| {
            craft::ui::runtime::env::with_env(
                ui,
                |set| set.read_only = Some(!disabled),
                |ui| {
                    ui.add_enabled_ui(!disabled, |ui| {
                        let out = text_button(ui, "off", "Apply", true).on_hover_text("Selected but unavailable");
                        clicked |= out.clicked();
                        assert!((out.rect.width() - text_button_size(ui, "Apply")).abs() < 0.01);
                    });
                },
            );
        };
        frames.settle(&mut draw);
        let target = frames.hit("button:off");
        frames.click(frames.rect("button:off").center(), PointerButton::Primary, &mut draw);
        frames.ctx.memory_mut(|m| m.request_focus(target.id));
        frames.frame(vec![], &mut draw);
        frames.frame(key(Key::Enter, false), &mut draw);
        frames.frame(key(Key::Space, false), &mut draw);
        drop(draw);
        assert!(!clicked, "disabled={disabled}");
        assert_eq!(frames.nodes.get("Apply").unwrap().is_disabled(), disabled);
    }
}

#[test]
fn import_dialog_tag_toggle_keeps_its_selected_state_for_pointer_and_keyboard() {
    let mut frames = Frames::new();
    let mut open = false;
    let mut draw = |ui: &mut Ui| open = crate::dialogs::import::tag_toggle(ui, "importRename");
    frames.settle(&mut draw);
    let target = frames.hit("button:importRenameTags");
    frames.click(frames.rect("button:importRenameTags").center(), PointerButton::Primary, &mut draw);
    drop(draw);
    assert!(open, "actual import-dialog caller opens its help");
    let mut draw = |ui: &mut Ui| open = crate::dialogs::import::tag_toggle(ui, "importRename");
    frames.frame(vec![], &mut draw);
    assert_eq!(frames.nodes.get("Tags").unwrap().toggled(), Some(egui::accesskit::Toggled::True));
    frames.ctx.memory_mut(|m| m.request_focus(target.id));
    frames.frame(vec![], &mut draw);
    frames.frame(key(Key::Space, false), &mut draw);
    drop(draw);
    assert!(!open, "Space closes that same help, not a synthetic action");
}

#[test]
fn selected_face_and_raw_user_label_are_preserved_in_each_app_theme() {
    let mut frames = Frames::new();
    for kind in crate::theme::ThemeKind::ALL {
        crate::theme::apply_kind(&frames.ctx, kind);
        frames.settle(|ui| {
            let _ = text_button(ui, "labelSet-Auto", "Auto", true);
        });
        let rect = frames.rect("button:labelSet-Auto");
        let t = Tokens::get(&frames.ctx);
        let mut rects = vec![];
        for clipped in &frames.shapes {
            rect_shapes(&clipped.shape, &mut rects);
        }
        assert!(rects.iter().any(|r| r.rect == rect && r.fill == t.pressed), "selected app face: {kind:?}");
        let node = frames.nodes.get("Auto").unwrap();
        assert_eq!(node.role(), egui::accesskit::Role::Button);
        assert_eq!(node.toggled(), Some(egui::accesskit::Toggled::True));
    }
}

#[test]
fn real_settings_tab_bar_wraps_inside_a_phone_width_without_changing_button_widths() {
    let mut frames = Frames::new();
    frames.size = vec2(390.0, 700.0);
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
    let mut tab = "general".to_owned();
    frames.settle(|ui| crate::dialogs::settings::window(&mut app, ui, &mut tab, 600.0));
    let rects: Vec<_> = crate::dialogs::settings::TABS.iter().map(|(id, _)| frames.rect(&format!("button:settingsTab-{id}"))).collect();
    assert!(rects.iter().any(|r| r.top() > rects[0].bottom()), "the real Settings tabs wrap");
    for rect in rects {
        assert!(rect.left() >= 0.0 && rect.right() <= frames.size.x, "{rect:?}");
        assert_eq!(rect.height(), 24.0);
    }
}

#[test]
fn right_to_left_allocation_preserves_labels_and_hit_owners() {
    let mut frames = Frames::new();
    frames.settle(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let a = text_button(ui, "rtlApply", "Apply", true);
            let b = text_button(ui, "rtlCancel", "Cancel", false);
            assert!(a.rect.left() > b.rect.right());
            assert!((a.rect.width() - text_button_size(ui, "Apply")).abs() < 0.01);
        });
    });
    for (id, _) in &frames.rects {
        assert_eq!(frames.hit(id).interact_rect, frames.rect(id));
    }
    assert!(frames.nodes.contains_key("Apply") && frames.nodes.contains_key("Cancel"));
}

#[test]
fn actual_chip_overflow_popup_opens_on_framework_release_and_escape_dismisses_it() {
    let mut frames = Frames::new();
    frames.size = vec2(520.0, 300.0);
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
    let chips: Vec<_> =
        (0..6).map(|i| lightcraft_engine::FilterChip { label: format!("Fixture filter {i}"), clear: serde_json::json!({"text": ""}) }).collect();
    let mut draw = |ui: &mut Ui| crate::panels::chips::show(&mut app, ui, &chips);
    frames.settle(&mut draw);
    let at = frames.rect("button:chipsMore").center();
    frames.frame(vec![Event::PointerMoved(at)], &mut draw);
    frames.frame(vec![pointer(at, PointerButton::Primary, true)], &mut draw);
    frames.time += 2.0;
    frames.frame(vec![], &mut draw);
    frames.frame(vec![pointer(at, PointerButton::Primary, false)], &mut draw);
    frames.frame(vec![], &mut draw);
    let open = |frames: &Frames| craft::ui::runtime::overlay::any_open(&frames.ctx, craft::ui::runtime::overlay::OpenFilter::Any);
    assert!(open(&frames), "actual +N more caller opens its framework menu by SDK activation");
    frames.frame(key(Key::Escape, false), &mut draw);
    frames.frame(vec![], &mut draw);
    assert!(!open(&frames));
}

#[test]
fn translated_actions_match_the_width_helper_but_user_label_set_names_stay_literal() {
    struct RestoreLocale(crate::i18n::Locale);
    impl Drop for RestoreLocale {
        fn drop(&mut self) {
            crate::i18n::set_language(self.0);
        }
    }
    let _restore = RestoreLocale(crate::i18n::language());
    crate::i18n::set_language(crate::i18n::Locale::De);
    let mut frames = Frames::new();
    let translated = crate::i18n::tr("Reset").to_owned();
    assert_ne!(translated, "Reset", "the fixture must exercise a real translation");
    frames.settle(|ui| {
        let action = text_button(ui, "resetTranslated", "Reset", false);
        assert!((action.rect.width() - text_button_size(ui, "Reset")).abs() < 0.01);
        let user = text_button(ui, "labelSet-Reset", "Reset", true);
        let t = Tokens::get(ui.ctx());
        let raw = ui.painter().layout_no_wrap("Reset".to_owned(), t.semibold(11.5), t.text);
        assert!((user.rect.width() - crate::widgets::text_button_width(raw.size().x)).abs() < 0.01);
    });
    assert!(frames.nodes.contains_key(&translated));
    assert!(frames.nodes.contains_key("Reset"), "a user's preset name is not a translation key");
}

#[test]
fn stock_sdk_button_and_dense_adapter_share_primary_and_keyboard_activation() {
    for keycode in [None, Some(Key::Enter), Some(Key::Space)] {
        for dense in [false, true] {
            let mut frames = Frames::new();
            let mut activations = 0;
            let mut target = None;
            let mut draw = |ui: &mut Ui| {
                if dense {
                    let out = text_button(ui, "oracle", "Apply", false).on_hover_text("Apply changes");
                    target = Some((out.id, out.rect));
                    activations += usize::from(out.clicked());
                } else {
                    let out = craft::ui::buttons::Button::new("Apply").id_salt("oracle").show(ui);
                    target = Some((out.response.id, out.response.rect));
                    activations += usize::from(out.clicked());
                }
            };
            frames.settle(&mut draw);
            drop(draw);
            let (id, rect) = target.unwrap();
            let mut draw = |ui: &mut Ui| {
                if dense {
                    activations += usize::from(text_button(ui, "oracle", "Apply", false).on_hover_text("Apply changes").clicked());
                } else {
                    activations += usize::from(craft::ui::buttons::Button::new("Apply").id_salt("oracle").show(ui).clicked());
                }
            };
            if let Some(keycode) = keycode {
                frames.ctx.memory_mut(|m| m.request_focus(id));
                frames.frame(vec![], &mut draw);
                frames.frame(key(keycode, false), &mut draw);
            } else {
                frames.click(rect.center(), PointerButton::Primary, &mut draw);
            }
            drop(draw);
            assert_eq!(activations, 1, "SDK oracle parity: dense={dense} key={keycode:?}");
        }
    }
}

#[test]
fn actual_masking_plus_and_minus_popups_open_on_long_release_and_escape_dismisses_them() {
    for (button, operation) in [("maskPlus", "add"), ("maskMinus", "subtract")] {
        let mut frames = Frames::new();
        frames.size = vec2(520.0, 1600.0);
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        app.run("mask.add", serde_json::json!({"kind": "brush"})).unwrap();
        let photo = app.session.active().unwrap();
        let mask = app.session.active_mask.unwrap();
        // Draw the real Masking panel, including its real +/− caller and component_menu content;
        // no app logic/render worker or model is needed for this deterministic brush mask.
        let mut draw = |ui: &mut Ui| crate::panels::masking::show(&mut app, ui, photo);
        frames.settle(&mut draw);
        let at = frames.rect(&format!("button:{button}:{mask}")).center();
        frames.frame(vec![Event::PointerMoved(at)], &mut draw);
        frames.frame(vec![pointer(at, PointerButton::Primary, true)], &mut draw);
        frames.time += 2.0;
        frames.frame(vec![], &mut draw);
        frames.frame(vec![pointer(at, PointerButton::Primary, false)], &mut draw);
        assert!(!frames.ctx.input(|i| i.pointer.primary_clicked()), "the real release exceeds egui's duration limit");
        frames.frame(vec![], &mut draw);
        // the menu is the framework's, not an egui popup: its rows are what shows it is open
        let entry = format!("maskComp:{operation}:brush");
        assert!(
            frames.rects.iter().any(|(id, _)| id == &entry),
            "actual {button} caller must keep SDK activation through its tooltip; the real {operation} component menu is shown: {:?}",
            frames.rects
        );
        frames.frame(key(Key::Escape, false), &mut draw);
        frames.time += 2.0;
        frames.settle(&mut draw);
        assert!(!frames.rects.iter().any(|(id, _)| id == &entry), "Escape dismisses the {operation} menu");
    }
}
