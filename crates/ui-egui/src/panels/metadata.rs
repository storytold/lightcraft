//! The Metadata panel (Window ▸ Info): field presets (Default, EXIF, IPTC, IPTC Extension,
//! Location, Large Caption, Minimal), metadata presets (apply / save / delete), copy and paste,
//! and editing many photos at once — a field whose value differs across the selection shows
//! `<mixed>` and typing into it sets every selected photo.

use std::sync::Arc;

use dac_catalog::{Photo, PhotoId};
use egui::{Sense, vec2};
use serde_json::json;

use crate::DacApp;
use crate::icons::Icon;
use crate::theme::Tokens;
use crate::widgets::{divider, register, text_button};

/// The panel's field presets: (id, menu label).
pub const PRESETS: [(&str, &str); 7] = [
    ("default", "Default"),
    ("exif", "EXIF"),
    ("iptc", "IPTC"),
    ("iptcExtension", "IPTC Extension"),
    ("location", "Location"),
    ("largeCaption", "Large Caption"),
    ("minimal", "Minimal"),
];

pub fn preset_ids() -> Vec<&'static str> {
    PRESETS.iter().map(|(k, _)| *k).collect()
}

/// What `<mixed>` reads as.
pub const MIXED: &str = "< mixed >";

/// One row of the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// An editable text field: photo.setMeta key, label, lines.
    Text(&'static str, &'static str, usize),
    /// A read-only value: key of [`value`], label.
    Info(&'static str, &'static str),
    /// A section title inside the panel.
    Section(&'static str),
    CopyrightStatus,
    FileName,
    Folder,
    Captured,
    Gps,
    Rating,
    Label,
    /// Camera card details for one photo: virtual copy, focus analysis, offline state, All Metadata….
    Extras,
}

/// The rows of a field preset (unknown ids fall back to Default).
pub fn rows(preset: &str) -> Vec<Row> {
    use Row::*;
    let place = [Text("location", "Sublocation", 1), Text("city", "City", 1), Text("state", "State / Province", 1), Text("country", "Country", 1)];
    let exif = [
        Info("dimensions", "Dimensions"),
        Info("cropped", "Cropped"),
        Captured,
        Info("exposure", "Exposure"),
        Info("focal", "Focal Length"),
        Info("iso", "ISO Speed Rating"),
        Info("camera", "Camera"),
        Info("lens", "Lens"),
    ];
    let mut v = match preset {
        "exif" => {
            let mut v = vec![FileName, Folder, Info("format", "File Type"), Info("fileSize", "File Size")];
            v.extend(exif);
            v.push(Gps);
            v
        }
        "iptc" => vec![
            Section("Contact"),
            Text("creator", "Creator", 1),
            Section("Content"),
            Text("title", "Title", 1),
            Text("caption", "Caption", 3),
            Info("keywords", "Keywords"),
            Section("Image"),
            Captured,
            Text("location", "Sublocation", 1),
            Text("city", "City", 1),
            Text("state", "State / Province", 1),
            Text("country", "Country", 1),
            Section("Copyright"),
            Text("copyright", "Copyright", 1),
            CopyrightStatus,
            Text("usageTerms", "Rights Usage Terms", 2),
            Text("copyrightUrl", "Copyright Info URL", 1),
        ],
        "iptcExtension" => vec![
            Section("Description"),
            Text("altText", "Alt Text (Accessibility)", 2),
            Text("extendedDescription", "Extended Description (Accessibility)", 3),
            Section("Licensing"),
            Text("usageTerms", "Rights Usage Terms", 2),
            Text("copyrightUrl", "Web Statement of Rights", 1),
        ],
        "location" => {
            let mut v = vec![FileName, Text("title", "Title", 1), Text("caption", "Caption", 2), Captured];
            v.extend(place);
            v.push(Gps);
            v
        }
        "largeCaption" => vec![Text("caption", "Caption", 12), Text("copyright", "Copyright", 1)],
        "minimal" => vec![FileName, Rating, Label, Text("caption", "Caption", 2), Text("copyright", "Copyright", 1)],
        _ => {
            let mut v = vec![
                FileName,
                Folder,
                Captured,
                Rating,
                Label,
                Text("title", "Title", 1),
                Text("caption", "Caption", 2),
                Text("altText", "Alt Text", 2),
                Text("extendedDescription", "Extended Description", 3),
                Text("copyright", "Copyright", 1),
                CopyrightStatus,
                Text("usageTerms", "Rights Usage Terms", 2),
                Text("copyrightUrl", "Copyright Info URL", 1),
                Text("creator", "Creator", 1),
            ];
            v.extend(place);
            v.push(Gps);
            v.extend(exif.into_iter().filter(|r| *r != Captured));
            v.push(Extras);
            v
        }
    };
    v.dedup();
    v
}

