//! Generative removal in two passes. The coarse pass sees the selection with wide context at the
//! model's resolution, so it can rebuild the scene behind it; when that crop had to be scaled
//! down, full-resolution detail tiles redraw the fill lightly, so it matches the photo's
//! sharpness. Every pass is blended through the full-resolution mask: unselected pixels never
//! change.
use std::collections::VecDeque;

use lightcraft_color::transfer::{encode_srgb8, srgb_to_linear};
use lightcraft_raster::Rgba8;
use serde_json::{Value, json};

use super::Settings;
use super::client::{Client, Pass};
use super::image::{self, Region};

/// Detail tiles' side at full resolution: the model's own working size, so tiles are 1:1.
pub(super) const TILE: usize = 1024;
/// At most this many detail tiles. Larger removals get proportionally larger tiles, which the
/// model then sees slightly scaled down (still far less than the coarse pass).
pub(super) const MAX_TILES: usize = 36;
/// The coarse fill keeps enough detail when its crop was scaled down by at most this factor.
const DETAIL_SCALE: f64 = 1.25;

/// The crop the coarse pass sees: the selection grown by `(factor - 1) ×` its longer side (half
/// on each side, at least 32 px), moved to stay inside the photo rather than shrunk.
pub(super) fn context_region(sel: Region, factor: f64, width: usize, height: usize) -> Region {
    let long = sel.w.max(sel.h) as f64;
    let pad = (((factor - 1.0).max(0.0) * long / 2.0).round() as usize).max(32);
    let axis = |start: usize, len: usize, limit: usize| {
        let size = len.saturating_add(pad.saturating_mul(2)).min(limit);
        let centre = start.saturating_add(len / 2);
        (centre.saturating_sub(size / 2).min(limit - size), size)
    };
    let (x, w) = axis(sel.x, sel.w, width);
    let (y, h) = axis(sel.y, sel.h, height);
    Region { x, y, w, h }
}

/// Whether the coarse fill was scaled down enough that detail tiles are worth their time.
pub(super) fn needs_detail(context: Region, detail: f64) -> bool {
    detail > 0.0 && context.w.max(context.h) as f64 > TILE as f64 * DETAIL_SCALE
}

/// The mask grown by `radius` pixels (a square maximum filter), so the outline and halo of what
/// is removed go too.
pub(super) fn grow(mask: &Rgba8, radius: usize) -> Rgba8 {
    if radius == 0 || !image::valid(mask) {
        return mask.clone();
    }
    let values: Vec<u8> = mask.data.iter().map(|p| p[0]).collect();
    let rows = max_filter(&values, mask.width, mask.height, radius, true);
    let grown = max_filter(&rows, mask.width, mask.height, radius, false);
    let mut out = Rgba8::new(mask.width, mask.height);
    for (p, v) in out.data.iter_mut().zip(grown) {
        *p = [v, v, v, 255];
    }
    out
}

/// The maximum over `[i - r, i + r]` along each row (`horizontal`) or column, in O(n).
fn max_filter(src: &[u8], width: usize, height: usize, r: usize, horizontal: bool) -> Vec<u8> {
    let mut out = vec![0u8; src.len()];
    let (lines, len) = if horizontal { (height, width) } else { (width, height) };
    let at = |line: usize, i: usize| if horizontal { line * width + i } else { i * width + line };
    let value = |line: usize, i: usize| src.get(at(line, i)).copied().unwrap_or(0);
    let mut window: VecDeque<usize> = VecDeque::new();
    for line in 0..lines {
        window.clear();
        let mut next = 0;
        for i in 0..len {
            let last = i.saturating_add(r).min(len - 1);
            while next <= last {
                let v = value(line, next);
                while window.back().is_some_and(|&b| value(line, b) <= v) {
                    window.pop_back();
                }
                window.push_back(next);
                next += 1;
            }
            while window.front().is_some_and(|&f| f + r < i) {
                window.pop_front();
            }
            if let Some(o) = out.get_mut(at(line, i)) {
                *o = window.front().map_or(0, |&f| value(line, f));
            }
        }
    }
    out
}

/// Tile starts of `size` (overlapping by at least `overlap`) covering `[start, start + len)`,
/// inside `[0, limit)`, evenly spread.
fn axis_tiles(start: usize, len: usize, size: usize, overlap: usize, limit: usize) -> Vec<usize> {
    let size = size.min(limit);
    if len <= size {
        let centre = start.saturating_add(len / 2);
        return vec![centre.saturating_sub(size / 2).min(limit - size)];
    }
    let step = size.saturating_sub(overlap).max(1);
    let count = (len - overlap.min(len)).div_ceil(step).max(2);
    (0..count).map(|i| (start + i * (len - size) / (count - 1)).min(limit - size)).collect()
}

