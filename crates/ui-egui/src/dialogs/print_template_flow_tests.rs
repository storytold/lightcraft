//! Actual Print parent template input, without app logic, previews of photos, services or file I/O.
use crate::LightcraftApp;
use crate::headless::HeadlessView;
use crate::state::Dialog;
use egui::{Event, Key, Modifiers, PointerButton, Rect};
use lightcraft_engine::print::layout::{Orientation, Unit};
use lightcraft_engine::print::template::{PrintSettings, PrintTemplate};
use serde_json::{Value, json};

const NAME: &str = "field:printTemplateName";

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
    old_language: crate::i18n::Locale,
    sdk_name_owner: bool,
    /// The quit prompt covers the dialog (`frame_blocked`).
    covered: bool,
}
impl Review {
    fn new() -> Self {
        // Empty memory session: the actual preview can paint paper but has no photo for
        // draw_image/request_thumb. No full app.ui/logic/renderer tick is ever mounted.
        let mut app =
            LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services { local_places: Some(vec![]), png: None, ..Default::default() });
        app.run(
            "print.set",
            json!({
                "layout":{"style":"contactSheet","grid":{"columns":3,"rows":2},
                    "page":{"paper":"a4","orientation":"landscape","margins":{"left":0.25}}},
                "guides":{"units":"mm","gridSize":0.5}
            }),
        )
        .unwrap();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut this =
            Self { app, ctx, tick: 0, rects: vec![], hits: vec![], old_language: crate::i18n::language(), sdk_name_owner: false, covered: false };
        // Production command opening, not menu-enabled/selected-photo acceptance.
        this.app.run("dialog.print", json!({})).unwrap();
        this.idle();
        this.assert_print();
        assert!(this.rect("print:page").is_positive(), "the actual empty-page preview is drawn");
        assert_eq!(this.app.ui.print.template_name, "");
        this
    }
    fn safety(&self) {
        assert_eq!(self.app.session.catalog.len(), 0, "no photo may request thumbnail work");
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.smart_previews_dir.is_none() && self.app.session.media.rendered.disk().is_none());
        assert!(self.app.services.write.is_none() && self.app.services.write_shared.is_none());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_folder.is_none() && self.app.services.pick_files.is_none());
        assert!(self.app.services.pick_print_templates.is_none() && self.app.services.save_print_template.is_none());
        assert!(self.app.services.pick_display_profile.is_none() && self.app.services.png.is_none());
        assert!(self.app.pending_picks.is_empty());
        assert!(self.app.export.is_none() && self.app.import.is_none() && self.app.scan.is_none());
        assert_eq!(self.app.renderer.in_flight(), 0);
    }
    fn frame(&mut self, events: Vec<Event>) {
        self.frame_blocked(events, false);
    }
    fn frame_blocked(&mut self, events: Vec<Event>, blocked: bool) {
        self.safety();
        crate::widgets::take_registries(&self.ctx);
        self.tick += 1;
        // While blocked, the real quit prompt covers the dialog: it appears in the first blocked frame and goes in the
        // first frame after, and each frame runs through its barrier as the app's window does.
        if blocked != self.covered {
            self.covered = blocked;
            self.app.quit_prompt = blocked.then(|| crate::QuitPrompt::Tasks("1 task is still running: Import. Quitting stops it.".into()));
        }
        let mut raw = HeadlessView::raw_input(egui::vec2(1600.0, 1200.0), 1.0, f64::from(self.tick) / 60.0, events);
        crate::panels::notices::filter_raw_input(&self.app, &mut raw);
        let app = &mut self.app;
        let ctx = self.ctx.clone();
        let mut last = None;
        let out = Output(ctx.run_ui(raw, |root| {
            // The production shell, scrolling settings, template actions, preview and footer.
            let (_, recorded) = craft::ui::runtime::inspect::record(root, egui::Id::unique("print-template-name-owner"), |_| {
                crate::dialogs::show_under_prompt(app, &ctx, |app, blocks| crate::dialogs::show(app, &ctx, blocks));
            });
            last = Some((crate::widgets::take_registries(&ctx), recorded));
        }));
        let ((rects, hits), recorded) = last.unwrap();
        self.sdk_name_owner = hits.iter().find(|(name, _)| name == NAME).is_some_and(|(_, hit)| {
            recorded.iter().flat_map(|record| record.elements.iter()).any(|element| element.id == hit.id && element.component == "TextField")
        });
        (self.rects, self.hits) = (rects, hits);
        drop(out);
        self.safety();
    }
    fn idle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![]);
        }
    }
    fn assert_print(&self) {
        assert!(matches!(self.app.ui.dialog, Some(Dialog::Print)), "actual Print remains open");
    }
    fn rect(&self, id: &str) -> Rect {
        let found: Vec<_> = self.rects.iter().filter(|(name, _)| name == id).collect();
        assert_eq!(found.len(), 1, "one current presentation of {id}");
        found[0].1
    }
    fn visible(&self, id: &str) -> Rect {
        let rect = self.rect(id);
        assert!(self.rect("dialog:window").contains_rect(rect), "{id} must be within its actual parent");
        assert!(self.ctx.content_rect().contains_rect(rect), "{id} must fit the real viewport");
        let hit = self.hits.iter().find(|(name, _)| name == id).expect("registered target has a current hit");
        assert!(hit.1.interact_rect.contains(rect.center()), "{id} is clipped, not a click target");
        rect
    }
    fn reveal_settings(&mut self, id: &str) {
        for step in 0..12 {
            let rect = self.rect(id);
            let hit = self.hits.iter().find(|(name, _)| name == id).expect("current settings hit").1;
            if self.rect("dialog:window").contains_rect(rect)
                && self.ctx.content_rect().contains_rect(rect)
                && hit.interact_rect.contains(rect.center())
            {
                // Visibility is not readiness: residual smooth scroll can move this control
                // outside a cached pointer position between move, press and release.
                self.settle_settings_target(id);
                // Reacquire after actual geometry readiness; pointer() still enforces visible().
                let rect = self.visible(id);
                let hit = self.hits.iter().find(|(name, _)| name == id).expect("settled settings hit").1;
                println!(
                    "PRINT_TEMPLATE_SETTINGS_SCROLL {}",
                    json!({"target":id,"step":step,"visible":true,
                    "rect":format!("{:?}",rect),"hit":format!("{:?}",hit.interact_rect)})
                );
                return;
            }
            // The real left ScrollArea is SETTINGS_W wide; its content is SETTINGS_W-14.
            // Name's x remains that content's left edge even while scrolled out of view.
            // The preview's page center is inside the shared body height, not the footer.
            let left = self.rect(NAME).left();
            // (over the column's own scroll bar, the 14 points beside the content: a framework control the focus is
            // in takes the wheel, so the wheel must not land on one)
            let at = egui::pos2(left + super::SETTINGS_W - 7.0, self.rect("print:page").center().y);
            assert!(at.x > left + super::SETTINGS_W - 14.0 && at.x < left + super::SETTINGS_W);
            assert!(self.rect("dialog:window").contains(at) && self.ctx.content_rect().contains(at));
            let delta = if rect.center().y > at.y { -160.0 } else { 160.0 };
            let before = self.persisted();
            let name = self.app.ui.print.template_name.clone();
            self.frame(vec![
                Event::PointerMoved(at),
                Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, delta),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Unknown,
                    modifiers: Modifiers::NONE,
                },
            ]);
            self.idle();
            self.assert_print();
            assert_eq!(self.persisted(), before, "revealing {id} must scroll the pane, not modify any setting or template");
            assert_eq!(self.app.ui.print.template_name, name, "scrolling is not a Name edit");
            println!(
                "PRINT_TEMPLATE_SETTINGS_SCROLL {}",
                json!({"target":id,"step":step,"visible":false,
                "wheelAt":[at.x,at.y],"delta":delta,"before":format!("{:?}",rect),"after":format!("{:?}",self.rect(id))})
            );
        }
        panic!("real Print settings scrolling did not reveal {id}");
    }
    fn settle_settings_target(&mut self, id: &str) {
        let before = self.persisted();
        let name = self.app.ui.print.template_name.clone();
        let mut previous = None;
        let mut stable = 0;
        // This is an observed-geometry readiness guard, not a fixed extra idle delay.
        // It is the same criterion proven by the isolated immediate/ready diagnostic.
        for step in 0..32 {
            self.frame(vec![]);
            let rect = self.visible(id);
            let hit = self.hits.iter().find(|(name, _)| name == id).expect("current settings hit").1.interact_rect;
            let geometry = (rect, hit);
            stable = if previous == Some(geometry) { stable + 1 } else { 0 };
            previous = Some(geometry);
            self.assert_print();
            assert_eq!(self.persisted(), before, "geometry readiness cannot edit Print settings, templates or history");
            assert_eq!(self.app.ui.print.template_name, name);
            println!(
                "PRINT_TEMPLATE_SETTINGS_READY {}",
                json!({"target":id,"step":step,"stable":stable,
                "rect":format!("{:?}",rect),"hit":format!("{:?}",hit)})
            );
            if stable >= 2 {
                return;
            }
        }
        panic!("visible Print settings target {id} did not reach geometry readiness");
    }
    fn pointer(&mut self, id: &str, button: PointerButton) {
        let pos = self.visible(id).center();
        self.frame(vec![Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            self.frame(vec![Event::PointerMoved(pos), Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE }]);
        }
        self.idle();
    }
    fn click(&mut self, id: &str) {
        self.pointer(id, PointerButton::Primary);
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
    fn replace(&mut self, id: &str, text: &str) {
        self.click(id);
        if id == NAME {
            assert!(self.sdk_name_owner, "registered Name belongs to the actually mounted SDK TextField");
            assert!(self.ctx.memory(|m| m.focused()).is_some(), "real pointer input focused the Name text adapter");
        }
        self.key_mods(Key::A, Modifiers { ctrl: true, command: true, ..Modifiers::NONE });
        if text.is_empty() {
            self.key(Key::Backspace);
        } else {
            self.frame(vec![Event::Text(text.into())]);
        }
    }
    fn number(&mut self, id: &str, text: &str) {
        self.replace(id, text);
        self.key(Key::Enter);
        self.assert_print();
    }
    fn no_document_changes(&self) -> Value {
        let s = &self.app.session;
        let mut albums = s.catalog.albums().collect::<Vec<_>>();
        albums.sort_by_key(|album| album.id.0);
        json!({"photos":s.catalog.photos().collect::<Vec<_>>(),"albums":albums,"revision":s.catalog.revision,
            "undo":s.undo.len(),"redo":s.redo.len(),"commits":s.commits(),"selected":s.selected(),
            "settings":self.app.ui.settings,"imports":s.import_defaults,"xmp":s.xmp,"lastExport":s.last_export,
            "presets":s.presets.iter().cloned().collect::<Vec<_>>()})
    }
    fn persisted(&self) -> Value {
        json!({"document":self.no_document_changes(),"print":self.app.session.print,"templates":self.app.session.print_templates,
            "journal":self.app.session.journal,"toast":self.app.ui.toast.as_ref().map(|t|t.0.as_str()),"pendingPicks":self.app.pending_picks.len()})
    }
    fn popup_open(&self) -> bool {
        // (the Print dialog is a framework layer itself: a popup is one above it)
        egui::Popup::is_any_open(&self.ctx) || crate::dialogs::popup_open(&self.ctx)
    }
    fn observation(&self, scenario: &str, stage: &str) {
        println!(
            "PRINT_TEMPLATE_SDK_ESCAPE {}",
            json!({"scenario":scenario,"stage":stage,
            "dialogAfter":self.app.ui.dialog,"templateName":self.app.ui.print.template_name,
            "focus":format!("{:?}",self.ctx.memory(|m|m.focused())),"nativePopup":egui::Popup::is_any_open(&self.ctx),"popupOpen":self.popup_open(),"sdkNameOwner":self.sdk_name_owner})
        );
    }
}
impl Drop for Review {
    fn drop(&mut self) {
        self.app.ui.dialog = None;
        crate::i18n::set_language(self.old_language);
    }
}

