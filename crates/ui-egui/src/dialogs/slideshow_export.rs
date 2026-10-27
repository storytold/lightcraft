//! View ▸ Export Slideshow…: the dialog's choices go to `app.slideshowExport`, which asks where
//! to write (a PDF or video file, or the folder of a JPEG sequence) and starts the engine's
//! `slideshow.export` in the background. The export's row in the activity stack shows its
//! progress and ✕ (the engine owns the row); [`poll`] tells the user how it ended.

use serde_json::{Value, json};

use super::frame::{choices, field};
use super::*;
use crate::LightcraftApp;
use crate::pick::{PickKind, PickRequest, Picked};

/// `app.slideshowExport {format, size?, quality?, fps?, container?, fullScreen?, speed?, path?}`:
/// without a path, a save dialog asks for one first.
pub fn start(app: &mut LightcraftApp, id: &str, p: &Value) -> Result<Value, String> {
    if app.session.slideshow_export.as_ref().is_some_and(|r| r.running()) {
        return Err("a slideshow export is already running".into());
    }
    let mut params = p.clone();
    if p.get("path").and_then(Value::as_str).is_none_or(|x| x.trim().is_empty()) {
        let req = match p.get("format").and_then(Value::as_str) {
            Some("jpeg") => PickRequest {
                kind: PickKind::Save,
                title: Some(crate::i18n::tr("Export Slideshow: folder for the JPEG files").to_string()),
                filter: None,
                file_name: Some("Slideshow".into()),
            },
            Some("video") if p.get("container").and_then(Value::as_str) == Some("webm") => {
                PickRequest::save(crate::i18n::tr("Export Slideshow"), "WebM", &["webm"], "Slideshow.webm")
            }
            Some("video") => PickRequest::save(crate::i18n::tr("Export Slideshow"), "MP4", &["mp4"], "Slideshow.mp4"),
            _ => PickRequest::save(crate::i18n::tr("Export Slideshow"), "PDF", &["pdf"], "Slideshow.pdf"),
        };
        let paths = match crate::pick::ask(app, id, p, "path", req, |_| None) {
            Picked::Now(paths) => paths,
            Picked::Later => return Ok(Value::Null),
            Picked::Unavailable => return Err("no save dialog on this platform; supply path".into()),
        };
        let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
        params["path"] = json!(path);
    }
    let r = app.run("slideshow.export", params)?;
    app.slideshow_export_reported = false;
    Ok(r)
}

/// Each frame: when the export has ended, say how (once).
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    if app.slideshow_export_reported {
        return;
    }
    let Some(run) = app.session.slideshow_export.clone() else { return };
    if run.running() {
        return;
    }
    app.slideshow_export_reported = true;
    let msg = match run.result() {
        Some(Ok(r)) => {
            let what = match r["format"].as_str() {
                Some("pdf") => crate::i18n::tr_format!("{n} pages", n = r["pages"].as_u64().unwrap_or(0)),
                Some("jpeg") => crate::i18n::tr_format!("{n} JPEG files", n = r["slides"].as_u64().unwrap_or(0)),
                _ => crate::i18n::tr_format!("{s} s of AV1 video", s = r["seconds"].as_f64().unwrap_or(0.0).round()),
            };
            let mut m = crate::i18n::tr_format!("Slideshow exported: {what}", what = what);
            if let Some(n) = r["skipped"].as_u64().filter(|n| *n > 0) {
                m += &crate::i18n::tr_format!(" · {n} skipped (could not be rendered)", n = n);
            }
            m
        }
        Some(Err(e)) => e,
        None => crate::i18n::tr("Export stopped unexpectedly").to_string(),
    };
    app.toast(ctx, msg);
}

