//! Modal dialogs (new album, rename, create preset, choose settings to copy, export, about, shortcuts).

use lcc_develop::{ControlSpec, Section, SettingsGroup, SettingsPick, Track};
use serde_json::json;

use crate::LccApp;
use crate::state::Dialog;
use crate::theme::Tokens;

/// Rows the Rename dialog previews.
const RENAME_PREVIEW_ROWS: usize = 6;

/// The Rename dialog's preview: its first rows (`None` while being planned) and how many files
/// are renamed. Which names are taken is checked on disk, which can block on a slow drive, so the
/// rows are planned on a worker thread, and only when the template, start number, photos or
/// catalog change — never once per frame.
pub(crate) fn rename_preview(
    app: &mut LccApp,
    ctx: &egui::Context,
    template: &str,
    start: usize,
) -> (Option<Vec<lcc_engine::rename::RenamePlan>>, usize) {
    use std::hash::{Hash, Hasher};
    type Rows = std::sync::Arc<std::sync::Mutex<Option<Vec<lcc_engine::rename::RenamePlan>>>>;
    let ids = app.session.selected();
    let photos = lcc_engine::rename::rename_photos(&app.session.catalog, &ids);
    let total = photos.len();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (template, start, app.session.catalog.revision, &ids).hash(&mut h);
    let key = h.finish();
    let id = egui::Id::new("rename-preview-rows");
    let read = |rows: &Rows| rows.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    if let Some((k, rows)) = ctx.data(|d| d.get_temp::<(u64, Rows)>(id))
        && k == key
    {
        return (read(&rows), total);
    }
    let rows: Rows = Default::default();
    let first: Vec<_> = photos.into_iter().take(RENAME_PREVIEW_ROWS).collect();
    let (out, template, repaint) = (rows.clone(), template.to_string(), ctx.clone());
    let exists = app.session.media.availability.probe();
    let work = move || {
        let plans = lcc_engine::rename::plan_rename_photos(&first, &template, start, &|f| exists(f));
        *out.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(plans);
        repaint.request_repaint();
    };
    #[cfg(not(target_arch = "wasm32"))]
    if let Err(e) = std::thread::Builder::new().name("lc-rename-preview".into()).spawn(work) {
        log::warn!("rename preview: {e}");
    }
    #[cfg(target_arch = "wasm32")]
    work();
    ctx.data_mut(|d| d.insert_temp(id, (key, rows.clone())));
    (read(&rows), total)
}

/// The Export dialog's watermark preview: the first exported photo, as wide as the dialog whatever
/// its shape, with the watermark drawn on it as the file will have it at size `resize` (`None` =
/// full size). On a worker thread, once per photo, catalog change and size step, the cached
/// thumbnail comes first (at once, a little soft) and then a render at the preview's own pixel size
/// replaces it. The watermark is redrawn on a copy only when it, the output size or the picture
/// changes, which is quick at this size, so the preview follows the sliders.
fn watermark_preview(app: &mut LccApp, ui: &mut egui::Ui, wm: &lcc_engine::export::Watermark, resize: Option<lcc_engine::export::Resize>) {
    use std::hash::{Hash, Hasher};
    // (pictures received so far: 1 the thumbnail, 2 the sharp render; the latest, or why none came)
    type Base = std::sync::Arc<std::sync::Mutex<(u8, Option<Result<lcc_raster::Rgba8, String>>)>>;
    type Shown = (u64, u8, lcc_engine::export::Watermark, usize, egui::TextureHandle);
    let t = Tokens::get(ui.ctx());
    let Some(&id) = app.session.selected().first() else { return };
    let Some(photo) = app.session.catalog.photo(id).cloned() else { return };
    let (out_w, out_h) = lcc_engine::export::output_size(&photo, &lcc_engine::export::ExportOptions { resize, ..Default::default() });
    let export_short = out_w.min(out_h);
    // pixels across the dialog, in steps so a scroll bar or a point of layout doesn't re-render
    let step = 128.0;
    let px = ((ui.available_width() * ui.ctx().pixels_per_point() / step).ceil() * step).clamp(step, 2048.0) as usize;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (id, app.session.catalog.revision, px).hash(&mut h);
    let key = h.finish();
    let ctx = ui.ctx().clone();
    let (base_id, shown_id) = (egui::Id::new("export-wm-preview-base"), egui::Id::new("export-wm-preview-shown"));
    let base = match ctx.data(|d| d.get_temp::<(u64, Base)>(base_id)) {
        Some((k, base)) if k == key => base,
        _ => {
            let base: Base = Default::default();
            // the thumbnail, then as wide as the dialog (a tall photo up to three times that high)
            let jobs: Vec<_> = [app.session.thumb_job(id, 512), app.session.render_job(id, px, px * 3, false, true)].into_iter().flatten().collect();
            // the web build has no worker thread: a full render there would hold up the page
            #[cfg(target_arch = "wasm32")]
            let jobs: Vec<_> = jobs.into_iter().take(1).collect();
            if jobs.is_empty() {
                *base.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = (1, Some(Err("no such photo".into())));
            } else {
                let (out, repaint) = (base.clone(), ctx.clone());
                let work = move || {
                    for job in jobs {
                        // (a panic in the render is its failure: `RenderJob::run`)
                        let r = job.run().rendered.map(|r| r.image);
                        let mut got = out.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        // a failed sharp render keeps the thumbnail
                        if r.is_ok() || !matches!(got.1, Some(Ok(_))) {
                            *got = (got.0.saturating_add(1), Some(r));
                            repaint.request_repaint();
                        }
                    }
                };
                #[cfg(not(target_arch = "wasm32"))]
                if let Err(e) = std::thread::Builder::new().name("lc-watermark-preview".into()).spawn(work) {
                    log::warn!("watermark preview: {e}");
                }
                #[cfg(target_arch = "wasm32")]
                work();
            }
            ctx.data_mut(|d| d.insert_temp(base_id, (key, base.clone())));
            base
        }
    };
    let shown = ctx.data(|d| d.get_temp::<Shown>(shown_id));
    let stage = base.lock().unwrap_or_else(std::sync::PoisonError::into_inner).0;
    let tex = match shown {
        Some((k, g, w, s, tex)) if k == key && g == stage && &w == wm && s == export_short => tex,
        _ => {
            let photo_px = match &base.lock().unwrap_or_else(std::sync::PoisonError::into_inner).1 {
                Some(Ok(img)) => Ok(img.clone()),
                Some(Err(e)) => Err(Some(e.clone())),
                None => Err(None),
            };
            let mut img = match photo_px {
                Ok(img) => img,
                Err(e) => {
                    let note = if e.is_some() { "Preview unavailable" } else { "Loading preview…" };
                    let r = ui
                        .horizontal(|ui| {
                            if e.is_none() {
                                crate::progress::spinner(ui, 12.0);
                            }
                            crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr(note)).color(t.text_dim))
                        })
                        .inner;
                    if let Some(e) = e {
                        r.on_hover_text(e);
                    }
                    return;
                }
            };
            lcc_engine::export::draw_watermark_preview(&mut img, wm, export_short);
            // shown through the monitor profile like every other preview
            if let Some(d) = app.renderer.display()
                && let Err(e) = d.profile.from_srgb(&mut img)
            {
                log::warn!("display profile: {e}");
            }
            let color = std::sync::Arc::new(egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.as_bytes()));
            let tex = match shown {
                Some((.., mut tex)) => {
                    tex.set(color, egui::TextureOptions::LINEAR);
                    tex
                }
                None => ctx.load_texture("export-wm-preview", color, egui::TextureOptions::LINEAR),
            };
            ctx.data_mut(|d| d.insert_temp(shown_id, (key, stage, wm.clone(), export_short, tex.clone())));
            tex
        }
    };
    let size = tex.size_vec2();
    let scale = ui.available_width() / size.x.max(1.0);
    let r = ui.vertical_centered(|ui| ui.add(egui::Image::new((tex.id(), size * scale)))).inner;
    crate::widgets::register(ui.ctx(), "image:exportWatermarkPreview", r.rect);
}

/// The width most dialogs open at (points).
const DIALOG_W: f32 = 380.0;

/// Help ▸ What's New's width on a window `screen_w` points wide: three times a dialog's
/// ([`DIALOG_W`]), so its long lines read as paragraphs, unless the window is narrower than that
/// (then the window's width, less [`crate::dialog_frame::SCREEN_MARGIN`] each side).
pub(crate) fn whats_new_width(screen_w: f32) -> f32 {
    let room = if screen_w.is_finite() { screen_w - 2.0 * crate::dialog_frame::SCREEN_MARGIN } else { 0.0 };
    (3.0 * DIALOG_W).min(room).max(1.0)
}

/// Help ▸ What's New: upstream's docs/whats-new.md and our docs/whats-new/ entries, joined by build.rs.
pub const WHATS_NEW: &str = include_str!(concat!(env!("OUT_DIR"), "/whats-new.md"));

/// The import review, Export and Settings keep window ids of their own, so the size each is given
/// doesn't carry over to the other dialogs.
///
/// Every other dialog has a window id of its own kind: egui keeps a window's size (and never
/// shrinks it to its content), so a wide dialog (the Presets Manager, Chart Profile) left every
/// later one that shared its id as wide, e.g. Create Collection ~2000 points with a full-width
/// Name field. Each kind now starts at its own default width.
pub(crate) fn window_id(dialog: &Dialog) -> egui::Id {
    let own = match dialog {
        Dialog::Import { .. } => "app-import-dialog",
        Dialog::Print => "app-print-dialog",
        Dialog::Export { .. } => "app-export-dialog",
        // (its own id: the size the user gives it doesn't carry over to the other dialogs)
        Dialog::PresetsManager { .. } => crate::panels::presets_manager::WINDOW,
        Dialog::ProfileManager { .. } => crate::panels::profile_manager::WINDOW,
        // (the Settings screen's own layer: `settings_screen::LAYER`)
        Dialog::Settings { .. } => crate::settings_screen::LAYER,
        _ => return egui::Id::new("app-dialog").with(std::mem::discriminant(dialog)),
    };
    egui::Id::new(own)
}

/// Block the underlying panels from the first frame, before the window establishes its modal
/// layer. Close background menus on entry; popups subsequently opened inside the dialog stay usable.
pub(crate) fn prepare(app: &LccApp, ui: &mut egui::Ui) {
    let Some(dialog) = &app.ui.dialog else { return };
    ui.disable();
    let layer = egui::LayerId::new(egui::Order::Middle, window_id(dialog));
    if ui.ctx().memory(|m| m.top_modal_layer()) != Some(layer) {
        egui::Popup::close_all(ui.ctx());
    }
}

