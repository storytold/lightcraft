//! Print, Book, Slideshow and Web layouts on the engine: `dac_layout` documents filled with
//! catalog photos, rendered with the export pipeline (so a print matches an export), and saved
//! creations (albums carrying a layout document, catalog format 7).
//!
//! Commands: `layout.templates`, `print.render` / `book.render` (headless page rendering to
//! JPEG/PNG; PDF output arrives with the `pdf` crate), `creation.save`, `creation.get`,
//! `creation.update`.
//!
//! Sources: own design.

use std::collections::HashMap;

use dac_catalog::{Album, AlbumId, Creation, Op, Photo, PhotoId};
use dac_layout::render::{PhotoSource, RenderOptions, Rendered, render_page};
use dac_layout::tokens::PhotoInfo;
use dac_layout::{CellKind, CreationKind, Document, Template, builtin_templates};
use dac_pipeline::{OutputDepth, OutputSpace};
use dac_raster::Rgba8;
use serde_json::{Value, json};

use crate::cmd::{CommandSpec, always, bad, cmd};
use crate::export::{ExportFormat, ExportOptions, encode_image};
use crate::media::RenderJob;
use crate::{Result, Session};

/// Most distinct photos one render prepares.
const MAX_PHOTOS: usize = 2_000;
/// Longest side a photo is rendered at for a layout.
const MAX_SIDE: usize = 12_000;

/// `PhotoInfo` (token text) from a catalog photo.
pub fn photo_info(p: &Photo) -> PhotoInfo {
    let m = &p.meta;
    PhotoInfo {
        filename: p.file_name.clone(),
        folder: String::new(),
        date: p.captured.clone().unwrap_or_default(),
        title: m.title.clone(),
        caption: m.caption.clone(),
        headline: String::new(),
        creator: m.creator.clone(),
        copyright: m.copyright.clone(),
        camera: m.camera.clone(),
        lens: m.lens.clone(),
        iso: m.iso,
        shutter: parse_shutter(&m.shutter),
        aperture: m.aperture.map(f64::from),
        focal: m.focal_mm.map(f64::from),
        rating: Some(p.rating).filter(|r| *r > 0),
        keywords: m.keywords.clone(),
        dimensions: Some((p.width, p.height)).filter(|(w, h)| *w > 0 && *h > 0),
        ..PhotoInfo::default()
    }
}

/// `1/250`, `1/250 s`, `0.5`, `2"` → seconds.
pub fn parse_shutter(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches('s').trim_end_matches('"').trim();
    let v = match s.split_once('/') {
        Some((a, b)) => a.trim().parse::<f64>().ok()? / b.trim().parse::<f64>().ok()?,
        None => s.parse::<f64>().ok()?,
    };
    (v.is_finite() && v > 0.0).then_some(v)
}

/// Photos rendered ahead (the session isn't borrowed while pages are drawn).
pub struct PreparedPhotos {
    images: HashMap<String, Rgba8>,
    infos: HashMap<String, PhotoInfo>,
}

impl PhotoSource for PreparedPhotos {
    fn image(&self, photo: &str, _long_side: usize) -> std::result::Result<Rgba8, String> {
        self.images.get(photo).cloned().ok_or_else(|| format!("photo {photo} was not prepared"))
    }
    fn info(&self, photo: &str) -> PhotoInfo {
        self.infos.get(photo).cloned().unwrap_or_default()
    }
}

