//! Focused import SDK interactions. Fixtures and all possible file writes are private temp paths.
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use super::ImportDialog;
use crate::headless::{Headless, HeadlessView};
use crate::state::{Dialog, ViewMode};
use crate::{LightcraftApp, Services};
use lightcraft_engine::import::ImportCandidate;
use serde_json::{Value, json};

const T: Duration = Duration::from_secs(30);

struct Review {
    h: Headless,
    dir: PathBuf,
    originals: Vec<Vec<u8>>,
}
impl Review {
    fn new(size: [f32; 2]) -> Self {
        let dir = std::env::temp_dir().join(format!("lc-sdk-import-{}-{}", std::process::id(), lightcraft_preview::next_tick()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut originals = Vec::new();
        let mut candidates = Vec::new();
        for i in 0..4u8 {
            let image = lightcraft_raster::Rgba8 { width: 24, height: 16, data: vec![[i * 53, 81, 19, 255]; 24 * 16] };
            let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap();
            let name = format!("source-{i}.png");
            let path = dir.join(&name);
            std::fs::write(&path, &bytes).unwrap();
            candidates.push(ImportCandidate {
                path: path.to_string_lossy().into(),
                name,
                format: "PNG".into(),
                width: 24,
                height: 16,
                file_size: bytes.len() as u64,
                ..Default::default()
            });
            originals.push(bytes);
        }
        // Duplicate and unreadable identities must not become selectable through Check All.
        candidates[2].duplicate = Some("content".into());
        candidates[3].error = Some("fixture unreadable".into());
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services { png: None, ..Default::default() });
        app.ui.view = ViewMode::PhotoGrid;
        app.ui.dialog = Some(Dialog::Import { opts: Box::new(ImportDialog::new(candidates)) });
        let mut h = Headless::new(app, size, 1.0);
        assert!(h.settle(T));
        Self { h, dir, originals }
    }
    fn unchanged_files(&self) {
        assert_eq!(std::fs::read_dir(&self.dir).unwrap().count(), 4);
        for (i, bytes) in self.originals.iter().enumerate() {
            assert_eq!(&std::fs::read(self.dir.join(format!("source-{i}.png"))).unwrap(), bytes);
        }
    }
}
impl Drop for Review {
    fn drop(&mut self) {
        self.h.app.ui.dialog = None;
        self.h.app.renderer.forget_imports();
        // A failing assertion inside an egui pass must not start another SDK pass while unwinding.
        if !std::thread::panicking() {
            self.h.settle(T);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
fn draft(h: &Headless) -> &ImportDialog {
    let Some(Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("review closed") };
    opts
}
fn ask(h: &mut Headless, method: &str, params: Value) -> Value {
    let r = h.request(method, params, T);
    assert_eq!(r["ok"], true, "{method}: {r}");
    h.step();
    h.step();
    r
}
fn click(h: &mut Headless, id: &str) {
    // Import retains a resizable scrolling native window. Reveal ordinary body controls,
    // not popup menu entries (their overlay can legitimately extend outside the window).
    if !id.starts_with("button:importOrganize-") {
        for _ in 0..30 {
            let widget = h.app.widgets.iter().find(|(key, _)| key == id).unwrap_or_else(|| panic!("missing {id}")).1;
            let window = h.app.widgets.iter().find(|(key, _)| key == "dialog:window").unwrap().1;
            let dy = if widget.bottom() > window.bottom() - 30.0 {
                -300.0
            } else if widget.top() < window.top() + 30.0 {
                300.0
            } else {
                break;
            };
            ask(h, "ui.move", json!({"x": window.right() - 24.0, "y": window.center().y}));
            ask(h, "ui.scroll", json!({"dy": dy}));
            assert!(h.settle(T));
        }
    }
    ask(h, "ui.clickWidget", json!({"id": id}));
}
fn edit(h: &mut Headless, id: &str, text: &str) {
    click(h, id);
    ask(h, "ui.key", json!({"key": "A", "cmd": true, "ctrl": true}));
    ask(h, "ui.text", json!({"text": text}));
    ask(h, "ui.key", json!({"key": "Enter"}));
}

#[test]
fn sdk_import_selection_identity_survives_slider_and_check_all() {
    let mut r = Review::new([1400.0, 1300.0]);
    let paths = draft(&r.h).selected_paths();
    assert_eq!(paths.len(), 2);
    click(&mut r.h, "import:0");
    assert_eq!(draft(&r.h).selected_paths(), vec![paths[1].clone()]);
    edit(&mut r.h, "sliderValue:importThumbSize", "180");
    assert_eq!(r.h.app.ui.import_thumb_size, 180.0);
    assert_eq!(draft(&r.h).selected_paths(), vec![paths[1].clone()]);
    click(&mut r.h, "button:importNone");
    assert!(draft(&r.h).selected_paths().is_empty());
    click(&mut r.h, "button:importAll");
    assert_eq!(draft(&r.h).selected_paths(), paths);
    assert_eq!(draft(&r.h).checked, vec![true, true, false, false]);
    assert!(r.h.app.import.is_none());
    r.unchanged_files();
}

#[test]
fn sdk_import_destination_picker_draft_and_cancel_write_nothing() {
    let mut r = Review::new([1600.0, 1500.0]);
    let count = Arc::new(AtomicUsize::new(0));
    let picked = count.clone();
    let destination = r.dir.join("must-not-exist");
    let choice = destination.to_string_lossy().to_string();
    r.h.app.services.pick_folder = Some(Box::new(move || (picked.fetch_add(1, Ordering::SeqCst) > 0).then(|| choice.clone())));
    let selection = r.h.app.session.selected();
    let undo = r.h.app.session.undo.len();
    let revision = r.h.app.session.catalog.revision;
    let presets = r.h.app.session.presets.iter().cloned().collect::<Vec<_>>();
    r.h.settle(T);
    click(&mut r.h, "button:importCopy");
    click(&mut r.h, "button:importDest");
    assert_eq!(draft(&r.h).destination, "", "picker cancel preserves original");
    click(&mut r.h, "button:importDest");
    assert_eq!(draft(&r.h).destination, destination.to_string_lossy());
    click(&mut r.h, "combo:importOrganize");
    click(&mut r.h, "button:importOrganize-flat");
    assert_eq!(draft(&r.h).organize, "flat");
    click(&mut r.h, "check:importDng");
    assert!(draft(&r.h).dng);
    edit(&mut r.h, "field:importKeywords", "draft, only");
    assert_eq!(draft(&r.h).keywords, "draft, only");
    assert!(r.h.app.import.is_none(), "Enter in a field cannot start import");
    assert!(!destination.exists());
    click(&mut r.h, "button:dialogCancel");
    assert!(r.h.app.ui.dialog.is_none());
    assert!(r.h.app.import.is_none());
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(r.h.app.session.selected(), selection);
    assert_eq!(r.h.app.session.undo.len(), undo);
    assert_eq!(r.h.app.session.catalog.revision, revision);
    assert_eq!(r.h.app.session.presets.iter().cloned().collect::<Vec<_>>(), presets);
    assert!(!destination.exists());
    r.unchanged_files();
}

#[test]
fn sdk_import_confirmation_imports_selected_identity_exactly_once() {
    let mut r = Review::new([1600.0, 1500.0]);
    click(&mut r.h, "import:0");
    let selected = draft(&r.h).selected_paths();
    assert_eq!(selected.len(), 1);
    edit(&mut r.h, "field:importKeywords", "confirmed");
    let undo = r.h.app.session.undo.len();
    click(&mut r.h, "button:dialogOk");
    assert!(r.h.app.ui.dialog.is_none());
    assert!(r.h.step_until(T, |h| h.app.import.is_none()));
    assert!(r.h.settle(T));
    assert_eq!(r.h.app.session.catalog.len(), 1);
    let photo = r.h.app.session.catalog.photos().next().unwrap();
    assert_eq!(photo.file_name, "source-1.png");
    assert_eq!(photo.source, lightcraft_catalog::Source::File { path: selected[0].clone() });
    assert_eq!(photo.meta.keywords, vec!["confirmed"]);
    assert_eq!(r.h.app.session.undo.len(), undo + 1);
    let revision = r.h.app.session.catalog.revision;
    let second = r.h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(second["ok"], false, "closed review cannot start a second import");
    r.h.settle(T);
    assert_eq!(r.h.app.session.catalog.len(), 1);
    assert_eq!(r.h.app.session.catalog.revision, revision);
    assert_eq!(r.h.app.session.undo.len(), undo + 1);
    r.unchanged_files();
}

#[test]
fn sdk_import_measure_preserves_draft_registry_and_never_starts_preview_or_picker() {
    let count = Arc::new(AtomicUsize::new(0));
    let picked = count.clone();
    let mut app = LightcraftApp::new(
        lightcraft_engine::Session::new().with_fs(),
        Services {
            pick_folder: Some(Box::new(move || {
                picked.fetch_add(1, Ordering::SeqCst);
                Some("/never".into())
            })),
            ..Default::default()
        },
    );
    let view = HeadlessView::new();
    app.install_framework(&view.ctx);
    let mut d = ImportDialog::new(vec![ImportCandidate {
        path: "/must-not-read.png".into(),
        name: "source.png".into(),
        format: "PNG".into(),
        ..Default::default()
    }]);
    d.copy = true;
    d.sources = vec!["/must-not-stat-source".into()];
    d.keywords = "untouched".into();
    let before = d.clone();
    let undo = app.session.undo.len();
    let presets = app.session.presets.iter().cloned().collect::<Vec<_>>();
    let raw = HeadlessView::raw_input(egui::vec2(1100.0, 1000.0), 1.0, 0.0, vec![]);
    let mut out = view.ctx.run_ui(raw, |ui| {
        let ctx = ui.ctx().clone();
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()).sizing_pass().invisible());
        craft::ui::runtime::measure::run(&ctx, || super::body(&mut app, &mut child, &mut d));
    });
    out.textures_delta.clear();
    assert_eq!(d, before);
    assert_eq!(app.renderer.in_flight(), 0);
    assert_eq!(crate::panels::left::fs_cached_running(&view.ctx), 0);
    assert!(app.import.is_none());
    assert!(app.scan.is_none());
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(app.session.undo.len(), undo);
    assert_eq!(app.session.presets.iter().cloned().collect::<Vec<_>>(), presets);
    assert!(crate::widgets::take_registry(&view.ctx).is_empty());
}

#[test]
fn sdk_import_clipped_control_refuses_offscreen_click_and_cancel_stays_noop() {
    let mut r = Review::new([1100.0, 650.0]);
    // Fixture setup: expand the recipe before the interaction under test, without scrolling
    // the short viewport to it. The SDK field at the section's end is outside the clip.
    r.h.view.ctx.data_mut(|data| data.insert_temp(egui::Id::unique("import-recipe-open"), true));
    r.h.settle(T);
    let before = draft(&r.h).clone();
    let id = "field:importRecipeName";
    let rect = r.h.app.widgets.iter().find(|(key, _)| key == id).expect("registered offscreen recipe field").1;
    let win = r.h.app.widgets.iter().find(|(key, _)| key == "dialog:window").unwrap().1;
    assert!(rect.bottom() > win.bottom(), "fixture must put the target below the viewport: {rect:?} {win:?}");
    let refused = r.h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(refused["ok"], false, "offscreen automation must not focus a clipped SDK field");
    assert_eq!(draft(&r.h), &before);
    ask(&mut r.h, "ui.dialog.cancel", json!({}));
    assert!(r.h.app.import.is_none());
    assert_eq!(r.h.app.session.catalog.len(), 0);
    r.unchanged_files();
}

#[test]
fn sdk_import_rename_focus_and_caret_survive_measurement_without_disabling_caller() {
    let mut r = Review::new([1600.0, 1500.0]);
    r.h.app.services.pick_folder = Some(Box::new(|| None));
    click(&mut r.h, "button:importCopy");
    click(&mut r.h, "field:importRename");
    ask(&mut r.h, "ui.text", json!({"text": "a"}));
    let rename = egui::Id::unique("import-rename");
    assert_eq!(draft(&r.h).rename, "a");
    // (the framework's Rename field keeps egui's focus on its text part)
    assert!(crate::text_field::has_focus_id(&r.h.view.ctx, rename));
    let mut measured = draft(&r.h).clone();
    let before = measured.clone();
    let ctx = r.h.view.ctx.clone();
    r.h.app.install_framework(&ctx);
    // This extra pass belongs to the current headless frame, not a future wall clock.
    // Keeping its time avoids moving egui's pointer history backwards on the next h.step().
    let time = ctx.input(|input| input.time);
    let input = HeadlessView::raw_input(egui::vec2(1600.0, 1500.0), 1.0, time, vec![]);
    let mut facts = (false, false, false, false, false);
    let mut out = ctx.run_ui(input, |ui| {
        facts.0 = ui.is_enabled();
        craft::ui::runtime::measure::run(&ctx, || super::body(&mut r.h.app, ui, &mut measured));
        facts.1 = ui.is_enabled();
        facts.2 = crate::text_field::has_focus_id(&ctx, rename);
        facts.3 = crate::widgets::take_registry(&ctx).is_empty();
        // Render the original dialog through the real app, on its actual modal layer.
        // Directly remounting body() at the root would legitimately lose modal focus.
        r.h.app.ui(ui);
        facts.4 = crate::text_field::has_focus_id(&ctx, rename);
    });
    out.textures_delta.clear();
    assert!(facts.0 && facts.1, "speculative disable cannot leak into the caller");
    assert!(facts.2, "measurement must not remount/disable the native Rename editor");
    assert!(facts.3, "neither SDK nor native automation may publish from measurement");
    assert!(facts.4, "focus remains on Rename after the real app renders its modal dialog");
    assert_eq!(measured, before);
    assert_eq!(draft(&r.h), &before);
    ask(&mut r.h, "ui.text", json!({"text": "b"}));
    assert_eq!(draft(&r.h).rename, "ab", "typing continues at the original native caret");
    assert!(!r.h.app.widgets.iter().any(|(id, _)| id.starts_with("button:reset:")), "port must not add Reset controls absent from main");
    ask(&mut r.h, "ui.dialog.cancel", json!({}));
    assert!(r.h.app.import.is_none());
    r.unchanged_files();
}
