//! Settings ▸ Slideshow: the slide's Layout, Options and Backdrop sections (Lightroom Classic's
//! Slideshow panels of the same names) and a small live preview of the composed slide (with the
//! overlays of [`super::slide_overlays`]).
//!
//! Every control goes through `slideshow.layout` (validated there, saved with the library).
//! Sliders change the layout in memory while dragged (the preview follows) and save it when let
//! go. The preview is the active photo's slide rendered off the UI thread ([`Slot::SlidePreview`])
//! by the same composer playback uses; it is requested once the layout has been still for
//! [`DEBOUNCE`] seconds, so a drag doesn't queue a render per frame.

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use lightcraft_engine::slide::{Aspect, SlideSettings};
use lightcraft_engine::slide_template::SlideLook;
use lightcraft_slideshow::{MARGIN_MAX, SHADOW_OFFSET_MAX, SHADOW_RADIUS_MAX, STROKE_MAX};
use serde_json::{Value, json};

use super::settings::{check, choices, heading, hint, row};
use crate::LightcraftApp;
use crate::render::Slot;
use crate::theme::Tokens;
use crate::widgets::register;

/// Seconds the layout must stay unchanged before the preview is rendered again.
pub const DEBOUNCE: f64 = 0.15;
/// The preview's greatest width in points (less where the room is narrower).
pub const PREVIEW_W: f32 = 300.0;
/// The least tall the preview is drawn.
pub const PREVIEW_MIN_H: f32 = 80.0;

#[derive(Clone, Copy)]
enum Unit {
    /// A fraction shown as a percentage (of the slide's shorter side, or an opacity).
    Percent,
    Degrees,
}

fn set(app: &mut LightcraftApp, p: Value) {
    let _ = app.run("slideshow.layout", p);
}

/// A layout slider (`slider:{id}`): `key`'s value, 0–`max` (percent) or −180–180° (degrees).
fn slider(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, key: &str, value: f64, max: f64, unit: Unit) {
    let (mut v, range, suffix) = match unit {
        Unit::Percent => (value * 100.0, 0.0..=max * 100.0, " %"),
        Unit::Degrees => (value, -180.0..=180.0, "°"),
    };
    let r = ui.add(egui::Slider::new(&mut v, range).suffix(suffix).max_decimals(1));
    register(ui.ctx(), format!("slider:{id}"), r.rect);
    let value = match unit {
        Unit::Percent => v / 100.0,
        Unit::Degrees => v,
    };
    if r.changed()
        && let Ok(next) = app.session.slide_layout.merged(&json!({ key: value }))
    {
        app.session.slide_layout = next;
    }
    if r.drag_stopped() || (r.changed() && !r.dragged()) {
        set(app, json!({ key: value }));
    }
}

/// A colour button (`button:{id}`) for `key`.
fn colour(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, key: &str, value: [u8; 3]) {
    let mut c = value;
    let r = egui::color_picker::color_edit_button_srgb(ui, &mut c);
    register(ui.ctx(), format!("button:{id}"), r.rect);
    if r.changed() {
        set(app, json!({ key: c }));
    }
}

/// A checkbox (`check:settings.{id}`) for `key`; true when it is on.
fn toggle(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, key: &str, value: bool, label: &str) -> bool {
    let mut v = value;
    if check(ui, &format!("settings.{id}"), &mut v, label) {
        set(app, json!({ key: v }));
    }
    v
}

