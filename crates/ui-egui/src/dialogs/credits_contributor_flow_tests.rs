//! Installed About contributor verification, not a migration, native fallback or desktop acceptance.
//! Reuses real baked credits and existing immutable DTOs; no network, app logic or generated data edits.
use super::*;
use crate::state::Dialog;
use crate::{Services, headless::HeadlessView};
use craft::ui::runtime::inspect::Element;
use egui::{Event, Key, Modifiers, PointerButton, Pos2};
use lightcraft_catalog::{HistoryStep, Op, Photo, PhotoId, Source, Version};
use lightcraft_develop::DevelopSettings;
use lightcraft_engine::{Selection, Session};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
const CLOCK: &str = "2026-11-22T00:00:00Z";
struct Drain(egui::FullOutput);
impl Drop for Drain {
    fn drop(&mut self) {
        self.0.textures_delta.clear();
        self.0.platform_output.commands.clear();
        self.0.viewport_output.clear();
        self.0.shapes.clear();
    }
}
fn baked_credit_snapshot() -> Value {
    json!({"contributors":format!("{:?}",CONTRIBUTORS),"models":format!("{:?}",MODELS),"commits":TOTAL_COMMITS,"projection":format!("{:?}",framework_credits())})
}
#[derive(Clone, Debug)]
struct Access {
    row_index: Option<usize>,
    children: Vec<u64>,
    sort_direction: String,
    id: u64,
    description: Option<String>,
    name: Option<String>,
    role: String,
    selected: String,
    toggled: String,
}
struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    tick: u32,
    elements: Vec<Element>,
    access: Vec<Access>,
    opened: Arc<Mutex<Vec<String>>>,
    baseline: Value,
    trace: VecDeque<String>,
    previous_locale: crate::i18n::Locale,
}
impl Fixture {
    fn new() -> Self {
        let previous_locale = crate::i18n::language();
        crate::i18n::set_language(crate::i18n::Locale::En);
        let mut session = Session::new();
        session.clock = Box::new(|| CLOCK.into());
        session.xmp.auto_write = false;
        for index in 0..2 {
            let id = PhotoId(101 + index);
            let mut photo = Photo::new(id, Source::Demo { scene: index as u32 }, &format!("Preserved photo {index}"), "JPEG", 8, 6, CLOCK);
            let mut d = DevelopSettings::default();
            d.light.contrast = index as f64 * 7.0;
            photo.develop = Arc::new(d);
            photo.meta.title = format!("Title {index}");
            photo.meta.caption = format!("Caption {index}");
            photo.meta.keywords = vec![format!("keep-{index}")];
            photo.history = vec![HistoryStep { label: format!("Old {index}"), settings: photo.develop.clone() }];
            photo.versions = vec![Version { name: format!("Saved {index}"), created: CLOCK.into(), settings: photo.develop.clone(), auto: false }];
            session.commit("Fixture", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        session.selection = Selection::single(PhotoId(101));
        let opened = Arc::new(Mutex::new(vec![]));
        let capture = opened.clone();
        let mut app = LightcraftApp::new(
            session,
            Services {
                png: None,
                local_places: Some(vec![]),
                open_url: Some(Box::new(move |url| {
                    capture.lock().unwrap().push(url.to_owned());
                    Ok(())
                })),
                ..Default::default()
            },
        );
        app.set_simulated_clock(Some(0.0));
        app.run("library.select", json!({"ids":[101,102],"active":101})).unwrap();
        app.run("develop.merge", json!({"settings":{"light":{"exposure":0.5}},"label":"Preserved redo"})).unwrap();
        app.run("edit.undo", json!({})).unwrap();
        assert!(!app.session.undo.is_empty() && !app.session.redo.is_empty());
        app.ui.settings.gpu = false;
        app.ui.settings.external_editor = "untouched preference".into();
        app.run("app.about", json!({})).unwrap();
        assert_eq!(app.ui.dialog, Some(Dialog::About));
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        ctx.enable_accesskit();
        let mut f =
            Self { app, ctx, tick: 0, elements: vec![], access: vec![], opened, baseline: Value::Null, trace: VecDeque::new(), previous_locale };
        f.baseline = f.preserved();
        f.settle();
        f.click("Contributors", Some("Tab"));
        f.assert_grab_bag(NameMode::Username, SortKey::FirstCommit, true);
        f
    }
    fn history(entries: &[lightcraft_engine::UndoEntry]) -> Value {
        json!(entries.iter().map(|entry| json!({"label":entry.label,"op":entry.op,"folder":format!("{:?}",entry.folder)})).collect::<Vec<_>>())
    }
    fn document(&self) -> Value {
        let s = &self.app.session;
        json!({"catalog":s.catalog,"revision":s.catalog.revision,"selection":s.selection,"previousActive":s.previous_active,
            "undo":Self::history(&s.undo),"redo":Self::history(&s.redo),"journal":s.journal,"commits":s.commits(),
            "interaction":format!("{:?}",s.interaction),"autoSync":s.auto_sync,"clipboard":s.clipboard,"copyGroups":s.copy_groups,
            "source":s.source,"filter":s.filter,"sort":s.sort,"activeMask":s.active_mask,"activeSpot":s.active_spot,
            "toolEffects":s.mask_tool_effects,"lastMerge":s.last_merge,"imports":s.import_defaults,"xmp":s.xmp,
            "lastExport":s.last_export,"presets":s.presets.iter().cloned().collect::<Vec<_>>(),"print":s.print,
            "templates":s.print_templates,"effects":s.mask_effects,"keywordSets":s.keyword_sets,"keywordSet":s.keyword_set})
    }
    fn preserved(&self) -> Value {
        json!({"baked":baked_credit_snapshot(),"document":self.document(),"settings":self.app.ui.settings,"hdr":self.app.ui.hdr_visualize,"epoch":self.app.caches.denoise.epoch})
    }
    fn safe(&self) {
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none() && self.app.session.media.rendered.disk().is_none());
        assert_eq!(self.app.session.catalog.photos().count(), 2);
        assert_eq!(self.app.renderer.live_workers(), 0);
        assert_eq!(self.app.renderer.in_flight(), 0);
        assert!(!self.app.session.segmenter.busy() && !self.app.session.denoise_busy());
        assert!(
            self.app.services.png.is_none()
                && self.app.services.pick_files.is_none()
                && self.app.services.pick_folder.is_none()
                && self.app.services.picker.is_none()
        );
    }
    fn frame(&mut self, mut events: Vec<Event>) {
        self.safe();
        self.tick += 1;
        let mods = events
            .iter()
            .rev()
            .find_map(|event| match event {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or(Modifiers::NONE);
        events.insert(0, Event::ModifiersChanged(mods));
        let input = format!("{events:?}");
        let raw = HeadlessView::raw_input(egui::vec2(1400.0, 1000.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let output = Drain(self.ctx.run_ui(raw, |root| {
            crate::widgets::take_registries(root.ctx());
            crate::dialogs::show_root(app, root, false);
        }));
        self.elements = if matches!(self.app.ui.dialog, Some(Dialog::About)) {
            craft::ui::runtime::inspect::recorded(&self.ctx, egui::Id::unique("about-app-contract"))
                .map(|tree| tree.elements.clone())
                .unwrap_or_default()
        } else {
            vec![]
        };
        self.access.clear();
        if let Some(update) = &output.0.platform_output.accesskit_update {
            for (id, node) in &update.nodes {
                self.access.push(Access {
                    row_index: node.row_index(),
                    children: node.children().iter().map(|child| child.0).collect(),
                    sort_direction: format!("{:?}", node.sort_direction()),
                    id: id.0,
                    description: node.description().map(str::to_owned),
                    name: node.label().map(str::to_owned),
                    role: format!("{:?}", node.role()),
                    selected: format!("{:?}", node.is_selected()),
                    toggled: format!("{:?}", node.toggled()),
                });
            }
        }
        self.trace.push_back(format!(
            "frame={} input={} focus={:?} dialog={:?} nodes={:?}",
            self.tick,
            input,
            craft::ui::runtime::focus::focused_rec(&self.ctx),
            self.app.ui.dialog,
            self.elements.iter().filter_map(|e| e.facts.as_ref().map(|f| (&f.role, &f.name, &f.value))).collect::<Vec<_>>()
        ));
        while self.trace.len() > 10 {
            self.trace.pop_front();
        }
        self.safe();
        assert_eq!(self.preserved(), self.baseline, "all actual credit/preference/photo/history data preserved");
    }
    fn settle(&mut self) {
        for _ in 0..5 {
            self.frame(vec![]);
        }
    }
    // An option row is semantic/hover geometry; the enclosing List::options owns
    // physical click activation. Do not require a second independent row click stop.
    fn live_sort_option(&self, element: &Element) -> bool {
        if element.component != "ListRow" || !element.facts.as_ref().is_some_and(|facts| facts.role == "ListBoxOption" && !facts.disabled) {
            return false;
        }
        let Some(index) = self.elements.iter().position(|e| e.id == element.id && e.part == element.part) else {
            return false;
        };
        let Some(parent) = self.elements[..index].iter().rev().find(|e| e.depth < element.depth) else {
            return false;
        };
        if parent.component != "List"
            || !parent
                .facts
                .as_ref()
                .is_some_and(|facts| facts.role == "ListBox" && facts.name.as_deref() == Some("Sort") && facts.interactive && !facts.disabled)
        {
            return false;
        }
        let Some(record) = craft::ui::runtime::inspect::recorded(&self.ctx, egui::Id::unique("about-app-contract")) else {
            return false;
        };
        let point = element.rect.center();
        record.hits.iter().any(|hit| hit.owner == parent.id && hit.part == parent.part && hit.rect.contains(point))
            && record.hits.iter().any(|hit| hit.owner == element.id && hit.part == element.part && hit.rect.contains(point))
    }
    fn target(&self, name: &str, role: Option<&str>) -> Element {
        let hits = self
            .elements
            .iter()
            .filter(|e| {
                e.facts.as_ref().is_some_and(|f| {
                    f.name.as_deref() == Some(name)
                        && (f.interactive || self.live_sort_option(e))
                        && !f.disabled
                        && role.is_none_or(|role| f.role == role)
                })
            })
            .collect::<Vec<_>>();
        if hits.len() != 1 {
            let candidates =
                self.elements.iter().filter(|e| e.facts.as_ref().is_some_and(|facts| facts.name.as_deref() == Some(name))).collect::<Vec<_>>();
            let record = craft::ui::runtime::inspect::recorded(&self.ctx, egui::Id::unique("about-app-contract")).expect("actual About record");
            let owners = candidates
                .iter()
                .map(|candidate| {
                    let nearby = record
                        .hits
                        .iter()
                        .filter(|hit| hit.rect.contains(candidate.rect.center()))
                        .map(|hit| {
                            let semantic = record.elements.iter().filter(|e| e.id == hit.owner && e.part == hit.part).collect::<Vec<_>>();
                            (hit, semantic)
                        })
                        .collect::<Vec<_>>();
                    (*candidate, nearby)
                })
                .collect::<Vec<_>>();
            panic!(
                "target-observation wanted={name}/{role:?} accepted_count={} candidate_count={} candidates={:#?} hit_owners={:#?} access={:#?} trace={:#?}",
                hits.len(),
                candidates.len(),
                candidates,
                owners,
                self.access,
                self.trace
            );
        }
        hits[0].clone()
    }
    fn live(&self, element: &Element) -> Element {
        self.elements.iter().find(|e| e.id == element.id && e.part == element.part).unwrap_or_else(|| panic!("owner disappeared {element:?}")).clone()
    }
    fn pointer(&mut self, at: Pos2, pressed: bool) {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
        ]);
    }
    fn click_element(&mut self, element: Element) {
        self.tick += 30;
        self.frame(vec![Event::PointerMoved(element.rect.center())]);
        self.settle();
        let at = self.live(&element).rect.center();
        assert!(at.x >= 0.0 && at.x <= 1400.0 && at.y >= 0.0 && at.y <= 1000.0);
        self.pointer(at, true);
        self.pointer(at, false);
        self.settle();
    }
    fn click(&mut self, name: &str, role: Option<&str>) {
        let element = self.target(name, role);
        self.click_element(element);
    }
    fn key_mod(&mut self, key: Key, mods: Modifiers) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: mods }]);
        }
        self.frame(vec![]);
    }
    fn key(&mut self, key: Key) {
        self.key_mod(key, Modifiers::NONE);
    }
    fn focused(&self, element: &Element) -> bool {
        craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|focus| focus.stop.id == element.id && focus.stop.part == element.part)
    }
    fn tab_to(&mut self, name: &str) {
        self.frame(vec![Event::PointerGone]);
        for _ in 0..128 {
            let element = self.target(name, None);
            if self.focused(&element) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab never acquired {name}; trace={:#?}", self.trace);
    }
    fn selected(&self, name: &str) {
        assert!(
            self.access.iter().any(|node| node.name.as_deref() == Some(name)
                && (node.selected == "true" || node.selected == "Some(true)" || node.toggled == "Some(True)")),
            "actual selected node {name}; access={:?}",
            self.access
        );
    }
    fn expected(mode: NameMode, key: SortKey, ascending: bool) -> Vec<&'static Contributor> {
        sorted(CONTRIBUTORS, mode, key, ascending)
    }
    fn grab_names(&self) -> Vec<String> {
        self.elements
            .iter()
            .filter(|e| e.component == "Link" && e.facts.as_ref().is_some_and(|f| f.role == "Link"))
            .filter_map(|e| e.facts.as_ref().and_then(|f| f.name.clone()))
            .collect()
    }
    fn assert_grab_bag(&self, mode: NameMode, key: SortKey, ascending: bool) {
        let expected = Self::expected(mode, key, ascending).iter().map(|c| c.name(mode)).collect::<Vec<_>>();
        assert_eq!(self.grab_names(), expected, "actual baked contributor labels/order, not a synthetic credit list");
        self.selected("Grab bag");
        self.selected(mode.label());
        assert_eq!(self.app.ui.dialog, Some(Dialog::About));
    }
    fn rows(&self) -> Vec<Element> {
        let table = self.target("Contributors", Some("Table"));
        let start = self.elements.iter().position(|element| element.id == table.id && element.part == table.part).expect("actual table owner");
        let end = self
            .elements
            .iter()
            .enumerate()
            .skip(start + 1)
            .find(|(_, element)| element.depth <= table.depth)
            .map_or(self.elements.len(), |(index, _)| index);
        // Select the actual row owners in this table's semantic subtree. Cell
        // parts remain in the complete record and are checked independently.
        let rows = self.elements[start + 1..end]
            .iter()
            .filter(|element| element.component == "ListRow" && element.part.is_none())
            .cloned()
            .collect::<Vec<_>>();
        for row in &rows {
            assert!(
                row.facts.as_ref().is_some_and(|facts| facts.role == "ListItem" && facts.name.is_some() && !facts.disabled),
                "unexpected row owner, not filtered out: {row:?}"
            );
        }
        rows
    }
    fn assert_table(&self, mode: NameMode, key: SortKey, ascending: bool) {
        self.selected("Table");
        self.selected(mode.label());
        self.target("Contributors", Some("Table"));
        assert_eq!(
            self.target("Sort", Some("ComboBox")).facts.and_then(|facts| facts.value),
            Some(key.label().0.to_owned()),
            "actual active sort, not inferred from visible order"
        );
        let header = self.access.iter().filter(|node| node.role == "ColumnHeader" && node.name.as_deref() == Some(key.label().1)).collect::<Vec<_>>();
        assert_eq!(header.len(), 1);
        assert_eq!(
            header[0].sort_direction,
            if ascending { "Some(Ascending)" } else { "Some(Descending)" },
            "documented header direction remains required"
        );
        let expected = Self::expected(mode, key, ascending);
        let rows = self.rows();
        assert!(!rows.is_empty());
        assert!(rows.len() <= expected.len(), "unexpected extra rendered row owners: {rows:#?}");
        let mut row_ids = std::collections::HashSet::new();
        let mut source_logins = std::collections::HashSet::new();
        let mut previous_position = None;
        for row in &rows {
            assert!(row_ids.insert(row.id), "duplicate rendered row owner, not deduplicated: {row:?}");
            let nodes = self.access.iter().filter(|node| node.id == row.id.accesskit_id().0).collect::<Vec<_>>();
            assert_eq!(nodes.len(), 1, "one actual access node per row owner");
            let access_cells =
                nodes[0].children.iter().filter_map(|id| self.access.iter().find(|node| node.id == *id && node.role == "Cell")).collect::<Vec<_>>();
            assert_eq!(access_cells.len(), SortKey::ALL.len(), "all ten published cell positions belong to this actual row owner");
            let position = access_cells[0].row_index.expect("published zero-based data-row position");
            for cell in &access_cells {
                assert_eq!(cell.row_index, Some(position), "all cells agree on the row's actual position");
            }
            if let Some(previous) = previous_position {
                assert_eq!(position, previous + 1, "entire rendered window is contiguous in declared source order");
            }
            previous_position = Some(position);
            let person =
                expected.get(position).unwrap_or_else(|| panic!("unexpected published row position {position}; source length {}", expected.len()));
            assert!(source_logins.insert(person.login), "duplicate source contributor cannot be hidden by repeated positions");
            assert_eq!(
                row.facts.as_ref().and_then(|facts| facts.name.as_deref()),
                Some(person.name(mode).as_str()),
                "exact rendered row order at published position {position}"
            );
            let description = nodes[0].description.as_deref().expect("real contributor row carries its credit-summary tooltip");
            assert!(
                description.starts_with(&format!("@{}:", person.login)),
                "row owner identity is actual contributor login {}, got {description:?}",
                person.login
            );
            let cells = self
                .elements
                .iter()
                .filter(|element| element.id == row.id && element.part.is_some() && element.facts.as_ref().is_some_and(|facts| facts.role == "Cell"))
                .collect::<Vec<_>>();
            assert_eq!(cells.len(), SortKey::ALL.len(), "all ten row cells remain represented, not thrown away");
            let mut parts = std::collections::HashSet::new();
            for cell in &cells {
                assert!(parts.insert(cell.part), "duplicate cell identity, not deduplicated");
            }
            assert_eq!(
                cells[0].facts.as_ref().and_then(|facts| facts.name.as_deref()),
                Some(person.name(mode).as_str()),
                "name cell stays bound to the same actual contributor"
            );
        }
        // Sorting preserves the keyed visible anchor. Assert every published
        // position in that actual window; do not assume it reset to data row0.
    }
    fn sort(&mut self, key: SortKey) {
        self.click("Sort", Some("ComboBox"));
        self.click(key.label().0, None);
        assert_eq!(
            self.target("Sort", Some("ComboBox")).facts.and_then(|facts| facts.value),
            Some(key.label().0.to_owned()),
            "actual selected sort value"
        );
        self.assert_grab_bag(NameMode::Username, key, key.default_ascending());
    }
    fn reverse(&mut self) {
        self.click("Reverse the order", None);
    }
    fn assert_opened(&self, expected: &[String]) {
        assert_eq!(
            *self.opened.lock().unwrap(),
            expected,
            "only the exact contributor login/profile target reaches the owned in-memory host recorder"
        );
        assert_eq!(self.app.ui.dialog, Some(Dialog::About));
        assert_eq!(self.preserved(), self.baseline);
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

#[test]
fn credits_installed_name_formats_render_actual_consent_fallbacks_and_preserve_order() {
    let mut f = Fixture::new();
    f.sort(SortKey::Name);
    for mode in [NameMode::DisplayName, NameMode::RealName, NameMode::Username] {
        f.click(mode.label(), None);
        f.assert_grab_bag(mode, SortKey::Name, true);
    }
    assert!(CONTRIBUTORS.iter().any(|c| c.real_name.is_none()));
    assert!(f.opened.lock().unwrap().is_empty());
}
#[test]
fn credits_installed_sort_defaults_and_reverse_preserve_real_data_and_active_view() {
    let mut f = Fixture::new();
    for key in SortKey::ALL {
        f.sort(key);
        f.reverse();
        f.assert_grab_bag(NameMode::Username, key, !key.default_ascending());
        f.reverse();
        f.assert_grab_bag(NameMode::Username, key, key.default_ascending());
    }
    assert!(f.opened.lock().unwrap().is_empty());
}
#[test]
fn credits_installed_table_headers_and_tab_roundtrip_retain_name_sort_direction_and_view() {
    let mut f = Fixture::new();
    f.sort(SortKey::Name);
    f.click("Real name", None);
    f.click("Table", None);
    f.assert_table(NameMode::RealName, SortKey::Name, true);
    f.click("Commits", Some("ColumnHeader"));
    f.assert_table(NameMode::RealName, SortKey::Commits, false);
    f.reverse();
    f.assert_table(NameMode::RealName, SortKey::Commits, true);
    f.click("Models", Some("Tab"));
    f.click("Contributors", Some("Tab"));
    f.assert_table(NameMode::RealName, SortKey::Commits, true);
    f.click("Grab bag", None);
    f.assert_grab_bag(NameMode::RealName, SortKey::Commits, true);
    assert!(f.opened.lock().unwrap().is_empty());
}
#[test]
fn credits_installed_profile_links_preserve_targets_across_names_and_table_activation() {
    let mut f = Fixture::new();
    f.sort(SortKey::Name);
    let mut opened = vec![];
    for mode in [NameMode::Username, NameMode::DisplayName, NameMode::RealName] {
        if mode != NameMode::Username {
            f.click(mode.label(), None);
        }
        f.assert_grab_bag(mode, SortKey::Name, true);
        let person = Fixture::expected(mode, SortKey::Name, true)[0];
        let link = f
            .elements
            .iter()
            .find(|e| e.component == "Link" && e.facts.as_ref().is_some_and(|facts| facts.name.as_deref() == Some(person.name(mode).as_str())))
            .unwrap()
            .clone();
        f.click_element(link);
        opened.push(format!("https://github.com/{}", person.login));
        f.assert_opened(&opened);
        f.assert_grab_bag(mode, SortKey::Name, true);
    }
    f.click("Table", None);
    f.assert_table(NameMode::RealName, SortKey::Name, true);
    let row = f.rows()[0].clone();
    let at = row.rect.center();
    f.frame(vec![Event::PointerMoved(at)]);
    f.pointer(at, true);
    f.pointer(at, false);
    f.frame(vec![]);
    f.pointer(at, true);
    f.pointer(at, false);
    f.settle();
    opened.push(format!("https://github.com/{}", Fixture::expected(NameMode::RealName, SortKey::Name, true)[0].login));
    f.assert_opened(&opened);
    f.assert_table(NameMode::RealName, SortKey::Name, true);
}
#[test]
fn credits_installed_keyboard_name_selection_reverse_and_view_use_actual_focus() {
    let mut f = Fixture::new();
    f.sort(SortKey::Name);
    f.tab_to("Username");
    f.key(Key::ArrowRight);
    f.key(Key::Space);
    f.settle();
    f.assert_grab_bag(NameMode::DisplayName, SortKey::Name, true);
    f.tab_to("Reverse the order");
    f.key(Key::Space);
    f.settle();
    f.assert_grab_bag(NameMode::DisplayName, SortKey::Name, false);
    f.tab_to("Grab bag");
    f.key(Key::ArrowRight);
    f.key(Key::Space);
    f.settle();
    f.assert_table(NameMode::DisplayName, SortKey::Name, false);
    f.key_mod(Key::Tab, Modifiers { shift: true, ..Modifiers::NONE });
    assert!(f.opened.lock().unwrap().is_empty());
}