pub fn show(app: &mut LccApp, ctx: &egui::Context) {
    // the Modern layout's Settings screen and Export sheet (fork)
    if crate::modern_sheets::show(app, ctx) {
        return;
    }
    let Some(mut dlg) = app.ui.dialog.clone() else { return };
    let at_start = dlg.clone();
    // Esc with a popup open in the dialog (a dropdown, a date picker's calendar) closes the popup
    // only: claimed before the dialog is drawn, since the popup is gone once it has handled the key
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && egui::Popup::is_any_open(ctx) {
        crate::widgets::take_escape(ctx);
    }
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    // The backdrop is an area below the dialog window (a bare `Middle` layer painter would be
    // painted after every area — i.e. over the dialog too).
    egui::Area::new(egui::Id::new("dialog-dim")).order(egui::Order::Middle).fixed_pos(screen.min).interactable(false).show(ctx, |ui| {
        ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(140));
    });
    let mut close = false;
    let mut confirm = false;
    // Return presses OK in a dialog without a text field of its own (Lightroom's default button);
    // a widget that has the keyboard focus (a text field, a button or checkbox reached with Tab)
    // takes Return itself. Consumed before the dialog is drawn, so nothing else sees it too.
    if return_confirms(&dlg)
        && ctx.memory(|m| m.focused().is_none())
        && !egui::Popup::is_any_open(ctx)
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
    {
        confirm = true;
    }
    let title: String = match &dlg {
        Dialog::TextPrompt { title, .. } => title.as_str(),
        Dialog::NewAlbum { folder: true, .. } => "Create Collection Set",
        Dialog::NewAlbum { .. } => "Create Collection",
        Dialog::RenameAlbum { .. } => "Rename Collection",
        Dialog::RenamePhoto { .. } => "Rename Photo",
        Dialog::Rename { .. } => "Rename Photos",
        Dialog::Import { .. } => "Import Photos",
        Dialog::LightroomResult { capture_one: true, .. } => "Capture One Catalog Import",
        Dialog::LightroomResult { .. } => "Lightroom Catalog Import",
        Dialog::LabelNames { .. } => "Edit Color Label Names",
        Dialog::CaptureTime { .. } => "Edit Capture Time",
        Dialog::RenameKeyword { .. } => "Rename Keyword",
        Dialog::RenamePerson { .. } => "Rename Person",
        Dialog::KeywordTag { editing: None, .. } => "Create Keyword Tag",
        Dialog::KeywordTag { .. } => "Edit Keyword Tag",
        Dialog::DeleteKeyword { .. } => "Delete Keyword",
        Dialog::MoveKeyword { .. } => "Merge Keywords",
        Dialog::KeywordSet { .. } => "Edit Keyword Set",
        Dialog::MergeKeywords { .. } => "Merge Keywords",
        Dialog::NewSmartAlbum { .. } => "Create Smart Collection",
        Dialog::AllMetadata { .. } => "All Metadata",
        Dialog::SystemInfo { .. } => "System Info",
        Dialog::DngValidation { .. } => "Validate DNG Files",
        Dialog::DenoiseModel { .. } => "AI Denoise Model",
        Dialog::FaceModel { info, .. } if info["download"].is_string() => "Download Face Model",
        Dialog::FaceModel { .. } => "Add Face Model",
        Dialog::WhatsNew => "What's New",
        Dialog::Cull { .. } => "Assisted Culling",
        Dialog::SmartRules { id: None, .. } => "Create Smart Collection",
        Dialog::SmartRules { .. } => "Edit Smart Collection",
        Dialog::AutoStack { .. } => "Auto-Stack by Capture Time",
        Dialog::AdvancedSearch { .. } => "Advanced Search",
        Dialog::CreatePreset { .. } => "Create Preset",
        Dialog::UpdatePreset { .. } => "Update Preset with Current Settings",
        Dialog::CreatePresetFolder { .. } => "Create Preset Folder",
        Dialog::CopySettings { .. } => "Choose Edit Settings to Copy",
        Dialog::PasteSettings { .. } => "Paste Selected Settings",
        Dialog::SyncSettings { .. } => "Synchronize Settings",
        Dialog::SyncCopies { .. } => "Synchronize Copies",
        Dialog::SyncSnapshots { .. } => "Synchronize Snapshots",
        Dialog::Export { .. } => "Export",
        Dialog::ContactSheet { .. } => "Contact Sheet PDF",
        Dialog::Timelapse { .. } => "Timelapse Video",
        Dialog::CropOverlays { .. } => "Choose Overlays to Cycle",
        Dialog::BookPrefs { .. } => "Book Preferences",
        Dialog::EmailPhoto { .. } => "Email Photo",
        Dialog::SlideshowExport { .. } => "Export Slideshow",
        Dialog::Print => "Print",
        Dialog::Merge { opts } => opts.title(),
        Dialog::Enhance { .. } => "Enhance Preview",
        Dialog::Settings { .. } => "Settings",
        Dialog::ConfirmDelete { .. } => "Delete Photos",
        Dialog::ConfirmPurge { all: true, .. } => "Empty Recently Deleted",
        Dialog::ConfirmPurge { .. } => "Delete Permanently",
        Dialog::MovePhotos { .. } => "Move Photos",
        Dialog::LightroomCatalog { .. } => "Lightroom Classic Catalog",
        Dialog::LightroomDestination { catalog, .. } if crate::lightroom_import::is_capture_one(std::path::Path::new(catalog)) => {
            "Import Capture One Catalog"
        }
        Dialog::LightroomDestination { .. } => "Import Lightroom Catalog",
        Dialog::SaveEffect { .. } => "New Effect Preset",
        Dialog::ManageEffects { .. } => "Manage Effect Presets",
        Dialog::PresetsManager { .. } => "Presets Manager",
        Dialog::ProfileManager { .. } => "Profile Manager",
        Dialog::DeleteEffects { names } if names.len() == 1 => "Delete Effect Preset",
        Dialog::DeleteEffects { .. } => "Delete Effect Presets",
        Dialog::DeleteRejected { .. } => "Delete Rejected Photos",
        Dialog::StopWaiting { .. } => "Stop Waiting",
        Dialog::VignettingCreateAnyway { .. } | Dialog::VignettingReplace { .. } => "Vignetting Calibration",
        Dialog::RemoveFolder { disk: true, .. } => "Remove Disk from Catalog",
        Dialog::RemoveFolder { .. } => "Remove Folder from Catalog",
        Dialog::SynchronizeFolder { .. } => "Synchronize Folder",
        // (nothing to download from in this build: the dialog explains the manual install)
        Dialog::ChartProfile { .. } => "Create Camera Profile from Color Chart",
        Dialog::SamModel { .. } if sam_by_hand(&app.session.segmenter) => "Install the SAM 3 Model",
        Dialog::SamModel { .. } => "Download the SAM 3 Model?",
        Dialog::AiModels { .. } => "AI Models",
        Dialog::GenfillConsent { .. } => "Generative Fill",
        Dialog::About => "About {app}",
        Dialog::Update => "Check for Updates",
        Dialog::Shortcuts => "Keyboard Shortcuts",
        Dialog::ViewOptions => "Library View Options",
        Dialog::NewPhotoDefault { .. } => "Save as Default for New Photos",
        Dialog::Watermark { .. } => "Watermark Editor",
    }
    .to_string();
    // a card: rounded, a faint light edge, a soft shadow; the title inside it (below)
    // On a phone the small dialogs (not the full-window screens) are bottom sheets: the window's
    // full width, on its bottom edge, the top corners rounded, a grab handle that closes them.
    let sheet = crate::dialog_frame::phone_sheet(ctx, &dlg);
    let insets = app.ui.modern.insets;
    let frame = egui::Frame::window(&ctx.global_style())
        .inner_margin(egui::Margin { left: 18, right: 18, top: if sheet { 6 } else { 14 }, bottom: 14 })
        .corner_radius(if sheet { egui::CornerRadius { nw: 20, ne: 20, sw: 0, se: 0 } } else { egui::CornerRadius::same(16) })
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_white_alpha(26)))
        .shadow(egui::epaint::Shadow { offset: [0, 10], blur: 32, spread: 0, color: egui::Color32::from_black_alpha(110) });
    // The import review can be resized (its photo grid takes the room); it keeps a window id of its
    // own so the size it is given doesn't carry over to the other dialogs. Its content scrolls
    // rather than growing the window when the options below the grid get taller (e.g. Copy).
    let import = matches!(dlg, Dialog::Import { .. });
    // The Print dialog sizes itself to the window and scrolls its settings column on its own.
    let print = matches!(dlg, Dialog::Print);
    // Export lays out its own columns (presets beside the settings, which scroll) and can be made
    // wider (a large UI font); its height follows its content, capped to the window.
    let export = matches!(dlg, Dialog::Export { .. });
    // The Presets Manager can be resized and keeps the size it is given (remembered across
    // starts), capped to the screen; its list fills it and never widens it.
    // (the Profile Manager is sized the same way, with a size of its own)
    let profiles_manager = matches!(dlg, Dialog::ProfileManager { .. });
    let presets = matches!(dlg, Dialog::PresetsManager { .. }) || profiles_manager;
    let remembered = if profiles_manager { app.ui.profile_manager_size } else { app.ui.presets_manager_size };
    let presets_size = crate::panels::presets_manager::window_size(remembered, screen);
    let presets_max = crate::panels::presets_manager::max_size(screen);
    let window_id = window_id(&dlg);
    // Settings keeps its top edge, and the widest it has been, while tabs of different sizes are
    // shown, so its tab row stays under the pointer (centred like the others, the row jumped with
    // each tab's height)
    let settings = matches!(dlg, Dialog::Settings { .. });
    let top = (screen.height() * 0.08).clamp(crate::dialog_frame::SCREEN_MARGIN, 96.0);
    let widest_id = window_id.with("widest");
    let widest = if settings { ctx.data(|d| d.get_temp::<f32>(widest_id)).unwrap_or(0.0).min(screen.width() * 0.9) } else { 0.0 };
    let mut content_w = 0.0f32;
    // before the window: drawing it closes a dropdown the click lands outside of
    let popup_was_open = egui::Popup::is_any_open(ctx);
    let heading = crate::i18n::tr(&title).to_string();
    let shown = egui::Window::new(heading.clone()).id(window_id)
        .title_bar(false)
        .collapsible(false)
        .resizable([import || export || presets, import || presets])
        .vscroll(import)
        .resize(|r| {
            if import {
                r.default_height(crate::import::DIALOG_SIZE[1])
            } else if presets {
                r.default_height(presets_size.y).min_size(crate::panels::presets_manager::MIN_SIZE).max_size(presets_max)
            } else {
                r
            }
        })
        .frame(frame)
        .anchor(
            if sheet {
                egui::Align2::CENTER_BOTTOM
            } else if settings {
                egui::Align2::CENTER_TOP
            } else {
                egui::Align2::CENTER_CENTER
            },
            [(insets.left - insets.right) / 2.0, if sheet { -insets.bottom } else if settings { top } else { 0.0 }],
        )
        .min_width(if sheet {
            sheet_w(screen, insets)
        } else if presets {
            crate::panels::presets_manager::MIN_SIZE[0].min(presets_max.x)
        } else if matches!(dlg, Dialog::WhatsNew) {
            whats_new_width(screen.width())
        } else {
            widest
        })
        .max_width(if sheet {
            sheet_w(screen, insets)
        } else if presets {
            // (before the others: the Presets Manager keeps its own cap)
            presets_max.x
        } else if export || !settings {
            // never wider than the window (a full-width field fills the dialog, not the screen)
            (screen.width() - 2.0 * crate::dialog_frame::SCREEN_MARGIN).max(320.0)
        } else {
            f32::INFINITY
        })
        .default_width(if sheet { sheet_w(screen, insets) } else { match dlg {
            Dialog::Import { .. } => crate::import::DIALOG_SIZE[0],
            Dialog::SmartRules { .. } => 680.0,
            Dialog::ChartProfile { .. } => 900.0,
            Dialog::AllMetadata { .. } => 620.0,
            Dialog::Update => 500.0,
            Dialog::About => 480.0,
            Dialog::WhatsNew => whats_new_width(screen.width()),
            Dialog::Print => 1100.0,
            Dialog::FaceModel { .. } => 460.0,
            Dialog::LightroomCatalog { .. } => 440.0,
            Dialog::LightroomDestination { .. } => 480.0,
            Dialog::ManageEffects { .. } => 560.0,
            Dialog::PresetsManager { .. } | Dialog::ProfileManager { .. } => presets_size.x,
            // (the preset column beside a form: its label column, then the controls)
            Dialog::Export { .. } => 900.0_f32.min(screen.width() - 2.0 * crate::dialog_frame::SCREEN_MARGIN),
            Dialog::Rename { .. } | Dialog::ContactSheet { .. } | Dialog::Timelapse { .. } | Dialog::SlideshowExport { .. } | Dialog::EmailPhoto { .. } => 480.0,
            Dialog::CaptureTime { .. } | Dialog::LabelNames { .. } | Dialog::CreatePreset { .. } | Dialog::CreatePresetFolder { .. } => 440.0,
            _ => DIALOG_W,
        } })
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            if sheet && crate::dialog_frame::grab_handle(ui) {
                close = true;
            }
            // the title, left-aligned at the card's top (no title bar)
            let h = ui.label(egui::RichText::new(&heading).font(t.semibold(16.0)).color(t.text));
            crate::widgets::register(ui.ctx(), "dialog:title", h.rect);
            ui.add_space(2.0);
            // fixed header (tabs) / scrolling body / fixed footer (buttons), capped to the screen;
            // the import review sizes and scrolls itself (it can be resized)
            let mut layout = if settings {
                crate::dialog_frame::ModalLayout::begin_below(ui, window_id, top)
            } else {
                crate::dialog_frame::ModalLayout::begin(ui, window_id)
            };
            // (Settings keeps its category list in place and scrolls its content itself)
            let settings_height = layout.body_max_height(ui);
            // dialogs laid out as a form (a label column beside the controls)
            let formed = matches!(
                dlg,
                Dialog::CreatePreset { .. }
                    | Dialog::CreatePresetFolder { .. }
                    | Dialog::Rename { .. }
                    | Dialog::ContactSheet { .. }
                    | Dialog::Timelapse { .. }
                    | Dialog::EmailPhoto { .. }
                    | Dialog::CaptureTime { .. }
                    | Dialog::LabelNames { .. }
                    | Dialog::Watermark { .. }
                    | Dialog::SlideshowExport { .. }
            );
            let mut body = |ui: &mut egui::Ui| match &mut dlg {
                Dialog::AdvancedSearch { search } => crate::advanced_search::body(ui, search, app),
                Dialog::AutoStack { gap } => {
                    ui.label(egui::RichText::new(crate::i18n::tr("Stack photos taken within this time of each other:")).color(t.text_label));
                    let r = ui.add(
                        egui::Slider::new(gap, 0.0..=86400.0)
                            .logarithmic(true)
                            .smallest_positive(1.0)
                            .custom_formatter(|v, _| crate::panels::dialogs::fmt_gap(v))
                            .custom_parser(crate::panels::dialogs::parse_gap),
                    );
                    crate::widgets::named(r, crate::i18n::tr("Stack photos taken within this time of each other:"));
                    let preview = app.session.execute("stack.auto", &json!({"gap": *gap, "preview": true})).unwrap_or_default();
                    let scope = if app.session.selection.ids.len() > 1 { "the selected photos" } else { "the photos in view" };
                    crate::dialog_frame::note(ui,
                        egui::RichText::new({
                            let (stacks, photos) = (preview["stacks"].as_u64().unwrap_or(0), preview["photos"].as_u64().unwrap_or(0));
                            crate::i18n::tr_format!("Creates {stacks} stack{} from {photos} of {scope}", if stacks == 1 { "" } else { "s" }, stacks = stacks, photos = photos, scope = crate::i18n::tr(scope))
                        }).color(t.text_dim),
                    );
                }
                Dialog::Cull { reject_below, pick_best, suggest_stars, suggest_flags, burst_gap, similarity, closed_eyes, blurry_eyes, soft_eyes, everyone } => {
                    let n = app.session.selection.ids.len();
                    let scope = if n > 1 { crate::i18n::tr_format!("the {n} selected photos", n = n) } else { crate::i18n::tr_format!("the {} photos in view", app.session.visible_cloned().len()) };
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("Scores {scope} for focus and exposure and finds similar shots taken within seconds of each other (bursts).", scope = scope)).color(t.text_label));
                    ui.add_space(6.0);
                    let r = ui.add(egui::Slider::new(reject_below, 0.0..=80.0).text(crate::i18n::tr("Reject below focus")).step_by(1.0));
                    crate::widgets::register(ui.ctx(), "field:cullReject", r.rect);
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr(if *reject_below > 0.0 { "Blurry photos below the score are flagged as rejects." } else { "0: nothing is rejected." })).color(t.text_dim));
                    let r = crate::dialog_frame::checkbox(ui, pick_best, crate::i18n::tr("Pick the sharpest photo of each burst"));
                    crate::widgets::register(ui.ctx(), "check:cullPick", r.rect);
                    // the suggestions (issue #489): stars and flags from the scores, only where
                    // the user has not decided
                    let r = crate::dialog_frame::checkbox(ui, suggest_stars, crate::i18n::tr("Suggest star ratings"));
                    crate::widgets::register(ui.ctx(), "check:cullSuggestStars", r.rect);
                    let r = crate::dialog_frame::checkbox(ui, suggest_flags, crate::i18n::tr("Suggest Pick and Reject flags"));
                    crate::widgets::register(ui.ctx(), "check:cullSuggestFlags", r.rect);
                    if *suggest_stars || *suggest_flags {
                        crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("Photos you have not rated or flagged get stars and flags from their scores; yours are never overwritten.")).color(t.text_dim));
                    }
                    // the eyes, judged as the eye strip judges them
                    let r = crate::dialog_frame::checkbox(ui, closed_eyes, crate::i18n::tr("Reject photos with closed eyes"));
                    crate::widgets::register(ui.ctx(), "check:cullClosedEyes", r.rect);
                    let r = crate::dialog_frame::checkbox(ui, blurry_eyes, crate::i18n::tr("Reject photos with blurry eyes"));
                    crate::widgets::register(ui.ctx(), "check:cullBlurryEyes", r.rect);
                    if *blurry_eyes {
                        // how soft is too soft: the strip's red only, or its amber too
                        ui.horizontal(|ui| {
                            ui.add_space(22.0);
                            let r = ui.radio_value(soft_eyes, false, crate::i18n::tr("Only blurry eyes (red in the eye strip)"));
                            crate::widgets::register(ui.ctx(), "radio:cullBlurryOnly", r.rect);
                        });
                        ui.horizontal(|ui| {
                            ui.add_space(22.0);
                            let r = ui.radio_value(soft_eyes, true, crate::i18n::tr("Soft eyes too (amber in the eye strip)"));
                            crate::widgets::register(ui.ctx(), "radio:cullSoftToo", r.rect);
                        });
                    }
                    if *closed_eyes || *blurry_eyes {
                        let mut main_only = !*everyone;
                        let r = crate::dialog_frame::checkbox(ui, &mut main_only, crate::i18n::tr("Main subjects only (the largest faces), not the people behind them"));
                        crate::widgets::register(ui.ctx(), "check:cullMainOnly", r.rect);
                        *everyone = !main_only;
                        crate::dialog_frame::note(
                            ui,
                            egui::RichText::new(crate::i18n::tr(
                                "Looks at the subjects' eyes the way the eye strip does, about a second a photo, in the background. Picked photos are left alone.",
                            ))
                            .color(t.text_dim),
                        );
                    }
                    ui.add_space(6.0);
                    // the sliders drag over the usual range; their value fields take whatever
                    // `photo.analyze` accepts (a gap of 0 s to a day, a similarity of 0 to 1)
                    let r = ui.add(
                        egui::Slider::new(burst_gap, 1.0..=600.0)
                            .logarithmic(true)
                            .smallest_positive(1.0)
                            .clamping(egui::SliderClamping::Never)
                            .text(crate::i18n::tr("Burst: shots at most this far apart"))
                            .custom_formatter(|v, _| crate::panels::dialogs::fmt_gap(v))
                            .custom_parser(crate::panels::dialogs::parse_gap),
                    );
                    crate::widgets::register(ui.ctx(), "field:cullGap", r.rect);
                    let r = ui.add(
                        egui::Slider::new(similarity, 0.70..=0.99)
                            .clamping(egui::SliderClamping::Never)
                            .text(crate::i18n::tr("Burst: how alike (loose … strict)"))
                            // a typed 0.995 is kept (whole hundredths would round it to 1.00)
                            .step_by(0.001)
                            .min_decimals(2)
                            .max_decimals(3),
                    );
                    crate::widgets::register(ui.ctx(), "field:cullSimilar", r.rect);
                    clamp_cull_burst(burst_gap, similarity);
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("Lower it when a panning or moving burst splits into several groups.")).color(t.text_dim));
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("Scores stay on the photos: filter or make smart collections with Focus, Eyes Open, Aesthetic and Cull Score.")).color(t.text_dim));
                }
                Dialog::WhatsNew => {
                    ui.scope(|ui| {
                        for line in WHATS_NEW.lines() {
                            let l = line.trim_end();
                            if let Some(h) = l.strip_prefix("### ") {
                                ui.add_space(6.0);
                                ui.label(egui::RichText::new(crate::i18n::tr(h)).font(t.semibold(12.5)).color(t.text));
                            } else if let Some(h) = l.strip_prefix("## ") {
                                ui.add_space(8.0);
                                ui.label(egui::RichText::new(crate::i18n::tr(h)).font(t.semibold(14.0)).color(t.text));
                            } else if l.starts_with("# ") || l.is_empty() {
                            } else if let Some(b) = l.strip_prefix("- ") {
                                ui.label(egui::RichText::new(format!("•  {}", crate::i18n::tr(b).replace('`', ""))).color(t.text_label));
                            } else {
                                ui.label(egui::RichText::new(crate::i18n::tr(l.trim()).replace('`', "")).color(t.text_label));
                            }
                        }
                    });
                }
                Dialog::DenoiseModel { info, accepted } => crate::panels::denoise::model_dialog(app, ui, &t, info, accepted),
                Dialog::FaceModel { info, accepted, .. } => crate::panels::faces::model_dialog(app, ui, &t, info, accepted),
                Dialog::LightroomResult { summary, library, details, capture_one, .. } => {
                    ui.label(summary.as_str());
                    ui.add_space(8.0);
                    if let Some(library) = library {
                        ui.label(crate::i18n::tr("Destination catalog"));
                        ui.add(egui::Label::new(library.as_str()).wrap().selectable(true));
                        ui.label(crate::i18n::tr(if *capture_one {
                            "Capture One catalogs import into the open catalog; no new catalog folder is created."
                        } else {
                            "Lightroom catalogs import into the open catalog; no new catalog folder is created."
                        }));
                    } else {
                        ui.label(crate::i18n::tr("These photos are only in memory. Open a catalog on disk and import again to save them."));
                    }
                    for detail in details.iter() {
                        ui.add(egui::Label::new(detail).wrap().selectable(true));
                    }
                }
                Dialog::DngValidation { summary, damaged, unverifiable } => {
                    ui.label(summary.as_str());
                    for (heading, lines) in [(crate::i18n::tr("Damaged files"), damaged), (crate::i18n::tr("Could not be verified"), unverifiable)] {
                        if lines.is_empty() {
                            continue;
                        }
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new(heading).color(t.text_dim));
                        egui::ScrollArea::vertical().id_salt(heading).max_height(160.0).show(ui, |ui| {
                            for line in lines.iter() {
                                ui.add(egui::Label::new(line.as_str()).wrap().selectable(true));
                            }
                        });
                    }
                }
                Dialog::SystemInfo { rows } => {
                    egui::Grid::new("sysinfo").num_columns(2).spacing([16.0, 4.0]).striped(true).show(ui, |ui| {
                        for (k, v) in rows.iter() {
                            crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr(k)).color(t.text_dim));
                            ui.add(egui::Label::new(egui::RichText::new(v).color(t.text_label)).wrap());
                            ui.end_row();
                        }
                    });
                    if ui.button(crate::i18n::tr("Copy to Clipboard")).clicked() {
                        let text: String = rows.iter().map(|(k, v)| format!("{k}: {v}\n")).collect();
                        ui.ctx().copy_text(text);
                    }
                }
                Dialog::AllMetadata { title, rows, search } => {
                    ui.label(egui::RichText::new(title.as_str()).color(t.text_label));
                    let r = crate::widgets::named(ui.add(egui::TextEdit::singleline(search).hint_text(crate::i18n::tr("Filter fields")).desired_width(f32::INFINITY)), crate::i18n::tr("Filter fields"));
                    crate::widgets::register(ui.ctx(), "field:metadataSearch", r.rect);
                    let q = search.trim().to_lowercase();
                    let keep = |n: &str, v: &str| q.is_empty() || n.to_lowercase().contains(&q) || v.to_lowercase().contains(&q);
                    let mut groups: Vec<(String, Vec<(String, String)>)> = Vec::new();
                    for row in rows["exif"].as_array().into_iter().flatten() {
                        let (g, n, v) = (row["group"].as_str().unwrap_or(""), row["name"].as_str().unwrap_or(""), row["value"].as_str().unwrap_or(""));
                        if !keep(n, v) {
                            continue;
                        }
                        match groups.iter_mut().find(|(x, _)| x == g) {
                            Some((_, list)) => list.push((n.into(), v.into())),
                            None => groups.push((g.into(), vec![(n.into(), v.into())])),
                        }
                    }
                    let xmp: Vec<(String, String)> = rows["xmp"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|r| (r["name"].as_str().unwrap_or("").to_string(), r["value"].as_str().unwrap_or("").to_string()))
                        .filter(|(n, v)| keep(n, v))
                        .collect();
                    if !xmp.is_empty() {
                        groups.push(("XMP".into(), xmp));
                    }
                    ui.scope(|ui| {
                        if groups.is_empty() {
                            crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr(rows["note"].as_str().unwrap_or("No metadata found"))).color(t.text_dim));
                        }
                        for (g, list) in &groups {
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new(g).font(t.semibold(12.5)).color(t.text));
                            egui::Grid::new(format!("meta-{g}")).num_columns(2).spacing([16.0, 3.0]).striped(true).show(ui, |ui| {
                                for (n, v) in list {
                                    crate::dialog_frame::note(ui, egui::RichText::new(n).color(t.text_dim));
                                    ui.add(egui::Label::new(egui::RichText::new(v).color(t.text_label)).wrap());
                                    ui.end_row();
                                }
                            });
                        }
                    });
                }
                Dialog::SmartRules { id, name, rules, .. } => {
                    let r = crate::widgets::named(ui.add(egui::TextEdit::singleline(name).hint_text(crate::i18n::tr("Name")).desired_width(f32::INFINITY)), crate::i18n::tr("Name"));
                    crate::widgets::register(ui.ctx(), "field:smartName", r.rect);
                    ui.add_space(6.0);
                    // the folder an album made from a folder view carries is not in the editor, but it counts
                    let folder = id.and_then(|id| app.session.catalog.album(lcc_catalog::AlbumId(id))).and_then(|a| a.smart.as_deref().and_then(|f| f.library_folder.clone()));
                    // problems, count and albums: cached until the catalog or the rules change (not per frame)
                    let now = (app.session.clock)();
                    let view = app.caches.rules_view(&app.session.catalog, rules, *id, folder, &now);
                    let env = crate::panels::rules_editor::Env { problems: view.problems.clone(), today: now, albums: view.albums.clone() };
                    egui::ScrollArea::vertical().max_height(list_height(ui, 360.0)).auto_shrink([false, true]).show(ui, |ui| {
                        crate::panels::rules_editor::edit(ui, rules, "rules", 0, &[], &env);
                    });
                    let n = view.count;
                    ui.add_space(4.0);
                    if view.problems.is_empty() {
                        crate::dialog_frame::note(ui,
                            egui::RichText::new(crate::i18n::tr_format!("{n} photo{} match · updates automatically as photos change", if n == 1 { "" } else { "s" }, n = n))
                                .color(t.text_dim),
                        );
                    } else {
                        ui.label(egui::RichText::new(crate::i18n::tr("Fix the marked rules to save this album.")).color(t.caution));
                    }
                }
                Dialog::NewSmartAlbum { name, .. } => {
                    if dialog_field(ui, "field:smartAlbumName", name, "Name") {
                        confirm = true;
                    }
                    let rules = app.session.view_rules();
                    // counted when the library or the rules change (or, for "in the last…" rules, the minute), not every frame
                    let now = (app.session.clock)();
                    let minute = if rules.depends_on_now() { now.get(..16).unwrap_or(&now) } else { "" };
                    let n = app.caches.dialog_count(&app.session.catalog, ("smartAlbum", format!("{rules:?}"), minute), || {
                        lcc_catalog::rules::set_now(Some(now.clone()));
                        app.session.catalog.query(&rules, &Default::default()).len()
                    });
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("Matches: {}", crate::i18n::filter_label(&rules, &app.session.catalog))).color(t.text_label));
                    crate::dialog_frame::note(ui,
                        egui::RichText::new(crate::i18n::tr_format!("{n} photo{} now · updates automatically as photos change", if n == 1 { "" } else { "s" }, n = n))
                            .color(t.text_dim),
                    );
                }
                Dialog::CaptureTime { mode, time, days, hours, minutes, zone, each } => {
                    let n = app.session.selected().len();
                    field(ui, "Change", |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for (i, (label, m)) in [("Set date & time", "set"), ("Shift by", "shift"), ("Time zone", "zone"), ("File date", "file"), ("File name", "name")].iter().enumerate() {
                            if crate::widgets::text_button(ui, &format!("captureMode-{i}"), label, mode == m).clicked() {
                                *mode = m.to_string();
                            }
                        }
                    });
                    let params = match mode.as_str() {
                        "set" => {
                            field(ui, "New time", |ui| {
                                let r = ui.add(egui::TextEdit::singleline(time).hint_text("2026-09-30 14:05:00 · 1983-04 · 1983").desired_width(f32::INFINITY));
                                crate::widgets::register(ui.ctx(), "field:captureTime", r.rect);
                            });
                            // several photos: give them all this time, rather than move them by the active photo's change
                            if n > 1 {
                                let r = ui.checkbox(each, crate::i18n::tr("Give every selected photo this time"));
                                crate::widgets::register(ui.ctx(), "captureTimeEach", r.rect);
                            }
                            json!({"time": time, "each": *each})
                        }
                        "shift" => {
                            field(ui, "Shift", |ui| {
                                ui.add(egui::DragValue::new(days).suffix(" d"));
                                ui.add(egui::DragValue::new(hours).suffix(" h"));
                                ui.add(egui::DragValue::new(minutes).suffix(" min"));
                            });
                            json!({"shift": *days as i64 * 86_400 + *hours as i64 * 3600 + *minutes as i64 * 60})
                        }
                        "file" => {
                            ui.label(
                                egui::RichText::new(crate::i18n::tr("Each photo gets its original file's modification date (UTC)."))
                                    .color(t.text_label),
                            );
                            json!({"fromFile": true})
                        }
                        "name" => {
                            ui.label(
                                egui::RichText::new(crate::i18n::tr("Each photo gets the date and time in its file name (IMG_20260921_143000)."))
                                    .color(t.text_label),
                            );
                            json!({"fromFilename": true})
                        }
                        _ => {
                            field(ui, "Hours", |ui| ui.add(egui::DragValue::new(zone).range(-26.0..=26.0).speed(0.25).fixed_decimals(2)));
                            json!({"hours": zone})
                        }
                    };
                    // preview on the active photo (the others move by the same amount)
                    let active = app.session.active().and_then(|id| app.session.catalog.photo(id)).map(|p| (p.file_name.clone(), p.date().to_string()));
                    if let Some((name, cur)) = active {
                        let delta = match mode.as_str() {
                            "set" => lcc_catalog::dates::normalize_iso(time)
                                .and_then(|t| Some(lcc_catalog::dates::iso_seconds(&t)? - lcc_catalog::dates::iso_seconds(&cur)?)),
                            "shift" => params["shift"].as_i64(),
                            "file" | "name" => None,
                            _ => Some((*zone as f64 * 3600.0).round() as i64),
                        };
                        // the photo's own time is the problem (a Lightroom catalog's capture time
                        // is copied verbatim), not what was typed
                        let unreadable = lcc_catalog::dates::shift_iso(&cur, 0).is_none();
                        // a year or a month alone (`1983`, `1983-04`) is kept as typed, by every selected photo
                        let partial = |d: &str| lcc_catalog::dates::partial_capture(d.trim()).is_some();
                        let after = match (mode.as_str(), unreadable && n == 1) {
                            ("name", _) => lcc_catalog::dates::date_from_filename(&name),
                            ("set", _) if partial(time) => Some(time.trim().to_string()),
                            ("set", _) if *each && n > 1 => lcc_catalog::dates::normalize_iso(time),
                            // alone, it simply gets the time typed (photo.setCaptureTime repairs it)
                            ("set", true) => lcc_catalog::dates::normalize_iso(time),
                            _ => delta.and_then(|d| lcc_catalog::dates::shift_iso(&cur, d)),
                        };
                        let show = |d: &str| if partial(d) { crate::i18n::display_time(d.trim()) } else { d.replace('T', " ") };
                        ui.label(
                            egui::RichText::new(match after {
                                Some(a) => format!("{name}: {} → {}", show(&cur), show(&a)),
                                None if mode == "file" => String::new(),
                                None if mode == "name" => crate::i18n::tr("No date in the active photo's file name").to_string(),
                                // it reads, but has no time of day to shift or to move the others by
                                None if partial(&cur) && !(mode == "set" && n == 1) => {
                                    crate::i18n::tr("The active photo's capture time is a year or month alone, with no time to shift").to_string()
                                }
                                None if unreadable && !(mode == "set" && n == 1) => crate::i18n::tr("The active photo's capture time can't be read").to_string(),
                                None => "Enter a date as YYYY-MM-DD HH:MM:SS, YYYY-MM or YYYY".into(),
                            })
                            .color(t.text_label),
                        );
                    }
                    if n > 1 && mode == "set" && lcc_catalog::dates::partial_capture(time.trim()).is_some() {
                        crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr_format!("All {n} selected photos get this date.", n = n)).color(t.text_dim));
                    } else if n > 1 && mode == "set" && *each {
                        crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr_format!("All {n} selected photos get this time.", n = n)).color(t.text_dim));
                    } else if n > 1 && !matches!(mode.as_str(), "file" | "name") {
                        crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr_format!("All {n} selected photos move by the same amount.", n = n)).color(t.text_dim));
                    }
                }
                Dialog::LabelNames { names, save_as } => {
                    names.resize(5, String::new());
                    // start from a set
                    let sets = lcc_engine::cmd::manage::label_sets_json(&app.session);
                    field(ui, "Set", |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for set in sets["sets"].as_array().into_iter().flatten() {
                            let name = set["name"].as_str().unwrap_or_default();
                            let shown = crate::i18n::builtin_label(name, set["builtin"].as_bool() == Some(true));
                            if crate::widgets::text_button(ui, &format!("labelSet-{name}"), shown, false).clicked() {
                                *names = set["names"].as_array().into_iter().flatten().map(|n| n.as_str().unwrap_or_default().to_string()).collect();
                            }
                        }
                    });
                    for (i, l) in lcc_catalog::ColorLabel::ALL.iter().enumerate() {
                        let colour = format!("{l:?}");
                        field(ui, &colour, |ui| {
                            let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            ui.painter().circle_filled(r.center(), 6.0, crate::panels::grid::label_color(*l));
                            let te = crate::widgets::named(ui.add(egui::TextEdit::singleline(&mut names[i]).hint_text(crate::i18n::tr(&colour)).desired_width(f32::INFINITY)), crate::i18n::tr(&colour));
                            crate::widgets::register(ui.ctx(), format!("field:labelName-{}", colour.to_lowercase()), te.rect);
                        });
                    }
                    field(ui, "Save as set", |ui| {
                        let te = crate::widgets::named(ui.add(egui::TextEdit::singleline(save_as).hint_text(crate::i18n::tr("Optional name")).desired_width(f32::INFINITY)), crate::i18n::tr("Save as set"));
                        crate::widgets::register(ui.ctx(), "field:labelSetName", te.rect);
                    });
                    crate::dialog_frame::note(ui,
                        egui::RichText::new(crate::i18n::tr("Names appear in the label menu, the filter bar and the Info panel, and are written to XMP. Empty = the colour's name."))
                            .color(t.text_dim),
                    );
                }
                Dialog::RenamePhoto { name, .. } => {
                    if dialog_field(ui, "field:photoName", name, "File name") {
                        confirm = true;
                    }
                }
                Dialog::Rename { template, start, texts } => {
                    let n = app.session.selected().len();
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr_format!("{n} photo{}", if n == 1 { "" } else { "s" }, n = n)).color(t.text_dim));
                    let template_id = egui::Id::new("rename-template");
                    let tags_open = field(ui, "Template", |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let w = crate::import::tag_field_width(ui);
                        let r = crate::widgets::named(ui.add(egui::TextEdit::singleline(template).id(template_id).hint_text("{name}").desired_width(w)), crate::i18n::tr("Template"));
                        crate::widgets::register(ui.ctx(), "field:renameTemplate", r.rect);
                        crate::import::tag_toggle(ui, "renameTemplate")
                    });
                    if tags_open {
                        crate::import::tag_help(ui, "renameTemplate", template, template_id);
                    }
                    crate::import::unknown_tags_warning(ui, template);
                    naming_presets_row(app, ui, "rename", template);
                    text_tag_fields(ui, "rename", template, texts);
                    field(ui, "Start at", |ui| ui.add(egui::DragValue::new(start).range(0..=999_999)));
                    ui.label(
                        egui::RichText::new(crate::i18n::tr("Tags: see Tags beside the template. Files are renamed on disk (with their XMP sidecars); existing names get -1, -2…"))
                            .color(t.text_dim),
                    );
                    let filled = lcc_engine::rename::fill_texts(template, &|l| texts.get(l).cloned());
                    let (preview, total) = rename_preview(app, ui.ctx(), &filled, *start as usize);
                    match &preview {
                        Some(rows) => {
                            egui::Grid::new("rename-preview").num_columns(3).spacing([8.0, 2.0]).show(ui, |ui| {
                                for pl in rows {
                                    crate::dialog_frame::note(ui, egui::RichText::new(&pl.from).color(t.text_dim));
                                    let (arrow, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                                    crate::icons::paint(ui.painter(), arrow, crate::icons::Icon::ChevronRight, t.text_dim);
                                    ui.label(egui::RichText::new(&pl.to).color(t.text));
                                    ui.end_row();
                                }
                            });
                        }
                        None => {
                            ui.horizontal(|ui| {
                                crate::progress::spinner(ui, 12.0);
                                crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("Checking names…")).color(t.text_dim));
                            });
                        }
                    }
                    let more = total.saturating_sub(RENAME_PREVIEW_ROWS);
                    if more > 0 {
                        crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr_format!("… and {more} more", more = more)).color(t.text_dim));
                    }
                }
                Dialog::RenamePerson { from, to, merge } => {
                    let submitted = dialog_field(ui, "field:personName", to, "Name");
                    let plan = person_rename_plan(&app.session.catalog, from, to);
                    if !plan.needs_merge {
                        *merge = false;
                    }
                    if submitted && plan.block.is_none() && (!plan.needs_merge || *merge) {
                        confirm = true;
                    }
                    if let Some(why) = plan.block {
                        ui.label(egui::RichText::new(crate::i18n::tr(why)).color(t.caution));
                    } else {
                        ui.label(
                            egui::RichText::new(crate::i18n::tr_format!(
                                "Renames {n} face{} across the catalog. The files are not changed.",
                                if plan.faces == 1 { "" } else { "s" },
                                n = plan.faces
                            ))
                            .color(t.text_dim),
                        );
                    }
                    if plan.needs_merge {
                        ui.label(
                            egui::RichText::new(crate::i18n::tr_format!("“{to}” is already a person. Combining them makes one person; Undo separates them again.", to = to.trim()))
                                .color(t.caution),
                        );
                        let r = crate::dialog_frame::checkbox(ui, merge, crate::i18n::tr("Combine with the existing person"));
                        crate::widgets::register(ui.ctx(), "check:personMerge", r.rect);
                    }
                }
                Dialog::RenameKeyword { from, to } => {
                    let n = app.caches.dialog_count(&app.session.catalog, ("renameKeyword", from.as_str()), || {
                        app.session.catalog.photos().filter(|p| p.meta.keywords.iter().any(|k| lcc_catalog::keywords::is_under(k, from))).count()
                    });
                    if dialog_field(ui, "field:keywordName", to, "New name") {
                        confirm = true;
                    }
                    crate::dialog_frame::note(ui,
                        egui::RichText::new(crate::i18n::tr_format!("Renames “{from}” on {n} photo{} (keywords below it too). Use | for levels, e.g. Travel|Italy. An existing name merges the two.",
                            if n == 1 { "" } else { "s" }, from = from, n = n))
                        .color(t.text_dim),
                    );
                }
                Dialog::MergeKeywords { from, into } => {
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("Replace {} with:", from.iter().map(|f| format!("“{f}”")).collect::<Vec<_>>().join(", "))).color(t.text_label));
                    if dialog_field(ui, "field:keywordInto", into, "Keyword") {
                        confirm = true;
                    }
                    let options: Vec<String> = app
                        .caches
                        .suggestions(&app.session.catalog, from, into, 12)
                        .iter()
                        .filter(|k| !from.iter().any(|f| lcc_catalog::keywords::is_under(k, f)))
                        .cloned()
                        .collect();
                    ui.horizontal_wrapped(|ui| {
                        for k in options {
                            if ui.add(egui::Button::new(egui::RichText::new(&k).color(t.text_label)).corner_radius(10.0)).clicked() {
                                *into = k;
                            }
                        }
                    });
                }
                Dialog::TextPrompt { value, hint, .. } => {
                    if dialog_field(ui, "field:prompt", value, hint) {
                        confirm = true;
                    }
                }
                Dialog::NewAlbum { name, folder, include_selected, .. } => {
                    if dialog_field(ui, "field:albumName", name, "Name") {
                        confirm = true;
                    }
                    // as in Lightroom, the option is only there when something is selected
                    if !*folder && !app.session.selected().is_empty() {
                        ui.add_space(6.0);
                        let r = crate::dialog_frame::checkbox(ui, include_selected, crate::i18n::tr("Include selected photos"));
                        crate::widgets::register(ui.ctx(), "check:albumIncludeSelected", r.rect);
                    }
                }
                Dialog::RenameAlbum { name, .. } => {
                    if dialog_field(ui, "field:albumName", name, "Name") {
                        confirm = true;
                    }
                }
                Dialog::CreatePreset { name, group, groups } => {
                    field(ui, "Preset name", |ui| {
                        crate::widgets::named(ui.add(egui::TextEdit::singleline(name).hint_text(crate::i18n::tr("Preset name")).desired_width(f32::INFINITY)), crate::i18n::tr("Preset name"));
                    });
                    // the user's groups in a list, and New Group… for a name typed in (LR-PRE-CREATE)
                    let mut known_groups: Vec<String> =
                        app.session.presets.iter().filter(|p| !p.builtin).map(|p| p.group.clone()).chain(app.session.preset_folders.iter().cloned()).collect();
                    if !known_groups.iter().any(|g| g == lcc_engine::app_store::DEFAULT_PRESET_GROUP) {
                        known_groups.push(lcc_engine::app_store::DEFAULT_PRESET_GROUP.to_string());
                    }
                    known_groups.sort_by_key(|g| g.to_lowercase());
                    known_groups.dedup();
                    // (an empty or new name is typed; one of the list's is picked from it)
                    let typing = !known_groups.iter().any(|g| g == group.trim());
                    field(ui, "Group", |ui| {
                        if typing {
                            let r = ui.add(egui::TextEdit::singleline(group).hint_text(crate::i18n::tr("New group name")).desired_width(f32::INFINITY));
                            crate::widgets::register(ui.ctx(), "field:presetNewGroup", r.rect);
                            crate::widgets::named(r, crate::i18n::tr("Group"));
                        } else {
                            let combo = egui::ComboBox::from_id_salt("presetGroupCombo").selected_text(group.as_str()).width(ui.available_width()).show_ui(ui, |ui| {
                                for g in &known_groups {
                                    ui.selectable_value(group, g.clone(), g.as_str());
                                }
                                ui.separator();
                                if ui.selectable_label(false, crate::i18n::tr("New Group…")).clicked() {
                                    group.clear();
                                }
                            });
                            crate::widgets::register(ui.ctx(), "combo:presetGroup", combo.response.rect);
                        }
                    });
                    let typed = group.trim().to_string();
                    let mut known = app.session.presets.iter().map(|p| p.group.clone()).chain(app.session.preset_folders.iter().cloned());
                    if let Some(same) = known.find(|g| g.trim().eq_ignore_ascii_case(&typed) && *g != typed) {
                        crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr_format!("Goes into the group “{group}” that exists already.", group = same)).small().color(t.text_dim));
                    }
                    crate::dialog_frame::group_begin(ui, Some(crate::i18n::tr("Settings to include")));
                    let _ = group_checklist(ui, "presetInclude", groups, None);
                }
                Dialog::UpdatePreset { name, groups, .. } => {
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr_format!("“{name}” takes the active photo's settings. Its name, group, place and marks stay.", name = name)).color(t.text_dim));
                    crate::dialog_frame::group_begin(ui, Some(crate::i18n::tr("Settings to keep in the preset")));
                    let _ = group_checklist(ui, "presetUpdate", groups, None);
                }
                Dialog::CreatePresetFolder { name, parent } => {
                    if let Some(par) = parent.as_ref() {
                        field(ui, "Parent folder", |ui| {
                            ui.label(par.as_str());
                        });
                    }
                    field(ui, "Folder name", |ui| {
                        let r = ui.add(egui::TextEdit::singleline(name).hint_text(crate::i18n::tr("Folder name")).desired_width(f32::INFINITY));
                        crate::widgets::named(r, crate::i18n::tr("Folder name"));
                    });
                }
                Dialog::CopySettings { groups } => {
                    let _ = group_checklist(ui, "copyGroup", groups, None);
                }
                Dialog::SyncSettings { groups, controls } => {
                    let n = app.session.selection.ids.len().saturating_sub(1);
                    crate::dialog_frame::note(ui,
                        egui::RichText::new(crate::i18n::tr_format!("The checked settings of the active photo are copied to {n} other selected photo(s)", n = n))
                            .color(t.text_dim),
                    );
                    // (single controls are ticks of the tree now)
                    controls.clear();
                    let _ = group_checklist(ui, "syncGroup", groups, None);
                }
                Dialog::SyncCopies { groups } => {
                    crate::dialog_frame::note(
                        ui,
                        egui::RichText::new(crate::i18n::tr("The checked settings of the active photo are copied to its master photo and virtual copies"))
                            .color(t.text_dim),
                    );
                    let _ = group_checklist(ui, "syncCopiesGroup", groups, None);
                }
                Dialog::SyncSnapshots { groups } => {
                    crate::dialog_frame::note(
                        ui,
                        egui::RichText::new(crate::i18n::tr("The checked settings of the current edit are applied to each snapshot of the photo"))
                            .color(t.text_dim),
                    );
                    let _ = group_checklist(ui, "syncSnapshotsGroup", groups, None);
                }
                Dialog::PasteSettings { groups } => {
                    let n = app.session.selected().len();
                    crate::dialog_frame::note(ui,
                        egui::RichText::new(crate::i18n::tr_format!("Paste into {n} photo{}. Only settings that were copied are pasted", if n == 1 { "" } else { "s" }, n = n))
                            .color(t.text_dim),
                    );
                    let copied = app.session.dialogs.copy.clone();
                    let _ = group_checklist(ui, "pasteGroup", groups, Some(&copied));
                }
                Dialog::SlideshowExport { settings } => slideshow_export_body(app, ui, settings),
                Dialog::CropOverlays { picked } => crop_overlays_body(ui, picked),
                Dialog::BookPrefs { prefs } => book_prefs_body(ui, prefs),
                Dialog::ContactSheet { options } => {
                    options.columns = options.columns.clamp(1, 8);
                    options.rows = options.rows.clamp(1, 10);
                    let n = app.session.selected().len();
                    ui.label(crate::i18n::tr_format!("{n} selected photos · {pages} pages", n = n, pages = n.div_ceil(options.columns * options.rows)));
                    field(ui, "Paper", |ui| {
                        crate::widgets::combo_box("contactSheetPaper").selected_text(&options.paper).show_ui(ui, |ui| {
                            ui.selectable_value(&mut options.paper, "a4".into(), "A4");
                            ui.selectable_value(&mut options.paper, "letter".into(), crate::i18n::tr("Letter"));
                        });
                    });
                    crate::dialog_frame::checkbox(ui, &mut options.landscape, crate::i18n::tr("Landscape"));
                    field(ui, "Columns", |ui| { ui.add(egui::DragValue::new(&mut options.columns).range(1..=8)); });
                    field(ui, "Rows", |ui| { ui.add(egui::DragValue::new(&mut options.rows).range(1..=10)); });
                    crate::dialog_frame::checkbox(ui, &mut options.captions, crate::i18n::tr("Filename captions"));
                    ui.label(crate::i18n::tr("150 dpi · sRGB · current edits · fit without cropping"));
                    ui.label(crate::i18n::tr("Choose a PDF destination after confirming. Print the PDF from your viewer."));
                }
                Dialog::Timelapse { fps, size, fill, hold, crossfade, prores, quality } => {
                    let n = app.session.selected().len();
                    let per = u64::from(*hold);
                    let frames = (n as u64 * per + (n as u64).saturating_sub(1) * u64::from(*crossfade)).max(1);
                    ui.label(
                        egui::RichText::new(crate::i18n::tr_format!(
                            "{n} photos · {frames} frames · {secs} s",
                            n = n,
                            frames = frames,
                            secs = format!("{:.1}", frames as f64 / f64::from((*fps).max(1)))
                        ))
                        .color(t.text_dim),
                    );
                    field(ui, "Size", |ui| {
                        let shown = crate::timelapse_ui::SIZES.iter().find(|s| s.0 == size.as_str()).map_or(size.as_str(), |s| s.1).to_string();
                        let c = egui::ComboBox::from_id_salt("timelapseSize").selected_text(shown).show_ui(ui, |ui| {
                            for (v, l) in crate::timelapse_ui::SIZES {
                                ui.selectable_value(size, v.to_string(), *l);
                            }
                        });
                        crate::widgets::register(ui.ctx(), "combo:timelapseSize", c.response.rect);
                    });
                    choices(ui, "time-lapse|Frame", "timelapseFit", &[(false, "Fit (letterbox)"), (true, "Fill (crop)")], fill);
                    choices(ui, "Frame Rate", "timelapseFps", &[(24, "24"), (25, "25"), (30, "30"), (50, "50"), (60, "60")], fps);
                    field(ui, "Frames per Photo", |ui| { ui.add(egui::DragValue::new(hold).range(1..=lcc_engine::timelapse::MAX_HOLD)); });
                    field(ui, "Cross-Fade Frames", |ui| { ui.add(egui::DragValue::new(crossfade).range(0..=lcc_engine::timelapse::MAX_HOLD)); });
                    choices(ui, "Format", "timelapseFormat", &[(false, "H.264 (MP4)"), (true, "ProRes 422 (MOV)")], prores);
                    field(ui, "Quality", |ui| { ui.add(egui::Slider::new(quality, 1..=100)); });
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("Each photo with its own edits, in the order shown. No audio.")).color(t.text_dim));
                }
                Dialog::EmailPhoto { long_edge, quality } => {
                    let n = app.session.selected().len();
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("{n} photos", n = n)).color(t.text_dim));
                    let edges: Vec<(u32, String)> = crate::email_photo::EDGES.iter().map(|e| (*e, e.to_string())).collect();
                    let options: Vec<(u32, &str)> = edges.iter().map(|(v, l)| (*v, l.as_str())).collect();
                    choices(ui, "Long Edge (pixels)", "emailEdge", &options, long_edge);
                    field(ui, "Quality", |ui| { ui.add(egui::Slider::new(quality, 10..=100)); });
                    crate::dialog_frame::note(
                        ui,
                        egui::RichText::new(crate::i18n::tr("JPEG copies with their edits, sRGB, without the location; then a new message opens in your mail program."))
                            .color(t.text_dim),
                    );
                }
                Dialog::Export { opts, full_size, resize, preset_name, limit_kb, dir } => {
                    export_body(app, ui, ExportForm { opts, full_size, resize, preset_name, limit_kb, dir }, window_id, settings_height)
                }
                Dialog::Merge { opts } => crate::merge::body(app, ui, opts),
                Dialog::Enhance { opts } => crate::enhance_ui::body(app, ui, opts),
                Dialog::Import { opts } => crate::import::body(app, ui, opts),
                Dialog::Settings { tab } => crate::panels::settings::body(app, ui, tab, 0.0),
                Dialog::ChartProfile { opts } => crate::chart_profile::body(app, ui, opts),
                Dialog::SamModel { error, .. } => sam_model_body(app, ui, error.as_deref()),
                Dialog::AiModels { error } => crate::panels::ai_models::body(app, ui, error),
                Dialog::GenfillConsent { provider, index, prompt, variations } => crate::panels::genfill::consent_body(app, ui, provider, *index, prompt, *variations),
                Dialog::KeywordTag {
                    editing,
                    name,
                    parent,
                    inside,
                    synonyms,
                    include_on_export,
                    export_containing,
                    export_synonyms,
                    person,
                    add_to_selected,
                    ..
                } => {
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("Keyword Name")).color(t.text_dim));
                    // several fields: the name takes the focus when the dialog opens, not after
                    let r = crate::text_field::TextField::singleline("field:keywordTagName", name)
                        .hint(crate::i18n::tr("Keyword Name"))
                        .width(f32::INFINITY)
                        .select_on_focus(true)
                        .show(ui);
                    if just_opened(ui, "keyword-tag") {
                        r.response.request_focus();
                    }
                    if r.ending == Some(crate::text_field::Ending::Return) {
                        confirm = true;
                    }
                    ui.add_space(4.0);
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("Synonyms")).color(t.text_dim));
                    // Return in either field creates (or saves) the keyword
                    let r = crate::text_field::TextField::singleline("field:keywordSynonyms", synonyms)
                        .hint(crate::i18n::tr("Separated by commas"))
                        .width(f32::INFINITY)
                        .show(ui);
                    if r.ending == Some(crate::text_field::Ending::Return) {
                        confirm = true;
                    }
                    ui.add_space(6.0);
                    if editing.is_none() {
                        if let Some(p) = parent {
                            let r = crate::dialog_frame::checkbox(ui, inside, crate::i18n::tr_format!("Put inside “{parent}”", parent = p.replace('|', " › ")));
                            crate::widgets::register(ui.ctx(), "check:keywordInside", r.rect);
                        }
                        let n = app.session.selection.ids.len();
                        let r = ui.add_enabled(n > 0, egui::Checkbox::new(add_to_selected, crate::i18n::tr("Add to Selected Photos")));
                        crate::widgets::register(ui.ctx(), "check:keywordAddToSelected", r.rect);
                        ui.add_space(4.0);
                    }
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("Keyword Tag Options")).color(t.text_dim));
                    for (on, label, id) in [
                        (include_on_export, "Include on Export", "check:keywordIncludeOnExport"),
                        (export_containing, "Export Containing Keywords", "check:keywordExportContaining"),
                        (export_synonyms, "Export Synonyms", "check:keywordExportSynonyms"),
                        (person, "Person", "check:keywordPerson"),
                    ] {
                        let r = crate::dialog_frame::checkbox(ui, on, crate::i18n::tr(label));
                        crate::widgets::register(ui.ctx(), id, r.rect);
                    }
                }
                Dialog::KeywordSet { replaces, name, slots, as_new } => {
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("Set name")).color(t.text_dim));
                    let r = crate::text_field::TextField::singleline("field:keywordSetName", name)
                        .hint(crate::i18n::tr("Set name"))
                        .width(f32::INFINITY)
                        .select_on_focus(true)
                        .show(ui);
                    if just_opened(ui, "keyword-set") {
                        r.response.request_focus();
                    }
                    let mut returned = r.ending == Some(crate::text_field::Ending::Return);
                    ui.add_space(6.0);
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("Keywords (⌥1–⌥9)")).color(t.text_dim));
                    slots.resize(9, String::new());
                    // equal columns of a set width (the window is still finding its own)
                    let w = 130.0;
                    egui::Grid::new("keyword-set-slots").num_columns(3).min_col_width(w).spacing([6.0, 4.0]).show(ui, |ui| {
                        for (i, slot) in slots.iter_mut().enumerate() {
                            let r = crate::text_field::TextField::singleline(&format!("field:keywordSetSlot:{}", i + 1), slot)
                                .hint(format!("⌥{}", i + 1))
                                .width(w)
                                .show(ui);
                            returned |= r.ending == Some(crate::text_field::Ending::Return);
                            if i % 3 == 2 {
                                ui.end_row();
                            }
                        }
                    });
                    if replaces.is_some() {
                        ui.add_space(6.0);
                        let r = crate::dialog_frame::checkbox(ui, as_new, crate::i18n::tr("Save as a new set"));
                        crate::widgets::register(ui.ctx(), "check:keywordSetAsNew", r.rect);
                    }
                    // Return in any field saves the set
                    if returned {
                        confirm = true;
                    }
                }
                Dialog::MoveKeyword { keyword, parent } => {
                    let name = keyword.rsplit('|').next().unwrap_or(keyword);
                    match parent {
                        Some(p) => ui.label(crate::i18n::tr_format!(
                            "“{parent}” has a “{name}” already. Merge the two?",
                            parent = p.replace('|', " › "),
                            name = name
                        )),
                        None => ui.label(crate::i18n::tr_format!("There is a “{name}” at the top level already. Merge the two?", name = name)),
                    };
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("Their photos and the keywords below them come together under one keyword.")).color(t.text_dim));
                }
                Dialog::DeleteKeyword { keyword, count } => {
                    let name = keyword.replace('|', " › ");
                    ui.label(crate::i18n::tr_format!("Delete the keyword “{name}”?", name = name));
                    ui.label(
                        egui::RichText::new(if *count == 0 {
                            crate::i18n::tr("No photo has it; the keywords below it go too.").to_string()
                        } else {
                            crate::i18n::tr_format!(
                                "It is taken off {count} photo{}, with the keywords below it. Undo brings it back.",
                                if *count == 1 { "" } else { "s" },
                                count = count
                            )
                        })
                        .color(t.text_dim),
                    );
                }
                Dialog::SaveEffect { name, group, fields, amount, brush } => {
                    if crate::panels::mask_effects::save_body(app, ui, name, group, fields, amount, brush) {
                        confirm = true;
                    }
                }
                Dialog::PresetsManager { query, confirm, rename } => crate::panels::presets_manager::body(app, ui, query, confirm, rename),
                Dialog::ProfileManager { query, confirm, rename } => crate::panels::profile_manager::body(app, ui, query, confirm, rename),
                Dialog::ManageEffects { search, selected, rename, confirm_delete, new_group } => {
                    crate::panels::mask_effects::manage_body(app, ui, search, selected, rename, confirm_delete, new_group)
                }
                Dialog::DeleteEffects { names } => {
                    let what = match names.as_slice() {
                        [one] => crate::i18n::tr_format!("Delete the effect preset “{name}”?", name = one),
                        _ => crate::i18n::tr_format!("Delete these {n} effect presets?", n = names.len()),
                    };
                    ui.label(what);
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("Masks keep their sliders. Export presets first to keep a copy.")).color(t.text_dim));
                }
                Dialog::ConfirmDelete { count, disk_files } => {
                    let what = if *count == 1 { crate::i18n::tr("this photo").to_string() } else { crate::i18n::tr_format!("these {count} photos", count = count) };
                    if *disk_files > 0 && app.session.trash.is_some() {
                        ui.label(if *count == 1 {
                            crate::i18n::tr("Delete this photo from disk, or only remove it from the catalog?").to_string()
                        } else {
                            crate::i18n::tr_format!("Delete these {count} photos from disk, or only remove them from the catalog?", count = count)
                        });
                        let files = if *disk_files == 1 {
                            crate::i18n::tr("Delete from Disk moves 1 file to the system trash.").to_string()
                        } else {
                            crate::i18n::tr_format!("Delete from Disk moves {n} files to the system trash.", n = disk_files)
                        };
                        crate::dialog_frame::note(ui, egui::RichText::new(files).color(t.text_dim));
                        crate::dialog_frame::note(ui,
                            egui::RichText::new(crate::i18n::tr("Remove keeps the files on disk and moves the photos to Recently Deleted.")).color(t.text_dim),
                        );
                    } else {
                        ui.label(crate::i18n::tr_format!("Move {what} to Recently Deleted?", what = what));
                        crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("They can be restored from Recently Deleted until it is emptied.")).color(t.text_dim));
                    }
                }
                Dialog::ConfirmPurge { count, .. } => {
                    let what = if *count == 1 { crate::i18n::tr("this photo").to_string() } else { crate::i18n::tr_format!("these {count} photos", count = count) };
                    ui.label(crate::i18n::tr_format!("Remove {what} from the catalog for good?", what = what));
                    crate::dialog_frame::note(ui,
                        egui::RichText::new(crate::i18n::tr("Their edits, keywords and collection memberships are removed; the files on disk are kept."))
                            .color(t.text_dim),
                    );
                }
                Dialog::MovePhotos { ids, name, dont_ask, .. } => {
                    let n = ids.len();
                    ui.label(crate::i18n::tr_format!("Move {n} photo{} to “{}”?", if n == 1 { "" } else { "s" }, name, n = n));
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("Their files and XMP sidecars move on disk. Edit → Undo moves them back.")).color(t.text_dim));
                    ui.add_space(4.0);
                    let c = crate::dialog_frame::checkbox(ui, dont_ask, crate::i18n::tr("Don't ask again"));
                    crate::widgets::register(ui.ctx(), "check:moveDontAsk", c.rect);
                    c.on_hover_text(crate::i18n::tr("Settings → Confirm before moving photos to another folder on disk turns it back on"));
                }
                d @ Dialog::SynchronizeFolder { .. } => crate::sync::body(app, ui, d),
                Dialog::LightroomCatalog { folder, catalog } => {
                    ui.label(crate::i18n::tr("This is a Lightroom catalog, not a {app} catalog. Import it into a {app} catalog instead?"));
                    crate::dialog_frame::note(ui, egui::RichText::new(catalog.as_deref().unwrap_or(folder.as_str())).color(t.text_dim));
                    // (the import then asks where the catalog goes, unless Settings says to merge, #2672)
                    let asks = app.ui.settings.lightroom_import == crate::state::LightroomImport::Ask
                        && app.session.library.as_ref().is_some_and(|l| l.on_disk);
                    let what = if asks {
                        "The import reads the catalog without changing it. Next, choose whether its photos, edits, keywords and collections join the catalog open now or go into a new catalog."
                    } else {
                        "The import reads the catalog without changing it and adds its photos, edits, keywords and collections to the catalog open now."
                    };
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr(what)).color(t.text_dim));
                }
                d @ Dialog::LightroomDestination { .. } => crate::lightroom_destination::body(app, ui, d),
                Dialog::StopWaiting { task, warning, .. } => {
                    ui.label(crate::i18n::tr_format!("Stop waiting: {task}?", task = task));
                    crate::dialog_frame::note(ui, egui::RichText::new(warning.as_str()).color(t.text_dim));
                }
                Dialog::VignettingReplace { question, .. } => {
                    ui.label(question.as_str());
                    crate::dialog_frame::note(
                        ui,
                        egui::RichText::new(crate::i18n::tr("Photos that use the old reference keep it.")).color(t.text_dim),
                    );
                }
                Dialog::VignettingCreateAnyway { reasons, .. } => {
                    ui.label(crate::i18n::tr("This photo doesn't look like a flat-field reference. Create the vignetting calibration anyway?"));
                    crate::dialog_frame::note(ui, egui::RichText::new(reasons.as_str()).color(t.text_dim));
                }
                Dialog::DeleteRejected { count, recycle } => {
                    ui.label(crate::i18n::tr_format!("Delete the {n} rejected photos of this view?", n = count));
                    ui.add_space(6.0);
                    let keep = ui.radio_value(recycle, false, crate::i18n::tr("Remove from the catalog (to Recently Deleted; the files stay on disk)"));
                    crate::widgets::register(ui.ctx(), "radio:rejectsKeep", keep.rect);
                    if app.session.trash.is_some() {
                        let bin = ui.radio_value(recycle, true, crate::i18n::tr("Remove, and move the files to the system trash"));
                        crate::widgets::register(ui.ctx(), "radio:rejectsRecycle", bin.rect);
                    }
                    let note = if *recycle {
                        "The files and their own XMP sidecars go to the system trash, where they can be put back; the photos leave the catalog."
                    } else {
                        "Edit ▸ Undo brings them back; Recently Deleted keeps them until it is emptied."
                    };
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr(note)).color(t.text_dim));
                }
                Dialog::RemoveFolder { name, count, path, disk } => {
                    ui.label(crate::i18n::tr_format!(
                        "Remove “{name}” and its {count} photo{} from the catalog?",
                        if *count == 1 { "" } else { "s" },
                        name = name,
                        count = count
                    ));
                    crate::dialog_frame::note(ui, egui::RichText::new(path.as_str()).color(t.text_dim));
                    let scope = if *disk { "All the photos imported from this disk are included." } else { "That includes the photos in the folders inside it." };
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr(scope)).color(t.text_dim));
                    crate::dialog_frame::note(ui,
                        egui::RichText::new(crate::i18n::tr("They move to Recently Deleted and can be restored; no file on disk is touched."))
                            .color(t.text_dim),
                    );
                }
                Dialog::About => {
                    ui.label(egui::RichText::new(lcc_brand::DISPLAY_NAME).font(t.semibold(20.0)).color(t.text));
                    ui.label(crate::i18n::tr_format!("Version {}. A clean-room, pure-Rust photo library and raw developer.", env!("CARGO_PKG_VERSION")));
                    ui.label(crate::i18n::tr_format!(
                        "MIT AND Apache-2.0. Fonts: {} (OFL). Icons: original.",
                        crate::theme::font_credits(app.chinese_font.is_some())
                    ));
                    ui.add_space(10.0);
                    for (id, label, url) in [
                        ("website", "{app} website", crate::links::APP_PAGE),
                        ("source", "Source code", crate::links::GITHUB),
                        ("discord", "Join the Discord", crate::links::DISCORD),
                    ]
                    .into_iter()
                    .filter(|(_, _, url)| !url.is_empty())
                    {
                        let r = ui.link(crate::i18n::tr(label)).on_hover_text(url);
                        crate::widgets::register(ui.ctx(), format!("link:about-{id}"), r.rect);
                        if r.clicked() {
                            let _ = crate::links::open(app, url);
                        }
                    }
                }
                Dialog::Shortcuts => crate::panels::keymap::body(app, ui, &t),
                // Grid View Style (the fork's Library View Options), then the loupe's Info 1 / Info 2 lines
                Dialog::ViewOptions => {
                    crate::panels::cells::view_options(app, ui);
                    ui.add_space(10.0);
                    crate::panels::settings::view_options(app, ui, &t);
                }
                Dialog::NewPhotoDefault { name } => {
                    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("Preset name")).color(t.text_dim));
                    let r = crate::text_field::TextField::singleline("field:newPhotoDefaultName", name)
                        .hint(crate::i18n::tr(lcc_engine::import::NEW_PHOTO_DEFAULT))
                        .width(f32::INFINITY)
                        .select_on_focus(true)
                        .show(ui);
                    if just_opened(ui, "new-photo-default") {
                        r.response.request_focus();
                    }
                    // Return in the field saves, as the other naming dialogs do
                    if r.ending == Some(crate::text_field::Ending::Return) {
                        confirm = true;
                    }
                    ui.add_space(6.0);
                    crate::dialog_frame::note(ui,
                        egui::RichText::new(crate::i18n::tr(
                            "Saves the active photo's Optics settings as this preset; photos already in the catalog keep theirs.",
                        ))
                        .color(t.text_dim),
                    );
                }
                #[cfg(not(target_arch = "wasm32"))]
                Dialog::Update => crate::panels::update::body(app, ui),
                // the updater is native only; the web build has nothing to show here
                #[cfg(target_arch = "wasm32")]
                Dialog::Update => {
                    ui.label(crate::i18n::tr("The web build updates by reloading the page."));
                }
                Dialog::Watermark { wm, preset_name } => {
                    field(ui, "Preset", |ui| {
                        let mut chosen_wm = None;
                        let mut chosen_name = None;
                        let selected_text = if preset_name.is_empty() { crate::i18n::tr("Custom / Unsaved").to_string() } else { preset_name.clone() };
                        let c = egui::ComboBox::from_id_salt("watermarkDialogPreset").width(220.0).selected_text(selected_text).show_ui(ui, |ui| {
                            for (p, b) in app.session.all_export_presets() {
                                let o = lcc_engine::export::ExportOptions::from_json(&p.params);
                                if let Some(w) = o.watermark
                                    && ui.selectable_label(preset_name == &p.name, crate::i18n::builtin_label(&p.name, b)).clicked()
                                {
                                    chosen_wm = Some(w);
                                    chosen_name = Some(p.name);
                                }
                            }
                        });
                        crate::widgets::register(ui.ctx(), "combo:watermarkDialogPreset", c.response.rect);
                        if let (Some(w), Some(n)) = (chosen_wm, chosen_name) {
                            *wm = w;
                            *preset_name = n;
                        }
                    });
                    ui.add_space(4.0);
                    watermark_preview(app, ui, wm, None);
                    ui.add_space(4.0);
                    let mut graphic = !wm.image.is_empty() || ui.data(|d| d.get_temp::<bool>(egui::Id::new("wm-dialog-graphic"))).unwrap_or(false);
                    let before = graphic;
                    choices(ui, "Style", "watermarkDialogStyle", &[(false, "Text"), (true, "Graphic")], &mut graphic);
                    if graphic != before {
                        ui.data_mut(|d| d.insert_temp(egui::Id::new("wm-dialog-graphic"), graphic));
                        if !graphic {
                            wm.image.clear();
                        }
                    }
                    if graphic {
                        field(ui, "Graphic", |ui| {
                            trailing_button_row(ui, |ui| {
                                if app.services.pick_files.is_some()
                                    && crate::widgets::text_button(ui, "watermarkDialogChoose", crate::i18n::tr("Choose…"), false).clicked()
                                    && let Some(f) = app.services.pick_files.as_mut().and_then(|pick| pick().into_iter().next())
                                {
                                    wm.image = f;
                                }
                                crate::widgets::named(ui.add(egui::TextEdit::singleline(&mut wm.image).hint_text("logo.png").desired_width(ui.available_width())), crate::i18n::tr("Image"));
                            });
                        });
                        let mut width = wm.image_width as f64 * 100.0;
                        if num(ui, &WM_IMAGE_WIDTH, &mut width) {
                            wm.image_width = (width / 100.0) as f32;
                        }
                    } else {
                        choices(ui, app.ui.language.tr("Text direction"), "watermarkDialogOrientation", &[(false, app.ui.language.tr("Horizontal text")), (true, app.ui.language.tr("Vertical text"))], &mut wm.vertical);
                        field(ui, "Text", |ui| {
                            crate::widgets::named(ui.add(egui::TextEdit::multiline(&mut wm.text).desired_rows(2).hint_text(crate::i18n::tr("© Your Name")).desired_width(f32::INFINITY)), crate::i18n::tr("Text"))
                        });
                    }
                    choices(ui, "Position", "watermarkDialogAnchor", &WM_POSITIONS, &mut wm.anchor);
                    choices(ui, "Rotate", "watermarkDialogRotation", &[(0u16, "0°"), (90, "90°"), (180, "180°"), (270, "270°")], &mut wm.rotation);
                    let mut size = wm.size as f64 * 100.0;
                    if !graphic && num(ui, &WM_SIZE, &mut size) {
                        wm.size = (size / 100.0) as f32;
                    }
                    let mut op = wm.opacity as f64 * 100.0;
                    if num(ui, &WM_OPACITY, &mut op) {
                        wm.opacity = (op / 100.0) as f32;
                    }
                    use lcc_engine::export::Anchor as P;
                    let across = !matches!(wm.anchor, P::Top | P::Center | P::Bottom);
                    if let Some(v) = crate::widgets::form_slider(ui, &WM_INSET_X, wm.inset_x as f64 * 100.0, across).value {
                        wm.inset_x = (v / 100.0) as f32;
                    }
                    let down = !matches!(wm.anchor, P::Left | P::Center | P::Right);
                    if let Some(v) = crate::widgets::form_slider(ui, &WM_INSET_Y, wm.inset_y as f64 * 100.0, down).value {
                        wm.inset_y = (v / 100.0) as f32;
                    }
                    if !graphic {
                        crate::dialog_frame::checkbox(ui, &mut wm.shadow, crate::i18n::tr("Shadow"));
                    }
                    crate::dialog_frame::group_begin(ui, None);
                    field(ui, "Save preset", |ui| {
                        trailing_button_row(ui, |ui| {
                            let named = !preset_name.trim().is_empty();
                            if crate::widgets::text_button(ui, "watermarkDialogSavePreset", crate::i18n::tr("Save"), false).clicked() && named {
                                let mut params = app.session.last_export.clone().unwrap_or_default();
                                params["watermark"] = serde_json::to_value(&wm).unwrap_or_default();
                                match app.run("export.savePreset", json!({"name": preset_name.trim(), "params": params})) {
                                    Ok(_) => {
                                        app.toast(ui.ctx(), format!("Saved watermark preset “{}”", preset_name.trim()));
                                    }
                                    Err(e) => app.toast(ui.ctx(), e),
                                }
                            }
                            crate::widgets::named(ui.add(egui::TextEdit::singleline(preset_name).hint_text(crate::i18n::tr("Preset name")).desired_width(ui.available_width())), crate::i18n::tr("Preset name"));
                        });
                    });
                }
                Dialog::Print => crate::print_view::body(app, ui, &t),
            };
            // the import review and Print size and scroll themselves
            if import || print || export || presets {
                body(ui);
            } else if formed {
                // labels right-aligned in a column, controls, checkboxes and notes beside them
                // (in one group, unless the dialog starts groups of its own)
                layout.body(ui, |ui| {
                    crate::dialog_frame::form(ui, FORM_LABEL_W, |ui| {
                        crate::dialog_frame::group_begin(ui, None);
                        body(ui)
                    })
                });
            } else {
                layout.body(ui, body);
            }
            layout.footer(ui, |ui| {
            // a rule across the dialog sets the buttons off from its content
            crate::dialog_frame::footer_rule(ui);
            ui.add_space(4.0);
            // the confirm on the right with Cancel to its left (macOS and GNOME's order), filled with
            // the accent when it is the dialog's primary action. The row spans the width the window
            // gives its content this frame (its default width, or its content's if wider), so it
            // reaches the edge without widening the window.
            let row_w = ui.available_width().max(ui.min_rect().width()).max(1.0);
            let row_h = ui.spacing().interact_size.y;
            ui.allocate_ui_with_layout(egui::vec2(row_w, row_h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                // a model file the app cannot use has nothing to confirm, nor has a dialog that
                // only shows something (one Close, no Cancel / OK)
                let unusable_model = matches!(&dlg, Dialog::DenoiseModel { info, .. } | Dialog::FaceModel { info, .. } if info["kind"] == "unsupported");
                let informational = unusable_model || matches!(dlg, Dialog::LightroomResult { first: None, .. }) || only_shows(&dlg);
                let sam = &app.session.segmenter;
                let (sam_installed, sam_running, sam_failed) = (sam.installed(), sam.download_status().running, sam.download_status().error.is_some());
                // no download location in this build: nothing to offer but the manual install
                let sam_nowhere = sam_by_hand(sam);
                let cancel = match &dlg {
                    Dialog::ChartProfile { .. } => "Close",
                    Dialog::SamModel { .. } if sam_running || sam_installed || sam_nowhere => "Close",
                    Dialog::SamModel { .. } => "Not Now",
                    Dialog::LightroomResult { .. } => "Close",
                    _ => "Cancel",
                };
                if let Dialog::ConfirmDelete { disk_files, .. } = &dlg
                    && *disk_files > 0
                    && app.session.trash.is_some()
                {
                    // (on the left of the row: Cancel and the confirm sit on the right)
                    if crate::button::Button::new("Delete from Disk").widget("button:dialogDeleteFromDisk").show(ui).clicked() {
                        if let Err(e) = app.run("photo.deleteFromDisk", json!({"confirm": true})) {
                            app.toast_error(ui.ctx(), e);
                        }
                        close = true;
                    }
                }
                let add_label;
                let ok = match &dlg {
                    Dialog::Import { opts } => {
                        #[cfg(not(target_arch = "wasm32"))]
                        let n = if opts.immich { crate::panels::connections::selected_count(app) } else { opts.selected_paths().len() };
                        #[cfg(target_arch = "wasm32")]
                        let n = opts.selected_paths().len();
                        let verb = if opts.copy && opts.move_files && !opts.immich { "Move" } else { "Import" };
                        add_label = crate::i18n::tr_format!("{verb} {n} Photo{}", if n == 1 { "" } else { "s" }, verb = crate::i18n::tr(verb), n = n);
                        add_label.as_str()
                    }
                    Dialog::LightroomResult { first: Some(_), .. } => "Show in Library",
                    Dialog::ContactSheet { .. } => "Choose PDF…",
                    Dialog::Timelapse { .. } => "Choose File…",
                    Dialog::EmailPhoto { .. } => "Export",
                    Dialog::SlideshowExport { .. } => "Export…",
                    Dialog::Print => "Print to PDF…",
                    Dialog::Merge { .. } => "Merge",
                    Dialog::Enhance { .. } => "Enhance",
                    Dialog::DenoiseModel { info, .. } if info["download"].is_string() => "Accept & Download",
                    Dialog::DenoiseModel { .. } => "Install",
                    Dialog::FaceModel { info, .. } if !informational && info["download"].is_string() => "Download",
                    Dialog::FaceModel { .. } if !informational => "Install",
                    Dialog::ConfirmDelete { disk_files, .. } if *disk_files > 0 && app.session.trash.is_some() => "Remove",
                    Dialog::ConfirmDelete { .. } | Dialog::DeleteKeyword { .. } | Dialog::DeleteEffects { .. } => "Delete",
                    Dialog::ConfirmPurge { all: true, .. } => "Empty",
                    Dialog::ConfirmPurge { .. } => "Delete Permanently",
                    Dialog::SaveEffect { .. } => "Create",
                    Dialog::MovePhotos { .. } => "Move",
                    // (not "Merge": that is photo merging, HDR / panorama, in some languages)
                    Dialog::MoveKeyword { .. } => "Merge Keywords",
                    Dialog::KeywordSet { .. } => "Save",
                    Dialog::NewPhotoDefault { .. } => "Save",
                    Dialog::KeywordTag { editing: None, .. } => "Create",
                    Dialog::KeywordTag { .. } => "Save",
                    Dialog::RemoveFolder { .. } => "Remove",
                    Dialog::DeleteRejected { recycle: true, .. } => "Move to Trash",
                    Dialog::DeleteRejected { .. } => "Remove",
                    Dialog::StopWaiting { .. } => "Stop Waiting",
                    Dialog::VignettingCreateAnyway { .. } => "Create Anyway",
                    Dialog::VignettingReplace { .. } => "Replace",
                    Dialog::LightroomCatalog { .. } => "Import Lightroom Catalog…",
                    d @ Dialog::LightroomDestination { .. } => crate::lightroom_destination::ok_label(d),
                    Dialog::SynchronizeFolder { .. } if informational => "Close",
                    Dialog::SynchronizeFolder { .. } | Dialog::SyncSettings { .. } | Dialog::SyncCopies { .. } | Dialog::SyncSnapshots { .. } => {
                        "Synchronize"
                    }
                    Dialog::ChartProfile { opts } if crate::chart_profile::is_built(app, opts) => "Profile created ✓",
                    Dialog::ChartProfile { .. } => "Create Profile",
                    Dialog::SamModel { then: Some(_), .. } if sam_installed => "Continue",
                    Dialog::SamModel { .. } if sam_installed || sam_running || sam_nowhere => "",
                    Dialog::SamModel { error, .. } if error.is_some() || sam_failed => "Try Again",
                    Dialog::SamModel { .. } => "Download",
                    Dialog::GenfillConsent { .. } => "Generate",
                    Dialog::RenamePerson { from, to, .. } if person_rename_plan(&app.session.catalog, from, to).needs_merge => "Merge",
                    _ if informational => "Close",
                    // the confirm button names its action (upstream #2075)
                    Dialog::Export { .. } => {
                        let n = app.session.selection.ids.len().max(1);
                        add_label = crate::i18n::tr_format!("{verb} {n} Photo{}", if n == 1 { "" } else { "s" }, verb = crate::i18n::tr("Export"), n = n);
                        add_label.as_str()
                    }
                    Dialog::NewAlbum { .. } | Dialog::NewSmartAlbum { .. } | Dialog::SmartRules { id: None, .. } | Dialog::CreatePreset { .. } => "Create",
                    Dialog::SmartRules { .. } | Dialog::LabelNames { .. } => "Save",
                    Dialog::RenameKeyword { from, to } if app.session.catalog.has_keyword(to) && !lcc_catalog::keywords::same(from, to) => "Merge Keywords",
                    Dialog::RenameAlbum { .. } | Dialog::Rename { .. } | Dialog::RenameKeyword { .. } | Dialog::RenamePerson { .. } => "Rename",
                    Dialog::MergeKeywords { .. } => "Merge Keywords",
                    Dialog::AutoStack { .. } => "Create Stacks",
                    Dialog::AdvancedSearch { .. } => "Search",
                    Dialog::CaptureTime { .. } => "Change Time",
                    Dialog::Cull { .. } => "Cull",
                    Dialog::CopySettings { .. } => "Copy",
                    Dialog::PasteSettings { .. } => "Paste",
                    Dialog::TextPrompt { command, .. } => prompt_verb(command),
                    _ => "OK",
                };
                // installing waits for the licence to be accepted
                // (and Synchronize waits for the scan)
                let rename_blocked = match &dlg {
                    Dialog::RenamePhoto { name, .. } => name.trim().is_empty(),
                    Dialog::RenamePerson { from, to, merge } => {
                        let plan = person_rename_plan(&app.session.catalog, from, to);
                        plan.block.is_some() || (plan.needs_merge && !*merge)
                    }
                    _ => false,
                };
                let can_confirm = !rename_blocked
                    && (informational
                        || !matches!(
                            &dlg,
                            Dialog::DenoiseModel { accepted: false, .. } | Dialog::FaceModel { accepted: false, .. } | Dialog::SynchronizeFolder { counts: None, .. }
                        )
                            // smart-album rules that can't mean anything wait to be fixed
                            && !matches!(&dlg, Dialog::SmartRules { id, rules, .. } if !rules.check_for(&app.session.catalog, id.map(lcc_catalog::AlbumId)).is_empty())
                            // a camera profile waits for its chart photos and their corners
                            && !matches!(&dlg, Dialog::ChartProfile { opts } if !crate::chart_profile::can_build(app, opts)));
                let primary = !informational && can_confirm && confirm_style(&dlg) == ConfirmStyle::Primary;
                use crate::button::{Button, Kind};
                // (each kind of dialog its own buttons: the keyboard's place doesn't carry from a
                // dialog to the one it opens)
                let kind = std::mem::discriminant(&dlg);
                // (finger-sized on a phone's bottom sheet)
                let (min_w, min_h) = if sheet { (110.0, 44.0) } else { (72.0, 0.0) };
                let cancel_button = Button::new(cancel).widget("button:dialogCancel").id_salt(kind).min_width(min_w).min_height(min_h);
                let ok_button = Button::new(ok)
                    .kind(if primary { Kind::Primary } else { Kind::Secondary })
                    .enabled(can_confirm)
                    .widget("button:dialogOk")
                    .id_salt(kind)
                    .min_width(min_w)
                    .min_height(min_h);
                // (Cancel first, then the confirm: the keyboard goes through them as the eye does)
                let gap = ui.spacing().item_spacing.x;
                let mut buttons_w = 0.0;
                if !informational {
                    buttons_w += cancel_button.desired_size(ui).x + gap;
                }
                if !ok.is_empty() {
                    buttons_w += ok_button.desired_size(ui).x;
                }
                ui.add_space((ui.available_width() - buttons_w).max(0.0));
                if !informational && cancel_button.show(ui).clicked() {
                    close = true;
                }
                let r = (!ok.is_empty()).then(|| ok_button.show(ui));
                if r.is_some_and(|r| r.clicked()) {
                    if informational {
                        close = true;
                    } else {
                        confirm = true;
                    }
                }
            });
            });
            if import {
                crate::import::note_dialog_bottom(ui);
            }
            content_w = ui.min_rect().width();
        });
    if settings && content_w > widest + 0.5 {
        ctx.data_mut(|d| d.insert_temp(widest_id, content_w));
    }
    if !import {
        crate::dialog_frame::ModalLayout::finish(ctx, window_id, shown.as_ref().map(|w| w.response.rect));
    }
    // the size the Presets Manager was left at (once the pointer is up: not in the middle of a drag)
    if let Some(w) = shown.as_ref().filter(|_| presets) {
        if profiles_manager {
            let window = crate::panels::profile_manager::WINDOW;
            crate::panels::presets_manager::note_size_of(ctx, window, &mut app.ui.profile_manager_size, presets_size, w.response.rect.size());
        } else {
            crate::panels::presets_manager::note_size(ctx, &mut app.ui.presets_manager_size, presets_size, w.response.rect.size());
        }
    }
    if let Some(w) = &shown {
        // (the import review keeps its hand-picked photos however the click lands)
        if app.ui.settings.close_dialogs_outside && !import && clicked_backdrop(ctx, w.response.rect, popup_was_open) {
            close = true;
        }
    }
    if let Some(w) = shown {
        ctx.memory_mut(|m| m.set_modal_layer(w.response.layer_id));
        ctx.move_to_top(w.response.layer_id);
        crate::widgets::register(ctx, "dialog:window", w.response.rect);
    }
    // (while the shortcuts editor records a key, it takes Esc itself to cancel; an open popup in
    // the dialog, such as a date picker's calendar, takes it to close itself)
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !crate::widgets::escape_taken(ctx) {
        close = true;
    }
    if confirm {
        match confirm_dialog(app, &dlg) {
            // the import review stays open on an error (e.g. an unusable folder template), and so
            // does Export (e.g. no folder chosen) so the choices aren't lost, and Edit Capture Time
            // (a time that isn't a date, or a shift it refuses)
            Err(e)
                if matches!(
                    dlg,
                    Dialog::Import { .. }
                        | Dialog::Export { .. }
                        | Dialog::ContactSheet { .. }
                        | Dialog::Timelapse { .. }
                        | Dialog::EmailPhoto { .. }
                        | Dialog::SlideshowExport { .. }
                        | Dialog::Print
                        | Dialog::NewAlbum { .. }
                        | Dialog::NewSmartAlbum { .. }
                        | Dialog::SmartRules { .. }
                        | Dialog::KeywordTag { .. }
                        | Dialog::KeywordSet { .. }
                        | Dialog::SynchronizeFolder { .. }
                        | Dialog::SaveEffect { .. }
                        | Dialog::CaptureTime { .. }
                        | Dialog::AdvancedSearch { .. }
                ) =>
            {
                app.toast(ctx, e)
            }
            // the SAM 3 dialog stays open to show the download (or why it can't start)
            Err(e) if matches!(dlg, Dialog::SamModel { .. }) => {
                if let Dialog::SamModel { error, .. } = &mut dlg {
                    *error = Some(e);
                }
            }
            Ok(r) if matches!(dlg, Dialog::NewAlbum { .. }) && r.get("skipped").and_then(serde_json::Value::as_u64).is_some_and(|n| n > 0) => {
                // albums hold library photos (issue #684): say which part of the selection stayed out
                crate::panels::grid::toast_skipped(app, ctx, &r);
                close = true;
            }
            // a move says how it went, or why nothing moved
            Ok(r) if matches!(dlg, Dialog::MovePhotos { .. }) => {
                if let Dialog::MovePhotos { name, .. } = &dlg {
                    let text = crate::panels::left::moved_text(r["moved"].as_u64().unwrap_or(0), name);
                    app.toast(ctx, text);
                }
                close = true;
            }
            // a prompt (Rename Folder, …) that is refused closes and says why
            Err(e) if matches!(dlg, Dialog::MovePhotos { .. } | Dialog::TextPrompt { .. }) => {
                app.toast(ctx, e);
                close = true;
            }
            Err(e) if matches!(dlg, Dialog::TextPrompt { .. }) => {
                app.toast(ctx, e);
                close = true;
            }
            // photos dated to a year or a month alone have no time to shift: say so (and, in the same
            // toast, about any whose time can't be read)
            Ok(r) if matches!(dlg, Dialog::CaptureTime { .. }) && r["noTime"].as_u64().is_some_and(|n| n > 0) => {
                let mut text = crate::i18n::tr("Photos dated to a year or month alone have no time to shift and were left as they were").to_string();
                if r["unreadable"].as_u64().is_some_and(|n| n > 0) {
                    text = format!("{text}. {}", crate::i18n::tr("Photos whose capture time can't be read were left as they were"));
                }
                app.toast(ctx, text);
                close = true;
            }
            // photos whose capture time doesn't read are left as they were: say so
            Ok(r) if matches!(dlg, Dialog::CaptureTime { .. }) && r["unreadable"].as_u64().is_some_and(|n| n > 0) => {
                app.toast(ctx, crate::i18n::tr("Photos whose capture time can't be read were left as they were"));
                close = true;
            }
            // where a Lightroom catalog goes: stays open to be corrected (a folder that isn't
            // empty), unless the library it names has gone
            Err(e) if matches!(dlg, Dialog::LightroomDestination { .. }) => {
                close = !crate::lightroom_destination::keeps_open_on_error(app, &dlg);
                app.toast(ctx, e);
            }
            Ok(_) if keeps_open(app, &dlg) => {
                if let Dialog::SamModel { error, .. } = &mut dlg {
                    *error = None;
                }
            }
            _ => close = true,
        }
    }
    // a dialog opened from inside this one (a button in Settings that shows a licence) takes its place
    if app.ui.dialog.as_ref().is_some_and(|now| *now != at_start) {
        return;
    }
    // a panel inside the dialog asked to close it (its own Close or Skip button)
    if std::mem::take(&mut app.close_dialog) {
        close = true;
    }
    if !close {
        let frame = ctx.cumulative_frame_nr();
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(BACKDROP_SHOWN), frame));
    }
    app.ui.dialog = if close { None } else { Some(dlg) };
}

