use craft::ui::contract::{Props as _, Show as _};
use serde_json::json;

use super::controls::{self, button, choice, register};
use super::tags::{tag_field_width, tag_help, tag_toggle, unknown_tags_warning};
use super::{Color32, ImportDialog, LightcraftApp, Rect, Sense, Slot, Stroke, StrokeKind, Tokens, Value, Vec2, pos2, vec2};

/// The review dialog's first size (points). It can be resized; the photo grid takes the height.
pub(crate) const DIALOG_SIZE: [f32; 2] = [960.0, 720.0];

/// Memory keys: where the photo grid ended in a run, and the height the dialog's content (its buttons) took below it.
const GRID_BOTTOM: &str = "import-grid-bottom";

const CONTENT_BOTTOM: &str = "import-content-bottom";

/// Room below the grid before it has been measured (more than the options and buttons take).
pub(super) const DEFAULT_BELOW_GRID: f32 = 360.0;

pub(crate) const THUMB_SIZE_MIN: f32 = 80.0;
pub(crate) const THUMB_SIZE_MAX: f32 = 320.0;
pub(crate) const THUMB_SIZE_DEFAULT: f32 = 116.0;

/// Bound values from saved preferences and the control channel before laying out the grid.
pub(crate) fn thumb_size(size: f32) -> f32 {
    if size.is_finite() { size.clamp(THUMB_SIZE_MIN, THUMB_SIZE_MAX) } else { THUMB_SIZE_DEFAULT }
}

/// A few physical-pixel sizes avoid decoding on every point of a slider drag. The engine caps
/// candidate previews at 1024 pixels; this covers the largest tile at up to 3× display density.
pub(crate) fn thumb_edge(size: f32, pixels_per_point: f32) -> usize {
    let scale = if pixels_per_point.is_finite() && pixels_per_point > 0.0 { pixels_per_point } else { 1.0 };
    let pixels = (thumb_size(size) - 8.0) * scale;
    [192, 384, 512, 768, 1024].into_iter().find(|edge| *edge as f32 >= pixels).unwrap_or(1024)
}

/// Use the shared control contract for pointer, keyboard, exact-value entry and reset interactions.
fn thumb_size_slider(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    use lightcraft_develop::controls::{ControlSpec, Section, Track};
    const SIZE: ControlSpec = ControlSpec {
        id: "importThumbSize",
        label: "Thumbnail size",
        section: Section::Light,
        min: THUMB_SIZE_MIN as f64,
        max: THUMB_SIZE_MAX as f64,
        default: THUMB_SIZE_DEFAULT as f64,
        step: 1.0,
        decimals: 0,
        track: Track::Plain,
    };
    if let Some(size) = controls::number(ui, &SIZE, f64::from(thumb_size(app.ui.import_thumb_size))) {
        let _ = app.run("view.importThumbSize", json!({"size": SIZE.clamp(size)}));
    }
}

/// Height of the photo grid: what is `available` once `below` (options and buttons) is left free,
/// in whole points so the window never grows by a fraction each frame, and at least one `row`.
pub(crate) fn grid_height(available: f32, below: f32, row: f32) -> f32 {
    let h = (available - below.max(0.0)).floor();
    if h.is_finite() { h.max(row) } else { (DIALOG_SIZE[1] - DEFAULT_BELOW_GRID).max(row) }
}

/// Where the review dialog's body starts and the height it has to show, for the photo grid this frame.
const BODY_ROOM: &str = "lc-import-body-room";

/// Note the room the review dialog's body has (`room` points from where `ui` starts), for [`grid_height`].
///
/// Noted in every run of the body, measuring ones too: each run places the body elsewhere, and the grid reads where its
/// own run's body starts.
///
/// A real run measures the room itself: the height of the body's visible part (its clip), which the dialog's own
/// chrome and the window's size decide; `room` stands in until a real run has measured it. A measuring run uses the
/// real run's room, so both size the grid alike. A room that changed asks for another frame.
pub(crate) fn note_body_room(ui: &egui::Ui, room: f32) {
    let measuring = controls::measuring(ui);
    let top = ui.cursor().top();
    let seen = egui::Id::unique((BODY_ROOM, "seen"));
    let ctx = ui.ctx();
    if !measuring {
        let clip = ui.clip_rect();
        let visible = (clip.bottom() - top.max(clip.top())).floor();
        if visible.is_finite() && visible > 0.0 && ctx.data(|m| m.get_temp::<f32>(seen)).is_none_or(|kept| (kept - visible).abs() > 0.5) {
            ctx.data_mut(|m| m.insert_temp(seen, visible));
            ctx.request_repaint();
        }
    }
    let room = ctx.data(|m| m.get_temp::<f32>(seen)).unwrap_or(room);
    ctx.data_mut(|m| m.insert_temp(egui::Id::unique((BODY_ROOM, measuring)), (top, room)));
}

