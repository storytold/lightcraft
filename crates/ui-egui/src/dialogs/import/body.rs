use super::tags::{tag_field_width, tag_help, tag_toggle, unknown_tags_warning};
use super::*;

/// The review dialog's first size (points). It can be resized; the photo grid takes the height.
pub(crate) const DIALOG_SIZE: [f32; 2] = [960.0, 720.0];

/// Memory keys: where the photo grid ended and where the dialog's content (its buttons) ended, last
/// frame. Their difference is the room the grid leaves below it.
const GRID_BOTTOM: &str = "import-grid-bottom";

const CONTENT_BOTTOM: &str = "import-content-bottom";

/// Room below the grid before it has been measured (more than the options and buttons take).
pub(super) const DEFAULT_BELOW_GRID: f32 = 360.0;

/// Height of the photo grid: what is `available` once `below` (options and buttons) is left free,
/// in whole points so the window never grows by a fraction each frame, and at least one `row`.
pub(crate) fn grid_height(available: f32, below: f32, row: f32) -> f32 {
    let h = (available - below.max(0.0)).floor();
    if h.is_finite() { h.max(row) } else { (DIALOG_SIZE[1] - DEFAULT_BELOW_GRID).max(row) }
}

/// Remember where the review dialog's content ends this frame (after its buttons), for
/// [`grid_height`] on the next.
pub(crate) fn note_dialog_bottom(ui: &egui::Ui) {
    if !ui.is_sizing_pass() {
        let y = ui.min_rect().bottom();
        ui.ctx().data_mut(|m| m.insert_temp(egui::Id::unique(CONTENT_BOTTOM), y));
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

/// The dialog body: options, then the candidate grid.
pub fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &mut ImportDialog) {
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
            if crate::widgets::text_button(ui, "importNone", crate::i18n::tr("Uncheck All"), false).clicked() {
                d.checked.iter_mut().for_each(|c| *c = false);
            }
            if crate::widgets::text_button(ui, "importAll", crate::i18n::tr("Check All"), false).clicked() {
                for i in 0..n {
                    d.checked[i] = d.checked_by_default(i);
                }
            }
        });
    });
    // with more than one file type: a toggle per type checks or unchecks all of its files
    let types = d.file_types();
    if types.len() > 1 {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("File types:")).color(t.text_dim));
            for (ty, count) in &types {
                let on = d.type_checked(ty);
                let r = crate::widgets::text_button(ui, &format!("importType:{ty}"), &format!("{ty} · {count}"), on)
                    .on_hover_text(crate::i18n::tr(if on { "Uncheck the files of this type" } else { "Check the files of this type" }));
                if r.clicked() {
                    d.check_type(ty, !on);
                }
            }
        });
    }
    // candidate grid
    let cell = 116.0;
    let avail = ui.available_width();
    let cols = ((avail + 6.0) / (cell + 6.0)).floor().max(1.0) as usize;
    let rows = n.div_ceil(cols);
    // as tall as the window leaves after the options and buttons below it (measured last frame),
    // so resizing the dialog resizes the grid
    let below = ui.ctx().data(|m| {
        let y = |key: &str| m.get_temp::<f32>(egui::Id::unique(key));
        y(CONTENT_BOTTOM).zip(y(GRID_BOTTOM)).map(|(content, grid)| content - grid)
    });
    let grid_h = grid_height(ui.available_height(), below.unwrap_or(DEFAULT_BELOW_GRID), cell + 26.0);
    let grid =
        egui::ScrollArea::vertical().id_salt("import-grid").max_height(grid_h).auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
            let (area, _) = ui.allocate_exact_size(vec2(avail, rows as f32 * (cell + 26.0)), Sense::hover());
            for i in 0..n {
                let (c, r) = (i % cols, i / cols);
                let local = Rect::from_min_size(pos2(c as f32 * (cell + 6.0), r as f32 * (cell + 26.0)), vec2(cell, cell + 20.0));
                if !local.intersects(viewport.expand(cell)) {
                    continue;
                }
                let rect = local.translate(area.min.to_vec2());
                candidate_cell(app, ui, d, i, rect);
            }
        });
    if !ui.is_sizing_pass() {
        ui.ctx().data_mut(|m| m.insert_temp(egui::Id::unique(GRID_BOTTOM), grid.inner_rect.bottom()));
    }
    ui.add_space(4.0);
    // options
    if !d.sources.is_empty() {
        field(ui, "Source", |ui| {
            // (whether each source is a folder is checked off the UI thread, once)
            let joined = d.sources.join("\n");
            let summary = crate::panels::left::fs_cached(ui, "import-source", &joined, f64::INFINITY, |s| {
                inspect_sources(&s.split('\n').map(str::to_string).collect::<Vec<_>>())
            })
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
        if crate::widgets::text_button(ui, "importAdd", crate::i18n::tr("Add in place"), !d.copy)
            .on_hover_text(crate::i18n::tr("Reference the files where they are"))
            .clicked()
        {
            d.copy = false;
            d.move_files = false;
        }
        let can_copy = app.session.library.as_ref().is_some_and(|l| l.on_disk) || app.services.pick_folder.is_some();
        let r =
            ui.add_enabled_ui(can_copy, |ui| crate::widgets::text_button(ui, "importCopy", crate::i18n::tr("Copy"), d.copy && !d.move_files)).inner;
        if r.on_hover_text(crate::i18n::tr("Copy the files (into the library's Originals/, or a folder you choose)")).clicked() {
            d.copy = true;
            d.move_files = false;
        }
        let r =
            ui.add_enabled_ui(can_copy, |ui| crate::widgets::text_button(ui, "importMove", crate::i18n::tr("Move"), d.copy && d.move_files)).inner;
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
                && crate::widgets::text_button(ui, "importDest", crate::i18n::tr("Choose…"), false).clicked()
                && let Some(f) = app.services.pick_folder.as_mut().and_then(|f| f())
            {
                d.destination = f;
            }
            if !d.destination.is_empty() && crate::widgets::text_button(ui, "importDestReset", crate::i18n::tr("Library"), false).clicked() {
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
            let cur = opts.iter().find(|o| o.0 == key).map_or(opts[0].1, |o| o.1);
            let combo = crate::widgets::combo_box("import-organize").selected_text(crate::i18n::tr(cur)).show_ui(ui, |ui| {
                for (k, label) in opts {
                    let r = ui.selectable_label(key == k, crate::i18n::tr(label));
                    register(ui, format!("button:importOrganize-{k}"), r.rect);
                    if r.clicked() {
                        d.organize = k.to_string();
                        if k == "custom" && d.folder_template.trim().is_empty() {
                            d.folder_template = DEFAULT_FOLDER_TEMPLATE.into();
                        }
                    }
                }
            });
            crate::widgets::name_combo(&combo.response, crate::i18n::tr("Organize into"));
            register(ui, "combo:importOrganize", combo.response.rect);
        });
        if d.organize == "custom" {
            let folders_id = egui::Id::unique("import-folder-template");
            let tags_open = field(ui, "Template", |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let w = tag_field_width(ui);
                let r = ui.add(egui::TextEdit::singleline(&mut d.folder_template).id(folders_id).hint_text(DEFAULT_FOLDER_TEMPLATE).desired_width(w));
                register(ui, "field:importFolderTemplate", r.rect);
                tag_toggle(ui, "importFolders")
            });
            if tags_open {
                tag_help(ui, "importFolders", &mut d.folder_template, folders_id);
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
                let r = ui.checkbox(&mut d.dng, crate::i18n::tr("Copy as DNG"));
                register(ui, "check:importDng", r.rect);
            });
        }
        let rename_id = egui::Id::unique("import-rename");
        let tags_open = field(ui, "Rename", |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let w = tag_field_width(ui);
            let r = ui.add(
                egui::TextEdit::singleline(&mut d.rename)
                    .id(rename_id)
                    .hint_text(crate::i18n::tr("keep names — or e.g. {date}_{seq:3}"))
                    .desired_width(w),
            );
            register(ui, "field:importRename", r.rect);
            tag_toggle(ui, "importRename")
        });
        if tags_open {
            tag_help(ui, "importRename", &mut d.rename, rename_id);
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
            let cur = match d.on_deleted.as_str() {
                "restore" => "Restore them",
                "fresh" => "Import as new",
                _ => "Leave them",
            };
            let mut pick = None;
            let combo = crate::widgets::combo_box("import-on-deleted").selected_text(crate::i18n::tr(cur)).show_ui(ui, |ui| {
                for (v, label) in [("restore", "Restore them"), ("fresh", "Import as new"), ("", "Leave them")] {
                    if ui.selectable_label(d.on_deleted == v, crate::i18n::tr(label)).clicked() {
                        pick = Some(v);
                    }
                }
            });
            crate::widgets::name_combo(&combo.response, crate::i18n::tr("Recently deleted files"));
            if let Some(v) = pick {
                d.set_on_deleted(v);
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
            let mut chosen = d.album;
            let mut extra = None;
            let empty = crate::i18n::tr(if d.new_album.is_empty() { "None" } else { "New album" });
            let response = crate::album_picker::AlbumPicker::new("import-album", &mut chosen, &albums)
                .width(ui.available_width())
                .empty_text(empty)
                .show_with_choices(ui, |ui| {
                    let none = ui.selectable_label(d.album.is_none() && d.new_album.is_empty(), crate::i18n::tr("None"));
                    register(ui, "importAlbum:none", none.rect);
                    if none.clicked() {
                        extra = Some(false);
                    }
                    let new = ui.selectable_label(d.album.is_none() && !d.new_album.is_empty(), crate::i18n::tr("New album…"));
                    register(ui, "importAlbum:new", new.rect);
                    if new.clicked() {
                        extra = Some(true);
                    }
                    ui.separator();
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
                let r = ui.add(egui::TextEdit::singleline(&mut d.new_album).desired_width(f32::INFINITY));
                register(ui, "field:importAlbumName", r.rect);
            }
        });
    });
    field(ui, "Preset", |ui| {
        let cur = app
            .session
            .presets
            .iter()
            .find(|p| p.id == d.preset)
            .map(|p| crate::i18n::builtin_label(&p.name, p.builtin).to_string())
            .unwrap_or_else(|| crate::i18n::tr("None").into());
        let combo = crate::widgets::combo_box("import-preset").selected_text(cur).height(300.0).show_ui(ui, |ui| {
            if ui.selectable_label(d.preset.is_empty(), crate::i18n::tr("None")).clicked() {
                d.preset.clear();
            }
            let selected = (!d.preset.is_empty()).then_some(d.preset.as_str());
            if let Some(pid) = crate::panels::presets::picker_rows(ui, "import-preset", &app.session.presets, selected) {
                d.preset = pid;
            }
        });
        crate::widgets::name_combo(&combo.response, crate::i18n::tr("Preset"));
    });
    if !app.session.metadata_presets.is_empty() {
        field(ui, "Metadata", |ui| {
            let cur = if d.metadata_preset.is_empty() { crate::i18n::tr("None").to_string() } else { d.metadata_preset.clone() };
            let combo = crate::widgets::combo_box("import-metadata").selected_text(cur).show_ui(ui, |ui| {
                if ui.selectable_label(d.metadata_preset.is_empty(), crate::i18n::tr("None")).clicked() {
                    d.metadata_preset.clear();
                }
                for m in &app.session.metadata_presets {
                    if ui.selectable_label(d.metadata_preset == m.name, &m.name).clicked() {
                        d.metadata_preset = m.name.clone();
                    }
                }
            });
            crate::widgets::name_combo(&combo.response, crate::i18n::tr("Metadata preset"));
        });
    }
    field(ui, "Keywords", |ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut d.keywords).hint_text(crate::i18n::tr("comma, separated")).desired_width(f32::INFINITY));
        register(ui, "field:importKeywords", r.rect);
    });
}

