use super::{Icon, LightcraftApp, PhotoId, Sense, Tokens, divider, header, json, padded, register, text_button, vec2};

pub(super) fn info(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let Some(p) = app.session.catalog.photo(id).cloned() else { return };
    header(ui, "Info");
    let t = Tokens::get(ui.ctx());
    padded(ui, |ui| {
        camera_card(ui, &p);
        ui.add_space(6.0);
        if let Some(why) = &p.preview_only {
            if crate::widgets::preview_only_notice(ui, "info", why) {
                crate::panels::edit::reload_preview_only(app, ui.ctx(), id);
            }
            ui.add_space(6.0);
        }
        if let Some(name) = &p.copy_name {
            let of = p
                .copy_of
                .and_then(|m| app.session.catalog.photo(m))
                .map(|m| m.file_name.clone())
                .unwrap_or_else(|| crate::i18n::tr("a removed photo").into());
            ui.label(egui::RichText::new(crate::i18n::tr_format!("Virtual copy “{name}” of {of}", name = name, of = of)).color(t.text_label));
        }
        ui.add_space(8.0);
        if let Some(r) = ui.horizontal(|ui| crate::rating::Rating::new("info", p.rating).show(ui)).inner {
            let mut params = json!({"rating": r});
            crate::panels::compare::target_active(app, &mut params);
            let _ = app.run("photo.rate", params);
        }
        ui.add_space(6.0);
        // colour label: one swatch per label (hover shows its name); clicking the current one clears it
        ui.horizontal(|ui| {
            for l in lightcraft_catalog::ColorLabel::ALL {
                let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                crate::widgets::announce(&resp, crate::i18n::tr(&format!("{l:?}")));
                let key = format!("{l:?}").to_lowercase();
                register(ui, format!("label:{key}"), r);
                let on = p.label == Some(l);
                ui.painter().circle_filled(r.center(), if on || resp.hovered() { 8.0 } else { 6.5 }, crate::panels::grid::label_color(l));
                if on {
                    ui.painter().circle_stroke(r.center(), 10.0, egui::Stroke::new(1.5, t.text));
                }
                let resp = resp.on_hover_text(crate::i18n::color_label(&app.session.catalog, l));
                if resp.clicked() {
                    // in Compare / Survey only the active photo, as the rating keys and the bottom pill do
                    let mut params = json!({"label": if on { "none".to_string() } else { key }});
                    crate::panels::compare::target_active(app, &mut params);
                    let _ = app.run("photo.label", params);
                }
            }
            if let Some(l) = p.label {
                ui.label(egui::RichText::new(app.session.catalog.label_name(l)).color(t.text_label));
            }
        });
    });
    divider(ui);
    // with several photos selected: Target Photo shows this photo's values, Selected Photos what
    // the selection shares (a field where they differ is empty and says Mixed)
    let selected = app.session.selected();
    let several = selected.len() > 1;
    let for_selected = several && app.ui.settings.info_selected_photos;
    let m = shown_meta(&app.session.catalog, &p, &selected, for_selected);
    let mixed = if for_selected { mixed_meta(&app.session.catalog, &selected, &m) } else { Vec::new() };
    // the photos an edit goes to: Target Photo edits the photo it shows
    let targets = if several && !for_selected { vec![id] } else { selected.clone() };
    padded(ui, |ui| {
        if several {
            metadata_for(app, ui);
        }
        for (label, key, value, lines) in [
            ("Title", "title", &m.title, 1),
            ("Caption", "caption", &m.caption, 2),
            ("Alt Text", "altText", &m.alt_text, 2),
            ("Extended Description", "extendedDescription", &m.extended_description, 3),
            ("Copyright", "copyright", &m.copyright, 1),
        ] {
            meta_field(app, ui, label, key, value, lines, mixed.contains(&key), &targets);
        }
        copyright_status(app, ui, m.copyright_status, mixed.contains(&"copyrightStatus"), &targets);
        for (label, key, value, lines) in [
            ("Rights Usage Terms", "usageTerms", &m.usage_terms, 2),
            ("Copyright Info URL", "copyrightUrl", &m.copyright_url, 1),
            ("Creator", "creator", &m.creator, 1),
        ] {
            meta_field(app, ui, label, key, value, lines, mixed.contains(&key), &targets);
        }
        // file: name (rename), path (reveal), capture time (edit)
        let small = |ui: &mut egui::Ui, text: &str| ui.label(egui::RichText::new(crate::i18n::tr(text)).size(11.5).color(t.text_dim));
        small(ui, "File Name");
        ui.allocate_ui_with_layout(vec2(ui.available_width(), 22.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::widgets::icon_button(ui, "infoRename", Icon::Pencil, vec2(20.0, 20.0), false, true, "Rename…").clicked() {
                let _ = app.run("dialog.rename", json!({}));
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&p.file_name).color(t.text)).truncate());
            });
        });
        ui.add_space(4.0);
        small(ui, "File Path");
        ui.horizontal(|ui| {
            let path = match &p.source {
                lightcraft_catalog::Source::File { path } => path.clone(),
                lightcraft_catalog::Source::Demo { .. } => crate::i18n::tr("Generated demo photo").into(),
            };
            ui.add(egui::Label::new(egui::RichText::new(path).size(12.0).color(t.text_label)).truncate());
            if crate::menus::ui_enabled(app, "app.showInFinder") {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::widgets::icon_button(ui, "infoReveal", Icon::Folder, vec2(20.0, 20.0), false, true, crate::menus::reveal_label())
                        .clicked()
                    {
                        let _ = app.run("app.showInFinder", json!({}));
                    }
                });
            }
        });
        ui.add_space(4.0);
        small(ui, "Captured");
        ui.allocate_ui_with_layout(vec2(ui.available_width(), 22.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::widgets::icon_button(ui, "editCaptureTime", Icon::Pencil, vec2(20.0, 20.0), false, true, "Edit Capture Time…").clicked() {
                let _ = app.run("dialog.captureTime", json!({}));
            }
            let cap = p.captured.as_deref().map(crate::i18n::display_time).unwrap_or_else(|| "—".into());
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(egui::RichText::new(cap).size(12.5).color(t.text)).truncate());
            });
        });
        ui.add_space(6.0);
        for (label, key, value) in [
            ("Location", "location", &m.location),
            ("City", "city", &m.city),
            ("State / Province", "state", &m.state),
            ("Country", "country", &m.country),
        ] {
            meta_field(app, ui, label, key, value, 1, mixed.contains(&key), &targets);
        }
        // GPS: typed as "lat, lon" (or degrees / minutes / seconds); empty clears it
        let gps_mixed = mixed.contains(&"gps");
        let gps = if gps_mixed { String::new() } else { m.gps.map(|(la, lo)| format!("{la:.6}, {lo:.6}")).unwrap_or_default() };
        let gid = egui::Id::unique("info-gps");
        ui.label(egui::RichText::new("GPS").size(11.5).color(t.text_dim));
        let mut edit: FieldEdit = ui.data(|d| d.get_temp(gid)).unwrap_or_else(|| FieldEdit::new(&gps, gps_mixed, &targets));
        let hint = crate::i18n::tr(if gps_mixed { MIXED } else { "latitude, longitude" });
        let r = crate::text_field::TextField::singleline("field:gps", &mut edit.text).hint(hint).label("GPS").width(f32::INFINITY).show(ui);
        edit.typed |= edit.text != edit.start;
        if r.editing {
            ui.data_mut(|d| d.insert_temp(gid, edit.clone()));
        } else {
            ui.data_mut(|d| d.remove::<FieldEdit>(gid));
        }
        // Return or leaving saves; Esc gives the edit up
        let text = edit.text.trim();
        if r.committed()
            && (text != edit.start || edit.clears())
            && let Err(e) = app.run("photo.setMeta", json!({"ids": edit.ids, "gps": text}))
        {
            app.toast(ui.ctx(), e);
        }
        // (one photo's place: not shown while the selected photos' places differ)
        if let Some((la, lo)) = m.gps.filter(|_| !gps_mixed) {
            let pretty = format!("{:.5}° {}, {:.5}° {}", la.abs(), if la >= 0.0 { "N" } else { "S" }, lo.abs(), if lo >= 0.0 { "E" } else { "W" });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(pretty).size(11.0).color(t.text_dim));
                if text_button(ui, "showOnMap", crate::i18n::tr("Show on Map"), false)
                    .on_hover_text(crate::i18n::tr("Open the place in OpenStreetMap"))
                    .clicked()
                {
                    let url = format!("https://www.openstreetmap.org/?mlat={la:.6}&mlon={lo:.6}#map=15/{la:.6}/{lo:.6}");
                    if let Err(e) = crate::links::open(app, &url) {
                        app.toast(ui.ctx(), e);
                    }
                }
            });
            if text_button(ui, "fillLocation", crate::i18n::tr("Fill Location"), false)
                .on_hover_text(format!(
                    "{}\n{}",
                    crate::i18n::tr("Fill empty City, State and Country from GPS (offline GeoNames place names)"),
                    crate::i18n::tr("The place names list towns of 15,000 people or more: in a smaller one, City gets the nearest larger town.")
                ))
                .clicked()
                && let Err(e) = app.run("metadata.reverseGeocode", json!({}))
            {
                app.toast_error(ui.ctx(), e);
            }
        }
        if let Some(a) = p.analysis {
            let mut line = crate::i18n::tr_format!("Focus {:.0}", a.sharpness);
            if a.clipped > 0.02 {
                line.push_str(&crate::i18n::tr_format!(" · {:.0}% clipped", a.clipped * 100.0));
            }
            if a.group.is_some() {
                line.push_str(if a.best { " · best of its burst" } else { " · in a burst" });
            }
            ui.add_space(4.0);
            ui.label(egui::RichText::new(line).color(t.text_dim));
        }
        // offline originals / smart previews
        // (cached answers, checked off the UI thread: unknown counts as online / no smart preview)
        if let lightcraft_catalog::Source::File { path } = &p.source {
            let avail = &app.session.media.availability;
            let online = !avail.is_offline(path);
            let smart = app.session.media.smart_dir.as_ref().is_some_and(|d| {
                let shard = lightcraft_engine::smart::sharded_file_path(d, &p);
                let legacy = d.join(lightcraft_engine::smart::file_name(&p));
                avail.exists(&shard.to_string_lossy()) == Some(true) || avail.exists(&legacy.to_string_lossy()) == Some(true)
            });
            if !online || smart {
                let text = match (online, smart) {
                    (false, true) => "Original offline · editing the smart preview",
                    (false, false) => "Original offline · no smart preview",
                    _ => "Smart preview available",
                };
                ui.add_space(6.0);
                ui.label(egui::RichText::new(crate::i18n::tr(text)).color(if online { t.text_dim } else { t.accent }));
            }
        }
        ui.add_space(10.0);
        if text_button(ui, "allMetadata", crate::i18n::tr("All Metadata…"), false)
            .on_hover_text(crate::i18n::tr("Every EXIF, GPS and XMP field in the file"))
            .clicked()
        {
            let _ = app.run("dialog.allMetadata", json!({}));
        }
    });
}

