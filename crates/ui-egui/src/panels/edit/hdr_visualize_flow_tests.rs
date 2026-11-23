//! Physical-input HDR visualization workflow through the actual right/Edit/Light animated drawer.
//! The native baseline and public-Checkbox candidate use this exact file. No fake focus, hits, SDK intent, renderer or source files.
use crate::{LightcraftApp, Services, headless::HeadlessView, state::RightPanel};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::{HistoryStep, Op, Photo, PhotoId, Source, Version};
use lightcraft_develop::{DevelopSettings, controls};
use lightcraft_engine::{Selection, Session};
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Arc};

const KEY: &str = "check:hdrVisualize";
const LABEL: &str = "Visualize HDR";
const HELP: &str = "Grey below SDR white; yellow, orange, red, magenta up to 1, 2, 3 and over 3 stops above it";
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
    description: Option<String>,
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
    panel: Rect,
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
            let mut p = Photo::new(id, Source::Demo { scene: 0 }, &format!("hdr-{index}.jpg"), "JPEG", 8, 8, CLOCK);
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
        app.ui.open_sections = vec!["light".into()];
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
            panel: Rect::NOTHING,
            record: None,
            trace: VecDeque::new(),
            previous_locale,
        };
        let before = f.state();
        f.settle_drawer();
        assert_eq!(f.state(), before, "opening the unchanged real parent is passive");
        assert!(f.has(KEY));
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
        // Only the subject flag is projected separately; no application field is overwritten.
        let mut rest = self.app.ui.clone();
        rest.hdr_visualize = false;
        json!({"document":self.document(),"flag":self.app.ui.hdr_visualize,"uiRest":format!("{rest:?}"),
            "workers":self.app.renderer.live_workers(),"inFlight":self.app.renderer.in_flight(),
            "segmenting":self.app.session.segmenter.busy(),"activity":format!("{:?}",self.app.session.activity.list())})
    }
    fn flag(&self) -> bool {
        self.app.ui.hdr_visualize
    }
    fn assert_flag_only(&self, mut before: Value, on: bool) {
        before["flag"] = json!(on);
        assert_eq!(self.state(), before, "only the app visualization flag may change; trace={:#?}", self.trace);
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
                        let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("hdr-visualize-actual-parent"), |ui| {
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
                    description: node.description().map(str::to_owned),
                    checked: format!("{:?}", node.toggled()),
                    bounds: node.bounds().map(|r| Rect::from_min_max(egui::pos2(r.x0 as f32, r.y0 as f32), egui::pos2(r.x1 as f32, r.y1 as f32))),
                });
            }
        }
        self.panel = panel;
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
        let nodes = self.nodes.iter().filter(|n| n.role == "CheckBox" && n.name.as_deref() == Some(LABEL)).collect::<Vec<_>>();
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
        self.click_at(self.rect(key).center());
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
        panic!("natural Tab never reached actual HDR checkbox; trace={:#?}", self.trace);
    }
    fn select(&mut self, indices: &[usize], active: usize) {
        self.app
            .run("library.select", json!({"ids":indices.iter().map(|i|self.photos[*i].0).collect::<Vec<_>>(),"active":self.photos[active].0}))
            .unwrap();
        self.settle_drawer();
    }
    fn overlay(&self) -> String {
        let d = self.app.session.develop_of(self.app.session.active().unwrap()).unwrap();
        format!("{:?}", crate::panels::detail::view_overlay(&self.app, &d))
    }
    fn numeric(&mut self, value: f64) {
        self.click("sliderValue:hdr.maxEv");
        let field = self.rect("sliderField:hdr.maxEv").expand(1.0);
        let focused = craft::ui::runtime::focus::focused_rec(&self.ctx);
        assert!(
            focused.is_some_and(|rec| rec.stop.text && self.ctx.read_response(rec.stop.root).is_some_and(|r| field.contains_rect(r.rect))),
            "real numeric owner"
        );
        self.key_mod(Key::A, Modifiers { ctrl: true, command: true, ..Default::default() });
        let before = self.document();
        self.frame(vec![Event::Text(value.to_string())]);
        assert_eq!(self.document(), before, "typing alone is not a photo edit");
        self.key(Key::Enter);
        self.settle();
    }
    fn native_scalar(&mut self, value: f64) {
        let spec = controls::find("hdr.maxEv").unwrap();
        self.app.run("develop.beginInteraction", json!({"label":spec.label})).unwrap();
        self.app.run("develop.set", json!({"control":"hdr.maxEv","value":value})).unwrap();
        self.app.run("develop.endInteraction", json!({})).unwrap();
        self.settle();
    }
    fn geometry(&self) {
        let n = self.checkbox();
        let r = self.rect(KEY);
        assert_eq!(n.checked, if self.flag() { "Some(True)" } else { "Some(False)" });
        assert_eq!(n.description.as_deref(), Some(HELP));
        assert!(r.is_finite() && r.width() > 20.0 && r.height() > 10.0 && self.panel.expand(1.0).contains_rect(r));
        let labels = self.texts.iter().filter(|t| t.text == LABEL).collect::<Vec<_>>();
        assert_eq!(labels.len(), 1, "one full literal original label");
        assert!(labels[0].clip.expand(1.0).contains_rect(labels[0].rect));
        assert!(r.expand(1.0).contains_rect(labels[0].rect));
        // SDK registrations must refer to the actual owner. Native hover probes intentionally do not.
        if let Some(hit) = self.hits.iter().find(|(key, hit)| key == KEY && hit.id.accesskit_id().0 == n.id).map(|(_, hit)| hit) {
            assert_eq!(r, hit.interact_rect);
            let record = self.record.as_ref().unwrap();
            let elements = record.elements.iter().filter(|e| e.id == hit.id && e.part.is_none()).collect::<Vec<_>>();
            assert_eq!(elements.len(), 1);
            assert_eq!(elements[0].facts.as_ref().unwrap().role, "CheckBox");
            assert!(elements[0].stop.is_some());
        }
    }
    fn restore(&mut self) {
        self.app.run("library.select", json!({"ids":[101,102],"active":101})).unwrap();
        if self.app.ui.right != RightPanel::Edit {
            self.app.run("panel.edit", json!({})).unwrap();
        }
        if !self.app.ui.section_open("light") {
            self.app.run("section.light", json!({})).unwrap();
        }
        if !self.app.session.develop_of(self.photos[0]).unwrap().hdr.enabled {
            self.app.run("develop.hdr", json!({"enabled":true})).unwrap();
        }
        self.settle_drawer();
        assert!(self.has(KEY));
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
fn hdr_visualize_real_pointer_changes_only_app_flag_and_existing_diagnostic_overlay() {
    for auto_sync in [false, true] {
        let mut f = Fixture::normal();
        f.app.run("develop.autoSync", json!({"on":auto_sync})).unwrap();
        f.settle();
        assert!(!f.app.session.redo.is_empty());
        assert_eq!(f.overlay(), "None");
        for on in [true, false, true, false] {
            let before = f.state();
            f.click(KEY);
            f.assert_flag_only(before, on);
            assert_eq!(f.overlay(), if on { "HdrRange" } else { "None" });
            assert!(!f.app.session.redo.is_empty(), "visualization must not clear the document Redo branch");
            f.geometry();
            f.idle();
        }
    }
}

#[test]
fn hdr_visualize_natural_keyboard_matches_native_action_counts_and_exit() {
    let mut f = Fixture::normal();
    f.tab_to();
    assert!(f.focused());
    for key in [Key::Space, Key::Enter, Key::Space, Key::Enter] {
        let before = f.state();
        let old = f.flag();
        f.key_frame(key, true, Modifiers::NONE);
        let on_down = f.flag();
        f.key_frame(key, false, Modifiers::NONE);
        f.settle();
        f.assert_flag_only(before, !old);
        assert!(f.focused());
        eprintln!("hdr-visualize keyboard key={key:?} old={old} on_down={on_down} after_release={}", f.flag());
        f.idle();
    }
    let before = f.state();
    f.key(Key::Escape);
    assert_eq!(f.state(), before, "Escape is not another visualization action");
    f.tab_to();
    f.key(Key::Tab);
    assert!(!f.focused(), "natural Tab exits to an existing neighbor");
    f.key_mod(Key::Tab, Modifiers { shift: true, ..Default::default() });
    assert!(f.focused(), "natural Shift+Tab re-enters the actual checkbox");
    let before = f.state();
    let old = f.flag();
    f.key_frame(Key::Space, true, Modifiers::NONE);
    f.key_frame(Key::Space, false, Modifiers::NONE);
    f.settle();
    f.assert_flag_only(before, !old);
}

#[test]
fn hdr_visualize_global_lifetime_menu_sync_and_later_headroom_edit_keep_document_history_distinct() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::normal();
    let before = f.state();
    f.click(KEY);
    f.assert_flag_only(before, true);
    oracle.app.run("view.visualizeHdr", json!({"on":true})).unwrap();
    oracle.settle();
    assert_eq!(f.state(), oracle.state());
    let saved = serde_json::to_value(&f.app.ui).unwrap();
    assert_eq!(saved["hdrVisualize"], true);
    let round_trip: crate::state::UiState = serde_json::from_value(saved).unwrap();
    assert!(round_trip.hdr_visualize, "same serialized app lifetime, not widget memory");
    for index in [1, 2, 0] {
        f.select(&[index], index);
        oracle.select(&[index], index);
        assert!(f.flag());
        assert_eq!(f.has(KEY), index != 1, "fully shut SDR drawer does not run its body");
        assert_eq!(f.overlay(), if index == 1 { "None" } else { "HdrRange" });
        assert_eq!(f.state(), oracle.state());
        f.idle();
    }
    f.app.run("view.fullScreenPreview", json!({})).unwrap();
    oracle.app.run("view.fullScreenPreview", json!({})).unwrap();
    assert!(f.flag());
    assert_eq!(f.overlay(), "None");
    assert_eq!(f.state(), oracle.state());
    f.app.run("view.fullScreenPreview", json!({})).unwrap();
    oracle.app.run("view.fullScreenPreview", json!({})).unwrap();
    f.settle_drawer();
    oracle.settle_drawer();
    f.app.run("view.visualizeHdr", json!({"on":false})).unwrap();
    oracle.app.run("view.visualizeHdr", json!({"on":false})).unwrap();
    f.settle();
    oracle.settle();
    assert_eq!(f.checkbox().checked, "Some(False)");
    let before = f.state();
    f.click(KEY);
    f.assert_flag_only(before, true);
    oracle.app.run("view.visualizeHdr", json!({"on":true})).unwrap();
    oracle.settle();
    assert_eq!(f.state(), oracle.state());
    let photos = f.photos_value();
    let undo = f.app.session.undo.len();
    let other_before = json!(f.app.session.catalog.photo(f.photos[1]).unwrap());
    let unselected_before = json!(f.app.session.catalog.photo(f.photos[2]).unwrap());
    f.numeric(3.5);
    oracle.native_scalar(3.5);
    assert_eq!(f.state(), oracle.state(), "exact real scalar history/payload/native oracle, not a visualization edit");
    assert_eq!(f.app.session.undo.len(), undo + 1);
    assert!(f.app.session.redo.is_empty());
    assert_eq!(f.app.session.develop_of(f.photos[0]).unwrap().hdr.max_ev, 3.5);
    assert_eq!(json!(f.app.session.catalog.photo(f.photos[1]).unwrap()), other_before);
    assert_eq!(json!(f.app.session.catalog.photo(f.photos[2]).unwrap()), unselected_before);
    let changed = f.photos_value();
    for command in ["edit.undo", "edit.redo", "edit.undo"] {
        f.app.run(command, json!({})).unwrap();
        oracle.app.run(command, json!({})).unwrap();
        f.settle();
        oracle.settle();
        assert_eq!(f.state(), oracle.state(), "complete native Undo/Redo entry, journal, commit and UI parity");
        assert_eq!(f.photos_value(), if command == "edit.redo" { changed.clone() } else { photos.clone() });
        assert!(f.flag(), "document Undo never restores a visualization preference");
    }
}

