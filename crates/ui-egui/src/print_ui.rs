//! The Print module: template browser (left), page canvas with rulers and guides (centre),
//! layout, image, page and print-job panels (right), filmstrip (bottom).
//!
//! The settings are a [`lcc_print::PrintSettings`]; the pages come from
//! [`lcc_print::PrintSettings::build`] over the selected photos (all photos in the filmstrip when
//! none is selected… as Print One prints just the active photo). The canvas draws the same
//! layout from thumbnails; Print renders every photo with the export pipeline at the print
//! resolution on a worker thread and writes a PDF or JPEG files, or sends the PDF to an IPP/CUPS
//! printer.
//!
//! Commands (`printui.*`, listed with the module shell's): `printui.template {name}`,
//! `printui.set {settings}` (merged into the current settings), `printui.pageSetup {paper,
//! landscape}`, `printui.page {page | delta}`, `printui.guides {...}`, `printui.print {destination,
//! path, printer}`, `printui.printOne`, `printui.printers {server}`, `printui.saveTemplate {name}`,
//! `printui.state`, `printui.saveCreation {name}` (a Saved Print: a collection holding the photos,
//! the page layout and these settings), `printui.openCreation {id}`, `printui.driver {mediaType,
//! mediaSource, printQuality, sides}` (the printer's own options; `null` leaves one to the
//! printer), `printui.trustCertificate {fingerprint?, uri?}` (an `ipps://` printer's certificate)
//! and `printui.trustOnFirstUse {on}`.

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lcc_catalog::PhotoId;
use lcc_engine::print::layout::Unit;
use lcc_layout::tokens::{PhotoInfo, expand};
use lcc_layout::{Align, CellKind, Document, Fit};
use lcc_print::job::{PAPERS, builtin_templates};
use lcc_print::{Destination, LayoutStyle, PrintSettings};
use serde_json::{Value, json};

use crate::LccApp;
use crate::module::{Edge, Module, ModuleId, ModuleKey, PanelId};
use crate::theme::Tokens;
use crate::widgets::register;

/// Most photos one print job takes from the filmstrip.
const MAX_JOB_PHOTOS: usize = lcc_print::job::MAX_PHOTOS;
const RULER: f32 = 18.0;
/// The selected custom cell's resize handle, screen points.
const RESIZE_HANDLE: f32 = 10.0;

/// A user template: `{name, folder, settings}`; `folder` `None` is "User Templates".
#[derive(Clone, Debug, PartialEq)]
pub struct UserTemplate {
    pub name: String,
    pub folder: Option<String>,
    pub settings: PrintSettings,
}

impl lcc_engine::template_folders::Foldered for UserTemplate {
    fn name(&self) -> &str {
        &self.name
    }
    fn folder(&self) -> Option<&str> {
        self.folder.as_deref()
    }
    fn set_folder(&mut self, folder: Option<String>) {
        self.folder = folder;
    }
}

/// The module's state (lives in [`LccApp::print`]).
#[derive(Clone, Debug)]
pub struct PrintUi {
    pub settings: PrintSettings,
    /// The template last applied.
    pub template: Option<String>,
    /// User templates, saved with `printui.saveTemplate`.
    pub user_templates: Vec<UserTemplate>,
    /// The Template Browser's folders besides "User Templates" (empty ones too), sorted by name.
    pub template_folders: Vec<String>,
    /// Preview page (0-based).
    pub page: usize,
    /// Master switch for the guides (margins, image cells, bleed, dimensions); the rulers have their own.
    pub show: bool,
    pub rulers: bool,
    pub margins: bool,
    pub cells: bool,
    pub bleed: bool,
    pub dimensions: bool,
    /// View ▸ Show Info Overlay: paper, printer, page and resolution in a corner box (saved in print.json).
    pub info_overlay: bool,
    pub printer_uri: String,
    pub cups_server: String,
    pub printers: Vec<lcc_print::ipp::PrinterEntry>,
    pub output_path: String,
    pub busy: bool,
    pub last: Option<Value>,
    /// The Saved Print (collection) last saved or opened.
    pub creation: Option<u64>,
    /// Paper sizes the chosen printer reports (IPP `media-col-database` / `media-supported`).
    pub media: Vec<lcc_print::ipp::Media>,
    /// `<config>/print.json` has been read.
    pub loaded: bool,
    /// What was last written to (or read from) `print.json`, to save only on change.
    pub stored: Option<Value>,
    /// The contents whose write to `print.json` failed, and how many attempts were made.
    pub save_failed: Option<(Value, u32)>,
    /// The printer profile's description (Color Management ▸ Profile).
    pub profile_name: Option<String>,
    /// The options the chosen printer offers (paper type, tray, quality, two-sided).
    pub options: lcc_print::ipp::PrinterOptions,
    /// An `ipps://` printer whose certificate was refused: (`host:port`, SHA-256 fingerprint).
    pub untrusted: Option<(String, String)>,
    /// Printer certificates trusted by fingerprint (`host:port`, fingerprint), saved in print.json.
    pub trusted_certs: Vec<(String, String)>,
    /// Trust an `ipps://` printer's certificate the first time it is seen.
    pub trust_on_first_use: bool,
    /// Unit of the rulers, cell dimensions and size fields (saved in print.json).
    pub units: Unit,
    /// Custom Package: the cell chosen on the page, whose size and place the panel edits.
    pub selected: Option<usize>,
    /// Size of the next custom cell, points.
    pub new_cell: (f32, f32),
    /// Page Setup… was chosen: open the paper choice on the next frame (`dialog.pageSetup`).
    pub open_paper: bool,
}

