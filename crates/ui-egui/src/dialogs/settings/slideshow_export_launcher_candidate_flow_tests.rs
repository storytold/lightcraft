//! SDK/native-boundary launcher comparisons. No renderer, app logic, files, picker or export worker.
//! Direct launcher controls use the installed native boundary; Cancel uses the actual shared dialog.
use super::*;
use crate::headless::HeadlessView;
use crate::state::Dialog;
use egui::{Event, Key, Modifiers, PointerButton, Rect, vec2};
use lightcraft_catalog::{Op, Photo, Source};
use lightcraft_engine::{
    Selection, Session,
    slideshow_export::{ExportRun, ExportSettings, Format},
};
use std::sync::{Arc, atomic::Ordering};

const BUTTONS: [(&str, Format, &str); 3] = [
    ("button:slideshowExportPdf", Format::Pdf, "Export PDF"),
    ("button:slideshowExportJpeg", Format::Jpeg, "Export JPEG"),
    ("button:slideshowExportVideo", Format::Video, "Export Video"),
];
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
    document: Value,
    pass: Pass,
    native: bool,
    tick: u32,
    rects: Vec<(String, Rect)>,
    labels: Vec<String>,
}
impl Fixture {
    fn new(native: bool) -> Self {
        let mut session = Session::new();
        let ids = [session.catalog.alloc_photo_id(), session.catalog.alloc_photo_id()];
        for (id, name) in ids.into_iter().zip(["Launcher A.jpg", "Launcher B.jpg"]) {
            session
                .catalog
                .apply(Op::AddPhoto { photo: Box::new(Photo::new(id, Source::Demo { scene: 0 }, name, "JPEG", 8, 6, "2026-11-22T00:00:00")) })
                .unwrap();
        }
        session.selection = Selection::single(ids[0]);
        let mut develop = (*session.develop_of(ids[0]).unwrap()).clone();
        develop.light.exposure = 0.5;
        session.set_develop(ids[0], develop, "Launcher fixture exposure").unwrap();
        assert!(!session.undo.is_empty());
        let app = LightcraftApp::new(session, crate::Services { local_places: Some(vec![]), png: None, ..Default::default() });
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let document = Self::document_of(&app);
        let mut f = Self { app, ctx, document, pass: Pass::Normal, native, tick: 0, rects: vec![], labels: vec![] };
        for _ in 0..3 {
            f.frame(vec![], "opening");
        }
        f
    }
    fn document_of(app: &LightcraftApp) -> Value {
        json!({"photos":app.session.catalog.photos().collect::<Vec<_>>(),"revision":app.session.catalog.revision,
            "selection":app.session.selection,"undo":format!("{:?}",app.session.undo),"redo":format!("{:?}",app.session.redo),
            "commits":app.session.commits(),"journal":app.session.journal,"layout":app.session.slide_layout.to_json(),
            "playback":app.session.slideshow,"overlays":app.session.slide_overlays,"titles":app.session.slide_titles})
    }
    fn guard(&self) {
        assert_eq!(Self::document_of(&self.app), self.document, "launcher/draft must not edit photos, preferences or Undo");
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.smart_previews_dir.is_none() && self.app.session.media.rendered.disk().is_none());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_files.is_none() && self.app.services.pick_folder.is_none());
        assert!(self.app.export.is_none() && self.app.import.is_none() && self.app.scan.is_none());
        assert!(self.app.ui.toast.is_none());
    }
    fn frame(&mut self, events: Vec<Event>, stage: &str) {
        self.tick += 1;
        let mut input = vec![Event::ModifiersChanged(Modifiers::NONE)];
        input.extend(events);
        let physical_point = input.iter().rev().find_map(|event| match event {
            Event::PointerMoved(at) | Event::PointerButton { pos: at, .. } => Some(*at),
            _ => None,
        });
        println!(
            "SLIDESHOW_LAUNCHER_INPUT {}",
            json!({"stage":stage,"tick":self.tick,"pass":format!("{:?}",self.pass),"events":format!("{:?}",input)})
        );
        let _ = crate::widgets::take_registries(&self.ctx);
        let raw = HeadlessView::raw_input(vec2(1000.0, 800.0), 1.0, f64::from(self.tick) / 60.0, input);
        let dialog_presented = self.app.ui.dialog.is_some();
        let app = &mut self.app;
        let pass = self.pass;
        let native = self.native;
        let output = Drain(self.ctx.run_ui(raw, |ui| {
            let ctx = ui.ctx().clone();
            if app.ui.dialog.is_some() {
                crate::dialogs::show(app, &ctx, false);
            } else {
                egui::CentralPanel::default().show(ui, |ui| {
                    let mut present = || {
                        let mut draw = |ui: &mut egui::Ui| {
                            export_section(app, ui, &Tokens::get(ui.ctx()));
                            let receipt = ui
                                .ctx()
                                .data(|data| data.get_temp::<Value>(egui::Id::unique("slideshow-launcher-current-controls")))
                                .expect("current production controls");
                            assert_eq!(receipt["frame"], json!(ui.ctx().cumulative_frame_nr()));
                            assert_eq!(receipt["pass"], json!(ui.ctx().cumulative_pass_nr()));
                            let controls = receipt["controls"].as_array().unwrap();
                            assert_eq!(controls.len(), 3);
                            for control in controls {
                                if matches!(pass, Pass::Disabled | Pass::Measure | Pass::Sizing) {
                                    assert_eq!(control["enabled"], json!(false));
                                    assert_eq!(control["clicked"], json!(false));
                                }
                            }
                            println!("SLIDESHOW_LAUNCHER_CURRENT_CONTROLS {}", receipt);
                        };
                        match pass {
                            Pass::Normal => draw(ui),
                            Pass::Disabled => {
                                ui.add_enabled_ui(false, |ui| draw(ui));
                            }
                            Pass::Measure => craft::ui::runtime::measure::run(&ctx, || draw(ui)),
                            Pass::Sizing => {
                                let mut child =
                                    ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()).sizing_pass().invisible());
                                draw(&mut child);
                            }
                        }
                    };
                    present();
                });
            }
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
        let (rects, response_hits) = crate::widgets::take_registries(&self.ctx);
        self.rects = rects;
        let cancel_hit = response_hits.iter().find(|(id, _)| id == "button:dialogCancel").map(|(_, hit)| {
            json!({
            "realId":format!("{:?}",hit.id),"layer":format!("{:?}",hit.layer_id),
            "currentInteractRect":format!("{:?}",hit.interact_rect),
            "physicalPointInside":physical_point.map(|at|hit.interact_rect.contains(at))})
        });
        self.guard();
        println!(
            "SLIDESHOW_LAUNCHER_STATE {}",
            json!({"stage":stage,"tick":self.tick,"labels":self.labels,"rects":self.rects.iter().map(|(id,rect)|json!({"id":id,"rect":format!("{:?}",rect)})).collect::<Vec<_>>(),
            "draft":format!("{:?}",self.app.ui.dialog),"currentCancelHit":cancel_hit,"nativeBoundary":!dialog_presented && native,"nativeRoute":native,"parent":if dialog_presented{"actual export dialog"}else{"direct launcher controls"},
            "focused":self.ctx.memory(|m|m.focused()).map(|id|format!("{:?}",id)),"run":self.app.session.slideshow_export.as_ref().map(|run|json!({"running":run.running(),"done":run.done.load(Ordering::Relaxed),"path":run.path}))})
        );
    }
    fn rect(&self, id: &str) -> Rect {
        self.rects.iter().find(|(name, _)| name == id).unwrap_or_else(|| panic!("missing {id}")).1
    }
    fn pointer(&mut self, at: egui::Pos2, pressed: bool, stage: &str) {
        assert!(self.ctx.content_rect().contains(at));
        self.frame(
            vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }],
            stage,
        );
    }
    fn click(&mut self, id: &str) {
        let hover = self.rect(id).center();
        self.frame(vec![Event::PointerMoved(hover)], "click-hover");
        // A first-show window can reposition during that existing hover draw. Aim the
        // actual press at its current real control, not the previous frame's geometry.
        let target = self.rect(id);
        let at = target.center();
        println!(
            "SLIDESHOW_LAUNCHER_CLICK_TARGET {}",
            json!({"id":id,"observedAfterHoverTick":self.tick,
            "hoverPoint":format!("{:?}",hover),"pressPoint":format!("{:?}",at),"targetRect":format!("{:?}",target)})
        );
        self.pointer(at, true, "click-press");
        assert!(self.rect(id).contains(at), "press must remain inside its actually drawn target");
        self.pointer(at, false, "click-release");
        self.frame(vec![], "click-quiet");
    }
    fn click_at(&mut self, at: egui::Pos2) {
        self.frame(vec![Event::PointerMoved(at)], "click-hover");
        self.pointer(at, true, "click-press");
        self.pointer(at, false, "click-release");
        self.frame(vec![], "click-quiet");
    }
    fn draft(&self, format: Format) {
        let Some(Dialog::SlideshowExport { settings }) = &self.app.ui.dialog else {
            panic!("actual slideshow export draft must open");
        };
        assert_eq!(*settings, ExportSettings::for_format(format), "launcher must preserve exact format-specific default draft");
    }
    fn cancel(&mut self) {
        assert!(self.app.ui.dialog.is_some());
        self.click("button:dialogCancel");
        assert!(self.app.ui.dialog.is_none(), "real parent Cancel must close without confirming");
        self.idle();
    }
    fn idle(&mut self) {
        let before = Self::document_of(&self.app);
        for _ in 0..3 {
            self.frame(vec![], "idle");
            assert_eq!(Self::document_of(&self.app), before);
        }
    }
    fn install_run(&mut self, running: bool) {
        assert!(self.app.session.slideshow_export.is_none());
        // In-memory status fixture only: construct a real run record without starting any job.
        let run = Arc::new(ExportRun::default());
        run.finished.store(!running, Ordering::Relaxed);
        assert_eq!(run.running(), running);
        self.app.session.slideshow_export = Some(run);
        self.frame(vec![], "fixture-run-status");
    }
    fn key(&mut self, key: Key) {
        self.frame(vec![Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: Modifiers::NONE }], "key-press");
        self.frame(vec![Event::Key { key, physical_key: Some(key), pressed: false, repeat: false, modifiers: Modifiers::NONE }], "key-release");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.ctx.forget_all_images();
        self.app.session.media.rendered.clear();
    }
}
fn open_and_cancel(native: bool, index: usize) {
    let mut f = Fixture::new(native);
    f.click(BUTTONS[index].0);
    f.draft(BUTTONS[index].1);
    assert!(f.app.session.slideshow_export.is_none(), "opening a draft must not start an export");
    f.cancel();
}
fn case_01_discovery_idle(native: bool) {
    let mut f = Fixture::new(native);
    for (id, _, label) in BUTTONS {
        let rect = f.rect(id);
        assert!(f.ctx.content_rect().contains_rect(rect));
        assert!(f.labels.iter().any(|text| text.contains(label)));
    }
    f.idle();
}
fn case_02_pdf_draft_cancel(native: bool) {
    open_and_cancel(native, 0);
}
fn case_03_jpeg_draft_cancel(native: bool) {
    open_and_cancel(native, 1);
}
fn case_04_video_draft_cancel(native: bool) {
    open_and_cancel(native, 2);
}
fn case_05_running_gates_all_formats(native: bool) {
    let mut f = Fixture::new(native);
    f.install_run(true);
    for (id, _, _) in BUTTONS {
        f.click(id);
        assert!(f.app.ui.dialog.is_none());
        assert!(f.app.session.slideshow_export.as_ref().unwrap().running());
    }
    f.idle();
}
fn case_06_disabled_is_inert(native: bool) {
    let mut f = Fixture::new(native);
    f.pass = Pass::Disabled;
    f.frame(vec![], "disabled-draw");
    for (id, _, _) in BUTTONS {
        f.click(id);
        assert!(f.app.ui.dialog.is_none());
    }
    assert!(f.app.session.slideshow_export.is_none());
}
fn speculative(native: bool, pass: Pass) {
    let mut f = Fixture::new(native);
    let observed = BUTTONS.map(|(id, _, _)| f.rect(id).center());
    f.pass = pass;
    f.frame(vec![], "speculative-draw");
    for at in observed {
        f.click_at(at);
        assert!(f.app.ui.dialog.is_none(), "speculative launcher must not dispatch a dialog command");
    }
    assert!(f.app.session.slideshow_export.is_none());
}
fn case_07_measurement_is_inert(native: bool) {
    speculative(native, Pass::Measure);
}
fn case_08_sizing_is_inert(native: bool) {
    speculative(native, Pass::Sizing);
}
fn case_09_finished_record_allows_reopen(native: bool) {
    let mut f = Fixture::new(native);
    f.install_run(false);
    for (id, format, _) in BUTTONS {
        f.click(id);
        f.draft(format);
        assert!(!f.app.session.slideshow_export.as_ref().unwrap().running());
        f.cancel();
    }
}
fn case_10_sequential_formats_reset_draft(native: bool) {
    let mut f = Fixture::new(native);
    for (id, format, _) in BUTTONS {
        f.click(id);
        f.draft(format);
        f.cancel();
        assert!(f.app.session.slideshow_export.is_none());
    }
}
fn case_11_physical_escape_cancels_parent(native: bool) {
    let mut f = Fixture::new(native);
    f.click(BUTTONS[2].0);
    f.draft(Format::Video);
    f.key(Key::Escape);
    assert!(f.app.ui.dialog.is_none());
    assert!(f.app.session.slideshow_export.is_none());
    f.idle();
}
fn case_12_natural_tab_and_activation(native: bool) {
    let mut f = Fixture::new(native);
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..12 {
        f.key(Key::Tab);
        if let Some(id) = f.ctx.memory(|m| m.focused())
            && let Some(response) = f.ctx.read_response(id)
        {
            for (name, _, _) in BUTTONS {
                if f.rect(name).contains(response.rect.center()) {
                    seen.insert(name);
                }
            }
        }
        if seen.len() == 3 {
            break;
        }
    }
    assert_eq!(seen.len(), 3, "natural Tab must reach each real launcher button");
    let focused = f.ctx.memory(|m| m.focused()).and_then(|id| f.ctx.read_response(id)).expect("real focused button").rect;
    let format = BUTTONS.into_iter().find(|(id, _, _)| f.rect(id).contains(focused.center())).unwrap().1;
    f.key(Key::Enter);
    f.draft(format);
    assert!(f.app.session.slideshow_export.is_none());
    f.cancel();
}

