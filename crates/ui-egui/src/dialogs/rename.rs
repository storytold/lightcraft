use super::LightcraftApp;
use super::frame::{field, just_opened};
use crate::state::Dialog;
use crate::theme::Tokens;
use craft::ui::buttons::Button;
use craft::ui::choice::Checkbox;
use craft::ui::contract::{Bind, Props as _, Show as _};
use craft::ui::runtime::focus::{self, FocusTarget};
use craft::ui::text::{NumberField, TextField};

fn measuring(ui: &egui::Ui) -> bool {
    ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx())
}

fn register(ui: &egui::Ui, id: &str, response: &egui::Response) {
    if !measuring(ui) {
        crate::widgets::register_response(id, response.rect, response);
    }
}

fn button(ui: &mut egui::Ui, id: &str, label: &str, selected: bool) -> bool {
    let shown = Button::new(crate::i18n::tr(label).to_owned()).id_salt(id).selected(selected).show(ui);
    register(ui, &format!("button:{id}"), &shown.response);
    !measuring(ui) && shown.clicked()
}

fn tags_toggle(ui: &mut egui::Ui) -> bool {
    let key = egui::Id::unique(("renameTemplate", "tags-open"));
    let mut open = ui.ctx().data(|d| d.get_temp::<bool>(key)).unwrap_or(false);
    let shown = Button::new(crate::i18n::tr("Tags").to_owned()).id_salt("renameTemplateTags").selected(open).show(ui);
    let clicked = !measuring(ui) && shown.clicked();
    let response = shown.response.on_hover_text(crate::i18n::tr("Show the template tags; click one to insert it"));
    register(ui, "button:renameTemplateTags", &response);
    if clicked {
        open = !open;
        ui.ctx().data_mut(|d| d.insert_temp(key, open));
    }
    open
}

fn start_field(ui: &mut egui::Ui, start: &mut u32) {
    let mut value = Some(f64::from(*start));
    let (rect, _) = ui.allocate_exact_size(egui::vec2(96.0_f32.min(ui.available_width().max(1.0)), 24.0), egui::Sense::hover());
    let shown = NumberField::new(crate::i18n::tr("Start at"), Bind::bound(&mut value))
        .id_salt("renameStart")
        .range(0.0..=999_999.0)
        .step(1.0)
        .snap_typed(true)
        .commit_decimals(0)
        .fixed_decimals(0)
        .show_at(ui, rect);
    register(ui, "field:renameStart", &shown.response);
    if !measuring(ui)
        && let Some(value) = value.filter(|v| v.is_finite())
    {
        *start = value.round().clamp(0.0, 999_999.0) as u32;
    }
}

/// Same single-field dialog contract as frame::dialog_field, with an SDK-owned editor.
fn name_field(ui: &mut egui::Ui, id: &str, value: &mut String, hint: &str) -> bool {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(1.0), 24.0), egui::Sense::hover());
    let shown = TextField::new(crate::i18n::tr(hint).to_owned(), Bind::bound(value))
        .id_salt(id)
        .placeholder(crate::i18n::tr(hint).to_owned())
        .limit(None)
        .select_on_focus()
        .submits()
        .show_at(ui, rect);
    register(ui, id, &shown.response);
    if !measuring(ui)
        && ui.is_enabled()
        && (just_opened(ui, id)
            || (!shown.response.has_focus()
                && !shown.submitted
                && !shown.escaped
                && shown.left_by_tab.is_none()
                && ui.memory(|m| m.focused().is_none())))
    {
        focus::request(ui.ctx(), FocusTarget::Id(shown.response.id));
    }
    !measuring(ui) && shown.submitted
}

/// Rows the Rename dialog previews.
pub(super) const RENAME_PREVIEW_ROWS: usize = 6;

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
    let ids = app.session.selected();
    let photos = lightcraft_engine::rename::rename_photos(&app.session.catalog, &ids);
    let total = photos.len();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (template, start, app.session.catalog.revision, &ids).hash(&mut h);
    let key = h.finish();
    let id = egui::Id::unique("rename-preview-rows");
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