fn has_selection(mask: &Rgba8, t: Region) -> bool {
    (t.y..t.y + t.h).any(|y| {
        let row = y * mask.width;
        mask.data.get(row + t.x..row + t.x + t.w).is_some_and(|px| px.iter().any(|p| p[0] > 0))
    })
}

/// Full-resolution detail tiles covering every selected pixel of `sel`. Tiles without a selected
/// pixel are left out; past [`MAX_TILES`] the tiles grow instead of multiplying.
pub(super) fn plan_tiles(mask: &Rgba8, sel: Region) -> Vec<Region> {
    if !image::valid(mask) {
        return Vec::new();
    }
    let mut size = TILE;
    let mut tiles = Vec::new();
    for _ in 0..16 {
        let overlap = size / 8;
        let (w, h) = (size.min(mask.width), size.min(mask.height));
        let xs = axis_tiles(sel.x, sel.w, size, overlap, mask.width);
        let ys = axis_tiles(sel.y, sel.h, size, overlap, mask.height);
        tiles = ys.iter().flat_map(|&y| xs.iter().map(move |&x| Region { x, y, w, h })).filter(|&t| has_selection(mask, t)).collect();
        if tiles.len() <= MAX_TILES || size >= mask.width.max(mask.height) {
            break;
        }
        let factor = (tiles.len() as f64 / MAX_TILES as f64).sqrt();
        size = ((size as f64 * factor / 32.0).ceil() as usize * 32).max(size + 32);
    }
    tiles
}

/// How much a tile's pixel at `v` counts: ramps from 0 at a tile edge inside the photo to 1 at
/// `ramp` pixels in, so neighbouring tiles and the coarse fill meet without seams. Edges on the
/// photo's border have no neighbour and do not ramp.
fn edge_weight(v: usize, start: usize, len: usize, limit: usize, ramp: f32) -> f32 {
    let mut w = 1.0_f32;
    if start > 0 {
        w = w.min(((v - start) as f32 + 0.5) / ramp);
    }
    if start + len < limit {
        w = w.min(((start + len - v) as f32 - 0.5) / ramp);
    }
    w.clamp(0.0, 1.0)
}

/// Blend one returned detail tile into `work`, in linear light, through the mask (relative to
/// the original photo) and the tile's edge ramps. Unselected pixels are never touched.
pub(super) fn blend_tile(work: &mut Rgba8, original: &Rgba8, mask: &Rgba8, tile: &Rgba8, t: Region) -> Result<(), String> {
    let same = |img: &Rgba8| image::valid(img) && (img.width, img.height) == (work.width, work.height);
    if !image::valid(work)
        || !same(original)
        || !same(mask)
        || !image::valid(tile)
        || image::network_size(t) != (tile.width, tile.height)
        || t.w == 0
        || t.h == 0
        || t.x.saturating_add(t.w) > work.width
        || t.y.saturating_add(t.h) > work.height
    {
        return Err("ComfyUI returned unexpected detail tile dimensions".into());
    }
    let ramp = (t.w.min(t.h) / 8).max(1) as f32;
    let width = work.width;
    for y in t.y..t.y + t.h {
        let wy = edge_weight(y, t.y, t.h, work.height, ramp);
        for x in t.x..t.x + t.w {
            let i = y * width + x;
            let m = mask.data.get(i).map_or(0, |p| p[0]);
            let w = edge_weight(x, t.x, t.w, width, ramp) * wy;
            if m == 0 || w <= 0.0 {
                continue;
            }
            let g = image::sample(tile, (x - t.x) as f64 * tile.width as f64 / t.w as f64, (y - t.y) as f64 * tile.height as f64 / t.h as f64);
            let a = f32::from(m) / 255.0;
            let (Some(o), Some(p)) = (original.data.get(i).copied(), work.data.get_mut(i)) else { continue };
            for ((p, o), g) in p.iter_mut().take(3).zip(o).zip(g) {
                let o = srgb_to_linear(f32::from(o) / 255.0);
                let target = o + (srgb_to_linear(g / 255.0) - o) * a;
                let current = srgb_to_linear(f32::from(*p) / 255.0);
                *p = encode_srgb8(current + (target - current) * w);
            }
        }
    }
    Ok(())
}

/// A finished removal and what went into it (for the recipe).
pub(super) struct Outcome {
    pub image: Rgba8,
    pub record: Value,
}

