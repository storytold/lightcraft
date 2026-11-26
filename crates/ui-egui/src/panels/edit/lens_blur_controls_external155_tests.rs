//! Source-only preparation. Run under separately admitted parent QA; never draw the full app or Find Subject.
use super::*;
use crate::headless::HeadlessView;
use crate::widgets::{WidgetHit, take_registries};
use egui::{Event, Key, Modifiers, PointerButton, Ui};
use lightcraft_catalog::{Op, Photo, PhotoId, Source};

const APPLY: &str = "check:lensBlur.enabled";
const FOCUS: &str = "dropdown:lensBlurFocus";
const SUBJECT: &str = "lensBlurFocus:subject";
const FIRST: &str = "lensBlurFocus:mask:17";
const SECOND: &str = "lensBlurFocus:mask:93";
const NOW: &str = "2026-11-20T00:00:00";

#[derive(Clone, Copy)]
enum Adapter {
    Previous,
    Candidate,
}
#[derive(Clone, Copy)]
enum Pass {
    Real,
    Measure,
    Sizing,
}

fn fixture(enabled: bool, amount: f64) -> (LightcraftApp, PhotoId) {
    let mut s = lightcraft_engine::Session::new();
    s.clock = Box::new(|| NOW.to_owned());
    let id = PhotoId(1);
    let photo = Photo::new(id, Source::Demo { scene: 0 }, "lens-controls.jpg", "JPEG", 2, 2, NOW);
    s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    s.execute("library.select", &json!({"ids": [id.0], "active": id.0})).unwrap();
    let masks = [17, 93].map(|id| lightcraft_develop::Mask { id, name: "Duplicate name".into(), ..Default::default() });
    s.execute("develop.merge", &json!({"settings": {"masks": masks, "lens_blur": {"enabled": enabled, "amount": amount}}})).unwrap();
    s.journal.clear();
    (LightcraftApp::new(s, Default::default()), id)
}

/// Calls only the migrated production helpers, in the panel's original margins and enabled scope.
fn controls(app: &mut LightcraftApp, ui: &mut Ui, adapter: Adapter) {
    let d = app.session.develop_of(app.session.active().unwrap()).unwrap();
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 4, bottom: 4 }).show(ui, |ui| match adapter {
        Adapter::Candidate => super::lens_blur_apply(app, ui, &d.lens_blur),
        Adapter::Previous => previous_apply(app, ui, &d),
    });
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 6, bottom: 4 }).show(ui, |ui| {
        ui.add_enabled_ui(d.lens_blur.enabled, |ui| match adapter {
            Adapter::Candidate => super::lens_blur_focus(app, ui, &d),
            Adapter::Previous => previous_focus(app, ui, &d),
        });
    });
}

/// A popup is open: the previous native candidate's egui popup, or the framework menu that replaced it (a layer of
/// the framework's that takes keys).
fn popup_open(ctx: &egui::Context) -> bool {
    egui::Popup::is_any_open(ctx) || craft::ui::runtime::overlay::any_open(ctx, craft::ui::runtime::overlay::OpenFilter::Keys)
}

struct Frames {
    app: LightcraftApp,
    photo: PhotoId,
    view: HeadlessView,
    adapter: Adapter,
    whole_panel: bool,
    time: f64,
    scale: f32,
    enabled: bool,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, WidgetHit)>,
}

impl Frames {
    fn new(adapter: Adapter, enabled: bool, amount: f64, scale: f32) -> Self {
        let (app, photo) = fixture(enabled, amount);
        let view = HeadlessView::new();
        app.install_framework(&view.ctx);
        let mut f = Self { app, photo, view, adapter, whole_panel: false, time: 0.0, scale, enabled: true, rects: vec![], hits: vec![] };
        f.settle();
        f
    }

