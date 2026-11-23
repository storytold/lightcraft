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

use crate::LightcraftApp;
use crate::panels::settings::{check, choices, heading, hint, row};
use crate::text_field::TextField;
use crate::theme::Tokens;

#[derive(Clone, Copy)]
enum Unit {
    /// A fraction shown as a percentage (of the slide's shorter side, or an opacity).
    Percent,
    Degrees,
}

fn set(app: &mut LightcraftApp, p: Value) {
    let _ = app.run("slideshow.overlays", p);
}

/// The span of a slider and how its value is shown.
struct Range {
    lo: f64,
    hi: f64,
    unit: Unit,
}

/// A slider (`slider:{id}`) over `range`; `wrap(value)` makes the `slideshow.overlays` request.
fn slider(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, label: &str, value: f64, range: Range, wrap: &dyn Fn(f64) -> Value) {
    use super::controls::{self, Shape, Step};
    let (k, suffix) = match range.unit {
        Unit::Percent => (100.0, "%"),
        Unit::Degrees => (1.0, "°"),
    };
    let shape = Shape { label, lo: range.lo * k, hi: range.hi * k, unit: suffix, step: 0.1, log: false };
    for step in controls::slider(ui, controls::owner(id), id, &shape, value * k) {
        match step {
            Step::Preview(v) => text_row_preview(app, &wrap(v / k)),
            Step::Save(v) => set(app, wrap(v / k)),
        }
    }
}

