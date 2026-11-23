//! Actual common Develop parent and real cached source facts; no files, renderer, forced focus or synthetic SDK intent.
//! The separate native oracle keeps the original checkbox/merge behavior; it is not a substitute production parent.
use crate::LightcraftApp;
use crate::headless::HeadlessView;
use crate::state::RightPanel;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_develop::{DevelopSettings, controls};
use lightcraft_engine::media::SourceLevel;
use lightcraft_engine::{Selection, Session};
use lightcraft_pipeline::SourceInfo;
use lightcraft_raster::Rgb32f;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::Arc;

const KEY: &str = "check:profile.cameraLocalTone";
const LABEL: &str = "Camera local tone mapping";
const HELP: &str = "Apply the local tone mapping stored in the raw (Apple ProRAW), as the camera renders it";
fn owned(name: &str) -> bool {
    name == KEY
}
#[derive(Clone, Copy, Debug)]
enum Probe {
    Real,
    Disabled,
    Sizing,
    Measure,
}
fn drain(mut output: egui::FullOutput) {
    output.textures_delta.clear();
    output.platform_output.commands.clear();
    output.viewport_output.clear();
    output.shapes.clear();
}
#[derive(Clone, Debug)]
struct PaintedText {
    text: String,
    rect: Rect,
    clip: Rect,
}
#[derive(Clone, Debug)]
struct AccessibleNode {
    id: u64,
    role: String,
    name: Option<String>,
    description: Option<String>,
}
fn local_tone() -> Arc<lightcraft_pipeline::local_tone::LocalTone> {
    Arc::new(
        serde_json::from_value(json!({
            "map": {"points_v":1,"points_h":1,"points_n":1,"spacing_v":1.0,"spacing_h":1.0,
                "origin_v":0.0,"origin_h":0.0,"weights":[1.0,0.0,0.0,0.0,0.0],"gamma":1.0,"gains":[2.0]},
            "placement": {"rect":[0.0,0.0,1.0,1.0],"orientation":DevelopSettings::default().orientation}
        }))
        .unwrap(),
    )
}
/// The original native control/command, with a response probe added only for the test oracle.
fn native(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    if app.ui.right != RightPanel::Edit {
        return;
    }
    let Some(id) = app.session.active() else { return };
    let d = app.session.develop_of(id).unwrap();
    if d.profile.id != "lc.color" {
        super::control(app, ui, &d, "profile.amount", true);
    }
    if app.session.source_info(id).local_tone.is_some() {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 0, bottom: 10 }).show(ui, |ui| {
            let mut on = d.profile.camera_local_tone;
            let r = ui.checkbox(&mut on, crate::i18n::tr(LABEL));
            crate::widgets::register_response(KEY, r.interact_rect, &r);
            if r.on_hover_text(crate::i18n::tr(HELP)).changed() {
                app.run("develop.merge", json!({"settings":{"profile":{"camera_local_tone":on}},"label":"Camera Local Tone Mapping"})).unwrap();
            }
        });
    }
}
struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    photos: [PhotoId; 3],
    width: f32,
    probe: Probe,
    native: bool,
    tick: u32,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    record: Option<Arc<craft::ui::runtime::inspect::Recorded>>,
    record_layer: Option<egui::LayerId>,
    panel: Rect,
    texts: Vec<PaintedText>,
    access_checks: std::collections::HashMap<String, String>,
    access_nodes: Vec<AccessibleNode>,
    trace: VecDeque<String>,
    prior_locale: crate::i18n::Locale,
}
impl Fixture {
    fn new(width: f32, locale: crate::i18n::Locale, plain_raw: bool) -> Self {
        let prior_locale = crate::i18n::language();
        crate::i18n::set_language(locale);
        let mut session = Session::new();
        session.clock = Box::new(|| "2026-11-21T00:00:00Z".into());
        let photos = [session.catalog.alloc_photo_id(), session.catalog.alloc_photo_id(), session.catalog.alloc_photo_id()];
        // Use the existing public decoder seam, not a replaced UI condition or engine method.
        let tone = local_tone();
        session.media.file_loader = Some(Arc::new(move |path, _, _| {
            let raw = !path.ends_with(".jpg");
            let info = SourceInfo {
                raw,
                as_shot_temp: 5500.0,
                as_shot_tint: 0.0,
                local_tone: (!path.contains("plain")).then(|| tone.clone()),
                ..Default::default()
            };
            Ok((Rgb32f::new(8, 8), info))
        }));
        for (index, id) in photos.into_iter().enumerate() {
            let format = if index < 2 || plain_raw { "DNG" } else { "JPEG" };
            let path = if index == 2 {
                if plain_raw { "/camera-tone/plain.dng".into() } else { "/camera-tone/plain.jpg".into() }
            } else {
                format!("/camera-tone/mapped-{index}.dng")
            };
            session
                .catalog
                .apply(Op::AddPhoto {
                    photo: Box::new(Photo::new(
                        id,
                        Source::File { path },
                        ["Mapped A.dng", "Mapped B.dng", "Plain source"][index],
                        format,
                        8,
                        8,
                        "2026-11-21T00:00:00Z",
                    )),
                })
                .unwrap();
            session.selection = Selection::single(id);
            session.source_now(id, SourceLevel::Preview).unwrap();
            assert_eq!(session.source_info(id).local_tone.is_some(), index < 2, "real decoder/cache eligibility");
            let mut d = (*session.develop_of(id).unwrap()).clone();
            d.profile.id = ["lc.vivid", "lc.mono", "lc.color"][index].into();
            d.profile.amount = [37.0, 63.0, 100.0][index];
            d.light.exposure = [0.25, -0.5, 0.75][index];
            d.light.contrast = [12.0, -15.0, 27.0][index];
            d.vignette.amount = [-25.0, -40.0, 10.0][index];
            d.profile.camera_local_tone = index == 1;
            session.set_develop(id, d, "Fixture develop").unwrap();
        }
        let mut app = LightcraftApp::new(session, crate::Services { png: None, local_places: Some(vec![]), ..Default::default() });
        app.run("library.select", json!({"ids":[photos[0].0,photos[1].0],"active":photos[0].0})).unwrap();
        // Initial fixture configuration only. Every tested change uses real input or commands.
        app.ui.right = RightPanel::Edit;
        app.ui.histogram = false;
        app.ui.open_sections.clear();
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
            probe: Probe::Real,
            native: false,
            tick: 0,
            rects: vec![],
            hits: vec![],
            record: None,
            record_layer: None,
            panel: Rect::NOTHING,
            texts: vec![],
            access_checks: Default::default(),
            access_nodes: vec![],
            trace: VecDeque::new(),
            prior_locale,
        };
        let before = f.document();
        f.settle();
        assert_eq!(f.document(), before, "passive actual parent must not edit the document");
        f
    }
    fn normal() -> Self {
        Self::new(crate::state::RIGHT_WIDTH.default, crate::i18n::Locale::En, false)
    }
    fn oracle() -> Self {
        let mut f = Self::normal();
        f.native = true;
        f.settle();
        f
    }
    fn frame(&mut self, mut events: Vec<Event>) {
        assert_eq!(self.app.session.catalog.photos().count(), 3);
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.media.rendered.disk().is_none());
        assert!(self.app.services.png.is_none() && self.app.services.picker.is_none() && self.app.services.pick_files.is_none());
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
        let app = &mut self.app;
        let width = self.width;
        let probe = self.probe;
        let is_native = self.native;
        let context = self.ctx.clone();
        let mut final_registry = (vec![], vec![]);
        let mut final_record = None;
        let mut final_layer = None;
        let mut panel = Rect::NOTHING;
        let output = self.ctx.run_ui(raw, |root| {
            crate::widgets::take_registries(root.ctx());
            egui::CentralPanel::default().show(root, |ui| {
                ui.set_max_width(width);
                ui.allocate_ui_with_layout(egui::vec2(width, 2350.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    panel = ui.max_rect();
                    ui.set_clip_rect(ui.clip_rect().intersect(panel));
                    let mut draw = |ui: &mut egui::Ui| {
                        final_layer = Some(ui.layer_id());
                        let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("camera-tone-actual-parent-record"), |ui| {
                            if is_native {
                                native(app, ui);
                            } else {
                                crate::panels::right::show(app, ui);
                            }
                        });
                        final_record = record;
                        // Keep this callback's original registry, never union/deduplicate observations.
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
            let registry = if leftover.0.is_empty() && leftover.1.is_empty() { std::mem::take(&mut final_registry) } else { leftover };
            for (name, _) in registry.0.iter().filter(|(name, _)| owned(name)) {
                assert_eq!(registry.0.iter().filter(|(other, _)| other == name).count(), 1, "duplicate actual callback alias {name}");
                assert_eq!(registry.1.iter().filter(|(other, _)| other == name).count(), 1, "one actual response {name}");
            }
            final_registry = registry;
        });
        fn texts(shape: &egui::Shape, clip: Rect, out: &mut Vec<PaintedText>) {
            match shape {
                egui::Shape::Text(text) => {
                    out.push(PaintedText { text: text.galley.job.text.clone(), rect: Rect::from_min_size(text.pos, text.galley.size()), clip })
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        texts(shape, clip, out);
                    }
                }
                _ => {}
            }
        }
        self.texts.clear();
        for shape in &output.shapes {
            texts(&shape.shape, shape.clip_rect, &mut self.texts);
        }
        // Retain this output's original published nodes, not a label-keyed approximation of the owner.
        self.access_nodes.clear();
        if let Some(update) = &output.platform_output.accesskit_update {
            for (id, node) in &update.nodes {
                self.access_nodes.push(AccessibleNode {
                    id: id.0,
                    role: format!("{:?}", node.role()),
                    name: node.label().map(str::to_owned),
                    description: node.description().map(str::to_owned),
                });
                if let Some(label) = node.label() {
                    self.access_checks.insert(label.to_owned(), format!("{:?}", node.toggled()));
                }
            }
        }
        self.panel = panel;
        self.record = final_record;
        self.record_layer = final_layer;
        (self.rects, self.hits) = final_registry;
        drain(output);
        self.trace.push_back(format!(
            "frame={} probe={probe:?} width={width} native={is_native} input={input} focus={:?} sdk_focus={:?} aliases={:?}",
            self.tick,
            self.ctx.memory(|m| m.focused()),
            craft::ui::runtime::focus::focused_rec(&self.ctx),
            self.rects.iter().filter(|(name, _)| owned(name)).collect::<Vec<_>>()
        ));
        while self.trace.len() > 18 {
            self.trace.pop_front();
        }
    }
    fn settle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![]);
        }
    }
    fn rect(&self, name: &str) -> Rect {
        self.rects.iter().find(|(id, _)| id == name).unwrap_or_else(|| panic!("missing {name}; trace={:#?}", self.trace)).1
    }
    fn hit(&self, name: &str) -> crate::widgets::WidgetHit {
        let hits: Vec<_> = self.hits.iter().filter(|(id, _)| id == name).collect();
        assert_eq!(hits.len(), 1, "one actual {name}; trace={:#?}", self.trace);
        hits[0].1
    }
    fn has(&self, name: &str) -> bool {
        self.rects.iter().any(|(id, _)| id == name)
    }
    fn pointer(&mut self, position: Pos2, pressed: bool) {
        self.frame(vec![
            Event::PointerMoved(position),
            Event::PointerButton { pos: position, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
        ]);
    }
    fn click_at(&mut self, position: Pos2) {
        self.tick += 30;
        self.frame(vec![Event::PointerMoved(position)]);
        self.pointer(position, true);
        self.pointer(position, false);
        self.settle();
    }
    fn click(&mut self, name: &str) {
        self.click_at(self.rect(name).center());
    }
    fn key_mod(&mut self, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
        }
        self.frame(vec![]);
    }
    fn key(&mut self, key: Key) {
        self.key_mod(key, Modifiers::NONE);
    }
    fn focus(&self) -> Option<egui::Id> {
        craft::ui::runtime::focus::focused_rec(&self.ctx).map(|record| record.stop.id).or_else(|| self.ctx.memory(|memory| memory.focused()))
    }
    fn focused(&self, name: &str) -> bool {
        self.focus() == Some(self.hit(name).id)
    }
    fn tab_to(&mut self, name: &str) {
        self.frame(vec![Event::PointerGone]);
        for _ in 0..48 {
            if self.focused(name) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab did not reach {name}; trace={:#?}", self.trace);
    }
    fn photos_value(&self) -> Value {
        serde_json::to_value(self.app.session.catalog.photos().collect::<Vec<_>>()).unwrap()
    }
    fn document(&self) -> Value {
        json!({"photos":self.app.session.catalog.photos().collect::<Vec<_>>(), "selection":self.app.session.selection,
            "revision":self.app.session.catalog.revision, "undo":format!("{:?}",self.app.session.undo), "redo":format!("{:?}",self.app.session.redo),
            "journal":self.app.session.journal, "commits":self.app.session.commits(), "interaction":format!("{:?}",self.app.session.interaction), "autoSync":self.app.session.auto_sync})
    }
    fn state(&self) -> Value {
        json!({"document":self.document(), "ui":self.app.ui, "dragging":self.app.ui.dragging_control,"workers":self.app.renderer.live_workers(),"segmenting":self.app.session.segmenter.busy()})
    }
    fn numeric(&mut self, control: &str, value: f64) {
        self.click(&format!("sliderValue:{control}"));
        assert!(self.focused(&format!("sliderField:{control}")), "real value click must focus the existing editor");
        self.key_mod(Key::A, Modifiers { ctrl: true, command: true, ..Default::default() });
        let before = self.document();
        self.frame(vec![Event::Text(value.to_string())]);
        assert_eq!(self.document(), before, "typing is not a committed scalar edit");
        self.key(Key::Enter);
        self.settle();
    }
    fn native_scalar(&mut self, control: &str, value: f64) {
        let spec = controls::find(control).unwrap();
        self.app.run("develop.beginInteraction", json!({"label":spec.label})).unwrap();
        self.app.run("develop.set", json!({"control":control,"value":value})).unwrap();
        self.app.run("develop.endInteraction", json!({})).unwrap();
        self.settle();
    }
    fn same_as(&self, oracle: &Self) {
        assert_eq!(
            self.state(),
            oracle.state(),
            "the UI must equal exactly the genuine existing command, including all photos/history/UI; trace={:#?}",
            self.trace
        );
    }
    fn idle(&mut self) {
        let before = self.state();
        self.settle();
        assert_eq!(self.state(), before, "no passive replay; trace={:#?}", self.trace);
    }
    fn owner_facts(&self, name: &str) -> &craft::ui::runtime::inspect::Facts {
        let hit = self.hit(name);
        let elements: Vec<_> =
            self.record.as_ref().unwrap().elements.iter().filter(|element| element.id == hit.id && element.part.is_none()).collect();
        assert_eq!(elements.len(), 1, "one actual SDK owner for {name}");
        elements[0].facts.as_ref().unwrap()
    }
    fn value(&self, index: usize) -> bool {
        self.app.session.develop_of(self.photos[index]).unwrap().profile.camera_local_tone
    }
    fn select(&mut self, indices: &[usize], active: usize) {
        self.app
            .run("library.select", json!({"ids":indices.iter().map(|i|self.photos[*i].0).collect::<Vec<_>>(),"active":self.photos[active].0}))
            .unwrap();
        self.settle();
    }
    fn toggle(&mut self) {
        self.click(KEY);
    }
    fn checkbox_description(&self) -> &str {
        let owner = self.hit(KEY).id.accesskit_id().0;
        let nodes: Vec<_> = self.access_nodes.iter().filter(|node| node.id == owner).collect();
        assert_eq!(nodes.len(), 1, "one actual AccessKit node for the observed checkbox response");
        assert_eq!(nodes[0].role, "CheckBox");
        assert_eq!(nodes[0].name.as_deref(), Some(crate::i18n::tr(LABEL)));
        nodes[0].description.as_deref().unwrap_or_else(|| panic!("actual checkbox {owner} has no description; nodes={:#?}", self.access_nodes))
    }
    fn geometry(&self) {
        let record = self.record.as_ref().unwrap();
        let hit = self.hit(KEY);
        let rect = self.rect(KEY);
        let elements: Vec<_> = record.elements.iter().filter(|e| e.id == hit.id && e.part.is_none()).collect();
        assert_eq!(elements.len(), 1);
        let facts = elements[0].facts.as_ref().unwrap();
        assert_eq!(facts.role, "CheckBox");
        assert_eq!(facts.name.as_deref(), Some(crate::i18n::tr(LABEL)));
        assert!(
            facts.interactive
                && !facts.disabled
                && facts.actions.contains(&"click")
                && facts.actions.contains(&"focus")
                && elements[0].stop.is_some()
        );
        let areas: Vec<_> = record.hits.iter().filter(|area| area.owner == hit.id && area.part.is_none()).collect();
        assert_eq!(areas.len(), 1);
        assert_eq!(areas[0].rect, hit.interact_rect);
        assert_eq!(rect, hit.interact_rect);
        assert_eq!(Some(hit.layer_id), self.record_layer);
        assert!(self.panel.expand(1.0).contains_rect(rect));
        assert_eq!(self.ctx.layer_id_at(rect.center()), Some(hit.layer_id));
        let label: Vec<_> = self.texts.iter().filter(|text| text.text == crate::i18n::tr(LABEL)).collect();
        assert_eq!(label.len(), 1, "one complete localized label");
        assert!(label[0].clip.expand(1.0).contains_rect(label[0].rect));
        assert!(rect.expand(1.0).contains_rect(label[0].rect), "the label belongs to the real hit rectangle");
        let active = self.app.session.active().unwrap();
        let value = self.app.session.develop_of(active).unwrap().profile.camera_local_tone;
        assert_eq!(self.access_checks.get(crate::i18n::tr(LABEL)).map(String::as_str), Some(if value { "Some(True)" } else { "Some(False)" }));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let raw = HeadlessView::raw_input(egui::vec2(1100.0, 3000.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            drain(self.ctx.run_ui(raw, |_| {}));
            crate::widgets::take_registries(&self.ctx);
        }
        crate::i18n::set_language(self.prior_locale);
    }
}

fn space(pressed: bool) -> Event {
    Event::Key { key: Key::Space, physical_key: Some(Key::Space), pressed, repeat: false, modifiers: Modifiers::NONE }
}

#[test]
fn camera_tone_real_pointer_on_and_off_match_native_merge_and_complete_undo_redo() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::oracle();
    f.same_as(&oracle);
    let original = f.photos_value();
    let untouched = serde_json::to_value(f.app.session.catalog.photo(f.photos[1]).unwrap()).unwrap();
    for expected in [true, false, true] {
        let undo = f.app.session.undo.len();
        let commits = f.app.session.commits();
        let history = f.app.session.catalog.photo(f.photos[0]).unwrap().history.len();
        f.toggle();
        oracle.toggle();
        f.same_as(&oracle);
        assert_eq!(f.value(0), expected);
        assert_eq!(f.app.session.undo.len(), undo + 1);
        assert_eq!(f.app.session.commits(), commits + 1);
        assert_eq!(f.app.session.catalog.photo(f.photos[0]).unwrap().history.len(), history + 1);
        assert_eq!(serde_json::to_value(f.app.session.catalog.photo(f.photos[1]).unwrap()).unwrap(), untouched);
        let info = f.app.session.source_info(f.photos[0]);
        assert_eq!(lightcraft_pipeline::local_tone::enabled(&info, &f.app.session.develop_of(f.photos[0]).unwrap()), expected);
        f.geometry();
        f.idle();
    }
    for command in ["edit.undo", "edit.undo", "edit.undo"] {
        f.app.run(command, json!({})).unwrap();
        oracle.app.run(command, json!({})).unwrap();
        f.settle();
        oracle.settle();
        f.same_as(&oracle);
    }
    assert_eq!(f.photos_value(), original);
    for _ in 0..3 {
        f.app.run("edit.redo", json!({})).unwrap();
        oracle.app.run("edit.redo", json!({})).unwrap();
        f.settle();
        oracle.settle();
        f.same_as(&oracle);
    }
    assert!(f.value(0));
}

#[test]
fn camera_tone_natural_tab_space_enter_and_escape_preserve_exact_action_counts() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::oracle();
    let before = f.state();
    f.tab_to(KEY);
    assert_eq!(f.state(), before);
    assert!(f.focused(KEY));
    let before = f.state();
    f.key(Key::Tab);
    assert!(!f.focused(KEY));
    assert_eq!(f.state(), before);
    f.key_mod(Key::Tab, Modifiers { shift: true, ..Default::default() });
    assert!(f.focused(KEY));
    assert_eq!(f.state(), before);
    for key in [Key::Space, Key::Enter, Key::Space, Key::Enter] {
        let undo = f.app.session.undo.len();
        f.key(key);
        oracle.toggle();
        f.same_as(&oracle);
        assert_eq!(f.app.session.undo.len(), undo + 1);
        f.geometry();
    }
    let before = f.state();
    f.key(Key::Escape);
    f.idle();
    assert_eq!(f.state(), before);
    f.tab_to(KEY);
    let at = f.rect(KEY).center();
    let before = f.state();
    f.pointer(at, true);
    f.pointer(egui::pos2(f.panel.right() + 30.0, at.y), false);
    f.settle();
    assert_eq!(f.state(), before, "release outside cancels the checkbox");
    f.toggle();
    oracle.toggle();
    f.same_as(&oracle);
}

