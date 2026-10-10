//! The Book module: a photo book laid out from the filmstrip, edited on a page canvas and
//! exported as a PDF or one JPEG per page. The document and every edit live in `dac-book`
//! ([`dac_book::ops`]); this file draws it and routes the `book.*` commands.
//!
//! Screen: a template browser and saved books on the left, the pages in the middle (multi-page,
//! spread, single page or zoomed page; toolbar below), the panels on the right (Book Settings,
//! Auto Layout, Page, Guides, Cell, Text, Type, Background, Export) and a filmstrip at the bottom
//! whose photos drag onto cells. Dragging a photo cell onto another swaps their photos.
//!
//! The canvas draws photos from the grid's thumbnail textures and text through `dac-text`
//! (shaped exactly as exported), so it matches the export at screen resolution.
//!
//! Saved books are JSON files in the settings folder (`books/`) until saved creations (catalog
//! collections holding a layout document) land; `book.save` / `book.open` then move there.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Mutex;

use dac_book::{Book, CellContent, PageRef};
use dac_catalog::PhotoId;
use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, UiBuilder, pos2, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::DacApp;
use crate::module::{Edge, Module, ModuleId, ModuleKey, PanelId};
use crate::theme::Tokens;
use crate::widgets::register;

/// How the pages are shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BookView {
    /// Every spread, small.
    #[default]
    Multi,
    /// The current spread.
    Spread,
    /// The current page.
    Single,
    /// The current page filling the width.
    Zoomed,
}

impl BookView {
    pub const ALL: [BookView; 4] = [BookView::Multi, BookView::Spread, BookView::Single, BookView::Zoomed];
    fn key(self) -> &'static str {
        match self {
            BookView::Multi => "multi",
            BookView::Spread => "spread",
            BookView::Single => "single",
            BookView::Zoomed => "zoomed",
        }
    }
    fn label(self) -> &'static str {
        match self {
            BookView::Multi => "Multi-Page",
            BookView::Spread => "Spread",
            BookView::Single => "Single Page",
            BookView::Zoomed => "Zoomed Page",
        }
    }
}

/// What the Type panel edits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TypeTarget {
    #[default]
    Cell,
    PhotoText,
    PageText,
    Numbers,
}

impl TypeTarget {
    fn key(self) -> &'static str {
        match self {
            TypeTarget::Cell => "cell",
            TypeTarget::PhotoText => "photoText",
            TypeTarget::PageText => "pageText",
            TypeTarget::Numbers => "numbers",
        }
    }
    fn label(self) -> &'static str {
        match self {
            TypeTarget::Cell => "Text Cell",
            TypeTarget::PhotoText => "Photo Text",
            TypeTarget::PageText => "Page Text",
            TypeTarget::Numbers => "Page Numbers",
        }
    }
}

/// The Book module's state (saved in `ui.json`, so the book in progress survives a restart).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BookUi {
    pub book: Book,
    pub view: BookView,
    pub current: PageRef,
    /// The selected cell on the current page.
    pub cell: Option<usize>,
    /// Auto Layout preset.
    pub preset: String,
    /// Page width in the multi-page view, points on screen.
    pub thumb: f32,
    pub type_target: TypeTarget,
    /// The name typed in the Saved Books box.
    pub save_name: String,
    /// The saved creation (catalog collection) this book was saved to or opened from.
    pub creation: Option<u64>,
    /// Book edits to undo and redo (this session only).
    #[serde(skip)]
    pub undo: Vec<Book>,
    #[serde(skip)]
    pub redo: Vec<Book>,
}

/// Most book edits kept for undo.
const UNDO_LIMIT: usize = 100;

impl Default for BookUi {
    fn default() -> Self {
        BookUi {
            book: Book::default(),
            view: BookView::Multi,
            current: PageRef::Page(0),
            cell: None,
            preset: "One Photo Per Page".into(),
            thumb: 150.0,
            type_target: TypeTarget::Cell,
            save_name: String::new(),
            creation: None,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }
}

// ------------------------------------------------------------------ the module

pub struct BookModule;

pub static BOOK: BookModule = BookModule;

impl Module for BookModule {
    fn id(&self) -> ModuleId {
        ModuleId::Book
    }
    // the module draws its own columns (see `center`)
    fn left_panels(&self) -> &'static [PanelId] {
        &[]
    }
    fn right_panels(&self) -> &'static [PanelId] {
        &[]
    }
    fn toolbar(&self, _ui: &mut egui::Ui, _app: &mut DacApp) {}
    fn center(&self, ui: &mut egui::Ui, app: &mut DacApp) {
        center(ui, app);
    }
    fn keymap(&self) -> &'static [ModuleKey] {
        BOOK_KEYS
    }
}

/// Book keys over the global keymap: ⌘⇧B adds a page with the default template; ⌘Z / ⌘⇧Z undo
/// and redo book edits (the book has its own history, separate from the catalog's).
pub const BOOK_KEYS: &[ModuleKey] = &[("Cmd+Shift+B", "book.addPage", "{}"), ("Cmd+Z", "book.undo", "{}"), ("Cmd+Shift+Z", "book.redo", "{}")];

// ------------------------------------------------------------------ commands

/// The `book.*` UI commands (listed in the module shell's table): the document operations of
/// [`dac_book::ops::OPS`] plus the views, navigation, export and saved books.
pub fn is_book_command(id: &str) -> bool {
    id.starts_with("book.")
}

fn current_status() -> &'static Mutex<ExportStatus> {
    static S: std::sync::OnceLock<Mutex<ExportStatus>> = std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(ExportStatus::default()))
}

/// The last export's progress and outcome.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportStatus {
    pub running: bool,
    pub done: usize,
    pub total: usize,
    pub result: Option<Result<Vec<String>, String>>,
}

fn with_status<T>(f: impl FnOnce(&mut ExportStatus) -> T) -> T {
    let mut g = current_status().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut g)
}

/// Ops that act on a page (and cell): the current ones fill in when the caller leaves them out.
const PAGE_OPS: &[&str] =
    &["book.cell", "book.text", "book.photoText", "book.pageText", "book.type", "book.place", "book.template", "book.removePage"];

fn page_json(r: PageRef) -> Value {
    match r {
        PageRef::Front => json!("front"),
        PageRef::Back => json!("back"),
        PageRef::Page(i) => json!(i + 1),
    }
}

/// Runs a `book.*` command.
pub fn run(app: &mut DacApp, id: &str, p: &Value) -> Result<Value, String> {
    let st = &mut app.ui.book;
    match id {
        "book.view" => {
            let m = p.get("mode").and_then(Value::as_str).ok_or("missing mode (multi|spread|single|zoomed)")?;
            st.view = BookView::ALL.into_iter().find(|v| v.key().eq_ignore_ascii_case(m)).ok_or_else(|| format!("unknown view: {m}"))?;
            Ok(json!({"view": st.view}))
        }
        "book.go" => {
            let v = p.get("page").ok_or("missing page")?;
            let r = PageRef::parse(v).ok_or_else(|| format!("invalid page: {v}"))?;
            st.book.page(r).ok_or_else(|| format!("no {}", r.label()))?;
            st.current = r;
            st.cell = None;
            Ok(json!({"page": page_json(r)}))
        }
        "book.select" => {
            if let Some(v) = p.get("page") {
                let r = PageRef::parse(v).ok_or_else(|| format!("invalid page: {v}"))?;
                st.book.page(r).ok_or_else(|| format!("no {}", r.label()))?;
                st.current = r;
            }
            let cell = p.get("cell").and_then(Value::as_u64).and_then(|c| usize::try_from(c).ok());
            if let Some(c) = cell
                && st.book.page(st.current).is_none_or(|pg| c >= pg.cells.len())
            {
                return Err(format!("no cell {c}"));
            }
            st.cell = cell;
            if let Some(c) = cell.and_then(|c| st.book.page(st.current).and_then(|pg| pg.cells.get(c))) {
                st.type_target = if c.is_photo() { TypeTarget::PhotoText } else { TypeTarget::Cell };
            }
            Ok(json!({"page": page_json(st.current), "cell": st.cell}))
        }
        "book.export" => export(app, id, p),
        "book.undo" | "book.redo" => {
            let (from, to) = if id == "book.undo" { (&mut st.undo, &mut st.redo) } else { (&mut st.redo, &mut st.undo) };
            let b = from.pop().ok_or(if id == "book.undo" { "nothing to undo in the book" } else { "nothing to redo in the book" })?;
            to.push(std::mem::replace(&mut st.book, b));
            if st.book.page(st.current).is_none() {
                st.current = st.book.all_pages().first().copied().unwrap_or(PageRef::Page(0));
            }
            st.cell = st.cell.filter(|c| st.book.page(st.current).is_some_and(|p| *c < p.cells.len()));
            Ok(json!({"undo": st.undo.len(), "redo": st.redo.len()}))
        }
        "book.exportStatus" => Ok(serde_json::to_value(with_status(|s| s.clone())).unwrap_or(Value::Null)),
        "book.save" | "book.open" | "book.saved" | "book.deleteSaved" => saved(app, id, p),
        "book.autoLayout" if p.get("photos").is_none() => {
            let mut ids = app.session.targets(&json!({}));
            if ids.len() < 2 {
                ids = app.session.visible_cloned();
            }
            ids.truncate(2000);
            let mut q = p.clone();
            q["photos"] = json!(ids.iter().map(|i| i.0.to_string()).collect::<Vec<_>>());
            if q.get("preset").is_none() {
                q["preset"] = json!(app.ui.book.preset);
            }
            run_op(app, id, &q)
        }
        _ => {
            let mut q = if p.is_object() { p.clone() } else { json!({}) };
            if PAGE_OPS.contains(&id) && q.get("page").is_none() && !(id == "book.type" && q.get("target").and_then(Value::as_str) == Some("numbers"))
            {
                q["page"] = page_json(st.current);
                if q.get("cell").is_none()
                    && let Some(c) = st.cell
                {
                    q["cell"] = json!(c);
                }
            }
            if id == "book.addPage"
                && q.get("after").is_none()
                && let PageRef::Page(_) = st.current
            {
                q["after"] = page_json(st.current);
            }
            run_op(app, id, &q)
        }
    }
}