/// Remember where the review dialog's content ends this frame (after its buttons), for
/// [`grid_height`] on the next.
///
/// Noted in the framework's measuring run too: the run that lays the body out for the view comes after it in the same
/// pass, so the grid there leaves room for rows that appeared this pass (a Copy's destination) instead of a frame
/// later, which moved everything below the grid between two frames.
///
/// What it keeps is the height below the grid, from a real run only: a measuring run stands in for a few rows with rows
/// of another height and reserves the real body's height (`body`), so its bottom says nothing about the rows. Both
/// kinds of run then size the grid alike. A height that changed asks for another frame, so the grid follows it before
/// the review counts as settled.
pub(crate) fn note_dialog_bottom(ui: &egui::Ui) {
    if controls::measuring(ui) {
        return;
    }
    let y = ui.min_rect().bottom();
    let ctx = ui.ctx();
    let Some(grid) = ctx.data(|m| m.get_temp::<f32>(egui::Id::unique((GRID_BOTTOM, false)))) else { return };
    let below = y - grid;
    let key = egui::Id::unique(CONTENT_BOTTOM);
    if ctx.data(|m| m.get_temp::<f32>(key)).is_none_or(|kept| (kept - below).abs() > 0.5) {
        ctx.data_mut(|m| m.insert_temp(key, below));
        ctx.request_repaint();
    }
}

/// Skipped candidates: (already in the library, identical copies within the source, in Recently
/// Deleted). Files of photos in Recently Deleted are not "already in the library", and a file with
/// the same bytes as one earlier in the list (no library photo) is a copy in the source.
pub(super) fn dup_counts(d: &ImportDialog) -> (usize, usize, usize) {
    let trashed = d.trashed.iter().filter(|t| **t).count();
    let in_source = d.candidates.iter().filter(|c| c.duplicate.as_deref() == Some("content") && c.existing.is_none()).count();
    let dups = d.candidates.iter().filter(|c| c.duplicate.is_some()).count().saturating_sub(trashed).saturating_sub(in_source);
    (dups, in_source, trashed)
}

