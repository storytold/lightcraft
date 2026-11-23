//! Source-only qualification tests: actual library.clearPreviews, never mocked eviction.
//! Procedural photos, synthetic cached pixels, no filesystem services or disk cache.
use super::*;
use crate::headless::HeadlessView;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use std::sync::{
    Arc, Weak,
    atomic::{AtomicUsize, Ordering},
};

struct Drain {
    output: Option<egui::FullOutput>,
    drained: Arc<AtomicUsize>,
}
impl Drop for Drain {
    fn drop(&mut self) {
        if let Some(mut output) = self.output.take() {
            // No renderer/OS host exists: explicitly discard uploads and all host outputs.
            output.textures_delta.clear();
            output.platform_output.commands.clear();
            output.viewport_output.clear();
            output.shapes.clear();
            self.drained.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[derive(Clone, Copy)]
enum Probe {
    Normal,
    Disabled,
    Measure,
    Sizing,
}

struct Frames {
    ctx: egui::Context,
    app: LightcraftApp,
    time: f64,
    rect: Option<Rect>,
    probe: Probe,
    native_settings: bool,
    drained: Arc<AtomicUsize>,
    painted: Vec<String>,
}

impl Frames {
    fn new() -> Self {
        Self::with_scope(false)
    }
    fn with_scope(native_settings: bool) -> Self {
        // with_demo is Session::new + pure Source::Demo catalog construction, no with_fs.
        let mut session = lightcraft_engine::Session::with_demo();
        assert!(session.library.is_none());
        assert!(session.library_folder.is_none());
        assert!(session.smart_previews_dir.is_none());
        assert!(session.media.rendered.disk().is_none());
        // A fresh explicitly memory-only object bounds fake preview ownership to this fixture.
        session.media.rendered = Arc::new(lightcraft_preview::PreviewCache::memory(64 * 1024));
        let app = LightcraftApp::new(session, crate::Services { png: None, ..Default::default() });
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut frames = Self {
            ctx,
            app,
            native_settings,
            time: 0.0,
            rect: None,
            probe: Probe::Normal,
            drained: Arc::new(AtomicUsize::new(0)),
            painted: Vec::new(),
        };
        frames.memory_only();
        for _ in 0..3 {
            frames.frame(Vec::new());
        }
        frames
    }

    /// Called before every seed, snapshot, raw-input pass and teardown pass, never after a clear as its only guard.
    fn memory_only(&self) {
        assert!(self.app.session.library.is_none(), "never attach an on-disk library");
        assert!(self.app.session.library_folder.is_none());
        assert!(self.app.session.smart_previews_dir.is_none());
        assert!(self.app.session.media.rendered.disk().is_none(), "NEVER clear any disk preview cache");
        assert!(self.app.services.picker.is_none() && self.app.services.pick_files.is_none());
        assert!(self.app.services.pick_lightroom_catalog.is_none());
    }

    fn seed(&mut self) -> (lightcraft_preview::Hash128, Weak<lightcraft_raster::Rgba8>) {
        self.memory_only();
        let key = lightcraft_preview::hash_bytes(b"clear-cache-memory-fixture");
        let image = Arc::new(lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[21, 43, 65, 255]; 16] });
        let weak = Arc::downgrade(&image);
        self.app.session.media.rendered.put(key, image);
        let id = self.app.session.active().unwrap();
        self.app.session.media.insert(
            id,
            lightcraft_engine::media::SourceLevel::Thumb,
            Arc::new(lightcraft_raster::Rgb32f::filled(4, 4, [0.1, 0.2, 0.3])),
        );
        assert!(self.app.session.media.rendered.get(key).is_some());
        assert!(self.app.session.media.has_source(id, lightcraft_engine::media::SourceLevel::Thumb));
        (key, weak)
    }

    fn snapshot(&mut self) -> Value {
        self.memory_only();
        let photos: Vec<_> = self.app.session.catalog.photos().map(|photo| serde_json::to_value(photo).unwrap()).collect();
        let s = &self.app.session;
        json!({"settings": self.app.ui.settings, "cacheMb": self.app.session.cache_mb,
            "photos": photos, "selection": self.app.session.selection.ids.iter().map(|id| id.0).collect::<Vec<_>>(),
            "active": self.app.session.selection.active.map(|id| id.0), "revision":s.catalog.revision,"undo":s.undo.len(),"redo":s.redo.len(),"commits":s.commits(),
            "smartPreviews": self.app.session.smart_previews_dir})
    }

    fn frame(&mut self, events: Vec<Event>) {
        self.frame_checked(events, |_, _| {});
    }
    fn frame_checked(&mut self, events: Vec<Event>, mut after: impl FnMut(&egui::Ui, &LightcraftApp)) {
        self.memory_only();
        self.time += 0.02;
        crate::widgets::take_registries(&self.ctx);
        let mut raw = HeadlessView::raw_input(egui::vec2(650.0, 540.0), 1.0, self.time, events);
        let modifiers = raw
            .events
            .iter()
            .find_map(|event| match event {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        raw.events.insert(0, Event::ModifiersChanged(modifiers));
        let app = &mut self.app;
        let probe = self.probe;
        let native_settings = self.native_settings;
        let output = self.ctx.run_ui(raw, |ui| {
            let mut draw = |ui: &mut egui::Ui, app: &mut LightcraftApp| {
                let ctx = ui.ctx().clone();
                let mut section = || {
                    let t = Tokens::get(&ctx);
                    preview_cache_section(app, ui, &t);
                    after(ui, app);
                };
                if native_settings {
                    (section)();
                } else {
                    section();
                }
            };
            match probe {
                Probe::Normal => draw(ui, app),
                Probe::Disabled => {
                    ui.add_enabled_ui(false, |ui| draw(ui, app));
                }
                Probe::Measure => craft::ui::runtime::measure::run(&ui.ctx().clone(), || draw(ui, app)),
                Probe::Sizing => {
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                    draw(&mut child, app);
                }
            }
        });
        // Own the entire returned output immediately, including if later observations panic.
        let held = Drain { output: Some(output), drained: Arc::clone(&self.drained) };
        fn text(shape: &egui::Shape, labels: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => labels.push(t.galley.job.text.clone()),
                egui::Shape::Vec(items) => {
                    for item in items {
                        text(item, labels);
                    }
                }
                _ => {}
            }
        }
        self.painted.clear();
        for shape in &held.output.as_ref().unwrap().shapes {
            text(&shape.shape, &mut self.painted);
        }
        (self.app.widgets, self.app.widget_hits) = crate::widgets::take_registries(&self.ctx);
        self.rect = self.app.widgets.iter().find(|(id, _)| id == "button:settingsClearCache").map(|(_, rect)| *rect);
        drop(held);
        self.memory_only();
    }

    fn pointer(&mut self, pos: Pos2, pressed: bool) {
        self.memory_only();
        self.frame(vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
    }

    fn key(&mut self, key: Key) {
        self.memory_only();
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }]);
        }
        self.frame(Vec::new());
    }

    fn natural_tab_to_button(&mut self) {
        self.memory_only();
        let rect = self.rect.unwrap();
        let mut path = Vec::new();
        self.frame(vec![Event::PointerGone]);
        for _ in 0..30 {
            let focus = self.ctx.memory(|memory| memory.focused());
            let focused = focus.and_then(|id| self.ctx.read_response(id)).map(|response| response.rect);
            path.push((focus, focused));
            if focused == Some(rect) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab failed to reach Clear Cache: {path:?}");
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        // Never re-enter an egui pass during unwinding. Prior outputs are already guarded/drained.
        if !std::thread::panicking() {
            self.memory_only();
            self.time += 0.02;
            let raw = HeadlessView::raw_input(egui::vec2(650.0, 540.0), 1.0, self.time, Vec::new());
            let output = self.ctx.run_ui(raw, |_ui| {});
            let held = Drain { output: Some(output), drained: Arc::clone(&self.drained) };
            drop(held); // The teardown FullOutput is drained too.
        }
    }
}