/// A field of a photo as text (editable keys are photo.setMeta's; the others read-only).
pub fn value(p: &Photo, key: &str) -> String {
    let m = &p.meta;
    match key {
        "title" => m.title.clone(),
        "caption" => m.caption.clone(),
        "altText" => m.alt_text.clone(),
        "extendedDescription" => m.extended_description.clone(),
        "copyright" => m.copyright.clone(),
        "copyrightStatus" => m.copyright_status.id().to_string(),
        "usageTerms" => m.usage_terms.clone(),
        "copyrightUrl" => m.copyright_url.clone(),
        "creator" => m.creator.clone(),
        "location" => m.location.clone(),
        "city" => m.city.clone(),
        "state" => m.state.clone(),
        "country" => m.country.clone(),
        "gps" => m.gps.map(|(la, lo)| format!("{la:.6}, {lo:.6}")).unwrap_or_default(),
        "fileName" => p.file_name.clone(),
        "folder" => match &p.source {
            dac_catalog::Source::File { path } => std::path::Path::new(path).parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default(),
            dac_catalog::Source::Demo { .. } => crate::i18n::tr("Generated demo photo").into(),
        },
        "captured" => p.captured.clone().unwrap_or_default(),
        "rating" => p.rating.to_string(),
        "label" => p.label.map(|l| format!("{l:?}").to_lowercase()).unwrap_or_default(),
        "dimensions" => format!("{} × {}", p.width, p.height),
        "cropped" => {
            let c = p.develop.crop.geometry.rect;
            let (w, h) = ((p.width as f64 * c.width()).round() as u64, (p.height as f64 * c.height()).round() as u64);
            if p.develop.orientation.swaps_axes() { format!("{h} × {w}") } else { format!("{w} × {h}") }
        }
        "exposure" => {
            let shutter = (!m.shutter.is_empty()).then(|| format!("{} sec", m.shutter));
            let f = m.aperture.map(|a| format!("f / {a:.1}").replace(".0", ""));
            [shutter, f].into_iter().flatten().collect::<Vec<_>>().join(" at ")
        }
        "focal" => m.focal_mm.map(|f| format!("{f:.1} mm").replace(".0 mm", " mm")).unwrap_or_default(),
        "iso" => m.iso.map(|i| format!("ISO {i}")).unwrap_or_default(),
        "camera" => m.camera.clone(),
        "lens" => m.lens.clone(),
        "format" => p.format.to_uppercase(),
        "fileSize" => human_size(p.file_size),
        "keywords" => m.keywords.iter().map(|k| k.rsplit('|').next().unwrap_or(k)).collect::<Vec<_>>().join(", "),
        _ => String::new(),
    }
}

/// The shared value of a field across photos; `None` = mixed.
pub fn common(photos: &[Arc<Photo>], key: &str) -> Option<String> {
    let mut it = photos.iter().map(|p| value(p, key));
    let first = it.next().unwrap_or_default();
    it.all(|v| v == first).then_some(first)
}

fn human_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} B"),
    }
}

fn padded(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 6, bottom: 6 }).show(ui, add);
}

