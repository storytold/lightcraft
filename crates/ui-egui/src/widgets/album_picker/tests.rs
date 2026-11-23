//! Raw pointer/keyboard regressions for SDK leaves inside the retained native popup host.
use std::sync::Arc;

use super::*;
use crate::headless::HeadlessView;
use crate::widgets::{WidgetHit, take_registries};
use egui::{Event, Id, Key, Modifiers, PointerButton, Pos2};

struct Frames {
    ctx: egui::Context,
    time: f64,
    entries: Vec<AlbumEntry>,
    value: Option<u64>,
    changes: usize,
    choices: usize,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, WidgetHit)>,
    measure: bool,
    nodes: Vec<egui::accesskit::Node>,
}

impl Frames {
    fn new() -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        ctx.enable_accesskit();
        let spec = craft::ui::InstallSpec::new(craft::host::target::ShellInfo::default(), Arc::new(craft::window::system::NoWindows));
        craft::ui::install(&ctx, spec).unwrap();
        let entry = |id, parent, folder, blocked| AlbumEntry { id, name: "Same name".to_owned(), parent, depth: 0, folder, smart: false, blocked };
        let mut entries = vec![
            entry(1, None, true, None),
            entry(2, Some(1), false, None),
            entry(3, None, false, None),
            entry(4, None, false, Some("Would create an album cycle".to_owned())),
            entry(5, None, false, None),
        ];
        set_depths(&mut entries);
        Self { ctx, time: 0.0, entries, value: None, changes: 0, choices: 0, rects: vec![], hits: vec![], measure: false, nodes: vec![] }
    }

    fn frame(&mut self, events: Vec<Event>) {
        let _ = take_registries(&self.ctx);
        self.time += 0.02;
        let raw = HeadlessView::raw_input(vec2(900.0, 560.0), 1.0, self.time, events);
        let ctx = self.ctx.clone();
        let mut out = ctx.run_ui(raw, |ui| {
            if self.measure {
                let focus = ui.memory(|m| m.focused());
                let before = self.ctx.data(|d| d.get_temp::<State>(Id::unique(("albumPicker", "fixture"))));
                craft::ui::runtime::measure::run(&ctx, || {
                    let mut child = ui.new_child(egui::UiBuilder::new().sizing_pass().max_rect(ui.max_rect()));
                    let response = AlbumPicker::new("fixture", &mut self.value, &self.entries).show_with_choices(&mut child, |_| {
                        panic!("a measuring pass must not run caller choices");
                    });
                    assert!(!response.changed());
                });
                assert_eq!(focus, ui.memory(|m| m.focused()));
                let after = self.ctx.data(|d| d.get_temp::<State>(Id::unique(("albumPicker", "fixture"))));
                assert_eq!(
                    before.map(|s| (s.query, s.open, s.highlight, s.focus, s.popup_open)),
                    after.map(|s| (s.query, s.open, s.highlight, s.focus, s.popup_open))
                );
                assert!(take_registries(&ctx).0.is_empty(), "no phantom targets from measurement");
            }
            let response = AlbumPicker::new("fixture", &mut self.value, &self.entries).show_with_choices(ui, |ui| {
                let choice = ui.button("Caller choice");
                crate::widgets::register_response("extra", choice.rect, &choice);
                if choice.clicked() {
                    self.choices += 1;
                    true
                } else {
                    false
                }
            });
            self.changes += usize::from(response.changed());
        });
        // This fixture exercises sensing/layout without a renderer; explicitly discard font uploads.
        out.textures_delta.clear();
        if let Some(update) = out.platform_output.accesskit_update {
            self.nodes.extend(update.nodes.into_iter().map(|(_, node)| node));
        }
        (self.rects, self.hits) = take_registries(&ctx);
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![]);
        }
    }

    fn rect(&self, name: &str) -> Rect {
        self.rects.iter().find(|(id, _)| id == name).unwrap_or_else(|| panic!("missing {name}: {:?}", self.rects)).1
    }

    fn hit(&self, name: &str) -> WidgetHit {
        self.hits.iter().find(|(id, _)| id == name).unwrap_or_else(|| panic!("missing {name}")).1
    }

    fn has(&self, name: &str) -> bool {
        self.rects.iter().any(|(id, _)| id == name)
    }

    fn click(&mut self, name: &str, button: PointerButton) {
        let at = self.rect(name).center();
        self.frame(vec![Event::PointerMoved(at)]);
        self.frame(vec![pointer(at, button, true)]);
        self.frame(vec![pointer(at, button, false)]);
        self.settle();
    }

    fn open(&mut self) {
        self.settle();
        self.click("albumPicker:fixture", PointerButton::Primary);
        assert!(self.has("albumPickerSearch:fixture"));
    }

    fn key(&mut self, key: Key) {
        self.frame(
            [true, false].map(|pressed| Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }).to_vec(),
        );
        self.settle();
    }
}

