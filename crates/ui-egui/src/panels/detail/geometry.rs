#[cfg(test)]
use super::info::label_badge_rect;
use super::*;

/// Screen ↔ normalized-image mapping for the displayed frame.
#[derive(Clone, Copy)]
pub struct CanvasMap {
    pub(super) rect: Rect,
    to_norm: Affine,
    from_norm: Affine,
    k: f32,
}

impl CanvasMap {
    pub(super) fn new(frame: &Frame, rect: Rect) -> CanvasMap {
        let k = 8.0f32;
        let (w, h) = ((rect.width() * k).round().max(1.0) as usize, (rect.height() * k).round().max(1.0) as usize);
        let to_norm = frame.out_to_norm(w, h);
        CanvasMap { rect, to_norm, from_norm: to_norm.inverse().unwrap_or(Affine::IDENTITY), k }
    }
    pub fn norm(&self, p: Pos2) -> Point {
        self.to_norm.apply(Point::new(((p.x - self.rect.left()) * self.k) as f64, ((p.y - self.rect.top()) * self.k) as f64))
    }
    pub fn screen(&self, n: Point) -> Pos2 {
        let q = self.from_norm.apply(n);
        pos2(self.rect.left() + q.x as f32 / self.k, self.rect.top() + q.y as f32 / self.k)
    }
}

pub(crate) fn fit_rect(area: Rect, aspect: f32, zoom: Zoom, img_px: [usize; 2], ppp: f32, pan: (f32, f32)) -> Rect {
    let zoom = match zoom {
        Zoom::Percent(p) if !p.is_finite() || p <= 0.0 => Zoom::Fit,
        _ => zoom,
    };
    let (aw, ah) = (area.width(), area.height());
    let (w, h) = match zoom {
        Zoom::Fit => {
            if aw / ah > aspect {
                (ah * aspect, ah)
            } else {
                (aw, aw / aspect)
            }
        }
        Zoom::Fill => {
            if aw / ah > aspect {
                (aw, aw / aspect)
            } else {
                (ah * aspect, ah)
            }
        }
        Zoom::Percent(p) => {
            // the photo's own pixels at p% (its width as shown: cropped and rotated)
            let w = img_px[0] as f32 * p / 100.0 / ppp;
            (w, w / aspect)
        }
    };
    let pan = bounded_pan(area, vec2(w, h), pan);
    let c = pos2(area.center().x - (pan.0 - 0.5) * w, area.center().y - (pan.1 - 0.5) * h);
    let rect = Rect::from_center_size(c, vec2(w, h));
    // At 100 % the texture maps texel for texel to screen pixels, so its corner must sit on a whole
    // physical pixel: half a pixel off and the GPU's bilinear filter blurs the view (an odd-width
    // canvas on a 2x display does this). Other zooms resample anyway, and a pinch must not jump.
    let one_to_one = ppp.is_finite() && ppp > 0.0 && (w * ppp - img_px[0] as f32).abs() < 0.5;
    if matches!(zoom, Zoom::Percent(_)) && one_to_one {
        let snap = |v: f32| (v * ppp).round() / ppp;
        return Rect::from_min_size(pos2(snap(rect.min.x), snap(rect.min.y)), rect.size());
    }
    rect
}

/// Fit a decoded image inside the frame calculated from catalog metadata. Unsupported RAWs can
/// only show their embedded camera JPEG, whose crop and aspect ratio can differ from that metadata.
pub(crate) fn fit_texture_rect(area: Rect, size: [usize; 2]) -> Rect {
    let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
    let scale = (area.width() / w).min(area.height() / h);
    Rect::from_center_size(area.center(), vec2(w * scale, h * scale))
}

/// `rect` moved onto whole physical pixels, at exactly the texture's size, when the texture is
/// already the size it is drawn at (within half a pixel), as a display-fitted render at Fit is: centred
/// on a fractional position, the GPU's bilinear filter would average every screen pixel with its
/// neighbour (the 100 % view does the same in [`fit_rect`]). Textures of other sizes are left to
/// the sampler.
pub(crate) fn snap_fit_rect(rect: Rect, size: [usize; 2], ppp: f32) -> Rect {
    let ppp = if ppp.is_finite() && ppp > 0.0 { ppp } else { 1.0 };
    let (w, h) = (size[0] as f32, size[1] as f32);
    if (rect.width() * ppp - w).abs() > 0.5 || (rect.height() * ppp - h).abs() > 0.5 {
        return rect;
    }
    let snap = |v: f32| (v * ppp).round() / ppp;
    Rect::from_min_size(pos2(snap(rect.min.x), snap(rect.min.y)), vec2(w / ppp, h / ppp))
}

