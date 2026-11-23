//! Real SDK control input tests. They intentionally draw only the draft controls: no merge worker or I/O.
use super::*;
use crate::headless::HeadlessView;
use egui::{Event, Key, Modifiers, PointerButton, Rect, Ui, vec2};

struct Frames {
    ctx: egui::Context,
    time: f64,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    escaped: bool,
}

impl Frames {
    fn new() -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        let spec = craft::ui::InstallSpec::new(craft::host::target::ShellInfo::default(), Arc::new(craft::window::system::NoWindows));
        craft::ui::install(&ctx, spec).unwrap();
        Self { ctx, time: 0.0, rects: vec![], hits: vec![], escaped: false }
    }

    fn frame(&mut self, events: Vec<Event>, mut draw: impl FnMut(&mut Ui)) {
        crate::widgets::take_registries(&self.ctx);
        self.time += 0.02;
        let raw = HeadlessView::raw_input(vec2(1100.0, 1000.0), 1.0, self.time, events);
        let mut output = self.ctx.run_ui(raw, |ui| draw(ui));
        output.textures_delta.clear();
        self.escaped = crate::widgets::escape_taken(&self.ctx);
        (self.rects, self.hits) = crate::widgets::take_registries(&self.ctx);
    }

    fn rect(&self, id: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing {id}: {:?}", self.rects)).1
    }
}

struct Controls {
    frames: Frames,
    opts: MergeDialog,
}

impl Controls {
    fn new(command: &str) -> Self {
        let mut this = Self { frames: Frames::new(), opts: MergeDialog::for_command(command) };
        this.settle();
        this
    }

    fn frame(&mut self, events: Vec<Event>) {
        let opts = &mut self.opts;
        self.frames.frame(events, |ui| {
            ui.set_width(220.0);
            options(ui, opts, 3);
        });
    }

    fn settle(&mut self) {
        for _ in 0..3 { self.frame(vec![]); }
    }

    fn click(&mut self, id: &str) {
        let pos = self.frames.rect(id).center();
        self.frame(vec![Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
        }
        self.settle();
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
        }
        self.settle();
    }

    fn type_draft(&mut self, id: &str, text: &str) {
        self.click(id);
        self.key(Key::A, Modifiers { ctrl: true, command: true, ..Default::default() });
        self.frame(vec![Event::Text(text.to_owned())]);
    }

    fn edit(&mut self, id: &str, text: &str) {
        self.type_draft(id, text);
        self.key(Key::Enter, Modifiers::NONE);
    }

    fn has(&self, id: &str) -> bool { self.frames.rects.iter().any(|(name, _)| name == id) }

    /// The framework convention: a pointer press activates a control without taking the keyboard, so a keyboard step
    /// starts from a control given the focus.
    fn focus(&mut self, id: &str) {
        let target = self.frames.hits.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing {id}")).1.id;
        assert!(craft::ui::runtime::focus::request(&self.frames.ctx, craft::ui::runtime::focus::FocusTarget::Id(target)));
        self.settle();
    }
}

#[test]
fn sdk155_merge_modes_render_only_applicable_controls_and_keep_hidden_parameters() {
    for command in ["merge.hdr", "merge.panorama", "merge.hdrPanorama"] {
        let mut h = Controls::new(command);
        h.opts.boundary_warp = 37.0;
        h.opts.bracket = 4;
        h.opts.show_overlay = true;
        h.opts.projection = "perspective".into();
        h.opts.deghost = "high".into();
        let before = h.opts.clone();
        h.settle();
        assert_eq!(h.opts, before, "drawing cannot normalize inactive mode parameters");
        assert_eq!(h.has("button:mergeProjection-0"), before.is_pano());
        assert_eq!(h.has("slider:merge.boundaryWarp"), before.is_pano());
        assert_eq!(h.has("button:mergeDeghost-0"), before.is_hdr());
        assert_eq!(h.has("check:merge.align"), before.is_hdr());
        assert_eq!(h.has("check:merge.showOverlay"), command == "merge.hdr");
        assert_eq!(h.has("slider:merge.bracket"), command == "merge.hdrPanorama");
        for id in ["check:merge.autoSettings", "check:merge.stack"] {
            assert!(h.has(id));
            let hit = &h.frames.hits.iter().find(|(name, _)| name == id).unwrap().1;
            let response = h.frames.ctx.read_response(hit.id).unwrap();
            assert_eq!(response.rect, h.frames.rect(id), "locator uses the actual checkbox response");
        }
        let mut names = std::collections::HashSet::new();
        for (id, rect) in &h.frames.rects {
            assert!(names.insert(id), "duplicate hit from measuring: {id}");
            assert!(rect.is_finite() && rect.is_positive(), "{id}: {rect:?}");
        }
    }
}

