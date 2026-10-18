//! Settings ▸ Slideshow ▸ Overlays (Lightroom Classic's Slideshow Overlays panel): the identity
//! plate (text or an image file), rating stars, text overlays with tokens and the watermark.
//!
//! Every control goes through `slideshow.overlays` (validated there, saved with the library), like
//! the Layout sections beside it ([`super::slide_layout`]); the live preview next to them shows the
//! overlays on the active photo's slide. Sliders change the settings in memory while dragged and
//! save them when let go; text fields save when the edit ends (Return or leaving the field).

use egui::RichText;
use lightcraft_engine::export::Anchor;
use lightcraft_engine::slide_overlay::{
    ANCHORS, OFFSET_MAX, OverlaySettings, PLATE_SIZE_MAX, Placement, RelativeTo, SHADOW_OFFSET_MAX, SHADOW_RADIUS_MAX, STAR_SIZE_MAX, TEXT_SIZE_MAX,
};
use serde_json::{Value, json};

use super::settings::{check, choices, heading, hint, row};
use crate::LightcraftApp;
use crate::text_field::TextField;
use crate::theme::Tokens;
use crate::widgets::register;

#[derive(Clone, Copy)]
enum Unit {
    /// A fraction shown as a percentage (of the slide's shorter side, or an opacity).
    Percent,
    Degrees,
}

fn set(app: &mut LightcraftApp, p: Value) {
    let _ = app.run("slideshow.overlays", p);
}

/// A slider (`slider:{id}`) over `lo..=hi`; `wrap(value)` makes the `slideshow.overlays` request.
fn slider(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, value: f64, lo: f64, hi: f64, unit: Unit, wrap: &dyn Fn(f64) -> Value) {
    let (k, suffix) = match unit {
        Unit::Percent => (100.0, " %"),
        Unit::Degrees => (1.0, "°"),
    };
    let mut v = value * k;
    let r = ui.add(egui::Slider::new(&mut v, lo * k..=hi * k).suffix(suffix).max_decimals(1));
    register(ui.ctx(), format!("slider:{id}"), r.rect);
    let p = wrap(v / k);
    if r.changed()
        && let Ok(next) = app.session.slide_overlays.merged(&p)
    {
        app.session.slide_overlays = next;
    }
    if r.drag_stopped() || (r.changed() && !r.dragged()) {
        set(app, p);
    }
}

/// A colour button (`button:{id}`).
fn colour(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, value: [u8; 3], wrap: &dyn Fn([u8; 3]) -> Value) {
    let mut c = value;
    let r = egui::color_picker::color_edit_button_srgb(ui, &mut c);
    register(ui.ctx(), format!("button:{id}"), r.rect);
    if r.changed() {
        set(app, wrap(c));
    }
}

/// A checkbox (`check:settings.{id}`); true when it is on.
fn toggle(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, value: bool, label: &str, wrap: &dyn Fn(bool) -> Value) -> bool {
    let mut v = value;
    if check(ui, &format!("settings.{id}"), &mut v, label) {
        set(app, wrap(v));
    }
    v
}

/// A one-line text field (`field:{id}`), saved when the edit ends.
fn text(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, value: &str, hint: &str, wrap: &dyn Fn(&str) -> Value) {
    let mem = egui::Id::new(("slideshow-overlay-field", id));
    let mut s: String = ui.data(|d| d.get_temp(mem)).unwrap_or_else(|| value.to_string());
    let widget = format!("field:{id}");
    let r = TextField::singleline(&widget, &mut s).hint(crate::i18n::tr(hint)).width(ui.available_width().clamp(80.0, 260.0)).show(ui);
    if r.editing {
        ui.data_mut(|d| d.insert_temp(mem, s.clone()));
    } else {
        ui.data_mut(|d| d.remove::<String>(mem));
    }
    if r.committed() && s != value {
        set(app, wrap(&s));
    }
}

/// The nine anchors as a 3 × 3 grid of buttons (`button:{id}-{0..8}`); true when one is picked.
fn anchor_grid(ui: &mut egui::Ui, id: &str, value: &mut Anchor) -> bool {
    const GLYPHS: [&str; 9] = ["↖", "↑", "↗", "←", "•", "→", "↙", "↓", "↘"];
    let mut changed = false;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
        for r in 0..3 {
            ui.horizontal(|ui| {
                for c in 0..3 {
                    let i = r * 3 + c;
                    let (Some(a), Some(g)) = (ANCHORS.get(i), GLYPHS.get(i)) else { continue };
                    if crate::widgets::text_button(ui, &format!("{id}-{i}"), g, *value == *a).clicked() && *value != *a {
                        *value = *a;
                        changed = true;
                    }
                }
            });
        }
    });
    changed
}

