//! Physical-input Solo Mode through the actual right/Edit section header context menu.
//! Public Menu lifecycle observes its live owner, not exiting alias paint. Original native packets remain frozen; no fake focus, hits or SDK intent.
use crate::{LightcraftApp, Services, headless::HeadlessView, state::RightPanel};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::{HistoryStep, Op, Photo, PhotoId, Source, Version};
use lightcraft_develop::DevelopSettings;
use lightcraft_engine::{Selection, Session};
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Arc};

const KEY: &str = "sectionMenu:solo";
const LABEL: &str = "Solo Mode";
const CLOCK: &str = "2026-11-21T20:00:00Z";
// The real drawer lasts 0.45 seconds. Thirty-six ordinary 60-Hz callbacks cover it with a fixed bound.
const DRAWER_FRAMES: usize = 36;
#[derive(Clone, Copy, Debug)]
enum Probe {
    Real,
    Disabled,
    Sizing,
    Measure,
}
#[derive(Clone, Debug)]
struct PaintedText {
    text: String,
    rect: Rect,
    clip: Rect,
}
#[derive(Clone, Debug)]
struct Node {
    id: u64,
    role: String,
    name: Option<String>,
    checked: String,
    bounds: Option<Rect>,
}
struct Output(egui::FullOutput);
impl Drop for Output {
    fn drop(&mut self) {
        self.0.textures_delta.clear();
        self.0.platform_output.commands.clear();
        self.0.viewport_output.clear();
        self.0.shapes.clear();
    }
}
struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    photos: [PhotoId; 3],
    width: f32,
    tick: u32,
    probe: Probe,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    nodes: Vec<Node>,
    texts: Vec<PaintedText>,
    record: Option<Arc<craft::ui::runtime::inspect::Recorded>>,
    trace: VecDeque<String>,
    previous_locale: crate::i18n::Locale,
}
impl Fixture {
    fn new(width: f32, locale: crate::i18n::Locale) -> Self {
        let previous_locale = crate::i18n::language();
        crate::i18n::set_language(locale);
        let mut session = Session::new();
        session.clock = Box::new(|| CLOCK.into());
        let photos = [PhotoId(101), PhotoId(102), PhotoId(103)];
        for (index, id) in photos.into_iter().enumerate() {
            let mut p = Photo::new(id, Source::Demo { scene: 0 }, &format!("solo-{index}.jpg"), "JPEG", 8, 8, CLOCK);
            p.rating = (index + 1) as u8;
            p.meta.title = format!("Preserved title {index}");
            p.meta.caption = format!("Preserved caption {index}");
            p.meta.keywords = vec![format!("preserved-{index}")];
            let mut d = DevelopSettings::default();
            d.hdr.enabled = index != 1;
            d.hdr.max_ev = [2.0, 1.0, 4.0][index];
            d.hdr.sdr_contrast = [12.0, -18.0, 23.0][index];
            d.light.exposure = [0.25, -0.75, 1.0][index];
            d.light.contrast = [8.0, -15.0, 25.0][index];
            d.profile.id = ["lc.vivid", "lc.mono", "lc.color"][index].into();
            d.profile.amount = [37.0, 63.0, 100.0][index];
            p.develop = Arc::new(d);
            p.edited = Some(CLOCK.into());
            p.history = vec![HistoryStep { label: format!("Existing {index}"), settings: p.develop.clone() }];
            p.versions = vec![Version { name: format!("Saved {index}"), created: CLOCK.into(), settings: p.develop.clone(), auto: false }];
            session.commit("HDR fixture", Op::AddPhoto { photo: Box::new(p) }).unwrap();
        }
        session.selection = Selection::single(photos[0]);
        let mut app = LightcraftApp::new(session, Services { png: None, local_places: Some(vec![]), ..Default::default() });
        app.set_simulated_clock(Some(0.0));
        app.run("library.select", json!({"ids":[101,102],"active":101})).unwrap();
        app.ui.right = RightPanel::Edit;
        app.ui.histogram = false;
        app.ui.open_sections = vec!["light".into(), "color".into()];
        app.ui.open_flyouts.clear();
        app.ui.right_width = width;
        app.run("develop.merge", json!({"settings":{"light":{"exposure":0.9}},"label":"Fixture redo"})).unwrap();
        app.run("edit.undo", json!({})).unwrap();
        assert!(!app.session.redo.is_empty());
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        ctx.enable_accesskit();
        crate::i18n::set_language(locale);
        let mut f = Self {
            app,
            ctx,
            photos,
            width,
            tick: 0,
            probe: Probe::Real,
            rects: vec![],
            hits: vec![],
            nodes: vec![],
            texts: vec![],
            record: None,
            trace: VecDeque::new(),
            previous_locale,
        };
        let before = f.state();
        f.settle_drawer();
        assert_eq!(f.state(), before, "opening the unchanged real parent is passive");
        assert!(!f.has(KEY), "no menu exists before a real context-menu open");
        f
    }
    fn normal() -> Self {
        Self::new(crate::state::RIGHT_WIDTH.default, crate::i18n::Locale::En)
    }
    fn history(entries: &[lightcraft_engine::UndoEntry]) -> Value {
        json!(entries.iter().map(|e| json!({"label":e.label,"op":e.op,"folder":format!("{:?}",e.folder)})).collect::<Vec<_>>())
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
    fn state(&self) -> Value {
        // UiState serialization omits transients. A clone's complete Debug projection also guards those fields.
        // Only the subject preference and open-section list are projected separately; no application field is overwritten.
        let mut rest = self.app.ui.clone();
        rest.single_panel = false;
        rest.open_sections.clear();
        json!({"document":self.document(),"flag":self.app.ui.single_panel,"openSections":self.app.ui.open_sections,"uiRest":format!("{rest:?}"),
            "workers":self.app.renderer.live_workers(),"inFlight":self.app.renderer.in_flight(),
            "segmenting":self.app.session.segmenter.busy(),"activity":format!("{:?}",self.app.session.activity.list())})
    }
    fn flag(&self) -> bool {
        self.app.ui.single_panel
    }
    fn assert_solo_only(&self, mut before: Value, on: bool) {
        before["flag"] = json!(on);
        if on {
            let keep = before["openSections"].as_array().unwrap().last().cloned();
            before["openSections"] = json!(keep.into_iter().collect::<Vec<_>>());
        }
        assert_eq!(self.state(), before, "only the exact Solo Mode preference/open-section effect is allowed; trace={:#?}", self.trace);
    }
    fn photos_value(&self) -> Value {
        json!(self.app.session.catalog.photos().collect::<Vec<_>>())
    }
    fn safety(&self) {
        assert_eq!(self.app.session.catalog.photos().count(), 3);
        assert!(self.app.session.catalog.photos().all(|p| matches!(p.source, Source::Demo { .. })));
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.media.rendered.disk().is_none());
        assert!(self.app.services.png.is_none() && self.app.services.picker.is_none() && self.app.services.pick_files.is_none());
        assert_eq!(self.app.renderer.in_flight(), 0);
        assert!(!self.app.session.segmenter.busy());
    }
    fn frame(&mut self, mut events: Vec<Event>) {
        self.safety();
        self.tick += 1;
        let input = format!("{events:?}");
        let modifiers = events
            .iter()
            .find_map(|e| match e {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        events.insert(0, Event::ModifiersChanged(modifiers));
        let raw = HeadlessView::raw_input(egui::vec2(1100.0, 3000.0), 1.0, f64::from(self.tick) / 60.0, events);
        let context = self.ctx.clone();
        let app = &mut self.app;
        let width = self.width;
        let probe = self.probe;
        let mut final_registry = (vec![], vec![]);
        let mut final_record = None;
        let mut panel = Rect::NOTHING;
        let output = Output(self.ctx.run_ui(raw, |root| {
            // Every callback starts clean, including discarded egui layout passes.
            crate::widgets::take_registries(root.ctx());
            egui::CentralPanel::default().show(root, |ui| {
                ui.set_max_width(width);
                ui.allocate_ui_with_layout(egui::vec2(width, 2350.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    panel = ui.max_rect();
                    ui.set_clip_rect(ui.clip_rect().intersect(panel));
                    let mut draw = |ui: &mut egui::Ui| {
                        let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("section-solo-actual-parent"), |ui| {
                            crate::panels::right::show(app, ui);
                        });
                        final_record = record;
                        final_registry = crate::widgets::take_registries(ui.ctx());
                    };
                    match probe {
                        Probe::Real => draw(ui),
                        Probe::Disabled => {
                            ui.add_enabled_ui(false, |ui| draw(ui));
                        }
                        Probe::Sizing => {
                            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                            draw(&mut child);
                        }
                        Probe::Measure => craft::ui::runtime::measure::run(&context, || draw(ui)),
                    }
                });
            });
            let leftover = crate::widgets::take_registries(root.ctx());
            final_registry = if leftover.0.is_empty() && leftover.1.is_empty() { std::mem::take(&mut final_registry) } else { leftover };
            assert!(final_registry.0.iter().filter(|(name, _)| name == KEY).count() <= 1, "duplicate original aliases");
            assert!(final_registry.1.iter().filter(|(name, _)| name == KEY).count() <= 1, "duplicate original hits");
        }));
        fn texts(shape: &egui::Shape, clip: Rect, out: &mut Vec<PaintedText>) {
            match shape {
                egui::Shape::Text(t) => {
                    out.push(PaintedText { text: t.galley.job.text.clone(), rect: Rect::from_min_size(t.pos, t.galley.size()), clip })
                }
                egui::Shape::Vec(parts) => {
                    for part in parts {
                        texts(part, clip, out);
                    }
                }
                _ => {}
            }
        }
        self.texts.clear();
        for shape in &output.0.shapes {
            texts(&shape.shape, shape.clip_rect, &mut self.texts);
        }
        self.nodes.clear();
        if let Some(update) = &output.0.platform_output.accesskit_update {
            for (id, node) in &update.nodes {
                self.nodes.push(Node {
                    id: id.0,
                    role: format!("{:?}", node.role()),
                    name: node.label().map(str::to_owned),
                    checked: format!("{:?}", node.toggled()),
                    bounds: node.bounds().map(|r| Rect::from_min_max(egui::pos2(r.x0 as f32, r.y0 as f32), egui::pos2(r.x1 as f32, r.y1 as f32))),
                });
            }
        }
        self.record = final_record;
        (self.rects, self.hits) = final_registry;
        self.trace.push_back(format!(
            "frame={} probe={probe:?} input={input} flag={} active={:?} right={:?} focus={:?} sdk_focus={:?} aliases={:?}",
            self.tick,
            self.flag(),
            self.app.session.active(),
            self.app.ui.right,
            self.ctx.memory(|m| m.focused()),
            craft::ui::runtime::focus::focused_rec(&self.ctx),
            self.rects.iter().filter(|(name, _)| name == KEY).collect::<Vec<_>>()
        ));
        while self.trace.len() > 18 {
            self.trace.pop_front();
        }
        self.safety();
    }
    fn live_menu_owner(&self) -> Option<egui::Id> {
        let record = self.record.as_ref()?;
        let owners = record
            .elements
            .iter()
            .filter(|e| e.part.is_none() && e.component == "Menu")
            .filter(|e| craft::ui::runtime::overlay::entry(&self.ctx, e.id).is_some_and(|layer| !layer.exiting))
            .map(|e| e.id)
            .collect::<Vec<_>>();
        assert!(owners.len() <= 1, "at most one real live context-menu owner: {owners:?}");
        let owner = owners.first().copied();
        if let Some(id) = owner {
            assert_eq!(craft::ui::runtime::overlay::top(&self.ctx), Some(id), "the observed Menu owns key routing, not only closing paint");
            assert!(craft::ui::runtime::overlay::any_open(&self.ctx, craft::ui::runtime::overlay::OpenFilter::Keys));
        }
        owner
    }
    fn live_checkbox_hit(&self) -> crate::widgets::WidgetHit {
        let owner = self.live_menu_owner().expect("a real interactive Menu must precede targeting its checkbox");
        let record = self.record.as_ref().unwrap();
        let index = record.elements.iter().position(|e| e.id == owner && e.part.is_none() && e.component == "Menu").unwrap();
        let depth = record.elements[index].depth;
        let checkboxes = record.elements[index + 1..]
            .iter()
            .take_while(|e| e.depth > depth || (e.id == owner && e.part.is_some()))
            .filter(|e| {
                e.part.is_none()
                    && e.component == "Checkbox"
                    && e.facts
                        .as_ref()
                        .is_some_and(|f| f.role == "CheckBox" && f.name.as_deref() == Some(crate::i18n::tr(LABEL)) && !f.disabled && f.interactive)
            })
            .collect::<Vec<_>>();
        assert_eq!(checkboxes.len(), 1, "one genuine enabled SDK checkbox within the live Menu subtree");
        let checkbox = checkboxes[0];
        assert!(checkbox.stop.is_some(), "the same live checkbox publishes its genuine focus stop");
        let hits = self.hits.iter().filter(|(name, hit)| name == KEY && hit.id == checkbox.id).map(|(_, hit)| *hit).collect::<Vec<_>>();
        assert_eq!(hits.len(), 1, "reacquire the original alias hit for that exact live SDK checkbox");
        let hit = hits[0];
        let nodes = self
            .nodes
            .iter()
            .filter(|n| n.id == checkbox.id.accesskit_id().0 && n.role == "CheckBox" && n.name.as_deref() == Some(crate::i18n::tr(LABEL)))
            .collect::<Vec<_>>();
        assert_eq!(nodes.len(), 1, "the live hit and the actual accessible checkbox have the same identity");
        assert!(nodes[0].bounds.is_some_and(|r| r.is_finite() && r.intersects(hit.interact_rect)));
        assert!(hit.interact_rect.is_finite() && hit.interact_rect.width() > 20.0 && hit.interact_rect.height() > 10.0);
        assert_eq!(self.ctx.layer_id_at(hit.interact_rect.center()), Some(hit.layer_id), "the genuine live hit's layer owns its pointer target");
        hit
    }
    fn settle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![]);
        }
    }
    fn settle_drawer(&mut self) {
        for _ in 0..DRAWER_FRAMES {
            self.frame(vec![]);
        }
    }
    fn idle(&mut self) {
        let before = self.state();
        self.settle();
        assert_eq!(self.state(), before, "no idle replay");
    }
    fn has(&self, key: &str) -> bool {
        self.rects.iter().any(|(name, _)| name == key)
    }
    fn rect(&self, key: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == key).unwrap_or_else(|| panic!("missing {key}; trace={:#?}", self.trace)).1
    }
    fn checkbox(&self) -> &Node {
        // Native register() is only a hover probe. Resolve the actual published CheckBox node, not that probe.
        let nodes = self.nodes.iter().filter(|n| n.role == "CheckBox" && n.name.as_deref() == Some(crate::i18n::tr(LABEL))).collect::<Vec<_>>();
        assert_eq!(nodes.len(), 1, "one actual named checkbox; trace={:#?}", self.trace);
        let n = nodes[0];
        assert!(n.bounds.is_some_and(|b| b.is_finite() && b.intersects(self.rect(KEY))), "actual owner has bounds");
        n
    }
    fn focused(&self) -> bool {
        let id = craft::ui::runtime::focus::focused_rec(&self.ctx).map(|r| r.stop.id).or_else(|| self.ctx.memory(|m| m.focused()));
        id.is_some_and(|id| id.accesskit_id().0 == self.checkbox().id)
    }
    fn pointer(&mut self, at: Pos2, pressed: bool) {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
        ]);
    }
    fn click_at(&mut self, at: Pos2) {
        self.tick += 30;
        self.frame(vec![Event::PointerMoved(at)]);
        self.pointer(at, true);
        self.pointer(at, false);
        self.settle();
    }
    fn click(&mut self, key: &str) {
        let at = if key == KEY { self.live_checkbox_hit().interact_rect.center() } else { self.rect(key).center() };
        self.click_at(at);
    }
    fn key_frame(&mut self, key: Key, pressed: bool, modifiers: Modifiers) {
        self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
    }
    fn key_mod(&mut self, key: Key, mods: Modifiers) {
        self.key_frame(key, true, mods);
        self.key_frame(key, false, mods);
        self.frame(vec![]);
    }
    fn key(&mut self, key: Key) {
        self.key_mod(key, Modifiers::NONE);
    }
    fn tab_to(&mut self) {
        self.frame(vec![Event::PointerGone]);
        for _ in 0..48 {
            if self.focused() {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab never reached actual Solo Mode checkbox; trace={:#?}", self.trace);
    }

    fn geometry(&self) {
        let n = self.checkbox();
        let rect = self.rect(KEY);
        assert_eq!(n.checked, if self.flag() { "Some(True)" } else { "Some(False)" });
        assert_eq!(n.name.as_deref(), Some(crate::i18n::tr(LABEL)));
        assert!(rect.is_finite() && rect.width() > 20.0 && rect.height() > 10.0);
        assert!(Rect::from_min_size(Pos2::ZERO, egui::vec2(1100.0, 3000.0)).contains_rect(rect));
        let labels = self.texts.iter().filter(|t| t.text == crate::i18n::tr(LABEL)).collect::<Vec<_>>();
        assert_eq!(labels.len(), 1, "one full original localized menu label");
        assert!(labels[0].clip.expand(1.0).contains_rect(labels[0].rect));
        assert!(rect.expand(1.0).contains_rect(labels[0].rect));
        let hit = self.hits.iter().find(|(name, _)| name == KEY).unwrap().1;
        assert_eq!(self.ctx.layer_id_at(rect.center()), Some(hit.layer_id), "actual context-menu layer owns the pointer");
        assert!(hit.layer_id.order > egui::Order::Background, "the item is in a real popup, not a substitute inline row");
        if hit.id.accesskit_id().0 == n.id {
            assert_eq!(rect, hit.interact_rect);
            let record = self.record.as_ref().unwrap();
            let elements = record.elements.iter().filter(|e| e.id == hit.id && e.part.is_none()).collect::<Vec<_>>();
            assert_eq!(elements.len(), 1, "one SDK owner in the genuine popup");
            assert_eq!(elements[0].facts.as_ref().unwrap().role, "CheckBox");
            assert!(elements[0].stop.is_some());
        }
    }
    fn restore(&mut self) {
        self.app.run("library.select", json!({"ids":[101,102],"active":101})).unwrap();
        if self.app.ui.right != RightPanel::Edit {
            self.app.run("panel.edit", json!({})).unwrap();
        }
        self.settle();
        assert!(self.has("section:light"), "genuine restored Edit parent");
    }
    fn button(&mut self, at: Pos2, button: PointerButton, pressed: bool, modifiers: Modifiers) {
        self.frame(vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button, pressed, modifiers }]);
    }
    fn click_mod(&mut self, key: &str, modifiers: Modifiers) {
        let at = self.rect(key).center();
        self.tick += 30;
        self.frame(vec![Event::PointerMoved(at)]);
        self.button(at, PointerButton::Primary, true, modifiers);
        self.button(at, PointerButton::Primary, false, modifiers);
        self.settle();
    }
    fn menu(&mut self, section: &str) {
        assert!(self.live_menu_owner().is_none(), "start with no existing menu; cancellation is physical and explicit");
        let header = self.rect(&format!("section:{section}"));
        let at = egui::pos2(header.left() + 80.0, header.center().y);
        let before = self.state();
        self.tick += 30;
        self.frame(vec![Event::PointerMoved(at)]);
        self.button(at, PointerButton::Secondary, true, Modifiers::NONE);
        self.button(at, PointerButton::Secondary, false, Modifiers::NONE);
        self.settle();
        assert_eq!(self.state(), before, "right-click opens the real popup without changing header, Solo Mode or document");
        assert!(self.has(KEY), "real context menu must publish its original item");
        assert!(self.live_menu_owner().is_some(), "the published item belongs to a live interactive Menu");
        self.geometry();
    }
    fn cancel(&mut self) {
        assert!(self.has(KEY));
        assert!(self.live_menu_owner().is_some(), "cancel starts with a live owner, not retained paint");
        let before = self.state();
        self.key(Key::Escape);
        self.settle();
        assert!(self.live_menu_owner().is_none(), "Escape closes the actual menu");
        assert_eq!(self.state(), before, "cancel changes no app/document state");
        self.idle();
    }
    fn fresh_toggle(&mut self, section: &str) {
        if self.live_menu_owner().is_none() {
            self.menu(section);
        }
        let before = self.state();
        let old = self.flag();
        self.click(KEY);
        self.assert_solo_only(before, !old);
        assert!(self.live_menu_owner().is_none(), "completed activation closes the real popup");
        self.idle();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let raw = HeadlessView::raw_input(egui::vec2(1100.0, 3000.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            let _output = Output(self.ctx.run_ui(raw, |ui| {
                crate::widgets::take_registries(ui.ctx());
            }));
            crate::widgets::take_registries(&self.ctx);
        }
        crate::i18n::set_language(self.previous_locale);
    }
}

#[test]
fn solo_menu_real_popup_pointer_checked_state_equals_exact_command_without_photo_history() {
    for locale in [crate::i18n::Locale::En, crate::i18n::Locale::De] {
        for width in [250.0, 270.0] {
            let mut f = Fixture::new(width, locale);
            let mut oracle = Fixture::new(width, locale);
            assert_eq!(f.app.ui.open_sections, ["light", "color"]);
            for (section, on) in [("light", true), ("color", false), ("light", true)] {
                let before = f.state();
                f.menu(section);
                assert_eq!(f.checkbox().checked, if !on { "Some(True)" } else { "Some(False)" });
                f.click(KEY);
                oracle.app.run("view.soloSections", json!({"on":on})).unwrap();
                oracle.settle();
                f.assert_solo_only(before, on);
                assert_eq!(f.state(), oracle.state(), "exact existing view.soloSections on payload, including last-open-section trimming");
                assert_eq!(f.app.ui.open_sections, ["color"], "enabling keeps the last open section, not the context-clicked Light header");
                assert_eq!(crate::menubar::checked(&f.app, "view.soloSections"), Some(on));
                assert!(f.live_menu_owner().is_none(), "activation closes the actual popup");
                assert!(!f.app.session.redo.is_empty(), "UI preference preserves the complete seeded Redo branch");
                f.idle();
            }
            f.menu("light");
            f.geometry();
            f.cancel();
        }
    }
}

#[test]
fn solo_menu_escape_outside_cancel_and_external_checked_updates_are_passive() {
    let mut f = Fixture::normal();
    let document = f.document();
    for section in ["light", "color"] {
        f.menu(section);
        let before = f.state();
        f.settle();
        assert_eq!(f.state(), before);
        f.cancel();
        f.menu(section);
        let before = f.state();
        f.click_at(egui::pos2(1000.0, 2800.0));
        assert!(f.live_menu_owner().is_none(), "outside empty-space click dismisses the actual popup");
        assert_eq!(f.state(), before, "outside cancellation does not click a section or document control");
        f.idle();
    }
    f.menu("light");
    for on in [true, false] {
        f.app.run("view.soloSections", json!({"on":on})).unwrap();
        let before = f.state();
        f.settle();
        assert_eq!(f.state(), before, "an externally changed checked value is observed, not replayed as another command");
        assert!(f.has(KEY));
        f.geometry();
        assert_eq!(f.checkbox().checked, if on { "Some(True)" } else { "Some(False)" });
    }
    f.cancel();
    assert_eq!(f.document(), document);
    assert!(!f.app.session.redo.is_empty());
}

#[test]
fn solo_menu_natural_keyboard_activates_once_closes_and_cancels_without_focus_bridge() {
    for key in [Key::Space, Key::Enter] {
        let mut f = Fixture::normal();
        f.menu("light");
        f.tab_to();
        assert!(f.focused(), "natural Tab focuses the actual checkbox in the popup");
        f.key_mod(Key::Tab, Modifiers { shift: true, ..Default::default() });
        assert!(f.focused(), "natural Shift+Tab stays within this sole-item popup");
        let before = f.state();
        let old = f.flag();
        f.key_frame(key, true, Modifiers::NONE);
        let on_down = f.flag();
        f.key_frame(key, false, Modifiers::NONE);
        f.settle();
        f.assert_solo_only(before, !old);
        assert!(f.live_menu_owner().is_none(), "keyboard activation closes the real popup");
        eprintln!("section-solo keyboard key={key:?} old={old} on_down={on_down} after_release={}", f.flag());
        let completed = f.state();
        f.key_frame(key, false, Modifiers::NONE);
        f.settle();
        assert_eq!(f.state(), completed, "a stale release after popup close is not another action");
        f.idle();
        f.menu("color");
        f.tab_to();
        f.cancel();
        f.fresh_toggle("light");
        assert!(!f.flag());
    }
}

#[test]
fn solo_menu_keeps_plain_option_and_section_enable_actions_distinct_with_exact_undo() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::normal();
    f.click("section:detail");
    oracle.app.run("section.detail", json!({"solo":false})).unwrap();
    oracle.settle();
    assert_eq!(f.state(), oracle.state());
    assert_eq!(f.app.ui.open_sections, ["light", "color", "detail"]);
    assert!(!f.flag());
    f.click_mod("section:optics", Modifiers { alt: true, ..Default::default() });
    oracle.app.run("section.optics", json!({"solo":true})).unwrap();
    oracle.settle();
    assert_eq!(f.state(), oracle.state());
    assert_eq!(f.app.ui.open_sections, ["optics"]);
    assert!(!f.flag(), "Option-click is not a Solo Mode preference change");
    f.menu("light");
    let before = f.state();
    f.click(KEY);
    f.assert_solo_only(before, true);
    oracle.app.run("view.soloSections", json!({"on":true})).unwrap();
    oracle.settle();
    assert_eq!(f.state(), oracle.state());
    f.click("section:color");
    oracle.app.run("section.color", json!({"solo":false})).unwrap();
    oracle.settle();
    assert_eq!(f.state(), oracle.state());
    assert_eq!(f.app.ui.open_sections, ["color"]);
    assert!(f.flag());
    assert!(!f.has(KEY));
    let photos = f.photos_value();
    let undo = f.app.session.undo.len();
    let sections = f.app.ui.open_sections.clone();
    let header = f.rect("section:light");
    f.frame(vec![Event::PointerMoved(header.center())]);
    assert!(f.has("sectionEye:light"), "existing eye appears only from genuine header hover");
    f.click("sectionEye:light");
    oracle.app.run("develop.sectionEnabled", json!({"section":"light","enabled":false})).unwrap();
    oracle.settle();
    assert_eq!(f.state(), oracle.state(), "the eye retains its exact document command, unlike Solo Mode");
    assert_eq!(f.app.session.undo.len(), undo + 1);
    assert!(f.app.session.redo.is_empty());
    assert_eq!(f.app.ui.open_sections, sections);
    assert!(f.flag());
    assert!(!f.app.session.develop_of(f.photos[0]).unwrap().section_enabled("light"));
    let edited = f.photos_value();
    for command in ["edit.undo", "edit.redo", "edit.undo"] {
        f.app.run(command, json!({})).unwrap();
        oracle.app.run(command, json!({})).unwrap();
        f.settle();
        oracle.settle();
        assert_eq!(f.state(), oracle.state(), "complete original section-enable Undo/Redo parity");
        assert_eq!(f.photos_value(), if command == "edit.redo" { edited.clone() } else { photos.clone() });
        assert!(f.flag(), "document Undo is independent of Solo Mode");
        assert_eq!(f.app.ui.open_sections, sections);
    }
}