#[test]
fn print_templates_actual_typed_save_apply_delete_preserve_exact_settings_and_current_guides() {
    let mut r = Review::new();
    let document = r.no_document_changes();
    // Real engine setup creates an unrelated existing template. The workflow below still
    // saves/applies/deletes its target only through actual production buttons.
    r.app.run("print.template.save", json!({"name":"Unrelated existing"})).unwrap();
    r.idle();
    let unrelated = r.app.session.print_templates.clone();
    assert_eq!(unrelated.len(), 1);
    let saved = r.app.session.print.clone();
    assert_saved_look(&saved);
    let mut expected_template = serde_json::to_value(&saved).unwrap();
    expected_template.as_object_mut().unwrap().remove("guides");
    let before = r.persisted();
    save_typed_template(&mut r, &unrelated, &saved, &expected_template, &before, &document);
    edit_grid_and_page(&mut r);
    let mut restored = saved.clone();
    restored.guides = r.app.session.print.guides;
    apply_template(&mut r, &restored);
    delete_target_template(&mut r, &unrelated, &restored, &document);
}

/// The active print look as the dialog opens it: A4 paper, landscape, a 3 × 2 grid, no cells.
fn assert_saved_look(saved: &PrintSettings) {
    assert_eq!(saved.layout.grid.columns, 3);
    assert_eq!(saved.layout.grid.rows, 2);
    assert_eq!(saved.layout.page.paper, "a4");
    assert_eq!(saved.layout.page.orientation, Orientation::Landscape);
    assert!(saved.layout.cells.is_empty());
}

