//! The buttons that open a file dialog and put the answer into a field or start a job with it
//! (Choose… beside a folder, Install from file…). Like a command's dialog (`ask`), theirs is shown
//! off the UI thread and the answer is applied when it closes. Each [`Field`] is its own entry in
//! the list of dialogs that are up, so a button opens one dialog at a time. An answer for a field
//! of a dialog that was closed meanwhile is dropped (`Field::is_open`).

use super::{PickRequest, PickStart, Then};
use crate::state::Dialog;
use crate::{LightcraftApp, Services};
use serde_json::json;

/// A control that takes a dialog's answer itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field {
    /// Local ▸ Browse Folder…: the folder is shown in Local.
    BrowseFolder,
    /// The import review's Copy to / Move to Choose….
    ImportDestination,
    /// The export dialog's watermark graphic.
    ExportWatermarkImage,
    /// The export dialog's folder.
    ExportFolder,
    /// Settings ▸ Import ▸ Watched folder.
    AutoImportFolder,
    /// Settings ▸ Performance ▸ Smart previews ▸ Choose Folder…. What happens to the previews
    /// already built is the "Existing previews" choice when the dialog closes.
    #[cfg(not(target_arch = "wasm32"))]
    SmartPreviewsFolder,
    /// The Lightroom import's location for a new library.
    LightroomLocation,
    /// Settings ▸ Denoise ▸ Install from file….
    DenoiseModel,
    /// Settings ▸ Slideshow ▸ Overlays ▸ identity plate image.
    SlidePlateImage,
    /// Print ▸ Templates ▸ Import…: every chosen file is imported.
    PrintTemplateImport,
    /// Print ▸ Templates ▸ Export… for the template `name`, suggested as `file`.
    PrintTemplateExport { name: String, file: String },
    /// Print ▸ Print Job ▸ Profile ▸ Other…: the ICC profile the job is printed with.
    PrintProfile,
    /// Settings ▸ Display ▸ Choose Profile….
    MonitorProfile,
    /// Settings ▸ Slideshow ▸ Templates ▸ Import…: every chosen file is imported.
    SlideshowTemplateImport,
    /// Settings ▸ Slideshow ▸ Templates ▸ Export… for the template `id`, suggested as `file`.
    SlideshowTemplateExport { id: String, file: String },
    /// Edit ▸ Soft Proofing ▸ profile ▸ Other….
    ProofProfile,
}

