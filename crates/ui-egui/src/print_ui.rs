//! The Print module: template browser (left), page canvas with rulers and guides (centre),
//! layout, image, page and print-job panels (right), filmstrip (bottom).
//!
//! The settings are a [`dac_print::PrintSettings`]; the pages come from
//! [`dac_print::PrintSettings::build`] over the selected photos (all photos in the filmstrip when
//! one or none is selected… as Print One prints just the active photo). The canvas draws the same
//! layout from thumbnails; Print renders every photo with the export pipeline at the print
//! resolution on a worker thread and writes a PDF or JPEG files, or sends the PDF to an IPP/CUPS
//! printer.
//!
//! Commands (`printui.*`, listed with the module shell's): `printui.template {name}`,
//! `printui.set {settings}` (merged into the current settings), `printui.pageSetup {paper,
//! landscape}`, `printui.page {page | delta}`, `printui.guides {...}`, `printui.print {destination,
//! path, printer}`, `printui.printOne`, `printui.printers {server}`, `printui.saveTemplate {name}`,
//! `printui.state`, `printui.saveCreation {name}` (a Saved Print: a collection holding the photos,
//! the page layout and these settings), `printui.openCreation {id}`.

use dac_catalog::PhotoId;
use dac_layout::tokens::{PhotoInfo, expand};
use dac_layout::{Align, CellKind, Document, Fit};
use dac_print::job::{PAPERS, builtin_templates};
use dac_print::{Destination, LayoutStyle, PrintSettings};
use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::{Value, json};

use crate::DacApp;
use crate::module::{Edge, Module, ModuleId, ModuleKey, PanelId};
use crate::theme::Tokens;
use crate::widgets::register;

/// Most photos one print job takes from the filmstrip.
const MAX_JOB_PHOTOS: usize = dac_print::job::MAX_PHOTOS;
const LEFT_W: f32 = 220.0;
const RIGHT_W: f32 = 320.0;
const RULER: f32 = 18.0;

/// The module's state (lives in [`DacApp::print`]).
#[derive(Clone, Debug)]
pub struct PrintUi {
    pub settings: PrintSettings,
    /// The template last applied.
    pub template: Option<String>,
    /// User templates (name, settings), saved with `printui.saveTemplate`.
    pub user_templates: Vec<(String, PrintSettings)>,
    /// Preview page (0-based).
    pub page: usize,
    pub rulers: bool,
    pub margins: bool,
    pub cells: bool,
    pub bleed: bool,
    pub dimensions: bool,
    pub printer_uri: String,
    pub cups_server: String,
    pub printers: Vec<dac_print::ipp::PrinterEntry>,
    pub output_path: String,
    pub busy: bool,
    pub last: Option<Value>,
    /// The Saved Print (collection) last saved or opened.
    pub creation: Option<u64>,
    /// Paper sizes the chosen printer reports (IPP `media-col-database` / `media-supported`).
    pub media: Vec<dac_print::ipp::Media>,
    /// `<config>/print.json` has been read.
    pub loaded: bool,
    /// What was last written to (or read from) `print.json`, to save only on change.
    pub stored: Option<Value>,
}

impl Default for PrintUi {
    fn default() -> Self {
        let (name, settings) = builtin_templates().into_iter().next().unwrap_or_else(|| ("Default".into(), PrintSettings::default()));
        PrintUi {
            settings,
            template: Some(name),
            user_templates: Vec::new(),
            page: 0,
            rulers: true,
            margins: true,
            cells: true,
            bleed: true,
            dimensions: false,
            printer_uri: String::new(),
            cups_server: "localhost:631".into(),
            printers: Vec::new(),
            output_path: String::new(),
            busy: false,
            last: None,
            creation: None,
            media: Vec::new(),
            loaded: false,
            stored: None,
        }
    }
}

/// UI commands: (id, label, shortcut, menu).
pub const COMMANDS: &[crate::menus::UiCommand] = &[
    ("printui.template", "Apply Print Template", None, ""),
    ("printui.set", "Print Settings", None, ""),
    ("printui.pageSetup", "Page Setup", None, ""),
    ("printui.page", "Print Preview Page", None, ""),
    ("printui.guides", "Print Guides", None, ""),
    ("printui.print", "Print", None, ""),
    ("printui.printOne", "Print One", None, ""),
    ("printui.printers", "Find Printers", None, ""),
    ("printui.saveTemplate", "New Print Template", None, ""),
    ("printui.deleteTemplate", "Delete Print Template", None, ""),
    ("printui.media", "Printer Paper Sizes", None, ""),
    ("printui.state", "Print State", None, ""),
    ("printui.saveCreation", "Create Saved Print", None, ""),
    ("printui.openCreation", "Open Saved Print", None, ""),
];

/// The photos a print takes: the selection when it has more than one photo, else every photo in
/// the filmstrip (Print One: the active photo).
pub fn job_photos(app: &mut DacApp, one: bool) -> Vec<PhotoId> {
    if one {
        return app.session.active().into_iter().collect();
    }
    let sel = app.session.selection.ids.clone();
    if sel.len() > 1 {
        return sel.into_iter().take(MAX_JOB_PHOTOS).collect();
    }
    app.session.visible().iter().copied().take(MAX_JOB_PHOTOS).collect()
}

fn ids_text(ids: &[PhotoId]) -> Vec<String> {
    ids.iter().map(|i| i.0.to_string()).collect()
}

/// Token values of a catalog photo.
fn info_of(app: &DacApp, id: &str) -> PhotoInfo {
    id.parse::<u64>().ok().and_then(|n| app.session.catalog.photo(PhotoId(n))).map(|p| dac_engine::creations::photo_info(p)).unwrap_or_default()
}

/// The document for the current settings and photos.
fn document(app: &mut DacApp) -> Result<(Document, Vec<PhotoId>), String> {
    let ids = job_photos(app, false);
    let doc = app.print.settings.build(&ids_text(&ids)).map_err(|e| e.to_string())?;
    Ok((doc, ids))
}