/// Keep the viewport inside the photo; axes smaller than the viewport stay centred.
fn bounded_pan(area: Rect, size: egui::Vec2, pan: (f32, f32)) -> (f32, f32) {
    let bound = |visible: f32, image: f32, centre: f32| {
        let centre = if centre.is_finite() { centre } else { 0.5 };
        if image > visible.max(0.0) {
            let half = visible.max(0.0) / image / 2.0;
            centre.clamp(half, 1.0 - half)
        } else {
            0.5
        }
    };
    (bound(area.width(), size.x, pan.0), bound(area.height(), size.y, pan.1))
}

pub(crate) fn pan_image(app: &mut LightcraftApp, area: Rect, img: Rect, delta: egui::Vec2) {
    let centre = area.center() - img.min - delta;
    let pan = bounded_pan(area, img.size(), (centre.x / img.width().max(1.0), centre.y / img.height().max(1.0)));
    let _ = app.run("view.navigate", json!({"pan": pan}));
}

/// A drag with the middle button (the mouse wheel pressed) pans the zoomed photo whatever tool is
/// active, so the healing brush, mask brushes and crop never paint or move while you look around;
/// the hand cursor shows while it does.
pub(super) fn middle_pan(app: &mut LightcraftApp, ui: &egui::Ui, resp: &egui::Response, area: Rect, img: Rect) -> bool {
    if !resp.dragged_by(egui::PointerButton::Middle) {
        return false;
    }
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    if img.width() > area.width() + 1.0 || img.height() > area.height() + 1.0 {
        pan_image(app, area, img, resp.drag_delta());
    }
    true
}

/// Net vertical notches (up is positive) of unmodified line/page-unit wheel events: a real mouse wheel, not a trackpad.
fn wheel_notches(events: &[egui::Event]) -> f32 {
    events
        .iter()
        .filter_map(|e| match e {
            egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page, delta, modifiers, .. }
                if *modifiers == egui::Modifiers::NONE && delta.y.is_finite() =>
            {
                Some(delta.y)
            }
            _ => None,
        })
        .sum()
}

/// Native pinch (also modifier-wheel zoom) and two-finger scroll, scoped to this image view.
pub(crate) fn navigate_gesture(app: &mut LightcraftApp, ui: &mut egui::Ui, resp: &egui::Response, area: Rect, img: Rect, native: [usize; 2]) -> bool {
    if !resp.hovered() {
        return false;
    }
    let (mut factor, mut delta, anchor) =
        ui.input(|i| (i.zoom_delta(), i.translation_delta(), i.multi_touch().map(|t| t.center_pos).or(i.pointer.hover_pos())));
    // A notched mouse wheel (line/page units, no modifier) zooms around the pointer, through egui's smoothed wheel
    // delta like Ctrl+wheel (so it glides instead of jumping); trackpads report points and still pan.
    let wheel_id = egui::Id::unique("loupe-wheel-zoom");
    if ui.input(|i| wheel_notches(&i.events)) != 0.0 {
        ui.data_mut(|d| d.insert_temp(wheel_id, true));
    }
    if ui.data(|d| d.get_temp::<bool>(wheel_id)).unwrap_or(false) {
        let smooth = ui.input(|i| i.smooth_scroll_delta.y);
        let speed = ui.ctx().options(|o| o.input_options.scroll_zoom_speed);
        if smooth == 0.0 {
            ui.data_mut(|d| d.remove_temp::<bool>(wheel_id));
        } else {
            factor *= (speed * smooth).exp();
            delta = egui::Vec2::ZERO;
        }
    }
    if !factor.is_finite() || factor <= 0.0 || !delta.is_finite() || (factor == 1.0 && delta == egui::Vec2::ZERO) {
        return false;
    }
    let Some(anchor) = anchor.filter(|p| area.contains(*p)) else { return false };
    let ppp = ui.ctx().pixels_per_point();
    let aspect = img.width() / img.height().max(1.0);
    let fit = fit_rect(area, aspect, Zoom::Fit, native, ppp, (0.5, 0.5));
    let fit_pct = fit.width() * ppp * 100.0 / native[0].max(1) as f32;
    let mut zoom = app.ui.zoom;
    let size = if factor != 1.0 {
        // Start at the displayed scale (including Fit/Fill and an interrupted click animation).
        let pct = (img.width() * ppp * 100.0 / native[0].max(1) as f32 * factor).clamp(fit_pct.min(800.0), 800.0);
        zoom = if pct <= fit_pct { Zoom::Fit } else { Zoom::Percent(pct) };
        fit_rect(area, aspect, zoom, native, ppp, (0.5, 0.5)).size()
    } else {
        img.size()
    };
    // The image point under the pointer remains there as its size changes, then follows the pan.
    let point = (anchor - img.min) / img.size();
    let offset = area.center() - anchor - delta;
    let pan = bounded_pan(area, size, (point.x + offset.x / size.x.max(1.0), point.y + offset.y / size.y.max(1.0)));
    let _ = app.run("view.navigate", json!({"zoom": zoom, "pan": pan}));
    ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
    ui.ctx().request_repaint();
    true
}

