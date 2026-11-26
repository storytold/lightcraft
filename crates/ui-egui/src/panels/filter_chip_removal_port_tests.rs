//! Real chip UI input over an in-memory catalog. No app renderer, filesystem fixtures or forced focus.
use super::*;
use crate::headless::HeadlessView;
use crate::widgets::{WidgetHit, take_registries};
use egui::{Event, Id, Key, Modifiers, PointerButton, Pos2};
use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::Value;
use std::collections::HashMap;

struct Frames {
    ctx: egui::Context,
    app: LightcraftApp,
    time: f64,
    width: f32,
    with_filterbar: bool,
    pressed_owner: Option<Id>,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, WidgetHit)>,
    names: HashMap<egui::accesskit::NodeId, String>,
    all: Vec<u64>,
    sdk_buttons: Vec<(Id, Rect, Option<String>)>,
}

impl Frames {
    fn new(width: f32) -> Self {
        let mut session = lightcraft_engine::Session::new();
        for (id, name, rating) in [(101, "harbor high", 5), (102, "harbor low", 1), (103, "forest high", 5), (104, "forest low", 1)] {
            let mut photo = Photo::new(PhotoId(id), Source::Demo { scene: 0 }, name, "JPEG", 64, 64, "2026-11-19T00:00:00");
            photo.rating = rating;
            session.commit("fixture", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        let all = session.visible_cloned().iter().map(|id| id.0).collect();
        session.execute("library.filter", &json!({"text": "harbor", "rating": 4})).unwrap();
        session.execute("library.select", &json!({"ids": [101]})).unwrap();
        session.journal.clear();
        let mut app = LightcraftApp::new(session, crate::Services::default());
        app.ui.search = "harbor".into();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        ctx.enable_accesskit();
        let spec = craft::ui::InstallSpec::new(craft::host::target::ShellInfo::default(), std::sync::Arc::new(craft::window::system::NoWindows));
        craft::ui::install(&ctx, spec).unwrap();
        let mut frames = Self {
            ctx,
            app,
            time: 0.0,
            width,
            pressed_owner: None,
            with_filterbar: false,
            rects: vec![],
            hits: vec![],
            names: HashMap::new(),
            all,
            sdk_buttons: vec![],
        };
        frames.settle();
        frames
    }

    fn frame(&mut self, events: Vec<Event>) {
        let _ = take_registries(&self.ctx);
        self.time += 0.02;
        let mut input = HeadlessView::raw_input(vec2(self.width, 300.0), 1.0, self.time, events);
        input.events.insert(0, Event::ModifiersChanged(Modifiers::NONE));
        let with_filterbar = self.with_filterbar;
        let app = &mut self.app;
        let mut sdk_buttons = Vec::new();
        let mut pressed_owner = None;
        let mut output = self.ctx.run_ui(input, |ui| {
            let chips = lightcraft_engine::filter_chips(&app.session.filter, &app.session.catalog);
            let (_, record) = craft::ui::runtime::inspect::record(ui, Id::unique("filter-chip-removal-inspect"), |ui| show(app, ui, &chips));
            if with_filterbar {
                crate::panels::filterbar::show(app, ui);
            }
            if let Some(record) = record {
                for e in &record.elements {
                    if e.component == "IconButton" && e.part.is_none() && ui.ctx().read_response(e.id).is_some_and(|r| r.is_pointer_button_down_on())
                    {
                        pressed_owner = Some(e.id);
                    }
                }
                sdk_buttons = record
                    .elements
                    .iter()
                    .filter(|element| element.component == "IconButton")
                    .map(|element| (element.id, element.rect, element.facts.as_ref().and_then(|facts| facts.name.clone())))
                    .collect();
            }
        });
        self.pressed_owner = pressed_owner;
        self.sdk_buttons = sdk_buttons;
        output.textures_delta.clear();
        if let Some(update) = output.platform_output.accesskit_update {
            for (id, node) in update.nodes {
                if let Some(name) = node.label() {
                    self.names.insert(id, name.to_owned());
                }
            }
        }
        (self.rects, self.hits) = take_registries(&self.ctx);
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![]);
        }
    }

    fn rect(&self, id: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing {id}: {:?}", self.rects)).1
    }