fn run_op(app: &mut DacApp, id: &str, p: &Value) -> Result<Value, String> {
    if !dac_book::ops::OPS.iter().any(|o| o.0 == id) {
        return Err(format!("unknown command: {id}"));
    }
    let st = &mut app.ui.book;
    let before = st.book.clone();
    let r = dac_book::ops::apply(&mut st.book, id, p).map_err(|e| e.to_string())?;
    if st.book != before {
        st.undo.push(before);
        if st.undo.len() > UNDO_LIMIT {
            st.undo.remove(0);
        }
        st.redo.clear();
    }
    match id {
        "book.addPage" => {
            if let Some(n) = r.get("page").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()) {
                st.current = PageRef::Page(n.saturating_sub(1));
                st.cell = None;
            }
        }
        "book.autoLayout" | "book.new" | "book.clearLayout" | "book.removePage" | "book.template" | "book.settings" => {
            if st.book.page(st.current).is_none() {
                st.current = st.book.all_pages().first().copied().unwrap_or(PageRef::Page(0));
            }
            if st.cell.is_some_and(|c| st.book.page(st.current).is_none_or(|p| c >= p.cells.len())) {
                st.cell = None;
            }
        }
        _ => {}
    }
    Ok(r)
}

// ------------------------------------------------------------------ saved books

fn books_dir() -> Option<std::path::PathBuf> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        dac_engine::config::config_dir().map(|d| d.join("books"))
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

fn safe_name(n: &str) -> Option<String> {
    let s: String = n.trim().chars().filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.')).take(100).collect();
    let s = s.trim_matches('.').trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Saved book names, sorted.
pub fn saved_books() -> Vec<String> {
    let Some(dir) = books_dir() else { return Vec::new() };
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok()).filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(str::to_string)).collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|s| s.to_lowercase());
    v
}

fn saved(app: &mut DacApp, id: &str, p: &Value) -> Result<Value, String> {
    let dir = books_dir().ok_or("saved books need a settings folder")?;
    let name = || -> Result<String, String> {
        let n = p.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| app.ui.book.book.name.clone());
        safe_name(&n).ok_or_else(|| "a saved book needs a name".to_string())
    };
    match id {
        "book.saved" => Ok(json!(saved_books())),
        "book.save" => {
            let n = name()?;
            // the collection (saved creation) in the catalog
            let doc = dac_book::layoutdoc::to_layout(&app.ui.book.book);
            let document = serde_json::to_value(&doc).map_err(|e| e.to_string())?;
            let existing = app
                .ui
                .book
                .creation
                .filter(|c| app.session.catalog.album(dac_catalog::AlbumId(*c)).is_some_and(|a| a.creation.is_some() && a.name == n));
            let creation = match existing {
                Some(c) => app.run("creation.update", json!({"id": c, "document": document})).map(|_| c)?,
                None => app
                    .run("creation.save", json!({"kind": "book", "name": n, "document": document}))?
                    .get("id")
                    .and_then(Value::as_u64)
                    .ok_or("the collection was not saved")?,
            };
            app.ui.book.creation = Some(creation);
            let mut b = app.ui.book.book.clone();
            b.name = n.clone();
            let text = b.to_json().map_err(|e| e.to_string())?;
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let path = dir.join(format!("{n}.json"));
            let tmp = dir.join(format!(".{n}.json.tmp"));
            std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path)).map_err(|e| format!("{}: {e}", path.display()))?;
            app.ui.book.book.name = n.clone();
            Ok(json!({"saved": n, "path": path.to_string_lossy(), "creation": creation}))
        }
        "book.open" if p.get("id").is_some() => {
            // a saved creation: the lossless book file of the same name when there is one, else
            // the book rebuilt from its layout
            let c = p.get("id").and_then(Value::as_u64).ok_or("id must be a collection id")?;
            let r = app.run("creation.get", json!({"id": c}))?;
            if r.get("kind").and_then(Value::as_str) != Some("book") {
                return Err("that collection is not a saved book".into());
            }
            let n = r.get("name").and_then(Value::as_str).unwrap_or("Book").to_string();
            let file = safe_name(&n).map(|f| dir.join(format!("{f}.json")));
            let b = match file.and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| Book::from_json(&t).ok()) {
                Some(b) => b,
                None => {
                    let doc =
                        dac_layout::Document::from_json(&r.get("document").map(Value::to_string).unwrap_or_default()).map_err(|e| e.to_string())?;
                    dac_book::layoutdoc::from_layout(&doc, &n).map_err(|e| e.to_string())?
                }
            };
            let st = &mut app.ui.book;
            st.undo.push(std::mem::replace(&mut st.book, b));
            st.creation = Some(c);
            st.current = st.book.all_pages().first().copied().unwrap_or(PageRef::Page(0));
            st.cell = None;
            Ok(json!({"opened": n, "pages": st.book.pages.len(), "creation": c}))
        }
        "book.open" => {
            let n = name()?;
            let path = dir.join(format!("{n}.json"));
            let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let b = Book::from_json(&text).map_err(|e| format!("{n}: {e}"))?;
            let st = &mut app.ui.book;
            st.undo.push(std::mem::replace(&mut st.book, b));
            st.creation = None;
            st.current = st.book.all_pages().first().copied().unwrap_or(PageRef::Page(0));
            st.cell = None;
            Ok(json!({"opened": n, "pages": st.book.pages.len()}))
        }
        _ => {
            let n = name()?;
            let path = dir.join(format!("{n}.json"));
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(json!({"deleted": n}))
        }
    }
}

// ------------------------------------------------------------------ export

/// Token values for a catalog photo.
pub fn photo_info(app: &DacApp, id: &str) -> dac_layout::tokens::PhotoInfo {
    id.parse::<u64>().ok().and_then(|n| app.session.catalog.photo(PhotoId(n))).map(|p| dac_engine::creations::photo_info(p)).unwrap_or_default()
}

fn export(app: &mut DacApp, id: &str, p: &Value) -> Result<Value, String> {
    if with_status(|s| s.running) {
        return Err("a book export is already running".into());
    }
    let book = app.ui.book.book.clone();
    book.validate().map_err(|e| e.to_string())?;
    if book.pages.is_empty() {
        return Err("the book has no pages: use Auto Layout or Add Page first".into());
    }
    let kind = match p.get("kind").and_then(Value::as_str) {
        Some(k) if k.eq_ignore_ascii_case("jpeg") || k.eq_ignore_ascii_case("jpg") => dac_book::BookKind::Jpeg,
        Some(k) if k.eq_ignore_ascii_case("pdf") => dac_book::BookKind::Pdf,
        Some(k) => return Err(format!("unknown kind: {k} (pdf|jpeg)")),
        None => book.settings.kind,
    };
    let path = match p.get("path").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
        Some(s) => s.to_string(),
        None => {
            let base = safe_name(&book.name).unwrap_or_else(|| "Book".into());
            let req = match kind {
                dac_book::BookKind::Pdf => crate::pick::PickRequest::save("Export Book", "PDF", &["pdf"], format!("{base}.pdf")),
                dac_book::BookKind::Jpeg => crate::pick::PickRequest::folder("Export Book Pages to Folder"),
            };
            match crate::pick::ask(app, id, p, "path", req, |_| None) {
                crate::pick::Picked::Now(paths) => paths.into_iter().next().ok_or("cancelled")?,
                crate::pick::Picked::Later => return Ok(json!({"waiting": "dialog"})),
                crate::pick::Picked::Unavailable => return Err("no save dialog here; pass path".into()),
            }
        }
    };
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (kind, path);
        Err("book export is not available in the browser yet".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let layout = dac_book::layoutdoc::to_layout(&book);
        let (jobs, infos) = dac_engine::creations::prepare_jobs(&mut app.session, &layout, book.settings.resolution as f32)?;
        let total = book.all_pages().len();
        with_status(|s| *s = ExportStatus { running: true, done: 0, total, result: None });
        let work = move || -> Result<Vec<String>, String> {
            let src = dac_engine::creations::run_jobs(jobs, infos)?;
            let mut engine = dac_text::TextEngine::with_system_fonts();
            let mut progress = |done: usize, total: usize| {
                with_status(|s| {
                    s.done = done;
                    s.total = total;
                });
                true
            };
            write_export(&book, kind, &path, &src, &mut engine, &mut progress)
        };
        let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
        if wait {
            let r = work();
            with_status(|s| {
                s.running = false;
                s.done = total;
                s.result = Some(r.clone());
            });
            return r.map(|files| json!({"files": files}));
        }
        std::thread::spawn(move || {
            let r =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|_| Err("the book export failed unexpectedly".into()));
            with_status(|s| {
                s.running = false;
                s.done = s.total;
                s.result = Some(r);
            });
        });
        Ok(json!({"started": true, "pages": total}))
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn write_export(
    book: &Book,
    kind: dac_book::BookKind,
    path: &str,
    src: &dyn dac_layout::render::PhotoSource,
    engine: &mut dac_text::TextEngine,
    progress: dac_book::export::Progress,
) -> Result<Vec<String>, String> {
    let write = |p: &std::path::Path, bytes: &[u8]| -> Result<(), String> {
        let tmp = p.with_extension("part");
        std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, p)).map_err(|e| format!("{}: {e}", p.display()))
    };
    match kind {
        dac_book::BookKind::Pdf => {
            let mut p = std::path::PathBuf::from(path);
            if p.extension().is_none_or(|e| !e.eq_ignore_ascii_case("pdf")) {
                p.set_extension("pdf");
            }
            let bytes = dac_book::export::export_pdf(book, src, engine, progress).map_err(|e| e.to_string())?;
            if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            write(&p, &bytes)?;
            Ok(vec![p.to_string_lossy().into_owned()])
        }
        dac_book::BookKind::Jpeg => {
            let p = std::path::PathBuf::from(path);
            let (dir, stem) = if p.is_dir() || path.ends_with(['/', '\\']) {
                (p.clone(), safe_name(&book.name).unwrap_or_else(|| "Book".into()))
            } else {
                (
                    p.parent().map(std::path::Path::to_path_buf).unwrap_or_default(),
                    p.file_stem().and_then(|s| s.to_str()).unwrap_or("Book").to_string(),
                )
            };
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let pages = dac_book::export::export_jpegs(book, src, engine, progress).map_err(|e| e.to_string())?;
            let mut out = Vec::with_capacity(pages.len());
            for (suffix, bytes) in pages {
                let f = dir.join(format!("{stem}-{suffix}.jpg"));
                write(&f, &bytes)?;
                out.push(f.to_string_lossy().into_owned());
            }
            Ok(out)
        }
    }
}

