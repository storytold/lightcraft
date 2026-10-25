//! The White Balance Selector's loupe (test notes 2026-10-25 note 13): while the selector is armed,
//! a small box by the pointer shows the pixels around it as a magnified grid, the area that is
//! sampled outlined, so the photographer sees what the click reads. The mouse wheel changes the
//! sample's size ([`step`]); a small sample is shown more zoomed in than a large one. Drawn in code
//! from the photo on screen: original work (see `assets/ATTRIBUTION.md`).

use egui::{Color32, Id, LayerId, Order, Pos2, Rect, Stroke, StrokeKind, pos2, vec2};

/// The smallest and largest sample: its side in screen pixels (odd, so the pointer's pixel is the
/// middle one).
pub const MIN_SAMPLE: u32 = 3;
pub const MAX_SAMPLE: u32 = 41;
/// The sample the selector starts with.
pub const DEFAULT_SAMPLE: u32 = 7;
/// The loupe's side, points.
const SIDE: f32 = 124.0;
/// How far the loupe sits from the pointer (down and to the right, unless that leaves the window).
const GAP: f32 = 26.0;

/// The sample side after `notches` of the mouse wheel (up: larger), two pixels per notch, clamped to
/// [`MIN_SAMPLE`]..=[`MAX_SAMPLE`] and kept odd.
pub fn step(size: u32, notches: f32) -> u32 {
    let size = size.clamp(MIN_SAMPLE, MAX_SAMPLE) | 1;
    if !notches.is_finite() || notches == 0.0 {
        return size;
    }
    let n = notches.round().clamp(-64.0, 64.0) as i64;
    let n = if n == 0 { notches.signum() as i64 } else { n };
    let next = (i64::from(size) + 2 * n).clamp(i64::from(MIN_SAMPLE), i64::from(MAX_SAMPLE));
    (next as u32) | 1
}

/// The sample of `size` screen pixels as a fraction of the long edge of the photo drawn in `img`
/// (points; `ppp` pixels per point): what `develop.wbPick` takes as `size`.
pub fn sample_fraction(size: u32, img: Rect, ppp: f32) -> f64 {
    let size = size.clamp(MIN_SAMPLE, MAX_SAMPLE) | 1;
    let long = f64::from(img.width().max(img.height())) * f64::from(ppp);
    if !long.is_finite() || long < 1.0 { 0.01 } else { (f64::from(size) / long).clamp(1e-4, 0.25) }
}

/// How many cells the loupe shows across for a sample of `size`: the sample and a margin around it.
pub fn cells(size: u32) -> u32 {
    let size = size.clamp(MIN_SAMPLE, MAX_SAMPLE) | 1;
    size + 2 * (size / 4).max(3)
}

/// Where the loupe goes for a pointer at `at` in a window `screen`: down and to the right of it,
/// flipped to the other side where the window ends.
pub fn place(at: Pos2, screen: Rect) -> Rect {
    let mut min = at + vec2(GAP, GAP);
    if min.x + SIDE > screen.right() {
        min.x = at.x - GAP - SIDE;
    }
    if min.y + SIDE > screen.bottom() {
        min.y = at.y - GAP - SIDE;
    }
    Rect::from_min_size(min, vec2(SIDE, SIDE))
}

