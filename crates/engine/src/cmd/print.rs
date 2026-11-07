//! `print.*`: the Print module as commands.
//!
//! The module's state is [`Session::print`] (layout, image settings, page, guides, job), so the
//! app, CLI, control channel and MCP share one print. Lengths are inches in page space (origin at
//! the top-left of the page as printed); cell ids and photo ids are numbers. Nothing here touches
//! the catalog, so none of it is journaled or undoable; saved templates live in prefs.json.
//!
//! * `print.get`, `print.set {layout?, image?, page?, guides?, job?}`, `print.reset`
//! * `print.cell.add|move|resize|delete|stack|photo|page`, `print.layout.autoLayout|newPage|clear`
//! * `print.exportPdf {path, ids?}`
//! * `print.template.list|save|apply|delete`
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, f64_req};
use crate::print::layout::{Layout, MAX_PAGES, Rect, Snap, Stack, paper_names, paper_size};
use crate::print::template::{MAX_TEMPLATES, PrintSettings, PrintTemplate, clean_name};
use crate::{Result, Session};

fn rect_json(r: &Rect) -> Value {
    json!({"x": r.x, "y": r.y, "w": r.w, "h": r.h})
}

/// The state, with what a front end needs to draw it (page size, printable area, grid cells).
fn state(s: &Session, photos: usize) -> Value {
    let l = &s.print.layout;
    let mut out = json!({
        "settings": s.print,
        "papers": paper_names().filter_map(|n| paper_size(n).map(|(w, h)| json!({"name": n, "width": w, "height": h}))).collect::<Vec<_>>(),
        "overlaps": l.has_overlaps(),
        "photos": photos,
    });
    if let (Ok((w, h)), Ok(area), Some(o)) = (l.page_size(), l.printable(), out.as_object_mut()) {
        o.insert("pageWidth".into(), json!(w));
        o.insert("pageHeight".into(), json!(h));
        o.insert("printable".into(), rect_json(&area));
        o.insert("pageCount".into(), json!(l.page_count(photos)));
        o.insert("cellsPerPage".into(), json!(l.cells_per_page()));
        if !l.style.is_package() {
            o.insert("gridCells".into(), json!(l.grid_rects().unwrap_or_default().iter().map(rect_json).collect::<Vec<_>>()));
        }
    }
    out
}

fn reply(s: &Session) -> Result<Value> {
    Ok(state(s, s.targets(&Value::Null)?.len()))
}

/// Applies `patch` over `dst`: objects merge key by key, anything else replaces.
fn merge(dst: &mut Value, patch: &Value, depth: usize) {
    match (dst, patch) {
        (Value::Object(d), Value::Object(p)) if depth < 8 => {
            for (k, v) in p {
                match d.get_mut(k) {
                    Some(slot) => merge(slot, v, depth + 1),
                    None => {
                        d.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (d, p) => *d = p.clone(),
    }
}

fn get(s: &mut Session, p: &Value) -> Result<Value> {
    Ok(state(s, s.targets(p)?.len()))
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.set";
    let patch = p.as_object().ok_or_else(|| bad(ID, "params must be an object"))?;
    if let Some(k) = patch.keys().find(|k| !matches!(k.as_str(), "layout" | "image" | "page" | "guides" | "job")) {
        return Err(bad(ID, format!("unknown parameter `{k}` (one of layout, image, page, guides, job)")));
    }
    if let Some(l) = patch.get("layout").and_then(Value::as_object)
        && let Some(k) = l.keys().find(|k| matches!(k.as_str(), "cells" | "pages" | "nextId"))
    {
        return Err(bad(ID, format!("`layout.{k}` is changed with the print.cell.* and print.layout.* commands")));
    }
    let mut value = serde_json::to_value(&s.print).map_err(|e| bad(ID, e.to_string()))?;
    merge(&mut value, p, 0);
    let mut next: PrintSettings = serde_json::from_value(value).map_err(|e| bad(ID, e.to_string()))?;
    // A new paper, orientation or margin may leave cells outside the page: pull them back.
    if next.layout.page.printable().is_ok() {
        next.layout.fit_cells().map_err(|e| bad(ID, e))?;
    }
    next.validate().map_err(|e| bad(ID, e))?;
    s.print = next;
    reply(s)
}

fn reset(s: &mut Session, _: &Value) -> Result<Value> {
    s.print = PrintSettings::default();
    reply(s)
}

fn cell_id(p: &Value, id: &str) -> Result<u32> {
    p.get("id").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()).ok_or_else(|| bad(id, "missing cell `id`"))
}

fn opt_f64(p: &Value, key: &str, id: &str) -> Result<Option<f64>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().map(Some).ok_or_else(|| bad(id, format!("`{key}` must be a number"))),
    }
}

/// Snaps `r` (a cell being moved or resized) when the call asks for it.
fn snapped(l: &Layout, guides: &crate::print::layout::Guides, id: u32, r: Rect, p: &Value, resize: bool, cmd: &str) -> Result<Rect> {
    let on = match p.get("snap") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(bad(cmd, "`snap` must be true or false")),
    };
    if !on || guides.snap == Snap::Off {
        return Ok(r);
    }
    let threshold = opt_f64(p, "threshold", cmd)?.unwrap_or(0.1);
    let f = if resize { Layout::snap_resize } else { Layout::snap_move };
    f(l, id, r, guides.snap, guides.grid_size, threshold).map_err(|e| bad(cmd, e))
}

