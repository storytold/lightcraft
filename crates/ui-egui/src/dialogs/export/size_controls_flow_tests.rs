//! Actual installed Export Size input. No file IO, writers, export/preview workers or app logic loop.
use crate::LightcraftApp;
use crate::headless::HeadlessView;
use crate::state::Dialog;
use egui::{Event, Key, Modifiers, PointerButton, Rect};
use lightcraft_engine::export::{ExportOptions, Resize, ResizeMode as R};
use serde_json::{Value, json};

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
}
impl Review {
    fn new() -> Self {
        let app = LightcraftApp::new(
            lightcraft_engine::Session::with_demo(),
            crate::Services { local_places: Some(vec![]), png: None, ..Default::default() },
        );
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let old_language = crate::i18n::language();
        let mut r = Self { app, ctx, tick: 0, rects: vec![], hits: vec![], old_language };
        // Use the actual existing opening command, never seed a resize/full-size draft.
        r.app.run("dialog.export", json!({})).unwrap();
        for _ in 0..3 {
            r.frame(vec![]);
        }
        assert!(!r.size().0);
        assert_eq!(r.size().1, Resize::default());
        r
    }
    fn safety(&self) {
        assert!(self.app.services.write.is_none() && self.app.services.write_shared.is_none(), "no writer service may be installed");
        assert!(self.app.services.picker.is_none() && self.app.services.pick_folder.is_none() && self.app.services.pick_files.is_none());
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.smart_previews_dir.is_none() && self.app.session.media.rendered.disk().is_none());
        assert!(self.app.export.is_none() && self.app.import.is_none() && self.app.scan.is_none());
        assert_eq!(self.app.renderer.in_flight(), 0, "the size-only parent must not start previews");
        if let Some(Dialog::Export { opts, .. }) = &self.app.ui.dialog {
            assert!(opts.watermark.is_none(), "do not enter the custom watermark-preview worker route");
        }
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
            // Production parent, content and footer; no app.ui or logic tick.
            crate::dialogs::show(app, &ctx, false);
            final_pass = Some(crate::widgets::take_registries(&ctx));
        }));
        (self.rects, self.hits) = final_pass.unwrap();
        drop(out);
        self.safety();
    }
    fn draft(&self) -> &Dialog {
        self.app.ui.dialog.as_ref().expect("actual Export review remains open")
    }
    fn size(&self) -> (bool, Resize, u16) {
        let Dialog::Export { full_size, resize, opts, .. } = self.draft() else { panic!("not Export") };
        (*full_size, *resize, opts.ppi)
    }
    fn params(&self) -> Value {
        let Dialog::Export { opts, full_size, resize, limit_kb, .. } = self.draft() else { panic!("not Export") };
        let params = super::export_dialog_params(opts, *full_size, resize, *limit_kb);
        let parsed = ExportOptions::from_params(&params).expect("real export options parser accepts the size draft");
        assert_eq!(parsed.resize, (!*full_size).then_some(*resize));
        assert_eq!(parsed.ppi, opts.ppi);
        params
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
            let footer = matches!(id, "button:dialogOk" | "button:dialogCancel");
            let visible = if footer {
                win.contains(rect.min) && win.contains(rect.max)
            } else {
                rect.top() >= win.top() + 30.0 && rect.bottom() <= win.bottom() - 20.0
            };
            if visible {
                if let Some((_, hit)) = self.hits.iter().find(|(name, _)| name == id) {
                    assert!(hit.interact_rect.contains(rect.center()), "real control is clipped: {id}");
                }
                return rect;
            }
            let bottom = win.bottom() - if footer { 0.0 } else { 20.0 };
            let dy = if rect.bottom() > bottom { -300.0 } else { 300.0 };
            let at = egui::pos2(win.right() - 24.0, win.center().y);
            let draft = self.draft().clone();
            // A focused NumberField owns wheel anywhere. Real blank-background input
            // blurs it before window scrolling; never fabricate or force a focus change.
            self.click_at(at);
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
            assert_eq!(self.draft(), &draft, "revealing {id} must scroll, not change the size draft");
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
        if text.is_empty() {
            self.key(Key::Backspace);
        } else {
            self.frame(vec![Event::Text(text.into())]);
        }
        self.key(Key::Enter);
        assert!(matches!(self.app.ui.dialog, Some(Dialog::Export { .. })), "numeric/text Enter is not Export confirmation");
        assert!(self.app.ui.toast.is_none(), "field Enter must not even attempt the no-writer export");
    }
    fn choose(&mut self, mode: R) {
        let index = match mode {
            R::LongEdge => 0,
            R::ShortEdge => 1,
            R::Width => 2,
            R::Height => 3,
            R::WidthHeight => 4,
            R::Dimensions => 5,
            R::Megapixels => 6,
            R::Percent => 7,
        };
        self.click("combo:exportResizeMode");
        self.key(Key::Home);
        for _ in 0..index {
            self.key(Key::ArrowDown);
        }
        self.key(Key::Enter);
        assert_eq!(self.size().1.mode, mode, "the real compact SDK Select picked the intended mode");
        // (the dialog is a framework layer itself: no popup stays above it)
        assert!(!crate::dialogs::popup_open(&self.ctx));
        assert!(self.app.ui.toast.is_none(), "dropdown Enter must not attempt Export");
    }
    fn unchanged_document(&self) -> Value {
        let s = &self.app.session;
        json!({"photos":s.catalog.photos().map(|p|serde_json::to_value(p).unwrap()).collect::<Vec<_>>(),
            "revision":s.catalog.revision,"undo":s.undo.len(),"redo":s.redo.len(),"commits":s.commits(),
            "selected":s.selected(),"settings":self.app.ui.settings,"imports":s.import_defaults,"xmp":s.xmp,
            "lastExport":s.last_export,"presets":s.presets.iter().cloned().collect::<Vec<_>>(),"journal":s.journal})
    }
}
impl Drop for Review {
    fn drop(&mut self) {
        self.app.ui.dialog = None;
        crate::i18n::set_language(self.old_language);
    }
}