/// Deep merge of `b` into `a`.
fn merge(a: &mut Value, b: &Value) {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(k) {
                    Some(x) if x.is_object() && v.is_object() => merge(x, v),
                    _ => {
                        a.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (a, b) => *a = b.clone(),
    }
}

/// Is `id` one of ours, and is it enabled?
pub fn enabled(app: &DacApp, id: &str) -> Option<bool> {
    if !COMMANDS.iter().any(|c| c.0 == id) {
        return None;
    }
    Some(match id {
        "printui.print" | "printui.printOne" => !app.print.busy && !app.session.catalog.is_empty(),
        _ => true,
    })
}

/// Run one of our commands; `None`: not one.
pub fn run(app: &mut DacApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if !COMMANDS.iter().any(|c| c.0 == id) {
        return None;
    }
    ensure_loaded(app);
    let r = run_inner(app, id, p);
    if r.is_ok()
        && let Err(e) = persist(app)
    {
        log::warn!("print settings not saved: {e}");
    }
    Some(r)
}

// ---------------------------------------------------------------- persistence

/// `<config>/print.json`: the user templates, the current settings and template, the printer.
fn store_path(app: &DacApp) -> Option<std::path::PathBuf> {
    app.session.remote.connections_path.as_ref().and_then(|p| p.parent()).map(|d| d.join("print.json"))
}

fn stored_json(pu: &PrintUi) -> Value {
    json!({
        "settings": serde_json::to_value(&pu.settings).unwrap_or(Value::Null),
        "template": pu.template,
        "userTemplates": pu.user_templates.iter().map(|(n, s)| json!({"name": n, "settings": serde_json::to_value(s).unwrap_or(Value::Null)})).collect::<Vec<_>>(),
        "printerUri": pu.printer_uri,
        "cupsServer": pu.cups_server,
    })
}

/// Apply a stored `print.json`; anything unreadable is skipped (a damaged file never stops the module).
fn apply_stored(pu: &mut PrintUi, v: &Value) {
    let settings = |x: &Value| PrintSettings::from_json(&x.to_string()).ok().filter(|s| s.validate().is_ok());
    if let Some(s) = v.get("settings").and_then(settings) {
        pu.settings = s;
    }
    if let Some(t) = v.get("template") {
        pu.template = t.as_str().map(str::to_string);
    }
    if let Some(list) = v.get("userTemplates").and_then(Value::as_array) {
        pu.user_templates = list
            .iter()
            .filter_map(|t| Some((t.get("name")?.as_str()?.trim().to_string(), settings(t.get("settings")?)?)))
            .filter(|(n, _)| !n.is_empty())
            .take(500)
            .collect();
    }
    if let Some(u) = v.get("printerUri").and_then(Value::as_str) {
        pu.printer_uri = u.to_string();
    }
    if let Some(u) = v.get("cupsServer").and_then(Value::as_str).filter(|u| !u.trim().is_empty()) {
        pu.cups_server = u.to_string();
    }
}

/// Read `print.json` once per session.
pub fn ensure_loaded(app: &mut DacApp) {
    if app.print.loaded {
        return;
    }
    app.print.loaded = true;
    let Some(path) = store_path(app) else { return };
    match std::fs::read(&path) {
        Ok(b) => match serde_json::from_slice::<Value>(&b) {
            Ok(v) => apply_stored(&mut app.print, &v),
            Err(e) => log::warn!("{}: {e}; using the default print settings", path.display()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("{}: {e}", path.display()),
    }
    app.print.stored = Some(stored_json(&app.print));
}

/// Write `print.json` when the settings, templates or printer changed since the last write.
pub fn persist(app: &mut DacApp) -> Result<(), String> {
    let now = stored_json(&app.print);
    if app.print.stored.as_ref() == Some(&now) {
        return Ok(());
    }
    let Some(path) = store_path(app) else { return Ok(()) };
    let bytes = serde_json::to_vec_pretty(&now).map_err(|e| e.to_string())?;
    app.print.stored = Some(now);
    dac_engine::export::write_file_durable(&path.to_string_lossy(), &bytes)
}

fn state_json(app: &mut DacApp) -> Value {
    let (pages, photos, err) = match document(app) {
        Ok((d, ids)) => (d.pages.len(), ids.len(), None),
        Err(e) => (0, 0, Some(e)),
    };
    let pu = &app.print;
    json!({
        "template": pu.template,
        "settings": serde_json::to_value(&pu.settings).unwrap_or(Value::Null),
        "page": pu.page + 1,
        "pages": pages,
        "photos": photos,
        "error": err,
        "guides": {"rulers": pu.rulers, "margins": pu.margins, "cells": pu.cells, "bleed": pu.bleed, "dimensions": pu.dimensions},
        "printer": pu.printer_uri,
        "printers": pu.printers.iter().map(|p| json!({"name": p.name, "uri": p.uri, "info": p.info})).collect::<Vec<_>>(),
        "busy": pu.busy,
        "creation": pu.creation,
        "last": pu.last,
        "templates": builtin_templates().into_iter().map(|t| t.0).chain(pu.user_templates.iter().map(|t| t.0.clone())).collect::<Vec<_>>(),
        "userTemplates": pu.user_templates.iter().map(|t| t.0.clone()).collect::<Vec<_>>(),
        "media": pu.media.iter().map(|m| json!({"name": m.name, "w": m.size.w, "h": m.size.h, "margins": m.margins})).collect::<Vec<_>>(),
    })
}

fn run_inner(app: &mut DacApp, id: &str, p: &Value) -> Result<Value, String> {
    match id {
        "printui.template" => {
            let name = p.get("name").and_then(Value::as_str).ok_or("missing name")?;
            let found = builtin_templates().into_iter().chain(app.print.user_templates.iter().cloned()).find(|(n, _)| n == name);
            let (n, s) = found.ok_or_else(|| format!("no print template named {name}"))?;
            app.print.settings = s;
            app.print.template = Some(n);
            app.print.page = 0;
        }
        "printui.set" => {
            let patch = p.get("settings").unwrap_or(p);
            let mut cur = serde_json::to_value(&app.print.settings).map_err(|e| e.to_string())?;
            merge(&mut cur, patch);
            let s = PrintSettings::from_json(&cur.to_string()).map_err(|e| e.to_string())?;
            app.print.settings = s;
        }
        "printui.pageSetup" => {
            let cur = app.print.settings.page.size;
            let landscape = p.get("landscape").and_then(Value::as_bool).unwrap_or(cur.w > cur.h);
            let size = match (p.get("paper").and_then(Value::as_str), p.get("media").and_then(Value::as_str)) {
                (Some(k), _) => dac_print::job::paper(k).ok_or_else(|| format!("unknown paper {k}"))?.size,
                (None, Some(m)) => app
                    .print
                    .media
                    .iter()
                    .find(|x| x.name == m)
                    .map(|x| x.size)
                    .or_else(|| dac_print::ipp::pwg_media_size(m))
                    .ok_or_else(|| format!("unknown printer paper {m}"))?,
                (None, None) => cur,
            };
            let s = &mut app.print.settings;
            s.set_paper(size, landscape);
            // grids and packages adapt; custom cells keep their places
            s.validate().map_err(|e| e.to_string())?;
        }
        "printui.page" => {
            let n = document(app).map(|d| d.0.pages.len()).unwrap_or(1).max(1);
            let want = match (p.get("page").and_then(Value::as_u64), p.get("delta").and_then(Value::as_i64)) {
                (Some(pg), _) => (pg as usize).saturating_sub(1),
                (None, Some(d)) => (app.print.page as i64 + d).clamp(0, n as i64 - 1) as usize,
                _ => app.print.page,
            };
            app.print.page = want.min(n - 1);
        }
        "printui.guides" => {
            let pu = &mut app.print;
            for (k, f) in [
                ("rulers", &mut pu.rulers),
                ("margins", &mut pu.margins),
                ("cells", &mut pu.cells),
                ("bleed", &mut pu.bleed),
                ("dimensions", &mut pu.dimensions),
            ] {
                if let Some(v) = p.get(k).and_then(Value::as_bool) {
                    *f = v;
                }
            }
        }
        "printui.saveTemplate" => {
            let name = p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).ok_or("missing name")?.to_string();
            let s = app.print.settings.clone();
            app.print.user_templates.retain(|t| t.0 != name);
            app.print.user_templates.push((name.clone(), s));
            app.print.template = Some(name);
        }
        "printui.deleteTemplate" => {
            let name = p.get("name").and_then(Value::as_str).ok_or("missing name")?;
            let before = app.print.user_templates.len();
            app.print.user_templates.retain(|t| t.0 != name);
            if app.print.user_templates.len() == before {
                return Err(format!("no user print template named {name}"));
            }
            if app.print.template.as_deref() == Some(name) {
                app.print.template = None;
            }
        }
        "printui.media" => {
            let uri = p.get("uri").and_then(Value::as_str).unwrap_or(&app.print.printer_uri).trim().to_string();
            if uri.is_empty() {
                return Err("no printer: give uri or choose one (printui.printers)".into());
            }
            app.print.printer_uri = uri.clone();
            #[cfg(not(target_arch = "wasm32"))]
            {
                crate::tasks::spawn(
                    app,
                    "Reading printer paper sizes",
                    None,
                    move || dac_print::ipp::printer_info(&uri),
                    |app, ctx, r| match r {
                        Ok(info) => {
                            let n = info.media.len();
                            app.print.media = info.media;
                            app.toast(ctx, crate::i18n::tr_format!("{n} paper size(s) from the printer", n = n));
                        }
                        Err(e) => app.toast_error(ctx, e.to_string()),
                    },
                )?;
                return Ok(json!({"background": true}));
            }
            #[cfg(target_arch = "wasm32")]
            return Err("printing is not available in the browser".into());
        }
        "printui.printers" => {
            let server = p.get("server").and_then(Value::as_str).unwrap_or(&app.print.cups_server).to_string();
            app.print.cups_server = server.clone();
            #[cfg(not(target_arch = "wasm32"))]
            {
                crate::tasks::spawn(
                    app,
                    "Finding printers",
                    None,
                    move || dac_print::ipp::cups_printers(&server),
                    |app, ctx, r| match r {
                        Ok(list) => {
                            if app.print.printer_uri.is_empty()
                                && let Some(first) = list.first()
                            {
                                app.print.printer_uri = first.uri.clone();
                            }
                            let n = list.len();
                            app.print.printers = list;
                            app.toast(ctx, crate::i18n::tr_format!("{n} printer(s) found", n = n));
                        }
                        Err(e) => app.toast_error(ctx, e.to_string()),
                    },
                )?;
                return Ok(json!({"background": true}));
            }
            #[cfg(target_arch = "wasm32")]
            return Err("printing is not available in the browser".into());
        }
        "printui.print" | "printui.printOne" => return start(app, id, p),
        "printui.saveCreation" => {
            let name = p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).ok_or("missing name")?.to_string();
            let (mut doc, ids) = document(app)?;
            if ids.is_empty() {
                return Err("no photos to save in the print".into());
            }
            doc.settings = Some(serde_json::to_value(&app.print.settings).map_err(|e| e.to_string())?);
            let document = serde_json::to_value(&doc).map_err(|e| e.to_string())?;
            // the Saved Print open keeps its collection when saved under its own name again
            let same = app.print.creation.filter(|c| {
                app.session
                    .catalog
                    .album(dac_catalog::AlbumId(*c))
                    .is_some_and(|a| a.name == name && a.creation.as_ref().is_some_and(|k| k.kind == "print"))
            });
            let cid = match same {
                Some(c) => {
                    app.run("creation.update", json!({"id": c, "document": document}))?;
                    c
                }
                None => app
                    .run("creation.save", json!({"kind": "print", "name": name, "document": document}))?
                    .get("id")
                    .and_then(Value::as_u64)
                    .ok_or("the print was not saved")?,
            };
            app.print.creation = Some(cid);
        }
        "printui.openCreation" => {
            let cid = p.get("id").and_then(Value::as_u64).ok_or("missing id")?;
            let r = app.run("creation.get", json!({"id": cid}))?;
            if r["kind"] != "print" {
                return Err("not a Saved Print".into());
            }
            // a print saved from this module carries its settings; one made from a template (MCP)
            // keeps the current settings
            if let Some(st) = r["document"].get("settings").filter(|v| v.is_object()) {
                app.print.settings = PrintSettings::from_json(&st.to_string()).map_err(|e| e.to_string())?;
                app.print.template = None;
            }
            app.print.creation = Some(cid);
            app.print.page = 0;
        }
        _ => {}
    }
    Ok(state_json(app))
}

/// Starts a print: renders photos on a worker thread, then writes the file(s) or sends the job.
fn start(app: &mut DacApp, id: &str, p: &Value) -> Result<Value, String> {
    if cfg!(target_arch = "wasm32") {
        return Err("printing is not available in the browser".into());
    }
    if app.print.busy {
        return Err("a print is already running".into());
    }
    let one = id == "printui.printOne";
    let mut settings = app.print.settings.clone();
    if let Some(d) = p.get("destination") {
        settings.destination = serde_json::from_value(d.clone()).map_err(|e| format!("destination: {e}"))?;
    }
    let ids = job_photos(app, one);
    if ids.is_empty() {
        return Err("no photos to print".into());
    }
    let printer = p.get("printer").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| app.print.printer_uri.clone());
    let path = match settings.destination {
        Destination::Printer => {
            if printer.trim().is_empty() {
                return Err("choose a printer (Find Printers, or enter an ipp:// URI)".into());
            }
            dac_print::ipp::IppUri::parse(&printer).map_err(|e| e.to_string())?;
            String::new()
        }
        Destination::Pdf | Destination::Jpeg => {
            let given = p.get("path").and_then(Value::as_str).map(str::to_string).filter(|s| !s.trim().is_empty());
            match given {
                Some(path) => path,
                None => {
                    let (ext, filter): (&'static [&'static str], &str) =
                        if settings.destination == Destination::Pdf { (&["pdf"], "PDF") } else { (&["jpg", "jpeg"], "JPEG") };
                    let name = if settings.destination == Destination::Pdf { "Print.pdf" } else { "Print.jpg" };
                    let req = crate::pick::PickRequest::save("Print to File", filter, ext, name);
                    let mut params = p.clone();
                    if let Some(o) = params.as_object_mut() {
                        o.insert("destination".into(), serde_json::to_value(&settings.destination).unwrap_or(Value::Null));
                    }
                    match crate::pick::ask(app, id, &params, "path", req, |_| None) {
                        crate::pick::Picked::Now(paths) => paths.into_iter().next().ok_or("cancelled")?,
                        crate::pick::Picked::Later => return Ok(json!({"pending": "path"})),
                        crate::pick::Picked::Unavailable => return Err("no save dialog on this platform; supply path".into()),
                    }
                }
            }
        }
    };
    if !path.is_empty() {
        app.session.check_write_target(&path)?;
    }
    let doc = settings.build(&ids_text(&ids)).map_err(|e| e.to_string())?;
    // draft mode prints from preview-sized renders
    let dpi = if settings.job.draft { settings.job.dpi.min(150.0) } else { settings.job.dpi };
    let (jobs, infos) = dac_engine::creations::prepare_jobs(&mut app.session, &doc, dpi)?;
    let pages = doc.pages.len();
    let title = app.print.template.clone().unwrap_or_else(|| "Print".into());
    let write = app.services.write_shared.clone();
    app.print.busy = true;
    let dest = settings.destination.clone();
    let work = move || -> Result<Value, String> {
        let src = dac_engine::creations::run_jobs(jobs, infos)?;
        let text = dac_print::text::ShapedText::with_system_fonts();
        let progress = &mut |_: usize, _: usize| true;
        let write_file = |path: &str, bytes: &[u8]| -> Result<(), String> {
            match &write {
                Some(w) => w(path, bytes),
                None => dac_engine::export::write_file(path, bytes),
            }
        };
        match dest {
            Destination::Pdf | Destination::Printer => {
                let opt = dac_print::output::PdfOptions {
                    metadata: dac_pdf::Metadata {
                        title: Some(title.clone()),
                        creator: Some(dac_brand::DISPLAY_NAME.to_string()),
                        ..Default::default()
                    },
                    output_intent: None,
                };
                let out = dac_print::output::to_pdf(&doc, &settings, &src, &text, &opt, progress).map_err(|e| e.to_string())?;
                let bytes = out.files.into_iter().next().unwrap_or_default();
                if dest == Destination::Printer {
                    let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "user".into());
                    let job = dac_print::ipp::print_job(&printer, &user, &title, "application/pdf", 1, &bytes).map_err(|e| e.to_string())?;
                    Ok(json!({"printer": printer, "job": job, "pages": out.pages, "warnings": out.warnings}))
                } else {
                    write_file(&path, &bytes)?;
                    Ok(json!({"path": path, "pages": out.pages, "bytes": bytes.len(), "warnings": out.warnings}))
                }
            }
            Destination::Jpeg => {
                let out = dac_print::output::to_jpeg(&doc, &settings, &src, &text, progress).map_err(|e| e.to_string())?;
                let n = out.files.len();
                let mut paths = Vec::new();
                for (i, bytes) in out.files.iter().enumerate() {
                    let p = if n == 1 {
                        path.clone()
                    } else {
                        let (stem, ext) = path.rsplit_once('.').unwrap_or((path.as_str(), "jpg"));
                        format!("{stem}-{}.{ext}", i + 1)
                    };
                    write_file(&p, bytes)?;
                    paths.push(p);
                }
                Ok(json!({"paths": paths, "pages": out.pages, "warnings": out.warnings}))
            }
        }
    };
    crate::tasks::spawn(app, "Printing", Some("export"), work, |app, ctx, r: Result<Value, String>| {
        app.print.busy = false;
        match r {
            Ok(v) => {
                let msg = if let Some(j) = v.get("job") {
                    crate::i18n::tr_format!("Sent {n} page(s) to the printer (job {j})", n = v["pages"], j = j)
                } else {
                    crate::i18n::tr_format!("Printed {n} page(s) to file", n = v["pages"])
                };
                app.print.last = Some(v);
                app.toast(ctx, msg);
            }
            Err(e) => {
                app.print.last = Some(json!({"error": e}));
                app.toast_error(ctx, e);
            }
        }
    })
    .inspect_err(|_| app.print.busy = false)?;
    Ok(json!({"background": true, "pages": pages, "photos": ids.len()}))
}

