//! Settings ▸ Slideshow ▸ Templates: the Template Browser (Lightroom Classic's, at the top of the
//! Slideshow tab until the Slideshow module view exists). Built-in templates (our own designs) and
//! the user's (files in `<settings>/slideshow-templates`), and "Previous settings" once one has been
//! applied (library preferences have no undo).
//!
//! Clicking a template applies it (`slideshow.templates.apply`); hovering one shows it on the
//! selected photo in the live preview beside it ([`hovered`], read by
//! [`super::slide_layout::preview`]) without changing anything. Right-click a user template to
//! update it with the current settings, rename, export or delete it; a built-in to export it.
//! Every action goes through a `slideshow.templates.*` command.

use egui::{RichText, Sense, Stroke, vec2};
use lightcraft_engine::slide_template::{PREVIOUS, SlideLook, Template};
use serde_json::json;

use crate::LightcraftApp;
use crate::panels::settings::{heading, hint};
use crate::text_field::TextField;
use crate::theme::Tokens;
use crate::widgets::{register, register_response};

/// The user templates list is at most this tall (it scrolls past it).
const USER_LIST_H: f32 = 180.0;

fn hovered_key() -> egui::Id {
    egui::Id::unique("settingsSlideshowTemplateHovered")
}

/// The template the pointer is over in the browser (its id), which the preview shows.
pub fn hovered(ctx: &egui::Context) -> Option<String> {
    ctx.data(|d| d.get_temp::<Option<String>>(hovered_key())).flatten()
}

fn set_hovered(ctx: &egui::Context, id: Option<String>) {
    ctx.data_mut(|d| d.insert_temp(hovered_key(), id));
}

/// What a row is doing: being renamed (with the text so far), or waiting for a delete to be confirmed.
#[derive(Clone, Debug, PartialEq)]
enum Pending {
    Rename(String, String),
    Delete(String),
}

fn pending_key() -> egui::Id {
    egui::Id::unique("settingsSlideshowTemplatePending")
}

fn pending(ctx: &egui::Context) -> Option<Pending> {
    ctx.data(|d| d.get_temp::<Option<Pending>>(pending_key())).flatten()
}

fn set_pending(ctx: &egui::Context, p: Option<Pending>) {
    ctx.data_mut(|d| d.insert_temp(pending_key(), p));
}

/// The name shown for a template: a built-in's in the interface language.
fn shown_name(t: &Template) -> String {
    if t.builtin || t.id == PREVIOUS { crate::i18n::tr(&t.name).to_string() } else { t.name.clone() }
}

/// Is `look` what the settings are now (prepare previews aside: templates don't set it)?
fn is_current(look: &SlideLook, now: &SlideLook) -> bool {
    look.layout == now.layout && look.overlays == now.overlays && look.titles == now.titles && {
        let mut p = look.playback.clone();
        p.prepare_previews = now.playback.prepare_previews;
        p == now.playback
    }
}

/// Run a template command; tell the user how it went.
fn act(app: &mut LightcraftApp, ctx: &egui::Context, cmd: &str, p: serde_json::Value, done: impl FnOnce(&serde_json::Value) -> String) {
    match app.run(cmd, p) {
        Ok(r) => {
            let mut msg = done(&r);
            let warnings: Vec<&str> = r["warnings"].as_array().map(|w| w.iter().filter_map(|w| w.as_str()).collect()).unwrap_or_default();
            if !warnings.is_empty() {
                msg = format!("{msg} · {}", warnings.join(" · "));
            }
            if !msg.is_empty() {
                app.toast(ctx, msg);
            }
        }
        Err(e) => app.toast(ctx, e),
    }
}

fn apply(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, name: &str) {
    let name = name.to_string();
    act(app, ctx, "slideshow.templates.apply", json!({"id": id}), |_| crate::i18n::tr_format!("Applied {name}", name = name));
}