/// The Layout, Options and Backdrop sections.
pub fn sections(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let s = app.session.slide_layout.clone();

    heading(ui, t, "Layout");
    let linked = toggle(app, ui, "slideshowLinkMargins", "linkMargins", s.link_margins, "Link all margins");
    if linked {
        row(ui, t, "Margins", |ui| slider(app, ui, "slideshowMargins", "marginLeft", s.margin_left, MARGIN_MAX, Unit::Percent));
    } else {
        for (id, key, label, v) in [
            ("slideshowMarginLeft", "marginLeft", "Left", s.margin_left),
            ("slideshowMarginRight", "marginRight", "Right", s.margin_right),
            ("slideshowMarginTop", "marginTop", "Top", s.margin_top),
            ("slideshowMarginBottom", "marginBottom", "Bottom", s.margin_bottom),
        ] {
            row(ui, t, label, |ui| slider(app, ui, id, key, v, MARGIN_MAX, Unit::Percent));
        }
    }
    hint(ui, t, "Margins are a share of the slide's shorter side, so slides look the same on any screen.");
    toggle(app, ui, "slideshowGuides", "showGuides", s.show_guides, "Show guides");
    row(ui, t, "Aspect preview", |ui| {
        let mut a = s.aspect;
        let opts = [(Aspect::Screen, "Screen"), (Aspect::Wide, "16:9"), (Aspect::Standard, "4:3")];
        if choices(ui, "settingsSlideshowAspect", &opts, &mut a) {
            set(app, json!({"aspect": a.id()}));
        }
    });

    ui.add_space(6.0);
    heading(ui, t, "Options");
    toggle(app, ui, "slideshowZoomToFill", "zoomToFill", s.zoom_to_fill, "Zoom to fill frame");
    if toggle(app, ui, "slideshowStroke", "strokeBorder", s.stroke_border, "Stroke border") {
        row(ui, t, "Colour", |ui| colour(app, ui, "slideshowStrokeColor", "strokeColor", s.stroke_color));
        row(ui, t, "Width", |ui| slider(app, ui, "slideshowStrokeWidth", "strokeWidth", s.stroke_width, STROKE_MAX, Unit::Percent));
    }
    if toggle(app, ui, "slideshowShadow", "castShadow", s.cast_shadow, "Cast shadow") {
        row(ui, t, "Opacity", |ui| slider(app, ui, "slideshowShadowOpacity", "shadowOpacity", s.shadow_opacity, 1.0, Unit::Percent));
        row(ui, t, "Offset", |ui| slider(app, ui, "slideshowShadowOffset", "shadowOffset", s.shadow_offset, SHADOW_OFFSET_MAX, Unit::Percent));
        row(ui, t, "Radius", |ui| slider(app, ui, "slideshowShadowRadius", "shadowRadius", s.shadow_radius, SHADOW_RADIUS_MAX, Unit::Percent));
        row(ui, t, "Angle", |ui| slider(app, ui, "slideshowShadowAngle", "shadowAngle", s.shadow_angle, 180.0, Unit::Degrees));
    }

    ui.add_space(6.0);
    heading(ui, t, "Backdrop");
    row(ui, t, "Background colour", |ui| colour(app, ui, "slideshowBackdropColor", "backdropColor", s.backdrop_color));
    if toggle(app, ui, "slideshowWash", "colorWash", s.color_wash, "Colour wash") {
        row(ui, t, "Colour", |ui| colour(app, ui, "slideshowWashColor", "washColor", s.wash_color));
        row(ui, t, "Opacity", |ui| slider(app, ui, "slideshowWashOpacity", "washOpacity", s.wash_opacity, 1.0, Unit::Percent));
        row(ui, t, "Angle", |ui| slider(app, ui, "slideshowWashAngle", "washAngle", s.wash_angle, 180.0, Unit::Degrees));
    }
    let active = app.session.active();
    let mut on = s.background_image.is_some();
    let r = ui.add_enabled(on || active.is_some(), egui::Checkbox::new(&mut on, crate::i18n::tr("Background image")));
    register(ui.ctx(), "check:settings.slideshowBackgroundImage", r.rect);
    if r.changed() {
        set(app, json!({"backgroundImage": if on { active.map(|a| a.0) } else { None }}));
    }
    if let Some(bid) = s.background_image {
        let name = app.session.catalog.photo(lightcraft_catalog::PhotoId(bid)).map(|p| p.file_name.clone());
        row(ui, t, "Photo", |ui| {
            let text = name.unwrap_or_else(|| crate::i18n::tr("(not in the library)").to_string());
            ui.label(egui::RichText::new(text).color(t.text));
            if let Some(a) = active.filter(|a| a.0 != bid)
                && crate::widgets::text_button(ui, "slideshowUseSelected", "Use selected photo", false).clicked()
            {
                set(app, json!({"backgroundImage": a.0}));
            }
        });
        row(ui, t, "Opacity", |ui| slider(app, ui, "slideshowBackgroundOpacity", "backgroundOpacity", s.background_opacity, 1.0, Unit::Percent));
    } else {
        hint(ui, t, "Select a photo, then turn this on to fill the slide behind every photo with it.");
    }
    ui.add_space(4.0);
    if crate::widgets::text_button(ui, "slideshowResetLayout", "Reset layout", false).clicked() {
        reset(app);
    }
}

/// The aspect of the screen a slideshow plays on (the monitor, else the window).
fn screen_aspect(ctx: &egui::Context) -> f64 {
    let monitor = ctx.input(|i| i.viewport().monitor_size).filter(|m| m.x > 0.0 && m.y > 0.0);
    let r = monitor.unwrap_or_else(|| ctx.content_rect().size());
    if r.y > 0.0 { (r.x / r.y) as f64 } else { 1.6 }
}

