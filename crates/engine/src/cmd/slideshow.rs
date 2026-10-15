//! `slideshow.playback`: the slideshow's Playback settings (slide and fade durations, fade colour,
//! random order, repeat, preparing previews in advance). `slideshow.layout`: how a slide looks
//! (Layout, Options and Backdrop: margins, aspect preview, zoom to fill, stroke, shadow, backdrop
//! colour, colour wash, background image). Both are library preferences, saved in the library's
//! `prefs.json`; the app's slideshow (View ▸ Slideshow) plays with them. `slideshow.overlays`:
//! the identity plate, rating stars, text overlays with tokens and the watermark drawn on every
//! slide (also a library preference). `slideshow.titles`: the intro and ending screens (colour,
//! identity plate or text, scale, duration; a library preference). `slideshow.preview` composes
//! one slide, or a title screen, as a PNG (with the library's settings, or a template's).
//! `slideshow.templates.*`: the Template Browser (built-in and user templates; see
//! [`crate::slide_template`]). `slideshow.export`: the slideshow as a PDF, a JPEG sequence or an
//! AV1 video, in the background ([`crate::slideshow_export`]); `slideshow.exportStatus` reads how
//! it is going.

use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::slide_template::{self as tpl, SlideLook};
use crate::{Result, Session};

/// Largest side `slideshow.preview` renders.
pub const PREVIEW_MAX: usize = 4096;

fn playback(s: &mut Session, p: &Value) -> Result<Value> {
    let changes = p.as_object().is_some_and(|o| !o.is_empty());
    if changes {
        let next = s.slideshow.merged(p).map_err(|e| bad("slideshow.playback", e))?;
        if next != s.slideshow {
            s.slideshow = next;
            s.save_prefs()?;
        }
    }
    Ok(s.slideshow.to_json())
}

fn layout(s: &mut Session, p: &Value) -> Result<Value> {
    let changes = p.as_object().is_some_and(|o| !o.is_empty());
    if changes {
        let next = s.slide_layout.merged(p).map_err(|e| bad("slideshow.layout", e))?;
        if next != s.slide_layout {
            s.slide_layout = next;
            s.save_prefs()?;
        }
    }
    Ok(s.slide_layout.to_json())
}

fn overlays(s: &mut Session, p: &Value) -> Result<Value> {
    let changes = p.as_object().is_some_and(|o| !o.is_empty());
    if changes {
        let next = s.slide_overlays.merged(p).map_err(|e| bad("slideshow.overlays", e))?;
        if next != s.slide_overlays {
            s.slide_overlays = next;
            s.save_prefs()?;
        }
    }
    let mut out = s.slide_overlays.to_json();
    out["warnings"] = json!(s.slide_overlays.warnings());
    Ok(out)
}

fn titles(s: &mut Session, p: &Value) -> Result<Value> {
    let changes = p.as_object().is_some_and(|o| !o.is_empty());
    if changes {
        let next = s.slide_titles.merged(p).map_err(|e| bad("slideshow.titles", e))?;
        if next != s.slide_titles {
            s.slide_titles = next;
            s.save_prefs()?;
        }
    }
    let mut out = s.slide_titles.to_json();
    out["warnings"] = json!(s.slide_titles.warnings(&s.slide_overlays));
    Ok(out)
}

/// A slide size from the request: both sides given, one (the other from the aspect preview), or
/// neither (1280 wide). Each side 16–[`PREVIEW_MAX`].
fn preview_size(layout: &lightcraft_slideshow::SlideSettings, p: &Value) -> Result<(usize, usize)> {
    const C: &str = "slideshow.preview";
    let side = |k: &str| -> Result<Option<usize>> {
        match p.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => match v.as_u64() {
                Some(n) if (16..=PREVIEW_MAX as u64).contains(&n) => Ok(Some(n as usize)),
                _ => Err(bad(C, format!("`{k}` must be a whole number of pixels from 16 to {PREVIEW_MAX}, not {v}"))),
            },
        }
    };
    let ratio = layout.aspect.ratio(f64::NAN);
    let clamp = |v: f64| (v.round() as usize).clamp(16, PREVIEW_MAX);
    Ok(match (side("width")?, side("height")?) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, clamp(w as f64 / ratio)),
        (None, Some(h)) => (clamp(h as f64 * ratio), h),
        (None, None) => (1280, clamp(1280.0 / ratio)),
    })
}