/// The last frame a dialog was drawn and stayed open, for [`clicked_backdrop`].
const BACKDROP_SHOWN: &str = "dialog-backdrop-shown";

/// A click on the dimmed backdrop around the dialog: it closes the dialog like Esc. Pressed and
/// released outside the window, so a slider dragged past its edge doesn't close it; a press while a
/// popup is open (a dropdown, a date picker's calendar) only closes the popup: `popup_was_open` is
/// read before the window is drawn, since drawing it already closes the popup. Only a press made
/// while the dialog is shown counts: in the frame a dialog appears, its input is the click that
/// opened it (a quick click on a slow frame presses and releases in that one frame), so it never
/// closes it. (Read from the raw events: egui forgets the press origin on release.)
fn clicked_backdrop(ctx: &egui::Context, window: egui::Rect, popup_was_open: bool) -> bool {
    let press_id = egui::Id::new("dialog-backdrop-press");
    let frame = ctx.cumulative_frame_nr();
    // the same frame too: a second pass of it gets no input
    let shown = ctx.data(|d| d.get_temp::<u64>(egui::Id::new(BACKDROP_SHOWN))).is_some_and(|f| f + 1 >= frame);
    if !shown {
        ctx.data_mut(|d| d.remove_temp::<bool>(press_id));
        return false;
    }
    let (press, release, clicked) = ctx.input(|i| {
        let at = |down: bool| {
            i.events.iter().rev().find_map(|e| match e {
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, .. } if *pressed == down => Some(*pos),
                _ => None,
            })
        };
        (at(true), at(false), i.pointer.primary_clicked())
    });
    if let Some(p) = press {
        let outside = !window.contains(p) && !popup_was_open;
        ctx.data_mut(|d| d.insert_temp(press_id, outside));
    }
    let Some(release) = release else { return false };
    let pressed_outside = ctx.data_mut(|d| d.remove_temp::<bool>(press_id)).unwrap_or(false);
    clicked && pressed_outside && !window.contains(release)
}