/// The one line that says what the scan left out, for the review's header: a folder walk that
/// stopped at its limit, folders it couldn't read, files that are not a supported photo type.
/// `None` when it left nothing out (the review then shows no line at all).
pub(crate) fn scan_note(d: &ImportDialog) -> Option<String> {
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let mut parts: Vec<String> = Vec::new();
    if d.scan_limited {
        parts.push(crate::i18n::tr("Stopped at the scan limit: there may be more photos").to_string());
    }
    if d.unreadable > 0 {
        parts.push(crate::i18n::tr_format!("{n} folder{} could not be read", plural(d.unreadable), n = d.unreadable));
    }
    if d.unsupported > 0 {
        parts.push(crate::i18n::tr_format!("{n} file{} skipped (not a supported photo type)", plural(d.unsupported), n = d.unsupported));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The dialog body: options, then the candidate grid.
pub fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &mut ImportDialog) {
    // The height the body took in its last real run: a measuring run stands in for a few rows (the album picker, the
    // source summary) with rows of another height, so it reserves at least that height, or the dialog would measure
    // its body shorter than it lays out and the last rows of an opened recipe could not be scrolled to.
    let real_height = egui::Id::unique("lc-import-body-height");
    let top = ui.cursor().top();
    if controls::measuring(ui) {
        // Keep speculative disabled state on a child, never on the caller's real Ui.
        ui.scope_builder(egui::UiBuilder::new().disabled(), |ui| body_inner(app, ui, &mut d.clone()));
        let took = ui.min_rect().bottom() - top;
        if let Some(height) = ui.ctx().data(|m| m.get_temp::<f32>(real_height)).filter(|height| *height > took) {
            ui.allocate_space(vec2(0.0, height - took));
        }
    } else {
        body_inner(app, ui, d);
        let took = ui.min_rect().bottom() - top;
        if ui.ctx().data(|m| m.get_temp::<f32>(real_height)).is_none_or(|height| (height - took).abs() > 0.5) {
            ui.ctx().data_mut(|m| m.insert_temp(real_height, took));
            ui.ctx().request_repaint();
        }
    }
}

/// The import review's preset choices: None (""), then the presets under their group headings. Kept until the presets
/// or the language change, since a library can hold thousands of presets.
fn preset_entries(app: &LightcraftApp, ui: &egui::Ui) -> std::sync::Arc<Vec<craft::ui::contract::Entry<String>>> {
    use craft::ui::contract::Entry;
    use std::sync::Arc;
    type Cached = (u64, crate::i18n::Locale, Arc<Vec<Entry<String>>>);
    let generation = app.session.presets.generation();
    let language = crate::i18n::language();
    let cache = egui::Id::unique("import-preset-entries");
    if let Some((_, _, entries)) = ui.data(|data| data.get_temp::<Cached>(cache)).filter(|(old, locale, _)| *old == generation && *locale == language) {
        return entries;
    }
    let mut entries = vec![Entry::new(String::new(), crate::i18n::tr("None").to_owned())];
    let mut group = None;
    for preset in app.session.presets.iter() {
        if group != Some(preset.group.as_str()) {
            group = Some(preset.group.as_str());
            entries.push(Entry::heading(crate::i18n::builtin_label(&preset.group, preset.builtin).to_owned()));
        }
        entries.push(Entry::new(preset.id.clone(), crate::i18n::builtin_label(&preset.name, preset.builtin).to_owned()));
    }
    let entries = Arc::new(entries);
    ui.data_mut(|data| data.insert_temp(cache, (generation, language, Arc::clone(&entries))));
    entries
}

fn body_inner(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &mut ImportDialog) {
    let t = Tokens::get(ui.ctx());
    let n = d.candidates.len();
    let (dups, in_source, trashed) = dup_counts(d);
    let sel = d.selected_paths().len();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::tr_format!("{n} found · {sel} selected", n = n, sel = sel)).color(t.text));
        if dups > 0 {
            ui.label(egui::RichText::new(crate::i18n::tr_format!("· {dups} already in the library", dups = dups)).color(t.text_dim));
        }
        if in_source > 0 {
            ui.label(egui::RichText::new(crate::i18n::tr_format!("· {in_source} identical copies", in_source = in_source)).color(t.text_dim))
                .on_hover_text(crate::i18n::tr("Files with the same content as another file in this import: only the first is imported."));
        }
        if trashed > 0 {
            ui.label(egui::RichText::new(crate::i18n::tr_format!("· {trashed} in Recently Deleted", trashed = trashed)).color(t.text_dim));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if button(ui, "importNone", crate::i18n::tr("Uncheck All"), false).clicked() {
                d.checked.iter_mut().for_each(|c| *c = false);
            }
            if button(ui, "importAll", crate::i18n::tr("Check All"), false).clicked() {
                for i in 0..n {
                    d.checked[i] = d.checked_by_default(i);
                }
            }
        });
    });
    // what the scan left out, when it left something out
    if let Some(note) = scan_note(d) {
        let r = ui.label(egui::RichText::new(note).color(t.caution).small());
        register(ui, "label:importScanNote", r.rect);
    }
    // with more than one file type: a toggle per type checks or unchecks all of its files
    let types = d.file_types();
    if types.len() > 1 {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("File types:")).color(t.text_dim));
            for (ty, count) in &types {
                let on = d.type_checked(ty);
                let r = button(ui, &format!("importType:{ty}"), &format!("{ty} · {count}"), on).on_hover_text(crate::i18n::tr(if on {
                    "Uncheck the files of this type"
                } else {
                    "Check the files of this type"
                }));
                if r.clicked() {
                    d.check_type(ty, !on);
                }
            }
        });
    }
    thumb_size_slider(app, ui);
    // candidate grid
    let cell = thumb_size(app.ui.import_thumb_size).min(ui.available_width().max(1.0));
    let avail = ui.available_width();
    let cols = ((avail + 6.0) / (cell + 6.0)).floor().max(1.0) as usize;
    let rows = n.div_ceil(cols);
    // as tall as the window leaves after the options and buttons below it (measured last frame),
    // so resizing the dialog resizes the grid
    let below = ui.ctx().data(|m| m.get_temp::<f32>(egui::Id::unique(CONTENT_BOTTOM)));
    // the room left in the dialog's body below this point ([`note_body_room`]), else what the ui has
    let room = ui.ctx().data(|m| m.get_temp::<(f32, f32)>(egui::Id::unique((BODY_ROOM, controls::measuring(ui))))).map(|(top, room)| top + room - ui.cursor().top());
    // (the room alone: a measuring run's ui may offer less height than the real run's, and the grid must be as tall
    // in both, or the body measures shorter than it lays out and its last rows can't be scrolled to)
    // (less one row gap: the body's content counts the gap after its last row, and a body a gap too tall scrolls)
    let available = room.map_or_else(|| ui.available_height(), |room| room - ui.spacing().item_spacing.y);
    let grid_h = grid_height(available, below.unwrap_or(DEFAULT_BELOW_GRID), cell + 26.0);
    let mut visible = std::collections::HashSet::new();
    let grid =
        egui::ScrollArea::vertical().id_salt("import-grid").max_height(grid_h).auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
            let (area, _) = ui.allocate_exact_size(vec2(avail, rows as f32 * (cell + 26.0)), Sense::hover());
            for i in 0..n {
                let (c, r) = (i % cols, i / cols);
                let local = Rect::from_min_size(pos2(c as f32 * (cell + 6.0), r as f32 * (cell + 26.0)), vec2(cell, cell + 20.0));
                if !local.intersects(viewport.expand(cell)) {
                    continue;
                }
                let Ok(slot) = u32::try_from(i) else { continue };
                visible.insert(slot);
                let rect = local.translate(area.min.to_vec2());
                candidate_cell(app, ui, d, i, rect);
            }
        });
    if !controls::measuring(ui) {
        app.renderer.retain_imports(&visible);
    }
    // (in a measuring run too: see `note_dialog_bottom`)
    ui.ctx().data_mut(|m| m.insert_temp(egui::Id::unique((GRID_BOTTOM, controls::measuring(ui))), grid.inner_rect.bottom()));
    ui.add_space(4.0);
    // options
    if !d.sources.is_empty() {
        field(ui, "Source", |ui| {
            // (whether each source is a folder is checked off the UI thread, once)
            let joined = d.sources.join("\n");
            let summary = (!controls::measuring(ui))
                .then(|| {
                    crate::panels::left::fs_cached(ui, "import-source", &joined, f64::INFINITY, |s| {
                        inspect_sources(&s.split('\n').map(str::to_string).collect::<Vec<_>>())
                    })
                })
                .flatten()
                .map(|summary| summary.display())
                .unwrap_or_else(|| crate::i18n::tr_format!("{} item{}", d.sources.len(), if d.sources.len() == 1 { "" } else { "s" }));
            let r = ui.label(egui::RichText::new(summary).color(t.text_label)).on_hover_text(joined);
            register(ui, "label:importSource", r.rect);
        });
        ui.label(
            egui::RichText::new(crate::i18n::tr("Importing scans the source for photos; it doesn't add the folder to Local (use Local → Browse Folder… to work in a folder without importing)."))
                .color(t.text_dim)
                .small(),
        );
    }
    field(ui, "Transfer", |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if choice(ui, "importAdd", crate::i18n::tr("Add in place"), !d.copy)
            .on_hover_text(crate::i18n::tr("Reference the files where they are"))
            .clicked()
        {
            d.copy = false;
            d.move_files = false;
        }
        let can_copy = app.session.library.as_ref().is_some_and(|l| l.on_disk) || app.services.pick_folder.is_some();
        let r = ui.add_enabled_ui(can_copy, |ui| choice(ui, "importCopy", crate::i18n::tr("Copy"), d.copy && !d.move_files)).inner;
        if r.on_hover_text(crate::i18n::tr("Copy the files (into the library's Originals/, or a folder you choose)")).clicked() {
            d.copy = true;
            d.move_files = false;
        }
        let r = ui.add_enabled_ui(can_copy, |ui| choice(ui, "importMove", crate::i18n::tr("Move"), d.copy && d.move_files)).inner;
        if r.on_hover_text(crate::i18n::tr("Move the files (into the library's Originals/, or a folder you choose), removing them from the source"))
            .clicked()
        {
            d.copy = true;
            d.move_files = true;
        }
    });
    let r = ui.label(
        egui::RichText::new(crate::i18n::tr(match (d.copy, d.move_files) {
            (true, false) => {
                "Copy: the files are copied to the folder below and the library uses the copies; the originals are left as they are."
            }
            (true, true) => {
                "Move: the files are moved to the folder below; each original (and its XMP sidecar) is removed from the source only after its copy is verified. Duplicates and files that fail stay where they are."
            }
            _ => "Add in place: the library references the files where they are; nothing is copied or moved.",
        }))
        .color(if d.copy && d.move_files { t.caution } else { t.text_dim })
        .small(),
    );
    register(ui, "label:importModeHelp", r.rect);
    if d.copy {
        field(ui, if d.move_files { "Move to" } else { "Copy to" }, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let shown = if d.destination.trim().is_empty() { crate::i18n::tr("Library Originals").to_string() } else { d.destination.clone() };
            ui.label(egui::RichText::new(shown).color(Tokens::get(ui.ctx()).text_label));
            if app.services.pick_folder.is_some()
                && button(ui, "importDest", crate::i18n::tr("Choose…"), false).clicked()
                && let Some(f) = app.services.pick_folder.as_mut().and_then(|f| f())
            {
                d.destination = f;
            }
            if !d.destination.is_empty() && button(ui, "importDestReset", crate::i18n::tr("Library"), false).clicked() {
                d.destination.clear();
            }
        });
        field(ui, "Folders", |ui| {
            let opts = [
                ("date", "By day (YYYY/YYYY-MM-DD)"),
                ("month", "By month (YYYY/YYYY-MM)"),
                ("flat", "Into one folder"),
                ("custom", "Custom template…"),
            ];
            let key = if d.organize.is_empty() { "date".to_string() } else { d.organize.clone() };
            if let Some(key) = controls::select(ui, "importOrganize", "Organize into", &key, &opts, true) {
                d.organize = key;
                if d.organize == "custom" && d.folder_template.trim().is_empty() {
                    d.folder_template = DEFAULT_FOLDER_TEMPLATE.into();
                }
            }
        });
        if d.organize == "custom" {
            let folders_id = egui::Id::unique("import-folder-template");
            let tags_open = field(ui, "Template", |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let w = tag_field_width(ui);
                native_template(ui, "field:importFolderTemplate", &mut d.folder_template, folders_id, DEFAULT_FOLDER_TEMPLATE, w);
                native_tags(ui, "importFolders")
            });
            if tags_open {
                native_tag_help(ui, "importFolders", &mut d.folder_template, folders_id);
            }
            let t = Tokens::get(ui.ctx());
            match lightcraft_engine::rename::folder_template_error(&d.folder_template) {
                Some(e) => {
                    ui.label(egui::RichText::new(e).color(t.caution));
                }
                None => unknown_tags_warning(ui, &d.folder_template),
            }
            ui.label(
                egui::RichText::new(crate::i18n::tr(
                    "Each / starts a folder level; tags are filled in per photo (a level with missing metadata is \"unknown\").",
                ))
                .color(t.text_dim)
                .small(),
            );
        }
        if !d.move_files {
            field(ui, "Raw files", |ui| {
                controls::check(ui, "check:importDng", "Copy as DNG", &mut d.dng);
            });
        }
        let rename_id = egui::Id::unique("import-rename");
        let tags_open = field(ui, "Rename", |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let w = tag_field_width(ui);
            native_template(ui, "field:importRename", &mut d.rename, rename_id, crate::i18n::tr("keep names — or e.g. {date}_{seq:3}"), w);
            native_tags(ui, "importRename")
        });
        if tags_open {
            native_tag_help(ui, "importRename", &mut d.rename, rename_id);
        }
        unknown_tags_warning(ui, &d.rename);
        if let Some(example) = example_destination(app, d) {
            let t = Tokens::get(ui.ctx());
            let r = ui.label(egui::RichText::new(crate::i18n::tr_format!("Example: {example}", example = example)).color(t.text_dim));
            register(ui, "label:importExample", r.rect);
            ui.label(
                egui::RichText::new(crate::i18n::tr("Folders and {date} use the capture time; a photo without one uses today's date."))
                    .color(t.text_dim)
                    .small(),
            );
        }
    }
    if d.trashed.iter().any(|t| *t) {
        field(ui, "In Recently Deleted", |ui| {
            if let Some(value) = controls::select(
                ui,
                "importOnDeleted",
                "Recently deleted files",
                &d.on_deleted,
                &[("restore", "Restore them"), ("fresh", "Import as new"), ("", "Leave them")],
                true,
            ) {
                d.set_on_deleted(&value);
            }
            let t = Tokens::get(ui.ctx());
            ui.label(
                egui::RichText::new(crate::i18n::tr("Restore keeps their edits; Import as new deletes the old record first."))
                    .color(t.text_dim)
                    .small(),
            );
        });
    }
    let mut albums = crate::album_picker::entries_from(&app.session.catalog, |_| None);
    albums.retain(|a| a.folder || !a.smart);
    // Catalog changes while the review is open must not leave a hidden, unselectable destination.
    d.album = d.album.filter(|id| albums.iter().any(|a| a.id == *id && !a.folder));
    field(ui, "Album", |ui| {
        ui.vertical(|ui| {
            if controls::measuring(ui) {
                // The retained native picker must not register its real widget or mutate focus.
                ui.allocate_exact_size(vec2(ui.available_width(), ui.spacing().interact_size.y), Sense::hover());
                if d.album.is_none() && !d.new_album.is_empty() {
                    controls::text(ui, "field:importAlbumName", "New album", &mut d.new_album, "", f32::INFINITY);
                }
                return;
            }
            let mut chosen = d.album;
            let mut extra = None;
            let empty = crate::i18n::tr(if d.new_album.is_empty() { "None" } else { "New album" });
            let response = crate::album_picker::AlbumPicker::new("import-album", &mut chosen, &albums)
                .width(ui.available_width())
                .empty_text(empty)
                .show_with_choices(ui, |ui| {
                    // The picker's own choices before its albums: framework rows, the current one selected.
                    for (name, label, new, current) in [
                        ("importAlbum:none", "None", false, d.album.is_none() && d.new_album.is_empty()),
                        ("importAlbum:new", "New album…", true, d.album.is_none() && !d.new_album.is_empty()),
                    ] {
                        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ui.spacing().interact_size.y.max(24.0)), Sense::hover());
                        let row = craft::ui::buttons::Button::new(crate::i18n::tr(label).to_owned())
                            .id_salt(("import-album-choice", name))
                            .variant(craft::theme::Variant::GHOST)
                            .align(craft::ui::primitives::HAlign::Start)
                            .selected(current)
                            .show_at(ui, rect);
                        crate::widgets::register_response(name, rect, &row.response);
                        if row.clicked() {
                            extra = Some(new);
                        }
                    }
                    craft::ui::display::Divider::horizontal().show(ui);
                    extra.is_some()
                });
            if response.changed() {
                d.album = chosen;
                d.new_album.clear();
            }
            if let Some(new) = extra {
                d.album = None;
                if !new {
                    d.new_album.clear();
                } else if d.new_album.is_empty() {
                    d.new_album = crate::i18n::tr("Imported Photos").into();
                }
            }
            if d.album.is_none() && !d.new_album.is_empty() {
                controls::text(ui, "field:importAlbumName", "New album", &mut d.new_album, "", f32::INFINITY);
            }
        });
    });
    field(ui, "Preset", |ui| {
        // "" is no preset; a preset that has gone shows as None until another is chosen
        let entries = preset_entries(app, ui);
        if let Some(pid) = crate::widgets::select(ui, "combo:importPreset", crate::i18n::tr("Preset"), d.preset.clone(), entries.as_slice(), Some(crate::i18n::tr("None").to_owned()), None) {
            d.preset = pid;
        }
    });
    if !app.session.metadata_presets.is_empty() {
        field(ui, "Metadata", |ui| {
            let mut options = vec![("", crate::i18n::tr("None"))];
            options.extend(app.session.metadata_presets.iter().map(|m| (m.name.as_str(), m.name.as_str())));
            if let Some(value) = controls::select(ui, "importMetadata", "Metadata preset", &d.metadata_preset, &options, false) {
                d.metadata_preset = value;
            }
        });
    }
    field(ui, "Keywords", |ui| {
        controls::text(ui, "field:importKeywords", "Keywords", &mut d.keywords, "comma, separated", f32::INFINITY);
    });
    recipe_section(app, ui, d);
}

