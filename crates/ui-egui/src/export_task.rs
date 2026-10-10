//! Background export: the Export dialog, File → Export with Preset and Export with Previous hand
//! their batch to a worker thread so the window stays responsive. Photos are prepared on the UI
//! thread ([`lightcraft_engine::export::prepare_export`]: cheap, needs the session) and rendered,
//! encoded and written on the worker (several side by side: [`run_batch`]); its row in the
//! activity stack shows the count, the file in progress and ✕ (issue #345).
//!
//! Needs [`crate::Services::write_shared`] (a thread-safe writer); without it (web) the batch runs
//! synchronously as before.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use lightcraft_engine::activity::{Cancel, TaskGuard};
use lightcraft_engine::export::{AfterExport, Destination, ExportOptions, PreparedExport, run_batch};
use serde_json::{Value, json};

use crate::LightcraftApp;

pub struct ExportTask {
    pub total: usize,
    /// Photos started so far and the file in progress.
    pub progress: Arc<Mutex<(usize, String)>>,
    pub cancel: Arc<AtomicBool>,
    /// The files (per-photo results) and what [`AfterExport`] did with them (`applePhotos`).
    rx: Receiver<Result<(Vec<Value>, Option<Value>), String>>,
    /// The export's row in the activity stack (`activity.cancel` sets `cancel`).
    guard: TaskGuard,
}

impl ExportTask {
    /// `{total, done, current}` for `ui.inspect`.
    pub fn status(&self) -> Value {
        let (done, current) = self.progress.lock().map(|g| g.clone()).unwrap_or_default();
        json!({"total": self.total, "done": done, "current": current})
    }
}