/// A colour well (`button:{id}`).
fn colour(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, label: &str, value: [u8; 3], wrap: &dyn Fn([u8; 3]) -> Value) {
    use super::controls;
    if let Some(c) = controls::colour(ui, controls::owner(id), id, label, value) {
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
    let mem = egui::Id::unique(("slideshow-overlay-field", id));
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
        slider(
            app,
            ui,
            &format!("{id}OffsetX"),
            "Offset X",
            p.offset_x,
            Range { lo: -OFFSET_MAX / 2.0, hi: OFFSET_MAX / 2.0, unit: Unit::Percent },
            &|v| wrap(json!({"offsetX": v})),
        )
    });
    row(ui, t, "Offset Y", |ui| {
        slider(
            app,
            ui,
            &format!("{id}OffsetY"),
            "Offset Y",
            p.offset_y,
            Range { lo: -OFFSET_MAX / 2.0, hi: OFFSET_MAX / 2.0, unit: Unit::Percent },
            &|v| wrap(json!({"offsetY": v})),
        )
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
        let mem = egui::Id::unique("slideshow-plate-graphic");
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
            row(ui, t, "Colour", |ui| colour(app, ui, "slideshowPlateColor", "Colour", p.color, &|c| plate(json!({"color": c}))));
        }
        row(ui, t, "Size", |ui| {
            slider(
                app,
                ui,
                "slideshowPlateSize",
                "Size",
                p.size,
                Range { lo: 0.01, hi: PLATE_SIZE_MAX, unit: Unit::Percent },
                &|v| plate(json!({"size": v})),
            )
        });
        row(ui, t, "Opacity", |ui| {
            slider(
                app,
                ui,
                "slideshowPlateOpacity",
                "Opacity",
                p.opacity,
                Range { lo: 0.0, hi: 1.0, unit: Unit::Percent },
                &|v| plate(json!({"opacity": v})),
            )
        });
        placement(app, ui, t, "slideshowPlate", &p.place, true, &plate);
        toggle(app, ui, "slideshowPlateBehind", p.behind, "Render behind image", &|v| plate(json!({"behind": v})));
    }

    // rating stars
    ui.add_space(4.0);
    let r = s.rating_stars.clone();
    let stars = |v: Value| json!({ "ratingStars": v });
    if toggle(app, ui, "slideshowStars", r.enabled, "Rating stars", &|v| stars(json!({"enabled": v}))) {
        row(ui, t, "Colour", |ui| colour(app, ui, "slideshowStarsColor", "Colour", r.color, &|c| stars(json!({"color": c}))));
        row(ui, t, "Size", |ui| {
            slider(
                app,
                ui,
                "slideshowStarsSize",
                "Size",
                r.size,
                Range { lo: 0.01, hi: STAR_SIZE_MAX, unit: Unit::Percent },
                &|v| stars(json!({"size": v})),
            )
        });
        placement(app, ui, t, "slideshowStars", &r.place, true, &stars);
    }

    if text_overlays(app, ui, t) {
        return; // The removed row changed the indices. Continue with fresh rows next pass.
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
        row(ui, t, "Size", |ui| {
            slider(
                app,
                ui,
                "slideshowWatermarkSize",
                "Size",
                w.size as f64,
                Range { lo: 0.01, hi: 0.15, unit: Unit::Percent },
                &|v| wm(json!({"size": v})),
            )
        });
        row(ui, t, "Opacity", |ui| {
            slider(app, ui, "slideshowWatermarkOpacity", "Opacity", w.opacity as f64, Range { lo: 0.05, hi: 1.0, unit: Unit::Percent }, &|v| {
                wm(json!({"opacity": v}))
            })
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

/// These library preferences have no persisted row IDs. Keep UI identities separate from their
/// command indices: our own removal shifts the indices, not the remaining editors' identities.
/// An external replacement or library switch retires all editors rather than retargeting a draft.
#[derive(Clone, Default)]
struct TextRows {
    generation: u64,
    next: u64,
    ids: Vec<egui::Id>,
    observed: Vec<lightcraft_engine::slide_overlay::TextOverlay>,
}

impl TextRows {
    fn append(&mut self) {
        self.next = self.next.saturating_add(1);
        self.ids.push(egui::Id::unique("slideshow-text-editor").with((self.generation, self.next)));
    }

    fn prepare(&mut self, generation: u64, texts: &[lightcraft_engine::slide_overlay::TextOverlay]) {
        if self.generation != generation || self.observed != texts {
            self.generation = generation;
            self.ids.clear();
            for _ in texts {
                self.append();
            }
            self.observed = texts.to_vec();
        }
    }
}

fn real_pass(ui: &egui::Ui) -> bool {
    !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx())
}

fn text_row_button(ui: &mut egui::Ui, owner: egui::Id, name: &str, label: &str, selected: Option<bool>, enabled: bool) -> bool {
    use craft::ui::buttons::{Button, ToggleButton};
    use craft::ui::contract::{Bind, Props as _, Show as _};
    let real = real_pass(ui);
    let (response, clicked) = if let Some(selected) = selected {
        let out = ToggleButton::new(crate::i18n::tr(label).to_owned(), Bind::given(selected)).id(owner).enabled(enabled).show(ui);
        let clicked = out.events.committed().is_some();
        (out.response, clicked)
    } else {
        let out = Button::new(crate::i18n::tr(label).to_owned()).id(owner).enabled(enabled).show(ui);
        let clicked = out.clicked();
        (out.response, clicked)
    };
    if real {
        crate::widgets::register_response(format!("button:{name}"), response.rect, &response);
    }
    real && ui.is_enabled() && enabled && clicked
}

fn text_row_field(app: &mut LightcraftApp, ui: &mut egui::Ui, owner: egui::Id, name: &str, value: &str, wrap: &dyn Fn(Value) -> Value) {
    use craft::ui::contract::{Bind, Props as _, Show as _};
    let real = real_pass(ui);
    let committed = {
        let draft_key = owner.with("sdk-draft");
        let mut draft: String = ui.data(|d| d.get_temp(draft_key)).unwrap_or_else(|| value.to_owned());
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().clamp(80.0, 260.0), 24.0), egui::Sense::hover());
        let out = craft::ui::text::TextField::new(crate::i18n::tr("Text"), Bind::bound(&mut draft))
            .id(owner.with("text"))
            .placeholder(crate::i18n::tr("{title} · {exposure}"))
            .limit(None) // Preserve engine-side character truncation at commit, not while typing.
            .submits()
            .show_at(ui, rect);
        if real {
            crate::widgets::register_response(format!("field:{name}"), out.response.rect, &out.response);
            if out.escaped {
                crate::widgets::take_escape(ui.ctx());
            }
            if out.events.ended().is_some() || !ui.is_enabled() {
                ui.data_mut(|d| d.remove::<String>(draft_key));
            } else {
                ui.data_mut(|d| d.insert_temp(draft_key, draft));
            }
        }
        out.events.committed().cloned()
    };
    if real
        && ui.is_enabled()
        && let Some(value_after) = committed.filter(|after| after != value)
    {
        set(app, wrap(json!({"text": value_after})));
    }
}

