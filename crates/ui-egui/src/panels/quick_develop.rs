//! Quick Develop panel in Library Mode (Lightroom Classic parity).
//!
//! Provides rapid batch tone, white balance, crop ratio, and preset adjustments
//! across single or multiple selected photos directly from Grid, Compare, or Survey views.

use egui::{Align2, CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_develop::WbMode;
use serde_json::json;

use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::{divider, register};

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let t = Tokens::get(ui.ctx());
    let touch = crate::theme::touch_ui(ui.ctx());

    // Target photos: all selected photos if multi-selection, else active photo
    let targets: Vec<PhotoId> = if !app.session.selection.ids.is_empty() { app.session.selection.ids.clone() } else { vec![id] };
    let target_ids: Vec<u64> = targets.iter().map(|p| p.0).collect();

    // 1. Histogram
    super::edit::histogram(app, ui, id);
    divider(ui);

    // 2. Header with photo count badge
    header_with_badge(ui, &t, touch, targets.len());
    divider(ui);

    // 3. Saved Preset Section
    section_title(ui, &t, "Saved Preset");
    preset_chips(app, ui, &t, touch, &target_ids);
    divider(ui);

    // 4. Crop Aspect Ratio Section
    section_title(ui, &t, "Crop Ratio");
    crop_ratio_chips(app, ui, &t, touch, &target_ids);
    divider(ui);

    // 5. White Balance Section
    section_title(ui, &t, "White Balance");
    wb_preset_row(app, ui, &t, touch, &target_ids);

    nudge_row(app, ui, &t, touch, "Temp", "wb.temp", &target_ids, (-200.0, -50.0, 50.0, 200.0), ("<< -200K", "< -50K", "+50K >", "+200K >>"));

    nudge_row(app, ui, &t, touch, "Tint", "wb.tint", &target_ids, (-10.0, -2.0, 2.0, 10.0), ("<< -10", "< -2", "+2 >", "+10 >>"));
    divider(ui);

    // 6. Tone Control Section
    section_title(ui, &t, "Tone Control");

    // Auto Tone Action Button
    auto_tone_button(app, ui, &t, touch, &target_ids);

    nudge_row(
        app,
        ui,
        &t,
        touch,
        "Exposure",
        "light.exposure",
        &target_ids,
        (-1.0, -0.33333, 0.33333, 1.0),
        ("<< -1.0", "< -0.33", "+0.33 >", "+1.0 >>"),
    );

    nudge_row(app, ui, &t, touch, "Contrast", "light.contrast", &target_ids, (-20.0, -5.0, 5.0, 20.0), ("<< -20", "< -5", "+5 >", "+20 >>"));

    nudge_row(app, ui, &t, touch, "Highlights", "light.highlights", &target_ids, (-20.0, -5.0, 5.0, 20.0), ("<< -20", "< -5", "+5 >", "+20 >>"));

    nudge_row(app, ui, &t, touch, "Shadows", "light.shadows", &target_ids, (-20.0, -5.0, 5.0, 20.0), ("<< -20", "< -5", "+5 >", "+20 >>"));

    nudge_row(app, ui, &t, touch, "Whites", "light.whites", &target_ids, (-20.0, -5.0, 5.0, 20.0), ("<< -20", "< -5", "+5 >", "+20 >>"));

    nudge_row(app, ui, &t, touch, "Blacks", "light.blacks", &target_ids, (-20.0, -5.0, 5.0, 20.0), ("<< -20", "< -5", "+5 >", "+20 >>"));
    divider(ui);

    // 7. Presence Section
    section_title(ui, &t, "Presence");

    nudge_row(app, ui, &t, touch, "Clarity", "effects.clarity", &target_ids, (-20.0, -5.0, 5.0, 20.0), ("<< -20", "< -5", "+5 >", "+20 >>"));

    nudge_row(app, ui, &t, touch, "Dehaze", "effects.dehaze", &target_ids, (-20.0, -5.0, 5.0, 20.0), ("<< -20", "< -5", "+5 >", "+20 >>"));

    nudge_row(app, ui, &t, touch, "Vibrance", "color.vibrance", &target_ids, (-20.0, -5.0, 5.0, 20.0), ("<< -20", "< -5", "+5 >", "+20 >>"));

    nudge_row(app, ui, &t, touch, "Saturation", "color.saturation", &target_ids, (-20.0, -5.0, 5.0, 20.0), ("<< -20", "< -5", "+5 >", "+20 >>"));
    divider(ui);

    // 8. Bottom Reset Action
    reset_row(app, ui, &t, touch, &target_ids);
}

