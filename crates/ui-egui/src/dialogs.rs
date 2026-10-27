//! Modal dialogs (new album, rename, create preset, choose settings to copy, export, about, shortcuts).

use lightcraft_develop::{ControlSpec, Section, SettingsGroup, Track};
use serde_json::json;

use crate::LightcraftApp;
use crate::state::Dialog;
use crate::theme::Tokens;

mod confirm;
mod cull;
mod export;
mod frame;
mod rename;
mod sam_model;

pub(crate) use confirm::album_entries;
pub use confirm::confirm_dialog;
pub use cull::{MAX_BURST_GAP, clamp_cull_burst, cull_params, fmt_gap, parse_gap};
use export::{
    LIMIT_KB, QUALITY, START_NUMBER, WM_IMAGE_WIDTH, WM_INSET_X, WM_INSET_Y, WM_OPACITY, WM_POSITIONS, WM_SIZE, export_dialog_params, export_size,
    group_checklist, watermark_preview,
};
pub use frame::{ConfirmStyle, confirm_style, list_height, prompt};
use frame::{
    DIALOG_CHROME_GUESS, DIALOG_MIN_BODY, DIALOG_SCREEN_MARGIN, choices, clicked_backdrop, dialog_body, dialog_field, field, just_opened, num,
    only_shows, return_confirms, trailing_button_row,
};
pub(crate) use rename::rename_preview;
use rename::{RENAME_PREVIEW_ROWS, person_rename_plan};
pub(crate) use sam_model::show_model_folder;
use sam_model::{sam_by_hand, sam_model_body};
use slideshow_export::slideshow_export_body;

pub mod credits;
pub mod faces;
pub mod import;
pub mod keymap;
pub mod lightroom_destination;
pub mod merge;
pub mod print_view;
pub mod rules_editor;
pub mod settings;
pub mod slideshow_export;
pub mod updates_ui;

/// About dialog tabs: (widget id suffix, label). The credits come from `crate::credits`.
pub const ABOUT_TABS: &[(&str, &str)] = &[("about", "About"), ("contributors", "Contributors"), ("models", "Models")];

/// Help ▸ What's New (docs/whats-new.md).
pub const WHATS_NEW: &str = include_str!("../../../docs/whats-new.md");

/// The import review and Settings keep window ids of their own, so the size each is given
/// doesn't carry over to the other dialogs.
fn window_id(dialog: &Dialog) -> egui::Id {
    egui::Id::unique(match dialog {
        Dialog::Import { .. } => "lightcraft-import-dialog",
        Dialog::Print => "lightcraft-print-dialog",
        Dialog::Settings { .. } => "lightcraft-settings-dialog",
        _ => "lightcraft-dialog",
    })
}

/// Block the underlying panels from the first frame, before the window establishes its modal
/// layer. Close background menus on entry; popups subsequently opened inside the dialog stay usable.
pub(crate) fn prepare(app: &LightcraftApp, ui: &mut egui::Ui) {
    let Some(dialog) = &app.ui.dialog else { return };
    ui.disable();
    let layer = egui::LayerId::new(egui::Order::Middle, window_id(dialog));
    if ui.ctx().memory(|m| m.top_modal_layer()) != Some(layer) {
        egui::Popup::close_all(ui.ctx());
    }
}