#[test]
fn clear_cache_pointer_release_executes_real_memory_eviction_once_and_preserves_document() {
    let mut f = Frames::new();
    let (key, weak) = f.seed();
    let before = f.snapshot();
    let generation = f.app.session.media.rendered.generation();
    let at = f.rect.unwrap().center();
    f.pointer(at, true);
    assert_eq!(f.app.session.media.rendered.generation(), generation, "press alone does not clear");
    f.pointer(at, false);
    assert_eq!(f.app.session.media.rendered.generation(), generation + 1);
    assert!(f.app.session.media.rendered.get(key).is_none());
    assert!(weak.upgrade().is_none(), "the real cache releases its only strong image reference");
    assert_eq!(f.app.session.media.source_usage(), (0, 0));
    assert_eq!(f.snapshot(), before, "photos/develop/settings/selection/history unchanged");
    let (fresh, kept) = f.seed();
    for _ in 0..8 {
        f.frame(Vec::new());
    }
    assert_eq!(f.app.session.media.rendered.generation(), generation + 1);
    assert!(f.app.session.media.rendered.get(fresh).is_some() && kept.upgrade().is_some());
}

#[test]
fn clear_cache_natural_tab_enter_and_space_execute_real_command_once() {
    for activation in [Key::Enter, Key::Space] {
        let mut f = Frames::new();
        let (key, weak) = f.seed();
        let before = f.snapshot();
        let generation = f.app.session.media.rendered.generation();
        f.natural_tab_to_button();
        f.key(activation);
        assert_eq!(f.app.session.media.rendered.generation(), generation + 1);
        assert!(f.app.session.media.rendered.get(key).is_none() && weak.upgrade().is_none());
        assert_eq!(f.snapshot(), before);
        for _ in 0..5 {
            f.frame(Vec::new());
        }
        assert_eq!(f.app.session.media.rendered.generation(), generation + 1);
    }
}