/// The remaining caret-aware native editors are never mounted by speculative layout.
fn native_template(ui: &mut egui::Ui, key: &str, value: &mut String, id: egui::Id, hint: &str, width: f32) {
    if controls::measuring(ui) {
        ui.allocate_exact_size(vec2(width, ui.spacing().interact_size.y), Sense::hover());
    } else {
        super::tags::template_field(ui, key, crate::i18n::tr("Template"), value, id, hint, width);
    }
}

fn native_tags(ui: &mut egui::Ui, key: &str) -> bool {
    if controls::measuring(ui) {
        ui.allocate_exact_size(vec2(crate::widgets::text_button_size(ui, "Tags"), 24.0), Sense::hover());
        ui.ctx().data(|data| data.get_temp::<bool>(egui::Id::unique((key, "tags-open")))).unwrap_or(false)
    } else {
        tag_toggle(ui, key)
    }
}

fn native_tag_help(ui: &mut egui::Ui, key: &str, text: &mut String, id: egui::Id) {
    let height_key = egui::Id::unique((key, "native-help-height"));
    if controls::measuring(ui) {
        // Reuse the last visible height. A first speculative pass reserves the bounded scroll
        // section plus its explanatory line and frame; it never mounts native tag buttons.
        let height = ui.ctx().data(|data| data.get_temp::<f32>(height_key)).unwrap_or(212.0);
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    } else {
        let shown = ui.scope(|ui| tag_help(ui, key, text, id));
        ui.ctx().data_mut(|data| data.insert_temp(height_key, shown.response.rect.height()));
    }
}