#[test]
fn export_size_missing_mode_transitions_keep_defaults_precision_and_dimensions_height() {
    let mut r = Review::new();
    let before = r.unchanged_document();
    r.choose(R::ShortEdge);
    assert_eq!(r.size().1.value, 2048.0);
    r.edit("sliderValue:export.sizePx", "1777");
    r.choose(R::Width);
    assert_eq!(r.size().1.value, 1777.0, "pixel-to-pixel mode keeps the entered size");
    r.edit("sliderValue:export.sizePx", "1235");
    r.choose(R::Height);
    assert_eq!(r.size().1.value, 1235.0);
    r.edit("sliderValue:export.sizePx", "777");
    r.choose(R::Dimensions);
    assert_eq!((r.size().1.value, r.size().1.height), (777.0, 2048));
    r.edit("sliderValue:export.sizeW", "1537");
    r.edit("sliderValue:export.sizeH", "919");
    assert_eq!(r.params()["resize"], json!({"mode":"dimensions","value":1537.0,"height":919,"dontEnlarge":true}));
    r.choose(R::Megapixels);
    assert_eq!((r.size().1.value, r.size().1.height), (12.0, 919));
    r.edit("sliderValue:export.sizeMp", "12.5");
    assert_eq!(r.params()["resize"], json!({"mode":"megapixels","value":12.5,"height":919,"dontEnlarge":true}));
    r.choose(R::Percent);
    assert_eq!((r.size().1.value, r.size().1.height), (50.0, 919));
    r.edit("sliderValue:export.sizePercent", "75");
    assert_eq!(r.params()["resize"], json!({"mode":"percent","value":75.0,"height":919,"dontEnlarge":true}));
    r.choose(R::ShortEdge);
    assert_eq!(
        (r.size().1.value, r.size().1.height),
        (2048.0, 919),
        "leaving percentage returns to the existing pixel default without losing height"
    );
    assert_eq!(r.params()["resize"], json!({"mode":"shortEdge","value":2048.0,"height":919,"dontEnlarge":true}));
    assert_eq!(r.unchanged_document(), before);
    r.click("button:dialogCancel");
    assert!(r.app.ui.dialog.is_none());
    assert_eq!(r.unchanged_document(), before);
}

