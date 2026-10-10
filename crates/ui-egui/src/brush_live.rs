//! The brush stroke being painted, drawn on the photo while the button is held (issue #517).
//!
//! The committed stroke is rasterized by the pipeline ([`lightcraft_pipeline::masks::brush_dabs`]
//! and friends): dabs a quarter radius apart along the path, each with a hard core and a smooth
//! feather, flow accumulating up to density. The live stroke places and paints its dabs with the
//! same functions, at screen resolution, so letting go of the button doesn't change what's
//! covered. It's incremental: each frame stamps only the dabs of the samples added since the last
//! one and uploads only the rectangle they touched.

use egui::{Color32, ColorImage, Pos2, Rect, TextureHandle, TextureOptions, pos2};
use lightcraft_geom::Point;
use lightcraft_pipeline::masks::{accumulate, dab_alpha, push_dabs};

/// The largest live-stroke buffer edge (pixels); a bigger canvas is painted at a lower resolution.
const MAX_SIDE: usize = 4096;

/// What a stroke is painted with; a change (a new canvas size, zoom, brush) repaints it from its
/// first sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveParams {
    /// The area the stroke may cover (screen points): the canvas.
    pub clip: Rect,
    /// Buffer pixels per screen point.
    pub scale: f32,
    /// Brush radius (screen points) and its hard core (no feather), as the pipeline computes them.
    pub r: f32,
    pub hard: f32,
    /// Flow and density, 0..1.
    pub flow: f32,
    pub density: f32,
    /// Overlay colour and its opacity (0..1) at full coverage.
    pub color: [u8; 3],
    pub opacity: f32,
}

/// An in-progress stroke's coverage at screen resolution and the texture showing it.
pub struct LiveStroke {
    params: LiveParams,
    w: usize,
    h: usize,
    alpha: Vec<f32>,
    /// Samples stamped so far, and the last one (buffer pixels).
    placed: usize,
    last: Option<Point>,
    /// Buffer pixels painted since the last upload: (x0, y0, x1, y1), exclusive ends.
    dirty: Option<(usize, usize, usize, usize)>,
    texture: Option<TextureHandle>,
}

impl LiveStroke {
    fn new(params: LiveParams) -> LiveStroke {
        let scale = buffer_scale(params.clip, params.scale);
        let params = LiveParams { scale, ..params };
        let (w, h) = buffer_size(params.clip, scale);
        LiveStroke { params, w, h, alpha: vec![0.0; w * h], placed: 0, last: None, dirty: None, texture: None }
    }

    /// Stroke coverage (0..1) at buffer pixel `(x, y)`; 0 outside.
    pub fn coverage(&self, x: usize, y: usize) -> f32 {
        if x >= self.w {
            return 0.0;
        }
        y.checked_mul(self.w).and_then(|i| i.checked_add(x)).and_then(|i| self.alpha.get(i)).copied().unwrap_or(0.0)
    }

    /// Buffer size (pixels) and pixels per screen point.
    pub fn size(&self) -> ([usize; 2], f32) {
        ([self.w, self.h], self.params.scale)
    }

    /// Stamp the dabs of `points` (screen positions of the stroke's samples so far) not yet
    /// painted. Returns `true` when anything new was painted.
    pub fn stamp(&mut self, points: &[Pos2]) -> bool {
        let Some(new) = points.get(self.placed..) else { return false };
        if new.is_empty() {
            return false;
        }
        let LiveParams { clip, scale, r, hard, flow, density, .. } = self.params;
        let (r, hard) = ((r * scale) as f64, (hard * scale) as f64);
        let mut dabs = Vec::new();
        for q in new {
            let q = Point::new(((q.x - clip.left()) * scale) as f64, ((q.y - clip.top()) * scale) as f64);
            push_dabs(&mut dabs, self.last, q, r);
            self.last = Some(q);
        }
        self.placed = points.len();
        for d in dabs {
            if !(d.x.is_finite() && d.y.is_finite()) {
                continue;
            }
            let x0 = (d.x - r).floor().clamp(0.0, self.w as f64) as usize;
            let x1 = (d.x + r).ceil().clamp(0.0, self.w as f64) as usize;
            let y0 = (d.y - r).floor().clamp(0.0, self.h as f64) as usize;
            let y1 = (d.y + r).ceil().clamp(0.0, self.h as f64) as usize;
            if x0 >= x1 || y0 >= y1 {
                continue;
            }
            for y in y0..y1 {
                let Some(row) = self.alpha.get_mut(y * self.w + x0..y * self.w + x1) else { continue };
                for (x, v) in (x0..x1).zip(row.iter_mut()) {
                    let dd = Point::new(x as f64 + 0.5, y as f64 + 0.5).dist(d);
                    if dd > r {
                        continue;
                    }
                    *v = accumulate(*v, dab_alpha(dd, r, hard), flow, density);
                }
            }
            self.dirty = Some(match self.dirty {
                Some((a, b, c, e)) => (a.min(x0), b.min(y0), c.max(x1), e.max(y1)),
                None => (x0, y0, x1, y1),
            });
        }
        true
    }

    /// The overlay colour of coverage `a`.
    fn pixel(&self, a: f32) -> Color32 {
        let [r, g, b] = self.params.color;
        let alpha = (a * self.params.opacity * 255.0).round().clamp(0.0, 255.0) as u8;
        Color32::from_rgba_unmultiplied(r, g, b, alpha)
    }