/// The develop controls offered in the import review's optional batch-edit recipe: the everyday
/// adjustments, not a second editor. Each must be one a recipe may name (`library.import` refuses
/// the others: no white balance, geometry or crop); a test holds the list to that. Anything else is
/// reached by choosing or saving a preset.
pub(super) const RECIPE_CONTROLS: &[&str] = &[
    "light.exposure",
    "light.contrast",
    "light.highlights",
    "light.shadows",
    "light.whites",
    "light.blacks",
    "color.vibrance",
    "color.saturation",
    "effects.clarity",
    "effects.texture",
    "effects.dehaze",
    "detail.sharpenAmount",
    "grain.amount",
    "vignette.amount",
];

/// A recipe as the `recipe` param of `library.import` and `preset.saveRecipe`.
pub(super) fn recipe_items(recipe: &std::collections::BTreeMap<String, f64>) -> Value {
    Value::Array(recipe.iter().map(|(id, v)| json!({"id": id, "value": v})).collect())
}

/// Record a slider's new value in the recipe: a control back at its default is no longer named.
pub(super) fn note_recipe_value(recipe: &mut std::collections::BTreeMap<String, f64>, spec: &lightcraft_develop::controls::ControlSpec, value: f64) {
    if (value - spec.default).abs() < 1e-9 {
        recipe.remove(spec.id);
    } else {
        recipe.insert(spec.id.to_string(), value);
    }
}