// ------------------------------------------------------------------------------------------- UI

pub struct PrintModule;

/// Print: `⌘P`-style keys are the global ones; ← → turn preview pages.
const PRINT_KEYS: &[ModuleKey] =
    &[("Left", "printui.page", r#"{"delta": -1}"#), ("Right", "printui.page", r#"{"delta": 1}"#), crate::help_overlay::KEY];

impl Module for PrintModule {
    fn id(&self) -> ModuleId {
        ModuleId::Print
    }
    fn left_panels(&self) -> &'static [PanelId] {
        &[]
    }
    fn right_panels(&self) -> &'static [PanelId] {
        &[]
    }
    fn toolbar(&self, ui: &mut egui::Ui, app: &mut DacApp) {
        toolbar(ui, app);
    }
    fn center(&self, ui: &mut egui::Ui, app: &mut DacApp) {
        center(ui, app);
    }
    fn keymap(&self) -> &'static [ModuleKey] {
        PRINT_KEYS
    }
    fn command_prefixes(&self) -> &'static [&'static str] {
        &["print.", "printui."]
    }
}

fn cmd(app: &mut DacApp, ctx: &egui::Context, id: &str, p: Value) {
    if let Err(e) = app.run(id, p) {
        app.toast_error(ctx, e);
    }
}

