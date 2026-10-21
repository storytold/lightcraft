//! The slide composer: [`layout`] (where the image cell, the photo, its frame and shadow go) and
//! [`compose`] (the slide's pixels, on the CPU).
//!
//! Drawing order: backdrop colour → background image (at its opacity, filling the slide) → colour
//! wash → cast shadow → stroke border → photo. The stroke is drawn around the outside of the
//! photo (the photo keeps its size); the shadow falls from the photo with its stroke.

use lightcraft_raster::blur::gaussian;
use lightcraft_raster::resample::{Filter, resize};
use lightcraft_raster::{Image, Plane, Rgba8, par_rows};

use crate::settings::{MAX_SIDE, SlideSettings};

/// A rectangle in slide pixels (`x0 ≤ x1`, `y0 ≤ y1`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }
    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }
    fn intersect(&self, o: &Rect) -> Rect {
        let (x0, y0) = (self.x0.max(o.x0), self.y0.max(o.y0));
        Rect { x0, y0, x1: self.x1.min(o.x1).max(x0), y1: self.y1.min(o.y1).max(y0) }
    }
    fn expand(&self, d: f64) -> Rect {
        Rect { x0: self.x0 - d, y0: self.y0 - d, x1: self.x1 + d, y1: self.y1 + d }
    }
    fn shift(&self, dx: f64, dy: f64) -> Rect {
        Rect { x0: self.x0 + dx, y0: self.y0 + dy, x1: self.x1 + dx, y1: self.y1 + dy }
    }
    /// Whole pixel bounds clipped to `w × h`: (x0, y0, x1, y1).
    fn pixels(&self, w: usize, h: usize) -> (usize, usize, usize, usize) {
        let c = |v: f64, max: usize| if v.is_finite() { v.round().clamp(0.0, max as f64) as usize } else { 0 };
        let (x0, y0) = (c(self.x0, w), c(self.y0, h));
        (x0, y0, c(self.x1, w).max(x0), c(self.y1, h).max(y0))
    }
}

/// Where a slide's parts go, in slide pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// The slide.
    pub slide: Rect,
    /// The image cell: the slide inside its margins.
    pub cell: Rect,
    /// The whole photo, scaled (with zoom to fill it extends past the cell).
    pub photo: Rect,
    /// The part of the photo that shows (the photo inside the cell).
    pub visible: Rect,
    /// The visible photo with its stroke border (the visible photo when there is none).
    pub frame: Rect,
    /// The rectangle that casts the shadow (the frame, offset), and the blur's sigma in pixels.
    pub shadow: Option<(Rect, f64)>,
}

/// A usable aspect: a non-finite, zero or absurd one takes `fallback`.
fn sane_aspect(a: f64, fallback: f64) -> f64 {
    if a.is_finite() && a > 1e-3 && a < 1e3 { a } else { fallback }
}

/// The slide size `out_w × out_h` limited to [`MAX_SIDE`] (proportionally) and at least 1 × 1.
fn slide_size(out_w: usize, out_h: usize) -> (usize, usize) {
    let (w, h) = (out_w.max(1), out_h.max(1));
    let long = w.max(h);
    if long <= MAX_SIDE {
        return (w, h);
    }
    let k = MAX_SIDE as f64 / long as f64;
    (((w as f64 * k).round() as usize).clamp(1, MAX_SIDE), ((h as f64 * k).round() as usize).clamp(1, MAX_SIDE))
}