    fn frame(&mut self, events: Vec<Event>, pass: Pass) {
        self.frame_checked(events, pass, |_, _| {});
    }
    fn frame_checked(&mut self, events: Vec<Event>, pass: Pass, mut after: impl FnMut(&Ui, &LightcraftApp)) {
        let _ = take_registries(&self.view.ctx);
        self.time += 0.04;
        let height = if self.whole_panel { 1250.0 } else { 360.0 };
        let raw = HeadlessView::raw_input(vec2(600.0, height), self.scale, self.time, events);
        self.view.run(raw, |ui| {
            ui.set_width(320.0);
            let mut draw = |ui: &mut Ui| {
                if self.whole_panel {
                    let d = self.app.session.develop_of(self.photo).unwrap();
                    super::lens_blur(&mut self.app, ui, &d);
                } else {
                    controls(&mut self.app, ui, self.adapter);
                }
                after(ui, &self.app);
            };
            ui.add_enabled_ui(self.enabled, |ui| match pass {
                Pass::Real => draw(ui),
                Pass::Measure => {
                    let ctx = ui.ctx().clone();
                    craft::ui::runtime::measure::run(&ctx, || draw(ui));
                }
                Pass::Sizing => {
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                    draw(&mut child);
                }
            });
        });
        (self.rects, self.hits) = take_registries(&self.view.ctx);
    }

    fn settle(&mut self) {
        for _ in 0..6 {
            self.frame(vec![], Pass::Real);
        }
    }
    fn rect(&self, id: &str) -> Rect {
        let found: Vec<_> = self.rects.iter().filter(|(name, _)| name == id).collect();
        assert_eq!(found.len(), 1, "unique actual locator {id}: {:?}", self.rects);
        found[0].1
    }
    fn response(&self, id: &str) -> egui::Response {
        let hits: Vec<_> = self.hits.iter().filter(|(name, _)| name == id).collect();
        assert_eq!(hits.len(), 1, "one response for {id}");
        let hit = hits[0].1;
        let r = self.view.ctx.read_response(hit.id).unwrap();
        assert_eq!(r.rect, self.rect(id));
        assert_eq!(hit.interact_rect, r.interact_rect);
        assert_eq!(hit.layer_id, r.layer_id);
        assert!(r.sense.senses_click() && r.sense.is_focusable(), "no hover-only probe");
        r
    }
    fn pointer(&mut self, pos: Pos2, pressed: bool, pass: Pass) {
        self.frame(
            vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }],
            pass,
        );
    }
    fn click_at(&mut self, pos: Pos2) {
        self.frame(vec![Event::PointerMoved(pos)], Pass::Real);
        self.pointer(pos, true, Pass::Real);
        self.pointer(pos, false, Pass::Real);
        self.settle();
    }
    fn click(&mut self, id: &str) {
        self.click_at(self.rect(id).center());
    }
    fn key(&mut self, key: Key) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE }], Pass::Real);
        }
        self.settle();
    }
    fn open(&mut self) {
        self.click(FOCUS);
        assert!(popup_open(&self.view.ctx), "real unforced popup");
        for id in [SUBJECT, FIRST, SECOND] {
            self.rect(id);
        }
    }
    fn settings(&self) -> DevelopSettings {
        (*self.app.session.develop_of(self.photo).unwrap()).clone()
    }
    fn merge(&mut self, settings: serde_json::Value) {
        self.app.run("develop.merge", json!({"settings": settings})).unwrap();
        self.settle();
    }
    fn snapshot(&self) -> serde_json::Value {
        json!({"settings": self.settings(), "undo": self.app.session.undo.len(), "redo": self.app.session.redo.len(),
            "history": self.app.session.catalog.photo(self.photo).unwrap().history,
            "journal": self.app.session.journal})
    }
    fn semantic_focus(&self) -> String {
        let focus = self.view.ctx.memory(|m| m.focused());
        for id in [SUBJECT, FIRST, SECOND] {
            // The previous oracle records native responses without changing their interaction.
            let target = match self.adapter {
                Adapter::Candidate => id.to_owned(),
                Adapter::Previous => format!("previous:{id}"),
            };
            if self.hits.iter().any(|(name, hit)| name == &target && Some(hit.id) == focus) {
                return id.to_owned();
            }
        }
        if focus.is_none() { "none".into() } else { "other".into() }
    }
}