fn toolbar(ui: &mut egui::Ui, app: &mut DacApp) {
    let t = Tokens::get(ui.ctx());
    let n = document(app).map(|d| d.0.pages.len()).unwrap_or(0);
    let photos = job_photos(app, false).len();
    egui::Panel::bottom("print_toolbar")
        .exact_size(t.bottom_bar_h)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin { left: 12, right: 12, top: 0, bottom: 0 }))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let ctx = ui.ctx().clone();
                let r = ui.button("◀");
                register(&ctx, "print:prevPage", r.rect);
                if r.clicked() {
                    cmd(app, &ctx, "printui.page", json!({"delta": -1}));
                }
                ui.label(
                    egui::RichText::new(crate::i18n::tr_format!(
                        "Page {page} of {pages}",
                        page = (app.print.page + 1).min(n.max(1)),
                        pages = n.max(1)
                    ))
                    .color(t.text),
                );
                let r = ui.button("▶");
                register(&ctx, "print:nextPage", r.rect);
                if r.clicked() {
                    cmd(app, &ctx, "printui.page", json!({"delta": 1}));
                }
                ui.add_space(16.0);
                ui.label(egui::RichText::new(crate::i18n::tr_format!("{photos} photo(s) selected for print", photos = photos)).color(t.text_dim));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let s = app.print.settings.page.size;
                    ui.label(egui::RichText::new(format!("{:.2} × {:.2} in", s.w / 72.0, s.h / 72.0)).color(t.text_dim));
                });
            });
        });
}

fn center(ui: &mut egui::Ui, app: &mut DacApp) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    ensure_loaded(app);
    if !ctx.input(|i| i.pointer.any_down())
        && let Err(e) = persist(app)
    {
        log::warn!("print settings not saved: {e}");
    }
    let mut area = ui.available_rect_before_wrap();
    if crate::module::edge_visible(app, Edge::Bottom) {
        let film = Rect::from_min_max(pos2(area.left(), area.bottom() - t.film_h), area.max);
        area.max.y = film.top();
        crate::panels::detail::filmstrip(app, ui, film);
    }
    ui.allocate_rect(area, Sense::hover());
    register(&ctx, "view:module:print", area);
    let p = ui.painter().clone();
    if crate::module::module_edge(app, Edge::Left) {
        let left = Rect::from_min_max(area.min, pos2(area.left() + LEFT_W, area.bottom()));
        area.min.x = left.right();
        p.rect_filled(left, 0.0, t.chrome);
        p.line_segment([left.right_top(), left.right_bottom()], Stroke::new(1.0, t.divider));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(left.shrink(10.0)));
        template_browser(&mut child, app);
    }
    if crate::module::module_edge(app, Edge::Right) {
        let right = Rect::from_min_max(pos2(area.right() - RIGHT_W, area.top()), area.max);
        area.max.x = right.left();
        p.rect_filled(right, 0.0, t.chrome);
        p.line_segment([right.left_top(), right.left_bottom()], Stroke::new(1.0, t.divider));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(right.shrink(10.0)));
        settings_panels(&mut child, app);
    }
    app.canvas_rect = Some(area);
    canvas(ui, app, area);
}

fn heading(ui: &mut egui::Ui, title: &str) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(6.0);
    ui.label(egui::RichText::new(crate::i18n::tr(title)).font(t.semibold(13.0)).color(t.text));
    ui.separator();
}

fn template_browser(ui: &mut egui::Ui, app: &mut DacApp) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    egui::ScrollArea::vertical().id_salt("print-templates").show(ui, |ui| {
        heading(ui, "Template Browser");
        let current = app.print.template.clone();
        let mut groups: Vec<(&str, Vec<String>)> = vec![("Built-in", builtin_templates().into_iter().map(|t| t.0).collect())];
        if !app.print.user_templates.is_empty() {
            groups.push(("User Templates", app.print.user_templates.iter().map(|t| t.0.clone()).collect()));
        }
        for (group, names) in groups {
            ui.label(egui::RichText::new(crate::i18n::tr(group)).color(t.text_dim).size(11.0));
            for name in names {
                // built-in templates show in the UI language, the user's own as named
                let shown = if group == "Built-in" { crate::i18n::tr(&name).to_string() } else { name.clone() };
                let r = ui.selectable_label(current.as_deref() == Some(name.as_str()), shown);
                register(&ctx, format!("print:template:{name}"), r.rect);
                if r.clicked() {
                    cmd(app, &ctx, "printui.template", json!({"name": name}));
                }
                if group == "User Templates" {
                    r.context_menu(|ui| {
                        if ui.button(crate::i18n::tr("Delete")).clicked() {
                            cmd(app, &ctx, "printui.deleteTemplate", json!({"name": name}));
                            ui.close();
                        }
                    });
                }
            }
            ui.add_space(6.0);
        }
        ui.horizontal(|ui| {
            let id = egui::Id::new("print-new-template");
            let mut name: String = ui.data_mut(|d| d.get_temp(id)).unwrap_or_default();
            let r = ui.add(egui::TextEdit::singleline(&mut name).hint_text(crate::i18n::tr("Template name")).desired_width(120.0));
            crate::access::label(&r, "Template name");
            app.text_focus |= r.has_focus();
            ui.data_mut(|d| d.insert_temp(id, name.clone()));
            let b = ui.button("+");
            register(&ctx, "print:saveTemplate", b.rect);
            if b.clicked() && !name.trim().is_empty() {
                cmd(app, &ctx, "printui.saveTemplate", json!({"name": name}));
                ui.data_mut(|d| d.insert_temp(id, String::new()));
            }
        });
        heading(ui, "Saved Prints");
        let open = app.print.creation;
        for c in dac_engine::creations::creations_of(&app.session, Some(dac_layout::CreationKind::Print)) {
            let r = ui.selectable_label(open == Some(c.id.0), format!("{} ({})", c.name, c.photos.len()));
            register(&ctx, format!("print:creation:{}", c.id.0), r.rect);
            if r.clicked() {
                cmd(app, &ctx, "creation.open", json!({"id": c.id.0}));
            }
        }
        ui.horizontal(|ui| {
            let id = egui::Id::new("print-new-creation");
            let mut name: String = ui.data_mut(|d| d.get_temp(id)).unwrap_or_default();
            let r = ui.add(egui::TextEdit::singleline(&mut name).hint_text(crate::i18n::tr("Name")).desired_width(120.0));
            crate::access::label(&r, "Name");
            app.text_focus |= r.has_focus();
            ui.data_mut(|d| d.insert_temp(id, name.clone()));
            let b = ui.button(crate::i18n::tr("Create Saved Print"));
            register(&ctx, "print:saveCreation", b.rect);
            if b.clicked() && !name.trim().is_empty() {
                cmd(app, &ctx, "printui.saveCreation", json!({"name": name}));
                ui.data_mut(|d| d.insert_temp(id, String::new()));
            }
        });
        heading(ui, "Guides");
        let g = &mut app.print;
        for (k, label, v) in [
            ("rulers", "Rulers", &mut g.rulers),
            ("margins", "Margins and Gutters", &mut g.margins),
            ("cells", "Image Cells", &mut g.cells),
            ("bleed", "Page Bleed", &mut g.bleed),
            ("dimensions", "Dimensions", &mut g.dimensions),
        ] {
            let r = ui.checkbox(v, crate::i18n::tr(label));
            register(&ctx, format!("print:guide:{k}"), r.rect);
        }
    });
}