    fn region(&self, (x0, y0, x1, y1): (usize, usize, usize, usize)) -> ColorImage {
        let mut px = Vec::with_capacity((x1 - x0) * (y1 - y0));
        for y in y0..y1 {
            for x in x0..x1 {
                px.push(self.pixel(self.coverage(x, y)));
            }
        }
        ColorImage::new([x1 - x0, y1 - y0], px)
    }

    /// Bring the texture up to date (the whole buffer the first time, then only what changed).
    fn upload(&mut self, ctx: &egui::Context) -> Option<&TextureHandle> {
        match (&mut self.texture, self.dirty.take()) {
            (None, _) => {
                let image = self.region((0, 0, self.w, self.h));
                self.texture = Some(ctx.load_texture("brush-live-stroke", image, TextureOptions::LINEAR));
            }
            (Some(_), Some(rect)) => {
                let image = self.region(rect);
                if let Some(t) = &mut self.texture {
                    t.set_partial([rect.0, rect.1], image, TextureOptions::LINEAR);
                }
            }
            (Some(_), None) => {}
        }
        self.texture.as_ref()
    }
}

/// Pixels per screen point of a buffer covering `clip`: `want`, lowered so no edge exceeds
/// [`MAX_SIDE`].
fn buffer_scale(clip: Rect, want: f32) -> f32 {
    let want = if want.is_finite() && want > 0.0 { want } else { 1.0 };
    let long = clip.width().max(clip.height());
    if long.is_finite() && long * want > MAX_SIDE as f32 { MAX_SIDE as f32 / long } else { want }
}

fn buffer_size(clip: Rect, scale: f32) -> (usize, usize) {
    let side = |v: f32| {
        let v = (v * scale).ceil();
        if v.is_finite() { (v.max(1.0) as usize).min(MAX_SIDE) } else { 1 }
    };
    (side(clip.width()), side(clip.height()))
}

/// Paint the live stroke whose samples so far are `points` (screen positions) into `painter`,
/// keeping its buffer in `slot` (a new stroke, or new parameters, start it afresh).
pub fn paint(slot: &mut Option<LiveStroke>, ctx: &egui::Context, painter: &egui::Painter, params: LiveParams, points: &[Pos2]) {
    let fresh = match slot {
        Some(s) => {
            // the same stroke when painted with the same parameters (at the scale it settled on)
            let same = s.params == LiveParams { scale: buffer_scale(params.clip, params.scale), ..params };
            !same || points.len() < s.placed
        }
        None => true,
    };
    if fresh {
        *slot = Some(LiveStroke::new(params));
    }
    let Some(s) = slot else { return };
    s.stamp(points);
    let clip = s.params.clip;
    // the buffer's pixels may overhang the canvas by a fraction of a pixel: map them exactly
    let (w, h, scale) = (s.w as f32, s.h as f32, s.params.scale);
    let rect = Rect::from_min_size(clip.min, egui::vec2(w / scale, h / scale));
    if let Some(t) = s.upload(ctx) {
        painter.image(t.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    fn params() -> LiveParams {
        LiveParams {
            clip: Rect::from_min_size(Pos2::ZERO, vec2(400.0, 200.0)),
            scale: 1.0,
            r: 10.0,
            hard: 5.0,
            flow: 1.0,
            density: 1.0,
            color: [230, 30, 40],
            opacity: 0.5,
        }
    }

    /// Issue #517: two samples far apart (a fast stroke) paint a continuous band between them,
    /// not two separate dabs.
    #[test]
    fn a_fast_segment_is_painted_continuously() {
        let mut s = LiveStroke::new(params());
        s.stamp(&[pos2(20.0, 100.0)]);
        s.stamp(&[pos2(20.0, 100.0), pos2(380.0, 100.0)]);
        for x in 20..380 {
            assert!(s.coverage(x, 100) > 0.99, "a gap at x = {x}: {}", s.coverage(x, 100));
        }
        // the feather: full inside the core, fading to nothing at the radius
        assert!(s.coverage(200, 104) > 0.99);
        assert!((0.01..0.99).contains(&s.coverage(200, 108)), "{}", s.coverage(200, 108));
        assert_eq!(s.coverage(200, 111), 0.0);
    }

    /// Painting the samples one frame at a time gives the same coverage as all at once (what the
    /// committed stroke does).
    #[test]
    fn incremental_stamping_matches_painting_the_whole_path() {
        let pts = [pos2(30.0, 40.0), pos2(200.0, 150.0), pos2(210.0, 152.0), pos2(390.0, 20.0)];
        let mut once = LiveStroke::new(LiveParams { flow: 0.4, ..params() });
        once.stamp(&pts);
        let mut steps = LiveStroke::new(LiveParams { flow: 0.4, ..params() });
        for n in 1..=pts.len() {
            steps.stamp(&pts[..n]);
        }
        assert_eq!(once.alpha, steps.alpha);
    }

    #[test]
    fn hostile_input_paints_nothing_and_does_not_panic() {
        let mut s = LiveStroke::new(params());
        s.stamp(&[pos2(f32::NAN, 3.0), pos2(f32::INFINITY, -1e30), pos2(-5000.0, 9000.0)]);
        assert!(s.alpha.iter().all(|a| *a == 0.0));
        // an enormous canvas is painted at a capped resolution
        let big = LiveStroke::new(LiveParams { clip: Rect::from_min_size(Pos2::ZERO, vec2(1e6, 10.0)), scale: 2.0, ..params() });
        assert!(big.w <= MAX_SIDE && big.h >= 1);
    }
}
