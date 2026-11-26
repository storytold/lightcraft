//! Actual Remove panel and right-panel-route controls; not an installed window or canvas test.
//! Pixel-statistics actions use tiny in-memory sources, never an app renderer or file.
use super::*;
use crate::headless::HeadlessView;
use egui::{Event, Key, Modifiers, PointerButton, Rect};
use lightcraft_catalog::{Op, Photo, Source};
use lightcraft_develop::{DevelopSettings, Spot, SpotMode};
use lightcraft_engine::{Selection, Session, SourceLevel};
use lightcraft_geom::Point;
use serde_json::Value;
use std::sync::Arc;

const SIZE: &str = "ui.removeSize";
const FEATHER: &str = "ui.removeFeather";
const OPACITY: &str = "ui.removeOpacity";
const THRESHOLD: &str = "ui.spotsThreshold";
const VISUALIZE: &str = "check:remove.visualizeSpots";
#[derive(Clone, Copy, Debug)]
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
    parent: bool,
    pass: Pass,
    tick: u32,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    trace: std::collections::VecDeque<String>,
}
impl Fixture {
    fn new(selected: Option<usize>, spots: bool, parent: bool) -> Self {
        let mut s = Session::new();
        s.media.set_full_capacity(2);
        let ids = [s.catalog.alloc_photo_id(), s.catalog.alloc_photo_id()];
        for (n, id) in ids.into_iter().enumerate() {
            s.catalog
                .apply(Op::AddPhoto {
                    photo: Box::new(Photo::new(
                        id,
                        Source::Demo { scene: 0 },
                        if n == 0 { "Remove A.jpg" } else { "Other B.jpg" },
                        "JPEG",
                        192,
                        128,
                        "2026-11-21T00:00:00",
                    )),
                })
                .unwrap();
            s.selection = Selection::single(id);
            let mut d = (*s.develop_of(id).unwrap()).clone();
            d.light.exposure = if n == 0 { 0.25 } else { -0.5 };
            if spots {
                d.spots = vec![
                    Spot {
                        mode: SpotMode::Heal,
                        points: vec![Point::new(0.3, 0.4)],
                        size: 0.02,
                        feather: 37.0,
                        opacity: 91.0,
                        source_offset: Some(Point::new(0.2, 0.0)),
                    },
                    Spot {
                        mode: SpotMode::Clone,
                        points: vec![Point::new(0.55, 0.6)],
                        size: 0.03,
                        feather: 53.0,
                        opacity: 87.0,
                        source_offset: Some(Point::new(-0.2, 0.0)),
                    },
                ];
            }
            s.set_develop(id, d, "Fixture develop").unwrap();
            // Source-picking and the dust command use genuine engine code over bounded
            // cached pixels. No synthetic command result or network/filesystem service.
            for level in [SourceLevel::Thumb, SourceLevel::Preview, SourceLevel::Full] {
                s.media.insert(id, level, Arc::new(lightcraft_raster::Rgb32f::filled(192, 128, [0.4, 0.4, 0.4])));
            }
        }
        s.selection = Selection::single(ids[0]);
        s.active_spot = selected;
        let mut app = LightcraftApp::new(s, crate::Services { local_places: Some(vec![]), png: None, ..Default::default() });
        app.ui.right = crate::state::RightPanel::Remove;
        app.ui.tool = "remove".into();
        app.ui.visualize_spots = false;
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut f = Self {
            app,
            ctx,
            ids,
            owner: ids[0],
            parent,
            pass: Pass::Normal,
            tick: 0,
            rects: vec![],
            hits: vec![],
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
        for id in self.ids {
            assert!(self.app.session.media.has_source(id, SourceLevel::Thumb));
            assert!(self.app.session.media.has_source(id, SourceLevel::Preview));
            assert!(self.app.session.media.has_source(id, SourceLevel::Full));
        }
    }
    fn frame(&mut self, mut events: Vec<Event>) {
        self.memory_only();
        self.tick += 1;
        let input_trace = format!("{events:?}");
        crate::widgets::take_registries(&self.ctx);
        let modifiers = events
            .iter()
            .find_map(|event| match event {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        events.insert(0, Event::ModifiersChanged(modifiers));
        let raw = HeadlessView::raw_input(egui::vec2(1100.0, 3000.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let (owner, parent, pass) = (self.owner, self.parent, self.pass);
        let mut final_pass = None;
        drop(Drain(self.ctx.run_ui(raw, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.set_width(360.0);
                let mut draw = |ui: &mut egui::Ui| {
                    if parent {
                        crate::panels::right::show(app, ui);
                    } else {
                        remove(app, ui, owner);
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
            // A discarded egui pass is not a duplicate control. Drain and validate
            // each callback pass, retaining only its final publication as the app does.
            let registry = crate::widgets::take_registries(ui.ctx());
            let owned = |name: &str| {
                name == "dropdown:removeMode"
                    || name.starts_with("removeMode:")
                    || matches!(name, "button:findDust" | "button:spotRefresh" | "button:spotDelete" | "button:removeClear" | VISUALIZE)
                    || [SIZE, FEATHER, OPACITY, THRESHOLD].iter().any(|control| name.ends_with(control))
            };
            for (name, _) in registry.0.iter().filter(|(name, _)| owned(name)) {
                assert_eq!(registry.0.iter().filter(|(key, _)| key == name).count(), 1, "duplicate {name} in one pass");
                let hits = registry.1.iter().filter(|(key, _)| key == name).count();
                if !name.starts_with("sliderThumb:") {
                    assert_eq!(hits, 1, "{name} must identify one actual owner");
                }
            }
            final_pass = Some(registry);
        })));
        (self.rects, self.hits) = final_pass.unwrap();
        if self.trace.len() == 16 {
            self.trace.pop_front();
        }
        self.trace.push_back(format!("pass={pass:?} input={input_trace} active={:?} selected={:?} dragging={:?} interaction={:?} sdk_focus={:?} native_focus={:?} hits={:?} journal={:?}",
            self.app.session.active(), self.app.session.active_spot, self.app.ui.dragging_control,
            self.app.session.interaction, craft::ui::runtime::focus::focused_rec(&self.ctx),
            self.ctx.memory(|m| m.focused()), self.hits, self.app.session.journal));
    }
    fn rect(&self, id: &str) -> Rect {
        self.rects.iter().find(|(key, _)| key == id).unwrap_or_else(|| panic!("missing {id}; trace={:#?}", self.trace)).1
    }
    fn has(&self, id: &str) -> bool {
        self.rects.iter().any(|(key, _)| key == id)
    }
    fn hit(&self, id: &str) -> crate::widgets::WidgetHit {
        self.hits.iter().find(|(key, _)| key == id).unwrap_or_else(|| panic!("missing owner {id}; trace={:#?}", self.trace)).1
    }
    fn pointer(&mut self, at: egui::Pos2, pressed: bool) {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
        ]);
    }
    fn click_at(&mut self, at: egui::Pos2) {
        self.tick += 30; // Avoid treating separate user actions as a reset double-click.
        self.frame(vec![Event::PointerMoved(at)]);
        self.pointer(at, true);
        self.pointer(at, false);
        self.frame(vec![]);
    }
    fn click(&mut self, id: &str) {
        self.click_at(self.rect(id).center());
    }
    fn track(&mut self, control: &str, fraction: f32) {
        let rect = self.rect(&format!("slider:{control}"));
        self.click_at(egui::pos2(rect.left() + rect.width() * fraction, rect.center().y));
    }
    fn begin_drag(&mut self, control: &str, fraction: f32) -> egui::Pos2 {
        self.tick += 30;
        let thumb = self.rect(&format!("sliderThumb:{control}")).center();
        let span = self.rect(&format!("slider:{control}"));
        let end = egui::pos2(span.left() + span.width() * fraction, span.center().y);
        self.frame(vec![Event::PointerMoved(thumb)]);
        self.pointer(thumb, true);
        self.frame(vec![Event::PointerMoved(thumb.lerp(end, 0.5))]);
        self.frame(vec![Event::PointerMoved(end)]);
        end
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
            if craft::ui::runtime::focus::focused_rec(&self.ctx).is_some_and(|r| r.stop.id == self.hit(id).id) {
                return;
            }
            self.key(Key::Tab);
        }
        panic!("natural Tab failed to reach {id}; trace={:#?}", self.trace);
    }
    fn type_value(&mut self, control: &str, text: &str) {
        self.tab_to(&format!("sliderValue:{control}"));
        self.key_mod(Key::A, Modifiers::COMMAND);
        self.frame(vec![Event::Text(text.into())]);
        self.key(Key::Enter);
    }
    fn develop(&self) -> DevelopSettings {
        (*self.app.session.develop_of(self.ids[0]).unwrap()).clone()
    }
    fn other(&self) -> Value {
        serde_json::to_value(self.app.session.catalog.photo(self.ids[1]).unwrap()).unwrap()
    }
    fn document(&self) -> Value {
        json!({"photos": self.app.session.catalog.photos().collect::<Vec<_>>(), "selection": self.app.session.selection,
            "revision": self.app.session.catalog.revision, "undo": format!("{:?}", self.app.session.undo),
            "redo": format!("{:?}", self.app.session.redo), "journal": self.app.session.journal, "commits": self.app.session.commits()})
    }
    fn state(&self) -> Value {
        json!({"document": self.document(), "selected": self.app.session.active_spot, "tool": self.app.ui.tool,
            "size": self.app.ui.remove_size, "feather": self.app.ui.remove_feather, "opacity": self.app.ui.remove_opacity,
            "visualize": self.app.ui.visualize_spots, "threshold": self.app.ui.spots_threshold,
            "dragging": self.app.ui.dragging_control, "interaction": format!("{:?}", self.app.session.interaction)})
    }
    fn undo(&mut self) {
        self.app.run("edit.undo", json!({})).unwrap();
        self.frame(vec![]);
    }
    fn redo(&mut self) {
        self.app.run("edit.redo", json!({})).unwrap();
        self.frame(vec![]);
    }
    fn idle(&mut self) {
        let before = self.state();
        for _ in 0..3 {
            self.frame(vec![]);
        }
        assert_eq!(self.state(), before, "idle frames must not replay edits; trace={:#?}", self.trace);
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
fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-5, "{actual} != {expected}");
}

#[test]
fn remove_panel_modes_and_unselected_brush_defaults_use_actual_controls_without_document_edits() {
    let mut f = Fixture::new(None, true, true);
    let before = f.document();
    for tool in ["heal", "clone", "fill", "remove"] {
        // the Mode dropdown (the menu item keys are the tools)
        f.click("dropdown:removeMode");
        f.click(&format!("removeMode:{tool}"));
        assert_eq!(f.app.ui.tool, tool);
        assert!(!f.has(&format!("removeMode:{tool}")), "the menu closed after the pick");
        assert_eq!(f.document(), before);
    }
    assert!(!f.has("button:spotRefresh") && !f.has("button:spotDelete"));
    for (control, fraction) in [(SIZE, 0.7), (FEATHER, 0.21), (OPACITY, 0.44)] {
        f.track(control, fraction);
    }
    close(f.app.ui.remove_size as f64, 0.175);
    close(f.app.ui.remove_feather as f64, 21.0);
    close(f.app.ui.remove_opacity as f64, 44.0);
    assert_eq!(f.document(), before);
    assert!(f.app.session.interaction.is_none());
    f.idle();
}

#[test]
fn remove_selected_brush_drags_edit_only_one_spot_and_photo_in_one_real_undo_transaction_each() {
    let mut f = Fixture::new(Some(1), true, false);
    let other = f.other();
    for (control, fraction, expected) in [(SIZE, 0.6, 0.15), (FEATHER, 0.24, 24.0), (OPACITY, 0.63, 63.0)] {
        let before = f.develop();
        let undo = f.app.session.undo.len();
        let journal = f.app.session.journal.len();
        let end = f.begin_drag(control, fraction);
        assert!(f.app.session.interaction.is_some(), "real live transaction; trace={:#?}", f.trace);
        assert_eq!(f.app.session.undo.len(), undo, "live updates do not append Undo steps");
        f.pointer(end, false);
        f.frame(vec![]);
        let after = f.develop();
        let mut wanted = before.clone();
        match control {
            SIZE => wanted.spots[1].size = expected,
            FEATHER => wanted.spots[1].feather = expected,
            OPACITY => wanted.spots[1].opacity = expected,
            _ => panic!("unexpected control {control}"),
        }
        assert_eq!(after, wanted, "the selected spot is the only develop delta");
        assert_eq!(f.other(), other);
        assert_eq!(f.app.session.undo.len(), undo + 1);
        assert!(f.app.session.interaction.is_none() && f.app.ui.dragging_control.is_none());
        let commands = &f.app.session.journal[journal..];
        assert_eq!(commands.iter().filter(|(command, _)| command == "develop.beginInteraction").count(), 1);
        assert_eq!(commands.iter().filter(|(command, _)| command == "develop.endInteraction").count(), 1);
        let key = match control {
            SIZE => "size",
            FEATHER => "feather",
            _ => "opacity",
        };
        assert!(commands.iter().any(|(command, payload)| command == "spot.update" && payload == &json!({key: expected})));
        assert!(commands.iter().all(|(command, payload)| match command.as_str() {
            "develop.beginInteraction" | "develop.endInteraction" => true,
            "spot.update" => payload.as_object().is_some_and(|fields| fields.len() == 1 && fields.contains_key(key)),
            _ => false,
        }));
        f.undo();
        assert_eq!(f.develop(), before);
        f.redo();
        assert_eq!(f.develop(), after);
        f.idle();
    }
}

#[test]
fn remove_natural_keyboard_modes_numeric_commit_and_slider_steps_use_the_real_focus_owners() {
    let mut f = Fixture::new(Some(1), true, false);
    let other = f.other();
    let undo = f.app.session.undo.len();
    f.tab_to("dropdown:removeMode");
    f.key(Key::Enter);
    assert!(f.has("removeMode:heal"), "Enter opens the Mode menu; trace={:#?}", f.trace);
    for _ in 0..6 {
        if craft::ui::runtime::focus::focused_rec(&f.ctx).is_some_and(|r| r.stop.id == f.hit("removeMode:heal").id) {
            break;
        }
        f.key(Key::ArrowDown);
    }
    assert!(
        craft::ui::runtime::focus::focused_rec(&f.ctx).is_some_and(|r| r.stop.id == f.hit("removeMode:heal").id),
        "arrows reach Heal; trace={:#?}",
        f.trace
    );
    f.key(Key::Space);
    assert_eq!(f.app.ui.tool, "heal");
    assert_eq!(f.app.session.undo.len(), undo);
    let before = f.develop();
    f.type_value(SIZE, "125");
    let mut wanted = before.clone();
    wanted.spots[1].size = 0.125;
    assert_eq!(f.develop(), wanted, "typed size preserves the displayed /1000 command scale");
    assert_eq!(f.app.session.undo.len(), undo + 1);
    f.tab_to(&format!("slider:{FEATHER}"));
    f.key(Key::ArrowRight);
    wanted.spots[1].feather += 1.0;
    assert_eq!(f.develop(), wanted);
    assert_eq!(f.app.session.undo.len(), undo + 2);
    assert_eq!(f.other(), other);
    assert!(f.app.session.interaction.is_none());
    f.idle();
}

#[test]
fn remove_visualize_and_threshold_are_ui_only_and_disabled_threshold_cannot_edit() {
    let mut f = Fixture::new(None, true, false);
    let before = f.document();
    let threshold = f.app.ui.spots_threshold;
    f.track(THRESHOLD, 0.8);
    assert_eq!(f.app.ui.spots_threshold, threshold);
    f.click(VISUALIZE);
    assert!(f.app.ui.visualize_spots);
    f.track(THRESHOLD, 0.8);
    close(f.app.ui.spots_threshold as f64, 80.0);
    f.type_value(THRESHOLD, "12,6%");
    close(f.app.ui.spots_threshold as f64, 13.0);
    f.click(VISUALIZE);
    assert!(!f.app.ui.visualize_spots);
    f.track(THRESHOLD, 0.1);
    f.frame(vec![Event::Text("99".into())]);
    close(f.app.ui.spots_threshold as f64, 13.0);
    assert_eq!(f.document(), before);
    assert!(f.app.session.interaction.is_none());
    f.idle();
}

#[test]
fn remove_selected_refresh_and_delete_run_existing_actions_once_with_one_target_and_undo() {
    let mut f = Fixture::new(Some(1), true, true);
    let before = f.develop();
    let other = f.other();
    let undo = f.app.session.undo.len();
    let journal = f.app.session.journal.len();
    f.click("button:spotRefresh");
    let refreshed = f.develop();
    let mut wanted = before.clone();
    wanted.spots[1].source_offset = refreshed.spots[1].source_offset;
    assert_ne!(wanted.spots[1].source_offset, before.spots[1].source_offset, "actual engine picks another source");
    assert_eq!(refreshed, wanted);
    assert_eq!(f.other(), other);
    assert_eq!(&f.app.session.journal[journal..], &[("spot.refreshSource".into(), json!({}))]);
    assert_eq!(f.app.session.undo.len(), undo + 1);
    f.undo();
    assert_eq!(f.develop(), before);
    f.redo();
    assert_eq!(f.develop(), refreshed);
    let journal = f.app.session.journal.len();
    let undo = f.app.session.undo.len();
    f.click("button:spotDelete");
    let mut deleted = refreshed.clone();
    deleted.spots.remove(1);
    assert_eq!(f.develop(), deleted);
    assert_eq!(f.app.session.active_spot, None);
    assert_eq!(&f.app.session.journal[journal..], &[("spot.delete".into(), json!({}))]);
    assert_eq!(f.app.session.undo.len(), undo + 1);
    assert_eq!(f.other(), other);
    f.undo();
    assert_eq!(f.develop(), refreshed);
    f.redo();
    assert_eq!(f.develop(), deleted);
    f.idle();
}

#[test]
fn remove_delete_all_preserves_reverse_indexed_payloads_and_existing_per_spot_undo_steps() {
    let mut f = Fixture::new(None, true, true);
    let before = f.develop();
    let other = f.other();
    let undo = f.app.session.undo.len();
    let journal = f.app.session.journal.len();
    f.click("button:removeClear");
    assert!(f.develop().spots.is_empty());
    assert_eq!(f.other(), other);
    assert_eq!(&f.app.session.journal[journal..], &[("spot.delete".into(), json!({"index": 1})), ("spot.delete".into(), json!({"index": 0}))]);
    assert_eq!(f.app.session.undo.len(), undo + 2, "preserve original per-command Undo, not a new batch policy");
    f.undo();
    assert_eq!(f.develop().spots, vec![before.spots[0].clone()]);
    f.undo();
    assert_eq!(f.develop(), before);
    f.redo();
    f.redo();
    assert!(f.develop().spots.is_empty());
    f.idle();
}

#[test]
fn remove_find_dust_uses_the_real_command_on_memory_pixels_once_without_false_edits() {
    let mut f = Fixture::new(None, false, true);
    let before = f.develop();
    let other = f.other();
    let undo = f.app.session.undo.len();
    let journal = f.app.session.journal.len();
    let prepared = f
        .app
        .session
        .render_job(f.ids[0], 1600, 1600, false, false)
        .map(|job| (job.level, matches!(job.source, lightcraft_engine::media::SourceRef::Loaded(_))));
    assert_eq!(prepared, Some((SourceLevel::Preview, true)), "Dust must use the fixture's actual cached pixels");
    f.click("button:findDust");
    assert_eq!(
        &f.app.session.journal[journal..],
        &[("spot.findDust".into(), json!({}))],
        "Dust prepared (level, loaded)={prepared:?}; toast={:?}; trace={:#?}",
        f.app.ui.toast,
        f.trace
    );
    assert_eq!(f.app.ui.toast.as_ref().map(|toast| toast.0.as_str()), Some(crate::i18n::tr("No dust spots found")));
    assert_eq!(f.develop(), before, "uniform image has no sensor dust");
    assert_eq!(f.other(), other);
    assert_eq!(f.app.session.undo.len(), undo);
    assert!(f.app.session.interaction.is_none());
    f.idle();
}

#[test]
fn remove_disabled_measurement_and_sizing_do_not_edit_commands_defaults_or_replay_input() {
    for pass in [Pass::Disabled, Pass::Measure, Pass::Sizing] {
        let mut f = Fixture::new(Some(1), true, false);
        let before = f.state();
        let targets: Vec<_> = [
            "dropdown:removeMode",
            "button:findDust",
            "button:spotDelete",
            "button:removeClear",
            VISUALIZE,
            "slider:ui.removeSize",
            "slider:ui.removeFeather",
            "slider:ui.removeOpacity",
        ]
        .iter()
        .map(|id| f.rect(id).center())
        .collect();
        f.pass = pass;
        f.frame(vec![]);
        f.frame(vec![]);
        for at in targets {
            f.click_at(at);
        }
        assert_eq!(f.state(), before, "{pass:?} is inert; trace={:#?}", f.trace);
        f.pass = Pass::Normal;
        f.frame(vec![]);
        f.idle();
        assert_eq!(f.state(), before, "ineligible input cannot replay on return");
    }
}

#[test]
fn remove_actual_right_panel_route_without_selection_has_no_live_remove_controls_or_replay() {
    let mut f = Fixture::new(Some(1), true, true);
    let old = f.rect("button:spotDelete").center();
    f.app.run("library.select", json!({"ids": []})).unwrap();
    f.frame(vec![]);
    let before = f.state();
    assert!(!f.has("button:spotDelete") && !f.has("button:findDust") && !f.has("slider:ui.removeSize"));
    f.click_at(old);
    f.frame(vec![Event::Text("125".into())]);
    assert_eq!(f.state(), before);
    f.app.run("library.select", json!({"ids": [f.ids[0].0]})).unwrap();
    f.frame(vec![]);
    assert_eq!(f.develop().spots.len(), 2);
    assert!(f.app.session.interaction.is_none());
    f.idle();
}

#[test]
fn remove_captured_buttons_and_brushes_cannot_retarget_another_photo_or_selected_spot() {
    let mut f = Fixture::new(Some(1), true, true);
    let old = f.hit("button:spotDelete");
    let at = f.rect("button:spotDelete").center();
    f.pointer(at, true);
    f.app.run("spot.select", json!({"index": 0})).unwrap();
    f.frame(vec![]);
    assert_ne!(f.hit("button:spotDelete").id, old.id, "selected-spot response ownership changes");
    let before = f.document();
    f.pointer(at, false);
    f.frame(vec![]);
    assert_eq!(f.document(), before);
    let at = f.rect("button:removeClear").center();
    f.pointer(at, true);
    f.app.run("library.select", json!({"ids": [f.ids[1].0]})).unwrap();
    f.frame(vec![]);
    let before = f.document();
    f.pointer(at, false);
    f.frame(vec![]);
    assert_eq!(f.document(), before);
    assert_eq!(f.app.session.develop_of(f.ids[1]).unwrap().spots.len(), 2);

    let mut f = Fixture::new(Some(1), true, false);
    let before = f.develop();
    let other = f.other();
    let undo = f.app.session.undo.len();
    let end = f.begin_drag(SIZE, 0.8);
    assert!(f.app.session.interaction.is_some());
    f.key(Key::Escape);
    f.pointer(end, false);
    f.frame(vec![]);
    assert_eq!(f.develop(), before, "actual Escape cancels the real selected-spot preview");
    assert_eq!(f.app.session.undo.len(), undo);
    assert!(f.app.session.interaction.is_none() && f.app.ui.dragging_control.is_none());
    let end = f.begin_drag(SIZE, 0.8);
    assert!(f.app.session.interaction.is_some());
    f.pass = Pass::Disabled;
    f.frame(vec![]);
    f.pointer(end, false);
    assert_eq!(f.develop(), before, "disabling an owned drag cancels, not commits it");
    assert_eq!(f.app.session.undo.len(), undo);
    f.pass = Pass::Normal;
    f.frame(vec![]);
    assert!(f.app.session.interaction.is_none() && f.app.ui.dragging_control.is_none());
    let end = f.begin_drag(SIZE, 0.8);
    assert!(f.app.session.interaction.is_some());
    f.app.run("spot.select", json!({"index": 0})).unwrap();
    let cut = f.app.session.journal.len();
    f.frame(vec![]);
    assert_eq!(f.develop(), before, "spot ownership change cancels old preview rather than retargeting");
    assert_eq!(f.app.session.undo.len(), undo);
    assert_eq!(f.other(), other);
    assert!(f.app.session.interaction.is_none() && f.app.ui.dragging_control.is_none());
    f.pointer(end, false);
    f.frame(vec![]);
    assert!(!f.app.session.journal[cut..].iter().any(|(command, _)| command == "spot.update"));
    assert_eq!(f.develop(), before);
    f.idle();
}