/// The largest texture the GPU behind `ctx` takes (egui reports it; 2048 until it does, the
/// smallest limit WebGL devices have).
///
/// The value egui keeps, not the frame's raw input: a native host (egui-winit) reports the limit
/// in the first frame's input only, so the raw value is `None` from the second frame on
/// (issue #652: every window render was cut to 2048 px, with blurry strips beside it).
pub(crate) fn texture_side(ctx: &egui::Context) -> usize {
    ctx.input(|i| i.max_texture_side)
}

/// The photo's own pixels as it is shown: its size after the crop and the user's rotation (what
/// 100 % zoom and the render size limit are measured against), at least 1 × 1.
pub(crate) fn output_px(frame: &Frame) -> [usize; 2] {
    let (w, h) = frame.native_size();
    let px = |v: f64| if v.is_finite() { v.round().clamp(1.0, 1e9) as usize } else { 1 };
    [px(w), px(h)]
}

/// The camera's focus point of `photo` in the displayed frame (its crop, rotation and flips), as the
/// normalized box over the drawn image: `None` when the photo has none or it is cropped away.
pub(crate) fn focus_box(photo: &lightcraft_catalog::Photo) -> Option<lightcraft_geom::Rect> {
    let f = photo.meta.focus.as_ref()?;
    let d = &photo.develop;
    let frame = Frame::with_lens(photo.width.max(1) as usize, photo.height.max(1) as usize, d, true, photo.embedded_lens.as_ref());
    let [w, h] = output_px(&frame);
    let to_out = frame.out_to_norm(w, h).inverse()?;
    // stored on the upright photo: the user's Rotate / Flip on top, then the crop
    let r = d.orientation.map_norm_rect(f.rect);
    let corners = [Point::new(r.x0, r.y0), Point::new(r.x1, r.y0), Point::new(r.x0, r.y1), Point::new(r.x1, r.y1)].map(|c| {
        let q = to_out.apply(c);
        (q.x / w as f64, q.y / h as f64)
    });
    let xs = corners.map(|c| c.0);
    let ys = corners.map(|c| c.1);
    let out = lightcraft_geom::Rect {
        x0: xs.iter().copied().fold(f64::INFINITY, f64::min),
        y0: ys.iter().copied().fold(f64::INFINITY, f64::min),
        x1: xs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        y1: ys.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    };
    if ![out.x0, out.y0, out.x1, out.y1].iter().all(|v| v.is_finite()) {
        return None;
    }
    // its centre must be in the frame; the box may reach past the edge (it is clipped when drawn)
    let (cx, cy) = ((out.x0 + out.x1) / 2.0, (out.y0 + out.y1) / 2.0);
    ((0.0..=1.0).contains(&cx) && (0.0..=1.0).contains(&cy)).then_some(out)
}

/// Where Toggle Zoom centres the view on the photo's focus point (`app.ui.pan`), if it has one in frame.
pub(crate) fn focus_pan(app: &LightcraftApp, id: PhotoId) -> Option<(f32, f32)> {
    let r = focus_box(app.session.catalog.photo(id)?)?;
    Some((((r.x0 + r.x1) / 2.0) as f32, ((r.y0 + r.y1) / 2.0) as f32))
}