#[test]
fn slideshow_launcher_sdk_01_discovery_idle() {
    case_01_discovery_idle(false);
}

#[test]
fn slideshow_launcher_sdk_02_pdf_draft_cancel() {
    case_02_pdf_draft_cancel(false);
}

#[test]
fn slideshow_launcher_sdk_03_jpeg_draft_cancel() {
    case_03_jpeg_draft_cancel(false);
}

#[test]
fn slideshow_launcher_sdk_04_video_draft_cancel() {
    case_04_video_draft_cancel(false);
}

#[test]
fn slideshow_launcher_sdk_05_running_gates_all_formats() {
    case_05_running_gates_all_formats(false);
}

#[test]
fn slideshow_launcher_sdk_06_disabled_is_inert() {
    case_06_disabled_is_inert(false);
}

#[test]
fn slideshow_launcher_sdk_07_measurement_is_inert() {
    case_07_measurement_is_inert(false);
}

#[test]
fn slideshow_launcher_sdk_08_sizing_is_inert() {
    case_08_sizing_is_inert(false);
}

#[test]
fn slideshow_launcher_sdk_09_finished_record_allows_reopen() {
    case_09_finished_record_allows_reopen(false);
}

#[test]
fn slideshow_launcher_sdk_10_sequential_formats_reset_draft() {
    case_10_sequential_formats_reset_draft(false);
}

#[test]
fn slideshow_launcher_sdk_11_physical_escape_cancels_parent() {
    case_11_physical_escape_cancels_parent(false);
}

#[test]
fn slideshow_launcher_sdk_12_natural_tab_and_activation() {
    case_12_natural_tab_and_activation(false);
}