/// Write `png` to `path`, or put it in the reply as base64.
fn deliver(png: &[u8], path: Option<&str>, out: &mut Value) -> Result<()> {
    match path {
        Some(path) => {
            crate::export::write_file(path, png).map_err(crate::EngineError::Other)?;
            out["path"] = json!(path);
        }
        None => out["png"] = json!(base64(png)),
    }
    Ok(())
}

fn encode_png(img: &lightcraft_raster::Rgba8) -> Result<Vec<u8>> {
    lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default())
        .map_err(|e| crate::EngineError::Other(format!("PNG: {e}")))
}

/// `slideshow.preview {title}`: a title screen (whether or not it is switched on).
fn preview_title(s: &mut Session, p: &Value, title: &Value, look: &SlideLook) -> Result<Value> {
    const C: &str = "slideshow.preview";
    let t = title
        .as_str()
        .and_then(crate::slideshow::Title::parse)
        .ok_or_else(|| bad(C, format!("`title` must be \"intro\" or \"ending\", not {title}")))?;
    if p.get("id").is_some_and(|v| !v.is_null()) {
        return Err(bad(C, "pass `id` or `title`, not both"));
    }
    let (w, h) = preview_size(&look.layout, p)?;
    let path = str_param(p, "path").map(str::trim).filter(|x| !x.is_empty());
    if let Some(path) = path {
        s.check_write_target(path).map_err(|e| bad(C, e))?;
    }
    let c = crate::slide_title::TitleCompose::new(&look.titles, &look.overlays, t, w, h);
    let img = c.compose();
    let png = encode_png(&img)?;
    let rect = |r: lightcraft_slideshow::Rect| json!([r.x0, r.y0, r.x1, r.y1]);
    let mut out = json!({
        "title": t.id(),
        "enabled": look.titles.screen(t).enabled,
        "width": img.width,
        "height": img.height,
        "plate": c.plate_box().map(rect),
    });
    deliver(&png, path, &mut out)?;
    Ok(out)
}

fn preview(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "slideshow.preview";
    // a template's look (the Template Browser's hover preview), else the library's settings
    let template = match p.get("template") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) => Some(s.slide_template(id.trim()).map_err(|e| bad(C, e))?),
        Some(v) => return Err(bad(C, format!("`template` must be a template id, not {v}"))),
    };
    let look = template.as_ref().map_or_else(|| SlideLook::of(s), |t| t.look.clone());
    if let Some(t) = p.get("title").filter(|v| !v.is_null()) {
        let mut out = preview_title(s, p, t, &look)?;
        if let Some(t) = &template {
            out["template"] = json!(t.id);
        }
        return Ok(out);
    }
    let id = match p.get("id") {
        None | Some(Value::Null) => s.active().ok_or_else(|| bad(C, "no photo selected (select one or pass `id`)"))?,
        Some(v) => PhotoId(v.as_u64().ok_or_else(|| bad(C, format!("`id` must be a photo id, not {v}")))?),
    };
    if s.catalog.photo(id).is_none() {
        return Err(bad(C, format!("no photo {}", id.0)));
    }
    let (w, h) = preview_size(&look.layout, p)?;
    let path = str_param(p, "path").map(str::trim).filter(|x| !x.is_empty());
    if let Some(path) = path {
        s.check_write_target(path).map_err(|e| bad(C, e))?;
    }
    let settings = look.layout.clone();
    let img = s.slide_now_look(&settings, &look.overlays, id, w, h).map_err(crate::EngineError::Other)?;
    let png = encode_png(&img)?;
    let aspect = s.slide_photo_aspect(id).unwrap_or(1.5);
    let l = lightcraft_slideshow::layout(&settings, aspect, img.width, img.height);
    let rect = |r: lightcraft_slideshow::Rect| json!([r.x0, r.y0, r.x1, r.y1]);
    let mut out = json!({
        "id": id.0,
        "width": img.width,
        "height": img.height,
        "cell": rect(l.cell),
        "photo": rect(l.visible),
    });
    let photo = s.catalog.photo(id).cloned();
    let o = crate::slide_overlay::SlideOverlays::resolve(&look.overlays, photo.as_deref());
    out["overlays"] = o.boxes(&l).into_iter().map(|(k, r)| json!({"kind": k.id(), "box": rect(r)})).collect();
    if let Some(t) = &template {
        out["template"] = json!(t.id);
    }
    deliver(&png, path, &mut out)?;
    Ok(out)
}

