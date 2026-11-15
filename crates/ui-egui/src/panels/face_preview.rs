//! Which face a face tile is about (issue #1147). A tile is a square around the face, and in a family photo it often
//! holds a second face just as large: the tile outlines its own face, and hovering it shows the whole photo with that
//! face marked. Used by the unnamed faces ([`super::unnamed`]) and a person's faces ([`super::person`]).
//!
//! The engine says where the face lies (`Session::face_in_tile`, `Session::face_photo_job`); this draws it.

use egui::{Color32, Rect, Stroke, StrokeKind, pos2, vec2};

use crate::LccApp;

/// The long edge of the hover preview, in points.
const PREVIEW_EDGE: f32 = 360.0;

/// `at` (fractions of `r`) as a rectangle on screen.
fn place(r: Rect, at: lcc_geom::Rect) -> Rect {
    Rect::from_min_max(
        pos2(r.left() + r.width() * at.x0 as f32, r.top() + r.height() * at.y0 as f32),
        pos2(r.left() + r.width() * at.x1 as f32, r.top() + r.height() * at.y1 as f32),
    )
}

/// A face's box: a light line with a dark one under it, so it shows on bright and dark faces alike.
fn mark(p: &egui::Painter, b: Rect) {
    p.rect_stroke(b, 2.0, Stroke::new(3.0, Color32::from_black_alpha(140)), StrokeKind::Middle);
    p.rect_stroke(b, 2.0, Stroke::new(1.2, Color32::from_white_alpha(230)), StrokeKind::Middle);
}

/// Outline the face of the tile drawn at `r` (`face`: the box the tile was made around, upright frame).
pub fn outline(app: &LccApp, p: &egui::Painter, r: Rect, photo: u64, face: lcc_geom::Rect) {
    if let Some(at) = app.session.face_in_tile(lcc_catalog::PhotoId(photo), face) {
        mark(&p.with_clip_rect(r), place(r, at).shrink(1.0));
    }
}

/// While the tile is hovered: the whole photo with its face marked, under `caption` when there is one.
pub fn on_hover(app: &mut LccApp, resp: &egui::Response, photo: u64, face: lcc_geom::Rect, caption: Option<String>) {
    let _ = resp.clone().on_hover_ui(|ui| {
        if let Some(c) = caption {
            ui.label(c);
        }
        let ppp = ui.ctx().pixels_per_point();
        let edge = (PREVIEW_EDGE * ppp).ceil() as usize;
        let shown = app.session.face_photo_job(lcc_catalog::PhotoId(photo), face, edge).and_then(|(job, at)| {
            let tex = app.renderer.variant(job)?;
            Some((tex.id(), tex.size, at))
        });
        match shown {
            Some((id, [w, h], at)) if w > 0 && h > 0 => {
                let s = PREVIEW_EDGE / w.max(h) as f32;
                let (r, _) = ui.allocate_exact_size(vec2(w as f32 * s, h as f32 * s), egui::Sense::hover());
                let p = ui.painter();
                p.image(id, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                mark(p, place(r, at));
            }
            // still rendering: a spinner where the photo will be
            _ => {
                let (r, _) = ui.allocate_exact_size(vec2(PREVIEW_EDGE, PREVIEW_EDGE * 2.0 / 3.0), egui::Sense::hover());
                egui::Spinner::new().paint_at(ui, Rect::from_center_size(r.center(), vec2(24.0, 24.0)));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // a box at fractions of the tile lands on the same fractions of the tile on screen
    #[test]
    fn a_box_is_placed_at_its_fractions_of_the_tile() {
        let r = Rect::from_min_size(pos2(100.0, 50.0), vec2(200.0, 100.0));
        let b = place(r, lcc_geom::Rect { x0: 0.25, y0: 0.5, x1: 0.75, y1: 1.0 });
        assert_eq!(b, Rect::from_min_max(pos2(150.0, 100.0), pos2(250.0, 150.0)));
    }
}