/// The panel. `id` is the active photo; the fields show (and edit) every selected photo.
pub fn show(app: &mut DacApp, ui: &mut egui::Ui, id: PhotoId) {
    let t = Tokens::get(ui.ctx());
    let targets: Vec<PhotoId> = app.session.targets(&json!({}));
    let photos: Vec<Arc<Photo>> = {
        let mut v: Vec<Arc<Photo>> = targets.iter().filter_map(|i| app.session.catalog.photo(*i).cloned()).collect();
        if v.is_empty()
            && let Some(p) = app.session.catalog.photo(id)
        {
            v.push(p.clone());
        }
        v
    };
    let Some(active) = photos.iter().find(|p| p.id == id).or(photos.first()).cloned() else { return };
    let ids: Vec<u64> = photos.iter().map(|p| p.id.0).collect();
    super::right::header(ui, "Metadata");
    padded(ui, |ui| {
        toolbar(app, ui, &ids);
        if photos.len() > 1 {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "{} {} · {} {MIXED}",
                    photos.len(),
                    crate::i18n::tr("selected photos"),
                    crate::i18n::tr("differing values show")
                ))
                .size(11.5)
                .color(t.text_dim),
            );
        }
    });
    divider(ui);
    let preset = app.ui.lib.metadata_preset.clone();
    padded(ui, |ui| {
        for row in rows(&preset) {
            match row {
                Row::Text(key, label, lines) => text_field(app, ui, &photos, &ids, label, key, lines),
                Row::Info(key, label) => info_row(ui, label, common(&photos, key).as_deref()),
                Row::Section(title) => {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(crate::i18n::tr(title)).font(t.semibold(12.5)).color(t.text));
                    ui.add_space(4.0);
                }
                Row::CopyrightStatus => copyright_status(app, ui, &photos),
                Row::FileName => file_name(app, ui, &photos),
                Row::Folder => folder(app, ui, &photos),
                Row::Captured => captured(app, ui, &photos),
                Row::Gps => gps(app, ui, &photos, &ids),
                Row::Rating => rating(app, ui, &photos),
                Row::Label => label(app, ui, &photos),
                Row::Extras => extras(app, ui, &active),
            }
        }
    });
}

/// Panel preset · metadata preset · copy / paste.
fn toolbar(app: &mut DacApp, ui: &mut egui::Ui, ids: &[u64]) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::tr("View")).size(11.5).color(t.text_dim));
        let current = PRESETS.iter().find(|(k, _)| *k == app.ui.lib.metadata_preset).map_or("Default", |(_, l)| *l);
        let r = egui::ComboBox::from_id_salt("metadata-panel-preset").selected_text(crate::i18n::tr(current)).show_ui(ui, |ui| {
            for (k, l) in PRESETS {
                if ui.selectable_label(k == app.ui.lib.metadata_preset, crate::i18n::tr(l)).clicked() {
                    let _ = app.run("metadata.panelPreset", json!({"preset": k}));
                }
            }
        });
        register(ui.ctx(), "metadata:panelPreset", r.response.rect);
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::tr("Preset")).size(11.5).color(t.text_dim));
        let names: Vec<String> = app.session.metadata_presets.iter().map(|m| m.name.clone()).collect();
        let r = egui::ComboBox::from_id_salt("metadata-preset").selected_text(crate::i18n::tr("Apply…")).show_ui(ui, |ui| {
            for n in &names {
                if ui.button(n).clicked()
                    && let Err(e) = app.run("metadata.applyPreset", json!({"name": n, "ids": ids}))
                {
                    app.toast(ui.ctx(), e);
                }
            }
            if !names.is_empty() {
                ui.separator();
            }
            if ui.button(crate::i18n::tr("Save Current Settings as New Preset…")).clicked() {
                app.ui.dialog = Some(crate::state::Dialog::TextPrompt {
                    title: "New Metadata Preset".into(),
                    hint: "Preset name".into(),
                    value: String::new(),
                    command: "metadata.savePreset".into(),
                    params: json!({"only": ["title", "caption", "copyright", "copyrightStatus", "usageTerms", "copyrightUrl", "creator", "location", "city", "state", "country", "keywords"]}),
                    key: "name".into(),
                });
            }
            for n in &names {
                if ui.button(format!("{} “{n}”", crate::i18n::tr("Delete"))).clicked() {
                    let _ = app.run("metadata.deletePreset", json!({"name": n}));
                }
            }
        });
        register(ui.ctx(), "metadata:preset", r.response.rect);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let can_paste = app.session.meta_clipboard.is_some();
            if text_button(ui, "metadataPaste", crate::i18n::tr("Paste"), false)
                .on_hover_text(crate::i18n::tr("Paste the copied metadata onto the selected photos"))
                .clicked()
                && can_paste
            {
                let _ = app.run("photo.pasteMetadata", json!({"ids": ids}));
            }
            if text_button(ui, "metadataCopy", crate::i18n::tr("Copy"), false)
                .on_hover_text(crate::i18n::tr("Copy the active photo's metadata"))
                .clicked()
            {
                let _ = app.run("photo.copyMetadata", json!({}));
            }
        });
    });
}

