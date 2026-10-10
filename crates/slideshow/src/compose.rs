//! Draw a slide: backdrop (colour, wash, image), the photo in its cell (fit or fill, pan and zoom),
//! its drop shadow and stroke, the overlays (identity plate, rating stars, watermark, token text),
//! and the intro / ending screens; plus the cross-fade and colour-fade between two frames.
//!
//! Sources: own design (straight alpha blending of 8-bit sRGB values, bilinear sampling, a soft
//! shadow from the distance to a rectangle).

use dac_engine::export::{Anchor, Watermark, draw_watermark};
use dac_raster::Rgba8;

use crate::settings::{Settings, TitleScreen};
use crate::tokens::{SlideInfo, expand};

/// Largest frame edge the compositor draws (an export or a 8K screen).
pub const MAX_EDGE: usize = 8192;

/// A rectangle in pixels (floating point).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn w(&self) -> f32 {
        (self.x1 - self.x0).max(0.0)
    }
    pub fn h(&self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }
    fn shrink(&self, d: f32) -> Rect {
        Rect { x0: self.x0 + d, y0: self.y0 + d, x1: self.x1 - d, y1: self.y1 - d }
    }
}

/// The geometry of a slide in a `w × h` frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    /// The slide (the frame, or the aspect rectangle inside it).
    pub slide: Rect,
    /// The photo cell (slide minus margins).
    pub cell: Rect,
    /// Where the photo is drawn (inside the cell when fitted; may exceed it when filling, clipped).
    pub photo: Rect,
}

/// Lay out a photo of `pw × ph` in a `w × h` frame.
pub fn geometry(w: usize, h: usize, pw: usize, ph: usize, s: &Settings) -> Geometry {
    let (fw, fh) = (w as f32, h as f32);
    let slide = match s.layout.aspect.ratio() {
        Some(r) if fw / fh.max(1.0) > r => {
            let sw = fh * r;
            Rect { x0: (fw - sw) / 2.0, y0: 0.0, x1: (fw + sw) / 2.0, y1: fh }
        }
        Some(r) => {
            let sh = fw / r;
            Rect { x0: 0.0, y0: (fh - sh) / 2.0, x1: fw, y1: (fh + sh) / 2.0 }
        }
        None => Rect { x0: 0.0, y0: 0.0, x1: fw, y1: fh },
    };
    let [l, t, r, b] = s.layout.margins;
    let cell = Rect { x0: slide.x0 + l * slide.w(), y0: slide.y0 + t * slide.h(), x1: slide.x1 - r * slide.w(), y1: slide.y1 - b * slide.h() };
    let (pw, ph) = (pw.max(1) as f32, ph.max(1) as f32);
    let (sx, sy) = (cell.w() / pw, cell.h() / ph);
    let k = if s.options.zoom_to_fill { sx.max(sy) } else { sx.min(sy) };
    let (dw, dh) = (pw * k, ph * k);
    let (cx, cy) = ((cell.x0 + cell.x1) / 2.0, (cell.y0 + cell.y1) / 2.0);
    let photo = Rect { x0: cx - dw / 2.0, y0: cy - dh / 2.0, x1: cx + dw / 2.0, y1: cy + dh / 2.0 };
    Geometry { slide, cell, photo }
}

fn checked_frame(w: usize, h: usize, fill: [u8; 3]) -> Result<Rgba8, String> {
    if w == 0 || h == 0 || w > MAX_EDGE || h > MAX_EDGE {
        return Err(format!("slide size {w}×{h} is outside 1…{MAX_EDGE}"));
    }
    Ok(Rgba8::filled(w, h, [fill[0], fill[1], fill[2], 255]))
}

fn blend_px(p: &mut [u8; 4], c: [u8; 3], k: f32) {
    let k = k.clamp(0.0, 1.0);
    for i in 0..3 {
        p[i] = (f32::from(p[i]) + (f32::from(c[i]) - f32::from(p[i])) * k).round() as u8;
    }
}

/// Bilinear sample of `img` at pixel coordinates (centres at +0.5).
fn sample(img: &Rgba8, x: f32, y: f32) -> [f32; 4] {
    let (w, h) = (img.width, img.height);
    if w == 0 || h == 0 {
        return [0.0; 4];
    }
    let fx = (x - 0.5).clamp(0.0, (w - 1) as f32);
    let fy = (y - 0.5).clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let px = |x: usize, y: usize| img.data.get(y * w + x).copied().unwrap_or([0; 4]);
    let (a, b, c, d) = (px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1));
    let mut out = [0.0; 4];
    for i in 0..4 {
        let top = f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * tx;
        let bottom = f32::from(c[i]) + (f32::from(d[i]) - f32::from(c[i])) * tx;
        out[i] = top + (bottom - top) * ty;
    }
    out
}

