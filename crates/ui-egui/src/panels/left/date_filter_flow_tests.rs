//! Actual By Date section and catalog grouping; genuine six-photo catalog and independent original source/filter command oracle.
//! No whole sidebar/AppShell, tick, file checks, picker, provider, GPU or persistence path is invoked.
use super::*;
use crate::{Services, headless::HeadlessView, sidebar::SidebarSection};
use egui::{Event, Key, Modifiers, PointerButton, Pos2};
use lightcraft_catalog::{HistoryStep, Op, Photo, PhotoId, Source, Version};
use lightcraft_develop::DevelopSettings;
use lightcraft_engine::{Selection, Session};
use serde_json::Value;
use std::{collections::VecDeque, sync::Arc};
const GROUPS: [&str; 5] = ["2026", "2026-04", "2026-03", "2025", "2025-03"];
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
fn model(empty: bool) -> LightcraftApp {
    let mut session = Session::new();
    session.clock = Box::new(|| CLOCK.into());
    session.xmp.auto_write = false;
    for index in 0..6 {
        let id = PhotoId(101 + index);
        let mut p = Photo::new(id, Source::Demo { scene: index as u32 }, &format!("Protected photo {index}"), "JPEG", 8, 6, CLOCK);
        let mut d = DevelopSettings::default();
        d.light.contrast = index as f64 * 7.0;
        p.develop = Arc::new(d);
        p.local = index == 5;
        let captures = [
            Some("2026-03-14T10:00:00Z"),
            Some("2026-03-15T11:00:00Z"),
            Some("2026-04-01T12:00:00Z"),
            Some("2025-03-14T10:00:00Z"),
            None,
            Some("2024-02-20T10:00:00Z"),
        ];
        p.captured = if empty { None } else { captures[index as usize].map(str::to_owned) };
        p.meta.title = format!("Title {index}");
        p.meta.caption = format!("Caption {index}");
        p.meta.keywords = match index {
            0 => vec!["Places|Harbour".into(), "Keep-0".into()],
            1 => vec!["Target".into(), "Places|Harbour|Night".into(), "Keep-1".into()],
            _ => vec!["Places|Harbour".into(), "Keep-2".into()],
        };
        p.history = vec![HistoryStep { label: format!("Old {index}"), settings: p.develop.clone() }];
        p.versions = vec![Version { name: format!("Saved {index}"), created: CLOCK.into(), settings: p.develop.clone(), auto: false }];
        session.commit("Fixture", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    session.selection = Selection::single(PhotoId(101));
    let mut app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
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
    app.run("library.select", json!({"ids":[101,102],"active":101})).unwrap();
    assert!(!app.session.undo.is_empty() && !app.session.redo.is_empty());
    app.ui.settings.gpu = false;
    app.ui.settings.external_editor = "protected preference".into();
    app.ui.show_counts = true;
    app
}
fn history(entries: &[lightcraft_engine::UndoEntry]) -> Value {
    json!(entries.iter().map(|entry| json!({"label":entry.label,"op":entry.op,"folder":format!("{:?}",entry.folder)})).collect::<Vec<_>>())
}
fn document_core(app: &LightcraftApp) -> Value {
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
fn document(app: &LightcraftApp) -> Value {
    json!({"core":document_core(app),"ui":app.ui})
}
fn action(key: &str) -> String {
    format!("source:date:{key}")
}
fn draw_section(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let rows = app.caches.date_groups(&app.session.catalog).len();
    super::super::sections::section(app, ui, SidebarSection::ByDate, rows, date_rows);
}
struct Fixture {
    app: LightcraftApp,
    oracle: LightcraftApp,
    ctx: egui::Context,
    tick: u32,
    widgets: Vec<(String, egui::Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    elements: Vec<craft::ui::runtime::inspect::Element>,
    trace: VecDeque<String>,
    baseline: Value,
    pending: Option<Value>,
    ui_baseline: Value,
    ui_pending: Option<Value>,
    settings: Value,
    blocked: bool,
    probe: u8,
    previous_locale: crate::i18n::Locale,
    expected_open: std::collections::BTreeSet<String>,
    pending_open: Option<std::collections::BTreeSet<String>>,
}
impl Fixture {
    fn new() -> Self {
        Self::with_dates(false)
    }
    fn with_dates(empty: bool) -> Self {
        let previous_locale = crate::i18n::language();
        crate::i18n::set_language(crate::i18n::Locale::En);
        let (app, oracle) = (model(empty), model(empty));
        let baseline = document(&app);
        assert_eq!(baseline, document(&oracle));
        let settings = json!(app.ui.settings);
        let ui_baseline = json!(app.ui.dialog);
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
            elements: vec![],
            trace: VecDeque::new(),
            baseline,
            pending: None,
            ui_baseline,
            ui_pending: None,
            settings,
            blocked: false,
            probe: 0,
            previous_locale,
            expected_open: Default::default(),
            pending_open: None,
        };
        f.settle();
        f
    }
    fn safe(&self) {
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none() && self.app.session.media.rendered.disk().is_none());
        assert_eq!(self.app.session.catalog.photos().count(), 6);
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
        assert_eq!(super::super::local::fs_cached_running(&self.ctx), 0);
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
        let raw = HeadlessView::raw_input(egui::vec2(1000.0, 800.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let ctx = self.ctx.clone();
        let (blocked, probe) = (self.blocked, self.probe);
        let mut elements = vec![];
        let _drain = Drain(ctx.run_ui(raw, |root| {
            crate::widgets::take_registries(&ctx);
            egui::Panel::left("by-date-workflow-left").resizable(false).default_size(360.0).size_range(360.0..=360.0).show(root, |ui| {
                if blocked {
                    ui.disable();
                }
                let mut draw = |ui: &mut egui::Ui| {
                    let (_, recorded) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("by-date-recording"), |ui| draw_section(app, ui));
                    elements = recorded.map(|recording| recording.elements.clone()).unwrap_or_default();
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
        }));
        let registries = crate::widgets::take_registries(&ctx);
        self.widgets = registries.0;
        self.hits = registries.1;
        self.elements = elements;
        self.trace.push_back(format!(
            "frame={} probe={} blocked={} input={} focused={:?} sdk={:?} dialog={:?} widgets={:?} lead={:?} actual_owners={:?}",
            self.tick,
            self.probe,
            self.blocked,
            input,
            self.ctx.memory(|m| m.focused()),
            craft::ui::runtime::focus::focused_rec(&self.ctx),
            self.app.ui.dialog,
            self.widgets,
            self.collection_lead(),
            self.elements
                .iter()
                .filter(|element| element.part.is_none())
                .map(|element| (element.id, element.component, element.facts.as_ref().map(|facts| (facts.role.clone(), facts.name.as_deref()))))
                .collect::<Vec<_>>()
        ));
        while self.trace.len() > 32 {
            self.trace.pop_front();
        }
        let actual = document(&self.app);
        assert!(
            actual == self.baseline || self.pending.as_ref() == Some(&actual),
            "full catalog/document/selection/preferences/history/command guard; trace={:#?}",
            self.trace
        );
        let actual_ui = json!(self.app.ui.dialog);
        assert!(
            actual_ui == self.ui_baseline || self.ui_pending.as_ref() == Some(&actual_ui),
            "no unrelated dialog dispatch; trace={:#?}",
            self.trace
        );
        let opened = self.opened();
        assert!(
            opened == self.expected_open || self.pending_open.as_ref() == Some(&opened),
            "exact app-owned expansion state; trace={:#?}",
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
    fn pointer(&mut self, point: Pos2, button: PointerButton, pressed: bool) {
        self.frame(vec![Event::PointerMoved(point), Event::PointerButton { pos: point, button, pressed, modifiers: Modifiers::NONE }]);
    }
    fn click(&mut self, id: &str) {
        self.click_button(id, PointerButton::Primary);
    }
    fn click_button(&mut self, id: &str, button: PointerButton) {
        self.tick += 30;
        self.frame(vec![Event::PointerMoved(self.rect(id).center())]);
        self.settle();
        let p = self.rect(id).center();
        self.pointer(p, button, true);
        self.pointer(p, button, false);
        self.settle();
    }
    fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }]);
        }
        self.settle();
    }

    fn focused(&self, id: &str) -> bool {
        if id.starts_with("source:date:") {
            if let Some(root) = self.collection_root() {
                // List owns one real native/SDK stop; its public lead is the
                // identity of the current row, not an egui focus on that row.
                let root_focused = self.ctx.memory(|m| m.focused()) == Some(root.id)
                    && craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|rec| rec.stop.id == root.id && rec.stop.part.is_none());
                let rows = self.widgets.iter().filter(|(name, _)| name.starts_with("source:date:")).collect::<Vec<_>>();
                return root_focused && self.collection_lead().and_then(|at| rows.get(at)).is_some_and(|(name, _)| name == id);
            }
        }
        let Some(native) = self.ctx.memory(|m| m.focused()) else { return false };
        let Some(response) = self.ctx.read_response(native) else { return false };
        response.enabled()
            && response.has_focus()
            && (self.hits.iter().any(|(name, hit)| name == id && hit.id == native) || response.rect == self.rect(id))
    }
    fn collection_root(&self) -> Option<&craft::ui::runtime::inspect::Element> {
        let roots = self
            .elements
            .iter()
            .filter(|element| {
                element.part.is_none() && element.facts.as_ref().is_some_and(|facts| facts.role == "List" && facts.name.as_deref() == Some("By Date"))
            })
            .collect::<Vec<_>>();
        assert!(roots.len() <= 1, "at most one real recorded By Date collection: {:#?}", self.elements);
        roots.into_iter().next()
    }
    /// Whether the focus is on the section header's list (named "Sidebar section").
    fn header_focused(&self) -> bool {
        let focused = self.ctx.memory(|m| m.focused());
        self.elements.iter().any(|element| {
            element.part.is_none()
                && Some(element.id) == focused
                && element.facts.as_ref().is_some_and(|facts| facts.role == "List" && facts.name.as_deref() == Some("Sidebar section"))
        })
    }
    fn collection_lead(&self) -> Option<usize> {
        // Public, read-only collection fact; root ID comes from its actual
        // recorded role/name, never a reconstructed widget-ID recipe.
        self.collection_root().and_then(|root| craft::ui::contract::lead_position(&self.ctx, root.id))
    }
    fn enter_dates(&mut self) {
        self.key(Key::Tab);
        if self.header_focused() {
            // The section header is a one-row framework list with a Tab stop of its own.
            self.key(Key::Tab);
        }
        let root = self.collection_root().expect("a Tab must reach the supported SDK collection");
        assert_eq!(self.ctx.memory(|m| m.focused()), Some(root.id), "actual SDK root, not missing focus; trace={:#?}", self.trace);
        if self.collection_lead().is_none() {
            // An unselected List starts without a lead (LeadOnOpen::Selected).
            // Real Down enters its first row; no synthetic focus or pick.
            self.key(Key::ArrowDown);
        }
        assert!(
            self.focused(&action("2026")),
            "natural entry reaches the exact first-year identity through real focus/lead; trace={:#?}",
            self.trace
        );
    }
    fn arm(&mut self, command: &str, params: Value) {
        self.oracle.run(command, params).unwrap();
        self.pending = Some(document(&self.oracle));
        assert_ne!(self.pending.as_ref().unwrap(), &self.baseline, "the original command must make a genuine change");
    }
    fn accepted(&mut self) {
        assert_eq!(document(&self.app), self.pending.take().unwrap());
        self.baseline = document(&self.app);
        self.settle();
    }

    fn run_both(&mut self, command: &str, params: Value) {
        self.oracle.run(command, params.clone()).unwrap();
        self.pending = Some(document(&self.oracle));
        self.app.run(command, params).unwrap();
        self.frame(vec![]);
        self.accepted();
    }
    fn opened(&self) -> std::collections::BTreeSet<String> {
        GROUPS
            .iter()
            .filter(|key| self.ctx.data(|data| data.get_temp::<bool>(egui::Id::unique(("date-open", key.to_string())))).unwrap_or(false))
            .map(|key| key.to_string())
            .collect()
    }
    fn toggle(&mut self, key: &str) {
        let mut expected = self.expected_open.clone();
        if !expected.remove(key) {
            expected.insert(key.into());
        }
        self.pending_open = Some(expected);
        self.click(&format!("dateToggle:{key}"));
        assert_eq!(self.opened(), self.pending_open.take().unwrap());
        self.expected_open = self.opened();
        self.settle();
    }
    fn arm_date(&mut self, key: &str) {
        let on = self.oracle.session.filter.date.as_deref() != Some(key);
        if on && self.oracle.session.source != LibrarySource::All {
            self.oracle.run("library.source", json!({"kind":"all"})).unwrap();
        }
        self.oracle.run("library.filter", json!({"date":if on {json!(key)}else{Value::Null}})).unwrap();
        self.pending = Some(document(&self.oracle));
        assert_ne!(self.pending.as_ref().unwrap(), &self.baseline, "real original source/filter transition");
    }
    fn choose(&mut self, key: &str) {
        self.arm_date(key);
        self.click(&action(key));
        self.accepted();
    }
    fn visible(&mut self) -> std::collections::BTreeSet<u64> {
        self.app.session.visible_cloned().iter().map(|id| id.0).collect()
    }
    fn undo_redo(&mut self) {
        for cmd in ["edit.undo", "edit.redo"] {
            self.run_both(cmd, json!({}));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.safe();
            assert_eq!(document(&self.app), self.baseline);
        }
        crate::i18n::set_language(self.previous_locale);
    }
}

#[test]
fn date_groups_use_actual_catalog_counts_and_empty_hint() {
    let mut f = Fixture::new();
    let groups = f.app.session.catalog.date_groups();
    assert_eq!(groups.iter().map(|g| (g.year.as_str(), g.count)).collect::<Vec<_>>(), vec![("2026", 3), ("2025", 1)]);
    assert_eq!(groups[0].months, vec![("2026-04".into(), 1), ("2026-03".into(), 2)]);
    f.rect(&action("2026"));
    f.rect(&action("2025"));
    assert!(!f.widgets.iter().any(|(name, _)| name == &action("2024")));
    assert!(!f.widgets.iter().any(|(name, _)| name == &action("2026-03")));
    f.toggle("2026");
    f.toggle("2026-03");
    f.rect(&action("2026-03-14"));
    assert!(!f.widgets.iter().any(|(name, _)| name == "dateToggle:2026-03-14"));
    drop(f);
    let empty = Fixture::with_dates(true);
    assert!(empty.app.session.catalog.date_groups().is_empty());
    empty.rect("sidebarEmpty:byDate");
    assert!(!empty.widgets.iter().any(|(name, _)| name.starts_with("source:date:")));
}
#[test]
fn date_year_from_album_uses_exact_original_source_filter_and_preserves_histories() {
    let mut f = Fixture::new();
    f.run_both("library.source", json!({"kind":"album","id":2}));
    let catalog = json!(f.app.session.catalog);
    let undo = history(&f.app.session.undo);
    let redo = history(&f.app.session.redo);
    f.choose("2026");
    assert_eq!(f.app.session.source, LibrarySource::All);
    assert_eq!(f.visible(), [101, 102, 103].into_iter().collect());
    assert_eq!(json!(f.app.session.catalog), catalog);
    assert_eq!(history(&f.app.session.undo), undo);
    assert_eq!(history(&f.app.session.redo), redo);
}
#[test]
fn date_same_month_caption_in_distinct_years_keeps_full_prefix_identity() {
    let mut f = Fixture::new();
    f.toggle("2026");
    f.toggle("2025");
    assert_eq!(crate::i18n::date_group_label("2026-03", true), crate::i18n::date_group_label("2025-03", true));
    let a = f.rect(&action("2026-03"));
    let b = f.rect(&action("2025-03"));
    assert_ne!(a, b);
    f.choose("2025-03");
    assert_eq!(f.app.session.filter.date.as_deref(), Some("2025-03"));
    assert_eq!(f.visible(), [104].into_iter().collect());
    f.choose("2026-03");
    assert_eq!(f.app.session.filter.date.as_deref(), Some("2026-03"));
    assert_eq!(f.visible(), [101, 102].into_iter().collect());
}
#[test]
fn date_day_filter_uses_original_command_and_exact_photo_identity() {
    let mut f = Fixture::new();
    f.toggle("2026");
    f.toggle("2026-03");
    f.choose("2026-03-14");
    assert_eq!(f.visible(), [101].into_iter().collect());
    assert_eq!(f.app.session.filter.date.as_deref(), Some("2026-03-14"));
}
#[test]
fn date_selected_prefix_clears_without_gratuitous_source_reset() {
    let mut f = Fixture::new();
    f.run_both("library.source", json!({"kind":"album","id":2}));
    f.run_both("library.filter", json!({"date":"2026"}));
    assert!(matches!(f.app.session.source, LibrarySource::Album(_)));
    f.choose("2026");
    assert!(f.app.session.filter.date.is_none());
    assert!(matches!(f.app.session.source, LibrarySource::Album(_)));
}
#[test]
fn date_collapse_reopen_preserves_active_filter_and_descendant_identity() {
    let mut f = Fixture::new();
    f.toggle("2026");
    f.toggle("2026-03");
    f.choose("2026-03-14");
    let before = document(&f.app);
    f.toggle("2026");
    assert!(!f.widgets.iter().any(|(name, _)| name == &action("2026-03")));
    f.toggle("2026");
    f.rect(&action("2026-03-14"));
    assert_eq!(document(&f.app), before);
    f.toggle("2026-03");
    assert!(!f.widgets.iter().any(|(name, _)| name == &action("2026-03-14")));
    f.toggle("2026-03");
    f.rect(&action("2026-03-14"));
}
#[test]
fn date_photo_count_preference_preserves_row_filter_and_full_history() {
    let mut f = Fixture::new();
    f.toggle("2026");
    f.rect("count:date:2026");
    f.rect("count:date:2026-03");
    f.run_both("view.photoCounts", json!({"show":false}));
    assert!(!f.widgets.iter().any(|(name, _)| name.starts_with("count:date:")));
    f.rect(&action("2026"));
    f.rect(&action("2026-03"));
    f.run_both("view.photoCounts", json!({"show":true}));
    f.rect("count:date:2026");
    f.choose("2026-03");
}
#[test]
fn date_natural_tab_enter_activates_actual_first_year() {
    let mut f = Fixture::new();
    f.enter_dates();
    f.arm_date("2026");
    f.key(Key::Enter);
    f.accepted();
    assert_eq!(f.visible(), [101, 102, 103].into_iter().collect());
}
#[test]
fn date_natural_tree_arrows_expand_descend_collapse_and_activate() {
    let mut f = Fixture::new();
    f.enter_dates();
    let mut expected = f.expected_open.clone();
    expected.insert("2026".into());
    f.pending_open = Some(expected);
    f.key(Key::ArrowRight);
    assert_eq!(f.opened(), f.pending_open.take().unwrap());
    f.expected_open = f.opened();
    f.key(Key::ArrowDown);
    assert!(f.focused(&action("2026-04")), "natural next month identity; trace={:#?}", f.trace);
    f.key(Key::ArrowDown);
    assert!(f.focused(&action("2026-03")), "second month identity; trace={:#?}", f.trace);
    let mut expected = f.expected_open.clone();
    expected.insert("2026-03".into());
    f.pending_open = Some(expected);
    f.key(Key::ArrowRight);
    assert_eq!(f.opened(), f.pending_open.take().unwrap());
    f.expected_open = f.opened();
    f.key(Key::ArrowDown);
    assert!(f.focused(&action("2026-03-15")), "newest first actual day; trace={:#?}", f.trace);
    f.arm_date("2026-03-15");
    f.key(Key::Enter);
    f.accepted();
    assert_eq!(f.visible(), [102].into_iter().collect());
    f.key(Key::ArrowLeft);
    assert!(f.focused(&action("2026-03")), "back reaches actual parent; trace={:#?}", f.trace);
    let mut expected = f.expected_open.clone();
    expected.remove("2026-03");
    f.pending_open = Some(expected);
    f.key(Key::ArrowLeft);
    assert_eq!(f.opened(), f.pending_open.take().unwrap());
    f.expected_open = f.opened();
    f.key(Key::Escape);
}
#[test]
fn date_disabled_measure_sizing_stale_row_release_then_fresh_positive() {
    for mode in 0..3 {
        let mut f = Fixture::new();
        let before = document(&f.app);
        let point = f.rect(&action("2026")).center();
        f.blocked = mode == 0;
        f.probe = mode;
        f.settle();
        f.pointer(point, PointerButton::Primary, true);
        f.pointer(point, PointerButton::Primary, false);
        f.key(Key::Enter);
        f.key(Key::Space);
        assert_eq!(document(&f.app), before, "nonreal row mode {mode}");
        f.pointer(point, PointerButton::Primary, true);
        f.blocked = false;
        f.probe = 0;
        f.frame(vec![]);
        f.pointer(point, PointerButton::Primary, false);
        f.settle();
        assert_eq!(document(&f.app), before, "no stale restored row activation mode {mode}");
        f.choose("2025");
        assert_eq!(f.visible(), [104].into_iter().collect());
    }
}
#[test]
fn date_disabled_measure_sizing_disclosure_preserves_open_state_then_fresh_positive() {
    for mode in 0..3 {
        let mut f = Fixture::new();
        let point = f.rect("dateToggle:2026").center();
        f.blocked = mode == 0;
        f.probe = mode;
        f.settle();
        f.pointer(point, PointerButton::Primary, true);
        f.pointer(point, PointerButton::Primary, false);
        assert!(f.opened().is_empty(), "nonreal disclosure mode {mode}");
        f.pointer(point, PointerButton::Primary, true);
        f.blocked = false;
        f.probe = 0;
        f.frame(vec![]);
        f.pointer(point, PointerButton::Primary, false);
        f.settle();
        assert!(f.opened().is_empty(), "no stale disclosure activation mode {mode}");
        f.toggle("2026");
        f.rect(&action("2026-03"));
    }
}
#[test]
fn date_cache_rebuild_keeps_real_prefix_identity_and_original_dispatch() {
    let mut f = Fixture::new();
    f.toggle("2026");
    f.toggle("2025");
    let a = f.rect(&action("2026-03"));
    let b = f.rect(&action("2025-03"));
    f.app.caches = Default::default();
    f.settle();
    assert_eq!(f.rect(&action("2026-03")), a);
    assert_eq!(f.rect(&action("2025-03")), b);
    f.choose("2025-03");
    assert_eq!(f.visible(), [104].into_iter().collect());
    f.choose("2026-03");
    assert_eq!(f.visible(), [101, 102].into_iter().collect());
}

#[test]
fn date_actual_section_has_supported_list_and_identity_keyed_tree_row_owners() {
    let mut f = Fixture::new();
    f.toggle("2026");
    f.toggle("2025");
    let lists = f
        .elements
        .iter()
        .filter(|element| {
            element.part.is_none() && element.facts.as_ref().is_some_and(|facts| facts.role == "List" && facts.name.as_deref() == Some("By Date"))
        })
        .collect::<Vec<_>>();
    assert_eq!(lists.len(), 1, "one actual collection owns this live section; trace={:#?}", f.trace);
    // (the section header is a row of its own list, named like the section)
    let rows = f
        .elements
        .iter()
        .filter(|element| element.part.is_none() && element.component.starts_with("ListRow"))
        .filter(|element| !element.facts.as_ref().is_some_and(|facts| facts.name.as_deref() == Some("By Date")))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 5, "two roots and their three actual months; trace={:#?}", f.trace);
    assert_eq!(rows.iter().filter(|element| element.facts.as_ref().is_some_and(|facts| facts.name.as_deref() == Some("March"))).count(), 2);
    let root = lists[0];
    let root_facts = root.facts.as_ref().expect("real collection root facts");
    assert!(!root_facts.disabled && root_facts.interactive, "enabled actionable collection root: {root:#?}");
    assert!(root_facts.actions.contains(&"click") && root_facts.actions.contains(&"focus"));
    assert!(root.stop.is_some(), "the collection is the real single Tab owner");
    assert_eq!(f.ctx.memory(|memory| memory.focused()), Some(root.id));
    assert!(f.focused(&action("2025")), "the second actual disclosure click put the public lead on its full-prefix row");
    let mut actual_ids = std::collections::HashSet::new();
    for (key, label) in [("2026", "2026"), ("2026-04", "April"), ("2026-03", "March"), ("2025", "2025"), ("2025-03", "March")] {
        let alias = f.rect(&action(key));
        let owned = rows.iter().filter(|row| row.rect == alias).collect::<Vec<_>>();
        assert_eq!(owned.len(), 1, "full-prefix {key} binds exactly one actual SDK face");
        let row = owned[0];
        assert!(actual_ids.insert(row.id), "different full prefixes have different actual row owners, including the two March captions");
        let facts = row.facts.as_ref().expect("actual tree row facts");
        assert_eq!(facts.role, "TreeItem");
        assert_eq!(facts.name.as_deref(), Some(label));
        assert!(!facts.disabled, "enabled collection-owned TreeItem face: {row:#?}");
        // (the framework marks a tree row interactive for its AccessKit actions; the row is still not a Tab stop of its own, below)
        assert!(row.stop.is_none(), "rows share the collection's one Tab owner");
        let open_action = if f.expected_open.contains(key) { "collapse" } else { "expand" };
        // (the framework's row also lists its click action now)
        assert!(facts.actions.contains(&open_action), "face exposes the actual tree state action for {key}: {:?}", facts.actions);
        let parts = f
            .elements
            .iter()
            .filter(|element| {
                element.id == row.id
                    && element.part.is_some()
                    && element
                        .facts
                        .as_ref()
                        .is_some_and(|facts| facts.role == "Button" && facts.actions.contains(&"click") && facts.actions.contains(&open_action))
            })
            .collect::<Vec<_>>();
        assert_eq!(parts.len(), 1, "exactly one real same-owner disclosure action for {key}");
        let disclosure = parts[0];
        let facts = disclosure.facts.as_ref().unwrap();
        assert!(!facts.disabled && facts.interactive, "enabled actionable disclosure, not an inactive face: {disclosure:#?}");
        assert_eq!(disclosure.rect, f.rect(&format!("dateToggle:{key}")), "actual part and public opener geometry bind the same full-prefix action");
        assert!(root.rect.contains(row.rect.center()));
    }
    assert_eq!(actual_ids.len(), 5);
    f.choose("2025-03");
    assert_eq!(f.visible(), [104].into_iter().collect());
}