fn settings_panels(ui: &mut egui::Ui, app: &mut DacApp) {
    let ctx = ui.ctx().clone();
    let before = app.print.settings.clone();
    egui::ScrollArea::vertical().id_salt("print-settings").show(ui, |ui| {
        let s = &mut app.print.settings;
        heading(ui, "Layout Style");
        let style = match s.layout {
            LayoutStyle::SingleImage { .. } => 0,
            LayoutStyle::PicturePackage(_) => 1,
            LayoutStyle::CustomPackage { .. } => 2,
        };
        for (i, label) in ["Single Image / Contact Sheet", "Picture Package", "Custom Package"].into_iter().enumerate() {
            let r = ui.radio(style == i, crate::i18n::tr(label));
            register(&ctx, format!("print:style:{i}"), r.rect);
            if r.clicked() && style != i {
                s.layout = match i {
                    0 => LayoutStyle::SingleImage { grid: dac_layout::grid::Grid::new(1, 1) },
                    1 => LayoutStyle::PicturePackage(dac_print::PackageSpec { cells: vec![dac_layout::Size::inches(5.0, 7.0)], gap: 9.0 }),
                    _ => {
                        let c = s.page.content();
                        LayoutStyle::CustomPackage {
                            pages: vec![dac_layout::PageLayout {
                                cells: vec![dac_layout::Cell::photo(dac_layout::Rect::new(c.x, c.y, c.w / 2.0, c.h / 2.0))],
                                guides: Vec::new(),
                            }],
                        }
                    }
                };
            }
        }
        heading(ui, "Image Settings");
        let im = &mut s.image;
        ui.checkbox(&mut im.zoom_to_fill, crate::i18n::tr("Zoom to Fill"));
        ui.checkbox(&mut im.rotate_to_fit, crate::i18n::tr("Rotate to Fit"));
        ui.checkbox(&mut im.repeat_one, crate::i18n::tr("Repeat One Photo per Page"));
        let mut stroke = im.stroke.is_some();
        if ui.checkbox(&mut stroke, crate::i18n::tr("Stroke Border")).changed() {
            im.stroke = stroke.then_some(dac_layout::Stroke { width: 1.0, color: dac_layout::BLACK });
        }
        if let Some(st) = &mut im.stroke {
            ui.add(egui::Slider::new(&mut st.width, 0.25..=20.0).text(crate::i18n::tr("Width (pt)")));
        }
        match &mut s.layout {
            LayoutStyle::SingleImage { grid } => {
                heading(ui, "Layout");
                ui.add(egui::Slider::new(&mut grid.rows, 1..=15).text(crate::i18n::tr("Rows")));
                ui.add(egui::Slider::new(&mut grid.cols, 1..=15).text(crate::i18n::tr("Columns")));
                ui.add(egui::Slider::new(&mut grid.gutter_y, 0.0..=72.0).text(crate::i18n::tr("Vertical Gutter (pt)")));
                ui.add(egui::Slider::new(&mut grid.gutter_x, 0.0..=72.0).text(crate::i18n::tr("Horizontal Gutter (pt)")));
                ui.checkbox(&mut grid.keep_square, crate::i18n::tr("Keep Square"));
            }
            LayoutStyle::PicturePackage(spec) => {
                heading(ui, "Cells");
                ui.horizontal_wrapped(|ui| {
                    for (w, h) in [(2.0, 2.5), (2.5, 3.5), (3.0, 7.0), (4.0, 6.0), (5.0, 7.0), (8.0, 10.0)] {
                        let r = ui.button(format!("{w}×{h}"));
                        register(&ctx, format!("print:addCell:{w}x{h}"), r.rect);
                        if r.clicked() && spec.cells.len() < 64 {
                            spec.cells.push(dac_layout::Size::inches(w, h));
                        }
                    }
                });
                if ui.button(crate::i18n::tr("Clear Layout")).clicked() {
                    spec.cells.clear();
                }
                ui.add(egui::Slider::new(&mut spec.gap, 0.0..=36.0).text(crate::i18n::tr("Gap (pt)")));
            }
            LayoutStyle::CustomPackage { pages } => {
                heading(ui, "Cells");
                ui.label(crate::i18n::tr("Drag cells on the page to move them."));
                ui.horizontal_wrapped(|ui| {
                    for (w, h) in [(2.5, 3.5), (4.0, 6.0), (5.0, 7.0), (8.0, 10.0)] {
                        if ui.button(format!("{w}×{h}")).clicked()
                            && let Some(pg) = pages.first_mut()
                        {
                            let c = s.page.content();
                            let sz = dac_layout::Size::inches(w, h);
                            pg.cells.push(dac_layout::Cell::photo(dac_layout::Rect::new(c.x, c.y, sz.w.min(c.w), sz.h.min(c.h))));
                        }
                    }
                });
                if ui.button(crate::i18n::tr("Clear Layout")).clicked()
                    && let Some(pg) = pages.first_mut()
                {
                    pg.cells.clear();
                }
            }
        }
        heading(ui, "Margins");
        let m = &mut s.page.margins;
        for (label, v) in [("Left", &mut m.left), ("Right", &mut m.right), ("Top", &mut m.top), ("Bottom", &mut m.bottom)] {
            ui.add(egui::Slider::new(v, 0.0..=216.0).text(crate::i18n::tr(label)));
        }
        heading(ui, "Page");
        let o = &mut s.options;
        let mut bg = Color32::from_rgba_unmultiplied(o.background[0], o.background[1], o.background[2], 255);
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr("Page Background Color"));
            let r = ui.color_edit_button_srgba(&mut bg);
            crate::access::label(&r, "Page Background Color");
            if r.changed() {
                o.background = [bg.r(), bg.g(), bg.b(), 255];
            }
        });
        let mut plate = o.identity_plate.is_some();
        let r = ui.checkbox(&mut plate, crate::i18n::tr("Identity Plate"));
        register(&ctx, "print:identityPlate", r.rect);
        if r.changed() {
            o.identity_plate = plate.then(|| dac_print::job::IdentityPlate { text: dac_brand::DISPLAY_NAME.to_string(), ..Default::default() });
        }
        if let Some(pl) = &mut o.identity_plate {
            let r = ui.text_edit_singleline(&mut pl.text);
            app.text_focus |= r.has_focus();
            ui.add(egui::Slider::new(&mut pl.opacity, 0.0..=1.0).text(crate::i18n::tr("Opacity")));
            ui.add(egui::Slider::new(&mut pl.size, 6.0..=96.0).text(crate::i18n::tr("Size")));
            ui.checkbox(&mut pl.every_image, crate::i18n::tr("Render on every image"));
        }
        let r = ui.checkbox(&mut o.page_numbers, crate::i18n::tr("Page Numbers"));
        register(&ctx, "print:pageNumbers", r.rect);
        ui.checkbox(&mut o.page_info, crate::i18n::tr("Page Info"));
        let r = ui.checkbox(&mut o.crop_marks, crate::i18n::tr("Crop Marks"));
        register(&ctx, "print:cropMarks", r.rect);
        let mut info = o.photo_info.is_some();
        let r = ui.checkbox(&mut info, crate::i18n::tr("Photo Info"));
        register(&ctx, "print:photoInfo", r.rect);
        if r.changed() {
            o.photo_info = info.then(|| "{Filename}".to_string());
        }
        if let Some(tpl) = &mut o.photo_info {
            egui::ComboBox::from_id_salt("print-photo-info").selected_text(tpl.as_str()).show_ui(ui, |ui| {
                for tok in ["{Filename}", "{Title}", "{Caption}", "{Date}", "{Exposure}", "{Camera}", "{Filename} · {Exposure}"] {
                    ui.selectable_value(tpl, tok.to_string(), tok);
                }
            });
        }
        ui.add(egui::Slider::new(&mut o.font_size, 4.0..=24.0).text(crate::i18n::tr("Font Size")));
        heading(ui, "Print Job");
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr("Print to:"));
            for (d, label) in [(Destination::Printer, "Printer"), (Destination::Pdf, "PDF File"), (Destination::Jpeg, "JPEG File")] {
                let r = ui.selectable_label(s.destination == d, crate::i18n::tr(label));
                register(&ctx, format!("print:dest:{label}"), r.rect);
                if r.clicked() {
                    s.destination = d;
                }
            }
        });
        ui.checkbox(&mut s.job.draft, crate::i18n::tr("Draft Mode Printing"));
        ui.add(egui::Slider::new(&mut s.job.dpi, 72.0..=720.0).step_by(1.0).text(crate::i18n::tr("Print Resolution (ppi)")));
        if s.destination == Destination::Jpeg {
            ui.add(egui::Slider::new(&mut s.job.jpeg_quality, 10..=100).text(crate::i18n::tr("JPEG Quality")));
        }
        if s.destination == Destination::Printer {
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut app.print.printer_uri).hint_text("ipp://host/printers/name").desired_width(190.0));
                crate::access::label(&r, "Printer address");
                app.text_focus |= r.has_focus();
                let b = ui.button(crate::i18n::tr("Find"));
                register(&ctx, "print:findPrinters", b.rect);
                if b.clicked() {
                    cmd(app, &ctx, "printui.printers", json!({}));
                }
            });
            let printers = app.print.printers.clone();
            for pr in printers {
                if ui.selectable_label(app.print.printer_uri == pr.uri, format!("{} {}", pr.name, pr.info)).clicked() {
                    app.print.printer_uri = pr.uri.clone();
                    // offer its paper sizes in Page Setup
                    cmd(app, &ctx, "printui.media", json!({"uri": pr.uri}));
                }
            }
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let busy = app.print.busy;
            let label = if app.print.settings.destination == Destination::Printer { "Print…" } else { "Print to File…" };
            let r = ui.add_enabled(!busy, egui::Button::new(crate::i18n::tr(label)));
            register(&ctx, "print:print", r.rect);
            if r.clicked() {
                cmd(app, &ctx, "printui.print", json!({}));
            }
            let r = ui.add_enabled(!busy, egui::Button::new(crate::i18n::tr("Print One")));
            register(&ctx, "print:printOne", r.rect);
            if r.clicked() {
                cmd(app, &ctx, "printui.printOne", json!({}));
            }
        });
    });
    // keep only valid edits (sliders can't produce invalid ones, but stay safe)
    if app.print.settings != before && app.print.settings.validate().is_err() {
        app.print.settings = before;
    }
}