impl Default for PrintUi {
    fn default() -> Self {
        let (name, settings) = builtin_templates().into_iter().next().unwrap_or_else(|| ("Default".into(), PrintSettings::default()));
        PrintUi {
            settings,
            template: Some(name),
            user_templates: Vec::new(),
            template_folders: Vec::new(),
            page: 0,
            show: true,
            rulers: true,
            margins: true,
            cells: true,
            bleed: true,
            dimensions: false,
            info_overlay: false,
            printer_uri: String::new(),
            cups_server: "localhost:631".into(),
            printers: Vec::new(),
            output_path: String::new(),
            profile_name: None,
            busy: false,
            last: None,
            creation: None,
            media: Vec::new(),
            loaded: false,
            stored: None,
            options: Default::default(),
            untrusted: None,
            trusted_certs: Vec::new(),
            trust_on_first_use: false,
            save_failed: None,
            units: Unit::Inches,
            selected: None,
            new_cell: (360.0, 504.0),
            open_paper: false,
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
    ("printui.toggleGuides", "Show Guides", None, ""),
    ("printui.toggleRulers", "Show Rulers", None, ""),
    ("printui.infoOverlay", "Show Info Overlay", None, ""),
    ("printui.print", "Print", None, ""),
    ("printui.printOne", "Print One", None, ""),
    ("printui.printers", "Find Printers", None, ""),
    ("printui.profile", "Printer Color Profile", None, ""),
    ("printui.saveTemplate", "New Print Template", None, ""),
    ("printui.deleteTemplate", "Delete Print Template", None, ""),
    ("printui.createTemplateFolder", "New Print Template Folder", None, ""),
    ("printui.renameTemplateFolder", "Rename Print Template Folder", None, ""),
    ("printui.deleteTemplateFolder", "Delete Print Template Folder", None, ""),
    ("printui.moveTemplate", "Move Print Template to Folder", None, ""),
    ("printui.media", "Printer Paper Sizes", None, ""),
    ("printui.state", "Print State", None, ""),
    ("printui.saveCreation", "Create Saved Print", None, ""),
    ("printui.openCreation", "Open Saved Print", None, ""),
    ("printui.driver", "Printer Options", None, ""),
    ("printui.trustCertificate", "Trust Printer Certificate", None, ""),
    ("printui.trustOnFirstUse", "Trust Printer Certificates on First Use", None, ""),
    ("printui.cell", "Print Cell Size", None, ""),
];

/// Push the trusted printer certificates and the first-use setting to the IPP client.
fn apply_trust(pu: &PrintUi) {
    lcc_print::ipp::set_trusted_certificates(&pu.trusted_certs);
    lcc_print::ipp::set_trust_on_first_use(pu.trust_on_first_use);
}

/// Take in certificates the IPP client trusted on first use (so they are saved).
fn sync_trust(pu: &mut PrintUi) {
    pu.trusted_certs = lcc_print::ipp::trusted_certificates();
}

/// The photos a print takes: the filmstrip's selection when it has one, else every photo in the
/// filmstrip (Print One: the active photo).
pub fn job_photos(app: &mut LccApp, one: bool) -> Vec<PhotoId> {
    if one {
        return app.session.active().into_iter().collect();
    }
    // the photos the filmstrip selects are what prints (Lightroom's Print module works on the
    // filmstrip selection); only with nothing selected does the job fall back to the whole view
    let sel = app.session.selection.ids.clone();
    if !sel.is_empty() {
        return sel.into_iter().take(MAX_JOB_PHOTOS).collect();
    }
    app.session.visible().iter().copied().take(MAX_JOB_PHOTOS).collect()
}

fn ids_text(ids: &[PhotoId]) -> Vec<String> {
    ids.iter().map(|i| i.0.to_string()).collect()
}

/// Token values of a catalog photo.
fn info_of(app: &LccApp, id: &str) -> PhotoInfo {
    id.parse::<u64>().ok().and_then(|n| app.session.catalog.photo(PhotoId(n))).map(|p| lcc_engine::creations::photo_info(p)).unwrap_or_default()
}

/// The document for the current settings and photos.
fn document(app: &mut LccApp) -> Result<(Document, Vec<PhotoId>), String> {
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
pub fn enabled(app: &LccApp, id: &str) -> Option<bool> {
    if !COMMANDS.iter().any(|c| c.0 == id) {
        return None;
    }
    Some(match id {
        "printui.print" | "printui.printOne" => !app.print.busy && !app.session.catalog.is_empty(),
        _ => true,
    })
}

/// Run one of our commands; `None`: not one.
pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if !COMMANDS.iter().any(|c| c.0 == id) {
        return None;
    }
    ensure_loaded(app);
    let r = run_inner(app, id, p);
    if r.is_ok() {
        persist_reported(app, None);
    }
    Some(r)
}

// ---------------------------------------------------------------- persistence

/// `<config>/print.json`: the user templates, the current settings and template, the printer.
#[cfg(not(target_arch = "wasm32"))]
fn store_path(app: &LccApp) -> Option<std::path::PathBuf> {
    app.session.remote.connections_path.as_ref().and_then(|p| p.parent()).map(|d| d.join("print.json"))
}

/// The web build keeps print settings for the session only.
#[cfg(target_arch = "wasm32")]
fn store_path(_app: &LccApp) -> Option<std::path::PathBuf> {
    None
}

fn stored_json(pu: &PrintUi) -> Value {
    json!({
        "settings": serde_json::to_value(&pu.settings).unwrap_or(Value::Null),
        "template": pu.template,
        "userTemplates": pu.user_templates.iter().map(|t| {
            let mut v = json!({"name": t.name, "settings": serde_json::to_value(&t.settings).unwrap_or(Value::Null)});
            if let (Some(f), Some(o)) = (&t.folder, v.as_object_mut()) {
                o.insert("folder".into(), json!(f));
            }
            v
        }).collect::<Vec<_>>(),
        "templateFolders": pu.template_folders,
        "printerUri": pu.printer_uri,
        "cupsServer": pu.cups_server,
        "trustedCertificates": pu.trusted_certs.iter().map(|(h, f)| json!({"printer": h, "fingerprint": f})).collect::<Vec<_>>(),
        "trustOnFirstUse": pu.trust_on_first_use,
        "units": pu.units,
        "infoOverlay": pu.info_overlay,
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
            .filter_map(|t| {
                Some(UserTemplate {
                    name: t.get("name")?.as_str()?.trim().to_string(),
                    folder: t.get("folder").and_then(Value::as_str).map(str::to_string),
                    settings: settings(t.get("settings")?)?,
                })
            })
            .filter(|t| !t.name.is_empty())
            .take(500)
            .collect();
    }
    if let Some(list) = v.get("templateFolders").and_then(Value::as_array) {
        pu.template_folders = list.iter().filter_map(|f| f.as_str().map(str::to_string)).collect();
    }
    // folders and templates agree: bad or repeated folder names dropped, a template in an unlisted
    // folder gets it listed (or goes to User Templates)
    lcc_engine::template_folders::tidy_folders(&mut pu.template_folders, &mut pu.user_templates);
    if let Some(u) = v.get("printerUri").and_then(Value::as_str) {
        pu.printer_uri = u.to_string();
    }
    if let Some(u) = v.get("cupsServer").and_then(Value::as_str).filter(|u| !u.trim().is_empty()) {
        pu.cups_server = u.to_string();
    }
    if let Some(list) = v.get("trustedCertificates").and_then(Value::as_array) {
        pu.trusted_certs = list
            .iter()
            .filter_map(|c| Some((c.get("printer")?.as_str()?.to_string(), c.get("fingerprint")?.as_str()?.to_string())))
            .filter(|(_, f)| lcc_print::ipp::is_fingerprint(f))
            .take(500)
            .collect();
    }
    if let Some(on) = v.get("trustOnFirstUse").and_then(Value::as_bool) {
        pu.trust_on_first_use = on;
    }
    if let Some(u) = v.get("units").and_then(|u| serde_json::from_value::<Unit>(u.clone()).ok()) {
        pu.units = u;
    }
    if let Some(on) = v.get("infoOverlay").and_then(Value::as_bool) {
        pu.info_overlay = on;
    }
}

/// Read `print.json` once per session.
pub fn ensure_loaded(app: &mut LccApp) {
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
    apply_trust(&app.print);
    app.print.stored = Some(stored_json(&app.print));
}

/// Attempts to write the same `print.json` contents before waiting for the next change.
const SAVE_ATTEMPTS: u32 = 3;

/// Write `print.json` when the settings, templates or printer changed since the last successful
/// write. A failed write is retried (up to [`SAVE_ATTEMPTS`] times for the same contents, then
/// again on the next change); every failure is returned so the caller can report it.
pub fn persist(app: &mut LccApp) -> Result<(), String> {
    let now = stored_json(&app.print);
    if app.print.stored.as_ref() == Some(&now) {
        return Ok(());
    }
    if let Some((v, n)) = &app.print.save_failed
        && *v == now
        && *n >= SAVE_ATTEMPTS
    {
        return Ok(());
    }
    let Some(path) = store_path(app) else { return Ok(()) };
    let r = serde_json::to_vec_pretty(&now)
        .map_err(|e| e.to_string())
        .and_then(|bytes| lcc_engine::export::write_file_durable(&path.to_string_lossy(), &bytes));
    match r {
        Ok(()) => {
            app.print.stored = Some(now);
            app.print.save_failed = None;
            Ok(())
        }
        Err(e) => {
            let n = match &app.print.save_failed {
                Some((v, n)) if *v == now => n + 1,
                _ => 1,
            };
            app.print.save_failed = Some((now, n));
            Err(format!("{}: {e}", path.display()))
        }
    }
}

/// [`persist`], logging a failure and telling the user once the retries for it are used up.
fn persist_reported(app: &mut LccApp, ctx: Option<&egui::Context>) {
    if let Err(e) = persist(app) {
        log::warn!("print settings not saved: {e}");
        if let (Some(ctx), Some((_, SAVE_ATTEMPTS))) = (ctx, &app.print.save_failed) {
            app.toast(ctx, crate::i18n::tr_format!("Print settings not saved: {e}", e = e));
        }
    }
}

fn state_json(app: &mut LccApp) -> Value {
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
        "infoOverlay": pu.info_overlay,
        "guides": {"show": pu.show, "rulers": pu.rulers, "margins": pu.margins, "cells": pu.cells, "bleed": pu.bleed, "dimensions": pu.dimensions, "units": pu.units},
        "selectedCell": pu.selected,
        "printer": pu.printer_uri,
        "profileName": pu.profile_name,
        "printers": pu.printers.iter().map(|p| json!({"name": p.name, "uri": p.uri, "info": p.info})).collect::<Vec<_>>(),
        "busy": pu.busy,
        "creation": pu.creation,
        "last": pu.last,
        "templates": builtin_templates().into_iter().map(|t| t.0).chain(pu.user_templates.iter().map(|t| t.name.clone())).collect::<Vec<_>>(),
        "userTemplates": pu.user_templates.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
        "templateFolders": crate::template_browser::listing(&pu.template_folders, &pu.user_templates),
        "media": pu.media.iter().map(|m| json!({"name": m.name, "w": m.size.w, "h": m.size.h, "margins": m.margins})).collect::<Vec<_>>(),
        "printerOptions": {
            "mediaTypes": pu.options.media_types, "mediaTypeDefault": pu.options.media_type_default,
            "mediaSources": pu.options.media_sources, "mediaSourceDefault": pu.options.media_source_default,
            "qualities": pu.options.qualities, "qualityDefault": pu.options.quality_default,
            "sides": pu.options.sides, "sidesDefault": pu.options.sides_default,
        },
        "driver": serde_json::to_value(&pu.settings.job.driver).unwrap_or(Value::Null),
        "untrustedCertificate": pu.untrusted.as_ref().map(|(h, f)| json!({"printer": h, "fingerprint": f})),
        "trustedCertificates": pu.trusted_certs.iter().map(|(h, f)| json!({"printer": h, "fingerprint": f})).collect::<Vec<_>>(),
        "trustOnFirstUse": pu.trust_on_first_use,
    })
}

/// Smallest and largest cell side, points (0.1 in and 30 in).
const MIN_CELL_PT: f32 = 7.2;
const MAX_CELL_PT: f32 = 2160.0;
/// Most cells a package holds.
const MAX_CELLS: usize = 64;

/// Points in one `unit`.
fn pt_per(unit: Unit) -> f32 {
    (72.0 / unit.per_inch()) as f32
}

/// A length in points shown in `unit`, e.g. "12.70 cm".
fn fmt_len(pt: f32, unit: Unit) -> String {
    format!("{} {}", fmt_num(pt, unit), unit.label())
}

/// A length in points as a number in `unit`, without its label.
fn fmt_num(pt: f32, unit: Unit) -> String {
    let d = match unit {
        Unit::Inches | Unit::Centimeters => 2,
        Unit::Millimeters | Unit::Points => 1,
    };
    let v = format!("{:.*}", d, pt / pt_per(unit));
    if v.contains('.') { v.trim_end_matches('0').trim_end_matches('.').to_string() } else { v }
}

/// Typed text ("12.5", "12,5", "12.5 cm") as points; a unit typed after the number wins over `unit`.
fn parse_len(text: &str, unit: Unit) -> Option<f32> {
    let t = text.trim().replace(',', ".");
    let split = t.find(|c: char| c.is_ascii_alphabetic() || c == '"').unwrap_or(t.len());
    let (num, suffix) = t.split_at(split);
    let unit = match suffix.trim() {
        "" => unit,
        "in" | "\"" => Unit::Inches,
        "cm" => Unit::Centimeters,
        "mm" => Unit::Millimeters,
        "pt" => Unit::Points,
        _ => return None,
    };
    let v: f32 = num.trim().parse().ok()?;
    (v.is_finite() && v > 0.0).then(|| v * pt_per(unit))
}

/// A `printui.cell` number in `unit`, as points.
fn len_param(p: &Value, key: &str, unit: Unit) -> Result<Option<f32>, String> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let n = v.as_f64().filter(|n| n.is_finite()).ok_or_else(|| format!("{key} must be a number"))?;
            Ok(Some(n as f32 * pt_per(unit)))
        }
    }
}

fn check_side(key: &str, pt: f32, unit: Unit) -> Result<f32, String> {
    if (MIN_CELL_PT..=MAX_CELL_PT).contains(&pt) {
        Ok(pt)
    } else {
        Err(format!("{key} must be between {} and {}", fmt_len(MIN_CELL_PT, unit), fmt_len(MAX_CELL_PT, unit)))
    }
}

