//! Read-only metadata: prominent catalog fields immediately, full file tags on a worker.

use super::{LightcraftApp, PhotoId, Tokens, header, padded, register};
use serde_json::Value;
use std::sync::Arc;

const TASK: &str = "Metadata panel";
const MAX_READS: usize = 4;
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const TAGS_PER_PAGE: usize = 100;
type ReadResult = Result<(Value, Option<String>), String>;

#[derive(Default)]
pub(crate) struct Runtime {
    key: Option<(u64, PhotoId, String)>,
    result: Option<ReadResult>,
    page: usize,
    started_at: Option<web_time::Instant>,
    attempt: Arc<()>,
    locale: String,
    queued: bool,
}

/// Dispatch at most one read for the selected photo and accept only its current result.
fn load(app: &mut LightcraftApp, id: PhotoId) {
    let Some(photo) = app.session.catalog.photo(id) else { return };
    let path = match &photo.source {
        lightcraft_catalog::Source::File { path } => path.clone(),
        _ => format!("demo:{}", id.0),
    };
    let key = (app.session.library_generation(), id, path);
    if app.metadata_panel.key.as_ref() != Some(&key) {
        app.metadata_panel = Runtime { key: Some(key.clone()), locale: sys_locale::get_locale().unwrap_or_default(), ..Default::default() };
    }
    if app.metadata_panel.result.is_some() {
        return;
    }
    if app.metadata_panel.started_at.is_some_and(|start| start.elapsed() >= READ_TIMEOUT) {
        // The OS may still own a blocking read. Keep counting that worker until it exits.
        app.metadata_panel.result = Some(Err(crate::i18n::tr("Metadata read timed out. Try Refresh.").into()));
        return;
    }
    if app.metadata_panel.started_at.is_some() {
        return;
    }
    app.metadata_panel.queued = app.tasks.labels().iter().filter(|name| name.starts_with(TASK)).count() >= MAX_READS;
    if app.metadata_panel.queued {
        return;
    }
    let request = match lightcraft_engine::metadata_read::Request::new(&app.session, id) {
        Ok(request) => request,
        Err(error) => {
            app.metadata_panel.result = Some(Err(error));
            return;
        }
    };
    let attempt = Arc::clone(&app.metadata_panel.attempt);
    let result = crate::tasks::spawn(
        app,
        TASK,
        None,
        move || {
            #[cfg(any(test, target_arch = "wasm32"))]
            return request.read_with_diagnostics();
            #[cfg(all(not(test), not(target_arch = "wasm32")))]
            request.read_with_deadline(READ_TIMEOUT)
        },
        move |app, _, result| {
            if Arc::ptr_eq(&app.metadata_panel.attempt, &attempt)
                && app.metadata_panel.key.as_ref() == Some(&key)
                && app.session.library_generation() == key.0
            {
                app.metadata_panel.result = Some(result);
            }
        },
    );
    match result {
        Ok(()) => app.metadata_panel.started_at = Some(web_time::Instant::now()),
        Err(error) => app.metadata_panel.result = Some(Err(error)),
    }
}