/// Merge preview values only. The real command remains the validation/persistence authority.
fn text_row_preview(app: &mut LightcraftApp, patch: &Value) {
    if let Ok(next) = app.session.slide_overlays.merged(patch) {
        app.session.slide_overlays = next;
    }
}

#[allow(clippy::too_many_arguments)]
fn text_row_slider(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    owner: egui::Id,
    name: &str,
    label: &str,
    value: f64,
    range: Range,
    wrap: &dyn Fn(Value) -> Value,
    key: &str,
) {
    use super::controls::{self, Shape, Step};
    let (k, suffix) = match range.unit {
        Unit::Percent => (100.0, "%"),
        Unit::Degrees => (1.0, "°"),
    };
    let shape = Shape { label, lo: range.lo * k, hi: range.hi * k, unit: suffix, step: 0.1, log: false };
    // (the preview and the saved value are what `key` of this text overlay becomes)
    let patch = |value: f64| wrap(json!({key: value / k}));
    for step in controls::slider(ui, owner, name, &shape, value * k) {
        match step {
            Step::Preview(v) => text_row_preview(app, &patch(v)),
            Step::Save(v) => set(app, patch(v)),
        }
    }
}

fn text_row_placement(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    t: &Tokens,
    owner: egui::Id,
    name: &str,
    p: &Placement,
    wrap: &dyn Fn(Value) -> Value,
) {
    const GLYPHS: [&str; 9] = ["↖", "↑", "↗", "←", "•", "→", "↙", "↓", "↘"];
    row(ui, t, "Position", |ui| {
        ui.vertical(|ui| {
            for r in 0..3 {
                ui.horizontal(|ui| {
                    for c in 0..3 {
                        let i = r * 3 + c;
                        let (Some(anchor), Some(glyph)) = (ANCHORS.get(i), GLYPHS.get(i)) else { continue };
                        if text_row_button(ui, owner.with(("anchor", i)), &format!("{name}Anchor-{i}"), glyph, Some(p.anchor == *anchor), true)
                            && p.anchor != *anchor
                        {
                            set(app, wrap(json!({"anchor": lightcraft_engine::slide_overlay::anchor_id(*anchor)})));
                        }
                    }
                });
            }
        });
    });
    row(ui, t, "Anchored to", |ui| {
        for (i, (value, label, id)) in [(RelativeTo::Slide, "Slide", "slide"), (RelativeTo::Photo, "Photo", "photo")].into_iter().enumerate() {
            if text_row_button(ui, owner.with(("relative", i)), &format!("{name}RelativeTo-{i}"), label, Some(p.relative_to == value), true)
                && p.relative_to != value
            {
                set(app, wrap(json!({"relativeTo": id})));
            }
        }
    });
    for (key, label, value, suffix) in [("offsetX", "Offset X", p.offset_x, "OffsetX"), ("offsetY", "Offset Y", p.offset_y, "OffsetY")] {
        row(ui, t, label, |ui| {
            text_row_slider(
                app,
                ui,
                owner,
                &format!("{name}{suffix}"),
                label,
                value,
                Range { lo: -OFFSET_MAX / 2.0, hi: OFFSET_MAX / 2.0, unit: Unit::Percent },
                wrap,
                key,
            )
        });
    }
}

fn text_row_shadow(app: &mut LightcraftApp, ui: &mut egui::Ui, owner: egui::Id, name: &str, value: bool, wrap: &dyn Fn(Value) -> Value) -> bool {
    use craft::ui::contract::{Bind, Props as _, Show as _};
    let real = real_pass(ui);
    let (response, next) = {
        let out = craft::ui::choice::Checkbox::new(Bind::given(value)).id(owner.with("shadow")).label(crate::i18n::tr("Shadow")).show(ui);
        let next = out.events.committed().copied();
        (out.response, next)
    };
    if real {
        crate::widgets::register_response(format!("check:settings.{name}Shadow"), response.rect, &response);
        if ui.is_enabled()
            && let Some(on) = next.filter(|on| *on != value)
        {
            set(app, wrap(json!({"shadow": on})));
            return on;
        }
    }
    value
}