#[test]
fn lens_blur_controls_external155_apply_first_enable_retains_amount_and_one_undo() {
    for scale in [1.0, 1.5, 2.0] {
        let mut f = Frames::new(Adapter::Candidate, false, 0.0, scale);
        f.response(APPLY);
        let before = f.settings();
        let undo = f.app.session.undo.len();
        let history = f.app.session.catalog.photo(f.photo).unwrap().history.len();
        let pos = f.rect(APPLY).center();
        f.frame(vec![Event::PointerMoved(pos)], Pass::Real);
        f.pointer(pos, true, Pass::Real);
        assert_eq!(f.settings(), before, "press does not dispatch");
        f.pointer(pos, false, Pass::Real);
        f.settle();
        let mut expected = before.clone();
        expected.lens_blur.enabled = true;
        expected.lens_blur.amount = 50.0;
        assert_eq!(f.settings(), expected);
        assert_eq!(f.app.session.undo.len(), undo + 1);
        assert_eq!(f.app.session.undo.last().unwrap().label, "Lens Blur");
        assert_eq!(f.app.session.catalog.photo(f.photo).unwrap().history.len(), history + 1);
        f.app.run("edit.undo", json!({})).unwrap();
        assert_eq!(f.settings(), before, "one Undo restores amount and enable together");
        f.merge(json!({"lens_blur": {"enabled": true, "amount": 31.0}}));
        f.click(APPLY);
        assert!(!f.settings().lens_blur.enabled);
        assert_eq!(f.settings().lens_blur.amount, 31.0);
        f.click(APPLY);
        assert!(f.settings().lens_blur.enabled);
        assert_eq!(f.settings().lens_blur.amount, 31.0, "re-enable retains nonzero amount");
    }
}

#[test]
fn lens_blur_controls_external155_duplicate_names_use_exact_mask_ids_and_subject_resets_auto() {
    let mut f = Frames::new(Adapter::Candidate, true, 31.0, 1.0);
    f.merge(json!({"lens_blur": {"shape": "wide"}}));
    for (target, mask) in [(FIRST, 17), (SECOND, 93)] {
        let before = f.settings();
        let undo = f.app.session.undo.len();
        f.open();
        assert_ne!(f.response(FIRST).id, f.response(SECOND).id, "duplicate display names cannot alias");
        f.response(SUBJECT);
        f.click(target);
        let mut expected = before.clone();
        expected.lens_blur.focus_mask = Some(mask);
        assert_eq!(f.settings(), expected, "row retains the exact u32 mask id");
        assert!(!popup_open(&f.view.ctx));
        assert_eq!(f.app.session.undo.len(), undo + 1);
        assert_eq!(f.app.session.undo.last().unwrap().label, "Lens Blur Focus");
        f.app.run("edit.undo", json!({})).unwrap();
        assert_eq!(f.settings(), before);
        f.settle();
    }
    f.open();
    let first_id = f.response(FIRST).id;
    let second_id = f.response(SECOND).id;
    f.key(Key::Escape);
    let mut masks = f.settings().masks;
    masks.reverse();
    masks[0].name = "Renamed mask".into();
    f.merge(json!({"masks": masks, "lens_blur": {"focus_mask": 93}}));
    f.open();
    assert_eq!(f.response(FIRST).id, first_id, "row identity survives reorder");
    assert_eq!(f.response(SECOND).id, second_id, "row identity survives rename");
    let before = f.settings();
    let undo = f.app.session.undo.len();
    f.click(SUBJECT);
    let mut expected = before.clone();
    expected.lens_blur.focus_mask = None;
    expected.lens_blur.shape = lightcraft_develop::FocusShape::Auto;
    assert_eq!(f.settings(), expected);
    assert_eq!(f.app.session.undo.len(), undo + 1);
    assert_eq!(f.app.session.undo.last().unwrap().label, "Lens Blur Focus");
    f.app.run("edit.undo", json!({})).unwrap();
    assert_eq!(f.settings(), before);
}

