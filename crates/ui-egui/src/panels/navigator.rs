//! The Library module's Navigator panel (top of the left column): the active photo with the
//! loupe's visible part outlined, and the zoom presets Fit / Fill / 1:1 / a custom level.
//! A click or drag pans the loupe (and opens it from the grids), like Lightroom Classic.

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::json;

use crate::DacApp;
use crate::state::{ViewMode, ZOOM_LEVELS, Zoom};
use crate::theme::Tokens;
use crate::widgets::register;

/// Where the loupe's visible part was last frame, in normalized image coordinates (written by
/// the loupe, read here).
pub(crate) fn note_visible(ctx: &egui::Context, r: Rect) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("navigator-visible"), r));
}

fn visible(ctx: &egui::Context) -> Option<Rect> {
    ctx.data(|d| d.get_temp::<Rect>(egui::Id::new("navigator-visible")))
}

/// The custom preset: the level picked from the fourth button's menu (default 3:1).
fn custom_level(ctx: &egui::Context) -> &'static str {
    let i = ctx.data(|d| d.get_temp::<usize>(egui::Id::new("navigator-custom"))).unwrap_or(5);
    ZOOM_LEVELS.get(i).map(|l| l.0).unwrap_or("3:1")
}

/// Which preset the loupe is at (for the highlight).
fn current(zoom: Zoom) -> Option<&'static str> {
    match zoom {
        Zoom::Fit => Some("fit"),
        Zoom::Fill => Some("fill"),
        Zoom::Percent(p) => ZOOM_LEVELS.iter().find(|l| (l.1 - p).abs() < 0.01).map(|l| l.0),
    }
}

/// Click on the Navigator at normalized `(u, v)`: centre the loupe there (opening it from a
/// grid at the custom or 1:1 level).
pub(crate) fn pan_to(app: &mut DacApp, u: f32, v: f32) {
    let (u, v) = (if u.is_finite() { u.clamp(0.0, 1.0) } else { 0.5 }, if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.5 });
    if app.ui.view != ViewMode::Detail {
        let _ = app.run("view.loupe", json!({}));
    }
    if matches!(app.ui.zoom, Zoom::Fit) {
        let _ = app.run("view.zoomLevel", json!({"level": "1:1"}));
    }
    app.ui.pan = (u, v);
}

/// A small disclosure triangle (down when `open`), painted (no font glyph needed).
fn triangle(p: &egui::Painter, c: egui::Pos2, open: bool, col: Color32) {
    let pts = if open {
        vec![c + vec2(-3.5, -2.0), c + vec2(3.5, -2.0), c + vec2(0.0, 2.5)]
    } else {
        vec![c + vec2(-2.0, -3.5), c + vec2(2.5, 0.0), c + vec2(-2.0, 3.5)]
    };
    p.add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
}