#[test]
fn camera_tone_auto_sync_on_and_off_preserve_distinct_selected_profiles_and_unselected_photo() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::oracle();
    for fixture in [&mut f, &mut oracle] {
        fixture.app.run("develop.autoSync", json!({"on":true})).unwrap();
        fixture.settle();
    }
    let original = f.photos_value();
    let original_settings: Vec<_> = f.photos.iter().map(|id| (*f.app.session.develop_of(*id).unwrap()).clone()).collect();
    let third = serde_json::to_value(f.app.session.catalog.photo(f.photos[2]).unwrap()).unwrap();
    assert!(!f.value(0) && f.value(1));
    for expected in [true, false] {
        let histories: Vec<_> = f.photos.iter().map(|id| f.app.session.catalog.photo(*id).unwrap().history.len()).collect();
        let before: Vec<_> = f.photos.iter().map(|id| f.app.session.develop_of(*id).unwrap().profile.camera_local_tone).collect();
        let undo = f.app.session.undo.len();
        f.toggle();
        oracle.toggle();
        f.same_as(&oracle);
        assert_eq!(f.app.session.undo.len(), undo + 1, "one genuine selected batch");
        for (index, id) in f.photos.iter().enumerate() {
            let mut wanted = original_settings[index].clone();
            if index < 2 {
                wanted.profile.camera_local_tone = expected;
            }
            let actual = f.app.session.develop_of(*id).unwrap();
            assert_eq!(actual.as_ref(), &wanted, "only the camera-tone field changes on applicable selected targets");
            let changed = index < 2 && before[index] != expected;
            assert_eq!(f.app.session.catalog.photo(*id).unwrap().history.len(), histories[index] + usize::from(changed));
            if !wanted.profile.camera_local_tone {
                let stored = actual.to_json();
                assert!(stored["profile"].get("camera_local_tone").is_none());
                assert_eq!(DevelopSettings::from_json(&stored).unwrap(), wanted, "persisted Off roundtrips without losing other fields");
            }
        }
        assert_eq!(serde_json::to_value(f.app.session.catalog.photo(f.photos[2]).unwrap()).unwrap(), third);
    }
    for _ in 0..2 {
        f.app.run("edit.undo", json!({})).unwrap();
        oracle.app.run("edit.undo", json!({})).unwrap();
        f.settle();
        oracle.settle();
        f.same_as(&oracle);
    }
    assert_eq!(f.photos_value(), original);
    for _ in 0..2 {
        f.app.run("edit.redo", json!({})).unwrap();
        oracle.app.run("edit.redo", json!({})).unwrap();
        f.settle();
        oracle.settle();
        f.same_as(&oracle);
    }
    assert!(!f.value(0) && !f.value(1));
    f.idle();
}

