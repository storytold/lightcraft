//! The selected mask's prompt and the explicit accept/discard step for ComfyUI results, and the
//! ComfyUI page of Settings (server, model, negative prompt, Qwen instruction).
use serde_json::json;

use lightcraft_engine::comfyui::{DEFAULT_QWEN_INSTRUCTION, PROMPT_PLACEHOLDER};

use super::settings::{heading, hint, row};
use crate::LightcraftApp;
use crate::i18n::tr;
use crate::text_field::TextField;
use crate::theme::Tokens;
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
                TextField::multiline("generation:prompt", &mut app.ui.generation.prompt)
                    .hint(tr("Describe what should replace the masked area"))
                    .rows(3)
                    .width(f32::INFINITY)
                    .show(ui);
                ui.horizontal(|ui| {
                    ui.label(tr("Seed"));
                    let r = ui.add(egui::DragValue::new(&mut app.ui.generation.seed).range(0..=9_007_199_254_740_991_u64));
                    register(ui, "generation:seed", r.rect);
                });
                if !app.ui.generation.prompt.trim().is_empty() {
                    let sent = app.ui.generation.resolved_prompt();
                    egui::CollapsingHeader::new(tr("Prompt sent to ComfyUI")).id_salt("generation-sent").show(ui, |ui| {
                        let r = ui.add(egui::Label::new(egui::RichText::new(sent).small()).wrap().selectable(true));
                        register(ui, "generation:sentPrompt", r.rect);
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    let kind = if app.ui.generation.is_qwen() { "Qwen" } else { "SDXL" };
                    ui.small(format!("{kind} · {}", app.ui.generation.model));
                    let r = crate::button::Button::new("ComfyUI settings…").small().ghost().widget("generation:settings").show(ui);
                    if r.clicked() {
                        app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "comfyui".into() });
                    }
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

/// Settings ▸ ComfyUI: the server, model, negative prompt and the instruction Qwen receives.
/// Saved with the app settings (`ui.json`); nothing is sent until Generate.
pub fn settings_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, "ComfyUI");
    if cfg!(target_arch = "wasm32") {
        hint(ui, t, "Generate replacement needs the desktop app.");
        return;
    }
    hint(
        ui,
        t,
        "Generate replacement in the Masking panel sends the selected region and mask to this ComfyUI server. Nothing is sent until you press Generate.",
    );
    let g = &mut app.ui.generation;
    row(ui, t, "Server URL", |ui| TextField::singleline("generation:endpoint", &mut g.endpoint).width(f32::INFINITY).show(ui));
    row(ui, t, "Model filename", |ui| TextField::singleline("generation:model", &mut g.model).width(f32::INFINITY).show(ui));
    hint(
        ui,
        t,
        "A .gguf filename runs the Qwen Image 2.1 workflow (for example qwen-image-2.1-UC-BF16.gguf); any other filename runs the SDXL workflow.",
    );
    heading(ui, t, "Negative prompt");
    TextField::multiline("generation:negative", &mut g.negative).rows(2).width(f32::INFINITY).show(ui);
    hint(ui, t, "Used by SDXL. The Qwen preset ignores it.");
    heading(ui, t, "Qwen instruction");
    hint(ui, t, "What Qwen receives: {prompt} is replaced by the prompt you type in the Masking panel.");
    TextField::multiline("generation:qwenInstruction", &mut g.qwen_instruction).rows(5).width(f32::INFINITY).show(ui);
    if !g.qwen_instruction.contains(PROMPT_PLACEHOLDER) {
        ui.colored_label(ui.visuals().error_fg_color, tr("The instruction must contain {prompt}."));
    }
    let r = crate::button::Button::new("Reset to default")
        .small()
        .enabled(g.qwen_instruction != DEFAULT_QWEN_INSTRUCTION)
        .widget("generation:resetInstruction")
        .show(ui);
    if r.clicked() {
        g.qwen_instruction = DEFAULT_QWEN_INSTRUCTION.into();
    }
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