    fn chip_id(&self, index: usize) -> Id {
        // The original native control and the SDK replacement share this explicit identity.
        // Native automation registries contain hover probes, so do not confuse their IDs with focus.
        Id::unique(("filter-chip-x", index))
    }

    fn click_at(&mut self, at: Pos2, button: PointerButton) {
        self.frame(vec![Event::PointerMoved(at)]);
        self.frame(vec![pointer(at, button, true)]);
        self.frame(vec![pointer(at, button, false)]);
        self.settle();
    }

    fn visible(&mut self) -> Vec<u64> {
        self.app.session.visible_cloned().iter().map(|id| id.0).collect()
    }

    fn edits(&self) -> Vec<(String, Value)> {
        self.app.session.journal.iter().filter(|(command, _)| command.starts_with("library.")).cloned().collect()
    }

    fn tab_to(&mut self, index: usize) -> Id {
        let target = self.chip_id(index);
        let mut path = Vec::new();
        for _ in 0..12 {
            self.frame(key(Key::Tab));
            let focused = self.ctx.memory(|memory| memory.focused());
            path.push(focused);
            if focused == Some(target) {
                println!("natural Tab path to chip:{index}: {path:?}");
                return target;
            }
        }
        panic!("natural Tab did not reach chip:{index}; path={path:?}");
    }
}

fn pointer(pos: Pos2, button: PointerButton, pressed: bool) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE }
}

fn key_event(key: Key, pressed: bool) -> Event {
    Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }
}

fn key(key: Key) -> Vec<Event> {
    [true, false].map(|pressed| key_event(key, pressed)).to_vec()
}

#[test]
fn rating_removal_sends_only_rating_clear_once_and_keeps_text_selection_and_order() {
    let mut f = Frames::new(1000.0);
    let mut expected = f.app.session.filter.clone();
    expected.rating = 0;
    let selection = f.app.session.selection.clone();
    let revision = f.app.session.catalog.revision;
    assert_eq!(f.visible(), [101]);
    let at = f.rect("chip:1").center();
    f.click_at(at, PointerButton::Primary);
    assert_eq!(f.app.session.filter, expected);
    assert_eq!(f.app.ui.search, "harbor");
    assert_eq!(f.edits(), [("library.filter".into(), json!({"rating": 0}))]);
    assert_eq!(f.app.session.selection, selection);
    assert_eq!(f.app.session.catalog.revision, revision);
    let order: Vec<_> = f.all.iter().copied().filter(|id| [101, 102].contains(id)).collect();
    assert_eq!(f.visible(), order);
}

#[test]
fn text_removal_sends_only_text_clear_once_and_empties_search_not_rating() {
    let mut f = Frames::new(1000.0);
    let mut expected = f.app.session.filter.clone();
    expected.text.clear();
    let selection = f.app.session.selection.clone();
    let at = f.rect("chip:0").center();
    f.click_at(at, PointerButton::Primary);
    assert_eq!(f.app.session.filter, expected);
    assert_eq!(f.app.ui.search, "");
    assert_eq!(f.edits(), [("library.filter".into(), json!({"text": ""}))]);
    assert_eq!(f.app.session.selection, selection);
    let order: Vec<_> = f.all.iter().copied().filter(|id| [101, 103].contains(id)).collect();
    assert_eq!(f.visible(), order);
}

#[test]
fn press_release_outside_secondary_and_chip_label_do_not_clear_any_filter() {
    let mut f = Frames::new(1000.0);
    let filter = f.app.session.filter.clone();
    let at = f.rect("chip:1").center();
    f.frame(vec![Event::PointerMoved(at)]);
    f.frame(vec![pointer(at, PointerButton::Primary, true)]);
    assert_eq!(f.app.session.filter, filter, "press alone is not activation");
    assert!(f.edits().is_empty());
    let outside = egui::pos2(at.x, at.y + 60.0);
    f.frame(vec![Event::PointerMoved(outside), pointer(outside, PointerButton::Primary, false)]);
    f.settle();
    f.click_at(at, PointerButton::Secondary);
    let label = egui::pos2(f.rect("chip:1").left() - 10.0, at.y);
    f.click_at(label, PointerButton::Primary);
    assert_eq!(f.app.session.filter, filter);
    assert_eq!(f.app.ui.search, "harbor");
    assert_eq!(f.visible(), [101]);
    assert!(f.edits().is_empty());
}