/// Standard padded base64 (for an image returned in the reply).
fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk.first().copied().unwrap_or(0), chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        let at = |shift: u32| A.get((n >> shift) as usize & 63).copied().unwrap_or(b'=') as char;
        out.push(at(18));
        out.push(at(12));
        out.push(if chunk.len() > 1 { at(6) } else { '=' });
        out.push(if chunk.len() > 2 { at(0) } else { '=' });
    }
    out
}

// ------------------------------------------------------------------------------- templates

/// The settings as the `slideshow.*` commands return them (for template replies).
fn settings_json(s: &Session) -> Value {
    json!({
        "playback": s.slideshow.to_json(),
        "layout": s.slide_layout.to_json(),
        "overlays": s.slide_overlays.to_json(),
        "titles": s.slide_titles.to_json(),
    })
}

fn tpl_list(s: &mut Session, _p: &Value) -> Result<Value> {
    let l = s.slide_templates();
    Ok(json!({
        "templates": l.templates.iter().map(|t| t.info()).collect::<Vec<_>>(),
        "previous": s.slide_templates_previous.is_some(),
        "folder": s.slide_templates_dir.as_ref().map(|d| d.to_string_lossy().to_string()),
        "problems": l.problems,
    }))
}

/// The template `id` or `name` names.
fn named(s: &mut Session, p: &Value, c: &str) -> Result<std::sync::Arc<tpl::Template>> {
    s.slide_template_named(str_param(p, "id"), str_param(p, "name")).map_err(|e| bad(c, e))
}

fn tpl_apply(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "slideshow.templates.apply";
    let t = named(s, p, C)?;
    let warnings = s.apply_slide_look(&t.look)?;
    let mut out = json!({"applied": t.id, "name": t.name, "warnings": warnings, "previous": s.slide_templates_previous.is_some()});
    out["settings"] = settings_json(s);
    Ok(out)
}

fn tpl_reply(t: &tpl::Template) -> Value {
    t.info()
}

fn tpl_save(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "slideshow.templates.save";
    let name = str_param(p, "name").ok_or_else(|| bad(C, "missing `name`"))?;
    let replace = match p.get("replace") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or_else(|| bad(C, format!("`replace` must be true or false, not {v}")))?,
    };
    let t = s.save_slide_template(name, replace).map_err(|e| bad(C, e))?;
    Ok(tpl_reply(&t))
}

fn id_param<'a>(p: &'a Value, c: &str) -> Result<&'a str> {
    str_param(p, "id").map(str::trim).filter(|x| !x.is_empty()).ok_or_else(|| bad(c, "missing `id` (a template id from slideshow.templates.list)"))
}

fn tpl_update(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "slideshow.templates.update";
    let id = id_param(p, C)?;
    let t = s.update_slide_template(id).map_err(|e| bad(C, e))?;
    Ok(tpl_reply(&t))
}

fn tpl_rename(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "slideshow.templates.rename";
    let id = id_param(p, C)?;
    let name = str_param(p, "name").ok_or_else(|| bad(C, "missing `name`"))?;
    let t = s.rename_slide_template(id, name).map_err(|e| bad(C, e))?;
    Ok(tpl_reply(&t))
}

fn tpl_delete(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "slideshow.templates.delete";
    let id = id_param(p, C)?;
    let t = s.delete_slide_template(id).map_err(|e| bad(C, e))?;
    Ok(json!({"deleted": t.id, "name": t.name}))
}

fn tpl_import(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "slideshow.templates.import";
    let path = str_param(p, "path").map(str::trim).filter(|x| !x.is_empty()).ok_or_else(|| bad(C, "missing `path`"))?;
    let (t, renamed) = s.import_slide_template(std::path::Path::new(path)).map_err(|e| bad(C, e))?;
    let mut out = tpl_reply(&t);
    out["renamed"] = json!(renamed);
    out["warnings"] = json!(t.look.warnings(s));
    Ok(out)
}

fn tpl_export(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "slideshow.templates.export";
    let id = id_param(p, C)?;
    let path = str_param(p, "path").map(str::trim).filter(|x| !x.is_empty()).ok_or_else(|| bad(C, "missing `path`"))?;
    let t = s.slide_template(id).map_err(|e| bad(C, e))?;
    s.check_write_target(path).map_err(|e| bad(C, e))?;
    let bytes = serde_json::to_vec_pretty(&t.to_doc()).map_err(|e| crate::EngineError::Other(e.to_string()))?;
    crate::export::write_file(path, &bytes).map_err(crate::EngineError::Other)?;
    Ok(json!({"id": t.id, "name": t.name, "path": path}))
}