/// The camera card at the top of Info: camera, lens, size · file size and format, then the
/// capture settings.
fn camera_card(ui: &mut egui::Ui, p: &lightcraft_catalog::Photo) {
    let t = Tokens::get(ui.ctx());
    let m = &p.meta;
    egui::Frame::NONE.fill(t.canvas).corner_radius(6.0).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let dim = |s: &str| egui::RichText::new(s.to_string()).size(12.0).color(t.text_label);
        if !m.camera.is_empty() {
            ui.label(dim(&m.camera));
        }
        if !m.lens.is_empty() {
            ui.add(egui::Label::new(dim(&m.lens)).truncate());
        }
        ui.horizontal(|ui| {
            ui.label(dim(&format!("{} × {}  ·  {}", p.width, p.height, human_size(p.file_size))));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::Frame::NONE.fill(t.chrome).corner_radius(3.0).inner_margin(egui::Margin::symmetric(5, 1)).show(ui, |ui| {
                    ui.label(egui::RichText::new(p.format.to_uppercase()).size(10.5).color(t.text_label));
                });
            });
        });
        let rows = [
            ("Focal length", m.focal_mm.map(|f| format!("{f:.1} mm").replace(".0 mm", " mm"))),
            ("Shutter speed", (!m.shutter.is_empty()).then(|| crate::i18n::tr_format!("{} sec", m.shutter))),
            ("Aperture", m.aperture.map(|f| format!("f / {f:.1}").replace(".0", ""))),
            ("ISO", m.iso.map(|i| i.to_string())),
        ];
        if rows.iter().any(|r| r.1.is_some()) {
            ui.add_space(6.0);
            egui::Grid::new("info-capture").num_columns(2).spacing([24.0, 3.0]).show(ui, |ui| {
                for (k, v) in rows {
                    ui.label(dim(crate::i18n::tr(k)));
                    ui.label(dim(&v.unwrap_or_else(|| "—".into())));
                    ui.end_row();
                }
            });
        }
        // where the camera focused, and how (View ▸ Focus Point draws it): only when known; the
        // whole card's width, so "AF-C · Expanded Flexible Spot · Face tracking" fits
        if let Some(text) = focus_text(m.focus.as_ref()) {
            ui.add_space(3.0);
            let r = ui.horizontal_wrapped(|ui| {
                ui.label(dim(crate::i18n::tr("Focus")));
                ui.add_space(12.0);
                ui.label(egui::RichText::new(text).size(12.0).color(t.text));
            });
            register(ui, "info:focus", r.response.rect);
        }
    });
}