#[test]
fn export_size_full_size_roundtrip_retains_resize_and_dont_enlarge_while_ppi_stays_live() {
    let mut r = Review::new();
    let before = r.unchanged_document();
    r.choose(R::Dimensions);
    r.edit("sliderValue:export.sizeW", "1499");
    r.edit("sliderValue:export.sizeH", "1001");
    assert!(r.size().1.dont_enlarge);
    r.click("check:exportDontEnlarge");
    assert!(!r.size().1.dont_enlarge);
    r.edit("sliderValue:export.ppi", "300");
    let retained = r.size().1;
    r.click("check:exportFullSize");
    assert_eq!(r.size(), (true, retained, 300));
    for id in [
        "combo:exportResizeMode",
        "check:exportDontEnlarge",
        "slider:export.sizePx",
        "sliderValue:export.sizePx",
        "slider:export.sizeW",
        "sliderValue:export.sizeW",
        "slider:export.sizeH",
        "sliderValue:export.sizeH",
        "slider:export.sizeMp",
        "sliderValue:export.sizeMp",
        "slider:export.sizePercent",
        "sliderValue:export.sizePercent",
    ] {
        assert!(!r.rects.iter().any(|(name, _)| name == id), "full-size hidden control must not publish {id}");
        assert!(!r.hits.iter().any(|(name, _)| name == id));
    }
    r.rect("sliderValue:export.ppi");
    r.edit("sliderValue:export.ppi", "360");
    let full = r.params();
    assert!(full.get("resize").is_none() && full.get("dontEnlarge").is_none());
    assert_eq!(full["longEdge"], json!(0), "production serializer uses its actual explicit full-size sentinel");
    assert_eq!(full["ppi"], json!(360));
    r.click("check:exportFullSize");
    assert_eq!(r.size(), (false, retained, 360));
    assert_eq!(r.params()["resize"], json!({"mode":"dimensions","value":1499.0,"height":1001,"dontEnlarge":false}));
    r.click("check:exportDontEnlarge");
    assert!(r.size().1.dont_enlarge);
    assert_eq!(r.params()["dontEnlarge"], json!(true));
    assert_eq!(r.unchanged_document(), before);
    r.click("button:dialogCancel");
    assert!(r.app.ui.dialog.is_none());
    assert_eq!(r.unchanged_document(), before);
}

#[test]
fn export_size_cancel_and_actual_no_writer_confirmation_preserve_the_size_draft_without_work() {
    for confirm in [false, true] {
        let mut r = Review::new();
        let before = r.unchanged_document();
        r.choose(R::Percent);
        r.edit("sliderValue:export.sizePercent", "73");
        r.edit("sliderValue:export.ppi", "301");
        r.click("check:exportDontEnlarge");
        // Destination is typed through its actual field, not a draft setter or host picker.
        r.edit("field:exportFolder", "/lc-size-no-writer-fixture");
        let draft = r.draft().clone();
        let params = r.params();
        assert_eq!(params["resize"], json!({"mode":"percent","value":73.0,"height":2048,"dontEnlarge":false}));
        assert_eq!(params["ppi"], json!(301));
        if confirm {
            // Real confirmation dispatches app.export, which refuses before rendering/batch
            // creation because both writer services are absent. This is not a successful export.
            r.click("button:dialogOk");
            assert_eq!(r.app.ui.toast.as_ref().map(|toast| toast.0.as_str()), Some("no writer"));
            assert_eq!(r.draft(), &draft, "refusal keeps every correctable draft field");
            assert_eq!(r.params(), params, "real serialization remains unchanged after refusal");
            assert_eq!(r.unchanged_document(), before);
            // Keep the same real dialog usable after refusal, without repeating confirmation.
            r.click("button:dialogCancel");
        } else {
            r.click("button:dialogCancel");
        }
        assert!(r.app.ui.dialog.is_none());
        for _ in 0..3 {
            r.frame(vec![]);
        }
        assert_eq!(r.unchanged_document(), before);
        assert!(r.app.session.last_export.is_none());
        r.safety();
    }
}