fn transition(f: &mut Fixture, which: u8) {
    match which {
        0 => {
            f.app.run("develop.hdr", json!({"enabled":false})).unwrap();
        }
        1 => {
            f.app.run("section.light", json!({})).unwrap();
        }
        2 => {
            f.app.run("panel.info", json!({})).unwrap();
        }
        3 => {
            f.app.run("library.select", json!({"ids":[]})).unwrap();
        }
        4 => {
            f.app.run("library.select", json!({"ids":[102],"active":102})).unwrap();
        }
        5 => {
            f.app.run("library.select", json!({"ids":[103],"active":103})).unwrap();
        }
        _ => unreachable!(),
    }
    f.settle_drawer();
    assert_eq!(f.has(KEY), which == 5, "observed parent after its real native animation");
}

#[test]
fn hdr_visualize_real_drawer_parent_changes_and_stale_inputs_match_native_without_replay() {
    for which in 0..6 {
        let mut f = Fixture::normal();
        let at = f.rect(KEY).center();
        f.frame(vec![Event::PointerMoved(at)]);
        f.pointer(at, true);
        assert!(!f.flag(), "pointer press alone has not completed a click");
        transition(&mut f, which);
        let before = f.state();
        let old = f.flag();
        f.pointer(at, false);
        f.settle();
        if which == 5 {
            // This preference is global. A release on the same still-visible control is not a photo edit.
            f.assert_flag_only(before.clone(), f.flag());
            eprintln!("hdr-visualize global-photo pointer before={old} after={} target_rect={:?} old_at={at:?}", f.flag(), f.rect(KEY));
        } else {
            assert_eq!(f.state(), before, "release in a genuinely absent parent is inert; transition={which}");
        }
        f.restore();
        f.idle();
        let before = f.state();
        let old = f.flag();
        f.click(KEY);
        f.assert_flag_only(before, !old);

        let mut f = Fixture::normal();
        f.tab_to();
        f.key_frame(Key::Space, true, Modifiers::NONE);
        let valid_down = f.flag();
        transition(&mut f, which);
        let before = f.state();
        f.key_frame(Key::Space, false, Modifiers::NONE);
        f.settle();
        // A valid prior keydown is retained; no second activation is allowed after hiding/replacing the parent.
        assert_eq!(f.state(), before, "held release must not duplicate an earlier valid activation; transition={which}");
        f.restore();
        f.idle();
        assert_eq!(f.flag(), valid_down);
        let before = f.state();
        let old = f.flag();
        f.click(KEY);
        f.assert_flag_only(before, !old);
    }
    // The real drawer's content moves/clips while its own animation is live. No fake fully-open geometry.
    let mut f = Fixture::normal();
    f.app.run("develop.hdr", json!({"enabled":false})).unwrap();
    let before = f.state();
    let mut observed = vec![];
    for _ in 0..DRAWER_FRAMES {
        f.frame(vec![]);
        let clip = f.texts.iter().find(|t| t.text == LABEL).map(|t| (t.rect, t.clip));
        observed.push((f.tick, f.has(KEY), clip));
        assert_eq!(f.state(), before, "animation itself never changes app or photo state");
    }
    assert!(!f.has(KEY));
    assert!(observed.iter().any(|(_, present, _)| *present), "native closing phase was actually observed");
    eprintln!("hdr-visualize actual closing frames={observed:?}");
    f.restore();
    let before = f.state();
    f.click(KEY);
    f.assert_flag_only(before, true);
}

