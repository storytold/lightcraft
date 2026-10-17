//! File ▸ Export ▸ Timelapse Video… (upstream #497): the dialog's commands and the background
//! export. The dialog is `Dialog::Timelapse` (drawn in `panels::dialogs`); confirming it asks for
//! a file and runs `lcc_engine::timelapse` on a worker with an activity row whose ✕ stops it.
//!
//! Sources: own design.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::LccApp;
use crate::pick::{PickRequest, Picked};
use crate::state::Dialog;

/// UI commands: (id, label, shortcut, menu).
#[cfg(not(target_arch = "wasm32"))]
pub const COMMANDS: &[crate::menus::UiCommand] = &[
    ("dialog.timelapse", "Timelapse Video…", None, "File>Export"),
    // {path?, …export.timelapse options}: asks for the file when `path` is missing
    ("app.timelapse", "Export Timelapse Video", None, ""),
];
#[cfg(target_arch = "wasm32")]
pub const COMMANDS: &[crate::menus::UiCommand] = &[];

/// Frame sizes the dialog offers.
pub const SIZES: &[(&str, &str)] = &[
    ("3840x2160", "4K UHD 3840×2160"),
    ("1920x1080", "1080p 1920×1080"),
    ("1280x720", "720p 1280×720"),
    ("1080x1080", "Square 1080×1080"),
    ("1080x1920", "Vertical 1080×1920"),
];

/// The dialog's state, default first.
pub fn new_dialog() -> Dialog {
    Dialog::Timelapse { fps: 24, size: "1920x1080".into(), fill: false, hold: 1, crossfade: 0, prores: false, quality: 80 }
}

pub fn enabled(app: &LccApp, id: &str) -> Option<bool> {
    COMMANDS.iter().any(|c| c.0 == id).then(|| app.session.active().is_some() && app.export.is_none())
}

pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    Some(match id {
        "dialog.timelapse" if COMMANDS.iter().any(|c| c.0 == id) => {
            app.ui.dialog = Some(new_dialog());
            Ok(Value::Null)
        }
        "app.timelapse" if COMMANDS.iter().any(|c| c.0 == id) => {
            if app.export.is_some() {
                return Some(Err("an export is already running".into()));
            }
            let mut params = p.clone();
            if p.get("path").is_none() {
                let prores = p.get("format").and_then(Value::as_str) == Some("prores");
                let (filter, ext, name): (&str, &'static [&'static str], &str) =
                    if prores { ("QuickTime", &["mov"], "Timelapse.mov") } else { ("MPEG-4", &["mp4"], "Timelapse.mp4") };
                let req = PickRequest::save(crate::i18n::tr("Timelapse Video"), filter, ext, name);
                let paths = match crate::pick::ask(app, id, p, "path", req, |_| None) {
                    Picked::Now(paths) => paths,
                    Picked::Later => return Some(Ok(Value::Null)),
                    Picked::Unavailable => return Some(Err("no save dialog on this platform; supply path".into())),
                };
                let Some(path) = paths.first() else { return Some(Ok(Value::Null)) };
                params["path"] = json!(path);
            }
            start(app, &params)
        }
        _ => return None,
    })
}

/// The dialog's `export.timelapse` options.
pub fn dialog_params(d: &Dialog) -> Option<Value> {
    let Dialog::Timelapse { fps, size, fill, hold, crossfade, prores, quality } = d else { return None };
    Some(json!({
        "fps": fps, "size": size, "fit": if *fill { "fill" } else { "fit" }, "hold": hold, "crossfade": crossfade,
        "format": if *prores { "prores" } else { "h264" }, "quality": quality,
    }))
}

/// Render and encode on a worker; the export's row in the activity stack shows the photo count.
pub fn start(app: &mut LccApp, params: &Value) -> Result<Value, String> {
    if app.export.is_some() {
        return Err("an export is already running".into());
    }
    let prepared = lcc_engine::timelapse::prepare(&mut app.session, params)?;
    let total = prepared.len();
    let progress = Arc::new(Mutex::new((0, String::new())));
    let cancel = Arc::new(AtomicBool::new(false));
    let (p, c) = (progress.clone(), cancel.clone());
    let (tx, rx) = channel();
    let work = move || {
        let r = prepared.run(&c, &mut |done, name| {
            if let Ok(mut g) = p.lock() {
                *g = (done, name.to_string());
            }
        });
        let _ = tx.send(r.map(|rep| (vec![rep.json()], None, c.load(Ordering::Relaxed))));
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::Builder::new().name("timelapse".into()).spawn(work).map_err(|e| e.to_string())?;
    #[cfg(target_arch = "wasm32")]
    work();
    let row = app.session.activity.start("export", "Exporting timelapse", lcc_engine::activity::Cancel::Flag(cancel.clone()));
    row.progress(0, total as u64);
    app.export = Some(crate::export_task::ExportTask::new(total, progress, cancel, rx, row));
    Ok(json!({"background": true, "total": total}))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn menu_dialog_and_background_timelapse() {
        let dir = std::env::temp_dir().join(format!("lcc-ui-timelapse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("t.mp4");
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), crate::Services::default());
        assert!(crate::menus::menu_entries(&app).iter().any(|e| e.id == "dialog.timelapse" && e.menu == ["File", "Export"]));
        let ids: Vec<u64> = app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
        app.run("dialog.timelapse", json!({})).unwrap();
        let Some(Dialog::Timelapse { size, hold, .. }) = app.ui.dialog.as_mut() else { panic!("no timelapse dialog") };
        *size = "64x48".into();
        *hold = 2;
        let mut params = dialog_params(app.ui.dialog.as_ref().unwrap()).unwrap();
        params["path"] = json!(out);
        params["ids"] = json!(ids);
        app.ui.dialog = None;
        app.run("app.timelapse", params).unwrap();
        let rows = app.session.activity.list();
        assert_eq!((rows.len(), rows[0].label.as_str(), rows[0].total), (1, "Exporting timelapse", 3));
        let mut h = crate::headless::Headless::new(app, [1000.0, 800.0], 1.0);
        let deadline = Instant::now() + Duration::from_secs(60);
        while h.app.last_export_result.is_none() {
            h.step();
            assert!(Instant::now() < deadline, "timelapse did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
        let f = &h.app.last_export_result.as_ref().unwrap()["files"][0];
        assert_eq!(f["frames"], 6, "{f}");
        assert!(out.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
