//! Map tiles on screen: the textures of the tiles the engine's tile service has loaded
//! (`lightcraft_engine::map::TileService`), drawn for a map view of any size.
//!
//! - One texture cache for every map on screen, keyed by (tile source, z, x, y), least recently
//!   used evicted past [`TEXTURE_CAP`]; a tile drawn in the current frame is never evicted. It lives
//!   in the egui context's memory, so each window (and each headless test) has its own.
//! - A tile not loaded yet is drawn from a loaded ancestor (up to [`PARENT_LEVELS`] zooms up),
//!   scaled; with none, the map's grey shows through with a faint outline.
//! - The service is told the union of the tiles the maps wanted this frame and the last; when no
//!   map is drawn for two frames ([`end_frame`]) it is told nothing, so it stops fetching.
//! - The offline source draws the grey map with a note and starts no tile service.
//!
//! The attribution of the source is painted by the views (always visible, bottom right).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use egui::{Align2, Color32, ColorImage, FontId, Painter, Rect, Stroke, StrokeKind, TextureHandle, TextureOptions, pos2, vec2};
use lightcraft_engine::Session;
use lightcraft_engine::map::{TileEvent, TileKey, TileService, Viewport};
use lightcraft_geom::LatLon;

/// Tile textures kept on the GPU (256 × 256 × 4 bytes each: 64 MB at most).
pub const TEXTURE_CAP: usize = 256;
/// How many zooms up a missing tile looks for a loaded ancestor to draw instead.
pub const PARENT_LEVELS: u8 = 4;
/// The map's colour where no tile is drawn.
pub const MAP_GREY: Color32 = Color32::from_rgb(214, 218, 222);

type TexKey = (String, TileKey);

#[derive(Default)]
pub(crate) struct Store {
    textures: HashMap<TexKey, (TextureHandle, u64)>,
    /// Tiles wanted by the maps drawn in the pass `frame`, and in the pass before.
    this_frame: Vec<TileKey>,
    last_frame: Vec<TileKey>,
    frame: u64,
}

impl Store {
    /// Start pass `pass` if it hasn't been started: this frame's wanted tiles become the last's.
    fn roll(&mut self, pass: u64) {
        if self.frame != pass {
            self.last_frame = std::mem::take(&mut self.this_frame);
            self.frame = pass;
        }
    }

    /// The tiles to ask the service for: this frame's, then the last frame's not yet listed.
    fn wanted(&self) -> Vec<TileKey> {
        let mut out = self.this_frame.clone();
        for k in &self.last_frame {
            if !out.contains(k) {
                out.push(*k);
            }
        }
        out
    }

    /// The texture of `key`, uploading the service's decoded tile if it has one.
    fn texture(&mut self, ctx: &egui::Context, svc: &TileService, source: &str, key: TileKey, pass: u64) -> Option<egui::TextureId> {
        let k = (source.to_string(), key);
        if let Some((tex, used)) = self.textures.get_mut(&k) {
            *used = pass;
            return Some(tex.id());
        }
        let img = svc.get(key)?;
        let size = [usize::try_from(img.width).ok()?, usize::try_from(img.height).ok()?];
        if size[0].checked_mul(size[1]).and_then(|n| n.checked_mul(4)) != Some(img.rgba.len()) {
            return None;
        }
        let image = ColorImage::from_rgba_unmultiplied(size, &img.rgba);
        let tex = ctx.load_texture(format!("map-tile-{source}-{}-{}-{}", key.z, key.x, key.y), image, TextureOptions::LINEAR);
        let id = tex.id();
        self.textures.insert(k, (tex, pass));
        Some(id)
    }

    /// Drop least recently used textures past the cap (never one drawn in pass `pass`).
    fn evict(&mut self, pass: u64) {
        if self.textures.len() <= TEXTURE_CAP {
            return;
        }
        let mut old: Vec<(u64, TexKey)> =
            self.textures.iter().filter(|(_, (_, used))| *used != pass).map(|(k, (_, used))| (*used, k.clone())).collect();
        old.sort_by_key(|(used, _)| *used);
        let excess = self.textures.len() - TEXTURE_CAP;
        for (_, k) in old.into_iter().take(excess) {
            self.textures.remove(&k);
        }
    }
}

/// The context's store (created on first use).
fn store(ctx: &egui::Context) -> Arc<Mutex<Store>> {
    let id = egui::Id::unique("lightcraft-map-tiles");
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Arc<Mutex<Store>>>(id).clone())
}

/// What [`draw`] did (for tests and `ui.inspect`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drawn {
    /// Tiles drawn from their own texture.
    pub tiles: usize,
    /// Tiles drawn from an ancestor's texture, scaled.
    pub from_parent: usize,
    /// Tiles with nothing to draw yet (grey).
    pub missing: usize,
    /// The source is offline: no tiles, no service.
    pub offline: bool,
}

/// The tile zoom shown for a view at `zoom` is a whole zoom; tiles are 256 points a side at it.
fn viewport(rect: Rect, center: LatLon, zoom: f64) -> Viewport {
    Viewport::new(center, zoom, f64::from(rect.width()), f64::from(rect.height()))
}