/// Render jobs for every photo `doc` places, each at the largest size a cell needs at `dpi`.
pub fn prepare_jobs(
    s: &mut Session,
    doc: &Document,
    dpi: f32,
) -> std::result::Result<(Vec<(String, RenderJob)>, HashMap<String, PhotoInfo>), String> {
    let k = dpi / dac_layout::PT_PER_INCH;
    let mut sizes: HashMap<String, usize> = HashMap::new();
    for page in &doc.pages {
        for c in &page.cells {
            if let CellKind::Photo(p) = &c.kind
                && let Some(id) = &p.photo
            {
                let zoom = if p.zoom.is_finite() { p.zoom.clamp(1.0, 20.0) } else { 1.0 };
                let side = ((c.rect.w.max(c.rect.h) * k * zoom).ceil().max(16.0) as usize).min(MAX_SIDE);
                let e = sizes.entry(id.clone()).or_insert(0);
                *e = (*e).max(side);
            }
        }
    }
    if sizes.len() > MAX_PHOTOS {
        return Err(format!("a layout renders at most {MAX_PHOTOS} photos"));
    }
    let mut jobs = Vec::with_capacity(sizes.len());
    let mut infos = HashMap::new();
    for (key, side) in sizes {
        let id = PhotoId(key.parse::<u64>().map_err(|_| format!("{key:?} is not a photo id"))?);
        let photo = s.catalog.photo(id).ok_or_else(|| format!("no photo {key}"))?;
        infos.insert(key.clone(), photo_info(photo));
        jobs.push((key, s.export_job(id, side, side, OutputSpace::Srgb, OutputDepth::U8)?));
    }
    Ok((jobs, infos))
}

/// Runs the jobs from [`prepare_jobs`].
pub fn run_jobs(jobs: Vec<(String, RenderJob)>, infos: HashMap<String, PhotoInfo>) -> std::result::Result<PreparedPhotos, String> {
    // a few renders at once (P6.1: a contact sheet's 20 photos one by one took 8 s)
    let width = crate::cmd::publish::render_width();
    let mut images = HashMap::with_capacity(jobs.len());
    let mut jobs = jobs.into_iter().peekable();
    while jobs.peek().is_some() {
        let batch: Vec<(String, RenderJob)> = jobs.by_ref().take(width).collect();
        let done: Vec<(String, std::result::Result<Rgba8, String>)> = if batch.len() <= 1 || cfg!(target_arch = "wasm32") {
            batch.into_iter().map(|(k, j)| (k, j.run().rendered.map(|r| r.image))).collect()
        } else {
            std::thread::scope(|scope| {
                let handles: Vec<_> = batch.into_iter().map(|(k, j)| (k, scope.spawn(move || j.run().rendered.map(|r| r.image)))).collect();
                handles.into_iter().map(|(k, h)| (k, h.join().unwrap_or_else(|_| Err("the render stopped unexpectedly".to_string())))).collect()
            })
        };
        for (key, image) in done {
            images.insert(key, image?);
        }
    }
    Ok(PreparedPhotos { images, infos })
}

/// Renders every page of `doc` with catalog photos.
pub fn render_document(s: &mut Session, doc: &Document, opt: &RenderOptions) -> std::result::Result<Vec<Rendered>, String> {
    doc.validate().map_err(|e| e.to_string())?;
    let (jobs, infos) = prepare_jobs(s, doc, opt.dpi)?;
    let photos = run_jobs(jobs, infos)?;
    let n = doc.pages.len();
    let text = dac_print::text::ShapedText::new();
    doc.pages
        .iter()
        .enumerate()
        .map(|(i, p)| render_page(&doc.page, p, &photos, &text, &RenderOptions { page: i + 1, pages: n, ..*opt }).map_err(|e| e.to_string()))
        .collect()
}

fn kind_name(k: CreationKind) -> &'static str {
    match k {
        CreationKind::Print => "print",
        CreationKind::Book => "book",
        CreationKind::Slideshow => "slideshow",
        CreationKind::Web => "web",
    }
}

fn find_template(name: &str) -> Option<Template> {
    builtin_templates().into_iter().find(|t| t.name.eq_ignore_ascii_case(name))
}

/// The document a call describes: `document` (layout JSON object), or `template` (a built-in's
/// name) filled with the call's photos (`ids`, else the selection).
fn document_from(s: &Session, c: &str, p: &Value, kind: Option<CreationKind>) -> Result<Document> {
    let doc = if let Some(d) = p.get("document") {
        let text = serde_json::to_string(d).map_err(|e| bad(c, e.to_string()))?;
        Document::from_json(&text).map_err(|e| bad(c, e.to_string()))?
    } else {
        let name = p.get("template").and_then(Value::as_str).ok_or_else(|| bad(c, "give a `template` name or a `document`"))?;
        let t = find_template(name).ok_or_else(|| bad(c, format!("no template named {name:?} (see layout.templates)")))?;
        let ids: Vec<String> = s.targets(p).into_iter().map(|id| id.0.to_string()).collect();
        let repeat = p.get("repeatOne").and_then(Value::as_bool).unwrap_or(false);
        t.instantiate(&ids, repeat).map_err(|e| bad(c, e.to_string()))?
    };
    if let Some(k) = kind
        && doc.kind != k
    {
        return Err(bad(c, format!("this is a {} layout, not a {}", kind_name(doc.kind), kind_name(k))));
    }
    Ok(doc)
}

