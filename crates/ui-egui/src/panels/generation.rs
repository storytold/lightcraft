//! The selected mask's prompt and the explicit accept/discard step for ComfyUI results.
use serde_json::json;

use crate::LightcraftApp;
use crate::i18n::tr;
use crate::widgets::{divider, register};

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    if cfg!(target_arch = "wasm32") {
        return;
    }
    divider(ui);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 8, bottom: 10 }).show(ui, |ui| {
        let expanded = !app.ui.generation.prompt.trim().is_empty();
        let section = egui::CollapsingHeader::new(tr("Generate replacement")).id_salt("generation-section").default_open(expanded).show(ui, |ui| {
            ui.add_space(4.0);
            let running = app.session.generation.as_ref().is_some_and(|g| g.running());
            let pending = app.session.generation.as_ref().is_some_and(|g| g.preview().is_some());
            ui.add_enabled_ui(!running && !pending, |ui| {
                let r = ui.add(
                    egui::TextEdit::multiline(&mut app.ui.generation.prompt)
                        .hint_text(tr("Describe what should replace the masked area"))
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                register(ui, "generation:prompt", r.rect);
                ui.collapsing(tr("ComfyUI settings"), |ui| {
                    ui.label(tr("Server URL"));
                    let r = ui.text_edit_singleline(&mut app.ui.generation.endpoint);
                    register(ui, "generation:endpoint", r.rect);
                    ui.label(tr("Checkpoint filename"));
                    let r = ui.text_edit_singleline(&mut app.ui.generation.model);
                    register(ui, "generation:model", r.rect);
                    ui.label(tr("Negative prompt"));
                    let r = ui.text_edit_multiline(&mut app.ui.generation.negative);
                    register(ui, "generation:negative", r.rect);
                    ui.horizontal(|ui| {
                        ui.label(tr("Seed"));
                        let r = ui.add(egui::DragValue::new(&mut app.ui.generation.seed).range(0..=9_007_199_254_740_991_u64));
                        register(ui, "generation:seed", r.rect);
                    });
                });
                ui.small(tr("Generate sends the selected region and mask to your ComfyUI server."));
                let r = ui.add_enabled(!app.ui.generation.prompt.trim().is_empty(), egui::Button::new(tr("Generate")));
                register(ui, "generation:generate", r.rect);
                if r.clicked() {
                    let params = serde_json::to_value(&app.ui.generation);
                    match params {
                        Ok(p) => {
                            let _ = app.run("mask.generate", p);
                        }
                        Err(e) => app.toast(ui.ctx(), e.to_string()),
                    }
                }
            });
            if let Some(g) = &app.session.generation
                && let Some(e) = g.status().get("error").and_then(|v| v.as_str())
            {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
        });
        register(ui, "generation:section", section.header_response.rect);
    });
}

pub fn preview(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(run) = app.session.generation.clone() else { return };
    if !run.belongs_to(&app.session) {
        let _ = app.run("mask.discardGeneration", json!({}));
        return;
    }
    if run.running() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return;
    }
    let Some(image) = run.preview() else { return };
    if app.ui.dialog.is_some() {
        return;
    }
    let key = egui::Id::unique("generation-preview-texture");
    let cached = ctx.data(|d| d.get_temp::<(u64, egui::TextureHandle)>(key));
    let texture = match cached.filter(|(id, _)| *id == run.id) {
        Some((_, texture)) => texture,
        None => {
            let bytes: Vec<u8> = image.data.iter().flatten().copied().collect();
            let texture = ctx.load_texture(
                "Generated replacement",
                egui::ColorImage::from_rgba_unmultiplied([image.width, image.height], &bytes),
                egui::TextureOptions::LINEAR,
            );
            ctx.data_mut(|d| d.insert_temp(key, (run.id, texture.clone())));
            texture
        }
    };
    let (mut accept, mut discard, mut retry) = (false, false, false);
    let mut open = true;
    let screen = ctx.content_rect().size();
    egui::Window::new(tr("Generated replacement"))
        .id(egui::Id::unique("generation-preview"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(780.0_f32.min(screen.x - 40.0))
        .show(ctx, |ui| {
            let max = egui::vec2(ui.available_width(), (screen.y - 220.0).max(80.0));
            ui.add(
                egui::Image::new(&texture)
                    .fit_to_exact_size(texture.size_vec2() * (max.x / texture.size_vec2().x).min(max.y / texture.size_vec2().y).min(1.0)),
            );
            ui.label(&run.settings.prompt);
            ui.small(tr("Accept saves a new image stacked with the original. Your original stays unchanged."));
            ui.horizontal_wrapped(|ui| {
                let r = ui.button(tr("Accept replacement"));
                register(ui, "generation:accept", r.rect);
                accept = r.clicked();
                let r = ui.add_enabled(app.session.active() == Some(run.photo), egui::Button::new(tr("Try again")));
                register(ui, "generation:retry", r.rect);
                retry = r.clicked();
                let r = ui.button(tr("Discard"));
                register(ui, "generation:discard", r.rect);
                discard = r.clicked();
            });
        });
    if accept {
        let _ = app.run("mask.acceptGeneration", json!({}));
    }
    if discard || !open {
        let _ = app.run("mask.discardGeneration", json!({}));
    }
    if retry {
        let mut settings = run.settings.clone();
        settings.seed = (settings.seed + 1) % 9_007_199_254_740_992;
        app.ui.generation = settings.clone();
        if let Ok(mut p) = serde_json::to_value(&settings) {
            p["maskId"] = json!(run.mask_id);
            let _ = app.run("mask.generate", p);
        }
    }
}