/// `slideshow.export`: prepare on this thread, run on a worker (or here with `wait`).
fn export(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "slideshow.export";
    if cfg!(target_arch = "wasm32") {
        return Err(bad(C, "slideshow export needs the desktop app or lightcraft-cli"));
    }
    if s.slideshow_export.as_ref().is_some_and(|r| r.running()) {
        return Err(bad(C, "a slideshow export is already running (activity.cancel stops it)"));
    }
    let prepared = crate::slideshow_export::prepare(s, p).map_err(|e| bad(C, e))?;
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task = s.activity.start("export", "Exporting slideshow", crate::activity::Cancel::Flag(cancel.clone()));
    let total = prepared.total();
    task.progress(0, total);
    task.detail(&prepared.request.path);
    let settings = &prepared.request.settings;
    let run = std::sync::Arc::new(crate::slideshow_export::ExportRun {
        task_id: task.id(),
        format: settings.format.id().into(),
        path: prepared.request.path.clone(),
        total: total.into(),
        cancel,
        ..Default::default()
    });
    s.slideshow_export = Some(run.clone());
    let started = json!({
        "taskId": task.id(),
        "background": !wait,
        "format": settings.format.id(),
        "path": prepared.request.path,
        "slides": prepared.len(),
        "seconds": prepared.seconds(),
        "total": total,
        "width": prepared.width,
        "height": prepared.height,
        "warnings": prepared.warnings,
    });
    if wait {
        crate::slideshow_export::run_to_end(prepared, &run, &task);
        drop(task);
        return match run.result() {
            Some(Ok(v)) => Ok(v),
            Some(Err(e)) => Err(bad(C, e)),
            None => Err(bad(C, "the export did not finish")),
        };
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // the row goes when the worker ends, however it ends
        let r = run.clone();
        std::thread::Builder::new()
            .name("lc-slideshow-export".into())
            .spawn(move || crate::slideshow_export::run_to_end(prepared, &r, &task))
            .map_err(|e| bad(C, format!("could not start: {e}")))?;
    }
    Ok(started)
}

