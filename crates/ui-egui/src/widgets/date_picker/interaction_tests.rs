//! Calendar interaction coverage on actual SDK components, without GPU or service work.
use super::*;
use std::collections::HashMap;

use crate::headless::HeadlessView;
use crate::widgets::{WidgetHit, take_registries};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};

struct Frames {
    ctx: egui::Context,
    time: f64,
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
        Self { ctx, time: 0.0, rects: vec![], hits: vec![], nodes: HashMap::new(), shapes: vec![] }
    }

    fn frame(&mut self, events: Vec<Event>, mut draw: impl FnMut(&mut Ui)) {
        let _ = take_registries(&self.ctx);
        self.time += 0.02;
        let mut raw = HeadlessView::raw_input(vec2(1200.0, 800.0), 1.0, self.time, events);
        let modifiers = raw
            .events
            .iter()
            .find_map(|event| match event {
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

    fn has(&self, id: &str) -> bool {
        self.rects.iter().any(|(name, _)| name == id)
    }

    fn click(&mut self, id: &str, button: PointerButton, mut draw: impl FnMut(&mut Ui)) {
        let at = self.rect(id).center();
        self.frame(vec![Event::PointerMoved(at)], &mut draw);
        self.frame(vec![pointer(at, button, true)], &mut draw);
        self.frame(vec![pointer(at, button, false)], &mut draw);
        self.settle(&mut draw);
    }

    fn focus(&mut self, id: &str, mut draw: impl FnMut(&mut Ui)) {
        self.ctx.memory_mut(|m| m.request_focus(self.hit(id).id));
        self.frame(vec![], &mut draw);
    }

    fn keyboard(&mut self, keycode: Key, shift: bool, mut draw: impl FnMut(&mut Ui)) {
        self.frame(key(keycode, shift), &mut draw);
        self.settle(&mut draw);
    }
}

fn pointer(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE }
}

fn key(key: Key, shift: bool) -> Vec<Event> {
    let modifiers = Modifiers { shift, ..Modifiers::NONE };
    [true, false].map(|pressed| Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }).to_vec()
}

struct Picker {
    value: Option<PickedDate>,
    changes: usize,
    precisions: Vec<Precision>,
    min: Option<PickedDate>,
    max: Option<PickedDate>,
    today: Option<PickedDate>,
    disabled: bool,
    sdk_disabled: bool,
    read_only: bool,
}

impl Picker {
    fn new(value: &str) -> Self {
        Self {
            value: PickedDate::parse(value),
            changes: 0,
            precisions: Precision::ALL.to_vec(),
            min: None,
            max: None,
            today: None,
            disabled: false,
            sdk_disabled: false,
            read_only: false,
        }
    }

    fn show(&mut self, ui: &mut Ui) {
        craft::ui::runtime::env::with_env(
            ui,
            |set| {
                set.read_only = Some(self.read_only);
                set.enabled = Some(!self.sdk_disabled);
            },
            |ui| {
                ui.add_enabled_ui(!self.disabled, |ui| {
                    let response = DatePicker::new("fixture", &mut self.value)
                        .precisions(&self.precisions)
                        .min(self.min)
                        .max(self.max)
                        .today(self.today)
                        .show(ui);
                    self.changes += usize::from(response.changed());
                });
            },
        );
    }

    fn open(&mut self, frames: &mut Frames) {
        frames.click("datePicker:fixture", PointerButton::Primary, |ui| self.show(ui));
        assert!(crate::dialogs::popup_open(&frames.ctx));
    }
}