/// Draw the open dialog. `blocked` is true when a quit prompt or settings notice was on screen at
/// the start of this frame: the dialog is still drawn (its state is preserved), but none of its raw
/// keyboard/backdrop handling may act. The caller freezes `blocked` for the whole frame so the
/// click or key that dismisses the prompt cannot also reach the dialog drawn after it. The
/// blocking library-problem/launch-list window gets the same raw-input treatment while it is
/// actually on screen over this dialog; ordinary dialogs without that window keep their normal
/// Escape and outside-click behaviour.
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context, blocked: bool) {
    let Some(mut dlg) = app.ui.dialog.clone() else { return };
    let at_start = dlg.clone();
    let library_blocks = crate::panels::library_problem::blocks_dialog_input(app);
    let raw_blocked = blocked || library_blocks;
    // Esc with a popup open in the dialog (a dropdown, a date picker's calendar) closes the popup
    // only: claimed before the dialog is drawn, since the popup is gone once it has handled the key
    if !raw_blocked && ctx.input(|i| i.key_pressed(egui::Key::Escape)) && egui::Popup::is_any_open(ctx) {
        crate::widgets::take_escape(ctx);
    }
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    // The backdrop is an area below the dialog window (a bare `Middle` layer painter would be
    // painted after every area — i.e. over the dialog too).
    egui::Area::new(egui::Id::unique("dialog-dim")).order(egui::Order::Middle).fixed_pos(screen.min).interactable(false).show(ctx, |ui| {
        ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(140));
    });
    let mut close = false;
    let mut confirm = false;
    // Return presses OK in a dialog without a text field of its own (Lightroom's default button);
    // a widget that has the keyboard focus (a text field, a button or checkbox reached with Tab)
    // takes Return itself. Consumed before the dialog is drawn, so nothing else sees it too.
    if !raw_blocked
        && return_confirms(&dlg)
        && ctx.memory(|m| m.focused().is_none())
        && !egui::Popup::is_any_open(ctx)
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
    {
        confirm = true;
    }
    let title: String = match &dlg {
        Dialog::TextPrompt { title, .. } => title.as_str(),
        Dialog::NewAlbum { folder: true, .. } => "Create Folder",
        Dialog::NewAlbum { .. } => "Create Album",
        Dialog::RenameAlbum { .. } => "Rename Album",
        Dialog::Rename { .. } => "Rename Photos",
        Dialog::Import { .. } => "Import Photos",
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
        Dialog::NewSmartAlbum { .. } => "Create Smart Album",
        Dialog::AllMetadata { .. } => "All Metadata",
        Dialog::SystemInfo { .. } => "System Info",
        Dialog::DenoiseModel { .. } => "AI Denoise Model",
        Dialog::FaceModel { info, .. } if info["download"].is_string() => "Download Face Model",
        Dialog::FaceModel { .. } => "Add Face Model",
        Dialog::WhatsNew => "What's New",
        Dialog::Cull { .. } => "Assisted Culling",
        Dialog::SmartRules { id: None, .. } => "New Smart Album",
        Dialog::SmartRules { .. } => "Edit Smart Album",
        Dialog::AutoStack { .. } => "Auto-Stack by Capture Time",
        Dialog::CreatePreset { .. } => "Create Preset",
        Dialog::CopySettings { .. } => "Choose Edit Settings to Copy",
        Dialog::PasteSettings { .. } => "Paste Selected Settings",
        Dialog::Export { .. } => "Export",
        Dialog::ContactSheet { .. } => "Contact Sheet PDF",
        Dialog::SlideshowExport { .. } => "Export Slideshow",
        Dialog::Print => "Print",
        Dialog::Merge { opts } => opts.title(),
        Dialog::Settings { .. } => "Settings",
        Dialog::ConfirmDelete { .. } => "Delete Photos",
        Dialog::MovePhotos { .. } => "Move Photos",
        Dialog::LightroomCatalog { .. } => "Lightroom Classic Catalog",
        Dialog::LightroomDestination { .. } => "Import Lightroom Catalog",
        Dialog::SaveEffect { .. } => "New Effect Preset",
        Dialog::ManageEffects { .. } => "Manage Effect Presets",
        Dialog::DeleteEffects { names } if names.len() == 1 => "Delete Effect Preset",
        Dialog::DeleteEffects { .. } => "Delete Effect Presets",
        Dialog::RemoveFolder { disk: true, .. } => "Remove Disk from Library",
        Dialog::RemoveFolder { .. } => "Remove Folder from Library",
        Dialog::SynchronizeFolder { .. } => "Synchronize Folder",
        // (nothing to download from in this build: the dialog explains the manual install)
        Dialog::SamModel { .. } if sam_by_hand(&app.session.segmenter) => "Install the SAM 3 Model",
        Dialog::SamModel { .. } => "Download the SAM 3 Model?",
        Dialog::About => "About LightCraft",
        Dialog::Shortcuts => "Keyboard Shortcuts",
        Dialog::ViewOptions => "View Options",
    }
    .to_string();
    let frame = egui::Frame::window(&ctx.global_style()).inner_margin(egui::Margin::symmetric(16, 12));
    // The import review can be resized (its photo grid takes the room); it keeps a window id of its
    // own so the size it is given doesn't carry over to the other dialogs. Its content scrolls
    // rather than growing the window when the options below the grid get taller (e.g. Copy).
    let import = matches!(dlg, Dialog::Import { .. });
    // The Print dialog sizes itself to the window and scrolls its settings column on its own.
    let print = matches!(dlg, Dialog::Print);
    let window_id = window_id(&dlg);
    // Every other dialog fits the window (#781): its options scroll when they don't, and the title
    // bar and the button row stay outside the scroll area. The room they take is measured on the
    // previous frame (a guess on the first one).
    let chrome_id = window_id.with("chrome");
    let chrome = ctx.data(|d| d.get_temp::<f32>(chrome_id)).unwrap_or(DIALOG_CHROME_GUESS);
    // Settings keeps its top edge, and the widest it has been, while tabs of different sizes are shown, so its tab
    // row stays where it is (centred like the others, the row jumped with each tab's height)
    let settings = matches!(dlg, Dialog::Settings { .. });
    let top = (screen.height() * 0.08).clamp(DIALOG_SCREEN_MARGIN, 96.0);
    let body_max =
        if settings { screen.height() - top - DIALOG_SCREEN_MARGIN - chrome } else { screen.height() - 2.0 * DIALOG_SCREEN_MARGIN - chrome }
            .max(DIALOG_MIN_BODY);
    let widest_id = window_id.with("widest");
    let widest = if settings { ctx.data(|d| d.get_temp::<f32>(widest_id)).unwrap_or(0.0).min(screen.width() * 0.9) } else { 0.0 };
    let mut content_w = 0.0f32;
    let mut body_h = None;
    // before the window: drawing it closes a dropdown the click lands outside of
    let popup_was_open = egui::Popup::is_any_open(ctx);
    let shown = egui::Window::new(crate::i18n::tr(&title)).id(window_id)
        .collapsible(false)
        .resizable(import)
        .vscroll(import)
        .resize(|r| if import { r.default_height(crate::import::DIALOG_SIZE[1]) } else { r })
        .frame(frame)
        .anchor(if settings { egui::Align2::CENTER_TOP } else { egui::Align2::CENTER_CENTER }, [0.0, if settings { top } else { 0.0 }])
        .min_width(widest)
        .default_width(match dlg {
            Dialog::Import { .. } => crate::import::DIALOG_SIZE[0],
            Dialog::SmartRules { .. } => 680.0,
            Dialog::AllMetadata { .. } => 620.0,
            Dialog::Print => 1100.0,
            Dialog::FaceModel { .. } => 460.0,
            Dialog::LightroomCatalog { .. } => 440.0,
            Dialog::LightroomDestination { .. } => 480.0,
            Dialog::ManageEffects { .. } => 560.0,
            _ => 380.0,
        })
        .show(ctx, |ui| {
            // Disabled, not skipped: the dialog keeps its layout and state, but a raw event in the
            // first prompt frame cannot toggle a checkbox or press a button underneath it. The
            // blocking library window gets the same treatment: it owns input while it is on screen,
            // so the dialog behind it must not take focus or accept typed text either.
            if raw_blocked {
                ui.disable();
            }
            ui.spacing_mut().item_spacing.y = 8.0;
            body_h = dialog_body(ui, (!import && !print).then_some(title.as_str()), body_max, |ui| match &mut dlg {
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
                    ui.label(
                        egui::RichText::new(crate::i18n::tr_format!("Creates {} stacks from {} of {scope}", preview["stacks"], preview["photos"], scope = crate::i18n::tr(scope))).color(t.text_dim),
                    );
                }
                Dialog::Cull { reject_below, pick_best, burst_gap, similarity } => {
                    let n = app.session.selection.ids.len();
                    let scope = if n > 1 { crate::i18n::tr_format!("the {n} selected photos", n = n) } else { crate::i18n::tr_format!("the {} photos in view", app.session.visible_cloned().len()) };
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("Scores {scope} for focus and exposure and finds similar shots taken within seconds of each other (bursts).", scope = scope)).color(t.text_label));
                    ui.add_space(6.0);
                    let r = ui.add(egui::Slider::new(reject_below, 0.0..=80.0).text(crate::i18n::tr("Reject below focus")).step_by(1.0));
                    crate::widgets::register(ui, "field:cullReject", r.rect);
                    ui.label(egui::RichText::new(crate::i18n::tr(if *reject_below > 0.0 { "Blurry photos below the score are flagged as rejects." } else { "0: nothing is rejected." })).color(t.text_dim));
                    let r = ui.checkbox(pick_best, crate::i18n::tr("Pick the sharpest photo of each burst"));
                    crate::widgets::register(ui, "check:cullPick", r.rect);
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
                    crate::widgets::register(ui, "field:cullGap", r.rect);
                    let r = ui.add(
                        egui::Slider::new(similarity, 0.70..=0.99)
                            .clamping(egui::SliderClamping::Never)
                            .text(crate::i18n::tr("Burst: how alike (loose … strict)"))
                            // a typed 0.995 is kept (whole hundredths would round it to 1.00)
                            .step_by(0.001)
                            .min_decimals(2)
                            .max_decimals(3),
                    );
                    crate::widgets::register(ui, "field:cullSimilar", r.rect);
                    clamp_cull_burst(burst_gap, similarity);
                    ui.label(egui::RichText::new(crate::i18n::tr("Lower it when a panning or moving burst splits into several groups.")).color(t.text_dim));
                    ui.label(egui::RichText::new(crate::i18n::tr("Scores stay on the photos: filter or make smart albums with Focus and Best of Similar Shots.")).color(t.text_dim));
                }
                Dialog::WhatsNew => {
                    egui::ScrollArea::vertical().max_height(list_height(ui, 460.0)).auto_shrink([false, true]).show(ui, |ui| {
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
                Dialog::LightroomResult { summary, library, details, .. } => {
                    ui.label(summary.as_str());
                    ui.add_space(8.0);
                    if let Some(library) = library {
                        ui.label(crate::i18n::tr("Destination library"));
                        ui.add(egui::Label::new(library.as_str()).wrap().selectable(true));
                        ui.label(crate::i18n::tr("Lightroom catalogs import into the open library; no separate catalog folder is created."));
                    } else {
                        ui.label(crate::i18n::tr("These photos are only in memory. Open a library on disk and import the catalog again to save them."));
                    }
                    for detail in details.iter() {
                        ui.add(egui::Label::new(detail).wrap().selectable(true));
                    }
                }
                Dialog::SystemInfo { rows } => {
                    egui::Grid::new("sysinfo").num_columns(2).spacing([16.0, 4.0]).striped(true).show(ui, |ui| {
                        for (k, v) in rows.iter() {
                            ui.label(egui::RichText::new(crate::i18n::tr(k)).color(t.text_dim));
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
                    crate::widgets::register(ui, "field:metadataSearch", r.rect);
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
                    egui::ScrollArea::vertical().max_height(list_height(ui, 440.0)).auto_shrink([false, true]).show(ui, |ui| {
                        if groups.is_empty() {
                            ui.label(egui::RichText::new(crate::i18n::tr(rows["note"].as_str().unwrap_or("No metadata found"))).color(t.text_dim));
                        }
                        for (g, list) in &groups {
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new(g).font(t.semibold(12.5)).color(t.text));
                            egui::Grid::new(format!("meta-{g}")).num_columns(2).spacing([16.0, 3.0]).striped(true).show(ui, |ui| {
                                for (n, v) in list {
                                    ui.label(egui::RichText::new(n).color(t.text_dim));
                                    ui.add(egui::Label::new(egui::RichText::new(v).color(t.text_label)).wrap());
                                    ui.end_row();
                                }
                            });
                        }
                    });
                }
                Dialog::SmartRules { id, name, rules, .. } => {
                    let r = crate::widgets::named(ui.add(egui::TextEdit::singleline(name).hint_text(crate::i18n::tr("Name")).desired_width(f32::INFINITY)), crate::i18n::tr("Name"));
                    crate::widgets::register(ui, "field:smartName", r.rect);
                    ui.add_space(6.0);
                    // the folder an album made from a folder view carries is not in the editor, but it counts
                    let folder = id.and_then(|id| app.session.catalog.album(lightcraft_catalog::AlbumId(id))).and_then(|a| a.smart.as_deref().and_then(|f| f.library_folder.clone()));
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
                        ui.label(
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
                        lightcraft_catalog::rules::set_now(Some(now.clone()));
                        app.session.catalog.query(&rules, &Default::default()).len()
                    });
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("Matches: {}", crate::i18n::filter_label(&rules, &app.session.catalog))).color(t.text_label));
                    ui.label(
                        egui::RichText::new(crate::i18n::tr_format!("{n} photo{} now · updates automatically as photos change", if n == 1 { "" } else { "s" }, n = n))
                            .color(t.text_dim),
                    );
                }
                Dialog::CaptureTime { mode, time, days, hours, minutes, zone } => {
                    let n = app.session.selected().len();
                    field(ui, "Change", |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for (i, (label, m)) in [("Set date & time", "set"), ("Shift by", "shift"), ("Time zone", "zone")].iter().enumerate() {
                            if crate::widgets::text_button(ui, &format!("captureMode-{i}"), label, mode == m).clicked() {
                                *mode = m.to_string();
                            }
                        }
                    });
                    let params = match mode.as_str() {
                        "set" => {
                            field(ui, "New time", |ui| {
                                let r = ui.add(egui::TextEdit::singleline(time).hint_text("2026-09-30 14:05:00").desired_width(f32::INFINITY));
                                crate::widgets::register(ui, "field:captureTime", r.rect);
                            });
                            json!({"time": time})
                        }
                        "shift" => {
                            field(ui, "Shift", |ui| {
                                ui.add(egui::DragValue::new(days).suffix(" d"));
                                ui.add(egui::DragValue::new(hours).suffix(" h"));
                                ui.add(egui::DragValue::new(minutes).suffix(" min"));
                            });
                            json!({"shift": *days as i64 * 86_400 + *hours as i64 * 3600 + *minutes as i64 * 60})
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
                            "set" => lightcraft_catalog::dates::normalize_iso(time)
                                .and_then(|t| Some(lightcraft_catalog::dates::iso_seconds(&t)? - lightcraft_catalog::dates::iso_seconds(&cur)?)),
                            "shift" => params["shift"].as_i64(),
                            _ => Some((*zone as f64 * 3600.0).round() as i64),
                        };
                        // the photo's own time is the problem (a Lightroom catalog's capture time
                        // is copied verbatim), not what was typed
                        let unreadable = lightcraft_catalog::dates::shift_iso(&cur, 0).is_none();
                        let after = match (mode.as_str(), unreadable && n == 1) {
                            // alone, it simply gets the time typed (photo.setCaptureTime repairs it)
                            ("set", true) => lightcraft_catalog::dates::normalize_iso(time),
                            _ => delta.and_then(|d| lightcraft_catalog::dates::shift_iso(&cur, d)),
                        };
                        ui.label(
                            egui::RichText::new(match after {
                                Some(a) => format!("{name}: {} → {}", cur.replace('T', " "), a.replace('T', " ")),
                                None if unreadable && !(mode == "set" && n == 1) => crate::i18n::tr("The active photo's capture time can't be read").to_string(),
                                None => "Enter a date as YYYY-MM-DD HH:MM:SS".into(),
                            })
                            .color(t.text_label),
                        );
                    }
                    if n > 1 {
                        ui.label(egui::RichText::new(crate::i18n::tr_format!("All {n} selected photos move by the same amount.", n = n)).color(t.text_dim));
                    }
                }
                Dialog::LabelNames { names, save_as } => {
                    names.resize(5, String::new());
                    // start from a set
                    let sets = lightcraft_engine::cmd::manage::label_sets_json(&app.session);
                    field(ui, "Set", |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for set in sets["sets"].as_array().into_iter().flatten() {
                            let name = set["name"].as_str().unwrap_or_default();
                            if crate::widgets::text_button(ui, &format!("labelSet-{name}"), name, false).clicked() {
                                *names = set["names"].as_array().into_iter().flatten().map(|n| n.as_str().unwrap_or_default().to_string()).collect();
                            }
                        }
                    });
                    for (i, l) in lightcraft_catalog::ColorLabel::ALL.iter().enumerate() {
                        let colour = format!("{l:?}");
                        field(ui, &colour, |ui| {
                            let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            ui.painter().circle_filled(r.center(), 6.0, crate::panels::grid::label_color(*l));
                            let te = crate::widgets::named(ui.add(egui::TextEdit::singleline(&mut names[i]).hint_text(colour.as_str()).desired_width(f32::INFINITY)), crate::i18n::tr(&colour));
                            crate::widgets::register(ui, format!("field:labelName-{}", colour.to_lowercase()), te.rect);
                        });
                    }
                    field(ui, "Save as set", |ui| {
                        let te = crate::widgets::named(ui.add(egui::TextEdit::singleline(save_as).hint_text(crate::i18n::tr("Optional name")).desired_width(f32::INFINITY)), crate::i18n::tr("Save as set"));
                        crate::widgets::register(ui, "field:labelSetName", te.rect);
                    });
                    ui.label(
                        egui::RichText::new(crate::i18n::tr("Names appear in the label menu, the filter bar and the Info panel, and are written to XMP. Empty = the colour's name."))
                            .color(t.text_dim),
                    );
                }
                Dialog::Rename { template, start } => {
                    let n = app.session.selected().len();
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("{n} photo{}", if n == 1 { "" } else { "s" }, n = n)).color(t.text_dim));
                    let template_id = egui::Id::unique("rename-template");
                    let tags_open = field(ui, "Template", |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let w = crate::import::tag_field_width(ui);
                        let r = crate::widgets::named(ui.add(egui::TextEdit::singleline(template).id(template_id).hint_text("{name}").desired_width(w)), crate::i18n::tr("Template"));
                        crate::widgets::register(ui, "field:renameTemplate", r.rect);
                        crate::import::tag_toggle(ui, "renameTemplate")
                    });
                    if tags_open {
                        crate::import::tag_help(ui, "renameTemplate", template, template_id);
                    }
                    crate::import::unknown_tags_warning(ui, template);
                    field(ui, "Presets", |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for (i, (label, tpl)) in
                            [("Name", "{name}"), ("Date–Name", "{date}_{name}"), ("Title–Seq", "{title}-{seq:3}"), ("Custom–Seq", "Photo-{seq:3}")].iter().enumerate()
                        {
                            if crate::widgets::text_button(ui, &format!("renamePreset-{i}"), label, template == tpl).clicked() {
                                *template = tpl.to_string();
                            }
                        }
                    });
                    field(ui, "Start at", |ui| ui.add(egui::DragValue::new(start).range(0..=999_999)));
                    ui.label(
                        egui::RichText::new(crate::i18n::tr("Tags: see Tags beside the template. Files are renamed on disk (with their XMP sidecars); existing names get -1, -2…"))
                            .color(t.text_dim),
                    );
                    let (preview, total) = rename_preview(app, ui.ctx(), template, *start as usize);
                    match &preview {
                        Some(rows) => {
                            egui::Grid::new("rename-preview").num_columns(3).spacing([8.0, 2.0]).show(ui, |ui| {
                                for pl in rows {
                                    ui.label(egui::RichText::new(&pl.from).color(t.text_dim));
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
                                ui.label(egui::RichText::new(crate::i18n::tr("Checking names…")).color(t.text_dim));
                            });
                        }
                    }
                    let more = total.saturating_sub(RENAME_PREVIEW_ROWS);
                    if more > 0 {
                        ui.label(egui::RichText::new(crate::i18n::tr_format!("… and {more} more", more = more)).color(t.text_dim));
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
                                "Renames {n} face{} across the library. The files are not changed.",
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
                        let r = ui.checkbox(merge, crate::i18n::tr("Combine with the existing person"));
                        crate::widgets::register(ui, "check:personMerge", r.rect);
                    }
                }
                Dialog::RenameKeyword { from, to } => {
                    let n = app.caches.dialog_count(&app.session.catalog, ("renameKeyword", from.as_str()), || {
                        app.session.catalog.photos().filter(|p| p.meta.keywords.iter().any(|k| lightcraft_catalog::keywords::is_under(k, from))).count()
                    });
                    if dialog_field(ui, "field:keywordName", to, "New name") {
                        confirm = true;
                    }
                    ui.label(
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
                        .filter(|k| !from.iter().any(|f| lightcraft_catalog::keywords::is_under(k, f)))
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
                        let r = ui.checkbox(include_selected, crate::i18n::tr("Include selected photos"));
                        crate::widgets::register(ui, "check:albumIncludeSelected", r.rect);
                    }
                }
                Dialog::RenameAlbum { name, .. } => {
                    if dialog_field(ui, "field:albumName", name, "Name") {
                        confirm = true;
                    }
                }
                Dialog::CreatePreset { name, group, groups } => {
                    crate::widgets::named(ui.add(egui::TextEdit::singleline(name).hint_text(crate::i18n::tr("Preset name")).desired_width(f32::INFINITY)), crate::i18n::tr("Preset name"));
                    crate::widgets::named(ui.add(egui::TextEdit::singleline(group).hint_text(crate::i18n::tr("Group")).desired_width(f32::INFINITY)), crate::i18n::tr("Group"));
                    ui.label(egui::RichText::new(crate::i18n::tr("Settings to include")).color(t.text_dim));
                    group_checklist(ui, "presetInclude", groups);
                }
                Dialog::CopySettings { groups } => group_checklist(ui, "copyGroup", groups),
                Dialog::PasteSettings { groups } => {
                    let n = app.session.selected().len();
                    ui.label(
                        egui::RichText::new(crate::i18n::tr_format!("Paste into {n} photo{} — only settings that were copied are pasted", if n == 1 { "" } else { "s" }, n = n))
                            .color(t.text_dim),
                    );
                    group_checklist(ui, "pasteGroup", groups);
                }
                Dialog::SlideshowExport { settings } => slideshow_export_body(app, ui, settings),
                Dialog::ContactSheet { options } => {
                    options.columns = options.columns.clamp(1, 8);
                    options.rows = options.rows.clamp(1, 10);
                    let n = app.session.selected().len();
                    ui.label(crate::i18n::tr_format!("{n} selected photos · {pages} pages", n = n, pages = n.div_ceil(options.columns * options.rows)));
                    field(ui, "Paper", |ui| {
                        let c = crate::widgets::combo_box("contactSheetPaper").selected_text(&options.paper).show_ui(ui, |ui| {
                            ui.selectable_value(&mut options.paper, "a4".into(), "A4");
                            ui.selectable_value(&mut options.paper, "letter".into(), crate::i18n::tr("Letter"));
                        });
                        crate::widgets::name_combo(&c.response, crate::i18n::tr("Paper"));
                    });
                    ui.checkbox(&mut options.landscape, crate::i18n::tr("Landscape"));
                    field(ui, "Columns", |ui| { ui.add(egui::DragValue::new(&mut options.columns).range(1..=8)); });
                    field(ui, "Rows", |ui| { ui.add(egui::DragValue::new(&mut options.rows).range(1..=10)); });
                    ui.checkbox(&mut options.captions, crate::i18n::tr("Filename captions"));
                    ui.label(crate::i18n::tr("150 dpi · sRGB · current edits · fit without cropping"));
                    ui.label(crate::i18n::tr("Choose a PDF destination after confirming. Print the PDF from your viewer."));
                }
                Dialog::Export { opts, full_size, resize, preset_name, limit_kb, dir } => {
                    use lightcraft_engine::export::{ExportFormat as F, MetadataPolicy as M, SharpenAmount as A, SharpenFor as S};
                    let n = app.session.selection.ids.len().max(1);
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("{n} photo{}", if n == 1 { "" } else { "s" }, n = n)).color(t.text_dim));
                    // Preset: load a built-in or saved set of options into the dialog
                    field(ui, "Preset", |ui| {
                        let mut chosen = None;
                        let c = crate::widgets::combo_box("exportPreset").width(220.0).selected_text(crate::i18n::tr("Choose…")).show_ui(ui, |ui| {
                            let mut builtin = true;
                            for (p, b) in app.session.all_export_presets() {
                                if builtin && !b {
                                    ui.separator();
                                }
                                builtin = b;
                                if ui.selectable_label(false, crate::i18n::builtin_label(&p.name, b)).clicked() {
                                    chosen = Some(p.name);
                                }
                            }
                        });
                        crate::widgets::name_combo(&c.response, crate::i18n::tr("Preset"));
                        crate::widgets::register(ui, "combo:exportPreset", c.response.rect);
                        if let Some(name) = chosen
                            && let Ok(params) = app.session.export_params(&json!({"preset": name}))
                        {
                            let o = lightcraft_engine::export::ExportOptions::from_json(&params);
                            *full_size = o.resize.is_none();
                            *resize = o.resize.unwrap_or_default();
                            *limit_kb = o.limit_kb.unwrap_or(0);
                            *opts = o;
                        }
                    });
                    ui.add_space(4.0);
                    let before = opts.format;
                    // "Rendered DNG" is a DNG option (below), not a format of its own in this list
                    let mut shown = if opts.format == F::RenderedDng { F::Dng } else { opts.format };
                    choices(
                        ui,
                        "Format",
                        "exportFormat",
                        &[(F::Jpeg, "JPEG"), (F::Png, "PNG"), (F::Tiff, "TIFF"), (F::Psd, "PSD"), (F::Webp, "WebP"), (F::Avif, "AVIF"), (F::Jxl, "JPEG XL"), (F::Dng, "DNG"), (F::Original, "Original files")],
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
                        ui.label(egui::RichText::new(note).color(t.text_dim));
                    }
                    if opts.format != before {
                        // each format starts at its own default depth (TIFF 16-bit, others 8-bit)
                        opts.bit_depth = None;
                    }
                    let rendered = opts.format.is_rendered();
                    let depths = lightcraft_engine::export::ExportOptions::bit_depths(opts.format);
                    if rendered && depths.len() > 1 {
                        let mut bd = opts.bit_depth.filter(|b| depths.iter().any(|d| d.0 == *b)).unwrap_or(depths[0].0);
                        choices(ui, "Bit depth", "exportBitDepth", depths, &mut bd);
                        opts.bit_depth = Some(bd);
                    }
                    if matches!(opts.format, F::Jpeg | F::Tiff | F::Avif | F::Png | F::Jxl) {
                        let tip = crate::i18n::tr("Photos edited in HDR: JPEG with an HDR gain map (looks right everywhere, brighter highlights on HDR displays), AVIF as 10-bit, PNG and JPEG XL as 16-bit Rec. 2020 PQ; TIFF needs 32-bit float");
                        ui.checkbox(&mut opts.hdr, crate::i18n::tr("HDR output")).on_hover_text(tip);
                        if opts.hdr && !opts.hdr_output() {
                            ui.label(egui::RichText::new(crate::i18n::tr("HDR TIFF needs 32-bit float.")).color(t.text_dim));
                        }
                        if opts.hdr && opts.format == F::Avif {
                            use lightcraft_engine::export::HdrCurve as H;
                            choices(ui, "HDR curve", "exportHdrCurve", &[(H::Pq, "PQ"), (H::Hlg, "HLG")], &mut opts.hdr_transfer);
                        }
                    }
                    if !rendered {
                    } else if opts.format == F::Avif && opts.hdr {
                        let label = if opts.hdr_transfer == lightcraft_engine::export::HdrCurve::Hlg {
                            crate::i18n::tr("Color space: Rec. 2020 HLG (HDR AVIF)")
                        } else {
                            crate::i18n::tr("Color space: Rec. 2020 PQ (HDR AVIF)")
                        };
                        ui.label(egui::RichText::new(label).color(t.text_dim));
                    } else if opts.format == F::Png && opts.hdr {
                        ui.label(egui::RichText::new(crate::i18n::tr("Color space: Rec. 2020 PQ, 16-bit (HDR PNG)")).color(t.text_dim))
                            .on_hover_text(crate::i18n::tr("Viewers that don't read the PNG cICP chunk show PQ values as sRGB: flat and washed out. For a picture that looks right everywhere, use JPEG (gain map)."));
                    } else if opts.format == F::Jxl && opts.hdr {
                        ui.label(egui::RichText::new(crate::i18n::tr("Color space: Rec. 2020 PQ (HDR JPEG XL)")).color(t.text_dim));
                    } else if opts.format == F::Avif {
                        ui.label(egui::RichText::new(crate::i18n::tr("Color space: sRGB (AVIF)")).color(t.text_dim));
                    } else {
                        use lightcraft_engine::export::OutputSpace as C;
                        choices(
                            ui,
                            "Color space",
                            "exportColorSpace",
                            &[
                                (C::Srgb, "sRGB"),
                                (C::DisplayP3, "P3"),
                                (C::AdobeRgb, "Adobe RGB"),
                                (C::ProPhoto, "ProPhoto"),
                                (C::Rec2020, "Rec.2020"),
                            ],
                            &mut opts.color_space,
                        );
                        if opts.effective_space() != opts.color_space {
                            let note = "Lossy JPEG XL writes Adobe RGB and ProPhoto as Rec. 2020; choose Lossless to keep them.";
                            ui.label(egui::RichText::new(crate::i18n::tr(note)).color(t.text_dim));
                        }
                    }
                    if opts.format == F::Jxl {
                        let tip = "Mathematically lossless: every pixel is kept exactly (larger files)";
                        ui.checkbox(&mut opts.jxl_lossless, crate::i18n::tr("Lossless")).on_hover_text(crate::i18n::tr(tip));
                    }
                    if matches!(opts.format, F::Jpeg | F::Avif) || (opts.format == F::Jxl && !opts.jxl_lossless) {
                        let mut q = opts.quality as f64;
                        if num(ui, &QUALITY, &mut q) {
                            opts.quality = q as u8;
                        }
                    }
                    if opts.format == F::Jxl {
                        use lightcraft_engine::export::JxlEffort as E;
                        choices(ui, "Effort", "exportJxlEffort", &[(E::Fast, "Fast"), (E::Normal, "Normal")], &mut opts.jxl_effort);
                        let tip = "Shows a coarse version first and refines it as the file loads";
                        ui.checkbox(&mut opts.jxl_progressive, crate::i18n::tr("Progressive")).on_hover_text(crate::i18n::tr(tip));
                    }
                    if opts.format == F::Jpeg {
                        let mut k = *limit_kb as f64;
                        if num(ui, &LIMIT_KB, &mut k) {
                            *limit_kb = k as u32;
                        }
                    }
                    if rendered {
                        export_size(ui, full_size, resize, &mut opts.ppi);
                    }
                    if rendered {
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
                    if rendered {
                    choices(
                        ui,
                        "Metadata",
                        "exportMetadata",
                        &[(M::All, "All"), (M::AllExceptCamera, "No camera"), (M::Copyright, "Copyright"), (M::None, "None")],
                        &mut opts.metadata,
                    );
                    if !matches!(opts.metadata, M::None | M::Copyright) {
                        ui.checkbox(&mut opts.remove_location, crate::i18n::tr("Remove location info"));
                    }
                    let mut wm_on = opts.watermark.is_some();
                    if ui.checkbox(&mut wm_on, crate::i18n::tr("Watermark")).changed() {
                        opts.watermark = wm_on.then(|| lightcraft_engine::export::Watermark { text: "© ".into(), ..Default::default() });
                    }
                    let wm_shown = opts.watermark.clone();
                    if let Some(wm) = &wm_shown {
                        watermark_preview(app, ui, wm, (!*full_size).then_some(*resize));
                    }
                    if let Some(wm) = &mut opts.watermark {
                        // text, or a graphic (a logo with transparency)
                        let mut graphic = !wm.image.is_empty() || ui.data(|d| d.get_temp::<bool>(egui::Id::unique("wm-graphic"))).unwrap_or(false);
                        let before = graphic;
                        choices(ui, "Style", "exportWmStyle", &[(false, "Text"), (true, "Graphic")], &mut graphic);
                        if graphic != before {
                            ui.data_mut(|d| d.insert_temp(egui::Id::unique("wm-graphic"), graphic));
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
                                    crate::widgets::named(ui.add(egui::TextEdit::singleline(&mut wm.image).hint_text("logo.png").desired_width(ui.available_width())), crate::i18n::tr("Image"));
                                });
                            });
                            let mut width = wm.image_width as f64 * 100.0;
                            if num(ui, &WM_IMAGE_WIDTH, &mut width) {
                                wm.image_width = (width / 100.0) as f32;
                            }
                        } else {
                            choices(ui, app.ui.language.tr("Text direction"), "exportWmOrientation", &[(false, app.ui.language.tr("Horizontal text")), (true, app.ui.language.tr("Vertical text"))], &mut wm.vertical);
                            field(ui, "Text", |ui| {
                                crate::widgets::named(ui.add(egui::TextEdit::multiline(&mut wm.text).desired_rows(2).hint_text(crate::i18n::tr("© Your Name")).desired_width(f32::INFINITY)), crate::i18n::tr("Text"))
                            });
                        }
                        choices(
                            ui,
                            "Position",
                            "exportWmAnchor",
                            &WM_POSITIONS,
                            &mut wm.anchor,
                        );
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
                        use lightcraft_engine::export::Anchor as P;
                        let across = !matches!(wm.anchor, P::Top | P::Center | P::Bottom);
                        if let Some(v) = crate::widgets::slider(ui, &WM_INSET_X, wm.inset_x as f64 * 100.0, across, None).value {
                            wm.inset_x = (v / 100.0) as f32;
                        }
                        let down = !matches!(wm.anchor, P::Left | P::Center | P::Right);
                        if let Some(v) = crate::widgets::slider(ui, &WM_INSET_Y, wm.inset_y as f64 * 100.0, down, None).value {
                            wm.inset_y = (v / 100.0) as f32;
                        }
                        if !graphic {
                            ui.checkbox(&mut wm.shadow, crate::i18n::tr("Shadow"));
                        }
                    }
                    if opts.watermark != wm_shown {
                        // the preview above was drawn with the old settings
                        ui.ctx().request_repaint();
                    }
                    }
                    ui.add_space(4.0);
                    if opts.format == F::Tiff {
                        use lightcraft_engine::export::TiffCompression as Z;
                        choices(ui, "Compression", "exportTiffCompression", &[(Z::None, "None"), (Z::Lzw, "LZW"), (Z::Deflate, "ZIP")], &mut opts.tiff_compression);
                    }
                    if opts.format == F::Dng {
                        use lightcraft_engine::export::DngCompression as Z;
                        choices(ui, "Compression", "exportDngCompression", &[(Z::Lossless, "Lossless"), (Z::Deflate, "ZIP"), (Z::Uncompressed, "None"), (Z::Lossy, "Lossy")], &mut opts.dng_compression);
                    }
                    let naming_id = egui::Id::unique("export-naming");
                    let tags_open = field(ui, "File name", |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let w = crate::import::tag_field_width(ui);
                        crate::widgets::named(ui.add(egui::TextEdit::singleline(&mut opts.naming).id(naming_id).hint_text("{name}-{seq}  ·  {date}  ·  {title}  ·  {folder}").desired_width(w)), crate::i18n::tr("File name"));
                        crate::import::tag_toggle(ui, "exportNaming")
                    });
                    if tags_open {
                        crate::import::tag_help(ui, "exportNaming", &mut opts.naming, naming_id);
                    }
                    crate::import::unknown_tags_warning(ui, &opts.naming);
                    if opts.naming.contains("{seq") {
                        let mut v = opts.start_number as f64;
                        if num(ui, &START_NUMBER, &mut v) {
                            opts.start_number = v as u32;
                        }
                    }
                    // like Lightroom Classic's Export To: one folder, or each photo's own folder (not on
                    // the web, where originals live in the browser's storage, not in folders)
                    use lightcraft_engine::export::DestinationMode as D;
                    if !cfg!(target_arch = "wasm32") || opts.destination_mode != D::SpecificFolder {
                    choices(
                        ui,
                        "Export to",
                        "exportDestinationMode",
                        &[(D::SpecificFolder, "Specific folder"), (D::SameAsOriginal, "Same folder as original photo")],
                        &mut opts.destination_mode,
                    );
                    }
                    if opts.destination_mode == D::SpecificFolder {
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
                    field(ui, "Subfolder", |ui| {
                        crate::widgets::named(ui.add(egui::TextEdit::singleline(&mut opts.subfolder).hint_text(crate::i18n::tr("none")).desired_width(f32::INFINITY)), crate::i18n::tr("Subfolder"))
                    });
                    use lightcraft_engine::export::Conflict as K;
                    choices(
                        ui,
                        "If file exists",
                        "exportConflict",
                        &[(K::Unique, "Add number"), (K::Overwrite, "Overwrite"), (K::Skip, "Skip")],
                        &mut opts.conflict,
                    );
                    // After export: Apple Photos imports the files written (macOS only)
                    #[cfg(target_os = "macos")]
                    {
                        let r = ui.checkbox(&mut opts.add_to_photos, crate::i18n::tr("Add to Apple Photos"));
                        crate::widgets::register(ui, "exportAddToPhotos", r.rect);
                        if opts.add_to_photos {
                            field(ui, "Album", |ui| {
                                crate::widgets::named(ui.add(egui::TextEdit::singleline(&mut opts.photos_album).hint_text(crate::i18n::tr("none")).desired_width(f32::INFINITY)), crate::i18n::tr("Album"))
                            });
                            ui.label(
                                egui::RichText::new(crate::i18n::tr("The first time, macOS asks whether LightCraft may control Photos."))
                                    .size(11.0)
                                    .color(t.text_dim),
                            );
                        }
                    }
                    // nowhere to add them: a setting carried over from a Mac is dropped, not refused
                    #[cfg(not(target_os = "macos"))]
                    {
                        opts.add_to_photos = false;
                    }
                    ui.add_space(4.0);
                    field(ui, "Save preset", |ui| {
                        trailing_button_row(ui, |ui| {
                            let named = !preset_name.trim().is_empty();
                            if crate::widgets::text_button(ui, "exportSavePreset", crate::i18n::tr("Save"), false).clicked() && named {
                                let params = export_dialog_params(opts, *full_size, resize, *limit_kb);
                                match app.run("export.savePreset", json!({"name": preset_name.trim(), "params": params})) {
                                    Ok(_) => {
                                        app.toast(ui.ctx(), crate::i18n::tr_format!("Saved export preset “{}”", preset_name.trim()));
                                        preset_name.clear();
                                    }
                                    Err(e) => app.toast(ui.ctx(), e),
                                }
                            }
                            crate::widgets::named(ui.add(egui::TextEdit::singleline(preset_name).hint_text(crate::i18n::tr("Preset name")).desired_width(ui.available_width())), crate::i18n::tr("Preset name"));
                        });
                    });
                }
                Dialog::Merge { opts } => crate::merge::body(app, ui, opts),
                Dialog::Import { opts } => crate::import::body(app, ui, opts),
                // (Settings is never taller than `body_max`, so the scroll area round it never
                // scrolls: its tab bar stays put and its pages scroll by themselves)
                Dialog::Settings { tab } => crate::panels::settings::window(app, ui, tab, body_max),
                Dialog::SamModel { error, .. } => sam_model_body(app, ui, error.as_deref()),
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
                    ui.label(egui::RichText::new(crate::i18n::tr("Keyword Name")).color(t.text_dim));
                    // several fields: the name takes the focus when the dialog opens, not after
                    let r = crate::text_field::TextField::singleline("field:keywordTagName", name)
                        .hint(crate::i18n::tr("Keyword Name"))
                        .width(f32::INFINITY)
                        .select_on_focus(true)
                        .show(ui);
                    if ui.is_enabled() && just_opened(ui, "keyword-tag") {
                        r.response.request_focus();
                    }
                    if r.ending == Some(crate::text_field::Ending::Return) {
                        confirm = true;
                    }
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(crate::i18n::tr("Synonyms")).color(t.text_dim));
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
                            let r = ui.checkbox(inside, crate::i18n::tr_format!("Put inside “{parent}”", parent = p.replace('|', " › ")));
                            crate::widgets::register(ui, "check:keywordInside", r.rect);
                        }
                        let n = app.session.selection.ids.len();
                        let r = ui.add_enabled(n > 0, egui::Checkbox::new(add_to_selected, crate::i18n::tr("Add to Selected Photos")));
                        crate::widgets::register(ui, "check:keywordAddToSelected", r.rect);
                        ui.add_space(4.0);
                    }
                    ui.label(egui::RichText::new(crate::i18n::tr("Keyword Tag Options")).color(t.text_dim));
                    for (on, label, id) in [
                        (include_on_export, "Include on Export", "check:keywordIncludeOnExport"),
                        (export_containing, "Export Containing Keywords", "check:keywordExportContaining"),
                        (export_synonyms, "Export Synonyms", "check:keywordExportSynonyms"),
                        (person, "Person", "check:keywordPerson"),
                    ] {
                        let r = ui.checkbox(on, crate::i18n::tr(label));
                        crate::widgets::register(ui, id, r.rect);
                    }
                }
                Dialog::KeywordSet { replaces, name, slots, as_new } => {
                    ui.label(egui::RichText::new(crate::i18n::tr("Set name")).color(t.text_dim));
                    let r = crate::text_field::TextField::singleline("field:keywordSetName", name)
                        .hint(crate::i18n::tr("Set name"))
                        .width(f32::INFINITY)
                        .select_on_focus(true)
                        .show(ui);
                    if ui.is_enabled() && just_opened(ui, "keyword-set") {
                        r.response.request_focus();
                    }
                    let mut returned = r.ending == Some(crate::text_field::Ending::Return);
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(crate::i18n::tr("Keywords (⌥1–⌥9)")).color(t.text_dim));
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
                        let r = ui.checkbox(as_new, crate::i18n::tr("Save as a new set"));
                        crate::widgets::register(ui, "check:keywordSetAsNew", r.rect);
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
                    ui.label(egui::RichText::new(crate::i18n::tr("Their photos and the keywords below them come together under one keyword.")).color(t.text_dim));
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
                Dialog::ManageEffects { search, selected, rename, confirm_delete, new_group } => {
                    crate::panels::mask_effects::manage_body(app, ui, search, selected, rename, confirm_delete, new_group)
                }
                Dialog::DeleteEffects { names } => {
                    let what = match names.as_slice() {
                        [one] => crate::i18n::tr_format!("Delete the effect preset “{name}”?", name = one),
                        _ => crate::i18n::tr_format!("Delete these {n} effect presets?", n = names.len()),
                    };
                    ui.label(what);
                    ui.label(egui::RichText::new(crate::i18n::tr("Masks keep their sliders. Export presets first to keep a copy.")).color(t.text_dim));
                }
                Dialog::ConfirmDelete { count } => {
                    let what = if *count == 1 { crate::i18n::tr("this photo").to_string() } else { crate::i18n::tr_format!("these {count} photos", count = count) };
                    ui.label(crate::i18n::tr_format!("Move {what} to Recently Deleted?", what = what));
                    ui.label(egui::RichText::new(crate::i18n::tr("They can be restored from Recently Deleted until it is emptied.")).color(t.text_dim));
                }
                Dialog::MovePhotos { ids, name, dont_ask, .. } => {
                    let n = ids.len();
                    ui.label(crate::i18n::tr_format!("Move {n} photo{} to “{}”?", if n == 1 { "" } else { "s" }, name, n = n));
                    ui.label(egui::RichText::new(crate::i18n::tr("Their files and XMP sidecars move on disk. Edit → Undo moves them back.")).color(t.text_dim));
                    ui.add_space(4.0);
                    let c = ui.checkbox(dont_ask, crate::i18n::tr("Don't ask again"));
                    crate::widgets::register(ui, "check:moveDontAsk", c.rect);
                    c.on_hover_text(crate::i18n::tr("Settings → Confirm before moving photos to another folder on disk turns it back on"));
                }
                d @ Dialog::SynchronizeFolder { .. } => crate::sync::body(app, ui, d),
                Dialog::LightroomCatalog { folder, catalog } => {
                    ui.label(crate::i18n::tr("This is a Lightroom catalog, not a LightCraft library. Import it into a LightCraft library instead?"));
                    ui.label(egui::RichText::new(catalog.as_deref().unwrap_or(folder.as_str())).color(t.text_dim));
                    // (the import then asks where the catalog goes, unless Settings says to merge, #2672)
                    let asks = app.ui.settings.lightroom_import == crate::state::LightroomImport::Ask
                        && app.session.library.as_ref().is_some_and(|l| l.on_disk);
                    let what = if asks {
                        "The import reads the catalog without changing it. Next, choose whether its photos, edits, keywords and collections join the library open now or go into a new library."
                    } else {
                        "The import reads the catalog without changing it and adds its photos, edits, keywords and collections to the library open now."
                    };
                    ui.label(egui::RichText::new(crate::i18n::tr(what)).color(t.text_dim));
                }
                d @ Dialog::LightroomDestination { .. } => crate::lightroom_destination::body(app, ui, d),
                Dialog::RemoveFolder { name, count, path, disk } => {
                    ui.label(crate::i18n::tr_format!(
                        "Remove “{name}” and its {count} photo{} from the library?",
                        if *count == 1 { "" } else { "s" },
                        name = name,
                        count = count
                    ));
                    ui.label(egui::RichText::new(path.as_str()).color(t.text_dim));
                    let scope = if *disk { "All the photos imported from this disk are included." } else { "That includes the photos in the folders inside it." };
                    ui.label(egui::RichText::new(crate::i18n::tr(scope)).color(t.text_dim));
                    ui.label(
                        egui::RichText::new(crate::i18n::tr("They move to Recently Deleted and can be restored; no file on disk is touched."))
                            .color(t.text_dim),
                    );
                }
                Dialog::About => {
                    ui.set_min_width(680.0);
                    let tab_id = egui::Id::unique("about_tab");
                    let mut tab = ui.data_mut(|d| d.get_temp::<u8>(tab_id)).unwrap_or(0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for (i, (id, label)) in ABOUT_TABS.iter().enumerate() {
                            if crate::widgets::text_button(ui, &format!("aboutTab-{id}"), label, usize::from(tab) == i).clicked() {
                                tab = u8::try_from(i).unwrap_or(0);
                            }
                        }
                    });
                    ui.data_mut(|d| d.insert_temp(tab_id, tab));
                    ui.separator();
                    match tab {
                        1 => crate::credits::contributors_ui(app, ui),
                        2 => crate::credits::models_ui(ui),
                        _ => {
                            ui.label(egui::RichText::new("LightCraft").font(t.semibold(20.0)).color(t.text));
                            ui.label(crate::i18n::tr_format!("Version {} — a clean-room, pure-Rust photo library and raw developer.", env!("CARGO_PKG_VERSION")));
                            ui.label(crate::i18n::tr_format!(
                                "MIT OR Apache-2.0. Fonts: {} (OFL). Icons: original.",
                                crate::theme::font_credits(app.chinese_font.is_some())
                            ));
                            ui.add_space(10.0);
                            let r = crate::button::Button::new("Join the ArtCraft Discord")
                                .primary()
                                .large()
                                .min_width(260.0)
                                .widget("button:aboutDiscord")
                                .tooltip(crate::links::DISCORD)
                                .show(ui);
                            if r.clicked() {
                                let _ = crate::links::open(app, crate::links::DISCORD);
                            }
                            ui.add_space(6.0);
                            for (label, url) in [
                                ("LightCraft website", crate::links::APP_PAGE),
                                ("Source code on GitHub", crate::links::GITHUB),
                                ("ArtCraft — more creative apps", crate::links::WEBSITE),
                            ] {
                                let r = ui.link(crate::i18n::tr(label)).on_hover_text(url);
                                if r.clicked() {
                                    let _ = crate::links::open(app, url);
                                }
                            }
                        }
                    }
                }
                Dialog::Shortcuts => crate::panels::keymap::body(app, ui, &t),
                Dialog::ViewOptions => crate::panels::settings::view_options(app, ui, &t),
                Dialog::Print => crate::print_view::body(app, ui, &t),
            });
            ui.add_space(4.0);
            // the confirm on the right with Cancel to its left (macOS and GNOME's order), filled with
            // the accent when it is the dialog's primary action. The row spans the width the window
            // gives its content this frame (its default width, or its content's if wider), so it
            // reaches the edge without widening the window.
            let row_w = ui.available_width().max(ui.min_rect().width()).max(1.0);
            let row_h = ui.spacing().interact_size.y;
            ui.allocate_ui_with_layout(egui::vec2(row_w, row_h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                // a model file LightCraft cannot use has nothing to confirm, nor has a dialog that
                // only shows something (one Close, no Cancel / OK)
                let unusable_model = matches!(&dlg, Dialog::DenoiseModel { info, .. } | Dialog::FaceModel { info, .. } if info["kind"] == "unsupported");
                let informational = unusable_model || matches!(dlg, Dialog::LightroomResult { first: None, .. }) || only_shows(&dlg);
                let sam = &app.session.segmenter;
                let (sam_installed, sam_running, sam_failed) = (sam.installed(), sam.download_status().running, sam.download_status().error.is_some());
                // no download location in this build: nothing to offer but the manual install
                let sam_nowhere = sam_by_hand(sam);
                let cancel = match &dlg {
                    Dialog::SamModel { .. } if sam_running || sam_installed || sam_nowhere => "Close",
                    Dialog::SamModel { .. } => "Not Now",
                    Dialog::LightroomResult { .. } => "Close",
                    _ => "Cancel",
                };
                let add_label;
                let ok = match &dlg {
                    Dialog::Import { opts } => {
                        let n = opts.selected_paths().len();
                        let verb = if opts.copy && opts.move_files { "Move" } else { "Import" };
                        add_label = crate::i18n::tr_format!("{verb} {n} Photo{}", if n == 1 { "" } else { "s" }, verb = crate::i18n::tr(verb), n = n);
                        add_label.as_str()
                    }
                    Dialog::LightroomResult { first: Some(_), .. } => "Show in Library",
                    Dialog::ContactSheet { .. } => "Choose PDF…",
                    Dialog::SlideshowExport { .. } => "Export…",
                    Dialog::Print => "Print to PDF…",
                    Dialog::Merge { .. } => "Merge",
                    Dialog::DenoiseModel { info, .. } if info["download"].is_string() => "Accept & Download",
                    Dialog::DenoiseModel { .. } => "Install",
                    Dialog::FaceModel { info, .. } if !informational && info["download"].is_string() => "Download",
                    Dialog::FaceModel { .. } if !informational => "Install",
                    Dialog::ConfirmDelete { .. } | Dialog::DeleteKeyword { .. } | Dialog::DeleteEffects { .. } => "Delete",
                    Dialog::SaveEffect { .. } => "Create",
                    Dialog::MovePhotos { .. } => "Move",
                    // (not "Merge": that is photo merging, HDR / panorama, in some languages)
                    Dialog::MoveKeyword { .. } => "Merge Keywords",
                    Dialog::KeywordSet { .. } => "Save",
                    Dialog::KeywordTag { editing: None, .. } => "Create",
                    Dialog::KeywordTag { .. } => "Save",
                    Dialog::RemoveFolder { .. } => "Remove",
                    Dialog::LightroomCatalog { .. } => "Import Lightroom Catalog…",
                    d @ Dialog::LightroomDestination { .. } => crate::lightroom_destination::ok_label(d),
                    Dialog::SynchronizeFolder { .. } if informational => "Close",
                    Dialog::SynchronizeFolder { .. } => "Synchronize",
                    Dialog::SamModel { then: Some(_), .. } if sam_installed => "Continue",
                    Dialog::SamModel { .. } if sam_installed || sam_running || sam_nowhere => "",
                    Dialog::SamModel { error, .. } if error.is_some() || sam_failed => "Try Again",
                    Dialog::SamModel { .. } => "Download",
                    Dialog::RenamePerson { from, to, .. } if person_rename_plan(&app.session.catalog, from, to).needs_merge => "Merge",
                    _ if informational => "Close",
                    _ => "OK",
                };
                // installing waits for the licence to be accepted
                // (and Synchronize waits for the scan)
                let rename_blocked = match &dlg {
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
                            && !matches!(&dlg, Dialog::SmartRules { id, rules, .. } if !rules.check_for(&app.session.catalog, id.map(lightcraft_catalog::AlbumId)).is_empty()));
                let primary = !informational && can_confirm && confirm_style(&dlg) == ConfirmStyle::Primary;
                use crate::button::{Button, Kind};
                // (each kind of dialog its own buttons: the keyboard's place doesn't carry from a
                // dialog to the one it opens)
                let kind = std::mem::discriminant(&dlg);
                let cancel_button = Button::new(cancel).widget("button:dialogCancel").id_salt(kind).min_width(72.0);
                let ok_button = Button::new(ok)
                    .kind(if primary { Kind::Primary } else { Kind::Secondary })
                    .enabled(can_confirm)
                    .widget("button:dialogOk")
                    .id_salt(kind)
                    .min_width(72.0);
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
            if import {
                crate::import::note_dialog_bottom(ui);
            }
            content_w = ui.min_rect().width();
        });
    if settings && content_w > widest + 0.5 {
        ctx.data_mut(|d| d.insert_temp(widest_id, content_w));
    }
    if let Some(w) = &shown {
        // (the import review keeps its hand-picked photos however the click lands)
        if !raw_blocked && app.ui.settings.close_dialogs_outside && !import && clicked_backdrop(ctx, w.response.rect, popup_was_open) {
            close = true;
        } else if raw_blocked {
            // A press made under the prompt or library window must not be remembered as a
            // backdrop press for a release that arrives after that covering window goes away.
            ctx.data_mut(|d| d.remove_temp::<bool>(egui::Id::unique("dialog-backdrop-press")));
        }
    }
    if let Some(w) = shown {
        ctx.memory_mut(|m| m.set_modal_layer(w.response.layer_id));
        if let Some(body_h) = body_h {
            let measured = (w.response.rect.height() - body_h).max(0.0);
            if (measured - chrome).abs() > 0.5 {
                ctx.data_mut(|d| d.insert_temp(chrome_id, measured));
                ctx.request_repaint();
            }
        }
        ctx.move_to_top(w.response.layer_id);
        crate::widgets::register_response("dialog:window", w.response.rect, &w.response);
    }
    // (while the shortcuts editor records a key, it takes Esc itself to cancel; an open popup in
    // the dialog, such as a date picker's calendar, takes it to close itself)
    if !raw_blocked && ctx.input(|i| i.key_pressed(egui::Key::Escape)) && !crate::widgets::escape_taken(ctx) {
        close = true;
    }
    if !raw_blocked && confirm {
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
            Err(e) if matches!(dlg, Dialog::MovePhotos { .. }) => {
                app.toast(ctx, e);
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
    if !close && !blocked {
        let frame = ctx.cumulative_frame_nr();
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique(BACKDROP_SHOWN), frame));
    }
    app.ui.dialog = if close { None } else { Some(dlg) };
}

/// The last frame a dialog was drawn and stayed open, for [`clicked_backdrop`].
const BACKDROP_SHOWN: &str = "dialog-backdrop-shown";

/// Whether a dialog stays open after its action succeeded (the SAM 3 dialog while the model
/// downloads).
pub fn keeps_open(app: &LightcraftApp, dlg: &Dialog) -> bool {
    matches!(dlg, Dialog::SamModel { .. }) && !app.session.segmenter.installed()
}