fn cell_add(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.cell.add";
    let (w, h) = (f64_req(p, "width", ID)?, f64_req(p, "height", ID)?);
    let mut next = s.print.layout.clone();
    let id = next.add_cell(w, h).map_err(|e| bad(ID, e))?;
    if let (Some(x), Some(y)) = (opt_f64(p, "x", ID)?, opt_f64(p, "y", ID)?) {
        if let Some(page) = p.get("page").and_then(Value::as_u64) {
            let page = u32::try_from(page).map_err(|_| bad(ID, "`page` is too large"))?;
            next.set_cell_page(id, page).map_err(|e| bad(ID, e))?;
        }
        next.move_cell(id, x, y).map_err(|e| bad(ID, e))?;
    }
    if let Some(v) = p.get("photo").filter(|v| !v.is_null()) {
        let photo = v.as_u64().ok_or_else(|| bad(ID, "`photo` must be a photo id"))?;
        next.set_cell_photo(id, Some(photo)).map_err(|e| bad(ID, e))?;
    }
    s.print.layout = next;
    let cell = s.print.layout.cell(id).cloned();
    Ok(json!({"id": id, "cell": cell, "pages": s.print.layout.pages}))
}

fn cell_move(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.cell.move";
    let id = cell_id(p, ID)?;
    let (x, y) = (f64_req(p, "x", ID)?, f64_req(p, "y", ID)?);
    let mut next = s.print.layout.clone();
    let size = next.cell(id).map(|c| c.rect).ok_or_else(|| bad(ID, format!("no cell {id}")))?;
    let target = snapped(&next, &s.print.guides, id, Rect::new(x, y, size.w, size.h), p, false, ID)?;
    let r = next.move_cell(id, target.x, target.y).map_err(|e| bad(ID, e))?;
    s.print.layout = next;
    Ok(json!({"id": id, "rect": r}))
}

fn cell_resize(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.cell.resize";
    let id = cell_id(p, ID)?;
    let mut next = s.print.layout.clone();
    let cur = next.cell(id).map(|c| c.rect).ok_or_else(|| bad(ID, format!("no cell {id}")))?;
    let want = Rect::new(
        opt_f64(p, "x", ID)?.unwrap_or(cur.x),
        opt_f64(p, "y", ID)?.unwrap_or(cur.y),
        opt_f64(p, "width", ID)?.unwrap_or(cur.w),
        opt_f64(p, "height", ID)?.unwrap_or(cur.h),
    );
    let target = snapped(&next, &s.print.guides, id, want, p, true, ID)?;
    let r = next.resize_cell(id, target).map_err(|e| bad(ID, e))?;
    s.print.layout = next;
    Ok(json!({"id": id, "rect": r}))
}

fn cell_delete(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.cell.delete";
    let id = cell_id(p, ID)?;
    s.print.layout.delete_cell(id).map_err(|e| bad(ID, e))?;
    Ok(json!({"deleted": id, "cells": s.print.layout.cells.len()}))
}

fn cell_stack(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.cell.stack";
    let id = cell_id(p, ID)?;
    let how = match p.get("order").and_then(Value::as_str) {
        Some("toBack") => Stack::ToBack,
        Some("backward") => Stack::Backward,
        Some("forward") => Stack::Forward,
        Some("toFront") => Stack::ToFront,
        _ => return Err(bad(ID, "`order` must be toBack, backward, forward or toFront")),
    };
    s.print.layout.restack(id, how).map_err(|e| bad(ID, e))?;
    Ok(json!({"order": s.print.layout.cells.iter().map(|c| c.id).collect::<Vec<_>>()}))
}

fn cell_photo(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.cell.photo";
    let id = cell_id(p, ID)?;
    let photo = match p.get("photo") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let n = v.as_u64().ok_or_else(|| bad(ID, "`photo` must be a photo id or null"))?;
            if s.catalog.photo(lightcraft_catalog::PhotoId(n)).is_none() {
                return Err(bad(ID, format!("no photo {n}")));
            }
            Some(n)
        }
    };
    s.print.layout.set_cell_photo(id, photo).map_err(|e| bad(ID, e))?;
    Ok(json!({"id": id, "photo": photo}))
}