/// The typed Name, Return and Save: the name survives Return, Save stores the look rather than the
/// layout change, and neither is journalled.
fn save_typed_template(r: &mut Review, unrelated: &[PrintTemplate], saved: &PrintSettings, expected: &Value, before: &Value, document: &Value) {
    r.replace(NAME, "   Typed proof sheet   ");
    assert_eq!(r.app.ui.print.template_name, "   Typed proof sheet   ");
    r.key(Key::Enter);
    r.assert_print();
    assert_eq!(&r.persisted(), before, "typing/Return does not save or start printing");
    assert_eq!(r.app.ui.print.template_name, "   Typed proof sheet   ", "the exact real Name survives Return until Save");
    assert_eq!(r.app.session.print_templates, unrelated);
    // Print template commands are intentionally non-journaled queries. An empty journal
    // is not a missed click and is not a raw-call payload observer.
    let journal = r.app.session.journal.clone();
    r.click("button:printSaveTemplate");
    assert_eq!(r.app.session.journal, journal, "Save is a non-journaled query");
    assert_eq!(r.app.session.print_templates.len(), unrelated.len() + 1, "one new target template");
    assert_eq!(&r.app.session.print_templates[..unrelated.len()], unrelated);
    assert_eq!(r.app.session.print_templates[1].name, "Typed proof sheet");
    assert_eq!(r.app.session.print_templates[1].settings, *expected);
    assert_eq!(r.app.session.print, *saved, "Save captures settings, not a layout change");
    assert_eq!(&r.no_document_changes(), document);
}

