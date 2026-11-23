//! Real Import-parent recipe input, not direct recipe/name setters. No app logic/import worker tick.
use super::{ImportDialog, LightcraftApp, body};
use crate::headless::HeadlessView;
use crate::state::Dialog;
use egui::{Event, Key, Modifiers, PointerButton, Rect};
use lightcraft_engine::import::ImportCandidate;
use serde_json::{Value, json};
use std::path::PathBuf;

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
    dir: PathBuf,
    bytes: Vec<u8>,
    tick: u32,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    old_language: crate::i18n::Locale,
}
impl Review {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("lc-recipe-flow-{}-{}", std::process::id(), lightcraft_preview::next_tick()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = lightcraft_raster::Rgba8 { width: 24, height: 16, data: vec![[53, 81, 19, 255]; 24 * 16] };
        let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap();
        let path = dir.join("private-source.png");
        std::fs::write(&path, &bytes).unwrap();
        let candidate = ImportCandidate {
            path: path.to_string_lossy().into(),
            name: "private-source.png".into(),
            format: "PNG".into(),
            width: 24,
            height: 16,
            file_size: bytes.len() as u64,
            ..Default::default()
        };
        let mut app = LightcraftApp::new(
            lightcraft_engine::Session::new().with_fs(),
            crate::Services { local_places: Some(vec![]), png: None, ..Default::default() },
        );
        // A private scanned-input fixture, not a setter standing in for any recipe interaction.
        app.ui.dialog = Some(Dialog::Import { opts: Box::new(ImportDialog::new(vec![candidate])) });
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let old_language = crate::i18n::language();
        let mut r = Self { app, ctx, dir, bytes, tick: 0, rects: vec![], hits: vec![], old_language };
        for _ in 0..3 {
            r.frame(vec![]);
        }
        r
    }
    fn frame(&mut self, events: Vec<Event>) {
        self.safety();
        crate::widgets::take_registries(&self.ctx);
        self.tick += 1;
        let raw = HeadlessView::raw_input(egui::vec2(1600.0, 1500.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let ctx = self.ctx.clone();
        let mut final_pass = None;
        let out = Output(ctx.run_ui(raw, |_| {
            // Installed production parent/body/button row. Deliberately no app.ui/logic:
            // confirmation queues the real task, but import::tick never starts its worker.
            crate::dialogs::show(app, &ctx, false);
            final_pass = Some(crate::widgets::take_registries(&ctx));
        }));
        (self.rects, self.hits) = final_pass.unwrap();
        drop(out);
        self.safety();
    }
    fn safety(&self) {
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.smart_previews_dir.is_none() && self.app.session.media.rendered.disk().is_none());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_folder.is_none() && self.app.services.pick_files.is_none());
        assert!(self.app.scan.is_none());
        if let Some(task) = &self.app.import {
            assert!(!task.listed(), "no import worker/activity tick is allowed in this fixture");
            assert_eq!(task.status(), json!({"done": 0, "total": 1, "imported": 0, "cancelled": false}));
        }
    }
    fn files_unchanged(&self) {
        assert_eq!(std::fs::read_dir(&self.dir).unwrap().count(), 1);
        assert_eq!(std::fs::read(self.dir.join("private-source.png")).unwrap(), self.bytes);
    }
    fn draft(&self) -> &ImportDialog {
        let Some(Dialog::Import { opts }) = &self.app.ui.dialog else { panic!("actual Import review closed") };
        opts
    }
    fn rect(&self, id: &str) -> Rect {
        let found: Vec<_> = self.rects.iter().filter(|(name, _)| name == id).collect();
        assert_eq!(found.len(), 1, "one real presented locator for {id}");
        found[0].1
    }
    fn reveal(&mut self, id: &str) -> Rect {
        for _ in 0..30 {
            let rect = self.rect(id);
            let win = self.rect("dialog:window");
            // Footer buttons can sit within the window's 12-point bottom margin. The
            // body-only 20-point inset is not a visibility requirement for the real footer.
            let footer = matches!(id, "button:dialogOk" | "button:dialogCancel");
            // The body ends where the framework dialog's footer starts (its row of buttons), not 20 points above the
            // window's bottom edge.
            let body_bottom = self
                .rects
                .iter()
                .find(|(name, _)| name == "button:dialogOk")
                .map_or(win.bottom() - 20.0, |(_, ok)| (ok.top() - 8.0).min(win.bottom() - 20.0));
            let visible = if footer {
                win.contains(rect.min) && win.contains(rect.max)
            } else {
                rect.top() >= win.top() + 30.0 && rect.bottom() <= body_bottom
            };
            if visible {
                let hit = self.hits.iter().find(|(name, _)| name == id).map(|(_, hit)| hit);
                if let Some(hit) = hit {
                    assert!(hit.interact_rect.contains(rect.center()), "revealed SDK control is not clipped: {id}");
                }
                return rect;
            }
            let bottom = if footer { win.bottom() } else { body_bottom };
            let dy = if rect.bottom() > bottom { -300.0 } else { 300.0 };
            // The dialog's scroll bar, at its right edge: the framework controls fill the body's width, so the old
            // blank spot 24 points in is a control now.
            let at = egui::pos2(win.right() - 6.0, win.center().y);
            let draft = self.draft().clone();
            // NumberField intentionally owns the wheel anywhere while focused. Commit and blur it with an actual
            // press on the dialog's title, which is blank, before scrolling the window.
            self.click_at(egui::pos2(win.center().x, win.top() + 14.0));
            self.frame(vec![
                Event::PointerMoved(at),
                Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, dy),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Unknown,
                    modifiers: Modifiers::NONE,
                },
            ]);
            self.frame(vec![]);
            self.frame(vec![]);
            assert_eq!(self.draft(), &draft, "revealing {id} must scroll, not edit the previous numeric field");
        }
        panic!("natural scrolling did not reveal {id}");
    }
    fn click_at(&mut self, at: egui::Pos2) {
        for pressed in [true, false] {
            self.frame(vec![
                Event::PointerMoved(at),
                Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
            ]);
        }
        self.frame(vec![]);
    }
    fn click(&mut self, id: &str) {
        let at = self.reveal(id).center();
        self.click_at(at);
    }
    fn key_mods(&mut self, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
        }
        self.frame(vec![]);
    }
    fn key(&mut self, key: Key) {
        self.key_mods(key, Modifiers::NONE);
    }
    fn edit(&mut self, id: &str, text: &str) {
        self.click(id);
        self.key_mods(Key::A, Modifiers { ctrl: true, command: true, ..Modifiers::NONE });
        self.frame(vec![Event::Text(text.into())]);
        self.key(Key::Enter);
        assert!(self.app.ui.dialog.is_some() && self.app.import.is_none(), "field Enter must not confirm Import");
    }
    fn open(&mut self) {
        assert!(!self.rects.iter().any(|(id, _)| id == "field:importRecipeName"));
        self.click("button:importRecipeToggle");
        for id in super::RECIPE_CONTROLS {
            self.rect(&format!("slider:{id}"));
            self.rect(&format!("sliderValue:{id}"));
        }
    }
    fn document(&self) -> Value {
        let s = &self.app.session;
        json!({"photos": s.catalog.photos().map(|p| serde_json::to_value(p).unwrap()).collect::<Vec<_>>(),
            "revision": s.catalog.revision, "undo": s.undo.len(), "redo": s.redo.len(), "commits": s.commits(),
            "selected": s.selected(), "settings": self.app.ui.settings, "imports": s.import_defaults, "xmp": s.xmp})
    }
    fn presets(&self) -> Value {
        serde_json::to_value(self.app.session.presets.iter().cloned().collect::<Vec<_>>()).unwrap()
    }
    fn confirm_payload(&mut self) -> Value {
        let paths = self.draft().selected_paths();
        assert_eq!(paths, vec![self.dir.join("private-source.png").to_string_lossy().to_string()]);
        self.click("button:dialogOk");
        assert!(self.app.ui.dialog.is_none());
        let task = self.app.import.as_ref().expect("actual parent confirmation queued one task");
        let params = task.params.clone();
        let mut parsed = params.clone();
        parsed["paths"] = json!(paths);
        let request = lightcraft_engine::cmd::library::import_params(&self.app.session, &parsed).expect("actual import parser accepts queued recipe");
        let expected: Vec<(String, f64)> = params
            .get("recipe")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|item| (item["id"].as_str().unwrap().to_string(), item["value"].as_f64().unwrap()))
            .collect();
        assert_eq!(request.opts.recipe, expected);
        // Idle parent presentations cannot dispatch/replay the task; only an app logic tick may.
        for _ in 0..3 {
            self.frame(vec![]);
        }
        assert_eq!(self.app.import.as_ref().unwrap().params, params);
        self.files_unchanged();
        params
    }
}
impl Drop for Review {
    fn drop(&mut self) {
        // No import tick occurred, so the queued task owns no worker and nothing to roll back.
        self.app.import = None;
        self.app.ui.dialog = None;
        self.app.renderer.forget_imports();
        crate::i18n::set_language(self.old_language);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn recipe_review_change_reset_and_confirm_preserve_the_exact_queued_recipe() {
    let mut r = Review::new();
    let before = r.document();
    let presets = r.presets();
    let journal = r.app.session.journal.clone();
    r.open();
    let track = r.reveal("slider:light.exposure");
    r.click_at(egui::pos2(track.left() + track.width() * 0.8, track.center().y));
    assert!(r.draft().recipe.get("light.exposure").is_some_and(|v| *v > 0.0 && *v <= 5.0), "real SDK track changes the draft");
    r.edit("sliderValue:light.exposure", "1.25");
    r.edit("sliderValue:light.contrast", "17");
    assert_eq!(super::recipe_items(&r.draft().recipe), json!([{"id":"light.contrast","value":17.0},{"id":"light.exposure","value":1.25}]));
    // The SDK track's existing double-click reset, not an invented Reset control or setter.
    let at = r.reveal("slider:light.exposure").center();
    r.click_at(at);
    r.click_at(at);
    assert_eq!(super::recipe_items(&r.draft().recipe), json!([{"id":"light.contrast","value":17.0}]));
    assert_eq!(r.document(), before);
    assert_eq!(r.presets(), presets);
    assert_eq!(r.app.session.journal, journal, "draft edits never execute develop or import commands");
    assert_eq!(r.confirm_payload(), json!({"mode":"add","keywords":[],"onDeleted":"restore","recipe":[{"id":"light.contrast","value":17.0}]}));
    assert_eq!(r.document(), before);
    assert_eq!(r.presets(), presets);
    assert_eq!(r.app.session.journal, journal);
}

#[test]
fn recipe_review_return_to_default_omits_recipe_from_confirmed_import_payload() {
    let mut r = Review::new();
    let before = r.document();
    r.open();
    r.edit("sliderValue:light.exposure", "0.75");
    assert_eq!(r.draft().recipe.get("light.exposure"), Some(&0.75));
    r.edit("sliderValue:light.exposure", "0");
    assert!(r.draft().recipe.is_empty(), "default removes the entry, it must not become an explicit zero edit");
    assert_eq!(r.confirm_payload(), json!({"mode":"add","keywords":[],"onDeleted":"restore"}));
    assert_eq!(r.document(), before);
    assert!(r.app.session.journal.is_empty());
}

#[test]
fn recipe_review_save_eligibility_and_trimmed_recipe_execute_once_without_importing() {
    let mut r = Review::new();
    let count = r.app.session.presets.len();
    let journal = r.app.session.journal.len();
    let revision = r.app.session.catalog.revision;
    r.open();
    r.edit("field:importRecipeName", "  Private recipe  ");
    assert_eq!(r.draft().recipe_name, "  Private recipe  ");
    assert!(r.draft().recipe.is_empty());
    r.click("button:importRecipeSave");
    assert_eq!(r.app.session.presets.len(), count, "a typed name without recipe cannot save");
    assert_eq!(r.app.session.journal.len(), journal);
    r.edit("sliderValue:light.exposure", "0.5");
    r.edit("field:importRecipeName", "   ");
    assert_eq!(r.draft().recipe_name, "   ");
    assert_eq!(r.draft().recipe.get("light.exposure"), Some(&0.5));
    r.click("button:importRecipeSave");
    assert_eq!(r.app.session.presets.len(), count, "whitespace is not a save name");
    assert_eq!(r.app.session.journal.len(), journal);
    r.edit("field:importRecipeName", "  Private recipe  ");
    r.click("button:importRecipeSave");
    assert_eq!(r.app.session.presets.len(), count + 1);
    let saved = r.app.session.presets.iter().find(|p| p.name == "Private recipe").expect("real command saved trimmed name");
    assert!(!saved.builtin);
    assert_eq!(saved.settings, json!({"light":{"exposure":0.5}}));
    let expected = vec![("preset.saveRecipe".to_string(), json!({"name":"Private recipe","recipe":[{"id":"light.exposure","value":0.5}]}))];
    assert_eq!(r.app.session.journal.get(journal..).unwrap(), expected);
    for _ in 0..3 {
        r.frame(vec![]);
    }
    assert_eq!(r.app.session.journal.get(journal..).unwrap(), expected, "idle frames cannot replay Save");
    assert_eq!(r.draft().preset, "", "saving must not replace the review's own preset choice");
    r.click("button:dialogCancel");
    assert!(r.app.ui.dialog.is_none() && r.app.import.is_none());
    assert_eq!(r.app.session.catalog.len(), 0);
    assert_eq!(r.app.session.catalog.revision, revision);
    assert!(r.app.session.undo.is_empty() && r.app.session.redo.is_empty());
    assert_eq!(r.app.session.journal.get(journal..).unwrap(), expected);
    r.files_unchanged();
}

#[test]
fn recipe_review_cancel_after_real_edits_discards_unsaved_recipe_and_preserves_sources() {
    let mut r = Review::new();
    let before = r.document();
    let presets = r.presets();
    let journal = r.app.session.journal.clone();
    let paths = r.draft().selected_paths();
    r.open();
    r.edit("sliderValue:light.exposure", "0.5");
    r.edit("sliderValue:color.vibrance", "20");
    r.edit("field:importRecipeName", "Never saved");
    let recipe = r.draft().recipe.clone();
    assert_eq!(super::recipe_items(&recipe), json!([{"id":"color.vibrance","value":20.0},{"id":"light.exposure","value":0.5}]));
    r.click("button:importRecipeToggle");
    assert!(!r.rects.iter().any(|(id, _)| id == "field:importRecipeName"));
    r.click("button:importRecipeToggle");
    assert_eq!(r.draft().recipe, recipe);
    assert_eq!(r.draft().recipe_name, "Never saved");
    assert_eq!(r.draft().selected_paths(), paths);
    r.click("button:dialogCancel");
    assert!(r.app.ui.dialog.is_none() && r.app.import.is_none());
    for _ in 0..3 {
        r.frame(vec![]);
    }
    assert_eq!(r.document(), before);
    assert_eq!(r.presets(), presets);
    assert_eq!(r.app.session.journal, journal);
    r.files_unchanged();
}

#[test]
fn recipe_expanded_body_measurement_keeps_draft_focus_and_registry_private() {
    let mut r = Review::new();
    r.open();
    r.edit("sliderValue:light.exposure", "0.75");
    r.edit("field:importRecipeName", "Untouched");
    let before = r.draft().clone();
    let document = r.document();
    let presets = r.presets();
    let focus = r.ctx.memory(|memory| memory.focused());
    assert!(focus.is_some(), "real field input established focus naturally");
    for sizing in [false, true] {
        let mut measured = before.clone();
        crate::widgets::take_registries(&r.ctx);
        r.tick += 1;
        let raw = HeadlessView::raw_input(egui::vec2(1600.0, 1500.0), 1.0, f64::from(r.tick) / 60.0, vec![]);
        let ctx = r.ctx.clone();
        let mut facts = (false, false, false, false, false);
        let out = Output(ctx.run_ui(raw, |ui| {
            let inflight = r.app.renderer.in_flight();
            facts.0 = ui.is_enabled();
            if sizing {
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                body(&mut r.app, &mut child, &mut measured);
            } else {
                craft::ui::runtime::measure::run(&ctx, || body(&mut r.app, ui, &mut measured));
            }
            facts.1 = ui.is_enabled();
            let registries = crate::widgets::take_registries(&ctx);
            facts.2 = registries.0.is_empty() && registries.1.is_empty();
            facts.3 = r.app.renderer.in_flight() == inflight;
            facts.4 = ctx.memory(|memory| memory.focused()) == focus;
            // Retain the installed modal, not a substitute root body, after speculative layout.
            crate::dialogs::show(&mut r.app, &ctx, false);
            crate::widgets::take_registries(&ctx);
        }));
        drop(out);
        assert!(facts.0 && facts.1 && facts.2 && facts.3 && facts.4);
        assert_eq!(measured, before);
        assert_eq!(r.draft(), &before);
        assert_eq!(r.ctx.memory(|memory| memory.focused()), focus);
    }
    assert_eq!(r.document(), document);
    assert_eq!(r.presets(), presets);
    assert!(r.app.import.is_none() && r.app.session.journal.is_empty());
    r.files_unchanged();
}