/// The optional "Batch edits" section of the import review: a small recipe of develop controls set
/// on every photo this import adds, on top of its XMP sidecar, camera default and the chosen preset.
/// The recipe lives only in the dialog until the import runs, so cancelling changes neither the
/// catalog nor the preset collections; "Save as Preset" is the explicit way to keep one for reuse.
fn recipe_section(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &mut ImportDialog) {
    let t = Tokens::get(ui.ctx());
    let open_id = egui::Id::unique("import-recipe-open");
    let mut open = ui.ctx().data(|m| m.get_temp::<bool>(open_id)).unwrap_or(false);
    field(ui, "Batch edits", |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if choice(ui, "importRecipeToggle", if open { "Hide adjustments" } else { "Adjust…" }, open).clicked() {
            open = !open;
        }
        let n = d.recipe.len();
        let summary = if n == 0 { crate::i18n::tr("None").to_string() } else { crate::i18n::tr_format!("Adjustments: {n}", n = n) };
        ui.label(egui::RichText::new(summary).color(if n == 0 { t.text_dim } else { t.text_label }));
    });
    if !controls::measuring(ui) {
        ui.ctx().data_mut(|m| m.insert_temp(open_id, open));
    }
    if !open {
        return;
    }
    ui.label(
        egui::RichText::new(crate::i18n::tr(
            "Set on every photo this import adds, after its XMP sidecar and the preset above; photos the library already has are left alone. A slider left at its default changes nothing. Nothing is saved unless you save it as a preset.",
        ))
        .color(t.text_dim)
        .small(),
    );
    for &id in RECIPE_CONTROLS {
        let Some(spec) = lightcraft_develop::controls::find(id) else { continue };
        let v = d.recipe.get(id).copied().unwrap_or(spec.default);
        if let Some(nv) = controls::number(ui, spec, v) {
            note_recipe_value(&mut d.recipe, spec, nv);
        }
    }
    field(ui, "Recipe", |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        controls::text(ui, "field:importRecipeName", "Recipe name", &mut d.recipe_name, "name for a saved preset", 160.0);
        let can = !d.recipe.is_empty() && !d.recipe_name.trim().is_empty();
        let b = ui.add_enabled_ui(can, |ui| button(ui, "importRecipeSave", "Save as Preset", false)).inner;
        if b.clicked() {
            let params = json!({"name": d.recipe_name.trim(), "recipe": recipe_items(&d.recipe)});
            match app.run("preset.saveRecipe", params) {
                Ok(_) => app.toast(ui.ctx(), crate::i18n::tr("Saved as a preset")),
                Err(e) => app.toast_error(ui.ctx(), e),
            }
        }
    });
}