#[test]
fn long_primary_release_selects_exactly_once_at_each_precision() {
    for (original, target) in [("2026-08-14", "2026-08-15"), ("2026-08", "2026-09"), ("2026", "2027")] {
        let mut frames = Frames::new();
        let mut picker = Picker::new(original);
        frames.settle(|ui| picker.show(ui));
        picker.open(&mut frames);
        assert_eq!(picker.changes, 0, "opening does not select");
        let at = frames.rect(&format!("datePickerCell:{target}:fixture")).center();
        frames.frame(vec![Event::PointerMoved(at)], |ui| picker.show(ui));
        frames.frame(vec![pointer(at, PointerButton::Primary, true)], |ui| picker.show(ui));
        frames.time += 2.0;
        frames.frame(vec![], |ui| picker.show(ui));
        assert_eq!(picker.value, PickedDate::parse(original), "press is not a pick");
        frames.frame(vec![pointer(at, PointerButton::Primary, false)], |ui| picker.show(ui));
        frames.settle(|ui| picker.show(ui));
        assert_eq!(picker.value, PickedDate::parse(target));
        assert_eq!(picker.changes, 1, "one SDK release, one changed response");
        assert!(!crate::dialogs::popup_open(&frames.ctx));
        assert_eq!(frames.ctx.memory(|m| m.focused()), Some(frames.hit("datePicker:fixture").id));
    }
}

#[test]
fn secondary_click_and_drag_out_never_pick_switch_precision_or_page() {
    for target in ["datePickerCell:2026-08-15:fixture", "datePickerNext:fixture", "datePickerPrecision:month:fixture"] {
        let mut frames = Frames::new();
        let mut picker = Picker::new("2026-08-14");
        frames.settle(|ui| picker.show(ui));
        picker.open(&mut frames);
        frames.click(target, PointerButton::Secondary, |ui| picker.show(ui));
        assert!(frames.has("datePickerCell:2026-08-14:fixture"));
        assert_eq!(picker.changes, 0);
        let at = frames.rect(target).center();
        let outside = egui::pos2(1100.0, 700.0);
        frames.frame(vec![pointer(at, PointerButton::Primary, true)], |ui| picker.show(ui));
        frames.frame(vec![Event::PointerMoved(outside), pointer(outside, PointerButton::Primary, false)], |ui| picker.show(ui));
        frames.settle(|ui| picker.show(ui));
        assert_eq!(picker.value, PickedDate::parse("2026-08-14"));
        assert_eq!(picker.changes, 0);
        // Escape and reopen also proves the original page/precision was not edited by the drag.
        frames.keyboard(Key::Escape, false, |ui| picker.show(ui));
        picker.open(&mut frames);
        assert!(frames.has("datePickerCell:2026-08-14:fixture"));
    }
}

#[test]
fn keyboard_open_tab_shift_tab_pick_and_escape_keep_focus_and_selection_distinct() {
    for opener in [Key::Enter, Key::Space] {
        let mut frames = Frames::new();
        let mut picker = Picker::new("2026-08-14");
        frames.settle(|ui| picker.show(ui));
        frames.focus("datePicker:fixture", |ui| picker.show(ui));
        frames.keyboard(opener, false, |ui| picker.show(ui));
        assert_eq!(frames.ctx.memory(|m| m.focused()), Some(frames.hit("datePickerCell:2026-08-14:fixture").id));
        frames.keyboard(Key::Tab, false, |ui| picker.show(ui));
        assert_eq!(frames.ctx.memory(|m| m.focused()), Some(frames.hit("datePickerCell:2026-08-15:fixture").id));
        frames.keyboard(Key::Tab, true, |ui| picker.show(ui));
        assert_eq!(frames.ctx.memory(|m| m.focused()), Some(frames.hit("datePickerCell:2026-08-14:fixture").id));
        frames.keyboard(Key::Tab, false, |ui| picker.show(ui));
        assert_eq!(picker.changes, 0, "focus movement never edits");
        frames.keyboard(Key::Space, false, |ui| picker.show(ui));
        assert_eq!(picker.value, PickedDate::parse("2026-08-15"));
        assert_eq!(picker.changes, 1);
        assert_eq!(frames.ctx.memory(|m| m.focused()), Some(frames.hit("datePicker:fixture").id));
        frames.keyboard(Key::Enter, false, |ui| picker.show(ui));
        frames.keyboard(Key::Escape, false, |ui| picker.show(ui));
        assert!(!crate::dialogs::popup_open(&frames.ctx));
        assert_eq!(picker.changes, 1, "cancel is not another selection");
        assert_eq!(frames.ctx.memory(|m| m.focused()), Some(frames.hit("datePicker:fixture").id));
    }
}