fn header_with_badge(ui: &mut Ui, t: &Tokens, touch: bool, count: usize) {
    let h = if touch { 48.0 } else { 40.0 };
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    // (the panel's title row above already names it)

    let badge_text = if count == 1 { "1 photo".to_string() } else { format!("{count} photos (Batch)") };
    let badge_font = t.semibold(11.0);
    let badge_galley = ui.painter().layout_no_wrap(badge_text.clone(), badge_font.clone(), t.accent);
    let badge_w = badge_galley.size().x + 14.0;
    let badge_rect = Rect::from_center_size(pos2(r.right() - badge_w * 0.5 - 20.0, r.center().y), vec2(badge_w, 22.0));

    ui.painter().rect(
        badge_rect,
        CornerRadius::same(11),
        t.accent.gamma_multiply(0.18),
        Stroke::new(1.0, t.accent.gamma_multiply(0.5)),
        StrokeKind::Inside,
    );
    ui.painter().text(badge_rect.center(), Align2::CENTER_CENTER, badge_text, badge_font, t.accent);
}

fn section_title(ui: &mut Ui, t: &Tokens, title: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    ui.painter().text(pos2(r.left() + 20.0, r.center().y + 2.0), Align2::LEFT_CENTER, crate::i18n::tr(title), t.semibold(12.5), t.text_label);
}

fn auto_tone_button(app: &mut LightcraftApp, ui: &mut Ui, t: &Tokens, touch: bool, target_ids: &[u64]) {
    let h = if touch { 40.0 } else { 30.0 };
    let w = (ui.available_width() - 40.0).max(100.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), h + 6.0), Sense::hover());
    let btn_rect = Rect::from_center_size(r.center(), vec2(w, h));
    let btn_resp = ui.interact(btn_rect, egui::Id::unique("quick_dev_auto_tone"), Sense::click());
    register(ui, "button:quickDevelopAutoTone", btn_rect);

    let fill = if btn_resp.is_pointer_button_down_on() {
        t.accent.gamma_multiply(0.35)
    } else if btn_resp.hovered() {
        t.accent.gamma_multiply(0.22)
    } else {
        t.accent.gamma_multiply(0.12)
    };
    ui.painter().rect(btn_rect, CornerRadius::same(if touch { 8 } else { 5 }), fill, Stroke::new(1.0, t.accent), StrokeKind::Inside);

    let label = crate::i18n::tr("✨ Auto Tone (Content-Aware)");
    ui.painter().text(btn_rect.center(), Align2::CENTER_CENTER, label, t.semibold(12.0), t.accent);

    if btn_resp.clicked() {
        let _ = app.run("develop.auto", json!({"ids": target_ids}));
    }
}

fn wb_preset_row(app: &mut LightcraftApp, ui: &mut Ui, t: &Tokens, touch: bool, target_ids: &[u64]) {
    let h = if touch { 36.0 } else { 26.0 };
    let row_rect = ui.allocate_exact_size(vec2(ui.available_width(), h + 4.0), Sense::hover()).0;
    let content_rect = Rect::from_min_max(pos2(row_rect.left() + 20.0, row_rect.top() + 2.0), pos2(row_rect.right() - 20.0, row_rect.bottom() - 2.0));

    let presets: [(&str, WbMode); 4] =
        [("As Shot", WbMode::AsShot), ("Auto", WbMode::Auto), ("Daylight", WbMode::Daylight), ("Cloudy", WbMode::Cloudy)];
    let col_w = (content_rect.width() - 3.0 * 6.0) / 4.0;

    for (i, (name, mode)) in presets.iter().enumerate() {
        let chip_rect = Rect::from_min_size(pos2(content_rect.left() + i as f32 * (col_w + 6.0), content_rect.top()), vec2(col_w, h));
        let chip_resp = ui.interact(chip_rect, egui::Id::unique(("wb_preset", *name)), Sense::click());
        register(ui, format!("chip:wbPreset:{name}"), chip_rect);

        let bg = if chip_resp.is_pointer_button_down_on() {
            t.pressed
        } else if chip_resp.hovered() {
            t.hover
        } else {
            t.button
        };
        ui.painter().rect(
            chip_rect,
            CornerRadius::same(if touch { 20 } else { 4 }),
            bg,
            if touch { Stroke::NONE } else { Stroke::new(1.0, t.button_border) },
            StrokeKind::Inside,
        );
        ui.painter().text(chip_rect.center(), Align2::CENTER_CENTER, crate::i18n::tr(name), t.font(11.0), t.text);

        if chip_resp.clicked() {
            let mode_str = match mode {
                WbMode::AsShot => "asShot",
                WbMode::Auto => "auto",
                WbMode::Daylight => "daylight",
                WbMode::Cloudy => "cloudy",
                _ => "custom",
            };
            let _ = app.run("develop.wb", json!({"mode": mode_str, "ids": target_ids}));
        }
    }
}