/// The camera's focus mode, AF area and tracking in the interface language ("AF-C · Wide · Animal Eye
/// Tracking"), or `None` when the photo has no focus point.
pub(crate) fn focus_text(f: Option<&lightcraft_meta::FocusPoint>) -> Option<String> {
    let f = f?;
    let words: Vec<&str> = f.words().into_iter().map(crate::i18n::tr).collect();
    Some(if words.is_empty() { crate::i18n::tr("Focus point").to_string() } else { words.join(" · ") })
}

fn human_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} B"),
    }
}

/// What a metadata field shows while the selected photos' values differ (Selected Photos).
const MIXED: &str = "Mixed";

/// An Info panel field's edit under way, kept in egui memory between frames. A field saves when
/// its text is not the text the edit started from, so a field left as it was saves nothing, even
/// when what the panel shows changed during the edit. It saves to the photos it edited when the
/// edit started (the selected photos, or Target Photo's photo alone), also when the selection
/// changed since.
#[derive(Clone)]
struct FieldEdit {
    text: String,
    start: String,
    /// The photos the field edited when the edit started (`photo.setMeta` `ids`).
    ids: Vec<u64>,
    /// The field said Mixed when the edit started.
    mixed: bool,
    /// Something was typed or deleted since.
    typed: bool,
}