/// A list that scrolls on its own never gets shorter than this to make its dialog fit.
const DIALOG_MIN_LIST: f32 = 120.0;
/// What a dialog needs besides such a list: its title bar, other options and buttons.
const DIALOG_LIST_CHROME: f32 = 200.0;

/// The height for a list in a dialog that scrolls on its own (What's New, All Metadata, the
/// shortcuts, smart-album rules): `preferred`, or less on a short window so the dialog fits it
/// (see [`crate::dialog_frame`]) rather than scrolling a list inside a scrolling dialog.
pub fn list_height(ui: &egui::Ui, preferred: f32) -> f32 {
    let room = crate::dialog_frame::max_window_height(ui.ctx()) - DIALOG_LIST_CHROME;
    preferred.min(room).max(preferred.min(DIALOG_MIN_LIST))
}

/// Apply a dialog's action (also used by `ui.dialog.confirm`).
/// What [`fmt_gap`] writes, read back: `90`, `90 s`, `1 min 30 s`, `2 h 5 min`, `1 day`
/// (units `s`, `min`, `h`, `day`, each optional after a plain number of seconds).
pub fn parse_gap(text: &str) -> Option<f64> {
    // the one translated text `fmt_gap` writes ("1 Tag")
    if text.trim() == crate::i18n::tr("1 day") {
        return Some(86400.0);
    }
    let mut total = 0.0;
    let mut seen = false;
    let mut words = text.split_whitespace().peekable();
    while let Some(word) = words.next() {
        // "90s" as well as "90 s"
        let split = word.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(word.len());
        let (num, unit) = word.split_at(split);
        let n: f64 = num.parse().ok().filter(|n: &f64| n.is_finite())?;
        let unit = if unit.is_empty() {
            match words.peek() {
                Some(u) if u.parse::<f64>().is_err() => words.next().unwrap_or_default(),
                _ => "s",
            }
        } else {
            unit
        };
        let scale = match unit.trim_end_matches('.').to_lowercase().as_str() {
            "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
            "d" | "day" | "days" => 86400.0,
            _ => return None,
        };
        total += n * scale;
        seen = true;
    }
    seen.then_some(total)
}

/// The longest burst gap `photo.analyze` accepts: a day.
pub const MAX_BURST_GAP: f32 = 86_400.0;

/// Keep what was typed in the Assisted Culling value fields within what `photo.analyze` accepts
/// (a gap of 0 s to a day, a similarity of 0 to 1), not within the sliders' drag range.
pub fn clamp_cull_burst(gap: &mut f32, similarity: &mut f32) {
    *gap = if gap.is_finite() { gap.clamp(0.0, MAX_BURST_GAP) } else { 10.0 };
    *similarity = if similarity.is_finite() { similarity.clamp(0.0, 1.0) } else { 0.93 };
}

/// The `photo.analyze` parameters of the Assisted Culling dialog, its values as they are.
pub fn cull_params(
    reject_below: f32,
    pick_best: bool,
    suggest_stars: bool,
    suggest_flags: bool,
    burst_gap: f32,
    similarity: f32,
) -> serde_json::Value {
    let mut p = json!({"pickBest": pick_best, "burstGap": burst_gap, "similarity": similarity});
    if reject_below > 0.0 {
        p["rejectBelow"] = json!(reject_below);
    }
    if suggest_stars {
        p["applyRatings"] = json!(true);
    }
    if suggest_flags {
        p["applyFlags"] = json!(true);
    }
    p
}

/// `90` → `1 min 30 s`.
pub fn fmt_gap(v: f64) -> String {
    let v = v.round() as u64;
    match v {
        0..60 => format!("{v} s"),
        60..3600 if v.is_multiple_of(60) => format!("{} min", v / 60),
        60..3600 => format!("{} min {} s", v / 60, v % 60),
        3600..86400 if v.is_multiple_of(3600) => format!("{} h", v / 3600),
        3600..86400 if v.is_multiple_of(60) => format!("{} h {} min", v / 3600, v % 3600 / 60),
        // the seconds too, so editing the text reads back the same gap
        3600..86400 => format!("{} h {} min {} s", v / 3600, v % 3600 / 60, v % 60),
        _ => crate::i18n::tr("1 day").into(),
    }
}

/// Whether a dialog stays open after its action succeeded (the SAM 3 dialog while the model
/// downloads).
pub fn keeps_open(app: &LccApp, dlg: &Dialog) -> bool {
    (matches!(dlg, Dialog::SamModel { .. }) && !app.session.segmenter.installed())
        // the profile is built in the background and its report shown here
        || matches!(dlg, Dialog::ChartProfile { .. })
}

/// No SAM 3 model, no download running and nowhere to download it from: installing it by hand is
/// all the dialog can offer.
fn sam_by_hand(sam: &lcc_engine::segment::Segmenter) -> bool {
    !sam.installed() && !sam.download_status().running && sam.mirrors().is_empty()
}

/// The model installation guide.
pub(crate) const SAM_HELP: &str = lcc_brand::REPOSITORY;

/// Open the configured SAM 3 folder itself; revealing a file opens its parent instead.
pub(crate) fn show_model_folder(app: &mut LccApp) -> Result<serde_json::Value, String> {
    let dir = app.session.segmenter.dir.as_ref().ok_or("no folder is set for the SAM 3 model")?;
    let path = dir.to_str().ok_or("the model folder path is not valid UTF-8")?;
    let open = app.services.open_with.as_mut().ok_or("opening model folders is not available here")?;
    // Create only after the host action is available. Existing model files are left alone.
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    #[cfg(target_os = "linux")]
    if let Some(open_folder) = app.services.open_model_folder.clone() {
        let path = path.to_string();
        let folder = path.clone();
        crate::tasks::spawn(
            app,
            "Show in File Manager",
            Some("openModelFolder"),
            move || open_folder(&folder),
            |app, ctx, result| {
                if let Err(e) = result {
                    app.toast_error(ctx, e);
                }
            },
        )?;
        return Ok(json!({"path": path, "background": true}));
    }
    open(path, "")?;
    Ok(json!({"path": path}))
}

/// The SAM 3 dialog: what the model is, its size and licence, and the download's progress.
fn sam_model_body(app: &mut LccApp, ui: &mut egui::Ui, error: Option<&str>) {
    use lcc_engine::segment::{LICENSE_NAME, LICENSE_URL, MODEL_BYTES};
    let t = Tokens::get(ui.ctx());
    let seg = &app.session.segmenter;
    let d = seg.download_status();
    if seg.installed() {
        ui.label(crate::i18n::tr("The SAM 3 model is installed: Object and Describe masks are ready."));
        return;
    }
    let gb = |b: u64| b as f64 / 1e9;
    ui.label(crate::i18n::tr(
        "Object and Describe masks use SAM 3, Meta's segmentation model. It isn't part of {app}, and everything else works without it.",
    ));
    let dir = seg.dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default();
    ui.label(format!("{} {:.1} GB, {} {dir}", crate::i18n::tr("A one-time download of about"), gb(MODEL_BYTES), crate::i18n::tr("saved in")));
    ui.label(
        egui::RichText::new(format!(
            "{} {LICENSE_NAME}: {}",
            crate::i18n::tr("Licence:"),
            crate::i18n::tr("Meta's terms, not {app}'s. Downloading it means accepting them.")
        ))
        .color(t.text_label),
    );
    let r = ui.link(crate::i18n::tr("Read the SAM License")).on_hover_text(LICENSE_URL);
    crate::widgets::register(ui.ctx(), "link:samLicense", r.rect);
    if r.clicked() {
        let _ = crate::links::open(app, LICENSE_URL);
    }
    let seg = &app.session.segmenter;
    if d.running {
        ui.add_space(4.0);
        let text = format!("{:.2} / {:.2} GB · {}", gb(d.done), gb(d.total), d.file);
        crate::progress::Progress::new(crate::progress::fraction_of(d.done, d.total)).label(text).widget("progress:samDownload").show(ui);
        let r = ui.button(crate::i18n::tr("Cancel Download"));
        crate::widgets::register(ui.ctx(), "button:samCancel", r.rect);
        if r.clicked() {
            app.session.segmenter.cancel_download();
        }
        crate::dialog_frame::note(
            ui,
            egui::RichText::new(crate::i18n::tr("You can close this: the download continues, and resumes if interrupted.")).color(t.text_dim),
        );
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        return;
    }
    if seg.mirrors().is_empty() {
        // nothing to download from: say how to install it by hand, with the folder and the guide a click away
        let dir = seg.dir.clone();
        crate::dialog_frame::note(ui,
            egui::RichText::new(crate::i18n::tr(
                "This build can't download the model yet. To install it by hand, put model.safetensors, vocab.json and merges.txt from Meta's facebook/sam3 in the folder above: Object and Describe work as soon as they are there.",
            ))
            .color(t.text_dim),
        );
        ui.horizontal_wrapped(|ui| {
            if dir.is_some() && crate::menus::ui_enabled(app, "app.openSamModelFolder") {
                let r = ui.button(crate::i18n::tr(crate::menus::reveal_label()));
                crate::widgets::register(ui.ctx(), "button:samFolder", r.rect);
                if r.clicked()
                    && let Err(e) = app.run("app.openSamModelFolder", json!({}))
                {
                    app.toast_error(ui.ctx(), e);
                }
            }
            let r = ui.link(crate::i18n::tr("How to install the model")).on_hover_text(SAM_HELP);
            crate::widgets::register(ui.ctx(), "link:samHelp", r.rect);
            if r.clicked()
                && let Err(e) = crate::links::open(app, SAM_HELP)
            {
                app.toast(ui.ctx(), e);
            }
        });
    }
    if let Some(e) = error.map(str::to_string).or(d.error) {
        ui.label(egui::RichText::new(format!("{} {e}", crate::i18n::tr("The download didn't work:"))).color(egui::Color32::from_rgb(230, 90, 80)));
    }
}