// ------------------------------------------------------------------ text textures

struct TextCache {
    engine: dac_text::TextEngine,
    tex: HashMap<u64, egui::TextureHandle>,
    families: Vec<String>,
}

thread_local! {
    static TEXT: RefCell<Option<TextCache>> = const { RefCell::new(None) };
}

fn with_text<T>(f: impl FnOnce(&mut TextCache) -> T) -> T {
    TEXT.with(|c| {
        let mut c = c.borrow_mut();
        let cache = c.get_or_insert_with(|| {
            #[cfg(not(target_arch = "wasm32"))]
            let mut engine = dac_text::TextEngine::with_system_fonts();
            #[cfg(target_arch = "wasm32")]
            let mut engine = dac_text::TextEngine::new();
            let families = engine.fonts.families();
            TextCache { engine, tex: HashMap::new(), families }
        });
        f(cache)
    })
}

fn font_families() -> Vec<String> {
    with_text(|c| c.families.clone())
}

/// A text item as a texture at `scale` screen pixels per point.
fn text_texture(ctx: &egui::Context, item: &dac_book::render::TextItem, scale: f32) -> Option<egui::TextureHandle> {
    use std::hash::{Hash, Hasher};
    let (w, h) = ((item.rect.w * scale).round() as usize, (item.rect.h * scale).round() as usize);
    if w == 0 || h == 0 || w > 8000 || h > 8000 {
        return None;
    }
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    item.text.hash(&mut hs);
    serde_json::to_string(&item.style).unwrap_or_default().hash(&mut hs);
    (w, h).hash(&mut hs);
    let key = hs.finish();
    with_text(|c| {
        if let Some(t) = c.tex.get(&key) {
            return Some(t.clone());
        }
        if c.tex.len() > 600 {
            c.tex.clear();
        }
        let placed = dac_book::render::layout_item(&mut c.engine, item, 72.0 * scale);
        let mut img = dac_raster::Rgba8::new(w, h);
        dac_book::text::draw(&mut img, &placed, 0.0, 0.0, w as f32, h as f32);
        let bytes: Vec<u8> = img.data.iter().flatten().copied().collect();
        let ci = egui::ColorImage::from_rgba_premultiplied([w, h], &bytes);
        let t = ctx.load_texture(format!("book-text-{key}"), ci, egui::TextureOptions::LINEAR);
        c.tex.insert(key, t.clone());
        Some(t)
    })
}

/// Token values from the catalog, for the canvas.
struct CatalogInfo<'a>(&'a DacApp);

impl dac_layout::render::PhotoSource for CatalogInfo<'_> {
    fn image(&self, _: &str, _: usize) -> Result<dac_raster::Rgba8, String> {
        Err("not used on screen".into())
    }
    fn info(&self, photo: &str) -> dac_layout::tokens::PhotoInfo {
        photo_info(self.0, photo)
    }
}

// ------------------------------------------------------------------ drawing