#[test]
fn solo_menu_disabled_sizing_and_measurement_release_are_pure_then_fresh_open_works() {
    for probe in [Probe::Disabled, Probe::Sizing, Probe::Measure] {
        let mut f = Fixture::normal();
        f.menu("light");
        let at = f.rect(KEY).center();
        let before = f.state();
        f.probe = probe;
        f.click_at(at);
        assert_eq!(f.state(), before, "disabled/speculative popup input is not a UI preference or photo edit");
        eprintln!("section-solo probe={probe:?} original_aliases={:?}", f.rects.iter().filter(|(name, _)| name == KEY).collect::<Vec<_>>());
        f.probe = Probe::Real;
        f.settle();
        assert_eq!(f.state(), before);
        f.idle();
        f.fresh_toggle("light");

        let mut f = Fixture::normal();
        f.menu("light");
        let at = f.rect(KEY).center();
        f.frame(vec![Event::PointerMoved(at)]);
        f.pointer(at, true);
        let before = f.state();
        f.probe = probe;
        f.pointer(at, false);
        assert_eq!(f.state(), before, "a blocked/speculative held pointer release cannot execute the command");
        f.probe = Probe::Real;
        f.settle();
        assert_eq!(f.state(), before);
        f.idle();
        f.fresh_toggle("light");

        let mut f = Fixture::normal();
        f.menu("light");
        f.tab_to();
        f.key_frame(Key::Space, true, Modifiers::NONE);
        let valid_down = f.state();
        f.probe = probe;
        f.key_frame(Key::Space, false, Modifiers::NONE);
        assert_eq!(f.state(), valid_down, "blocked release retains only any earlier valid keydown activation");
        f.probe = Probe::Real;
        f.settle();
        assert_eq!(f.state(), valid_down);
        f.idle();
        f.fresh_toggle("color");
    }
}