fn pointer(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE }
}

#[test]
fn sdk_rows_accept_primary_release_but_reject_secondary_and_cancelled_presses() {
    let mut f = Frames::new();
    f.open();
    f.click("albumPickerItem:3:fixture", PointerButton::Secondary);
    assert_eq!(f.value, None);
    assert_eq!(f.changes, 0);
    let at = f.rect("albumPickerItem:3:fixture").center();
    let other = f.rect("albumPickerItem:5:fixture").center();
    f.frame(vec![Event::PointerMoved(at)]);
    f.frame(vec![pointer(at, PointerButton::Primary, true)]);
    f.frame(vec![Event::PointerMoved(other)]);
    f.frame(vec![pointer(other, PointerButton::Primary, false)]);
    f.settle();
    assert_eq!(f.value, None);
    assert_eq!(f.changes, 0);
    f.click("albumPickerItem:3:fixture", PointerButton::Primary);
    assert_eq!(f.value, Some(3));
    assert_eq!(f.changes, 1);
    assert!(!f.has("albumPickerSearch:fixture"));
}

#[test]
fn duplicate_names_keep_catalog_identity_through_folder_open_filter_and_reorder() {
    let mut f = Frames::new();
    f.open();
    let third = f.hit("albumPickerItem:3:fixture").id;
    let fifth = f.hit("albumPickerItem:5:fixture").id;
    assert_ne!(third, fifth);
    f.click("albumPickerFolder:1:fixture", PointerButton::Primary);
    assert!(f.has("albumPickerItem:2:fixture"));
    assert_eq!(f.hit("albumPickerItem:3:fixture").id, third);
    assert_ne!(f.hit("albumPickerItem:2:fixture").id, third);
    f.frame(vec![Event::Text("same".to_owned())]);
    f.settle();
    assert_eq!(f.hit("albumPickerItem:3:fixture").id, third, "search and tree share one catalog identity");
    f.entries.swap(2, 4);
    f.settle();
    assert_eq!(f.hit("albumPickerItem:3:fixture").id, third);
    assert_eq!(f.hit("albumPickerItem:5:fixture").id, fifth);
    f.click("albumPickerItem:5:fixture", PointerButton::Primary);
    assert_eq!(f.value, Some(5));
    f.open();
    assert!(f.nodes.iter().any(|n| n.is_selected() == Some(true)), "selected SDK row remains accessible");
}

#[test]
fn folder_click_restores_search_focus_and_search_keys_skip_blocked_cycle_rows() {
    let mut f = Frames::new();
    f.open();
    let search = f.hit("albumPickerSearch:fixture").id;
    assert_eq!(f.ctx.memory(|m| m.focused()), Some(search));
    f.click("albumPickerFolder:1:fixture", PointerButton::Primary);
    assert_eq!(f.ctx.memory(|m| m.focused()), Some(search));
    assert_eq!(f.value, None, "folder is never picked");
    f.click("albumPickerItem:4:fixture", PointerButton::Primary);
    assert_eq!(f.value, None, "blocked cycles remain visible but cannot be picked");
    f.ctx.memory_mut(|m| m.request_focus(search));
    f.frame(vec![Event::Text("same".to_owned())]);
    f.settle();
    f.key(Key::ArrowDown);
    f.key(Key::ArrowDown);
    f.key(Key::Enter);
    assert_eq!(f.value, Some(5), "2, 3, 5 are pickable: 4 is blocked");
    assert_eq!(f.changes, 1);
}