#[test]
fn hdr_visualize_disabled_and_both_measurement_modes_are_pure_then_fresh_input_works() {
    for probe in [Probe::Disabled, Probe::Sizing, Probe::Measure] {
        let mut f = Fixture::normal();
        let at = f.rect(KEY).center();
        f.probe = probe;
        let before = f.state();
        f.click_at(at);
        f.assert_flag_only(before.clone(), false);
        f.key_frame(Key::Space, true, Modifiers::NONE);
        f.key_frame(Key::Space, false, Modifiers::NONE);
        assert_eq!(f.state(), before, "disabled/speculative raw input is not an app or document edit");
        // Keep original observations, including any native hover-only aliases, visible in the baseline receipt.
        eprintln!(
            "hdr-visualize speculative probe={probe:?} aliases={:?} hits={:?}",
            f.rects.iter().filter(|(name, _)| name == KEY).collect::<Vec<_>>(),
            f.hits.iter().filter(|(name, _)| name == KEY).collect::<Vec<_>>()
        );
        f.probe = Probe::Real;
        f.settle();
        assert_eq!(f.state(), before);
        f.idle();
        let before = f.state();
        f.click(KEY);
        f.assert_flag_only(before, true);

        let mut f = Fixture::normal();
        f.tab_to();
        f.key_frame(Key::Space, true, Modifiers::NONE);
        let before = f.state();
        f.probe = probe;
        f.key_frame(Key::Space, false, Modifiers::NONE);
        assert_eq!(f.state(), before, "blocked held release cannot replay its valid keydown");
        f.probe = Probe::Real;
        f.settle();
        assert_eq!(f.state(), before);
        f.idle();
        let before = f.state();
        let old = f.flag();
        f.click(KEY);
        f.assert_flag_only(before, !old);

        let mut f = Fixture::normal();
        let at = f.rect(KEY).center();
        f.frame(vec![Event::PointerMoved(at)]);
        f.pointer(at, true);
        let before = f.state();
        f.probe = probe;
        f.pointer(at, false);
        assert_eq!(f.state(), before, "a release observed only by a blocked/speculative pass must not commit");
        f.probe = Probe::Real;
        f.settle();
        assert_eq!(f.state(), before);
        f.idle();
        let before = f.state();
        f.click(KEY);
        f.assert_flag_only(before, true);
    }
}