/// The grid and page edits, through the real dialog's number field and orientation buttons.
fn edit_grid_and_page(r: &mut Review) {
    r.number("drag:printColumns", "7");
    r.number("drag:printRows", "4");
    // Do the already-planned real units action first: it naturally blurs the committed
    // NumberField before wheel input (a focused NumberField legitimately owns the wheel).
    r.click("button:printUnits-3");
    r.reveal_settings("button:printOrientation-0");
    r.click("button:printOrientation-0");
    assert_eq!(r.app.session.print.layout.grid.columns, 7);
    assert_eq!(r.app.session.print.layout.grid.rows, 4);
    assert_eq!(r.app.session.print.layout.page.orientation, Orientation::Portrait);
    assert_eq!(r.app.session.print.guides.units, Unit::Points);
    assert_eq!(r.app.session.print.layout.page.paper, "a4", "other settings retain the selected paper");
}

/// Apply restores the template's exact look and keeps the guides in use.
fn apply_template(r: &mut Review, restored: &PrintSettings) {
    let journal = r.app.session.journal.clone();
    let templates = r.app.session.print_templates.clone();
    r.reveal_settings("button:printTemplateApply-1");
    r.click("button:printTemplateApply-1");
    assert_eq!(r.app.session.journal, journal, "Apply is a non-journaled query");
    assert_eq!(r.app.session.print_templates, templates, "Apply leaves both stored templates untouched");
    assert_eq!(r.app.session.print, *restored, "Apply restores exact saved look but keeps current guides");
    assert_eq!(r.app.ui.print.page, 0);
    assert_eq!(r.app.ui.print.selected, None);
}

/// Delete removes only the target template: the unrelated one, the active look and the document stay.
fn delete_target_template(r: &mut Review, unrelated: &[PrintTemplate], restored: &PrintSettings, document: &Value) {
    let journal = r.app.session.journal.clone();
    r.click("button:printTemplateDelete-1");
    assert_eq!(r.app.session.journal, journal, "Delete is a non-journaled query");
    assert_eq!(r.app.session.print_templates, unrelated, "Delete removes only the target, not the unrelated existing template");
    assert_eq!(r.app.session.print, *restored, "Delete does not edit the active look");
    assert_eq!(&r.no_document_changes(), document);
    r.assert_print();
    r.safety();
}