/// Runs a command that creates an album and asks the Albums tree to open the folders down to it.
fn created_in(app: &mut LccApp, command: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    let r = app.run(command, params)?;
    app.ui.reveal_album = r.get("id").and_then(serde_json::Value::as_u64);
    Ok(r)
}

/// The albums an Album rule in smart album `editing` picks from, in the sidebar's order: the album
/// itself and any smart album that tests it (testing it back would loop) are blocked, with why.
pub(crate) fn album_entries(cat: &lcc_catalog::Catalog, editing: Option<u64>) -> Vec<crate::album_picker::AlbumEntry> {
    let editing = editing.map(lcc_catalog::AlbumId);
    crate::album_picker::entries_from(cat, |a| match editing {
        Some(e) if a.id == e => Some(crate::i18n::tr("This is the collection you're editing").to_string()),
        Some(e) if cat.album_reaches(a.id, e) => Some(crate::i18n::tr("It tests this collection, so testing it back would loop").to_string()),
        _ => None,
    })
}

pub fn confirm_dialog(app: &mut LccApp, dlg: &Dialog) -> Result<serde_json::Value, String> {
    match dlg {
        Dialog::GenfillConsent { provider, index, prompt, variations } => {
            app.run("genfill.consent", json!({"provider": provider, "accept": true}))?;
            crate::panels::genfill::send(app, provider, *index, prompt, *variations)
        }
        Dialog::LightroomResult { first, .. } => {
            if let Some(first) = first {
                app.run("library.source", json!({"kind": "all"}))?;
                let mut filter = serde_json::to_value(lcc_catalog::Filter::default()).map_err(|e| e.to_string())?;
                // The filter command merges a patch; these defaults are omitted by serde.
                filter["only"] = json!([]);
                filter["labels"] = json!([]);
                filter["ruleSet"] = serde_json::Value::Null;
                app.run("library.filter", filter)?;
                app.run("library.select", json!({"ids": [first]}))?;
                app.ui.view = crate::state::ViewMode::PhotoGrid;
            }
            Ok(serde_json::Value::Null)
        }

        Dialog::ChartProfile { opts } => crate::chart_profile::start_build(app, opts),
        Dialog::SamModel { then, .. } => {
            if app.session.segmenter.installed() {
                // installed: start what the user was doing
                if let Some((kind, op)) = then {
                    crate::panels::masking::begin_ai(app, kind, op)?;
                }
                return Ok(serde_json::Value::Null);
            }
            let r = app.run("segment.model.download", json!({"acknowledged": true}));
            if r.is_ok() {
                app.ui.sam_downloading = true;
            }
            r
        }
        Dialog::NewAlbum { name, folder, parent, include_selected } => {
            created_in(app, "album.create", json!({"name": name, "folder": folder, "addSelected": !folder && *include_selected, "parent": parent}))
        }
        Dialog::RenameAlbum { id, name } => app.run("album.rename", json!({"id": id, "name": name})),
        Dialog::TextPrompt { value, command, params, key, .. } => {
            let mut p = params.clone();
            p[key.as_str()] = json!(value.trim());
            app.run(command, p)
        }
        Dialog::CaptureTime { mode, time, days, hours, minutes, zone, each } => app.run(
            "photo.setCaptureTime",
            match mode.as_str() {
                // a year or a month alone has no time to move the others by: every photo gets it
                "set" if lcc_catalog::dates::partial_capture(time.trim()).is_some() => json!({"time": time, "each": true}),
                "set" => json!({"time": time, "each": each}),
                "shift" => json!({"shift": *days as i64 * 86_400 + *hours as i64 * 3600 + *minutes as i64 * 60}),
                "file" => json!({"fromFile": true}),
                "name" => json!({"fromFilename": true}),
                _ => json!({"hours": zone}),
            },
        ),
        Dialog::LabelNames { names, save_as } => {
            let mut m = serde_json::Map::new();
            for (l, n) in lcc_catalog::ColorLabel::ALL.iter().zip(names) {
                m.insert(format!("{l:?}").to_lowercase(), if n.trim().is_empty() { serde_json::Value::Null } else { json!(n.trim()) });
            }
            let r = app.run("label.setNames", json!({"names": m}));
            if r.is_ok() && !save_as.trim().is_empty() {
                return app.run("label.saveSet", json!({"name": save_as.trim()}));
            }
            r
        }
        Dialog::RenamePhoto { id, name } => {
            let name_trim = name.trim();
            if name_trim.is_empty() {
                return Err("Type a name".into());
            }
            let photo = app.session.catalog.photo(lcc_catalog::PhotoId(*id));
            let template = if let Some(p) = photo {
                let (_, ext) = lcc_engine::rename::split_ext(&p.file_name);
                let (s, e) = lcc_engine::rename::split_ext(name_trim);
                if !ext.is_empty()
                    && (e.eq_ignore_ascii_case(ext) || lcc_engine::open_path::is_media(std::path::Path::new(name_trim)))
                    && !s.is_empty()
                {
                    s.to_string()
                } else {
                    name_trim.to_string()
                }
            } else {
                name_trim.to_string()
            };
            app.run("photo.rename", json!({"ids": [*id], "template": template}))
        }
        Dialog::Rename { template, start, texts } => app.run("photo.rename", json!({"template": template, "start": start, "texts": texts})),
        Dialog::RenameKeyword { from, to } => app.run("keyword.rename", json!({"from": from, "to": to})),
        Dialog::RenamePerson { from, to, merge } => app.run("faces.renamePerson", json!({"from": from, "to": to.trim(), "merge": merge})),
        Dialog::KeywordTag {
            editing,
            name,
            parent,
            inside,
            synonyms,
            include_on_export,
            export_containing,
            export_synonyms,
            person,
            add_to_selected,
            reveal,
        } => {
            let mut params = json!({
                "synonyms": synonyms.split(',').map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>(),
                "includeOnExport": include_on_export,
                "exportContaining": export_containing,
                "exportSynonyms": export_synonyms,
                "person": person,
            });
            match editing {
                Some(keyword) => {
                    params["keyword"] = json!(keyword);
                    params["name"] = json!(name);
                    let r = app.run("keyword.edit", params);
                    if r.is_ok() {
                        let to = match keyword.rsplit_once('|') {
                            Some((parent, _)) => format!("{parent}|{}", name.trim()),
                            None => name.trim().to_string(),
                        };
                        crate::panels::keyword_list::follow(app, keyword, &to);
                    }
                    r
                }
                None => {
                    params["name"] = json!(name);
                    params["parent"] = if *inside { json!(parent) } else { serde_json::Value::Null };
                    params["addToSelected"] = json!(add_to_selected);
                    let r = app.run("keyword.create", params);
                    if *reveal && let Ok(made) = &r {
                        app.ui.reveal_keyword = made["keyword"].as_str().map(str::to_string);
                    }
                    r
                }
            }
        }
        Dialog::DeleteKeyword { keyword, .. } => app.run("keyword.delete", json!({"keyword": keyword})),
        Dialog::KeywordSet { replaces, name, slots, as_new } => {
            use lcc_catalog::keywords::same;
            let name = name.trim();
            if name.is_empty() {
                return Err(crate::i18n::tr("A keyword set needs a name").to_string());
            }
            if same(name, lcc_engine::cmd::keywords::RECENT) {
                return Err(crate::i18n::tr("Recent Keywords is built in: choose another name").to_string());
            }
            // the set it renames: none when saving as new, or when it was deleted meanwhile (then it is
            // saved anew rather than refused)
            let sets = &app.session.keyword_sets;
            let renames = replaces.as_deref().filter(|_| !as_new).filter(|old| sets.iter().any(|x| same(&x.name, old)));
            if sets.iter().any(|x| same(&x.name, name) && !renames.is_some_and(|old| same(&x.name, old))) {
                return Err(crate::i18n::tr_format!("There is a keyword set “{name}” already", name = name));
            }
            let mut params = json!({"name": name, "keywords": slots});
            match renames {
                Some(old) => params["replace"] = json!(old),
                None => params["new"] = json!(true),
            }
            app.run("keyword.saveSet", params)
        }
        Dialog::MoveKeyword { keyword, parent } => {
            let r = app.run("keyword.move", json!({"keyword": keyword, "parent": parent, "merge": true}));
            if r.is_ok() {
                let leaf = keyword.rsplit('|').next().unwrap_or(keyword);
                let to = parent.as_deref().map_or_else(|| leaf.to_string(), |p| format!("{p}|{leaf}"));
                crate::panels::keyword_list::follow(app, keyword, &to);
            }
            r
        }
        Dialog::MergeKeywords { from, into } => app.run("keyword.merge", json!({"from": from, "into": into})),
        Dialog::AutoStack { gap } => app.run("stack.auto", json!({"gap": gap})),
        Dialog::AdvancedSearch { search } => {
            // a half-typed number or an unsupported condition keeps the panel open on the problem
            crate::advanced_search::draft_query(search).and_then(|query| app.run("library.advancedSearch", json!({"query": query})))
        }
        Dialog::DenoiseModel { info, accepted } => crate::panels::denoise::install(app, info, *accepted),
        Dialog::FaceModel { path, info, accepted } => crate::panels::faces::install(app, path, info, *accepted),
        Dialog::AllMetadata { .. } | Dialog::SystemInfo { .. } | Dialog::DngValidation { .. } | Dialog::WhatsNew | Dialog::AiModels { .. } => {
            Ok(serde_json::Value::Null)
        }
        Dialog::Cull {
            reject_below,
            pick_best,
            suggest_stars,
            suggest_flags,
            burst_gap,
            similarity,
            closed_eyes,
            blurry_eyes,
            soft_eyes,
            everyone,
        } => {
            app.ui.cull_burst = Some((*burst_gap, *similarity));
            let r = app.run("photo.analyze", cull_params(*reject_below, *pick_best, *suggest_stars, *suggest_flags, *burst_gap, *similarity));
            if r.is_ok() && (*closed_eyes || *blurry_eyes) {
                // the eyes take longer: judged in the background, the rejects come as one step
                app.run("photo.rejectEyes", json!({"closed": closed_eyes, "blurry": blurry_eyes, "soft": soft_eyes, "mainOnly": !everyone}))?;
            }
            r
        }
        Dialog::SmartRules { id, name, rules, parent } => {
            // refused before anything changes (not even the name)
            if let Some(problem) = rules.check_for(&app.session.catalog, id.map(lcc_catalog::AlbumId)).first() {
                return Err(problem.to_string());
            }
            let name = if name.trim().is_empty() { "Smart Collection".to_string() } else { name.trim().to_string() };
            match id {
                // the name and the rules together: one undo step, both or neither
                Some(id) => app.run("album.setRules", json!({"id": id, "name": name, "replace": true, "rules": {"ruleSet": rules}})),
                None => created_in(app, "album.createSmart", json!({"name": name, "rules": {"ruleSet": rules}, "parent": parent})),
            }
        }
        Dialog::NewSmartAlbum { name, parent } => {
            created_in(app, "album.createSmart", json!({"name": if name.trim().is_empty() { "Smart Collection" } else { name }, "parent": parent}))
        }
        Dialog::CreatePreset { name, group, groups } => {
            let mut p = pick_params(groups);
            p["name"] = json!(if name.trim().is_empty() { "My Preset" } else { name.trim() });
            p["group"] = json!(if group.trim().is_empty() { "User Presets" } else { group.trim() });
            app.run("preset.create", p)
        }
        Dialog::UpdatePreset { id, name, groups } => {
            let mut p = pick_params(groups);
            p["id"] = json!(id);
            let r = app.run("preset.update", p);
            if r.is_ok()
                && let Some(ctx) = app.tasks.repaint.clone()
            {
                let text = crate::i18n::tr_format!("Updated “{name}”", name = name);
                app.toast_with_action(&ctx, text, crate::i18n::tr("Undo"), "preset.undoUpdate", json!({}), 10.0);
            }
            r
        }
        Dialog::CreatePresetFolder { name, parent } => {
            let n = name.trim();
            if n.is_empty() { Ok(serde_json::Value::Null) } else { app.run("preset.createGroup", json!({"name": n, "parent": parent})) }
        }
        Dialog::ContactSheet { options } => app.run("app.contactSheet", json!(options)),
        Dialog::Timelapse { .. } => app.run("app.timelapse", crate::timelapse_ui::dialog_params(dlg).unwrap_or_default()),
        Dialog::EmailPhoto { .. } => app.run("app.emailPhoto", crate::email_photo::dialog_params(dlg).unwrap_or_default()),
        Dialog::SlideshowExport { settings } => app.run("app.slideshowExport", json!(settings)),
        Dialog::Print => app.run("app.print", json!({})),
        Dialog::CropOverlays { picked } => app.run("view.cropOverlays", json!({"overlays": picked})),
        Dialog::BookPrefs { prefs } => app.run("book.preferences", json!(prefs)),
        Dialog::CopySettings { groups } => app.run("develop.copy", pick_params(groups)),
        Dialog::PasteSettings { groups } => app.run("develop.paste", pick_params(groups)),
        Dialog::SyncSettings { groups, .. } => app.run("develop.sync", pick_params(groups)),
        Dialog::SyncCopies { groups } => {
            let mut params = pick_params(groups);
            if let Some(o) = params.as_object_mut() {
                o.insert("copies".into(), json!(true));
            }
            app.run("develop.sync", params)
        }
        Dialog::SyncSnapshots { groups } => app.run("version.sync", pick_params(groups)),
        Dialog::Export { opts, full_size, resize, limit_kb, dir, preset_name } => {
            start_export(app, opts, *full_size, resize, *limit_kb, dir, preset_name)
        }
        Dialog::Merge { opts } => crate::merge::start_final(app, opts),
        Dialog::Enhance { opts } => crate::enhance_ui::confirm(app, opts),
        Dialog::Import { opts } => crate::import::start(app, opts),
        Dialog::ConfirmDelete { .. } => app.run("photo.delete", json!({})),
        Dialog::ConfirmPurge { all: true, .. } => app.run("library.emptyRecentlyDeleted", json!({})),
        Dialog::ConfirmPurge { .. } => app.run("photo.deletePermanently", json!({})),
        Dialog::MovePhotos { ids, folder, dont_ask, library, .. } => {
            // another library opened meanwhile (from the menu bar, an agent): these numbers are
            // other photos there
            if *library != app.session.library_generation() {
                return Err(crate::i18n::tr("Another catalog was opened since the photos were dropped: nothing was moved").to_string());
            }
            let moved = crate::panels::left::move_photos(app, Some(ids), folder)?;
            // only a move that began turns the question off: one refused, or that found the photos there already,
            // leaves it as it was
            if *dont_ask && moved > 0 {
                app.ui.settings.ask_before_move = false;
            }
            Ok(json!({"moved": moved}))
        }
        // the catalog found: no file dialog; none: File ▸ Import Lightroom or Capture One Catalog… asks for it
        Dialog::LightroomCatalog { catalog, .. } => {
            app.run("file.importLightroom", catalog.as_ref().map_or_else(|| json!({}), |c| json!({"path": c})))
        }
        d @ Dialog::LightroomDestination { .. } => crate::lightroom_destination::confirm(app, d),
        Dialog::SaveEffect { name, group, fields, amount, brush } => {
            crate::panels::mask_effects::save_confirm(app, name, group, fields, *amount, *brush)
        }
        Dialog::DeleteEffects { names } => app.run("mask.deleteEffect", json!({"names": names})),
        Dialog::RemoveFolder { path, disk, .. } => app.run("library.removeFolder", json!({"path": path, "disk": disk})),
        Dialog::StopWaiting { id, .. } => {
            // the task may have ended meanwhile: then there is nothing left to stop
            crate::panels::activity::cancel(app, *id);
            Ok(serde_json::Value::Null)
        }
        Dialog::VignettingCreateAnyway { photo, .. } => crate::panels::edit::create_vignetting_calibration_anyway(app, *photo),
        Dialog::VignettingReplace { photo, .. } => crate::panels::edit::replace_vignetting_calibration(app, *photo),
        Dialog::DeleteRejected { recycle, .. } => {
            let r = app.run("library.deleteRejected", json!({"files": if *recycle { "recycle" } else { "keep" }, "confirm": true}));
            if let Ok(v) = &r {
                let n = v["deleted"].as_u64().unwrap_or(0);
                let text = if *recycle {
                    crate::i18n::tr_format!("{n} rejected photos moved to the trash", n = n)
                } else {
                    crate::i18n::tr_format!("{n} rejected photos removed (Edit ▸ Undo brings them back)", n = n)
                };
                if let Some(ctx) = app.tasks.repaint.clone() {
                    app.toast(&ctx, text);
                }
            }
            r
        }
        d @ Dialog::SynchronizeFolder { .. } => crate::sync::confirm(app, d),
        Dialog::NewPhotoDefault { name } => app.run("library.setImportDefault", json!({"name": name.trim()})),
        Dialog::Watermark { wm, preset_name } => {
            let mut params = app.session.last_export.clone().unwrap_or_default();
            params["watermark"] = serde_json::to_value(wm).unwrap_or_default();
            app.session.last_export = Some(params.clone());
            if !preset_name.trim().is_empty() {
                let _ = app.run("export.savePreset", json!({"name": preset_name.trim(), "params": params}));
            }
            Ok(serde_json::Value::Null)
        }
        Dialog::About
        | Dialog::Shortcuts
        | Dialog::ViewOptions
        | Dialog::Settings { .. }
        | Dialog::Update
        | Dialog::ManageEffects { .. }
        | Dialog::PresetsManager { .. }
        | Dialog::ProfileManager { .. } => Ok(serde_json::Value::Null),
    }
}

/// A bottom sheet's width: the window's, inside its side insets.
fn sheet_w(screen: egui::Rect, insets: crate::modern::Insets) -> f32 {
    (screen.width() - insets.left - insets.right).max(200.0)
}

/// Open a one-field dialog that runs `command` with `params` + `{key: typed value}`.
/// What a one-field prompt's confirm button says, by the command it runs. A command that isn't
/// listed (one opened by automation) has no verb to name and keeps "OK".
fn prompt_verb(command: &str) -> &'static str {
    match command {
        "album.rename"
        | "folder.rename"
        | "version.rename"
        | "preset.rename"
        | "mask.renameEffect"
        | "printui.renameTemplateFolder"
        | "slideshow.renameTemplateFolder"
        | "app.renameLibrary"
        | "print.template.folder.rename" => "Rename",
        "folder.create"
        | "mask.setEffectGroup"
        | "app.newLibrary"
        | "preset.createGroup"
        | "printui.createTemplateFolder"
        | "slideshow.createTemplateFolder"
        | "print.template.folder.create" => "Create",
        "preset.move" => "Move",
        "keyword.saveSet"
        | "printui.saveCreation"
        | "printui.saveTemplate"
        | "book.save"
        | "slideshow.saveSlideshow"
        | "slideshow.saveTemplate"
        | "web.saveGallery"
        | "filter.savePreset"
        | "curve.savePreset"
        | "metadata.savePreset" => "Save",
        "photo.tagFromTracklog" => "Add Locations",
        _ => "OK",
    }
}

pub fn prompt(app: &mut LccApp, title: &str, hint: &str, value: &str, command: &str, params: serde_json::Value, key: &str) {
    app.ui.dialog =
        Some(Dialog::TextPrompt { title: title.into(), hint: hint.into(), value: value.into(), command: command.into(), params, key: key.into() });
}

/// Starts the export the Export dialog (or the Export module) is set up for: saves a preset name
/// typed in the name field, then runs `app.export` in the background.
pub(crate) fn start_export(
    app: &mut LccApp,
    opts: &lcc_engine::export::ExportOptions,
    full_size: bool,
    resize: &lcc_engine::export::Resize,
    limit_kb: u32,
    dir: &str,
    preset_name: &str,
) -> Result<serde_json::Value, String> {
    // a name typed in the preset name field is saved by Export too (the Add button is easy to miss)
    if !preset_name.trim().is_empty() {
        let params = preset_params(opts, full_size, resize, limit_kb, dir);
        let saved = app.run("export.savePreset", json!({"name": preset_name.trim(), "params": params}));
        if let Some(ctx) = app.tasks.repaint.clone() {
            match saved {
                Ok(_) => app.toast(&ctx, crate::i18n::tr_format!("Saved export preset “{}”", preset_name.trim())),
                Err(e) => app.toast(&ctx, e),
            }
        }
    }
    let mut p = export_dialog_params(opts, full_size, resize, limit_kb);
    // (Choose folder later: `app.export` asks for it)
    if opts.destination_mode != lcc_engine::export::DestinationMode::ChooseLater {
        p["dir"] = json!(dir);
    }
    p["background"] = json!(true);
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(d) = crate::panels::plugins::chosen_destination() {
        p["pluginDestination"] = d;
    }
    app.run("app.export", p)
}

/// The Export dialog's choices as `app.export` params (without the folder).
pub(crate) fn export_dialog_params(
    opts: &lcc_engine::export::ExportOptions,
    full_size: bool,
    resize: &lcc_engine::export::Resize,
    limit_kb: u32,
) -> serde_json::Value {
    let o =
        lcc_engine::export::ExportOptions { resize: (!full_size).then_some(*resize), limit_kb: (limit_kb > 0).then_some(limit_kb), ..opts.clone() };
    o.to_json()
}

/// The Export dialog's fields, borrowed from [`Dialog::Export`].
pub(crate) struct ExportForm<'a> {
    pub(crate) opts: &'a mut lcc_engine::export::ExportOptions,
    pub(crate) full_size: &'a mut bool,
    pub(crate) resize: &'a mut lcc_engine::export::Resize,
    pub(crate) preset_name: &'a mut String,
    pub(crate) limit_kb: &'a mut u32,
    pub(crate) dir: &'a mut String,
}

/// The Export dialog's preset list column, in points.
const EXPORT_PRESETS_W: f32 = 200.0;
/// Whether a JPEG size limit of `limit_kb` is tight for the largest of the photos (`(width, height)`
/// in pixels) at the chosen size (`lcc_engine::export::size_limit_is_tight`).
pub(crate) fn size_limit_is_tight_for(limit_kb: u32, full_size: bool, resize: &lcc_engine::export::Resize, photos: &[(u32, u32)]) -> bool {
    let pixels = photos
        .iter()
        .map(|&(w, h)| {
            let k = if full_size { 1.0 } else { resize.scale(f64::from(w), f64::from(h)) };
            let k = if k.is_finite() { k.clamp(0.0, 64.0) } else { 1.0 };
            let k = if !full_size && resize.dont_enlarge { k.min(1.0) } else { k };
            (f64::from(w) * k) * (f64::from(h) * k)
        })
        .fold(0.0_f64, f64::max);
    pixels >= 1.0 && limit_kb > 0 && lcc_engine::export::size_limit_is_tight(limit_kb, pixels as usize)
}

/// The Export dialog's label column: its slider names are long ("Limit file size (KB, 0 = off)").
const EXPORT_LABEL_W: f32 = 180.0;
/// Narrower than this, the Export dialog drops its preset column for a Preset menu at the top.
const EXPORT_TWO_COLUMNS_W: f32 = 640.0;

/// The Export dialog, laid out like Lightroom Classic's: the presets in a column on the left, the
/// settings on the right in its order (Export Location, File Naming, Video, File Settings, Image
/// Sizing, Output Sharpening, Metadata, Watermarking, Post-Processing), each a section that folds
/// and says what it is set to on its header. The settings scroll within `max_h`; the window can be
/// made wider (a large UI font) and remembers it.
fn export_body(app: &mut LccApp, ui: &mut egui::Ui, f: ExportForm<'_>, id: egui::Id, max_h: f32) {
    let two_columns = ui.available_width() >= EXPORT_TWO_COLUMNS_W;
    let ExportForm { opts, full_size, resize, preset_name, limit_kb, dir } = f;
    let left_w = if two_columns { EXPORT_PRESETS_W } else { 0.0 };
    let cols = crate::dialog_frame::TwoColumns::begin(ui, id.with("export"), left_w, 12.0, if two_columns { 320.0 } else { 0.0 }, max_h);
    if two_columns {
        ui.scope_builder(egui::UiBuilder::new().max_rect(cols.left).layout(egui::Layout::top_down(egui::Align::Min)), |ui| {
            let f = ExportForm {
                opts: &mut *opts,
                full_size: &mut *full_size,
                resize: &mut *resize,
                preset_name: &mut *preset_name,
                limit_kb: &mut *limit_kb,
                dir: &mut *dir,
            };
            export_presets(app, ui, f, cols.left.height());
        });
    }
    cols.right(ui, |ui| {
        crate::dialog_frame::form(ui, if two_columns { EXPORT_LABEL_W } else { FORM_LABEL_W + 28.0 }, |ui| {
            export_settings(app, ui, ExportForm { opts, full_size, resize, preset_name, limit_kb, dir }, !two_columns);
        });
    });
}

/// Loads the export preset `name` into the dialog. Where to export (the destination mode and the
/// folder) stays as set unless the preset says.
#[allow(clippy::too_many_arguments)]
pub(crate) fn load_export_preset(
    app: &LccApp,
    name: &str,
    opts: &mut lcc_engine::export::ExportOptions,
    full_size: &mut bool,
    resize: &mut lcc_engine::export::Resize,
    limit_kb: &mut u32,
    dir: &mut String,
) {
    let Ok(params) = app.session.export_params(&json!({"preset": name})) else { return };
    let mut o = lcc_engine::export::ExportOptions::from_json(&params);
    if params.get("destinationMode").is_none() {
        o.destination_mode = opts.destination_mode;
    }
    if let Some(d) = params.get("dir").and_then(serde_json::Value::as_str).filter(|d| !d.trim().is_empty()) {
        *dir = d.to_string();
    }
    *full_size = o.resize.is_none();
    *resize = o.resize.unwrap_or_default();
    *limit_kb = o.limit_kb.unwrap_or(0);
    *opts = o;
}

/// If the Export dialog is open, select and load the export preset `name` into it.
pub(crate) fn apply_export_preset_to_open_dialog(app: &mut LccApp, ctx: &egui::Context, name: &str) {
    let Some(Dialog::Export { mut opts, mut full_size, mut resize, preset_name, mut limit_kb, mut dir }) = app.ui.dialog.clone() else { return };
    load_export_preset(app, name, &mut opts, &mut full_size, &mut resize, &mut limit_kb, &mut dir);
    app.ui.dialog = Some(Dialog::Export { opts, full_size, resize, preset_name, limit_kb, dir });
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("export-preset-picked"), name.to_string()));
}

/// The dialog's settings as preset params, with the folder when the export goes to a specific
/// one (as Lightroom's presets keep it).
fn preset_params(
    opts: &lcc_engine::export::ExportOptions,
    full_size: bool,
    resize: &lcc_engine::export::Resize,
    limit_kb: u32,
    dir: &str,
) -> serde_json::Value {
    let mut params = export_dialog_params(opts, full_size, resize, limit_kb);
    if opts.destination_mode == lcc_engine::export::DestinationMode::SpecificFolder && !dir.trim().is_empty() {
        params["dir"] = json!(dir.trim());
    }
    params
}

