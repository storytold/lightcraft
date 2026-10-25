//! LightCraft's half of Open and Save against cloud storage.
//!
//! The pages themselves are `craft_ui::cloud`. Every Craft app offers the same
//! ones. What is here is what only LightCraft can answer. A document is a
//! photo. These extensions and content types are what that means. Bytes become
//! one through an import, and go back as an exported JPEG or TIFF through the
//! engine's export pipeline.
//!
//! LightCraft holds no provider credential and speaks no provider protocol.
//! Every call goes to the CollabCraft service, which is the single place a
//! provider is configured and permissions are decided. Replacing Nextcloud
//! with something else changes nothing here.
//!
//! Nothing in this file opens a socket either. The transport half is a
//! [`CloudBackend`] the host application supplies (see `apps/lightcraft/src/cloud.rs`
//! for desktop; `apps/lightcraft-web/src/cloud.rs` for the browser). Every
//! CollabCraft call blocks. A blocking read of a large raw file inside a frame
//! would freeze the window for seconds, so the pages state a [`Job`], the host
//! runs it on a worker (or async task in the browser), and the answer arrives
//! on a channel that [`pump`] drains on a later frame.

use egui::{Color32, Painter, Rect, Ui};
use lightcraft_catalog::PhotoId;
use serde_json::{Value as Json, json};

// Re-exported so a host application, and a test, can name these types without
// depending on the crates they come from.
pub use craft_cloud::{Capability, Entry, EntryKind};
pub use craft_ui::cloud::{
    Account, Busy, CloudApp, CloudBackend, CloudFiles, Conflict, CopyFormat, Destination, DocumentKind, Event, Fault, Folder, Job, Link, Origin,
    Post, SaveIntent, Source, UNAVAILABLE, available, browse, can_converse, conflict_modal, destination_name, has_target, link, load_shared,
    open_entry, open_page, origin, parent_of, prefers_cloud, progress, reload_active, save, save_as, save_page, share_button, shares, sign_in,
    sign_out, start, who, work_together,
};

use crate::LightcraftApp;
use crate::theme::Tokens;

// ---------------------------------------------------------------- document kind

/// Extensions LightCraft opens from cloud storage. The same set the desktop
/// file picker offers (see `apps/lightcraft/src/main.rs`).
const OPENS: [&str; 20] =
    ["jpg", "jpeg", "png", "tif", "tiff", "webp", "dng", "cr2", "cr3", "nef", "arw", "rw2", "raf", "orf", "pef", "srw", "gpr", "bmp", "heic", "jxl"];

/// Content types LightCraft opens, matched against the start of what the
/// provider said. Read beside the extension because neither is reliable
/// alone. A WebDAV server hands back `application/octet-stream` for a
/// perfectly good raw file.
const MIMES: [&str; 5] = ["image/jpeg", "image/png", "image/tiff", "image/webp", "image/"];

/// What one LightCraft photo is, as a cloud file list needs to know.
pub const PHOTO: DocumentKind = DocumentKind { extensions: &OPENS, mimes: &MIMES, noun: "photo", default_extension: "jpg" };

// ---------------------------------------------------------------- service address

/// The host part of the service address, for the account menu's first line.
///
/// Read from the environment and compiled in nowhere, because the service is
/// deployment-specific. Somebody with an account on two deployments needs to
/// know which one a photo is about to land in.
fn service_host() -> String {
    service_url().map(|v| v.trim_start_matches("https://").trim_start_matches("http://").to_owned()).unwrap_or_default()
}

