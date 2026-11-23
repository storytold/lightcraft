//! Native baseline only: real owned PNG folders, Browse workers and catalog transactions.
//! No mock command dispatch, forced focus, altered production behavior or SDK candidate.
use crate::{LightcraftApp, Services, headless::Headless};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};
use lightcraft_catalog::{Op, PhotoId, Source};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const CHECK: &str = "check:includeSubfolders";
const ADD: &str = "button:addToLibrary";
const WAIT: Duration = Duration::from_secs(120);

struct OwnedFolders {
    root: PathBuf,
    files: BTreeMap<PathBuf, Vec<u8>>,
}
impl OwnedFolders {
    fn new() -> Self {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("lc-folder-header-{}-{nonce}", std::process::id()));
        // Never remove a pre-existing path. This guard owns only a successfully created directory.
        std::fs::create_dir(&root).unwrap();
        let mut owned = Self { root, files: BTreeMap::new() };
        for (n, rel) in ["first/a.png", "first/b.png", "first/hidden.png", "first/inner/c.png", "second/d.png"].iter().enumerate() {
            let path = owned.root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let image = lightcraft_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 20 + 31 * n as u8, 255]);
            let options = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
            let bytes = lightcraft_engine::export::encode_image(&image, &options).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            owned.files.insert(path, bytes);
        }
        println!("FOLDER_OWNED_FIXTURE {}", owned.root.display());
        owned
    }
    fn intact(&self) {
        for (path, bytes) in &self.files {
            assert_eq!(&std::fs::read(path).unwrap(), bytes, "original image bytes: {}", path.display());
        }
        fn walk(path: &Path, found: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, found);
                } else {
                    found.push(path);
                }
            }
        }
        let mut found = vec![];
        walk(&self.root, &mut found);
        found.sort();
        assert_eq!(found, self.files.keys().cloned().collect::<Vec<_>>(), "no sidecars, exports or library files");
    }
}
impl Drop for OwnedFolders {
    fn drop(&mut self) {
        // app/Headless is declared before this field and drops first; all normal tests settle workers.
        if let Err(error) = std::fs::remove_dir_all(&self.root) {
            eprintln!("owned fixture cleanup failed: {}: {error}", self.root.display());
        }
    }
}
struct Review {
    h: Headless,
    owned: OwnedFolders,
    ids: BTreeMap<String, PhotoId>,
}
impl Review {
    fn new() -> Self {
        crate::i18n::set_language(crate::i18n::Locale::En);
        let owned = OwnedFolders::new();
        let mut session = lightcraft_engine::Session::new();
        session.xmp.auto_write = false;
        // Genuine synchronous engine Browse seeds both owned trees, including the later-hidden row.
        for dir in ["first", "second"] {
            let result = session.execute("library.browse", &json!({"path":owned.root.join(dir),"subfolders":true})).unwrap();
            assert_eq!(result["failed"], 0, "fixture PNGs really imported: {result}");
        }
        let ids = session
            .catalog
            .photos()
            .map(|photo| {
                let Source::File { path } = &photo.source else { panic!("only fixture files are permitted") };
                (Path::new(path).strip_prefix(&owned.root).unwrap().to_string_lossy().replace('\\', "/"), photo.id)
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(ids.len(), 5);
        let mut app = LightcraftApp::new(session, Services { local_places: Some(vec![]), png: None, ..Default::default() });
        app.ui.view = crate::state::ViewMode::PhotoGrid;
        app.run("library.browse", json!({"path":owned.root.join("first"),"subfolders":false})).unwrap();
        let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
        assert!(h.settle(WAIT));
        let mut r = Self { h, owned, ids };
        assert!(r.settle());
        assert_eq!(r.visible(), r.named(&["first/a.png", "first/b.png", "first/hidden.png"]));
        r.safety();
        r
    }
    fn named(&self, names: &[&str]) -> Vec<PhotoId> {
        let mut ids = names.iter().map(|name| self.ids[*name]).collect::<Vec<_>>();
        ids.sort();
        ids
    }
    fn visible(&mut self) -> Vec<PhotoId> {
        let mut ids = self.h.app.session.visible_cloned();
        ids.sort();
        ids
    }
    fn safety(&self) {
        self.owned.intact();
        let s = &self.h.app.session;
        assert!(s.library.is_none() && s.library_folder.is_none());
        assert!(self.h.app.services.write.is_none() && self.h.app.services.write_shared.is_none());
        assert!(self.h.app.services.picker.is_none() && self.h.app.services.pick_files.is_none() && self.h.app.services.pick_folder.is_none());
        assert!(self.h.app.export.is_none() && self.h.app.merge.preview_task.is_none() && self.h.app.merge.final_task.is_none());
        assert!(s.catalog.photos().all(|p| matches!(&p.source, Source::File { path } if Path::new(path).starts_with(&self.owned.root))));
    }
    fn exec(&mut self, command: &str, params: Value) {
        self.h.app.run(command, params).unwrap();
        assert!(self.settle());
        self.safety();
    }
    fn rect(&self, alias: &str) -> Rect {
        self.h.app.widgets.iter().find(|(name, _)| name == alias).unwrap_or_else(|| panic!("missing {alias}: {:?}", self.h.app.widgets)).1
    }
    fn frame(&mut self, events: Vec<Event>) {
        self.grid_frame(events, None);
    }
    fn settle(&mut self) -> bool {
        let start = std::time::Instant::now();
        let mut quiet = 0;
        loop {
            self.frame(vec![]);
            if self.h.busy() {
                quiet = 0;
                std::thread::sleep(Duration::from_millis(1));
            } else {
                quiet += 1;
            }
            if quiet >= 12 {
                return true;
            }
            if start.elapsed() > WAIT {
                return false;
            }
        }
    }

    fn pointer(&mut self, pos: Pos2, pressed: bool) {
        self.frame(vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
    }
    fn ready(&mut self, alias: &str) -> Pos2 {
        let point = self.rect(alias).center();
        for _ in 0..3 {
            self.frame(vec![Event::PointerMoved(point)]);
        }
        let rect = self.rect(alias);
        assert!(rect.contains(point));
        let ctx = &self.h.view.ctx;
        let ids = ctx.interaction_snapshot(|s| s.contains_pointer.iter().copied().collect::<Vec<_>>());
        let owners = ids
            .into_iter()
            .filter_map(|id| ctx.read_response(id))
            .filter(|response| {
                response.sense.senses_click()
                    && response.enabled()
                    && (response.rect == rect || response.interact_rect == rect)
                    && response.interact_rect.contains(point)
                    && ctx.layer_id_at(point) == Some(response.layer_id)
            })
            .collect::<Vec<_>>();
        assert_eq!(owners.len(), 1, "exactly one real native owner at {alias}; hover probes cannot sense click");
        println!(
            "FOLDER_BOUNDARY_READY {}",
            json!({"alias":alias,"owner":format!("{:?}",owners[0].id),"point":[point.x,point.y],"rect":[rect.left(),rect.top(),rect.right(),rect.bottom()]})
        );
        point
    }
    fn click(&mut self, alias: &str) {
        let pos = self.ready(alias);
        self.pointer(pos, true);
        self.pointer(pos, false);
        assert!(self.settle());
    }
    fn key(&mut self, key: Key, shift: bool) {
        let modifiers = Modifiers { shift, ..Default::default() };
        for pressed in [true, false] {
            self.frame(vec![Event::ModifiersChanged(modifiers), Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
        }
        self.h.step();
    }
    fn focus(&self) -> Option<String> {
        let id = self.h.view.ctx.memory(|m| m.focused())?;
        let response = self.h.view.ctx.read_response(id)?;
        self.h
            .app
            .widgets
            .iter()
            .find(|(_, rect)| (*rect == response.rect || *rect == response.interact_rect) && response.sense.senses_click())
            .map(|(name, _)| name.clone())
    }
    fn tab_to(&mut self, alias: &str, shift: bool) -> Vec<String> {
        let mut stops = vec![];
        for _ in 0..180 {
            self.key(Key::Tab, shift);
            let name = self.focus().unwrap_or_else(|| format!("other:{:?}", self.h.view.ctx.memory(|m| m.focused())));
            stops.push(name.clone());
            if name == alias {
                println!("FOLDER_BOUNDARY_TAB {}", json!({"shift":shift,"stops":stops}));
                return stops;
            }
        }
        panic!("natural traversal did not reach {alias}: {stops:?}");
    }
    fn history(entries: &[lightcraft_engine::UndoEntry]) -> Value {
        json!(entries.iter().map(|e| json!({"label":e.label,"op":e.op,"folder":format!("{:?}",e.folder)})).collect::<Vec<_>>())
    }
    fn stable(&self) -> Value {
        let s = &self.h.app.session;
        json!({"catalog":s.catalog,"revision":s.catalog.revision,"undo":Self::history(&s.undo),"redo":Self::history(&s.redo),"commits":s.commits(),"journal":s.journal,"selection":s.selection,"source":s.source,"browse":s.browse,"filter":s.filter,"effects":s.mask_effects,"clipboard":s.clipboard})
    }
    fn docs(&self) -> Value {
        json!(self.h.app.session.catalog.photos().collect::<Vec<_>>())
    }
    fn add_journal(&self) -> Vec<Value> {
        self.h.app.session.journal.iter().filter(|(id, _)| id == "photo.addToLibrary").map(|(_, p)| p.clone()).collect()
    }
    fn chrome(&self) -> Value {
        json!(
            self.h
                .app
                .widgets
                .iter()
                .filter(|(name, _)| name.starts_with("crumb:") || name == "grid:count")
                .map(|(name, r)| json!({"alias":name,"rect":[r.left(),r.top(),r.right(),r.bottom()]}))
                .collect::<Vec<_>>()
        )
    }
    fn assert_add(&mut self, activate: impl FnOnce(&mut Self)) {
        let visible = self.h.app.session.visible_cloned();
        let s = &self.h.app.session;
        let targets = visible.into_iter().filter(|id| s.catalog.photo(*id).unwrap().local).collect::<Vec<_>>();
        assert!(!targets.is_empty());
        let before = s.catalog.clone();
        let photos = self.docs();
        let undo = Self::history(&s.undo);
        let n = s.undo.len();
        let journal = self.add_journal();
        let selection = json!(s.selection);
        let browse = json!(s.browse);
        let filter = json!(s.filter);
        let source = json!(s.source);
        let forward = Op::Batch { ops: targets.iter().map(|id| Op::SetLocal { id: *id, local: false }).collect() };
        let mut expected = before.clone();
        let inverse = expected.apply(forward.clone()).unwrap();
        let chrome = self.chrome();
        activate(self);
        assert!(self.settle());
        self.safety();
        let s = &self.h.app.session;
        assert_eq!(json!(s.catalog), json!(expected), "complete expected catalog after native Add");
        assert_eq!(s.catalog.revision, expected.revision);
        assert_eq!(s.undo.len(), n + 1);
        assert!(s.redo.is_empty());
        assert_eq!(Self::history(&s.undo[..n]), undo);
        assert_eq!(s.undo.last().unwrap().label, "Add to My Photos");
        assert_eq!(json!(s.undo.last().unwrap().op), json!(inverse));
        let mut expected_journal = journal;
        expected_journal.push(json!({"ids":targets.iter().map(|id|id.0).collect::<Vec<_>>()}));
        assert_eq!(self.add_journal(), expected_journal, "exactly one visible-Local payload");
        assert_eq!(
            (json!(s.selection), json!(s.browse), json!(s.filter), json!(s.source)),
            (selection.clone(), browse.clone(), filter.clone(), source.clone())
        );
        assert_eq!(self.chrome(), chrome, "breadcrumbs/count geometry survives removing only the Add action");
        assert!(!self.h.app.widgets.iter().any(|(name, _)| name == ADD));
        self.exec("edit.undo", json!({}));
        expected.apply(inverse.clone()).unwrap();
        assert_eq!(json!(self.h.app.session.catalog), json!(expected));
        assert_eq!(self.docs(), photos);
        assert_eq!(Self::history(&self.h.app.session.undo), undo);
        assert_eq!(self.h.app.session.redo.len(), 1);
        assert!(self.h.app.widgets.iter().any(|(name, _)| name == ADD));
        self.exec("edit.redo", json!({}));
        expected.apply(forward.clone()).unwrap();
        assert_eq!(json!(self.h.app.session.catalog), json!(expected), "complete expected catalog after Redo");
        assert_eq!(self.h.app.session.catalog.revision, expected.revision);
        assert_eq!(json!(self.h.app.session.undo.last().unwrap().op), json!(inverse));
        assert_eq!(self.h.app.session.undo.len(), n + 1);
        assert!(self.h.app.session.redo.is_empty());
        let mut expected_photos = before.clone();
        expected_photos.apply(forward).unwrap();
        assert_eq!(json!(self.h.app.session.catalog.photos().collect::<Vec<_>>()), json!(expected_photos.photos().collect::<Vec<_>>()));
        assert_eq!(
            (
                json!(self.h.app.session.selection),
                json!(self.h.app.session.browse),
                json!(self.h.app.session.filter),
                json!(self.h.app.session.source)
            ),
            (selection, browse, filter, source)
        );
    }
}

#[derive(Clone, Copy, Debug)]
enum NonrealPass {
    Disabled,
    Measure,
    Sizing,
}
impl Review {
    fn nonreal_frame(&mut self, events: Vec<Event>, pass: NonrealPass) {
        self.grid_frame(events, Some(pass));
        if matches!(pass, NonrealPass::Measure | NonrealPass::Sizing) {
            assert!(!self.h.app.widgets.iter().any(|(name, _)| name == CHECK || name == ADD), "nonreal boundary must publish neither SDK target");
        }
    }
    fn grid_frame(&mut self, events: Vec<Event>, pass: Option<NonrealPass>) {
        let h = &mut self.h;
        let ctx = h.view.ctx.clone();
        let mut raw = crate::headless::HeadlessView::raw_input(h.size, h.pixels_per_point, h.time, events);
        h.app.set_simulated_clock(Some(h.time));
        h.app.install_framework(&ctx);
        h.app.raw_input_hook(&ctx, &mut raw);
        let app = &mut h.app;
        crate::widgets::take_registries(&ctx);
        let mut last_registry = None;
        h.view.run(raw, |root| {
            app.logic(root.ctx());
            let mut draw = |ui: &mut egui::Ui| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                    // The real production grid owns source/visible-count/header selection and
                    // calls folder_header. No copied header or test-only command path is used.
                    super::show(app, ui);
                });
            };
            match pass {
                None => draw(root),
                Some(NonrealPass::Disabled) => {
                    root.disable();
                    draw(root);
                }
                Some(NonrealPass::Measure) => craft::ui::runtime::measure::run(&ctx, || draw(root)),
                Some(NonrealPass::Sizing) => {
                    let mut child = root.new_child(egui::UiBuilder::new().max_rect(root.max_rect()).sizing_pass());
                    draw(&mut child);
                }
            }
            last_registry = Some(crate::widgets::take_registries(&ctx));
        });
        // Direct grid rendering has no whole-app completion snapshot. Publish only the registry
        // this actual completed draw produced; do not invent widgets or alter their responses.
        let (rects, hits) = last_registry.unwrap();
        h.app.widgets = rects;
        h.app.widget_hits = hits;
        h.app.drain_ui_completions(&ctx);
        h.time += 1.0 / 60.0;
        self.safety();
    }
}
#[test]
fn folder_header_boundary_disabled_measure_sizing_input_is_inert_and_cannot_replay() {
    for pass in [NonrealPass::Disabled, NonrealPass::Measure, NonrealPass::Sizing] {
        for alias in [CHECK, ADD] {
            for real_press in [false, true] {
                println!("FOLDER_BOUNDARY_CASE {pass:?} {alias} real_press={real_press}");
                let mut r = Review::new();
                let point = r.ready(alias);
                let before = r.stable();
                let pointer = |pressed| {
                    vec![
                        Event::PointerMoved(point),
                        Event::PointerButton { pos: point, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
                    ]
                };
                if real_press {
                    r.pointer(point, true);
                    r.nonreal_frame(pointer(false), pass);
                    r.frame(vec![]);
                } else {
                    r.nonreal_frame(pointer(true), pass);
                    r.pointer(point, false);
                }
                assert!(r.settle());
                assert_eq!(r.stable(), before, "{pass:?}, {alias}, real_press={real_press}: no dispatch or delayed release replay");
                // Every negative is followed by fresh ordinary observed input on this same owner.
                if alias == ADD {
                    r.assert_add(|r| r.click(ADD));
                } else {
                    r.click(CHECK);
                    assert!(r.h.app.session.browse.as_ref().unwrap().subfolders);
                }
            }
        }
    }
}