#[test]
fn camera_tone_choice_is_photo_scoped_and_follows_genuine_photo_and_treatment_changes() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::oracle();
    f.toggle();
    oracle.toggle();
    f.same_as(&oracle);
    let first = f.hit(KEY).id;
    f.select(&[1], 1);
    oracle.select(&[1], 1);
    f.same_as(&oracle);
    assert_ne!(f.hit(KEY).id, first, "SDK ownership is photo-specific");
    assert!(f.value(1));
    f.toggle();
    oracle.toggle();
    f.same_as(&oracle);
    assert!(!f.value(1));
    assert!(f.value(0));
    for _ in 0..2 {
        f.app.run("develop.treatment", json!({})).unwrap();
        oracle.app.run("develop.treatment", json!({})).unwrap();
        f.settle();
        oracle.settle();
        f.same_as(&oracle);
        assert!(!f.value(1));
        f.geometry();
    }
    for command in ["panel.info", "panel.edit"] {
        f.app.run(command, json!({})).unwrap();
        oracle.app.run(command, json!({})).unwrap();
        f.settle();
        oracle.settle();
        f.same_as(&oracle);
        assert_eq!(f.has(KEY), command == "panel.edit");
    }
    f.select(&[0], 0);
    oracle.select(&[0], 0);
    f.same_as(&oracle);
    assert!(f.value(0));
    f.geometry();
    f.idle();
}