#[test]
fn lens_blur_controls_external155_apply_keyboard_and_inherited_disabled_do_not_dispatch() {
    let mut f = Frames::new(Adapter::Candidate, false, 0.0, 1.0);
    let response = f.response(APPLY);
    for _ in 0..16 {
        if f.view.ctx.memory(|m| m.has_focus(response.id)) {
            break;
        }
        f.key(Key::Tab);
    }
    assert_eq!(f.view.ctx.memory(|m| m.focused()), Some(response.id));
    f.key(Key::Space);
    assert!(f.settings().lens_blur.enabled);
    assert_eq!(f.settings().lens_blur.amount, 50.0);
    f.enabled = false;
    f.settle();
    let before = f.snapshot();
    f.click(APPLY);
    f.click(FOCUS);
    f.key(Key::Space);
    assert_eq!(f.snapshot(), before);
    assert!(!popup_open(&f.view.ctx));
    f.enabled = true;
    f.merge(json!({"lens_blur": {"enabled": false}}));
    let before = f.snapshot();
    f.click(FOCUS);
    assert!(!popup_open(&f.view.ctx), "focus controls disabled with blur off");
    assert_eq!(f.snapshot(), before);
}

#[test]
fn lens_blur_controls_external155_speculative_passes_publish_no_sdk_targets_or_edits() {
    for pass in [Pass::Measure, Pass::Sizing] {
        for id in [APPLY, SUBJECT, FIRST, SECOND] {
            let mut f = Frames::new(Adapter::Candidate, true, 31.0, 1.0);
            if id != APPLY {
                f.open();
            }
            let response = f.response(id);
            let before = f.snapshot();
            for pressed in [true, false] {
                f.pointer(response.rect.center(), pressed, pass);
            }
            f.view.ctx.memory_mut(|m| m.request_focus(response.id));
            for pressed in [true, false] {
                f.frame(
                    vec![Event::Key { key: Key::Space, physical_key: Some(Key::Space), pressed, repeat: false, modifiers: Modifiers::NONE }],
                    pass,
                );
                for target in [APPLY, SUBJECT, FIRST, SECOND] {
                    assert!(!f.rects.iter().any(|(name, _)| name == target), "speculative rect {target}");
                    assert!(!f.hits.iter().any(|(name, _)| name == target), "speculative hit {target}");
                }
                assert_eq!(f.snapshot(), before);
            }
            f.settle();
            assert_eq!(f.snapshot(), before, "speculative activation cannot replay");
        }
    }
}