/// Where the CollabCraft service is, as the environment gave it.
///
/// Public because the host application reads the same answer to decide whether
/// to start a worker at all. Two spellings of one setting would be a setting
/// that works in one half of the application.
pub fn service_url() -> Option<String> {
    ["LIGHTCRAFT_CLOUD_URL", "CRAFT_CLOUD_URL", "COLLABCRAFT_URL"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .map(|v| v.trim().trim_end_matches('/').to_owned())
        .find(|v| !v.is_empty())
}

// ---------------------------------------------------------------- helpers

/// Whether LightCraft can open this cloud entry.
pub fn opens(entry: &Entry) -> bool {
    craft_ui::cloud::opens(entry, PHOTO)
}

/// The whole of the cloud state, for the control channel and for tests.
pub fn status(app: &LightcraftApp) -> Json {
    craft_ui::cloud::report(app)
}

// ---------------------------------------------------------------- export format from destination

/// Extensions CollabCraft Save can actually encode (developed output, not RAW originals).
const WRITABLE: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff", "webp"];

/// The export format to write for a given cloud destination.
///
/// The name is the first answer. The content type is the second. Both are
/// needed: a provider hands back `application/octet-stream` for a perfectly
/// good JPEG, and a file exported by a web application often has a correct
/// content type and a bare name.
///
/// RAW and other open-only extensions are refused rather than writing JPEG
/// bytes under a `.cr3` / `.dng` name.
fn export_format_for(into: Destination<'_>) -> Result<lightcraft_engine::export::ExportFormat, String> {
    use lightcraft_engine::export::ExportFormat;
    let ext = into.extension();
    let mime = into.mime.unwrap_or("");
    match ext.as_str() {
        "jpg" | "jpeg" => Ok(ExportFormat::Jpeg),
        "png" => Ok(ExportFormat::Png),
        "tif" | "tiff" => Ok(ExportFormat::Tiff),
        "webp" => Ok(ExportFormat::Webp),
        _ if mime.starts_with("image/jpeg") => Ok(ExportFormat::Jpeg),
        _ if mime.starts_with("image/png") => Ok(ExportFormat::Png),
        _ if mime.starts_with("image/tiff") => Ok(ExportFormat::Tiff),
        _ if mime.starts_with("image/webp") => Ok(ExportFormat::Webp),
        _ => Err(format!(
            "CollabCraft save writes JPEG, PNG, TIFF, or WebP; rename to {}.jpg (or .png / .tif / .webp)",
            std::path::Path::new(into.name).file_stem().and_then(|s| s.to_str()).unwrap_or("photo")
        )),
    }
}

fn encode_prepared(
    item: lightcraft_engine::export::PreparedExport,
    opts: &lightcraft_engine::export::ExportOptions,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Vec<u8>, String> {
    use std::sync::atomic::Ordering;
    let dest = lightcraft_engine::export::Destination { dir: String::new(), exact: Some("cloud_export".into()) };
    let mut captured = Vec::new();
    lightcraft_engine::export::run_batch(
        vec![item],
        opts,
        &dest,
        &mut |_path, b| {
            captured = b.to_vec();
            Ok(())
        },
        &|_| false,
        true,
        &mut |_, _| cancel.map(|c| !c.load(Ordering::Relaxed)).unwrap_or(true),
    )
    .map_err(|e| format!("could not export photo: {e}"))?;
    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return Err("the cloud save was cancelled".into());
    }
    if captured.is_empty() {
        return Err("photo export produced no bytes".into());
    }
    Ok(captured)
}

/// Suggested cloud Save name: keep a writable extension, otherwise use `.jpg`.
pub(crate) fn cloud_save_name(file_name: &str) -> String {
    let base = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name).trim();
    if base.is_empty() {
        return String::new();
    }
    let ext = std::path::Path::new(base).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if WRITABLE.iter().any(|w| *w == ext) {
        return base.to_owned();
    }
    let stem = std::path::Path::new(base).file_stem().and_then(|s| s.to_str()).unwrap_or(base);
    format!("{stem}.jpg")
}

/// Durable path for a cloud Open on desktop: under the library's `Originals/Cloud/` when the
/// library is on disk, otherwise a process-unique folder under the OS temp directory.
fn durable_cloud_path(app: &LightcraftApp, name: &str) -> Result<std::path::PathBuf, String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let file_name = format!("{stamp}-{base}");
    let dir = app
        .session
        .library
        .as_ref()
        .filter(|l| l.on_disk)
        .map(|l| l.dir.join("Originals").join("Cloud"))
        .unwrap_or_else(|| std::env::temp_dir().join(format!("lightcraft-cloud-{}", std::process::id())));
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create cloud import folder: {e}"))?;
    Ok(dir.join(file_name))
}

/// Drain CollabCraft answers, and remember which library a Cloud Open started in.
pub fn pump(app: &mut LightcraftApp, ctx: &egui::Context) {
    if matches!(app.cloud.busy, Some(Busy::Opening { .. })) {
        app.cloud_open_library.get_or_insert_with(|| app.session.library_generation());
    }
    craft_ui::cloud::pump(app, ctx);
    if !matches!(app.cloud.busy, Some(Busy::Opening { .. })) {
        // Success consumes the stamp inside open_bytes; a failed read leaves it — drop it.
        app.cloud_open_library = None;
    }
    // Drop the activity row once encode finished (Saving, idle, or failed).
    if !matches!(app.cloud.busy, Some(Busy::Encoding { .. })) {
        app.cloud_encode = None;
    }
}

// ---------------------------------------------------------------- CloudApp impl

impl CloudApp for LightcraftApp {
    fn cloud(&self) -> &CloudFiles {
        &self.cloud
    }

    fn cloud_mut(&mut self) -> &mut CloudFiles {
        &mut self.cloud
    }

    fn kind(&self) -> DocumentKind {
        PHOTO
    }

