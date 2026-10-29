//! App-level preview preferences, shared by Settings and the control command.

use serde_json::{Value, json};

use crate::{LightcraftApp, state::AppSettings};

/// Current preferences in command units: -1 means no user pixel limit.
pub fn value(settings: &AppSettings) -> Value {
    json!({"percent": settings.preview_percent, "pixelLimit": pixel_limit(settings)})
}

/// The saved zero sentinel expressed in the units shown in Settings.
pub fn pixel_limit(settings: &AppSettings) -> i64 {
    if settings.preview_limit == 0 { -1 } else { i64::from(settings.preview_limit) }
}

/// Query or atomically update `app.previewSize {percent?, pixelLimit?}`. Malformed,
/// fractional or unknown parameters fail before either preference changes.
pub fn command(app: &mut LightcraftApp, params: &Value) -> Result<Value, String> {
    if !params.is_null() && !params.is_object() {
        return Err("app.previewSize: parameters must be an object".into());
    }
    if let Some(fields) = params.as_object() {
        for key in fields.keys() {
            if !matches!(key.as_str(), "percent" | "pixelLimit") {
                return Err(format!("app.previewSize: unknown parameter `{key}` (percent, pixelLimit)"));
            }
        }
    }
    let percent = match params.get("percent") {
        None => app.ui.settings.preview_percent,
        Some(v) => v.as_u64().filter(|n| *n <= 100).ok_or("app.previewSize: percent must be an integer from 0 to 100")? as u32,
    };
    let limit = match params.get("pixelLimit") {
        None => app.ui.settings.preview_limit,
        Some(v) => match v.as_i64() {
            Some(-1) => 0,
            Some(n) if (1..=i64::from(u32::MAX)).contains(&n) => n as u32,
            _ => return Err("app.previewSize: pixelLimit must be -1 or an integer from 1 to 4294967295".into()),
        },
    };
    app.ui.settings.preview_percent = percent;
    app.ui.settings.preview_limit = limit;
    Ok(value(&app.ui.settings))
}

/// Preview percentage slider, named for accessibility and registered for real-input automation.
pub fn percent_slider(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    use lightcraft_develop::controls::{ControlSpec, Section, Track};
    const PERCENT: ControlSpec = ControlSpec {
        id: "settingsPreviewPercent",
        label: "Preview size",
        section: Section::Light,
        min: 0.0,
        max: 100.0,
        default: 100.0,
        step: 1.0,
        decimals: 0,
        track: Track::Plain,
    };
    let label = format!("{} (%)", crate::i18n::tr("Preview size"));
    let out = crate::widgets::slider(ui, &PERCENT, f64::from(app.ui.settings.preview_percent), true, Some(&label));
    if let Some(value) = out.value {
        let percent = PERCENT.clamp(value).round() as u32;
        let _ = app.run("app.previewSize", json!({"percent": percent}));
    }
}

/// Editable pixel cap. Return/Tab commits, Escape cancels, invalid text leaves the
/// active preference intact and shows a localized correction.
pub fn pixel_field(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    const WIDGET: &str = "field:settingsPreviewPixels";
    let memo = egui::Id::unique(WIDGET).with("value");
    let limit = pixel_limit(&app.ui.settings);
    let current = limit.to_string();
    let mut text = if crate::text_field::was_editing(ui.ctx(), WIDGET) {
        ui.data(|d| d.get_temp::<(i64, String)>(memo).filter(|(saved, _)| *saved == limit).map(|(_, text)| text)).unwrap_or(current)
    } else {
        current
    };
    let response = crate::text_field::TextField::singleline(WIDGET, &mut text)
        .label(crate::i18n::tr("Pixel limit"))
        .width(100.0_f32.min(ui.available_width()))
        .select_on_focus(true)
        .show(ui);
    if response.committed() {
        let parsed = text.trim().parse::<i64>().ok();
        if let Some(n) = parsed.filter(|n| *n == -1 || (1..=i64::from(u32::MAX)).contains(n)) {
            let _ = app.run("app.previewSize", json!({"pixelLimit": n}));
        } else {
            app.toast(ui.ctx(), crate::i18n::tr("Enter a positive pixel limit, or -1 for no limit."));
        }
    }
    ui.data_mut(|d| d.insert_temp(memo, (pixel_limit(&app.ui.settings), text)));
}