fn crop_ratio_chips(app: &mut LightcraftApp, ui: &mut Ui, t: &Tokens, touch: bool, target_ids: &[u64]) {
    let h = if touch { 34.0 } else { 24.0 };
    let row_rect = ui.allocate_exact_size(vec2(ui.available_width(), h + 4.0), Sense::hover()).0;
    let content_rect = Rect::from_min_max(pos2(row_rect.left() + 20.0, row_rect.top() + 2.0), pos2(row_rect.right() - 20.0, row_rect.bottom() - 2.0));

    let ratios: [(&str, Option<f64>); 5] =
        [("Orig", None), ("1:1", Some(1.0)), ("4:5", Some(4.0 / 5.0)), ("2:3", Some(2.0 / 3.0)), ("16:9", Some(16.0 / 9.0))];
    let col_w = (content_rect.width() - 4.0 * 5.0) / 5.0;

    for (i, (label, aspect)) in ratios.iter().enumerate() {
        let chip_rect = Rect::from_min_size(pos2(content_rect.left() + i as f32 * (col_w + 5.0), content_rect.top()), vec2(col_w, h));
        let chip_resp = ui.interact(chip_rect, egui::Id::unique(("crop_ratio", *label)), Sense::click());
        register(ui, format!("chip:cropRatio:{label}"), chip_rect);

        let bg = if chip_resp.is_pointer_button_down_on() {
            t.pressed
        } else if chip_resp.hovered() {
            t.hover
        } else {
            t.button
        };
        ui.painter().rect(
            chip_rect,
            CornerRadius::same(if touch { 20 } else { 4 }),
            bg,
            if touch { Stroke::NONE } else { Stroke::new(1.0, t.button_border) },
            StrokeKind::Inside,
        );
        ui.painter().text(chip_rect.center(), Align2::CENTER_CENTER, *label, t.semibold(11.0), t.text);

        if chip_resp.clicked() {
            if let Some(a) = aspect {
                let _ = app.run("develop.set", json!({"control": "crop.aspect", "value": a, "ids": target_ids}));
            } else {
                let _ = app.run("develop.resetControl", json!({"control": "crop.aspect", "ids": target_ids}));
            }
        }
    }
}

fn preset_chips(app: &mut LightcraftApp, ui: &mut Ui, t: &Tokens, touch: bool, target_ids: &[u64]) {
    let h = if touch { 34.0 } else { 24.0 };
    let row_rect = ui.allocate_exact_size(vec2(ui.available_width(), h + 4.0), Sense::hover()).0;
    let content_rect = Rect::from_min_max(pos2(row_rect.left() + 20.0, row_rect.top() + 2.0), pos2(row_rect.right() - 20.0, row_rect.bottom() - 2.0));

    let presets = [
        ("B&W", json!({"develop.treatment": {"bw": true}})),
        ("Vivid", json!({"color.vibrance": 25.0, "light.contrast": 15.0})),
        ("Portrait", json!({"effects.clarity": -10.0, "color.vibrance": 12.0})),
        ("Landscape", json!({"effects.clarity": 15.0, "effects.dehaze": 10.0, "color.vibrance": 20.0})),
    ];
    let col_w = (content_rect.width() - 3.0 * 6.0) / 4.0;

    for (i, (label, values)) in presets.iter().enumerate() {
        let chip_rect = Rect::from_min_size(pos2(content_rect.left() + i as f32 * (col_w + 6.0), content_rect.top()), vec2(col_w, h));
        let chip_resp = ui.interact(chip_rect, egui::Id::unique(("preset_quick", *label)), Sense::click());
        register(ui, format!("chip:quickPreset:{label}"), chip_rect);

        let bg = if chip_resp.is_pointer_button_down_on() {
            t.pressed
        } else if chip_resp.hovered() {
            t.hover
        } else {
            t.button
        };
        ui.painter().rect(
            chip_rect,
            CornerRadius::same(if touch { 20 } else { 4 }),
            bg,
            if touch { Stroke::NONE } else { Stroke::new(1.0, t.button_border) },
            StrokeKind::Inside,
        );
        ui.painter().text(chip_rect.center(), Align2::CENTER_CENTER, *label, t.semibold(11.0), t.text);

        if chip_resp.clicked() {
            if *label == "B&W" {
                let _ = app.run("develop.treatment", json!({"bw": true, "ids": target_ids}));
            } else {
                let _ = app.run("develop.set", json!({"values": values, "ids": target_ids}));
            }
        }
    }
}