#[test]
fn original_close_geometry_and_first_chip_overflow_boundary_are_retained() {
    for width in [300.0, 1000.0] {
        let f = Frames::new(width);
        let chips = lightcraft_engine::filter_chips(&f.app.session.filter, &f.app.session.catalog);
        let tokens = Tokens::get(&f.ctx);
        let bar = f.rect("filterchips");
        let label = display_label(&chips[0], &f.app.session.filter, &f.app.session.catalog);
        let text_width = f.ctx.fonts_mut(|fonts| fonts.layout_no_wrap(label, tokens.font(12.0), egui::Color32::WHITE).size().x);
        let chip_width = (text_width + 12.0 + 22.0).min(bar.width() - 36.0);
        let expected = Rect::from_center_size(pos2(bar.left() + 20.0 + chip_width - 12.0, bar.center().y), vec2(18.0, 18.0));
        assert_eq!(f.rect("chip:0"), expected);
        let count = f.rects.iter().filter(|(name, _)| name.starts_with("chip:")).count();
        assert_eq!(count, if width < 400.0 { 1 } else { 2 });
        assert_eq!(chips.len() - count, if width < 400.0 { 1 } else { 0 });
        assert_eq!(f.rects.iter().filter(|(name, _)| name == "chip:0").count(), 1);
        assert_eq!(f.hits.iter().filter(|(name, _)| name == "chip:0").count(), 1);
        assert_eq!(f.rect("chip:0").size(), vec2(18.0, 18.0));
        assert!(f.edits().is_empty());
    }
}

#[test]
fn translated_accessible_remove_name_keeps_the_specific_filter_label() {
    struct Restore(crate::i18n::Locale);
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::i18n::set_language(self.0);
        }
    }
    let _restore = Restore(crate::i18n::language());
    crate::i18n::set_language(crate::i18n::Locale::De);
    let f = Frames::new(1000.0);
    for chip in lightcraft_engine::filter_chips(&f.app.session.filter, &f.app.session.catalog) {
        let label = display_label(&chip, &f.app.session.filter, &f.app.session.catalog);
        let expected = format!("{}: {label}", crate::i18n::tr("Remove this filter"));
        assert!(f.names.values().any(|name| name == &expected), "missing translated button name {expected:?}: {:?}", f.names);
    }
}

fn observe_focus(f: &Frames, stage: &str) {
    let focused = f.ctx.memory(|memory| memory.focused());
    let chips = lightcraft_engine::filter_chips(&f.app.session.filter, &f.app.session.catalog);
    let constraint = focused.and_then(|id| {
        chips.iter().enumerate().find(|(index, _)| Id::unique(("filter-chip-x", *index)) == id).map(|(index, chip)| (index, chip.clear.clone()))
    });
    let rect = focused.and_then(|id| f.ctx.read_response(id)).map(|response| response.rect);
    println!("{stage}: focused={focused:?}, rect={rect:?}, constraint={constraint:?}, journal={:?}", f.edits());
}

#[test]
fn natural_enter_press_removal_reindex_and_release_never_activate_the_next_chip() {
    let mut f = Frames::new(1000.0);
    observe_focus(&f, "before natural Tab");
    let first = f.tab_to(0);
    observe_focus(&f, "text chip reached by Tab");
    f.frame(vec![key_event(Key::Enter, true)]);
    observe_focus(&f, "after Enter down / before Enter up");
    assert_eq!(f.app.session.filter.rating, 4, "Enter down must never target the next chip");
    assert!(f.edits().len() <= 1, "at most one clear from one physical press");
    // The next UI frame recomputes chips from the filter. If Enter-down removed text,
    // index 0 now denotes rating when this physical release is delivered.
    f.frame(vec![key_event(Key::Enter, false)]);
    f.settle();
    observe_focus(&f, "after Enter up and reindex settlement");
    assert_eq!(f.app.session.filter.text, "");
    assert_eq!(f.app.ui.search, "");
    assert_eq!(f.app.session.filter.rating, 4);
    assert_eq!(f.edits(), [("library.filter".into(), json!({"text": ""}))]);
    assert_eq!(f.chip_id(0), first, "existing index-derived widget IDs are retained");

    // Do not assume where focus landed. Reach the surviving rating chip naturally.
    f.tab_to(0);
    observe_focus(&f, "rating chip naturally reached before Space");
    f.frame(vec![key_event(Key::Space, true)]);
    observe_focus(&f, "after Space down");
    f.frame(vec![key_event(Key::Space, false)]);
    f.settle();
    observe_focus(&f, "after Space up / final chip removal");
    assert_eq!(f.app.session.filter.rating, 0);
    assert_eq!(f.edits(), [("library.filter".into(), json!({"text": ""})), ("library.filter".into(), json!({"rating": 0}))]);
}

