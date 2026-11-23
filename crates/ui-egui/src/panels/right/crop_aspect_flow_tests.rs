//! Actual aspect-picker leaf and full Crop-panel pointer coverage. No canvas, renderer,
//! engine geometry rewrite, filesystem, forced focus, or installed-window acceptance.
use super::*;
use crate::headless::HeadlessView;
use egui::{Event, Key, Modifiers, PointerButton, Rect};
use lightcraft_catalog::{Op, Photo, Source};
use lightcraft_engine::{Selection, Session};
use serde_json::Value;

const OPEN: &str = "dropdown:cropAspect";
const WIDTH: &str = "field:cropCustomW";
const HEIGHT: &str = "field:cropCustomH";
const APPLY: &str = "button:cropCustomApply";
#[derive(Clone, Copy, Debug)]
enum Presentation {
    AspectLeaf,
    CropPanel,
}
#[derive(Clone, Copy)]
enum Pass {
    Normal,
    Disabled,
    Measure,
    Sizing,
}
struct Drain(egui::FullOutput);
impl Drop for Drain {
    fn drop(&mut self) {
        self.0.textures_delta.clear();
        self.0.platform_output.commands.clear();
        self.0.viewport_output.clear();
        self.0.shapes.clear();
    }
}
struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    ids: [PhotoId; 2],
    owner: PhotoId,
    presentation: Presentation,
    pass: Pass,
    tick: u32,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    labels: Vec<String>,
    trace: std::collections::VecDeque<String>,
}
impl Fixture {
    fn new(presentation: Presentation) -> Self {
        let mut s = Session::new();
        let ids = [s.catalog.alloc_photo_id(), s.catalog.alloc_photo_id()];
        for (id, name, w, h) in [(ids[0], "Landscape A.jpg", 600, 400), (ids[1], "Other B.jpg", 800, 600)] {
            s.catalog
                .apply(Op::AddPhoto { photo: Box::new(Photo::new(id, Source::Demo { scene: 0 }, name, "JPEG", w, h, "2026-11-21T00:00:00")) })
                .unwrap();
            s.selection = Selection::single(id);
            let mut d = (*s.develop_of(id).unwrap()).clone();
            d.light.exposure = if id == ids[0] { 0.25 } else { -0.5 };
            s.set_develop(id, d, "Fixture exposure").unwrap();
        }
        s.selection = Selection::single(ids[0]);
        assert_eq!(s.undo.len(), 2);
        let mut app = LightcraftApp::new(s, crate::Services { local_places: Some(vec![]), png: None, ..Default::default() });
        app.ui.tool = "crop".into(); // Actual crop-tool presentation, no canvas action.
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut f = Self {
            app,
            ctx,
            ids,
            owner: ids[0],
            presentation,
            pass: Pass::Normal,
            tick: 0,
            rects: vec![],
            hits: vec![],
            labels: vec![],
            trace: std::collections::VecDeque::new(),
        };
        for _ in 0..3 {
            f.frame(vec![]);
        }
        f
    }
    fn memory_only(&self) {
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.smart_previews_dir.is_none() && self.app.session.media.rendered.disk().is_none());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_files.is_none());
        assert_eq!(self.app.session.catalog.photos().count(), 2);
    }
    fn frame(&mut self, mut events: Vec<Event>) {
        self.memory_only();
        self.tick += 1;
        let input_trace = format!("{events:?}");
        crate::widgets::take_registries(&self.ctx);
        let mods = events
            .iter()
            .find_map(|event| match event {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        events.insert(0, Event::ModifiersChanged(mods));
        let raw = HeadlessView::raw_input(egui::vec2(1100.0, 3000.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let owner = self.owner;
        let presentation = self.presentation;
        let pass = self.pass;
        let mut final_pass = None;
        let output = Drain(self.ctx.run_ui(raw, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.set_width(320.0);
                let mut draw = |ui: &mut egui::Ui| match presentation {
                    Presentation::CropPanel => crop(app, ui, owner),
                    Presentation::AspectLeaf => {
                        let p = app.session.catalog.photo(owner).unwrap();
                        let original = p.width as f64 / p.height as f64;
                        ui.horizontal(|ui| {
                            ui.label(crate::i18n::tr("Aspect Ratio"));
                            aspect_picker(app, ui, owner, Some(original));
                        });
                    }
                };
                match pass {
                    Pass::Normal => draw(ui),
                    Pass::Disabled => {
                        ui.add_enabled_ui(false, |ui| draw(ui));
                    }
                    Pass::Measure => craft::ui::runtime::measure::run(&ui.ctx().clone(), || draw(ui)),
                    Pass::Sizing => {
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()).sizing_pass().invisible());
                        draw(&mut child);
                    }
                }
            });
            // Publication follows the real app: one registry per pass, never accumulated
            // across a discarded layout pass. Actual same-pass duplicates still fail.
            let registry = crate::widgets::take_registries(ui.ctx());
            for name in [OPEN, WIDTH, HEIGHT, APPLY] {
                let rects = registry.0.iter().filter(|(id, _)| id == name).count();
                let hits = registry.1.iter().filter(|(id, _)| id == name).count();
                assert!(rects <= 1, "duplicate {name} in one pass");
                assert_eq!(hits, rects, "one actual owner for every locator");
            }
            final_pass = Some(registry);
        }));
        fn text(shape: &egui::Shape, labels: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => labels.push(t.galley.job.text.clone()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        text(shape, labels);
                    }
                }
                _ => {}
            }
        }
        self.labels.clear();
        for shape in &output.0.shapes {
            text(&shape.shape, &mut self.labels);
        }
        (self.rects, self.hits) = final_pass.unwrap();
        let face = self.hits.iter().find(|(name, _)| name == OPEN).map(|(_, hit)| hit.id);
        let binding = face.and_then(|id| self.ctx.data(|d| d.get_temp::<(u64, bool)>(id.with("menu-open"))));
        let draft = self.ctx.data(|d| d.get_temp::<(String, String)>(egui::Id::unique(("crop-custom-aspect", owner.0))));
        let popup = craft::ui::runtime::overlay::top(&self.ctx);
        let focus = craft::ui::runtime::focus::focused_rec(&self.ctx);
        let controls: Vec<_> = [OPEN, WIDTH, HEIGHT, APPLY]
            .into_iter()
            .map(|name| {
                let hit = self.hits.iter().find(|(key, _)| key == name).map(|(_, hit)| *hit);
                let response = hit.and_then(|hit| self.ctx.read_response(hit.id)).map(|r| {
                    (
                        r.id,
                        r.rect,
                        r.interact_rect,
                        r.enabled(),
                        r.hovered(),
                        r.contains_pointer(),
                        r.is_pointer_button_down_on(),
                        self.ctx.layer_id_at(r.rect.center()),
                        r.layer_id,
                    )
                });
                (name, response)
            })
            .collect();
        self.trace.push_back(format!("input={input_trace} binding={binding:?} draft={draft:?} native_focus={:?} sdk_focus={focus:?} top={popup:?} controls={controls:?} journal={:?}",
            self.ctx.memory(|m| m.focused()), self.app.session.journal));
        while self.trace.len() > 16 {
            self.trace.pop_front();
        }
    }
    fn has(&self, id: &str) -> bool {
        self.rects.iter().any(|(name, _)| name == id)
    }
    fn rect(&self, id: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing {id} in {:?}: {:?}", self.presentation, self.rects)).1
    }
    fn hit(&self, id: &str) -> crate::widgets::WidgetHit {
        self.hits.iter().find(|(name, _)| name == id).unwrap().1
    }
    fn pointer(&mut self, at: egui::Pos2, pressed: bool) {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
        ]);
    }
    fn click(&mut self, id: &str) {
        self.tick += 30;
        let at = self.rect(id).center();
        self.frame(vec![Event::PointerMoved(at)]);
        self.pointer(at, true);
        self.pointer(at, false);
        self.frame(vec![]);
    }
    fn key_mod(&mut self, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
        }
        self.frame(vec![]);
    }
    fn key(&mut self, key: Key) {
        self.key_mod(key, Modifiers::NONE);
    }
    fn tab_to(&mut self, id: &str) {
        self.frame(vec![Event::PointerGone]);
        for _ in 0..48 {
            let owner = self.hit(id).id;
            if craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|r| r.stop.id == owner) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab failed to reach {id}");
    }
    fn menu_open(&self) -> bool {
        let owner = self.hit(OPEN).id;
        self.ctx.data(|d| d.get_temp::<(u64, bool)>(owner.with("menu-open"))).expect("production popup binding").1
    }
    fn assert_menu_closed(&self) {
        assert!(!self.menu_open(), "the actual production popup binding is closed");
        assert!(
            !craft::ui::runtime::overlay::any_open(&self.ctx, craft::ui::runtime::overlay::OpenFilter::Any),
            "no live SDK overlay remains; exit painting is not an active popup"
        );
        // The SDK may still paint exit rows. None may retain the live keyboard stop.
        if let Some(focused) = craft::ui::runtime::focus::focused_rec(&self.ctx) {
            assert!(
                self.hits
                    .iter()
                    .filter(|(name, _)| name == WIDTH || name == HEIGHT || name == APPLY || name.starts_with("button:cropAspect-"))
                    .all(|(_, hit)| hit.id != focused.stop.id),
                "closed popup cannot retain a live child focus owner; trace={:#?}",
                self.trace
            );
        }
    }
    fn arrow_to_preset(&mut self, id: &str) {
        self.frame(vec![Event::PointerGone]);
        for _ in 0..=ASPECT_PRESETS.len() {
            assert!(self.menu_open(), "keyboard navigation must stay inside the active menu");
            let owner = self.hit(id).id;
            if craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|r| r.stop.id == owner) {
                return;
            }
            self.key(Key::ArrowDown);
        }
        panic!("natural menu arrows failed to reach {id}; trace={:#?}", self.trace);
    }
    fn open(&mut self) {
        self.click(OPEN);
        assert!(self.menu_open(), "actual production popup opens, not merely its exit-painted rows");
        assert!(craft::ui::runtime::overlay::any_open(&self.ctx, craft::ui::runtime::overlay::OpenFilter::Any));
        assert!(self.has(WIDTH) && self.has(HEIGHT) && self.has(APPLY));
        let hit = self.hit(OPEN);
        let response = self.ctx.read_response(hit.id).unwrap();
        assert_eq!(response.id, hit.id);
        assert!(response.sense.senses_click() && response.sense.is_focusable(), "actual SDK opener, not a probe");
    }
    fn replace(&mut self, id: &str, value: &str) {
        self.click(id);
        self.key_mod(Key::A, Modifiers::COMMAND);
        if value.is_empty() {
            self.key(Key::Backspace);
        } else {
            self.frame(vec![Event::Text(value.to_owned())]);
        }
        self.frame(vec![]);
    }
    fn custom(&mut self, width: &str, height: &str) {
        self.replace(WIDTH, width);
        self.replace(HEIGHT, height);
    }
    fn aspect(&self) -> Option<(u32, u32)> {
        self.app.session.develop_of(self.ids[0]).unwrap().crop.aspect
    }
    fn other_photo(&self) -> Value {
        serde_json::to_value(self.app.session.catalog.photo(self.ids[1]).unwrap()).unwrap()
    }
    fn snapshot(&self) -> Value {
        json!({"photos": self.app.session.catalog.photos().collect::<Vec<_>>(),
            "selection": self.app.session.selection, "revision": self.app.session.catalog.revision,
            "undo": format!("{:?}", self.app.session.undo), "redo": format!("{:?}", self.app.session.redo),
            "journal": self.app.session.journal, "commits": self.app.session.commits(),
            "remembered": self.app.ui.crop_default_aspect})
    }
    fn assert_command(&self, from: usize, payload: Value) {
        assert_eq!(self.app.session.journal.get(from..).unwrap(), &[("crop.aspect".into(), payload)]);
    }
    fn only_crop_changed(&self, before: &lightcraft_develop::DevelopSettings) {
        let after = self.app.session.develop_of(self.ids[0]).unwrap();
        let mut expected = before.clone();
        expected.crop = after.crop;
        assert_eq!(*after, expected);
    }
    fn preset(&mut self, value: &str) {
        self.open();
        self.click(&format!("button:cropAspect-{value}"));
        self.assert_menu_closed();
    }
    fn idle_is_inert(&mut self) {
        let before = self.snapshot();
        for _ in 0..3 {
            self.frame(vec![]);
        }
        assert_eq!(self.snapshot(), before);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.memory_only();
            let raw = HeadlessView::raw_input(egui::vec2(1100.0, 3000.0), 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            drop(Drain(self.ctx.run_ui(raw, |_| {})));
            crate::widgets::take_registries(&self.ctx);
        }
    }
}