#[test]
fn camera_tone_original_source_info_guard_hides_plain_raw_jpeg_and_empty_selection() {
    for plain_raw in [false, true] {
        let mut f = Fixture::new(crate::state::RIGHT_WIDTH.default, crate::i18n::Locale::En, plain_raw);
        assert!(f.app.session.source_info(f.photos[0]).local_tone.is_some());
        assert!(f.has(KEY));
        f.select(&[2], 2);
        assert_eq!(f.app.session.source_info(f.photos[2]).raw, plain_raw);
        assert!(f.app.session.source_info(f.photos[2]).local_tone.is_none());
        assert!(!f.has(KEY));
        assert!(!f.hits.iter().any(|(name, _)| owned(name)));
        // Existing settings do not create source metadata or a UI target.
        f.app.run("develop.merge", json!({"settings":{"profile":{"camera_local_tone":true}},"label":"Fixture stored tone"})).unwrap();
        let before = f.state();
        f.settle();
        assert_eq!(f.state(), before);
        assert!(!f.has(KEY));
        assert!(!lightcraft_pipeline::local_tone::enabled(&f.app.session.source_info(f.photos[2]), &f.app.session.develop_of(f.photos[2]).unwrap()));
        f.app.run("library.select", json!({"ids":[]})).unwrap();
        f.settle();
        let before = f.state();
        f.idle();
        assert_eq!(f.state(), before);
        assert!(!f.has(KEY));
        f.select(&[0], 0);
        assert!(f.has(KEY));
        f.toggle();
        assert!(f.value(0));
    }
}