/// `printui.cell`: add (no `index`), resize or move (`index` with `w`, `h`, `x`, `y`), or remove
/// (`index`, `remove: true`) a Picture Package or Custom Package cell; lengths are in `unit`.
fn edit_cell(s: &mut PrintSettings, p: &Value, unit: Unit) -> Result<Value, String> {
    let index = match p.get("index") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_u64().ok_or("index must be a cell number from 0")? as usize),
    };
    let remove = p.get("remove").and_then(Value::as_bool).unwrap_or(false);
    let w = len_param(p, "w", unit)?.map(|v| check_side("w", v, unit)).transpose()?;
    let h = len_param(p, "h", unit)?.map(|v| check_side("h", v, unit)).transpose()?;
    let x = len_param(p, "x", unit)?;
    let y = len_param(p, "y", unit)?;
    let content = s.page.content();
    let size = s.page.size;
    let (index, cells) = match &mut s.layout {
        LayoutStyle::SingleImage { .. } => return Err("cells are sized in Picture Package and Custom Package; the grid sizes its own".into()),
        LayoutStyle::PicturePackage(spec) => {
            if x.is_some() || y.is_some() {
                return Err("Picture Package places its cells itself: give w and h only".into());
            }
            let i = match (index, remove) {
                (None, _) => {
                    if spec.cells.len() >= MAX_CELLS {
                        return Err(format!("a package holds at most {MAX_CELLS} cells"));
                    }
                    let (w, h) = w.zip(h).ok_or("a new cell needs w and h")?;
                    spec.cells.push(lcc_layout::Size::new(w, h));
                    spec.cells.len() - 1
                }
                (Some(i), true) => {
                    if i >= spec.cells.len() {
                        return Err(format!("no cell {i}"));
                    }
                    spec.cells.remove(i);
                    i
                }
                (Some(i), false) => {
                    let c = spec.cells.get_mut(i).ok_or_else(|| format!("no cell {i}"))?;
                    c.w = w.unwrap_or(c.w);
                    c.h = h.unwrap_or(c.h);
                    i
                }
            };
            (i, spec.cells.iter().map(|c| json!({"w": c.w, "h": c.h})).collect::<Vec<_>>())
        }
        LayoutStyle::CustomPackage { pages } => {
            if pages.is_empty() {
                pages.push(lcc_layout::PageLayout { cells: Vec::new(), guides: Vec::new() });
            }
            let Some(pg) = pages.first_mut() else { return Err("no page".into()) };
            // keep a cell on the paper
            let place = |r: &mut lcc_layout::Rect| {
                r.w = r.w.min(size.w);
                r.h = r.h.min(size.h);
                r.x = r.x.clamp(0.0, (size.w - r.w).max(0.0));
                r.y = r.y.clamp(0.0, (size.h - r.h).max(0.0));
            };
            let i = match (index, remove) {
                (None, _) => {
                    if pg.cells.len() >= MAX_CELLS {
                        return Err(format!("a package holds at most {MAX_CELLS} cells"));
                    }
                    let (w, h) = w.zip(h).ok_or("a new cell needs w and h")?;
                    let mut r = lcc_layout::Rect::new(x.unwrap_or(content.x), y.unwrap_or(content.y), w, h);
                    place(&mut r);
                    pg.cells.push(lcc_layout::Cell::photo(r));
                    pg.cells.len() - 1
                }
                (Some(i), true) => {
                    if i >= pg.cells.len() {
                        return Err(format!("no cell {i}"));
                    }
                    pg.cells.remove(i);
                    i
                }
                (Some(i), false) => {
                    let c = pg.cells.get_mut(i).ok_or_else(|| format!("no cell {i}"))?;
                    let r = &mut c.rect;
                    r.w = w.unwrap_or(r.w);
                    r.h = h.unwrap_or(r.h);
                    r.x = x.unwrap_or(r.x);
                    r.y = y.unwrap_or(r.y);
                    place(r);
                    i
                }
            };
            (i, pg.cells.iter().map(|c| json!({"x": c.rect.x, "y": c.rect.y, "w": c.rect.w, "h": c.rect.h})).collect::<Vec<_>>())
        }
    };
    let index = (!remove).then_some(index);
    Ok(json!({"index": index, "cells": cells, "unit": unit}))
}

/// "Custom size: [w] × [h] unit [Add]": true when Add was clicked (the size is in `cell`).
fn new_cell_row(ui: &mut egui::Ui, ctx: &egui::Context, cell: &mut (f32, f32), unit: Unit) -> bool {
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Custom Size"));
        if let Some(v) = len_field(ui, "print:newCellW", cell.0, unit) {
            cell.0 = v;
        }
        ui.label("×");
        if let Some(v) = len_field(ui, "print:newCellH", cell.1, unit) {
            cell.1 = v;
        }
        ui.label(unit.label());
        let b = ui.button(crate::i18n::tr("Add Cell"));
        register(ctx, "print:addCustomCell", b.rect);
        b.clicked()
    })
    .inner
}

/// A position field in the print unit (0 allowed): the new position in points once committed.
fn pos_field(ui: &mut egui::Ui, id: &str, pt: f32, unit: Unit) -> Option<f32> {
    let key = egui::Id::new(("print-len", id));
    let mut text = ui.data_mut(|d| d.get_temp::<String>(key)).unwrap_or_else(|| fmt_num(pt, unit));
    let r = crate::text_field::TextField::singleline(id, &mut text).width(52.0).select_on_focus(true).show(ui);
    let out = if r.committed() {
        let t = text.trim();
        if t.trim_end_matches(|c: char| c.is_ascii_alphabetic()).trim().parse::<f32>() == Ok(0.0) { Some(0.0) } else { parse_len(t, unit) }
    } else {
        None
    };
    if r.editing && !r.committed() && !r.cancelled() {
        ui.data_mut(|d| d.insert_temp(key, text));
    } else {
        ui.data_mut(|d| d.remove::<String>(key));
    }
    out
}

/// A length field in the print unit: the new length in points once an edit is committed.
fn len_field(ui: &mut egui::Ui, id: &str, pt: f32, unit: Unit) -> Option<f32> {
    let key = egui::Id::new(("print-len", id));
    let mut text = ui.data_mut(|d| d.get_temp::<String>(key)).unwrap_or_else(|| fmt_num(pt, unit));
    let r = crate::text_field::TextField::singleline(id, &mut text).width(52.0).select_on_focus(true).show(ui);
    let out = if r.committed() { parse_len(&text, unit).map(|v| v.clamp(MIN_CELL_PT, MAX_CELL_PT)) } else { None };
    if r.editing && !r.committed() && !r.cancelled() {
        ui.data_mut(|d| d.insert_temp(key, text));
    } else {
        ui.data_mut(|d| d.remove::<String>(key));
    }
    out
}