#[test]
fn sdk155_merge_hdr_overlay_is_disabled_until_deghost_and_retains_its_draft() {
    let mut h = Controls::new("merge.hdr");
    h.opts.show_overlay = true;
    h.settle();
    let before = h.opts.clone();
    h.click("check:merge.showOverlay");
    assert_eq!(h.opts, before, "disabled overlay is a no-op, not silently cleared");
    h.click("button:mergeDeghost-1");
    assert_eq!(h.opts.deghost, "low");
    h.click("check:merge.showOverlay");
    assert!(!h.opts.show_overlay);
    assert_eq!(h.opts.projection, before.projection);
    assert_eq!(h.opts.bracket, before.bracket);
    h.click("check:merge.align");
    assert!(!h.opts.align);
    h.focus("check:merge.align");
    h.key(Key::Space, Modifiers::NONE);
    assert!(h.opts.align, "focused SDK checkbox activates from the keyboard");
}

#[test]
fn sdk155_merge_choices_use_roving_keyboard_and_write_only_their_parameter() {
    let mut h = Controls::new("merge.hdrPanorama");
    h.click("button:mergeProjection-2");
    assert_eq!(h.opts.projection, "cylindrical");
    h.click("button:mergeDeghost-1");
    assert_eq!(h.opts.deghost, "low");
    h.focus("button:mergeDeghost-1");
    h.key(Key::ArrowRight, Modifiers::NONE);
    h.key(Key::Space, Modifiers::NONE);
    assert_eq!(h.opts.deghost, "medium");
    assert_eq!(h.opts.projection, "cylindrical");
    assert_eq!(h.opts.boundary_warp, 0.0);
    assert_eq!(h.opts.bracket, 0);
}

#[test]
fn sdk155_merge_exact_integer_fields_clamp_snap_and_keep_bracket_auto_zero() {
    let mut h = Controls::new("merge.hdrPanorama");
    h.edit("sliderField:merge.boundaryWarp", "37.4");
    assert_eq!(h.opts.boundary_warp, 37.0);
    h.edit("sliderField:merge.boundaryWarp", "999");
    assert_eq!(h.opts.boundary_warp, 100.0);
    h.edit("sliderField:merge.bracket", "3");
    assert_eq!(h.opts.bracket, 3);
    h.edit("sliderField:merge.bracket", "99");
    assert_eq!(h.opts.bracket, 9);
    h.edit("sliderField:merge.bracket", "0");
    assert_eq!(h.opts.bracket, 0, "zero still means EXIF detection");
    assert_eq!(h.opts.boundary_warp, 100.0, "bracket typing cannot write warp");
}

