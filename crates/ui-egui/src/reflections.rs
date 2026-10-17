//! Reflection removal in the Remove panel: native background inference, per-photo safety controls.
use crate::{LightcraftApp, i18n::tr};
use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};
const TASK: &str = "Reflection Removal";
const MODELS: [(&str, &str); 3] = [("dsrnet-s", "DSRNet-S"), ("dsrnet-l-nature", "DSRNet-L (Nature)"), ("dsrnet-l", "DSRNet-L")];

pub fn command(app: &mut LightcraftApp, name: &str, p: &Value) -> Result<Value, String> {
    if name == "reflection.instructions" {
        let open = app.services.open_url.as_mut().ok_or("No browser service available")?;
        open("https://github.com/mingcv/DSRNet#trained-weights")?;
        return Ok(json!({"opened":true}));
    }
    if name == "reflection.install" {
        return install(app, p);
    }
    if name == "reflection.models" {
        return Ok(
            json!({"models":MODELS.iter().map(|(id,label)|json!({"id":id,"label":label,"installed":lightcraft_engine::reflections::available_model(id)})).collect::<Vec<_>>() }),
        );
    }
    let id = app.session.active().ok_or("No photo selected")?;
    match name {
        "reflection.panel" => {
            app.reflection_open = p.get("open").and_then(Value::as_bool).unwrap_or(!app.reflection_open);
            Ok(json!({"open":app.reflection_open}))
        }
        "reflection.toggle" => {
            if app.tasks.is_running(TASK) {
                return Err(tr("Wait for reflection removal to finish").into());
            }
            let mut d = (*app.session.develop_of(id).ok_or("Photo missing")?).clone();
            let on = p.get("enabled").and_then(Value::as_bool).unwrap_or(!d.reflection.enabled);
            if on
                && (d.reflection.correction.is_empty()
                    || p.get("quality").and_then(Value::as_u64).is_some_and(|q| q != d.reflection.quality as u64)
                    || p.get("model").and_then(Value::as_str).is_some_and(|m| m != d.reflection.model))
            {
                return start(app, id, p);
            }
            d.reflection.enabled = on;
            app.session.set_develop(id, d, "Reflection visibility").map_err(|e| e.to_string())?;
            Ok(json!({"enabled":on}))
        }
        "reflection.reset" => {
            if app.tasks.is_running(TASK) {
                return Err(tr("Wait for reflection removal to finish").into());
            }
            let mut d = (*app.session.develop_of(id).ok_or("Photo missing")?).clone();
            d.reflection = Default::default();
            app.session.set_develop(id, d, "Reset Reflection Removal").map_err(|e| e.to_string())?;
            Ok(json!({"reset":true}))
        }
        "reflection.status" => {
            let d = app.session.develop_of(id).ok_or("Photo missing")?;
            Ok(
                json!({"running":app.tasks.is_running(TASK),"ready":!d.reflection.correction.is_empty(),"settings":d.reflection,"amount":d.reflection.amount,"quality":d.reflection.quality,"enabled":d.reflection.enabled}),
            )
        }
        "reflection.apply" | "photo.removeReflections" => start(app, id, p),
        _ => Err("Unknown reflection command".into()),
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn install(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    use crate::pick::{PickRequest, Picked};
    let path = match p.get("path").and_then(Value::as_str) {
        Some(path) => path.to_string(),
        None => match crate::pick::ask(app, "reflection.install", p, "path", PickRequest::folder(tr("Install Reflection Model…")), |s| {
            s.pick_folder.as_mut().and_then(|f| f().map(|p| vec![p]))
        }) {
            Picked::Now(paths) => match paths.into_iter().next() {
                Some(p) => p,
                None => return Ok(Value::Null),
            },
            Picked::Later | Picked::Unavailable => return Ok(Value::Null),
        },
    };
    if app.tasks.is_running(TASK) {
        return Err(tr("Wait for reflection removal to finish").into());
    }
    let root = lightcraft_engine::reflections::models_dir().ok_or("No model directory")?;
    crate::tasks::spawn(
        app,
        TASK,
        Some("reflection"),
        move || -> Result<String, String> {
            let source = std::path::Path::new(&path);
            let (m, bytes) = lightcraft_reflection_install(source)?;
            let target = root.join(&m);
            if target.exists() {
                return Err("Reflection model already installed".into());
            }
            std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
            // Exclusive directory creation; a failure never replaces another installation.
            std::fs::create_dir(&target).map_err(|e| e.to_string())?;
            let result = (|| {
                std::fs::write(target.join("model.safetensors"), bytes).map_err(|e| e.to_string())?;
                std::fs::copy(source.join("manifest.json"), target.join("manifest.json")).map_err(|e| e.to_string())?;
                Ok(m)
            })();
            if result.is_err() {
                let _ = std::fs::remove_dir_all(target);
            }
            result
        },
        |app, ctx, result| match result {
            Ok(model) => {
                app.reflection_model = model;
                app.toast(ctx, tr("Reflection model installed"));
            }
            Err(e) => app.toast(ctx, format!("{}: {e}", tr("Reflection removal failed"))),
        },
    )?;
    Ok(json!({"background":true}))
}
#[cfg(not(target_arch = "wasm32"))]
fn lightcraft_reflection_install(path: &std::path::Path) -> Result<(String, Vec<u8>), String> {
    lightcraft_engine::reflections::install_check(path)
}
#[cfg(target_arch = "wasm32")]
fn install(_app: &mut LightcraftApp, _p: &Value) -> Result<Value, String> {
    Err(tr("Reflection removal requires the desktop app").into())
}

#[cfg(not(target_arch = "wasm32"))]
fn start(app: &mut LightcraftApp, id: PhotoId, p: &Value) -> Result<Value, String> {
    if app.tasks.is_running(TASK) {
        return Err(tr("Wait for reflection removal to finish").into());
    }
    let model = p.get("model").and_then(Value::as_str).unwrap_or(&app.reflection_model).to_string();
    if !lightcraft_engine::reflections::available_model(&model) {
        return Err(tr("Install a reflection model first").into());
    }
    let edge = p.get("quality").and_then(Value::as_u64).unwrap_or(app.reflection_quality as u64);
    if !matches!(edge, 384 | 768 | 1024) {
        return Err("Invalid reflection quality".into());
    }
    let job = app.session.render_job(id, edge as usize, edge as usize, false, false).ok_or("Photo cannot be loaded")?;
    app.reflection_model = model.clone();
    app.reflection_quality = edge as u32;
    let generation = app.session.library_generation();
    let key = job.source_key;
    let identity = lightcraft_engine::reflections::identity(key);
    crate::tasks::spawn(
        app,
        TASK,
        Some("reflection"),
        move || lightcraft_engine::reflections::make(job.source, key, edge as u32, &model),
        move |app, ctx, result| {
            if generation != app.session.library_generation() {
                return;
            }
            match result {
                Ok(mut correction) => {
                    let current = app.session.render_job(id, 384, 384, false, false).map(|j| lightcraft_engine::reflections::identity(j.source_key));
                    if current.as_deref() != Some(&identity) {
                        app.toast(ctx, tr("The photo changed; calculate reflections again"));
                        return;
                    }
                    let Some(d) = app.session.develop_of(id) else {
                        return;
                    };
                    let mut d = (*d).clone();
                    // Recalculation keeps the user's chosen strength and protections.
                    if !d.reflection.correction.is_empty() && !d.reflection.model.is_empty() {
                        correction.amount = d.reflection.amount;
                        correction.limit_region = d.reflection.limit_region;
                        correction.left = d.reflection.left;
                        correction.top = d.reflection.top;
                        correction.right = d.reflection.right;
                        correction.bottom = d.reflection.bottom;
                        correction.protect_sky = d.reflection.protect_sky;
                        correction.detail_protection = d.reflection.detail_protection;
                    }
                    d.reflection = correction;
                    match app.session.set_develop(id, d, "Remove Reflections") {
                        Ok(()) => app.toast(ctx, tr("Reflections calculated; adjust Amount on the right")),
                        Err(e) => app.toast(ctx, e.to_string()),
                    }
                }
                Err(e) => app.toast(ctx, format!("{}: {e}", tr("Reflection removal failed"))),
            }
            ctx.request_repaint();
        },
    )?;
    Ok(json!({"background":true,"photo":id.0,"quality":edge}))
}
#[cfg(target_arch = "wasm32")]
fn start(_app: &mut LightcraftApp, _id: PhotoId, _p: &Value) -> Result<Value, String> {
    Err(tr("Reflection removal requires the desktop app").into())
}

pub fn panel(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    use crate::widgets::register;
    crate::widgets::divider(ui);
    crate::panels::right::header(ui, "Distraction Removal");
    if crate::widgets::flyout_row(ui, "reflections", "Reflections", crate::icons::Icon::Reflection, app.reflection_open).clicked() {
        let _ = app.run("reflection.panel", json!({}));
    }
    if !app.reflection_open {
        return;
    }
    let busy = app.tasks.is_running(TASK);
    let d = app.session.develop_of(id).unwrap_or_default();
    let ready = !d.reflection.correction.is_empty();
    if app.reflection_photo != Some(id) {
        app.reflection_photo = Some(id);
        app.reflection_quality = if matches!(d.reflection.quality, 384 | 768 | 1024) { d.reflection.quality } else { 768 };
        app.reflection_model = if lightcraft_engine::reflections::valid_model(&d.reflection.model) {
            d.reflection.model.clone()
        } else if lightcraft_engine::reflections::available_model("dsrnet-l-nature") {
            "dsrnet-l-nature".into()
        } else {
            "dsrnet-s".into()
        };
    }
    let available = lightcraft_engine::reflections::available_model(&app.reflection_model);
    let pad = egui::Margin { left: 24, right: 22, top: 4, bottom: 8 };
    egui::Frame::NONE.inner_margin(pad).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        let mut on = d.reflection.enabled;
        let r = ui.add_enabled(!busy && (available || on), egui::Checkbox::new(&mut on, tr("Apply")));
        register(ui.ctx(), "reflection:enabled", r.rect);
        register(ui.ctx(), "reflection:apply", r.rect);
        if r.changed()
            && let Err(e) = app.run("reflection.toggle", json!({"enabled":on,"quality":app.reflection_quality,"model":app.reflection_model}))
        {
            app.toast(ui.ctx(), e);
        }
        ui.horizontal(|ui| {
            ui.add_sized([58.0, 22.0], egui::Label::new(tr("Model")));
            let label = MODELS.iter().find(|(id, _)| *id == app.reflection_model).map(|(_, name)| *name).unwrap_or("DSRNet-S");
            let r = egui::ComboBox::from_id_salt("reflection-model").selected_text(label).show_ui(ui, |ui| {
                for (id, name) in MODELS {
                    let r = ui.selectable_value(&mut app.reflection_model, id.into(), name);
                    register(ui.ctx(), format!("reflection:model:{id}"), r.rect);
                }
            });
            register(ui.ctx(), "reflection:model", r.response.rect);
        });
        ui.horizontal(|ui| {
            ui.add_sized([58.0, 22.0], egui::Label::new(tr("Quality")));
            let name = match app.reflection_quality {
                384 => "Preview",
                1024 => "High",
                _ => "Standard",
            };
            let r = egui::ComboBox::from_id_salt("reflection-quality").selected_text(tr(name)).show_ui(ui, |ui| {
                for (q, name) in [(384, "Preview"), (768, "Standard"), (1024, "High")] {
                    let r = ui.selectable_value(&mut app.reflection_quality, q, tr(name));
                    register(ui.ctx(), format!("reflection:quality:{q}"), r.rect);
                }
            });
            register(ui.ctx(), "reflection:quality", r.response.rect);
        });
        if !available {
            ui.add(egui::Label::new(tr("Install a reflection model first")).wrap());
            let r = ui.add_enabled(!busy, egui::Button::new(tr("Install Reflection Model…")));
            register(ui.ctx(), "reflection:install", r.rect);
            if r.clicked() {
                let _ = app.run("reflection.install", json!({}));
            }
            let r = ui.link(tr("Model download instructions"));
            register(ui.ctx(), "reflection:instructions", r.rect);
            if r.clicked()
                && let Err(e) = app.run("reflection.instructions", json!({}))
            {
                app.toast(ui.ctx(), e);
            }
        }
        if ready {
            let r = ui.add_enabled(!busy && available, egui::Button::new(tr("Recalculate")));
            register(ui.ctx(), "reflection:recompute", r.rect);
            if r.clicked()
                && let Err(e) = app.run("reflection.apply", json!({"quality":app.reflection_quality,"model":app.reflection_model}))
            {
                app.toast(ui.ctx(), e);
            }
        }
        if busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tr("Analyzing reflections…"));
            });
        }
    });
    controls(app, ui, &d, "reflection.amount", ready && !busy);
    egui::Frame::NONE.inner_margin(pad).show(ui, |ui| {
        let mut sky = d.reflection.protect_sky;
        let r = ui.add_enabled(ready && !busy, egui::Checkbox::new(&mut sky, tr("Protect sky (estimate)")));
        register(ui.ctx(), "reflection:protectSky", r.rect);
        if r.on_hover_text(tr("Uses an embedded sky matte or an estimate. Limit the area for exact protection.")).changed() {
            let _ = app.run("develop.merge", json!({"settings":{"reflection":{"protect_sky":sky}},"label":"Reflection Sky Protection"}));
        }
    });
    controls(app, ui, &d, "reflection.detailProtection", ready && !busy);
    egui::Frame::NONE.inner_margin(pad).show(ui, |ui| {
        let mut limited = d.reflection.limit_region;
        let r = ui.add_enabled(ready && !busy, egui::Checkbox::new(&mut limited, tr("Limit to area")));
        register(ui.ctx(), "reflection:limitRegion", r.rect);
        if r.changed() {
            let _ = app.run("develop.merge", json!({"settings":{"reflection":{"limit_region":limited}},"label":"Reflection Area"}));
        }
        if limited {
            ui.add(egui::Label::new(tr("Percent of the original image, before crop or rotation. Outside this area stays unchanged.")).wrap());
        }
    });
    if d.reflection.limit_region {
        for name in ["reflection.left", "reflection.top", "reflection.right", "reflection.bottom"] {
            controls(app, ui, &d, name, ready && !busy);
        }
    }
    egui::Frame::NONE.inner_margin(pad).show(ui, |ui| {
        let r = ui.add_enabled(ready && !busy, egui::Button::new(tr("Reset")));
        register(ui.ctx(), "reflection:reset", r.rect);
        if r.clicked() {
            let _ = app.run("reflection.reset", json!({}));
        }
        ui.add_space(10.0);
        ui.add(
            egui::Label::new(
                egui::RichText::new(tr("Local AI · experimental. Start gently and compare clouds and fine details."))
                    .size(11.0)
                    .color(crate::theme::Tokens::get(ui.ctx()).text_dim),
            )
            .wrap(),
        );
    });
}
fn controls(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &lightcraft_develop::DevelopSettings, name: &str, enabled: bool) {
    crate::panels::edit::control(app, ui, d, name, enabled);
}

#[cfg(test)]
mod tests {
    #[test]
    fn reflection_messages_exist_in_every_shipped_language() {
        let messages = [
            "Distraction Removal",
            "Reflections",
            "Remove Reflections",
            "Install Reflection Model…",
            "Install a reflection model first",
            "Model download instructions",
            "Reflection model installed",
            "Analyzing reflections…",
            "Protect sky (estimate)",
            "Uses an embedded sky matte or an estimate. Limit the area for exact protection.",
            "Detail Protection",
            "Limit to area",
            "Percent of the original image, before crop or rotation. Outside this area stays unchanged.",
            "Local AI · experimental. Start gently and compare clouds and fine details.",
            "Wait for reflection removal to finish",
            "Reflection removal requires the desktop app",
            "The photo changed; calculate reflections again",
            "Reflections calculated; adjust Amount on the right",
            "Recalculate",
            "Reflection removal failed",
        ];
        for locale in crate::i18n::Locale::ALL.iter().filter(|locale| **locale != crate::i18n::Locale::En) {
            for source in messages {
                assert_ne!(locale.tr(source), source, "missing {} translation: {source}", locale.code());
            }
        }
    }
}