#[test]
fn lens_blur_controls_external155_previous_candidate_unforced_native_popup_keyboard_and_dismissal() {
    for scale in [1.0, 1.5, 2.0] {
        // Never request focus on any row here: keyboard travel must come from the actual popup. The focus menu is the
        // framework's value menu now, not the previous native popup, so its own keyboard model is the reference: it
        // opens with the checked row (Subject) holding the lead, arrows and Tab move among the rows without choosing,
        // Enter or Space picks the row the keyboard is on (a pick of the current choice changes nothing) and closes,
        // and Escape closes without a pick.
        for keys in [vec![Key::ArrowDown, Key::ArrowDown, Key::Enter], vec![Key::Tab, Key::ArrowDown, Key::Space], vec![Key::ArrowUp, Key::Escape]] {
            let mut new = Frames::new(Adapter::Candidate, true, 31.0, scale);
            let opened = new.snapshot();
            new.open();
            assert_eq!(new.snapshot(), opened, "opening picks nothing");
            assert_eq!(new.semantic_focus(), SUBJECT, "the checked row leads a value menu");
            for key in keys {
                let (focused, before) = (new.semantic_focus(), new.snapshot());
                new.key(key);
                match key {
                    Key::Enter | Key::Space => {
                        assert!(!popup_open(&new.view.ctx), "{key:?} closes the menu");
                        assert!([SUBJECT, FIRST, SECOND].contains(&focused.as_str()), "{key:?} lands on a row: {focused}");
                        // Subject is the current choice: picking it again records nothing; a mask row is one edit
                        assert_eq!(new.snapshot() != before, focused != SUBJECT, "{key:?} picks the focused row {focused}");
                    }
                    Key::Escape => {
                        assert!(!popup_open(&new.view.ctx), "Escape closes the menu");
                        assert_eq!(new.snapshot(), before, "Escape picks nothing");
                    }
                    _ => {
                        assert!(popup_open(&new.view.ctx), "{key:?} keeps the menu open");
                        assert!([SUBJECT, FIRST, SECOND].contains(&new.semantic_focus().as_str()), "{key:?} keeps the focus on a row");
                        assert_eq!(new.snapshot(), before, "{key:?} moves without choosing");
                    }
                }
            }
        }
        for adapter in [Adapter::Previous, Adapter::Candidate] {
            let mut f = Frames::new(adapter, true, 31.0, scale);
            let before = f.snapshot();
            f.open();
            f.key(Key::Escape);
            assert!(!popup_open(&f.view.ctx));
            assert_eq!(f.snapshot(), before);
            f.open();
            f.click_at(pos2(580.0, 330.0));
            assert!(!popup_open(&f.view.ctx));
            assert_eq!(f.snapshot(), before);
            f.open();
            assert_eq!(f.snapshot(), before, "reopen never replays a dismissed row");
        }
    }
}

#[test]
fn lens_blur_controls_external155_already_open_popup_rows_obey_disabled_parent() {
    for blur_off in [false, true] {
        let mut f = Frames::new(Adapter::Candidate, true, 31.0, 1.0);
        f.open();
        let at = f.response(SECOND).rect.center();
        if blur_off {
            f.merge(json!({"lens_blur": {"enabled": false}}));
        } else {
            f.enabled = false;
            f.settle();
        }
        let before = f.snapshot();
        f.click_at(at);
        f.key(Key::Enter);
        f.key(Key::Space);
        assert_eq!(f.snapshot(), before, "an already-open menu cannot escape its disabled parent");
    }
}

// Original control bodies from preimage 862aa7df…: a matched oracle, not a second product implementation.
fn previous_apply(app: &mut LightcraftApp, ui: &mut Ui, d: &DevelopSettings) {
    let b = &d.lens_blur;
    let mut on = b.enabled;
    let r = ui.checkbox(&mut on, crate::i18n::tr("Apply"));
    register(ui, "check:lensBlur.enabled", r.rect);
    if r.on_hover_text(crate::i18n::tr("Blur what is out of focus, as a wide aperture would")).changed() {
        let mut patch = json!({"enabled": on});
        // switched on the first time: a visible starting amount, as in Lightroom
        if on && b.amount == 0.0 {
            patch["amount"] = json!(50.0);
        }
        let _ = app.run("develop.merge", json!({"settings": {"lens_blur": patch}, "label": "Lens Blur"}));
    }
}

