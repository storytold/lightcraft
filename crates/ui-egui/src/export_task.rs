//! Background export: the Export dialog, File → Export with Preset and Export with Previous hand
//! their batch to a worker thread so the window stays responsive. Photos are prepared on the UI
//! thread ([`lightcraft_engine::export::prepare_export`]: cheap, needs the session) and rendered,
//! encoded and written on the worker (several side by side: [`run_batch`]); a progress panel shows
//! the count and a Cancel button.
//!
//! Needs [`crate::Services::write_shared`] (a thread-safe writer); without it (web) the batch runs
//! synchronously as before.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};

use egui::Align2;
use lightcraft_engine::export::{Destination, ExportOptions, PreparedExport, run_batch};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::theme::Tokens;

pub struct ExportTask {
    pub total: usize,
    /// Photos started so far and the file in progress.
    pub progress: Arc<Mutex<(usize, String)>>,
    pub cancel: Arc<AtomicBool>,
    rx: Receiver<Result<Vec<Value>, String>>,
}

impl ExportTask {
    /// `{total, done, current}` for `ui.inspect`.
    pub fn status(&self) -> Value {
        let (done, current) = self.progress.lock().map(|g| g.clone()).unwrap_or_default();
        json!({"total": self.total, "done": done, "current": current})
    }
}

/// Start exporting `items` in the background. Errors per photo are collected, not fatal.
pub fn start(app: &mut LightcraftApp, items: Vec<PreparedExport>, opts: ExportOptions, to: Destination) -> Result<Value, String> {
    if app.export.is_some() {
        return Err("an export is already running".into());
    }
    let write = app.services.write_shared.clone().ok_or("no background writer")?;
    let total = items.len();
    let progress = Arc::new(Mutex::new((0, String::new())));
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = channel();
    let (p, c) = (progress.clone(), cancel.clone());
    let work = move || {
        let mut w = |path: &str, bytes: &[u8]| write(path, bytes);
        let r = run_batch(items, &opts, &to, &mut w, &|path| std::path::Path::new(path).exists(), false, &mut |done, name| {
            if let Ok(mut g) = p.lock() {
                *g = (done, name.to_string());
            }
            !c.load(Ordering::Relaxed)
        });
        let _ = tx.send(r);
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::Builder::new().name("export".into()).spawn(work).map_err(|e| e.to_string())?;
    #[cfg(target_arch = "wasm32")]
    work();
    app.export = Some(ExportTask { total, progress, cancel, rx });
    Ok(json!({"background": true, "total": total}))
}

/// Export a single paginated PDF using the same progress and cancellation UI as image exports.
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
        let _ = tx.send(result);
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::Builder::new().name("contact-sheet".into()).spawn(work).map_err(|e| e.to_string())?;
    #[cfg(target_arch = "wasm32")]
    work();
    app.export = Some(ExportTask { total, progress, cancel, rx });
    Ok(json!({"background": true, "total": total}))
}

/// Per frame: draw the progress panel; when the batch finishes, report it.
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(task) = &app.export else { return };
    match task.rx.try_recv() {
        Ok(r) => {
            let cancelled = task.cancel.load(Ordering::Relaxed);
            let total = task.total;
            app.export = None;
            let msg = match r {
                Ok(files) => {
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
                    app.last_export_result = Some(json!({"files": files, "cancelled": cancelled}));
                    m
                }
                Err(e) => e,
            };
            app.toast(ctx, msg);
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            let (done, current) = task.progress.lock().map(|g| g.clone()).unwrap_or_default();
            let (total, cancel) = (task.total, task.cancel.clone());
            let t = Tokens::get(ctx);
            egui::Window::new(crate::i18n::tr("Exporting"))
                .title_bar(false)
                .resizable(false)
                .anchor(Align2::LEFT_BOTTOM, [16.0, -56.0])
                .fixed_size([300.0, 64.0])
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(crate::i18n::tr_format!("Exporting {} of {total}", (done + 1).min(total), total = total))
                                .color(t.text),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let stopping = cancel.load(Ordering::Relaxed);
                            if crate::widgets::text_button(ui, "exportCancel", if stopping { "Stopping…" } else { "Cancel" }, false).clicked() {
                                cancel.store(true, Ordering::Relaxed);
                            }
                        });
                    });
                    ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).desired_width(280.0));
                    ui.label(egui::RichText::new(current).size(11.0).color(t.text_dim));
                });
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
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
            // The progress window lays out text, which egui only allows inside a frame.
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