/// The Export dialog's preset column: the built-in presets, then the user's, each loaded with a
/// click; below, a name with Add (save the dialog's settings as a preset), Rename (the user preset
/// picked, to the name typed) and Remove, then Import… (Lightroom Classic export presets,
/// `.lcexport`) and Export… (the user presets as `.lcexport`).
pub(crate) fn export_presets(app: &mut LccApp, ui: &mut egui::Ui, f: ExportForm<'_>, max_h: f32) {
    let ExportForm { opts, full_size, resize, preset_name, limit_kb, dir } = f;
    let t = Tokens::get(ui.ctx());
    let picked_id = egui::Id::new("export-preset-picked");
    let mut picked = ui.data(|d| d.get_temp::<String>(picked_id)).unwrap_or_default();
    let n = app.session.selection.ids.len().max(1);
    ui.label(egui::RichText::new(crate::i18n::tr_format!("{n} photo{}", if n == 1 { "" } else { "s" }, n = n)).color(t.text_label));
    ui.add_space(2.0);
    ui.label(egui::RichText::new(crate::i18n::tr("Preset")).color(t.text_dim));
    let presets = app.session.all_export_presets();
    let rows_h = (max_h - 116.0).max(60.0);
    let mut chosen = None;
    egui::Frame::NONE.fill(t.inset).stroke(egui::Stroke::new(1.0, t.divider)).corner_radius(4.0).inner_margin(4.0).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt("export-preset-list").max_height(rows_h).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            for (heading, builtin) in [("Built-in", true), ("User Presets", false)] {
                let group: Vec<_> = presets.iter().filter(|(_, b)| *b == builtin).collect();
                if group.is_empty() {
                    continue;
                }
                ui.add_space(2.0);
                ui.label(egui::RichText::new(crate::i18n::tr(heading)).font(t.semibold(11.5)).color(t.text_label));
                for (i, (p, b)) in group.iter().enumerate() {
                    let on = picked.eq_ignore_ascii_case(&p.name);
                    let label = crate::i18n::builtin_label(&p.name, *b).to_string();
                    let r = ui.add(
                        egui::Button::selectable(on, label).frame_when_inactive(false).min_size(egui::vec2(ui.available_width(), 20.0)).truncate(),
                    );
                    crate::widgets::register(ui.ctx(), format!("button:exportPreset-{}-{i}", if builtin { "builtin" } else { "user" }), r.rect);
                    if r.clicked() {
                        chosen = Some(p.name.clone());
                    }
                }
            }
        });
    });
    if let Some(name) = chosen {
        load_export_preset(app, &name, opts, full_size, resize, limit_kb, dir);
        picked = name;
    }
    ui.add_space(4.0);
    crate::widgets::named(
        ui.add(egui::TextEdit::singleline(preset_name).hint_text(crate::i18n::tr("Preset name")).desired_width(f32::INFINITY)),
        crate::i18n::tr("Preset name"),
    );
    ui.horizontal(|ui| {
        let named = !preset_name.trim().is_empty();
        if crate::widgets::text_button(ui, "exportSavePreset", crate::i18n::tr("Add"), false).clicked() && named {
            let params = preset_params(opts, *full_size, resize, *limit_kb, dir);
            match app.run("export.savePreset", json!({"name": preset_name.trim(), "params": params})) {
                Ok(_) => {
                    app.toast(ui.ctx(), crate::i18n::tr_format!("Saved export preset “{}”", preset_name.trim()));
                    picked = preset_name.trim().to_string();
                    preset_name.clear();
                }
                Err(e) => app.toast(ui.ctx(), e),
            }
        }
        let user = presets.iter().any(|(p, b)| !*b && p.name.eq_ignore_ascii_case(&picked));
        let r = ui.add_enabled_ui(user, |ui| crate::widgets::text_button(ui, "exportRemovePreset", crate::i18n::tr("Remove"), false)).inner;
        if r.clicked() && user {
            match app.run("export.deletePreset", json!({"name": picked})) {
                Ok(_) => picked.clear(),
                Err(e) => app.toast(ui.ctx(), e),
            }
        }
        let can_rename = user && named;
        let r = ui.add_enabled_ui(can_rename, |ui| crate::widgets::text_button(ui, "exportRenamePreset", crate::i18n::tr("Rename"), false)).inner;
        if r.clicked() && can_rename {
            match app.run("export.renamePreset", json!({"name": picked, "newName": preset_name.trim()})) {
                Ok(_) => {
                    picked = preset_name.trim().to_string();
                    preset_name.clear();
                }
                Err(e) => app.toast(ui.ctx(), e),
            }
        }
    });
    ui.horizontal(|ui| {
        if crate::widgets::text_button(ui, "exportImportPresets", crate::i18n::tr("Import…"), false).clicked()
            && let Ok(v) = app.run("file.importPresets", json!({}))
            && let Some(first) = v["exportPresets"].as_array().and_then(|a| a.iter().find(|r| r["skipped"] != true)).and_then(|r| r["name"].as_str())
        {
            load_export_preset(app, first, opts, full_size, resize, limit_kb, dir);
            picked = first.to_string();
        }
        let any = presets.iter().any(|(_, b)| !*b);
        let r = ui.add_enabled_ui(any, |ui| crate::widgets::text_button(ui, "exportExportPresets", crate::i18n::tr("Export…"), false)).inner;
        if r.clicked() && any {
            let _ = app.run("file.exportExportPresets", json!({}));
        }
    });
    ui.data_mut(|d| d.insert_temp(picked_id, picked));
}

/// Export dialog sections shown folded until the person opens them: the ones usually set once
/// (where the files go, their names and format stay open).
const EXPORT_FOLDED_AT_FIRST: &[&str] = &["Sizing", "Sharpening", "Metadata", "Watermark"];

/// Opens or folds an Export dialog section. Each starts open or folded
/// ([`EXPORT_FOLDED_AT_FIRST`]); once the person flips it, that is kept in the settings
/// (`groupOpen`, `export:<key>`).
fn export_section(app: &mut LccApp, ui: &mut egui::Ui, key: &str, title: &str, summary: &str) -> bool {
    // the Export screen: every section open, as a titled card the rail can jump to
    if ui.ctx().data(|d| d.get_temp::<bool>(egui::Id::new(EXPORT_SCREEN)).unwrap_or(false)) {
        let heading_top = ui.cursor().top();
        crate::dialog_frame::group_begin(ui, Some(title));
        let top = ui.cursor().top();
        // (the old fold bar's id, on the heading)
        crate::widgets::register(ui.ctx(), format!("section:export{key}"), egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), heading_top..=top));
        ui.ctx().data_mut(|d| {
            let mut tops: Vec<(String, f32, String)> = d.get_temp(egui::Id::new(EXPORT_TOPS)).unwrap_or_default();
            tops.push((key.to_string(), top, summary.to_string()));
            d.insert_temp(egui::Id::new(EXPORT_TOPS), tops);
        });
        if ui.ctx().data_mut(|d| d.get_temp::<String>(egui::Id::new(EXPORT_JUMP)).is_some_and(|k| k == key)) {
            ui.scroll_to_rect(egui::Rect::from_min_size(egui::pos2(ui.max_rect().left(), top - 40.0), egui::vec2(1.0, 1.0)), Some(egui::Align::TOP));
            ui.ctx().data_mut(|d| d.remove::<String>(egui::Id::new(EXPORT_JUMP)));
        }
        return true;
    }
    let id = format!("export:{key}");
    let mut open = app.ui.group_is_open(&id, !EXPORT_FOLDED_AT_FIRST.contains(&key));
    if crate::dialog_frame::fold_group(ui, &format!("export{key}"), title, summary, &mut open) {
        app.ui.group_open.insert(id, open);
    }
    open
}

/// Context flag: the export rows are drawn on the Export screen (sections are open cards).
const EXPORT_SCREEN: &str = "export-screen-mode";
/// This frame's section tops on the Export screen: (key, y, summary).
const EXPORT_TOPS: &str = "export-screen-tops";
/// A section the rail asked to scroll to.
const EXPORT_JUMP: &str = "export-screen-jump";

/// The Export screen's rail: its groups (name, the section keys it covers).
const EXPORT_GROUPS: [(&str, &[&str]); 7] = [
    ("Where", &["Location", "Naming"]),
    ("Video", &["Video"]),
    ("File", &["FileSettings"]),
    ("Size & Sharpening", &["Sizing", "Sharpening"]),
    ("Metadata", &["Metadata"]),
    ("Watermark", &["Watermark"]),
    ("After Export", &["PostProcessing"]),
];

/// Export as a full-window screen, in every layout: the header (Back, how many photos, where they
/// go), the presets and a rail of the settings' groups on the left (a tap scrolls to one; the one in
/// view is lit), every setting on one scrolling page, and a bar along the bottom that always says
/// what will be written, with Export. Back, Esc and Cancel close it. Own design.
pub(crate) fn export_screen(app: &mut LccApp, ctx: &egui::Context) {
    use lcc_engine::export::DestinationMode as D;
    let Some(Dialog::Export { mut opts, mut full_size, mut resize, mut preset_name, mut limit_kb, mut dir }) = app.ui.dialog.clone() else { return };
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let roomy = screen.width() >= crate::settings_screen::ROOMY;
    let mut cancel = !egui::Popup::is_any_open(ctx)
        && !ctx.egui_wants_keyboard_input()
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let mut go = false;
    let n = app.session.selection.ids.len().max(1);
    // last frame's section tops, for the rail's highlight
    let tops: Vec<(String, f32, String)> = ctx.data_mut(|d| d.remove_temp(egui::Id::new(EXPORT_TOPS))).unwrap_or_default();
    // the dialog's own id and the modal layer: popups opened in it (dropdowns) stay open
    let shown = egui::Area::new(egui::Id::new("app-export-dialog")).movable(false).order(egui::Order::Middle).fixed_pos(screen.min).show(ctx, |ui| {
        // (hover only: the area on top already keeps clicks from the window below, and a
        // nameless clickable backdrop would be an unnamed control to a screen reader)
        let (bg, _) = ui.allocate_exact_size(screen.size(), egui::Sense::hover());
        ui.painter().rect_filled(bg, 0.0, t.chrome);
        // (the dialog's id, as the window had)
        crate::widgets::register(ui.ctx(), "dialog:window", bg);
        // header
        let header = egui::Rect::from_min_size(bg.min, egui::vec2(bg.width(), 56.0));
        let b = egui::Rect::from_min_size(egui::pos2(header.left() + 10.0, header.center().y - 18.0), egui::Vec2::splat(36.0));
        let back = ui.interact(b, ui.id().with("export-back"), egui::Sense::click());
        back.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, crate::i18n::tr("Back")));
        crate::widgets::register(ui.ctx(), "export:back", b);
        ui.painter().circle_filled(b.center(), 18.0, if back.hovered() { t.pressed } else { t.button });
        crate::icons::paint(ui.painter(), egui::Rect::from_center_size(b.center(), egui::Vec2::splat(18.0)), crate::icons::Icon::ChevronLeft, t.text);
        if back.clicked() {
            cancel = true;
        }
        let title = crate::i18n::tr_format!("Export {n} photo{}", if n == 1 { "" } else { "s" }, n = n);
        ui.painter().text(egui::pos2(b.right() + 12.0, header.center().y), egui::Align2::LEFT_CENTER, title, t.semibold(17.0), t.text);
        // where: the destination as one segmented choice, top right
        const WHERE: [(D, &str); 3] = [(D::ChooseLater, "Ask"), (D::SpecificFolder, "Folder"), (D::SameAsOriginal, "Next to Originals")];
        if roomy && !cfg!(target_arch = "wasm32") {
            let w = 330.0;
            let r = egui::Rect::from_min_size(egui::pos2(header.right() - 16.0 - w, header.top() + 8.0), egui::vec2(w, 40.0));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::right_to_left(egui::Align::Center)));
            let labels: Vec<&str> = WHERE.iter().map(|(_, l)| *l).collect();
            let chosen = WHERE.iter().position(|(m, _)| *m == opts.destination_mode);
            if let Some(i) = crate::widgets::segments(&mut child, "exportWhere", &labels, chosen)
                && let Some((m, _)) = WHERE.get(i)
            {
                opts.destination_mode = *m;
            }
        }
        ui.painter().hline(bg.x_range(), header.bottom(), egui::Stroke::new(1.0, t.divider));
        // the bar along the bottom: what will be written, Cancel, Export
        let bar = egui::Rect::from_min_max(egui::pos2(bg.left(), bg.bottom() - 60.0), bg.max);
        ui.painter().hline(bg.x_range(), bar.top(), egui::Stroke::new(1.0, t.divider));
        let summary_of = |key: &str| tops.iter().find(|(k, _, _)| k == key).map(|(_, _, s)| s.clone()).filter(|s| !s.is_empty());
        let mut parts = vec![crate::i18n::tr_format!("{n} photo{}", if n == 1 { "" } else { "s" }, n = n)];
        parts.extend(["FileSettings", "Sizing"].iter().filter_map(|k| summary_of(k)));
        let summary = parts.join(" · ");
        let room = bar.width() - 300.0;
        let mut job = egui::text::LayoutJob::simple_singleline(summary, t.font(13.0), t.text_label);
        job.wrap = egui::text::TextWrapping { max_width: room.max(60.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        let g = ui.painter().layout_job(job);
        let gr = egui::Rect::from_min_size(egui::pos2(bar.left() + 18.0, bar.center().y - g.size().y / 2.0), g.size());
        crate::widgets::register(ui.ctx(), "export:summary", gr);
        ui.painter().galley(gr.min, g, t.text_label);
        let mut buttons = ui
            .new_child(egui::UiBuilder::new().max_rect(bar.shrink2(egui::vec2(16.0, 10.0))).layout(egui::Layout::right_to_left(egui::Align::Center)));
        buttons.spacing_mut().item_spacing.x = 8.0;
        if crate::button::Button::new("Export")
            .kind(crate::button::Kind::Primary)
            .size(crate::button::Size::Large)
            .widget("button:dialogOk")
            .show(&mut buttons)
            .clicked()
        {
            go = true;
        }
        if crate::button::Button::new("Cancel").size(crate::button::Size::Large).widget("button:dialogCancel").show(&mut buttons).clicked() {
            cancel = true;
        }
        let body = egui::Rect::from_min_max(egui::pos2(bg.left(), header.bottom() + 1.0), egui::pos2(bg.right(), bar.top() - 1.0));
        // the rail: the groups (a tap scrolls there; the one in view is lit), then the presets
        let rail_w = if roomy { 240.0 } else { 0.0 };
        let content = egui::Rect::from_min_max(egui::pos2(body.left() + rail_w, body.top()), body.max);
        if roomy {
            let rail = egui::Rect::from_min_size(body.min, egui::vec2(rail_w, body.height()));
            ui.painter().vline(rail.right(), body.y_range(), egui::Stroke::new(1.0, t.divider));
            let mut r = ui.new_child(egui::UiBuilder::new().max_rect(rail.shrink2(egui::vec2(10.0, 10.0))));
            r.spacing_mut().item_spacing.y = 2.0;
            let lit = tops.iter().rev().find(|(_, y, _)| *y <= content.top() + 60.0).or(tops.first()).map(|(k, _, _)| k.clone()).unwrap_or_default();
            for (label, keys) in EXPORT_GROUPS {
                if !keys.iter().any(|k| tops.iter().any(|(t, _, _)| t == k)) && !tops.is_empty() {
                    continue;
                }
                let (row, resp) = r.allocate_exact_size(egui::vec2(r.available_width(), 38.0), egui::Sense::click());
                let on = keys.contains(&lit.as_str());
                let name = crate::i18n::tr(label);
                resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, name));
                crate::widgets::register(r.ctx(), format!("export:group:{}", keys.first().copied().unwrap_or_default()), row);
                if on {
                    r.painter().rect_filled(row, 10.0, egui::Color32::from_white_alpha(30));
                } else if resp.hovered() {
                    r.painter().rect_filled(row, 10.0, egui::Color32::from_white_alpha(14));
                }
                r.painter().text(
                    egui::pos2(row.left() + 14.0, row.center().y),
                    egui::Align2::LEFT_CENTER,
                    name,
                    t.font(14.0),
                    if on { t.text } else { t.text_label },
                );
                if resp.clicked()
                    && let Some(k) = keys.first()
                {
                    r.ctx().data_mut(|d| d.insert_temp(egui::Id::new(EXPORT_JUMP), k.to_string()));
                }
            }
            r.add_space(10.0);
            let f = ExportForm {
                opts: &mut opts,
                full_size: &mut full_size,
                resize: &mut resize,
                preset_name: &mut preset_name,
                limit_kb: &mut limit_kb,
                dir: &mut dir,
            };
            let h = r.available_height();
            export_presets(app, &mut r, f, h);
        }
        // every setting, one scrolling page
        let w = content.width().min(860.0);
        let column = egui::Rect::from_min_size(
            egui::pos2(content.left() + ((content.width() - w) / 2.0).max(0.0), content.top()),
            egui::vec2(w, content.height()),
        );
        let mut page = ui.new_child(egui::UiBuilder::new().max_rect(column.shrink2(egui::vec2(16.0, 0.0))));
        page.ctx().data_mut(|d| d.insert_temp(egui::Id::new(EXPORT_SCREEN), true));
        egui::ScrollArea::vertical().id_salt("export-page").auto_shrink([false, false]).show(&mut page, |ui| {
            ui.add_space(12.0);
            ui.set_width((ui.available_width() - 12.0).max(120.0));
            let stacked = column.width() < 520.0;
            let f = ExportForm {
                opts: &mut opts,
                full_size: &mut full_size,
                resize: &mut resize,
                preset_name: &mut preset_name,
                limit_kb: &mut limit_kb,
                dir: &mut dir,
            };
            let body = |ui: &mut egui::Ui| export_settings(app, ui, f, !roomy);
            if stacked {
                crate::dialog_frame::stacked_form(ui, 120.0, body);
            } else {
                crate::dialog_frame::form(ui, EXPORT_LABEL_W, body);
            }
            ui.add_space(40.0);
        });
        page.ctx().data_mut(|d| d.remove::<bool>(egui::Id::new(EXPORT_SCREEN)));
    });
    ctx.memory_mut(|m| m.set_modal_layer(shown.response.layer_id));
    // the rows may have closed Export or opened another dialog (a folder picker, Watermark…)
    if !matches!(app.ui.dialog, Some(Dialog::Export { .. })) {
        return;
    }
    let now = Some(Dialog::Export { opts, full_size, resize, preset_name, limit_kb, dir });
    app.ui.dialog = now.clone();
    if cancel {
        app.ui.dialog = None;
        return;
    }
    if go && let Some(dlg) = now {
        match confirm_dialog(app, &dlg) {
            Ok(_) => {
                if app.ui.dialog.as_ref() == Some(&dlg) {
                    app.ui.dialog = None;
                }
            }
            Err(e) => app.toast_error(ctx, e),
        }
    }
}

/// One line saying what an Export dialog setting is, for its section's header.
fn label_of<V: PartialEq + Copy>(options: &[(V, &str)], value: V) -> String {
    options.iter().find(|(v, _)| *v == value).map(|(_, l)| crate::i18n::tr(l).to_string()).unwrap_or_default()
}