fn small(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(crate::i18n::tr(text)).size(11.5).color(t.text_dim));
}

fn info_row(ui: &mut egui::Ui, label: &str, v: Option<&str>) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(110.0, 18.0), Sense::hover());
        ui.painter().text(r.left_center(), egui::Align2::LEFT_CENTER, crate::i18n::tr(label), t.font(11.5), t.text_dim);
        let (text, color) = match v {
            None => (MIXED.to_string(), t.text_dim),
            Some("") => ("—".to_string(), t.text_dim),
            Some(v) => (v.to_string(), t.text_label),
        };
        ui.add(egui::Label::new(egui::RichText::new(text).size(12.0).color(color)).truncate());
    });
    ui.add_space(2.0);
}

/// A labelled text field: the typed text lives in egui memory while focused and is saved
/// (photo.setMeta `key`, on every photo shown) when the field loses focus. A mixed field is empty
/// with a `<mixed>` hint; leaving it empty changes nothing.
fn text_field(app: &mut DacApp, ui: &mut egui::Ui, photos: &[Arc<Photo>], ids: &[u64], label: &str, key: &str, lines: usize) {
    small(ui, label);
    let shared = common(photos, key);
    let shown = shared.clone().unwrap_or_default();
    let id = egui::Id::new(("info-field", key));
    let mut text: String = ui.data(|d| d.get_temp(id)).unwrap_or_else(|| shown.clone());
    // the shared text field (upstream's Info panel): Return (one line) or leaving saves, Esc gives the
    // edit up, right-click has the edit menu
    let widget = format!("field:{key}");
    let field = if lines > 1 {
        crate::text_field::TextField::multiline(&widget, &mut text).rows(lines)
    } else {
        crate::text_field::TextField::singleline(&widget, &mut text)
    };
    let field = if shared.is_none() { field.hint(MIXED) } else { field };
    let r = field.width(f32::INFINITY).show(ui);
    crate::access::label(&r.response, label);
    if r.editing {
        ui.data_mut(|d| d.insert_temp(id, text.clone()));
    } else {
        ui.data_mut(|d| d.remove::<String>(id));
    }
    if r.committed() && text != shown && !(shared.is_none() && text.is_empty()) {
        let _ = app.run("photo.setMeta", json!({"ids": ids, key: text}));
    }
    ui.add_space(6.0);
}

/// Copyright Status: Unknown / Copyrighted / Public Domain (`xmpRights:Marked`).
fn copyright_status(app: &mut DacApp, ui: &mut egui::Ui, photos: &[Arc<Photo>]) {
    small(ui, "Copyright Status");
    let current = photos.first().map(|p| p.meta.copyright_status).unwrap_or_default();
    let mixed = common(photos, "copyrightStatus").is_none();
    let text = if mixed { MIXED.to_string() } else { crate::i18n::tr(current.label()).to_string() };
    let r = egui::ComboBox::from_id_salt("info-copyright-status").selected_text(text).show_ui(ui, |ui| {
        for st in dac_catalog::CopyrightStatus::ALL {
            if ui.selectable_label(!mixed && st == current, crate::i18n::tr(st.label())).clicked() && (mixed || st != current) {
                let _ = app.run("photo.setMeta", json!({"copyrightStatus": st.id()}));
            }
        }
    });
    register(ui.ctx(), "field:copyrightStatus", r.response.rect);
    ui.add_space(6.0);
}

fn file_name(app: &mut DacApp, ui: &mut egui::Ui, photos: &[Arc<Photo>]) {
    let t = Tokens::get(ui.ctx());
    small(ui, "File Name");
    let name = common(photos, "fileName").unwrap_or_else(|| MIXED.into());
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 22.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if crate::widgets::icon_button(ui, "infoRename", Icon::Pencil, vec2(20.0, 20.0), false, true, "Rename…").clicked() {
            let _ = app.run("dialog.rename", json!({}));
        }
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add(egui::Label::new(egui::RichText::new(name).color(t.text)).truncate());
        });
    });
    if let [p] = photos
        && let Some(n) = &p.copy_name
    {
        ui.label(egui::RichText::new(format!("{} {n}", crate::i18n::tr("Copy Name:"))).size(11.5).color(t.text_label));
    }
    ui.add_space(4.0);
}