fn previous_focus(app: &mut LightcraftApp, ui: &mut Ui, d: &DevelopSettings) {
    let t = Tokens::get(ui.ctx());
    let b = &d.lens_blur;
    ui.horizontal(|ui| {
        ui.label(RichText::new(crate::i18n::tr("In Focus")).font(t.font(13.0)).color(t.text_dim));
        let chosen = b.focus_mask.and_then(|id| d.masks.iter().find(|m| m.id == id));
        // without a mask: the subject estimate, or the fixed shape chosen below
        let auto = b.shape == lightcraft_develop::FocusShape::Auto;
        let none = if auto { crate::i18n::tr("Subject") } else { crate::i18n::tr(b.shape.label()) };
        let shown = chosen.map_or_else(|| none.to_string(), |m| m.name.clone());
        let r = crate::widgets::dropdown(ui, "lensBlurFocus", &shown, t.font(14.0), t.text_label);
        let r = r.on_hover_text(crate::i18n::tr("What stays sharp: the photo's subject, or the selection of one of its masks"));
        let mut pick: Option<Option<u32>> = None;
        egui::Popup::menu(&r).show(|ui| {
            let item = ui.selectable_label(chosen.is_none() && auto, crate::i18n::tr("Subject"));
            register(ui, "lensBlurFocus:subject", item.rect);
            crate::widgets::register_response("previous:lensBlurFocus:subject", item.rect, &item);
            if item.clicked() {
                pick = Some(None);
            }
            for m in &d.masks {
                let item = ui.selectable_label(chosen.is_some_and(|c| c.id == m.id), m.name.as_str());
                register(ui, format!("lensBlurFocus:mask:{}", m.id), item.rect);
                crate::widgets::register_response(format!("previous:lensBlurFocus:mask:{}", m.id), item.rect, &item);
                if item.clicked() {
                    pick = Some(Some(m.id));
                }
            }
        });
        if let Some(focus) = pick {
            // "Subject": the estimate again, not a fixed shape
            let mut patch = json!({"focus_mask": focus});
            if focus.is_none() {
                patch["shape"] = json!("auto");
            }
            let _ = app.run("develop.merge", json!({"settings": {"lens_blur": patch}, "label": "Lens Blur Focus"}));
        }
    });
}

/// Optional matched screenshots of the isolated actual controls, not the photo renderer or full panel.
/// No output or rendering unless the parent explicitly supplies an admitted capture directory.
#[test]
#[ignore = "opt-in GPU capture, not a CPU behavior test"]
fn capture_lens_blur_controls_external155_matched_previous_candidate() {
    use egui_kittest::kittest::Queryable;
    let Some(directory) = std::env::var_os("CRAFT_UI_LENS_BLUR_CAPTURES").map(std::path::PathBuf::from) else { return };
    std::fs::create_dir_all(&directory).unwrap();
    for scale in [1.0, 1.5, 2.0] {
        for adapter in [Adapter::Previous, Adapter::Candidate] {
            for state in ["disabled", "subject", "mask"] {
                let (mut app, photo) = fixture(state != "disabled", if state == "disabled" { 0.0 } else { 31.0 });
                if state == "mask" {
                    app.run("develop.merge", json!({"settings": {"lens_blur": {"focus_mask": 93, "shape": "wide"}}})).unwrap();
                    app.session.journal.clear();
                }
                let mut ready = false;
                let mut harness = egui_kittest::Harness::builder().with_size(vec2(600.0, 360.0)).with_pixels_per_point(scale).wgpu().build_ui_state(
                    move |ui, app: &mut LightcraftApp| {
                        if !ready {
                            crate::theme::install_fonts(ui.ctx());
                            crate::theme::apply(ui.ctx());
                            app.install_framework(ui.ctx());
                            ready = true;
                            return;
                        }
                        ui.set_width(320.0);
                        let _ = take_registries(ui.ctx());
                        controls(app, ui, adapter);
                    },
                    app,
                );
                harness.input_mut().max_texture_side = Some(8192);
                harness.run_steps(8);
                if state != "disabled" {
                    // Before opening the popup the combobox is the only node with this name.
                    harness.get_by_label(if state == "mask" { "Duplicate name" } else { "Subject" }).click();
                    harness.run_steps(8);
                    assert!(popup_open(&harness.ctx));
                }
                let variant = match adapter {
                    Adapter::Previous => "previous",
                    Adapter::Candidate => "candidate",
                };
                let stem = format!("lens-blur-{variant}-{state}-{scale}x");
                harness.render().unwrap().save(directory.join(format!("{stem}.png"))).unwrap();
                let actual = harness.state();
                let metadata = json!({"fixture": "in-memory-2x2-photo-no-renderer", "screen": [600, 360], "scale": scale,
                    "variant": variant, "state": state, "settings": actual.session.develop_of(photo).unwrap(),
                    "journal": actual.session.journal, "popup": popup_open(&harness.ctx)});
                std::fs::write(directory.join(format!("{stem}.json")), serde_json::to_vec_pretty(&metadata).unwrap()).unwrap();
            }
        }
    }
}