fn nudge_row(
    app: &mut LightcraftApp,
    ui: &mut Ui,
    t: &Tokens,
    touch: bool,
    label: &str,
    control: &str,
    target_ids: &[u64],
    deltas: (f64, f64, f64, f64),
    button_labels: (&str, &str, &str, &str),
) {
    // the label sits above its four buttons so they share the panel's full width (side by side the label
    // column left each only ~33pt, narrower than its own text)
    let row_h = if touch { 40.0 } else { 28.0 };
    let label_h = 20.0;
    let row_rect = ui.allocate_exact_size(vec2(ui.available_width(), label_h + row_h + 6.0), Sense::hover()).0;
    let content = Rect::from_min_max(pos2(row_rect.left() + 20.0, row_rect.top() + 2.0), pos2(row_rect.right() - 20.0, row_rect.bottom() - 4.0));
    let btn_w = (content.width() - 3.0 * 6.0) / 4.0;

    ui.painter().text(pos2(content.left(), content.top() + label_h / 2.0), Align2::LEFT_CENTER, crate::i18n::tr(label), t.semibold(12.0), t.text);

    let (d0, d1, d2, d3) = deltas;
    let (l0, l1, l2, l3) = button_labels;
    let buttons = [(d0, l0), (d1, l1), (d2, l2), (d3, l3)];

    for (i, (delta, blabel)) in buttons.iter().enumerate() {
        let bx = content.left() + i as f32 * (btn_w + 6.0);
        let b_rect = Rect::from_min_size(pos2(bx, content.top() + label_h), vec2(btn_w, row_h));
        let b_resp = ui.interact(b_rect, egui::Id::unique(("nudge", control, i)), Sense::click());
        register(ui, format!("nudge:{control}:{i}"), b_rect);

        let bg = if b_resp.is_pointer_button_down_on() {
            t.pressed
        } else if b_resp.hovered() {
            t.hover
        } else {
            t.button
        };
        ui.painter().rect(
            b_rect,
            CornerRadius::same(if touch { 20 } else { 4 }),
            bg,
            if touch { Stroke::NONE } else { Stroke::new(1.0, t.button_border) },
            StrokeKind::Inside,
        );

        let font_size = if touch { 11.5 } else { 10.5 };
        // (the "<<" / ">" arrows only crowded the label: the sign and size already say which way it nudges)
        let plain = blabel.replace("<< ", "").replace("< ", "").replace(" >>", "").replace(" >", "");
        ui.painter().text(b_rect.center(), Align2::CENTER_CENTER, plain, t.semibold(font_size), t.text);

        if b_resp.clicked() {
            let _ = app.run("develop.adjust", json!({"control": control, "delta": delta, "ids": target_ids}));
        }
    }
}

fn reset_row(app: &mut LightcraftApp, ui: &mut Ui, t: &Tokens, touch: bool, target_ids: &[u64]) {
    let h = if touch { 40.0 } else { 30.0 };
    let row_rect = ui.allocate_exact_size(vec2(ui.available_width(), h + 12.0), Sense::hover()).0;
    let w = (row_rect.width() - 40.0).max(100.0);
    let btn_rect = Rect::from_center_size(row_rect.center(), vec2(w, h));
    let resp = ui.interact(btn_rect, egui::Id::unique("quick_develop_reset_all"), Sense::click());
    register(ui, "button:quickDevelopResetAll", btn_rect);

    let bg = if resp.is_pointer_button_down_on() {
        t.pressed
    } else if resp.hovered() {
        t.hover
    } else {
        t.button
    };
    ui.painter().rect(btn_rect, CornerRadius::same(if touch { 8 } else { 5 }), bg, Stroke::new(1.0, t.button_border), StrokeKind::Inside);

    let label = crate::i18n::tr("Reset Selected Photos");
    ui.painter().text(btn_rect.center(), Align2::CENTER_CENTER, label, t.semibold(12.0), t.text_dim);

    if resp.clicked() {
        let _ = app.run("develop.reset", json!({"ids": target_ids}));
    }
}