/// The actual production text-overlay subtree. It can be exercised without drawing a photo or
/// running the app logic loop. Both native Settings and isolated SDK presentation call this path.
fn text_overlays(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) -> bool {
    use lightcraft_engine::slide_overlay::MAX_TEXTS;
    let real = real_pass(ui);
    let state_key = egui::Id::unique("slideshow-text-rows").with(ui.ctx().viewport_id());
    let mut rows = ui.data(|d| d.get_temp::<TextRows>(state_key)).unwrap_or_default();
    let texts = app.session.slide_overlays.texts.clone();
    rows.prepare(app.session.library_generation(), &texts);
    ui.add_space(6.0);
    ui.label(RichText::new(crate::i18n::tr("Text overlays")).font(t.semibold(11.5)).color(t.text));
    let n = texts.len();
    for (i, o) in texts.iter().enumerate() {
        let Some(owner) = rows.ids.get(i).copied() else { continue };
        let name = format!("slideshowText{i}");
        let wrap = |v: Value| text_change(i, n, v);
        let removed = ui
            .push_id(owner, |ui| {
                ui.add_space(2.0);
                let mut remove = false;
                row(ui, t, &format!("{} {}", crate::i18n::tr("Text"), i + 1), |ui| {
                    text_row_field(app, ui, owner, &name, &o.text, &wrap);
                    remove = text_row_button(ui, owner.with("remove"), &format!("{name}Remove"), "Remove", None, true);
                });
                if remove {
                    let before = app.session.slide_overlays.texts.len();
                    set(app, json!({"removeText": i}));
                    return app.session.slide_overlays.texts.len() != before;
                }
                for (key, label, value, lo, hi, suffix) in [("size", "Size", o.size, 0.01, TEXT_SIZE_MAX / 2.0, "Size")] {
                    row(ui, t, label, |ui| {
                        text_row_slider(app, ui, owner, &format!("{name}{suffix}"), label, value, Range { lo, hi, unit: Unit::Percent }, &wrap, key)
                    });
                }
                row(ui, t, "Colour", |ui| {
                    if let Some(color) = super::controls::colour(ui, owner.with("colour"), &format!("{name}Color"), "Colour", o.color) {
                        set(app, wrap(json!({"color": color})));
                    }
                });
                row(ui, t, "Opacity", |ui| {
                    text_row_slider(
                        app,
                        ui,
                        owner,
                        &format!("{name}Opacity"),
                        "Opacity",
                        o.opacity,
                        Range { lo: 0.0, hi: 1.0, unit: Unit::Percent },
                        &wrap,
                        "opacity",
                    )
                });
                text_row_placement(app, ui, t, owner, &name, &o.place, &wrap);
                // The existing shadow presentation stays native/scoped with the same command policy.
                // Give its shared helper IDs the stable row identity, rather than the shifting index.
                let sh = o.shadow;
                let on = text_row_shadow(app, ui, owner, &name, sh.on, &wrap);
                if on {
                    for (key, label, value, lo, hi, unit, suffix) in [
                        ("shadowOpacity", "Opacity", sh.opacity, 0.0, 1.0, Unit::Percent, "ShadowOpacity"),
                        ("shadowOffset", "Offset", sh.offset, 0.0, SHADOW_OFFSET_MAX, Unit::Percent, "ShadowOffset"),
                        ("shadowRadius", "Radius", sh.radius, 0.0, SHADOW_RADIUS_MAX, Unit::Percent, "ShadowRadius"),
                        ("shadowAngle", "Angle", sh.angle, -180.0, 180.0, Unit::Degrees, "ShadowAngle"),
                    ] {
                        row(ui, t, label, |ui| {
                            text_row_slider(app, ui, owner, &format!("{name}{suffix}"), label, value, Range { lo, hi, unit }, &wrap, key)
                        });
                    }
                }
                false
            })
            .inner;
        if removed {
            rows.ids.remove(i);
            rows.observed = app.session.slide_overlays.texts.clone();
            ui.data_mut(|d| d.insert_temp(state_key, rows));
            return true;
        }
    }
    // Keep the original hidden-at-limit presentation, not an enabled command that will fail.
    if n < MAX_TEXTS && text_row_button(ui, state_key.with((rows.generation, "add")), "slideshowAddText", "Add text overlay", None, true) {
        set(app, json!({"addText": {}}));
        if app.session.slide_overlays.texts.len() == n + 1 {
            rows.append();
        }
    }
    if real {
        rows.observed = app.session.slide_overlays.texts.clone();
        ui.data_mut(|d| d.insert_temp(state_key, rows));
    }
    false
}

#[cfg(test)]
#[path = "slide_overlays_sdk_tests.rs"]
mod slide_overlays_sdk_tests;