/// Ease the loupe rect toward `target` while a click-zoom animation runs; otherwise follow it exactly.
pub(super) fn animated_rect(ctx: &egui::Context, anim: &mut bool, target: Rect) -> Rect {
    let t = if *anim { 0.22 } else { 0.0 };
    let id = egui::Id::unique("loupe_anim");
    let v = |k: &str, x: f32| ctx.animate_value_with_time(id.with(k), x, t);
    let (c, s) = (target.center(), target.size());
    let r = Rect::from_center_size(pos2(v("cx", c.x), v("cy", c.y)), vec2(v("w", s.x), v("h", s.y)));
    if *anim && (r.center() - c).abs().max_elem() < 0.5 && (r.size() - s).abs().max_elem() < 0.5 {
        *anim = false;
    }
    r
}

#[cfg(test)]
mod preview_geometry_tests {
    use super::{fit_rect, fit_texture_rect, label_badge_rect};
    use crate::state::Zoom;
    use egui::{Rect, pos2, vec2};

    /// The "Before" label sits on a dark pill that wraps the text with padding on every side.
    #[test]
    fn before_label_badge_wraps_the_text() {
        let text = vec2(40.0, 14.0);
        let pane = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let photo = Rect::from_min_size(pos2(20.0, 30.0), vec2(400.0, 300.0));
        let bg = label_badge_rect(photo, pane, text, 0.0);
        assert_eq!(bg.min, pos2(28.0, 38.0));
        assert_eq!(bg.size(), vec2(56.0, 22.0));
        assert!(photo.contains_rect(bg));
    }

    /// Zoomed in, the photo's corner is off the canvas: the pill moves to the canvas' corner
    /// instead of leaving with it (issue #2267), on whichever axes the photo reaches past.
    #[test]
    fn before_label_badge_stays_in_the_pane_when_the_photo_reaches_past_it() {
        let text = vec2(40.0, 14.0);
        let pane = Rect::from_min_size(pos2(100.0, 50.0), vec2(800.0, 600.0));
        let both = label_badge_rect(Rect::from_min_size(pos2(-900.0, -700.0), vec2(3000.0, 2000.0)), pane, text, 0.0);
        assert_eq!(both.min, pos2(108.0, 58.0));
        assert!(pane.contains_rect(both));
        let left_only = label_badge_rect(Rect::from_min_size(pos2(-900.0, 120.0), vec2(3000.0, 400.0)), pane, text, 0.0);
        assert_eq!(left_only.min, pos2(108.0, 128.0));
        assert!(pane.contains_rect(left_only));
    }

    /// With a filter active, the pill in the canvas' corner has that corner: the label sits below
    /// it, whether the photo is past the pane or just under the pill. A photo lower down is left
    /// alone. A wide label near the pane's right edge stays inside the pane.
    #[test]
    fn before_label_badge_keeps_clear_of_the_filter_pill_and_the_right_edge() {
        let text = vec2(40.0, 14.0);
        let pane = Rect::from_min_size(pos2(100.0, 50.0), vec2(800.0, 600.0));
        let inset = super::FILTER_PILL_INSET + super::FILTER_PILL_HEIGHT;
        let past = label_badge_rect(Rect::from_min_size(pos2(-900.0, -700.0), vec2(3000.0, 2000.0)), pane, text, inset);
        assert_eq!(past.min, pos2(108.0, 50.0 + inset + 8.0));
        let under = label_badge_rect(Rect::from_min_size(pos2(200.0, 60.0), vec2(400.0, 300.0)), pane, text, inset);
        assert_eq!(under.min.y, 50.0 + inset + 8.0, "a photo whose corner is under the pill moves down too");
        let lower = label_badge_rect(Rect::from_min_size(pos2(200.0, 200.0), vec2(400.0, 300.0)), pane, text, inset);
        assert_eq!(lower.min, pos2(208.0, 208.0));
        let wide = label_badge_rect(Rect::from_min_size(pos2(860.0, 200.0), vec2(400.0, 300.0)), pane, text, 0.0);
        assert!(pane.contains_rect(wide), "{wide:?}");
        assert_eq!(wide.max.x, pane.right() - 8.0, "8 points from the pane's right edge, like the left inset");
    }