#[test]
fn camera_tone_localized_narrow_checkbox_has_one_real_owner_checked_cue_and_tooltip() {
    for locale in [crate::i18n::Locale::En, crate::i18n::Locale::De, crate::i18n::Locale::Fr] {
        for width in [250.0, 270.0] {
            let mut f = Fixture::new(width, locale, false);
            f.geometry();
            let before = f.document();
            let rect = f.rect(KEY);
            f.frame(vec![Event::PointerMoved(rect.center())]);
            f.tick += 60;
            f.frame(vec![]);
            assert_eq!(f.document(), before, "a tooltip is never an edit");
            // Compare the actual checkbox nodes, not the same-labelled SDK text node.
            // Native and SDK keep the identical existing hover schedule; do not extend the deadline.
            let mut original_native = Fixture::new(width, locale, false);
            original_native.native = true;
            original_native.settle();
            let native_before = original_native.state();
            let native_at = original_native.rect(KEY).center();
            original_native.frame(vec![Event::PointerMoved(native_at)]);
            original_native.tick += 60;
            original_native.frame(vec![]);
            assert_eq!(original_native.state(), native_before, "the original native tooltip is also passive");
            assert_eq!(f.checkbox_description(), crate::i18n::tr(HELP));
            assert_eq!(original_native.checkbox_description(), crate::i18n::tr(HELP));
            assert_eq!(f.checkbox_description(), original_native.checkbox_description(), "actual localized owner descriptions preserve native help");
            let sdk_help_painted = f.texts.iter().any(|text| text.text == crate::i18n::tr(HELP));
            let native_help_painted = original_native.texts.iter().any(|text| text.text == crate::i18n::tr(HELP));
            eprintln!(
                "camera-tone help parity locale={locale:?} width={width} sdk_frame={} native_frame={} sdk_checkbox={:?} native_checkbox={:?} sdk_description={:?} native_description={:?} sdk_help_painted={sdk_help_painted} native_help_painted={native_help_painted}",
                f.tick,
                original_native.tick,
                f.hit(KEY).id,
                original_native.hit(KEY).id,
                f.checkbox_description(),
                original_native.checkbox_description()
            );
            assert_eq!(
                sdk_help_painted, native_help_painted,
                "visual help observations at the original fixed schedule match native; neither false result claims a shown tooltip"
            );
            for at_right in [true, false] {
                let before = f.value(0);
                let rect = f.rect(KEY);
                let point = if at_right { egui::pos2(rect.right() - 1.5, rect.center().y) } else { rect.left_center() + egui::vec2(1.5, 0.0) };
                f.click_at(point);
                assert_eq!(f.value(0), !before, "the whole labelled interaction rectangle toggles the option");
                f.geometry();
            }
            f.idle();
        }
    }
}