/// Paint the loupe for the pointer at `at` over the photo drawn in `img` from texture `tex`, a
/// sample of `size` screen pixels (`ppp` pixels per point). Registered as `wbLoupe`, the sampled
/// area as `wbLoupe:sample`.
pub fn show(ctx: &egui::Context, tex: egui::TextureId, img: Rect, at: Pos2, size: u32, ppp: f32) {
    if !at.x.is_finite() || !at.y.is_finite() || !img.is_positive() || !ppp.is_finite() || ppp <= 0.0 {
        return;
    }
    let size = size.clamp(MIN_SAMPLE, MAX_SAMPLE) | 1;
    let n = cells(size);
    let px = 1.0 / ppp;
    let side = SIDE / n as f32;
    let r = place(at, ctx.content_rect());
    let p = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::unique("lc-wb-loupe")));
    p.rect_filled(r.expand(2.0), 4.0, Color32::from_black_alpha(200));
    let half = (n / 2) as f32;
    // the pointer's screen pixel, its centre
    let centre = pos2((at.x * ppp).floor() / ppp + px / 2.0, (at.y * ppp).floor() / ppp + px / 2.0);
    for j in 0..n {
        for i in 0..n {
            let q = centre + vec2((i as f32 - half) * px, (j as f32 - half) * px);
            let cell = Rect::from_min_size(r.min + vec2(i as f32 * side, j as f32 * side), vec2(side, side));
            let uv = pos2((q.x - img.min.x) / img.width(), (q.y - img.min.y) / img.height());
            if (0.0..=1.0).contains(&uv.x) && (0.0..=1.0).contains(&uv.y) {
                // a UV point, not a span: the cell takes that one pixel's colour, crisp
                p.image(tex, cell, Rect::from_min_max(uv, uv), Color32::WHITE);
            } else {
                p.rect_filled(cell, 0.0, Color32::from_gray(40));
            }
        }
    }
    if side >= 4.0 {
        let line = Stroke::new(0.5, Color32::from_black_alpha(70));
        for k in 1..n {
            let o = k as f32 * side;
            p.line_segment([pos2(r.left() + o, r.top()), pos2(r.left() + o, r.bottom())], line);
            p.line_segment([pos2(r.left(), r.top() + o), pos2(r.right(), r.top() + o)], line);
        }
    }
    let margin = ((n - size) / 2) as f32 * side;
    let sample = Rect::from_min_size(r.min + vec2(margin, margin), vec2(size as f32 * side, size as f32 * side));
    p.rect_stroke(sample, 0.0, Stroke::new(3.0, Color32::from_black_alpha(160)), StrokeKind::Outside);
    p.rect_stroke(sample, 0.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Outside);
    p.rect_stroke(r, 3.0, Stroke::new(1.0, Color32::from_gray(200)), StrokeKind::Outside);
    // the sample's size, under the grid
    let text = format!("{size} × {size}");
    let font = egui::FontId::proportional(11.0);
    let g = p.layout_no_wrap(text, font, Color32::from_gray(230));
    let label = Rect::from_min_size(pos2(r.center().x - g.size().x / 2.0 - 5.0, r.bottom() + 4.0), g.size() + vec2(10.0, 4.0));
    p.rect_filled(label, 3.0, Color32::from_black_alpha(200));
    p.galley(label.min + vec2(5.0, 2.0), g, Color32::from_gray(230));
    crate::widgets::register_geometry(ctx, "wbLoupe", r);
    crate::widgets::register_geometry(ctx, "wbLoupe:sample", sample);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wheel changes the sample two pixels a notch, up larger, from 3 × 3 to its largest, odd.
    #[test]
    fn the_wheel_steps_the_sample_within_its_range() {
        assert_eq!(step(7, 1.0), 9);
        assert_eq!(step(7, -1.0), 5);
        assert_eq!(step(7, 3.0), 13);
        assert_eq!(step(3, -5.0), MIN_SAMPLE, "never below 3 × 3");
        assert_eq!(step(MAX_SAMPLE, 2.0), MAX_SAMPLE, "never above the largest");
        assert_eq!(step(7, 0.4), 9, "a part notch still steps");
        assert_eq!(step(7, f32::NAN), 7);
        assert_eq!(step(8, 0.0), 9, "kept odd");
        assert_eq!(step(0, 0.0), MIN_SAMPLE);
        assert_eq!(step(u32::MAX, 1.0), MAX_SAMPLE);
    }

    /// A small sample is shown more zoomed in (fewer, larger cells) than a large one, with a margin
    /// around it either way; the loupe stays in the window.
    #[test]
    fn small_samples_show_more_zoomed_and_the_loupe_stays_on_screen() {
        assert!(cells(3) < cells(21));
        for s in [3, 7, 21, MAX_SAMPLE] {
            assert!(cells(s) > s && (cells(s) - s).is_multiple_of(2), "{s}: a margin on each side");
        }
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 700.0));
        for at in [pos2(10.0, 10.0), pos2(990.0, 690.0), pos2(500.0, 690.0)] {
            let r = place(at, screen);
            assert!(screen.contains_rect(r), "{at:?}: {r:?}");
            assert!(!r.contains(at), "{at:?}: never under the pointer");
        }
    }

    /// The sample outline covers `size` of the cells, centred; nothing is drawn for a pointer that is
    /// not a number.
    #[test]
    fn the_sample_is_outlined_in_the_middle_of_the_grid() {
        let ctx = egui::Context::default();
        let img = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| show(ui.ctx(), egui::TextureId::default(), img, pos2(300.0, 200.0), 5, 1.0));
        out.textures_delta.clear();
        let w = crate::widgets::take_registry(&ctx);
        let loupe = w.iter().find(|(id, _)| id == "wbLoupe").map(|w| w.1).expect("the loupe");
        let sample = w.iter().find(|(id, _)| id == "wbLoupe:sample").map(|w| w.1).expect("the sample");
        assert!((sample.center() - loupe.center()).length() < 0.5, "centred");
        let ratio = sample.width() / loupe.width();
        assert!((ratio - 5.0 / cells(5) as f32).abs() < 1e-3, "{ratio}");
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| show(ui.ctx(), egui::TextureId::default(), img, pos2(f32::NAN, 1.0), 5, 1.0));
        out.textures_delta.clear();
        assert!(out.shapes.is_empty());
    }
}
