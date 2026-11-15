//! The AI Models dialog (`dialog.aiModels`): every AI mask model with its size, licence and
//! download, and which model computes Object masks (`ai.models.setObjectModel`). Each Download
//! button sits next to the size and licence it agrees to; nothing downloads by itself. The
//! downloadable AI Denoise models are listed too (`denoise::ai_models_section`).

use serde_json::json;

use crate::LccApp;
use crate::theme::Tokens;

/// The Object model choices: (command value, label).
const OBJECT_MODELS: &[(&str, &str)] = &[("auto", "Automatic"), ("sam3", "SAM 3"), ("sam2", "SAM 2.1")];

/// The Sky / Landscape / People model choices: (command value, label).
const SCENE_MODELS: &[(&str, &str)] = &[("analysis", "Photo analysis"), ("eomt", "EoMT")];

/// Settings ▸ AI Models: every AI model in one place, with its download, and where SAM 3 is kept.
pub fn settings_tab(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    let key = egui::Id::new("ai-models-error");
    let mut error: Option<String> = ui.data(|d| d.get_temp(key)).unwrap_or_default();
    super::settings::heading_help(ui, t, "AI Models", "aiModels");
    body(app, ui, &mut error);
    ui.data_mut(|d| d.insert_temp(key, error));
    sam3_folder(app, ui, t);
    super::faces::models_section(app, ui, t);
    super::denoise::other_models_section(app, ui, t);
}

/// `app.sam3Folder {dir?}`: keep the SAM 3 model in `dir` (empty: the default folder); without it, a folder dialog.
pub fn sam3_folder_command(app: &mut LccApp, p: &serde_json::Value) -> Result<serde_json::Value, String> {
    let dir = match p.get("dir").and_then(serde_json::Value::as_str) {
        Some(d) => d.trim().to_string(),
        None => {
            let req = crate::pick::PickRequest::folder(crate::i18n::tr("Choose SAM 3 Model Folder"));
            match crate::pick::ask(app, "app.sam3Folder", p, "dir", req, |s| s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x])) {
                crate::pick::Picked::Now(v) => match v.into_iter().next() {
                    Some(d) => d,
                    None => return Ok(serde_json::Value::Null),
                },
                crate::pick::Picked::Later => return Ok(serde_json::Value::Null),
                crate::pick::Picked::Unavailable => return Err("no folder dialog here: pass {dir}".into()),
            }
        }
    };
    app.ui.settings.sam3_dir = dir.clone();
    Ok(json!({"dir": dir}))
}

/// Where the SAM 3 model is kept: the folder it downloads to, or is installed by hand into.
fn sam3_folder(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    super::settings::heading_help(ui, t, "SAM 3 model folder", "sam3");
    let current = app.session.segmenter.dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default();
    let custom = !app.ui.settings.sam3_dir.is_empty();
    super::settings::hint(ui, t, "SAM 3 is large. Choose another folder (another drive, say), or point to one that already holds it.");
    super::settings::row(ui, t, crate::i18n::tr("Folder"), |ui| {
        ui.add(
            egui::Label::new(
                egui::RichText::new(if current.is_empty() { crate::i18n::tr("Not chosen").to_string() } else { current.clone() }).color(t.text),
            )
            .truncate(),
        );
    });
    crate::dialog_frame::control_row(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let can = app.services.picker.is_some() || app.services.pick_folder.is_some();
        let r = ui.add_enabled_ui(can, |ui| crate::widgets::text_button(ui, "samFolderChoose", "Choose…", false)).inner;
        if r.clicked()
            && let Err(e) = app.run("app.sam3Folder", json!({}))
        {
            app.toast_error(ui.ctx(), e);
        }
        if custom {
            let r = crate::widgets::text_button(ui, "samFolderDefault", "Use the Default Folder", false);
            if r.clicked() {
                let _ = app.run("app.sam3Folder", json!({"dir": ""}));
            }
        }
        if app.session.segmenter.dir.is_some() && crate::menus::ui_enabled(app, "app.openSamModelFolder") {
            let r = crate::widgets::text_button(ui, "samFolder", crate::menus::reveal_label(), false);
            if r.clicked()
                && let Err(e) = app.run("app.openSamModelFolder", json!({}))
            {
                app.toast_error(ui.ctx(), e);
            }
        }
    });
    if app.session.segmenter.mirrors().is_empty() {
        super::settings::hint(
            ui,
            t,
            "This build has no download address for SAM 3: put model.safetensors, vocab.json and merges.txt from Meta's facebook/sam3 in the folder above, and Object and Describe masks work as soon as they are there.",
        );
        let help = super::dialogs::SAM_HELP;
        let r = crate::dialog_frame::link(ui, Some("samHelp"), "How to install the model").on_hover_text(help);
        if r.clicked()
            && let Err(e) = crate::links::open(app, help)
        {
            app.toast(ui.ctx(), e);
        }
    }
    super::settings::hint(ui, t, "Changing the folder doesn't move a model already downloaded: copy its files over, or download it again.");
}