#[test]
fn crop_aspect_pointer_presets_free_original_and_custom_change_only_the_same_photo_with_one_undo_each() {
    for presentation in [Presentation::AspectLeaf, Presentation::CropPanel] {
        let mut f = Fixture::new(presentation);
        let other = f.other_photo();
        let owner = f.app.session.selection.clone();
        assert!(f.labels.iter().any(|s| s == crate::i18n::tr("Original")));
        for (value, expected) in [("1x1", Some((100, 100))), ("free", None), ("original", Some((60000, 40000)))] {
            let before = f.app.session.develop_of(f.ids[0]).unwrap();
            let j = f.app.session.journal.len();
            let undo = f.app.session.undo.len();
            let history = f.app.session.catalog.photo(f.ids[0]).unwrap().history.len();
            f.preset(value);
            f.assert_command(j, json!({"aspect":value}));
            assert_eq!(f.aspect(), expected);
            assert_eq!(f.app.session.undo.len(), undo + 1);
            assert_eq!(f.app.session.undo.last().unwrap().label, "Crop Aspect");
            assert_eq!(f.app.session.catalog.photo(f.ids[0]).unwrap().history.len(), history + 1);
            f.only_crop_changed(&before);
            assert_eq!(f.other_photo(), other);
            assert_eq!(f.app.session.selection, owner);
            f.idle_is_inert();
        }
        let before = f.app.session.develop_of(f.ids[0]).unwrap();
        let undo = f.app.session.undo.len();
        let j = f.app.session.journal.len();
        f.open();
        f.custom("3,5", "2");
        assert_eq!(f.app.session.journal.len(), j, "typing and blur never apply a crop");
        f.click(APPLY);
        f.assert_command(j, json!({"aspect":[3.5,2.0]}));
        assert_eq!(f.aspect(), Some((350, 200)));
        assert_eq!(f.app.session.undo.len(), undo + 1);
        f.only_crop_changed(&before);
        assert_eq!(f.other_photo(), other);
        f.app.run("edit.undo", json!({})).unwrap();
        f.frame(vec![]);
        assert_eq!(f.app.session.develop_of(f.ids[0]).unwrap(), before, "actual Undo restores the previous crop");
        assert_eq!(f.app.session.undo.len(), undo);
        assert_eq!(f.other_photo(), other);
        f.app.run("edit.redo", json!({})).unwrap();
        f.frame(vec![]);
        assert_eq!(f.aspect(), Some((350, 200)));
        let j = f.app.session.journal.len();
        f.preset("1x1");
        f.assert_command(j, json!({"aspect":"1x1"}));
        assert_eq!(f.aspect(), Some((100, 100)));
        assert_eq!(f.other_photo(), other);
        f.idle_is_inert();
    }
}
#[test]
fn crop_aspect_all_named_presets_keep_their_exact_command_values() {
    let mut f = Fixture::new(Presentation::AspectLeaf);
    let other = f.other_photo();
    for (_, value) in ASPECT_PRESETS {
        let j = f.app.session.journal.len();
        f.preset(value);
        f.assert_command(j, json!({"aspect":value}));
        assert_eq!(f.other_photo(), other);
    }
}
#[test]
fn crop_aspect_natural_keyboard_open_custom_enter_apply_and_return_to_preset_use_real_owners() {
    let mut f = Fixture::new(Presentation::AspectLeaf);
    let other = f.other_photo();
    let j = f.app.session.journal.len();
    f.tab_to(OPEN);
    f.key(Key::Enter);
    assert!(f.has(APPLY));
    f.tab_to(WIDTH);
    f.key_mod(Key::A, Modifiers::COMMAND);
    f.frame(vec![Event::Text("2.5".into())]);
    f.tab_to(HEIGHT);
    f.key_mod(Key::A, Modifiers::COMMAND);
    f.frame(vec![Event::Text("3".into())]);
    assert_eq!(f.app.session.journal.len(), j, "Tab only leaves a draft field; it never applies");
    f.key(Key::Enter);
    f.assert_command(j, json!({"aspect":[2.5,3.0]}));
    assert_eq!(f.aspect(), Some((250, 300)), "custom dimensions keep the given portrait orientation");
    f.tab_to(OPEN);
    f.key(Key::Space);
    f.arrow_to_preset("button:cropAspect-16x9");
    let j = f.app.session.journal.len();
    f.key(Key::Enter);
    f.assert_command(j, json!({"aspect":"16x9"}));
    assert_eq!(f.aspect(), Some((900, 1600)), "named preset retains current portrait orientation");
    assert_eq!(f.other_photo(), other);
    f.idle_is_inert();
}
#[test]
fn crop_aspect_invalid_custom_values_are_inert_and_keep_the_popup_open() {
    for (w, h) in [("0", "2"), ("-1", "2"), ("text", "2"), ("NaN", "2"), ("inf", "2"), ("1001", "1000"), ("21", "1"), ("1", "21"), ("", "2")] {
        let mut f = Fixture::new(Presentation::AspectLeaf);
        f.open();
        let before = f.snapshot();
        f.custom(w, h);
        f.click(APPLY);
        assert_eq!(f.snapshot(), before, "invalid custom {w} × {h} must never dispatch");
        assert!(f.menu_open() && f.has(WIDTH) && f.has(APPLY), "invalid input {w:?} × {h:?}; trace={:#?}", f.trace);
        f.idle_is_inert();
    }
}
#[test]
fn crop_aspect_custom_inclusive_limits_preserve_parser_and_engine_refusal_boundary() {
    for (w, h, x, y, aspect) in
        [("1000", "50", 1000.0, 50.0, (100000, 5000)), ("50", "1000", 50.0, 1000.0, (5000, 100000)), (" 0,05 ", "1", 0.05, 1.0, (5, 100))]
    {
        let mut f = Fixture::new(Presentation::AspectLeaf);
        f.open();
        let j = f.app.session.journal.len();
        f.custom(w, h);
        f.click(APPLY);
        f.assert_command(j, json!({"aspect":[x,y]}));
        assert_eq!(f.aspect(), Some(aspect));
    }
    let mut f = Fixture::new(Presentation::AspectLeaf);
    f.open();
    let before = f.snapshot();
    f.custom("0.001", "0.001"); // Existing UI parser accepts, but engine hundredths cannot represent it.
    f.click(APPLY);
    assert_eq!(f.snapshot(), before);
    assert!(f.app.ui.status.contains("aspect numbers are out of range"));
    f.assert_menu_closed(); // Retain close-after-dispatch behavior, including engine refusal.
}
#[test]
fn crop_aspect_escape_and_outside_dismissal_never_edit_and_reopening_keeps_the_owner() {
    for outside in [false, true] {
        let mut f = Fixture::new(Presentation::AspectLeaf);
        f.open();
        let before = f.snapshot();
        let field_owner = f.hit(WIDTH).id;
        f.custom("7", "4");
        if outside {
            f.pointer(egui::pos2(1000.0, 1000.0), true);
            f.pointer(egui::pos2(1000.0, 1000.0), false);
        } else {
            f.key(Key::Escape); // Cancels the current text edit first.
            if f.menu_open() {
                f.key(Key::Escape);
            }
        }
        f.frame(vec![]);
        let draft_key = egui::Id::unique(("crop-custom-aspect", f.owner.0));
        let draft_before = f.ctx.data(|d| d.get_temp::<(String, String)>(draft_key));
        // A historical text-root record must not make a closed field receive real typing.
        f.frame(vec![Event::Text("closed popup input".into())]);
        assert_eq!(f.ctx.data(|d| d.get_temp::<(String, String)>(draft_key)), draft_before, "closed fields cannot take text; trace={:#?}", f.trace);
        f.assert_menu_closed();
        assert_eq!(f.snapshot(), before);
        f.open();
        assert_eq!(f.hit(WIDTH).id, field_owner);
        assert_eq!(f.snapshot(), before);
        f.idle_is_inert();
    }
}
#[test]
fn crop_aspect_stale_menu_cannot_apply_to_another_active_photo_or_an_absent_owner() {
    for active in [Some(1usize), None] {
        let mut f = Fixture::new(Presentation::AspectLeaf);
        f.open();
        f.custom("5", "4");
        let at = f.rect(APPLY).center();
        f.frame(vec![Event::PointerMoved(at)]);
        f.pointer(at, true);
        let ids: Vec<_> = active.into_iter().map(|i| f.ids[i].0).collect();
        f.app.run("library.select", json!({"ids":ids})).unwrap();
        let before = f.snapshot();
        f.pointer(at, false); // Render input still names the original photo; active selection changed.
        assert_eq!(f.snapshot(), before);
        f.assert_menu_closed();
        // An exit-painted Apply is not an active target. A fresh real click at its old
        // position must also be inert, not just the release of the earlier live press.
        f.pointer(at, true);
        f.pointer(at, false);
        f.frame(vec![]);
        assert_eq!(f.snapshot(), before);
        f.assert_menu_closed();
        f.click(OPEN); // Disabled owner cannot open a new stale popup either.
        assert_eq!(f.snapshot(), before);
        f.assert_menu_closed();
    }
}
#[test]
fn crop_aspect_disabled_sizing_and_measurement_cannot_open_commit_or_replay() {
    for pass in [Pass::Disabled, Pass::Sizing, Pass::Measure] {
        for popup in [false, true] {
            let mut f = Fixture::new(Presentation::AspectLeaf);
            if popup {
                f.open();
                f.custom("5", "4");
            }
            let before = f.snapshot();
            let target = if popup { APPLY } else { OPEN };
            let at = f.rect(target).center();
            f.frame(vec![Event::PointerMoved(at)]);
            f.pointer(at, true);
            f.pass = pass;
            f.pointer(at, false);
            f.key(Key::Enter);
            f.key(Key::Space);
            assert_eq!(f.snapshot(), before);
            f.pass = Pass::Normal;
            f.frame(vec![]);
            assert_eq!(f.snapshot(), before);
            f.idle_is_inert();
        }
    }
}