fn c32(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn photo_tex(app: &mut DacApp, id: &str) -> Option<(egui::TextureId, [usize; 2])> {
    let pid = PhotoId(id.parse().ok()?);
    app.session.catalog.photo(pid)?;
    crate::panels::grid::request_thumb(app, pid, 512, 6);
    app.renderer.thumb(pid).map(|t| (t.tex.id(), t.size))
}

/// Paints a photo filling/fitting `cell` like the export does.
fn paint_photo(p: &egui::Painter, tex: egui::TextureId, size: [usize; 2], cell: Rect, fill: bool, zoom: f32, pan: [f32; 2], tint: Color32) {
    let lr = dac_layout::Rect::new(cell.left(), cell.top(), cell.width(), cell.height());
    let pc = dac_layout::PhotoCell { fit: if fill { dac_layout::Fit::Fill } else { dac_layout::Fit::Fit }, zoom, pan, ..Default::default() };
    let (iw, ih) = (size[0].max(1) as f32, size[1].max(1) as f32);
    let (s, ox, oy) = dac_layout::render::photo_placement(lr, iw, ih, &pc);
    if !(s.is_finite() && s > 0.0) {
        return;
    }
    let shown = Rect::from_min_size(pos2(ox, oy), vec2(iw * s, ih * s));
    let vis = shown.intersect(cell);
    if vis.width() <= 0.0 || vis.height() <= 0.0 {
        return;
    }
    let uv = Rect::from_min_max(
        pos2((vis.left() - shown.left()) / shown.width(), (vis.top() - shown.top()) / shown.height()),
        pos2((vis.right() - shown.left()) / shown.width(), (vis.bottom() - shown.top()) / shown.height()),
    );
    p.image(tex, vis, uv, tint);
}

/// A drag of a filmstrip photo.
#[derive(Clone, Copy, Debug)]
struct PhotoDrag(u64);
/// A drag of a placed photo (swap).
#[derive(Clone, Copy, Debug)]
struct CellDrag(PageRef, usize);

/// Draws one page at `rect` (the trim box on screen). Returns whether it was clicked.
fn draw_page(ui: &mut egui::Ui, app: &mut DacApp, r: PageRef, rect: Rect, interactive: bool) -> bool {
    let t = Tokens::get(ui.ctx());
    let Some(page) = app.ui.book.book.page(r).cloned() else { return false };
    let book = app.ui.book.book.clone();
    let trim = book.trim();
    let k = rect.width() / trim.w.max(1.0);
    let to_screen = |lr: dac_layout::Rect| Rect::from_min_size(rect.min + vec2(lr.x * k, lr.y * k), vec2(lr.w * k, lr.h * k));
    let painter = ui.painter_at(rect.expand(2.0));
    let bg = book.background_of(r).clone();
    painter.rect_filled(rect, 0.0, c32(bg.color));
    if let Some(id) = &bg.photo
        && let Some((tex, size)) = photo_tex(app, id)
    {
        let a = (bg.photo_opacity.clamp(0.0, 1.0) * 255.0) as u8;
        paint_photo(&painter, tex, size, rect, true, 1.0, [0.0, 0.0], Color32::from_white_alpha(a));
    }
    let ga = (bg.graphic_opacity.clamp(0.0, 1.0) * 255.0) as u8;
    for g in dac_book::render::graphic_rects(bg.graphic, trim.w, trim.h) {
        let gc = c32(bg.graphic_color);
        painter.rect_filled(to_screen(g), 0.0, Color32::from_rgba_unmultiplied(gc.r(), gc.g(), gc.b(), ga));
    }
    let current = app.ui.book.current == r;
    let mut clicked = false;
    for (i, c) in page.cells.iter().enumerate() {
        let cell_rect = to_screen(c.rect.to_points(trim));
        let inner = to_screen(c.rect.to_points(trim).inset(&c.padding.insets()));
        if let CellContent::Photo { photo, fill, zoom, pan, .. } = &c.content {
            match photo.as_deref().and_then(|id| photo_tex(app, id)) {
                Some((tex, size)) => paint_photo(&painter, tex, size, inner, *fill, *zoom, *pan, Color32::WHITE),
                None => {
                    painter.rect_filled(inner, 0.0, Color32::from_gray(205));
                    if photo.is_none() && rect.width() > 120.0 {
                        painter.text(
                            inner.center(),
                            Align2::CENTER_CENTER,
                            "+",
                            t.font((inner.height() * 0.25).clamp(8.0, 40.0)),
                            Color32::from_gray(150),
                        );
                    }
                }
            }
        }
        if interactive {
            let id = ui.id().with(("book-cell", format!("{r:?}"), i));
            let resp = ui.interact(cell_rect, id, Sense::click_and_drag());
            register(ui.ctx(), format!("book:cell:{i}"), cell_rect);
            if resp.clicked() {
                app.ui.book.current = r;
                let _ = run(app, "book.select", &json!({"page": page_json(r), "cell": i}));
                clicked = true;
            }
            if c.is_photo() && c.photo_id().is_some() {
                if ui.input(|i| i.modifiers.alt) && resp.dragged() {
                    // Alt-drag pans the photo in its cell
                    let d = resp.drag_delta();
                    if let CellContent::Photo { pan, .. } = &c.content {
                        let np = [
                            (pan[0] - d.x / cell_rect.width().max(1.0) * 2.0).clamp(-1.0, 1.0),
                            (pan[1] - d.y / cell_rect.height().max(1.0) * 2.0).clamp(-1.0, 1.0),
                        ];
                        let _ = run(app, "book.cell", &json!({"page": page_json(r), "cell": i, "pan": np}));
                    }
                } else {
                    resp.dnd_set_drag_payload(CellDrag(r, i));
                }
            }
            if c.is_photo() {
                if let Some(d) = resp.dnd_release_payload::<PhotoDrag>() {
                    let _ = run(app, "book.place", &json!({"page": page_json(r), "cell": i, "photo": d.0.to_string()}));
                } else if let Some(d) = resp.dnd_release_payload::<CellDrag>()
                    && (d.0 != r || d.1 != i)
                {
                    let _ = run(app, "book.swap", &json!({"from": {"page": page_json(d.0), "cell": d.1}, "to": {"page": page_json(r), "cell": i}}));
                }
                if resp.dnd_hover_payload::<PhotoDrag>().is_some() || resp.dnd_hover_payload::<CellDrag>().is_some() {
                    painter.rect_stroke(cell_rect, 0.0, Stroke::new(2.0, t.accent), StrokeKind::Inside);
                }
            }
        }
    }
    // text
    let editor = interactive || rect.width() > 200.0;
    let src = CatalogInfo(app);
    let items = dac_book::render::text_items(&book, r, &src, editor && book.guides.show);
    let ppp = ui.ctx().pixels_per_point();
    for item in items {
        let tr = to_screen(item.rect);
        if let Some(tex) = text_texture(ui.ctx(), &item, k * ppp) {
            painter.image(tex.id(), tr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
    }
    // guides
    let g = book.guides;
    if g.show && editor {
        if g.text_safe {
            let s = dac_book::TEXT_SAFE * k;
            painter.rect_stroke(rect.shrink(s), 0.0, Stroke::new(1.0, Color32::from_rgba_unmultiplied(40, 140, 230, 150)), StrokeKind::Inside);
        }
        if g.bleed && book.settings.bleed > 0.0 {
            let b = book.settings.bleed * k;
            painter.rect_stroke(
                rect.expand(b.min(2.0)),
                0.0,
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(230, 40, 40, 160)),
                StrokeKind::Outside,
            );
        }
        if g.photo_cells {
            for c in &page.cells {
                painter.rect_stroke(to_screen(c.rect.to_points(trim)), 0.0, Stroke::new(1.0, Color32::from_gray(150)), StrokeKind::Inside);
            }
        }
    }
    if current && let Some(c) = app.ui.book.cell.and_then(|i| page.cells.get(i)) {
        painter.rect_stroke(to_screen(c.rect.to_points(trim)), 0.0, Stroke::new(2.0, Color32::from_rgb(250, 200, 40)), StrokeKind::Inside);
    }
    let border = if current { t.accent } else { Color32::from_gray(90) };
    painter.rect_stroke(rect, 0.0, Stroke::new(if current { 2.0 } else { 1.0 }, border), StrokeKind::Outside);
    if interactive {
        let resp = ui.interact(rect, ui.id().with(("book-page", format!("{r:?}"))), Sense::click());
        register(ui.ctx(), format!("book:page:{}", page_json(r).to_string().trim_matches('"')), rect);
        if resp.clicked() && !clicked {
            app.ui.book.current = r;
            app.ui.book.cell = None;
            clicked = true;
        }
        if resp.double_clicked() {
            app.ui.book.current = r;
            app.ui.book.view = BookView::Single;
        }
    }
    clicked
}

fn page_label(book: &Book, r: PageRef) -> String {
    match r {
        PageRef::Page(i) if book.pages.get(i).is_some_and(|p| p.template == "blank" && p.cells.is_empty()) => format!("Page {} (blank)", i + 1),
        _ => r.label(),
    }
}

/// The pages arranged as spreads: the front cover alone, then left/right pairs, then the back.
fn spreads(book: &Book) -> Vec<[Option<PageRef>; 2]> {
    let mut v = Vec::new();
    if book.has_cover() {
        v.push([None, Some(PageRef::Front)]);
    }
    for s in book.spreads() {
        v.push([s[0].map(PageRef::Page), s[1].map(PageRef::Page)]);
    }
    if book.has_cover() {
        v.push([Some(PageRef::Back), None]);
    }
    v
}

fn draw_spread(ui: &mut egui::Ui, app: &mut DacApp, s: [Option<PageRef>; 2], origin: egui::Pos2, pw: f32, ph: f32, labels: bool) {
    let t = Tokens::get(ui.ctx());
    for (side, r) in s.iter().enumerate() {
        let rect = Rect::from_min_size(origin + vec2(side as f32 * pw, 0.0), vec2(pw, ph));
        if let Some(r) = r {
            ui.painter().rect_filled(rect.translate(vec2(2.0, 3.0)), 0.0, Color32::from_black_alpha(60));
            draw_page(ui, app, *r, rect, true);
            if labels {
                let label = page_label(&app.ui.book.book, *r);
                ui.painter().text(pos2(rect.center().x, rect.bottom() + 9.0), Align2::CENTER_CENTER, label, t.font(10.5), t.text_dim);
            }
        }
    }
}

fn canvas(ui: &mut egui::Ui, app: &mut DacApp, area: Rect) {
    let book = app.ui.book.book.clone();
    let trim = book.trim();
    let aspect = trim.h / trim.w.max(1.0);
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_filled(area, 0.0, t.canvas);
    register(ui.ctx(), "view:module:book", area);
    if book.pages.is_empty() && !book.has_cover() {
        ui.painter().text(
            area.center(),
            Align2::CENTER_CENTER,
            crate::i18n::tr("Use Auto Layout or Add Page to start the book"),
            t.font(15.0),
            t.text_dim,
        );
        return;
    }
    if book.page(app.ui.book.current).is_none() {
        app.ui.book.current = book.all_pages().first().copied().unwrap_or(PageRef::Page(0));
    }
    match app.ui.book.view {
        BookView::Multi => {
            let pw = app.ui.book.thumb.clamp(60.0, 400.0);
            let ph = pw * aspect;
            let gap = vec2(28.0, 34.0);
            let per_row = (((area.width() - gap.x) / (2.0 * pw + gap.x)).floor() as usize).max(1);
            let list = spreads(&book);
            let rows = list.len().div_ceil(per_row);
            ui.scope_builder(UiBuilder::new().max_rect(area), |ui| {
                egui::ScrollArea::vertical().id_salt("book-multi").auto_shrink([false, false]).show(ui, |ui| {
                    let total = vec2(area.width(), rows as f32 * (ph + gap.y) + gap.y);
                    let (r, _) = ui.allocate_exact_size(total, Sense::hover());
                    let row_w = per_row as f32 * (2.0 * pw + gap.x) - gap.x;
                    let x0 = r.left() + ((r.width() - row_w) / 2.0).max(gap.x / 2.0);
                    for (i, s) in list.iter().enumerate() {
                        let (row, col) = (i / per_row, i % per_row);
                        let o = pos2(x0 + col as f32 * (2.0 * pw + gap.x), r.top() + gap.y / 2.0 + row as f32 * (ph + gap.y));
                        if ui.is_rect_visible(Rect::from_min_size(o, vec2(2.0 * pw, ph))) {
                            draw_spread(ui, app, *s, o, pw, ph, true);
                        }
                    }
                });
            });
        }
        BookView::Spread | BookView::Single => {
            let spread = app.ui.book.view == BookView::Spread;
            let cur = app.ui.book.current;
            let s = if spread { spreads(&book).into_iter().find(|s| s.contains(&Some(cur))).unwrap_or([None, Some(cur)]) } else { [Some(cur), None] };
            let cols = if spread { 2.0 } else { 1.0 };
            let m = 40.0;
            let pw = ((area.width() - 2.0 * m) / cols).min((area.height() - 2.0 * m) / aspect).max(20.0);
            let ph = pw * aspect;
            let o = pos2(area.center().x - pw * cols / 2.0, area.center().y - ph / 2.0);
            let s = if spread { s } else { [s[0], None] };
            draw_spread(ui, app, s, o, pw, ph, true);
        }
        BookView::Zoomed => {
            let cur = app.ui.book.current;
            ui.scope_builder(UiBuilder::new().max_rect(area), |ui| {
                egui::ScrollArea::vertical().id_salt("book-zoomed").auto_shrink([false, false]).show(ui, |ui| {
                    let pw = (area.width() - 40.0).max(50.0);
                    let ph = pw * aspect;
                    let (r, _) = ui.allocate_exact_size(vec2(area.width(), ph + 40.0), Sense::hover());
                    draw_page(ui, app, cur, Rect::from_min_size(r.min + vec2(20.0, 20.0), vec2(pw, ph)), true);
                });
            });
        }
    }
}

fn toolbar(ui: &mut egui::Ui, app: &mut DacApp, r: Rect) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_filled(r, 0.0, t.chrome);
    ui.scope_builder(UiBuilder::new().max_rect(r.shrink2(vec2(10.0, 5.0))), |ui| {
        ui.horizontal_centered(|ui| {
            for v in BookView::ALL {
                let on = app.ui.book.view == v;
                let resp = ui.selectable_label(on, crate::i18n::tr(v.label()));
                register(ui.ctx(), format!("book:view:{}", v.key()), resp.rect);
                if resp.clicked() {
                    app.ui.book.view = v;
                }
            }
            ui.separator();
            let all = app.ui.book.book.all_pages();
            let pos = all.iter().position(|p| *p == app.ui.book.current);
            if ui.button("◀").clicked()
                && let Some(i) = pos.and_then(|i| i.checked_sub(1))
                && let Some(p) = all.get(i)
            {
                app.ui.book.current = *p;
                app.ui.book.cell = None;
            }
            ui.label(format!("{} / {}", app.ui.book.current.label(), app.ui.book.book.pages.len()));
            if ui.button("▶").clicked()
                && let Some(p) = pos.and_then(|i| all.get(i + 1))
            {
                app.ui.book.current = *p;
                app.ui.book.cell = None;
            }
            if app.ui.book.view == BookView::Multi {
                ui.separator();
                ui.add(egui::Slider::new(&mut app.ui.book.thumb, 60.0..=400.0).show_value(false).text(crate::i18n::tr("Size")));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = match app.ui.book.book.settings.kind {
                    dac_book::BookKind::Pdf => "Export Book to PDF…",
                    dac_book::BookKind::Jpeg => "Export Book to JPEG…",
                };
                let resp = ui.button(crate::i18n::tr(label));
                register(ui.ctx(), "book:export", resp.rect);
                if resp.clicked() {
                    op(app, ui.ctx(), "book.export", json!({}));
                }
                let status = with_status(|s| s.clone());
                if status.running {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                    ui.label(format!("Exporting {}/{}", status.done, status.total));
                } else if let Some(r) = &status.result {
                    match r {
                        Ok(files) => ui.label(egui::RichText::new(format!("Exported {} file(s)", files.len())).color(t.text_dim)),
                        Err(e) => ui.label(egui::RichText::new(e.clone()).color(Color32::from_rgb(230, 90, 80))),
                    };
                }
            });
        });
    });
}