/// Pixel bounds of `r` clipped to the image.
fn bounds(img: &Rgba8, r: Rect) -> (usize, usize, usize, usize) {
    let cl = |v: f32, max: usize| if v.is_finite() { v.clamp(0.0, max as f32) as usize } else { 0 };
    (cl(r.x0.floor(), img.width), cl(r.y0.floor(), img.height), cl(r.x1.ceil(), img.width), cl(r.y1.ceil(), img.height))
}

fn backdrop(img: &mut Rgba8, s: &Settings, slide: Rect, background: Option<&Rgba8>) {
    let b = &s.backdrop;
    let (x0, y0, x1, y1) = bounds(img, slide);
    let w = img.width;
    for y in y0..y1 {
        for x in x0..x1 {
            let Some(p) = img.data.get_mut(y * w + x) else { continue };
            *p = [b.color[0], b.color[1], b.color[2], 255];
        }
    }
    if let Some(bg) = background.filter(|_| b.image_opacity > 0.0) {
        // cover the slide with the image
        let k = (slide.w() / bg.width.max(1) as f32).max(slide.h() / bg.height.max(1) as f32).max(1e-6);
        let (ox, oy) = ((slide.w() - bg.width as f32 * k) / 2.0, (slide.h() - bg.height as f32 * k) / 2.0);
        for y in y0..y1 {
            for x in x0..x1 {
                let sx = (x as f32 + 0.5 - slide.x0 - ox) / k;
                let sy = (y as f32 + 0.5 - slide.y0 - oy) / k;
                let c = sample(bg, sx, sy);
                if let Some(p) = img.data.get_mut(y * w + x) {
                    blend_px(p, [c[0] as u8, c[1] as u8, c[2] as u8], b.image_opacity * c[3] / 255.0);
                }
            }
        }
    }
    if b.wash && b.wash_opacity > 0.0 {
        let a = b.wash_angle.to_radians();
        let (dx, dy) = (a.cos(), -a.sin());
        let (cx, cy) = ((slide.x0 + slide.x1) / 2.0, (slide.y0 + slide.y1) / 2.0);
        let half = ((slide.w() * dx).abs() + (slide.h() * dy).abs()).max(1.0) / 2.0;
        for y in y0..y1 {
            for x in x0..x1 {
                // 1 at the wash's side, 0 at the opposite one
                let t = ((x as f32 + 0.5 - cx) * dx + (y as f32 + 0.5 - cy) * dy) / half;
                let k = ((t + 1.0) / 2.0).clamp(0.0, 1.0) * b.wash_opacity;
                if let Some(p) = img.data.get_mut(y * w + x) {
                    blend_px(p, b.wash_color, k);
                }
            }
        }
    }
}

fn shadow(img: &mut Rgba8, r: Rect, s: &Settings, short: f32) {
    let o = &s.options;
    let a = o.shadow_angle.to_radians();
    let off = o.shadow_offset * short;
    let rad = (o.shadow_radius * short).max(0.5);
    let sr = Rect { x0: r.x0 + a.cos() * off, y0: r.y0 - a.sin() * off, x1: r.x1 + a.cos() * off, y1: r.y1 - a.sin() * off };
    let (x0, y0, x1, y1) = bounds(img, Rect { x0: sr.x0 - rad, y0: sr.y0 - rad, x1: sr.x1 + rad, y1: sr.y1 + rad });
    let w = img.width;
    for y in y0..y1 {
        for x in x0..x1 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let dx = (sr.x0 - px).max(px - sr.x1).max(0.0);
            let dy = (sr.y0 - py).max(py - sr.y1).max(0.0);
            let d = (dx * dx + dy * dy).sqrt() / rad;
            let k = (1.0 - d).clamp(0.0, 1.0);
            let k = k * k * (3.0 - 2.0 * k) * o.shadow_opacity;
            if let Some(p) = img.data.get_mut(y * w + x) {
                blend_px(p, [0, 0, 0], k);
            }
        }
    }
}