#[test]
fn natural_last_chip_enter_press_and_release_preserve_the_first_constraint() {
    let mut f = Frames::new(1000.0);
    f.tab_to(1);
    observe_focus(&f, "last rating chip reached by Tab");
    f.frame(vec![key_event(Key::Enter, true)]);
    observe_focus(&f, "last-chip Enter down");
    assert_eq!(f.app.session.filter.text, "harbor");
    f.frame(vec![key_event(Key::Enter, false)]);
    f.settle();
    observe_focus(&f, "last-chip Enter up / removal settlement");
    assert_eq!(f.app.session.filter.text, "harbor");
    assert_eq!(f.app.ui.search, "harbor");
    assert_eq!(f.app.session.filter.rating, 0);
    assert_eq!(f.edits(), [("library.filter".into(), json!({"rating": 0}))]);
}

#[test]
fn visible_close_targets_are_actual_sdk_icon_buttons_not_native_click_surrogates() {
    let f = Frames::new(1000.0);
    assert_eq!(f.sdk_buttons.len(), 2, "ownership discriminator: native main has no SDK close buttons");
    for index in 0..2 {
        let id = f.chip_id(index);
        let button = f.sdk_buttons.iter().find(|button| button.0 == id).unwrap();
        assert_eq!(button.1, f.rect(&format!("chip:{index}")));
        assert_eq!(button.1.size(), vec2(18.0, 18.0));
        let hit = f.hits.iter().find(|(name, _)| name == &format!("chip:{index}")).unwrap().1;
        assert_eq!(hit.id, id, "locator records the actual SDK owner, not a hover probe");
        assert_eq!(hit.interact_rect, button.1);
        assert!(button.2.as_deref().is_some_and(|name| name.starts_with(crate::i18n::tr("Remove this filter"))));
    }
}

#[test]
fn chip_removal_then_filterbar_clear_restores_library_order_without_photo_edits() {
    let mut f = Frames::new(1440.0);
    f.with_filterbar = true;
    f.settle();
    let selection = f.app.session.selection.clone();
    let revision = f.app.session.catalog.revision;
    let undo = f.app.session.undo.len();
    let redo = f.app.session.redo.len();
    let original_order = f.all.clone();
    let histories = photo_histories(&f.app);
    assert_eq!(f.visible(), [101]);
    let real_rect = |f: &Frames, key: &str| {
        let hits = f.hits.iter().filter(|(name, _)| name == key).collect::<Vec<_>>();
        assert_eq!(hits.len(), 1, "one real owner for {key}");
        hits[0].1.interact_rect
    };
    let at = real_rect(&f, "chip:1").center();
    f.click_at(at, PointerButton::Primary);
    assert_eq!(f.app.ui.search, "harbor");
    assert_eq!(f.app.session.filter.text, "harbor");
    assert_eq!(f.visible(), original_order.iter().copied().filter(|id| [101, 102].contains(id)).collect::<Vec<_>>());
    assert_eq!(f.edits(), [("library.filter".into(), json!({"rating":0}))]);
    let clear = real_rect(&f, "button:filterClear").center();
    f.click_at(clear, PointerButton::Primary);
    assert_eq!(f.app.session.filter, Default::default());
    assert!(f.app.ui.search.is_empty());
    assert_eq!(f.visible(), original_order);
    assert!(!f.hits.iter().any(|(key, _)| key.starts_with("chip:")));
    let expected = vec![("library.filter".into(), json!({"rating":0})), ("library.clearFilter".into(), json!({}))];
    assert_eq!(f.edits(), expected);
    let disabled = real_rect(&f, "button:filterClear").center();
    f.click_at(disabled, PointerButton::Primary);
    assert_eq!(f.edits(), expected, "no second clear from the now-disabled action");
    assert_eq!(f.app.session.selection, selection);
    assert_eq!(f.app.session.catalog.revision, revision);
    assert_eq!((f.app.session.undo.len(), f.app.session.redo.len()), (undo, redo));
    assert_eq!(photo_histories(&f.app), histories);
    assert_eq!(f.app.renderer.in_flight(), 0);
    assert_eq!(f.app.renderer.live_workers(), 0);
}

