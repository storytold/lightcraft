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
use serde_json::Value as Json;

// Re-exported so a host application, and a test, can name these types without
// depending on the crates they come from.
pub use craft_cloud::{Capability, Entry, EntryKind};
pub use craft_ui::cloud::{
    Account, Busy, CloudApp, CloudBackend, CloudFiles, Conflict, CopyFormat, Destination, DocumentKind, Event, Fault, Folder, Job, Link, Origin,
    Post, Source, UNAVAILABLE, available, browse, can_converse, conflict_modal, destination_name, has_target, link, load_shared, open_entry,
    open_page, origin, parent_of, prefers_cloud, progress, pump, reload_active, save, save_as, save_page, share_button, shares, sign_in, sign_out,
    start, who, work_together,
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

/// The export format to write for a given cloud destination.
///
/// The name is the first answer. The content type is the second. Both are
/// needed: a provider hands back `application/octet-stream` for a perfectly
/// good JPEG, and a file exported by a web application often has a correct
/// content type and a bare name.
fn export_format_for(into: Destination<'_>) -> lightcraft_engine::export::ExportFormat {
    use lightcraft_engine::export::ExportFormat;
    let ext = into.extension();
    let mime = into.mime.unwrap_or("");
    match ext.as_str() {
        "jpg" | "jpeg" => ExportFormat::Jpeg,
        "png" => ExportFormat::Png,
        "tif" | "tiff" => ExportFormat::Tiff,
        "webp" => ExportFormat::Webp,
        _ if mime.starts_with("image/jpeg") => ExportFormat::Jpeg,
        _ if mime.starts_with("image/png") => ExportFormat::Png,
        _ if mime.starts_with("image/tiff") => ExportFormat::Tiff,
        _ if mime.starts_with("image/webp") => ExportFormat::Webp,
        // Default: JPEG, which every viewer can read.
        _ => ExportFormat::Jpeg,
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
    fn title(&self, uid: u64) -> String {
        self.session.catalog.photo(PhotoId(uid)).map(|p| p.file_name.clone()).unwrap_or_default()
    }

    /// Turn bytes from cloud storage into an imported photo.
    ///
    /// LightCraft's import is asynchronous: the bytes are written to a temporary
    /// file and queued for the import worker. `Ok(None)` signals that the
    /// application has taken the bytes and will show the result itself once the
    /// import completes.
    fn open_bytes(&mut self, name: &str, bytes: Vec<u8>) -> Result<Option<u64>, String> {
        // Write to a named temporary file so the existing import pipeline can
        // read it as a file-backed photo. The OS temp dir is writable on every
        // platform this runs on.
        let dir = std::env::temp_dir().join("lightcraft-cloud");
        std::fs::create_dir_all(&dir).map_err(|e| format!("could not create temp folder: {e}"))?;

        // Use a timestamp suffix so simultaneous cloud opens do not collide.
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let file_name = format!("{stamp}-{name}");
        let path = dir.join(&file_name);
        std::fs::write(&path, &bytes).map_err(|e| format!("could not write {name} to a temporary file: {e}"))?;

        let path_str = path.to_string_lossy().to_string();
        crate::import::start_paths(self, vec![path_str])?;
        Ok(None)
    }

    /// Turn the active photo into bytes to send to cloud storage.
    ///
    /// The export format follows the destination: JPEG unless the name or
    /// content type says TIFF, PNG or WebP. A photo opened as a raw file goes
    /// back as a developed JPEG (or the format the destination asks for) rather
    /// than as the original raw bytes, which would not be what the destination
    /// expects to receive and which most viewers cannot open.
    ///
    /// An application that cannot write what the destination asks for should
    /// refuse and say which formats it can write. A refusal a person can act
    /// on beats a file that silently is not what it claims.
    fn encode(&mut self, uid: u64, into: Destination<'_>) -> Result<Vec<u8>, String> {
        let id = PhotoId(uid);
        let format = export_format_for(into);
        let opts = lightcraft_engine::export::ExportOptions { format, ..Default::default() };
        let item = lightcraft_engine::export::prepare_export(&mut self.session, id, &opts, 1)
            .map_err(|e| format!("could not prepare photo for export: {e}"))?;
        let dest = lightcraft_engine::export::Destination { dir: String::new(), exact: Some("cloud_export".into()) };
        let mut captured = Vec::new();
        lightcraft_engine::export::run_batch(
            vec![item],
            &opts,
            &dest,
            &mut |_path, b| {
                captured = b.to_vec();
                Ok(())
            },
            &|_| false,
            true,
            &mut |_, _| true,
        )
        .map_err(|e| format!("could not export photo: {e}"))?;
        if captured.is_empty() {
            return Err("photo export produced no bytes".into());
        }
        Ok(captured)
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

/// Draw the cloud Open page inside an existing panel.
///
/// Drawn only when a service is configured. An empty list where a heading
/// should be reads to a user as a broken window.
pub fn open_section(app: &mut LightcraftApp, ui: &mut Ui) {
    if !available(app) {
        return;
    }
    let t = Tokens::get(ui.ctx());
    ui.add_space(12.0);
    ui.label(egui::RichText::new("Cloud Storage").font(egui::FontId::new(15.0, egui::FontFamily::Name("semibold".into()))).color(t.text));
    ui.add_space(8.0);
    open_page(app, ui);
}

/// The cloud Open and Save pages as a modal, reached from the File menu.
pub fn modal(app: &mut LightcraftApp, page: CloudPage, ctx: &egui::Context) {
    let title = match page {
        CloudPage::Open => "Open from Cloud Storage",
        CloudPage::Save => "Save to Cloud Storage",
    };
    let mut close = false;
    egui::Modal::new(egui::Id::new("lightcraft_cloud_page")).show(ctx, |ui| {
        let margin = egui::Frame::popup(ui.style()).total_margin().sum();
        let room = (ctx.content_rect().size() - margin - egui::vec2(16.0, 16.0)).max(egui::Vec2::splat(1.0));
        let width = 680.0_f32.min(room.x);
        ui.set_min_width(360.0_f32.min(width));
        ui.set_max_width(width);
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