    /// The active (selected) photo's id, as the cloud pages need it.
    ///
    /// `PhotoId` wraps a `u64`, so the raw id carries through.
    fn active(&self) -> Option<u64> {
        self.session.active().map(|PhotoId(n)| n)
    }

    /// The file name of one photo, for a Save As field.
    ///
    /// RAW and other open-only names become `.jpg` so the suggested CollabCraft
    /// name matches the developed bytes encode will write.
    fn title(&self, uid: u64) -> String {
        let raw = self.session.catalog.photo(PhotoId(uid)).map(|p| p.file_name.clone()).unwrap_or_default();
        cloud_save_name(&raw)
    }

    /// Turn bytes from cloud storage into an imported photo.
    ///
    /// On the web the host stores the bytes in Originals (same path as a drop or
    /// keyed invite) via [`Services::store_bytes`]. On desktop the bytes are
    /// written under the library's `Originals/Cloud/` when the library is on
    /// disk, then imported in place so the catalog keeps a durable source path.
    /// `Ok(None)` means the import is asynchronous.
    fn open_bytes(&mut self, name: &str, bytes: Vec<u8>) -> Result<Option<u64>, String> {
        if let Some(expected) = self.cloud_open_library.take() {
            if expected != self.session.library_generation() {
                return Err("the library changed before the cloud photo arrived; open it again".into());
            }
        }
        if let Some(store) = self.services.store_bytes.as_mut() {
            store(name.to_owned(), bytes);
            return Ok(None);
        }
        #[cfg(target_arch = "wasm32")]
        {
            return Err("this browser host cannot store a cloud photo (missing store_bytes)".into());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            // Refuse before writing: start_paths fails the same way, and a written-but-unimported
            // file under Originals/Cloud would pile up on every retry while Auto Import is busy.
            crate::import::ensure_can_start(self)?;
            let path = durable_cloud_path(self, name)?;
            std::fs::write(&path, &bytes).map_err(|e| format!("could not write {name} into the library: {e}"))?;
            let path_str = path.to_string_lossy().to_string();
            // Already inside Originals (or a process-scoped durable folder): add in place.
            if let Err(e) = crate::import::start_paths(self, vec![path_str]) {
                let _ = std::fs::remove_file(&path);
                return Err(e);
            }
            Ok(None)
        }
    }

    /// Turn the active photo into bytes to send to cloud storage.
    ///
    /// The export format follows the destination: JPEG, PNG, TIFF, or WebP.
    /// A photo opened as a raw file goes back as a developed image in one of
    /// those formats. A destination name that still looks like RAW (or another
    /// open-only extension) is refused so the uploaded file is not mislabeled.
    /// Prefer [`CloudApp::begin_encode`] for Save; this sync path is for tests
    /// and the default craft-ui fallback.
    fn encode(&mut self, uid: u64, into: Destination<'_>) -> Result<Vec<u8>, String> {
        let format = export_format_for(into)?;
        let opts = lightcraft_engine::export::ExportOptions { format, ..Default::default() };
        let item = lightcraft_engine::export::prepare_export(&mut self.session, PhotoId(uid), &opts, 1)
            .map_err(|e| format!("could not prepare photo for export: {e}"))?;
        encode_prepared(item, &opts, None)
    }

    /// Prepare on the UI thread, encode on a worker (desktop) with an activity
    /// row, then post [`Event::Encoded`]. Wasm encodes on the calling task.
    fn begin_encode(&mut self, intent: SaveIntent) -> Result<(), String> {
        if self.cloud_encode.is_some() {
            return Err("a cloud save is already preparing".into());
        }
        let dest = Destination { name: &intent.name, mime: intent.mime.as_deref() };
        let format = export_format_for(dest)?;
        let opts = lightcraft_engine::export::ExportOptions { format, ..Default::default() };
        let item = lightcraft_engine::export::prepare_export(&mut self.session, PhotoId(intent.uid), &opts, 1)
            .map_err(|e| format!("could not prepare photo for export: {e}"))?;

        #[cfg(target_arch = "wasm32")]
        {
            let bytes = encode_prepared(item, &opts, None).map_err(Fault::Refused);
            self.cloud.post(Event::Encoded { intent, bytes });
            return Ok(());
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            use lightcraft_engine::activity::Cancel;
            use std::sync::atomic::{AtomicBool, Ordering};
            use std::sync::Arc;

            let cancel = Arc::new(AtomicBool::new(false));
            let guard = self.session.activity.start("cloud-save", "Preparing cloud save", Cancel::Flag(cancel.clone()));
            self.cloud_encode = Some(guard);
            let tx = self.cloud.event_sender();
            let work = move || {
                let bytes = if cancel.load(Ordering::Relaxed) {
                    Err(Fault::Refused("the cloud save was cancelled".into()))
                } else {
                    encode_prepared(item, &opts, Some(cancel.as_ref())).map_err(Fault::Refused)
                };
                // Cancel after encode still refuses upload so a cancelled save cannot overwrite.
                let bytes = match bytes {
                    Ok(_) if cancel.load(Ordering::Relaxed) => Err(Fault::Refused("the cloud save was cancelled".into())),
                    other => other,
                };
                let _ = tx.send(Event::Encoded { intent, bytes });
            };
            std::thread::Builder::new().name("cloud-encode".into()).spawn(work).map_err(|e| {
                self.cloud_encode = None;
                format!("could not start cloud encode: {e}")
            })?;
            Ok(())
        }
    }

