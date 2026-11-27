use super::{LightcraftApp, Tokens, json};

/// No SAM 3 model, no download running and nowhere to download it from: installing it by hand is
/// all the dialog can offer.
pub(super) fn sam_by_hand(sam: &lightcraft_engine::segment::Segmenter) -> bool {
    !sam.installed() && !sam.download_status().running && sam.mirrors().is_empty()
}

/// The model installation guide.
const SAM_HELP: &str = "https://github.com/storytold/lightcraft/blob/main/docs/ai-masks.md#getting-the-model";

/// Open the configured SAM 3 folder itself; revealing a file opens its parent instead.
pub(crate) fn show_model_folder(app: &mut LightcraftApp) -> Result<serde_json::Value, String> {
    let dir = app.session.segmenter.dir.as_ref().ok_or("no folder is set for the SAM 3 model")?;
    let path = dir.to_str().ok_or("the model folder path is not valid UTF-8")?;
    let open = app.services.open_with.as_mut().ok_or("opening model folders is not available here")?;
    // Create only after the host action is available. Existing model files are left alone.
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    #[cfg(target_os = "linux")]
    if let Some(open_folder) = app.services.open_model_folder.clone() {
        let path = path.to_string();
        let folder = path.clone();
        crate::tasks::spawn(
            app,
            "Show in File Manager",
            Some("openModelFolder"),
            move || open_folder(&folder),
            |app, ctx, result| {
                if let Err(e) = result {
                    app.toast_error(ctx, e);
                }
            },
        )?;
        return Ok(json!({"path": path, "background": true}));
    }
    open(path, "")?;
    Ok(json!({"path": path}))
}

/// The SAM 3 dialog: what the model is, its size and licence, and the download's progress.
pub(super) fn sam_model_body(app: &mut LightcraftApp, ui: &mut egui::Ui, error: Option<&str>) {
    use lightcraft_engine::segment::{LICENSE_NAME, LICENSE_URL, MODEL_BYTES};
    let t = Tokens::get(ui.ctx());
    let seg = &app.session.segmenter;
    let d = seg.download_status();
    if seg.installed() {
        ui.label(crate::i18n::tr("The SAM 3 model is installed: Object and Describe masks are ready."));
        return;
    }
    let gb = |b: u64| b as f64 / 1e9;
    ui.label(crate::i18n::tr(
        "Object and Describe masks use SAM 3, Meta's segmentation model. It isn't part of LightCraft, and everything else works without it.",
    ));
    let dir = seg.dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default();
    ui.label(format!("{} {:.1} GB, {} {dir}", crate::i18n::tr("A one-time download of about"), gb(MODEL_BYTES), crate::i18n::tr("saved in")));
    ui.label(
        egui::RichText::new(format!(
            "{} {LICENSE_NAME} — {}",
            crate::i18n::tr("Licence:"),
            crate::i18n::tr("Meta's terms, not LightCraft's. Downloading it means accepting them.")
        ))
        .color(t.text_label),
    );
    let r = ui.link(crate::i18n::tr("Read the SAM License")).on_hover_text(LICENSE_URL);
    crate::widgets::register(ui, "link:samLicense", r.rect);
    if r.clicked() {
        let _ = crate::links::open(app, LICENSE_URL);
    }
    let seg = &app.session.segmenter;
    if d.running {
        ui.add_space(4.0);
        let text = format!("{:.2} / {:.2} GB · {}", gb(d.done), gb(d.total), d.file);
        crate::progress::Progress::new(crate::progress::fraction_of(d.done, d.total)).label(text).widget("progress:samDownload").show(ui);
        let r = ui.button(crate::i18n::tr("Cancel Download"));
        crate::widgets::register(ui, "button:samCancel", r.rect);
        if r.clicked() {
            app.session.segmenter.cancel_download();
        }
        ui.label(egui::RichText::new(crate::i18n::tr("You can close this: the download continues, and resumes if interrupted.")).color(t.text_dim));
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        return;
    }
    if seg.mirrors().is_empty() {
        // nothing to download from: say how to install it by hand, with the folder and the guide a click away
        let dir = seg.dir.clone();
        ui.label(
            egui::RichText::new(crate::i18n::tr(
                "This build can't download the model yet. To install it by hand, put model.safetensors, vocab.json and merges.txt from Meta's facebook/sam3 in the folder above: Object and Describe work as soon as they are there.",
            ))
            .color(t.text_dim),
        );
        ui.horizontal_wrapped(|ui| {
            if dir.is_some() && crate::menus::ui_enabled(app, "app.openSamModelFolder") {
                let r = ui.button(crate::i18n::tr(crate::menus::reveal_label()));
                crate::widgets::register(ui, "button:samFolder", r.rect);
                if r.clicked()
                    && let Err(e) = app.run("app.openSamModelFolder", json!({}))
                {
                    app.toast_error(ui.ctx(), e);
                }
            }
            let r = ui.link(crate::i18n::tr("How to install the model")).on_hover_text(SAM_HELP);
            crate::widgets::register(ui, "link:samHelp", r.rect);
            if r.clicked()
                && let Err(e) = crate::links::open(app, SAM_HELP)
            {
                app.toast(ui.ctx(), e);
            }
        });
    }
    if let Some(e) = error.map(str::to_string).or(d.error) {
        ui.label(egui::RichText::new(format!("{} {e}", crate::i18n::tr("The download didn't work:"))).color(egui::Color32::from_rgb(230, 90, 80)));
    }
}