fn run_inner(app: &mut LccApp, id: &str, p: &Value) -> Result<Value, String> {
    match id {
        "printui.template" => {
            let name = p.get("name").and_then(Value::as_str).ok_or("missing name")?;
            let found = builtin_templates()
                .into_iter()
                .chain(app.print.user_templates.iter().map(|t| (t.name.clone(), t.settings.clone())))
                .find(|(n, _)| n == name);
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
            let (size, margins) = match (p.get("paper").and_then(Value::as_str), p.get("media").and_then(Value::as_str)) {
                (Some(k), _) => (lcc_print::job::paper(k).ok_or_else(|| format!("unknown paper {k}"))?.size, None),
                (None, Some(m)) => match app.print.media.iter().find(|x| x.name == m) {
                    Some(x) => (x.size, Some(x.margins)),
                    None => (lcc_print::ipp::pwg_media_size(m).ok_or_else(|| format!("unknown printer paper {m}"))?, None),
                },
                (None, None) => (cur, None),
            };
            let s = &mut app.print.settings;
            s.set_paper(size, landscape);
            // the printer's own paper: keep the layout inside what it can print
            if let Some(m) = margins {
                s.fit_printer_margins(m);
            }
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
        "printui.infoOverlay" => {
            let pu = &mut app.print;
            pu.info_overlay = p.get("on").and_then(Value::as_bool).unwrap_or(!pu.info_overlay);
        }
        "printui.guides" => {
            let pu = &mut app.print;
            for (k, f) in [
                ("show", &mut pu.show),
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
            if let Some(u) = p.get("units") {
                pu.units = serde_json::from_value(u.clone()).map_err(|_| format!("unknown unit {u}: in, cm, mm or pt"))?;
            }
        }
        "printui.toggleGuides" => app.print.show = !app.print.show,
        "printui.toggleRulers" => app.print.rulers = !app.print.rulers,
        "printui.cell" => {
            let unit = match p.get("unit") {
                Some(u) => serde_json::from_value(u.clone()).map_err(|_| format!("unknown unit {u}: in, cm, mm or pt"))?,
                None => app.print.units,
            };
            let out = edit_cell(&mut app.print.settings, p, unit)?;
            if matches!(app.print.settings.layout, LayoutStyle::CustomPackage { .. }) {
                app.print.selected = out.get("index").and_then(Value::as_u64).map(|i| i as usize);
            }
            return Ok(out);
        }
        "printui.saveTemplate" => {
            let name = p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).ok_or("missing name")?.to_string();
            let settings = app.print.settings.clone();
            let given = crate::template_browser::folder_param(&app.print.template_folders, p)?;
            // a template saved over its own name stays in its folder unless the call names another
            let kept = app.print.user_templates.iter().find(|t| t.name == name).and_then(|t| t.folder.clone());
            let folder = given.unwrap_or(kept);
            app.print.user_templates.retain(|t| t.name != name);
            app.print.user_templates.push(UserTemplate { name: name.clone(), folder, settings });
            app.print.template = Some(name);
        }
        "printui.createTemplateFolder" | "printui.renameTemplateFolder" | "printui.deleteTemplateFolder" | "printui.moveTemplate" => {
            let op = match id {
                "printui.createTemplateFolder" => "create",
                "printui.renameTemplateFolder" => "rename",
                "printui.deleteTemplateFolder" => "delete",
                _ => "move",
            };
            let pu = &mut app.print;
            return crate::template_browser::folder_command(op, &mut pu.template_folders, &mut pu.user_templates, p);
        }
        "printui.deleteTemplate" => {
            let name = p.get("name").and_then(Value::as_str).ok_or("missing name")?;
            let before = app.print.user_templates.len();
            app.print.user_templates.retain(|t| t.name != name);
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
                    move || lcc_print::ipp::printer_info(&uri),
                    |app, ctx, r| {
                        sync_trust(&mut app.print);
                        match r {
                            Ok(info) => {
                                let n = info.media.len();
                                app.print.media = info.media;
                                app.print.options = info.options;
                                app.print.untrusted = None;
                                app.toast(ctx, crate::i18n::tr_format!("{n} paper size(s) from the printer", n = n));
                            }
                            Err(lcc_print::PrintError::UntrustedCertificate { printer, fingerprint }) => {
                                let e = lcc_print::PrintError::UntrustedCertificate { printer: printer.clone(), fingerprint: fingerprint.clone() };
                                app.print.untrusted = Some((printer, fingerprint));
                                app.toast_error(ctx, e.to_string());
                            }
                            Err(e) => app.toast_error(ctx, e.to_string()),
                        }
                    },
                )?;
                return Ok(json!({"background": true}));
            }
            #[cfg(target_arch = "wasm32")]
            return Err("printing is not available in the browser".into());
        }
        "printui.driver" => {
            // each given key sets that option; null or "" leaves it to the printer
            let mut d = app.print.settings.job.driver.clone();
            let keyword = |k: &str| -> Result<Option<Option<String>>, String> {
                match p.get(k) {
                    None => Ok(None),
                    Some(Value::Null) => Ok(Some(None)),
                    Some(Value::String(v)) if v.trim().is_empty() => Ok(Some(None)),
                    Some(Value::String(v)) if v.len() <= 255 && v.chars().all(|c| c.is_ascii_graphic()) => Ok(Some(Some(v.clone()))),
                    Some(_) => Err(format!("{k} must be a printer keyword (no spaces) or null")),
                }
            };
            if let Some(v) = keyword("mediaType")? {
                d.media_type = v;
            }
            if let Some(v) = keyword("mediaSource")? {
                d.media_source = v;
            }
            if let Some(v) = keyword("sides")? {
                d.sides = v;
            }
            match p.get("printQuality") {
                None => {}
                Some(Value::Null) => d.print_quality = None,
                Some(v) => {
                    let q = v
                        .as_i64()
                        .or_else(|| match v.as_str() {
                            Some("draft") => Some(3),
                            Some("normal") => Some(4),
                            Some("high") => Some(5),
                            _ => None,
                        })
                        .filter(|q| (3..=5).contains(q))
                        .ok_or("printQuality must be 3 (draft), 4 (normal), 5 (high), their names, or null")?;
                    d.print_quality = Some(q as i32);
                }
            }
            app.print.settings.job.driver = d;
        }
        "printui.trustCertificate" => {
            let uri = p.get("uri").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| app.print.printer_uri.clone());
            let fp = match p.get("fingerprint").and_then(Value::as_str) {
                Some(f) => f.to_string(),
                None => app.print.untrusted.as_ref().map(|u| u.1.clone()).ok_or("no refused printer certificate: give fingerprint")?,
            };
            if uri.trim().is_empty() {
                return Err("no printer: give uri".into());
            }
            lcc_print::ipp::trust_certificate(&uri, &fp).map_err(|e| e.to_string())?;
            sync_trust(&mut app.print);
            app.print.untrusted = None;
            return Ok(json!({"trusted": app.print.trusted_certs.iter().map(|(h, f)| json!({"printer": h, "fingerprint": f})).collect::<Vec<_>>()}));
        }
        "printui.trustOnFirstUse" => {
            let on = p.get("on").and_then(Value::as_bool).ok_or("on must be true or false")?;
            app.print.trust_on_first_use = on;
            apply_trust(&app.print);
        }
        "printui.profile" => {
            // {path: "…icc"} picks a printer profile, {path: null} = Managed by Printer, {} asks
            let path = match p.get("path") {
                Some(Value::Null) => None,
                Some(Value::String(s)) if !s.trim().is_empty() => Some(s.clone()),
                Some(_) => return Err("path must be an ICC profile file or null".into()),
                None => {
                    let req = crate::pick::PickRequest::file("Choose Printer Profile", "ICC profile", &["icc", "icm"]);
                    match crate::pick::ask(app, id, p, "path", req, |_| None) {
                        crate::pick::Picked::Now(paths) => Some(paths.into_iter().next().ok_or("cancelled")?),
                        crate::pick::Picked::Later => return Ok(json!({"pending": "path"})),
                        crate::pick::Picked::Unavailable => return Err("no file dialog on this platform; supply path".into()),
                    }
                }
            };
            let mut color = app.print.settings.job.color.clone();
            color.profile = path;
            let job = lcc_print::color::ColorJob::load(&color).map_err(|e| e.to_string())?;
            app.print.settings.job.color = color;
            app.print.profile_name = job.profile_name;
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
                    move || lcc_print::send::find_printers(&server),
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
                    .album(lcc_catalog::AlbumId(*c))
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
fn start(app: &mut LccApp, id: &str, p: &Value) -> Result<Value, String> {
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
                return Err("choose a printer (Find Printers, or enter an ipp:// URI or, on Windows, the printer's name)".into());
            }
            lcc_print::send::check_printer(&printer).map_err(|e| e.to_string())?;
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
    // Color Management: read the printer profile now so a bad one is refused before rendering
    let color = lcc_print::color::ColorJob::load(&settings.job.color).map_err(|e| e.to_string())?;
    let depth = if settings.job.color.bit16 && settings.destination != Destination::Jpeg {
        lcc_pipeline::OutputDepth::U16
    } else {
        lcc_pipeline::OutputDepth::U8
    };
    let (jobs, infos) = lcc_engine::creations::prepare_jobs_depth(&mut app.session, &doc, dpi, depth)?;
    let pages = doc.pages.len();
    let title = app.print.template.clone().unwrap_or_else(|| "Print".into());
    let write = app.services.write_shared.clone();
    app.print.busy = true;
    let dest = settings.destination.clone();
    let work = move || -> Result<Value, String> {
        let mut src = lcc_engine::creations::run_jobs(jobs, infos)?;
        src.sharpen_for_print(settings.job.sharpening, settings.job.media);
        src.apply_print_color(&color);
        let text = lcc_print::text::ShapedText::with_system_fonts();
        let progress = &mut |_: usize, _: usize| true;
        let write_file = |path: &str, bytes: &[u8]| -> Result<(), String> {
            match &write {
                Some(w) => w(path, bytes),
                None => lcc_engine::export::write_file(path, bytes),
            }
        };
        match dest {
            Destination::Pdf | Destination::Printer => {
                let opt = lcc_print::output::PdfOptions {
                    metadata: lcc_pdf::Metadata {
                        title: Some(title.clone()),
                        creator: Some(lcc_brand::DISPLAY_NAME.to_string()),
                        ..Default::default()
                    },
                    output_intent: color.profile.clone().map(|profile| lcc_pdf::OutputIntent {
                        subtype: "GTS_PDFX".into(),
                        identifier: color.profile_name.clone().unwrap_or_else(|| "Printer profile".into()),
                        info: color.profile_name.clone(),
                        profile,
                        components: 3,
                    }),
                    photo_profile: color.profile.clone(),
                };
                if dest == Destination::Printer {
                    let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "user".into());
                    let send = lcc_print::send::SendOptions {
                        user,
                        title: title.clone(),
                        pdf: opt,
                        wait: std::time::Duration::from_secs(20),
                        poll: std::time::Duration::from_millis(500),
                    };
                    let sent =
                        lcc_print::send::send_to_printer(&printer, &doc, &settings, &src, &text, &send, progress).map_err(|e| e.to_string())?;
                    return Ok(json!({
                        "printer": printer,
                        "job": sent.jobs.last().copied().unwrap_or(0),
                        "jobs": sent.jobs,
                        "format": sent.format,
                        "copies": sent.copies,
                        "state": sent.state.as_ref().map(|s| s.label()),
                        "pages": sent.pages,
                        "warnings": sent.warnings,
                    }));
                }
                let out = lcc_print::output::to_pdf(&doc, &settings, &src, &text, &opt, progress).map_err(|e| e.to_string())?;
                let bytes = out.files.into_iter().next().unwrap_or_default();
                {
                    write_file(&path, &bytes)?;
                    Ok(json!({"path": path, "pages": out.pages, "bytes": bytes.len(), "warnings": out.warnings}))
                }
            }
            Destination::Jpeg => {
                let out = lcc_print::output::to_jpeg_icc(&doc, &settings, &src, &text, color.profile.as_deref().map(Vec::as_slice), progress)
                    .map_err(|e| e.to_string())?;
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
        sync_trust(&mut app.print);
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
    fn toolbar(&self, ui: &mut egui::Ui, app: &mut LccApp) {
        toolbar(ui, app);
    }
    fn center(&self, ui: &mut egui::Ui, app: &mut LccApp) {
        center(ui, app);
    }
    fn keymap(&self) -> &'static [ModuleKey] {
        PRINT_KEYS
    }
    fn command_prefixes(&self) -> &'static [&'static str] {
        &["print.", "printui."]
    }
}

fn cmd(app: &mut LccApp, ctx: &egui::Context, id: &str, p: Value) {
    if let Err(e) = app.run(id, p) {
        app.toast_error(ctx, e);
    }
}

fn toolbar(ui: &mut egui::Ui, app: &mut LccApp) {
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

fn center(ui: &mut egui::Ui, app: &mut LccApp) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    ensure_loaded(app);
    if !ctx.input(|i| i.pointer.any_down()) {
        persist_reported(app, Some(&ctx));
    }
    let mut area = ui.available_rect_before_wrap();
    if crate::module::edge_visible(app, Edge::Bottom) {
        let film = Rect::from_min_max(pos2(area.left(), area.bottom() - app.ui.film_height()), area.max);
        area.max.y = film.top();
        crate::panels::detail::filmstrip(app, ui, film);
    }
    ui.allocate_rect(area, Sense::hover());
    register(&ctx, "view:module:print", area);
    let p = ui.painter().clone();
    if crate::module::module_edge(app, Edge::Left) {
        let left = crate::panels::split_side(ui, &mut area, true, "print:left", &mut app.ui.left_width, crate::state::LEFT_WIDTH);
        p.rect_filled(left, 0.0, t.chrome);
        p.line_segment([left.right_top(), left.right_bottom()], Stroke::new(1.0, t.divider));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(left.shrink(10.0)));
        template_browser(&mut child, app);
    }
    if crate::module::module_edge(app, Edge::Right) {
        let right = crate::panels::split_side(ui, &mut area, false, "print:right", &mut app.ui.right_width, crate::state::RIGHT_WIDTH);
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

/// The commands the Template Browser runs.
const TEMPLATE_CMDS: crate::template_browser::Cmds = crate::template_browser::Cmds {
    apply: "printui.template",
    delete: "printui.deleteTemplate",
    move_to: "printui.moveTemplate",
    create_folder: "printui.createTemplateFolder",
    rename_folder: "printui.renameTemplateFolder",
    delete_folder: "printui.deleteTemplateFolder",
};

fn template_browser(ui: &mut egui::Ui, app: &mut LccApp) {
    let ctx = ui.ctx().clone();
    egui::ScrollArea::vertical().id_salt("print-templates").show(ui, |ui| {
        heading(ui, "Template Browser");
        let current = app.print.template.clone();
        let mut rows: Vec<crate::template_browser::Row> = builtin_templates()
            .into_iter()
            .map(|t| crate::template_browser::Row { shown: crate::i18n::tr(&t.0).to_string(), name: t.0, builtin: true, folder: None })
            .collect();
        rows.extend(app.print.user_templates.iter().map(|t| crate::template_browser::Row {
            name: t.name.clone(),
            shown: t.name.clone(),
            builtin: false,
            folder: t.folder.clone(),
        }));
        let ids = crate::template_browser::Ids { template: "print:template:", folder: "print:templateFolder:", salt: "print-template-folder" };
        let actions = crate::template_browser::show(ui, &ids, &rows, &app.print.template_folders, current.as_deref());
        crate::template_browser::dispatch(app, &ctx, actions, &TEMPLATE_CMDS);
        ui.add_space(6.0);
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
        for c in lcc_engine::creations::creations_of(&app.session, Some(lcc_layout::CreationKind::Print)) {
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
            ("show", "Show Guides", &mut g.show),
            ("rulers", "Rulers", &mut g.rulers),
            ("margins", "Margins and Gutters", &mut g.margins),
            ("cells", "Image Cells", &mut g.cells),
            ("bleed", "Page Bleed", &mut g.bleed),
            ("dimensions", "Dimensions", &mut g.dimensions),
        ] {
            let r = ui.checkbox(v, crate::i18n::tr(label));
            register(&ctx, format!("print:guide:{k}"), r.rect);
        }
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr("Units"));
            for u in [Unit::Inches, Unit::Centimeters, Unit::Millimeters] {
                let r = ui.selectable_label(g.units == u, u.label());
                register(&ctx, format!("print:units:{}", u.label()), r.rect);
                if r.clicked() {
                    g.units = u;
                }
            }
        });
    });
}