fn folder(app: &mut DacApp, ui: &mut egui::Ui, photos: &[Arc<Photo>]) {
    let t = Tokens::get(ui.ctx());
    small(ui, "Folder");
    let path = common(photos, "folder").unwrap_or_else(|| MIXED.into());
    ui.horizontal(|ui| {
        ui.add(egui::Label::new(egui::RichText::new(path).size(12.0).color(t.text_label)).truncate());
        if crate::menus::ui_enabled(app, "app.showInFinder") {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::widgets::icon_button(ui, "infoReveal", Icon::Folder, vec2(20.0, 20.0), false, true, crate::menus::reveal_label()).clicked()
                {
                    let _ = app.run("app.showInFinder", json!({}));
                }
            });
        }
    });
    ui.add_space(4.0);
}

fn captured(app: &mut DacApp, ui: &mut egui::Ui, photos: &[Arc<Photo>]) {
    let t = Tokens::get(ui.ctx());
    small(ui, "Capture Time");
    let cap = match common(photos, "captured") {
        None => MIXED.to_string(),
        Some(c) if c.is_empty() => "—".into(),
        Some(c) => crate::i18n::display_time(&c),
    };
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 22.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if crate::widgets::icon_button(ui, "editCaptureTime", Icon::Pencil, vec2(20.0, 20.0), false, true, "Edit Capture Time…").clicked() {
            let _ = app.run("dialog.captureTime", json!({}));
        }
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add(egui::Label::new(egui::RichText::new(cap).size(12.5).color(t.text)).truncate());
        });
    });
    ui.add_space(6.0);
}

/// GPS: typed as "lat, lon" (or degrees / minutes / seconds); empty clears it.
fn gps(app: &mut DacApp, ui: &mut egui::Ui, photos: &[Arc<Photo>], ids: &[u64]) {
    let t = Tokens::get(ui.ctx());
    let shared = common(photos, "gps");
    let gps = shared.clone().unwrap_or_default();
    let gid = egui::Id::new("info-gps");
    small(ui, "GPS");
    let mut text: String = ui.data(|d| d.get_temp(gid)).unwrap_or_else(|| gps.clone());
    let hint = if shared.is_none() { MIXED } else { crate::i18n::tr("latitude, longitude") };
    let r = ui.add(egui::TextEdit::singleline(&mut text).hint_text(hint).desired_width(f32::INFINITY));
    register(ui.ctx(), "field:gps", r.rect);
    crate::access::label(&r, "GPS");
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(gid, text.clone()));
    } else {
        ui.data_mut(|d| d.remove::<String>(gid));
    }
    if r.lost_focus()
        && text.trim() != gps
        && !(shared.is_none() && text.trim().is_empty())
        && let Err(e) = app.run("photo.setMeta", json!({"ids": ids, "gps": text.trim()}))
    {
        app.toast(ui.ctx(), e);
    }
    if let [p] = photos
        && let Some((la, lo)) = p.meta.gps
    {
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
    }
    ui.add_space(6.0);
}

fn rating(app: &mut DacApp, ui: &mut egui::Ui, photos: &[Arc<Photo>]) {
    let t = Tokens::get(ui.ctx());
    small(ui, "Rating");
    let shared = common(photos, "rating");
    let current = shared.as_deref().and_then(|r| r.parse::<u8>().ok()).unwrap_or(0);
    ui.horizontal(|ui| {
        if let Some(r) = crate::widgets::stars(ui, "info", current, 18.0) {
            let _ = app.run("photo.rate", json!({"rating": r}));
        }
        if shared.is_none() {
            ui.label(egui::RichText::new(MIXED).size(11.5).color(t.text_dim));
        }
    });
    ui.add_space(6.0);
}