/// The layout of a `out_w × out_h` slide showing a photo of aspect `photo_aspect` (width /
/// height; a bad one fills the cell).
pub fn layout(s: &SlideSettings, photo_aspect: f64, out_w: usize, out_h: usize) -> Layout {
    let s = s.clone().sanitized();
    let (w, h) = slide_size(out_w, out_h);
    let (wf, hf) = (w as f64, h as f64);
    let short = wf.min(hf);
    // margins in pixels; when they leave no room (a tiny slide), they shrink to leave one pixel
    let fit = |a: f64, b: f64, room: f64| {
        let (a, b) = ((a * short).round(), (b * short).round());
        if a + b <= room - 1.0 {
            (a, b)
        } else {
            let k = ((room - 1.0).max(0.0)) / (a + b).max(1e-9);
            ((a * k).floor(), (b * k).floor())
        }
    };
    let (ml, mr) = fit(s.margin_left, s.margin_right, wf);
    let (mt, mb) = fit(s.margin_top, s.margin_bottom, hf);
    let slide = Rect { x0: 0.0, y0: 0.0, x1: wf, y1: hf };
    let cell = Rect { x0: ml, y0: mt, x1: wf - mr, y1: hf - mb };
    let (cw, ch) = (cell.width().max(1.0), cell.height().max(1.0));
    let a = sane_aspect(photo_aspect, cw / ch);
    let wider = a > cw / ch;
    // fit: the photo's longer side (relative to the cell) meets the cell; fill: the shorter
    let (dw, dh) = if wider != s.zoom_to_fill { (cw, cw / a) } else { (ch * a, ch) };
    let (dw, dh) = (dw.round().max(1.0), dh.round().max(1.0));
    let x0 = (cell.x0 + (cw - dw) / 2.0).round();
    let y0 = (cell.y0 + (ch - dh) / 2.0).round();
    let photo = Rect { x0, y0, x1: x0 + dw, y1: y0 + dh };
    let visible = photo.intersect(&cell);
    let stroke = if s.stroke_border && s.stroke_width > 0.0 { (s.stroke_width * short).round().max(1.0) } else { 0.0 };
    let frame = visible.expand(stroke).intersect(&slide);
    let shadow = (s.cast_shadow && s.shadow_opacity > 0.0).then(|| {
        let (d, t) = (s.shadow_offset * short, s.shadow_angle.to_radians());
        // counter-clockwise from the right, y down on screen
        (frame.shift(d * t.cos(), -d * t.sin()), s.shadow_radius * short / 2.0)
    });
    Layout { slide, cell, photo, visible, frame, shadow }
}

/// The size to render a photo of aspect `photo_aspect` at for a `out_w × out_h` slide: the
/// photo's whole size in the slide (with zoom to fill, larger than its cell), at most 4× the
/// slide's long side.
pub fn photo_size(s: &SlideSettings, photo_aspect: f64, out_w: usize, out_h: usize) -> (usize, usize) {
    let l = layout(s, photo_aspect, out_w, out_h);
    let (w, h) = (l.photo.width().max(1.0), l.photo.height().max(1.0));
    let cap = 4.0 * l.slide.width().max(l.slide.height());
    let k = (cap / w.max(h)).min(1.0);
    (((w * k).round() as usize).max(1), ((h * k).round() as usize).max(1))
}

fn mix(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8
}

fn mix_px(dst: &mut [u8; 4], src: [u8; 3], t: f32) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d = mix(*d, s, t);
    }
    dst[3] = 255;
}

/// Draw `src` scaled to `dest` (slide pixels), only where it meets `clip`, at `opacity`.
/// Only the part of `src` that shows is resampled (a zoomed-to-fill panorama stays cheap).
fn draw_image(out: &mut Rgba8, src: &Rgba8, dest: &Rect, clip: &Rect, opacity: f32) {
    if src.width == 0 || src.height == 0 || src.data.len() != src.width * src.height || opacity <= 0.0 {
        return;
    }
    let (w, h) = (out.width, out.height);
    let (vx0, vy0, vx1, vy1) = dest.intersect(clip).pixels(w, h);
    let (vw, vh) = (vx1 - vx0, vy1 - vy0);
    if vw == 0 || vh == 0 || dest.width() <= 0.0 || dest.height() <= 0.0 {
        return;
    }
    // the source pixels behind the visible part
    let (sx, sy) = (src.width as f64 / dest.width(), src.height as f64 / dest.height());
    let to_src = |v: f64, origin: f64, k: f64, max: usize| ((v - origin) * k).clamp(0.0, max as f64);
    let (s0x, s1x) = (to_src(vx0 as f64, dest.x0, sx, src.width).floor(), to_src(vx1 as f64, dest.x0, sx, src.width).ceil());
    let (s0y, s1y) = (to_src(vy0 as f64, dest.y0, sy, src.height).floor(), to_src(vy1 as f64, dest.y0, sy, src.height).ceil());
    let (cx, cy) = (s0x as usize, s0y as usize);
    let (cw, ch) = (((s1x - s0x) as usize).max(1), ((s1y - s0y) as usize).max(1));
    let part = if (cx, cy, cw, ch) == (0, 0, src.width, src.height) { None } else { Some(src.crop(cx, cy, cw, ch)) };
    let part = part.as_ref().unwrap_or(src);
    let scaled: Rgba8 = if (part.width, part.height) == (vw, vh) {
        part.clone()
    } else {
        let f = part.map(|p| [p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32]);
        let filter = if vw < part.width { Filter::Lanczos3 } else { Filter::Mitchell };
        resize(&f, vw, vh, filter).map(|p| [0, 1, 2, 3].map(|i| p[i].round().clamp(0.0, 255.0) as u8))
    };
    for (row, y) in (vy0..vy1).enumerate() {
        let (Some(dst), Some(s)) = (out.data.get_mut(y * w + vx0..y * w + vx1), scaled.data.get(row * vw..(row + 1) * vw)) else {
            continue;
        };
        for (d, p) in dst.iter_mut().zip(s) {
            let a = opacity * p[3] as f32 / 255.0;
            mix_px(d, [p[0], p[1], p[2]], a);
        }
    }
}