fn settings_panels(ui: &mut egui::Ui, app: &mut LccApp) {
    let ctx = ui.ctx().clone();
    let before = app.print.settings.clone();
    let unit = app.print.units;
    let mut selected = app.print.selected;
    let mut new_cell = app.print.new_cell;
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
                    0 => LayoutStyle::SingleImage { grid: lcc_layout::grid::Grid::new(1, 1) },
                    1 => LayoutStyle::PicturePackage(lcc_print::PackageSpec { cells: vec![lcc_layout::Size::inches(5.0, 7.0)], gap: 9.0 }),
                    _ => {
                        let c = s.page.content();
                        LayoutStyle::CustomPackage {
                            pages: vec![lcc_layout::PageLayout {
                                cells: vec![lcc_layout::Cell::photo(lcc_layout::Rect::new(c.x, c.y, c.w / 2.0, c.h / 2.0))],
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
            im.stroke = stroke.then_some(lcc_layout::Stroke { width: 1.0, color: lcc_layout::BLACK });
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
                            spec.cells.push(lcc_layout::Size::inches(w, h));
                        }
                    }
                });
                let mut remove = None;
                for (i, c) in spec.cells.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        if let Some(v) = len_field(ui, &format!("print:cellW:{i}"), c.w, unit) {
                            c.w = v;
                        }
                        ui.label("×");
                        if let Some(v) = len_field(ui, &format!("print:cellH:{i}"), c.h, unit) {
                            c.h = v;
                        }
                        ui.label(unit.label());
                        let r = ui.small_button("✕").on_hover_text(crate::i18n::tr("Remove Cell"));
                        register(&ctx, format!("print:removeCell:{i}"), r.rect);
                        if r.clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove.filter(|i| *i < spec.cells.len()) {
                    spec.cells.remove(i);
                }
                if new_cell_row(ui, &ctx, &mut new_cell, unit) && spec.cells.len() < MAX_CELLS {
                    spec.cells.push(lcc_layout::Size::new(new_cell.0, new_cell.1));
                }
                if ui.button(crate::i18n::tr("Clear Layout")).clicked() {
                    spec.cells.clear();
                }
                ui.add(egui::Slider::new(&mut spec.gap, 0.0..=36.0).text(crate::i18n::tr("Gap (pt)")));
            }
            LayoutStyle::CustomPackage { pages } => {
                heading(ui, "Cells");
                ui.label(crate::i18n::tr("Drag cells on the page to move them."));
                ui.label(crate::i18n::tr("Click a cell, then drag its corner to resize it."));
                ui.horizontal_wrapped(|ui| {
                    for (w, h) in [(2.5, 3.5), (4.0, 6.0), (5.0, 7.0), (8.0, 10.0)] {
                        if ui.button(format!("{w}×{h}")).clicked()
                            && let Some(pg) = pages.first_mut()
                        {
                            let c = s.page.content();
                            let sz = lcc_layout::Size::inches(w, h);
                            pg.cells.push(lcc_layout::Cell::photo(lcc_layout::Rect::new(c.x, c.y, sz.w.min(c.w), sz.h.min(c.h))));
                        }
                    }
                });
                if new_cell_row(ui, &ctx, &mut new_cell, unit)
                    && let Some(pg) = pages.first_mut()
                    && pg.cells.len() < MAX_CELLS
                {
                    let c = s.page.content();
                    pg.cells.push(lcc_layout::Cell::photo(lcc_layout::Rect::new(c.x, c.y, new_cell.0.min(c.w), new_cell.1.min(c.h))));
                    selected = Some(pg.cells.len() - 1);
                }
                let size = s.page.size;
                if let Some(pg) = pages.first_mut()
                    && let Some(i) = selected.filter(|i| *i < pg.cells.len())
                {
                    ui.label(crate::i18n::tr_format!("Selected cell {n}", n = i + 1));
                    let mut remove = false;
                    if let Some(c) = pg.cells.get_mut(i) {
                        let r = &mut c.rect;
                        egui::Grid::new("print-selected-cell").num_columns(4).show(ui, |ui| {
                            ui.label(crate::i18n::tr("W"));
                            if let Some(v) = len_field(ui, "print:selW", r.w, unit) {
                                r.w = v.min(size.w);
                            }
                            ui.label(crate::i18n::tr("H"));
                            if let Some(v) = len_field(ui, "print:selH", r.h, unit) {
                                r.h = v.min(size.h);
                            }
                            ui.end_row();
                            // positions may be 0: parsed without the cell-size limits
                            ui.label(crate::i18n::tr("X"));
                            if let Some(v) = pos_field(ui, "print:selX", r.x, unit) {
                                r.x = v.clamp(0.0, (size.w - r.w).max(0.0));
                            }
                            ui.label(crate::i18n::tr("Y"));
                            if let Some(v) = pos_field(ui, "print:selY", r.y, unit) {
                                r.y = v.clamp(0.0, (size.h - r.h).max(0.0));
                            }
                            ui.end_row();
                        });
                        let b = ui.button(crate::i18n::tr("Remove Cell"));
                        register(&ctx, "print:removeSelected", b.rect);
                        remove = b.clicked();
                    }
                    if remove {
                        pg.cells.remove(i);
                        selected = None;
                    }
                }
                if ui.button(crate::i18n::tr("Clear Layout")).clicked()
                    && let Some(pg) = pages.first_mut()
                {
                    pg.cells.clear();
                    selected = None;
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
            let r = crate::color_readout::on_picker(ui.color_edit_button_srgba(&mut bg), [bg.r(), bg.g(), bg.b()]);
            crate::access::label(&r, "Page Background Color");
            if r.changed() {
                o.background = [bg.r(), bg.g(), bg.b(), 255];
            }
        });
        let mut plate = o.identity_plate.is_some();
        let r = ui.checkbox(&mut plate, crate::i18n::tr("Identity Plate"));
        register(&ctx, "print:identityPlate", r.rect);
        if r.changed() {
            o.identity_plate = plate.then(|| lcc_print::job::IdentityPlate { text: lcc_brand::DISPLAY_NAME.to_string(), ..Default::default() });
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
        ui.horizontal(|ui| {
            use lcc_print::PrintSharpening;
            let mut on = s.job.sharpening != PrintSharpening::Off;
            let r = ui.checkbox(&mut on, crate::i18n::tr("Print Sharpening"));
            register(&ctx, "print:sharpening", r.rect);
            if r.changed() {
                s.job.sharpening = if on { PrintSharpening::Standard } else { PrintSharpening::Off };
            }
            if on {
                let levels = [(PrintSharpening::Low, "Low"), (PrintSharpening::Standard, "Standard"), (PrintSharpening::High, "High")];
                let cur = levels.iter().find(|l| l.0 == s.job.sharpening).map_or("Standard", |l| l.1);
                egui::ComboBox::from_id_salt("print-sharpen-amount").selected_text(crate::i18n::tr(cur)).show_ui(ui, |ui| {
                    for (v, label) in levels {
                        ui.selectable_value(&mut s.job.sharpening, v, crate::i18n::tr(label));
                    }
                });
            }
        });
        if s.job.sharpening != lcc_print::PrintSharpening::Off {
            ui.horizontal(|ui| {
                use lcc_print::PrintMedia;
                ui.label(crate::i18n::tr("Media Type"));
                for (v, label) in [(PrintMedia::Glossy, "Glossy"), (PrintMedia::Matte, "Matte")] {
                    let r = ui.selectable_label(s.job.media == v, crate::i18n::tr(label));
                    if r.clicked() {
                        s.job.media = v;
                    }
                }
            });
        }
        color_management(ui, app, &ctx);
        let s = &mut app.print.settings;
        if s.destination == Destination::Jpeg {
            ui.add(egui::Slider::new(&mut s.job.jpeg_quality, 10..=100).text(crate::i18n::tr("JPEG Quality")));
        }
        if s.destination == Destination::Printer {
            ui.add(egui::Slider::new(&mut s.job.copies, 1..=99).text(crate::i18n::tr("Copies")));
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
            printer_trust(ui, app, &ctx);
            printer_options(ui, app, &ctx);
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
    app.print.selected = selected;
    app.print.new_cell = new_cell;
    // keep only valid edits (sliders can't produce invalid ones, but stay safe)
    if app.print.settings != before && app.print.settings.validate().is_err() {
        app.print.settings = before;
    }
}

/// An `ipps://` printer whose certificate was refused: its fingerprint and Trust Certificate;
/// and the first-use setting.
fn printer_trust(ui: &mut egui::Ui, app: &mut LccApp, ctx: &egui::Context) {
    let t = Tokens::get(ctx);
    if let Some((printer, fp)) = app.print.untrusted.clone() {
        ui.label(
            egui::RichText::new(format!(
                "{printer}: {}",
                crate::i18n::tr("this printer's certificate isn't trusted. Compare its fingerprint with the printer's status page:")
            ))
            .color(t.text_dim)
            .size(11.0),
        );
        ui.label(egui::RichText::new(&fp).monospace().size(10.0).color(t.text));
        let r = ui.button(crate::i18n::tr("Trust Certificate"));
        register(ctx, "print:trustCertificate", r.rect);
        if r.clicked() {
            cmd(app, ctx, "printui.trustCertificate", json!({}));
            cmd(app, ctx, "printui.media", json!({}));
        }
    }
    if app.print.printer_uri.trim_start().starts_with("ipps://") || app.print.trust_on_first_use {
        let mut on = app.print.trust_on_first_use;
        let r = ui.checkbox(&mut on, crate::i18n::tr("Trust a printer's certificate the first time"));
        register(ctx, "print:trustOnFirstUse", r.rect);
        if r.changed() {
            cmd(app, ctx, "printui.trustOnFirstUse", json!({"on": on}));
        }
    }
}

/// The printer's own options (once its attributes are read): paper type, tray, quality, two-sided.
fn printer_options(ui: &mut egui::Ui, app: &mut LccApp, ctx: &egui::Context) {
    let o = app.print.options.clone();
    let d = app.print.settings.job.driver.clone();
    let pretty = |k: &str| -> String {
        let s = k.replace(['-', '_'], " ");
        let mut c = s.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    };
    let default_label = |def: Option<String>| match def {
        Some(v) => format!("{} ({v})", crate::i18n::tr("Printer default")),
        None => crate::i18n::tr("Printer default").to_string(),
    };
    let mut choose = |ui: &mut egui::Ui, key: &str, label: &str, list: &[String], def: Option<&String>, cur: Option<&String>| {
        if list.is_empty() {
            return;
        }
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr(label));
            let shown = cur.map(|c| pretty(c)).unwrap_or_else(|| default_label(def.map(|v| pretty(v))));
            let r = egui::ComboBox::from_id_salt(("print-driver", key)).selected_text(shown).show_ui(ui, |ui| {
                if ui.selectable_label(cur.is_none(), default_label(def.map(|v| pretty(v)))).clicked() {
                    cmd(app, ctx, "printui.driver", json!({key: null}));
                }
                for v in list {
                    if ui.selectable_label(cur == Some(v), pretty(v)).clicked() {
                        cmd(app, ctx, "printui.driver", json!({key: v}));
                    }
                }
            });
            crate::access::label(&r.response, label);
            register(ctx, format!("print:driver:{key}"), r.response.rect);
        });
    };
    choose(ui, "mediaType", "Paper Type", &o.media_types, o.media_type_default.as_ref(), d.media_type.as_ref());
    choose(ui, "mediaSource", "Paper Tray", &o.media_sources, o.media_source_default.as_ref(), d.media_source.as_ref());
    choose(ui, "sides", "Two-Sided", &o.sides, o.sides_default.as_ref(), d.sides.as_ref());
    if !o.qualities.is_empty() {
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr("Print Quality"));
            let name = |q: i32| crate::i18n::tr(lcc_print::ipp::quality_label(q)).to_string();
            let def = default_label(o.quality_default.map(name));
            let shown = d.print_quality.map(name).unwrap_or_else(|| def.clone());
            let r = egui::ComboBox::from_id_salt(("print-driver", "printQuality")).selected_text(shown).show_ui(ui, |ui| {
                if ui.selectable_label(d.print_quality.is_none(), def.clone()).clicked() {
                    cmd(app, ctx, "printui.driver", json!({"printQuality": null}));
                }
                for q in &o.qualities {
                    if ui.selectable_label(d.print_quality == Some(*q), name(*q)).clicked() {
                        cmd(app, ctx, "printui.driver", json!({"printQuality": q}));
                    }
                }
            });
            crate::access::label(&r.response, "Print Quality");
            register(ctx, "print:driver:printQuality", r.response.rect);
        });
    }
}

