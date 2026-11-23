//! Real Rename parent preview/Cancel, with catalog-only Demo sources and no rename confirmation.
use crate::LightcraftApp;
use crate::headless::HeadlessView;
use crate::state::Dialog;
use egui::{Event, Key, Modifiers, PointerButton, Rect};
use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_engine::rename::RenamePlan;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TEMPLATE: &str = "field:renameTemplate";
type Rows = Arc<Mutex<Option<Vec<RenamePlan>>>>;

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
    tick: u32,
    ids: [PhotoId; 2],
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    sdk: BTreeMap<String, String>,
    painted: Vec<String>,
    unchanged: Value,
    old_language: crate::i18n::Locale,
}
impl Review {
    fn new() -> Self {
        let old_language = crate::i18n::language();
        crate::i18n::set_language(crate::i18n::Locale::En);
        let mut session = lightcraft_engine::Session::new();
        let ids = [session.catalog.alloc_photo_id(), session.catalog.alloc_photo_id()];
        for (id, name, format) in [(ids[0], "IMG_0042.CR2", "CR2"), (ids[1], "IMG_0099.JPG", "JPEG")] {
            let photo = Photo::new(id, Source::Demo { scene: 0 }, name, format, 6000, 4000, "2026-01-14T05:58:48");
            session.commit("fixture", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        session.execute("library.select", &json!({"ids":ids})).unwrap();
        assert_eq!(session.selected().len(), 2);
        let app = LightcraftApp::new(session, crate::Services { local_places: Some(vec![]), png: None, ..Default::default() });
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut this =
            Self { app, ctx, tick: 0, ids, rects: vec![], hits: vec![], sdk: BTreeMap::new(), painted: vec![], unchanged: Value::Null, old_language };
        this.unchanged = this.document();
        this.app.run("dialog.rename", json!({"template":"{name}","start":5})).unwrap();
        this.idle();
        this.draft("{name}", 5);
        this
    }
    fn document(&self) -> Value {
        let s = &self.app.session;
        json!({"catalog":s.catalog,"undo":format!("{:?}",s.undo),"redo":format!("{:?}",s.redo),
            "commits":s.commits(),"journal":s.journal,"selection":format!("{:?}",s.selection),"selected":s.selected(),
            "settings":self.app.ui.settings,"imports":s.import_defaults,"xmp":s.xmp,"lastExport":s.last_export,
            "presets":s.presets.iter().cloned().collect::<Vec<_>>(),"print":s.print,"printTemplates":s.print_templates,
            "toast":self.app.ui.toast.as_ref().map(|toast|toast.0.as_str()),"pendingPicks":self.app.pending_picks.len()})
    }
    fn safety(&self) {
        let s = &self.app.session;
        assert_eq!(s.catalog.len(), 2);
        assert!(s.catalog.photos().all(|photo| matches!(photo.source, Source::Demo { .. })), "no file source may reach a disk probe or rename");
        assert!(s.library.is_none() && s.library_folder.is_none());
        assert!(s.smart_previews_dir.is_none() && s.media.rendered.disk().is_none());
        assert!(self.app.services.write.is_none() && self.app.services.write_shared.is_none());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_files.is_none() && self.app.services.pick_folder.is_none());
        assert!(self.app.services.png.is_none());
        assert!(self.app.pending_picks.is_empty());
        assert!(self.app.import.is_none() && self.app.export.is_none() && self.app.scan.is_none());
        assert_eq!(self.app.renderer.in_flight(), 0);
        assert_eq!(self.document(), self.unchanged, "preview and draft controls never rename or mutate the catalog/history/journal");
    }
    fn texts(shape: &egui::epaint::Shape, clip: Rect, out: &mut Vec<String>) {
        match shape {
            egui::epaint::Shape::Vec(shapes) => {
                for shape in shapes {
                    Self::texts(shape, clip, out);
                }
            }
            egui::epaint::Shape::Text(text) if clip.contains_rect(text.galley.rect.translate(text.pos.to_vec2())) => {
                out.push(text.galley.job.text.clone());
            }
            _ => {}
        }
    }
    fn frame(&mut self, events: Vec<Event>) {
        self.safety();
        crate::widgets::take_registries(&self.ctx);
        self.tick += 1;
        let raw = HeadlessView::raw_input(egui::vec2(1600.0, 1200.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let ctx = self.ctx.clone();
        let mut last = None;
        let out = Output(ctx.run_ui(raw, |root| {
            let (_, recorded) = craft::ui::runtime::inspect::record(root, egui::Id::unique("rename-preview-sdk-owner"), |_| {
                // Only the actual dialog. Never app.ui/logic/renderer or a grid thumbnail pass.
                crate::dialogs::show(app, &ctx, false);
            });
            last = Some((crate::widgets::take_registries(&ctx), recorded));
        }));
        let ((rects, hits), recorded) = last.unwrap();
        self.sdk.clear();
        for (name, hit) in &hits {
            if let Some(element) =
                recorded.iter().flat_map(|record| record.elements.iter()).find(|element| element.id == hit.id && element.part.is_none())
            {
                self.sdk.insert(name.clone(), element.component.to_string());
            }
        }
        self.rects = rects;
        self.hits = hits;
        self.painted.clear();
        for shape in &out.0.shapes {
            Self::texts(&shape.shape, shape.clip_rect, &mut self.painted);
        }
        drop(out);
        self.safety();
    }
    fn idle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![]);
        }
    }
    fn draft(&self, template: &str, start: u32) {
        assert_eq!(self.app.ui.dialog, Some(Dialog::Rename { template: template.into(), start }));
    }
    fn rect(&self, id: &str) -> Rect {
        let rects: Vec<_> = self.rects.iter().filter(|(name, _)| name == id).collect();
        assert_eq!(rects.len(), 1, "one actual presentation of {id}");
        rects[0].1
    }
    fn visible(&self, id: &str) -> Rect {
        let rect = self.rect(id);
        let hit = self.hits.iter().find(|(name, _)| name == id).unwrap().1;
        assert!(self.rect("dialog:window").contains_rect(rect) && self.ctx.content_rect().contains_rect(rect));
        assert!(hit.interact_rect.contains(rect.center()), "{id} is clipped, not a click target");
        rect
    }
    fn settle_target(&mut self, id: &str) {
        let mut previous = None;
        let mut stable = 0;
        for _ in 0..32 {
            self.frame(vec![]);
            let rect = self.visible(id);
            let hit = self.hits.iter().find(|(name, _)| name == id).unwrap().1.interact_rect;
            let geometry = (rect, hit);
            if previous == Some(geometry) {
                stable += 1;
            } else {
                stable = 0;
            }
            previous = Some(geometry);
            if stable >= 2 {
                return;
            }
        }
        panic!("Rename target did not settle: {id}");
    }
    fn click(&mut self, id: &str) {
        self.settle_target(id);
        let at = self.visible(id).center();
        self.frame(vec![Event::PointerMoved(at)]);
        for pressed in [true, false] {
            self.visible(id);
            assert!(self.hits.iter().find(|(name, _)| name == id).unwrap().1.interact_rect.contains(at), "{id} moved away from the real pointer");
            self.frame(vec![
                Event::PointerMoved(at),
                Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
            ]);
        }
        self.idle();
    }
    fn key(&mut self, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
        }
    }
    fn select_all(&mut self) {
        self.key(Key::A, Modifiers { ctrl: true, command: true, ..Modifiers::NONE });
    }
    fn replace(&mut self, id: &str, text: &str) {
        self.click(id);
        if id == TEMPLATE {
            assert_eq!(self.ctx.memory(|memory| memory.focused()), Some(egui::Id::unique("rename-template")));
            assert!(!self.sdk.contains_key(TEMPLATE), "the shared tag-help Template boundary stays native");
        }
        self.select_all();
        self.frame(vec![Event::Text(text.into())]);
    }
    fn sdk(&self, id: &str, component: &str) {
        assert_eq!(self.sdk.get(id).map(String::as_str), Some(component), "the registered ordinary control really belongs to the SDK");
    }
    fn cache(&self) -> (u64, Rows) {
        self.ctx.data(|data| data.get_temp::<(u64, Rows)>(egui::Id::unique("rename-preview-rows"))).unwrap()
    }
    fn originals(&self, id: PhotoId) -> (&'static str, &'static str) {
        if id == self.ids[0] {
            ("IMG_0042.CR2", "CR2")
        } else {
            assert_eq!(id, self.ids[1]);
            ("IMG_0099.JPG", "JPG")
        }
    }
    fn expected(&self, stems: Option<[&str; 2]>) -> Vec<RenamePlan> {
        // This references only fixture literals and the real selected ordering, never the production expander/planner.
        self.app
            .session
            .selected()
            .into_iter()
            .enumerate()
            .map(|(i, id)| {
                let (from, ext) = self.originals(id);
                RenamePlan {
                    id: id.0,
                    from: from.into(),
                    to: stems.map_or_else(|| from.into(), |stems| format!("{}.{ext}", stems[i])),
                    from_path: None,
                    to_path: None,
                }
            })
            .collect()
    }
    fn preview(&mut self, expected: &[RenamePlan], stage: &str) -> (u64, Rows) {
        let deadline = Instant::now() + Duration::from_secs(3);
        for frame in 0..120 {
            self.frame(vec![]);
            let (key, shared) = self.cache();
            let rows = shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            if let Some(rows) = rows {
                assert_eq!(rows, expected, "the real async planner returned exact full preview rows");
                assert!(rows.iter().all(|row| row.from_path.is_none() && row.to_path.is_none()));
                if rows.iter().all(|row| self.painted.contains(&row.from) && self.painted.contains(&row.to)) {
                    println!(
                        "RENAME_PREVIEW_READY {}",
                        json!({"stage":stage,"frames":frame,"key":key,"rows":rows,"paintedTargets":rows.iter().map(|row|&row.to).collect::<Vec<_>>() })
                    );
                    return (key, shared);
                }
            }
            assert!(Instant::now() < deadline, "actual async preview/UI did not become ready: {stage}");
            std::thread::yield_now();
        }
        panic!("real preview did not paint within bounded readiness: {stage}");
    }
    fn quiet_reuses(&mut self, previous: &(u64, Rows)) {
        self.idle();
        let current = self.cache();
        assert_eq!(current.0, previous.0);
        assert!(Arc::ptr_eq(&current.1, &previous.1), "unchanged parent frames reuse the actual preview rows cache");
    }
    fn cancel(&mut self) {
        self.click("button:dialogCancel");
        assert!(self.app.ui.dialog.is_none());
        self.safety();
    }
}
impl Drop for Review {
    fn drop(&mut self) {
        crate::i18n::set_language(self.old_language);
    }
}

