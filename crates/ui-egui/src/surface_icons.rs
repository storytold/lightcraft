//! Control-surface icons (`assets/surface-icons`, the same pack the surface plugins draw on keys)
//! as egui textures, for Settings ▸ Controls and the on-screen readout. Rasterised once per name
//! and size and kept for the session.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use lightcraft_surface as ls;

type Cache = Arc<Mutex<HashMap<(String, u32), Option<egui::TextureHandle>>>>;

/// The most icon textures kept (names × sizes); beyond it the cache starts over.
const MAX_CACHED: usize = 1024;

fn cache(ctx: &egui::Context) -> Cache {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Cache>(egui::Id::unique("surfaceIcons")).clone())
}

/// Rasterise an SVG to `px` × `px` straight RGBA (`None`: not an SVG resvg reads).
pub fn rasterize(svg: &str, px: u32) -> Option<egui::ColorImage> {
    let px = px.clamp(8, 512);
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(px, px)?;
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return None;
    }
    let s = (px as f32 / w).min(px as f32 / h);
    let dx = (px as f32 - w * s) / 2.0;
    let dy = (px as f32 - h * s) / 2.0;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_row(s, 0.0, 0.0, s, dx, dy), &mut pixmap.as_mut());
    // tiny-skia keeps premultiplied alpha, as egui's textures do
    let pixels: Vec<egui::Color32> =
        pixmap.pixels().iter().map(|p| egui::Color32::from_rgba_premultiplied(p.red(), p.green(), p.blue(), p.alpha())).collect();
    Some(egui::ColorImage { size: [px as usize, px as usize], source_size: egui::vec2(px as f32, px as f32), pixels })
}

/// The texture of icon `name` at `points` (sized for the screen's pixels).
pub fn texture(ctx: &egui::Context, name: &str, points: f32) -> Option<egui::TextureHandle> {
    let svg = ls::icons::svg(name)?;
    let px = (points * ctx.pixels_per_point()).round().clamp(8.0, 512.0) as u32;
    let c = cache(ctx);
    let mut map = c.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(t) = map.get(&(name.to_string(), px)) {
        return t.clone();
    }
    if map.len() >= MAX_CACHED {
        map.clear();
    }
    let tex = rasterize(svg, px).map(|img| ctx.load_texture(format!("surfaceIcon:{name}:{px}"), img, egui::TextureOptions::LINEAR));
    map.insert((name.to_string(), px), tex.clone());
    tex
}

/// Paint icon `name` into `rect` (nothing when there's no such icon).
pub fn paint(ui: &egui::Ui, name: &str, rect: egui::Rect, tint: egui::Color32) -> bool {
    paint_with(ui.ctx(), ui.painter(), name, rect, tint)
}

/// [`paint`] with any painter (the readout draws on its own layer).
pub fn paint_with(ctx: &egui::Context, painter: &egui::Painter, name: &str, rect: egui::Rect, tint: egui::Color32) -> bool {
    let side = rect.width().min(rect.height());
    let Some(t) = texture(ctx, name, side) else { return false };
    let r = egui::Rect::from_center_size(rect.center(), egui::vec2(side, side));
    painter.image(t.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pack_rasterises_and_bad_svg_does_not() {
        let svg = ls::icons::svg("exposure").expect("exposure icon");
        let img = rasterize(svg, 48).expect("rasterised");
        assert_eq!(img.size, [48, 48]);
        assert!(img.pixels.iter().any(|p| p.a() > 0), "something is drawn");
        assert!(rasterize("not svg", 32).is_none());
        assert!(rasterize("<svg xmlns='http://www.w3.org/2000/svg' width='0' height='0'/>", 32).is_none());
        // every icon of the pack reads
        for n in ls::icons::names() {
            assert!(rasterize(ls::icons::svg(n).unwrap_or_default(), 16).is_some(), "{n}");
        }
    }
}

/// Where an icon really draws, in its own viewBox units, found on a canvas twice its size (so a
/// drawing that spills over the viewBox shows): (left, top, right, bottom, viewBox width, height).
pub fn drawn_box(svg: &str) -> Option<(f32, f32, f32, f32, f32, f32)> {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).ok()?;
    let (w, h) = (tree.size().width(), tree.size().height());
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    let px = 256u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(px, px)?;
    // the viewBox in the middle half of the canvas
    let s = (px as f32 / 2.0) / w.max(h);
    let off = px as f32 / 4.0;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_row(s, 0.0, 0.0, s, off, off), &mut pixmap.as_mut());
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for (i, p) in pixmap.pixels().iter().enumerate() {
        if p.alpha() > 24 {
            let (x, y) = (i as u32 % px, i as u32 / px);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    if x0 > x1 {
        return None;
    }
    let u = |v: u32| (v as f32 - off) / s;
    Some((u(x0), u(y0), u(x1), u(y1), w, h))
}

#[cfg(test)]
mod placement {
    /// Every icon draws fully inside its key with a margin, horizontally centred, and not tiny:
    /// the keys show the whole drawing at about the same size (`assets/surface-icons`).
    #[test]
    fn every_icon_is_inside_centred_and_full_size() {
        let mut bad = Vec::new();
        for n in lightcraft_surface::icons::names() {
            let Some((l, t, r, b, w, h)) = super::drawn_box(lightcraft_surface::icons::svg(n).unwrap_or_default()) else {
                bad.push(format!("{n}: draws nothing"));
                continue;
            };
            if l < 0.5 || t < 0.5 || r > w - 0.5 || b > h - 0.5 {
                bad.push(format!("{n}: outside the key ({l:.1} {t:.1} {r:.1} {b:.1})"));
            }
            if ((l + r) / 2.0 - w / 2.0).abs() > 1.0 {
                bad.push(format!("{n}: off-centre ({:.1})", (l + r) / 2.0));
            }
            if (r - l).max(b - t) < 0.45 * w {
                bad.push(format!("{n}: small ({:.2})", (r - l).max(b - t) / w));
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");
    }
}