fn templates(_: &mut Session, p: &Value) -> Result<Value> {
    let kind = p.get("kind").and_then(Value::as_str);
    let list: Vec<Value> = builtin_templates()
        .into_iter()
        .filter(|t| kind.is_none_or(|k| kind_name(t.document.kind) == k))
        .map(|t| {
            let slots: usize = t.document.pages.iter().map(|p| p.photo_cells().count()).sum();
            json!({"name": t.name, "kind": kind_name(t.document.kind), "builtin": t.builtin, "pages": t.document.pages.len(), "photoCells": slots, "page": {"w": t.document.page.size.w, "h": t.document.page.size.h}})
        })
        .collect();
    Ok(json!({"templates": list}))
}

fn render_cmd(s: &mut Session, p: &Value, c: &str, kind: CreationKind) -> Result<Value> {
    let doc = document_from(s, c, p, Some(kind))?;
    let path = p.get("path").and_then(Value::as_str).filter(|v| !v.trim().is_empty()).ok_or_else(|| bad(c, "missing path"))?;
    let dpi = p.get("dpi").and_then(Value::as_f64).unwrap_or(150.0) as f32;
    let opt = RenderOptions {
        dpi,
        include_bleed: p.get("bleed").and_then(Value::as_bool).unwrap_or(false),
        show_guides: p.get("guides").and_then(Value::as_bool).unwrap_or(false),
        ..RenderOptions::default()
    };
    if path.to_ascii_lowercase().ends_with(".pdf") {
        // vector PDF: photos at `dpi`, shaped text with embedded fonts, optional crop marks
        s.check_write_target(path).map_err(|e| bad(c, e))?;
        let mut settings = dac_print::PrintSettings::default();
        settings.job.dpi = dpi.clamp(36.0, 1200.0);
        settings.options.crop_marks = p.get("cropMarks").and_then(Value::as_bool).unwrap_or(false);
        let (jobs, infos) = prepare_jobs(s, &doc, settings.job.dpi).map_err(|e| bad(c, e))?;
        let photos = run_jobs(jobs, infos).map_err(|e| bad(c, e))?;
        let out = dac_print::output::to_pdf(&doc, &settings, &photos, &dac_print::text::ShapedText::new(), &Default::default(), &mut |_, _| true)
            .map_err(|e| bad(c, e.to_string()))?;
        let bytes = out.files.into_iter().next().unwrap_or_default();
        crate::export::write_file(path, &bytes).map_err(|e| bad(c, e))?;
        return Ok(json!({"pages": [{"path": path, "pages": out.pages, "bytes": bytes.len()}], "warnings": out.warnings}));
    }
    let format = if path.to_ascii_lowercase().ends_with(".png") { ExportFormat::Png } else { ExportFormat::Jpeg };
    let pages = render_document(s, &doc, &opt).map_err(|e| bad(c, e))?;
    let mut written = Vec::new();
    let mut warnings = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        let target = if pages.len() == 1 {
            path.to_string()
        } else {
            match path.rfind('.') {
                Some(dot) => format!("{}-{:03}{}", path.get(..dot).unwrap_or(path), i + 1, path.get(dot..).unwrap_or("")),
                None => format!("{path}-{:03}", i + 1),
            }
        };
        s.check_write_target(&target).map_err(|e| bad(c, e))?;
        let bytes = encode_image(&page.image, &ExportOptions { format, quality: 92, ..Default::default() }).map_err(|e| bad(c, e))?;
        crate::export::write_file(&target, &bytes).map_err(|e| bad(c, e))?;
        warnings.extend(page.warnings.iter().cloned());
        written.push(json!({"path": target, "width": page.image.width, "height": page.image.height}));
    }
    Ok(json!({"pages": written, "warnings": warnings}))
}