/// The small composed slide of the active photo, with the margin guides when they are on. It is
/// at most `max_h` tall (but for [`PREVIEW_MIN_H`], and without counting its heading).
pub fn preview(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, max_h: f32) {
    heading(ui, t, "Preview");
    // the template the pointer is over in the Template Browser, else the settings
    let template = super::slide_templates::hovered(ui.ctx()).and_then(|id| app.session.slide_template(&id).ok());
    let look = template.as_ref().map_or_else(|| SlideLook::of(&app.session), |t| t.look.clone());
    let s = look.layout.clone();
    let overlays = look.overlays.clone();
    // The room for the preview is the settings' own shape, whichever template is pointed at: the
    // page below it must not move as the pointer goes over the templates (it would leave the
    // row it is over). A template of another shape is drawn as large as fits in that room.
    let ratio = s.aspect.ratio(screen_aspect(ui.ctx())) as f32;
    let own = SlideLook::of(&app.session).layout.aspect.ratio(screen_aspect(ui.ctx())) as f32;
    let w = ui.available_width().clamp(1.0, PREVIEW_W);
    let room_h = (w / own).clamp(PREVIEW_MIN_H, 320.0).min(max_h.max(PREVIEW_MIN_H));
    let (room, _) = ui.allocate_exact_size(vec2(w, room_h), Sense::hover());
    let (fit_w, fit_h) = if ratio >= w / room_h { (w, w / ratio) } else { (room_h * ratio, room_h) };
    let rect = Rect::from_center_size(room.center(), vec2(fit_w, fit_h));
    let h = rect.height();
    let w = rect.width();
    register(ui.ctx(), "slideshow:preview", rect);
    let p = ui.painter_at(room.expand(1.0));
    p.rect_stroke(rect, 0.0, Stroke::new(1.0, t.text_dim.gamma_multiply(0.5)), egui::StrokeKind::Outside);
    super::slide_titles::preview_choice(ui);
    if let Some(tpl) = &template {
        // (said over the bottom of the preview and not below it, which would move the page)
        let name = if tpl.builtin { crate::i18n::tr(&tpl.name).to_string() } else { tpl.name.clone() };
        let mut said = crate::i18n::tr_format!("Template preview: {name}", name = name).to_string();
        if !tpl.description.is_empty() {
            said = format!("{said}\n{}", tpl.description);
        }
        let galley = ui.painter().layout(said, t.font(11.0), Color32::WHITE, (room.width() - 8.0).max(1.0));
        let band = Rect::from_min_size(
            pos2(room.left(), (room.bottom() - galley.size().y - 6.0).max(room.top())),
            vec2(room.width(), galley.size().y + 6.0),
        );
        let paint = ui
            .ctx()
            .layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("settingsSlideshowTemplateSaid")))
            .with_clip_rect(room.intersect(ui.clip_rect()));
        paint.rect_filled(band, 0.0, Color32::from_black_alpha(190));
        paint.galley(pos2(band.left() + 4.0, band.top() + 3.0), galley, Color32::WHITE);
    }
    if let Some(title) = super::slide_titles::preview_shows(ui.ctx()) {
        title_preview(app, ui, t, rect, title, &look);
        return;
    }
    let Some(id) = app.session.active() else {
        p.rect_filled(rect, 0.0, Color32::from_rgb(s.backdrop_color[0], s.backdrop_color[1], s.backdrop_color[2]));
        p.text(rect.center(), Align2::CENTER_CENTER, crate::i18n::tr("Select a photo to preview its slide"), t.font(11.5), Color32::from_gray(200));
        return;
    };
    let ppp = ui.ctx().pixels_per_point();
    let (pw, ph) = (((w * ppp).round() as usize).max(16), ((h * ppp).round() as usize).max(16));
    // the overlays and what they show of the photo (its rating, its metadata) change it too
    let shown = app.session.catalog.photo(id).filter(|_| overlays.any()).map_or(0, |p| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        p.rating.hash(&mut h);
        serde_json::to_string(&p.meta).unwrap_or_default().hash(&mut h);
        h.finish()
    });
    let overlay_key = if overlays.any() { overlays.pixel_key() ^ shown.rotate_left(5) } else { 0 };
    let key = s.pixel_key() ^ id.0.rotate_left(17) ^ ((pw as u64) << 32 | ph as u64) ^ overlay_key.rotate_left(29);
    let now = ui.input(|i| i.time);
    let mem = egui::Id::new("settingsSlideshowPreviewKey");
    let (last, since) = ui.ctx().data(|d| d.get_temp::<(u64, f64)>(mem)).unwrap_or((0, f64::NEG_INFINITY));
    let current = app.renderer.textures.get(&Slot::SlidePreview).filter(|tex| tex.photo == id);
    if last != key {
        ui.ctx().data_mut(|d| d.insert_temp(mem, (key, now)));
    }
    let still = if last == key { now - since } else { 0.0 };
    // the first preview (or another photo's) at once, a changing layout once it has settled
    if current.is_none() || still >= DEBOUNCE {
        let side = super::detail::texture_side(ui.ctx());
        let edge = app.ui.settings.loupe_edge(pw.max(ph) as f32, usize::MAX, side);
        if let Some(job) = app.session.slide_job_look(&s, &overlays, id, pw, ph, Some(edge), false) {
            app.renderer.request(Slot::SlidePreview, job, 90);
        }
    } else {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64((DEBOUNCE - still).clamp(0.01, DEBOUNCE)));
    }
    let Some(tex) = app.renderer.textures.get(&Slot::SlidePreview).filter(|tex| tex.photo == id) else {
        p.rect_filled(rect, 0.0, Color32::from_rgb(s.backdrop_color[0], s.backdrop_color[1], s.backdrop_color[2]));
        p.text(rect.center(), Align2::CENTER_CENTER, crate::i18n::tr("Rendering…"), t.font(11.5), Color32::from_gray(200));
        return;
    };
    let drawn = super::detail::fit_texture_rect(rect, tex.size);
    p.image(tex.tex.id(), drawn, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    if s.show_guides {
        // the margins, as lines across the slide (where the image cell's edges are)
        let aspect = app.session.slide_photo_aspect(id).unwrap_or(1.5);
        let l = lightcraft_slideshow::layout(&s, aspect, tex.size[0], tex.size[1]);
        let (kx, ky) = (drawn.width() / tex.size[0].max(1) as f32, drawn.height() / tex.size[1].max(1) as f32);
        let stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(120, 170, 255, 200));
        let x = |v: f64| drawn.left() + v as f32 * kx;
        let y = |v: f64| drawn.top() + v as f32 * ky;
        for gx in [x(l.cell.x0), x(l.cell.x1)] {
            p.line_segment([pos2(gx, drawn.top()), pos2(gx, drawn.bottom())], stroke);
        }
        for gy in [y(l.cell.y0), y(l.cell.y1)] {
            p.line_segment([pos2(drawn.left(), gy), pos2(drawn.right(), gy)], stroke);
        }
    }
    hint(ui, t, "The selected photo as a slide. Playback fills the screen; the aspect preview only changes this preview.");
}