/// Draw the tiles of the map centred on `center` at `zoom` into `rect` (clipped by `painter`),
/// and ask the tile service for them (`ring`: also the ring of tiles around the view, at most one,
/// for a map that pans; 0 for a small fixed map).
pub fn draw(session: &mut Session, painter: &Painter, rect: Rect, center: LatLon, zoom: f64, ring: u32) -> Drawn {
    let ctx = painter.ctx().clone();
    let pass = ctx.cumulative_pass_nr();
    painter.rect_filled(rect, 0.0, MAP_GREY);
    let source = session.map.effective_source();
    if source.is_offline() {
        let at = if rect.height() < 240.0 { egui::pos2(rect.center().x, rect.top() + 14.0) } else { rect.center() };
        painter.text(
            at,
            Align2::CENTER_CENTER,
            crate::i18n::tr("Map tiles are off (Settings › Map)"),
            FontId::proportional(12.0),
            Color32::from_rgb(90, 96, 104),
        );
        return Drawn { offline: true, ..Drawn::default() };
    }
    let Some(svc) = session.map_tiles() else { return Drawn { offline: true, ..Drawn::default() } };
    if !svc.can_load() {
        // wasm32 has no tile transport yet: say so instead of leaving a silent grey map
        let at = if rect.height() < 240.0 { egui::pos2(rect.center().x, rect.top() + 14.0) } else { rect.center() };
        painter.text(
            at,
            Align2::CENTER_CENTER,
            crate::i18n::tr("Map tiles can't load in this build"),
            FontId::proportional(12.0),
            Color32::from_rgb(90, 96, 104),
        );
        return Drawn { offline: true, ..Drawn::default() };
    }
    let view = viewport(rect, center, zoom);
    let max_zoom = source.max_zoom();
    let source_id = source.id().to_string();

    let shared = store(&ctx);
    let mut guard = shared.lock().unwrap_or_else(PoisonError::into_inner);
    let store = &mut *guard;
    store.roll(pass);
    // a refreshed tile replaces its texture (uploaded again from the service on the next draw)
    let events = svc.poll();
    for e in &events {
        if let TileEvent::Ready { key, .. } = e {
            store.textures.remove(&(source_id.clone(), *key));
        }
    }
    for k in view.tile_keys(max_zoom, ring.min(1)) {
        if !store.this_frame.contains(&k) {
            store.this_frame.push(k);
        }
    }
    svc.want(&store.wanted());

    let mut drawn = Drawn::default();
    let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    for vt in view.tiles(max_zoom) {
        let r = Rect::from_min_max(
            pos2(rect.left() + vt.rect.x0 as f32, rect.top() + vt.rect.y0 as f32),
            pos2(rect.left() + vt.rect.x1 as f32, rect.top() + vt.rect.y1 as f32),
        );
        if !r.intersects(rect) {
            continue;
        }
        if let Some(id) = store.texture(&ctx, svc, &source_id, vt.key, pass) {
            painter.image(id, r, full, Color32::WHITE);
            drawn.tiles += 1;
            continue;
        }
        let parent = (1..=PARENT_LEVELS.min(vt.key.z)).find_map(|d| {
            let pk = TileKey::new(vt.key.z - d, vt.key.x >> d, vt.key.y >> d)?;
            let id = store.texture(&ctx, svc, &source_id, pk, pass)?;
            // the part of the ancestor this tile covers
            let n = f32::from(1u16 << d);
            let (ox, oy) = ((vt.key.x - (pk.x << d)) as f32, (vt.key.y - (pk.y << d)) as f32);
            Some((id, Rect::from_min_size(pos2(ox / n, oy / n), vec2(1.0 / n, 1.0 / n))))
        });
        match parent {
            Some((id, uv)) => {
                painter.image(id, r, uv, Color32::WHITE);
                drawn.from_parent += 1;
            }
            None => {
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_black_alpha(18)), StrokeKind::Inside);
                drawn.missing += 1;
            }
        }
    }
    store.evict(pass);
    drop(guard);
    if !events.is_empty() {
        ctx.request_repaint();
    } else if drawn.missing + drawn.from_parent > 0 || !session.map.service().is_none_or(TileService::is_idle) {
        // tiles arrive on the service's threads: look again shortly
        ctx.request_repaint_after(Duration::from_millis(100));
    }
    drawn
}

/// Call once per frame after every map was drawn: when no map drew this frame nor the last, the
/// tile service is told nothing is wanted, so it stops fetching (the OSM policy: only what is in
/// view). Never starts a service.
pub fn end_frame(session: &Session, ctx: &egui::Context) {
    let pass = ctx.cumulative_pass_nr();
    let shared = store(ctx);
    let mut guard = shared.lock().unwrap_or_else(PoisonError::into_inner);
    let store = &mut *guard;
    if store.frame == pass {
        return;
    }
    store.roll(pass);
    if let Some(svc) = session.map.service() {
        svc.want(&store.wanted());
    }
}

/// Texture count, for tests and diagnostics.
pub fn texture_count(ctx: &egui::Context) -> usize {
    store(ctx).lock().unwrap_or_else(PoisonError::into_inner).textures.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wanted_tiles_are_this_frame_and_the_last() {
        let k = |x| TileKey::new(5, x, 1).unwrap();
        let mut s = Store::default();
        s.roll(1);
        s.this_frame = vec![k(1), k(2)];
        s.roll(2);
        s.this_frame.push(k(3));
        assert_eq!(s.wanted(), vec![k(3), k(1), k(2)]);
        // a frame with no map, then another: nothing left
        s.roll(3);
        assert_eq!(s.wanted(), vec![k(3)]);
        s.roll(4);
        assert!(s.wanted().is_empty());
    }
}
