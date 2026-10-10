//! Modal dialogs (new album, rename, create preset, choose settings to copy, export, about, shortcuts).

use lightcraft_develop::{ControlSpec, Section, SettingsGroup, Track};
use serde_json::json;

use crate::LightcraftApp;
use crate::state::Dialog;
use crate::theme::Tokens;

/// Rows the Rename dialog previews.
const RENAME_PREVIEW_ROWS: usize = 6;

/// The Rename dialog's preview: its first rows (`None` while being planned) and how many files
/// are renamed. Which names are taken is checked on disk, which can block on a slow drive, so the
/// rows are planned on a worker thread, and only when the template, start number, photos or
/// catalog change — never once per frame.
pub(crate) fn rename_preview(
    app: &mut LightcraftApp,
    ctx: &egui::Context,
    template: &str,
    start: usize,
) -> (Option<Vec<lightcraft_engine::rename::RenamePlan>>, usize) {
    use std::hash::{Hash, Hasher};
    type Rows = std::sync::Arc<std::sync::Mutex<Option<Vec<lightcraft_engine::rename::RenamePlan>>>>;
    let ids = app.session.targets(&json!({}));
    let photos = lightcraft_engine::rename::rename_photos(&app.session.catalog, &ids);
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
        let plans = lightcraft_engine::rename::plan_rename_photos(&first, &template, start, &|f| exists(f));
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

/// About dialog tabs: (widget id suffix, label). The credits come from `crate::credits`.
pub const ABOUT_TABS: &[(&str, &str)] = &[("about", "About"), ("contributors", "Contributors"), ("models", "Models")];

/// Help ▸ What's New (docs/whats-new.md).
pub const WHATS_NEW: &str = include_str!("../../../../docs/whats-new.md");

pub fn show(app: &mut LightcraftApp, ctx: &egui::Context) {
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
    let title: String = match &dlg {
        Dialog::TextPrompt { title, .. } => title.as_str(),
        Dialog::NewAlbum { folder: true, .. } => "Create Folder",
        Dialog::NewAlbum { .. } => "Create Album",
        Dialog::RenameAlbum { .. } => "Rename Album",
        Dialog::Rename { .. } => "Rename Photos",
        Dialog::Import { .. } => "Import Photos",
        Dialog::LabelNames { .. } => "Edit Color Label Names",
        Dialog::CaptureTime { .. } => "Edit Capture Time",
        Dialog::RenameKeyword { .. } => "Rename Keyword",
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
        Dialog::Merge { opts } => opts.title(),
        Dialog::Settings { .. } => "Settings",
        Dialog::ConfirmDelete { .. } => "Delete Photos",
        Dialog::RemoveFolder { disk: true, .. } => "Remove Disk from Library",
        Dialog::RemoveFolder { .. } => "Remove Folder from Library",
        Dialog::SynchronizeFolder { .. } => "Synchronize Folder",
        // (nothing to download from in this build: the dialog explains the manual install)
        Dialog::SamModel { .. } if sam_by_hand(&app.session.segmenter) => "Install the SAM 3 Model",
        Dialog::SamModel { .. } => "Download the SAM 3 Model?",
        Dialog::About => "About LightCraft",
        Dialog::Shortcuts => "Keyboard Shortcuts",
    }
    .to_string();
    let frame = egui::Frame::window(&ctx.global_style()).inner_margin(egui::Margin::symmetric(16, 12));
    // The import review can be resized (its photo grid takes the room); it keeps a window id of its
    // own so the size it is given doesn't carry over to the other dialogs. Its content scrolls
    // rather than growing the window when the options below the grid get taller (e.g. Copy).
    let import = matches!(dlg, Dialog::Import { .. });
    let window_id = egui::Id::new(if import { "lightcraft-import-dialog" } else { "lightcraft-dialog" });
    let shown = egui::Window::new(crate::i18n::tr(&title)).id(window_id)
        .collapsible(false)
        .resizable(import)
        .vscroll(import)
        .resize(|r| if import { r.default_height(crate::import::DIALOG_SIZE[1]) } else { r })
        .frame(frame)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(match dlg {
            Dialog::Import { .. } => crate::import::DIALOG_SIZE[0],
            Dialog::SmartRules { .. } => 680.0,
            Dialog::AllMetadata { .. } => 620.0,
            Dialog::FaceModel { .. } => 460.0,
            _ => 380.0,
        })
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            match &mut dlg {
                Dialog::AutoStack { gap } => {
                    ui.label(egui::RichText::new(crate::i18n::tr("Stack photos taken within this time of each other:")).color(t.text_label));
                    ui.add(
                        egui::Slider::new(gap, 0.0..=86400.0)
                            .logarithmic(true)
                            .smallest_positive(1.0)
                            .custom_formatter(|v, _| crate::panels::dialogs::fmt_gap(v))
                            .custom_parser(|s| s.trim().trim_end_matches('s').parse().ok()),
                    );
                    let preview = app.session.execute("stack.auto", &json!({"gap": *gap, "preview": true})).unwrap_or_default();
                    let scope = if app.session.selection.ids.len() > 1 { "the selected photos" } else { "the photos in view" };
                    ui.label(
                        egui::RichText::new(crate::i18n::tr_format!("Creates {} stacks from {} of {scope}", preview["stacks"], preview["photos"], scope = crate::i18n::tr(scope))).color(t.text_dim),
                    );
                }
                Dialog::Cull { reject_below, pick_best } => {
                    let n = app.session.selection.ids.len();
                    let scope = if n > 1 { crate::i18n::tr_format!("the {n} selected photos", n = n) } else { crate::i18n::tr_format!("the {} photos in view", app.session.visible_cloned().len()) };
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("Scores {scope} for focus and exposure and finds similar shots taken within seconds of each other (bursts).", scope = scope)).color(t.text_label));
                    ui.add_space(6.0);
                    let r = ui.add(egui::Slider::new(reject_below, 0.0..=80.0).text(crate::i18n::tr("Reject below focus")).step_by(1.0));
                    crate::widgets::register(ui.ctx(), "field:cullReject", r.rect);
                    ui.label(egui::RichText::new(crate::i18n::tr(if *reject_below > 0.0 { "Blurry photos below the score are flagged as rejects." } else { "0: nothing is rejected." })).color(t.text_dim));
                    let r = ui.checkbox(pick_best, crate::i18n::tr("Pick the sharpest photo of each burst"));
                    crate::widgets::register(ui.ctx(), "check:cullPick", r.rect);
                    ui.label(egui::RichText::new(crate::i18n::tr("Scores stay on the photos: filter or make smart albums with Focus and Best of Similar Shots.")).color(t.text_dim));
                }
                Dialog::WhatsNew => {
                    egui::ScrollArea::vertical().max_height(460.0).auto_shrink([false, true]).show(ui, |ui| {
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
                    let r = ui.add(egui::TextEdit::singleline(search).hint_text(crate::i18n::tr("Filter fields")).desired_width(f32::INFINITY));
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
                    egui::ScrollArea::vertical().max_height(440.0).auto_shrink([false, true]).show(ui, |ui| {
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
                    let r = ui.add(egui::TextEdit::singleline(name).hint_text(crate::i18n::tr("Name")).desired_width(f32::INFINITY));
                    crate::widgets::register(ui.ctx(), "field:smartName", r.rect);
                    ui.add_space(6.0);
                    // the folder an album made from a folder view carries is not in the editor, but it counts
                    let folder = id.and_then(|id| app.session.catalog.album(lightcraft_catalog::AlbumId(id))).and_then(|a| a.smart.as_deref().and_then(|f| f.library_folder.clone()));
                    // problems, count and albums: cached until the catalog or the rules change (not per frame)
                    let now = (app.session.clock)();
                    let view = app.caches.rules_view(&app.session.catalog, rules, *id, folder, &now);
                    let env = crate::panels::rules_editor::Env { problems: view.problems.clone(), today: now, albums: view.albums.clone() };
                    egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(ui, |ui| {
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
                    let n = app.session.catalog.query(&rules, &Default::default()).len();
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("Matches: {}", crate::i18n::filter_label(&rules, &app.session.catalog))).color(t.text_label));
                    ui.label(
                        egui::RichText::new(crate::i18n::tr_format!("{n} photo{} now · updates automatically as photos change", if n == 1 { "" } else { "s" }, n = n))
                            .color(t.text_dim),
                    );
                }
                Dialog::CaptureTime { mode, time, days, hours, minutes, zone } => {
                    let n = app.session.targets(&json!({})).len();
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
                                crate::widgets::register(ui.ctx(), "field:captureTime", r.rect);
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
                        let after = delta.and_then(|d| lightcraft_catalog::dates::shift_iso(&cur, d));
                        ui.label(
                            egui::RichText::new(match after {
                                Some(a) => format!("{name}: {} → {}", cur.replace('T', " "), a.replace('T', " ")),
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
                            let te = ui.add(egui::TextEdit::singleline(&mut names[i]).hint_text(colour.as_str()).desired_width(f32::INFINITY));
                            crate::widgets::register(ui.ctx(), format!("field:labelName-{}", colour.to_lowercase()), te.rect);
                        });
                    }
                    field(ui, "Save as set", |ui| {
                        let te = ui.add(egui::TextEdit::singleline(save_as).hint_text(crate::i18n::tr("Optional name")).desired_width(f32::INFINITY));
                        crate::widgets::register(ui.ctx(), "field:labelSetName", te.rect);
                    });
                    ui.label(
                        egui::RichText::new(crate::i18n::tr("Names appear in the label menu, the filter bar and the Info panel, and are written to XMP. Empty = the colour's name."))
                            .color(t.text_dim),
                    );
                }
                Dialog::Rename { template, start } => {
                    let n = app.session.targets(&json!({})).len();
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("{n} photo{}", if n == 1 { "" } else { "s" }, n = n)).color(t.text_dim));
                    let template_id = egui::Id::new("rename-template");
                    let tags_open = field(ui, "Template", |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let w = crate::import::tag_field_width(ui);
                        let r = ui.add(egui::TextEdit::singleline(template).id(template_id).hint_text("{name}").desired_width(w));
                        crate::widgets::register(ui.ctx(), "field:renameTemplate", r.rect);
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
                                    ui.label(egui::RichText::new("→").color(t.text_dim));
                                    ui.label(egui::RichText::new(&pl.to).color(t.text));
                                    ui.end_row();
                                }
                            });
                        }
                        None => {
                            ui.label(egui::RichText::new(crate::i18n::tr("Checking names…")).color(t.text_dim));
                        }
                    }
                    let more = total.saturating_sub(RENAME_PREVIEW_ROWS);
                    if more > 0 {
                        ui.label(egui::RichText::new(crate::i18n::tr_format!("… and {more} more", more = more)).color(t.text_dim));
                    }
                }
                Dialog::RenameKeyword { from, to } => {
                    let n = app.session.catalog.photos().filter(|p| p.meta.keywords.iter().any(|k| lightcraft_catalog::keywords::is_under(k, from))).count();
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
                        .session
                        .catalog
                        .keyword_suggestions(from, into, 12)
                        .into_iter()
                        .filter(|k| !from.iter().any(|f| lightcraft_catalog::keywords::is_under(k, f)))
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
                Dialog::NewAlbum { name, .. } | Dialog::RenameAlbum { name, .. } => {
                    if dialog_field(ui, "field:albumName", name, "Name") {
                        confirm = true;
                    }
                }
                Dialog::CreatePreset { name, group, groups } => {
                    ui.add(egui::TextEdit::singleline(name).hint_text(crate::i18n::tr("Preset name")).desired_width(f32::INFINITY));
                    ui.add(egui::TextEdit::singleline(group).hint_text(crate::i18n::tr("Group")).desired_width(f32::INFINITY));
                    ui.label(egui::RichText::new(crate::i18n::tr("Settings to include")).color(t.text_dim));
                    group_checklist(ui, "presetInclude", groups);
                }
                Dialog::CopySettings { groups } => group_checklist(ui, "copyGroup", groups),
                Dialog::PasteSettings { groups } => {
                    let n = app.session.targets(&json!({})).len();
                    ui.label(
                        egui::RichText::new(crate::i18n::tr_format!("Paste into {n} photo{} — only settings that were copied are pasted", if n == 1 { "" } else { "s" }, n = n))
                            .color(t.text_dim),
                    );
                    group_checklist(ui, "pasteGroup", groups);
                }
                Dialog::ContactSheet { options } => {
                    options.columns = options.columns.clamp(1, 8);
                    options.rows = options.rows.clamp(1, 10);
                    let n = app.session.targets(&json!({})).len();
                    ui.label(format!("{n} selected photos · {} pages", n.div_ceil(options.columns * options.rows)));
                    field(ui, "Paper", |ui| {
                        egui::ComboBox::from_id_salt("contactSheetPaper").selected_text(&options.paper).show_ui(ui, |ui| {
                            ui.selectable_value(&mut options.paper, "a4".into(), "A4");
                            ui.selectable_value(&mut options.paper, "letter".into(), "Letter");
                        });
                    });
                    ui.checkbox(&mut options.landscape, "Landscape");
                    field(ui, "Columns", |ui| { ui.add(egui::DragValue::new(&mut options.columns).range(1..=8)); });
                    field(ui, "Rows", |ui| { ui.add(egui::DragValue::new(&mut options.rows).range(1..=10)); });
                    ui.checkbox(&mut options.captions, "Filename captions");
                    ui.label("150 dpi · sRGB · current edits · fit without cropping");
                    ui.label("Choose a PDF destination after confirming. Print the PDF from your viewer.");
                }
                Dialog::Export { opts, full_size, resize, preset_name, limit_kb, dir } => {
                    use lightcraft_engine::export::{Anchor as P, ExportFormat as F, MetadataPolicy as M, SharpenAmount as A, SharpenFor as S};
                    let n = app.session.selection.ids.len().max(1);
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("{n} photo{}", if n == 1 { "" } else { "s" }, n = n)).color(t.text_dim));
                    // Preset: load a built-in or saved set of options into the dialog
                    field(ui, "Preset", |ui| {
                        let mut chosen = None;
                        let c = egui::ComboBox::from_id_salt("exportPreset").width(220.0).selected_text(crate::i18n::tr("Choose…")).show_ui(ui, |ui| {
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
                        crate::widgets::register(ui.ctx(), "combo:exportPreset", c.response.rect);
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
                    choices(
                        ui,
                        "Format",
                        "exportFormat",
                        &[(F::Jpeg, "JPEG"), (F::Png, "PNG"), (F::Tiff, "TIFF"), (F::Webp, "WebP"), (F::Avif, "AVIF"), (F::Dng, "DNG"), (F::Original, "Original files")],
                        &mut opts.format,
                    );
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
                    if matches!(opts.format, F::Jpeg | F::Tiff | F::Avif) {
                        let tip = "Photos edited in HDR: JPEG with an HDR gain map (looks right everywhere, brighter highlights on HDR displays), AVIF as 10-bit Rec. 2020 PQ; TIFF needs 32-bit float";
                        ui.checkbox(&mut opts.hdr, "HDR output").on_hover_text(tip);
                        if opts.hdr && !opts.hdr_output() {
                            ui.label(egui::RichText::new("HDR TIFF needs 32-bit float.").color(t.text_dim));
                        }
                    }
                    if !rendered {
                    } else if opts.format == F::Avif && opts.hdr {
                        ui.label(egui::RichText::new("Color space: Rec. 2020 PQ (HDR AVIF)").color(t.text_dim));
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
                    }
                    if matches!(opts.format, F::Jpeg | F::Avif) {
                        let mut q = opts.quality as f64;
                        if num(ui, &QUALITY, &mut q) {
                            opts.quality = q as u8;
                        }
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
                                    ui.add(egui::TextEdit::singleline(&mut wm.image).hint_text("logo.png").desired_width(ui.available_width()));
                                });
                            });
                            let mut width = wm.image_width as f64 * 100.0;
                            if num(ui, &WM_IMAGE_WIDTH, &mut width) {
                                wm.image_width = (width / 100.0) as f32;
                            }
                        } else {
                            choices(ui, app.ui.language.tr("Text direction"), "exportWmOrientation", &[(false, app.ui.language.tr("Horizontal text")), (true, app.ui.language.tr("Vertical text"))], &mut wm.vertical);
                            field(ui, "Text", |ui| {
                                ui.add(egui::TextEdit::multiline(&mut wm.text).desired_rows(2).hint_text(crate::i18n::tr("© Your Name")).desired_width(f32::INFINITY))
                            });
                        }
                        choices(
                            ui,
                            "Position",
                            "exportWmAnchor",
                            &[(P::TopLeft, "↖"), (P::TopRight, "↗"), (P::Center, "•"), (P::BottomLeft, "↙"), (P::BottomRight, "↘")],
                            &mut wm.anchor,
                        );
                        let mut size = wm.size as f64 * 100.0;
                        if !graphic && num(ui, &WM_SIZE, &mut size) {
                            wm.size = (size / 100.0) as f32;
                        }
                        let mut op = wm.opacity as f64 * 100.0;
                        if num(ui, &WM_OPACITY, &mut op) {
                            wm.opacity = (op / 100.0) as f32;
                        }
                        if !graphic {
                            ui.checkbox(&mut wm.shadow, crate::i18n::tr("Shadow"));
                        }
                    }
                    }
                    ui.add_space(4.0);
                    if opts.format == F::Tiff {
                        use lightcraft_engine::export::TiffCompression as Z;
                        choices(ui, "Compression", "exportTiffCompression", &[(Z::None, "None"), (Z::Lzw, "LZW"), (Z::Deflate, "ZIP")], &mut opts.tiff_compression);
                    }
                    if opts.format == F::Dng {
                        use lightcraft_engine::export::DngCompression as Z;
                        choices(ui, "Compression", "exportDngCompression", &[(Z::Lossless, "Lossless"), (Z::Deflate, "ZIP"), (Z::Uncompressed, "None")], &mut opts.dng_compression);
                    }
                    let naming_id = egui::Id::new("export-naming");
                    let tags_open = field(ui, "File name", |ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let w = crate::import::tag_field_width(ui);
                        ui.add(egui::TextEdit::singleline(&mut opts.naming).id(naming_id).hint_text("{name}-{seq}  ·  {date}  ·  {title}  ·  {folder}").desired_width(w));
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
                    field(ui, "Folder", |ui| {
                        trailing_button_row(ui, |ui| {
                            if app.services.pick_folder.is_some()
                                && crate::widgets::text_button(ui, "exportChooseFolder", crate::i18n::tr("Choose…"), false).clicked()
                                && let Some(pick) = app.services.pick_folder.as_mut()
                                && let Some(d) = pick()
                            {
                                *dir = d;
                            }
                            ui.add(egui::TextEdit::singleline(dir).desired_width(ui.available_width()));
                        });
                    });
                    field(ui, "Subfolder", |ui| {
                        ui.add(egui::TextEdit::singleline(&mut opts.subfolder).hint_text(crate::i18n::tr("none")).desired_width(f32::INFINITY))
                    });
                    use lightcraft_engine::export::Conflict as K;
                    choices(
                        ui,
                        "If file exists",
                        "exportConflict",
                        &[(K::Unique, "Add number"), (K::Overwrite, "Overwrite"), (K::Skip, "Skip")],
                        &mut opts.conflict,
                    );
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
                            ui.add(egui::TextEdit::singleline(preset_name).hint_text(crate::i18n::tr("Preset name")).desired_width(ui.available_width()));
                        });
                    });
                }
                Dialog::Merge { opts } => crate::merge::body(app, ui, opts),
                Dialog::Import { opts } => crate::import::body(app, ui, opts),
                Dialog::Settings { tab } => crate::panels::settings::body(app, ui, tab),
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
                } => {
                    ui.label(egui::RichText::new(crate::i18n::tr("Keyword Name")).color(t.text_dim));
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
                            crate::widgets::register(ui.ctx(), "check:keywordInside", r.rect);
                        }
                        let n = app.session.selection.ids.len();
                        let r = ui.add_enabled(n > 0, egui::Checkbox::new(add_to_selected, crate::i18n::tr("Add to Selected Photos")));
                        crate::widgets::register(ui.ctx(), "check:keywordAddToSelected", r.rect);
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
                        crate::widgets::register(ui.ctx(), id, r.rect);
                    }
                }
                Dialog::KeywordSet { replaces, name, slots, as_new } => {
                    ui.label(egui::RichText::new(crate::i18n::tr("Set name")).color(t.text_dim));
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
                Dialog::ConfirmDelete { count } => {
                    let what = if *count == 1 { crate::i18n::tr("this photo").to_string() } else { crate::i18n::tr_format!("these {count} photos", count = count) };
                    ui.label(crate::i18n::tr_format!("Move {what} to Recently Deleted?", what = what));
                    ui.label(egui::RichText::new(crate::i18n::tr("They can be restored from Recently Deleted until it is emptied.")).color(t.text_dim));
                }
                d @ Dialog::SynchronizeFolder { .. } => crate::sync::body(app, ui, d),
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
                    let tab_id = egui::Id::new("about_tab");
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
                            let discord = egui::Button::new(egui::RichText::new(crate::i18n::tr("Join the ArtCraft Discord")).font(t.semibold(15.0)).color(egui::Color32::WHITE))
                                .fill(t.accent)
                                .min_size(egui::vec2(260.0, 34.0));
                            let r = ui.add(discord).on_hover_text(crate::links::DISCORD);
                            crate::widgets::register(ui.ctx(), "button:aboutDiscord", r.rect);
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
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                // a model file LightCraft cannot use has nothing to confirm
                let unusable_model = matches!(&dlg, Dialog::DenoiseModel { info, .. } | Dialog::FaceModel { info, .. } if info["kind"] == "unsupported");
                let informational = unusable_model || matches!(dlg, Dialog::About | Dialog::Shortcuts | Dialog::Settings { .. });
                let sam = &app.session.segmenter;
                let (sam_installed, sam_running, sam_failed) = (sam.installed(), sam.download_status().running, sam.download_status().error.is_some());
                // no download location in this build: nothing to offer but the manual install
                let sam_nowhere = sam_by_hand(sam);
                let cancel = match &dlg {
                    Dialog::SamModel { .. } if sam_running || sam_installed || sam_nowhere => "Close",
                    Dialog::SamModel { .. } => "Not Now",
                    _ => "Cancel",
                };
                if !informational {
                    let r = ui.button(crate::i18n::tr(cancel));
                    crate::widgets::register(ui.ctx(), "button:dialogCancel", r.rect);
                    if r.clicked() {
                        close = true;
                    }
                }
                let add_label;
                let ok = match &dlg {
                    Dialog::Import { opts } => {
                        let n = opts.selected_paths().len();
                        let verb = if opts.copy && opts.move_files { "Move" } else { "Import" };
                        add_label = crate::i18n::tr_format!("{verb} {n} Photo{}", if n == 1 { "" } else { "s" }, verb = crate::i18n::tr(verb), n = n);
                        add_label.as_str()
                    }
                    Dialog::ContactSheet { .. } => "Choose PDF…",
                    Dialog::Merge { .. } => "Merge",
                    Dialog::DenoiseModel { info, .. } if info["download"].is_string() => "Accept & Download",
                    Dialog::DenoiseModel { .. } => "Install",
                    Dialog::FaceModel { info, .. } if !informational && info["download"].is_string() => "Download",
                    Dialog::FaceModel { .. } if !informational => "Install",
                    Dialog::ConfirmDelete { .. } | Dialog::DeleteKeyword { .. } => "Delete",
                    // (not "Merge": that is photo merging, HDR / panorama, in some languages)
                    Dialog::MoveKeyword { .. } => "Merge Keywords",
                    Dialog::KeywordSet { .. } => "Save",
                    Dialog::KeywordTag { editing: None, .. } => "Create",
                    Dialog::KeywordTag { .. } => "Save",
                    Dialog::RemoveFolder { .. } => "Remove",
                    Dialog::SynchronizeFolder { .. } => "Synchronize",
                    Dialog::SamModel { then: Some(_), .. } if sam_installed => "Continue",
                    Dialog::SamModel { .. } if sam_installed || sam_running || sam_nowhere => "",
                    Dialog::SamModel { error, .. } if error.is_some() || sam_failed => "Try Again",
                    Dialog::SamModel { .. } => "Download",
                    _ if informational => "Close",
                    _ => "OK",
                };
                // installing waits for the licence to be accepted
                // (and Synchronize waits for the scan)
                let can_confirm = informational
                    || !matches!(
                        &dlg,
                        Dialog::DenoiseModel { accepted: false, .. } | Dialog::FaceModel { accepted: false, .. } | Dialog::SynchronizeFolder { counts: None, .. }
                    )
                        // smart-album rules that can't mean anything wait to be fixed
                        && !matches!(&dlg, Dialog::SmartRules { id, rules, .. } if !rules.check_for(&app.session.catalog, id.map(lightcraft_catalog::AlbumId)).is_empty());
                let r = (!ok.is_empty()).then(|| ui.add_enabled(can_confirm, egui::Button::new(crate::i18n::tr(ok))));
                if let Some(r) = &r {
                    crate::widgets::register(ui.ctx(), "button:dialogOk", r.rect);
                }
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
        });
    if let Some(w) = shown {
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
            // does Export (e.g. no folder chosen) so the choices aren't lost
            Err(e)
                if matches!(
                    dlg,
                    Dialog::Import { .. }
                        | Dialog::Export { .. }
                        | Dialog::ContactSheet { .. }
                        | Dialog::NewAlbum { .. }
                        | Dialog::NewSmartAlbum { .. }
                        | Dialog::SmartRules { .. }
                        | Dialog::KeywordTag { .. }
                        | Dialog::KeywordSet { .. }
                        | Dialog::SynchronizeFolder { .. }
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
    app.ui.dialog = if close { None } else { Some(dlg) };
}

/// Apply a dialog's action (also used by `ui.dialog.confirm`).
/// `90` → `1 min 30 s`.
pub fn fmt_gap(v: f64) -> String {
    let v = v.round() as u64;
    match v {
        0..60 => format!("{v} s"),
        60..3600 if v.is_multiple_of(60) => format!("{} min", v / 60),
        60..3600 => format!("{} min {} s", v / 60, v % 60),
        3600..86400 if v.is_multiple_of(3600) => format!("{} h", v / 3600),
        3600..86400 => format!("{} h {} min", v / 3600, v % 3600 / 60),
        _ => crate::i18n::tr("1 day").into(),
    }
}

/// Whether a dialog stays open after its action succeeded (the SAM 3 dialog while the model
/// downloads).
pub fn keeps_open(app: &LightcraftApp, dlg: &Dialog) -> bool {
    matches!(dlg, Dialog::SamModel { .. }) && !app.session.segmenter.installed()
}

/// No SAM 3 model, no download running and nowhere to download it from: installing it by hand is
/// all the dialog can offer.
fn sam_by_hand(sam: &lightcraft_engine::segment::Segmenter) -> bool {
    !sam.installed() && !sam.download_status().running && sam.mirrors().is_empty()
}

/// The model installation guide.
const SAM_HELP: &str = "https://github.com/storytold/lightcraft/blob/main/docs/ai-masks.md#getting-the-model";

/// Show the SAM 3 model folder in the file manager (created first, so there is something to show).
fn show_model_folder(app: &mut LightcraftApp, dir: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let reveal = app.services.reveal.as_mut().ok_or("not available here")?;
    reveal(&dir.to_string_lossy())
}

/// The SAM 3 dialog: what the model is, its size and licence, and the download's progress.
fn sam_model_body(app: &mut LightcraftApp, ui: &mut egui::Ui, error: Option<&str>) {
    use lightcraft_engine::segment::{LICENSE_NAME, LICENSE_URL, MODEL_BYTES};
    let t = Tokens::get(ui.ctx());
    let seg = &app.session.segmenter;
    let d = seg.download_status();
    if seg.installed() {
        ui.label(crate::i18n::tr("The SAM 3 model is installed: Object and Describe masks are ready."));
        return;
    }
    let gb = |b: u64| b as f64 / 1e9;
    ui.label(crate::i18n::tr(
        "Object and Describe masks use SAM 3, Meta's segmentation model. It isn't part of LightCraft, and everything else works without it.",
    ));
    let dir = seg.dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default();
    ui.label(format!("{} {:.1} GB, {} {dir}", crate::i18n::tr("A one-time download of about"), gb(MODEL_BYTES), crate::i18n::tr("saved in")));
    ui.label(
        egui::RichText::new(format!(
            "{} {LICENSE_NAME} — {}",
            crate::i18n::tr("Licence:"),
            crate::i18n::tr("Meta's terms, not LightCraft's. Downloading it means accepting them.")
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
        let frac = if d.total > 0 { d.done as f64 / d.total as f64 } else { 0.0 };
        let text = format!("{:.2} / {:.2} GB · {}", gb(d.done), gb(d.total), d.file);
        let r = ui.add(egui::ProgressBar::new(frac as f32).text(text));
        crate::widgets::register(ui.ctx(), "progress:samDownload", r.rect);
        let r = ui.button(crate::i18n::tr("Cancel Download"));
        crate::widgets::register(ui.ctx(), "button:samCancel", r.rect);
        if r.clicked() {
            app.session.segmenter.cancel_download();
        }
        ui.label(egui::RichText::new(crate::i18n::tr("You can close this: the download continues, and resumes if interrupted.")).color(t.text_dim));
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        return;
    }
    if seg.mirrors().is_empty() {
        // nothing to download from: say how to install it by hand, with the folder and the guide a click away
        let dir = seg.dir.clone();
        ui.label(
            egui::RichText::new(crate::i18n::tr(
                "This build can't download the model yet. To install it by hand, put model.safetensors, vocab.json and merges.txt from Meta's facebook/sam3 in the folder above: Object and Describe work as soon as they are there.",
            ))
            .color(t.text_dim),
        );
        ui.horizontal_wrapped(|ui| {
            if let Some(dir) = dir.filter(|_| app.services.reveal.is_some()) {
                let r = ui.button(crate::i18n::tr(crate::menus::reveal_label()));
                crate::widgets::register(ui.ctx(), "button:samFolder", r.rect);
                if r.clicked()
                    && let Err(e) = show_model_folder(app, &dir)
                {
                    app.toast(ui.ctx(), e);
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
fn created_in(app: &mut LightcraftApp, command: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    let r = app.run(command, params)?;
    app.ui.reveal_album = r.get("id").and_then(serde_json::Value::as_u64);
    Ok(r)
}

/// The albums an Album rule in smart album `editing` picks from, in the sidebar's order: the album
/// itself and any smart album that tests it (testing it back would loop) are blocked, with why.
pub(crate) fn album_entries(cat: &lightcraft_catalog::Catalog, editing: Option<u64>) -> Vec<crate::album_picker::AlbumEntry> {
    let editing = editing.map(lightcraft_catalog::AlbumId);
    crate::album_picker::entries_from(cat, |a| match editing {
        Some(e) if a.id == e => Some(crate::i18n::tr("This is the album you're editing").to_string()),
        Some(e) if cat.album_reaches(a.id, e) => Some(crate::i18n::tr("It tests this album, so testing it back would loop").to_string()),
        _ => None,
    })
}

pub fn confirm_dialog(app: &mut LightcraftApp, dlg: &Dialog) -> Result<serde_json::Value, String> {
    match dlg {
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
        Dialog::NewAlbum { name, folder, parent } => {
            created_in(app, "album.create", json!({"name": name, "folder": folder, "addSelected": !folder, "parent": parent}))
        }
        Dialog::RenameAlbum { id, name } => app.run("album.rename", json!({"id": id, "name": name})),
        Dialog::TextPrompt { value, command, params, key, .. } => {
            let mut p = params.clone();
            p[key.as_str()] = json!(value.trim());
            app.run(command, p)
        }
        Dialog::CaptureTime { mode, time, days, hours, minutes, zone } => app.run(
            "photo.setCaptureTime",
            match mode.as_str() {
                "set" => json!({"time": time}),
                "shift" => json!({"shift": *days as i64 * 86_400 + *hours as i64 * 3600 + *minutes as i64 * 60}),
                _ => json!({"hours": zone}),
            },
        ),
        Dialog::LabelNames { names, save_as } => {
            let mut m = serde_json::Map::new();
            for (l, n) in lightcraft_catalog::ColorLabel::ALL.iter().zip(names) {
                m.insert(format!("{l:?}").to_lowercase(), if n.trim().is_empty() { serde_json::Value::Null } else { json!(n.trim()) });
            }
            let r = app.run("label.setNames", json!({"names": m}));
            if r.is_ok() && !save_as.trim().is_empty() {
                return app.run("label.saveSet", json!({"name": save_as.trim()}));
            }
            r
        }
        Dialog::Rename { template, start } => app.run("photo.rename", json!({"template": template, "start": start})),
        Dialog::RenameKeyword { from, to } => app.run("keyword.rename", json!({"from": from, "to": to})),
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
                    app.run("keyword.create", params)
                }
            }
        }
        Dialog::DeleteKeyword { keyword, .. } => app.run("keyword.delete", json!({"keyword": keyword})),
        Dialog::KeywordSet { replaces, name, slots, as_new } => {
            use lightcraft_catalog::keywords::same;
            let name = name.trim();
            if name.is_empty() {
                return Err(crate::i18n::tr("A keyword set needs a name").to_string());
            }
            if same(name, lightcraft_engine::cmd::keywords::RECENT) {
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
        Dialog::DenoiseModel { info, accepted } => crate::panels::denoise::install(app, info, *accepted),
        Dialog::FaceModel { path, info, accepted } => crate::panels::faces::install(app, path, info, *accepted),
        Dialog::AllMetadata { .. } | Dialog::SystemInfo { .. } | Dialog::WhatsNew => Ok(serde_json::Value::Null),
        Dialog::Cull { reject_below, pick_best } => {
            let mut p = json!({"pickBest": pick_best});
            if *reject_below > 0.0 {
                p["rejectBelow"] = json!(reject_below);
            }
            app.run("photo.analyze", p)
        }
        Dialog::SmartRules { id, name, rules, parent } => {
            // refused before anything changes (not even the name)
            if let Some(problem) = rules.check_for(&app.session.catalog, id.map(lightcraft_catalog::AlbumId)).first() {
                return Err(problem.to_string());
            }
            let name = if name.trim().is_empty() { "Smart Album".to_string() } else { name.trim().to_string() };
            match id {
                // the name and the rules together: one undo step, both or neither
                Some(id) => app.run("album.setRules", json!({"id": id, "name": name, "replace": true, "rules": {"ruleSet": rules}})),
                None => created_in(app, "album.createSmart", json!({"name": name, "rules": {"ruleSet": rules}, "parent": parent})),
            }
        }
        Dialog::NewSmartAlbum { name, parent } => {
            created_in(app, "album.createSmart", json!({"name": if name.trim().is_empty() { "Smart Album" } else { name }, "parent": parent}))
        }
        Dialog::CreatePreset { name, group, groups } => app.run(
            "preset.create",
            json!({
                "name": if name.trim().is_empty() { "My Preset" } else { name },
                "group": if group.trim().is_empty() { "User Presets" } else { group },
                "groups": groups,
            }),
        ),
        Dialog::ContactSheet { options } => app.run("app.contactSheet", json!(options)),
        Dialog::CopySettings { groups } => app.run("develop.copy", json!({"groups": groups})),
        Dialog::PasteSettings { groups } => app.run("develop.paste", json!({"groups": groups})),
        Dialog::Export { opts, full_size, resize, limit_kb, dir, .. } => {
            let mut p = export_dialog_params(opts, *full_size, resize, *limit_kb);
            p["dir"] = json!(dir);
            p["background"] = json!(true);
            app.run("app.export", p)
        }
        Dialog::Merge { opts } => crate::merge::start_final(app, opts),
        Dialog::Import { opts } => crate::import::start(app, opts),
        Dialog::ConfirmDelete { .. } => app.run("photo.delete", json!({})),
        Dialog::RemoveFolder { path, disk, .. } => app.run("library.removeFolder", json!({"path": path, "disk": disk})),
        d @ Dialog::SynchronizeFolder { .. } => crate::sync::confirm(app, d),
        Dialog::About | Dialog::Shortcuts | Dialog::Settings { .. } => Ok(serde_json::Value::Null),
    }
}

/// Open a one-field dialog that runs `command` with `params` + `{key: typed value}`.
pub fn prompt(app: &mut LightcraftApp, title: &str, hint: &str, value: &str, command: &str, params: serde_json::Value, key: &str) {
    app.ui.dialog =
        Some(Dialog::TextPrompt { title: title.into(), hint: hint.into(), value: value.into(), command: command.into(), params, key: key.into() });
}

/// The Export dialog's choices as `app.export` params (without the folder).
fn export_dialog_params(
    opts: &lightcraft_engine::export::ExportOptions,
    full_size: bool,
    resize: &lightcraft_engine::export::Resize,
    limit_kb: u32,
) -> serde_json::Value {
    let o = lightcraft_engine::export::ExportOptions {
        resize: (!full_size).then_some(*resize),
        limit_kb: (limit_kb > 0).then_some(limit_kb),
        ..opts.clone()
    };
    o.to_json()
}

// ------------------------------------------------------------------------------------------ dialog widgets

/// Two columns of settings-group checkboxes (Copy Settings, Create Preset) plus All / None.
fn group_checklist(ui: &mut egui::Ui, tag: &str, groups: &mut Vec<String>) {
    let key_of = |g: &SettingsGroup| serde_json::to_value(g).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    ui.columns(2, |cols| {
        for (i, g) in SettingsGroup::ALL.iter().enumerate() {
            let key = key_of(g);
            let mut on = groups.contains(&key);
            let r = cols[i % 2].checkbox(&mut on, crate::i18n::tr(g.label()));
            crate::widgets::register(&r.ctx, format!("{tag}:{key}"), r.rect);
            if r.changed() {
                if on {
                    groups.push(key);
                } else {
                    groups.retain(|x| *x != key);
                }
            }
        }
    });
    ui.horizontal(|ui| {
        let all = ui.small_button(crate::i18n::tr("All"));
        crate::widgets::register(ui.ctx(), format!("{tag}:all"), all.rect);
        if all.clicked() {
            *groups = SettingsGroup::ALL.iter().map(key_of).collect();
        }
        let none = ui.small_button(crate::i18n::tr("None"));
        crate::widgets::register(ui.ctx(), format!("{tag}:none"), none.rect);
        if none.clicked() {
            groups.clear();
        }
    });
}

/// Width of the label column in dialogs.
const LABEL_W: f32 = 78.0;

const fn spec(id: &'static str, label: &'static str, min: f64, max: f64, default: f64, step: f64) -> ControlSpec {
    ControlSpec { id, label, section: Section::Light, min, max, default, step, decimals: 0, track: Track::Plain }
}
const QUALITY: ControlSpec = spec("export.quality", "Quality", 1.0, 100.0, 90.0, 1.0);
const LIMIT_KB: ControlSpec = spec("export.limitKb", "Limit file size (KB, 0 = off)", 0.0, 20_000.0, 0.0, 10.0);
const SIZE_PX: ControlSpec = spec("export.sizePx", "Pixels", 16.0, 20_000.0, 2048.0, 16.0);
const SIZE_W: ControlSpec = spec("export.sizeW", "Width (px)", 16.0, 20_000.0, 2048.0, 16.0);
const SIZE_H: ControlSpec = spec("export.sizeH", "Height (px)", 16.0, 20_000.0, 2048.0, 16.0);
const SIZE_MP: ControlSpec = ControlSpec { decimals: 1, ..spec("export.sizeMp", "Megapixels", 0.1, 100.0, 12.0, 0.1) };
const SIZE_PCT: ControlSpec = spec("export.sizePercent", "Percent", 1.0, 400.0, 50.0, 1.0);
const START_NUMBER: ControlSpec = spec("export.startNumber", "Start number", 1.0, 9999.0, 1.0, 1.0);
const PPI: ControlSpec = spec("export.ppi", "Resolution (ppi)", 1.0, 1200.0, 240.0, 1.0);

/// Image Sizing: full size, or a resize mode and its value(s), don't enlarge, ppi.
fn export_size(ui: &mut egui::Ui, full: &mut bool, r: &mut lightcraft_engine::export::Resize, ppi: &mut u16) {
    use lightcraft_engine::export::ResizeMode as R;
    const MODES: [(R, &str); 7] = [
        (R::LongEdge, "Long Edge"),
        (R::ShortEdge, "Short Edge"),
        (R::Width, "Width"),
        (R::Height, "Height"),
        (R::Dimensions, "Width × Height"),
        (R::Megapixels, "Megapixels"),
        (R::Percent, "Percentage"),
    ];
    let r_full = ui.checkbox(full, crate::i18n::tr("Full size"));
    crate::widgets::register(ui.ctx(), "check:exportFullSize", r_full.rect);
    if !*full {
        field(ui, "Resize to", |ui| {
            let cur = MODES.iter().find(|m| m.0 == r.mode).map_or("Long Edge", |m| m.1);
            let before = r.mode;
            let c = egui::ComboBox::from_id_salt("exportResizeMode").width(150.0).selected_text(crate::i18n::tr(cur)).show_ui(ui, |ui| {
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
                if r.mode == R::Dimensions && r.height == 0 {
                    r.height = r.value as u32;
                }
            }
        });
        let mut v = r.value as f64;
        let spec = match r.mode {
            R::Megapixels => &SIZE_MP,
            R::Percent => &SIZE_PCT,
            R::Dimensions => &SIZE_W,
            _ => &SIZE_PX,
        };
        if num(ui, spec, &mut v) {
            r.value = v as f32;
        }
        if r.mode == R::Dimensions {
            let mut h = r.height as f64;
            if num(ui, &SIZE_H, &mut h) {
                r.height = h as u32;
            }
        }
        let c = ui.checkbox(&mut r.dont_enlarge, crate::i18n::tr("Don't enlarge"));
        crate::widgets::register(ui.ctx(), "check:exportDontEnlarge", c.rect);
    }
    let mut p = *ppi as f64;
    if num(ui, &PPI, &mut p) {
        *ppi = p as u16;
    }
}
const WM_IMAGE_WIDTH: ControlSpec = spec("export.watermarkImageWidth", "Width (% of photo)", 2.0, 100.0, 20.0, 1.0);
const WM_SIZE: ControlSpec = spec("export.watermarkSize", "Size (% of short edge)", 1.0, 15.0, 3.5, 0.5);
const WM_OPACITY: ControlSpec = spec("export.watermarkOpacity", "Opacity (%)", 5.0, 100.0, 70.0, 1.0);

/// A themed slider row editing `v`; true when it changed.
fn num(ui: &mut egui::Ui, spec: &ControlSpec, v: &mut f64) -> bool {
    match crate::widgets::slider(ui, spec, *v, true, None).value {
        Some(n) => {
            *v = n;
            true
        }
        None => false,
    }
}

/// A labelled row (fixed label column).
fn field<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
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

/// A dialog's one text field: it keeps the focus (except while its own menu has it) and opens
/// with its text selected, so typing replaces it. True when Return confirms the dialog; Esc
/// cancels the dialog (`show`). Taking the focus back with a bare `TextEdit` every frame hid the
/// focus Return gives up, and Return did nothing.
fn dialog_field(ui: &mut egui::Ui, widget: &str, text: &mut String, hint: &str) -> bool {
    let r = crate::text_field::TextField::singleline(widget, text).hint(crate::i18n::tr(hint)).width(f32::INFINITY).select_on_focus(true).show(ui);
    if !r.editing {
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
