//! Installed color-label names/saved sets; actual session commands, no tick/provider/disk.
//! Existing set-choice SDK buttons are reused; six raw native text edits are the comparison target.
use super::*;
use crate::{Services, headless::HeadlessView};
use egui::{Event, Key, Modifiers, PointerButton, Pos2};
use lightcraft_catalog::{HistoryStep, Op, Photo, PhotoId, Source, Version};
use lightcraft_develop::DevelopSettings;
use lightcraft_engine::{Selection, Session};
use serde_json::Value;
use std::{collections::VecDeque, sync::Arc};
const CLOCK: &str = "2026-10-01T12:00:00Z";
struct Drain(egui::FullOutput);
impl Drop for Drain {
    fn drop(&mut self) {
        self.0.textures_delta.clear();
        self.0.platform_output.commands.clear();
        self.0.viewport_output.clear();
        self.0.shapes.clear();
    }
}
fn model() -> LightcraftApp {
    let mut session = Session::new();
    session.clock = Box::new(|| CLOCK.into());
    session.xmp.auto_write = false;
    for index in 0..3 {
        let id = PhotoId(101 + index);
        let mut p = Photo::new(id, Source::Demo { scene: index as u32 }, &format!("Protected photo {index}"), "JPEG", 8, 6, CLOCK);
        let mut d = DevelopSettings::default();
        d.light.contrast = index as f64 * 7.0;
        p.develop = Arc::new(d);
        p.local = index == 2;
        p.meta.title = format!("Title {index}");
        p.meta.caption = format!("Caption {index}");
        p.meta.keywords = vec![format!("keep-{index}")];
        p.history = vec![HistoryStep { label: format!("Old {index}"), settings: p.develop.clone() }];
        p.versions = vec![Version { name: format!("Saved {index}"), created: CLOCK.into(), settings: p.develop.clone(), auto: false }];
        session.commit("Fixture", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    session.selection = Selection::single(PhotoId(101));
    let mut app = LightcraftApp::new(session, Services { png: None, local_places: Some(vec![]), ..Default::default() });
    app.set_simulated_clock(Some(0.0));
    assert_eq!(app.run("album.create", json!({"name":"Parent folder","folder":true})).unwrap()["id"], 1);
    assert_eq!(app.run("album.create", json!({"name":"Unrelated album","addSelected":true})).unwrap()["id"], 2);
    for (id, label) in [(101, "red"), (102, "green"), (103, "blue")] {
        app.run("photo.label", json!({"ids":[id],"label":label})).unwrap();
    }
    app.run(
        "label.setNames",
        json!({"names":{"red":"Seed Red","yellow":"Seed Yellow","green":"Seed Green","blue":"Seed Blue","purple":"Seed Purple"}}),
    )
    .unwrap();
    app.run("label.saveSet", json!({"name":"Kept"})).unwrap();
    app.run(
        "label.setNames",
        json!({"names":{"red":"Other Red","yellow":"Other Yellow","green":"Other Green","blue":"Other Blue","purple":"Other Purple"}}),
    )
    .unwrap();
    app.run("label.saveSet", json!({"name":"Other"})).unwrap();
    app.run(
        "label.setNames",
        json!({"names":{"red":"Seed Red","yellow":"Seed Yellow","green":"Seed Green","blue":"Seed Blue","purple":"Seed Purple"}}),
    )
    .unwrap();
    app.run("develop.merge", json!({"settings":{"light":{"exposure":0.5}},"label":"Protected redo"})).unwrap();
    app.run("edit.undo", json!({})).unwrap();
    app.run("library.select", json!({"ids":[101,103],"active":101})).unwrap();
    assert!(!app.session.undo.is_empty() && !app.session.redo.is_empty());
    app.ui.settings.gpu = false;
    app.ui.settings.external_editor = "protected preference".into();
    app
}
fn history(entries: &[lightcraft_engine::UndoEntry]) -> Value {
    json!(entries.iter().map(|entry| json!({"label":entry.label,"op":entry.op,"folder":format!("{:?}",entry.folder)})).collect::<Vec<_>>())
}
fn document(app: &LightcraftApp) -> Value {
    let s = &app.session;
    json!({"catalog":s.catalog,"revision":s.catalog.revision,"selection":s.selection,"previousActive":s.previous_active,
        "undo":history(&s.undo),"redo":history(&s.redo),"journal":s.journal,"commits":s.commits(),
        "interaction":format!("{:?}",s.interaction),"autoSync":s.auto_sync,"clipboard":s.clipboard,"copyGroups":s.copy_groups,
        "source":s.source,"filter":s.filter,"sort":s.sort,"activeMask":s.active_mask,"activeSpot":s.active_spot,
        "toolEffects":s.mask_tool_effects,"lastMerge":s.last_merge,"imports":s.import_defaults,"xmp":s.xmp,
        "lastExport":s.last_export,"presets":s.presets.iter().cloned().collect::<Vec<_>>(),"print":s.print,
        "templates":s.print_templates,"effects":s.mask_effects,"keywordSets":s.keyword_sets,"keywordSet":s.keyword_set,
        "userLabelSets":s.label_sets,"labelSetReader":lightcraft_engine::cmd::manage::label_sets_json(s),
        "exportPresets":s.export_presets,"metadataPresets":s.metadata_presets,"filterPresets":s.filter_presets,"curvePresets":s.curve_presets,
        "recentKeywords":s.recent_keywords,"cacheMb":s.cache_mb,"slideshow":s.slideshow,"slideLayout":s.slide_layout,
        "slideOverlays":s.slide_overlays,"slideTitles":s.slide_titles,"holdView":s.hold_view})
}
struct Fixture {
    app: LightcraftApp,
    oracle: LightcraftApp,
    ctx: egui::Context,
    tick: u32,
    widgets: Vec<(String, egui::Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    trace: VecDeque<String>,
    baseline: Value,
    pending: Option<Value>,
    settings: Value,
    blocked: bool,
    probe: u8,
    previous_locale: crate::i18n::Locale,
}
impl Fixture {
    fn new() -> Self {
        let previous_locale = crate::i18n::language();
        crate::i18n::set_language(crate::i18n::Locale::En);
        let (mut app, oracle) = (model(), model());
        let baseline = document(&app);
        assert_eq!(baseline, document(&oracle));
        let settings = json!(app.ui.settings);
        app.run("dialog.labelNames", json!({})).unwrap();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        ctx.enable_accesskit();
        let mut f = Self {
            app,
            oracle,
            ctx,
            tick: 0,
            widgets: vec![],
            hits: vec![],
            trace: VecDeque::new(),
            baseline,
            pending: None,
            settings,
            blocked: false,
            probe: 0,
            previous_locale,
        };
        f.settle();
        f
    }
    fn safe(&self) {
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none() && self.app.session.media.rendered.disk().is_none());
        assert_eq!(self.app.session.catalog.photos().count(), 3);
        assert_eq!(self.app.renderer.live_workers(), 0);
        assert_eq!(self.app.renderer.in_flight(), 0);
        assert!(!self.app.session.segmenter.busy() && !self.app.session.denoise_busy());
        assert!(
            self.app.services.png.is_none()
                && self.app.services.pick_files.is_none()
                && self.app.services.pick_folder.is_none()
                && self.app.services.picker.is_none()
        );
        assert_eq!(json!(self.app.ui.settings), self.settings);
    }
    fn frame(&mut self, mut events: Vec<Event>) {
        self.safe();
        self.tick += 1;
        let mods = events
            .iter()
            .find_map(|event| match event {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or(Modifiers::NONE);
        events.insert(0, Event::ModifiersChanged(mods));
        let input = format!("{events:?}");
        let raw = HeadlessView::raw_input(egui::vec2(1400.0, 1000.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let ctx = self.ctx.clone();
        let (blocked, probe) = (self.blocked, self.probe);
        let _drain = Drain(self.ctx.run_ui(raw, |root| {
            crate::widgets::take_registries(&ctx);
            if probe == 0 {
                crate::dialogs::show(app, &ctx, blocked);
            } else {
                egui::CentralPanel::default().show(root, |ui| {
                    let mut draw = |ui: &mut egui::Ui| {
                        let mut dlg = app.ui.dialog.clone().expect("actual draft");
                        super::label_names_body(app, ui, &Tokens::get(&ctx), &mut dlg);
                        app.ui.dialog = Some(dlg);
                    };
                    match probe {
                        1 => craft::ui::runtime::measure::run(&ctx, || draw(ui)),
                        2 => {
                            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                            draw(&mut child);
                        }
                        _ => draw(ui),
                    }
                });
            }
        }));
        let registries = crate::widgets::take_registries(&self.ctx);
        self.widgets = registries.0;
        self.hits = registries.1;
        self.trace.push_back(format!(
            "frame={} probe={} blocked={} input={} focused={:?} sdk={:?} dialog={:?} widgets={:?}",
            self.tick,
            self.probe,
            self.blocked,
            input,
            self.ctx.memory(|m| m.focused()),
            craft::ui::runtime::focus::focused_rec(&self.ctx),
            self.app.ui.dialog,
            self.widgets
        ));
        while self.trace.len() > 8 {
            self.trace.pop_front();
        }
        let actual = document(&self.app);
        assert!(
            actual == self.baseline || self.pending.as_ref() == Some(&actual),
            "exact document/command/history preservation; trace={:#?}",
            self.trace
        );
        self.safe();
    }
    fn settle(&mut self) {
        for _ in 0..8 {
            self.frame(vec![]);
        }
    }
    fn rect(&self, id: &str) -> egui::Rect {
        let hits = self.widgets.iter().filter(|(name, _)| name == id).collect::<Vec<_>>();
        assert_eq!(hits.len(), 1, "one actual target {id}; trace={:#?}", self.trace);
        hits[0].1
    }
    fn pointer(&mut self, point: Pos2, pressed: bool) {
        self.frame(vec![
            Event::PointerMoved(point),
            Event::PointerButton { pos: point, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
        ]);
    }
    fn click(&mut self, id: &str) {
        self.tick += 30;
        self.frame(vec![Event::PointerMoved(self.rect(id).center())]);
        self.settle();
        let point = self.rect(id).center();
        self.pointer(point, true);
        self.pointer(point, false);
        self.settle();
    }
    fn key_mod(&mut self, key: Key, modifiers: Modifiers) {
        self.frame(vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }]);
        self.frame(vec![Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers }]);
        self.settle();
    }
    fn key(&mut self, key: Key) {
        self.key_mod(key, Modifiers::NONE);
    }
    fn text(&mut self, text: &str) {
        self.frame(vec![Event::Text(text.into())]);
        self.settle();
    }
    fn focused(&self, widget: &str) -> bool {
        let Some(native) = self.ctx.memory(|memory| memory.focused()) else {
            return false;
        };
        let Some(response) = self.ctx.read_response(native) else {
            return false;
        };
        if let Some(record) = craft::ui::runtime::focus::focused_rec(&self.ctx) {
            return record.stop.part.is_none()
                && native == record.stop.root
                && response.enabled()
                && response.has_focus()
                && self.hits.iter().filter(|(name, hit)| name == widget && hit.id == record.stop.id).count() == 1;
        }
        let rect = self.rect(widget);
        (response.rect.min - rect.min).length() < 0.5 && (response.rect.max - rect.max).length() < 0.5
    }
    fn draft(&self) -> Value {
        json!(self.app.ui.dialog)
    }
    fn names(&self) -> Vec<String> {
        let Some(Dialog::LabelNames { names, .. }) = &self.app.ui.dialog else { panic!("live LabelNames expected") };
        names.clone()
    }
    fn arm(&mut self, values: [&str; 5], save_as: &str, refusal: bool) {
        let mut names = serde_json::Map::new();
        for (key, value) in ["red", "yellow", "green", "blue", "purple"].into_iter().zip(values) {
            names.insert(key.into(), if value.trim().is_empty() { Value::Null } else { json!(value.trim()) });
        }
        self.oracle.run("label.setNames", json!({"names":names})).unwrap();
        if !save_as.trim().is_empty() {
            let result = self.oracle.run("label.saveSet", json!({"name":save_as.trim()}));
            assert_eq!(result.is_err(), refusal, "original saveSet outcome");
        } else {
            assert!(!refusal);
        }
        self.pending = Some(document(&self.oracle));
    }
    fn accepted(&mut self, closed: bool) {
        assert_eq!(self.app.ui.dialog.is_none(), closed);
        assert_eq!(document(&self.app), self.pending.take().unwrap());
        self.baseline = document(&self.app);
        self.settle();
    }
    fn replace(&mut self, alias: &str, value: &str) {
        self.click(alias);
        self.key_mod(Key::A, Modifiers::COMMAND);
        if value.is_empty() {
            self.key(Key::Backspace);
        } else {
            self.text(value);
        }
    }
    fn replace_names(&mut self, values: [&str; 5]) {
        for (color, value) in ["red", "yellow", "green", "blue", "purple"].into_iter().zip(values) {
            self.replace(&format!("field:labelName-{color}"), value);
        }
        assert_eq!(self.names(), values.map(str::to_string));
    }
    fn undo_redo(&mut self) {
        for command in ["edit.undo", "edit.redo"] {
            self.oracle.run(command, json!({})).unwrap();
            self.pending = Some(document(&self.oracle));
            self.app.run(command, json!({})).unwrap();
            self.frame(vec![]);
            assert_eq!(document(&self.app), self.pending.take().unwrap());
            self.baseline = document(&self.app);
            self.settle();
        }
    }
    fn select(&mut self, ids: Value) {
        for app in [&mut self.app, &mut self.oracle] {
            app.run("library.select", json!({"ids":ids})).unwrap();
        }
        self.baseline = document(&self.app);
        assert_eq!(self.baseline, document(&self.oracle));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.safe();
            let raw = HeadlessView::raw_input(egui::vec2(1400.0, 1000.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            let _drain = Drain(self.ctx.run_ui(raw, |_| {}));
            crate::widgets::take_registries(&self.ctx);
        }
        crate::i18n::set_language(self.previous_locale);
    }
}

const SEED: [&str; 5] = ["Seed Red", "Seed Yellow", "Seed Green", "Seed Blue", "Seed Purple"];
const REVIEW: [&str; 5] = ["Reject", "Needs Work", "Approved", "Retouch", "Print"];
const OTHER: [&str; 5] = ["Other Red", "Other Yellow", "Other Green", "Other Blue", "Other Purple"];
const CUSTOM: [&str; 5] = ["  Ready  ", "  ", "Sélection 🌿", "bLuE", "Retouch 2"];
#[test]
fn label_names_pointer_all_rows_trim_empty_and_colour_defaults_keep_photos_and_undo() {
    let mut f = Fixture::new();
    assert_eq!(f.names(), SEED.map(str::to_string));
    f.replace_names(CUSTOM);
    f.arm(CUSTOM, "", false);
    f.click("button:dialogOk");
    f.accepted(true);
    assert_eq!(f.app.session.catalog.custom_label_name(lightcraft_catalog::ColorLabel::Red), Some("Ready"));
    assert_eq!(f.app.session.catalog.custom_label_name(lightcraft_catalog::ColorLabel::Yellow), None);
    assert_eq!(f.app.session.catalog.custom_label_name(lightcraft_catalog::ColorLabel::Blue), None);
    f.undo_redo();
}
#[test]
fn label_names_builtin_and_user_choices_only_preview_until_save_and_escape_discards() {
    let mut f = Fixture::new();
    for (set, expected) in [("Colors", [""; 5]), ("Review", REVIEW), ("Other", OTHER), ("Kept", SEED)] {
        f.click(&format!("button:labelSet-{set}"));
        assert_eq!(f.names(), expected.map(str::to_string));
        assert_eq!(document(&f.app), f.baseline);
    }
    f.replace("field:labelName-red", "Unsaved");
    f.replace("field:labelSetName", "Unsaved set");
    f.key(Key::Escape);
    assert!(f.app.ui.dialog.is_none());
    assert_eq!(document(&f.app), f.baseline);
}
#[test]
fn label_names_saved_choice_override_save_new_set_and_undo_names_not_preferences() {
    let mut f = Fixture::new();
    f.click("button:labelSet-Other");
    assert_eq!(f.names(), OTHER.map(str::to_string));
    f.replace("field:labelName-green", "Chosen Green");
    f.replace("field:labelSetName", "  New proof  ");
    f.arm(["Other Red", "Other Yellow", "Chosen Green", "Other Blue", "Other Purple"], "  New proof  ", false);
    f.click("button:dialogOk");
    f.accepted(true);
    assert_eq!(f.app.session.label_sets.len(), 3);
    let stored = json!(f.app.session.label_sets);
    f.undo_redo();
    assert_eq!(json!(f.app.session.label_sets), stored);
}
#[test]
fn label_names_overwrite_user_set_case_insensitively_preserves_other_set_and_undo() {
    let mut f = Fixture::new();
    let other = json!(f.app.session.label_sets[1]);
    f.click("button:labelSet-Review");
    f.replace("field:labelSetName", "  kEpT  ");
    f.arm(REVIEW, "  kEpT  ", false);
    f.click("button:dialogOk");
    f.accepted(true);
    assert_eq!(f.app.session.label_sets.len(), 2);
    assert_eq!(f.app.session.label_sets[0].name, "kEpT");
    assert_eq!(f.app.session.label_sets[0].names, REVIEW.map(str::to_string));
    assert_eq!(json!(f.app.session.label_sets[1]), other);
    f.undo_redo();
}
#[test]
fn label_names_builtin_save_refusal_closes_after_name_commit_then_reopen_correction_and_undo() {
    let mut f = Fixture::new();
    f.replace_names(CUSTOM);
    f.replace("field:labelSetName", "  rEvIeW  ");
    let before_undo = f.app.session.undo.len();
    f.arm(CUSTOM, "  rEvIeW  ", true);
    f.click("button:dialogOk");
    f.accepted(true);
    assert_eq!(f.app.session.undo.len(), before_undo + 1);
    assert_eq!(f.app.session.label_sets.len(), 2);
    f.app.run("dialog.labelNames", json!({})).unwrap();
    f.settle();
    let normalized = ["Ready", "", "Sélection 🌿", "", "Retouch 2"];
    assert_eq!(f.names(), normalized.map(str::to_string));
    f.replace("field:labelSetName", "Corrected proof");
    f.arm(normalized, "Corrected proof", false);
    f.click("button:dialogOk");
    f.accepted(true);
    assert_eq!(f.app.session.undo.len(), before_undo + 1, "saveSet is preferences, not an extra Undo entry");
    assert_eq!(f.app.session.label_sets.len(), 3);
    f.undo_redo();
}
#[test]
fn label_names_cancel_and_field_return_do_not_save_and_empty_selection_can_apply() {
    let mut f = Fixture::new();
    f.replace("field:labelName-red", "Not saved by Return");
    let draft = f.draft();
    f.key(Key::Enter);
    assert_eq!(f.draft(), draft, "raw native singleline Return never confirms this dialog");
    assert_eq!(document(&f.app), f.baseline);
    f.click("button:dialogCancel");
    assert!(f.app.ui.dialog.is_none());
    assert_eq!(document(&f.app), f.baseline);
    f.select(json!([]));
    f.app.run("dialog.labelNames", json!({})).unwrap();
    f.settle();
    f.click("button:labelSet-Review");
    f.arm(REVIEW, "", false);
    f.click("button:dialogOk");
    f.accepted(true);
    assert!(f.app.session.selection.ids.is_empty());
    f.undo_redo();
}
#[test]
fn label_names_natural_tab_reverse_tab_through_set_choices_six_inputs_and_footer() {
    let mut f = Fixture::new();
    let order = [
        "button:labelSet-Colors",
        "button:labelSet-Review",
        "button:labelSet-Kept",
        "button:labelSet-Other",
        "field:labelName-red",
        "field:labelName-yellow",
        "field:labelName-green",
        "field:labelName-blue",
        "field:labelName-purple",
        "field:labelSetName",
        "button:dialogCancel",
        "button:dialogOk",
    ];
    f.click(order[0]);
    // Pointer activation does not take focus; start keyboard traversal with a real Tab.
    f.key(Key::Tab);
    assert!(f.focused(order[0]));
    for target in order.iter().skip(1).chain(std::iter::once(&order[0])) {
        f.key(Key::Tab);
        assert!(f.focused(target), "forward Tab requires {target}; trace={:#?}", f.trace);
    }
    f.key_mod(Key::Tab, Modifiers::SHIFT);
    assert!(f.focused("button:dialogOk"));
    f.key_mod(Key::Tab, Modifiers::SHIFT);
    assert!(f.focused("button:dialogCancel"));
    f.key(Key::Enter);
    assert!(f.app.ui.dialog.is_none());
    assert_eq!(document(&f.app), f.baseline);
}
#[test]
fn label_names_disabled_parent_and_isolated_nonreal_leaf_keep_draft_stale_release_then_fresh_choice() {
    for mode in 0..=2 {
        let mut f = Fixture::new();
        if mode != 0 {
            f.probe = 3;
            f.settle();
        }
        f.click("field:labelName-red");
        f.key_mod(Key::A, Modifiers::COMMAND);
        let before = f.draft();
        let review_point = f.rect("button:labelSet-Review").center();
        if mode == 0 {
            f.blocked = true;
        } else {
            f.probe = mode;
        }
        f.text("Forbidden edit");
        f.key(Key::Enter);
        f.frame(vec![Event::PointerMoved(review_point)]);
        f.settle();
        f.pointer(review_point, true);
        f.pointer(review_point, false);
        f.settle();
        assert_eq!(f.draft(), before, "nonreal/disabled mode={mode}; trace={:#?}", f.trace);
        if mode == 0 {
            f.blocked = false;
        } else {
            f.probe = 3;
        }
        f.settle();
        let point = f.rect("button:labelSet-Review").center();
        if mode == 0 {
            f.blocked = true;
        } else {
            f.probe = mode;
        }
        f.pointer(point, true);
        if mode == 0 {
            f.blocked = false;
        } else {
            f.probe = 3;
        }
        f.pointer(point, false);
        f.settle();
        assert_eq!(f.draft(), before, "a press from a nonreal/disabled pass cannot activate on release");
        f.click("button:labelSet-Review");
        assert_eq!(f.names(), REVIEW.map(str::to_string), "fresh actual activation is still required");
        assert_eq!(document(&f.app), f.baseline);
        if mode != 0 {
            f.probe = 0;
            f.settle();
        }
        f.click("button:dialogCancel");
        assert!(f.app.ui.dialog.is_none());
        assert_eq!(document(&f.app), f.baseline);
    }
}

/// Additive positive discriminator, not a replacement for any of the eight native comparisons.
#[test]
fn label_names_actual_parent_publishes_six_enabled_sdk_text_field_owners() {
    let mut f = Fixture::new();
    let initial_draft = f.draft();
    let mut elements = vec![];
    for _ in 0..8 {
        f.safe();
        f.tick += 1;
        assert!(
            !super::settings::native_controls_active(&f.ctx),
            "actual generic parent defaults to SDK"
        );
        let raw = HeadlessView::raw_input(
            egui::vec2(1400.0, 1000.0),
            1.0,
            f64::from(f.tick) / 60.0,
            vec![],
        );
        let app = &mut f.app;
        let ctx = f.ctx.clone();
        let _drain = Drain(ctx.run_ui(raw, |root| {
            crate::widgets::take_registries(&ctx);
            let (_, recorded) = craft::ui::runtime::inspect::record(
                root,
                egui::Id::unique("label-names-sdk-route"),
                |_| crate::dialogs::show(app, &ctx, false),
            );
            elements = recorded
                .map(|recording| recording.elements.clone())
                .unwrap_or_default();
        }));
        crate::widgets::take_registries(&ctx);
        assert_eq!(document(&f.app), f.baseline);
        assert_eq!(f.draft(), initial_draft);
        f.safe();
    }
    let inputs = elements
        .iter()
        .filter(|element| element.part.is_none() && element.component.starts_with("TextField"))
        .collect::<Vec<_>>();
    assert_eq!(
        inputs.len(),
        6,
        "six actual SDK text input owners; records={elements:#?}"
    );
    for name in ["Red", "Yellow", "Green", "Blue", "Purple", "Save as set"] {
        let owners = inputs
            .iter()
            .filter(|element| {
                element
                    .facts
                    .as_ref()
                    .is_some_and(|facts| facts.name.as_deref() == Some(name) && !facts.disabled)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            owners.len(),
            1,
            "one actual enabled SDK owner for {name}; records={elements:#?}"
        );
    }
}

/// Additional reverse traversal coverage; the original comparison reverses only the two footer stops.
#[test]
fn label_names_reverse_tab_all_twelve_owners_preserves_draft_and_escapes() {
    let mut f = Fixture::new();
    let draft = f.draft();
    let order = [
        "button:labelSet-Colors",
        "button:labelSet-Review",
        "button:labelSet-Kept",
        "button:labelSet-Other",
        "field:labelName-red",
        "field:labelName-yellow",
        "field:labelName-green",
        "field:labelName-blue",
        "field:labelName-purple",
        "field:labelSetName",
        "button:dialogCancel",
        "button:dialogOk",
    ];
    f.key(Key::Tab);
    assert!(f.focused(order[0]));
    for target in order.iter().rev() {
        f.key_mod(Key::Tab, Modifiers::SHIFT);
        assert!(
            f.focused(target),
            "reverse Tab requires {target}; trace={:#?}",
            f.trace
        );
        assert_eq!(f.draft(), draft);
    }
    f.key(Key::Escape);
    assert!(f.app.ui.dialog.is_none());
    assert_eq!(document(&f.app), f.baseline);
}