#[test]
fn precision_options_and_page_buttons_activate_from_keyboard_without_committing() {
    let mut frames = Frames::new();
    let mut picker = Picker::new("2026-08-14");
    frames.settle(|ui| picker.show(ui));
    picker.open(&mut frames);
    frames.focus("datePickerPrecision:month:fixture", |ui| picker.show(ui));
    frames.keyboard(Key::Enter, false, |ui| picker.show(ui));
    assert!(frames.has("datePickerCell:2026-08:fixture"));
    frames.focus("datePickerNext:fixture", |ui| picker.show(ui));
    frames.keyboard(Key::Space, false, |ui| picker.show(ui));
    assert!(frames.has("datePickerCell:2027-08:fixture"));
    frames.focus("datePickerPrecision:year:fixture", |ui| picker.show(ui));
    frames.keyboard(Key::Space, false, |ui| picker.show(ui));
    frames.focus("datePickerPrev:fixture", |ui| picker.show(ui));
    frames.keyboard(Key::Enter, false, |ui| picker.show(ui));
    assert!(frames.has("datePickerCell:2015:fixture"));
    assert_eq!(picker.value, PickedDate::parse("2026-08-14"));
    assert_eq!(picker.changes, 0);
    frames.focus("datePickerCell:2015:fixture", |ui| picker.show(ui));
    frames.keyboard(Key::Enter, false, |ui| picker.show(ui));
    assert_eq!(picker.value, Some(PickedDate::year(2015)));
    assert_eq!(picker.changes, 1);
}

#[test]
fn bounds_disable_cells_but_keep_overlapping_months_and_years_selectable() {
    for (original, blocked, allowed) in [("2026-08-14", "2026-08-13", "2026-08-15"), ("2026-08", "2026-07", "2026-08"), ("2026", "2025", "2026")] {
        let mut frames = Frames::new();
        let mut picker = Picker::new(original);
        picker.min = PickedDate::parse("2026-08-14");
        picker.max = PickedDate::parse("2026-08-15");
        frames.settle(|ui| picker.show(ui));
        picker.open(&mut frames);
        assert!(frames.nodes.get(blocked).unwrap().is_disabled());
        assert!(!frames.nodes.get(allowed).unwrap().is_disabled());
        frames.click(&format!("datePickerCell:{blocked}:fixture"), PointerButton::Primary, |ui| picker.show(ui));
        assert_eq!(picker.changes, 0);
        assert!(crate::dialogs::popup_open(&frames.ctx));
        frames.focus(&format!("datePickerCell:{blocked}:fixture"), |ui| picker.show(ui));
        frames.keyboard(Key::Enter, false, |ui| picker.show(ui));
        assert_eq!(picker.changes, 0, "disabled cells cannot activate by key");
        frames.click(&format!("datePickerCell:{allowed}:fixture"), PointerButton::Primary, |ui| picker.show(ui));
        assert_eq!(picker.value, PickedDate::parse(allowed));
        assert_eq!(picker.changes, 1, "picking the same date still reports the user's selection");
    }
}

#[test]
fn already_open_popup_keeps_its_parent_disabled_and_read_only_contract() {
    for mode in 0..3 {
        let mut frames = Frames::new();
        let mut picker = Picker::new("2026-08-14");
        frames.settle(|ui| picker.show(ui));
        picker.open(&mut frames);
        picker.disabled = mode == 0;
        picker.sdk_disabled = mode == 1;
        picker.read_only = mode == 2;
        frames.settle(|ui| picker.show(ui));
        for target in ["datePickerCell:2026-08-15:fixture", "datePickerNext:fixture", "datePickerPrecision:month:fixture"] {
            frames.click(target, PointerButton::Primary, |ui| picker.show(ui));
            assert!(frames.has("datePickerCell:2026-08-14:fixture"), "mode={mode}, target={target}");
            assert_eq!(picker.changes, 0);
        }
        frames.keyboard(Key::Escape, false, |ui| picker.show(ui));
        assert_eq!(picker.value, PickedDate::parse("2026-08-14"));
        assert_eq!(picker.changes, 0);
    }
}