/// The preview showing title screen `title` (composed off the UI thread like the photo's slide,
/// once its settings have been still for [`DEBOUNCE`] seconds).
fn title_preview(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, rect: Rect, title: lightcraft_engine::slideshow::Title, look: &SlideLook) {
    let ppp = ui.ctx().pixels_per_point();
    let (pw, ph) = (((rect.width() * ppp).round() as usize).max(16), ((rect.height() * ppp).round() as usize).max(16));
    let compose = std::sync::Arc::new(lightcraft_engine::slide_title::TitleCompose::new(&look.titles, &look.overlays, title, pw, ph));
    let key = compose.key();
    let now = ui.input(|i| i.time);
    let mem = egui::Id::new("settingsSlideshowTitlePreviewKey");
    let (last, since) = ui.ctx().data(|d| d.get_temp::<(u64, f64)>(mem)).unwrap_or((0, f64::NEG_INFINITY));
    if last != key {
        ui.ctx().data_mut(|d| d.insert_temp(mem, (key, now)));
    }
    let still = if last == key { now - since } else { 0.0 };
    // a title render in the slot (a photo's has its id): shown until the new one lands
    let current = app.renderer.textures.get(&Slot::SlidePreview).filter(|tex| tex.photo == lightcraft_catalog::PhotoId(0));
    if current.is_none() || still >= DEBOUNCE {
        app.renderer.request_title(Slot::SlidePreview, compose, 90);
    } else {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64((DEBOUNCE - still).clamp(0.01, DEBOUNCE)));
    }
    let p = ui.painter_at(rect.expand(1.0));
    match app.renderer.textures.get(&Slot::SlidePreview).filter(|tex| tex.photo == lightcraft_catalog::PhotoId(0)) {
        Some(tex) => {
            let drawn = super::detail::fit_texture_rect(rect, tex.size);
            p.image(tex.tex.id(), drawn, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            let c = look.titles.screen(title).color;
            p.rect_filled(rect, 0.0, Color32::from_rgb(c[0], c[1], c[2]));
            p.text(rect.center(), Align2::CENTER_CENTER, crate::i18n::tr("Rendering…"), t.font(11.5), Color32::from_gray(200));
        }
    }
    let hint_text = match (title, look.titles.screen(title).enabled) {
        (_, false) => "This screen is off: turn it on under Titles to play it.",
        (lightcraft_engine::slideshow::Title::Intro, true) => "The intro screen, before the first photo.",
        (lightcraft_engine::slideshow::Title::Ending, true) => "The ending screen, after the last photo.",
    };
    hint(ui, t, hint_text);
}

/// The default layout (for the Reset button).
pub fn reset(app: &mut LightcraftApp) {
    let d = SlideSettings::default().to_json();
    set(app, d);
}