pub(super) struct PersonRenamePlan {
    pub(super) faces: usize,
    pub(super) needs_merge: bool,
    /// Why Rename stays disabled. `None` when the typed name can be applied.
    pub(super) block: Option<&'static str>,
}

/// What renaming `from` to `to` would do, using the same rules as `faces.renamePerson`.
pub(super) fn person_rename_plan(cat: &lightcraft_catalog::Catalog, from: &str, to: &str) -> PersonRenamePlan {
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
            for region in photo.meta.regions.iter().filter(|r| r.kind == lightcraft_meta::RegionKind::Face) {
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

/// The body of the Rename dialog.
pub(super) fn rename_body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    let Dialog::Rename { template, start } = dlg else { return };
    let n = app.session.selected().len();
    ui.label(egui::RichText::new(crate::i18n::tr_format!("{n} photo{}", if n == 1 { "" } else { "s" }, n = n)).color(t.text_dim));
    // The template is the framework's text field; a tag from the Tags list goes in at its caret.
    let template_id = egui::Id::unique("rename-template");
    let tags_open = field(ui, "Template", |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let mut open = false;
        super::frame::trailing_button_row(ui, |ui| {
            open = tags_toggle(ui);
            let width = ui.available_width();
            crate::import::template_field(ui, "field:renameTemplate", crate::i18n::tr("Template"), template, template_id, "{name}", width);
        });
        open
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
            if button(ui, &format!("renamePreset-{i}"), label, template == tpl) {
                *template = tpl.to_string();
            }
        }
    });
    field(ui, "Start at", |ui| start_field(ui, start));
    ui.label(
        egui::RichText::new(crate::i18n::tr(
            "The Tags button lists placeholders for the template, such as {date} or {seq}. Each file is renamed on disk along with its XMP sidecar; a name that is taken gets -1, -2…",
        ))
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

/// The body of the RenamePerson dialog.
pub(super) fn rename_person_body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, confirm: &mut bool, dlg: &mut Dialog) {
    let Dialog::RenamePerson { from, to, merge } = dlg else { return };
    let submitted = name_field(ui, "field:personName", to, "Name");
    let plan = person_rename_plan(&app.session.catalog, from, to);
    if !plan.needs_merge {
        *merge = false;
    }
    if submitted && plan.block.is_none() && (!plan.needs_merge || *merge) {
        *confirm = true;
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
            egui::RichText::new(crate::i18n::tr_format!(
                "“{to}” is already a person. Combining them makes one person; Undo separates them again.",
                to = to.trim()
            ))
            .color(t.caution),
        );
        let shown =
            Checkbox::new(Bind::bound(merge)).id_salt("personMerge").label(crate::i18n::tr("Combine with the existing person").to_owned()).show(ui);
        register(ui, "check:personMerge", &shown.response);
    }
}

/// The body of the RenameKeyword dialog.
pub(super) fn rename_keyword_body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, confirm: &mut bool, dlg: &mut Dialog) {
    let Dialog::RenameKeyword { from, to } = dlg else { return };
    let n = app.caches.dialog_count(&app.session.catalog, ("renameKeyword", from.as_str()), || {
        app.session.catalog.photos().filter(|p| p.meta.keywords.iter().any(|k| lightcraft_catalog::keywords::is_under(k, from))).count()
    });
    if name_field(ui, "field:keywordName", to, "New name") {
        *confirm = true;
    }
    ui.label(
        egui::RichText::new(crate::i18n::tr_format!(
            "Renames “{from}” on {n} photo{} (keywords below it too). Use | for levels, e.g. Travel|Italy. An existing name merges the two.",
            if n == 1 { "" } else { "s" },
            from = from,
            n = n
        ))
        .color(t.text_dim),
    );
}

#[cfg(test)]
#[path = "rename_wave110_tests.rs"]
mod rename_wave110_tests;

#[cfg(test)]
#[path = "rename_preview_flow_tests.rs"]
mod rename_preview_flow_tests;