/// Fill `r` with `c`.
fn fill(out: &mut Rgba8, r: &Rect, c: [u8; 3]) {
    let w = out.width;
    let (x0, y0, x1, y1) = r.pixels(out.width, out.height);
    for y in y0..y1 {
        if let Some(row) = out.data.get_mut(y * w + x0..y * w + x1) {
            row.fill([c[0], c[1], c[2], 255]);
        }
    }
}

/// A 1-D blurred step: 1 inside `[a, b)`, blurred with `sigma` (pixels), over `0..n`.
fn blurred_step(n: usize, a: f64, b: f64, sigma: f64) -> Vec<f32> {
    let inside = Plane::from_fn(n, 1, |x, _| {
        let c = x as f64 + 0.5;
        if c >= a && c < b { 1.0 } else { 0.0 }
    });
    gaussian(&inside, sigma as f32).data
}

/// The slide: `photo` (the developed photo, any size: it is scaled into its cell) and the
/// backdrop image (any size; it fills the slide) composed with `s` at `out_w × out_h` pixels
/// (limited to [`MAX_SIDE`] a side, at least 1 × 1). An empty `photo` draws the backdrop alone.
pub fn compose(photo: &Rgba8, backdrop_image: Option<&Rgba8>, s: &SlideSettings, out_w: usize, out_h: usize) -> Rgba8 {
    compose_with(photo, backdrop_image, s, out_w, out_h, &mut |_, _| {}).0
}