#[test]
fn camera_tone_sizing_sdk_measure_and_disabled_pending_releases_do_not_edit_or_replay() {
    for probe in [Probe::Sizing, Probe::Measure, Probe::Disabled] {
        let mut f = Fixture::normal();
        let at = f.rect(KEY).center();
        let before = f.state();
        f.frame(vec![Event::PointerMoved(at)]);
        f.pointer(at, true);
        assert_eq!(f.state(), before);
        f.probe = probe;
        f.pointer(at, false);
        assert_eq!(f.state(), before);
        if matches!(probe, Probe::Disabled) {
            assert!(f.owner_facts(KEY).disabled);
        } else {
            assert!(!f.has(KEY));
            assert!(!f.hits.iter().any(|(name, _)| owned(name)));
            assert!(!f.record.as_ref().is_some_and(|record| record.elements.iter().any(|element| {
                element.facts.as_ref().is_some_and(|facts| facts.role == "CheckBox" && facts.name.as_deref() == Some(crate::i18n::tr(LABEL)))
            })));
        }
        f.probe = Probe::Real;
        f.settle();
        assert_eq!(f.state(), before);
        f.toggle();
        assert!(f.value(0));
        f.idle();
        f.tab_to(KEY);
        let undo = f.app.session.undo.len();
        f.frame(vec![space(true)]);
        assert!(!f.value(0), "a genuine old-owner keydown is retained before its later blocked release");
        assert_eq!(f.app.session.undo.len(), undo + 1);
        let before = f.state();
        f.probe = probe;
        f.frame(vec![space(false)]);
        assert_eq!(f.state(), before);
        f.probe = Probe::Real;
        f.settle();
        assert_eq!(f.state(), before);
        f.idle();
        f.toggle();
        assert!(f.value(0));
    }
}

