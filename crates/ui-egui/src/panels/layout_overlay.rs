//! View ▸ Loupe Overlay ▸ Layout Image, as in Lightroom Classic: a graphic with transparency (a
//! magazine cover's masthead and cover lines, a web banner's safe areas, a print layout's
//! margins) drawn over the photo in the loupe, so a shot can be judged against the page it is made
//! for. The graphic is fitted inside the photo and centred, and follows zoom and pan with it.
//! `opacity` fades it; `matte` darkens the photo outside its box, where the page would crop it.
//!
//! The file is read and decoded off the UI thread, once per path and Choose Layout Image… (an
//! offline drive never holds up a frame); until it arrives nothing is drawn, and a file that can't
//! be read says so at the canvas' bottom left. Graphics larger than [`MAX_SIDE`] are scaled down
//! once, premultiplied so transparent edges stay clean.

use std::sync::{Arc, Mutex, PoisonError};

use egui::{Color32, Rect, pos2, vec2};
use lightcraft_raster::Rgba8;

use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// The longest side a layout image keeps (a screen never shows more of it).
pub const MAX_SIDE: usize = 4096;

/// The decoded graphic once the worker is done, or why it couldn't be read.
type Loaded = Arc<Mutex<Option<Result<Rgba8, String>>>>;

/// What the loupe keeps between frames (in egui's temporary memory).
#[derive(Clone)]
struct Cache {
    /// The (path, revision) the file was read for.
    file: (String, u32),
    loaded: Loaded,
    /// The texture made from the graphic, and the display profile it was made for.
    tex: Option<(Option<u64>, egui::TextureHandle)>,
}

/// The rectangle a `size` (pixels) graphic takes fitted inside `photo` and centred on it.
pub fn fit(photo: Rect, size: [usize; 2]) -> Rect {
    let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
    let scale = (photo.width() / w).min(photo.height() / h).max(0.0);
    Rect::from_center_size(photo.center(), vec2(w * scale, h * scale))
}

/// The parts of `photo` outside `inner` (the matte): up to four strips.
pub fn outside(photo: Rect, inner: Rect) -> Vec<Rect> {
    let inner = inner.intersect(photo);
    let strips = [
        Rect::from_min_max(photo.min, pos2(inner.left(), photo.bottom())),
        Rect::from_min_max(pos2(inner.right(), photo.top()), photo.max),
        Rect::from_min_max(pos2(inner.left(), photo.top()), pos2(inner.right(), inner.top())),
        Rect::from_min_max(pos2(inner.left(), inner.bottom()), pos2(inner.right(), photo.bottom())),
    ];
    strips.into_iter().filter(|r| r.width() > 0.0 && r.height() > 0.0).collect()
}

/// `img` with its longest side at most `max` pixels (scaled premultiplied, so transparent edges
/// keep their colour); an empty graphic is an error.
pub fn fit_within(img: Rgba8, max: usize) -> Result<Rgba8, String> {
    let (w, h) = (img.width, img.height);
    if w == 0 || h == 0 {
        return Err("the layout image is empty".into());
    }
    let max = max.max(1);
    if w.max(h) <= max {
        return Ok(img);
    }
    let s = max as f32 / w.max(h) as f32;
    let (nw, nh) = (((w as f32 * s).round() as usize).clamp(1, max), ((h as f32 * s).round() as usize).clamp(1, max));
    let pre: lightcraft_raster::Image<[f32; 4]> = lightcraft_raster::Image::from_fn(w, h, |x, y| {
        let p = img.get(x, y);
        let a = p[3] as f32 / 255.0;
        [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, a]
    });
    let scaled = lightcraft_raster::resample::resize(&pre, nw, nh, lightcraft_raster::resample::Filter::Mitchell);
    Ok(Rgba8::from_fn(nw, nh, |x, y| {
        let [r, g, b, a] = scaled.get(x, y);
        let a = a.clamp(0.0, 1.0);
        let c = |v: f32| if a <= 1e-4 { 0 } else { (v / a).round().clamp(0.0, 255.0) as u8 };
        [c(r), c(g), c(b), (a * 255.0).round() as u8]
    }))
}

/// Reads and decodes `path` on a worker thread (in place on the web, which has no threads and no
/// files to read: the error says so).
fn start(ctx: &egui::Context, path: &str) -> Loaded {
    let loaded: Loaded = Default::default();
    let max = match ctx.input(|i| i.max_texture_side) {
        0 => MAX_SIDE,
        m => m.min(MAX_SIDE),
    };
    let (out, repaint, path) = (loaded.clone(), ctx.clone(), path.to_string());
    let work = move || {
        let r = lightcraft_engine::export::read_graphic(&path).and_then(|img| fit_within(img, max));
        *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
        repaint.request_repaint();
    };
    #[cfg(not(target_arch = "wasm32"))]
    if let Err(e) = std::thread::Builder::new().name("lc-layout-image".into()).spawn(work) {
        *loaded.lock().unwrap_or_else(PoisonError::into_inner) = Some(Err(format!("can't start reading the layout image: {e}")));
    }
    #[cfg(target_arch = "wasm32")]
    work();
    loaded
}