impl Field {
    /// The name this field's dialog goes by in the list of dialogs that are up.
    pub fn id(&self) -> &'static str {
        match self {
            Field::BrowseFolder => "field:browseFolder",
            Field::ImportDestination => "field:importDestination",
            Field::ExportWatermarkImage => "field:exportWatermarkImage",
            Field::ExportFolder => "field:exportFolder",
            Field::AutoImportFolder => "field:autoImportFolder",
            #[cfg(not(target_arch = "wasm32"))]
            Field::SmartPreviewsFolder => "field:smartPreviewsFolder",
            Field::LightroomLocation => "field:lightroomLocation",
            Field::DenoiseModel => "field:denoiseModel",
            Field::SlidePlateImage => "field:slidePlateImage",
            Field::PrintTemplateImport => "field:printTemplateImport",
            Field::PrintTemplateExport { .. } => "field:printTemplateExport",
            Field::PrintProfile => "field:printProfile",
            Field::MonitorProfile => "field:monitorProfile",
            Field::SlideshowTemplateImport => "field:slideshowTemplateImport",
            Field::SlideshowTemplateExport { .. } => "field:slideshowTemplateExport",
            Field::ProofProfile => "field:proofProfile",
        }
    }

    fn request(&self) -> PickRequest {
        use crate::i18n::tr;
        match self {
            Field::BrowseFolder => PickRequest::folder(tr("Browse Folder…")),
            #[cfg(not(target_arch = "wasm32"))]
            Field::SmartPreviewsFolder => PickRequest::folder(tr("Choose Folder…")),
            Field::ImportDestination | Field::ExportFolder | Field::AutoImportFolder | Field::LightroomLocation => PickRequest::folder(tr("Choose…")),
            Field::ExportWatermarkImage | Field::SlidePlateImage => {
                PickRequest::file(tr("Choose…"), tr("Photos"), lightcraft_engine::import::EXTENSIONS)
            }
            Field::DenoiseModel => PickRequest::file(tr("Install from file…"), tr("ONNX model"), &["onnx"]),
            Field::PrintTemplateImport => PickRequest::files(
                Some(tr("Import Print Templates").to_string()),
                tr("Print Templates"),
                &[lightcraft_engine::print::template::FILE_EXT],
            ),
            Field::PrintTemplateExport { file, .. } => {
                PickRequest::save(tr("Export Print Template"), tr("Print Templates"), &[lightcraft_engine::print::template::FILE_EXT], file.clone())
            }
            Field::PrintProfile | Field::MonitorProfile | Field::ProofProfile => {
                PickRequest::file(tr("Choose Monitor Profile"), tr("ICC Profiles"), &["icc", "icm"]).starting_in(PickStart::MonitorProfiles)
            }
            Field::SlideshowTemplateImport => {
                PickRequest::files(Some(tr("Import Slideshow Templates").to_string()), tr("Slideshow Templates"), &["json"])
            }
            Field::SlideshowTemplateExport { file, .. } => {
                PickRequest::save(tr("Export Slideshow Template"), tr("Slideshow Templates"), &["json"], file.clone())
            }
        }
    }

    /// The synchronous dialog of hosts that cannot show one elsewhere: `None` when it has none.
    fn sync_dialog(&self, services: &mut Services) -> Option<Vec<String>> {
        match self {
            Field::ExportWatermarkImage | Field::SlidePlateImage => services.pick_files.as_mut().map(|f| f()),
            Field::DenoiseModel => services.pick_denoise_model.as_mut().map(|f| f()),
            Field::PrintTemplateImport => services.pick_print_templates.as_mut().map(|f| f()),
            Field::PrintTemplateExport { file, .. } => services.save_print_template.as_mut().map(|f| f(file).into_iter().collect()),
            Field::PrintProfile | Field::MonitorProfile | Field::ProofProfile => services.pick_display_profile.as_mut().map(|f| f()),
            Field::SlideshowTemplateImport => services.pick_slideshow_templates.as_mut().map(|f| f()),
            Field::SlideshowTemplateExport { file, .. } => services.save_slideshow_template.as_mut().map(|f| f(file).into_iter().collect()),
            _ => services.pick_folder.as_mut().map(|f| f().into_iter().collect()),
        }
    }

    /// Whether the dialog or tab holding the field is still open (always, for one that is not in a dialog).
    pub(super) fn is_open(&self, app: &LightcraftApp) -> bool {
        match self {
            Field::BrowseFolder => true,
            Field::ImportDestination => matches!(app.ui.dialog, Some(Dialog::Import { .. })),
            Field::ExportWatermarkImage | Field::ExportFolder => matches!(app.ui.dialog, Some(Dialog::Export { .. })),
            Field::LightroomLocation => matches!(app.ui.dialog, Some(Dialog::LightroomDestination { .. })),
            Field::AutoImportFolder
            | Field::SlidePlateImage
            | Field::MonitorProfile
            | Field::SlideshowTemplateImport
            | Field::SlideshowTemplateExport { .. } => matches!(app.ui.dialog, Some(Dialog::Settings { .. })),
            Field::PrintTemplateImport | Field::PrintTemplateExport { .. } | Field::PrintProfile => matches!(app.ui.dialog, Some(Dialog::Print)),
            // the soft-proof menu is in the Edit panel, not in a dialog
            Field::ProofProfile => true,
            Field::DenoiseModel => matches!(&app.ui.dialog, Some(Dialog::Settings { tab }) if tab == "denoise"),
            #[cfg(not(target_arch = "wasm32"))]
            Field::SmartPreviewsFolder => matches!(app.ui.dialog, Some(Dialog::Settings { .. })),
        }
    }
}