impl FieldEdit {
    fn new(value: &str, mixed: bool, targets: &[PhotoId]) -> Self {
        let ids = targets.iter().map(|id| id.0).collect();
        FieldEdit { text: value.to_string(), start: value.to_string(), ids, mixed, typed: false }
    }

    /// A Mixed field that was typed in and left empty: it clears the field on every selected photo.
    fn clears(&self) -> bool {
        self.mixed && self.typed
    }
}

/// The Info panel's text fields, by their `photo.setMeta` key.
const META_TEXT_KEYS: [&str; 12] = [
    "title",
    "caption",
    "altText",
    "extendedDescription",
    "copyright",
    "usageTerms",
    "copyrightUrl",
    "creator",
    "location",
    "city",
    "state",
    "country",
];

/// The value of one of [`META_TEXT_KEYS`].
fn meta_text<'a>(m: &'a lightcraft_catalog::Meta, key: &str) -> Option<&'a str> {
    Some(match key {
        "title" => &m.title,
        "caption" => &m.caption,
        "altText" => &m.alt_text,
        "extendedDescription" => &m.extended_description,
        "copyright" => &m.copyright,
        "usageTerms" => &m.usage_terms,
        "copyrightUrl" => &m.copyright_url,
        "creator" => &m.creator,
        "location" => &m.location,
        "city" => &m.city,
        "state" => &m.state,
        "country" => &m.country,
        _ => return None,
    })
}

/// The metadata the Info panel's fields show: the active photo `p`'s, or with `for_selected`
/// (Selected Photos) the active photo's when it is one of the `selected` photos, else the first
/// selected photo's.
pub(crate) fn shown_meta(
    catalog: &lightcraft_catalog::Catalog,
    p: &lightcraft_catalog::Photo,
    selected: &[PhotoId],
    for_selected: bool,
) -> lightcraft_catalog::Meta {
    if !for_selected || selected.contains(&p.id) {
        return p.meta.clone();
    }
    selected.iter().find_map(|&id| catalog.photo(id)).map_or_else(|| p.meta.clone(), |q| q.meta.clone())
}

/// The Info panel fields (`photo.setMeta` keys, plus `copyrightStatus` and `gps`) where one of
/// the photos `ids` has a value other than `shown`'s, the metadata the panel draws.
pub(crate) fn mixed_meta(catalog: &lightcraft_catalog::Catalog, ids: &[PhotoId], shown: &lightcraft_catalog::Meta) -> Vec<&'static str> {
    let mut mixed = Vec::new();
    for m in ids.iter().filter_map(|&id| catalog.photo(id)).map(|p| &p.meta) {
        for key in META_TEXT_KEYS {
            if !mixed.contains(&key) && meta_text(m, key) != meta_text(shown, key) {
                mixed.push(key);
            }
        }
        if !mixed.contains(&"copyrightStatus") && m.copyright_status != shown.copyright_status {
            mixed.push("copyrightStatus");
        }
        if !mixed.contains(&"gps") && m.gps != shown.gps {
            mixed.push("gps");
        }
        if mixed.len() == META_TEXT_KEYS.len() + 2 {
            break;
        }
    }
    mixed
}