#[test]
fn clear_cache_outside_release_and_escape_cancel_without_eviction() {
    for escape in [false, true] {
        let mut f = Frames::new();
        let (key, weak) = f.seed();
        let before = f.snapshot();
        let generation = f.app.session.media.rendered.generation();
        let at = f.rect.unwrap().center();
        f.pointer(at, true);
        if escape {
            f.key(Key::Escape);
        }
        f.pointer(if escape { at } else { Pos2::new(620.0, 220.0) }, false);
        assert_eq!(f.app.session.media.rendered.generation(), generation);
        assert!(f.app.session.media.rendered.get(key).is_some() && weak.upgrade().is_some());
        assert_eq!(f.snapshot(), before);
    }
}

#[test]
fn clear_cache_disabled_measurement_and_sizing_refuse_activation() {
    for probe in [Probe::Disabled, Probe::Measure, Probe::Sizing] {
        let mut f = Frames::new();
        let (key, weak) = f.seed();
        let before = f.snapshot();
        let generation = f.app.session.media.rendered.generation();
        let at = f.rect.unwrap().center();
        f.probe = probe;
        f.frame(Vec::new());
        f.pointer(at, true);
        f.pointer(at, false);
        f.key(Key::Enter);
        f.key(Key::Space);
        assert_eq!(f.app.session.media.rendered.generation(), generation);
        assert!(f.app.session.media.rendered.get(key).is_some() && weak.upgrade().is_some());
        assert_eq!(f.snapshot(), before);
        if matches!(probe, Probe::Measure | Probe::Sizing) {
            assert!(f.rect.is_none());
        }
    }
}

#[test]
fn clear_cache_usage_label_and_target_remain_visible_in_the_original_row() {
    let f = Frames::new();
    assert!(f.painted.iter().any(|s| s == crate::i18n::tr("In use")));
    assert!(f.painted.iter().any(|s| s == crate::i18n::tr("memory only")));
    assert!(f.painted.iter().any(|s| s == crate::i18n::tr("Clear Cache")));
    let rect = f.rect.unwrap();
    assert!(rect.width() > 0.0 && rect.height() > 0.0 && rect.left() >= 0.0 && rect.right() <= 650.0);
}

#[test]
fn clear_cache_real_command_accepts_memory_only_session_without_a_mock_refusal() {
    let mut f = Frames::new();
    let (key, weak) = f.seed();
    f.memory_only(); // Real command, with the disk-absence check BEFORE dispatch.
    let generation = f.app.session.media.rendered.generation();
    assert!(f.app.run("library.clearPreviews", json!({})).is_ok());
    assert_eq!(f.app.session.media.rendered.generation(), generation + 1);
    assert!(f.app.session.media.rendered.get(key).is_none() && weak.upgrade().is_none());
    // The native command is always-enabled and infallible here; do not invent an error mock.
}