/// Anchor, offsets and what the overlay is anchored to; `wrap` turns placement keys into the request.
fn placement(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, id: &str, p: &Placement, photo_too: bool, wrap: &dyn Fn(Value) -> Value) {
    row(ui, t, "Position", |ui| {
        let mut a = p.anchor;
        if anchor_grid(ui, &format!("{id}Anchor"), &mut a) {
            set(app, wrap(json!({"anchor": lightcraft_engine::slide_overlay::anchor_id(a)})));
        }
    });
    if photo_too {
        row(ui, t, "Anchored to", |ui| {
            let mut r = p.relative_to;
            if choices(ui, &format!("{id}RelativeTo"), &[(RelativeTo::Slide, "Slide"), (RelativeTo::Photo, "Photo")], &mut r) {
                let v = if r == RelativeTo::Photo { "photo" } else { "slide" };
                set(app, wrap(json!({"relativeTo": v})));
            }
        });
    }
    row(ui, t, "Offset X", |ui| {
        slider(app, ui, &format!("{id}OffsetX"), p.offset_x, -OFFSET_MAX / 2.0, OFFSET_MAX / 2.0, Unit::Percent, &|v| wrap(json!({"offsetX": v})))
    });
    row(ui, t, "Offset Y", |ui| {
        slider(app, ui, &format!("{id}OffsetY"), p.offset_y, -OFFSET_MAX / 2.0, OFFSET_MAX / 2.0, Unit::Percent, &|v| wrap(json!({"offsetY": v})))
    });
}

/// The request changing text overlay `i` of `n` by `change` (the others kept).
fn text_change(i: usize, n: usize, change: Value) -> Value {
    let list: Vec<Value> = (0..n).map(|k| if k == i { change.clone() } else { json!({}) }).collect();
    json!({ "texts": list })
}

