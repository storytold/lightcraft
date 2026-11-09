//! The selected mask's Replace / Remove controls and the explicit accept/discard step for ComfyUI
//! results, and the ComfyUI page of Settings (server, model, negative prompt, instructions).
use serde_json::json;

use lightcraft_engine::comfyui::{
    CONTEXT_RANGE, DEFAULT_QWEN_INSTRUCTION, DEFAULT_REFINE_INSTRUCTION, DEFAULT_REMOVAL_INSTRUCTION, DETAIL_MAX, GROW_MAX, Mode, PROMPT_PLACEHOLDER,
};

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
        let expanded = !app.ui.generation.prompt.trim().is_empty() || app.ui.generation.mode == Mode::Remove;
        let section = egui::CollapsingHeader::new(tr("Generate with ComfyUI")).id_salt("generation-section").default_open(expanded).show(ui, |ui| {
            ui.add_space(4.0);
            let running = app.session.generation.as_ref().is_some_and(|g| g.running());
            let pending = app.session.generation.as_ref().is_some_and(|g| g.preview().is_some());
            ui.add_enabled_ui(!running && !pending, |ui| {
                let modes = [("Replace", "replace"), ("Remove", "remove")];
                let active = Some(usize::from(app.ui.generation.mode == Mode::Remove));
                if let Some(i) = crate::widgets::segmented(ui, "generationMode", &modes, active, 2) {
                    app.ui.generation.mode = if i == 1 { Mode::Remove } else { Mode::Replace };
                }
                ui.add_space(4.0);
                let remove = app.ui.generation.mode == Mode::Remove;
                let hint = if remove { tr("What to remove (empty: the mask's name)") } else { tr("Describe what should replace the masked area") };
                if remove {
                    TextField::multiline("generation:subject", &mut app.ui.generation.subject).hint(hint).rows(2).width(f32::INFINITY).show(ui);
                } else {
                    TextField::multiline("generation:prompt", &mut app.ui.generation.prompt).hint(hint).rows(3).width(f32::INFINITY).show(ui);
                }
                if remove {
                    removal_controls(app, ui);
                }
                ui.horizontal(|ui| {
                    ui.label(tr("Seed"));
                    let r = ui.add(egui::DragValue::new(&mut app.ui.generation.seed).range(0..=9_007_199_254_740_991_u64));
                    register(ui, "generation:seed", r.rect);
                });
                if remove || !app.ui.generation.prompt.trim().is_empty() {
                    let sent = if remove {
                        // What Generate will send: the typed subject, else the selected mask's name.
                        let name = selected_mask_name(app).unwrap_or_default();
                        match app.ui.generation.removal_subject(&name) {
                            Ok(subject) => lightcraft_engine::comfyui::Settings { prompt: subject, ..app.ui.generation.clone() }.resolved_prompt(),
                            Err(e) => e,
                        }
                    } else {
                        app.ui.generation.resolved_prompt()
                    };
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
                ui.small(if remove {
                    tr("Remove sends a wide region around the mask, then full-resolution detail tiles, to your ComfyUI server.")
                } else {
                    tr("Generate sends the selected region and mask to your ComfyUI server.")
                });
                let ready = remove || !app.ui.generation.prompt.trim().is_empty();
                let r = ui.add_enabled(ready, egui::Button::new(tr(if remove { "Remove" } else { "Generate" })));
                register(ui, "generation:generate", r.rect);
                // Why Generate/Remove did not start shows right here, not only in the status bar.
                let refused = egui::Id::unique("generation-refused");
                if r.clicked() {
                    let result = serde_json::to_value(&app.ui.generation).map_err(|e| e.to_string()).and_then(|p| app.run("mask.generate", p));
                    ui.ctx().data_mut(|d| match result {
                        Ok(_) => d.remove::<String>(refused),
                        Err(e) => {
                            d.insert_temp(refused, e);
                        }
                    });
                }
                if let Some(e) = ui.ctx().data(|d| d.get_temp::<String>(refused)) {
                    let r = ui.colored_label(ui.visuals().error_fg_color, e);
                    register(ui, "generation:refused", r.rect);
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

/// The selected mask's name on the active photo, if any.
fn selected_mask_name(app: &LightcraftApp) -> Option<String> {
    let photo = app.session.active()?;
    let id = app.session.active_mask?;
    let develop = app.session.develop_of(photo)?;
    develop.masks.iter().find(|m| m.id == id).map(|m| m.name.clone())
}

/// Remove: how much surrounding scene the coarse pass sees, how far the mask grows, and how
/// strongly the full-resolution detail pass may redraw.
fn removal_controls(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let g = &mut app.ui.generation;
    // Leave room for the label column and the value box, so nothing is cut at the panel edge.
    ui.spacing_mut().slider_width = (ui.available_width() - 150.0).max(60.0);
    egui::Grid::new("generation-removal").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
        let row = |ui: &mut egui::Ui, id: &str, label: &str, widget: egui::Slider<'_>| {
            ui.label(tr(label));
            let r = ui.add(widget);
            register(ui, id, r.rect);
            ui.end_row();
        };
        row(ui, "generation:context", "Context", egui::Slider::new(&mut g.context, CONTEXT_RANGE).step_by(0.25).fixed_decimals(2).suffix("×"));
        row(ui, "generation:grow", "Grow edge", egui::Slider::new(&mut g.grow, 0..=GROW_MAX).suffix(" px"));
        row(ui, "generation:detail", "Detail", egui::Slider::new(&mut g.detail, 0.0..=DETAIL_MAX).step_by(0.05).fixed_decimals(2));
    });
    ui.small(tr("Context: how much of the scene around the mask the model sees. Detail 0 skips the full-resolution pass."));
}

/// Settings ▸ ComfyUI: the server, model, negative prompt and the instruction Qwen receives.
/// Saved with the app settings (`ui.json`); nothing is sent until Generate.
pub fn settings_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, "ComfyUI");
    if cfg!(target_arch = "wasm32") {
        hint(ui, t, "Generate with ComfyUI needs the desktop app.");
        return;
    }
    hint(
        ui,
        t,
        "Generate with ComfyUI in the Masking panel sends the selected region and mask to this ComfyUI server. Nothing is sent until you press Generate or Remove.",
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
    heading(ui, t, "Removal instruction");
    hint(ui, t, "What Qwen receives for Remove's first pass: {prompt} is replaced by what to remove (or the mask's name).");
    TextField::multiline("generation:removalInstruction", &mut g.removal_instruction).rows(5).width(f32::INFINITY).show(ui);
    if !g.removal_instruction.contains(PROMPT_PLACEHOLDER) {
        ui.colored_label(ui.visuals().error_fg_color, tr("The instruction must contain {prompt}."));
    }
    let r = crate::button::Button::new("Reset to default")
        .small()
        .enabled(g.removal_instruction != DEFAULT_REMOVAL_INSTRUCTION)
        .widget("generation:resetRemovalInstruction")
        .show(ui);
    if r.clicked() {
        g.removal_instruction = DEFAULT_REMOVAL_INSTRUCTION.into();
    }
    heading(ui, t, "Removal detail instruction");
    hint(ui, t, "What Qwen receives for each full-resolution detail tile after the first pass.");
    TextField::multiline("generation:refineInstruction", &mut g.refine_instruction).rows(4).width(f32::INFINITY).show(ui);
    let r = crate::button::Button::new("Reset to default")
        .small()
        .enabled(g.refine_instruction != DEFAULT_REFINE_INSTRUCTION)
        .widget("generation:resetRefineInstruction")
        .show(ui);
    if r.clicked() {
        g.refine_instruction = DEFAULT_REFINE_INSTRUCTION.into();
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
    let removal = run.settings.mode == Mode::Remove;
    egui::Window::new(tr(if removal { "Generated removal" } else { "Generated replacement" }))
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
            if removal {
                ui.label(tr("The masked area was removed and the background rebuilt."));
            } else {
                ui.label(&run.settings.prompt);
            }
            ui.small(tr("Accept saves a new image stacked with the original. Your original stays unchanged."));
            ui.horizontal_wrapped(|ui| {
                let r = ui.button(tr(if removal { "Accept removal" } else { "Accept replacement" }));
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
