//! Actual installed Create Album/Folder parent; no app tick, provider, picker or disk library.
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
        "templates":s.print_templates,"effects":s.mask_effects,"keywordSets":s.keyword_sets,"keywordSet":s.keyword_set})
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
    fn new(folder: bool, name: &str, include: bool, empty: bool) -> Self {
        let previous_locale = crate::i18n::language();
        crate::i18n::set_language(crate::i18n::Locale::En);
        let (mut app, mut oracle) = (model(), model());
        if empty {
            for app in [&mut app, &mut oracle] {
                app.run("library.select", json!({"ids":[]})).unwrap();
            }
        }
        let baseline = document(&app);
        assert_eq!(baseline, document(&oracle));
        let settings = json!(app.ui.settings);
        app.run(if folder { "dialog.newFolder" } else { "dialog.newAlbum" }, json!({"name":name,"parent":1,"includeSelected":include})).unwrap();
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
                        let mut confirm = false;
                        super::new_album_body(app, ui, &mut confirm, &mut dlg);
                        assert!(!confirm, "nonreal field cannot submit");
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
            "frame={} input={} focused={:?} dialog={:?} widgets={:?}",
            self.tick,
            input,
            self.ctx.memory(|m| m.focused()),
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
    fn has(&self, id: &str) -> bool {
        self.widgets.iter().any(|(name, _)| name == id)
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
    fn tab_to(&mut self, id: &str) {
        for _ in 0..16 {
            if self.focused(id) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab did not reach {id}; trace={:#?}", self.trace);
    }
    fn draft(&self) -> Value {
        json!(self.app.ui.dialog)
    }
    fn include(&self) -> bool {
        let Some(Dialog::NewAlbum { include_selected, .. }) = &self.app.ui.dialog else { panic!("live NewAlbum expected") };
        *include_selected
    }
    fn arm(&mut self, name: &str, folder: bool, include: bool) -> u64 {
        let result = self.oracle.run("album.create", json!({"name":name,"folder":folder,"addSelected":!folder&&include,"parent":1})).unwrap();
        self.pending = Some(document(&self.oracle));
        result["id"].as_u64().unwrap()
    }
    fn committed(&mut self, id: u64) {
        assert!(self.app.ui.dialog.is_none());
        assert_eq!(document(&self.app), self.pending.take().unwrap());
        self.baseline = document(&self.app);
        assert_eq!(self.app.ui.reveal_album, Some(id));
        self.settle();
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
#[test]
fn create_album_pointer_toggle_excludes_selection_and_keeps_parent_history_and_undo() {
    let mut f = Fixture::new(false, "Draft", true, false);
    assert!(f.include());
    f.click("check:albumIncludeSelected");
    assert!(!f.include());
    f.click("field:albumName");
    f.key_mod(Key::A, Modifiers::COMMAND);
    f.text("Excluded");
    let id = f.arm("Excluded", false, false);
    f.click("button:dialogOk");
    f.committed(id);
    assert!(f.app.session.catalog.album(lightcraft_catalog::AlbumId(id)).unwrap().photos.is_empty());
    f.undo_redo();
}
#[test]
fn create_album_includes_only_selected_library_ids_and_preserves_local_and_other_album() {
    let mut f = Fixture::new(false, "Included", true, false);
    let id = f.arm("Included", false, true);
    f.key(Key::Enter);
    f.committed(id);
    let album = f.app.session.catalog.album(lightcraft_catalog::AlbumId(id)).unwrap();
    assert_eq!(album.photos, vec![PhotoId(101)]);
    assert_eq!(album.parent, Some(lightcraft_catalog::AlbumId(1)));
    assert_eq!(f.app.session.catalog.album(lightcraft_catalog::AlbumId(2)).unwrap().photos, vec![PhotoId(101)]);
    assert!(f.app.session.catalog.photo(PhotoId(103)).unwrap().local);
    f.undo_redo();
}
#[test]
fn create_album_folder_and_empty_selection_hide_include_without_adding_photos() {
    for (folder, empty, name) in [(true, false, "Nested folder"), (false, true, "Empty album")] {
        let mut f = Fixture::new(folder, name, true, empty);
        assert!(!f.has("check:albumIncludeSelected"));
        let id = f.arm(name, folder, true);
        f.key(Key::Enter);
        f.committed(id);
        let album = f.app.session.catalog.album(lightcraft_catalog::AlbumId(id)).unwrap();
        assert_eq!(album.folder, folder);
        assert!(album.photos.is_empty());
        f.undo_redo();
    }
}
#[test]
fn create_album_empty_name_refuses_without_document_effect_and_then_corrects_once() {
    let mut f = Fixture::new(false, "", false, false);
    f.key(Key::Enter);
    assert!(f.app.ui.dialog.is_some());
    assert_eq!(document(&f.app), f.baseline);
    assert!(f.focused("field:albumName"));
    f.text("Corrected");
    let id = f.arm("Corrected", false, false);
    f.key(Key::Enter);
    f.committed(id);
    f.undo_redo();
}
#[test]
fn create_album_cancel_and_escape_discard_changed_drafts_without_commands() {
    for pointer in [false, true] {
        let mut f = Fixture::new(false, "Draft", true, false);
        f.text("Discarded");
        f.click("check:albumIncludeSelected");
        assert!(!f.include());
        if pointer {
            f.click("button:dialogCancel");
        } else {
            f.key(Key::Escape);
        }
        assert!(f.app.ui.dialog.is_none());
        assert_eq!(document(&f.app), f.baseline);
        f.settle();
    }
}
#[test]
fn create_album_natural_keyboard_toggles_then_creates_once_with_reverse_tab() {
    let mut f = Fixture::new(false, "Keyboard", true, false);
    assert!(f.focused("field:albumName"));
    f.tab_to("check:albumIncludeSelected");
    f.key(Key::Space);
    assert!(!f.include());
    f.tab_to("button:dialogOk");
    f.key_mod(Key::Tab, Modifiers::SHIFT);
    assert!(f.focused("button:dialogCancel"));
    f.key(Key::Tab);
    assert!(f.focused("button:dialogOk"));
    let id = f.arm("Keyboard", false, false);
    f.key(Key::Enter);
    f.committed(id);
    f.undo_redo();
}
#[test]
fn create_album_checkbox_disappearance_cancels_release_and_accepts_fresh_press() {
    let mut f = Fixture::new(false, "Draft", true, false);
    f.frame(vec![Event::PointerMoved(f.rect("check:albumIncludeSelected").center())]);
    f.settle();
    let point = f.rect("check:albumIncludeSelected").center();
    f.pointer(point, true);
    f.select(json!([]));
    f.frame(vec![]);
    assert!(!f.has("check:albumIncludeSelected"));
    f.pointer(point, false);
    f.settle();
    assert!(f.include());
    f.select(json!([101, 103]));
    f.settle();
    assert!(f.include());
    f.click("check:albumIncludeSelected");
    assert!(!f.include());
}
#[test]
fn create_album_blocked_parent_and_nonreal_body_preserve_draft_document_and_release() {
    for initial in [false, true] {
        for probe in 0..3 {
            let mut f = Fixture::new(false, "Protected", initial, false);
            // Prime this exact real leaf before nonreal input. Inert SDK passes
            // correctly publish no actionable automation aliases of their own.
            f.probe = if probe == 0 { 0 } else { 3 };
            f.settle();
            let point = f.rect("check:albumIncludeSelected").center();
            f.blocked = probe == 0;
            f.probe = probe;
            f.settle();
            let before = f.draft();
            f.pointer(point, true);
            f.pointer(point, false);
            f.text("not a draft edit");
            f.key(Key::Space);
            f.key(Key::Enter);
            assert_eq!(f.draft(), before, "disabled/measure/sizing draft stays exact");
            assert_eq!(document(&f.app), f.baseline);
            f.blocked = false;
            f.probe = if probe == 0 { 0 } else { 3 };
            f.settle();
            f.frame(vec![
                Event::PointerMoved(point),
                Event::PointerButton { pos: point, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE },
            ]);
            f.settle();
            assert_eq!(f.draft(), before, "no replay on the first real pass");
            f.click("check:albumIncludeSelected");
            assert_eq!(f.include(), !initial);
        }
    }
}
