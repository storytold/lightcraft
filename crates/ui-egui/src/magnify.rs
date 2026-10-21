//! Zoomed past 100 %, a window render holds the photo's own pixels and is drawn larger than it is.
//! The GPU's bilinear filter stretches it soft; on top of that stretch goes a copy of the part on
//! screen resampled to the screen's pixels (Catmull-Rom, the render's own resampler), drawn texel
//! for texel. Sharpening has already run at the photo's pixels, so this is the order Lightroom's
//! zoomed view has: sharpen at full size, then magnify.
//!
//! The copy covers the visible part with a margin around it and is made on a worker thread: a pan
//! within the margin only moves it (no work in the frame), and a pan past it or a new zoom shows
//! the plain stretch until the next copy is ready, so dragging the view never waits for a resample.

use std::sync::{Arc, Mutex, PoisonError};

use egui::{Color32, ColorImage, Context, Rect, TextureHandle, TextureOptions, pos2, vec2};
use lightcraft_raster::Image;
use lightcraft_raster::resample::{Filter, resize_window};

/// Below this magnification the GPU's filter is close enough (and the window is about 1:1).
const MIN_MAGNIFY: f32 = 1.05;

/// Largest magnified copy made, in pixels.
const MAX_PIXELS: usize = 24 << 20;

/// Margin around the visible part, as a fraction of its size on each side.
const MARGIN: f64 = 0.5;

/// What a copy is of: the source texture and its content, the magnification and the span of the
/// tile's pixels it covers (`[x0, y0, x1, y1]`).
#[derive(Clone, Copy, PartialEq)]
struct Of {
    texture: egui::TextureId,
    content: u64,
    scale: [f32; 2],
    span: [f64; 4],
}

impl Of {
    fn same_source(&self, o: &Of) -> bool {
        self.texture == o.texture && self.content == o.content && self.scale.iter().zip(o.scale).all(|(a, b)| (a / b - 1.0).abs() < 1e-4)
    }
    fn covers(&self, span: [f64; 4]) -> bool {
        let [x0, y0, x1, y1] = self.span;
        x0 <= span[0] + 0.01 && y0 <= span[1] + 0.01 && x1 >= span[2] - 0.01 && y1 >= span[3] - 0.01
    }
}

#[derive(Clone)]
struct Ready {
    of: Of,
    tex: TextureHandle,
    size: [usize; 2],
}

#[derive(Clone)]
struct Pending {
    of: Of,
    result: Arc<Mutex<Option<ColorImage>>>,
}

#[derive(Clone, Default)]
struct Cache {
    ready: Option<Ready>,
    pending: Option<Pending>,
}

fn cache_id(slot: &str) -> egui::Id {
    egui::Id::new(("magnify", slot))
}

/// The magnified texture last drawn for `slot`, if any.
pub fn texture_of(ctx: &Context, slot: &str) -> Option<egui::TextureId> {
    ctx.data(|d| d.get_temp::<Cache>(cache_id(slot))).and_then(|c| c.ready).map(|r| r.tex.id())
}

