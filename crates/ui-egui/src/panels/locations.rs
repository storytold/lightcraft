//! Location filling uses the current view and the shared activity stack.
use crate::{
    LightcraftApp,
    theme::Tokens,
    widgets::{register, text_button},
};
use serde_json::json;

pub fn pump(app: &mut LightcraftApp, ctx: &egui::Context) {
    let key = egui::Id::new("location-fill-next-pump");
    let now = ctx.input(|i| i.time);
    if ctx.data(|d| d.get_temp::<f64>(key)).is_some_and(|next| now < next) {
        return;
    }
    let result = app.session.execute("metadata.pumpLocations", &json!({}));
    let active = result.as_ref().is_ok_and(|v| v["active"] == true);
    match result {
        Ok(v) if v["complete"] == true => {
            app.toast(ctx, format!("{}: {}", crate::i18n::tr("Locations saved to catalog"), v["changed"].as_u64().unwrap_or(0)))
        }
        Err(e) => app.toast_error(ctx, e.to_string()),
        _ => {}
    }
    let delay = if active { 0.1 } else { 5.0 };
    ctx.data_mut(|d| d.insert_temp(key, now + delay));
    if active {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(delay));
    }
}

pub fn controls(app: &mut LightcraftApp, ui: &mut egui::Ui, has_gps: bool) {
    let task = app.session.activity.list().into_iter().find(|t| t.kind == "geocode");
    ui.horizontal_wrapped(|ui| {
        let fill=ui.add_enabled_ui(has_gps,|ui|text_button(ui,"fillLocation","Fill Location",false)).inner;
        if fill.on_hover_text(crate::i18n::tr("Fill missing places for selected photos and save to the catalog.")).clicked() {
            match app.run("metadata.reverseGeocode",json!({})) {
                Ok(v) => app.toast(ui.ctx(),format!("{}: {}",crate::i18n::tr("Locations saved to catalog"),v["changed"].as_u64().unwrap_or(0))),
                Err(e) => app.toast_error(ui.ctx(),e),
            }
        }
        let fill_view=ui.add_enabled_ui(task.is_none(),|ui|text_button(ui,"fillLocationView","Fill view",false)).inner;
        if fill_view.on_hover_text(crate::i18n::tr("Fill missing places for every GPS photo in the current view. Respects the folder, subfolders and filters; preserves existing values.")).clicked() {
            match app.run("metadata.fillLocations",json!({})) {
                Ok(v) if v["started"]==false => app.toast(ui.ctx(),crate::i18n::tr("No missing places to fill in this view.")),
                Ok(_) => {ui.ctx().data_mut(|d|d.remove::<f64>(egui::Id::new("location-fill-next-pump")));},
                Err(e) => app.toast_error(ui.ctx(),e),
            }
        }
    });
    if let Some(task) = task {
        ui.label(format!("{} · {} / {}", crate::i18n::tr("Filling places"), task.done, task.total));
        let bar = super::activity::bar(ui, &Tokens::get(ui.ctx()), &task, ui.available_width());
        register(ui.ctx(), "locations:progress", bar);
    }
}
