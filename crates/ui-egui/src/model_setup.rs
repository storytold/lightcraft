//! Resume a denoise request after model setup, bound to its original photo and library.

use crate::i18n::tr;
use crate::{LightcraftApp, state::Dialog};
use lightcraft_catalog::{PhotoId, Source};
use lightcraft_engine::denoise::PhotoState;
use serde_json::{Value, json};

struct Request {
    library: u64,
    photo: PhotoId,
    source: Source,
}

impl Request {
    fn capture(app: &LightcraftApp, id: PhotoId) -> Result<Self, String> {
        let photo = app.session.catalog.photo(id).filter(|p| !p.deleted).ok_or("the requested photo is no longer available")?;
        Ok(Self { library: app.session.library_generation(), photo: id, source: photo.source.clone() })
    }

    fn valid(&self, app: &LightcraftApp) -> bool {
        self.library == app.session.library_generation()
            && app.session.catalog.photo(self.photo).is_some_and(|p| !p.deleted && p.source == self.source)
    }
}

#[derive(Default)]
pub(crate) struct Pending {
    denoise: Option<Request>,
    next_check: f64,
}

pub(crate) fn denoise_requested(app: &LightcraftApp, id: PhotoId) -> bool {
    app.model_setup.denoise.as_ref().is_some_and(|r| r.photo == id && r.valid(app))
}

pub(crate) fn intercept(app: &mut LightcraftApp, command: &str, params: &Value) -> Option<Result<Value, String>> {
    if matches!(command, "app.openLibrary" | "file.restoreLibrary") {
        app.model_setup = Pending::default();
        return None;
    }
    if command == "modelSetup.cancel" {
        if params.get("kind").and_then(Value::as_str) != Some("denoise") {
            return Some(Err("modelSetup.cancel: use kind denoise".into()));
        }
        app.model_setup.denoise = None;
        app.ui.status = tr("Pending photo action cancelled").into();
        return Some(Ok(json!({"cancelled": true})));
    }
    if command == "denoise.models.downloadCancel" {
        app.model_setup.denoise = None;
    }
    if command != "denoise.toggle" {
        return None;
    }
    let id = match params.get("id") {
        Some(v) => match v.as_u64() {
            Some(id) => PhotoId(id),
            None => return Some(Err("denoise.toggle: id must be a photo id".into())),
        },
        None => match app.session.active() {
            Some(id) => id,
            None => return Some(Err("select a raw photo".into())),
        },
    };
    let enabled = app.session.develop_of(id).is_some_and(|d| d.enhance.denoise_enabled());
    let on = match params.get("enabled") {
        Some(v) => match v.as_bool() {
            Some(on) => on,
            None => return Some(Err("denoise.toggle: enabled must be true or false".into())),
        },
        None => !enabled && !denoise_requested(app, id),
    };
    if !on {
        app.model_setup.denoise = None;
        return Some(app.session.execute("denoise.toggle", &json!({"id": id.0, "enabled": false})).map_err(|e| e.to_string()));
    }
    // refusal expected: only refreshes the model state read on the next line; this is inside `run`, which answers for denoise.toggle
    let _ = app.session.execute("denoise.status", &json!({"id": id.0}));
    if !matches!(app.session.denoise_photo_state(id), PhotoState::NoModel) {
        return None;
    }
    if app.session.execute("denoise.models.list", &json!({})).ok().is_none_or(|v| v["dir"].is_null()) {
        return None;
    }
    Some(Request::capture(app, id).map(|request| {
        app.model_setup.denoise = Some(request);
        app.model_setup.next_check = 0.0;
        app.ui.dialog = Some(Dialog::Settings { tab: "denoise".into() });
        app.ui.status = tr("Install a denoise model to enable AI Denoise on this photo").into();
        json!({"setupRequired": true, "photo": id.0})
    }))
}

pub(crate) fn notice(app: &mut LightcraftApp, ui: &mut egui::Ui, kind: &str) {
    if kind != "denoise" || !app.model_setup.denoise.as_ref().is_some_and(|r| r.valid(app)) {
        return;
    }
    ui.add(egui::Label::new(tr("After installation, AI Denoise will turn on for the requesting photo.")).wrap());
    let r = ui.small_button(tr("Cancel pending action"));
    crate::widgets::register(ui.ctx(), "modelSetup:cancel:denoise", r.rect);
    if r.clicked() {
        app.act("modelSetup.cancel", json!({"kind": "denoise"}));
    }
    ui.add_space(6.0);
}

pub(crate) fn pump(app: &mut LightcraftApp, ctx: &egui::Context) {
    if app.model_setup.denoise.is_none() {
        return;
    }
    if app.session.interaction.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        return;
    }
    let now = ctx.input(|i| i.time);
    if now < app.model_setup.next_check {
        return;
    }
    app.model_setup.next_check = now + 0.25;
    let Some(request) = app.model_setup.denoise.take() else { return };
    if !request.valid(app) {
        app.ui.status = tr("Pending photo action cancelled because its library or photo changed").into();
        return;
    }
    if matches!(app.session.denoise_photo_state(request.photo), PhotoState::NoModel | PhotoState::NotApplicable) {
        app.model_setup.denoise = Some(request);
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        return;
    }
    match app.run("denoise.toggle", json!({"id": request.photo.0, "enabled": true})) {
        Ok(_) => app.ui.status = tr("AI Denoise is enabled on the requesting photo").into(),
        Err(error) => app.ui.status = error,
    }
}