/// Run both passes. `mask` is already grown; `progress(done, total, what)` reports each request.
pub(super) fn run(
    client: &Client,
    settings: &Settings,
    original: &Rgba8,
    mask: &Rgba8,
    prefix: &str,
    cancelled: &(dyn Fn() -> bool + Sync),
    progress: &dyn Fn(u64, u64, &str),
) -> Result<Outcome, String> {
    let sel = image::bounds(mask)?;
    let context = context_region(sel, settings.context, mask.width, mask.height);
    let tiles = if needs_detail(context, settings.detail) { plan_tiles(mask, sel) } else { Vec::new() };
    let total = 1 + tiles.len() as u64;
    progress(0, total, "coarse fill");
    let coarse = super::request(client, &Pass::fill(settings), original, mask, context, &format!("{prefix}-fill"), cancelled)?;
    let mut work = image::composite(original.clone(), mask, &coarse, context)?;
    let mut seeds = Vec::with_capacity(tiles.len());
    for (i, &t) in tiles.iter().enumerate() {
        progress(1 + i as u64, total, &format!("detail {} of {}", i + 1, tiles.len()));
        let pass = Pass::detail(settings, i);
        seeds.push(pass.seed);
        // Each tile starts from the work so far, so overlaps continue earlier tiles.
        let out = super::request(client, &pass, &work, mask, t, &format!("{prefix}-tile{i}"), cancelled)?;
        blend_tile(&mut work, original, mask, &out, t)?;
    }
    progress(total, total, "done");
    let record = json!({"selection": sel, "context": context, "contextFactor": settings.context, "grow": settings.grow,
        "detail": settings.detail, "tiles": tiles, "tileSeeds": seeds, "sentDetailPrompt": settings.resolved_refine_prompt()});
    Ok(Outcome { image: work, record })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask_with(width: usize, height: usize, sel: Region) -> Rgba8 {
        let mut m = Rgba8::new(width, height);
        for y in sel.y..sel.y + sel.h {
            for x in sel.x..sel.x + sel.w {
                m.data[y * width + x] = [255, 255, 255, 255];
            }
        }
        m
    }

    #[test]
    fn context_grows_around_the_selection_and_shifts_inside_the_photo() {
        let sel = Region { x: 1000, y: 800, w: 400, h: 200 };
        // 2.5× → 0.75 × 400 = 300 px each side.
        assert_eq!(context_region(sel, 2.5, 5000, 4000), Region { x: 700, y: 500, w: 1000, h: 800 });
        // Near the top-left corner the crop moves right/down instead of shrinking.
        let corner = Region { x: 10, y: 5, w: 400, h: 200 };
        assert_eq!(context_region(corner, 2.5, 5000, 4000), Region { x: 0, y: 0, w: 1000, h: 800 });
        // Never larger than the photo; a factor of 1 still keeps 32 px of context.
        assert_eq!(context_region(sel, 6.0, 1500, 1200), Region { x: 0, y: 0, w: 1500, h: 1200 });
        assert_eq!(context_region(sel, 1.0, 5000, 4000), Region { x: 968, y: 768, w: 464, h: 264 });
    }

    #[test]
    fn detail_runs_only_when_the_coarse_crop_was_scaled_down() {
        assert!(!needs_detail(Region { x: 0, y: 0, w: 1200, h: 900 }, 0.35));
        assert!(needs_detail(Region { x: 0, y: 0, w: 3000, h: 900 }, 0.35));
        assert!(!needs_detail(Region { x: 0, y: 0, w: 3000, h: 900 }, 0.0));
    }

    #[test]
    fn grow_dilates_by_the_radius_and_keeps_soft_values() {
        let mut m = Rgba8::new(20, 10);
        m.data[5 * 20 + 10] = [255; 4];
        m.data[5 * 20 + 2] = [100, 100, 100, 255];
        let g = grow(&m, 2);
        for y in 0..10_usize {
            for x in 0..20_usize {
                let near = x.abs_diff(10) <= 2 && y.abs_diff(5) <= 2;
                let soft = x.abs_diff(2) <= 2 && y.abs_diff(5) <= 2;
                let want = if near {
                    255
                } else if soft {
                    100
                } else {
                    0
                };
                assert_eq!(g.data[y * 20 + x][0], want, "({x}, {y})");
            }
        }
        assert_eq!(grow(&m, 0).data, m.data);
        // A radius larger than the image fills it, without overflow.
        assert!(grow(&m, 1000).data.iter().all(|p| p[0] == 255));
    }

    #[test]
    fn tiles_cover_every_selected_pixel_with_overlap_inside_the_photo() {
        let (w, h) = (5184, 3888);
        let sel = Region { x: 900, y: 700, w: 2600, h: 1500 };
        let mask = mask_with(w, h, sel);
        let tiles = plan_tiles(&mask, sel);
        assert!(!tiles.is_empty() && tiles.len() <= MAX_TILES);
        for t in &tiles {
            assert_eq!((t.w, t.h), (TILE, TILE));
            assert!(t.x + t.w <= w && t.y + t.h <= h);
            assert_eq!(image::network_size(*t), (TILE, TILE), "tiles reach the model 1:1");
        }
        for y in (sel.y..sel.y + sel.h).step_by(7) {
            for x in (sel.x..sel.x + sel.w).step_by(7) {
                assert!(tiles.iter().any(|t| x >= t.x && x < t.x + t.w && y >= t.y && y < t.y + t.h), "({x}, {y}) uncovered");
            }
        }
        // Neighbours overlap by at least the ramp, so blends have room.
        let mut xs: Vec<usize> = tiles.iter().map(|t| t.x).collect();
        xs.sort_unstable();
        xs.dedup();
        assert!(xs.windows(2).all(|p| p[1] - p[0] <= TILE - TILE / 8));
    }

    #[test]
    fn tiles_skip_unselected_areas_and_grow_past_the_limit() {
        // Two small selections far apart: only the tiles over them.
        let (w, h) = (6000, 4000);
        let mut mask = mask_with(w, h, Region { x: 100, y: 100, w: 50, h: 50 });
        for y in 3800..3850 {
            for x in 5800..5850 {
                mask.data[y * w + x] = [255; 4];
            }
        }
        let tiles = plan_tiles(&mask, image::bounds(&mask).unwrap());
        assert!(tiles.len() < 20, "{}", tiles.len());
        assert!(tiles.iter().all(|&t| has_selection(&mask, t)));
        // A whole 80 MP frame stays within the limit with larger tiles.
        let full = Region { x: 0, y: 0, w: 10_328, h: 7_746 };
        let mask = mask_with(full.w, full.h, full);
        let tiles = plan_tiles(&mask, full);
        assert!(tiles.len() <= MAX_TILES && tiles.len() > 1, "{}", tiles.len());
        assert!(tiles.iter().all(|t| t.w > TILE && t.x + t.w <= full.w && t.y + t.h <= full.h));
        // A photo smaller than a tile gets one tile of the photo's size.
        let small = mask_with(600, 400, Region { x: 10, y: 10, w: 500, h: 300 });
        assert_eq!(plan_tiles(&small, image::bounds(&small).unwrap()), vec![Region { x: 0, y: 0, w: 600, h: 400 }]);
        assert!(plan_tiles(&Rgba8::new(0, 0), full).is_empty());
    }

    #[test]
    fn tile_blend_changes_only_selected_pixels_and_ramps_at_inner_edges() {
        let (w, h) = (300, 200);
        let mut original = Rgba8::new(w, h);
        original.data.fill([120, 120, 120, 255]);
        let mask = mask_with(w, h, Region { x: 0, y: 0, w: 250, h: 200 });
        let mut work = original.clone();
        let t = Region { x: 64, y: 0, w: 160, h: 160 };
        let (tw, th) = image::network_size(t);
        let mut tile = Rgba8::new(tw, th);
        tile.data.fill([240, 10, 10, 255]);
        blend_tile(&mut work, &original, &mask, &tile, t).unwrap();
        // Outside the tile and outside the mask: untouched.
        assert_eq!(work.data[10 * w + 10], original.data[10 * w + 10]);
        assert_eq!(work.data[180 * w + 100], original.data[180 * w + 100]);
        // Tile centre: fully the tile.
        assert_eq!(work.data[80 * w + 144][..3], [240, 10, 10]);
        // Inner left edge ramps from the work towards the tile; the top edge is the photo's
        // border, so it does not ramp.
        let edge = work.data[80 * w + 64];
        assert!(edge[0] > 120 && edge[0] < 240, "{edge:?}");
        assert_eq!(work.data[144][..3], [240, 10, 10]);
        // Wrong tile size is an error, not a panic.
        assert!(blend_tile(&mut work, &original, &mask, &Rgba8::new(10, 10), t).is_err());
        let outside = Region { x: 250, y: 100, w: 160, h: 160 };
        assert!(blend_tile(&mut work, &original, &mask, &tile, outside).is_err());
    }
}