#[test]
fn lens_blur_panel_apply_shape_duplicate_mask_subject_and_undo_preserve_exact_targets() {
    let mut f = Frames::new(Adapter::Candidate, false, 0.0, 1.0);
    f.whole_panel = true;
    f.settle();
    let before = f.settings();
    let mut expected = before.clone();
    let undo = f.app.session.undo.len();
    let history = f.app.session.catalog.photo(f.photo).unwrap().history.len();
    let selection = f.app.session.selection.clone();
    let mut revision = f.app.session.catalog.revision;
    for (index, (locator, label, patch)) in [
        (APPLY, "Lens Blur", json!({"enabled":true,"amount":50.0})),
        ("lensBlurShape:round", "Lens Blur Shape", json!({"shape":"round","area":null})),
        (SECOND, "Lens Blur Focus", json!({"focus_mask":93})),
        (SUBJECT, "Lens Blur Focus", json!({"focus_mask":null,"shape":"auto"})),
    ]
    .into_iter()
    .enumerate()
    {
        if index == 2 {
            let state = f.snapshot();
            f.click("button:lensBlurEditShape");
            assert_eq!(f.app.ui.tool, "lensBlurShape");
            assert_eq!(f.snapshot(), state);
        }
        if index >= 2 {
            f.open();
            assert_ne!(f.response(FIRST).id, f.response(SECOND).id);
        }
        f.click(locator);
        match index {
            0 => {
                expected.lens_blur.enabled = true;
                expected.lens_blur.amount = 50.0;
            }
            1 => expected.lens_blur.shape = lightcraft_develop::FocusShape::Round,
            2 => expected.lens_blur.focus_mask = Some(93),
            _ => {
                expected.lens_blur.focus_mask = None;
                expected.lens_blur.shape = lightcraft_develop::FocusShape::Auto;
            }
        }
        assert_eq!(f.settings(), expected);
        assert_eq!(f.app.session.journal.last(), Some(&("develop.merge".into(), json!({"settings":{"lens_blur":patch},"label":label}))));
        assert_eq!(f.app.session.undo.len(), undo + index + 1);
        assert_eq!(f.app.session.catalog.photo(f.photo).unwrap().history.len(), history + index + 1);
        assert!(f.app.session.catalog.revision > revision);
        revision = f.app.session.catalog.revision;
        assert_eq!(f.app.session.selection, selection);
        if index >= 2 {
            assert!(f.app.ui.tool.is_empty());
            assert!(!popup_open(&f.view.ctx));
        }
    }
    for _ in 0..4 {
        f.app.run("edit.undo", json!({})).unwrap();
        f.settle();
    }
    assert_eq!(f.settings(), before);
    assert_eq!(f.app.session.undo.len(), undo);
    assert_eq!(f.app.session.selection, selection);
    assert!(!f.app.session.segmenter.busy());
    assert_eq!(f.app.renderer.live_workers(), 0);
}