/// Page Setup sits above the page: paper and orientation.
fn page_setup_bar(ui: &mut egui::Ui, app: &mut DacApp, r: Rect) {
    let ctx = ui.ctx().clone();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let s = app.print.settings.page.size;
    let landscape = s.w > s.h;
    let cur = PAPERS
        .iter()
        .find(|p| (p.size.w - s.w.min(s.h)).abs() < 0.5 && (p.size.h - s.w.max(s.h)).abs() < 0.5)
        .map(|p| crate::i18n::tr(p.label).to_string())
        .unwrap_or_else(|| format!("{:.2} × {:.2} in", s.w / 72.0, s.h / 72.0));
    child.label(crate::i18n::tr("Page Setup:"));
    let resp = egui::ComboBox::from_id_salt("print-paper").selected_text(cur).width(200.0).show_ui(&mut child, |ui| {
        for p in PAPERS {
            if ui.selectable_label(false, crate::i18n::tr(p.label)).clicked() {
                cmd(app, &ctx, "printui.pageSetup", json!({"paper": p.key, "landscape": landscape}));
            }
        }
        if !app.print.media.is_empty() {
            ui.separator();
            ui.label(egui::RichText::new(crate::i18n::tr("Printer paper")).small().weak());
            for m in app.print.media.clone() {
                let label = format!("{}  ({:.2} × {:.2} in)", m.name, m.size.w / 72.0, m.size.h / 72.0);
                if ui.selectable_label(false, label).clicked() {
                    cmd(app, &ctx, "printui.pageSetup", json!({"media": m.name, "landscape": landscape}));
                }
            }
        }
    });
    register(&ctx, "print:paper", resp.response.rect);
    for (label, land) in [("Portrait", false), ("Landscape", true)] {
        let r = child.selectable_label(landscape == land, crate::i18n::tr(label));
        register(&ctx, format!("print:{}", label.to_lowercase()), r.rect);
        if r.clicked() && landscape != land {
            cmd(app, &ctx, "printui.pageSetup", json!({"landscape": land}));
        }
    }
}