fn photo_histories(app: &LightcraftApp) -> [Value; 4] {
    [101, 102, 103, 104].map(|id| serde_json::to_value(&app.session.catalog.photo(PhotoId(id)).unwrap().history).unwrap())
}

// Check actual registries inside every measuring callback, not after another pass ran.
fn measured_release_keeps_filter(mode: &str) {
    let mut f = Frames::new(1440.0);
    f.with_filterbar = true;
    f.settle();
    let at = f.rect("chip:1").center();
    let owner = f.chip_id(1);
    let filter = f.app.session.filter.clone();
    let selection = f.app.session.selection.clone();
    let search = f.app.ui.search.clone();
    let revision = f.app.session.catalog.revision;
    let journal = f.app.session.journal.clone();
    let visible = f.visible();
    let undo = f.app.session.undo.len();
    let redo = f.app.session.redo.len();
    let histories = photo_histories;
    let before = histories(&f.app);
    f.frame(vec![Event::PointerMoved(at)]);
    f.frame(vec![pointer(at, PointerButton::Primary, true)]);
    assert_eq!(f.pressed_owner, Some(owner));
    f.time += 0.02;
    let input = HeadlessView::raw_input(vec2(f.width, 300.0), 1.0, f.time, vec![pointer(at, PointerButton::Primary, false)]);
    let mut callbacks = 0;
    let app = &mut f.app;
    let mut out = f.ctx.run_ui(input, |ui| {
        let mut draw = |ui: &mut egui::Ui| {
            callbacks += 1;
            assert_eq!(ui.is_sizing_pass(), mode == "native");
            assert_eq!(craft::ui::runtime::measure::active(ui.ctx()), mode == "sdk");
            let _ = take_registries(ui.ctx());
            let chips = lightcraft_engine::filter_chips(&app.session.filter, &app.session.catalog);
            assert_eq!(chips.len(), 2);
            show(app, ui, &chips);
            crate::panels::filterbar::show(app, ui);
            let (rects, hits) = take_registries(ui.ctx());
            let guarded = |k: &str| k.starts_with("chip:") || matches!(k, "button:filterClear" | "button:filterSave");
            assert!(rects.iter().all(|(k, _)| !guarded(k)) && hits.iter().all(|(k, _)| !guarded(k)));
            assert_eq!(app.session.journal, journal);
            assert_eq!(app.session.filter, filter);
            assert_eq!(app.ui.search, search);
            assert_eq!(app.session.selection, selection);
            assert_eq!(app.session.catalog.revision, revision);
            assert_eq!(histories(app), before);
            assert_eq!((app.session.undo.len(), app.session.redo.len()), (undo, redo));
            assert_eq!(app.session.visible_cloned().iter().map(|p| p.0).collect::<Vec<_>>(), visible);
        };
        if mode == "sdk" {
            craft::ui::runtime::measure::run(&ui.ctx().clone(), || draw(ui));
        } else {
            ui.scope_builder(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible(), |ui| draw(ui));
        }
    });
    out.textures_delta.clear();
    assert!(callbacks > 0);
    f.settle();
    assert_eq!(f.app.session.journal, journal);
    assert_eq!(f.app.session.filter, filter);
    assert_eq!(f.visible(), visible);
    let hit = f.hits.iter().find(|(k, _)| k == "chip:1").unwrap().1;
    assert_eq!(hit.id, owner);
    f.click_at(hit.interact_rect.center(), PointerButton::Primary);
    assert_eq!(f.edits(), [("library.filter".into(), json!({"rating":0}))]);
    assert_eq!(histories(&f.app), before);
    assert_eq!(f.app.session.selection, selection);
    assert_eq!(f.app.session.catalog.revision, revision);
    assert_eq!((f.app.session.undo.len(), f.app.session.redo.len()), (undo, redo));
}
#[test]
fn chip_real_press_sdk_measure_release_idle_then_real_removal() {
    measured_release_keeps_filter("sdk");
}
#[test]
fn chip_real_press_native_sizing_release_idle_then_real_removal() {
    measured_release_keeps_filter("native");
}