#[test]
fn sdk_trigger_keyboard_long_release_escape_and_caller_choices_preserve_contract() {
    for key in [Key::Enter, Key::Space] {
        let mut f = Frames::new();
        f.settle();
        let trigger = f.hit("albumPicker:fixture").id;
        f.ctx.memory_mut(|m| m.request_focus(trigger));
        f.frame(vec![]);
        f.key(key);
        assert!(f.has("albumPickerSearch:fixture"), "SDK trigger opens by {key:?}");
        f.key(Key::Escape);
        assert!(!f.has("albumPickerSearch:fixture"));
        assert_eq!(f.value, None);
        assert_eq!(f.changes, 0);
    }
    let mut f = Frames::new();
    f.settle();
    let at = f.rect("albumPicker:fixture").center();
    f.frame(vec![Event::PointerMoved(at)]);
    f.frame(vec![pointer(at, PointerButton::Primary, true)]);
    f.time += 2.0;
    f.frame(vec![]);
    f.frame(vec![pointer(at, PointerButton::Primary, false)]);
    f.settle();
    assert!(f.has("albumPickerSearch:fixture"), "SDK long-release activation is not lost at the native popup boundary");
    f.click("extra", PointerButton::Primary);
    assert_eq!(f.choices, 1);
    assert_eq!(f.changes, 0, "caller actions do not masquerade as album picks");
    assert_eq!(f.value, None);
    assert!(!f.has("albumPickerSearch:fixture"));
}

#[test]
fn measuring_an_open_picker_never_runs_choices_registers_hits_or_mutates_focus() {
    let mut f = Frames::new();
    f.open();
    f.frame(vec![Event::Text("same".to_owned())]);
    f.settle();
    let focused = f.ctx.memory(|m| m.focused());
    f.measure = true;
    f.settle();
    assert_eq!(f.ctx.memory(|m| m.focused()), focused);
    assert_eq!(f.changes, 0);
    assert_eq!(f.value, None);
    for (name, _) in &f.rects {
        assert_eq!(f.rects.iter().filter(|(id, _)| id == name).count(), 1, "one real registration for {name}");
        assert_eq!(f.hits.iter().filter(|(id, _)| id == name).count(), 1, "one real hit owner for {name}");
    }
}

#[test]
fn tab_skips_disabled_rows_and_keyboard_activates_the_actual_sdk_row() {
    let mut f = Frames::new();
    f.open();
    let third = f.hit("albumPickerItem:3:fixture").id;
    let fifth = f.hit("albumPickerItem:5:fixture").id;
    f.ctx.memory_mut(|m| m.request_focus(third));
    f.frame(vec![]);
    f.key(Key::Tab);
    assert_eq!(f.ctx.memory(|m| m.focused()), Some(fifth), "disabled cycle row and allocation rectangles are not focus stops");
    f.key(Key::Space);
    assert_eq!(f.value, Some(5));
    assert_eq!(f.changes, 1);
}

#[test]
fn no_match_enter_keeps_search_ready_and_escape_preserves_the_existing_choice() {
    let mut f = Frames::new();
    f.value = Some(3);
    f.open();
    let search = f.hit("albumPickerSearch:fixture").id;
    f.frame(vec![Event::Text("No such album".to_owned())]);
    f.settle();
    f.key(Key::Enter);
    assert!(f.has("albumPickerSearch:fixture"));
    assert_eq!(f.ctx.memory(|m| m.focused()), Some(search));
    assert_eq!(f.value, Some(3));
    f.key(Key::Escape);
    assert!(!f.has("albumPickerSearch:fixture"));
    assert_eq!(f.value, Some(3));
    assert_eq!(f.changes, 0);
}