pub fn body(app: &mut LccApp, ui: &mut egui::Ui, error: &mut Option<String>) {
    let t = Tokens::get(ui.ctx());
    ui.label(crate::i18n::tr("AI masks use models that aren't part of {app}. Each is a one-time download; everything else works without them."));
    let status = app.session.segmenter.model_status();
    let mut running = false;
    for m in status.iter().filter(|m| m.offered) {
        crate::dialog_frame::group_begin(ui, Some(m.title.as_str()));
        ui.label(crate::i18n::tr(&m.purpose));
        ui.label(
            egui::RichText::new(format!(
                "{} {:.0} MB · {} {}",
                crate::i18n::tr("About"),
                m.size_bytes as f64 / 1e6,
                crate::i18n::tr("Licence:"),
                m.license
            ))
            .color(t.text_label),
        );
        if let Some(credit) = &m.credit {
            ui.label(egui::RichText::new(credit).color(t.text_dim));
        }
        let r = ui.link(crate::i18n::tr("Read the licence")).on_hover_text(&m.license_url);
        crate::widgets::register(ui.ctx(), format!("link:aiModelLicense-{}", m.id), r.rect);
        if r.clicked() {
            let _ = crate::links::open(app, &m.license_url);
        }
        let d = &m.download;
        if m.installed {
            ui.label(egui::RichText::new(crate::i18n::tr("Installed")).color(t.text_dim));
        } else if d.running {
            running = true;
            let mb = |b: u64| b as f64 / 1e6;
            let text = format!("{:.0} / {:.0} MB", mb(d.done), mb(d.total));
            crate::progress::Progress::new(crate::progress::fraction_of(d.done, d.total))
                .label(text)
                .widget(format!("progress:aiModel-{}", m.id))
                .show(ui);
            if crate::button::Button::new("Cancel Download").widget(format!("button:aiModelCancel-{}", m.id)).show(ui).clicked() {
                let _ = app.run("ai.models.cancel", json!({"id": m.id}));
            }
        } else {
            if let Some(e) = &d.error
                && !d.cancelled
            {
                ui.label(
                    egui::RichText::new(format!("{} {e}", crate::i18n::tr("The download didn't work:"))).color(egui::Color32::from_rgb(230, 90, 80)),
                );
            }
            // the button agrees to the size and licence shown right above it
            if crate::button::Button::new("Download").widget(format!("button:aiModelDownload-{}", m.id)).show(ui).clicked() {
                *error = app.run("ai.models.download", json!({"id": m.id, "acknowledged": true})).err();
            }
        }
    }
    crate::panels::denoise::ai_models_section(app, ui, &t);
    crate::panels::ml_models::models_section(app, ui, &t);
    crate::dialog_frame::group_begin(ui, Some("Object masks use"));
    let current = app.session.segmenter.object_model().key();
    let items: Vec<(&str, &str)> = OBJECT_MODELS.iter().map(|(k, l)| (*l, *k)).collect();
    let active = OBJECT_MODELS.iter().position(|(k, _)| *k == current);
    if let Some(i) = choice_row(ui, "aiObjectModel", &items, active)
        && let Some((k, _)) = OBJECT_MODELS.get(i)
    {
        *error = app.run("ai.models.setObjectModel", json!({"model": k})).err();
    }
    crate::dialog_frame::note(
        ui,
        egui::RichText::new(crate::i18n::tr("Automatic uses SAM 3 when it is installed, otherwise SAM 2.1. Describe masks always use SAM 3."))
            .color(t.text_dim),
    );
    crate::dialog_frame::group_end(ui);
    crate::dialog_frame::group_begin(ui, Some("Sky, Landscape and People masks use"));
    let current = app.session.segmenter.scene_model().key();
    let items: Vec<(&str, &str)> = SCENE_MODELS.iter().map(|(k, l)| (*l, *k)).collect();
    let active = SCENE_MODELS.iter().position(|(k, _)| *k == current);
    if let Some(i) = choice_row(ui, "aiSceneModel", &items, active)
        && let Some((k, _)) = SCENE_MODELS.get(i)
    {
        *error = app.run("ai.models.setSceneModel", json!({"model": k})).err();
    }
    crate::dialog_frame::note(
        ui,
        egui::RichText::new(crate::i18n::tr(
            "The photo analysis needs no download. EoMT is used once it is installed; until then the photo analysis stands in.",
        ))
        .color(t.text_dim),
    );
    crate::dialog_frame::group_end(ui);
    if let Some(e) = error.as_deref() {
        ui.label(egui::RichText::new(e).color(egui::Color32::from_rgb(230, 90, 80)));
    }
    if running {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
}

/// The model choices side by side, each sharing the row; in the Classic Settings window the row is
/// no wider than its help text, so two or three buttons don't stretch across a wide window.
fn choice_row(ui: &mut egui::Ui, id: &str, items: &[(&str, &str)], active: Option<usize>) -> Option<usize> {
    ui.scope(|ui| {
        if crate::dialog_frame::plain(ui) {
            ui.set_max_width(ui.available_width().min(crate::dialog_frame::HELP_MAX_W));
        }
        crate::widgets::segmented(ui, id, items, active, items.len())
    })
    .inner
}