#[test]
fn offered_precision_today_fallback_and_reopen_preserve_values() {
    let mut frames = Frames::new();
    let mut picker = Picker::new("not a date");
    picker.precisions = vec![Precision::Month];
    picker.today = PickedDate::parse("2026-10-09");
    frames.settle(|ui| picker.show(ui));
    picker.open(&mut frames);
    assert!(frames.has("datePickerCell:2026-10:fixture"));
    assert!(!frames.has("datePickerPrecision:day:fixture"));
    assert!(!frames.has("datePickerCell:2026-10-09:fixture"));
    assert_eq!(frames.ctx.memory(|m| m.focused()), Some(frames.hit("datePickerCell:2026-10:fixture").id));
    frames.click("datePickerNext:fixture", PointerButton::Primary, |ui| picker.show(ui));
    frames.keyboard(Key::Escape, false, |ui| picker.show(ui));
    assert_eq!(picker.value, None);
    assert_eq!(picker.changes, 0);
    picker.open(&mut frames);
    assert!(frames.has("datePickerCell:2026-10:fixture"), "reopen resets to value/today, not the dismissed page");
}

fn text_shapes<'a>(shape: &'a egui::Shape, out: &mut Vec<&'a egui::epaint::TextShape>) {
    match shape {
        egui::Shape::Text(text) => out.push(text),
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                text_shapes(shape, out);
            }
        }
        _ => {}
    }
}

#[test]
fn locale_headers_monday_first_grid_and_today_underline_survive_the_port() {
    struct Restore(crate::i18n::Locale);
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::i18n::set_language(self.0);
        }
    }
    let _restore = Restore(crate::i18n::language());
    for locale in [crate::i18n::Locale::En, crate::i18n::Locale::Fr, crate::i18n::Locale::Ja] {
        crate::i18n::set_language(locale);
        let mut frames = Frames::new();
        let mut picker = Picker::new("2026-08-14");
        picker.today = PickedDate::parse("2026-08-15");
        frames.settle(|ui| picker.show(ui));
        picker.open(&mut frames);
        let first = frames.rect("datePickerCell:2026-08-01:fixture");
        let monday = frames.rect("datePickerCell:2026-08-03:fixture");
        let next_saturday = frames.rect("datePickerCell:2026-08-08:fixture");
        assert!(first.center().x > monday.center().x && monday.top() > first.top());
        assert!((first.left() - next_saturday.left()).abs() < 0.1, "weeks keep their columns");
        assert_eq!(frames.nodes.get("2026-08-14").unwrap().is_selected(), Some(true));
        assert_ne!(frames.nodes.get("2026-08-15").unwrap().is_selected(), Some(true), "SDK omits the selected attribute when off");
        assert!(frames.nodes.contains_key(crate::i18n::tr("Month")));
        let mut text = vec![];
        for shape in &frames.shapes {
            text_shapes(&shape.shape, &mut text);
        }
        assert!(text.iter().any(|text| text.galley.job.text == crate::i18n::date_group_label("2026-08", false)));
        for name in WEEKDAY_SHORT {
            assert!(text.iter().any(|text| text.galley.job.text == crate::i18n::tr(name)), "{locale:?}: {name}");
        }
        assert!(
            text.iter()
                .any(|text| text.galley.job.text == "15" && text.galley.job.sections.iter().any(|section| section.format.underline != Stroke::NONE)),
            "today remains underlined"
        );
    }
}

#[test]
fn sizing_and_framework_measurement_publish_no_calendar_option_or_focus_stop() {
    let mut frames = Frames::new();
    let mut activations = 0;
    frames.settle(|ui| {
        let ctx = ui.ctx().clone();
        let focus = ctx.memory(|m| m.focused());
        craft::ui::runtime::measure::run(&ctx, || {
            let mut child = ui.new_child(egui::UiBuilder::new().sizing_pass().max_rect(ui.max_rect()));
            let response = option(&mut child, "measured".into(), RichText::new("14"), "2026-08-14".into(), true, true, 26.0);
            activations += usize::from(clicked(&response));
        });
        assert_eq!(focus, ctx.memory(|m| m.focused()));
    });
    assert_eq!(activations, 0);
    assert!(frames.rects.is_empty() && frames.hits.is_empty());
}