/// A labelled row (fixed label column).
/// View ▸ Export Slideshow…: what to write (PDF, JPEG sequence, video) and how big; where comes
/// after (a save dialog).
pub(super) fn slideshow_export_body(app: &mut LightcraftApp, ui: &mut egui::Ui, settings: &mut lightcraft_engine::slideshow_export::ExportSettings) {
    use lightcraft_engine::slideshow_export::{ExportSettings, Format};
    let t = Tokens::get(ui.ctx());
    let photos = app.session.visible_cloned().len();
    let slide = app.session.slideshow.slide_duration;
    let (intro, ending) = app.session.slide_titles.durations(slide);
    let titles = usize::from(intro.is_some()) + usize::from(ending.is_some());
    let seconds = photos as f64 * slide + intro.unwrap_or(0.0) + ending.unwrap_or(0.0);
    let summary = crate::i18n::tr_format!(
        "{n} photos in view · {slides} slides · {time}",
        n = photos,
        slides = photos + titles,
        time = format!("{}:{:02}", (seconds / 60.0).floor() as u64, (seconds % 60.0).round() as u64)
    );
    ui.label(egui::RichText::new(summary).color(t.text_dim));
    let before = settings.format;
    choices(ui, "Format", "slideshowExportFormat", &[(Format::Pdf, "PDF"), (Format::Jpeg, "JPEG"), (Format::Video, "Video")], &mut settings.format);
    if settings.format != before {
        *settings = ExportSettings { full_screen: settings.full_screen, ..ExportSettings::for_format(settings.format) };
    }
    match settings.format {
        Format::Video => {
            choices(ui, "Size", "slideshowExportSize", &[(720, "720p"), (1080, "1080p")], &mut settings.size);
            choices(ui, "Frame rate", "slideshowExportFps", &[(24, "24 fps"), (30, "30 fps")], &mut settings.fps);
            let mut webm = settings.container == "webm";
            choices(ui, "Container", "slideshowExportContainer", &[(false, "MP4"), (true, "WebM")], &mut webm);
            settings.container = if webm { "webm".into() } else { "mp4".into() };
        }
        _ => {
            choices(ui, "Size", "slideshowExportSize", &[(1024, "1024 px"), (1920, "1920 px"), (3840, "3840 px")], &mut settings.size);
        }
    }
    field(ui, "Quality", |ui| {
        let r = ui.add(egui::Slider::new(&mut settings.quality, 1..=100));
        crate::widgets::register(ui, "slider:slideshowExportQuality", r.rect);
    });
    let note = match settings.format {
        Format::Pdf => {
            let r = ui.checkbox(&mut settings.full_screen, crate::i18n::tr("Automatically show full screen"));
            crate::widgets::register(ui, "check:slideshowExportFullScreen", r.rect);
            "One page per slide (the long side in pixels = points), with the slide duration and fade for the viewer's presentation mode."
        }
        Format::Jpeg => "Numbered JPEG files (name-001.jpg…) in a new folder named in the next step.",
        Format::Video => "AV1 video, each slide held for its duration with its fades. No sound: music is not exported yet.",
    };
    ui.label(egui::RichText::new(crate::i18n::tr(note)).color(t.text_dim));
    ui.label(
        egui::RichText::new(crate::i18n::tr("Slides use the slideshow's layout, overlays and intro and ending screens, in view order."))
            .color(t.text_dim),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Dialog;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// The app with a picker that answers `answer` and remembers what it was asked.
    fn app_answering(answer: String) -> (LightcraftApp, Arc<Mutex<Vec<PickRequest>>>) {
        let asked = Arc::new(Mutex::new(Vec::<PickRequest>::new()));
        let requests = asked.clone();
        let services = crate::Services {
            picker: Some(Box::new(move |req| {
                requests.lock().unwrap().push(req);
                let (tx, rx) = std::sync::mpsc::channel();
                tx.send(vec![answer.clone()]).unwrap();
                Ok(rx)
            })),
            ..Default::default()
        };
        let mut s = lightcraft_engine::Session::with_demo();
        s.execute("slideshow.playback", &json!({"slideDuration": 1, "fadeDuration": 0.5})).unwrap();
        (LightcraftApp::new(s, services), asked)
    }

    fn run_dialog(h: &mut crate::headless::Headless) -> Value {
        let dialog = h.app.ui.dialog.take().unwrap();
        crate::panels::dialogs::confirm_dialog(&mut h.app, &dialog).unwrap();
        let deadline = Instant::now() + Duration::from_secs(300);
        let mut seen_row = false;
        loop {
            h.step();
            seen_row |= h.app.session.activity.list().iter().any(|t| t.kind == "export" && t.label == "Exporting slideshow");
            if h.app.slideshow_export_reported && h.app.session.slideshow_export.is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "export did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
        let run = h.app.session.slideshow_export.clone().unwrap();
        let mut r = run.result().unwrap().unwrap();
        r["sawRow"] = json!(seen_row);
        r["toast"] = json!(h.app.ui.toast.as_ref().map(|t| t.0.clone()));
        r
    }

    #[test]
    fn menu_dialog_picker_and_background_pdf_export() {
        let dir = std::env::temp_dir().join(format!("lc-ui-ssexp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("Show.pdf").to_string_lossy().to_string();
        let (mut app, asked) = app_answering(path.clone());
        assert!(crate::menus::ui_enabled(&app, "dialog.slideshowExport"));
        app.run("dialog.slideshowExport", json!({"format": "pdf"})).unwrap();
        let Some(Dialog::SlideshowExport { settings }) = app.ui.dialog.as_mut() else { panic!("no slideshow export dialog") };
        assert_eq!((settings.size, settings.quality, settings.full_screen), (1920, 90, true));
        settings.size = 96;
        let mut h = crate::headless::Headless::new(app, [1000.0, 800.0], 1.0);
        let r = run_dialog(&mut h);
        let req = asked.lock().unwrap();
        assert_eq!(req[0].kind, PickKind::Save);
        assert_eq!(req[0].filter.as_ref().unwrap().1, &["pdf"]);
        assert_eq!(r["width"], 96);
        assert!(r["pages"].as_u64().unwrap() > 1);
        assert!(std::fs::read(&path).unwrap().starts_with(b"%PDF-1.4"));
        assert!(r["toast"].as_str().unwrap().contains("Slideshow exported"), "{r}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn video_dialog_asks_for_an_mp4_and_jpeg_for_a_folder_name() {
        let dir = std::env::temp_dir().join(format!("lc-ui-ssexp-v-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("Show.mp4").to_string_lossy().to_string();
        let (mut app, asked) = app_answering(path.clone());
        app.run("dialog.slideshowExport", json!({"format": "video"})).unwrap();
        let Some(Dialog::SlideshowExport { settings }) = app.ui.dialog.as_mut() else { panic!("no dialog") };
        assert_eq!((settings.size, settings.fps, settings.container.as_str()), (720, 30, "mp4"));
        settings.size = 64;
        settings.fps = 24;
        let mut h = crate::headless::Headless::new(app, [1000.0, 800.0], 1.0);
        let r = run_dialog(&mut h);
        assert_eq!(asked.lock().unwrap()[0].filter.as_ref().unwrap().1, &["mp4"]);
        assert_eq!((r["codec"].as_str(), r["height"].as_u64()), (Some("av1"), Some(64)));
        assert!(r["sawRow"].as_bool().unwrap(), "the export showed in the activity stack");
        assert!(std::path::Path::new(&path).is_file());
        // JPEG: a save dialog names the folder (no file type)
        let (mut app, asked) = app_answering(dir.join("Frames").to_string_lossy().to_string());
        app.run("app.slideshowExport", json!({"format": "jpeg", "size": 64})).unwrap();
        assert!(asked.lock().unwrap()[0].filter.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