/// Page Setup sits above the page: paper and orientation.
/// Print Job ▸ 16 Bit Output, Color Management (profile, intent, black point compensation) and
/// Print Adjustment.
fn color_management(ui: &mut egui::Ui, app: &mut LccApp, ctx: &egui::Context) {
    use lcc_print::PrintIntent;
    if app.print.settings.destination != Destination::Jpeg {
        let r = ui.checkbox(&mut app.print.settings.job.color.bit16, crate::i18n::tr("16 Bit Output"));
        register(ctx, "print:bit16", r.rect);
    }
    ui.label(egui::RichText::new(crate::i18n::tr("Color Management")).strong());
    let managed = app.print.settings.job.color.profile.is_none();
    let current = if managed {
        crate::i18n::tr("Managed by Printer").to_string()
    } else {
        app.print
            .profile_name
            .clone()
            .or_else(|| {
                app.print
                    .settings
                    .job
                    .color
                    .profile
                    .as_deref()
                    .and_then(|p| std::path::Path::new(p).file_name())
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_default()
    };
    let mut choose: Option<Value> = None;
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Profile:"));
        let r = egui::ComboBox::from_id_salt("print-profile").selected_text(current).show_ui(ui, |ui| {
            if ui.selectable_label(managed, crate::i18n::tr("Managed by Printer")).clicked() {
                choose = Some(json!({"path": null}));
            }
            if let Some((path, name)) = app.ui.proof_profile.clone()
                && ui.selectable_label(app.print.settings.job.color.profile.as_deref() == Some(path.as_str()), name).clicked()
            {
                choose = Some(json!({"path": path}));
            }
            if ui.selectable_label(false, crate::i18n::tr("Other…")).clicked() {
                choose = Some(json!({}));
            }
        });
        register(ctx, "print:profile", r.response.rect);
    });
    if let Some(p) = choose {
        cmd(app, ctx, "printui.profile", p);
    }
    let c = &mut app.print.settings.job.color;
    if c.profile.is_some() {
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr("Intent:"));
            for (v, label) in [(PrintIntent::Perceptual, "Perceptual"), (PrintIntent::Relative, "Relative")] {
                let r = ui.selectable_label(c.intent == v, crate::i18n::tr(label));
                register(ctx, format!("print:intent:{label}"), r.rect);
                if r.clicked() {
                    c.intent = v;
                }
            }
        });
        if c.intent == PrintIntent::Relative {
            let r = ui.checkbox(&mut c.black_point_compensation, crate::i18n::tr("Black Point Compensation"));
            register(ctx, "print:bpc", r.rect);
        }
    }
    let r = ui.checkbox(&mut c.adjust, crate::i18n::tr("Print Adjustment"));
    register(ctx, "print:adjust", r.rect);
    if c.adjust {
        ui.add(egui::Slider::new(&mut c.brightness, -100.0..=100.0).step_by(1.0).text(crate::i18n::tr("Brightness")));
        ui.add(egui::Slider::new(&mut c.contrast, -100.0..=100.0).step_by(1.0).text(crate::i18n::tr("Contrast")));
    }
}

fn page_setup_bar(ui: &mut egui::Ui, app: &mut LccApp, r: Rect) {
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
    // File ▸ Page Setup…: the paper choice, open
    if std::mem::take(&mut app.print.open_paper) {
        egui::Popup::open_id(&ctx, resp.response.id.with("popup"));
        ctx.request_repaint();
    }
    for (label, land) in [("Portrait", false), ("Landscape", true)] {
        let r = child.selectable_label(landscape == land, crate::i18n::tr(label));
        register(&ctx, format!("print:{}", label.to_lowercase()), r.rect);
        if r.clicked() && landscape != land {
            cmd(app, &ctx, "printui.pageSetup", json!({"landscape": land}));
        }
    }
}