pub fn show(app: &mut DacApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    // header: title and the four presets at the right
    let (hr, _) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
    // the title folds the panel (remembered like the sidebar's sections)
    let title = Rect::from_min_max(hr.min, pos2(hr.left() + 110.0, hr.bottom()));
    register(ui.ctx(), "sidebarSection:navigator", title);
    if ui.interact(title, egui::Id::new("navigator-title"), Sense::click()).clicked() {
        app.ui.toggle_sidebar_section("navigator");
    }
    let open = !app.ui.sidebar_section_collapsed("navigator");
    triangle(ui.painter(), pos2(hr.left() + 22.0, hr.center().y), open, t.text_dim);
    ui.painter().text(pos2(hr.left() + 30.0, hr.center().y), Align2::LEFT_CENTER, crate::i18n::tr("Navigator"), t.semibold(12.0), t.text);
    let custom = custom_level(ui.ctx());
    let cur = if app.ui.view == ViewMode::Detail { current(app.ui.zoom) } else { None };
    let mut x = hr.right() - 10.0;
    for (key, label) in [("custom", custom), ("1:1", "1:1"), ("fill", "Fill"), ("fit", "Fit")] {
        let level = if key == "custom" { custom } else { key };
        let galley = ui.painter().layout_no_wrap(crate::i18n::tr(label).to_string(), t.font(11.0), t.text);
        let bw = galley.size().x + 8.0 + if key == "custom" { 8.0 } else { 0.0 };
        let r = Rect::from_min_max(pos2(x - bw, hr.top() + 3.0), pos2(x, hr.bottom() - 3.0));
        x -= bw + 4.0;
        register(ui.ctx(), format!("navigator:{key}"), r);
        let resp = ui.interact(r, egui::Id::new(("navigator-preset", key)), Sense::click());
        let on = cur == Some(level);
        let col = if on || resp.hovered() { t.text } else { t.text_dim };
        ui.painter().text(pos2(r.left() + 4.0, r.center().y), Align2::LEFT_CENTER, crate::i18n::tr(label), t.font(11.0), col);
        if on {
            ui.painter().line_segment([pos2(r.left() + 3.0, r.bottom()), pos2(r.right() - 3.0, r.bottom())], Stroke::new(1.0, t.text));
        }
        if key == "custom" {
            triangle(ui.painter(), pos2(r.right() - 6.0, r.center().y), true, col);
            egui::Popup::menu(&resp).show(|ui| {
                for (i, (name, _)) in ZOOM_LEVELS.iter().enumerate() {
                    if ui.selectable_label(*name == custom, *name).clicked() {
                        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("navigator-custom"), i));
                        if app.ui.view != ViewMode::Detail {
                            let _ = app.run("view.loupe", json!({}));
                        }
                        let _ = app.run("view.zoomLevel", json!({"level": name}));
                    }
                }
            });
        } else if resp.clicked() {
            if app.ui.view != ViewMode::Detail {
                let _ = app.run("view.loupe", json!({}));
            }
            let _ = app.run("view.zoomLevel", json!({"level": level}));
        }
    }
    if !open {
        return;
    }
    // the photo, letterboxed in a 3:2 well
    let (well, _) = ui.allocate_exact_size(vec2(w, (w * 0.66).min(220.0)), Sense::hover());
    let p = ui.painter_at(well);
    p.rect_filled(well, 0.0, t.inset);
    let Some(id) = app.session.selection.active else {
        p.text(well.center(), Align2::CENTER_CENTER, crate::i18n::tr("No photo selected"), t.font(11.0), t.text_dim);
        return;
    };
    let inner = well.shrink(10.0);
    let aspect = app.session.catalog.photo(id).map(|ph| ph.width.max(1) as f32 / ph.height.max(1) as f32).unwrap_or(1.5);
    let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { 1.5 };
    let (iw, ih) = if inner.width() / inner.height().max(1.0) > aspect {
        (inner.height() * aspect, inner.height())
    } else {
        (inner.width(), inner.width() / aspect)
    };
    let img = Rect::from_center_size(inner.center(), vec2(iw, ih));
    match app.renderer.thumb(id) {
        Some(tex) => p.image(tex.tex.id(), img, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE),
        None => p.rect_filled(img, 0.0, Color32::from_gray(40)),
    };
    register(ui.ctx(), "navigator:image", img);
    // the loupe's visible part (only while zoomed past the canvas)
    if app.ui.view == ViewMode::Detail
        && let Some(v) = visible(ui.ctx()).filter(|v| v.width() < 0.999 || v.height() < 0.999)
    {
        let to = |q: egui::Pos2| pos2(img.left() + q.x * img.width(), img.top() + q.y * img.height());
        let r = Rect::from_min_max(to(v.min), to(v.max));
        p.rect_stroke(r, 0.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
        p.rect_stroke(r.expand(1.5), 0.0, Stroke::new(1.0, Color32::from_black_alpha(160)), StrokeKind::Outside);
    }
    let resp = ui.interact(img, egui::Id::new("navigator-panel-image"), Sense::click_and_drag());
    if (resp.clicked() || resp.dragged())
        && let Some(q) = resp.interact_pointer_pos()
    {
        pan_to(app, (q.x - img.left()) / img.width().max(1.0), (q.y - img.top()) / img.height().max(1.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_name_the_loupe_level() {
        assert_eq!(current(Zoom::Fit), Some("fit"));
        assert_eq!(current(Zoom::Fill), Some("fill"));
        assert_eq!(current(Zoom::Percent(100.0)), Some("1:1"));
        assert_eq!(current(Zoom::Percent(300.0)), Some("3:1"));
        assert_eq!(current(Zoom::Percent(137.0)), None);
    }
}