#[test]
fn sdk155_merge_escape_restores_number_draft_before_canceling_dialog() {
    let mut h = Controls::new("merge.panorama");
    h.opts.boundary_warp = 42.0;
    h.settle();
    h.type_draft("sliderField:merge.boundaryWarp", "73");
    // (Escape in a dialog field cancels the whole dialog by the framework convention, so there is no separate
    // "taken" marker for the outer dialog to honour; the draft is still given back and nothing is written)
    h.frame(vec![Event::Key { key: Key::Escape, physical_key: Some(Key::Escape), pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
    assert_eq!(h.opts.boundary_warp, 42.0);
    h.frame(vec![Event::Key { key: Key::Escape, physical_key: Some(Key::Escape), pressed: false, repeat: false, modifiers: Modifiers::NONE }]);
    h.settle();
    assert_eq!(h.opts.boundary_warp, 42.0);
}

#[test]
fn sdk155_merge_speculative_native_measurement_publishes_no_hits_or_edits() {
    let mut frames = Frames::new();
    let mut draft = MergeDialog { boundary_warp: 37.0, bracket: 3, ..MergeDialog::for_command("merge.hdrPanorama") };
    let before = draft.clone();
    frames.frame(vec![], |ui| {
        let mut child = ui.new_child(egui::UiBuilder::new().sizing_pass().invisible());
        child.set_width(220.0);
        options(&mut child, &mut draft, 3);
    });
    assert_eq!(draft, before);
    assert!(frames.rects.is_empty() && frames.hits.is_empty());
}

#[test]
fn sdk155_merge_cancel_discards_draft_and_invalid_confirm_cannot_start_or_save() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
    let mut frames = Frames::new();
    app.install_framework(&frames.ctx);
    let opts = MergeDialog { stack: true, deghost: "high".into(), ..Default::default() };
    app.ui.dialog = Some(crate::state::Dialog::Merge { opts: opts.clone() });
    for _ in 0..3 { frames.frame(vec![], |ui| crate::dialogs::show(&mut app, ui.ctx(), false)); }
    let pos = frames.rect("button:dialogCancel").center();
    frames.frame(vec![Event::PointerMoved(pos)], |ui| crate::dialogs::show(&mut app, ui.ctx(), false));
    for pressed in [true, false] {
        frames.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }],
            |ui| crate::dialogs::show(&mut app, ui.ctx(), false));
    }
    assert!(app.ui.dialog.is_none());
    assert!(app.merge.final_task.is_none() && app.merge.preview_task.is_none());
    assert!(app.session.last_merge.is_empty(), "Cancel cannot persist dialog choices");
    let dialog = crate::state::Dialog::Merge { opts };
    assert!(crate::dialogs::confirm_dialog(&mut app, &dialog).is_err(), "real engine rejects the empty selection");
    assert!(app.merge.final_task.is_none());
    assert!(app.session.last_merge.is_empty(), "failed confirmation cannot save last options");
}

#[test]
fn sdk155_merge_track_pointer_keyboard_and_label_reset_preserve_value_contract() {
    let mut h = Controls::new("merge.panorama");
    let span = h.frames.rect("slider:merge.boundaryWarp");
    let at = egui::pos2(span.left() + span.width() * 0.25, span.center().y);
    h.frame(vec![Event::PointerMoved(at)]);
    for pressed in [true, false] {
        h.frame(vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
    }
    h.settle();
    assert_eq!(h.opts.boundary_warp, 25.0, "automation fraction is measured between endpoint thumb centres");
    // Keep the pointer over the track while this exact SDK stop owns the keyboard.
    let track = h.frames.hits.iter().find(|(name, _)| name == "slider:merge.boundaryWarp").unwrap().1.id;
    assert!(craft::ui::runtime::focus::request(&h.frames.ctx, craft::ui::runtime::focus::FocusTarget::Id(track)));
    h.settle();
    assert_eq!(craft::ui::runtime::focus::focused_rec(&h.frames.ctx).map(|f| f.stop.id), Some(track));
    h.key(Key::ArrowUp, Modifiers::NONE);
    assert_eq!(h.opts.boundary_warp, 26.0, "hover Up steps once");
    h.key(Key::ArrowDown, Modifiers::SHIFT);
    assert_eq!(h.opts.boundary_warp, 21.0, "legacy Shift nudge is five steps");
    let label = h.frames.rect("sliderLabel:merge.boundaryWarp").center();
    h.frame(vec![Event::PointerMoved(label)]);
    for _ in 0..2 {
        for pressed in [true, false] {
            h.frame(vec![Event::PointerButton { pos: label, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
        }
    }
    h.settle();
    assert_eq!(h.opts.boundary_warp, 0.0, "double-click label still resets the draft");
}