/// A filmstrip of the photos shown in Library; each one drags onto a photo cell.
fn filmstrip(ui: &mut egui::Ui, app: &mut DacApp, r: Rect) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_filled(r, 0.0, t.chrome);
    register(ui.ctx(), "book:filmstrip", r);
    let ids = app.session.visible_cloned();
    let used: HashMap<String, usize> = app.ui.book.book.photos().into_iter().fold(HashMap::new(), |mut m, p| {
        *m.entry(p.to_string()).or_insert(0) += 1;
        m
    });
    let h = (r.height() - 12.0).max(20.0);
    ui.scope_builder(UiBuilder::new().max_rect(r.shrink(4.0)), |ui| {
        egui::ScrollArea::horizontal().id_salt("book-film").auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal(|ui| {
                for id in ids.iter().take(5000) {
                    let (cell, resp) = ui.allocate_exact_size(vec2(h, h), Sense::click_and_drag());
                    if !ui.is_rect_visible(cell) {
                        continue;
                    }
                    let inner = cell.shrink(3.0);
                    crate::panels::grid::request_thumb(app, *id, 256, 8);
                    ui.painter().rect_filled(cell, 2.0, Color32::from_gray(45));
                    if let Some(tex) = app.renderer.thumb(*id) {
                        paint_photo(ui.painter(), tex.tex.id(), tex.size, inner, false, 1.0, [0.0, 0.0], Color32::WHITE);
                    }
                    if let Some(n) = used.get(&id.0.to_string()) {
                        let b = Rect::from_min_size(cell.right_top() + vec2(-18.0, 3.0), vec2(15.0, 13.0));
                        ui.painter().rect_filled(b, 3.0, t.accent);
                        ui.painter().text(b.center(), Align2::CENTER_CENTER, n.to_string(), t.font(9.0), Color32::WHITE);
                    }
                    register(ui.ctx(), format!("book:film:{}", id.0), cell);
                    resp.dnd_set_drag_payload(PhotoDrag(id.0));
                    if resp.clicked() {
                        let _ = app.run("library.select", json!({"ids": [id.0]}));
                    }
                }
            });
        });
    });
}

/// Runs a book command from a panel, toasting errors.
fn op(app: &mut DacApp, ctx: &egui::Context, id: &str, p: Value) {
    if let Err(e) = run(app, id, &p) {
        app.toast_error(ctx, e);
    }
}

fn center(ui: &mut egui::Ui, app: &mut DacApp) {
    let t = Tokens::get(ui.ctx());
    let mut area = ui.available_rect_before_wrap();
    app.canvas_rect = Some(area);
    ui.allocate_rect(area, Sense::hover());
    if crate::module::edge_visible(app, Edge::Bottom) {
        let film = Rect::from_min_max(pos2(area.left(), area.bottom() - t.film_h), area.max);
        area.max.y = film.top();
        filmstrip(ui, app, film);
    }
    if app.ui.right_edge {
        let w = 300.0f32.min(area.width() * 0.45);
        let right = Rect::from_min_max(pos2(area.right() - w, area.top()), area.max);
        area.max.x = right.left();
        ui.scope_builder(UiBuilder::new().max_rect(right), |ui| right_panels(ui, app, right));
    }
    if app.ui.left_panel {
        let w = 210.0f32.min(area.width() * 0.3);
        let left = Rect::from_min_max(area.min, pos2(area.left() + w, area.bottom()));
        area.min.x = left.right();
        ui.scope_builder(UiBuilder::new().max_rect(left), |ui| left_panels(ui, app, left));
    }
    let bar = Rect::from_min_max(pos2(area.left(), area.bottom() - 34.0), area.max);
    area.max.y = bar.top();
    canvas(ui, app, area);
    toolbar(ui, app, bar);
}

// ------------------------------------------------------------------ panels

fn side_frame(ui: &mut egui::Ui, r: Rect, salt: &str, add: impl FnOnce(&mut egui::Ui)) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_filled(r, 0.0, t.chrome);
    ui.painter().line_segment([r.left_top(), r.left_bottom()], Stroke::new(1.0, t.divider));
    egui::ScrollArea::vertical().id_salt(salt).auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
            ui.set_width(r.width() - 24.0);
            ui.spacing_mut().item_spacing.y = 5.0;
            add(ui);
        });
    });
}

fn section(ui: &mut egui::Ui, title: &str, open: bool, add: impl FnOnce(&mut egui::Ui)) {
    let resp = egui::CollapsingHeader::new(egui::RichText::new(crate::i18n::tr(title)).strong()).default_open(open).show(ui, add);
    register(ui.ctx(), format!("book:panel:{title}"), resp.header_response.rect);
}

fn left_panels(ui: &mut egui::Ui, app: &mut DacApp, r: Rect) {
    side_frame(ui, r, "book-left", |ui| {
        section(ui, "Page Templates", true, |ui| templates_panel(ui, app));
        section(ui, "Saved Books", true, |ui| saved_panel(ui, app));
    });
}

fn templates_panel(ui: &mut egui::Ui, app: &mut DacApp) {
    ui.label(egui::RichText::new(crate::i18n::tr("Click to apply to the current page; ★ marks favourites.")).small());
    let all = dac_book::templates::builtin();
    let favs = app.ui.book.book.favorites.clone();
    let mut groups: Vec<(String, Vec<&dac_book::templates::Template>)> = Vec::new();
    if !favs.is_empty() {
        groups.push(("Favorites".into(), all.iter().filter(|t| favs.iter().any(|f| f == t.id)).collect()));
    }
    for g in dac_book::templates::Group::ALL {
        groups.push((g.label().into(), all.iter().filter(|t| t.group == g).collect()));
    }
    let cur_tpl = app.ui.book.book.page(app.ui.book.current).map(|p| p.template.clone()).unwrap_or_default();
    for (name, list) in groups {
        ui.label(egui::RichText::new(crate::i18n::tr(&name)).small().strong());
        ui.horizontal_wrapped(|ui| {
            for tpl in list {
                let (rect, resp) = ui.allocate_exact_size(vec2(56.0, 44.0), Sense::click());
                let p = ui.painter();
                let on = tpl.id == cur_tpl;
                p.rect_filled(rect, 2.0, if on { Color32::from_gray(235) } else { Color32::from_gray(200) });
                for c in &tpl.cells {
                    let cr = Rect::from_min_size(
                        rect.min + vec2(c.rect.x * rect.width(), c.rect.y * rect.height()),
                        vec2(c.rect.w * rect.width(), c.rect.h * rect.height()),
                    );
                    if c.is_photo() {
                        p.rect_filled(cr, 0.0, Color32::from_gray(120));
                    } else {
                        for l in 0..3 {
                            let y = cr.top() + 3.0 + l as f32 * 4.0;
                            if y < cr.bottom() {
                                p.line_segment([pos2(cr.left() + 2.0, y), pos2(cr.right() - 2.0, y)], Stroke::new(1.0, Color32::from_gray(140)));
                            }
                        }
                    }
                }
                if favs.iter().any(|f| f == tpl.id) {
                    p.text(
                        rect.right_top() + vec2(-6.0, 6.0),
                        Align2::CENTER_CENTER,
                        "★",
                        egui::FontId::proportional(10.0),
                        Color32::from_rgb(240, 190, 40),
                    );
                }
                register(ui.ctx(), format!("book:template:{}", tpl.id), rect);
                let resp = resp.on_hover_text(crate::i18n::tr(tpl.name));
                if resp.clicked() {
                    op(app, ui.ctx(), "book.template", json!({"template": tpl.id}));
                }
                resp.context_menu(|ui| {
                    let fav = favs.iter().any(|f| f == tpl.id);
                    if ui.button(crate::i18n::tr(if fav { "Remove from Favorites" } else { "Add to Favorites" })).clicked() {
                        op(app, ui.ctx(), "book.favorite", json!({"template": tpl.id, "on": !fav}));
                        ui.close();
                    }
                    if ui.button(crate::i18n::tr("Add Page with This Template")).clicked() {
                        op(app, ui.ctx(), "book.addPage", json!({"template": tpl.id}));
                        ui.close();
                    }
                });
            }
        });
    }
}

