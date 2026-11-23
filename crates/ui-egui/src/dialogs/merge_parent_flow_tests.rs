//! Panorama parent routing only. No poll, merge/render worker, confirm, pixels or file output.
use super::MergeDialog;
use crate::LightcraftApp;
use crate::headless::HeadlessView;
use crate::state::Dialog;
use egui::{Event, Key, Modifiers, PointerButton, Rect};
use lightcraft_catalog::{Op, Photo, Source};
use serde_json::{Value, json};

const FIELD: &str = "sliderField:merge.boundaryWarp";
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
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    number_owned: bool,
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
        for (id, name) in [(ids[0], "IMG_0042.JPG"), (ids[1], "IMG_0099.JPG")] {
            let photo = Photo::new(id, Source::Demo { scene: 0 }, name, "JPEG", 6000, 4000, "2026-01-14T05:58:48");
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
            Self { app, ctx, tick: 0, rects: vec![], hits: vec![], number_owned: false, painted: vec![], unchanged: Value::Null, old_language };
        this.unchanged = this.document();
        this.open();
        this
    }
    fn document(&self) -> Value {
        let s = &self.app.session;
        json!({"catalog":s.catalog,"undo":format!("{:?}",s.undo),"redo":format!("{:?}",s.redo),
            "commits":s.commits(),"journal":s.journal,"selection":format!("{:?}",s.selection),"selected":s.selected(),
            "lastMerge":s.last_merge,"settings":self.app.ui.settings,"imports":s.import_defaults,"xmp":s.xmp,"lastExport":s.last_export,
            "presets":s.presets.iter().cloned().collect::<Vec<_>>(),"print":s.print,"printTemplates":s.print_templates,
            "toast":self.app.ui.toast.as_ref().map(|toast|toast.0.as_str()),"pendingPicks":self.app.pending_picks.len()})
    }
    fn safety(&self) {
        let s = &self.app.session;
        assert_eq!(s.catalog.len(), 2);
        assert!(s.catalog.photos().all(|photo| matches!(photo.source, Source::Demo { .. })));
        assert!(s.library.is_none() && s.library_folder.is_none());
        assert!(s.smart_previews_dir.is_none() && s.media.rendered.disk().is_none());
        assert!(self.app.services.write.is_none() && self.app.services.write_shared.is_none());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_files.is_none() && self.app.services.pick_folder.is_none());
        assert!(self.app.services.png.is_none() && self.app.pending_picks.is_empty());
        assert!(self.app.import.is_none() && self.app.export.is_none() && self.app.scan.is_none());
        assert_eq!(self.app.renderer.in_flight(), 0);
        let m = &self.app.merge;
        assert!(m.preview_task.is_none() && m.final_task.is_none());
        assert!(m.preview.is_none() && m.preview_pixels.is_none());
        assert!(m.error.is_none() && m.failed_options.is_none() && m.zoom.is_none());
        assert!(m.last_result.is_none() && m.batch.is_empty());
        assert!(s.activity.list().is_empty());
        assert_eq!(self.document(), self.unchanged, "draft/Escape/reopen/Cancel cannot write catalog/history/journal/last options");
    }
    fn texts(shape: &egui::epaint::Shape, clip: Rect, out: &mut Vec<String>) {
        match shape {
            egui::epaint::Shape::Vec(shapes) => {
                for shape in shapes {
                    Self::texts(shape, clip, out);
                }
            }
            egui::epaint::Shape::Text(text) if clip.contains_rect(text.galley.rect.translate(text.pos.to_vec2())) => {
                out.push(text.galley.job.text.clone())
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
            let (_, recorded) = craft::ui::runtime::inspect::record(root, egui::Id::unique("merge-parent-number-owner"), |_| {
                // The real dialog only; poll/full app frames would start a preview and are forbidden.
                crate::dialogs::show(app, &ctx, false);
            });
            last = Some((crate::widgets::take_registries(&ctx), recorded));
        }));
        let ((rects, hits), recorded) = last.unwrap();
        self.number_owned = hits.iter().find(|(name, _)| name == FIELD).is_some_and(|(_, hit)| {
            recorded
                .iter()
                .flat_map(|record| record.elements.iter())
                .any(|element| element.id == hit.id && element.part.is_none() && element.component == "NumberField")
        });
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
    fn open(&mut self) {
        self.safety();
        let opened = self.app.run("dialog.mergePanorama", json!({})).unwrap();
        assert_eq!(opened, json!({"photos":2}));
        assert_eq!(self.app.merge.ids, self.app.session.selected());
        self.idle();
        self.warp(0.0);
        assert!(self.painted.iter().any(|text| text == "0"));
    }
    fn warp(&self, value: f64) {
        let expected = MergeDialog { boundary_warp: value, ..MergeDialog::for_command("merge.panorama") };
        assert_eq!(self.app.ui.dialog, Some(Dialog::Merge { opts: expected }));
    }
    fn rect(&self, id: &str) -> Rect {
        let rows: Vec<_> = self.rects.iter().filter(|(name, _)| name == id).collect();
        assert_eq!(rows.len(), 1, "one actual mounted {id}");
        rows[0].1
    }
    fn visible(&self, id: &str) -> Rect {
        let rect = self.rect(id);
        let hit = self.hits.iter().find(|(name, _)| name == id).unwrap().1;
        assert!(self.rect("dialog:window").contains_rect(rect) && self.ctx.content_rect().contains_rect(rect));
        assert!(hit.interact_rect.contains(rect.center()), "{id} is clipped");
        rect
    }
    fn settle(&mut self, id: &str) {
        let mut previous = None;
        let mut stable = 0;
        for _ in 0..32 {
            self.frame(vec![]);
            let geometry = (self.visible(id), self.hits.iter().find(|(name, _)| name == id).unwrap().1.interact_rect);
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
        panic!("real Merge target did not settle: {id}");
    }
    fn click(&mut self, id: &str) {
        self.settle(id);
        let at = self.visible(id).center();
        self.frame(vec![Event::PointerMoved(at)]);
        for pressed in [true, false] {
            self.visible(id);
            assert!(self.hits.iter().find(|(name, _)| name == id).unwrap().1.interact_rect.contains(at));
            self.frame(vec![
                Event::PointerMoved(at),
                Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
            ]);
        }
        self.idle();
    }
    fn focused_number(&self) {
        assert!(self.number_owned, "locator belongs to the real mounted SDK NumberField");
        let field = self.hits.iter().find(|(name, _)| name == FIELD).unwrap().1.id;
        let focus = craft::ui::runtime::focus::focused_rec(&self.ctx).unwrap();
        assert!(focus.stop.text);
        assert_eq!(focus.stop.id, field, "actual pointer focused this numeric component, not some other editor");
        assert_eq!(self.ctx.memory(|memory| memory.focused()), Some(focus.stop.root));
    }
    fn key(&mut self, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
        }
    }
    fn replace(&mut self, text: &str) {
        self.click(FIELD);
        self.focused_number();
        self.key(Key::A, Modifiers { ctrl: true, command: true, ..Modifiers::NONE });
        self.frame(vec![Event::Text(text.into())]);
        self.idle();
        self.focused_number();
        assert!(self.painted.iter().any(|shown| shown == text), "the real focused numeric draft was painted: {:?}", self.painted);
    }
    fn observe(&self, stage: &str) {
        println!(
            "MERGE_PARENT_ESCAPE {}",
            json!({"stage":stage,"dialogAfter":self.app.ui.dialog,
            "numberOwned":self.number_owned,"focus":format!("{:?}",craft::ui::runtime::focus::focused_rec(&self.ctx).map(|focus|(focus.stop.id,focus.stop.part,focus.stop.text))),
            "escapeTaken":crate::widgets::escape_taken(&self.ctx),"lastOptions":self.app.session.last_merge,
            "previewTask":self.app.merge.preview_task.is_some(),"finalTask":self.app.merge.final_task.is_some(),
            "preview":self.app.merge.preview.is_some(),"pixels":self.app.merge.preview_pixels.is_some()})
        );
    }
    fn escape(&mut self, prefix: &str, stay_open: bool) {
        for (stage, pressed) in [("press", true), ("release", false)] {
            self.frame(vec![Event::Key { key: Key::Escape, physical_key: Some(Key::Escape), pressed, repeat: false, modifiers: Modifiers::NONE }]);
            self.observe(&format!("{prefix}-{stage}"));
            if stay_open {
                self.warp(42.0);
            } else {
                assert!(self.app.ui.dialog.is_none(), "clean Escape dismisses the real parent");
            }
        }
        self.idle();
        self.observe(&format!("{prefix}-quiet"));
        if stay_open {
            self.warp(42.0);
        } else {
            assert!(self.app.ui.dialog.is_none());
        }
    }
}
impl Drop for Review {
    fn drop(&mut self) {
        crate::i18n::set_language(self.old_language);
    }
}

#[test]
fn panorama_actual_parent_dirty_escape_restores_committed_warp_then_clean_escape_reopen_cancel_never_save() {
    let mut r = Review::new();
    r.replace("42");
    r.key(Key::Enter, Modifiers::NONE);
    r.idle();
    r.warp(42.0);
    r.observe("committed-42");
    r.replace("73");
    r.observe("dirty-73-before-escape");
    r.escape("dirty-escape", true);
    r.escape("clean-escape", false);
    r.open(); // unchanged last options, not the discarded draft, governs the real opener.
    r.observe("reopened-default-0");
    r.click("button:dialogCancel");
    r.observe("after-actual-cancel");
    assert!(r.app.ui.dialog.is_none());
    r.safety();
}