/// Start exporting `items` in the background, then do `after` with the files written (Add to Apple
/// Photos). Errors per photo are collected, not fatal.
pub fn start(app: &mut LightcraftApp, items: Vec<PreparedExport>, opts: ExportOptions, to: Destination, after: AfterExport) -> Result<Value, String> {
    if app.export.is_some() {
        return Err("an export is already running".into());
    }
    let write = app.services.write_shared.clone().ok_or("no background writer")?;
    let total = items.len();
    let progress = Arc::new(Mutex::new((0, String::new())));
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = channel();
    let (p, c) = (progress.clone(), cancel.clone());
    // translated here: the language is the UI thread's, and the worker would show English
    let adding = crate::i18n::tr("Adding to Apple Photos…").to_string();
    let work = move || {
        let mut w = |path: &str, bytes: &[u8]| write(path, bytes);
        let r = run_batch(items, &opts, &to, &mut w, &|path| std::path::Path::new(path).exists(), false, &mut |done, name| {
            if let Ok(mut g) = p.lock() {
                *g = (done, name.to_string());
            }
            !c.load(Ordering::Relaxed)
        });
        let r = r.map(|files| {
            if !after.is_none()
                && let Ok(mut g) = p.lock()
            {
                g.1 = adding;
            }
            let photos = after.run(&files, c.load(Ordering::Relaxed));
            (files, photos)
        });
        let _ = tx.send(r);
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::Builder::new().name("export".into()).spawn(work).map_err(|e| e.to_string())?;
    #[cfg(target_arch = "wasm32")]
    work();
    let guard = app.session.activity.start("export", "Exporting", Cancel::Flag(cancel.clone()));
    guard.progress(0, total as u64);
    app.export = Some(ExportTask { total, progress, cancel, rx, guard });
    Ok(json!({"background": true, "total": total}))
}

/// Export a single paginated PDF with the same activity row and cancellation as image exports.
pub fn start_contact_sheet(app: &mut LightcraftApp, params: &Value) -> Result<Value, String> {
    if app.export.is_some() {
        return Err("an export is already running".into());
    }
    let path = params.get("path").and_then(Value::as_str).filter(|s| !s.trim().is_empty()).ok_or("missing path")?.to_string();
    app.session.check_write_target(&path)?;
    let guard = app.session.original_guard();
    let prepared = lightcraft_engine::contact_sheet::prepare(&mut app.session, params)?;
    let total = prepared.len();
    let write = app.services.write_shared.clone();
    let progress = Arc::new(Mutex::new((0, String::new())));
    let cancel = Arc::new(AtomicBool::new(false));
    let (p, c) = (progress.clone(), cancel.clone());
    let (tx, rx) = channel();
    let work = move || {
        let result = prepared
            .run(&mut |done, name| {
                if let Ok(mut g) = p.lock() {
                    *g = (done, name.to_string());
                }
                !c.load(Ordering::Relaxed)
            })
            .and_then(|doc| {
                if c.load(Ordering::Relaxed) {
                    return Err("Contact sheet cancelled".into());
                }
                guard.check(std::path::Path::new(&path))?;
                match write {
                    Some(write) => write(&path, &doc.bytes)?,
                    None => lightcraft_engine::export::write_file(&path, &doc.bytes)?,
                }
                Ok(vec![json!({"path": path, "pages": doc.pages, "photos": doc.photos, "bytes": doc.bytes.len(), "contactSheet": true})])
            });
        // a PDF isn't something Photos imports: contact sheets never add to Apple Photos
        let _ = tx.send(result.map(|files| (files, None)));
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::Builder::new().name("contact-sheet".into()).spawn(work).map_err(|e| e.to_string())?;
    #[cfg(target_arch = "wasm32")]
    work();
    let row = app.session.activity.start("export", "Exporting contact sheet", Cancel::Flag(cancel.clone()));
    row.progress(0, total as u64);
    app.export = Some(ExportTask { total, progress, cancel, rx, guard: row });
    Ok(json!({"background": true, "total": total}))
}

/// The toast's account of [`AfterExport::run`]'s Apple Photos outcome.
pub fn apple_photos_note(outcome: &Value) -> String {
    if let Some(e) = outcome.get("error").and_then(Value::as_str) {
        return crate::i18n::tr_format!("not added to Apple Photos: {e}", e = e);
    }
    if let Some(why) = outcome.get("skipped").and_then(Value::as_str) {
        if why.contains("cancelled") {
            return crate::i18n::tr("not added to Apple Photos (cancelled)").to_string();
        }
        return crate::i18n::tr_format!("not added to Apple Photos: {e}", e = why);
    }
    let n = outcome.get("imported").and_then(Value::as_u64).unwrap_or(0);
    let mut m = match outcome.get("album").and_then(Value::as_str) {
        Some(album) => crate::i18n::tr_format!("{n} added to Apple Photos album “{album}”", n = n, album = album),
        None => crate::i18n::tr_format!("{n} added to Apple Photos", n = n),
    };
    if let Some(w) = outcome.get("warning").and_then(Value::as_str) {
        m += &format!(" ({w})");
    }
    m
}

/// Per frame: when an Apple Photos import that no export task reports finishes (`app.export`
/// without `background`, `export.addToPhotos`), say how it went.
#[cfg(not(target_arch = "wasm32"))]
pub fn poll_photos(app: &mut LightcraftApp, ctx: &egui::Context) {
    let jobs = app.session.apple_photos_imports.all();
    if jobs.iter().any(|j| !j.finished()) {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
    for job in jobs {
        if job.take_announcement() {
            let outcome = job.json();
            let msg = apple_photos_note(&outcome);
            if outcome.get("error").is_some() {
                app.toast_for(ctx, msg, 15.0);
            } else {
                app.toast(ctx, msg);
            }
        }
    }
}

/// No Apple Photos on the web.
#[cfg(target_arch = "wasm32")]
pub fn poll_photos(_: &mut LightcraftApp, _: &egui::Context) {}

/// Per frame: keep the activity row up to date; when the batch finishes, report it.
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(task) = &app.export else { return };
    match task.rx.try_recv() {
        Ok(r) => {
            let cancelled = task.cancel.load(Ordering::Relaxed);
            let total = task.total;
            app.export = None;
            // Photos refused (permission, not installed…): leave the way out on screen longer
            let photos_failed = matches!(&r, Ok((_, Some(p))) if p.get("error").is_some());
            let msg = match r {
                Ok((files, photos)) => {
                    let sheet = files.first().filter(|f| f["contactSheet"] == true);
                    let ok = files.iter().filter(|f| f.get("path").is_some()).count();
                    let failed: Vec<&Value> = files.iter().filter(|f| f.get("error").is_some()).collect();
                    let skipped = files.iter().filter(|f| f.get("skipped").is_some()).count();
                    let mut m =
                        crate::i18n::tr_format!("Exported {ok} of {total} photo{}", if total == 1 { "" } else { "s" }, ok = ok, total = total);
                    if let Some(sheet) = sheet {
                        m = format!("Exported contact sheet: {} photos, {} pages", sheet["photos"], sheet["pages"]);
                    }
                    if skipped > 0 {
                        m += &crate::i18n::tr_format!(" · {skipped} skipped (file exists)", skipped = skipped);
                    }
                    if let Some(first) = failed.first() {
                        m += &crate::i18n::tr_format!(" · {} failed: {}", failed.len(), first["error"].as_str().unwrap_or("error"));
                    }
                    if cancelled {
                        m += " · cancelled";
                    }
                    if let Some(photos) = &photos {
                        m += " · ";
                        m += &apple_photos_note(photos);
                    }
                    app.last_export_result = Some(json!({"files": files, "cancelled": cancelled, "applePhotos": photos}));
                    m
                }
                Err(e) => e,
            };
            if photos_failed {
                app.toast_for(ctx, msg, 15.0);
            } else {
                app.toast(ctx, msg);
            }
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            let (done, current) = task.progress.lock().map(|g| g.clone()).unwrap_or_default();
            task.guard.progress(done as u64, task.total as u64);
            task.guard.detail(&current);
        }
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            app.export = None;
            app.toast(ctx, crate::i18n::tr("Export stopped unexpectedly"));
        }
    }
}

#[cfg(test)]
mod contact_sheet_tests {
    use super::*;
    use crate::pick::{PickKind, PickRequest};
    use crate::state::Dialog;
    use std::time::{Duration, Instant};

    #[test]
    fn contact_sheet_menu_dialog_picker_and_background_export() {
        let asked = Arc::new(Mutex::new(Vec::<PickRequest>::new()));
        let requests = asked.clone();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let written = bytes.clone();
        let services = crate::Services {
            picker: Some(Box::new(move |req| {
                requests.lock().unwrap().push(req);
                let (tx, rx) = channel();
                tx.send(vec!["Contact Sheet.pdf".into()]).unwrap();
                Ok(rx)
            })),
            write_shared: Some(Arc::new(move |path, data| {
                assert_eq!(path, "Contact Sheet.pdf");
                *written.lock().unwrap() = data.to_vec();
                Ok(())
            })),
            ..Default::default()
        };
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
        app.run("dialog.contactSheet", json!({})).unwrap();
        let Some(Dialog::ContactSheet { options }) = app.ui.dialog.as_mut() else { panic!("no contact sheet settings") };
        options.paper = "letter".into();
        options.landscape = true;
        let mut h = crate::headless::Headless::new(app, [1000.0, 800.0], 1.0);
        let dialog = h.app.ui.dialog.take().unwrap();
        crate::panels::dialogs::confirm_dialog(&mut h.app, &dialog).unwrap();
        assert_eq!(h.app.pending_picks.len(), 1);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            // The activity stack lays out text, which egui only allows inside a frame.
            h.step();
            if h.app.last_export_result.is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "export did not finish");
            std::thread::sleep(Duration::from_millis(10));
        }
        let req = asked.lock().unwrap();
        assert_eq!(req[0].kind, PickKind::Save);
        assert_eq!(req[0].filter.as_ref().unwrap().1, &["pdf"]);
        assert_eq!(req[0].file_name.as_deref(), Some("Contact Sheet.pdf"));
        let b = bytes.lock().unwrap();
        assert!(b.starts_with(b"%PDF-1.4"));
        assert!(String::from_utf8_lossy(&b).contains("/MediaBox [0 0 792.00 612.00]"));
        assert_eq!(h.app.last_export_result.as_ref().unwrap()["files"][0]["photos"], 1);
    }

    #[test]
    fn contact_sheet_export_shows_a_row_and_its_cross_stops_it() {
        let mut app = LightcraftApp::new(
            lightcraft_engine::Session::with_demo(),
            crate::Services { write_shared: Some(Arc::new(|_, _| Ok(()))), ..Default::default() },
        );
        let ids: Vec<_> = app.session.catalog.photos().take(24).map(|p| p.id.0).collect();
        start_contact_sheet(&mut app, &json!({"path": "Sheet.pdf", "ids": ids})).unwrap();
        let rows = app.session.activity.list();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].total), ("export", "Exporting contact sheet", 24));
        assert!(rows[0].cancellable);
        app.session.activity.cancel(rows[0].id).unwrap();
        assert!(app.export.as_ref().unwrap().cancel.load(Ordering::Relaxed), "the stack's ✕ stops the sheet");
        let task = app.export.take().unwrap();
        assert!(task.rx.recv_timeout(Duration::from_secs(20)).unwrap().is_err(), "a stopped sheet is not written");
        drop(task);
        assert!(app.session.activity.list().is_empty(), "the row goes with the export");
    }

    #[test]
    fn cancelled_contact_sheet_never_calls_writer() {
        let mut app = LightcraftApp::new(
            lightcraft_engine::Session::with_demo(),
            crate::Services { write_shared: Some(Arc::new(|_, _| panic!("cancelled export wrote a file"))), ..Default::default() },
        );
        let ids: Vec<_> = app.session.catalog.photos().take(24).map(|p| p.id.0).collect();
        start_contact_sheet(&mut app, &json!({"path": "Cancelled.pdf", "ids": ids})).unwrap();
        app.export.as_ref().unwrap().cancel.store(true, Ordering::Relaxed);
        let task = app.export.take().unwrap();
        assert!(task.rx.recv_timeout(Duration::from_secs(20)).unwrap().is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The toast's Apple Photos part (issue #236): the count and album, a partial import's
    /// warning, an error with what to do, a cancelled export.
    #[test]
    fn apple_photos_outcomes_read_well() {
        assert_eq!(apple_photos_note(&json!({"imported": 3, "requested": 3, "ids": [], "album": null})), "3 added to Apple Photos");
        assert_eq!(apple_photos_note(&json!({"imported": 1, "album": "Trip"})), "1 added to Apple Photos album “Trip”");
        let partial = apple_photos_note(&json!({"imported": 1, "requested": 2, "warning": "Photos added 1 of 2 files"}));
        assert_eq!(partial, "1 added to Apple Photos (Photos added 1 of 2 files)");
        let denied = apple_photos_note(
            &json!({"error": "LightCraft isn't allowed to control Photos. Allow it in System Settings › Privacy & Security › Automation"}),
        );
        assert!(denied.starts_with("not added to Apple Photos: ") && denied.contains("Automation"), "{denied}");
        assert_eq!(apple_photos_note(&json!({"skipped": "the export was cancelled"})), "not added to Apple Photos (cancelled)");
        assert_eq!(apple_photos_note(&json!({"skipped": "the export wrote no files"})), "not added to Apple Photos: the export wrote no files");
    }
}