/// Draw immediate catalog fields followed by asynchronously read file metadata.
pub(super) fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    load(app, id);
    let Some(photo) = app.session.catalog.photo(id).cloned() else { return };
    header(ui, "Metadata");
    padded(ui, |ui| {
        let t = Tokens::get(ui.ctx());
        for (section, rows) in sections(&photo, &app.metadata_panel.locale) {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(crate::i18n::tr(section)).font(t.semibold(12.5)).color(t.text));
            ui.add_space(4.0);
            for (name, value) in rows {
                row(ui, name, &value, &t);
            }
            if section == "File"
                && let lightcraft_catalog::Source::File { path } = &photo.source
            {
                ui.label(egui::RichText::new(crate::i18n::tr("File Path")).size(11.5).color(t.text_dim));
                let response = ui.add(egui::Label::new(egui::RichText::new(path).size(12.0).color(t.text)).wrap().selectable(true));
                register(ui, "metadata:File Path", response.rect);
                ui.horizontal_wrapped(|ui| {
                    if crate::widgets::text_button(ui, "metadataCopyPath", "Copy File Path", false).clicked() {
                        ui.ctx().copy_text(path.clone());
                    }
                    let response = ui.add_enabled(app.services.reveal.is_some(), egui::Button::new(crate::i18n::tr(crate::menus::reveal_label())));
                    register(ui, "button:metadataReveal", response.rect);
                    if response.clicked()
                        && let Err(error) = app.run("app.showInFinder", serde_json::json!({}))
                    {
                        app.toast(ui.ctx(), error);
                    }
                });
            }
            ui.add_space(8.0);
            ui.separator();
        }
        // Every populated catalog field, including structured focus and region information.
        if let Ok(meta) = serde_json::to_value(&photo.meta) {
            ui.label(egui::RichText::new(crate::i18n::tr("Catalog Metadata")).font(t.semibold(12.5)));
            catalog_rows(ui, "", &meta, 0, &t);
        }
        ui.add_space(8.0);
        ui.separator();
        ui.label(egui::RichText::new(crate::i18n::tr("All File Metadata")).font(t.semibold(12.5)));
        if app.metadata_panel.result.is_some() && ui.button(crate::i18n::tr("Refresh")).clicked() {
            app.metadata_panel.result = None;
            app.metadata_panel.page = 0;
            app.metadata_panel.started_at = None;
            app.metadata_panel.attempt = Arc::new(());
            app.metadata_panel.locale = sys_locale::get_locale().unwrap_or_default();
            ui.ctx().request_repaint();
        }
        match &app.metadata_panel.result {
            None if app.metadata_panel.queued => {
                ui.label(crate::i18n::tr("Waiting for an earlier metadata read to finish…"));
            }
            None => {
                ui.label(crate::i18n::tr("Reading metadata…"));
            }
            Some(Err(error)) => {
                ui.label(crate::i18n::tr("Original metadata unavailable"));
                ui.add(egui::Label::new(error).wrap());
            }
            Some(Ok((data, warning))) => {
                if let Some(warning) = warning {
                    ui.add(egui::Label::new(warning).wrap());
                }
                let tags = data["exif"].as_array().into_iter().flatten().chain(data["xmp"].as_array().into_iter().flatten());
                let count = tags.clone().count();
                let page = app.metadata_panel.page;
                if count > TAGS_PER_PAGE {
                    ui.horizontal(|ui| {
                        if ui.add_enabled(page > 0, egui::Button::new("‹")).clicked() {
                            app.metadata_panel.page -= 1;
                        }
                        ui.label(format!("{} / {}", page + 1, count.div_ceil(TAGS_PER_PAGE)));
                        if ui.add_enabled(page + 1 < count.div_ceil(TAGS_PER_PAGE), egui::Button::new("›")).clicked() {
                            app.metadata_panel.page += 1;
                        }
                    });
                }
                for tag in tags.skip(page * TAGS_PER_PAGE).take(TAGS_PER_PAGE) {
                    let group = tag["group"].as_str().unwrap_or("XMP");
                    let name = tag["name"].as_str().unwrap_or("");
                    let value = tag["value"].as_str().unwrap_or("");
                    let value = tag_value(name, value, &app.metadata_panel.locale);
                    row(ui, &format!("{group} · {name}"), &value, &t);
                }
                if let Some(note) = data["note"].as_str() {
                    ui.label(crate::i18n::tr(note));
                }
            }
        }
    });
}

/// Labels and values share the top edge and both align left, including wrapped rows.
/// Draw a wrapping, selectable label/value pair.
fn row(ui: &mut egui::Ui, name: &str, value: &str, t: &Tokens) {
    ui.horizontal_top(|ui| {
        let label_width = (ui.available_width() * 0.38).min(110.0);
        ui.allocate_ui_with_layout(egui::vec2(label_width, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_width(label_width);
            ui.add(egui::Label::new(egui::RichText::new(crate::i18n::tr(name)).size(11.5).color(t.text_dim)).wrap());
        });
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
            let response = ui.add(egui::Label::new(egui::RichText::new(value).size(12.0).color(t.text)).wrap().selectable(true));
            register(ui, format!("metadata:{name}"), response.rect);
        });
    });
    ui.add_space(4.0);
}

/// Flatten nested catalog metadata into labeled rows, with a recursion limit.
fn catalog_rows(ui: &mut egui::Ui, prefix: &str, value: &Value, depth: usize, t: &Tokens) {
    if depth > 16 {
        return;
    }
    match value {
        Value::Null => {}
        Value::String(text) if text.is_empty() => {}
        Value::Object(fields) => {
            for (key, value) in fields {
                let name = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
                catalog_rows(ui, &name, value, depth + 1, t);
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                catalog_rows(ui, &format!("{prefix}.{}", index + 1), value, depth + 1, t);
            }
        }
        Value::String(text) => row(ui, prefix, text, t),
        _ => row(ui, prefix, &value.to_string(), t),
    }
}

/// Recognize date-bearing EXIF and XMP field names emitted by the metadata parser.
fn is_date_tag(name: &str) -> bool {
    matches!(
        name.rsplit(':').next(),
        Some(
            "DateTime"
                | "DateTimeOriginal"
                | "DateTimeDigitized"
                | "CreateDate"
                | "ModifyDate"
                | "MetadataDate"
                | "DateCreated"
                | "GPSDateStamp"
                | "GPS Date Stamp"
                | "Date/Time Original"
                | "Date/Time Digitized"
                | "Date/Time Modified"
                | "DateTimeCreated"
        )
    )
}

/// Format recognized date fields while preserving every other metadata value literally.
fn tag_value(name: &str, value: &str, locale: &str) -> String {
    if is_date_tag(name) { numeric_date(value, locale) } else { value.to_owned() }
}