fn fill_rect(img: &mut Rgba8, r: Rect, c: [u8; 3], k: f32) {
    let (x0, y0, x1, y1) = bounds(img, r);
    let w = img.width;
    for y in y0..y1 {
        for x in x0..x1 {
            if let Some(p) = img.data.get_mut(y * w + x) {
                blend_px(p, c, k);
            }
        }
    }
}

/// Draw the photo into `dest` clipped to `clip`, showing the window `(zoom, cx, cy)` of it.
fn draw_photo(img: &mut Rgba8, photo: &Rgba8, dest: Rect, clip: Rect, view: (f32, f32, f32)) {
    let (zoom, cx, cy) = view;
    let zoom = if zoom.is_finite() { zoom.max(1.0) } else { 1.0 };
    let r = Rect { x0: dest.x0.max(clip.x0), y0: dest.y0.max(clip.y0), x1: dest.x1.min(clip.x1), y1: dest.y1.min(clip.y1) };
    let (x0, y0, x1, y1) = bounds(img, r);
    let (dw, dh) = (dest.w().max(1e-3), dest.h().max(1e-3));
    let (pw, ph) = (photo.width as f32, photo.height as f32);
    let w = img.width;
    for y in y0..y1 {
        let v = (y as f32 + 0.5 - dest.y0) / dh;
        let sy = (cy + (v - 0.5) / zoom) * ph;
        for x in x0..x1 {
            let u = (x as f32 + 0.5 - dest.x0) / dw;
            let sx = (cx + (u - 0.5) / zoom) * pw;
            let c = sample(photo, sx, sy);
            if let Some(p) = img.data.get_mut(y * w + x) {
                blend_px(p, [c[0].round() as u8, c[1].round() as u8, c[2].round() as u8], c[3] / 255.0);
            }
        }
    }
}

/// Draw `text` inside `area` of `img` at `anchor`; `size` is a fraction of `short`.
pub fn text_in(img: &mut Rgba8, area: Rect, text: &str, anchor: Anchor, size: f32, short: f32, color: [u8; 3], opacity: f32, shadow: bool) {
    if text.trim().is_empty() || opacity <= 0.0 {
        return;
    }
    let (x0, y0, x1, y1) = bounds(img, area);
    if x1 <= x0 + 2 || y1 <= y0 + 2 {
        return;
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let mut sub = img.crop(x0, y0, w, h);
    let sub_short = w.min(h) as f32;
    let wm = Watermark {
        text: text.to_string(),
        size: (size * short / sub_short).clamp(0.001, 1.0),
        opacity: opacity.clamp(0.0, 1.0),
        anchor,
        inset: (0.02 * short / sub_short).clamp(0.0, 0.5),
        color,
        shadow,
        ..Default::default()
    };
    draw_watermark(&mut sub, &wm);
    let iw = img.width;
    for row in 0..h {
        let (Some(dst), Some(src)) = (img.data.get_mut((y0 + row) * iw + x0..(y0 + row) * iw + x0 + w), sub.data.get(row * w..(row + 1) * w)) else {
            continue;
        };
        dst.copy_from_slice(src);
    }
}

/// A five-pointed star polygon centred at (cx, cy) with outer radius r.
fn star(cx: f32, cy: f32, r: f32) -> [(f32, f32); 10] {
    let mut pts = [(0.0, 0.0); 10];
    for (i, p) in pts.iter_mut().enumerate() {
        let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
        let rr = if i % 2 == 0 { r } else { r * 0.42 };
        *p = (cx + rr * a.cos(), cy + rr * a.sin());
    }
    pts
}

fn inside(poly: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut c = false;
    let n = poly.len();
    for i in 0..n {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[(i + n - 1) % n];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            c = !c;
        }
    }
    c
}

/// Rating stars at the photo's bottom-right corner, inside it.
fn stars(img: &mut Rgba8, photo: Rect, rating: u8, s: &Settings, short: f32) {
    let n = rating.min(5);
    if n == 0 {
        return;
    }
    let o = &s.overlays;
    let r = o.rating_size * short / 2.0;
    let gap = r * 2.2;
    let pad = 0.015 * short + r;
    let w = img.width;
    for i in 0..n {
        let cx = photo.x1 - pad - (n - 1 - i) as f32 * gap;
        let cy = photo.y1 - pad;
        let poly = star(cx, cy, r);
        let (x0, y0, x1, y1) = bounds(img, Rect { x0: cx - r, y0: cy - r, x1: cx + r, y1: cy + r });
        for y in y0..y1 {
            for x in x0..x1 {
                // 2×2 supersampling for smooth edges
                let mut cov = 0.0;
                for (dx, dy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    if inside(&poly, x as f32 + dx, y as f32 + dy) {
                        cov += 0.25;
                    }
                }
                if cov > 0.0
                    && let Some(p) = img.data.get_mut(y * w + x)
                {
                    blend_px(p, o.rating_color, cov * o.rating_opacity);
                }
            }
        }
    }
}

