//! Native Denoise work/cache workflow on the actual Settings parent, plus supported panel-only speculative input.
//! No downloads, injected device, inference, user settings or focus requests. Full owns runtime.
use crate::{LightcraftApp, Services};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_engine::Session;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const CLEAR: &str = "denoise:clear";
const LIMIT: &str = "Cache limit";
const AUTO: &str = "check:settings.denoiseAuto";
const QUEUE: &str = "denoise:queueAll";
#[derive(Clone, Copy, Debug)]
enum Scope {
    Full,
    Panel,
}
#[derive(Clone, Copy, Debug)]
enum Probe {
    Real,
    Disabled,
    Measure,
    Sizing,
}
struct Owned {
    root: PathBuf,
    protected: BTreeMap<PathBuf, Vec<u8>>,
}
impl Owned {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "lc-denoise-work-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let mut owned = Self { root: root.canonicalize().unwrap(), protected: BTreeMap::new() };
        for name in ["originals", "models", "models/keep-model", "library", "library/denoise"] {
            std::fs::create_dir_all(owned.root.join(name)).unwrap();
        }
        for (index, name) in ["originals/a.png", "originals/b.png"].into_iter().enumerate() {
            let image = lightcraft_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 57 + index as u8 * 83, 255]);
            let options = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
            let bytes = lightcraft_engine::export::encode_image(&image, &options).unwrap();
            owned.keep(name, bytes);
        }
        owned.keep("models/keep-model/model.onnx", b"inert model preservation sentinel; never installed or loaded".to_vec());
        owned.keep("models/keep-model/model-note.txt", b"keep all original model files".to_vec());
        owned.keep("library/catalog-preservation.txt", b"not generated denoise cache".to_vec());
        println!("DENOISE_OWNED_ROOT {}", owned.root.display());
        owned
    }
    fn keep(&mut self, name: &str, bytes: Vec<u8>) {
        let path = self.root.join(name);
        self.inside(&path);
        std::fs::write(&path, &bytes).unwrap();
        self.protected.insert(path, bytes);
    }
    fn inside(&self, path: &Path) -> PathBuf {
        // Existing paths must resolve inside our successfully-created root, not just share text.
        let resolved =
            if path.exists() { path.canonicalize().unwrap() } else { path.parent().unwrap().canonicalize().unwrap().join(path.file_name().unwrap()) };
        assert!(resolved.starts_with(&self.root) && resolved != self.root, "unsafe test path {} -> {}", path.display(), resolved.display());
        resolved
    }
    fn intact(&self) {
        for (path, bytes) in &self.protected {
            self.inside(path);
            assert_eq!(&std::fs::read(path).unwrap(), bytes, "protected original/model/library file");
        }
        let originals = std::fs::read_dir(self.root.join("originals")).unwrap().map(|e| e.unwrap().path()).collect::<Vec<_>>();
        assert_eq!(originals.len(), 2, "no XMP sidecars, exports or extra originals");
    }
    fn contain(&self, s: &mut Session) {
        self.intact();
        let library = s.library.as_ref().expect("owned on-disk library");
        assert!(library.on_disk);
        assert_eq!(self.inside(&library.dir), self.root.join("library"));
        let list = s.execute("denoise.models.list", &json!({})).unwrap();
        assert_eq!(self.inside(Path::new(list["dir"].as_str().expect("owned models path"))), self.root.join("models"));
        assert_eq!(self.inside(Path::new(list["productsDir"].as_str().expect("owned products path"))), self.root.join("library/denoise"));
        for entry in std::fs::read_dir(self.root.join("library/denoise")).unwrap() {
            self.inside(&entry.unwrap().path());
        }
    }
    fn cache(&self) -> BTreeMap<String, Vec<u8>> {
        std::fs::read_dir(self.root.join("library/denoise"))
            .unwrap()
            .map(|e| {
                let path = e.unwrap().path();
                self.inside(&path);
                (path.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(path).unwrap())
            })
            .collect()
    }
    fn seed_cache(&self) {
        for (name, bytes) in [
            ("one.lcdn", b"generated-one".as_slice()),
            ("two.lcdn", b"generated-two-longer".as_slice()),
            ("keep-not-a-product.txt", b"not a denoise product".as_slice()),
        ] {
            let path = self.root.join("library/denoise").join(name);
            self.inside(&path);
            std::fs::write(path, bytes).unwrap();
        }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.root) {
            eprintln!("owned Denoise fixture cleanup {}: {e}", self.root.display());
        }
    }
}
struct Node {
    name: Option<String>,
    role: String,
    rect: Option<Rect>,
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
struct Review {
    app: LightcraftApp,
    ctx: egui::Context,
    owned: Owned,
    tick: u64,
    scope: Scope,
    probe: Probe,
    nodes: Vec<Node>,
    texts: Vec<(String, Rect)>,
    size: egui::Vec2,
}
fn text_shapes(shape: &egui::epaint::Shape, out: &mut Vec<(String, Rect)>) {
    match shape {
        egui::epaint::Shape::Vec(shapes) => {
            for shape in shapes {
                text_shapes(shape, out)
            }
        }
        egui::epaint::Shape::Text(text) => out.push((text.galley.job.text.clone(), Rect::from_min_size(text.pos, text.galley.size()))),
        _ => {}
    }
}
impl Review {
    fn new(scope: Scope) -> Self {
        crate::i18n::set_language(crate::i18n::Locale::En);
        let owned = Owned::new();
        let mut s = Session::new().with_fs();
        s.clock = Box::new(|| "2026-11-21T21:00:00Z".into());
        s.xmp.auto_write = false;
        let library = owned.inside(&owned.root.join("library"));
        s.open_library(library, false).unwrap();
        s.set_denoise_models_dir(Some(owned.inside(&owned.root.join("models"))));
        owned.contain(&mut s);
        s.execute("denoise.settings", &json!({"auto":false,"runOn":"cpu","cacheGb":20})).unwrap();
        s.execute("library.import", &json!({"paths":[owned.root.join("originals/a.png"),owned.root.join("originals/b.png")]})).unwrap();
        let ids = s.catalog.photos().map(|p| p.id.0).collect::<Vec<_>>();
        assert_eq!(ids.len(), 2);
        s.execute("library.select", &json!({"ids":[ids[0]],"active":ids[0]})).unwrap();
        let mut app = LightcraftApp::new(s, Services { png: None, local_places: Some(vec![]), ..Default::default() });
        app.headless_host = true;
        app.run("develop.merge", json!({"settings":{"light":{"exposure":0.4}},"label":"Owned fixture first step"})).unwrap();
        app.run("develop.merge", json!({"settings":{"light":{"exposure":0.8}},"label":"Owned fixture redo branch"})).unwrap();
        app.run("edit.undo", json!({})).unwrap();
        assert!(!app.session.undo.is_empty() && !app.session.redo.is_empty());
        app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "denoise".into() });
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        ctx.enable_accesskit();
        let mut r = Self { app, ctx, owned, tick: 0, scope, probe: Probe::Real, nodes: vec![], texts: vec![], size: egui::vec2(1400.0, 1100.0) };
        r.idle(18);
        assert_eq!(r.app.session.execute("denoise.status", &json!({})).unwrap()["enabled"], true);
        r.safe();
        r
    }
    fn docs(&self) -> Value {
        let s = &self.app.session;
        let history = |entries: &[lightcraft_engine::UndoEntry]| {
            json!(entries.iter().map(|e| json!({"label":e.label,"op":e.op,"folder":format!("{:?}",e.folder)})).collect::<Vec<_>>())
        };
        json!({"catalog":s.catalog,"revision":s.catalog.revision,"undo":history(&s.undo),"redo":history(&s.redo),"commits":s.commits(),"journal":s.journal,"selection":s.selection,"source":s.source,"browse":s.browse,"filter":s.filter,"interaction":s.interaction.is_some()})
    }
    fn settings(&self) -> Value {
        let path = self.owned.root.join("models/settings.json");
        self.owned.inside(&path);
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }
    fn safe(&mut self) {
        self.owned.contain(&mut self.app.session);
        assert!(!self.app.session.denoise_busy(), "non-raw fixtures must never run inference");
    }
    fn frame(&mut self, events: Vec<Event>) {
        self.safe();
        let ctx = self.ctx.clone();
        let mut raw = crate::headless::HeadlessView::raw_input(self.size, 1.0, self.tick as f64 / 60.0, events);
        self.app.set_simulated_clock(Some(self.tick as f64 / 60.0));
        self.app.install_framework(&ctx);
        self.app.raw_input_hook(&ctx, &mut raw);
        let app = &mut self.app;
        let scope = self.scope;
        let probe = self.probe;
        let mut registry = None;
        let output = Output(ctx.run_ui(raw, |root| {
            app.logic(&ctx);
            let mut draw = |ui: &mut egui::Ui| match scope {
                Scope::Full => app.ui(ui),
                Scope::Panel => {
                    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| crate::dialogs::settings::body(app, ui, "denoise"));
                }
            };
            match probe {
                Probe::Real => draw(root),
                Probe::Disabled => {
                    root.disable();
                    draw(root)
                }
                Probe::Measure => craft::ui::runtime::measure::run(&ctx, || draw(root)),
                Probe::Sizing => {
                    let mut child = root.new_child(egui::UiBuilder::new().max_rect(root.max_rect()).sizing_pass());
                    draw(&mut child)
                }
            }
            if matches!(scope, Scope::Panel) {
                registry = Some(crate::widgets::take_registries(&ctx));
            }
        }));
        self.app.drain_ui_completions(&ctx);
        if let Some((rects, hits)) = registry {
            self.app.widgets = rects;
            self.app.widget_hits = hits;
        }
        self.nodes.clear();
        if let Some(update) = &output.0.platform_output.accesskit_update {
            for (_, node) in &update.nodes {
                self.nodes.push(Node {
                    name: node.label().map(str::to_owned),
                    role: format!("{:?}", node.role()),
                    rect: node.bounds().map(|r| Rect::from_min_max(egui::pos2(r.x0 as f32, r.y0 as f32), egui::pos2(r.x1 as f32, r.y1 as f32))),
                })
            }
        }
        self.texts.clear();
        for shape in &output.0.shapes {
            text_shapes(&shape.shape, &mut self.texts)
        }
        self.tick += 1;
        self.safe();
    }
    fn idle(&mut self, n: usize) {
        for _ in 0..n {
            self.frame(vec![])
        }
    }
    fn rect(&self, name: &str) -> Option<Rect> {
        self.app
            .widgets
            .iter()
            .find(|(alias, _)| alias == name)
            .map(|(_, r)| *r)
            .or_else(|| self.nodes.iter().find(|node| node.name.as_deref() == Some(name) && node.role == "ComboBox").and_then(|node| node.rect))
    }
    fn reveal(&mut self, name: &str) -> Rect {
        for _ in 0..10 {
            if let Some(rect) = self.rect(name) {
                if rect.min.y >= 0.0 && rect.max.y < self.size.y {
                    return rect;
                }
            }
            self.frame(vec![
                Event::PointerMoved(egui::pos2(self.size.x / 2.0, self.size.y / 2.0)),
                Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, -180.0),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Unknown,
                    modifiers: Modifiers::NONE,
                },
            ]);
            self.idle(3);
        }
        panic!("real visible target absent {name}; aliases {:?}", self.app.widgets);
    }
    fn point(&mut self, name: &str) -> Pos2 {
        let rect = self.reveal(name);
        let at = rect.center();
        for _ in 0..3 {
            self.frame(vec![Event::PointerMoved(at)])
        }
        let owners = self
            .ctx
            .interaction_snapshot(|s| s.contains_pointer.iter().copied().collect::<Vec<_>>())
            .into_iter()
            .filter_map(|id| self.ctx.read_response(id))
            .filter(|r| {
                r.enabled()
                    && r.sense.senses_click()
                    && (r.rect == rect || r.interact_rect == rect)
                    && r.interact_rect.contains(at)
                    && self.ctx.layer_id_at(at) == Some(r.layer_id)
            })
            .collect::<Vec<_>>();
        assert_eq!(owners.len(), 1, "one exact real click owner, never a semantic/hover-only fake");
        println!(
            "DENOISE_NATIVE_TARGET {}",
            json!({"name":name,"point":[at.x,at.y],"owners":owners.iter().map(|r|format!("{:?}",r.id)).collect::<Vec<_>>()})
        );
        at
    }
    fn pointer(&mut self, at: Pos2, pressed: bool) {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
        ])
    }
    fn click(&mut self, name: &str) {
        let at = self.point(name);
        self.pointer(at, true);
        self.pointer(at, false);
        self.idle(6)
    }
    fn key(&mut self, key: Key, shift: bool) {
        let modifiers = Modifiers { shift, ..Default::default() };
        for pressed in [true, false] {
            self.frame(vec![Event::ModifiersChanged(modifiers), Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }])
        }
        self.idle(2)
    }
    fn pick(&mut self, gb: u32) {
        self.click(LIMIT);
        let text = format!("{gb} GB");
        let matches = self.texts.iter().filter(|(s, _)| s == &text).map(|(_, r)| *r).collect::<Vec<_>>();
        assert!(!matches.is_empty(), "real painted popup row {text}");
        let at = matches.last().unwrap().center();
        self.frame(vec![Event::PointerMoved(at)]);
        let owners = self
            .ctx
            .interaction_snapshot(|s| s.contains_pointer.iter().copied().collect::<Vec<_>>())
            .into_iter()
            .filter_map(|id| self.ctx.read_response(id))
            .filter(|r| r.enabled() && r.sense.senses_click() && r.interact_rect.contains(at))
            .collect::<Vec<_>>();
        assert!(!owners.is_empty(), "popup row has actual owner");
        self.pointer(at, true);
        self.pointer(at, false);
        self.idle(5)
    }
    fn tab_to(&mut self, name: &str, reverse: bool) {
        let mut trace = vec![];
        for _ in 0..180 {
            self.key(Key::Tab, reverse);
            let id = self.ctx.memory(|m| m.focused());
            let response = id.and_then(|id| self.ctx.read_response(id));
            trace.push(format!("{:?}: {:?}", id, response.as_ref().map(|r| (r.rect, r.interact_rect))));
            if let (Some(response), Some(rect)) = (response, self.rect(name)) {
                if response.rect == rect || response.interact_rect == rect {
                    println!("DENOISE_NATIVE_TAB {}", json!({"reverse":reverse,"target":name,"trace":trace}));
                    return;
                }
            }
        }
        panic!("natural Tab did not reach {name}: {trace:?}")
    }
}
#[test]
fn denoise_work_native_speed_auto_queue_reopen_preserve_complete_documents() {
    let mut r = Review::new(Scope::Full);
    let docs = r.docs();
    for (index, value) in [(0, "auto"), (1, "gpu"), (2, "cpu")] {
        let epoch = r.app.caches.denoise.epoch;
        r.click(&format!("button:denoiseRunOn-{index}"));
        assert_eq!(r.settings()["runOn"], value);
        assert_eq!(r.app.caches.denoise.epoch, epoch + 1);
        assert_eq!(r.docs(), docs);
        let epoch = r.app.caches.denoise.epoch;
        r.click(&format!("button:denoiseRunOn-{index}"));
        assert_eq!(r.app.caches.denoise.epoch, epoch);
        assert_eq!(r.docs(), docs);
    }
    for expected in [true, false] {
        r.click(AUTO);
        assert_eq!(r.settings()["auto"], expected);
        assert_eq!(r.docs(), docs);
    }
    r.click(QUEUE);
    assert_eq!(r.docs(), docs);
    assert!(!r.app.widgets.iter().any(|(name, _)| name == "denoise:stop"), "non-raw fixtures cannot queue a denoise job");
    println!(
        "DENOISE_UNTESTED active Stop/inference: PNG originals intentionally ineligible; existing Queue/Stop recorder tests retained separately"
    );
    r.app.ui.dialog = None;
    r.idle(4);
    r.app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "denoise".into() });
    r.idle(8);
    assert_eq!(r.settings()["runOn"], "cpu");
    assert_eq!(r.settings()["auto"], false);
    assert_eq!(r.docs(), docs);
}
#[test]
fn denoise_work_native_clear_removes_only_owned_products_and_preserves_history() {
    let mut r = Review::new(Scope::Full);
    r.owned.seed_cache();
    r.safe();
    let docs = r.docs();
    let settings = r.settings();
    let original_photos = json!(r.app.session.catalog.photos().collect::<Vec<_>>());
    assert_eq!(r.app.session.execute("denoise.status", &json!({})).unwrap()["cache"]["files"], 2);
    r.click(CLEAR);
    assert_eq!(r.docs(), docs);
    assert_eq!(r.settings(), settings);
    assert_eq!(r.owned.cache().keys().cloned().collect::<Vec<_>>(), vec!["keep-not-a-product.txt"]);
    r.click(CLEAR);
    assert_eq!(r.docs(), docs);
    r.app.run("edit.redo", json!({})).unwrap();
    r.app.run("edit.undo", json!({})).unwrap();
    assert_eq!(json!(r.app.session.catalog.photos().collect::<Vec<_>>()), original_photos, "actual photo Undo/Redo survives cache clearing");
    r.safe();
}
#[test]
fn denoise_work_native_limit_popup_select_cancel_current_preserve_below_budget_cache() {
    let mut r = Review::new(Scope::Full);
    r.owned.seed_cache();
    let cache = r.owned.cache();
    let docs = r.docs();
    let epoch = r.app.caches.denoise.epoch;
    r.pick(5);
    assert_eq!(r.settings()["cacheGb"], 5);
    assert_eq!(r.app.caches.denoise.epoch, epoch + 1);
    assert_eq!(r.owned.cache(), cache, "tiny below-budget products survive immediate eviction check");
    assert_eq!(r.docs(), docs);
    let epoch = r.app.caches.denoise.epoch;
    r.pick(5);
    assert_eq!(r.app.caches.denoise.epoch, epoch);
    r.click(LIMIT);
    r.key(Key::Escape, false);
    assert_eq!(r.settings()["cacheGb"], 5);
    assert_eq!(r.docs(), docs);
    println!("DENOISE_UNTESTED over-budget deletion: no multi-gigabyte fake cache; engine eviction tests retained separately");
}
#[test]
fn denoise_work_native_natural_keyboard_limit_then_clear_reverse_and_popup_keys() {
    let mut r = Review::new(Scope::Full);
    r.owned.seed_cache();
    let docs = r.docs();
    r.tab_to(LIMIT, false);
    r.key(Key::Space, false);
    r.key(Key::ArrowDown, false);
    r.key(Key::Enter, false);
    assert_eq!(r.settings()["cacheGb"], 50);
    r.tab_to(CLEAR, false);
    r.key(Key::Space, false);
    assert_eq!(r.owned.cache().len(), 1);
    r.tab_to(LIMIT, true);
    r.key(Key::Enter, false);
    r.key(Key::Escape, false);
    assert_eq!(r.docs(), docs);
}
#[test]
fn denoise_work_native_hidden_parent_and_outside_releases_cannot_clear() {
    let mut r = Review::new(Scope::Full);
    r.owned.seed_cache();
    let before = r.owned.cache();
    let docs = r.docs();
    let at = r.point(CLEAR);
    r.pointer(at, true);
    r.app.ui.dialog = None;
    r.idle(4);
    r.pointer(at, false);
    r.app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "denoise".into() });
    r.idle(6);
    assert_eq!(r.owned.cache(), before);
    assert_eq!(r.docs(), docs);
    let at = r.point(CLEAR);
    r.pointer(at, true);
    r.pointer(Pos2::new(3.0, 3.0), false);
    assert_eq!(r.owned.cache(), before);
    r.click(CLEAR);
    assert_eq!(r.owned.cache().len(), 1);
    assert_eq!(r.docs(), docs);
}
fn nonreal(probe: Probe) {
    for name in [CLEAR, LIMIT] {
        for real_press in [false, true] {
            let mut r = Review::new(Scope::Panel);
            r.owned.seed_cache();
            let docs = r.docs();
            let settings = r.settings();
            let cache = r.owned.cache();
            let at = r.point(name);
            println!("DENOISE_NATIVE_NONREAL {probe:?} {name} real_press={real_press}");
            if real_press {
                r.pointer(at, true);
                r.probe = probe;
                r.pointer(at, false);
            } else {
                r.probe = probe;
                r.pointer(at, true);
                r.probe = Probe::Real;
                r.pointer(at, false);
            }
            r.probe = Probe::Real;
            r.idle(6);
            assert_eq!(r.docs(), docs);
            assert_eq!(r.settings(), settings);
            assert_eq!(r.owned.cache(), cache, "nonreal release cannot clear, evict or replay");
            if name == CLEAR {
                r.click(CLEAR);
                assert_eq!(r.owned.cache().len(), 1);
            } else {
                r.pick(5);
                assert_eq!(r.settings()["cacheGb"], 5);
            }
            assert_eq!(r.docs(), docs);
        }
    }
}
#[test]
fn denoise_work_native_disabled_pending_input_cannot_replay() {
    nonreal(Probe::Disabled)
}
#[test]
fn denoise_work_native_sdk_measure_pending_input_cannot_replay() {
    nonreal(Probe::Measure)
}
#[test]
fn denoise_work_native_egui_sizing_pending_input_cannot_replay() {
    nonreal(Probe::Sizing)
}
#[test]
fn denoise_work_native_retry_visibility_reports_unreachable_without_hardware() {
    let mut r = Review::new(Scope::Full);
    let status = r.app.session.execute("denoise.status", &json!({})).unwrap();
    assert_ne!(status["device"]["retry"], true, "no test may force GPU work merely to manufacture retry");
    assert!(!r.app.widgets.iter().any(|(name, _)| name == "denoise:retryGpu"));
    println!("DENOISE_UNTESTED retry activation: genuine public no-inference status {}", status["device"]);
    r.safe();
}
#[test]
fn denoise_work_native_owned_cleanup_after_catalog_and_settings_are_dropped() {
    let mut r = Review::new(Scope::Full);
    let path = r.owned.root.clone();
    let docs = r.docs();
    r.click(CLEAR);
    assert_eq!(r.docs(), docs);
    r.safe();
    drop(r);
    assert!(!path.exists(), "owned test tree cleaned after app/session drop");
}