/// Colour label: one swatch per label (hover shows its name); clicking the current one clears it.
fn label(app: &mut DacApp, ui: &mut egui::Ui, photos: &[Arc<Photo>]) {
    let t = Tokens::get(ui.ctx());
    small(ui, "Label");
    let shared = common(photos, "label");
    let current = shared.as_deref().and_then(dac_catalog::ColorLabel::parse);
    ui.horizontal(|ui| {
        for l in dac_catalog::ColorLabel::ALL {
            let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
            let key = format!("{l:?}").to_lowercase();
            register(ui.ctx(), format!("label:{key}"), r);
            let on = current == Some(l);
            ui.painter().circle_filled(r.center(), if on || resp.hovered() { 8.0 } else { 6.5 }, crate::panels::grid::label_color(l));
            if on {
                ui.painter().circle_stroke(r.center(), 10.0, egui::Stroke::new(1.5, t.text));
            }
            let name = crate::i18n::color_label(&app.session.catalog, l);
            crate::access::named(&resp, egui::WidgetType::RadioButton, &name);
            let resp = resp.on_hover_text(name);
            if resp.clicked() {
                let _ = app.run("photo.label", json!({"label": if on { "none".to_string() } else { key }}));
            }
        }
        match (current, shared) {
            (Some(l), _) => {
                ui.label(egui::RichText::new(app.session.catalog.label_name(l)).color(t.text_label));
            }
            (None, None) => {
                ui.label(egui::RichText::new(MIXED).size(11.5).color(t.text_dim));
            }
            _ => {}
        }
    });
    ui.add_space(6.0);
}

/// The active photo's extras: virtual copy, analysis, offline original / smart preview, All Metadata….
fn extras(app: &mut DacApp, ui: &mut egui::Ui, p: &Photo) {
    let t = Tokens::get(ui.ctx());
    if let Some(why) = &p.preview_only {
        crate::widgets::preview_only_notice(ui, "info", why);
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
    if let Some(a) = p.analysis {
        let mut line = crate::i18n::tr_format!("Focus {:.0}", a.sharpness);
        if a.clipped > 0.02 {
            line.push_str(&crate::i18n::tr_format!(" · {:.0}% clipped", a.clipped * 100.0));
        }
        if a.group.is_some() {
            line.push_str(crate::i18n::tr(if a.best { " · best of its burst" } else { " · in a burst" }));
        }
        ui.add_space(4.0);
        ui.label(egui::RichText::new(line).color(t.text_dim));
    }
    // offline originals / smart previews (cached answers, checked off the UI thread: unknown
    // counts as online / no smart preview)
    if let dac_catalog::Source::File { path } = &p.source {
        let avail = &app.session.media.availability;
        let online = !avail.is_offline(path);
        let smart = app
            .session
            .media
            .smart_dir
            .as_ref()
            .is_some_and(|d| avail.exists(&d.join(dac_engine::smart::file_name(p)).to_string_lossy()) == Some(true));
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
    #[cfg(not(target_arch = "wasm32"))]
    super::connections::metadata_rows(app, ui, p.id);
    if text_button(ui, "allMetadata", crate::i18n::tr("All Metadata…"), false)
        .on_hover_text(crate::i18n::tr("Every EXIF, GPS and XMP field in the file"))
        .clicked()
    {
        let _ = app.run("dialog.allMetadata", json!({}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dac_catalog::Source;

    fn photo(id: u64) -> Arc<Photo> {
        let mut p = Photo::new(PhotoId(id), Source::Demo { scene: 0 }, "a.jpg", "JPEG", 300, 200, "2026-01-01T00:00:00");
        p.meta.title = "Dawn".into();
        p.meta.city = format!("City {}", id % 2);
        Arc::new(p)
    }

    #[test]
    fn every_preset_has_rows_and_no_duplicates_next_to_each_other() {
        for (k, _) in PRESETS {
            let r = rows(k);
            assert!(!r.is_empty(), "{k}");
            assert!(r.windows(2).all(|w| w[0] != w[1]), "{k}");
        }
        assert_eq!(rows("nope"), rows("default"));
        assert!(rows("largeCaption").contains(&Row::Text("caption", "Caption", 12)));
        assert!(rows("location").contains(&Row::Gps));
        assert!(rows("exif").contains(&Row::Info("lens", "Lens")));
    }

    #[test]
    fn mixed_values_across_photos() {
        let ps = vec![photo(1), photo(2), photo(3)];
        assert_eq!(common(&ps, "title").as_deref(), Some("Dawn"));
        assert_eq!(common(&ps, "city"), None, "mixed");
        assert_eq!(common(&ps[..1], "city").as_deref(), Some("City 1"));
        assert_eq!(common(&[], "title").as_deref(), Some(""));
        assert_eq!(value(&ps[0], "dimensions"), "300 × 200");
        assert_eq!(value(&ps[0], "unknown"), "");
    }
}