    /// Closing a cloud photo in LightCraft means nothing special: the photo
    /// stays in the catalog. The cloud-origin record is discarded by the caller.
    fn close(&mut self, _uid: u64) {}

    fn sharing(&mut self) -> &mut craft_ui::share::ShareDialog {
        &mut self.share
    }

    fn presence(&mut self) -> &mut craft_ui::presence::PresenceBar {
        &mut self.presence
    }

    fn paint_icon(&self, painter: &Painter, rect: Rect, kind: EntryKind, colour: Color32) {
        let icon = if kind == EntryKind::Folder { crate::icons::Icon::Folder } else { crate::icons::Icon::GridPhoto };
        crate::icons::paint(painter, rect, icon, colour);
    }

    fn open_url(&self, url: &str) {
        // `services.open_url` is FnMut, which requires &mut self, and this
        // method only has &self. Route through egui's output command queue
        // instead; the runner opens the browser on the next output-processing
        // pass, which is exactly what FnMut is used for elsewhere.
        if let Some(ctx) = &self.tasks.repaint {
            ctx.open_url(egui::OpenUrl::new_tab(url));
        }
    }

    fn say(&mut self, text: String) {
        self.notices.push(text);
    }

    fn toast(&mut self, text: String) {
        self.ui.status = text;
    }

    fn app_name(&self) -> &str {
        "LightCraft"
    }

    fn service(&self) -> String {
        service_host()
    }

    fn save_note(&self) -> Option<String> {
        Some("The photo is exported as a developed image in the format the destination expects.".into())
    }

    fn recents(&self) -> Vec<String> {
        self.ui.settings.recent_libraries.clone()
    }

    fn open_recent(&mut self, path: &str) {
        let _ = self.run("file.open", json!({"path": path}));
    }

    fn browse_local(&mut self) {
        let _ = self.run("file.open", json!({}));
    }

    fn style(&self, ui: &Ui) -> craft_ui::cloud::CloudStyle {
        let t = Tokens::get(ui.ctx());
        craft_ui::cloud::CloudStyle {
            surface: t.chrome,
            border: t.divider,
            text: t.text,
            dim: t.text_dim,
            hover: t.hover,
            soft: t.cell_selected,
            family: egui::FontFamily::Proportional,
            bold: egui::FontFamily::Name("semibold".into()),
        }
    }
}

// ---------------------------------------------------------------- where the pages are drawn

/// File › Open chrome: Recent, Cloud, Shared with me, and This computer.
pub fn open_section(app: &mut LightcraftApp, ui: &mut Ui) {
    open_page(app, ui);
}

/// The cloud Open and Save pages as a modal, reached from the File menu.
pub fn modal(app: &mut LightcraftApp, page: CloudPage, ctx: &egui::Context) {
    let title = match page {
        CloudPage::Open => "Open",
        CloudPage::Save => "Save to Cloud Storage",
    };
    let mut close = false;
    egui::Modal::new(egui::Id::new("lightcraft_cloud_page")).show(ctx, |ui| {
        let margin = egui::Frame::popup(ui.style()).total_margin().sum();
        let room = (ctx.content_rect().size() - margin - egui::vec2(16.0, 16.0)).max(egui::Vec2::splat(1.0));
        let width = 680.0_f32.min(room.x);
        ui.set_min_width(360.0_f32.min(width));
        ui.set_max_width(width);
        // Register the modal container so accessibility/regression tests can detect it.
        crate::widgets::register(ui.ctx(), "modal:cloud", ui.max_rect());
        ui.label(egui::RichText::new(title).strong().size(16.0));
        ui.add_space(10.0);
        let body = (room.y - ui.min_size().y - 48.0).max(1.0);
        egui::ScrollArea::vertical().id_salt(("lightcraft_cloud_page", page == CloudPage::Save)).max_height(body).show(ui, |ui| match page {
            CloudPage::Open => open_page(app, ui),
            CloudPage::Save => {
                close = save_page(app, ui);
            }
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
            });
        });
    });
    if close {
        app.cloud_page = None;
    }
}

/// Which cloud page the window is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudPage {
    Open,
    Save,
}