/// The Overlays sections.
pub fn sections(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let s: OverlaySettings = app.session.slide_overlays.clone();
    heading(ui, t, "Overlays");

    // identity plate
    let p = s.identity_plate.clone();
    let plate = |v: Value| json!({ "identityPlate": v });
    if toggle(app, ui, "slideshowPlate", p.enabled, "Identity plate", &|v| plate(json!({"enabled": v}))) {
        let mem = egui::Id::new("slideshow-plate-graphic");
        let mut graphic = !p.image.is_empty() || ui.data(|d| d.get_temp::<bool>(mem)).unwrap_or(false);
        let before = graphic;
        row(ui, t, "Style", |ui| choices(ui, "slideshowPlateStyle", &[(false, "Text"), (true, "Image")], &mut graphic));
        if graphic != before {
            ui.data_mut(|d| d.insert_temp(mem, graphic));
            if !graphic && !p.image.is_empty() {
                set(app, plate(json!({"image": ""})));
            }
        }
        if graphic {
            row(ui, t, "Image", |ui| {
                if app.services.pick_files.is_some()
                    && crate::widgets::text_button(ui, "slideshowPlateChoose", "Choose…", false).clicked()
                    && let Some(f) = app.services.pick_files.as_mut().and_then(|pick| pick().into_iter().next())
                {
                    set(app, plate(json!({"image": f})));
                }
                text(app, ui, "slideshowPlateImage", &p.image, "logo.png", &|v| plate(json!({"image": v})));
            });
        } else {
            row(ui, t, "Text", |ui| text(app, ui, "slideshowPlateText", &p.text, "Your name or studio", &|v| plate(json!({"text": v}))));
            row(ui, t, "Colour", |ui| colour(app, ui, "slideshowPlateColor", p.color, &|c| plate(json!({"color": c}))));
        }
        row(ui, t, "Size", |ui| slider(app, ui, "slideshowPlateSize", p.size, 0.01, PLATE_SIZE_MAX, Unit::Percent, &|v| plate(json!({"size": v}))));
        row(ui, t, "Opacity", |ui| slider(app, ui, "slideshowPlateOpacity", p.opacity, 0.0, 1.0, Unit::Percent, &|v| plate(json!({"opacity": v}))));
        placement(app, ui, t, "slideshowPlate", &p.place, true, &plate);
        toggle(app, ui, "slideshowPlateBehind", p.behind, "Render behind image", &|v| plate(json!({"behind": v})));
    }

    // rating stars
    ui.add_space(4.0);
    let r = s.rating_stars.clone();
    let stars = |v: Value| json!({ "ratingStars": v });
    if toggle(app, ui, "slideshowStars", r.enabled, "Rating stars", &|v| stars(json!({"enabled": v}))) {
        row(ui, t, "Colour", |ui| colour(app, ui, "slideshowStarsColor", r.color, &|c| stars(json!({"color": c}))));
        row(ui, t, "Size", |ui| slider(app, ui, "slideshowStarsSize", r.size, 0.01, STAR_SIZE_MAX, Unit::Percent, &|v| stars(json!({"size": v}))));
        placement(app, ui, t, "slideshowStars", &r.place, true, &stars);
    }

    // text overlays
    ui.add_space(6.0);
    ui.label(RichText::new(crate::i18n::tr("Text overlays")).font(t.semibold(11.5)).color(t.text));
    let n = s.texts.len();
    for (i, o) in s.texts.iter().enumerate() {
        let id = format!("slideshowText{i}");
        let wrap = |v: Value| text_change(i, n, v);
        ui.add_space(2.0);
        row(ui, t, &format!("{} {}", crate::i18n::tr("Text"), i + 1), |ui| {
            text(app, ui, &id, &o.text, "{title} · {exposure}", &|v| wrap(json!({"text": v})));
            if crate::widgets::text_button(ui, &format!("{id}Remove"), "Remove", false).clicked() {
                set(app, json!({"removeText": i}));
            }
        });
        if app.session.slide_overlays.texts.len() != n {
            return; // removed: the indices moved; drawn again next frame
        }
        row(ui, t, "Size", |ui| {
            slider(app, ui, &format!("{id}Size"), o.size, 0.01, TEXT_SIZE_MAX / 2.0, Unit::Percent, &|v| wrap(json!({"size": v})))
        });
        row(ui, t, "Colour", |ui| colour(app, ui, &format!("{id}Color"), o.color, &|c| wrap(json!({"color": c}))));
        row(ui, t, "Opacity", |ui| slider(app, ui, &format!("{id}Opacity"), o.opacity, 0.0, 1.0, Unit::Percent, &|v| wrap(json!({"opacity": v}))));
        placement(app, ui, t, &id, &o.place, true, &wrap);
        let sh = o.shadow;
        if toggle(app, ui, &format!("{id}Shadow"), sh.on, "Shadow", &|v| wrap(json!({"shadow": v}))) {
            row(ui, t, "Opacity", |ui| {
                slider(app, ui, &format!("{id}ShadowOpacity"), sh.opacity, 0.0, 1.0, Unit::Percent, &|v| wrap(json!({"shadowOpacity": v})))
            });
            row(ui, t, "Offset", |ui| {
                slider(app, ui, &format!("{id}ShadowOffset"), sh.offset, 0.0, SHADOW_OFFSET_MAX, Unit::Percent, &|v| wrap(json!({"shadowOffset": v})))
            });
            row(ui, t, "Radius", |ui| {
                slider(app, ui, &format!("{id}ShadowRadius"), sh.radius, 0.0, SHADOW_RADIUS_MAX, Unit::Percent, &|v| wrap(json!({"shadowRadius": v})))
            });
            row(ui, t, "Angle", |ui| {
                slider(app, ui, &format!("{id}ShadowAngle"), sh.angle, -180.0, 180.0, Unit::Degrees, &|v| wrap(json!({"shadowAngle": v})))
            });
        }
    }
    if n < lightcraft_engine::slide_overlay::MAX_TEXTS && crate::widgets::text_button(ui, "slideshowAddText", "Add text overlay", false).clicked() {
        set(app, json!({"addText": {}}));
    }
    hint(
        ui,
        t,
        "Tokens: {file}, {name}, {title}, {caption}, {date}, {exposure}, {iso}, {focal}, {camera}, {lens}, {rating}, {creator}, {copyright}. Other text shows as typed.",
    );

    // watermark
    ui.add_space(4.0);
    let w = s.watermark.clone();
    let wm = |v: Value| json!({ "watermark": v });
    if toggle(app, ui, "slideshowWatermark", s.show_watermark, "Watermark", &|v| json!({"showWatermark": v})) {
        row(ui, t, "Text", |ui| text(app, ui, "slideshowWatermarkText", &w.text, "© Your Name", &|v| wm(json!({"text": v}))));
        row(ui, t, "Size", |ui| slider(app, ui, "slideshowWatermarkSize", w.size as f64, 0.01, 0.15, Unit::Percent, &|v| wm(json!({"size": v}))));
        row(ui, t, "Opacity", |ui| {
            slider(app, ui, "slideshowWatermarkOpacity", w.opacity as f64, 0.05, 1.0, Unit::Percent, &|v| wm(json!({"opacity": v})))
        });
        row(ui, t, "Position", |ui| {
            let mut a = w.anchor;
            if anchor_grid(ui, "slideshowWatermarkAnchor", &mut a) {
                set(app, wm(json!({"anchor": lightcraft_engine::slide_overlay::anchor_id(a)})));
            }
        });
        let export_wm = app.session.last_export.as_ref().and_then(|e| e.get("watermark")).filter(|v| v.is_object() || v.is_string()).cloned();
        if let Some(e) = export_wm
            && crate::widgets::text_button(ui, "slideshowUseExportWatermark", "Use export watermark", false).clicked()
        {
            set(app, wm(e));
        }
        hint(ui, t, "The export watermark, drawn on the photo and sized relative to it.");
    }
    for warning in app.session.slide_overlays.warnings() {
        ui.label(RichText::new(warning).size(11.0).color(t.caution));
    }
}