fn export_status(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(s.slideshow_export.as_ref().map_or_else(|| json!({"running": false}), |r| r.json()))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "slideshow.playback",
            "Slideshow Playback Settings",
            [],
            None,
            "{slideDuration?: seconds (0.5–120), fadeDuration?: seconds (0 = cut; at most 10 and the slide duration), fadeColor?: \"#rrggbb\"|[r,g,b]|null (fade through a colour instead of crossfading), randomOrder?: bool, repeat?: bool, preparePreviews?: bool (render the next slides before they are due)} — omitted keys keep their value, {} reads them; saved with the library → {slideDuration, fadeDuration, fadeColor, randomOrder, repeat, preparePreviews}",
            always,
            playback
        ),
        cmd!(
            "slideshow.layout",
            "Slideshow Layout Settings",
            [],
            None,
            "{margins?: all four, marginLeft?, marginRight?, marginTop?, marginBottom?: fractions of the slide's shorter side (0–0.45; with linkMargins one sets all four), linkMargins?: bool, showGuides?: bool (margin guides in the slide editor), aspect?: \"screen\"|\"16:9\"|\"4:3\" (aspect preview), zoomToFill?: bool (crop the photo to fill its cell), strokeBorder?: bool, strokeColor?: \"#rrggbb\"|[r,g,b], strokeWidth?: fraction of the shorter side (0–0.05), castShadow?: bool, shadowOpacity?: 0–1, shadowOffset?: 0–0.1, shadowRadius?: 0–0.1 (fractions of the shorter side), shadowAngle?: degrees counter-clockwise from the right (−45 = down right), backdropColor?: colour, colorWash?: bool, washColor?: colour, washOpacity?: 0–1, washAngle?: degrees (the side the wash colour is full at), backgroundImage?: photo id|null, backgroundOpacity?: 0–1} — omitted keys keep their value, {} reads them; out-of-range numbers are clamped, wrong types refused; saved with the library → the settings",
            always,
            layout
        ),
        cmd!(
            "slideshow.overlays",
            "Slideshow Overlay Settings",
            [],
            None,
            "{identityPlate?: {enabled?, text?, image?: PNG/JPEG file (drawn instead of the text; \"\" = the text), size?: height as a fraction of the slide's shorter side (0.005–0.5), color?: \"#rrggbb\"|[r,g,b], opacity?: 0–1, …placement, behind?: bool (render behind the photo)}, ratingStars?: {enabled?, color?, size?: a star's height (0.005–0.15), …placement}, texts?: [text overlay] (replaces the list, at most 16; each entry merged onto the one at its index), addText?: text overlay ({} = the default \"{file}\"), removeText?: index, showWatermark?: bool, watermark?: text | the export watermark object {text, vertical, size, opacity, anchor, inset, color, shadow, image, imageWidth} (merged; sizes relative to the photo)} where a text overlay is {text?: template with tokens ({file}, {name}, {title}, {caption}, {date} (YYYY-MM-DD) / {date:%d.%m.%Y}, {exposure}, {shutter}, {aperture}, {focal}, {iso}, {camera}, {lens}, {rating}, {creator}, {copyright}, {folder}, {ext}; unknown tags show as typed), size?: line height (0.005–0.3), color?, opacity?, …placement, shadow?: bool, shadowOpacity?: 0–1, shadowOffset?, shadowRadius?: 0–0.05 (fractions of the shorter side), shadowAngle?: degrees counter-clockwise from the right} and the placement keys are anchor?: topLeft|top|topRight|left|center|right|bottomLeft|bottom|bottomRight, offsetX?, offsetY?: fractions of the slide's shorter side (x right, y down; −1–1), relativeTo?: \"slide\"|\"photo\" (the overlay is kept on the slide) — omitted keys keep their value, {} reads them; out-of-range numbers are clamped, wrong types refused; saved with the library; slideshow playback and slideshow.preview draw them → the settings and warnings (an unreadable identity plate image, unknown tokens)",
            always,
            overlays
        ),
        cmd!(
            "slideshow.titles",
            "Slideshow Title Settings",
            [],
            None,
            "{intro?: screen, ending?: screen} where a screen is {enabled?: bool, color?: background \"#rrggbb\"|[r,g,b], content?: \"none\"|\"identityPlate\" (the Overlays identity plate, its text or image)|\"text\", text?: the text shown with content \"text\", overrideColor?: bool (draw the plate or text in textColor instead of the identity plate's colour; an image plate keeps its colours), textColor?: colour, scale?: height of the plate or text line as a fraction of the slide's shorter side (0.005–0.5; text wider than the slide shrinks to fit), duration?: seconds (0.5–120) | null (the slide duration)} — the intro plays once before the first slide, the ending after the last one when repeat is off; both fade with the playback fade; random order never moves them. Omitted keys keep their value, {} reads them; out-of-range numbers are clamped, wrong types refused; saved with the library → the settings and warnings (an unreadable identity plate image, a screen with nothing to show)",
            always,
            titles
        ),
        cmd!(
            query "slideshow.templates.list",
            "Slideshow Templates",
            [],
            None,
            "{} → {templates: [{id: \"builtin:…\"|\"user:…\", name, builtin, group: \"builtin\"|\"user\", description?, path? (a user template's file)}], previous: bool (the settings before the last apply can be restored with apply {id: \"previous\"}), folder: the user templates folder | null, problems: [\"file: why it was skipped\"]} — built-ins first, then user templates by name (files in <settings>/slideshow-templates, at most 1000)",
            always,
            tpl_list
        ),
        cmd!(
            "slideshow.templates.apply",
            "Apply Slideshow Template",
            [],
            None,
            "{id: template id | \"previous\" (the settings before the last apply) | name: template name (case-insensitive)} — replaces the playback (prepare previews kept), layout, overlays and titles settings with the template's and saves them with the library; there is no undo: the replaced settings are kept as \"previous\" for this session → {applied, name, warnings: [a background photo not in this library, an unreadable identity plate / watermark image…], previous, settings: {playback, layout, overlays, titles}}",
            always,
            tpl_apply
        ),
        cmd!(
            "slideshow.templates.save",
            "Save Slideshow Template",
            [],
            None,
            "{name: 1–80 characters, replace?: bool (over a user template of that name)} — the current playback, layout, overlays and titles settings as a user template, <settings>/slideshow-templates/<name>.json (path separators and other reserved characters become _); a name taken (case-insensitive) or a built-in's is refused unless replace → {id, name, builtin, group, path}",
            always,
            tpl_save
        ),
        cmd!(
            "slideshow.templates.update",
            "Update Slideshow Template",
            [],
            None,
            "{id: user template} — replace its settings with the current ones (name kept); built-ins can't change → {id, name, builtin, group, path}",
            always,
            tpl_update
        ),
        cmd!(
            "slideshow.templates.rename",
            "Rename Slideshow Template",
            [],
            None,
            "{id: user template, name} — its file follows the name; a name taken is refused → {id (the new one), name, builtin, group, path}",
            always,
            tpl_rename
        ),
        cmd!(
            "slideshow.templates.delete",
            "Delete Slideshow Template",
            [],
            None,
            "{id: user template} — deletes its file → {deleted, name}",
            always,
            tpl_delete
        ),
        cmd!(
            "slideshow.templates.import",
            "Import Slideshow Template",
            [],
            None,
            "{path: a template .json file (at most 256 KB)} — checked like a saved one (format, version, every setting) and saved as a user template under its name (else the file's); a name taken gets \" (2)\"… → {id, name, builtin, group, path, renamed, warnings}",
            always,
            tpl_import
        ),
        cmd!(
            "slideshow.templates.export",
            "Export Slideshow Template",
            [],
            None,
            "{id: template (built-in or user), path: .json file to write} → {id, name, path}",
            always,
            tpl_export
        ),
        cmd!(
            query "slideshow.preview",
            "Slideshow Slide Preview",
            [],
            None,
            "{id?: photo (default: the active one) | title?: \"intro\"|\"ending\", template?: a template id (slideshow.templates.list): compose with its settings instead of the library's, which don't change (the Template Browser's hover preview), width?, height?: pixels (16–4096; one given: the other from the aspect preview; neither: 1280 wide), path?: write the PNG here} — the photo's slide composed with the slideshow layout and overlays → {id, width, height, cell: [x0,y0,x1,y1], photo: [x0,y0,x1,y1] (the visible photo, slide pixels), overlays: [{kind: identityPlate|ratingStars|textN|watermark, box: [x0,y0,x1,y1]}], path | png (base64 PNG)}; with `title`, that title screen (`slideshow.titles`; drawn even while it is off) → {title, enabled, width, height, plate: [x0,y0,x1,y1] | null (the plate or text), path | png}",
            always,
            preview
        ),
        cmd!(
            query "slideshow.export",
            "Export Slideshow",
            [],
            None,
            "{format: \"pdf\"|\"jpeg\"|\"video\", path: the .pdf / .mp4 / .webm file, or for jpeg the folder (created if missing), size?: pdf/jpeg the long side in pixels (64–8192, default 1920), video the frame height (64–2160 or \"720p\"/\"1080p\", default 720), quality?: 1–100 (JPEG quality, default 90; video default 75), fps?: 24|25|30|60 (video, default 30), container?: \"mp4\"|\"webm\" (video, default mp4), fullScreen?: bool (pdf: open in the viewer's full screen mode, default true), speed?: 1–10 (video encoder speed, default 10 = fastest), name?: jpeg file name prefix (default the folder's name: <name>-001.jpg…), ids?: photo ids (default the photos in view, in view order), wait?: bool (run here and reply with the result)} — the photos' slides with the slideshow layout, overlays and the intro/ending screens that are on; video is AV1 (no audio), each slide held for its duration and fades rendered as frames, at most 2 hours. Runs in the background with a row in the activity stack (activity.cancel {id: taskId} stops it; nothing is left behind: files are written as .part and renamed when complete); a photo that can't be rendered is skipped with a warning → {taskId, background, format, path, slides, seconds, total, width, height, warnings}; with wait: the result (slideshow.exportStatus)",
            always,
            export
        ),
        cmd!(
            query "slideshow.exportStatus",
            "Slideshow Export Status",
            [],
            None,
            "{} → the last slideshow export: {taskId, format, path, running, done, total (slides, or frames for video), cancelled, result?: {format, path, width, height, slides, skipped, warnings, pages? | files? | frames, samples, keyFrames, seconds, fps, container, codec: \"av1\", audio: false; bytes}, error?} | {running: false} when there was none",
            always,
            export_status
        ),
    ]
}