/// Draw one import-dialog candidate cell: register `import:{i}`, request the cell's thumbnail,
/// paint the selection checkbox, and label duplicates, unreadable or trashed files. A click on an
/// importable cell toggles it (Shift extends the range); `rect` is the full cell and the thumbnail
/// uses its width as height.
fn candidate_cell(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &mut ImportDialog, i: usize, rect: Rect) {
    let t = Tokens::get(ui.ctx());
    let c = d.candidates[i].clone();
    let ok = d.importable(i);
    let img = Rect::from_min_size(rect.min, vec2(rect.width(), rect.width()));
    let resp = ui.interact(img, egui::Id::unique(("import-cell", i)), Sense::click());
    crate::widgets::announce(&resp, c.name.clone());
    register(ui, format!("import:{i}"), img);
    let p = ui.painter();
    p.rect_filled(img, 3.0, t.skeleton);
    // thumbnail (background job; the grid only asks for the cells in view)
    let slot = Slot::Import(i as u32);
    if !app.renderer.textures.contains_key(&slot)
        && let Some(job) = app.session.candidate_thumb_job(&c, 192, i as u64)
    {
        app.renderer.request_quick(slot, job, 4);
    }
    if let Some(tex) = app.renderer.textures.get(&slot) {
        let [tw, th] = tex.size;
        let s = ((img.width() - 8.0) / tw as f32).min((img.height() - 8.0) / th as f32);
        let fit = Rect::from_center_size(img.center(), vec2(tw as f32 * s, th as f32 * s));
        let tint = if ok { Color32::WHITE } else { Color32::from_gray(110) };
        p.image(tex.tex.id(), fit, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
    }
    let on = d.checked[i] && ok;
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
        let g = p.layout_no_wrap(crate::i18n::tr(b).to_string(), t.semibold(10.0), Color32::WHITE);
        let br = badge_rect(img, cb, g.size());
        p.rect_filled(br, 3.0, Color32::from_rgba_unmultiplied(170, 60, 50, 220));
        p.galley(br.min + vec2(4.0, 2.0), g, Color32::WHITE);
    }
    let name = if c.name.chars().count() > 18 { format!("{}…", c.name.chars().take(17).collect::<String>()) } else { c.name.clone() };
    p.text(pos2(rect.left() + 2.0, img.bottom() + 9.0), Align2::LEFT_CENTER, name, t.font(10.5), if ok { t.text_label } else { t.text_dim });
    let tip = format!(
        "{}\n{} × {} · {} · {:.1} MB{}",
        c.path,
        c.width,
        c.height,
        c.format,
        c.file_size as f64 / 1e6,
        c.captured.as_deref().map(|d| format!("\n{}", d.replace('T', " "))).unwrap_or_default()
    );
    let resp = resp.on_hover_text(tip);
    if resp.clicked() && ok {
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