/// Draws the layout image (when shown) over `photo`, the photo's rectangle on screen; `p` clips it
/// to the canvas.
pub fn draw(app: &LightcraftApp, p: &egui::Painter, photo: Rect) {
    let li = &app.ui.layout_image;
    let path = li.path.trim();
    if !li.show || path.is_empty() || photo.width() < 1.0 || photo.height() < 1.0 {
        return;
    }
    let ctx = p.ctx().clone();
    let id = egui::Id::new("loupe-layout-image");
    let file = (path.to_string(), li.revision);
    let mut cache = match ctx.data(|d| d.get_temp::<Cache>(id)) {
        Some(c) if c.file == file => c,
        _ => Cache { loaded: start(&ctx, path), file, tex: None },
    };
    let display_id = app.renderer.display_id();
    if cache.tex.as_ref().is_none_or(|(d, _)| *d != display_id) {
        let made = match cache.loaded.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            // still reading: the worker repaints when it is done
            None => None,
            Some(Err(e)) => Some(Err(e.clone())),
            Some(Ok(img)) => {
                let mut img = img.clone();
                // shown through the monitor profile like every other preview
                if let Some(d) = app.renderer.display()
                    && let Err(e) = d.profile.from_srgb(&mut img)
                {
                    log::warn!("layout image, display profile: {e}");
                }
                let color = egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.as_bytes());
                Some(Ok(ctx.load_texture("loupe-layout-image", color, egui::TextureOptions::LINEAR)))
            }
        };
        match made {
            None => {}
            Some(Ok(tex)) => cache.tex = Some((display_id, tex)),
            Some(Err(e)) => note(p, &e),
        }
    }
    if let Some((_, tex)) = &cache.tex {
        let r = fit(photo, tex.size());
        let matte = li.matte.clamp(0.0, 1.0);
        if matte > 0.0 {
            let veil = Color32::from_black_alpha((matte * 255.0).round() as u8);
            for strip in outside(photo, r) {
                p.rect_filled(strip, 0.0, veil);
            }
        }
        let alpha = (li.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        p.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::from_white_alpha(alpha));
        register(&ctx, "canvas:layoutImage", r.intersect(p.clip_rect()));
    }
    ctx.data_mut(|d| d.insert_temp(id, cache));
}

/// Why the layout image isn't shown, at the canvas' bottom left.
fn note(p: &egui::Painter, why: &str) {
    let t = Tokens::get(p.ctx());
    let canvas = p.clip_rect();
    let lines = [crate::i18n::tr("The layout image can't be shown"), why];
    let mut y = canvas.bottom() - 14.0;
    for (i, l) in lines.iter().enumerate().rev() {
        let font = if i == 0 { t.semibold(13.0) } else { t.font(12.0) };
        let g = p.layout_no_wrap(l.to_string(), font, Color32::WHITE);
        y -= g.size().y;
        let at = pos2(canvas.left() + 14.0, y);
        p.galley(at + vec2(1.0, 1.0), g.clone(), Color32::from_black_alpha(200));
        p.galley(at, g, Color32::WHITE);
        y -= 3.0;
    }
    register(p.ctx(), "canvas:layoutImageError", Rect::from_min_max(pos2(canvas.left(), y), pos2(canvas.left() + 360.0, canvas.bottom())));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_graphic_fits_inside_the_photo_centred() {
        let photo = Rect::from_min_size(pos2(100.0, 50.0), vec2(600.0, 400.0));
        // a portrait cover over a landscape photo: as tall as the photo, centred across
        let r = fit(photo, [800, 1000]);
        assert!((r.height() - 400.0).abs() < 1e-3 && (r.width() - 320.0).abs() < 1e-3, "{r:?}");
        assert!((r.center().x - photo.center().x).abs() < 1e-3 && (r.center().y - photo.center().y).abs() < 1e-3);
        // the same shape fills it
        let same = fit(photo, [1200, 800]);
        assert!((same.width() - 600.0).abs() < 1e-3 && (same.height() - 400.0).abs() < 1e-3);
        // a zero-size graphic never divides by zero
        assert!(fit(photo, [0, 0]).width().is_finite());
    }

    #[test]
    fn the_matte_covers_the_photo_outside_the_graphic_only() {
        let photo = Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0));
        let r = fit(photo, [800, 1000]);
        let strips = outside(photo, r);
        assert_eq!(strips.len(), 2, "{strips:?}");
        let area: f32 = strips.iter().map(|s| s.area()).sum();
        assert!((area - (photo.area() - r.area())).abs() < 1e-2);
        assert!(strips.iter().all(|s| s.intersect(r).area() < 1e-3));
        // a graphic of the photo's own shape leaves nothing to darken
        assert!(outside(photo, fit(photo, [3, 2])).is_empty());
    }

    #[test]
    fn large_graphics_are_scaled_down_keeping_shape_and_transparency() {
        // a transparent graphic with an opaque red bar across the middle
        let img = Rgba8::from_fn(5000, 2500, |_, y| if (1000..1500).contains(&y) { [255, 0, 0, 255] } else { [0, 0, 0, 0] });
        let out = fit_within(img, 1000).unwrap();
        assert_eq!((out.width, out.height), (1000, 500));
        assert_eq!(out.get(500, 250), [255, 0, 0, 255]);
        assert_eq!(out.get(500, 20)[3], 0);
        // small graphics are kept as they are, empty ones are refused
        let small = Rgba8::from_fn(10, 4, |x, _| [x as u8, 0, 0, 255]);
        assert_eq!(fit_within(small.clone(), 1000).unwrap().data, small.data);
        assert!(fit_within(Rgba8::new(0, 0), 1000).is_err());
    }
}