fn transition(f: &mut Fixture, which: u8) {
    match which {
        0 => {
            f.app.run("panel.info", json!({})).unwrap();
        }
        1 => {
            f.app.run("library.select", json!({"ids":[]})).unwrap();
        }
        2 => f.select(&[2], 2),
        3 => f.select(&[1], 1),
        _ => unreachable!(),
    }
    f.settle();
    assert_eq!(f.has(KEY), which == 3);
}
fn restore(f: &mut Fixture) {
    // panel.edit is a genuine toggle. Restore selection first, and never close an already-open parent.
    f.app.run("library.select", json!({"ids":[f.photos[0].0],"active":f.photos[0].0})).unwrap();
    if f.app.ui.right != RightPanel::Edit {
        f.app.run("panel.edit", json!({})).unwrap();
    }
    f.settle();
    assert_eq!(f.app.ui.right, RightPanel::Edit);
    assert_eq!(f.app.session.active(), Some(f.photos[0]));
    assert!(f.app.session.source_info(f.photos[0]).local_tone.is_some());
    assert!(f.has(KEY), "an actually restored eligible Develop parent publishes its owner");
}

#[test]
fn camera_tone_stale_pointer_and_held_key_releases_do_not_retarget_after_real_parent_changes() {
    for which in 0..4 {
        let mut f = Fixture::normal();
        f.select(&[0], 0);
        let old = f.hit(KEY);
        let at = f.rect(KEY).center();
        let before = f.state();
        f.frame(vec![Event::PointerMoved(at)]);
        f.pointer(at, true);
        assert_eq!(f.state(), before);
        transition(&mut f, which);
        if which == 3 {
            assert_ne!(old.id, f.hit(KEY).id);
            assert!(f.rect(KEY).contains(at), "photo transition retains a real tempting target at release");
        }
        let before = f.state();
        f.pointer(at, false);
        f.settle();
        assert_eq!(f.state(), before, "stale pointer release must not edit another photo or hidden parent");
        restore(&mut f);
        let before = f.state();
        f.idle();
        assert_eq!(f.state(), before);
        f.toggle();
        assert!(f.value(0));

        let mut f = Fixture::normal();
        f.select(&[0], 0);
        f.tab_to(KEY);
        let undo = f.app.session.undo.len();
        f.frame(vec![space(true)]);
        assert!(f.value(0));
        assert_eq!(f.app.session.undo.len(), undo + 1);
        transition(&mut f, which);
        let before = f.state();
        f.frame(vec![space(false)]);
        f.idle();
        assert_eq!(f.state(), before, "held key release does not replay its valid earlier activation");
        restore(&mut f);
        f.idle();
        assert!(f.value(0));
        f.toggle();
        assert!(!f.value(0), "a later fresh input on the restored owner remains usable");
    }
}