fn canvas(ui: &mut egui::Ui, app: &mut DacApp, area: Rect) {
    let t = Tokens::get(ui.ctx());
    let p = ui.painter_at(area);
    p.rect_filled(area, 0.0, t.canvas);
    let bar = Rect::from_min_size(area.min + vec2(12.0, 6.0), vec2(area.width() - 24.0, 28.0));
    page_setup_bar(ui, app, bar);
    let (doc, _) = match document(app) {
        Ok(d) => d,
        Err(e) => {
            p.text(area.center(), Align2::CENTER_CENTER, e, t.font(14.0), t.caution);
            return;
        }
    };
    let n = doc.pages.len().max(1);
    app.print.page = app.print.page.min(n - 1);
    let Some(layout) = doc.pages.get(app.print.page) else { return };
    let page = &doc.page;
    let rulers = app.print.rulers;
    let inner = Rect::from_min_max(
        pos2(area.left() + 24.0 + if rulers { RULER } else { 0.0 }, bar.bottom() + 12.0 + if rulers { RULER } else { 0.0 }),
        area.max - vec2(24.0, 24.0),
    );
    if inner.width() < 20.0 || inner.height() < 20.0 {
        return;
    }
    let k = (inner.width() / page.size.w).min(inner.height() / page.size.h);
    let sheet = Rect::from_center_size(inner.center(), vec2(page.size.w * k, page.size.h * k));
    let to = |x: f32, y: f32| pos2(sheet.left() + x * k, sheet.top() + y * k);
    let rect = |r: &dac_layout::Rect| Rect::from_min_size(to(r.x, r.y), vec2(r.w * k, r.h * k));
    register(ui.ctx(), "print:page", sheet);
    // shadow + paper
    p.rect_filled(sheet.translate(vec2(3.0, 4.0)), 0.0, Color32::from_black_alpha(90));
    let bg = page.background;
    p.rect_filled(sheet, 0.0, Color32::from_rgb(bg[0], bg[1], bg[2]));
    let guide = Color32::from_rgb(70, 140, 220);
    if app.print.margins {
        let c = rect(&page.content());
        p.rect_stroke(c, 0.0, Stroke::new(1.0, guide.gamma_multiply(0.8)), StrokeKind::Inside);
    }
    let infos: Vec<Option<PhotoInfo>> =
        layout.cells.iter().map(|c| c.as_photo().and_then(|ph| ph.photo.as_deref()).map(|id| info_of(app, id))).collect();
    let dimensions = app.print.dimensions;
    let show_cells = app.print.cells;
    let pi = app.print.page;
    for (i, cell) in layout.cells.iter().enumerate() {
        let r = rect(&cell.rect);
        match &cell.kind {
            CellKind::Photo(ph) => {
                let Some(id) = ph.photo.as_deref().and_then(|s| s.parse::<u64>().ok()).map(PhotoId) else {
                    p.rect_filled(r, 0.0, Color32::from_gray(205));
                    continue;
                };
                crate::panels::grid::request_thumb(app, id, 512, 6);
                draw_photo(&p, app, id, r, ph);
                if show_cells {
                    p.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_gray(150)), StrokeKind::Inside);
                }
                if dimensions {
                    let txt = format!("{:.2} × {:.2} in", cell.rect.w / 72.0, cell.rect.h / 72.0);
                    let g = p.layout_no_wrap(txt, t.font(10.0), Color32::WHITE);
                    let br = Rect::from_min_size(r.min + vec2(3.0, 3.0), g.size() + vec2(6.0, 2.0));
                    p.rect_filled(br, 2.0, Color32::from_black_alpha(160));
                    p.galley(br.min + vec2(3.0, 1.0), g, Color32::WHITE);
                }
            }
            CellKind::Graphic(g) => {
                if let Some(f) = g.fill {
                    p.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(f[0], f[1], f[2], f[3]));
                }
            }
            CellKind::Text(tc) => {
                let src = tc.source.and_then(|s| infos.get(s).cloned().flatten()).or_else(|| infos.iter().flatten().next().cloned());
                let info = PhotoInfo { page: pi + 1, pages: n, ..src.unwrap_or_default() };
                let s = expand(&tc.text, &info);
                let c = tc.style.color;
                let size = (tc.style.size * k).max(1.0);
                if size < 3.0 {
                    // too small to read: a bar like a print preview shows
                    p.rect_filled(
                        Rect::from_center_size(r.center(), vec2((s.chars().count() as f32 * size * 0.5).min(r.width()), size)),
                        0.0,
                        Color32::from_gray(150),
                    );
                    continue;
                }
                let (x, al) = match tc.style.align {
                    Align::Left => (r.left(), Align2::LEFT_TOP),
                    Align::Center => (r.center().x, Align2::CENTER_TOP),
                    Align::Right => (r.right(), Align2::RIGHT_TOP),
                };
                let clip = p.with_clip_rect(r.expand(1.0).intersect(area));
                clip.text(pos2(x, r.top()), al, s, t.font(size), Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]));
            }
        }
        let _ = i;
    }
    if app.print.settings.options.crop_marks {
        let m = 12.0;
        let col = Color32::from_gray(200);
        for (x, dx) in [(sheet.left(), -1.0), (sheet.right(), 1.0)] {
            for (y, dy) in [(sheet.top(), -1.0), (sheet.bottom(), 1.0)] {
                p.line_segment([pos2(x + dx * 3.0, y), pos2(x + dx * (3.0 + m), y)], Stroke::new(1.0, col.gamma_multiply(0.8)));
                p.line_segment([pos2(x, y + dy * 3.0), pos2(x, y + dy * (3.0 + m))], Stroke::new(1.0, col.gamma_multiply(0.8)));
            }
        }
    }
    if app.print.bleed && page.bleed > 0.0 {
        p.rect_stroke(rect(&page.bleed_box()), 0.0, Stroke::new(1.0, Color32::from_rgb(220, 60, 60)), StrokeKind::Outside);
    }
    for g in &layout.guides {
        match *g {
            dac_layout::Guide::Vertical(x) => p.line_segment([to(x, 0.0), to(x, page.size.h)], Stroke::new(1.0, Color32::from_rgb(255, 0, 170))),
            dac_layout::Guide::Horizontal(y) => p.line_segment([to(0.0, y), to(page.size.w, y)], Stroke::new(1.0, Color32::from_rgb(255, 0, 170))),
        };
    }
    if rulers {
        draw_rulers(&p, &t, sheet, k, page.size);
    }
    if matches!(app.print.settings.layout, LayoutStyle::CustomPackage { .. }) {
        custom_drag(ui, app, sheet, k);
    }
    // page numbers under the sheet
    p.text(
        pos2(sheet.center().x, sheet.bottom() + 12.0),
        Align2::CENTER_CENTER,
        crate::i18n::tr_format!("Page {page} of {pages}", page = pi + 1, pages = n),
        t.font(11.0),
        t.text_dim,
    );
}

/// The photo's thumbnail placed in its cell as the print will place it.
fn draw_photo(p: &egui::Painter, app: &DacApp, id: PhotoId, r: Rect, ph: &dac_layout::PhotoCell) {
    let Some(tex) = app.renderer.thumb(id) else {
        p.rect_filled(r, 0.0, Color32::from_gray(120));
        return;
    };
    let [tw, th] = tex.size;
    let (mut iw, mut ih) = (tw as f32, th as f32);
    let mut turns = ph.rotate % 4;
    if ph.rotate_to_fit && tw != th && ((tw > th) != (r.width() > r.height())) {
        turns = (turns + 1) % 4;
    }
    if turns % 2 == 1 {
        std::mem::swap(&mut iw, &mut ih);
    }
    let cell = dac_layout::Rect::new(r.left(), r.top(), r.width(), r.height());
    let (s, ox, oy) = dac_layout::render::photo_placement(cell, iw, ih, ph);
    if !(s.is_finite() && s > 0.0) {
        return;
    }
    let shown = Rect::from_min_size(pos2(ox, oy), vec2(iw * s, ih * s));
    let vis = shown.intersect(r);
    if vis.width() <= 0.0 || vis.height() <= 0.0 {
        return;
    }
    // displayed (u', v') in [0, 1] → texture uv, by quarter turns clockwise
    let uv = |q: Pos2| -> Pos2 {
        let (u, v) = ((q.x - shown.left()) / shown.width(), (q.y - shown.top()) / shown.height());
        match turns {
            1 => pos2(v, 1.0 - u),
            2 => pos2(1.0 - u, 1.0 - v),
            3 => pos2(1.0 - v, u),
            _ => pos2(u, v),
        }
    };
    let mut mesh = egui::Mesh::with_texture(tex.tex.id());
    for q in [vis.left_top(), vis.right_top(), vis.right_bottom(), vis.left_bottom()] {
        mesh.vertices.push(egui::epaint::Vertex { pos: q, uv: uv(q), color: Color32::WHITE });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.add(egui::Shape::mesh(mesh));
    if let Some(st) = ph.stroke {
        let c = st.color;
        let k = r.width() / cell.w.max(1.0);
        let _ = k;
        p.rect_stroke(vis, 0.0, Stroke::new(st.width.max(0.5), Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])), StrokeKind::Inside);
    }
    let _ = Fit::Fit;
}