fn creation_kind(c: &str, p: &Value) -> Result<CreationKind> {
    match p.get("kind").and_then(Value::as_str).unwrap_or("print") {
        "print" => Ok(CreationKind::Print),
        "book" => Ok(CreationKind::Book),
        "slideshow" => Ok(CreationKind::Slideshow),
        "web" => Ok(CreationKind::Web),
        k => Err(bad(c, format!("unknown kind {k:?} (print, book, slideshow or web)"))),
    }
}

fn save(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "creation.save";
    let kind = creation_kind(c, p)?;
    let name = p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(c, "missing name"))?;
    let doc = document_from(s, c, p, Some(kind))?;
    let mut photos: Vec<PhotoId> = Vec::new();
    for key in doc.photos() {
        let id = PhotoId(key.parse::<u64>().map_err(|_| bad(c, format!("{key:?} is not a photo id")))?);
        if s.catalog.photo(id).is_none() {
            return Err(bad(c, format!("no photo {key}")));
        }
        if !photos.contains(&id) {
            photos.push(id);
        }
    }
    let document = doc.to_json().map_err(|e| bad(c, e.to_string()))?;
    let id = s.catalog.alloc_album_id();
    let album = Album { photos, creation: Some(Creation { kind: kind_name(kind).into(), document }), ..Album::new(id, name) };
    s.commit(&format!("Save {}", kind.label()), Op::AddAlbum { album })?;
    Ok(json!({"id": id.0, "kind": kind_name(kind), "pages": doc.pages.len()}))
}

fn album_id(c: &str, p: &Value) -> Result<AlbumId> {
    p.get("id").and_then(Value::as_u64).map(AlbumId).ok_or_else(|| bad(c, "missing id"))
}

fn get(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "creation.get";
    let id = album_id(c, p)?;
    let a = s.catalog.album(id).ok_or_else(|| bad(c, "no such collection"))?;
    let cr = a.creation.as_ref().ok_or_else(|| bad(c, "not a saved creation"))?;
    let doc: Value = serde_json::from_str(&cr.document).map_err(|e| bad(c, format!("the stored layout is damaged: {e}")))?;
    Ok(json!({"id": id.0, "name": a.name, "kind": cr.kind, "document": doc}))
}