fn saved_panel(ui: &mut egui::Ui, app: &mut DacApp) {
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut app.ui.book.save_name).hint_text(app.ui.book.book.name.clone()).desired_width(110.0));
        if ui.button(crate::i18n::tr("Save")).clicked() {
            let n = if app.ui.book.save_name.trim().is_empty() { app.ui.book.book.name.clone() } else { app.ui.book.save_name.clone() };
            op(app, ui.ctx(), "book.save", json!({"name": n}));
        }
    });
    for n in saved_books() {
        ui.horizontal(|ui| {
            if ui.link(&n).clicked() {
                op(app, ui.ctx(), "book.open", json!({"name": n}));
            }
            if ui.small_button("✕").on_hover_text(crate::i18n::tr("Delete Saved Book")).clicked() {
                op(app, ui.ctx(), "book.deleteSaved", json!({"name": n}));
            }
        });
    }
}

fn right_panels(ui: &mut egui::Ui, app: &mut DacApp, r: Rect) {
    side_frame(ui, r, "book-right", |ui| {
        section(ui, "Book Settings", true, |ui| settings_panel(ui, app));
        section(ui, "Auto Layout", true, |ui| auto_panel(ui, app));
        section(ui, "Page", true, |ui| page_panel(ui, app));
        section(ui, "Guides", false, |ui| guides_panel(ui, app));
        section(ui, "Cell", true, |ui| cell_panel(ui, app));
        section(ui, "Text", true, |ui| text_panel(ui, app));
        section(ui, "Type", true, |ui| type_panel(ui, app));
        section(ui, "Background", false, |ui| background_panel(ui, app));
    });
}

fn combo<T: Copy + PartialEq>(ui: &mut egui::Ui, salt: &str, label: &str, cur: T, all: &[T], name: impl Fn(T) -> String) -> Option<T> {
    let mut out = None;
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr(label));
        egui::ComboBox::from_id_salt(salt).selected_text(name(cur)).show_ui(ui, |ui| {
            for v in all {
                if ui.selectable_label(*v == cur, name(*v)).clicked() && *v != cur {
                    out = Some(*v);
                }
            }
        });
    });
    out
}

fn enum_key<T: Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn settings_panel(ui: &mut egui::Ui, app: &mut DacApp) {
    let ctx = ui.ctx().clone();
    let s = app.ui.book.book.settings.clone();
    let mut name = app.ui.book.book.name.clone();
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Name"));
        if ui.text_edit_singleline(&mut name).changed() {
            op(app, &ctx, "book.settings", json!({"name": name}));
        }
    });
    if let Some(k) = combo(ui, "book-kind", "Book", s.kind, &[dac_book::BookKind::Pdf, dac_book::BookKind::Jpeg], |k| match k {
        dac_book::BookKind::Pdf => "PDF".into(),
        dac_book::BookKind::Jpeg => "JPEG".into(),
    }) {
        op(app, &ctx, "book.settings", json!({"kind": enum_key(k)}));
    }
    let mut sizes: Vec<dac_book::BookSize> = dac_book::BookSize::PRESETS.to_vec();
    let custom = match s.size {
        dac_book::BookSize::Custom { .. } => s.size,
        _ => dac_book::BookSize::Custom { w: 9.0, h: 9.0 },
    };
    sizes.push(custom);
    if let Some(sz) = combo(ui, "book-size", "Size", s.size, &sizes, |b| b.label()) {
        let mut p = json!({"size": sz.key()});
        if let dac_book::BookSize::Custom { w, h } = sz {
            p["width"] = json!(w);
            p["height"] = json!(h);
        }
        op(app, &ctx, "book.settings", p);
    }
    if let dac_book::BookSize::Custom { mut w, mut h } = s.size {
        ui.horizontal(|ui| {
            let a = ui.add(egui::DragValue::new(&mut w).range(2.0..=40.0).speed(0.05).suffix(" in"));
            ui.label("×");
            let b = ui.add(egui::DragValue::new(&mut h).range(2.0..=40.0).speed(0.05).suffix(" in"));
            if a.changed() || b.changed() {
                op(app, &ctx, "book.settings", json!({"width": w, "height": h}));
            }
        });
    }
    if let Some(c) =
        combo(ui, "book-cover", "Cover", s.cover, &[dac_book::CoverType::Hardcover, dac_book::CoverType::Softcover, dac_book::CoverType::None], |c| {
            match c {
                dac_book::CoverType::Hardcover => "Hardcover",
                dac_book::CoverType::Softcover => "Softcover",
                dac_book::CoverType::None => "No Cover",
            }
            .into()
        })
    {
        op(app, &ctx, "book.settings", json!({"cover": enum_key(c)}));
    }
    let mut paper = s.paper.clone();
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Paper"));
        if ui.add(egui::TextEdit::singleline(&mut paper).hint_text(crate::i18n::tr("e.g. Lustre, 148 gsm"))).changed() {
            op(app, &ctx, "book.settings", json!({"paper": paper}));
        }
    });
    let mut q = s.jpeg_quality;
    if ui.add(egui::Slider::new(&mut q, 1..=100).text(crate::i18n::tr("JPEG Quality"))).changed() {
        op(app, &ctx, "book.settings", json!({"jpegQuality": q}));
    }
    let mut res = s.resolution;
    if ui.add(egui::Slider::new(&mut res, 72..=600).suffix(" ppi").text(crate::i18n::tr("File Resolution"))).changed() {
        op(app, &ctx, "book.settings", json!({"resolution": res}));
    }
    if let Some(sh) = combo(
        ui,
        "book-sharp",
        "Sharpening",
        s.sharpening,
        &[dac_book::Sharpening::Off, dac_book::Sharpening::Low, dac_book::Sharpening::Standard, dac_book::Sharpening::High],
        |v| format!("{v:?}"),
    ) {
        op(app, &ctx, "book.settings", json!({"sharpening": enum_key(sh)}));
    }
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Colour Profile"));
        ui.label(egui::RichText::new(&s.color_profile).weak());
    });
    let mut bleed = s.bleed;
    if ui.add(egui::Slider::new(&mut bleed, 0.0..=36.0).suffix(" pt").text(crate::i18n::tr("Bleed"))).changed() {
        op(app, &ctx, "book.settings", json!({"bleed": bleed}));
    }
}

fn auto_panel(ui: &mut egui::Ui, app: &mut DacApp) {
    let ctx = ui.ctx().clone();
    let names: Vec<String> = dac_book::auto::builtin_presets().into_iter().map(|p| p.name).collect();
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Preset"));
        egui::ComboBox::from_id_salt("book-preset").selected_text(app.ui.book.preset.clone()).width(170.0).show_ui(ui, |ui| {
            for n in &names {
                if ui.selectable_label(*n == app.ui.book.preset, n).clicked() {
                    app.ui.book.preset = n.clone();
                }
            }
        });
    });
    ui.horizontal(|ui| {
        let a = ui.button(crate::i18n::tr("Auto Layout"));
        register(ui.ctx(), "book:autoLayout", a.rect);
        if a.clicked() {
            op(app, &ctx, "book.autoLayout", json!({}));
        }
        let c = ui.button(crate::i18n::tr("Clear Layout"));
        register(ui.ctx(), "book:clearLayout", c.rect);
        if c.clicked() {
            op(app, &ctx, "book.clearLayout", json!({}));
        }
    });
    ui.label(egui::RichText::new(crate::i18n::tr("Lays out the selected photos, or every photo in the filmstrip.")).small().weak());
}

fn page_panel(ui: &mut egui::Ui, app: &mut DacApp) {
    let ctx = ui.ctx().clone();
    let cur = app.ui.book.current;
    ui.label(egui::RichText::new(page_label(&app.ui.book.book, cur)).strong());
    ui.horizontal(|ui| {
        if ui.button(crate::i18n::tr("Add Page")).clicked() {
            op(app, &ctx, "book.addPage", json!({}));
        }
        if ui.button(crate::i18n::tr("Add Blank")).clicked() {
            op(app, &ctx, "book.addPage", json!({"blank": true}));
        }
        if matches!(cur, PageRef::Page(_)) && ui.button(crate::i18n::tr("Remove Page")).clicked() {
            op(app, &ctx, "book.removePage", json!({}));
        }
    });
    let n = app.ui.book.book.numbers.clone();
    let mut show = n.show;
    ui.horizontal(|ui| {
        if ui.checkbox(&mut show, crate::i18n::tr("Page Numbers")).changed() {
            op(app, &ctx, "book.pageNumbers", json!({"show": show}));
        }
        if let Some(p) = combo(ui, "book-numpos", "", n.position, &dac_book::NumberPos::ALL, |p| p.label().into()) {
            op(app, &ctx, "book.pageNumbers", json!({"position": enum_key(p)}));
        }
    });
    if let PageRef::Page(i) = cur {
        let mut hide = app.ui.book.book.pages.get(i).is_some_and(|p| p.hide_number);
        ui.horizontal(|ui| {
            if ui.checkbox(&mut hide, crate::i18n::tr("Hide Number on This Page")).changed() {
                op(app, &ctx, "book.pageNumbers", json!({"page": i + 1, "hide": hide}));
            }
            if ui.button(crate::i18n::tr("Apply to All")).clicked() {
                op(app, &ctx, "book.pageNumbers", json!({"applyToAll": true}));
            }
        });
    }
}

