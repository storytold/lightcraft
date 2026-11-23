//! Wave110 slideshow controls: real input through the production dialog and command path.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lightcraft_engine::slideshow_export::{ExportSettings, Format};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::pick::PickRequest;
use crate::state::Dialog;
use crate::tests_fixtures::SyntheticLibrary;

const WAIT: Duration = Duration::from_secs(30);

fn open(format: &str, tag: &str) -> (SyntheticLibrary, Headless) {
    let files = SyntheticLibrary::with_photos(tag, 2);
    let mut app = files.app();
    app.run("dialog.slideshowExport", json!({"format": format})).unwrap();
    let mut h = Headless::new(app, [1200.0, 1000.0], 1.0);
    assert!(h.settle(WAIT));
    (files, h)
}

fn settings(h: &Headless) -> &ExportSettings {
    let Some(Dialog::SlideshowExport { settings }) = &h.app.ui.dialog else { panic!("slideshow dialog closed") };
    settings
}

fn ask(h: &mut Headless, method: &str, params: Value) {
    let result = h.request(method, params, WAIT);
    assert_eq!(result["ok"], true, "{method}: {result}");
    h.step();
    h.step();
}

fn click(h: &mut Headless, id: &str) {
    ask(h, "ui.clickWidget", json!({"id": id}));
}

#[test]
fn slideshow_controls_format_defaults_and_video_choices() {
    let (_files, mut h) = open("pdf", "wave110-slideshow-formats");
    click(&mut h, "check:slideshowExportFullScreen");
    assert!(!settings(&h).full_screen);
    click(&mut h, "button:slideshowExportSize-2");
    assert_eq!(settings(&h).size, 3840);
    click(&mut h, "button:slideshowExportFormat-2");
    assert_eq!(settings(&h).format, Format::Video);
    assert_eq!((settings(&h).size, settings(&h).quality, settings(&h).fps), (720, 75, 30));
    assert!(!settings(&h).full_screen, "format changes preserve the PDF preference");
    click(&mut h, "button:slideshowExportSize-1");
    click(&mut h, "button:slideshowExportFps-0");
    click(&mut h, "button:slideshowExportContainer-1");
    assert_eq!((settings(&h).size, settings(&h).fps, settings(&h).container.as_str()), (1080, 24, "webm"));
    click(&mut h, "button:slideshowExportFormat-1");
    assert_eq!(settings(&h).format, Format::Jpeg);
    assert_eq!((settings(&h).size, settings(&h).quality, settings(&h).container.as_str()), (1920, 90, "mp4"));
    assert!(!h.app.widgets.iter().any(|(id, _)| id == "check:slideshowExportFullScreen"));
    click(&mut h, "button:slideshowExportFormat-0");
    assert!(!settings(&h).full_screen);
    click(&mut h, "button:dialogCancel");
    assert!(h.app.ui.dialog.is_none());
    assert!(h.app.session.slideshow_export.is_none());
}

#[test]
fn slideshow_controls_quality_pointer_keyboard_and_exact_entry() {
    let (_files, mut h) = open("pdf", "wave110-slideshow-quality");
    click(&mut h, "slider:slideshowExportQuality");
    let pointer_value = settings(&h).quality;
    assert!((1..=100).contains(&pointer_value));
    assert_ne!(pointer_value, 90, "the actual slider input must update the draft");
    ask(&mut h, "ui.key", json!({"key": "right"}));
    assert_eq!(settings(&h).quality, pointer_value + 1);
    click(&mut h, "sliderValue:slideshowExportQuality");
    ask(&mut h, "ui.key", json!({"key": "A", "cmd": true, "ctrl": true}));
    ask(&mut h, "ui.text", json!({"text": "37"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(settings(&h).quality, 37);
    click(&mut h, "button:dialogCancel");
    assert!(h.app.session.slideshow_export.is_none());
}

#[test]
fn slideshow_controls_confirm_keeps_webm_picker_and_cancel_semantics() {
    let (_files, mut h) = open("video", "wave110-slideshow-picker");
    let requests = Arc::new(Mutex::new(Vec::<PickRequest>::new()));
    let seen = Arc::clone(&requests);
    h.app.services.picker = Some(Box::new(move |request| {
        seen.lock().unwrap().push(request);
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Vec::new()).unwrap(); // The user cancels the real save picker.
        Ok(rx)
    }));
    click(&mut h, "button:slideshowExportContainer-1");
    click(&mut h, "button:dialogOk");
    assert!(h.settle(WAIT));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].filter.as_ref().unwrap().1, ["webm"]);
    assert_eq!(requests[0].file_name.as_deref(), Some("Slideshow.webm"));
    assert!(h.app.ui.dialog.is_none());
    assert!(h.app.session.slideshow_export.is_none(), "cancel must not start an export");
}

#[test]
fn slideshow_controls_radio_keyboard_selects_focused_format() {
    let (_files, mut h) = open("pdf", "wave110-slideshow-radio-keyboard");
    click(&mut h, "button:slideshowExportFormat-0");
    // The framework convention: a pointer press picks without taking the keyboard, so the keys start from the
    // radio given the focus.
    let radio = h.app.widget_hits.iter().find(|(id, _)| id == "button:slideshowExportFormat-0").expect("the PDF radio").1.id;
    assert!(craft::ui::runtime::focus::request(&h.view.ctx, craft::ui::runtime::focus::FocusTarget::Id(radio)));
    h.step();
    h.step();
    ask(&mut h, "ui.key", json!({"key": "right"}));
    ask(&mut h, "ui.key", json!({"key": "Space"}));
    assert_eq!(settings(&h).format, Format::Jpeg);
    assert_eq!((settings(&h).size, settings(&h).quality), (1920, 90));
    click(&mut h, "button:dialogCancel");
    assert!(h.app.session.slideshow_export.is_none());
}