/// Target Photo / Selected Photos: whose metadata the fields below show while several photos
/// are selected (`view.infoSelectedPhotos`).
fn metadata_for(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(crate::i18n::tr("Show Metadata For")).size(11.5).color(t.text_dim));
    ui.horizontal_wrapped(|ui| {
        let on = app.ui.settings.info_selected_photos;
        for (id, label, selected) in [("infoFor-target", "Target Photo", false), ("infoFor-selected", "Selected Photos", true)] {
            if text_button(ui, id, label, on == selected).clicked() && on != selected {
                let _ = app.run("view.infoSelectedPhotos", json!({"on": selected}));
            }
        }
    });
    ui.add_space(6.0);
}

/// Copyright Status: Unknown / Copyrighted / Public Domain (`xmpRights:Marked`). `mixed`: the
/// selected photos' statuses differ, and any status picked is given to all of them. A status
/// picked goes to the photos `targets`.
fn copyright_status(app: &mut LightcraftApp, ui: &mut egui::Ui, current: lightcraft_catalog::CopyrightStatus, mixed: bool, targets: &[PhotoId]) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(crate::i18n::tr("Copyright Status")).size(11.5).color(t.text_dim));
    let shown = crate::i18n::tr(if mixed { MIXED } else { current.label() });
    let r = crate::widgets::combo_box("info-copyright-status").selected_text(shown).show_ui(ui, |ui| {
        for st in lightcraft_catalog::CopyrightStatus::ALL {
            let on = !mixed && st == current;
            if ui.selectable_label(on, crate::i18n::tr(st.label())).clicked() && !on {
                let ids: Vec<u64> = targets.iter().map(|id| id.0).collect();
                let _ = app.run("photo.setMeta", json!({"ids": ids, "copyrightStatus": st.id()}));
            }
        }
    });
    crate::widgets::name_combo(&r.response, crate::i18n::tr("Copyright Status"));
    register(ui, "field:copyrightStatus", r.response.rect);
    ui.add_space(6.0);
}

/// A labeled metadata text field: the typed text lives in egui memory while focused and is
/// saved (photo.setMeta `key`) when the field loses focus. `mixed`: the selected photos' values
/// differ, so the field starts empty and says so, and is saved only when it was typed in (left
/// empty, it clears the field on every selected photo). The edit is saved to the photos `targets`.
#[allow(clippy::too_many_arguments)]
fn meta_field(app: &mut LightcraftApp, ui: &mut egui::Ui, label: &str, key: &str, value: &str, lines: usize, mixed: bool, targets: &[PhotoId]) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(crate::i18n::tr(label)).size(11.5).color(t.text_dim));
    let id = egui::Id::unique(("info-field", key));
    let value = if mixed { "" } else { value };
    let mut edit: FieldEdit = ui.data(|d| d.get_temp(id)).unwrap_or_else(|| FieldEdit::new(value, mixed, targets));
    let widget = format!("field:{key}");
    let field = if lines > 1 {
        crate::text_field::TextField::multiline(&widget, &mut edit.text).rows(lines)
    } else {
        crate::text_field::TextField::singleline(&widget, &mut edit.text)
    };
    let field = if mixed { field.hint(crate::i18n::tr(MIXED)) } else { field };
    let r = field.label(crate::i18n::tr(label)).width(f32::INFINITY).show(ui);
    edit.typed |= edit.text != edit.start;
    // the typed text outlives frames while it is edited (the photo's value is drawn otherwise)
    if r.editing {
        ui.data_mut(|d| d.insert_temp(id, edit.clone()));
    } else {
        ui.data_mut(|d| d.remove::<FieldEdit>(id));
    }
    // Return (one line) or leaving saves; Esc gives the edit up
    if r.committed() && (edit.text != edit.start || edit.clears()) {
        let _ = app.run("photo.setMeta", json!({"ids": edit.ids, key: edit.text}));
    }
    ui.add_space(6.0);
}