    #[test]
    fn portrait_preview_keeps_its_ratio_inside_a_landscape_frame() {
        let frame = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 200.0));
        let image = fit_texture_rect(frame, [100, 200]);

        assert!((image.width() / image.height() - 0.5).abs() < 1e-6);
        assert_eq!(image.center(), frame.center());
        assert!(frame.contains_rect(image));
    }

    /// A 100 % view must put the photo's corner on a whole physical pixel, or the GPU's bilinear
    /// filter softens it (a 1279.5 pt canvas on a 2x display centred the photo half a pixel off).
    #[test]
    fn percent_zoom_lands_on_whole_physical_pixels() {
        let area = Rect::from_min_size(pos2(0.0, 42.0), vec2(1279.5, 726.0));
        for ppp in [1.0, 1.25, 2.0, 3.0] {
            for pan in [(0.5, 0.5), (0.3, 0.7), (0.4999, 0.5001)] {
                let image = fit_rect(area, 5152.0 / 7728.0, Zoom::Percent(100.0), [5152, 7728], ppp, pan);
                for v in [image.min.x, image.min.y] {
                    assert!((v * ppp - (v * ppp).round()).abs() < 1e-3, "{v} pt is not a whole pixel at {ppp}x, pan {pan:?}");
                }
                assert!((image.width() * ppp - 5152.0).abs() < 1e-3, "size stays the photo's own pixels");
            }
        }
    }

    #[test]
    fn portrait_preview_aspect_fits_detail_area_without_stretching() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 200.0));
        let image = fit_rect(area, 0.5, Zoom::Fit, [4000, 6000], 1.0, (0.5, 0.5));

        assert!((image.width() / image.height() - 0.5).abs() < 1e-6);
        assert_eq!(image.size(), vec2(100.0, 200.0));
        assert_eq!(image.center(), area.center());
    }
}

#[cfg(test)]
mod wheel_zoom_tests {
    use super::*;

    fn wheel(unit: egui::MouseWheelUnit, y: f32, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::MouseWheel { unit, delta: egui::vec2(0.0, y), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers }
    }

    #[test]
    fn only_notched_unmodified_wheels_count() {
        let m = egui::Modifiers::NONE;
        assert_eq!(wheel_notches(&[wheel(egui::MouseWheelUnit::Line, 1.0, m), wheel(egui::MouseWheelUnit::Line, 2.0, m)]), 3.0);
        assert_eq!(wheel_notches(&[wheel(egui::MouseWheelUnit::Point, 40.0, m)]), 0.0);
        assert_eq!(wheel_notches(&[wheel(egui::MouseWheelUnit::Line, 1.0, egui::Modifiers::CTRL)]), 0.0);
        assert_eq!(wheel_notches(&[wheel(egui::MouseWheelUnit::Line, f32::NAN, m)]), 0.0);
    }
}

#[cfg(test)]
mod snap_tests {
    use super::*;

    /// A texture drawn at its own size lands on whole physical pixels at exactly that size (no
    /// bilinear blur); a texture of another size (zoomed, a stand-in) keeps the rect it was given.
    #[test]
    fn a_fitted_texture_is_drawn_on_whole_pixels_at_its_own_size() {
        let r = Rect::from_min_size(pos2(394.3, 130.5), vec2(1088.4, 720.2));
        let s = snap_fit_rect(r, [1088, 720], 1.0);
        assert_eq!((s.min.x, s.min.y, s.width(), s.height()), (394.0, 131.0, 1088.0, 720.0));
        // at 1.5 points per pixel the snap is to physical pixels and the size stays in points
        let hi = snap_fit_rect(Rect::from_min_size(pos2(100.3, 50.2), vec2(725.4, 480.1)), [1088, 720], 1.5);
        assert!(((hi.min.x * 1.5).fract()).abs() < 1e-3 && (hi.width() - 1088.0 / 1.5).abs() < 1e-3, "{hi:?}");
        let other = snap_fit_rect(r, [2560, 1694], 1.0);
        assert_eq!(other, r);
        let bad = snap_fit_rect(r, [1088, 720], f32::NAN);
        assert_eq!((bad.width(), bad.height()), (1088.0, 720.0));
    }
}