#[test]
fn clear_cache_full_output_is_drained_for_every_frame_and_teardown() {
    let mut f = Frames::new();
    let drained = Arc::clone(&f.drained);
    let before = drained.load(Ordering::Relaxed);
    f.frame(Vec::new());
    assert_eq!(drained.load(Ordering::Relaxed), before + 1);
    drop(f);
    assert_eq!(drained.load(Ordering::Relaxed), before + 2, "teardown output is explicitly drained");
}

#[test]
fn cache_custom_size_to_limit_clear_reseed_and_automatic_keep_documents_and_previews_separate() {
    size_clear_journey(false);
}
fn size_clear_journey(native: bool) {
    let mut f = Frames::with_scope(native);
    f.memory_only();
    f.app.session.set_cache_mb(3000).unwrap();
    f.frame(vec![]);
    let (key, old) = f.seed();
    let generation = f.app.session.media.rendered.generation();
    let mut doc = f.snapshot();
    let j = f.app.session.journal.len();
    let click = |f: &mut Frames, id: &str| {
        let at = f.app.widgets.iter().find(|(n, _)| n == id).unwrap().1.center();
        f.frame(vec![Event::PointerMoved(at)]);
        f.pointer(at, true);
        f.pointer(at, false);
        f.frame(vec![]);
    };
    assert!(f.painted.iter().any(|l| l == "3000 MB"));
    click(&mut f, "button:settingsCache-0");
    doc["cacheMb"] = json!(512);
    assert_eq!(f.snapshot(), doc);
    assert_eq!(f.app.session.media.rendered.generation(), generation);
    assert!(old.upgrade().is_some() && f.app.session.media.rendered.get(key).is_some());
    assert_eq!(f.app.session.journal[j..], [("library.preferences".to_owned(), json!({"cacheMb":512}))]);
    click(&mut f, "button:settingsCache-0");
    assert_eq!(f.app.session.journal.len(), j + 1);
    click(&mut f, "button:settingsClearCache");
    assert_eq!(f.snapshot(), doc);
    assert_eq!(f.app.session.media.rendered.generation(), generation + 1);
    assert!(old.upgrade().is_none() && f.app.session.media.rendered.get(key).is_none());
    assert_eq!(f.app.session.media.source_usage(), (0, 0));
    let (fresh, kept) = f.seed();
    click(&mut f, "button:settingsCache-7");
    doc["cacheMb"] = json!(0);
    assert_eq!(f.snapshot(), doc);
    assert_eq!(f.app.session.media.rendered.generation(), generation + 1);
    assert!(kept.upgrade().is_some() && f.app.session.media.rendered.get(fresh).is_some());
    assert_eq!(
        f.app.session.journal[j..],
        [
            ("library.preferences".into(), json!({"cacheMb":512})),
            ("library.clearPreviews".into(), json!({})),
            ("library.preferences".into(), json!({"cacheMb":0}))
        ]
    );
    f.memory_only();
}