fn update(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "creation.update";
    let id = album_id(c, p)?;
    let kind = {
        let a = s.catalog.album(id).ok_or_else(|| bad(c, "no such collection"))?;
        let cr = a.creation.as_ref().ok_or_else(|| bad(c, "not a saved creation"))?;
        creation_kind(c, &json!({"kind": cr.kind}))?
    };
    let d = p.get("document").ok_or_else(|| bad(c, "missing document"))?;
    let doc = Document::from_json(&serde_json::to_string(d).map_err(|e| bad(c, e.to_string()))?).map_err(|e| bad(c, e.to_string()))?;
    if doc.kind != kind {
        return Err(bad(c, "the document's kind differs from the creation's"));
    }
    let mut photos: Vec<PhotoId> = Vec::new();
    for key in doc.photos() {
        let pid = PhotoId(key.parse::<u64>().map_err(|_| bad(c, format!("{key:?} is not a photo id")))?);
        if !photos.contains(&pid) && s.catalog.photo(pid).is_some() {
            photos.push(pid);
        }
    }
    let document = doc.to_json().map_err(|e| bad(c, e.to_string()))?;
    let ops =
        vec![Op::SetAlbumCreation { id, creation: Some(Creation { kind: kind_name(kind).into(), document }) }, Op::SetAlbumPhotos { id, photos }];
    s.commit(&format!("Edit {}", kind.label()), Op::Batch { ops })?;
    Ok(json!({"id": id.0, "pages": doc.pages.len()}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "layout.templates", "Layout Templates", [], None, "{kind?: print|book|slideshow|web} → {templates: [{name, kind, builtin, pages, photoCells, page: {w, h} (points)}]}", always, templates),
        cmd!(query "print.render", "Render Print Pages", [], None,
            "{path (.jpg or .png; several pages get -001, -002…), template? (built-in name) + ids? (else the selection) + repeatOne?, or document? (layout JSON), dpi? (150), bleed?, guides?} → {pages: [{path, width, height}], warnings}",
            always, |s, p| render_cmd(s, p, "print.render", CreationKind::Print)),
        cmd!(query "book.render", "Render Book Pages", [], None,
            "as print.render, for book layouts",
            always, |s, p| render_cmd(s, p, "book.render", CreationKind::Book)),
        cmd!(
            "creation.save",
            "Save Creation",
            [],
            None,
            "{kind (print|book|slideshow|web), name, template + ids? or document} → {id, kind, pages}: a collection holding the photos and the layout",
            always,
            save
        ),
        cmd!(query "creation.get", "Get Creation", [], None, "{id} → {id, name, kind, document}", always, get),
        cmd!(
            "creation.update",
            "Update Creation",
            [],
            None,
            "{id, document} → {id, pages}: replaces the layout; the collection's photos follow it",
            always,
            update
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutter_strings_parse() {
        assert_eq!(parse_shutter("1/250"), Some(0.004));
        assert_eq!(parse_shutter("1/250 s"), Some(0.004));
        assert_eq!(parse_shutter("2\""), Some(2.0));
        assert_eq!(parse_shutter("1/0"), None);
        assert_eq!(parse_shutter("fast"), None);
    }

    #[test]
    fn print_renders_demo_photos_and_saves_a_creation() {
        let mut s = Session::with_demo();
        let ids: Vec<u64> = s.catalog.photos().take(4).map(|p| p.id.0).collect();
        let dir = std::env::temp_dir().join(format!("layout-render-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sheet.png");
        let out = s.execute("print.render", &json!({"template": "2×2 Cells, A4", "ids": ids, "path": path.to_string_lossy(), "dpi": 30})).unwrap();
        assert_eq!(out["pages"].as_array().unwrap().len(), 1, "{out}");
        assert!(out["warnings"].as_array().unwrap().is_empty(), "{out}");
        assert!(std::fs::read(&path).unwrap().starts_with(b"\x89PNG"));
        let pdf = dir.join("sheet.pdf");
        let out = s
            .execute(
                "print.render",
                &json!({"template": "4×5 Contact Sheet", "ids": ids, "path": pdf.to_string_lossy(), "dpi": 40, "cropMarks": true}),
            )
            .unwrap();
        assert_eq!(out["pages"][0]["pages"], 1, "{out}");
        assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF-"));
        let t = find_template("2×2 Cells, A4").unwrap();
        let doc = t.instantiate(&ids.iter().map(u64::to_string).collect::<Vec<_>>(), false).unwrap();
        let img = &render_document(&mut s, &doc, &RenderOptions { dpi: 30.0, ..RenderOptions::default() }).unwrap()[0].image;
        // a photo landed in the first cell: not the empty-slot grey, not paper white
        let px = img.get(img.width / 4, img.height / 4);
        assert_ne!(px, [255, 255, 255, 255]);
        assert_ne!(px, [200, 200, 200, 255]);
        // wrong kind, unknown template and missing photos are errors
        assert!(s.execute("book.render", &json!({"template": "2×2 Cells, A4", "path": path.to_string_lossy()})).is_err());
        assert!(s.execute("print.render", &json!({"template": "nope", "path": path.to_string_lossy()})).is_err());

        let saved = s.execute("creation.save", &json!({"kind": "print", "name": "Sheet", "template": "4×5 Contact Sheet", "ids": ids})).unwrap();
        let id = saved["id"].as_u64().unwrap();
        let a = s.catalog.album(AlbumId(id)).unwrap();
        assert_eq!(a.photos.len(), 4);
        assert_eq!(a.creation.as_ref().unwrap().kind, "print");
        let got = s.execute("creation.get", &json!({"id": id})).unwrap();
        let mut doc = got["document"].clone();
        doc["pages"][0]["cells"] = json!([]);
        s.execute("creation.update", &json!({"id": id, "document": doc})).unwrap();
        assert!(s.catalog.album(AlbumId(id)).unwrap().photos.is_empty());
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.catalog.album(AlbumId(id)).unwrap().photos.len(), 4);
        assert!(s.execute("creation.save", &json!({"kind": "poster", "name": "x", "template": "Triptych"})).is_err());
        assert!(s.execute("creation.get", &json!({"id": 999_999})).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