#[test]
fn rename_actual_sdk_preset_start_preview_and_cancel_leave_catalog_sources_and_history_unchanged() {
    let mut r = Review::new();
    let initial = r.preview(&r.expected(None), "initial-name");
    r.quiet_reuses(&initial);
    r.sdk("button:renamePreset-3", "Button");
    r.click("button:renamePreset-3");
    r.draft("Photo-{seq:3}", 5);
    let preset = r.preview(&r.expected(Some(["Photo-005", "Photo-006"])), "custom-preset");
    assert_ne!(preset.0, initial.0);
    assert!(!Arc::ptr_eq(&preset.1, &initial.1));
    r.sdk("field:renameStart", "NumberField");
    r.replace("field:renameStart", "42");
    r.key(Key::Enter, Modifiers::NONE);
    r.draft("Photo-{seq:3}", 42); // Return commits Start, never confirms Rename.
    let numbered = r.preview(&r.expected(Some(["Photo-042", "Photo-043"])), "typed-start");
    assert_ne!(numbered.0, preset.0);
    assert!(!Arc::ptr_eq(&numbered.1, &preset.1));
    r.quiet_reuses(&numbered);
    r.cancel();
}

#[test]
fn rename_actual_native_selection_tag_warning_preview_and_cancel_never_apply_a_rename() {
    let mut r = Review::new();
    r.replace(TEMPLATE, "Trip-{bogus}-{seq:3}");
    r.draft("Trip-{bogus}-{seq:3}", 5);
    r.preview(&r.expected(Some(["Trip-{bogus}-005", "Trip-{bogus}-006"])), "unknown-tag-literal");
    let warning = "Unknown tag {bogus} stays as typed (see Tags)";
    assert!(r.painted.iter().any(|text| text == warning), "the actual unknown-tag warning was painted: {:?}", r.painted);
    r.sdk("button:renameTemplateTags", "Button");
    r.click("button:renameTemplateTags");
    // Natural Ctrl/Cmd+A selection, not a stored cursor or forced focus. The real shared
    // tag helper owns its existing selection replacement and focus restoration behavior.
    r.click(TEMPLATE);
    assert_eq!(r.ctx.memory(|memory| memory.focused()), Some(egui::Id::unique("rename-template")));
    r.select_all();
    r.click("button:renameTemplateTag-0");
    r.draft("{name}", 5);
    let named = r.preview(&r.expected(None), "real-tag-selection-replacement");
    assert!(!r.painted.iter().any(|text| text == warning), "replacing the unknown template removes its real warning");
    r.quiet_reuses(&named);
    r.cancel();
}