#[test]
fn camera_tone_then_real_profile_amount_edit_keeps_separate_native_history_steps_and_neighbors() {
    let mut f = Fixture::normal();
    let mut oracle = Fixture::oracle();
    let original = f.photos_value();
    let original_undo = f.app.session.undo.len();
    f.toggle();
    oracle.toggle();
    f.same_as(&oracle);
    assert!(f.value(0));
    let after_tone = f.photos_value();
    let tone_undo = f.app.session.undo.len();
    assert_eq!(tone_undo, original_undo + 1);
    f.numeric("profile.amount", 59.0);
    oracle.native_scalar("profile.amount", 59.0);
    f.same_as(&oracle);
    assert_eq!(f.app.session.undo.len(), tone_undo + 1);
    assert!(f.value(0));
    assert_eq!(f.app.session.develop_of(f.photos[0]).unwrap().profile.amount, 59.0);
    assert!(f.app.session.interaction.is_none() && f.app.ui.dragging_control.is_none());
    f.app.run("edit.undo", json!({})).unwrap();
    oracle.app.run("edit.undo", json!({})).unwrap();
    f.settle();
    oracle.settle();
    f.same_as(&oracle);
    assert_eq!(f.photos_value(), after_tone);
    f.app.run("edit.undo", json!({})).unwrap();
    oracle.app.run("edit.undo", json!({})).unwrap();
    f.settle();
    oracle.settle();
    f.same_as(&oracle);
    assert_eq!(f.photos_value(), original);
    for _ in 0..2 {
        f.app.run("edit.redo", json!({})).unwrap();
        oracle.app.run("edit.redo", json!({})).unwrap();
        f.settle();
        oracle.settle();
        f.same_as(&oracle);
    }
    assert!(f.value(0));
    assert_eq!(f.app.session.develop_of(f.photos[0]).unwrap().profile.amount, 59.0);
    f.idle();
}