fn guides_panel(ui: &mut egui::Ui, app: &mut DacApp) {
    let ctx = ui.ctx().clone();
    let g = app.ui.book.book.guides;
    for (key, label, v) in [
        ("show", "Show Guides", g.show),
        ("bleed", "Page Bleed", g.bleed),
        ("textSafe", "Text Safe Area", g.text_safe),
        ("photoCells", "Photo Cells", g.photo_cells),
        ("fillerText", "Filler Text", g.filler_text),
    ] {
        let mut on = v;
        if ui.checkbox(&mut on, crate::i18n::tr(label)).changed() {
            op(app, &ctx, "book.guides", json!({key: on}));
        }
    }
}

fn selected_cell(app: &DacApp) -> Option<dac_book::BookCell> {
    app.ui.book.cell.and_then(|i| app.ui.book.book.page(app.ui.book.current).and_then(|p| p.cells.get(i).cloned()))
}

fn cell_panel(ui: &mut egui::Ui, app: &mut DacApp) {
    let ctx = ui.ctx().clone();
    let Some(c) = selected_cell(app) else {
        ui.label(egui::RichText::new(crate::i18n::tr("Click a cell on a page to edit it.")).weak());
        return;
    };
    let mut pad = c.padding;
    let mut linked = pad.linked;
    if ui.checkbox(&mut linked, crate::i18n::tr("Link All Sides")).changed() {
        op(app, &ctx, "book.cell", json!({"linked": linked}));
    }
    if pad.linked {
        let mut v = pad.top;
        if ui.add(egui::Slider::new(&mut v, 0.0..=144.0).suffix(" pt").text(crate::i18n::tr("Padding"))).changed() {
            op(app, &ctx, "book.cell", json!({"padding": v}));
        }
    } else {
        let mut changed = false;
        for (label, v) in [("Top", &mut pad.top), ("Right", &mut pad.right), ("Bottom", &mut pad.bottom), ("Left", &mut pad.left)] {
            changed |= ui.add(egui::Slider::new(v, 0.0..=144.0).suffix(" pt").text(crate::i18n::tr(label))).changed();
        }
        if changed {
            op(app, &ctx, "book.cell", json!({"padding": {"top": pad.top, "right": pad.right, "bottom": pad.bottom, "left": pad.left}}));
        }
    }
    if let CellContent::Photo { fill, zoom, pan, photo, .. } = c.content {
        let mut f = fill;
        if ui.checkbox(&mut f, crate::i18n::tr("Zoom to Fill")).changed() {
            op(app, &ctx, "book.cell", json!({"fill": f}));
        }
        let mut z = zoom;
        if ui.add(egui::Slider::new(&mut z, 1.0..=10.0).logarithmic(true).text(crate::i18n::tr("Zoom"))).changed() {
            op(app, &ctx, "book.cell", json!({"zoom": z}));
        }
        let (mut px, mut py) = (pan[0], pan[1]);
        let a = ui.add(egui::Slider::new(&mut px, -1.0..=1.0).text(crate::i18n::tr("Pan X")));
        let b = ui.add(egui::Slider::new(&mut py, -1.0..=1.0).text(crate::i18n::tr("Pan Y")));
        if a.changed() || b.changed() {
            op(app, &ctx, "book.cell", json!({"pan": [px, py]}));
        }
        ui.label(egui::RichText::new(crate::i18n::tr("Alt-drag a photo to pan it; drag it onto another cell to swap.")).small().weak());
        if photo.is_some() && ui.button(crate::i18n::tr("Remove Photo from Cell")).clicked() {
            op(app, &ctx, "book.place", json!({"photo": null}));
        }
    }
}

fn text_panel(ui: &mut egui::Ui, app: &mut DacApp) {
    let ctx = ui.ctx().clone();
    match selected_cell(app).map(|c| c.content) {
        Some(CellContent::Text { mut text, .. }) => {
            ui.label(crate::i18n::tr("Cell Text (tokens like {Title} work)"));
            if ui.add(egui::TextEdit::multiline(&mut text).desired_rows(4).desired_width(f32::INFINITY)).changed() {
                op(app, &ctx, "book.text", json!({"text": text}));
            }
        }
        Some(CellContent::Photo { text: pt, .. }) => {
            let mut on = pt.is_some();
            if ui.checkbox(&mut on, crate::i18n::tr("Photo Text")).changed() {
                op(app, &ctx, "book.photoText", json!({"enabled": on}));
                app.ui.book.type_target = TypeTarget::PhotoText;
            }
            if let Some(pt) = pt {
                let mut text = pt.text.clone();
                if ui.add(egui::TextEdit::singleline(&mut text).hint_text("{Title}")).changed() {
                    op(app, &ctx, "book.photoText", json!({"text": text}));
                }
                ui.horizontal_wrapped(|ui| {
                    for tok in ["{Title}", "{Caption}", "{Filename}", "{Date}", "{Exposure}", "{Camera}"] {
                        if ui.small_button(tok).clicked() {
                            op(app, &ctx, "book.photoText", json!({"text": tok}));
                        }
                    }
                });
                if let Some(pos) = combo(
                    ui,
                    "book-ptpos",
                    "Align",
                    pt.position,
                    &[dac_book::PhotoTextPos::Above, dac_book::PhotoTextPos::Below, dac_book::PhotoTextPos::Over],
                    |p| format!("{p:?}"),
                ) {
                    op(app, &ctx, "book.photoText", json!({"position": enum_key(pos)}));
                }
                let mut off = pt.offset;
                if ui.add(egui::Slider::new(&mut off, -72.0..=144.0).suffix(" pt").text(crate::i18n::tr("Offset"))).changed() {
                    op(app, &ctx, "book.photoText", json!({"offset": off}));
                }
            }
        }
        None => {}
    }
    ui.separator();
    let pt = app.ui.book.book.page(app.ui.book.current).and_then(|p| p.text.clone());
    let mut on = pt.is_some();
    if ui.checkbox(&mut on, crate::i18n::tr("Page Text")).changed() {
        op(app, &ctx, "book.pageText", json!({"enabled": on}));
        app.ui.book.type_target = TypeTarget::PageText;
    }
    if let Some(pt) = pt {
        let mut text = pt.text.clone();
        if ui.add(egui::TextEdit::multiline(&mut text).desired_rows(2).desired_width(f32::INFINITY)).changed() {
            op(app, &ctx, "book.pageText", json!({"text": text}));
        }
        if let Some(pos) =
            combo(ui, "book-pgpos", "Align", pt.position, &[dac_book::PageTextPos::Top, dac_book::PageTextPos::Bottom], |p| format!("{p:?}"))
        {
            op(app, &ctx, "book.pageText", json!({"position": enum_key(pos)}));
        }
        let mut off = pt.offset;
        if ui.add(egui::Slider::new(&mut off, 0.0..=288.0).suffix(" pt").text(crate::i18n::tr("Offset"))).changed() {
            op(app, &ctx, "book.pageText", json!({"offset": off}));
        }
    }
}

/// The style the Type panel shows.
fn type_style(app: &DacApp) -> Option<dac_book::TypeStyle> {
    let st = &app.ui.book;
    match st.type_target {
        TypeTarget::Numbers => Some(st.book.numbers.style.clone()),
        TypeTarget::PageText => st.book.page(st.current).and_then(|p| p.text.as_ref()).map(|t| t.style.clone()),
        TypeTarget::Cell | TypeTarget::PhotoText => match selected_cell(app)?.content {
            CellContent::Text { style, .. } => Some(style),
            CellContent::Photo { text, .. } => text.map(|t| t.style),
        },
    }
}