/// Compose one slide at `w × h`. `photo`: the rendered photo (`None` draws the empty cell, as a
/// layout preview); `view`: pan and zoom (`(1, 0.5, 0.5)` = still); `background`: the decoded
/// backdrop image, if any; `plate`: the identity plate text when the settings leave theirs empty.
pub fn compose_slide(
    w: usize,
    h: usize,
    photo: Option<&Rgba8>,
    info: &SlideInfo,
    s: &Settings,
    view: (f32, f32, f32),
    background: Option<&Rgba8>,
    plate: &str,
) -> Result<Rgba8, String> {
    let mut img = checked_frame(w, h, [0, 0, 0])?;
    let (pw, ph) = photo.map_or((3, 2), |p| (p.width, p.height));
    let g = geometry(w, h, pw, ph, s);
    let short = g.slide.w().min(g.slide.h()).max(1.0);
    backdrop(&mut img, s, g.slide, background);
    let visible = Rect { x0: g.photo.x0.max(g.cell.x0), y0: g.photo.y0.max(g.cell.y0), x1: g.photo.x1.min(g.cell.x1), y1: g.photo.y1.min(g.cell.y1) };
    if s.options.shadow && s.options.shadow_opacity > 0.0 {
        shadow(&mut img, visible, s, short);
    }
    if s.options.stroke && s.options.stroke_width > 0.0 {
        let sw = (s.options.stroke_width * short).max(1.0);
        fill_rect(&mut img, visible.shrink(-sw), s.options.stroke_color, 1.0);
    }
    match photo {
        Some(p) if p.width > 0 && p.height > 0 => draw_photo(&mut img, p, g.photo, g.cell, view),
        _ => fill_rect(&mut img, visible, [90, 90, 90], 1.0),
    }
    let o = &s.overlays;
    if o.identity_plate {
        let text = if o.plate_text.trim().is_empty() { plate } else { o.plate_text.as_str() };
        text_in(&mut img, g.slide, text, o.plate_anchor, o.plate_size, short, [235, 235, 235], o.plate_opacity, false);
    }
    if o.rating {
        stars(&mut img, visible, info.rating, s, short);
    }
    if !o.watermark.trim().is_empty() {
        text_in(&mut img, visible, &expand(&o.watermark, info), Anchor::BottomRight, 0.025, short, [255, 255, 255], 0.6, true);
    }
    for t in &o.texts {
        let area = if t.on_photo { visible } else { g.slide };
        text_in(&mut img, area, &expand(&t.text, info), t.anchor, t.size, short, t.color, t.opacity, t.shadow);
    }
    Ok(img)
}

/// The intro or ending screen.
pub fn compose_title(w: usize, h: usize, t: &TitleScreen, plate: &str) -> Result<Rgba8, String> {
    let mut img = checked_frame(w, h, t.color)?;
    let short = w.min(h) as f32;
    let full = Rect { x0: 0.0, y0: 0.0, x1: w as f32, y1: h as f32 };
    let text = if !t.text.trim().is_empty() {
        t.text.as_str()
    } else if t.plate {
        plate
    } else {
        ""
    };
    text_in(&mut img, full, text, Anchor::Center, t.size, short, t.text_color, 1.0, false);
    Ok(img)
}

/// `a` → `b` by `k` (0..1); frames of different sizes give `a`.
pub fn crossfade(a: &Rgba8, b: &Rgba8, k: f32) -> Rgba8 {
    let mut out = a.clone();
    if a.width != b.width || a.height != b.height {
        return out;
    }
    for (p, q) in out.data.iter_mut().zip(&b.data) {
        blend_px(p, [q[0], q[1], q[2]], k);
    }
    out
}

/// Fade `a` towards `color` by `k` (0..1).
pub fn fade_to(a: &Rgba8, color: [u8; 3], k: f32) -> Rgba8 {
    let mut out = a.clone();
    for p in &mut out.data {
        blend_px(p, color, k);
    }
    out
}