fn draw_rulers(p: &egui::Painter, t: &Tokens, sheet: Rect, k: f32, size: dac_layout::Size) {
    let top = Rect::from_min_max(pos2(sheet.left(), sheet.top() - RULER - 6.0), pos2(sheet.right(), sheet.top() - 6.0));
    let left = Rect::from_min_max(pos2(sheet.left() - RULER - 6.0, sheet.top()), pos2(sheet.left() - 6.0, sheet.bottom()));
    for r in [top, left] {
        p.rect_filled(r, 0.0, t.chrome);
    }
    let col = t.text_dim;
    let quarter = 18.0 * k;
    if quarter < 2.0 {
        return;
    }
    let mut i = 0u32;
    while (i as f32) * 18.0 <= size.w + 0.01 && i < 4000 {
        let x = sheet.left() + i as f32 * quarter;
        let len = if i.is_multiple_of(4) {
            RULER * 0.7
        } else if i.is_multiple_of(2) {
            RULER * 0.4
        } else {
            RULER * 0.25
        };
        p.line_segment([pos2(x, top.bottom()), pos2(x, top.bottom() - len)], Stroke::new(1.0, col));
        if i.is_multiple_of(4) && i > 0 {
            p.text(pos2(x + 2.0, top.top() + 1.0), Align2::LEFT_TOP, format!("{}", i / 4), t.font(9.0), col);
        }
        i += 1;
    }
    let mut j = 0u32;
    while (j as f32) * 18.0 <= size.h + 0.01 && j < 4000 {
        let y = sheet.top() + j as f32 * quarter;
        let len = if j.is_multiple_of(4) {
            RULER * 0.7
        } else if j.is_multiple_of(2) {
            RULER * 0.4
        } else {
            RULER * 0.25
        };
        p.line_segment([pos2(left.right(), y), pos2(left.right() - len, y)], Stroke::new(1.0, col));
        if j.is_multiple_of(4) && j > 0 {
            p.text(pos2(left.left() + 1.0, y + 2.0), Align2::LEFT_TOP, format!("{}", j / 4), t.font(9.0), col);
        }
        j += 1;
    }
}

/// Custom Package: drag a cell to move it (snapped to page, margins and other cells).
fn custom_drag(ui: &mut egui::Ui, app: &mut DacApp, sheet: Rect, k: f32) {
    let page = app.print.settings.page.clone();
    let LayoutStyle::CustomPackage { pages } = &mut app.print.settings.layout else { return };
    let Some(pg) = pages.first_mut() else { return };
    let n = pg.cells.len();
    for i in 0..n {
        let Some(c) = pg.cells.get(i) else { continue };
        let r = Rect::from_min_size(pos2(sheet.left() + c.rect.x * k, sheet.top() + c.rect.y * k), vec2(c.rect.w * k, c.rect.h * k));
        let resp = ui.interact(r, egui::Id::new(("print-cell", i)), Sense::drag());
        register(ui.ctx(), format!("print:cell:{i}"), r);
        let raw_id = egui::Id::new(("print-cell-raw", i));
        if resp.drag_started() {
            ui.data_mut(|d| d.insert_temp(raw_id, [c.rect.x, c.rect.y]));
        }
        if resp.dragged() {
            let d = resp.drag_delta() / k;
            let mut raw: [f32; 2] = ui.data_mut(|m| m.get_temp(raw_id)).unwrap_or([c.rect.x, c.rect.y]);
            raw[0] += d.x;
            raw[1] += d.y;
            ui.data_mut(|m| m.insert_temp(raw_id, raw));
            let targets = dac_layout::snap::SnapTargets::for_page(&page, pg, Some(i));
            if let Some(c) = pg.cells.get_mut(i) {
                let moved = dac_layout::Rect::new(raw[0], raw[1], c.rect.w, c.rect.h);
                c.rect = dac_layout::snap::snap_move(moved, &targets, 6.0 / k.max(0.01)).0;
            }
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_patches_nested_settings() {
        let mut a = json!({"page": {"size": {"w": 1, "h": 2}}, "job": {"dpi": 240}});
        merge(&mut a, &json!({"job": {"dpi": 300}, "page": {"size": {"w": 5}}}));
        assert_eq!(a, json!({"page": {"size": {"w": 5, "h": 2}}, "job": {"dpi": 300}}));
    }

    #[test]
    fn user_templates_settings_and_printer_paper_persist() {
        let dir = std::env::temp_dir().join(format!("dac-print-persist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let open = || {
            let mut s = dac_engine::Session::with_demo();
            s.remote.connections_path = Some(dir.join("connections.json"));
            DacApp::new(s, crate::Services::default())
        };
        let mut app = open();
        app.run("printui.template", json!({"name": "2×2 Cells"})).unwrap();
        app.run("printui.set", json!({"settings": {"job": {"dpi": 200}}})).unwrap();
        app.run("printui.saveTemplate", json!({"name": "Gone"})).unwrap();
        app.run("printui.deleteTemplate", json!({"name": "Gone"})).unwrap();
        assert_eq!(app.print.template, None);
        app.run("printui.saveTemplate", json!({"name": "Mine"})).unwrap();
        assert!(app.run("printui.deleteTemplate", json!({"name": "Gone"})).is_err());
        // a printer's PWG media name sets the page size (A5: 148 × 210 mm)
        app.run("printui.pageSetup", json!({"media": "iso_a5_148x210mm", "landscape": false})).unwrap();
        assert!((app.print.settings.page.size.w - 148.0 * 72.0 / 25.4).abs() < 0.1);
        assert!(app.run("printui.pageSetup", json!({"media": "bogus"})).is_err());
        assert!(app.run("printui.media", json!({"uri": ""})).is_err(), "no printer");
        assert!(dir.join("print.json").is_file());
        // a new session finds them again
        let mut again = open();
        let st = again.run("printui.state", json!({})).unwrap();
        assert_eq!(st["userTemplates"], json!(["Mine"]));
        assert_eq!(st["template"], "Mine");
        assert_eq!(st["settings"]["job"]["dpi"].as_f64(), Some(200.0));
        assert!((again.print.settings.page.size.w - 148.0 * 72.0 / 25.4).abs() < 0.1);
        // a damaged file is ignored, never a crash
        std::fs::write(dir.join("print.json"), b"{not json").unwrap();
        let mut broken = open();
        assert!(broken.run("printui.state", json!({})).unwrap()["userTemplates"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn prints_a_contact_sheet_pdf_in_the_background() {
        use std::sync::{Arc, Mutex};
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let written = bytes.clone();
        let services = crate::Services {
            write_shared: Some(Arc::new(move |path: &str, data: &[u8]| {
                assert_eq!(path, "Print.pdf");
                *written.lock().unwrap() = data.to_vec();
                Ok(())
            })),
            ..Default::default()
        };
        let mut app = DacApp::new(dac_engine::Session::with_demo(), services);
        app.run("module.print", json!({})).unwrap();
        app.run("printui.template", json!({"name": "2×2 Cells"})).unwrap();
        let st = app.run("printui.set", json!({"settings": {"options": {"pageNumbers": true}, "job": {"dpi": 72}}})).unwrap();
        assert!(st["pages"].as_u64().unwrap() >= 1);
        assert!(app.run("printui.set", json!({"settings": {"job": {"dpi": -5}}})).is_err());
        assert!(app.run("printui.template", json!({"name": "nope"})).is_err());
        app.run("printui.print", json!({"path": "Print.pdf"})).unwrap();
        let mut h = crate::headless::Headless::new(app, [1200.0, 800.0], 1.0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while h.app.print.busy && std::time::Instant::now() < deadline {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!h.app.print.busy, "print did not finish");
        let last = h.app.print.last.clone().unwrap();
        assert!(last.get("error").is_none(), "{last}");
        assert!(bytes.lock().unwrap().starts_with(b"%PDF-"));
        // printer destination without a printer is refused up front
        assert!(h.app.run("printui.print", json!({"destination": "printer"})).is_err());
    }
}