fn type_panel(ui: &mut egui::Ui, app: &mut DacApp) {
    let ctx = ui.ctx().clone();
    let targets = [TypeTarget::Cell, TypeTarget::PhotoText, TypeTarget::PageText, TypeTarget::Numbers];
    if let Some(t) = combo(ui, "book-typetarget", "Edit", app.ui.book.type_target, &targets, |t| t.label().into()) {
        app.ui.book.type_target = t;
    }
    let target = app.ui.book.type_target.key();
    let Some(s) = type_style(app) else {
        ui.label(egui::RichText::new(crate::i18n::tr("Select a text cell, or turn on photo or page text.")).weak());
        return;
    };
    let set = |app: &mut DacApp, patch: Value| op(app, &ctx, "book.type", json!({"target": target, "style": patch}));
    // presets
    let mut presets: Vec<String> = dac_book::text::builtin_presets().into_iter().map(|p| p.name).collect();
    presets.extend(app.ui.book.book.text_presets.iter().map(|p| p.name.clone()));
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Text Style Preset"));
        egui::ComboBox::from_id_salt("book-tpreset").selected_text("…").show_ui(ui, |ui| {
            for n in &presets {
                if ui.selectable_label(false, n).clicked() {
                    op(app, &ctx, "book.type", json!({"target": target, "preset": n}));
                }
            }
            ui.separator();
            if ui.selectable_label(false, crate::i18n::tr("Save Current Settings as New Preset")).clicked() {
                let name = format!("Style {}", app.ui.book.book.text_presets.len() + 1);
                op(app, &ctx, "book.textPreset", json!({"name": name, "style": s}));
            }
        });
    });
    let families = font_families();
    let font_label = if s.font.is_empty() { "Inter (default)".to_string() } else { s.font.clone() };
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Font"));
        egui::ComboBox::from_id_salt("book-font").selected_text(font_label).width(170.0).height(300.0).show_ui(ui, |ui| {
            if ui.selectable_label(s.font.is_empty(), "Inter (default)").clicked() {
                set(app, json!({"font": ""}));
            }
            for f in &families {
                if ui.selectable_label(*f == s.font, f).clicked() {
                    set(app, json!({"font": f}));
                }
            }
        });
    });
    if let Some(fs) = combo(ui, "book-fstyle", "Style", s.style, &dac_book::FontStyle::ALL, |f| f.label().into()) {
        set(app, json!({"style": enum_key(fs)}));
    }
    let mut v = s.size;
    if ui.add(egui::Slider::new(&mut v, 1.0..=200.0).logarithmic(true).suffix(" pt").text(crate::i18n::tr("Size"))).changed() {
        set(app, json!({"size": v}));
    }
    let mut o = s.opacity * 100.0;
    if ui.add(egui::Slider::new(&mut o, 0.0..=100.0).suffix(" %").text(crate::i18n::tr("Opacity"))).changed() {
        set(app, json!({"opacity": o / 100.0}));
    }
    let mut col = s.color;
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Character Color"));
        if ui.color_edit_button_srgb(&mut col).changed() {
            set(app, json!({"color": col}));
        }
    });
    let mut tr = s.tracking;
    if ui.add(egui::Slider::new(&mut tr, -200.0..=1000.0).text(crate::i18n::tr("Tracking"))).changed() {
        set(app, json!({"tracking": tr}));
    }
    let mut bl = s.baseline;
    if ui.add(egui::Slider::new(&mut bl, -72.0..=72.0).suffix(" pt").text(crate::i18n::tr("Baseline"))).changed() {
        set(app, json!({"baseline": bl}));
    }
    let mut auto = s.leading.is_none();
    let mut lead = s.leading.unwrap_or(s.size * 1.2);
    ui.horizontal(|ui| {
        if ui.checkbox(&mut auto, crate::i18n::tr("Auto Leading")).changed() {
            set(app, json!({"leading": if auto { Value::Null } else { json!(lead) }}));
        }
    });
    if !auto && ui.add(egui::Slider::new(&mut lead, 1.0..=300.0).suffix(" pt").text(crate::i18n::tr("Leading"))).changed() {
        set(app, json!({"leading": lead}));
    }
    if let Some(kk) =
        combo(ui, "book-kern", "Kerning", s.kerning, &[dac_book::Kerning::Metrics, dac_book::Kerning::Optical, dac_book::Kerning::Off], |k| {
            format!("{k:?}")
        })
    {
        set(app, json!({"kerning": enum_key(kk)}));
    }
    let mut cols = s.columns;
    if ui.add(egui::Slider::new(&mut cols, 1..=6).text(crate::i18n::tr("Columns"))).changed() {
        set(app, json!({"columns": cols}));
    }
    let mut gut = s.gutter;
    if ui.add(egui::Slider::new(&mut gut, 0.0..=72.0).suffix(" pt").text(crate::i18n::tr("Gutter"))).changed() {
        set(app, json!({"gutter": gut}));
    }
    ui.horizontal(|ui| {
        for (a, l) in
            [(dac_book::HAlign::Left, "⇤"), (dac_book::HAlign::Center, "↔"), (dac_book::HAlign::Right, "⇥"), (dac_book::HAlign::Justify, "☰")]
        {
            if ui.selectable_label(s.align == a, l).on_hover_text(format!("{a:?}")).clicked() {
                set(app, json!({"align": enum_key(a)}));
            }
        }
        ui.separator();
        for (a, l) in [(dac_book::VAlign::Top, "⤒"), (dac_book::VAlign::Middle, "↕"), (dac_book::VAlign::Bottom, "⤓")] {
            if ui.selectable_label(s.valign == a, l).on_hover_text(format!("{a:?}")).clicked() {
                set(app, json!({"valign": enum_key(a)}));
            }
        }
    });
}

fn background_panel(ui: &mut egui::Ui, app: &mut DacApp) {
    let ctx = ui.ctx().clone();
    let cur = app.ui.book.current;
    let own = app.ui.book.book.page(cur).is_some_and(|p| p.background.is_some());
    let mut all = !own;
    if ui.checkbox(&mut all, crate::i18n::tr("Apply Background Globally")).changed() {
        if all {
            op(app, &ctx, "book.background", json!({"page": page_json(cur), "clear": true}));
        } else {
            op(app, &ctx, "book.background", json!({"page": page_json(cur)}));
        }
    }
    let bg = app.ui.book.book.background_of(cur).clone();
    let target = if own { json!({"page": page_json(cur)}) } else { json!({}) };
    let set = |app: &mut DacApp, patch: Value| {
        let mut p = target.clone();
        if let (Some(o), Some(x)) = (p.as_object_mut(), patch.as_object()) {
            o.extend(x.clone());
        }
        op(app, &ctx, "book.background", p);
    };
    let mut col = bg.color;
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Background Color"));
        if ui.color_edit_button_srgb(&mut col).changed() {
            set(app, json!({"color": col}));
        }
    });
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Photo"));
        match &bg.photo {
            Some(id) => {
                ui.label(photo_info(app, id).filename);
                if ui.small_button("✕").clicked() {
                    set(app, json!({"photo": null}));
                }
            }
            None => {
                if ui.button(crate::i18n::tr("Use Selected Photo")).clicked()
                    && let Some(a) = app.session.active()
                {
                    set(app, json!({"photo": a.0.to_string()}));
                }
            }
        }
    });
    let mut po = bg.photo_opacity * 100.0;
    if ui.add(egui::Slider::new(&mut po, 0.0..=100.0).suffix(" %").text(crate::i18n::tr("Photo Opacity"))).changed() {
        set(app, json!({"photoOpacity": po / 100.0}));
    }
    if let Some(g) = combo(ui, "book-graphic", "Graphic", bg.graphic, &dac_book::Graphic::ALL, |g| g.label().into()) {
        set(app, json!({"graphic": enum_key(g)}));
    }
    let mut gc = bg.graphic_color;
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Graphic Color"));
        if ui.color_edit_button_srgb(&mut gc).changed() {
            set(app, json!({"graphicColor": gc}));
        }
    });
    let mut go = bg.graphic_opacity * 100.0;
    if ui.add(egui::Slider::new(&mut go, 0.0..=100.0).suffix(" %").text(crate::i18n::tr("Graphic Opacity"))).changed() {
        set(app, json!({"graphicOpacity": go / 100.0}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::Headless;
    use std::time::Duration;

    const T: Duration = Duration::from_secs(30);

    fn demo() -> Headless {
        let app = DacApp::new(dac_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        let mut h = Headless::new(app, [1500.0, 950.0], 1.0);
        h.settle(Duration::from_secs(120));
        h
    }

    fn run(h: &mut Headless, id: &str, params: Value) -> Value {
        let r = h.request("engine.execute", json!({"command": id, "params": params}), T);
        assert_eq!(r["ok"], true, "{id}: {r}");
        h.step();
        r["result"].clone()
    }

    #[test]
    fn book_module_lays_out_edits_and_exports() {
        let mut h = demo();
        run(&mut h, "module.book", json!({}));
        let r = run(&mut h, "book.autoLayout", json!({"preset": "Two Per Page"}));
        assert!(r["pages"].as_u64().unwrap() >= 1, "{r}");
        h.settle(Duration::from_secs(60));
        assert!(h.app.widgets.iter().any(|(w, _)| w == "book:page:1"));
        run(&mut h, "book.view", json!({"mode": "spread"}));
        run(&mut h, "book.select", json!({"page": 1, "cell": 0}));
        run(&mut h, "book.cell", json!({"zoom": 1.5, "padding": 4}));
        run(&mut h, "book.photoText", json!({"text": "{Filename}"}));
        run(&mut h, "book.addPage", json!({"template": "text-page"}));
        run(&mut h, "book.select", json!({"cell": 0}));
        run(&mut h, "book.text", json!({"text": "A day by the sea"}));
        run(&mut h, "book.type", json!({"style": {"size": 20, "columns": 2}}));
        run(&mut h, "book.background", json!({"color": "#f4efe6", "graphic": "corners"}));
        run(&mut h, "book.pageNumbers", json!({"show": true}));
        run(&mut h, "book.undo", json!({}));
        assert!(!h.app.ui.book.book.numbers.show);
        run(&mut h, "book.redo", json!({}));
        assert!(h.app.ui.book.book.numbers.show);
        let doc = dac_book::layoutdoc::to_layout(&h.app.ui.book.book);
        let saved = run(&mut h, "creation.save", json!({"kind": "book", "name": "Test Book", "document": serde_json::to_value(&doc).unwrap()}));
        let cid = saved["id"].as_u64().unwrap();
        let pages = h.app.ui.book.book.pages.len();
        run(&mut h, "book.clearLayout", json!({}));
        let r = run(&mut h, "book.open", json!({"id": cid}));
        assert_eq!(r["pages"].as_u64().unwrap() as usize, pages, "{r}");
        for v in BookView::ALL {
            run(&mut h, "book.view", json!({"mode": v.key()}));
            let img = h.snapshot(T);
            assert!(img.width() > 0);
        }
        // swap by command, bad input is an error
        let bad = h.request("engine.execute", json!({"command": "book.cell", "params": {"page": 1, "cell": 0, "zoom": "x"}}), T);
        assert_eq!(bad["ok"], false);
        let dir = std::env::temp_dir().join(format!("book-ui-test-{}", std::process::id()));
        let pdf = dir.join("b.pdf");
        let r = run(&mut h, "book.export", json!({"path": pdf.to_string_lossy(), "wait": true}));
        let f = r["files"][0].as_str().unwrap().to_string();
        let bytes = std::fs::read(&f).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        let r = run(&mut h, "book.export", json!({"path": dir.join("pages").to_string_lossy(), "kind": "jpeg", "wait": true}));
        assert!(r["files"].as_array().unwrap().len() >= 3, "{r}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names() {
        assert_eq!(safe_name(" a/b:c "), Some("abc".into()));
        assert_eq!(safe_name(".."), None);
    }
}