#[test]
fn solo_menu_absent_parent_old_menu_owner_and_photo_changes_do_not_retarget_releases() {
    for empty in [false, true] {
        let mut f = Fixture::normal();
        f.menu("light");
        let at = f.rect(KEY).center();
        f.frame(vec![Event::PointerMoved(at)]);
        f.pointer(at, true);
        if empty {
            f.app.run("library.select", json!({"ids":[]})).unwrap();
        } else {
            f.app.run("panel.info", json!({})).unwrap();
        }
        f.settle();
        assert!(!f.has(KEY), "actual ineligible parent no longer publishes a menu");
        let before = f.state();
        f.pointer(at, false);
        f.settle();
        assert_eq!(f.state(), before, "hidden-owner release is inert");
        f.restore();
        f.idle();
        assert!(!f.flag());
        f.fresh_toggle("light");
    }
    let mut f = Fixture::normal();
    f.menu("light");
    let old_at = f.rect(KEY).center();
    f.frame(vec![Event::PointerMoved(old_at)]);
    f.pointer(old_at, true);
    f.cancel();
    f.menu("color");
    let before = f.state();
    f.pointer(old_at, false);
    f.settle();
    assert_eq!(f.state(), before, "old Light-menu press is not retargeted to the new Color-menu owner");
    if f.live_menu_owner().is_some() {
        f.cancel();
    }
    f.fresh_toggle("color");

    let mut f = Fixture::normal();
    f.menu("light");
    f.app.run("library.select", json!({"ids":[103],"active":103})).unwrap();
    f.settle();
    assert!(!f.flag(), "a genuine photo change does not toggle the global Solo preference");
    let retained_menu = f.live_menu_owner().is_some();
    eprintln!("section-solo genuine photo change retained_menu={retained_menu}");
    // The native header's auto identity/position can change with the photo's profile controls.
    // Observe dismissal honestly, and reopen only with a real right click if the owner disappeared.
    if !retained_menu {
        f.menu("light");
    }
    let before = f.state();
    f.click(KEY);
    f.assert_solo_only(before, true);
    assert!(f.live_menu_owner().is_none());
    f.idle();
    assert_eq!(f.app.session.active(), Some(f.photos[2]));
}