/// Draw one import-dialog candidate cell: register `import:{i}`, request the cell's thumbnail,
/// paint the selection checkbox, and label duplicates, unreadable or trashed files. A click on an
/// importable cell toggles it (Shift extends the range); `rect` is the full cell and the thumbnail
/// uses its width as height.
fn candidate_cell(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &mut ImportDialog, i: usize, rect: Rect) {
    let t = Tokens::get(ui.ctx());
    let Some(c) = d.candidates.get(i).cloned() else { return };
    let Ok(slot_index) = u32::try_from(i) else { return };
    let ok = d.importable(i);
    let img = Rect::from_min_size(rect.min, vec2(rect.width(), rect.width()));
    let tip = format!(
        "{}\n{} × {} · {} · {:.1} MB{}",
        c.path,
        c.width,
        c.height,
        c.format,
        c.file_size as f64 / 1e6,
        c.captured.as_deref().map(|d| format!("\n{}", d.replace('T', " "))).unwrap_or_default()
    );
    let on = d.checked.get(i).copied().unwrap_or(false) && ok;
    // The cell is host-painted; the framework senses it (click, tooltip, its accessible node), no Tab stop as before;
    // none in speculative layout.
    let clicked = (!controls::measuring(ui)).then(|| {
        let out = crate::panels::detail::host::target(
            ui,
            ("import-cell", i),
            img,
            crate::panels::detail::host::Target::new(c.name.clone()).tooltip(tip).selected(on).pointer_only(),
        );
        crate::widgets::register_response(format!("import:{i}"), img, &out.response);
        out.clicked()
    });
    let p = ui.painter();
    p.rect_filled(img, 3.0, t.skeleton);
    // thumbnail (background job; the grid only asks for the cells in view)
    let slot = Slot::Import(slot_index);
    let edge = thumb_edge(img.width(), ui.ctx().pixels_per_point());
    if !controls::measuring(ui) {
        app.renderer.import_preview(&mut app.session, &c, slot_index, edge);
    }
    if let Some(tex) = app.renderer.textures.get(&slot) {
        let [tw, th] = tex.size;
        let s = ((img.width() - 8.0) / tw as f32).min((img.height() - 8.0) / th as f32);
        let fit = Rect::from_center_size(img.center(), vec2(tw as f32 * s, th as f32 * s));
        let tint = if ok { Color32::WHITE } else { Color32::from_gray(110) };
        p.image(tex.id(), fit, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
    }
    if on {
        p.rect_stroke(img, 3.0, Stroke::new(2.0, t.accent), StrokeKind::Inside);
    }
    // checkbox
    let cb = Rect::from_min_size(img.min + vec2(6.0, 6.0), vec2(16.0, 16.0));
    p.rect(cb, 3.0, if on { t.accent } else { Color32::from_black_alpha(150) }, Stroke::new(1.0, Color32::from_gray(200)), StrokeKind::Inside);
    if on {
        p.line_segment([cb.left_center() + vec2(3.5, 0.5), cb.center_bottom() + vec2(-1.0, -4.0)], Stroke::new(2.0, Color32::WHITE));
        p.line_segment([cb.center_bottom() + vec2(-1.0, -4.0), cb.right_top() + vec2(-3.5, 4.0)], Stroke::new(2.0, Color32::WHITE));
    }
    let in_trash = c.existing.is_some_and(|id| app.session.catalog.photo(lightcraft_catalog::PhotoId(id)).is_some_and(|p| p.deleted));
    let badge = match (&c.duplicate, &c.error) {
        // checked with "Leave them", the file goes without `onDeleted`: the engine restores it
        (Some(_), _) if in_trash && on => Some(if d.on_deleted == "fresh" { "Will be replaced" } else { "Will be restored" }),
        (Some(_), _) if in_trash => Some("In Recently Deleted"),
        (Some(r), _) if r == "path" => Some("In library"),
        (Some(_), _) => Some("Duplicate"),
        (None, Some(_)) => Some("Unreadable"),
        _ => None,
    };
    if let Some(b) = badge {
        let mut label = egui::text::LayoutJob::simple_singleline(crate::i18n::tr(b).to_string(), t.semibold(10.0), Color32::WHITE);
        label.wrap.max_width = (img.width() - 16.0).max(1.0);
        label.wrap.max_rows = 1;
        let g = p.layout_job(label);
        let br = badge_rect(img, cb, g.size());
        p.rect_filled(br, 3.0, Color32::from_rgba_unmultiplied(170, 60, 50, 220));
        p.galley(br.min + vec2(4.0, 2.0), g, Color32::WHITE);
    }
    let color = if ok { t.text_label } else { t.text_dim };
    let mut name = egui::text::LayoutJob::simple_singleline(c.name.clone(), t.font(10.5), color);
    name.wrap.max_width = (rect.width() - 4.0).max(1.0);
    name.wrap.max_rows = 1;
    let name = p.layout_job(name);
    p.galley(pos2(rect.left() + 2.0, img.bottom() + 2.0), name, color);
    if clicked == Some(true) && ok {
        let shift = ui.input(|input| input.modifiers.shift);
        d.click(i, shift);
    }
}

/// Where a cell's badge goes: top right, or under the checkbox when the text is too wide to sit
/// beside it — drawn over the checkbox it hid it (issue #1285).
pub(super) fn badge_rect(img: Rect, cb: Rect, text: Vec2) -> Rect {
    let size = text + vec2(8.0, 4.0);
    let left = img.right() - size.x - 4.0;
    let top = if left < cb.right() + 4.0 { cb.bottom() + 4.0 } else { img.top() + 6.0 };
    Rect::from_min_size(pos2(left, top), size)
}

/// The folder template the Custom choice starts with (`2026/20260114/`).
pub const DEFAULT_FOLDER_TEMPLATE: &str = "{date:%Y}/{date:%Y%m%d}";

/// Where the first selected photo would be copied to (destination, folders, name), for the
/// dialog's example line; `None` without a photo or with an unusable folder template.
pub fn example_destination(app: &LightcraftApp, d: &ImportDialog) -> Option<String> {
    let c = d.candidates.iter().zip(&d.checked).enumerate().find(|(i, (_, on))| **on && d.importable(*i)).map(|(_, (c, _))| c)?;
    let organize = match d.organize_param().ok()? {
        Some(o) => lightcraft_engine::import::Organize::parse(&o)?,
        None => Default::default(),
    };
    let mut q = lightcraft_catalog::Photo::new(
        lightcraft_catalog::PhotoId(0),
        lightcraft_catalog::Source::File { path: c.path.clone() },
        &c.name,
        &c.format,
        0,
        0,
        // a photo without a capture time is dated by the import (now)
        &(app.session.clock)(),
    );
    q.captured = c.captured.clone();
    // the probe's metadata, so {camera}, {title}… preview as they will import
    if let Some(info) = app.session.import_probes.get(&c.path) {
        q.meta = info.meta.clone();
    }
    let name = if d.rename.trim().is_empty() { c.name.clone() } else { lightcraft_engine::rename::expand(d.rename.trim(), &q, 1) };
    let root = if d.destination.trim().is_empty() {
        app.session.library.as_ref().map_or_else(|| "Originals".to_string(), |l| l.dir.join("Originals").to_string_lossy().to_string())
    } else {
        d.destination.trim().trim_end_matches(['/', '\\']).to_string()
    };
    let sep = std::path::MAIN_SEPARATOR_STR;
    let mut parts = vec![root];
    parts.extend(organize.folders(&q));
    parts.push(name);
    Some(parts.join(sep))
}

/// The import source in a few words: a folder's name, a file's name, or "N files and folders".
pub fn source_summary(sources: &[String]) -> String {
    inspect_sources(sources).display()
}

/// Facts cached by the filesystem worker, formatted in the UI thread's active language.
/// Caching a translated String on the worker would always use its default language (English)
/// and also retain that language after the user changes the UI language.
#[derive(Clone)]
enum SourceSummary {
    Folder(String),
    File(String),
    Many { files: usize, folders: usize },
}

impl SourceSummary {
    fn display(&self) -> String {
        match self {
            Self::Folder(name) => crate::i18n::tr_format!("Folder “{}” (and its subfolders)", name),
            Self::File(name) => name.clone(),
            Self::Many { files, folders } => match (files, folders) {
                (_, 0) => crate::i18n::tr_format!("{} files", files),
                (0, f) => crate::i18n::tr_format!("{f} folders (and their subfolders)", f = f),
                (_, f) => crate::i18n::tr_format!("{} files and {f} folder{}", files, if *f == 1 { "" } else { "s" }, f = f),
            },
        }
    }
}

fn inspect_sources(sources: &[String]) -> SourceSummary {
    let name = |p: &str| std::path::Path::new(p).file_name().map_or_else(|| p.to_string(), |n| n.to_string_lossy().to_string());
    match sources {
        [one] if std::path::Path::new(one).is_dir() => SourceSummary::Folder(name(one)),
        [one] => SourceSummary::File(name(one)),
        many => {
            let folders = many.iter().filter(|p| std::path::Path::new(p.as_str()).is_dir()).count();
            SourceSummary::Many { files: many.len().saturating_sub(folders), folders }
        }
    }
}

/// A labelled row (fixed label column).
fn field<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(vec2(78.0, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_width(78.0);
            ui.label(egui::RichText::new(crate::i18n::tr(label)).color(t.text_label));
        });
        add(ui)
    })
    .inner
}

#[cfg(test)]
#[path = "recipe_controls_flow_tests.rs"]
mod recipe_controls_flow_tests;