fn guarded_state(app: &LightcraftApp) -> serde_json::Value {
    let photo = app.session.active().unwrap();
    json!({"photo":app.session.catalog.photo(photo).unwrap(),"selection":app.session.selection,"revision":app.session.catalog.revision,"journal":app.session.journal,"undo":app.session.undo.len(),"redo":app.session.redo.len(),"commits":app.session.commits(),"tool":app.ui.tool,"workers":app.renderer.live_workers(),"segmenting":app.session.segmenter.busy()})
}
fn measured_release(pass: Pass) {
    for target in [APPLY, SECOND] {
        let mut f = Frames::new(Adapter::Candidate, true, 31.0, 1.0);
        f.whole_panel = true;
        f.settle();
        if target == SECOND {
            f.open();
        }
        let r = f.response(target);
        let at = r.interact_rect.center();
        let ptr = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        f.frame(vec![Event::PointerMoved(at)], Pass::Real);
        let before = guarded_state(&f.app);
        let mut pressed = 0;
        f.frame_checked(vec![ptr(true)], Pass::Real, |ui, app| {
            assert!(ui.ctx().read_response(r.id).unwrap().is_pointer_button_down_on());
            assert_eq!(guarded_state(app), before);
            pressed += 1;
        });
        assert!(pressed > 0);
        // Recreate a stale shape tool after the real press. With Auto active the next ordinary frame should clear it, not measurement.
        f.app.ui.tool = "lensBlurShape".into();
        let stale = guarded_state(&f.app);
        let mut count = 0;
        f.frame_checked(vec![ptr(false)], pass, |ui, app| {
            count += 1;
            assert_eq!(ui.is_sizing_pass(), matches!(pass, Pass::Sizing));
            assert_eq!(craft::ui::runtime::measure::active(ui.ctx()), matches!(pass, Pass::Measure));
            let (rects, hits) = take_registries(ui.ctx());
            let guarded = |n: &str| n == APPLY || n.starts_with("lensBlurFocus:");
            assert!(rects.iter().all(|(n, _)| !guarded(n)) && hits.iter().all(|(n, _)| !guarded(n)));
            assert_eq!(guarded_state(app), stale);
        });
        assert!(count > 0);
        f.settle();
        assert_eq!(guarded_state(&f.app), before);
        if target == SECOND && !popup_open(&f.view.ctx) {
            f.open();
        }
        assert_eq!(f.response(target).id, r.id);
        let mut expected = f.settings();
        let journal = f.app.session.journal.len();
        let undo = f.app.session.undo.len();
        f.click(target);
        if target == APPLY {
            expected.lens_blur.enabled = false;
        } else {
            expected.lens_blur.focus_mask = Some(93);
        }
        assert_eq!(f.settings(), expected);
        assert_eq!(f.app.session.undo.len(), undo + 1);
        assert_eq!(f.app.session.journal.len(), journal + 1);
    }
}
#[test]
fn lens_blur_native_sizing_release_defers_tool_recovery_and_preserves_same_owners() {
    measured_release(Pass::Sizing);
}
#[test]
fn lens_blur_sdk_measure_release_defers_tool_recovery_and_preserves_same_owners() {
    measured_release(Pass::Measure);
}

#[test]
fn lens_blur_off_defers_shape_and_both_brush_tool_recovery_until_real_layout() {
    for pass in [Pass::Sizing, Pass::Measure] {
        for tool in ["lensBlurShape", "lensBlurFocus", "lensBlurBlur"] {
            let mut f = Frames::new(Adapter::Candidate, false, 31.0, 1.0);
            f.whole_panel = true;
            f.settle();
            let initial = guarded_state(&f.app);
            f.app.ui.tool = tool.into();
            let stale = guarded_state(&f.app);
            let mut callbacks = 0;
            f.frame_checked(vec![], pass, |ui, app| {
                callbacks += 1;
                assert_eq!(ui.is_sizing_pass(), matches!(pass, Pass::Sizing));
                assert_eq!(craft::ui::runtime::measure::active(ui.ctx()), matches!(pass, Pass::Measure));
                assert_eq!(guarded_state(app), stale);
            });
            assert!(callbacks > 0);
            f.settle();
            assert_eq!(guarded_state(&f.app), initial);
        }
    }
}
