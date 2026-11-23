//! Physical-input per-photo AI Denoise workflow through the actual right/Edit/Detail animated drawer.
//! The native baseline and public-Checkbox candidate use this exact file. No fake focus, hits, SDK intent, command or catalog. Inert private raw sentinels are not decoded.
use crate::{LightcraftApp, Services, headless::HeadlessView, state::RightPanel};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::{HistoryStep, MediaKind, Op, Photo, PhotoId, Source, Version};
use lightcraft_develop::DevelopSettings;
use lightcraft_engine::{Selection, Session};
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Arc};

const KEY: &str = "denoise:enabled";
const LABEL: &str = "AI Denoise";
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

/// Newly created test-only source/model tree. Session has no library or file loader: no denoise
/// product directory exists, so denoise_enqueue returns before opening a raw or starting a worker.
struct Owned {
    root: std::path::PathBuf,
    models: std::path::PathBuf,
    protected: Vec<(std::path::PathBuf, Vec<u8>)>,
}
impl Owned {
    fn new() -> Self {
        let mut root = None;
        for index in 0..10000 {
            let path = std::env::temp_dir().join(format!("lc-denoise-photo-{}-{index}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => {
                    root = Some(path.canonicalize().unwrap());
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("private fixture root: {e}"),
            }
        }
        let root = root.expect("new private root");
        let models = root.join("models");
        std::fs::create_dir(&models).unwrap();
        let mut owned = Self { root, models, protected: vec![] };
        for index in 0..5 {
            owned.keep(
                owned.root.join(format!("photo-{index}.dng")),
                format!("owned inert original sentinel {index}; no raw decode or inference").into_bytes(),
            );
        }
        owned.keep(owned.models.join("NOT_INSTALLED.txt"), b"owned model preservation sentinel; never an installed model".to_vec());
        println!("DENOISE_PHOTO_OWNED_ROOT {}", owned.root.display());
        owned
    }
    fn keep(&mut self, path: std::path::PathBuf, bytes: Vec<u8>) {
        std::fs::write(&path, &bytes).unwrap();
        assert!(path.canonicalize().unwrap().starts_with(&self.root));
        self.protected.push((path, bytes));
    }
    fn safe(&self) {
        assert_eq!(self.root.canonicalize().unwrap(), self.root);
        assert!(self.models.canonicalize().unwrap().starts_with(&self.root));
        for (path, bytes) in &self.protected {
            assert!(path.canonicalize().unwrap().starts_with(&self.root));
            assert_eq!(std::fs::read(path).unwrap(), *bytes);
            assert!(!path.with_extension("xmp").exists());
        }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        if self.root.canonicalize().is_ok_and(|path| path == self.root) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    photos: [PhotoId; 5],
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
    owned: Owned,
}
impl Fixture {
    fn new(width: f32, locale: crate::i18n::Locale) -> Self {
        let previous_locale = crate::i18n::language();
        crate::i18n::set_language(locale);
        let owned = Owned::new();
        let mut session = Session::new();
        session.clock = Box::new(|| CLOCK.into());
        session.xmp.auto_write = false;
        session.set_denoise_models_dir(Some(owned.models.clone()));
        assert!(session.library.is_none());
        let list = session.execute("denoise.models.list", &json!({})).unwrap();
        assert!(list["productsDir"].is_null());
        assert!(std::path::Path::new(list["dir"].as_str().unwrap()).canonicalize().unwrap().starts_with(&owned.root));
        session.execute("denoise.settings", &json!({"auto":false,"runOn":"cpu"})).unwrap();
        let photos = [PhotoId(101), PhotoId(102), PhotoId(103), PhotoId(104), PhotoId(105)];
        for (index, id) in photos.into_iter().enumerate() {
            let mut p = Photo::new(
                id,
                Source::File { path: owned.root.join(format!("photo-{index}.dng")).to_string_lossy().into_owned() },
                &format!("private-{index}.dng"),
                if index == 3 { "JPEG" } else { "DNG" },
                64,
                48,
                CLOCK,
            );
            p.kind = if index == 3 { MediaKind::Image } else { MediaKind::Raw };
            if index == 4 {
                p.preview_only = Some("private unsupported raw preview-only fixture".into());
            }
            p.rating = (index + 1) as u8;
            p.meta.title = format!("Preserved title {index}");
            p.meta.caption = format!("Preserved caption {index}");
            p.meta.keywords = vec![format!("preserved-{index}")];
            let mut d = DevelopSettings::default();
            d.light.exposure = index as f64 * 0.25;
            d.light.contrast = index as f64 * 3.0;
            if index == 1 {
                d.enhance.denoise_on = Some(false);
                d.enhance.denoise = 80.0;
            }
            if index == 2 {
                d.enhance.denoise_on = Some(false);
                d.enhance.denoise = 0.0;
            }
            p.develop = Arc::new(d);
            p.edited = Some(CLOCK.into());
            p.history = vec![HistoryStep { label: format!("Existing {index}"), settings: p.develop.clone() }];
            p.versions = vec![Version { name: format!("Saved {index}"), created: CLOCK.into(), settings: p.develop.clone(), auto: false }];
            session.commit("Per-photo fixture", Op::AddPhoto { photo: Box::new(p) }).unwrap();
        }
        session.selection = Selection::single(photos[0]);
        let mut app = LightcraftApp::new(session, Services { png: None, local_places: Some(vec![]), ..Default::default() });
        app.set_simulated_clock(Some(0.0));
        app.run("library.select", json!({"ids":[101,102,103,104,105],"active":101})).unwrap();
        app.ui.right = RightPanel::Edit;
        app.ui.histogram = false;
        app.ui.open_sections = vec!["detail".into()];
        app.ui.open_flyouts.clear();
        app.ui.right_width = width;
        app.run("develop.merge", json!({"settings":{"light":{"exposure":0.9}},"label":"Fixture redo"})).unwrap();
        app.run("edit.undo", json!({})).unwrap();
        assert!(!app.session.undo.is_empty() && !app.session.redo.is_empty());
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
            owned,
        };
        let before = f.state();
        f.settle_drawer();
        assert_eq!(f.state(), before, "opening actual Detail parent is passive");
        assert!(f.has(KEY));
        assert!(!f.flag());
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
        json!({"document":self.document(),"hdrVisualize":self.app.ui.hdr_visualize,"denoiseEpoch":self.app.caches.denoise.epoch,"workers":self.app.renderer.live_workers(),"inFlight":self.app.renderer.in_flight(),"segmenting":self.app.session.segmenter.busy(),"activity":format!("{:?}",self.app.session.activity.list())})
    }
    fn flag(&self) -> bool {
        self.app.session.active().and_then(|id| self.app.session.develop_of(id)).is_some_and(|d| d.enhance.denoise_enabled())
    }

    fn photos_value(&self) -> Value {
        json!(self.app.session.catalog)["photos"].clone()
    }
    fn safety(&mut self) {
        self.owned.safe();
        let actual = self.app.session.execute("denoise.models.list", &json!({})).unwrap();
        assert!(actual["productsDir"].is_null());
        assert!(std::path::Path::new(actual["dir"].as_str().unwrap()).canonicalize().unwrap().starts_with(&self.owned.root));
        let status = self.app.session.execute("denoise.status", &json!({})).unwrap();
        assert_eq!(status["queued"], 0);
        assert!(status["running"].is_null());
        let s = &self.app.session;
        assert_eq!(s.catalog.photos().count(), 5);
        assert!(s.library.is_none() && s.library_folder.is_none());
        assert!(s.media.rendered.disk().is_none());
        assert!(!s.denoise_busy());
        assert!(self.app.services.png.is_none() && self.app.services.picker.is_none() && self.app.services.pick_files.is_none());
        assert_eq!(self.app.renderer.in_flight(), 0);
        assert_eq!(self.app.renderer.live_workers(), 0);
        assert!(!s.segmenter.busy());
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
                        let (_, record) = craft::ui::runtime::inspect::record(ui, egui::Id::unique("denoise-per-photo-actual-parent"), |ui| {
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
        for _ in 0..180 {
            if self.focused() {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab never reached actual per-photo checkbox; trace={:#?}", self.trace);
    }
    fn select(&mut self, indices: &[usize], active: usize) {
        self.app
            .run("library.select", json!({"ids":indices.iter().map(|i|self.photos[*i].0).collect::<Vec<_>>(),"active":self.photos[active].0}))
            .unwrap();
        self.settle_drawer();
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

impl Fixture {
    fn stable_document(mut value: Value) -> Value {
        for key in ["catalog", "revision", "undo", "redo", "journal", "commits"] {
            value.as_object_mut().unwrap().remove(key);
        }
        value
    }
    fn expected_toggle(&self, id: PhotoId, on: bool) -> Value {
        let mut d = (*self.app.session.develop_of(id).unwrap()).clone();
        if on && d.enhance.denoise_on.is_none() && d.enhance.denoise <= 0.0 {
            d.enhance.denoise = 50.0;
        }
        d.enhance.denoise_on = Some(on);
        let op = self.app.session.develop_op(id, d, "AI Denoise").unwrap();
        let mut catalog = self.app.session.catalog.clone();
        let inverse = catalog.apply(op).unwrap();
        let mut expected = self.state();
        let doc = &mut expected["document"];
        doc["catalog"] = json!(catalog);
        doc["revision"] = json!(catalog.revision);
        doc["undo"].as_array_mut().unwrap().push(json!({"label":"AI Denoise","op":inverse,"folder":"None"}));
        doc["redo"] = json!([]);
        doc["journal"].as_array_mut().unwrap().push(json!(["denoise.toggle",{"id":id.0,"enabled":on}]));
        doc["commits"] = json!(self.app.session.commits() + 1);
        expected
    }
    fn verify_toggle_and_roundtrip(&mut self, before: Value, expected: Value) {
        assert_eq!(self.state(), expected, "exact requesting photo, history, one inverse Undo op and exact command; trace={:#?}", self.trace);
        let before_photos = before["document"]["catalog"]["photos"].clone();
        let after_photos = self.photos_value();
        let stable = Self::stable_document(before["document"].clone());
        self.app.run("edit.undo", json!({})).unwrap();
        self.settle();
        assert_eq!(self.photos_value(), before_photos, "Undo restores every photo byte of catalog metadata/settings/history/versions");
        assert_eq!(Self::history(&self.app.session.undo), before["document"]["undo"]);
        assert_eq!(self.app.session.redo.last().unwrap().label, "AI Denoise");
        assert_eq!(Self::stable_document(self.document()), stable);
        self.app.run("edit.redo", json!({})).unwrap();
        self.settle();
        assert_eq!(self.photos_value(), after_photos);
        assert_eq!(Self::history(&self.app.session.undo), expected["document"]["undo"]);
        assert!(self.app.session.redo.is_empty());
        assert_eq!(Self::stable_document(self.document()), stable);
        self.safety();
    }
    fn short_edit(&mut self, fraction: f32) {
        let id = self.app.session.active().unwrap();
        let on = !self.flag();
        let before = self.state();
        let expected = self.expected_toggle(id, on);
        let rect = self.rect(KEY);
        let point = egui::pos2(rect.left() + rect.width() * fraction, rect.center().y);
        self.click_at(point);
        self.verify_toggle_and_roundtrip(before, expected);
    }
}

#[test]
fn denoise_photo_sdk_active_photo_handoff_cancels_inflight_press_and_fresh_edit_works() {
    let mut f = Fixture::normal();
    let point = f.rect(KEY).center();
    f.frame(vec![Event::PointerMoved(point)]);
    f.pointer(point, true);
    f.select(&[0, 1, 2, 3, 4], 1);
    let before = f.state();
    f.pointer(point, false);
    f.settle();
    assert_eq!(f.state(), before, "old photo press cannot retarget new photo");
    f.short_edit(0.5);
}

#[test]
fn denoise_photo_sdk_closed_detail_cancels_inflight_press_and_reentry_is_fresh() {
    let mut f = Fixture::normal();
    let point = f.rect(KEY).center();
    f.frame(vec![Event::PointerMoved(point)]);
    f.pointer(point, true);
    f.app.ui.open_sections.clear();
    f.settle_drawer();
    assert!(!f.has(KEY));
    let before = f.state();
    f.pointer(point, false);
    f.settle();
    assert_eq!(f.state(), before);
    f.app.ui.open_sections = vec!["detail".into()];
    f.settle_drawer();
    assert_eq!(f.state(), before);
    f.short_edit(0.5);
}

#[test]
fn denoise_photo_sdk_disabled_parent_no_commit_replay_and_fresh_positive() {
    nonreal_flow(Probe::Disabled)
}

#[test]
fn denoise_photo_sdk_egui_sizing_no_commit_replay_and_fresh_positive() {
    nonreal_flow(Probe::Sizing)
}

#[test]
fn denoise_photo_sdk_existing_positive_and_explicit_zero_amounts_are_preserved() {
    for index in [1, 2] {
        let mut f = Fixture::normal();
        f.select(&[0, 1, 2, 3, 4], index);
        let amount = f.app.session.develop_of(f.photos[index]).unwrap().enhance.denoise;
        f.short_edit(0.5);
        assert_eq!(f.app.session.develop_of(f.photos[index]).unwrap().enhance.denoise, amount);
        f.short_edit(0.5);
        assert_eq!(f.app.session.develop_of(f.photos[index]).unwrap().enhance.denoise, amount);
    }
}

#[test]
fn denoise_photo_sdk_natural_tab_space_enter_reverse_preserves_exact_undo() {
    let mut f = Fixture::normal();
    f.tab_to();
    let before = f.state();
    let expected = f.expected_toggle(f.photos[0], true);
    f.key(Key::Space);
    f.verify_toggle_and_roundtrip(before, expected);
    f.tab_to();
    let before = f.state();
    let expected = f.expected_toggle(f.photos[0], false);
    f.key(Key::Enter);
    f.verify_toggle_and_roundtrip(before, expected);
    f.key_mod(Key::Tab, Modifiers { shift: true, ..Modifiers::NONE });
    f.tab_to();
    assert!(f.focused());
}

#[test]
fn denoise_photo_sdk_rendered_and_preview_only_sources_stay_disabled() {
    for index in [3, 4] {
        let mut f = Fixture::normal();
        f.select(&[0, 1, 2, 3, 4], index);
        assert_eq!(f.app.session.denoise_photo_state(f.photos[index]), lightcraft_engine::denoise::PhotoState::NotApplicable);
        let before = f.state();
        f.click(KEY);
        f.key(Key::Space);
        f.key(Key::Enter);
        assert_eq!(f.state(), before);
    }
}

#[test]
fn denoise_photo_sdk_sdk_measure_no_commit_replay_and_fresh_positive() {
    nonreal_flow(Probe::Measure)
}

#[test]
fn denoise_photo_sdk_live_acquisition_after_auto_sync_configuration_preserves_exact_pointer_undo() {
    for auto in [false, true] {
        for fraction in [0.15, 0.85] {
            let mut f = Fixture::normal();
            f.app.session.auto_sync = auto;
            let configured = f.state();
            // Auto Sync changes the real parent layout; wait for that layout before aiming.
            f.settle_drawer();
            assert_eq!(f.state(), configured, "configuration settling is passive");
            let live = f.rect(KEY);
            assert!(live.is_finite() && live.is_positive());
            assert!(f.checkbox().bounds.is_some_and(|bounds| bounds.intersects(live)), "live named checkbox owns the reacquired geometry");
            f.short_edit(fraction);
            assert!(f.flag());
            f.short_edit(fraction);
            assert!(!f.flag());
            assert_eq!(f.app.session.develop_of(f.photos[0]).unwrap().enhance.denoise, 50.0);
        }
    }
}

fn nonreal_flow(probe: Probe) {
    for real_press in [false, true] {
        let mut f = Fixture::normal();
        let point = f.rect(KEY).center();
        f.frame(vec![Event::PointerMoved(point)]);
        let before = f.state();
        if !real_press {
            f.probe = probe;
        }
        f.pointer(point, true);
        assert_eq!(f.state(), before);
        if !real_press && matches!(probe, Probe::Sizing | Probe::Measure) {
            assert!(!f.has(KEY), "nonreal controls publish no automation alias");
        }
        f.probe = if real_press { probe } else { Probe::Real };
        f.pointer(point, false);
        assert_eq!(f.state(), before, "both physical press directions are inert across nonreal/disabled boundary");
        if real_press && matches!(probe, Probe::Sizing | Probe::Measure) {
            assert!(!f.has(KEY), "nonreal release publishes no automation alias");
        }
        f.probe = Probe::Real;
        f.settle();
        assert_eq!(f.state(), before);
        f.frame(vec![]);
        assert_eq!(f.state(), before, "no deferred replay");
        f.short_edit(0.5);
    }
}