/// The transition between `a` and `b` at `k`: a cross-fade, or through `color` (first half out,
/// second half in) when the playback fades through a colour.
pub fn transition(a: &Rgba8, b: &Rgba8, k: f32, s: &Settings) -> Rgba8 {
    if s.playback.color_fade {
        if k < 0.5 { fade_to(a, s.playback.fade_color, k * 2.0) } else { fade_to(b, s.playback.fade_color, (1.0 - k) * 2.0) }
    } else {
        crossfade(a, b, k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo() -> Rgba8 {
        Rgba8::filled(300, 200, [200, 30, 30, 255])
    }

    #[test]
    fn fit_keeps_the_photo_inside_the_cell() {
        let s = Settings::default();
        let g = geometry(1000, 500, 300, 200, &s);
        assert!(g.photo.x0 >= g.cell.x0 - 0.01 && g.photo.x1 <= g.cell.x1 + 0.01);
        assert!((g.photo.h() - g.cell.h()).abs() < 0.01);
        let mut f = s.clone();
        f.options.zoom_to_fill = true;
        let g = geometry(1000, 500, 300, 200, &f);
        assert!((g.photo.w() - g.cell.w()).abs() < 0.01 && g.photo.h() > g.cell.h());
    }

    #[test]
    fn aspect_letterboxes_the_slide() {
        let mut s = Settings::default();
        s.layout.aspect = crate::settings::Aspect::Classic4x3;
        let g = geometry(1600, 900, 300, 200, &s);
        assert!((g.slide.w() - 1200.0).abs() < 0.01);
    }

    #[test]
    fn slide_has_photo_in_the_middle_and_backdrop_at_the_edge() {
        let mut s = Settings::default();
        s.backdrop.color = [10, 20, 30];
        s.overlays.rating = true;
        s.overlays.texts.push(Default::default());
        let info = SlideInfo { filename: "x.jpg".into(), rating: 3, ..Default::default() };
        let img = compose_slide(400, 300, Some(&photo()), &info, &s, (1.0, 0.5, 0.5), None, "Plate").unwrap();
        assert_eq!(img.get(200, 150), [200, 30, 30, 255]);
        assert_eq!(img.get(1, 1), [10, 20, 30, 255]);
    }

    #[test]
    fn hostile_sizes_are_errors_not_panics() {
        let s = Settings::default();
        assert!(compose_slide(0, 10, None, &SlideInfo::default(), &s, (1.0, 0.5, 0.5), None, "").is_err());
        assert!(compose_slide(100_000, 10, None, &SlideInfo::default(), &s, (1.0, 0.5, 0.5), None, "").is_err());
        let empty = Rgba8::new(0, 0);
        assert!(compose_slide(64, 48, Some(&empty), &SlideInfo::default(), &s, (f32::NAN, f32::NAN, 0.5), None, "").is_ok());
        let tiny = Rgba8::filled(1, 1, [9; 4]);
        let mut all = s.clone();
        all.options.stroke = true;
        all.overlays.identity_plate = true;
        all.overlays.rating = true;
        all.overlays.watermark = "{Filename}".into();
        all.backdrop.wash = true;
        all.layout.margins = [0.45; 4];
        assert!(compose_slide(3, 3, Some(&tiny), &SlideInfo { rating: 9, ..Default::default() }, &all, (1.3, 2.0, -1.0), Some(&tiny), "P").is_ok());
    }

    #[test]
    fn transitions_blend() {
        let a = Rgba8::filled(4, 4, [0, 0, 0, 255]);
        let b = Rgba8::filled(4, 4, [200, 200, 200, 255]);
        assert_eq!(crossfade(&a, &b, 0.5).get(0, 0), [100, 100, 100, 255]);
        let mut s = Settings::default();
        s.playback.color_fade = true;
        s.playback.fade_color = [255, 255, 255];
        assert_eq!(transition(&a, &b, 0.5, &s).get(0, 0), [255, 255, 255, 255]);
        assert_eq!(crossfade(&a, &Rgba8::new(2, 2), 0.5).get(0, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn title_screen_has_its_colour() {
        let t = TitleScreen { enabled: true, color: [5, 6, 7], text: "Hello".into(), ..Default::default() };
        let img = compose_title(320, 200, &t, "").unwrap();
        assert_eq!(img.get(2, 2), [5, 6, 7, 255]);
        assert!(img.data.iter().any(|p| p[0] > 100), "text drawn");
    }
}