/// The button for `field` was pressed. With a host that shows dialogs elsewhere this returns at
/// once, and [`apply`] runs when the dialog closes (nothing happens meanwhile if one is already
/// up for the field). Without one the synchronous dialog answers here: the chosen path comes back
/// for the caller to use (none: cancelled, or this host has no such dialog).
pub fn choose(app: &mut LightcraftApp, field: Field) -> Option<String> {
    choose_all(app, field).into_iter().next()
}

/// [`choose`] for a field that takes every file chosen (the template imports).
pub fn choose_all(app: &mut LightcraftApp, field: Field) -> Vec<String> {
    match super::start(app, field.id(), field.request(), Then::Field(field.clone())) {
        super::Started::Later => Vec::new(),
        super::Started::No => {
            let mut paths = field.sync_dialog(&mut app.services).unwrap_or_default();
            // only the template imports take several files, as the dialogs of the other fields always chose one
            if !matches!(field, Field::PrintTemplateImport | Field::SlideshowTemplateImport) {
                paths.truncate(1);
            }
            paths
        }
    }
}

/// [`choose`] for a field outside a dialog's own state (the import, export and Lightroom dialogs
/// are drawn from a copy, so their callers take the synchronous answer into it): a synchronous
/// answer is applied at once.
pub fn choose_and_apply(app: &mut LightcraftApp, ctx: &egui::Context, field: Field) {
    for path in choose_all(app, field.clone()) {
        apply(app, ctx, &field, path);
    }
}

/// Put the chosen `path` where `field` takes it.
pub(super) fn apply(app: &mut LightcraftApp, ctx: &egui::Context, field: &Field, path: String) {
    match field {
        Field::BrowseFolder => match app.run("library.browse", json!({"path": path})) {
            // the chosen folder stays in Local (and comes back if it was hidden)
            Ok(r) => {
                let dir = r["path"].as_str().unwrap_or(&path).to_string();
                let _ = app.run("local.addRoot", json!({"path": dir}));
            }
            Err(e) => app.toast(ctx, e),
        },
        Field::ImportDestination => {
            if let Some(Dialog::Import { opts }) = &mut app.ui.dialog {
                opts.destination = path;
            }
        }
        Field::ExportWatermarkImage => {
            if let Some(Dialog::Export { opts, .. }) = &mut app.ui.dialog
                && let Some(wm) = opts.watermark.as_mut()
            {
                wm.image = path;
            }
        }
        Field::ExportFolder => {
            if let Some(Dialog::Export { dir, .. }) = &mut app.ui.dialog {
                *dir = path;
            }
        }
        Field::LightroomLocation => {
            if let Some(Dialog::LightroomDestination { location, .. }) = &mut app.ui.dialog {
                *location = path;
            }
        }
        Field::AutoImportFolder => {
            if let Err(e) = app.run("library.autoImport", json!({"folder": path})) {
                app.toast(ctx, e);
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        Field::SmartPreviewsFolder => {
            let existing = crate::dialogs::settings::existing_previews(ctx);
            let result = app.run("library.smartPreviewsLocation", json!({"path": path, "existing": existing}));
            crate::dialogs::settings::smart_previews_changed(app, ctx, existing, result);
        }
        Field::DenoiseModel => crate::panels::denoise::inspect_model(app, &path),
        Field::SlidePlateImage => {
            let _ = app.run("slideshow.overlays", json!({"identityPlate": {"image": path}}));
        }
        Field::PrintTemplateImport => crate::print_view::template_imported(app, ctx, path),
        Field::PrintTemplateExport { name, .. } => crate::print_view::template_exported(app, ctx, name, path),
        Field::PrintProfile => crate::print_view::use_profile(app, ctx, path),
        Field::MonitorProfile => app.ui.settings.display_profile = path,
        Field::SlideshowTemplateImport => crate::dialogs::settings::slide_templates::imported(app, ctx, path),
        Field::SlideshowTemplateExport { id, .. } => crate::dialogs::settings::slide_templates::exported(app, ctx, id, path),
        Field::ProofProfile => {
            if let Err(e) = app.run("view.softProof", json!({"profile": path})) {
                app.toast(ctx, e);
            }
        }
    }
}