/// Convert supported numeric dates to the locale's day/month order and separators.
fn numeric_date(value: &str, locale: &str) -> String {
    let tag = locale.to_ascii_lowercase().replace('_', "-");
    let date = if tag.starts_with("de") || ["ru", "fi", "cs", "pl", "sk"].iter().any(|s| tag.starts_with(s)) {
        "%d.%m.%Y"
    } else if tag == "en-us" {
        "%m/%d/%Y"
    } else if ["ja", "zh", "ko", "sv"].iter().any(|s| tag.starts_with(s)) {
        "%Y-%m-%d"
    } else {
        "%d/%m/%Y"
    };
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(value) {
        return time.format(&format!("{date} %H:%M:%S %:z")).to_string();
    }
    for pattern in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y:%m:%d %H:%M:%S"] {
        if let Ok(time) = chrono::NaiveDateTime::parse_from_str(value, pattern) {
            return time.format(&format!("{date} %H:%M:%S")).to_string();
        }
    }
    for pattern in ["%Y-%m-%d", "%Y:%m:%d"] {
        if let Ok(day) = chrono::NaiveDate::parse_from_str(value, pattern) {
            return day.format(date).to_string();
        }
    }
    value.to_string()
}

type Rows = Vec<(&'static str, String)>;

/// Stable sections, with absent optional values omitted instead of empty editing boxes.
fn sections(p: &lightcraft_catalog::Photo, locale: &str) -> Vec<(&'static str, Rows)> {
    let mut result = Vec::new();
    let mut file =
        vec![("File Name", p.file_name.clone()), ("Format", p.format.to_uppercase()), ("Dimensions", format!("{} × {}", p.width, p.height))];
    if p.file_size > 0 {
        file.push(("File Size", super::info::human_size(p.file_size)));
    }
    if let Some(captured) = &p.captured {
        file.push(("Captured", numeric_date(captured, locale)));
    }
    result.push(("File", file));
    let m = &p.meta;
    let mut camera = Vec::new();
    push(&mut camera, "Camera", &m.camera);
    push(&mut camera, "Lens", &m.lens);
    if let Some(f) = m.focal_mm {
        camera.push(("Focal length", format!("{f:.1} mm")));
    }
    push(&mut camera, "Shutter speed", &m.shutter);
    if let Some(f) = m.aperture {
        camera.push(("Aperture", format!("f/{f:.1}")));
    }
    if let Some(iso) = m.iso {
        camera.push(("ISO", iso.to_string()));
    }
    if let Some(text) = super::info::focus_text(m.focus.as_ref()) {
        camera.push(("Focus", text));
    }
    if !camera.is_empty() {
        result.push(("Camera", camera));
    }
    let mut description = Vec::new();
    for (name, value) in [
        ("Title", &m.title),
        ("Caption", &m.caption),
        ("Alt Text", &m.alt_text),
        ("Extended Description", &m.extended_description),
        ("Creator", &m.creator),
        ("Copyright", &m.copyright),
    ] {
        push(&mut description, name, value);
    }
    if !m.keywords.is_empty() {
        description.push(("Keywords", m.keywords.join(", ")));
    }
    if !description.is_empty() {
        result.push(("Description", description));
    }
    let mut location = Vec::new();
    for (name, value) in [("Location", &m.location), ("City", &m.city), ("State / Province", &m.state), ("Country", &m.country)] {
        push(&mut location, name, value);
    }
    if let Some((lat, lon)) = m.gps {
        location.push(("GPS", format!("{lat:.6}, {lon:.6}")));
    }
    if !location.is_empty() {
        result.push(("Location", location));
    }
    result
}

/// Add a non-empty optional field to a stable metadata section.
fn push(rows: &mut Rows, name: &'static str, value: &str) {
    if !value.trim().is_empty() {
        rows.push((name, value.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::numeric_date;

    #[test]
    fn dates_follow_locale_with_two_digit_day_and_month() {
        for name in ["Date/Time Original", "Date/Time Digitized", "Date/Time Modified"] {
            assert_eq!(super::tag_value(name, "2014:01:01 11:48:16", "de_DE"), "01.01.2014 11:48:16");
        }
        assert_eq!(super::tag_value("GPS Date Stamp", "2014:01:01", "de_DE"), "01.01.2014");
        assert_eq!(super::tag_value("dc:title", "2014:01:01", "de_DE"), "2014:01:01");
        assert_eq!(numeric_date("2014-01-01", "de_DE"), "01.01.2014");
        assert_eq!(numeric_date("2014-01-01T11:48:16", "de-DE"), "01.01.2014 11:48:16");
        assert_eq!(numeric_date("2014:01:01 11:48:16", "de_DE"), "01.01.2014 11:48:16");
        assert_eq!(numeric_date("2014-01-01T11:48:16+01:00", "de_DE"), "01.01.2014 11:48:16 +01:00");
        assert_eq!(numeric_date("2014-12-01", "en-US"), "12/01/2014");
        assert_eq!(numeric_date("2014-12-01", "en-GB"), "01/12/2014");
        assert_eq!(numeric_date("IMG_2014-01-01.JPG", "de_DE"), "IMG_2014-01-01.JPG");
    }
}