fn rules_app(value: serde_json::Value) -> crate::LightcraftApp {
    let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
    app.session.clock = Box::new(|| "2026-10-09T12:00:00".to_owned());
    app.ui.dialog = Some(crate::state::Dialog::SmartRules {
        id: None,
        name: "Unsaved dates".to_owned(),
        parent: None,
        rules: serde_json::from_value(serde_json::json!({"rules": [{"field":"captureDate", "op":"is", "value":value}]})).unwrap(),
    });
    app
}

fn rule_value(app: &crate::LightcraftApp) -> serde_json::Value {
    let Some(crate::state::Dialog::SmartRules { rules, name, .. }) = &app.ui.dialog else { panic!("editor closed") };
    assert_eq!(name, "Unsaved dates");
    let lightcraft_catalog::Rule::Field { value, .. } = &rules.rules[0] else { panic!("no date rule") };
    value.clone()
}

#[test]
fn real_smart_rules_dialog_keyboard_navigation_pick_and_escape_preserve_unsaved_edits() {
    let mut frames = Frames::new();
    let mut app = rules_app(serde_json::json!("2026-08-14T10:00"));
    frames.settle(|ui| crate::dialogs::show(&mut app, ui.ctx(), false));
    frames.focus("datePicker:rules-0", |ui| crate::dialogs::show(&mut app, ui.ctx(), false));
    frames.keyboard(Key::Enter, false, |ui| crate::dialogs::show(&mut app, ui.ctx(), false));
    assert_eq!(frames.ctx.memory(|m| m.focused()), Some(frames.hit("datePickerCell:2026-08-14:rules-0").id));
    frames.keyboard(Key::Tab, false, |ui| crate::dialogs::show(&mut app, ui.ctx(), false));
    assert_eq!(frames.ctx.memory(|m| m.focused()), Some(frames.hit("datePickerCell:2026-08-15:rules-0").id));
    assert_eq!(rule_value(&app), serde_json::json!("2026-08-14T10:00"));
    frames.keyboard(Key::Enter, false, |ui| crate::dialogs::show(&mut app, ui.ctx(), false));
    assert_eq!(rule_value(&app), serde_json::json!("2026-08-15"), "caller still strips time only on a pick");
    assert!(!crate::dialogs::popup_open(&frames.ctx));
    assert_eq!(frames.ctx.memory(|m| m.focused()), Some(frames.hit("datePicker:rules-0").id));
    frames.keyboard(Key::Enter, false, |ui| crate::dialogs::show(&mut app, ui.ctx(), false));
    frames.keyboard(Key::Tab, false, |ui| crate::dialogs::show(&mut app, ui.ctx(), false));
    frames.keyboard(Key::Escape, false, |ui| crate::dialogs::show(&mut app, ui.ctx(), false));
    assert_eq!(rule_value(&app), serde_json::json!("2026-08-15"));
    assert!(!crate::dialogs::popup_open(&frames.ctx));
    assert_eq!(frames.ctx.memory(|m| m.focused()), Some(frames.hit("datePicker:rules-0").id));
}

#[test]
fn compact_option_footprint_matches_original_egui_leaves_in_the_calendar_grid() {
    for today in [false, true] {
        let mut before = Frames::new();
        let mut after = Frames::new();
        let mut expected = vec![];
        before.settle(|ui| {
            expected.clear();
            ui.horizontal(|ui| {
                for label in ["Year", "Month", "Day"] {
                    expected.push(ui.selectable_label(label == "Day", label).rect);
                }
            });
            egui::Grid::new("original").spacing(vec2(2.0, 2.0)).show(ui, |ui| {
                for name in WEEKDAY_SHORT {
                    ui.label(RichText::new(name).weak());
                }
                ui.end_row();
                for day in 1..=14 {
                    let text = if today && day == 14 { RichText::new(day.to_string()).strong().underline() } else { RichText::new(day.to_string()) };
                    expected.push(ui.add(egui::Button::selectable(day == 13, text).min_size(vec2(26.0, 0.0))).rect);
                    if day % 7 == 0 {
                        ui.end_row();
                    }
                }
            });
        });
        let mut actual = vec![];
        after.settle(|ui| {
            actual.clear();
            ui.horizontal(|ui| {
                for label in ["Year", "Month", "Day"] {
                    actual.push(option(ui, label.into(), RichText::new(label), label.into(), label == "Day", true, 0.0).rect);
                }
            });
            egui::Grid::new("ported").spacing(vec2(2.0, 2.0)).show(ui, |ui| {
                for name in WEEKDAY_SHORT {
                    ui.label(RichText::new(name).weak());
                }
                ui.end_row();
                for day in 1..=14 {
                    let text = if today && day == 14 { RichText::new(day.to_string()).strong().underline() } else { RichText::new(day.to_string()) };
                    actual.push(option(ui, day.to_string(), text, day.to_string(), day == 13, true, 26.0).rect);
                    if day % 7 == 0 {
                        ui.end_row();
                    }
                }
            });
        });
        assert_eq!(actual, expected, "identical compact faces and seven-column hit rectangles; today={today}");
    }
}