/// One template row (`template:{id}`): click applies, hover previews, right-click for its menu.
fn template_row(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, tpl: &Template, current: bool, hovered: &mut Option<String>) {
    let ctx = ui.ctx().clone();
    let name = shown_name(tpl);
    if let Some(Pending::Rename(id, mut text)) = pending(&ctx).filter(|p| matches!(p, Pending::Rename(id, _) if *id == tpl.id)) {
        let r = TextField::singleline("field:slideshowTemplateRename", &mut text)
            .width(ui.available_width().clamp(80.0, 260.0))
            .select_on_focus(true)
            .show(ui);
        if !r.editing && r.ending.is_none() {
            r.response.request_focus();
        }
        if r.committed() {
            set_pending(&ctx, None);
            if text.trim() != tpl.name {
                act(app, &ctx, "slideshow.templates.rename", json!({"id": id, "name": text}), |_| String::new());
            }
        } else if r.cancelled() {
            set_pending(&ctx, None);
        } else {
            set_pending(&ctx, Some(Pending::Rename(id, text)));
        }
        return;
    }
    if pending(&ctx) == Some(Pending::Delete(tpl.id.clone())) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(crate::i18n::tr_format!("Delete “{name}”?", name = name)).color(t.caution));
            if crate::widgets::text_button(ui, "slideshowTemplateDeleteConfirm", "Delete", false).clicked() {
                set_pending(&ctx, None);
                act(app, &ctx, "slideshow.templates.delete", json!({"id": tpl.id}), |_| String::new());
            }
            if crate::widgets::text_button(ui, "slideshowTemplateDeleteCancel", "Cancel", false).clicked() {
                set_pending(&ctx, None);
            }
        });
        return;
    }
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 22.0), Sense::click());
    register_response(format!("template:{}", tpl.id), rect, &resp);
    if resp.hovered() || resp.context_menu_opened() {
        *hovered = Some(tpl.id.clone());
    }
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 3.0, t.hover);
    }
    if current {
        p.rect_stroke(rect.shrink(0.5), 3.0, Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
    }
    let colour = if current { t.text } else { t.text_label };
    p.text(rect.left_center() + vec2(8.0, 0.0), egui::Align2::LEFT_CENTER, &name, t.font(12.0), colour);
    if resp.clicked() {
        apply(app, &ctx, &tpl.id, &name);
    }
    crate::widgets::context_menu(&resp, |ui| menu(app, ui, tpl, &name));
}

/// A menu entry registered as `button:{id}`.
fn item(ui: &mut egui::Ui, id: &str, label: &str, enabled: bool) -> bool {
    let r = ui.add_enabled(enabled, egui::Button::new(crate::i18n::tr(label)));
    register(ui, format!("button:{id}"), r.rect);
    r.clicked()
}

fn menu(app: &mut LightcraftApp, ui: &mut egui::Ui, tpl: &Template, name: &str) {
    let ctx = ui.ctx().clone();
    if item(ui, "slideshowTemplateApply", "Apply", true) {
        apply(app, &ctx, &tpl.id, name);
        ui.close();
    }
    let user = !tpl.builtin && tpl.id != PREVIOUS;
    if user {
        if item(ui, "slideshowTemplateUpdate", "Update with Current Settings", true) {
            let n = name.to_string();
            act(app, &ctx, "slideshow.templates.update", json!({"id": tpl.id}), |_| crate::i18n::tr_format!("Updated {name}", name = n));
            ui.close();
        }
        if item(ui, "slideshowTemplateRename", "Rename…", true) {
            set_pending(&ctx, Some(Pending::Rename(tpl.id.clone(), tpl.name.clone())));
            ui.close();
        }
    }
    if tpl.id != PREVIOUS && item(ui, "slideshowTemplateExport", "Export…", app.services.save_slideshow_template.is_some()) {
        let file = format!("{}.json", lightcraft_engine::slide_template::file_stem(&tpl.name).unwrap_or_else(|| "Slideshow Template".into()));
        if let Some(path) = app.services.save_slideshow_template.as_mut().and_then(|save| save(&file)) {
            act(app, &ctx, "slideshow.templates.export", json!({"id": tpl.id, "path": path}), |r| {
                crate::i18n::tr_format!("Exported to {path}", path = r["path"].as_str().unwrap_or_default())
            });
        }
        ui.close();
    }
    if user {
        ui.separator();
        if item(ui, "slideshowTemplateDelete", "Delete", true) {
            set_pending(&ctx, Some(Pending::Delete(tpl.id.clone())));
            ui.close();
        }
    }
}