fn canvas(ui: &mut egui::Ui, app: &mut LccApp, area: Rect) {
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
    let rect = |r: &lcc_layout::Rect| Rect::from_min_size(to(r.x, r.y), vec2(r.w * k, r.h * k));
    register(ui.ctx(), "print:page", sheet);
    // shadow + paper
    p.rect_filled(sheet.translate(vec2(3.0, 4.0)), 0.0, Color32::from_black_alpha(90));
    let bg = page.background;
    p.rect_filled(sheet, 0.0, Color32::from_rgb(bg[0], bg[1], bg[2]));
    let guide = Color32::from_rgb(70, 140, 220);
    if app.print.show && app.print.margins {
        let c = rect(&page.content());
        p.rect_stroke(c, 0.0, Stroke::new(1.0, guide.gamma_multiply(0.8)), StrokeKind::Inside);
    }
    let infos: Vec<Option<PhotoInfo>> =
        layout.cells.iter().map(|c| c.as_photo().and_then(|ph| ph.photo.as_deref()).map(|id| info_of(app, id))).collect();
    let dimensions = app.print.show && app.print.dimensions;
    let units = app.print.units;
    let show_cells = app.print.show && app.print.cells;
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
                    let txt = format!("{} × {}", fmt_num(cell.rect.w, units), fmt_len(cell.rect.h, units));
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
    if app.print.show && app.print.bleed && page.bleed > 0.0 {
        p.rect_stroke(rect(&page.bleed_box()), 0.0, Stroke::new(1.0, Color32::from_rgb(220, 60, 60)), StrokeKind::Outside);
    }
    for g in &layout.guides {
        match *g {
            lcc_layout::Guide::Vertical(x) => p.line_segment([to(x, 0.0), to(x, page.size.h)], Stroke::new(1.0, Color32::from_rgb(255, 0, 170))),
            lcc_layout::Guide::Horizontal(y) => p.line_segment([to(0.0, y), to(page.size.w, y)], Stroke::new(1.0, Color32::from_rgb(255, 0, 170))),
        };
    }
    if rulers {
        draw_rulers(&p, &t, sheet, k, page.size, app.print.units);
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
    if app.print.info_overlay {
        let rows = info_rows(&app.print, page.size, pi, n);
        if let Some(r) = crate::overlay::paint(&ui.painter_at(area), &t, area, crate::overlay::Corner::BottomLeft, vec2(0.0, 0.0), &rows) {
            register(ui.ctx(), "print:info", r);
        }
    }
}

/// The paper's name when it is one of Page Setup's, else its size in the ruler unit.
fn paper_text(size: lcc_layout::Size, unit: Unit) -> String {
    let near = |a: f32, b: f32| (a - b).abs() < 0.6;
    if let Some(p) = PAPERS.iter().find(|p| (near(p.size.w, size.w) && near(p.size.h, size.h)) || (near(p.size.w, size.h) && near(p.size.h, size.w)))
    {
        return crate::i18n::tr(p.label).to_string();
    }
    let per = unit.per_inch() as f32 / 72.0;
    format!("{:.2} × {:.2} {}", size.w * per, size.h * per, unit.label())
}

/// The info overlay's rows: paper, printer, page `page` (0-based) of `pages`, and the resolution.
pub fn info_rows(pu: &PrintUi, size: lcc_layout::Size, page: usize, pages: usize) -> Vec<crate::overlay::Row> {
    use crate::overlay::Row;
    let printer = if pu.printer_uri.trim().is_empty() {
        crate::i18n::tr("Print to PDF").to_string()
    } else {
        pu.printers.iter().find(|p| p.uri == pu.printer_uri).map_or_else(|| pu.printer_uri.clone(), |p| p.name.clone())
    };
    vec![
        Row::text(format!("{}: {}", crate::i18n::tr("Paper"), paper_text(size, pu.units))),
        Row::text(format!("{}: {printer}", crate::i18n::tr("Printer"))),
        Row::text(crate::i18n::tr_format!("Page {page} of {pages}", page = page + 1, pages = pages)),
        Row::text(format!("{:.0} ppi", pu.settings.job.dpi)),
    ]
}

/// The photo's thumbnail placed in its cell as the print will place it.
fn draw_photo(p: &egui::Painter, app: &LccApp, id: PhotoId, r: Rect, ph: &lcc_layout::PhotoCell) {
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
    let cell = lcc_layout::Rect::new(r.left(), r.top(), r.width(), r.height());
    let (s, ox, oy) = lcc_layout::render::photo_placement(cell, iw, ih, ph);
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
    let mut mesh = egui::Mesh::with_texture(tex.id());
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

fn draw_rulers(p: &egui::Painter, t: &Tokens, sheet: Rect, k: f32, size: lcc_layout::Size, unit: Unit) {
    let top = Rect::from_min_max(pos2(sheet.left(), sheet.top() - RULER - 6.0), pos2(sheet.right(), sheet.top() - 6.0));
    let left = Rect::from_min_max(pos2(sheet.left() - RULER - 6.0, sheet.top()), pos2(sheet.left() - 6.0, sheet.bottom()));
    for r in [top, left] {
        p.rect_filled(r, 0.0, t.chrome);
    }
    let col = t.text_dim;
    // (labelled step in points, ticks per step, label value per step)
    let (major, subs, per) = match unit {
        Unit::Inches | Unit::Points => (72.0, 4u32, 1u32),
        Unit::Centimeters => (pt_per(Unit::Centimeters), 2, 1),
        Unit::Millimeters => (pt_per(Unit::Centimeters), 2, 10),
    };
    let tick = major / subs as f32;
    if tick * k < 2.0 {
        return;
    }
    let len = |i: u32| {
        if i.is_multiple_of(subs) {
            RULER * 0.7
        } else if subs == 4 && i.is_multiple_of(2) {
            RULER * 0.4
        } else {
            RULER * 0.25
        }
    };
    let mut i = 0u32;
    while (i as f32) * tick <= size.w + 0.01 && i < 4000 {
        let x = sheet.left() + i as f32 * tick * k;
        p.line_segment([pos2(x, top.bottom()), pos2(x, top.bottom() - len(i))], Stroke::new(1.0, col));
        if i.is_multiple_of(subs) && i > 0 {
            p.text(pos2(x + 2.0, top.top() + 1.0), Align2::LEFT_TOP, format!("{}", i / subs * per), t.font(9.0), col);
        }
        i += 1;
    }
    let mut j = 0u32;
    while (j as f32) * tick <= size.h + 0.01 && j < 4000 {
        let y = sheet.top() + j as f32 * tick * k;
        p.line_segment([pos2(left.right(), y), pos2(left.right() - len(j), y)], Stroke::new(1.0, col));
        if j.is_multiple_of(subs) && j > 0 {
            p.text(pos2(left.left() + 1.0, y + 2.0), Align2::LEFT_TOP, format!("{}", j / subs * per), t.font(9.0), col);
        }
        j += 1;
    }
}

/// Custom Package: click a cell to select it, drag it to move it and drag its bottom-right
/// corner to resize it (both snapped to page, margins and other cells).
fn custom_drag(ui: &mut egui::Ui, app: &mut LccApp, sheet: Rect, k: f32) {
    let page = app.print.settings.page.clone();
    let mut selected = app.print.selected;
    let LayoutStyle::CustomPackage { pages } = &mut app.print.settings.layout else { return };
    let Some(pg) = pages.first_mut() else { return };
    let n = pg.cells.len();
    if selected.is_some_and(|i| i >= n) {
        selected = None;
    }
    let screen = |r: &lcc_layout::Rect| Rect::from_min_size(pos2(sheet.left() + r.x * k, sheet.top() + r.y * k), vec2(r.w * k, r.h * k));
    let tol = 6.0 / k.max(0.01);
    for i in 0..n {
        let Some(c) = pg.cells.get(i) else { continue };
        let r = screen(&c.rect);
        let resp = ui.interact(r, egui::Id::new(("print-cell", i)), Sense::click_and_drag());
        register(ui.ctx(), format!("print:cell:{i}"), r);
        let raw_id = egui::Id::new(("print-cell-raw", i));
        if resp.clicked() || resp.drag_started() {
            selected = Some(i);
        }
        if resp.drag_started() {
            ui.data_mut(|d| d.insert_temp(raw_id, [c.rect.x, c.rect.y]));
        }
        if resp.dragged() {
            let d = resp.drag_delta() / k;
            let mut raw: [f32; 2] = ui.data_mut(|m| m.get_temp(raw_id)).unwrap_or([c.rect.x, c.rect.y]);
            raw[0] += d.x;
            raw[1] += d.y;
            ui.data_mut(|m| m.insert_temp(raw_id, raw));
            let targets = lcc_layout::snap::SnapTargets::for_page(&page, pg, Some(i));
            if let Some(c) = pg.cells.get_mut(i) {
                let moved = lcc_layout::Rect::new(raw[0], raw[1], c.rect.w, c.rect.h);
                c.rect = lcc_layout::snap::snap_move(moved, &targets, tol).0;
            }
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }
    // the selected cell: outline and a corner handle, on top of every cell
    if let Some(i) = selected
        && let Some(c) = pg.cells.get(i)
    {
        let r = screen(&c.rect);
        let p = ui.painter();
        p.rect_stroke(r, 0.0, Stroke::new(2.0, Color32::from_rgb(70, 140, 220)), StrokeKind::Outside);
        let handle = Rect::from_center_size(r.right_bottom(), vec2(RESIZE_HANDLE, RESIZE_HANDLE));
        p.rect_filled(handle, 1.0, Color32::from_rgb(70, 140, 220));
        let resp = ui.interact(handle, egui::Id::new(("print-cell-resize", i)), Sense::drag());
        register(ui.ctx(), format!("print:resize:{i}"), handle);
        let raw_id = egui::Id::new(("print-cell-raw-size", i));
        if resp.drag_started() {
            ui.data_mut(|d| d.insert_temp(raw_id, [c.rect.w, c.rect.h]));
        }
        if resp.dragged() {
            let d = resp.drag_delta() / k;
            let mut raw: [f32; 2] = ui.data_mut(|m| m.get_temp(raw_id)).unwrap_or([c.rect.w, c.rect.h]);
            raw[0] += d.x;
            raw[1] += d.y;
            ui.data_mut(|m| m.insert_temp(raw_id, raw));
            let targets = lcc_layout::snap::SnapTargets::for_page(&page, pg, Some(i));
            if let Some(c) = pg.cells.get_mut(i) {
                let w = raw[0].clamp(MIN_CELL_PT, (page.size.w - c.rect.x).max(MIN_CELL_PT));
                let h = raw[1].clamp(MIN_CELL_PT, (page.size.h - c.rect.y).max(MIN_CELL_PT));
                let sized = lcc_layout::snap::snap_resize(lcc_layout::Rect::new(c.rect.x, c.rect.y, w, h), &targets, tol);
                c.rect.w = sized.w.max(MIN_CELL_PT);
                c.rect.h = sized.h.max(MIN_CELL_PT);
            }
        }
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
        }
    }
    app.print.selected = selected;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_lengths_follow_the_unit_or_their_own() {
        let near = |a: Option<f32>, b: f32| a.is_some_and(|a| (a - b).abs() < 0.01);
        assert!(near(parse_len("2.54", Unit::Centimeters), 72.0));
        assert!(near(parse_len("2,54", Unit::Centimeters), 72.0));
        assert!(near(parse_len("1 in", Unit::Centimeters), 72.0));
        assert!(near(parse_len("25.4mm", Unit::Inches), 72.0));
        for bad in ["", "abc", "-1", "0", "1 furlong", "NaN", "inf", "1e40"] {
            assert_eq!(parse_len(bad, Unit::Inches), None, "{bad}");
        }
        assert_eq!(fmt_len(72.0 * 5.0 / 2.54, Unit::Centimeters), "5 cm");
    }

    #[test]
    fn merge_patches_nested_settings() {
        let mut a = json!({"page": {"size": {"w": 1, "h": 2}}, "job": {"dpi": 240}});
        merge(&mut a, &json!({"job": {"dpi": 300}, "page": {"size": {"w": 5}}}));
        assert_eq!(a, json!({"page": {"size": {"w": 5, "h": 2}}, "job": {"dpi": 300}}));
    }

    /// The Print module's own templates: folders (empty ones too) are made through the menu's
    /// prompt, templates move in and out, everything survives a new session, and deleting a folder
    /// keeps its templates.
    #[test]
    fn template_folders_group_user_templates_and_persist() {
        let dir = lcc_testkit::TempDir::new("lcc-print-folders");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let open = || {
            let mut s = lcc_engine::Session::with_demo();
            s.remote.connections_path = Some(dir.join("connections.json"));
            LccApp::new(s, crate::Services::default())
        };
        let mut app = open();
        // New Template Folder asks for a name first
        app.run(
            "ui.prompt",
            json!({"title": "New Template Folder", "hint": "Folder name", "command": "printui.createTemplateFolder", "key": "name"}),
        )
        .unwrap();
        let Some(crate::state::Dialog::TextPrompt { command, key, .. }) = &app.ui.dialog else { panic!("{:?}", app.ui.dialog) };
        assert_eq!((command.as_str(), key.as_str()), ("printui.createTemplateFolder", "name"));
        let r = app.run("printui.createTemplateFolder", json!({"name": " Weddings "})).unwrap();
        assert_eq!(r["folders"][1], json!({"name": "Weddings", "default": false, "templates": 0}));
        app.run("printui.createTemplateFolder", json!({"name": "Proofs"})).unwrap();
        app.run("printui.saveTemplate", json!({"name": "Sheet", "folder": "weddings"})).unwrap();
        app.run("printui.saveTemplate", json!({"name": "Plain"})).unwrap();
        // saving over a name keeps its folder; a move changes only the folder
        app.run("printui.saveTemplate", json!({"name": "Sheet"})).unwrap();
        assert_eq!(app.print.user_templates.iter().find(|t| t.name == "Sheet").and_then(|t| t.folder.clone()).as_deref(), Some("Weddings"));
        let r = app.run("printui.moveTemplate", json!({"name": "Plain", "folder": "Proofs"})).unwrap();
        assert_eq!(r["moved"], true);
        for (id, p) in [
            ("printui.moveTemplate", json!({"name": "Plain", "folder": "Nope"})),
            ("printui.moveTemplate", json!({"name": "Nope", "folder": "Proofs"})),
            ("printui.createTemplateFolder", json!({"name": "PROOFS"})),
            ("printui.renameTemplateFolder", json!({"name": "User Templates", "to": "X"})),
            ("printui.deleteTemplateFolder", json!({"name": "Nope"})),
            ("printui.saveTemplate", json!({"name": "New", "folder": "Nope"})),
        ] {
            assert!(app.run(id, p.clone()).is_err(), "{id} {p}");
        }
        // a new session finds the folders and where each template is
        let mut again = open();
        let st = again.run("printui.state", json!({})).unwrap();
        assert_eq!(st["templateFolders"]["folders"].as_array().unwrap().len(), 3, "{st}");
        let places: Vec<(String, String)> = st["templateFolders"]["templates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["name"].as_str().unwrap().into(), t["folder"].as_str().unwrap().into()))
            .collect();
        assert!(places.contains(&("Sheet".into(), "Weddings".into())) && places.contains(&("Plain".into(), "Proofs".into())), "{places:?}");
        // a folder's rename follows its templates; its deletion keeps them
        let r = again.run("printui.renameTemplateFolder", json!({"name": "proofs", "to": "Contact Sheets"})).unwrap();
        assert!(r["templates"].as_array().unwrap().iter().any(|t| t["name"] == "Plain" && t["folder"] == "Contact Sheets"));
        let r = again.run("printui.deleteTemplateFolder", json!({"name": "Contact Sheets"})).unwrap();
        assert!(r["templates"].as_array().unwrap().iter().any(|t| t["name"] == "Plain" && t["folder"] == "User Templates"));
        assert_eq!(r["moved"], 1);
        // a damaged print.json is ignored, and a stored template in an unlisted or hostile folder is tidied
        let hostile = json!({"userTemplates": [
            {"name": "Odd", "folder": "a\nb", "settings": serde_json::to_value(PrintSettings::default()).unwrap()},
            {"name": "Kept", "folder": " family ", "settings": serde_json::to_value(PrintSettings::default()).unwrap()},
        ], "templateFolders": ["Family", "", "family", "User Templates"]});
        let mut pu = open().print;
        apply_stored(&mut pu, &hostile);
        assert_eq!(pu.template_folders, ["Family"]);
        assert_eq!(pu.user_templates.iter().map(|t| t.folder.clone()).collect::<Vec<_>>(), [None, Some("Family".to_string())]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn user_templates_settings_and_printer_paper_persist() {
        let dir = lcc_testkit::TempDir::new("lcc-print-persist");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let open = || {
            let mut s = lcc_engine::Session::with_demo();
            s.remote.connections_path = Some(dir.join("connections.json"));
            LccApp::new(s, crate::Services::default())
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
    fn info_overlay_toggles_persists_and_describes_the_page() {
        let dir = lcc_testkit::TempDir::new("lcc-print-info");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let open = || {
            let mut s = lcc_engine::Session::with_demo();
            s.remote.connections_path = Some(dir.join("connections.json"));
            LccApp::new(s, crate::Services::default())
        };
        let mut app = open();
        assert_eq!(crate::menubar::checked(&app, "printui.infoOverlay"), Some(false));
        app.run("printui.infoOverlay", json!({})).unwrap();
        assert_eq!(crate::menubar::checked(&app, "printui.infoOverlay"), Some(true));
        assert_eq!(app.run("printui.state", json!({})).unwrap()["infoOverlay"], true);
        app.run("printui.set", json!({"settings": {"job": {"dpi": 200}}})).unwrap();
        let mut again = open();
        assert_eq!(again.run("printui.state", json!({})).unwrap()["infoOverlay"], true, "kept in print.json for the next session");
        let rows: Vec<String> = info_rows(&app.print, lcc_layout::Size::new(612.0, 792.0), 1, 3).into_iter().map(|r| r.text).collect();
        assert!(rows.iter().any(|t| t.contains('2') && t.contains('3')), "page 2 of 3: {rows:?}");
        assert!(rows.iter().any(|t| t.contains("US Letter")), "{rows:?}");
        assert!(rows.iter().any(|t| t.ends_with("ppi")), "{rows:?}");
        app.run("printui.infoOverlay", json!({"on": false})).unwrap();
        assert!(!app.print.info_overlay);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_print_json_write_is_reported_and_retried() {
        let dir = lcc_testkit::TempDir::new("lcc-print-retry");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // the config "directory" is a file, so print.json cannot be written
        let cfg = dir.join("cfg");
        std::fs::write(&cfg, b"").unwrap();
        let mut s = lcc_engine::Session::with_demo();
        s.remote.connections_path = Some(cfg.join("connections.json"));
        let mut app = LccApp::new(s, crate::Services::default());
        ensure_loaded(&mut app);
        app.print.settings.job.dpi = 200.0;
        for n in 1..=SAVE_ATTEMPTS {
            assert!(persist(&mut app).is_err());
            assert_eq!(app.print.save_failed.as_ref().map(|f| f.1), Some(n));
        }
        // bounded: the same contents are not retried forever
        assert!(persist(&mut app).is_ok());
        assert_eq!(app.print.save_failed.as_ref().map(|f| f.1), Some(SAVE_ATTEMPTS));
        // once the directory is usable, the next change is saved
        std::fs::remove_file(&cfg).unwrap();
        std::fs::create_dir_all(&cfg).unwrap();
        app.print.settings.job.dpi = 300.0;
        assert!(persist(&mut app).is_ok());
        assert!(app.print.save_failed.is_none());
        assert!(cfg.join("print.json").is_file());
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
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
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

    #[test]
    fn printer_profile_is_checked_and_a_colour_managed_16_bit_pdf_prints() {
        use std::sync::{Arc, Mutex};
        let dir = lcc_testkit::TempDir::new("print-profile");
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let written = bytes.clone();
        let services = crate::Services {
            write_shared: Some(Arc::new(move |_: &str, data: &[u8]| {
                *written.lock().unwrap() = data.to_vec();
                Ok(())
            })),
            ..Default::default()
        };
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
        app.run("module.print", json!({})).unwrap();
        assert!(app.run("printui.profile", json!({"path": "/nonexistent/printer.icc"})).is_err());
        assert!(app.print.settings.job.color.profile.is_none());
        std::fs::create_dir_all(&dir).unwrap();
        let icc = dir.join("printer.icc");
        std::fs::write(&icc, lcc_codecs::icc::write_named(lcc_codecs::space::NamedSpace::AdobeRgb)).unwrap();
        app.run("printui.profile", json!({"path": icc.to_string_lossy()})).unwrap();
        assert!(app.print.settings.job.color.profile.is_some());
        app.run(
            "printui.set",
            json!({"settings": {"job": {"dpi": 72, "color": {"intent": "relative", "adjust": true, "brightness": 20, "bit16": true}}}}),
        )
        .unwrap();
        // one photo: every demo photo is a full-size procedural render (~3 CPU-minutes for the
        // whole library in a debug build), which on a loaded CI runner outlasted any deadline
        app.run("printui.printOne", json!({"path": "Print.pdf"})).unwrap();
        let mut h = crate::headless::Headless::new(app, [1200.0, 800.0], 1.0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        // until the job has reported: under load it may not be busy yet on the first frame
        while (h.app.print.busy || h.app.print.last.is_none()) && std::time::Instant::now() < deadline {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let last = h.app.print.last.clone().expect("the print job reported within 600 s");
        assert!(last.get("error").is_none(), "{last}");
        let pdf = bytes.lock().unwrap().clone();
        let has = |needle: &[u8]| pdf.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"/ICCBased") && has(b"/OutputIntents") && has(b"/BitsPerComponent 16"));
        h.app.run("printui.profile", json!({"path": null})).unwrap();
        assert!(h.app.print.settings.job.color.profile.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prints_to_a_virtual_ipp_printer_with_its_paper_margins_and_copies() {
        use lcc_print::testprinter::{TestPrinter, TestPrinterConfig};
        let printer = TestPrinter::start(TestPrinterConfig::default()).unwrap();
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), crate::Services::default());
        app.run("module.print", json!({})).unwrap();
        app.run("printui.template", json!({"name": "2×2 Cells"})).unwrap();
        app.run(
            "printui.set",
            json!({"settings": {"page": {"margins": {"top": 0, "right": 0, "bottom": 0, "left": 0}}, "job": {"dpi": 72, "copies": 2}}}),
        )
        .unwrap();
        app.run("printui.media", json!({"uri": printer.uri()})).unwrap();
        let mut h = crate::headless::Headless::new(app, [1200.0, 800.0], 1.0);
        let wait = |h: &mut crate::headless::Headless, done: &dyn Fn(&LccApp) -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while !done(&h.app) && std::time::Instant::now() < deadline {
                h.step();
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        wait(&mut h, &|a| !a.print.media.is_empty());
        h.app.run("printui.pageSetup", json!({"media": "iso_a4_210x297mm", "landscape": false})).unwrap();
        let m = h.app.print.settings.page.margins;
        assert!((m.top - 14.17).abs() < 0.1 && (m.left - 8.5).abs() < 0.1, "{m:?}");
        h.app.run("printui.print", json!({"destination": "printer", "printer": printer.uri()})).unwrap();
        wait(&mut h, &|a| !a.print.busy);
        let last = h.app.print.last.clone().unwrap();
        assert_eq!(last["state"], "completed", "{last}");
        assert_eq!(last["copies"], 2);
        let jobs = printer.jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].copies, 2);
        assert!(jobs[0].document.starts_with(b"%PDF-"));
    }

    /// The printer's own options come with its paper sizes, show in the Print Job panel, and go
    /// out with the job; a bad choice is refused.
    #[test]
    fn printer_options_are_offered_drawn_and_sent() {
        use lcc_print::testprinter::{TestPrinter, TestPrinterConfig};
        let printer = TestPrinter::start(TestPrinterConfig::default()).unwrap();
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), crate::Services::default());
        app.run("module.print", json!({})).unwrap();
        app.run("printui.template", json!({"name": "2×2 Cells"})).unwrap();
        app.run("printui.set", json!({"settings": {"destination": "printer", "job": {"dpi": 72}}})).unwrap();
        app.run("printui.media", json!({"uri": printer.uri()})).unwrap();
        let mut h = crate::headless::Headless::new(app, [1400.0, 1000.0], 1.0);
        let wait = |h: &mut crate::headless::Headless, done: &dyn Fn(&LccApp) -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while !done(&h.app) && std::time::Instant::now() < deadline {
                h.step();
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        wait(&mut h, &|a| !a.print.options.media_types.is_empty());
        let st = h.app.run("printui.state", json!({})).unwrap();
        assert_eq!(st["printerOptions"]["mediaTypeDefault"], "stationery", "{st}");
        assert_eq!(st["printerOptions"]["qualities"], json!([4, 3, 5]));
        h.step();
        h.step();
        for w in ["print:driver:mediaType", "print:driver:mediaSource", "print:driver:sides", "print:driver:printQuality"] {
            assert!(h.app.widgets.iter().any(|(id, _)| id == w), "{w} is drawn");
        }
        assert!(h.app.run("printui.driver", json!({"sides": "two sided"})).is_err());
        assert!(h.app.run("printui.driver", json!({"printQuality": 9})).is_err());
        h.app
            .run("printui.driver", json!({"mediaType": "photographic-matte", "mediaSource": "photo", "printQuality": "high", "sides": null}))
            .unwrap();
        assert_eq!(h.app.print.settings.job.driver.print_quality, Some(5));
        h.app.run("printui.print", json!({"printer": printer.uri()})).unwrap();
        wait(&mut h, &|a| !a.print.busy);
        let j = &printer.jobs()[0];
        assert_eq!(
            (j.media_type.as_deref(), j.media_source.as_deref(), j.print_quality, j.sides.as_deref()),
            (Some("photographic-matte"), Some("photo"), Some(5), None)
        );
        // trust settings: a bad fingerprint is refused, a good one is kept for saving
        assert!(h.app.run("printui.trustCertificate", json!({"uri": "ipps://printer.local/ipp/print", "fingerprint": "zz"})).is_err());
        h.app.run("printui.trustCertificate", json!({"uri": "ipps://printer.local/ipp/print", "fingerprint": "AB".repeat(32)})).unwrap();
        assert!(stored_json(&h.app.print)["trustedCertificates"][0]["printer"] == "printer.local:631");
        lcc_print::ipp::set_trusted_certificates(&[]);
    }
}