/// Draw the `size` texture `tex` (CPU copy `pixels`, render key `content`) at `dst` (points),
/// clipped to `clip`, with a sharp magnified copy of the part on screen over it when it is drawn
/// larger than its pixels. `slot` names the cache (one per window view).
#[allow(clippy::too_many_arguments)]
pub fn draw(
    ctx: &Context,
    p: &egui::Painter,
    slot: &str,
    tex: egui::TextureId,
    content: u64,
    size: [usize; 2],
    pixels: Option<&ColorImage>,
    dst: Rect,
    clip: Rect,
    ppp: f32,
) {
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    p.image(tex, dst, uv, Color32::WHITE);
    let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
    let scale = [dst.width() * ppp / w, dst.height() * ppp / h];
    let Some(pixels) = pixels.filter(|px| px.size == size) else { return };
    if !ppp.is_finite() || ppp <= 0.0 || !scale.iter().all(|s| s.is_finite() && *s >= MIN_MAGNIFY) {
        return;
    }
    // the tile's pixels on screen
    let shown = dst.intersect(clip);
    if shown.width() <= 0.0 || shown.height() <= 0.0 {
        return;
    }
    let to_tile = |x: f32, y: f32| (((x - dst.left()) / dst.width() * w) as f64, ((y - dst.top()) / dst.height() * h) as f64);
    let (a, b) = (to_tile(shown.left(), shown.top()), to_tile(shown.right(), shown.bottom()));
    let visible = [a.0, a.1, b.0, b.1];
    let want = Of { texture: tex, content, scale, span: visible };

    let id = cache_id(slot);
    let mut cache = ctx.data(|d| d.get_temp::<Cache>(id)).unwrap_or_default();
    // a copy the worker finished becomes the one drawn
    let finished = cache.pending.as_ref().and_then(|j| j.result.lock().unwrap_or_else(PoisonError::into_inner).take().map(|img| (j.of, img)));
    if let Some((of, image)) = finished {
        let size = image.size;
        let tex = match cache.ready.take() {
            Some(mut r) => {
                r.tex.set(image, TextureOptions::NEAREST);
                r.tex
            }
            None => ctx.load_texture(format!("magnified {slot}"), image, TextureOptions::NEAREST),
        };
        cache.ready = Some(Ready { of, tex, size });
        cache.pending = None;
    }
    let covered = cache.ready.as_ref().is_some_and(|r| r.of.same_source(&want) && r.of.covers(visible));
    // a new copy when this one does not cover the view, one job at a time
    if !covered && cache.pending.is_none() {
        let margin = [(visible[2] - visible[0]) * MARGIN, (visible[3] - visible[1]) * MARGIN];
        let span = [
            (visible[0] - margin[0]).max(0.0),
            (visible[1] - margin[1]).max(0.0),
            (visible[2] + margin[0]).min(f64::from(w)),
            (visible[3] + margin[1]).min(f64::from(h)),
        ];
        let out = [((span[2] - span[0]) * f64::from(scale[0])).round() as usize, ((span[3] - span[1]) * f64::from(scale[1])).round() as usize];
        if out[0] > 0 && out[1] > 0 && out[0].saturating_mul(out[1]) <= MAX_PIXELS {
            let result = Arc::new(Mutex::new(None));
            spawn(ctx.clone(), crop(pixels, span), out, result.clone());
            cache.pending = Some(Pending { of: Of { span, ..want }, result });
        }
    }
    // the copy over the stretch, its pixels on whole screen pixels
    if let Some(r) = cache.ready.as_ref().filter(|r| r.of.same_source(&want)) {
        let origin = pos2((dst.left() * ppp + r.of.span[0] as f32 * scale[0]).round(), (dst.top() * ppp + r.of.span[1] as f32 * scale[1]).round());
        let at = Rect::from_min_size(pos2(origin.x / ppp, origin.y / ppp), vec2(r.size[0] as f32 / ppp, r.size[1] as f32 / ppp));
        p.image(r.tex.id(), at, uv, Color32::WHITE);
    }
    ctx.data_mut(|d| d.insert_temp(id, cache));
}

/// The tile's pixels a copy of `span` reads (Catmull-Rom: two past each edge), and the span
/// relative to them.
fn crop(pixels: &ColorImage, span: [f64; 4]) -> (Image<[f32; 4]>, [f64; 4]) {
    let (pw, ph) = (pixels.size[0], pixels.size[1]);
    let range = |lo: f64, hi: f64, n: usize| {
        let first = (lo.floor() - 3.0).clamp(0.0, n.saturating_sub(1) as f64) as usize;
        let end = ((hi.ceil() + 3.0).max(0.0) as usize).clamp(first + 1, n.max(1));
        (first, end)
    };
    let ((x0, x1), (y0, y1)) = (range(span[0], span[2], pw), range(span[1], span[3], ph));
    let src = Image::from_fn(x1 - x0, y1 - y0, |x, y| pixels.pixels.get((y0 + y) * pw + x0 + x).map_or([0.0; 4], |c| c.to_array().map(f32::from)));
    let (ox, oy) = (x0 as f64, y0 as f64);
    (src, [span[0] - ox, span[1] - oy, span[2] - ox, span[3] - oy])
}

/// Resample `src`'s `span` to `out` pixels off the UI thread (inline in the browser build), then
/// ask for a frame.
fn spawn(ctx: Context, (src, span): (Image<[f32; 4]>, [f64; 4]), out: [usize; 2], result: Arc<Mutex<Option<ColorImage>>>) {
    let job = move || {
        let img = resize_window(&src, (span[0], span[2]), (span[1], span[3]), out[0], out[1], Filter::CatmullRom);
        let px = img
            .data
            .iter()
            .map(|v| v.map(|c| c.round().clamp(0.0, 255.0) as u8))
            .map(|[r, g, b, a]| Color32::from_rgba_premultiplied(r, g, b, a))
            .collect();
        *result.lock().unwrap_or_else(PoisonError::into_inner) = Some(ColorImage::new(out, px));
        ctx.request_repaint();
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(job);
    #[cfg(target_arch = "wasm32")]
    job();
}