/// A group heading inside the browser.
fn group(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    ui.add_space(2.0);
    ui.label(RichText::new(crate::i18n::tr(text)).size(11.0).color(t.text_dim));
}

/// The Templates section.
pub fn section(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let ctx = ui.ctx().clone();
    heading(ui, t, "Templates");
    let listing = app.session.slide_templates();
    let now = SlideLook::of(&app.session);
    let mut over: Option<String> = None;
    group(ui, t, "Built-in");
    for tpl in listing.templates.iter().filter(|t| t.builtin) {
        template_row(app, ui, t, tpl, is_current(&tpl.look, &now), &mut over);
    }
    group(ui, t, "Your templates");
    let user: Vec<_> = listing.templates.iter().filter(|t| !t.builtin).cloned().collect();
    if user.is_empty() {
        hint(ui, t, "None yet: save the current settings as a template below.");
    } else {
        egui::ScrollArea::vertical().id_salt("settingsSlideshowTemplatesUser").max_height(USER_LIST_H).auto_shrink([false, true]).show(ui, |ui| {
            for tpl in &user {
                template_row(app, ui, t, tpl, is_current(&tpl.look, &now), &mut over);
            }
        });
    }
    if let Ok(prev) = app.session.slide_template(PREVIOUS) {
        group(ui, t, "Before the last template");
        template_row(app, ui, t, &prev, false, &mut over);
    }
    set_hovered(&ctx, over);

    // save the current settings as a new template
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let mem = egui::Id::unique("settingsSlideshowTemplateNewName");
        let mut name: String = ui.data(|d| d.get_temp(mem)).unwrap_or_default();
        let r = TextField::singleline("field:slideshowTemplateName", &mut name).hint(crate::i18n::tr("New template name")).width(180.0).show(ui);
        let save = crate::widgets::text_button(ui, "slideshowTemplateSave", "Save as Template", false).clicked()
            || (r.ending == Some(crate::text_field::Ending::Return) && !name.trim().is_empty());
        if save {
            let n = name.trim().to_string();
            match app.run("slideshow.templates.save", json!({"name": n})) {
                Ok(_) => {
                    app.toast(&ctx, crate::i18n::tr_format!("Saved template {name}", name = n));
                    name.clear();
                }
                Err(e) => app.toast(&ctx, e),
            }
        }
        ui.data_mut(|d| d.insert_temp(mem, name));
        if app.services.pick_slideshow_templates.is_some() && crate::widgets::text_button(ui, "slideshowTemplateImport", "Import…", false).clicked()
        {
            let paths = app.services.pick_slideshow_templates.as_mut().map(|pick| pick()).unwrap_or_default();
            for path in paths {
                act(app, &ctx, "slideshow.templates.import", json!({"path": path}), |r| {
                    crate::i18n::tr_format!("Imported {name}", name = r["name"].as_str().unwrap_or_default())
                });
            }
        }
    });
    if app.session.slide_templates_dir.is_none() {
        hint(ui, t, "Your own templates need a settings folder, which this build doesn't have; the built-in ones work.");
    } else {
        hint(
            ui,
            t,
            "Click a template to apply it; point at one to preview it on the selected photo. Right-click your own to update, rename, export or delete it.",
        );
    }
    // files that couldn't be read
    for p in listing.problems.iter().take(4) {
        ui.label(RichText::new(crate::i18n::tr_format!("Skipped {problem}", problem = p)).size(11.0).color(t.caution));
    }
    if listing.problems.len() > 4 {
        let more = listing.problems.len() - 4;
        ui.label(RichText::new(crate::i18n::tr_format!("…and {more} more", more = more)).size(11.0).color(t.caution));
    }
}