/// [`compose`], calling `behind(slide, layout)` once the backdrop is drawn and before the shadow,
/// the stroke and the photo (so it draws behind the photo: a slideshow identity plate with
/// "render behind image"), and returning the layout it drew with (for overlays drawn on top).
pub fn compose_with(
    photo: &Rgba8,
    backdrop_image: Option<&Rgba8>,
    s: &SlideSettings,
    out_w: usize,
    out_h: usize,
    behind: &mut dyn FnMut(&mut Rgba8, &Layout),
) -> (Rgba8, Layout) {
    let s = s.clone().sanitized();
    let (w, h) = slide_size(out_w, out_h);
    let has_photo = photo.width > 0 && photo.height > 0 && photo.data.len() == photo.width * photo.height;
    let aspect = if has_photo { photo.width as f64 / photo.height as f64 } else { f64::NAN };
    let l = layout(&s, aspect, w, h);
    let c = s.backdrop_color;
    let mut out = Image::filled(w, h, [c[0], c[1], c[2], 255]);
    // background image: the photo filling the slide, at its opacity
    if let Some(bg) = backdrop_image.filter(|_| s.background_image.is_some() && s.background_opacity > 0.0)
        && bg.width > 0
        && bg.height > 0
    {
        let a = bg.width as f64 / bg.height as f64;
        let fill_all = SlideSettings { zoom_to_fill: true, margin_left: 0.0, margin_right: 0.0, margin_top: 0.0, margin_bottom: 0.0, ..s.clone() };
        let bl = layout(&fill_all, a, w, h);
        draw_image(&mut out, bg, &bl.photo, &bl.slide, s.background_opacity as f32);
    }
    // colour wash: full wash colour at the side the angle points to, none at the opposite side
    if s.color_wash && s.wash_opacity > 0.0 {
        let t = s.wash_angle.to_radians();
        let (dx, dy) = (t.cos(), -t.sin());
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let extent = (cx * dx.abs() + cy * dy.abs()).max(1e-9);
        let (wc, op) = (s.wash_color, s.wash_opacity);
        par_rows(&mut out.data, w, |y, row| {
            let py = y as f64 + 0.5 - cy;
            for (x, p) in row.iter_mut().enumerate() {
                let proj = (x as f64 + 0.5 - cx) * dx + py * dy;
                let k = ((proj / extent + 1.0) / 2.0).clamp(0.0, 1.0);
                mix_px(p, wc, (k * op) as f32);
            }
        });
    }
    behind(&mut out, &l);
    // cast shadow: a blurred rectangle (separable: the product of two blurred steps)
    if let Some((r, sigma)) = l.shadow {
        let op = s.shadow_opacity as f32;
        let fx = blurred_step(w, r.x0, r.x1, sigma);
        let fy = blurred_step(h, r.y0, r.y1, sigma);
        par_rows(&mut out.data, w, |y, row| {
            let ky = fy.get(y).copied().unwrap_or(0.0);
            if ky <= 1e-4 {
                return;
            }
            for (p, kx) in row.iter_mut().zip(&fx) {
                let a = op * ky * kx;
                if a > 1e-4 {
                    mix_px(p, [0, 0, 0], a);
                }
            }
        });
    }
    if l.frame != l.visible {
        fill(&mut out, &l.frame, s.stroke_color);
    }
    if has_photo {
        draw_image(&mut out, photo, &l.photo, &l.cell, 1.0);
    }
    (out, l)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{MARGIN_MAX, SHADOW_OFFSET_MAX, SHADOW_RADIUS_MAX, STROKE_MAX};
    use serde_json::json;

    fn solid(w: usize, h: usize, c: [u8; 3]) -> Rgba8 {
        Image::filled(w, h, [c[0], c[1], c[2], 255])
    }

    fn with(p: serde_json::Value) -> SlideSettings {
        SlideSettings::default().merged(&p).unwrap()
    }

    fn px(img: &Rgba8, x: usize, y: usize) -> [u8; 3] {
        let p = img.get(x, y);
        [p[0], p[1], p[2]]
    }

    const RED: [u8; 3] = [255, 0, 0];
    const GREEN: [u8; 3] = [0, 200, 0];

    #[test]
    fn margins_make_the_cell_and_the_photo_fits_inside_it() {
        // 1000 × 500 slide, 10 % margins (of the 500 px short side = 50 px)
        let s = with(json!({"margins": 0.1}));
        let l = layout(&s, 1.0, 1000, 500);
        assert_eq!(l.cell, Rect { x0: 50.0, y0: 50.0, x1: 950.0, y1: 450.0 });
        // a square photo fits the 900 × 400 cell's height, centred
        assert_eq!(l.photo, Rect { x0: 300.0, y0: 50.0, x1: 700.0, y1: 450.0 });
        assert_eq!(l.visible, l.photo);
        // independent margins
        let s = with(json!({"linkMargins": false, "marginLeft": 0.2, "marginTop": 0.0, "marginRight": 0.0, "marginBottom": 0.4}));
        let l = layout(&s, 3.0, 1000, 500);
        assert_eq!(l.cell, Rect { x0: 100.0, y0: 0.0, x1: 1000.0, y1: 300.0 });
        assert_eq!((l.photo.width(), l.photo.height()), (900.0, 300.0), "3:1 fits 900 × 300 exactly");

        let img = compose(&solid(64, 64, GREEN), None, &with(json!({"margins": 0.1, "backdropColor": "#ff0000"})), 1000, 500);
        assert_eq!((img.width, img.height), (1000, 500));
        assert_eq!(px(&img, 10, 10), RED, "margin: backdrop");
        assert_eq!(px(&img, 500, 250), GREEN, "cell centre: photo");
        assert_eq!(px(&img, 290, 250), RED, "beside a fitted square photo: backdrop");
        assert_eq!(px(&img, 310, 60), GREEN);
    }

    #[test]
    fn zoom_to_fill_covers_the_cell_and_crops() {
        let s = with(json!({"margins": 0.1, "zoomToFill": true}));
        let l = layout(&s, 1.0, 1000, 500);
        assert_eq!((l.photo.width(), l.photo.height()), (900.0, 900.0), "the square photo covers the 900 × 400 cell");
        assert_eq!(l.visible, l.cell, "and shows only inside it");
        assert_eq!(photo_size(&s, 1.0, 1000, 500), (900, 900), "rendered whole at the covering size");
        // a gradient photo: zoomed to fill, its middle rows show, its top and bottom are cut
        let photo = Rgba8::from_fn(100, 100, |_, y| [(y * 2) as u8, 0, 0, 255]);
        let img = compose(&photo, None, &s, 1000, 500);
        assert_eq!(px(&img, 500, 10), [0, 0, 0], "margin stays the backdrop");
        assert_eq!(px(&img, 60, 250), px(&img, 940, 250), "covers the cell edge to edge");
        let top = img.get(500, 51)[0];
        assert!(top > 40, "the photo's top rows are cropped away ({top})");
        // fit draws the whole photo (its first rows near black)
        let img = compose(&photo, None, &with(json!({"margins": 0.1})), 1000, 500);
        assert!(img.get(500, 51)[0] < 6);
    }

    #[test]
    fn aspect_preview_ratios() {
        use crate::settings::Aspect;
        let s = with(json!({"aspect": "4:3"}));
        let r = s.aspect.ratio(2.0);
        assert!((r - 4.0 / 3.0).abs() < 1e-12);
        assert_eq!(with(json!({"aspect": "screen"})).aspect.ratio(2.0), 2.0);
        assert_eq!(Aspect::parse("16:9"), Some(Aspect::Wide));
        // a 4:3 slide of a 3:2 photo letterboxes top and bottom
        let l = layout(&s, 1.5, 800, 600);
        assert_eq!((l.photo.width(), l.photo.height(), l.photo.y0), (800.0, 533.0, 34.0));
    }

    #[test]
    fn stroke_surrounds_the_photo_and_shadow_falls_at_its_angle() {
        let s = with(json!({"margins": 0.2, "strokeBorder": true, "strokeWidth": 0.02, "strokeColor": "#ffffff"}));
        let l = layout(&s, 1.0, 1000, 1000);
        assert_eq!(l.visible, Rect { x0: 200.0, y0: 200.0, x1: 800.0, y1: 800.0 });
        assert_eq!(l.frame, Rect { x0: 180.0, y0: 180.0, x1: 820.0, y1: 820.0 }, "20 px outside the photo");
        let img = compose(&solid(10, 10, GREEN), None, &s, 1000, 1000);
        assert_eq!(px(&img, 190, 500), [255, 255, 255], "stroke left");
        assert_eq!(px(&img, 810, 810), [255, 255, 255], "stroke corner");
        assert_eq!(px(&img, 175, 500), [0, 0, 0], "outside the stroke");
        assert_eq!(px(&img, 205, 500), GREEN, "the photo keeps its size");

        // shadow at −45° (down-right) by 5 %, hard edged (radius 0), opaque
        let s = with(
            json!({"margins": 0.2, "castShadow": true, "shadowOffset": 0.05, "shadowAngle": -45, "shadowRadius": 0, "shadowOpacity": 1, "backdropColor": "#ffffff"}),
        );
        let l = layout(&s, 1.0, 1000, 1000);
        let (r, _) = l.shadow.unwrap();
        let d = 50.0 / 2f64.sqrt();
        assert!((r.x0 - (200.0 + d)).abs() < 1e-9 && (r.y0 - (200.0 + d)).abs() < 1e-9, "{r:?}");
        let img = compose(&solid(10, 10, GREEN), None, &s, 1000, 1000);
        assert_eq!(px(&img, 820, 820), [0, 0, 0], "shadow below right");
        assert_eq!(px(&img, 180, 180), [255, 255, 255], "no shadow above left");
        assert_eq!(px(&img, 850, 500), [255, 255, 255], "shadow ends at its offset");
        // at 90° the shadow goes straight up; a soft radius spreads it but fades out
        let s = with(
            json!({"margins": 0.2, "castShadow": true, "shadowOffset": 0.05, "shadowAngle": 90, "shadowRadius": 0.04, "shadowOpacity": 0.8, "backdropColor": "#ffffff"}),
        );
        let img = compose(&solid(10, 10, GREEN), None, &s, 1000, 1000);
        assert!(img.get(500, 170)[0] < 200, "above: in shadow");
        assert_eq!(px(&img, 500, 950), [255, 255, 255], "below: none");
        assert_eq!(px(&img, 500, 50), [255, 255, 255], "beyond the blur: none");
        let mid = img.get(500, 151)[0];
        assert!(mid > 60 && mid < 250, "a soft edge ({mid})");
    }

    #[test]
    fn colour_wash_follows_its_angle() {
        let wash = |angle: f64| {
            compose(
                &Rgba8::new(0, 0),
                None,
                &with(json!({"colorWash": true, "washColor": "#ffffff", "washOpacity": 1, "washAngle": angle})),
                200,
                100,
            )
        };
        // 0°: white at the right, black at the left
        let img = wash(0.0);
        assert!(img.get(199, 50)[0] > 250 && img.get(0, 50)[0] < 5);
        assert!((img.get(100, 50)[0] as i32 - 128).abs() < 3, "half way in the middle");
        assert_eq!(img.get(150, 0), img.get(150, 99), "constant along the other axis");
        // 90°: white at the top
        let img = wash(90.0);
        assert!(img.get(100, 0)[0] > 245 && img.get(100, 99)[0] < 10);
        // 180°: white at the left
        assert!(wash(180.0).get(0, 50)[0] > 250);
        // −45°: bottom-right is the brightest corner
        let img = wash(-45.0);
        assert!(img.get(199, 99)[0] > 250 && img.get(0, 0)[0] < 5);
        // opacity scales it
        let img =
            compose(&Rgba8::new(0, 0), None, &with(json!({"colorWash": true, "washColor": "#ffffff", "washOpacity": 0.5, "washAngle": 0})), 200, 100);
        assert!((img.get(199, 50)[0] as i32 - 127).abs() < 3);
    }

    #[test]
    fn background_image_fills_the_slide_at_its_opacity() {
        let bg = solid(30, 10, [200, 100, 0]);
        let s = with(json!({"backgroundImage": 5, "backgroundOpacity": 0.5, "backdropColor": "#000000", "margins": 0.25}));
        let img = compose(&solid(4, 4, GREEN), Some(&bg), &s, 400, 200);
        assert_eq!(px(&img, 2, 2), [100, 50, 0], "half the image over black");
        assert_eq!(px(&img, 398, 198), [100, 50, 0], "fills the whole slide");
        assert_eq!(px(&img, 200, 100), GREEN, "the photo on top");
        // off, or no image given: the backdrop colour
        let off = SlideSettings { background_image: None, ..s.clone() };
        assert_eq!(px(&compose(&solid(4, 4, GREEN), Some(&bg), &off, 400, 200), 2, 2), [0, 0, 0]);
        assert_eq!(px(&compose(&solid(4, 4, GREEN), None, &s, 400, 200), 2, 2), [0, 0, 0]);
    }

    #[test]
    fn layouts_are_resolution_independent() {
        let s = with(
            json!({"linkMargins": false, "marginLeft": 0.1, "marginTop": 0.05, "marginRight": 0.2, "marginBottom": 0.08, "strokeBorder": true, "strokeWidth": 0.01, "castShadow": true, "shadowOffset": 0.02}),
        );
        let a = layout(&s, 1.5, 800, 450);
        let b = layout(&s, 1.5, 1600, 900);
        for (ra, rb) in [(a.cell, b.cell), (a.photo, b.photo), (a.frame, b.frame), (a.shadow.unwrap().0, b.shadow.unwrap().0)] {
            for (va, vb) in [(ra.x0, rb.x0), (ra.y0, rb.y0), (ra.x1, rb.x1), (ra.y1, rb.y1)] {
                assert!((va * 2.0 - vb).abs() <= 2.0, "{ra:?} vs {rb:?}");
            }
        }
        assert!((a.shadow.unwrap().1 * 2.0 - b.shadow.unwrap().1).abs() < 1e-9);
        // and the pixels agree: the big slide, halved, is the small one (away from edges)
        let photo = Rgba8::from_fn(60, 40, |x, y| [(x * 4) as u8, (y * 6) as u8, 90, 255]);
        let small = compose(&photo, None, &s, 800, 450);
        let big = compose(&photo, None, &s, 1600, 900);
        for (x, y) in [(10, 10), (400, 225), (300, 100), (700, 400)] {
            let (p, q) = (small.get(x, y), big.get(x * 2, y * 2));
            assert!((0..3).all(|i| (p[i] as i32 - q[i] as i32).abs() < 24), "({x}, {y}): {p:?} vs {q:?}");
        }
    }

    #[test]
    fn the_default_slide_is_the_photo_fitted_on_black() {
        let photo = Rgba8::from_fn(300, 200, |x, y| [x as u8, y as u8, 7, 255]);
        let s = SlideSettings::default();
        assert!(s.is_plain());
        // a 3:2 photo on a 16:10 screen: the photo at its own pixels, black bars beside it
        assert_eq!(photo_size(&s, 1.5, 320, 200), (300, 200));
        let img = compose(&photo, None, &s, 320, 200);
        assert_eq!(px(&img, 5, 100), [0, 0, 0]);
        assert_eq!(px(&img, 314, 100), [0, 0, 0]);
        for (x, y) in [(0, 0), (150, 100), (299, 199)] {
            assert_eq!(img.get(x + 10, y), photo.get(x, y), "unchanged pixels at ({x}, {y})");
        }
    }

    #[test]
    fn behind_draws_under_the_photo_and_the_layout_comes_back() {
        let s = with(json!({"margins": 0.2}));
        let mut seen = None;
        let (img, l) = compose_with(&solid(10, 10, GREEN), None, &s, 200, 100, &mut |out, l| {
            seen = Some(*l);
            out.data.fill([255, 0, 0, 255]);
        });
        assert_eq!(seen, Some(l), "the hook gets the layout the slide is drawn with");
        assert_eq!(px(&img, 2, 2), RED, "the hook's paint stays on the backdrop");
        assert_eq!(px(&img, 100, 50), GREEN, "and the photo covers it");
    }

    #[test]
    fn hostile_input_never_panics() {
        let wild = SlideSettings {
            margin_left: f64::NAN,
            margin_right: 1e300,
            margin_top: -5.0,
            margin_bottom: f64::INFINITY,
            stroke_border: true,
            stroke_width: 1e9,
            cast_shadow: true,
            shadow_opacity: f64::NAN,
            shadow_offset: -1.0,
            shadow_radius: f64::INFINITY,
            shadow_angle: f64::NAN,
            color_wash: true,
            wash_opacity: 9.0,
            wash_angle: f64::INFINITY,
            background_image: Some(u64::MAX),
            background_opacity: f64::NEG_INFINITY,
            zoom_to_fill: true,
            ..SlideSettings::default()
        };
        let photos = [Rgba8::new(0, 0), Rgba8::new(1, 1), Rgba8::new(5000, 1), Rgba8::new(1, 3000), Rgba8 { width: 4, height: 4, data: vec![] }];
        for photo in &photos {
            for (w, h) in [(0, 0), (1, 1), (3, 2), (1, 400), (100_000, 2), (64, 48)] {
                let img = compose(photo, Some(&Rgba8::new(0, 5)), &wild, w, h);
                assert!(img.width >= 1 && img.height >= 1 && img.width <= MAX_SIDE && img.height <= MAX_SIDE);
                assert_eq!(img.data.len(), img.width * img.height);
                let (pw, ph) = photo_size(&wild, photo.width as f64 / photo.height.max(1) as f64, w, h);
                assert!(pw >= 1 && ph >= 1 && pw <= 4 * MAX_SIDE && ph <= 4 * MAX_SIDE);
            }
        }
        for a in [f64::NAN, 0.0, -1.0, f64::INFINITY, 1e-300, 1e300] {
            let l = layout(&wild, a, 640, 480);
            assert!(l.photo.width() >= 1.0 && l.photo.width().is_finite());
        }
        // the clamps: biggest margins still leave a cell, biggest stroke and shadow stay bounded
        let s = SlideSettings {
            margin_left: MARGIN_MAX,
            margin_right: MARGIN_MAX,
            stroke_border: true,
            stroke_width: STROKE_MAX,
            cast_shadow: true,
            shadow_offset: SHADOW_OFFSET_MAX,
            shadow_radius: SHADOW_RADIUS_MAX,
            ..SlideSettings::default()
        };
        let l = layout(&s, 1.0, 100, 100);
        assert!(l.cell.width() >= 1.0, "{l:?}");
        assert!(l.frame.x0 >= 0.0 && l.frame.x1 <= 100.0, "the stroke stays on the slide");
    }
}