fn live_cache_release(probe: Probe) {
    measured_cache_neighbors(probe, false);
}
fn measured_cache_neighbors(probe: Probe, native: bool) {
    for clear in [false, true] {
        if native && !clear {
            continue;
        }
        let mut f = Frames::with_scope(native);
        let (key, held) = f.seed();
        let generation = f.app.session.media.rendered.generation();
        let doc = f.snapshot();
        let journal = f.app.session.journal.clone();
        let id = if clear { "button:settingsClearCache" } else { "button:settingsCache-0" };
        let owner = f.app.widget_hits.iter().find(|(n, _)| n == id).unwrap().1.id;
        let at = f.ctx.read_response(owner).unwrap().interact_rect.center();
        let ptr = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        f.frame(vec![Event::PointerMoved(at)]);
        f.frame_checked(vec![ptr(true)], |ui, _| assert!(ui.ctx().read_response(owner).unwrap().is_pointer_button_down_on()));
        assert_eq!(f.snapshot(), doc);
        f.probe = probe;
        let mut seen = 0;
        let state = |a: &LightcraftApp| json!({"photos":a.session.catalog.photos().collect::<Vec<_>>(),"revision":a.session.catalog.revision,"history":(a.session.undo.len(),a.session.redo.len()),"selection":a.session.selection,"settings":a.ui.settings,"size":a.session.cache_mb,"journal":a.session.journal,"commits":a.session.commits(),"usage":a.session.media.source_usage()});
        let before = state(&f.app);
        f.frame_checked(vec![ptr(false)], |ui, app| {
            seen += 1;
            assert_eq!(ui.is_sizing_pass(), matches!(probe, Probe::Sizing));
            assert_eq!(craft::ui::runtime::measure::active(ui.ctx()), matches!(probe, Probe::Measure));
            assert!(app.session.library.is_none() && app.session.media.rendered.disk().is_none());
            assert_eq!(state(app), before);
            assert_eq!(app.session.media.rendered.generation(), generation);
            assert!(held.upgrade().is_some());
            let (rects, hits) = crate::widgets::take_registries(ui.ctx());
            assert!(rects.iter().all(|(n, _)| n != "button:settingsClearCache") && hits.iter().all(|(n, _)| n != "button:settingsClearCache"));
        });
        assert!(seen > 0);
        f.probe = Probe::Normal;
        f.frame(vec![]);
        assert_eq!(f.snapshot(), doc);
        assert_eq!(f.app.session.journal, journal);
        assert!(f.app.session.media.rendered.get(key).is_some());
        assert_eq!(f.app.widget_hits.iter().find(|(n, _)| n == id).unwrap().1.id, owner);
        let at = f.ctx.read_response(owner).unwrap().interact_rect.center();
        f.frame(vec![Event::PointerMoved(at)]);
        f.pointer(at, true);
        f.pointer(at, false);
        assert_eq!(f.app.session.journal.len(), journal.len() + 1);
        if clear {
            assert_eq!(f.app.session.media.rendered.generation(), generation + 1);
            assert!(held.upgrade().is_none());
        } else {
            assert_eq!(f.app.session.cache_mb, 512);
            assert_eq!(f.app.session.media.rendered.generation(), generation);
            assert!(held.upgrade().is_some());
        }
    }
}
#[test]
fn preview_cache_sdk_measure_release_preserves_size_and_clear_then_recovers() {
    live_cache_release(Probe::Measure);
}
#[test]
fn preview_cache_native_sizing_release_preserves_size_and_clear_then_recovers() {
    live_cache_release(Probe::Sizing);
}

#[test]
fn shipped_settings_custom_limit_clear_reseed_automatic() {
    size_clear_journey(true);
}
#[test]
fn shipped_native_size_neighbors_tab_to_native_clear_and_activate_once() {
    for activation in [Key::Enter, Key::Space] {
        let mut f = Frames::with_scope(true);
        let (key, weak) = f.seed();
        let before = f.snapshot();
        let g = f.app.session.media.rendered.generation();
        let j = f.app.session.journal.len();
        let clear = f.app.widget_hits.iter().find(|(n, _)| n == "button:settingsClearCache").unwrap().1.id;
        let mut size_seen = false;
        let mut visited = Vec::new();
        f.frame(vec![Event::PointerGone]);
        for _ in 0..30 {
            let focused = f.ctx.memory(|m| m.focused());
            visited.push(focused);
            if size_seen && focused == Some(clear) {
                break;
            }
            if let Some(r) = focused.and_then(|id| f.ctx.read_response(id)) {
                size_seen |= f.app.widgets.iter().any(|(name, rect)| name.starts_with("button:settingsCache-") && *rect == r.rect);
            }
            f.key(Key::Tab);
        }
        assert!(size_seen, "Tab must visit a retained native size choice: {visited:?}; clear={clear:?}; targets={:?}", f.app.widgets);
        assert_eq!(f.ctx.memory(|m| m.focused()), Some(clear));
        f.key(activation);
        assert_eq!(f.app.session.journal[j..], [("library.clearPreviews".into(), json!({}))]);
        assert_eq!(f.app.session.media.rendered.generation(), g + 1);
        assert!(weak.upgrade().is_none() && f.app.session.media.rendered.get(key).is_none());
        assert_eq!(f.snapshot(), before);
    }
}
#[test]
fn shipped_settings_sdk_measure_release_inert_then_same_clear_owner_works() {
    measured_cache_neighbors(Probe::Measure, true);
}
#[test]
fn shipped_settings_native_sizing_release_inert_then_same_clear_owner_works() {
    measured_cache_neighbors(Probe::Sizing, true);
}