/// The Export dialog's settings, in Lightroom Classic's order (see [`export_body`]). `presets_here`:
/// no preset column beside them (a narrow window), so the Preset menu and the save row are here.
pub(crate) fn export_settings(app: &mut LccApp, ui: &mut egui::Ui, f: ExportForm<'_>, presets_here: bool) {
    use lcc_engine::export::{
        Conflict as K, DestinationMode as D, ExportFormat as F, MetadataPolicy as M, OutputSpace as C, ResizeMode as R, SharpenAmount as A,
        SharpenFor as S,
    };
    let t = Tokens::get(ui.ctx());
    let ExportForm { opts, full_size, resize, preset_name, limit_kb, dir } = f;
    let picked_id = egui::Id::new("export-preset-picked");
    if presets_here {
        let n = app.session.selection.ids.len().max(1);
        crate::dialog_frame::group_begin(ui, None);
        crate::dialog_frame::note(
            ui,
            egui::RichText::new(crate::i18n::tr_format!("{n} photo{}", if n == 1 { "" } else { "s" }, n = n)).color(t.text_dim),
        );
        // Preset: load a built-in or saved set of options into the dialog
        field(ui, "Preset", |ui| {
            let mut chosen = None;
            let cur_picked = ui.data(|d| d.get_temp::<String>(picked_id)).unwrap_or_default();
            let selected = if cur_picked.is_empty() { crate::i18n::tr("Choose…").to_string() } else { cur_picked.clone() };
            let c = crate::widgets::combo_box("exportPreset").width(220.0).selected_text(selected).show_ui(ui, |ui| {
                let mut builtin = true;
                for (p, b) in app.session.all_export_presets() {
                    if builtin && !b {
                        ui.separator();
                    }
                    builtin = b;
                    if ui.selectable_label(cur_picked.eq_ignore_ascii_case(&p.name), crate::i18n::builtin_label(&p.name, b)).clicked() {
                        chosen = Some(p.name);
                    }
                }
            });
            crate::widgets::register(ui.ctx(), "combo:exportPreset", c.response.rect);
            if let Some(name) = chosen {
                load_export_preset(app, &name, opts, full_size, resize, limit_kb, dir);
                ui.data_mut(|d| d.insert_temp(picked_id, name));
            }
        });
    }
    let picked = ui.data(|d| d.get_temp::<String>(picked_id)).unwrap_or_default();
    if let Some(p) = app.session.export_presets.iter().find(|p| p.name.eq_ignore_ascii_case(&picked)).filter(|p| !p.unmapped.is_empty()) {
        let text = crate::i18n::tr_format!("Not carried over from “{}” (configure manually): {}", p.name, p.unmapped.join(", "));
        let r = crate::dialog_frame::note(ui, egui::RichText::new(text).color(t.text_dim));
        crate::widgets::register(ui.ctx(), "label:exportPresetUnmapped", r.rect);
    }

    // ---- Export Location
    // like Lightroom Classic's Export To: a folder asked for when the export starts, one folder, or
    // each photo's own folder (on the web, where originals live in the browser's storage, only one
    // folder: the download)
    const PLACES: [(D, &str); 3] =
        [(D::ChooseLater, "Choose folder later"), (D::SpecificFolder, "Specific folder"), (D::SameAsOriginal, "Same folder as original photo")];
    let place = match opts.destination_mode {
        D::SpecificFolder if dir.trim().is_empty() => crate::i18n::tr("No folder chosen").to_string(),
        D::SpecificFolder => dir.trim().to_string(),
        m => label_of(&PLACES, m),
    };
    let sub = opts.subfolder.trim();
    let summary = if sub.is_empty() { place } else { format!("{place} / {sub}") };
    if export_section(app, ui, "Location", "Export Location", &summary) {
        if !cfg!(target_arch = "wasm32") || opts.destination_mode != D::SpecificFolder {
            field(ui, "Export to", |ui| {
                let c = crate::widgets::combo_box("exportDestinationMode")
                    .width(260.0)
                    .selected_text(label_of(&PLACES, opts.destination_mode))
                    .show_ui(ui, |ui| {
                        for (m, l) in PLACES {
                            ui.selectable_value(&mut opts.destination_mode, m, crate::i18n::tr(l));
                        }
                    });
                crate::widgets::register(ui.ctx(), "combo:exportDestinationMode", c.response.rect);
            });
        }
        match opts.destination_mode {
            D::SpecificFolder => {
                field(ui, "Folder", |ui| {
                    trailing_button_row(ui, |ui| {
                        if app.services.pick_folder.is_some()
                            && crate::widgets::text_button(ui, "exportChooseFolder", crate::i18n::tr("Choose…"), false).clicked()
                            && let Some(pick) = app.services.pick_folder.as_mut()
                            && let Some(d) = pick()
                        {
                            *dir = d;
                        }
                        crate::widgets::named(ui.add(egui::TextEdit::singleline(dir).desired_width(ui.available_width())), crate::i18n::tr("Folder"));
                    });
                });
            }
            D::ChooseLater => {
                crate::dialog_frame::help(ui, "The folder is asked for when the export starts.");
            }
            D::SameAsOriginal => {}
        }
        field(ui, "Subfolder", |ui| {
            crate::widgets::named(
                ui.add(egui::TextEdit::singleline(&mut opts.subfolder).hint_text(crate::i18n::tr("none")).desired_width(f32::INFINITY)),
                crate::i18n::tr("Subfolder"),
            )
        });
        choices(
            ui,
            "If file exists",
            "exportConflict",
            &[(K::Unique, "Add number"), (K::Overwrite, "Overwrite"), (K::Skip, "Skip")],
            &mut opts.conflict,
        );
    }

    // ---- File Naming: the first photo's name as the summary
    let first = app.session.selection.ids.first().copied().or_else(|| app.session.active());
    let example = first.and_then(|id| app.session.catalog.photo(id)).map(|p| opts.file_name_for(p, 1)).unwrap_or_default();
    if export_section(app, ui, "Naming", "File Naming", &example) {
        // Lightroom Classic's File Naming templates; any other template is "Custom"
        let current = EXPORT_NAMING_TEMPLATES.iter().find(|(t, _)| *t == opts.naming).map_or("Custom", |(_, l)| *l);
        field(ui, "Template", |ui| {
            egui::ComboBox::from_id_salt("exportNamingTemplate").selected_text(crate::i18n::tr(current)).show_ui(ui, |ui| {
                for (t, l) in EXPORT_NAMING_TEMPLATES {
                    if ui.selectable_label(opts.naming == *t, crate::i18n::tr(l)).clicked() {
                        opts.naming = (*t).to_string();
                    }
                }
            });
        });
        if opts.naming.contains("{custom}") {
            field(ui, "Custom text", |ui| {
                crate::widgets::named(
                    ui.add(egui::TextEdit::singleline(&mut opts.custom_text).desired_width(f32::INFINITY)),
                    crate::i18n::tr("Custom text"),
                )
            });
        }
        let naming_id = egui::Id::new("export-naming");
        let tags_open = field(ui, "File name", |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let w = crate::import::tag_field_width(ui);
            crate::widgets::named(
                ui.add(
                    egui::TextEdit::singleline(&mut opts.naming)
                        .id(naming_id)
                        .hint_text("{name}-{seq}  ·  {date}  ·  {title}  ·  {folder}")
                        .desired_width(w),
                ),
                crate::i18n::tr("File name"),
            );
            crate::import::tag_toggle(ui, "exportNaming")
        });
        if tags_open {
            crate::import::tag_help(ui, "exportNaming", &mut opts.naming, naming_id);
        }
        crate::import::unknown_tags_warning(ui, &opts.naming);
        naming_presets_row(app, ui, "export", &mut opts.naming);
        let naming = opts.naming.clone();
        text_tag_fields(ui, "export", &naming, &mut opts.texts);
        if opts.naming.contains("{seq") {
            let mut v = opts.start_number as f64;
            if num(ui, &START_NUMBER, &mut v) {
                opts.start_number = v as u32;
            }
        }
    }

    // ---- Video (M14.8): clips in the selection export as movies (their trim and video grade)
    let rendered = opts.format.is_rendered();
    let videos = app.session.selection.ids.iter().filter(|id| app.session.catalog.photo(**id).is_some_and(|p| p.video.is_some())).count();
    if rendered && videos > 0 {
        use lcc_engine::export_video::VideoFormat as V;
        const VIDEO_FORMATS: [(V, &str); 2] = [(V::H264, "H.264 (MP4)"), (V::ProRes, "ProRes (MOV)")];
        if export_section(app, ui, "Video", "Video", &label_of(&VIDEO_FORMATS, opts.video_format)) {
            let note = format!("{videos} · {}", crate::i18n::tr("video clips export as movies, trimmed, with their video edits"));
            crate::dialog_frame::note(ui, egui::RichText::new(note).color(t.text_dim));
            choices(ui, "Video format", "exportVideoFormat", &VIDEO_FORMATS, &mut opts.video_format);
            let mut q = opts.video_quality as f64;
            if num(ui, &VIDEO_QUALITY, &mut q) {
                opts.video_quality = q.clamp(1.0, 100.0) as u8;
            }
            let mut side = opts.video_max_side.map_or(0.0, f64::from);
            if num(ui, &VIDEO_MAX_SIDE, &mut side) {
                opts.video_max_side = (side >= 16.0).then_some(side as u32);
            }
        }
    }

    // ---- File Settings: format (quality) / color space / bit depth
    const FORMATS: [(F, &str); 10] = [
        (F::Jpeg, "JPEG"),
        (F::Png, "PNG"),
        (F::Tiff, "TIFF"),
        (F::Psd, "PSD"),
        (F::Webp, "WebP"),
        (F::Avif, "AVIF"),
        (F::Jxl, "JPEG XL"),
        (F::Dng, "DNG"),
        (F::RenderedDng, "DNG"),
        (F::Original, "Original files"),
    ];
    const SPACES: [(C, &str); 5] =
        [(C::Srgb, "sRGB"), (C::DisplayP3, "P3"), (C::AdobeRgb, "Adobe RGB"), (C::ProPhoto, "ProPhoto"), (C::Rec2020, "Rec.2020")];
    let mut summary = label_of(&FORMATS, opts.format);
    if matches!(opts.format, F::Jpeg | F::Avif) || (opts.format == F::Jxl && !opts.jxl_lossless) {
        summary = format!("{summary} ({}%)", opts.quality);
    }
    if rendered {
        if !(opts.hdr && matches!(opts.format, F::Avif | F::Png | F::Jxl)) && opts.format != F::Avif {
            summary = format!("{summary} / {}", label_of(&SPACES, opts.effective_space()));
        }
        let depths = lcc_engine::export::ExportOptions::bit_depths(opts.format);
        let depth = opts.bit_depth.filter(|b| depths.iter().any(|d| d.0 == *b)).or_else(|| depths.first().map(|d| d.0));
        if let Some(depth) = depth {
            summary = format!("{summary} / {}", label_of(depths, depth));
        }
        if opts.hdr && opts.hdr_output() {
            summary = format!("{summary} / HDR");
        }
    }
    if export_section(app, ui, "FileSettings", "File Settings", &summary) {
        let before = opts.format;
        // "Rendered DNG" is a DNG option (below), not a format of its own in this list
        let mut shown = if opts.format == F::RenderedDng { F::Dng } else { opts.format };
        choices(
            ui,
            "Format",
            "exportFormat",
            &[
                (F::Jpeg, "JPEG"),
                (F::Png, "PNG"),
                (F::Tiff, "TIFF"),
                (F::Psd, "PSD"),
                (F::Webp, "WebP"),
                (F::Avif, "AVIF"),
                (F::Jxl, "JPEG XL"),
                (F::Dng, "DNG"),
                (F::Original, "Original files"),
            ],
            &mut shown,
        );
        if shown != opts.format && !(shown == F::Dng && opts.format == F::RenderedDng) {
            opts.format = shown;
        }
        if matches!(opts.format, F::Dng | F::RenderedDng) {
            choices(ui, "DNG of", "exportDngKind", &[(F::Dng, "Raw data"), (F::RenderedDng, "Rendered photo")], &mut opts.format);
        }
        if !opts.format.is_rendered() {
            let note = if opts.format == F::Dng {
                "Raw photos as DNG, with the edits embedded. Size, color and output options don't apply."
            } else {
                "The original files, unchanged, each with an XMP sidecar holding its edits."
            };
            crate::dialog_frame::note(ui, egui::RichText::new(note).color(t.text_dim));
        }
        if opts.format != before {
            // each format starts at its own default depth (TIFF 16-bit, others 8-bit)
            opts.bit_depth = None;
        }
        let rendered = opts.format.is_rendered();
        let depths = lcc_engine::export::ExportOptions::bit_depths(opts.format);
        if rendered && depths.len() > 1 {
            let mut bd = opts.bit_depth.filter(|b| depths.iter().any(|d| d.0 == *b)).unwrap_or(depths[0].0);
            choices(ui, "Bit depth", "exportBitDepth", depths, &mut bd);
            opts.bit_depth = Some(bd);
        }
        if opts.format == F::Tiff {
            use lcc_engine::export::TiffCompression as Z;
            choices(
                ui,
                "Compression",
                "exportTiffCompression",
                &[(Z::None, "None"), (Z::Lzw, "LZW"), (Z::Deflate, "ZIP")],
                &mut opts.tiff_compression,
            );
            // upstream #2890 (issue #2882 there): LZW stays as picked, with a note on what it costs
            if opts.tiff_compression == Z::Lzw {
                let note = if opts.effective_depth() == lcc_engine::export::OutputDepth::U16 {
                    "At 16 bit, LZW is often larger than no compression, and larger and slower than ZIP. Use it only if other software needs LZW."
                } else {
                    "LZW is usually larger and slower than ZIP, and can be larger than no compression. Use it only if other software needs LZW."
                };
                crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr(note)).color(t.text_dim));
            }
        }
        if opts.format == F::Dng {
            use lcc_engine::export::DngCompression as Z;
            choices(
                ui,
                "Compression",
                "exportDngCompression",
                &[(Z::Lossless, "Lossless"), (Z::Deflate, "ZIP"), (Z::Uncompressed, "None"), (Z::Lossy, "Lossy")],
                &mut opts.dng_compression,
            );
        }
        if matches!(opts.format, F::Jpeg | F::Tiff | F::Avif | F::Png | F::Jxl) {
            let tip = crate::i18n::tr(
                "Photos edited in HDR: JPEG with an HDR gain map (looks right everywhere, brighter highlights on HDR displays), AVIF as 10-bit, PNG and JPEG XL as 16-bit Rec. 2020 PQ; TIFF needs 32-bit float",
            );
            crate::dialog_frame::checkbox(ui, &mut opts.hdr, crate::i18n::tr("HDR output")).on_hover_text(tip);
            if opts.hdr && !opts.hdr_output() {
                crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("HDR TIFF needs 32-bit float.")).color(t.text_dim));
            }
            if opts.hdr && opts.format == F::Avif {
                use lcc_engine::export::HdrCurve as H;
                choices(ui, "HDR curve", "exportHdrCurve", &[(H::Pq, "PQ"), (H::Hlg, "HLG")], &mut opts.hdr_transfer);
            }
        }
        if !rendered {
        } else if opts.format == F::Avif && opts.hdr {
            let label = if opts.hdr_transfer == lcc_engine::export::HdrCurve::Hlg {
                crate::i18n::tr("Color space: Rec. 2020 HLG (HDR AVIF)")
            } else {
                crate::i18n::tr("Color space: Rec. 2020 PQ (HDR AVIF)")
            };
            crate::dialog_frame::note(ui, egui::RichText::new(label).color(t.text_dim));
        } else if opts.format == F::Png && opts.hdr {
            crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("Color space: Rec. 2020 PQ, 16-bit (HDR PNG)")).color(t.text_dim))
                .on_hover_text(crate::i18n::tr("Viewers that don't read the PNG cICP chunk show PQ values as sRGB: flat and washed out. For a picture that looks right everywhere, use JPEG (gain map)."));
        } else if opts.format == F::Jxl && opts.hdr {
            crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("Color space: Rec. 2020 PQ (HDR JPEG XL)")).color(t.text_dim));
        } else if opts.format == F::Avif {
            crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("Color space: sRGB (AVIF)")).color(t.text_dim));
        } else {
            use lcc_engine::export::OutputSpace as C;
            choices(
                ui,
                "Color space",
                "exportColorSpace",
                &[(C::Srgb, "sRGB"), (C::DisplayP3, "P3"), (C::AdobeRgb, "Adobe RGB"), (C::ProPhoto, "ProPhoto"), (C::Rec2020, "Rec.2020")],
                &mut opts.color_space,
            );
            if opts.effective_space() != opts.color_space {
                let note = "Lossy JPEG XL writes Adobe RGB and ProPhoto as Rec. 2020; choose Lossless to keep them.";
                crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr(note)).color(t.text_dim));
            }
        }
        if opts.format == F::Jxl {
            let tip = "Mathematically lossless: every pixel is kept exactly (larger files)";
            crate::dialog_frame::checkbox(ui, &mut opts.jxl_lossless, crate::i18n::tr("Lossless")).on_hover_text(crate::i18n::tr(tip));
        }
        if matches!(opts.format, F::Jpeg | F::Avif) || (opts.format == F::Jxl && !opts.jxl_lossless) {
            let mut q = opts.quality as f64;
            if num(ui, &QUALITY, &mut q) {
                opts.quality = q as u8;
            }
        }
        if opts.format == F::Jxl {
            use lcc_engine::export::JxlEffort as E;
            choices(ui, "Effort", "exportJxlEffort", &[(E::Fast, "Fast"), (E::Normal, "Normal")], &mut opts.jxl_effort);
            let tip = "Shows a coarse version first and refines it as the file loads";
            crate::dialog_frame::checkbox(ui, &mut opts.jxl_progressive, crate::i18n::tr("Progressive")).on_hover_text(crate::i18n::tr(tip));
        }
        if opts.format == F::Jpeg {
            let mut k = *limit_kb as f64;
            if num(ui, &LIMIT_KB, &mut k) {
                *limit_kb = k as u32;
            }
            if *limit_kb > 0 {
                crate::dialog_frame::note(
                    ui,
                    egui::RichText::new(crate::i18n::tr("The size limit picks the highest quality, up to Quality, whose file fits."))
                        .color(t.text_dim),
                );
                let sizes: Vec<(u32, u32)> =
                    crate::export_module::targets(app).iter().filter_map(|id| app.session.catalog.photo(*id)).map(|p| (p.width, p.height)).collect();
                if size_limit_is_tight_for(*limit_kb, *full_size, resize, &sizes) {
                    crate::dialog_frame::note(
                        ui,
                        egui::RichText::new(crate::i18n::tr(
                            "This limit is tight for the largest photo at this size: the quality may drop low enough to show blocks in smooth areas. A smaller size or a larger limit keeps more quality.",
                        ))
                        .color(t.caution),
                    );
                }
            }
        }
    }

    // ---- Image Sizing, Output Sharpening, Metadata, Watermarking: rendered formats only
    let rendered = opts.format.is_rendered();
    if rendered {
        let size = if *full_size {
            crate::i18n::tr("Full size").to_string()
        } else {
            let v = resize.value;
            match resize.mode {
                R::LongEdge => format!("{} {v:.0} px", crate::i18n::tr("Long Edge")),
                R::ShortEdge => format!("{} {v:.0} px", crate::i18n::tr("Short Edge")),
                R::Width => format!("{} {v:.0} px", crate::i18n::tr("Width")),
                R::Height => format!("{} {v:.0} px", crate::i18n::tr("Height")),
                R::Dimensions | R::WidthHeight => format!("{v:.0} × {} px", resize.height),
                R::Megapixels => format!("{v} MP"),
                R::Percent => format!("{v:.0}%"),
            }
        };
        if export_section(app, ui, "Sizing", "Image Sizing", &format!("{size}, {} ppi", opts.ppi)) {
            export_size(ui, full_size, resize, &mut opts.ppi);
        }

        const SHARPEN: [(S, &str); 4] = [(S::None, "None"), (S::Screen, "Screen"), (S::Matte, "Matte"), (S::Glossy, "Glossy")];
        const AMOUNTS: [(A, &str); 3] = [(A::Low, "Low"), (A::Standard, "Standard"), (A::High, "High")];
        let summary = if opts.sharpen == S::None {
            crate::i18n::tr("None").to_string()
        } else {
            format!("{}, {}", label_of(&SHARPEN, opts.sharpen), label_of(&AMOUNTS, opts.sharpen_amount))
        };
        if export_section(app, ui, "Sharpening", "Output Sharpening", &summary) {
            choices(
                ui,
                "Sharpen",
                "exportSharpen",
                &[(S::None, "None"), (S::Screen, "Screen"), (S::Matte, "Matte"), (S::Glossy, "Glossy")],
                &mut opts.sharpen,
            );
            if opts.sharpen != S::None {
                choices(
                    ui,
                    "Amount",
                    "exportSharpenAmount",
                    &[(A::Low, "Low"), (A::Standard, "Standard"), (A::High, "High")],
                    &mut opts.sharpen_amount,
                );
            }
        }

        const METADATA: [(M, &str); 5] = [
            (M::All, "All"),
            (M::AllExceptCamera, "No camera"),
            (M::Copyright, "Copyright"),
            (M::CopyrightContact, "Copyright & Contact"),
            (M::None, "None"),
        ];
        let mut summary = label_of(&METADATA, opts.metadata);
        if opts.remove_location && !matches!(opts.metadata, M::None | M::Copyright | M::CopyrightContact) {
            summary = format!("{summary}, {}", crate::i18n::tr("Remove location info"));
        }
        if export_section(app, ui, "Metadata", "Metadata", &summary) {
            choices(ui, "Metadata", "exportMetadata", &METADATA, &mut opts.metadata);
            if !matches!(opts.metadata, M::None | M::Copyright | M::CopyrightContact) {
                crate::dialog_frame::checkbox(ui, &mut opts.remove_location, crate::i18n::tr("Remove location info"));
            }
        }

        let summary = match &opts.watermark {
            None => crate::i18n::tr("No watermark").to_string(),
            Some(wm) if !wm.image.is_empty() => crate::i18n::tr("Graphic").to_string(),
            Some(wm) => wm.text.lines().next().unwrap_or_default().to_string(),
        };
        if export_section(app, ui, "Watermark", "Watermarking", &summary) {
            let mut wm_on = opts.watermark.is_some();
            if crate::dialog_frame::checkbox(ui, &mut wm_on, crate::i18n::tr("Watermark")).changed() {
                opts.watermark = wm_on.then(|| lcc_engine::export::Watermark { text: "© ".into(), ..Default::default() });
            }
            let wm_shown = opts.watermark.clone();
            if let Some(wm) = &wm_shown {
                watermark_preview(app, ui, wm, (!*full_size).then_some(*resize));
            }
            if let Some(wm) = &mut opts.watermark {
                // text, or a graphic (a logo with transparency)
                let mut graphic = !wm.image.is_empty() || ui.data(|d| d.get_temp::<bool>(egui::Id::new("wm-graphic"))).unwrap_or(false);
                let before = graphic;
                choices(ui, "Style", "exportWmStyle", &[(false, "Text"), (true, "Graphic")], &mut graphic);
                if graphic != before {
                    ui.data_mut(|d| d.insert_temp(egui::Id::new("wm-graphic"), graphic));
                    if !graphic {
                        wm.image.clear();
                    }
                }
                if graphic {
                    field(ui, "Graphic", |ui| {
                        trailing_button_row(ui, |ui| {
                            if app.services.pick_files.is_some()
                                && crate::widgets::text_button(ui, "exportWmChoose", crate::i18n::tr("Choose…"), false).clicked()
                                && let Some(f) = app.services.pick_files.as_mut().and_then(|pick| pick().into_iter().next())
                            {
                                wm.image = f;
                            }
                            crate::widgets::named(
                                ui.add(egui::TextEdit::singleline(&mut wm.image).hint_text("logo.png").desired_width(ui.available_width())),
                                crate::i18n::tr("Image"),
                            );
                        });
                    });
                    let mut width = wm.image_width as f64 * 100.0;
                    if num(ui, &WM_IMAGE_WIDTH, &mut width) {
                        wm.image_width = (width / 100.0) as f32;
                    }
                } else {
                    choices(
                        ui,
                        app.ui.language.tr("Text direction"),
                        "exportWmOrientation",
                        &[(false, app.ui.language.tr("Horizontal text")), (true, app.ui.language.tr("Vertical text"))],
                        &mut wm.vertical,
                    );
                    field(ui, "Text", |ui| {
                        crate::widgets::named(
                            ui.add(
                                egui::TextEdit::multiline(&mut wm.text)
                                    .desired_rows(2)
                                    .hint_text(crate::i18n::tr("© Your Name"))
                                    .desired_width(f32::INFINITY),
                            ),
                            crate::i18n::tr("Text"),
                        )
                    });
                }
                choices(ui, "Position", "exportWmAnchor", &WM_POSITIONS, &mut wm.anchor);
                choices(ui, "Rotate", "exportWmRotation", &[(0u16, "0°"), (90, "90°"), (180, "180°"), (270, "270°")], &mut wm.rotation);
                let mut size = wm.size as f64 * 100.0;
                if !graphic && num(ui, &WM_SIZE, &mut size) {
                    wm.size = (size / 100.0) as f32;
                }
                let mut op = wm.opacity as f64 * 100.0;
                if num(ui, &WM_OPACITY, &mut op) {
                    wm.opacity = (op / 100.0) as f32;
                }
                // like Lightroom Classic: a margin from the side edges and one from the top and bottom
                // edges, each unused (greyed) when the position is centred along that axis
                use lcc_engine::export::Anchor as P;
                let across = !matches!(wm.anchor, P::Top | P::Center | P::Bottom);
                if let Some(v) = crate::widgets::form_slider(ui, &WM_INSET_X, wm.inset_x as f64 * 100.0, across).value {
                    wm.inset_x = (v / 100.0) as f32;
                }
                let down = !matches!(wm.anchor, P::Left | P::Center | P::Right);
                if let Some(v) = crate::widgets::form_slider(ui, &WM_INSET_Y, wm.inset_y as f64 * 100.0, down).value {
                    wm.inset_y = (v / 100.0) as f32;
                }
                if !graphic {
                    crate::dialog_frame::checkbox(ui, &mut wm.shadow, crate::i18n::tr("Shadow"));
                }
            }
            if opts.watermark != wm_shown {
                // the preview above was drawn with the old settings
                ui.ctx().request_repaint();
            }
        }
    }

    // ---- Post-Processing: what happens to the files written
    #[cfg(not(target_arch = "wasm32"))]
    let plugin_destinations = lcc_engine::cmd::plugins::manager().as_ref().is_some_and(|m| !m.export_destinations().is_empty());
    #[cfg(target_arch = "wasm32")]
    let plugin_destinations = false;
    if plugin_destinations || cfg!(target_os = "macos") {
        #[cfg(not(target_arch = "wasm32"))]
        let sent = crate::panels::plugins::chosen_destination().is_some();
        #[cfg(target_arch = "wasm32")]
        let sent = false;
        let summary = match (opts.add_to_photos, sent) {
            (true, _) => crate::i18n::tr("Add to Apple Photos").to_string(),
            (false, true) => crate::i18n::tr("Then send to").to_string(),
            (false, false) => crate::i18n::tr("Do nothing").to_string(),
        };
        if export_section(app, ui, "PostProcessing", "Post-Processing", &summary) {
            // G19: a plug-in export destination the written files are handed to
            #[cfg(not(target_arch = "wasm32"))]
            crate::panels::plugins::export_destination_ui(ui);
            // After export: Apple Photos imports the files written (macOS only)
            #[cfg(target_os = "macos")]
            {
                let r = crate::dialog_frame::checkbox(ui, &mut opts.add_to_photos, crate::i18n::tr("Add to Apple Photos"));
                crate::widgets::register(ui.ctx(), "exportAddToPhotos", r.rect);
                if opts.add_to_photos {
                    field(ui, "Collection", |ui| {
                        crate::widgets::named(
                            ui.add(
                                egui::TextEdit::singleline(&mut opts.photos_album).hint_text(crate::i18n::tr("none")).desired_width(f32::INFINITY),
                            ),
                            crate::i18n::tr("Collection"),
                        )
                    });
                    crate::dialog_frame::note(
                        ui,
                        egui::RichText::new(crate::i18n::tr("The first time, macOS asks whether {app} may control Photos."))
                            .size(11.0)
                            .color(t.text_dim),
                    );
                }
            }
        }
    }
    // nowhere to add them: a setting carried over from a Mac is dropped, not refused
    #[cfg(not(target_os = "macos"))]
    {
        opts.add_to_photos = false;
    }

    if presets_here {
        crate::dialog_frame::group_begin(ui, None);
        field(ui, "Save preset", |ui| {
            trailing_button_row(ui, |ui| {
                let named = !preset_name.trim().is_empty();
                if crate::widgets::text_button(ui, "exportSavePreset", crate::i18n::tr("Save"), false).clicked() && named {
                    let params = preset_params(opts, *full_size, resize, *limit_kb, dir);
                    match app.run("export.savePreset", json!({"name": preset_name.trim(), "params": params})) {
                        Ok(_) => {
                            app.toast(ui.ctx(), crate::i18n::tr_format!("Saved export preset “{}”", preset_name.trim()));
                            preset_name.clear();
                        }
                        Err(e) => app.toast(ui.ctx(), e),
                    }
                }
                crate::widgets::named(
                    ui.add(egui::TextEdit::singleline(preset_name).hint_text(crate::i18n::tr("Preset name")).desired_width(ui.available_width())),
                    crate::i18n::tr("Preset name"),
                );
            });
        });
    }
}

// ------------------------------------------------------------------------------------------ dialog widgets

/// The per-setting tree of Copy Settings, Paste Selected, Sync, New Preset and Update Preset
/// (LR-PRE-CREATE; Lightroom's checklist): a box per settings group that ticks the whole group and
/// shows a partial state when only some of its settings are ticked, and under it (opened with the
/// arrow) a box per setting. `ids` holds the ticked leaves ([`SettingsPick::to_ids`]); `offered`
/// limits the boxes that can be ticked (Paste Selected: what was copied). Widget ids: `<tag>:<group>`,
/// `<tag>:<setting>`, `<tag>:open:<group>`, `<tag>:all`, `<tag>:none`. → `Some(true/false)` when
/// Check All / Check None was pressed.
fn group_checklist(ui: &mut egui::Ui, tag: &str, ids: &mut Vec<String>, offered: Option<&SettingsPick>) -> Option<bool> {
    // (ids from an older ui.json may name whole groups: one tick per setting from here on)
    let normal = SettingsPick::from_ids(ids).to_ids();
    if normal != *ids {
        *ids = normal;
    }
    let can = |id: &str| offered.is_none_or(|o| o.has(id));
    let leaves =
        |g: SettingsGroup| -> Vec<&'static str> { if g.items().is_empty() { vec![g.id()] } else { g.items().iter().map(|i| i.id).collect() } };
    let set = |ids: &mut Vec<String>, id: &str, on: bool| {
        if on {
            if !ids.iter().any(|x| x == id) {
                ids.push(id.to_string());
            }
        } else {
            ids.retain(|x| x != id);
        }
    };
    let t = Tokens::get(ui.ctx());
    ui.columns(2, |cols| {
        // top to bottom, then the second column, in the panels' order
        let half = SettingsGroup::ALL.len().div_ceil(2);
        for (i, g) in SettingsGroup::ALL.into_iter().enumerate() {
            let Some(ui) = cols.get_mut(usize::from(i >= half)) else { continue };
            let mine: Vec<&str> = leaves(g).into_iter().filter(|id| can(id)).collect();
            let ticked = mine.iter().filter(|id| ids.iter().any(|x| x == *id)).count();
            let open_id = egui::Id::new(("settings-tree-open", tag, g.id()));
            let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if g.items().is_empty() {
                    ui.add_space(18.0);
                } else {
                    let (rect, arrow) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
                    let colour = if arrow.hovered() { t.text } else { t.text_dim };
                    let icon = if open { crate::icons::Icon::ChevronDown } else { crate::icons::Icon::ChevronRight };
                    crate::icons::paint(ui.painter(), egui::Rect::from_center_size(rect.center(), egui::vec2(14.0, 14.0)), icon, colour);
                    let arrow = arrow.on_hover_text(crate::i18n::tr(if open { "Hide the single settings" } else { "Show the single settings" }));
                    crate::access::button(&arrow, if open { "Hide the single settings" } else { "Show the single settings" });
                    crate::widgets::register(ui.ctx(), format!("{tag}:open:{}", g.id()), arrow.rect);
                    if arrow.clicked() {
                        open = !open;
                        ui.data_mut(|d| d.insert_temp(open_id, open));
                    }
                }
                let mut on = !mine.is_empty() && ticked == mine.len();
                let partial = ticked > 0 && ticked < mine.len();
                let r = ui.add_enabled(!mine.is_empty(), egui::Checkbox::new(&mut on, crate::i18n::tr(g.label())).indeterminate(partial));
                crate::widgets::register(ui.ctx(), format!("{tag}:{}", g.id()), r.rect);
                if r.changed() {
                    // a partly ticked group ticks all of it, as in Lightroom
                    let on = partial || on;
                    for id in &mine {
                        set(ids, id, on);
                    }
                }
            });
            if open {
                for it in g.items() {
                    ui.horizontal(|ui| {
                        ui.add_space(38.0);
                        let mut on = ids.iter().any(|x| x == it.id);
                        let r = ui.add_enabled(can(it.id), egui::Checkbox::new(&mut on, crate::i18n::tr(it.label)));
                        crate::widgets::register(ui.ctx(), format!("{tag}:{}", it.id), r.rect);
                        if r.changed() {
                            set(ids, it.id, on);
                        }
                    });
                }
            }
        }
    });
    ui.horizontal(|ui| {
        let all = ui.small_button(crate::i18n::tr("Check All"));
        crate::widgets::register(ui.ctx(), format!("{tag}:all"), all.rect);
        if all.clicked() {
            *ids = SettingsGroup::ALL.into_iter().flat_map(leaves).filter(|id| can(id)).map(str::to_string).collect();
            return Some(true);
        }
        let none = ui.small_button(crate::i18n::tr("Check None"));
        crate::widgets::register(ui.ctx(), format!("{tag}:none"), none.rect);
        if none.clicked() {
            ids.clear();
            return Some(false);
        }
        ui.label(egui::RichText::new(crate::i18n::tr("Open a group with its arrow to pick single settings.")).small().color(t.text_dim));
        None
    })
    .inner
}

/// The ticked leaves of the tree as the command parameters (`groups`, `settings`).
fn pick_params(ids: &[String]) -> serde_json::Value {
    let pick = SettingsPick::from_ids(ids);
    json!({"groups": pick.groups, "settings": pick.settings})
}

/// Width of the label column in dialogs.
const LABEL_W: f32 = 78.0;
/// The label column of the dialogs laid out as a form (Export, Rename, …).
const FORM_LABEL_W: f32 = 112.0;

const fn spec(id: &'static str, label: &'static str, min: f64, max: f64, default: f64, step: f64) -> ControlSpec {
    ControlSpec { id, label, section: Section::Light, min, max, default, step, decimals: 0, track: Track::Plain, shade: lcc_develop::Shade::Plain }
}
const QUALITY: ControlSpec = spec("export.quality", "Quality", 1.0, 100.0, 90.0, 1.0);
const VIDEO_QUALITY: ControlSpec = spec("export.videoQuality", "Video quality", 1.0, 100.0, 80.0, 1.0);
const VIDEO_MAX_SIDE: ControlSpec = spec("export.videoMaxSide", "Video long edge (px, 0 = full)", 0.0, 8192.0, 0.0, 16.0);
const LIMIT_KB: ControlSpec = spec("export.limitKb", "Limit file size (KB, 0 = off)", 0.0, 20_000.0, 0.0, 10.0);
const SIZE_PX: ControlSpec = spec("export.sizePx", "Pixels", 16.0, 20_000.0, 2048.0, 1.0);
const SIZE_W: ControlSpec = spec("export.sizeW", "Width (px)", 16.0, 20_000.0, 2048.0, 1.0);
const SIZE_H: ControlSpec = spec("export.sizeH", "Height (px)", 16.0, 20_000.0, 2048.0, 1.0);
const SIZE_MP: ControlSpec = ControlSpec { decimals: 1, ..spec("export.sizeMp", "Megapixels", 0.1, 100.0, 12.0, 0.1) };
const SIZE_PCT: ControlSpec = spec("export.sizePercent", "Percent", 1.0, 400.0, 50.0, 1.0);
const START_NUMBER: ControlSpec = spec("export.startNumber", "Start number", 1.0, 9999.0, 1.0, 1.0);
const PPI: ControlSpec = spec("export.ppi", "Resolution (ppi)", 1.0, 1200.0, 240.0, 1.0);