fn cell_page(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.cell.page";
    let id = cell_id(p, ID)?;
    let page = p.get("page").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()).ok_or_else(|| bad(ID, "missing zero-based `page`"))?;
    s.print.layout.set_cell_page(id, page).map_err(|e| bad(ID, e))?;
    Ok(json!({"id": id, "page": page}))
}

fn auto_layout(s: &mut Session, _: &Value) -> Result<Value> {
    s.print.layout.auto_layout().map_err(|e| bad("print.layout.autoLayout", e))?;
    Ok(json!({"pages": s.print.layout.pages, "cells": s.print.layout.cells}))
}

fn new_page(s: &mut Session, _: &Value) -> Result<Value> {
    let page = s.print.layout.new_page().map_err(|e| bad("print.layout.newPage", e))?;
    Ok(json!({"page": page, "pages": s.print.layout.pages, "max": MAX_PAGES}))
}

fn clear(s: &mut Session, _: &Value) -> Result<Value> {
    s.print.layout.clear();
    Ok(json!({"pages": 1, "cells": 0}))
}

fn export_pdf(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.exportPdf";
    let path = p.get("path").and_then(Value::as_str).filter(|s| !s.trim().is_empty()).ok_or_else(|| bad(ID, "missing path"))?;
    if let Some(k) = p.as_object().and_then(|o| o.keys().find(|k| !matches!(k.as_str(), "path" | "ids" | "id"))) {
        return Err(bad(ID, format!("unknown parameter `{k}` (one of path, ids; change the print with print.set)")));
    }
    if p.get("ids").is_some_and(|v| v.as_array().is_none_or(|a| a.iter().any(|v| v.as_u64().is_none()))) {
        return Err(bad(ID, "ids must be an array of photo ids"));
    }
    if p.get("id").is_some_and(|v| v.as_u64().is_none()) {
        return Err(bad(ID, "id must be a photo id"));
    }
    s.check_write_target(path).map_err(|e| bad(ID, e))?;
    let ids = s.targets(p)?;
    let settings = s.print.clone();
    // a printer profile that can't be used fails the print before anything renders
    crate::print_color::PrintColorJob::new(&settings.job.color).map_err(|e| bad(ID, e))?;
    let title = std::path::Path::new(path).file_stem().and_then(|n| n.to_str()).unwrap_or("Print").to_string();
    let work = crate::print::compose::prepare(s, &settings, &ids).map_err(|e| bad(ID, e))?;
    let total = work.len() as u64;
    let row = s.activity.start("export", "Printing to PDF", crate::activity::Cancel::No);
    row.progress(0, total);
    let pages = work
        .run(&mut |done, name| {
            row.progress(done as u64, total);
            row.detail(name);
            true
        })
        .map_err(|e| bad(ID, e))?;
    let bytes = crate::print::output::write(&pages, &settings.job, &title).map_err(|e| bad(ID, e))?;
    s.check_write_target(path).map_err(|e| bad(ID, e))?;
    crate::export::write_file(path, &bytes).map_err(|e| bad(ID, e))?;
    Ok(json!({"path": path, "pages": pages.len(), "photos": ids.len(), "bytes": bytes.len()}))
}

// ---- templates ------------------------------------------------------------------------------

fn template_list(s: &Session) -> Value {
    json!({"templates": s.print_templates.iter().map(|t| json!({"name": t.name, "style": t.settings.pointer("/layout/style"), "settings": t.settings})).collect::<Vec<_>>()})
}

fn template_name(p: &Value, id: &str) -> Result<String> {
    clean_name(p.get("name").and_then(Value::as_str).ok_or_else(|| bad(id, "missing `name`"))?).map_err(|e| bad(id, e))
}

fn template_save(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.template.save";
    let name = template_name(p, ID)?;
    let t = PrintTemplate::new(&name, &s.print).map_err(|e| bad(ID, e))?;
    match s.print_templates.iter_mut().find(|x| x.name.eq_ignore_ascii_case(&name)) {
        Some(x) => *x = t,
        None => {
            if s.print_templates.len() >= MAX_TEMPLATES {
                return Err(bad(ID, format!("at most {MAX_TEMPLATES} templates; delete one first")));
            }
            s.print_templates.push(t);
        }
    }
    s.save_prefs()?;
    Ok(template_list(s))
}