#[test]
fn print_templates_actual_empty_and_whitespace_return_and_save_do_not_dispatch_or_write() {
    for name in ["", "   "] {
        let mut r = Review::new();
        let before = r.persisted();
        r.replace(NAME, name);
        assert_eq!(r.app.ui.print.template_name, name);
        r.key(Key::Enter);
        r.assert_print();
        assert_eq!(r.persisted(), before, "Return in blank Name is not Save or Print to PDF");
        r.click("button:printSaveTemplate");
        r.assert_print();
        assert_eq!(r.persisted(), before, "blank Save cannot change the real template list, settings, journal or errors");
        assert!(r.app.session.print_templates.is_empty());
    }
}

#[test]
fn print_templates_actual_cancel_keeps_live_settings_but_never_saves_the_typed_name() {
    let mut r = Review::new();
    let document = r.no_document_changes();
    let initial = r.app.session.print.clone();
    r.number("drag:printColumns", "5");
    assert_eq!(r.app.session.print.layout.grid.columns, 5);
    assert_eq!(r.app.session.print.layout.grid.rows, initial.layout.grid.rows);
    let live = r.app.session.print.clone();
    r.replace(NAME, "Never saved");
    assert_eq!(r.app.ui.print.template_name, "Never saved");
    let before = r.persisted();
    r.click("button:dialogCancel");
    assert!(r.app.ui.dialog.is_none());
    assert_eq!(r.persisted(), before, "Cancel only dismisses; it is not rollback or Save/Print");
    assert_eq!(r.app.session.print, live, "Print settings were live print.set, not a staged dialog draft");
    assert!(r.app.session.print_templates.is_empty());
    assert_eq!(r.no_document_changes(), document);
}

#[test]
fn print_templates_sdk_name_single_escape_popup_and_covered_outcomes_without_writes() {
    for scenario in ["focused-name", "name-popup", "covered-first-pass", "covered-delayed"] {
        let mut r = Review::new();
        let before = r.persisted();
        r.replace(NAME, "Only a draft");
        assert_eq!(r.app.ui.print.template_name, "Only a draft");
        assert!(r.sdk_name_owner);
        assert!(r.ctx.memory(|m| m.focused()).is_some());
        if scenario == "name-popup" {
            r.pointer(NAME, PointerButton::Secondary);
            assert!(r.popup_open(), "the actual SDK Name context menu opened");
        }
        if scenario == "covered-delayed" {
            r.frame_blocked(vec![], true);
            r.observation(scenario, "covered-before-key");
            assert_eq!(r.app.ui.print.template_name, "Only a draft", "covered presentation retains the exact draft");
            r.assert_print();
            assert_eq!(r.persisted(), before);
        }
        r.observation(scenario, "before-key");
        let blocked = scenario.starts_with("covered");
        for pressed in [true, false] {
            r.frame_blocked(
                vec![Event::Key { key: Key::Escape, physical_key: Some(Key::Escape), pressed, repeat: false, modifiers: Modifiers::NONE }],
                blocked,
            );
            r.observation(scenario, if pressed { "after-press" } else { "after-release" });
            assert_eq!(r.persisted(), before, "{scenario}: Escape cannot write settings, templates, catalog/history or Print output");
            if blocked {
                r.assert_print();
                assert_eq!(r.app.ui.print.template_name, "Only a draft", "{scenario}: even the first covered Escape must retain the exact draft");
            }
            if scenario == "focused-name" {
                assert!(r.app.ui.dialog.is_none(), "one uncovered physical Escape closes actual Print without a second key");
            }
        }
        for _ in 0..3 {
            r.frame_blocked(vec![], blocked);
        }
        r.observation(scenario, "after-quiet");
        assert_eq!(r.persisted(), before);
        if blocked {
            r.assert_print();
            assert_eq!(r.app.ui.print.template_name, "Only a draft");
            r.frame(vec![]); // Release the covering presentation without any replayed key.
            r.observation(scenario, "after-uncover");
            r.assert_print();
            assert_eq!(r.app.ui.print.template_name, "Only a draft", "retention survives uncovering");
            assert_eq!(r.persisted(), before);
        }
        if scenario == "name-popup" {
            r.assert_print();
            assert!(!r.popup_open());
            assert_eq!(r.app.ui.print.template_name, "Only a draft", "popup Escape does not cancel the Name edit");
        }
        // First-covered retention deliberately improves the observed native clearing.
        // The covering prompt itself is not rendered: this is the production frozen-parent flag.
    }
}