/// Image Sizing: full size, or a resize mode and its value(s), don't enlarge, ppi.
fn export_size(ui: &mut egui::Ui, full: &mut bool, r: &mut lcc_engine::export::Resize, ppi: &mut u16) {
    use lcc_engine::export::ResizeMode as R;
    const MODES: [(R, &str); 8] = [
        (R::LongEdge, "Long Edge"),
        (R::ShortEdge, "Short Edge"),
        (R::Width, "Width"),
        (R::Height, "Height"),
        (R::WidthHeight, "Width & Height"),
        (R::Dimensions, "Dimensions"),
        (R::Megapixels, "Megapixels"),
        (R::Percent, "Percentage"),
    ];
    let r_full = crate::dialog_frame::checkbox(ui, full, crate::i18n::tr("Full size"));
    crate::widgets::register(ui.ctx(), "check:exportFullSize", r_full.rect);
    if !*full {
        field(ui, "Resize to", |ui| {
            let cur = MODES.iter().find(|m| m.0 == r.mode).map_or("Long Edge", |m| m.1);
            let before = r.mode;
            let c = crate::widgets::combo_box("exportResizeMode").width(150.0).selected_text(crate::i18n::tr(cur)).show_ui(ui, |ui| {
                for (m, l) in MODES {
                    ui.selectable_value(&mut r.mode, m, crate::i18n::tr(l));
                }
            });
            crate::widgets::register(ui.ctx(), "combo:exportResizeMode", c.response.rect);
            if r.mode != before {
                // a sensible value for the new unit
                r.value = match r.mode {
                    R::Megapixels => 12.0,
                    R::Percent => 50.0,
                    _ if matches!(before, R::Megapixels | R::Percent) => 2048.0,
                    _ => r.value,
                };
                if matches!(r.mode, R::Dimensions | R::WidthHeight) && r.height == 0 {
                    r.height = r.value as u32;
                }
            }
        });
        let mut v = r.value as f64;
        let spec = match r.mode {
            R::Megapixels => &SIZE_MP,
            R::Percent => &SIZE_PCT,
            R::Dimensions | R::WidthHeight => &SIZE_W,
            _ => &SIZE_PX,
        };
        if num(ui, spec, &mut v) {
            r.value = v as f32;
        }
        if matches!(r.mode, R::Dimensions | R::WidthHeight) {
            let mut h = r.height as f64;
            if num(ui, &SIZE_H, &mut h) {
                r.height = h as u32;
            }
        }
        let c = crate::dialog_frame::checkbox(ui, &mut r.dont_enlarge, crate::i18n::tr("Don't enlarge"));
        crate::widgets::register(ui.ctx(), "check:exportDontEnlarge", c.rect);
    }
    let mut p = *ppi as f64;
    if num(ui, &PPI, &mut p) {
        *ppi = p as u16;
    }
}
/// All nine positions supported by both the text and graphic watermark renderer.
/// Keep them in grid order so bottom-centre and edge-centre anchors are available in Export.
const WM_POSITIONS: [(lcc_engine::export::Anchor, &str); 9] = {
    use lcc_engine::export::Anchor::*;
    [(TopLeft, "↖"), (Top, "↑"), (TopRight, "↗"), (Left, "←"), (Center, "•"), (Right, "→"), (BottomLeft, "↙"), (Bottom, "↓"), (BottomRight, "↘")]
};

const WM_IMAGE_WIDTH: ControlSpec = spec("export.watermarkImageWidth", "Width (% of photo)", 2.0, 100.0, 20.0, 1.0);
const WM_SIZE: ControlSpec = spec("export.watermarkSize", "Size (% of short edge)", 1.0, 15.0, 3.5, 0.5);
const WM_OPACITY: ControlSpec = spec("export.watermarkOpacity", "Opacity (%)", 5.0, 100.0, 70.0, 1.0);
const WM_INSET_X: ControlSpec =
    ControlSpec { decimals: 1, ..spec("export.watermarkInsetX", "Horizontal inset (% of short edge)", 0.0, 40.0, 2.5, 0.5) };
const WM_INSET_Y: ControlSpec =
    ControlSpec { decimals: 1, ..spec("export.watermarkInsetY", "Vertical inset (% of short edge)", 0.0, 40.0, 2.5, 0.5) };

/// A themed slider row editing `v`; true when it changed.
fn num(ui: &mut egui::Ui, spec: &ControlSpec, v: &mut f64) -> bool {
    // (in a form, its name in the label column like the other rows: room for the whole name)
    let out = if crate::dialog_frame::in_form(ui) {
        crate::widgets::form_slider(ui, spec, *v, true).value
    } else {
        crate::dialog_frame::in_control_column(ui, |ui| crate::widgets::slider(ui, spec, *v, true, None).value)
    };
    match out {
        Some(n) => {
            *v = n;
            true
        }
        None => false,
    }
}

/// A labelled row (fixed label column).
/// View ▸ Export Slideshow…: what to write (PDF, JPEG sequence, video) and how big; where comes
/// after (a save dialog).
/// Choose Overlays to Cycle…: one checkbox per crop guide; the last ticked one can't be cleared.
fn crop_overlays_body(ui: &mut egui::Ui, picked: &mut Vec<crate::state::CropOverlay>) {
    use crate::state::CropOverlay as O;
    let t = Tokens::get(ui.ctx());
    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr("The crop guides that Cycle Crop Overlay steps through.")).color(t.text_dim));
    let names = [
        (O::Thirds, "Rule of Thirds"),
        (O::Grid, "Grid"),
        (O::Golden, "Golden Ratio"),
        (O::Diagonal, "Diagonal"),
        (O::Triangle, "Triangle"),
        (O::Spiral, "Golden Spiral"),
        (O::None, "None (hide the guides)"),
    ];
    for (o, name) in names {
        let mut on = picked.contains(&o);
        let only = on && picked.len() == 1;
        let r = ui.add_enabled_ui(!only, |ui| crate::dialog_frame::checkbox(ui, &mut on, crate::i18n::tr(name))).inner;
        crate::widgets::register(ui.ctx(), format!("check:cropOverlay-{o:?}"), r.rect);
        if on && !picked.contains(&o) {
            picked.push(o);
        } else if !on && picked.len() > 1 {
            picked.retain(|x| *x != o);
        }
    }
}

/// Book Preferences…: what a new book starts with.
fn book_prefs_body(ui: &mut egui::Ui, prefs: &mut crate::book::BookPrefs) {
    choices(ui, "Photo zoom", "bookPrefsZoom", &[(true, "Fill cell"), (false, "Fit photo")], &mut prefs.zoom_to_fill);
    let rows: [(&mut bool, &str, &str); 3] = [
        (&mut prefs.autofill, "Auto-fill new books with the selected photos", "bookPrefsAutofill"),
        (&mut prefs.filler_text, "Show filler text in empty text cells", "bookPrefsFiller"),
        (&mut prefs.caption_safe_area, "Show the caption safe area", "bookPrefsSafe"),
    ];
    for (value, label, id) in rows {
        let r = crate::dialog_frame::checkbox(ui, value, crate::i18n::tr(label));
        crate::widgets::register(ui.ctx(), format!("check:{id}"), r.rect);
    }
}

fn slideshow_export_body(app: &mut LccApp, ui: &mut egui::Ui, settings: &mut lcc_engine::slideshow_export::ExportSettings) {
    use lcc_engine::slideshow_export::{ExportSettings, Format};
    let t = Tokens::get(ui.ctx());
    let photos = app.session.visible_cloned().len();
    let slide = app.session.slideshow.slide_duration;
    let (intro, ending) = app.session.slide_titles.durations(slide);
    let titles = usize::from(intro.is_some()) + usize::from(ending.is_some());
    let seconds = photos as f64 * slide + intro.unwrap_or(0.0) + ending.unwrap_or(0.0);
    let summary = crate::i18n::tr_format!(
        "{n} photos in view · {slides} slides · {time}",
        n = photos,
        slides = photos + titles,
        time = format!("{}:{:02}", (seconds / 60.0).floor() as u64, (seconds % 60.0).round() as u64)
    );
    crate::dialog_frame::note(ui, egui::RichText::new(summary).color(t.text_dim));
    let before = settings.format;
    choices(ui, "Format", "slideshowExportFormat", &[(Format::Pdf, "PDF"), (Format::Jpeg, "JPEG"), (Format::Video, "Video")], &mut settings.format);
    if settings.format != before {
        *settings = ExportSettings { full_screen: settings.full_screen, ..ExportSettings::for_format(settings.format) };
    }
    match settings.format {
        Format::Video => {
            choices(ui, "Size", "slideshowExportSize", &[(720, "720p"), (1080, "1080p")], &mut settings.size);
            choices(ui, "Frame rate", "slideshowExportFps", &[(24, "24 fps"), (30, "30 fps")], &mut settings.fps);
            let mut webm = settings.container == "webm";
            choices(ui, "Container", "slideshowExportContainer", &[(false, "MP4"), (true, "WebM")], &mut webm);
            settings.container = if webm { "webm".into() } else { "mp4".into() };
        }
        _ => {
            choices(ui, "Size", "slideshowExportSize", &[(1024, "1024 px"), (1920, "1920 px"), (3840, "3840 px")], &mut settings.size);
        }
    }
    field(ui, "Quality", |ui| {
        let r = ui.add(egui::Slider::new(&mut settings.quality, 1..=100));
        crate::widgets::register(ui.ctx(), "slider:slideshowExportQuality", r.rect);
    });
    let note = match settings.format {
        Format::Pdf => {
            let r = crate::dialog_frame::checkbox(ui, &mut settings.full_screen, crate::i18n::tr("Automatically show full screen"));
            crate::widgets::register(ui.ctx(), "check:slideshowExportFullScreen", r.rect);
            "One page per slide (the long side in pixels = points), with the slide duration and fade for the viewer's presentation mode."
        }
        Format::Jpeg => "Numbered JPEG files (name-001.jpg…) in a new folder named in the next step.",
        Format::Video => "AV1 video, each slide held for its duration with its fades. No sound: music is not exported yet.",
    };
    crate::dialog_frame::note(ui, egui::RichText::new(crate::i18n::tr(note)).color(t.text_dim));
    crate::dialog_frame::note(
        ui,
        egui::RichText::new(crate::i18n::tr("Slides use the slideshow's layout, overlays and intro and ending screens, in view order."))
            .color(t.text_dim),
    );
}

fn field<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    if crate::dialog_frame::in_form(ui) {
        return crate::dialog_frame::form_row(ui, label, add);
    }
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(LABEL_W, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_width(LABEL_W);
            ui.label(egui::RichText::new(crate::i18n::tr(label)).color(t.text_label));
        });
        add(ui)
    })
    .inner
}

/// The rest of a [`field`] row, laid out right to left: `add` places its trailing button(s) first,
/// then a text field taking exactly the width that's left. (Subtracting a guessed button width
/// instead made the auto-sized dialog grow every frame when the real button was wider, #8.)
fn trailing_button_row(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let h = ui.spacing().interact_size.y.max(24.0);
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), h), egui::Layout::right_to_left(egui::Align::Center), add);
}

/// A labelled row of mutually exclusive choice buttons (ids `button:{id}-{index}`).
/// The Export dialog's File Naming templates (Lightroom Classic's), as `naming` templates.
const EXPORT_NAMING_TEMPLATES: &[(&str, &str)] = &[
    ("{name}", "Filename"),
    ("{name}-{seq:1}", "Filename - Sequence"),
    ("{custom}-{seq:1}", "Custom Name - Sequence"),
    ("{custom}", "Custom Name"),
    ("{date:%Y%m%d}-{name}", "Date - Filename"),
];

fn choices<V: PartialEq + Copy>(ui: &mut egui::Ui, label: &str, id: &str, options: &[(V, &str)], value: &mut V) {
    field(ui, label, |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, (v, l)) in options.iter().enumerate() {
            if crate::widgets::text_button(ui, &format!("{id}-{i}"), l, *value == *v).clicked() {
                *value = *v;
            }
        }
    });
}

/// Dialogs that Return confirms with nothing focused (those with text fields confirm from the
/// field: `dialog_field`).
fn return_confirms(dlg: &Dialog) -> bool {
    matches!(
        dlg,
        Dialog::CopySettings { .. }
            | Dialog::PasteSettings { .. }
            | Dialog::SyncSettings { .. }
            | Dialog::SyncCopies { .. }
            | Dialog::SyncSnapshots { .. }
            | Dialog::UpdatePreset { .. }
    )
}

struct PersonRenamePlan {
    faces: usize,
    needs_merge: bool,
    /// Why Rename stays disabled. `None` when the typed name can be applied.
    block: Option<&'static str>,
}

/// What renaming `from` to `to` would do, using the same rules as `faces.renamePerson`.
fn person_rename_plan(cat: &lcc_catalog::Catalog, from: &str, to: &str) -> PersonRenamePlan {
    let to_trim = to.trim();
    if to_trim.is_empty() {
        return PersonRenamePlan { faces: 0, needs_merge: false, block: Some("Type a name") };
    }
    if to_trim.chars().count() > 200 || to_trim.chars().any(char::is_control) {
        return PersonRenamePlan { faces: 0, needs_merge: false, block: Some("A name is at most 200 characters, without control characters") };
    }
    let from_key = from.trim().to_lowercase();
    let to_key = to_trim.to_lowercase();
    let mut matched = 0usize;
    let mut faces = 0usize;
    let mut needs_merge = false;
    if !from_key.is_empty() {
        for photo in cat.photos().filter(|p| p.in_library()) {
            for region in photo.meta.regions.iter().filter(|r| r.kind == lcc_meta::RegionKind::Face) {
                let Some(name) = region.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) else { continue };
                let key = name.to_lowercase();
                if key == from_key {
                    matched += 1;
                    if region.name.as_deref() != Some(to_trim) {
                        faces += 1;
                    }
                } else if key == to_key {
                    needs_merge = true;
                }
            }
        }
    }
    let block = if matched == 0 {
        Some("No person by that name")
    } else if faces == 0 {
        Some("That is already their name")
    } else {
        None
    };
    // a case-only change is the same person, not a merge
    PersonRenamePlan { faces, needs_merge: needs_merge && from_key != to_key, block }
}

/// A dialog's one text field: it takes the focus when the dialog opens, and opens with its text
/// selected, so typing replaces it. It takes the focus back only while nothing else has it: taking
/// it back every frame would keep Tab from ever reaching the dialog's buttons and check boxes.
/// True when Return confirms the dialog; Esc cancels the dialog (`show`).
/// Naming presets: the built-in templates, then the ones saved (right-click one to delete it), a
/// "+ Number" that puts `{seq}` at the end, and Save as Preset… for the template typed.
fn naming_presets_row(app: &mut LccApp, ui: &mut egui::Ui, tag: &str, template: &mut String) {
    let presets: Vec<(String, String, bool)> = lcc_engine::cmd::manage::builtin_naming_presets()
        .into_iter()
        .map(|p| (p.name, p.template, true))
        .chain(app.session.naming_presets.iter().map(|p| (p.name.clone(), p.template.clone(), false)))
        .collect();
    let mut delete: Option<String> = None;
    let save_id = egui::Id::new(("naming-save-as", tag));
    let mut save_as: Option<String> = ui.data(|d| d.get_temp(save_id));
    field(ui, "Presets", |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (i, (name, tpl, builtin)) in presets.iter().enumerate() {
                let r =
                    crate::widgets::text_button(ui, &format!("{tag}Preset-{i}"), crate::i18n::tr(name), template == tpl).on_hover_text(tpl.as_str());
                if r.clicked() {
                    *template = tpl.clone();
                }
                if !builtin {
                    r.context_menu(|ui| {
                        if ui.button(crate::i18n::tr("Delete Preset")).clicked() {
                            delete = Some(name.clone());
                            ui.close();
                        }
                    });
                }
            }
            let numbered = template.contains("{seq") || template.contains("{n}") || template.contains("{n:");
            if crate::widgets::text_button(ui, &format!("{tag}Number"), crate::i18n::tr("+ Number"), false)
                .on_hover_text(crate::i18n::tr("Add {seq}: 1, 2, 3… in the order the photos are shown"))
                .clicked()
                && !numbered
            {
                let t = template.trim_end();
                let sep = if t.is_empty() || t.ends_with(['_', '-', ' ', '.']) { "" } else { "_" };
                *template = format!("{t}{sep}{{seq}}");
            }
            if crate::widgets::text_button(ui, &format!("{tag}NamingSaveAs"), crate::i18n::tr("Save as Preset…"), save_as.is_some()).clicked() {
                save_as = if save_as.is_some() { None } else { Some(String::new()) };
            }
        });
    });
    if let Some(name) = delete
        && let Err(e) = app.run("naming.deletePreset", json!({"name": name}))
    {
        app.toast(ui.ctx(), e);
    }
    if let Some(name) = save_as.as_mut() {
        let mut save = false;
        field(ui, "Preset name", |ui| {
            let r = crate::text_field::TextField::singleline(&format!("field:{tag}NamingPresetName"), name)
                .hint(crate::i18n::tr("Preset name"))
                .width(220.0)
                .show(ui);
            let named = !name.trim().is_empty() && !template.trim().is_empty();
            if (crate::widgets::text_button(ui, &format!("{tag}NamingSave"), crate::i18n::tr("Save"), false).clicked()
                || r.ending == Some(crate::text_field::Ending::Return))
                && named
            {
                save = true;
            }
        });
        if save {
            match app.run("naming.savePreset", json!({"name": name.trim(), "template": template.trim()})) {
                Ok(_) => save_as = None,
                Err(e) => app.toast(ui.ctx(), e),
            }
        }
    }
    ui.data_mut(|d| match save_as {
        Some(v) => {
            d.insert_temp(save_id, v);
        }
        None => d.remove::<String>(save_id),
    });
}

/// A text field for each `{text:Label}` of `template`, asked at each rename or export.
fn text_tag_fields(ui: &mut egui::Ui, tag: &str, template: &str, texts: &mut std::collections::BTreeMap<String, String>) {
    let labels = lcc_engine::rename::text_labels(template);
    texts.retain(|k, _| labels.contains(k));
    for (i, label) in labels.iter().enumerate() {
        let mut v = texts.get(label).cloned().unwrap_or_default();
        field(ui, label, |ui| {
            crate::text_field::TextField::singleline(&format!("field:{tag}Text-{i}"), &mut v)
                .hint(crate::i18n::tr("typed for this time only"))
                .width(220.0)
                .show(ui);
        });
        texts.insert(label.clone(), v);
    }
}

fn dialog_field(ui: &mut egui::Ui, widget: &str, text: &mut String, hint: &str) -> bool {
    let r = crate::text_field::TextField::singleline(widget, text).hint(crate::i18n::tr(hint)).width(f32::INFINITY).select_on_focus(true).show(ui);
    // (and takes it back when nothing has it: after a click on the dialog's background or a check box,
    // or when Return gave it up on a refusal; a control Tab reached keeps it)
    if just_opened(ui, widget) || (!r.editing && r.ending.is_none() && ui.memory(|m| m.focused().is_none())) {
        r.response.request_focus();
    }
    r.ending == Some(crate::text_field::Ending::Return)
}

/// The dialog part `key` is drawn this frame but wasn't the frame before: the dialog has just
/// opened (to give its first field the focus once).
fn just_opened(ui: &egui::Ui, key: &str) -> bool {
    let id = egui::Id::new(("dialog-shown", key));
    let pass = ui.ctx().cumulative_pass_nr();
    let last: Option<u64> = ui.data_mut(|d| d.get_temp(id));
    ui.data_mut(|d| d.insert_temp(id, pass));
    last.and_then(|l| l.checked_add(1)) != Some(pass)
}

#[cfg(test)]
mod watermark_position_tests {
    use super::WM_POSITIONS;
    use lcc_engine::export::Anchor;

    #[test]
    fn export_dialog_offers_every_rendered_anchor_exactly_once() {
        let anchors = [
            Anchor::TopLeft,
            Anchor::Top,
            Anchor::TopRight,
            Anchor::Left,
            Anchor::Center,
            Anchor::Right,
            Anchor::BottomLeft,
            Anchor::Bottom,
            Anchor::BottomRight,
        ];
        assert_eq!(WM_POSITIONS.len(), anchors.len());
        for anchor in anchors {
            assert_eq!(WM_POSITIONS.iter().filter(|(position, _)| *position == anchor).count(), 1);
        }
    }
}

/// How a dialog's confirm button looks: [`ConfirmStyle::Primary`] (the accent's fill) when it
/// makes or brings in something, plain when it deletes or removes (never the eye-catching
/// default) or when the dialog only shows something (its Close).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmStyle {
    Primary,
    Plain,
}

pub fn confirm_style(dlg: &Dialog) -> ConfirmStyle {
    match dlg {
        Dialog::ConfirmDelete { .. }
        | Dialog::ConfirmPurge { .. }
        | Dialog::DeleteKeyword { .. }
        | Dialog::DeleteEffects { .. }
        | Dialog::RemoveFolder { .. }
        | Dialog::DeleteRejected { .. }
        | Dialog::StopWaiting { .. }
        | Dialog::VignettingCreateAnyway { .. }
        | Dialog::VignettingReplace { .. } => ConfirmStyle::Plain,
        _ if only_shows(dlg) => ConfirmStyle::Plain,
        _ => ConfirmStyle::Primary,
    }
}

/// A dialog that only shows something: one Close, nothing to confirm or cancel. (Synchronize
/// Folder is one when its folder turned out up to date.)
fn only_shows(dlg: &Dialog) -> bool {
    matches!(
        dlg,
        Dialog::About
            | Dialog::Shortcuts
            | Dialog::ViewOptions
            | Dialog::Settings { .. }
            | Dialog::SystemInfo { .. }
            | Dialog::DngValidation { .. }
            | Dialog::AllMetadata { .. }
            | Dialog::WhatsNew
            | Dialog::ManageEffects { .. }
            // each model has its own Download button in the body
            | Dialog::AiModels { .. }
            | Dialog::PresetsManager { .. }
            | Dialog::ProfileManager { .. }
            // the updater has its own buttons in the body; the footer only closes
            | Dialog::Update
    ) || matches!(dlg, Dialog::SynchronizeFolder { counts: Some(c), .. } if c.up_to_date())
}

#[cfg(test)]
mod gap_tests {
    use super::{fmt_gap, parse_gap};

    #[test]
    fn gap_text_reads_back_what_it_shows() {
        for v in [0.0, 1.0, 12.0, 59.0, 60.0, 90.0, 600.0, 3599.0, 3600.0, 3601.0, 3661.0, 3900.0, 7325.0, 86340.0, 86399.0] {
            assert_eq!(parse_gap(&fmt_gap(v)), Some(v), "{}", fmt_gap(v));
        }
        assert_eq!(parse_gap("1 day"), Some(86400.0));
        assert_eq!(parse_gap("45"), Some(45.0));
        assert_eq!(parse_gap(" 45s "), Some(45.0));
        assert_eq!(parse_gap("2min 5s"), Some(125.0));
        for bad in ["", "abc", "5 parsecs", "NaN", "1 min x"] {
            assert_eq!(parse_gap(bad), None, "{bad:?}");
        }
        // a day reads back in the UI language too ("1 Tag")
        crate::i18n::set_language(crate::i18n::Locale::De);
        let shown = fmt_gap(86400.0);
        let back = parse_gap(&shown);
        crate::i18n::set_language(crate::i18n::Locale::En);
        assert_eq!((shown.as_str(), back), ("1 Tag", Some(86400.0)));
    }

    #[test]
    fn gaps_past_the_slider_read_back_too() {
        // the Assisted Culling slider drags to 10 min; its value field takes up to a day
        for (v, text) in [(1800.0, "30 min"), (5400.0, "1 h 30 min"), (3601.0, "1 h 0 min 1 s"), (86400.0, "1 day")] {
            assert_eq!(fmt_gap(v), text);
            assert_eq!(parse_gap(text), Some(v), "{text}");
        }
        assert_eq!(parse_gap("1800"), Some(1800.0));
        assert_eq!(parse_gap("0"), Some(0.0));
    }
}

#[cfg(test)]
mod cull_dialog_tests {
    use std::time::Duration;

    use serde_json::json;

    use super::{clamp_cull_burst, cull_params};
    use crate::headless::Headless;
    use crate::state::Dialog;
    use crate::{LccApp, Services};

    const T: Duration = Duration::from_secs(20);

    #[test]
    fn typed_values_are_kept_within_what_photo_analyze_accepts() {
        for (gap, sim) in [(0.0, 0.0), (1800.0, 1.0), (86400.0, 0.5), (0.5, 0.995)] {
            let (mut g, mut s) = (gap, sim);
            clamp_cull_burst(&mut g, &mut s);
            assert_eq!((g, s), (gap, sim), "in range: kept as typed");
        }
        for ((gap, sim), want) in [((-5.0, -0.1), (0.0, 0.0)), ((90000.0, 1.5), (86400.0, 1.0)), ((f32::NAN, f32::INFINITY), (10.0, 0.93))] {
            let (mut g, mut s) = (gap, sim);
            clamp_cull_burst(&mut g, &mut s);
            assert_eq!((g, s), want);
        }
        assert_eq!(
            cull_params(0.0, true, true, false, 1800.0, 1.0),
            json!({"pickBest": true, "applyRatings": true, "burstGap": 1800.0, "similarity": 1.0})
        );
        assert_eq!(cull_params(20.0, false, false, true, 0.0, 0.7)["rejectBelow"], 20.0);
        assert_eq!(cull_params(20.0, false, false, true, 0.0, 0.7)["applyFlags"], true);
    }

    /// Click the value field right of a slider registered as `id`, type `text`, Enter.
    fn type_value(h: &mut Headless, id: &str, text: &str) {
        let r = h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("{id} not on screen"));
        let spacing = h.view.ctx.global_style().spacing.clone();
        let x = r.left() + spacing.slider_width + spacing.item_spacing.x + 6.0;
        assert_eq!(h.request("ui.click", json!({"x": x, "y": r.center().y}), T)["ok"], true);
        h.step();
        for key in ["a", "Backspace"] {
            assert_eq!(h.request("ui.key", json!({"key": key, "cmd": key == "a"}), T)["ok"], true);
        }
        assert_eq!(h.request("ui.text", json!({"text": text}), T)["ok"], true);
        assert_eq!(h.request("ui.key", json!({"key": "Enter"}), T)["ok"], true);
        h.step();
        h.step();
    }

    fn burst(h: &Headless) -> (f32, f32) {
        match &h.app.ui.dialog {
            Some(Dialog::Cull { burst_gap, similarity, .. }) => (*burst_gap, *similarity),
            d => panic!("{d:?}"),
        }
    }

    #[test]
    fn the_culling_value_fields_take_values_past_the_sliders() {
        let app = LccApp::new(lcc_engine::Session::with_demo(), Services::default());
        let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
        h.step();
        h.app.run("dialog.cull", json!({})).unwrap();
        for _ in 0..4 {
            h.step();
        }
        assert_eq!(burst(&h), (10.0, 0.93));
        // half an hour and "only identical shots": past the sliders' 10 min and 0.99
        type_value(&mut h, "field:cullGap", "30 min");
        type_value(&mut h, "field:cullSimilar", "1.0");
        assert_eq!(burst(&h), (1800.0, 1.0));
        // a day is as far as photo.analyze goes; more is held there
        type_value(&mut h, "field:cullGap", "2 days");
        assert_eq!(burst(&h).0, 86400.0);
        type_value(&mut h, "field:cullGap", "0");
        assert_eq!(burst(&h), (0.0, 1.0));
        type_value(&mut h, "field:cullGap", "1800");
        assert_eq!(burst(&h).0, 1800.0);
        // a similarity between the slider's hundredths is kept to the thousandth
        type_value(&mut h, "field:cullSimilar", "0.995");
        assert_eq!(burst(&h).1, 0.995);
        type_value(&mut h, "field:cullSimilar", "0.9951");
        assert_eq!(burst(&h).1, 0.995);
        type_value(&mut h, "field:cullSimilar", "1.0");
        assert_eq!(burst(&h).1, 1.0);
        // OK hands them to photo.analyze as typed (it refuses values out of its range)
        let r = h.request("ui.dialog.confirm", json!({}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.app.ui.dialog.is_none(), "{:?}", h.app.ui.dialog);
        assert_eq!(h.app.ui.cull_burst, Some((1800.0, 1.0)));
        // and the next opening starts from them, not from the sliders' range
        h.app.run("dialog.cull", json!({})).unwrap();
        h.step();
        h.step();
        assert_eq!(burst(&h), (1800.0, 1.0));
    }
}

#[cfg(test)]
mod size_limit_note_tests {
    use super::size_limit_is_tight_for;
    use lcc_engine::export::Resize;

    /// 1 MB over a 40 MP photo at full size is tight; at 2048 px, or with no limit, it is not.
    #[test]
    fn a_size_limit_is_tight_only_for_big_outputs() {
        let photos = [(7728, 5152), (3000, 2000)];
        assert!(size_limit_is_tight_for(1000, true, &Resize::default(), &photos));
        assert!(!size_limit_is_tight_for(1000, false, &Resize::long_edge(2048), &photos));
        assert!(!size_limit_is_tight_for(0, true, &Resize::default(), &photos));
        assert!(!size_limit_is_tight_for(1000, true, &Resize::default(), &[]));
        assert!(!size_limit_is_tight_for(1000, false, &Resize { value: f32::NAN, ..Resize::default() }, &[(0, 0)]));
    }
}