fn template_apply(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.template.apply";
    let name = template_name(p, ID)?;
    let t = s
        .print_templates
        .iter()
        .find(|x| x.name.eq_ignore_ascii_case(&name))
        .ok_or_else(|| bad(ID, format!("no print template `{name}` (see print.template.list)")))?;
    s.print = t.settings(&s.print.guides).map_err(|e| bad(ID, e))?;
    reply(s)
}

fn template_delete(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "print.template.delete";
    let name = template_name(p, ID)?;
    let before = s.print_templates.len();
    s.print_templates.retain(|x| !x.name.eq_ignore_ascii_case(&name));
    if s.print_templates.len() == before {
        return Err(bad(ID, format!("no print template `{name}`")));
    }
    s.save_prefs()?;
    Ok(template_list(s))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "print.get", "Print Settings", [], None,
            "{ids?} → {settings: {layout, image, page, guides, job}, papers, pageWidth, pageHeight, printable, pageCount, cellsPerPage, gridCells?, overlaps}; lengths are inches",
            always, get),
        cmd!(query "print.set", "Set Print Settings", [], None,
            "{layout?: {style: singleImage|contactSheet|picturePackage|customPackage, page: {paper, customWidth, customHeight, orientation: portrait|landscape, margins: {top,right,bottom,left}}, grid: {columns, rows, spacingX, spacingY, keepSquare}, spacing}, image?: {zoomToFill, rotateToFit, borderEnabled, borderWidth, borderColor: [r,g,b], strokeEnabled, strokeWidthPt, strokeColor}, page?: {background: [r,g,b], pageNumbers, pageInfo, cropMarks} (Page Options: single image and contact sheet only), guides?: {showRulers, units: in|cm|mm|pt, snap: off|cells|grid, gridSize, showMargins, showBleed, showPageGrid, showCells, showDimensions}, job?: {ppi: 72..720, color?: {profile: {kind: managed, space: srgb|adobeRgb} | {kind: icc, path}, intent: perceptual|relative, blackPointCompensation, brightness: -100..100, contrast: -100..100, sharpen: off|low|standard|high, media: matte|glossy, sixteenBit}}} — merges into the current print; cells and pages change with print.cell.* / print.layout.*; → print.get",
            always, set),
        cmd!(query "print.reset", "Reset Print Settings", [], None, "{} → print.get", always, reset),
        cmd!(query "print.cell.add", "Add Print Cell", [], None,
            "{width, height, x?, y?, page?, photo?} inches; picture/custom package → {id, cell, pages}; the cell goes to the first free spot unless x and y are given",
            always, cell_add),
        cmd!(query "print.cell.move", "Move Print Cell", [], None, "{id, x, y, snap?: bool, threshold?: inches} → {id, rect}; snap uses the guides' snap mode", always, cell_move),
        cmd!(query "print.cell.resize", "Resize Print Cell", [], None,
            "{id, width?, height?, x?, y?, snap?: bool, threshold?} → {id, rect}", always, cell_resize),
        cmd!(query "print.cell.delete", "Delete Print Cell", [], None, "{id}", always, cell_delete),
        cmd!(query "print.cell.stack", "Arrange Print Cell", [], None, "{id, order: toBack|backward|forward|toFront} → {order: cell ids back to front}", always, cell_stack),
        cmd!(query "print.cell.photo", "Set Print Cell Photo", [], None, "{id, photo: photo id | null} (custom package: the photo printed in the cell)", always, cell_photo),
        cmd!(query "print.cell.page", "Move Print Cell to Page", [], None, "{id, page: zero-based}", always, cell_page),
        cmd!(query "print.layout.autoLayout", "Auto Layout", [], None, "{} — packs the package's cells onto as few pages as possible → {pages, cells}", always, auto_layout),
        cmd!(query "print.layout.newPage", "New Print Page", [], None, "{} → {page, pages}", always, new_page),
        cmd!(query "print.layout.clear", "Clear Print Layout", [], None, "{} — removes every cell and extra page", always, clear),
        cmd!(query "print.exportPdf", "Print to PDF", [], None,
            "{path, ids?: photo ids (default selection)} → {path, pages, photos, bytes}; prints the current print.get settings; replaces an existing output, never an original",
            always, export_pdf),
        cmd!(query "print.template.list", "Print Templates", [], None, "{} → {templates: [{name, style, settings}]}", always, |s, _| Ok(template_list(s))),
        cmd!(query "print.template.save", "Save Print Template", [], None,
            "{name} — saves the layout, image settings, page and job (not the cells' photos or the guides); an existing name (any case) is replaced → print.template.list",
            always, template_save),
        cmd!(query "print.template.apply", "Apply Print Template", [], None, "{name} → print.get", always, template_apply),
        cmd!(query "print.template.delete", "Delete Print Template", [], None, "{name} → print.template.list", always, template_delete),
    ]
}