#[test]
fn long_primary_release_pages_and_switches_precision_without_a_selection() {
    let mut frames = Frames::new();
    let mut picker = Picker::new("2026-08-14");
    frames.settle(|ui| picker.show(ui));
    picker.open(&mut frames);
    for (target, content) in
        [("datePickerNext:fixture", "datePickerCell:2026-09-01:fixture"), ("datePickerPrecision:month:fixture", "datePickerCell:2026-09:fixture")]
    {
        let at = frames.rect(target).center();
        frames.frame(vec![Event::PointerMoved(at)], |ui| picker.show(ui));
        frames.frame(vec![pointer(at, PointerButton::Primary, true)], |ui| picker.show(ui));
        frames.time += 2.0;
        frames.frame(vec![], |ui| picker.show(ui));
        frames.frame(vec![pointer(at, PointerButton::Primary, false)], |ui| picker.show(ui));
        frames.settle(|ui| picker.show(ui));
        assert!(frames.has(content));
        assert_eq!(picker.value, PickedDate::parse("2026-08-14"));
        assert_eq!(picker.changes, 0);
    }
}

#[test]
fn outside_click_dismisses_without_selecting_or_stealing_another_field_focus() {
    let mut frames = Frames::new();
    let mut picker = Picker::new("2026-08-14");
    let mut text = String::from("Keep editing");
    let mut draw = |ui: &mut Ui| {
        picker.show(ui);
        ui.add_space(400.0);
        let response = ui.text_edit_singleline(&mut text);
        crate::widgets::register_response("outside", response.rect, &response);
    };
    frames.settle(&mut draw);
    frames.click("datePicker:fixture", PointerButton::Primary, &mut draw);
    frames.click("datePickerNext:fixture", PointerButton::Primary, &mut draw);
    frames.click("outside", PointerButton::Primary, &mut draw);
    assert!(!crate::dialogs::popup_open(&frames.ctx));
    assert_eq!(frames.ctx.memory(|m| m.focused()), Some(frames.hit("outside").id));
    drop(draw);
    assert_eq!(picker.value, PickedDate::parse("2026-08-14"));
    assert_eq!(picker.changes, 0);
}

#[test]
fn popup_tab_wraps_and_skips_disabled_dates_without_editing() {
    let mut frames = Frames::new();
    let mut picker = Picker::new("2026-08-14");
    picker.min = PickedDate::parse("2026-08-14");
    picker.max = PickedDate::parse("2026-08-15");
    frames.settle(|ui| picker.show(ui));
    picker.open(&mut frames);
    for (shift, expected) in [
        (false, "datePickerCell:2026-08-15:fixture"),
        (false, "datePickerPrecision:year:fixture"),
        (true, "datePickerCell:2026-08-15:fixture"),
        (true, "datePickerCell:2026-08-14:fixture"),
        (true, "datePickerNext:fixture"),
        (false, "datePickerCell:2026-08-14:fixture"),
    ] {
        frames.keyboard(Key::Tab, shift, |ui| picker.show(ui));
        assert_eq!(frames.ctx.memory(|m| m.focused()), Some(frames.hit(expected).id), "shift={shift}, expected={expected}");
        assert_eq!(picker.changes, 0, "Tab only navigates");
    }
}
