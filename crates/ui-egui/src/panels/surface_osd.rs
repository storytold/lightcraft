//! The on-screen readout of control surfaces: a pill in the window showing the value a dial is
//! turning (name, value and a bar), the command a key ran, or a surface notice (page, profile).
//!
//! Portions © RAD Films - Roman Alurkoff (from TactileBridge), MIT: TactileBridge's OSD overlay
//! (`OsdOverlay.cs`): its pill shape, layout, positions, opacity, colour and fade.

use egui::{Align2, Color32, Rect, pos2, vec2};
use lightcraft_surface as ls;
use ls::runtime::Shown;
use ls::settings::OsdPosition;

use crate::LightcraftApp;
use crate::theme::Tokens;

/// The pill's size (points), as TactileBridge's.
const WIDTH: f32 = 320.0;
const HEIGHT: f32 = 72.0;
/// The fade after the display time (s).
const FADE: f64 = 0.4;

/// What the readout says: (title, value text, bar 0…1, age in s, icon).
pub fn content(app: &LightcraftApp) -> Option<(String, String, Option<f64>, f64, Option<&'static str>)> {
    let now_ms = app.surface.now() * 1000.0;
    let (shown, at) = app.surface.runtime.shown(now_ms)?;
    let (title, value, bar, icon) = match shown {
        Shown::Value { target } => {
            let spec = ls::targets::target_spec(target)?;
            let v = crate::surface::value(app, target);
            let title = crate::i18n::tr(spec.label).to_string();
            let title = if spec.kind == ls::TargetKind::Mask { format!("{} · {}", crate::i18n::tr("Mask"), title) } else { title };
            let icon = ls::icons::icon_for(&ls::Binding::value(target));
            (title, v.map(|v| spec.format(v)).unwrap_or_else(|| "—".into()), v.map(|v| spec.position(v)), icon)
        }
        // a command without a short label of its own names itself by id: show its registry label
        Shown::Command { label, icon } => {
            let text = if !label.contains(' ') && label.contains('.') {
                crate::surface::registry_label(app, label).unwrap_or_else(|| label.clone())
            } else {
                crate::i18n::tr(label).to_string()
            };
            (text, String::new(), None, *icon)
        }
        Shown::Notice { text } => (notice_text(text), String::new(), None, None),
    };
    Some((title, value, bar, (now_ms - at) / 1000.0, icon))
}

/// Notices come in English from the engine ("Dial: Exposure", "Apply to all selected: On"):
/// translate their parts.
fn notice_text(text: &str) -> String {
    match text.split_once(": ") {
        Some((head, tail)) => {
            let tail = match tail {
                "On" | "Off" | "page" => crate::i18n::tr(tail).to_string(),
                t => ls::targets::all_targets()
                    .into_iter()
                    .find(|s| s.label == t)
                    .map_or_else(|| t.to_string(), |s| crate::i18n::tr(s.label).to_string()),
            };
            format!("{}: {tail}", crate::i18n::tr(head))
        }
        None => crate::i18n::tr(text).to_string(),
    }
}

pub fn show(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some((title, value, bar, age, icon)) = content(app) else { return };
    let osd = &app.surface.settings().osd;
    let display = f64::from(osd.display_ms) / 1000.0;
    let fade = if age <= display { 1.0 } else { (1.0 - (age - display) / FADE).clamp(0.0, 1.0) } as f32;
    if fade <= 0.0 {
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(30));
    let area = app.canvas_rect.unwrap_or_else(|| ctx.content_rect());
    let center_x = area.center().x;
    let y = match osd.position {
        OsdPosition::TopEdge => area.top() + 20.0 + HEIGHT / 2.0,
        OsdPosition::Top => area.top() + 80.0 + HEIGHT / 2.0,
        OsdPosition::BottomEdge => area.bottom() - 20.0 - HEIGHT / 2.0,
        OsdPosition::Bottom => area.bottom() - 80.0 - HEIGHT / 2.0,
    };
    let opacity = osd.opacity.clamp(0.1, 1.0) as f32 * fade;
    let (r, g, b) = ls::settings::rgb(&osd.background).unwrap_or((32, 32, 32));
    let t = Tokens::get(ctx);
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("surface_osd")));
    let width = WIDTH.min(area.width() - 16.0).max(160.0);
    let rect = Rect::from_center_size(pos2(center_x, y), vec2(width, HEIGHT));
    painter.rect_filled(rect, HEIGHT / 2.0, Color32::from_rgb(r, g, b).gamma_multiply(opacity));
    painter.rect_stroke(rect, HEIGHT / 2.0, egui::Stroke::new(1.0, Color32::from_white_alpha(40).gamma_multiply(fade)), egui::StrokeKind::Inside);
    crate::widgets::register_geometry(ctx, "osd:surface", rect);
    let title_c = Color32::from_rgb(220, 220, 220).gamma_multiply(fade);
    let value_c = Color32::WHITE.gamma_multiply(fade);
    // the action's icon (the same one the surface's key shows) at the left of the pill
    let icon_rect = Rect::from_center_size(pos2(rect.left() + 40.0, rect.center().y), vec2(40.0, 40.0));
    let has_icon = icon.is_some_and(|name| crate::surface_icons::paint_with(ctx, &painter, name, icon_rect, Color32::WHITE.gamma_multiply(fade)));
    let left = if has_icon { rect.left() + 70.0 } else { rect.left() + 30.0 };
    let right = rect.right() - 30.0;
    if value.is_empty() {
        let center = if has_icon { pos2((left + right) / 2.0, rect.center().y) } else { rect.center() };
        painter.text(center, Align2::CENTER_CENTER, &title, t.semibold(17.0), value_c);
        return;
    }
    let text_y = rect.top() + 26.0;
    painter.text(pos2(left, text_y), Align2::LEFT_CENTER, &title, t.font(14.0), title_c);
    painter.text(pos2(right, text_y), Align2::RIGHT_CENTER, &value, t.semibold(18.0), value_c);
    if let Some(p) = bar {
        let track = Rect::from_min_max(pos2(left, rect.top() + 46.0), pos2(right, rect.top() + 50.0));
        painter.rect_filled(track, 2.0, Color32::from_white_alpha(80).gamma_multiply(fade));
        let fill = Rect::from_min_max(track.min, pos2(track.left() + track.width() * p.clamp(0.0, 1.0) as f32, track.bottom()));
        painter.rect_filled(fill, 2.0, Color32::from_rgb(100, 180, 255).gamma_multiply(fade));
    }
}