#[test]
fn hdr_visualize_narrow_geometry_checked_owner_and_original_help_match_native() {
    for locale in [crate::i18n::Locale::En, crate::i18n::Locale::De, crate::i18n::Locale::Fr] {
        for width in [250.0, 270.0, crate::state::RIGHT_WIDTH.default] {
            let mut f = Fixture::new(width, locale);
            f.geometry();
            let before = f.state();
            let at = f.rect(KEY).center();
            f.frame(vec![Event::PointerMoved(at)]);
            f.tick += 60;
            f.frame(vec![]);
            assert_eq!(f.state(), before, "the original help is passive");
            let n = f.checkbox();
            assert_eq!(n.description.as_deref(), Some(HELP));
            let help_painted = f.texts.iter().any(|t| t.text == HELP);
            eprintln!(
                "hdr-visualize help locale={locale:?} width={width} frame={} checkbox={} description={:?} help_painted={help_painted}",
                f.tick, n.id, n.description
            );
            // Description and actual painted tooltip are different evidence. Do not fabricate shown help.
            for right in [true, false] {
                let r = f.rect(KEY);
                let at = if right { egui::pos2(r.right() - 1.5, r.center().y) } else { r.left_center() + egui::vec2(1.5, 0.0) };
                let before = f.state();
                let old = f.flag();
                f.click_at(at);
                f.assert_flag_only(before, !old);
                f.geometry();
            }
            f.idle();
        }
    }
}
