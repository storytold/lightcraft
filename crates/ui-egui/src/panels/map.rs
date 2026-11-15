//! Map module view: interactive slippy map canvas, dual-provider tile engine (OSM & Esri),
//! photo marker clustering, loupe popover, GPX tracklog overlay, and saved locations.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError, Weak};
use std::time::Duration;
use web_time::Instant;

use egui::{Align2, Color32, ColorImage, FontId, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_engine::map::{LocationFilter, MapProvider};
use lightcraft_geom::{GeoBounds, LatLon, TileCoord};
use serde_json::json;

use crate::LightcraftApp;
use crate::state::ViewMode;
use crate::theme::Tokens;

/// Number of screen pixels per tile at standard resolution.
const TILE_SIZE: f32 = 256.0;

/// Screen distance threshold in points for merging photo markers into a cluster.
const CLUSTER_RADIUS_PTS: f32 = 36.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TileKey {
    provider: MapProvider,
    z: u8,
    x: u32,
    y: u32,
    layer: u8,
}

/// At most this many tiles download at once. One connection per visible tile (60 and more on a
/// large screen) left most of them stuck past the 6 s connect timeout; a small pool loads them all.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // the web has no tile fetcher (see `fetch_and_cache_tile`)
const MAX_TILE_WORKERS: usize = 6;

/// A tile whose load failed is asked for again after this long, not on every frame.
const TILE_RETRY_AFTER: Duration = Duration::from_secs(5);

/// A finished tile load: the image, or `None` when it could not be fetched or decoded.
type TileResult = (TileKey, Option<ColorImage>);

struct TileStore {
    textures: HashMap<TileKey, TextureHandle>,
    pending: HashSet<TileKey>,
    /// Tiles whose last load failed, and when; they are retried after [`TILE_RETRY_AFTER`].
    failed: HashMap<TileKey, Instant>,
    tx: Sender<TileResult>,
    rx: Receiver<TileResult>,
    /// The tile loads in flight, while there are any. Only the queued jobs hold it strongly.
    batch: Weak<TileBatch>,
}

impl TileStore {
    fn new() -> Self {
        let (tx, rx) = channel();
        Self { textures: HashMap::new(), pending: HashSet::new(), failed: HashMap::new(), tx, rx, batch: Weak::new() }
    }

    /// Whether `key` should be requested now: not loaded, not in flight, and not failed recently.
    fn wants(&self, key: &TileKey, now: Instant) -> bool {
        !self.textures.contains_key(key)
            && !self.pending.contains(key)
            && self.failed.get(key).is_none_or(|at| now.saturating_duration_since(*at) >= TILE_RETRY_AFTER)
    }

    /// Takes in a finished load: a failed one leaves `pending` so it can be retried later.
    fn finish(&mut self, key: TileKey, ok: bool, now: Instant) {
        self.pending.remove(&key);
        if ok {
            self.failed.remove(&key);
        } else {
            if self.failed.len() > 1000 {
                self.failed.retain(|_, at| now.saturating_duration_since(*at) < TILE_RETRY_AFTER);
            }
            self.failed.insert(key, now);
        }
    }
}

/// One tile to load. Dropping it (done or never run) counts it as done in the batch's row.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
struct TileJob {
    key: TileKey,
    url: String,
    disk_path: std::path::PathBuf,
    tx: Sender<TileResult>,
    ctx: egui::Context,
    /// The tile failed before: it waits behind every tile not yet tried (see [`TileQueue::push`]).
    retry: bool,
    _counted: TileLoadEnd,
}

/// A bounded set of worker threads that load tiles, newest first-attempt request first, so the
/// tiles on screen now come before ones the view has already panned past; retries come last.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
struct TilePool {
    queue: Mutex<TileQueue>,
    wake: Condvar,
    load: fn(&TileJob) -> Option<ColorImage>,
    max_workers: usize,
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
struct TileQueue {
    /// Popped from the end: first attempts are pushed there (newest first), retries go to the
    /// front, so a tile that keeps failing cannot starve tiles that have never been tried.
    jobs: Vec<TileJob>,
    workers: usize,
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
impl TileQueue {
    /// Queues `job`: a first attempt is taken before every queued job, a retry after all of them.
    fn push(&mut self, job: TileJob) {
        if job.retry {
            self.jobs.insert(0, job);
        } else {
            self.jobs.push(job);
        }
    }
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
impl TilePool {
    fn new(load: fn(&TileJob) -> Option<ColorImage>, max_workers: usize) -> Arc<Self> {
        Arc::new(Self { queue: Mutex::new(TileQueue { jobs: Vec::new(), workers: 0 }), wake: Condvar::new(), load, max_workers })
    }

    fn submit(self: &Arc<Self>, job: TileJob) {
        let mut q = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        q.push(job);
        if q.workers < self.max_workers {
            let pool = Arc::clone(self);
            let spawned = std::thread::Builder::new().name("lc-tile-fetch".into()).spawn(move || pool.work()).is_ok();
            if spawned {
                q.workers += 1;
            } else if q.workers == 0 {
                // no worker to run it: report the jobs as failed so they are retried, not stuck
                for job in q.jobs.drain(..) {
                    let _ = job.tx.send((job.key, None));
                }
            }
        }
        drop(q);
        self.wake.notify_one();
    }

    /// Runs jobs until none has come for a while, then ends the thread.
    fn work(&self) {
        loop {
            let mut q = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
            let job = loop {
                if let Some(job) = q.jobs.pop() {
                    break job;
                }
                let (guard, timeout) = self.wake.wait_timeout(q, Duration::from_secs(20)).unwrap_or_else(PoisonError::into_inner);
                q = guard;
                if timeout.timed_out() && q.jobs.is_empty() {
                    q.workers = q.workers.saturating_sub(1);
                    return;
                }
            };
            drop(q);
            let img = (self.load)(&job);
            let _ = job.tx.send((job.key, img));
            job.ctx.request_repaint();
        }
    }
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
static TILE_POOL: OnceLock<Arc<TilePool>> = OnceLock::new();

/// The tile loads in flight share one activity row. Each worker holds an `Arc` until it ends, so
/// the last one to finish (fetched, failed or not started) drops the row, whether or not the Map
/// view is still drawn.
struct TileBatch {
    guard: lightcraft_engine::activity::TaskGuard,
    done: AtomicU64,
    total: AtomicU64,
}

impl TileBatch {
    fn progress(&self) {
        self.guard.progress(self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed));
    }

    fn finish(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
        self.progress();
    }
}

/// Held by a tile worker: counts its load as done when the worker ends, then lets go of the batch.
struct TileLoadEnd(Arc<TileBatch>);

impl Drop for TileLoadEnd {
    fn drop(&mut self) {
        self.0.finish();
    }
}

/// Counts one more tile load into the batch in flight, starting a batch (and its row) if none is.
fn join_tile_batch(slot: &mut Weak<TileBatch>, activity: &lightcraft_engine::activity::Activity) -> Arc<TileBatch> {
    let batch = slot.upgrade().unwrap_or_else(|| {
        let batch = Arc::new(TileBatch {
            guard: activity.start("download", "Loading map tiles", lightcraft_engine::activity::Cancel::No),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
        });
        *slot = Arc::downgrade(&batch);
        batch
    });
    batch.total.fetch_add(1, Ordering::Relaxed);
    batch.progress();
    batch
}

static TILES: Mutex<Option<TileStore>> = Mutex::new(None);

/// Represents a clustered group of photos on the map screen.
struct MapCluster {
    center: Pos2,
    lat_lon: LatLon,
    photos: Vec<PhotoId>,
    has_selected: bool,
}

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let _t = Tokens::get(ui.ctx());
    let rect = ui.available_rect_before_wrap();
    if rect.width() <= 10.0 || rect.height() <= 10.0 {
        return;
    }

    let mut map_ui = app.ui.map.clone();
    let mut center = LatLon::new(map_ui.center_lat, map_ui.center_lon).unwrap_or(LatLon { lat: 45.57, lon: -73.2 });
    let mut zoom = map_ui.zoom.clamp(1.0, map_ui.provider.max_zoom() as f64);

    let (left_w, navigator_h) = if map_ui.navigator_open { (220.0_f32, 160.0_f32) } else { (0.0, 0.0) };
    let canvas_rect = Rect::from_min_max(pos2(rect.left() + left_w, rect.top()), rect.max);

    // 1. Background Map Canvas & Interaction
    let canvas_response = ui.allocate_rect(canvas_rect, Sense::click_and_drag());
    let painter = ui.painter_at(canvas_rect);

    // Canvas background fill
    let bg_color = match map_ui.provider {
        MapProvider::EsriImagery | MapProvider::EsriHybrid => Color32::from_rgb(18, 24, 28),
        _ => Color32::from_rgb(228, 234, 238),
    };
    painter.rect_filled(canvas_rect, 0.0, bg_color);

    let screen_center = canvas_rect.center();

    let lat_lon_to_screen = |coord: LatLon, cur_zoom: f64, cur_center: LatLon| -> Pos2 {
        let (c_tx, c_ty) = cur_center.to_tile_coords(cur_zoom);
        let (tx, ty) = coord.to_tile_coords(cur_zoom);
        pos2(screen_center.x + ((tx - c_tx) * TILE_SIZE as f64) as f32, screen_center.y + ((ty - c_ty) * TILE_SIZE as f64) as f32)
    };

    let screen_to_lat_lon = |screen_pos: Pos2, cur_zoom: f64, cur_center: LatLon| -> LatLon {
        let (c_tx, c_ty) = cur_center.to_tile_coords(cur_zoom);
        let tx = c_tx + (screen_pos.x - screen_center.x) as f64 / TILE_SIZE as f64;
        let ty = c_ty + (screen_pos.y - screen_center.y) as f64 / TILE_SIZE as f64;
        LatLon::from_tile_coords(tx, ty, cur_zoom)
    };

    // Pan with mouse drag
    if canvas_response.dragged() && !canvas_response.drag_started() {
        let delta = canvas_response.drag_delta();
        let n = 2.0_f64.powf(zoom.clamp(0.0, 24.0));
        let delta_tile_x = -delta.x as f64 / TILE_SIZE as f64;
        let delta_tile_y = -delta.y as f64 / TILE_SIZE as f64;

        let (mut cur_tx, mut cur_ty) = center.to_tile_coords(zoom);
        cur_tx = (cur_tx + delta_tile_x).rem_euclid(n);
        cur_ty = (cur_ty + delta_tile_y).clamp(0.0, n);
        center = LatLon::from_tile_coords(cur_tx, cur_ty, zoom);
    }

    // Scroll wheel zoom anchored to cursor
    if canvas_response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        let double_click = canvas_response.double_clicked();
        let delta_zoom = if double_click {
            1.0
        } else if scroll.abs() > 0.5 {
            scroll as f64 * 0.005
        } else {
            0.0
        };

        if delta_zoom != 0.0 {
            let max_z = map_ui.provider.max_zoom() as f64;
            let new_zoom = (zoom + delta_zoom).clamp(1.0, max_z);
            if let Some(pointer_pos) = ui.input(|i| i.pointer.hover_pos()).filter(|&pos| canvas_rect.contains(pos)) {
                let mouse_geo = screen_to_lat_lon(pointer_pos, zoom, center);
                let (m_tx, m_ty) = mouse_geo.to_tile_coords(new_zoom);
                let n = 2.0_f64.powf(new_zoom.clamp(0.0, 24.0));
                let new_center_tx = (m_tx - (pointer_pos.x - screen_center.x) as f64 / TILE_SIZE as f64).rem_euclid(n);
                let new_center_ty = (m_ty - (pointer_pos.y - screen_center.y) as f64 / TILE_SIZE as f64).clamp(0.0, n);
                center = LatLon::from_tile_coords(new_center_tx, new_center_ty, new_zoom);
            }
            zoom = new_zoom;
        }
    }

    // Draw Map Grid / Tile outlines
    render_tile_grid(&painter, &app.session.activity, canvas_rect, screen_center, center, zoom, map_ui.provider);

    // 2. Fetch and Cluster Candidate Photos
    let candidate_photos = collect_photos(app, map_ui.filter, |p| screen_to_lat_lon(p, zoom, center), canvas_rect);
    let clusters = cluster_photos(&candidate_photos, |coord| lat_lon_to_screen(coord, zoom, center), canvas_rect);

    // 3. Render Clusters and Markers
    let mut clicked_photo = None;
    let mut double_clicked_photo = None;
    let mut zoom_target = None;

    for cluster in &clusters {
        let marker_rect = Rect::from_center_size(cluster.center, vec2(32.0, 32.0));
        let is_hovered = ui.rect_contains_pointer(marker_rect);

        if cluster.photos.len() == 1 {
            let photo_id = cluster.photos[0];
            let is_selected = cluster.has_selected;
            render_single_pin(&painter, cluster.center, is_selected, is_hovered);

            if is_hovered && ui.input(|i| i.pointer.primary_clicked()) {
                clicked_photo = Some(photo_id);
            }
            if is_hovered && ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
                double_clicked_photo = Some(photo_id);
            }
        } else {
            render_cluster_badge(&painter, cluster.center, cluster.photos.len(), cluster.has_selected, is_hovered);

            if is_hovered && ui.input(|i| i.pointer.primary_clicked()) {
                clicked_photo = cluster.photos.first().copied();
            }
            if is_hovered && ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
                let next_z = (zoom + 2.0).min(map_ui.provider.max_zoom() as f64);
                zoom_target = Some((cluster.lat_lon, next_z));
            }
        }
    }

    if let Some((target_center, next_z)) = zoom_target {
        center = target_center;
        zoom = next_z;
    }

    if let Some(id) = clicked_photo {
        map_ui.active_pin_photo = Some(id);
        let _ = app.run("library.select", json!({"ids": [id.0]}));
    }
    if let Some(id) = double_clicked_photo {
        let _ = app.run("library.select", json!({"ids": [id.0]}));
        app.ui.view = ViewMode::Detail;
    }

    // 4. Render Active Pin Loupe Popover Card
    if let Some(active_id) = map_ui.active_pin_photo {
        let pin_coord = app.session.catalog.photo(active_id).and_then(|ph| ph.meta.gps).and_then(|(lat, lon)| LatLon::new(lat, lon));
        if let Some(coord) = pin_coord {
            let pin_pos = lat_lon_to_screen(coord, zoom, center);
            if canvas_rect.contains(pin_pos) {
                render_loupe_popover(app, ui, pin_pos, active_id, &mut map_ui);
            }
        }
    }

    // 4.5. Render active / matching saved location boundary circle
    for loc in &app.session.saved_locations {
        if (loc.lat - center.lat).abs() < 0.0001 && (loc.lon - center.lon).abs() < 0.0001 {
            let loc_coord = LatLon { lat: loc.lat, lon: loc.lon };
            let screen_pos = lat_lon_to_screen(loc_coord, zoom, center);
            let meters_per_px = (40075016.686 * (loc.lat.to_radians().cos().abs())) / (256.0 * 2.0_f64.powf(zoom));
            if meters_per_px > 0.0 {
                let radius_px = (loc.radius / meters_per_px) as f32;
                if radius_px > 4.0 && radius_px < 8000.0 {
                    painter.circle_filled(screen_pos, radius_px, Color32::from_rgba_unmultiplied(255, 204, 0, 20));
                    painter.circle_stroke(screen_pos, radius_px, Stroke::new(1.5, Color32::from_rgba_unmultiplied(255, 204, 0, 160)));
                }
            }
        }
    }

    // 5. Top Location Filter Bar & Breadcrumb
    if map_ui.show_filter_bar {
        render_top_filter_bar(app, ui, canvas_rect, &mut map_ui, center);
    }

    // 6. Bottom Map Toolbar (Style dropdown, Zoom slider, Lock button, Scale & Attribution)
    render_bottom_toolbar(app, ui, canvas_rect, &mut map_ui, &mut zoom);

    // 7. Map Legend Overlay
    if map_ui.legend_open {
        render_legend_overlay(ui, canvas_rect, &mut map_ui);
    }

    // 8. Left Navigator Mini-Map and Saved Locations Panel
    if map_ui.navigator_open {
        render_left_navigator(app, ui, (rect, canvas_rect), (left_w, navigator_h), &mut center, &mut zoom, &mut map_ui);
    }

    // Update persistent state
    map_ui.center_lat = center.lat;
    map_ui.center_lon = center.lon;
    map_ui.zoom = zoom;
    app.ui.map = map_ui;
}

/// Queues a tile load on the shared pool. On the web there is no fetcher, so the job is dropped.
fn fetch_and_cache_tile(job: TileJob) {
    #[cfg(not(target_arch = "wasm32"))]
    TILE_POOL.get_or_init(|| TilePool::new(load_tile, MAX_TILE_WORKERS)).submit(job);
    #[cfg(target_arch = "wasm32")]
    drop(job);
}

/// Reads a tile from the disk cache, or downloads it and caches it, then decodes it.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn load_tile(job: &TileJob) -> Option<ColorImage> {
    if let Ok(cached) = std::fs::read(&job.disk_path) {
        if let Some(img) = decode_image(&cached) {
            return Some(img);
        }
        // a cached file that does not decode (truncated, or from a bad response) is replaced, not retried forever
        let _ = std::fs::remove_file(&job.disk_path);
    }
    let bytes = match lightcraft_engine::map::fetch_tile_bytes(&job.url) {
        Ok(b) => b,
        Err(e) => {
            log::debug!("tile fetch error for {}: {e}", job.url);
            return None;
        }
    };
    // decode before caching, so a bad response is never written to the cache
    let img = decode_image(&bytes)?;
    if let Some(parent) = job.disk_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&job.disk_path, &bytes);
    Some(img)
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn decode_image(bytes: &[u8]) -> Option<ColorImage> {
    let rgba = lightcraft_engine::map::decode_tile(bytes).ok()?;
    Some(ColorImage::from_rgba_unmultiplied([rgba.width, rgba.height], &rgba.as_bytes()))
}

fn render_tile_grid(
    p: &Painter,
    activity: &lightcraft_engine::activity::Activity,
    canvas: Rect,
    center_pos: Pos2,
    center: LatLon,
    zoom: f64,
    provider: MapProvider,
) {
    let int_zoom = (zoom.floor() as u8).min(provider.max_zoom());
    let scale = 2.0_f64.powf(zoom - int_zoom as f64);
    let tile_screen_size = (TILE_SIZE as f64 * scale) as f32;
    let (center_int_tx, center_int_ty) = center.to_tile_coords(int_zoom as f64);
    let n = 2.0_f64.powi(int_zoom as i32);

    let min_x = ((center_int_tx - (center_pos.x - canvas.left()) as f64 / tile_screen_size as f64).floor() as i64).max(0);
    let max_x = ((center_int_tx + (canvas.right() - center_pos.x) as f64 / tile_screen_size as f64).ceil() as i64).min(n as i64 - 1);
    let min_y = ((center_int_ty - (center_pos.y - canvas.top()) as f64 / tile_screen_size as f64).floor() as i64).max(0);
    let max_y = ((center_int_ty + (canvas.bottom() - center_pos.y) as f64 / tile_screen_size as f64).ceil() as i64).min(n as i64 - 1);

    let grid_stroke = match provider {
        MapProvider::EsriImagery | MapProvider::EsriHybrid => Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 30)),
        _ => Stroke::new(1.0, Color32::from_rgba_unmultiplied(0, 0, 0, 25)),
    };

    let text_color = match provider {
        MapProvider::EsriImagery | MapProvider::EsriHybrid => Color32::from_rgba_unmultiplied(200, 210, 220, 100),
        _ => Color32::from_rgba_unmultiplied(80, 90, 100, 120),
    };

    let mut store_guard = TILES.lock().unwrap_or_else(PoisonError::into_inner);
    let store = store_guard.get_or_insert_with(TileStore::new);

    let now = Instant::now();
    while let Ok((key, img)) = store.rx.try_recv() {
        let ok = img.is_some();
        if let Some(img) = img {
            let name = format!("tile-{}-{}-{}-{}-{}", key.provider.id(), key.layer, key.z, key.x, key.y);
            let tex = p.ctx().load_texture(name, img, TextureOptions::LINEAR);
            store.textures.insert(key, tex);
        } else {
            // draw again once the failed tile may be retried, even if nothing else moves
            p.ctx().request_repaint_after(TILE_RETRY_AFTER);
        }
        store.finish(key, ok, now);
    }

    if store.textures.len() > 300 {
        store.textures.retain(|k, _| k.provider == provider && (k.z as i32 - int_zoom as i32).abs() <= 1);
    }

    let cache_root = lightcraft_engine::map::tile_cache_dir();

    for x in min_x..=max_x {
        for y in min_y..=max_y {
            let left = center_pos.x + ((x as f64 - center_int_tx) * tile_screen_size as f64) as f32;
            let top = center_pos.y + ((y as f64 - center_int_ty) * tile_screen_size as f64) as f32;
            let tile_rect = Rect::from_min_size(pos2(left, top), vec2(tile_screen_size, tile_screen_size));

            let coord = TileCoord::new(int_zoom, x as u32, y as u32);
            let urls = provider.tile_urls(coord);
            let mut all_loaded = true;

            for (layer_idx, url) in urls.into_iter().enumerate() {
                let key = TileKey { provider, z: int_zoom, x: x as u32, y: y as u32, layer: layer_idx as u8 };

                if let Some(tex) = store.textures.get(&key) {
                    p.image(tex.id(), tile_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                } else {
                    all_loaded = false;
                    if store.wants(&key, now) {
                        store.pending.insert(key);
                        let disk_path = lightcraft_engine::map::tile_cache_path(&cache_root, provider, coord, layer_idx);
                        let batch = join_tile_batch(&mut store.batch, activity);
                        fetch_and_cache_tile(TileJob {
                            key,
                            url,
                            disk_path,
                            tx: store.tx.clone(),
                            ctx: p.ctx().clone(),
                            retry: store.failed.contains_key(&key),
                            _counted: TileLoadEnd(batch),
                        });
                    } else if let Some(failed_at) = store.failed.get(&key).filter(|_| !store.pending.contains(&key)) {
                        // keep the retry wake-up alive while cooling down; a retry already in flight needs no wake-up
                        p.ctx().request_repaint_after(TILE_RETRY_AFTER.saturating_sub(now.saturating_duration_since(*failed_at)));
                    }
                }
            }

            if !all_loaded {
                p.rect_stroke(tile_rect, 0.0, grid_stroke, StrokeKind::Inside);
                p.text(
                    pos2(tile_rect.left() + 8.0, tile_rect.top() + 8.0),
                    Align2::LEFT_TOP,
                    format!("{int_zoom}/{x}/{y}"),
                    FontId::proportional(11.0),
                    text_color,
                );
            }
        }
    }
}

fn collect_photos<F>(app: &mut LightcraftApp, filter: LocationFilter, screen_to_lat_lon: F, canvas: Rect) -> Vec<(PhotoId, LatLon, bool)>
where
    F: Fn(Pos2) -> LatLon,
{
    let mut out = Vec::new();
    let bounds = GeoBounds::new(
        screen_to_lat_lon(pos2(canvas.left(), canvas.bottom())).lat,
        screen_to_lat_lon(pos2(canvas.left(), canvas.top())).lat,
        screen_to_lat_lon(pos2(canvas.left(), canvas.top())).lon,
        screen_to_lat_lon(pos2(canvas.right(), canvas.top())).lon,
    );

    let visible_ids = app.session.visible().to_vec();
    for id in visible_ids {
        let Some(ph) = app.session.catalog.photo(id) else { continue };
        let is_selected = app.session.selection.contains(id);
        if let Some((lat, lon)) = ph.meta.gps {
            if let Some(coord) = LatLon::new(lat, lon) {
                match filter {
                    LocationFilter::Untagged => {}
                    LocationFilter::Tagged | LocationFilter::None => {
                        out.push((id, coord, is_selected));
                    }
                    LocationFilter::VisibleOnMap => {
                        if bounds.contains(coord) {
                            out.push((id, coord, is_selected));
                        }
                    }
                }
            }
        } else if filter == LocationFilter::Untagged || filter == LocationFilter::None {
            // Untagged photo
        }
    }
    out
}

fn cluster_photos<F>(photos: &[(PhotoId, LatLon, bool)], lat_lon_to_screen: F, canvas: Rect) -> Vec<MapCluster>
where
    F: Fn(LatLon) -> Pos2,
{
    let mut clusters: Vec<MapCluster> = Vec::new();

    for &(id, coord, is_selected) in photos {
        let pos = lat_lon_to_screen(coord);
        if !canvas.expand(50.0).contains(pos) {
            continue;
        }

        let mut merged = false;
        for c in &mut clusters {
            if c.center.distance(pos) <= CLUSTER_RADIUS_PTS {
                c.photos.push(id);
                if is_selected {
                    c.has_selected = true;
                }
                merged = true;
                break;
            }
        }

        if !merged {
            clusters.push(MapCluster { center: pos, lat_lon: coord, photos: vec![id], has_selected: is_selected });
        }
    }
    clusters
}

fn render_single_pin(p: &Painter, pos: Pos2, selected: bool, hovered: bool) {
    let pin_color = if selected {
        Color32::from_rgb(255, 204, 0) // Yellow for selected
    } else {
        Color32::from_rgb(255, 115, 0) // Orange for unselected
    };
    let border_color = if hovered { Color32::WHITE } else { Color32::from_black_alpha(180) };

    // Pin head circle
    let head_center = pos2(pos.x, pos.y - 12.0);
    p.circle_filled(head_center, 7.0, pin_color);
    p.circle_stroke(head_center, 7.0, Stroke::new(1.5, border_color));

    // Center dot
    p.circle_filled(head_center, 2.5, Color32::WHITE);

    // Pin point
    p.line_segment([pos2(pos.x, pos.y - 5.0), pos], Stroke::new(2.0, pin_color));
}

fn render_cluster_badge(p: &Painter, pos: Pos2, count: usize, selected: bool, hovered: bool) {
    let badge_color = if selected { Color32::from_rgb(255, 204, 0) } else { Color32::from_rgb(240, 100, 20) };
    let text_color = if selected { Color32::BLACK } else { Color32::WHITE };
    let border_stroke = Stroke::new(if hovered { 2.0 } else { 1.2 }, if hovered { Color32::WHITE } else { Color32::from_black_alpha(160) });

    let label = count.to_string();
    let badge_w = (label.len() as f32 * 8.0 + 16.0).max(28.0);
    let badge_rect = Rect::from_center_size(pos, vec2(badge_w, 22.0));

    p.rect_filled(badge_rect, 11.0, badge_color);
    p.rect_stroke(badge_rect, 11.0, border_stroke, StrokeKind::Inside);
    p.text(pos, Align2::CENTER_CENTER, label, FontId::proportional(12.0), text_color);
}

fn render_top_filter_bar(_app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect, map_ui: &mut crate::state::MapUi, center: LatLon) {
    let t = Tokens::get(ui.ctx());
    let bar_rect = Rect::from_min_size(pos2(canvas.left() + 16.0, canvas.top() + 14.0), vec2(canvas.width() - 32.0, 36.0));

    let bar_bg = Color32::from_rgba_unmultiplied(20, 24, 28, 220);
    ui.painter().rect_filled(bar_rect, 6.0, bar_bg);
    ui.painter().rect_stroke(bar_rect, 6.0, Stroke::new(1.0, t.button_border), StrokeKind::Inside);

    let mut bar_ui = ui.new_child(egui::UiBuilder::new().max_rect(bar_rect));
    bar_ui.horizontal_centered(|ui| {
        ui.add_space(8.0);
        ui.label(egui::RichText::new(crate::i18n::tr("Location Filter:")).strong().color(Color32::WHITE));
        ui.add_space(8.0);

        let filters = [
            (LocationFilter::VisibleOnMap, "Visible On Map"),
            (LocationFilter::Tagged, "Tagged"),
            (LocationFilter::Untagged, "Untagged"),
            (LocationFilter::None, "None"),
        ];

        for (f, name) in filters {
            let active = map_ui.filter == f;
            let btn = ui.selectable_label(active, crate::i18n::tr(name));
            if btn.clicked() {
                map_ui.filter = f;
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(10.0);
            ui.label(egui::RichText::new(format!("{:.4}°, {:.4}°", center.lat, center.lon)).small().color(Color32::LIGHT_GRAY));
        });
    });
}

fn render_bottom_toolbar(_app: &mut LightcraftApp, ui: &mut egui::Ui, canvas: Rect, map_ui: &mut crate::state::MapUi, zoom: &mut f64) {
    let t = Tokens::get(ui.ctx());
    let bar_h = 36.0;
    let bar_rect = Rect::from_min_max(pos2(canvas.left(), canvas.bottom() - bar_h), canvas.max);

    ui.painter().rect_filled(bar_rect, 0.0, Color32::from_rgba_unmultiplied(16, 20, 24, 235));
    ui.painter().line_segment([pos2(bar_rect.left(), bar_rect.top()), pos2(bar_rect.right(), bar_rect.top())], Stroke::new(1.0, t.button_border));

    let mut bar_ui = ui.new_child(egui::UiBuilder::new().max_rect(bar_rect));
    bar_ui.horizontal_centered(|ui| {
        ui.add_space(12.0);
        ui.label(egui::RichText::new(crate::i18n::tr("Map Style:")).small().color(Color32::LIGHT_GRAY));

        crate::widgets::combo_box("map_style_combo").selected_text(map_ui.provider.label()).show_ui(ui, |ui| {
            for p in MapProvider::ALL {
                if ui.selectable_label(map_ui.provider == p, p.label()).clicked() {
                    map_ui.provider = p;
                }
            }
        });

        ui.add_space(16.0);
        if ui.button("-").clicked() {
            *zoom = (*zoom - 1.0).max(1.0);
        }
        ui.add(egui::Slider::new(zoom, 1.0..=map_ui.provider.max_zoom() as f64).show_value(false));
        if ui.button("+").clicked() {
            *zoom = (*zoom + 1.0).min(map_ui.provider.max_zoom() as f64);
        }

        ui.add_space(12.0);
        let lock_btn = ui.selectable_label(map_ui.locked, crate::i18n::tr("Lock Markers"));
        if lock_btn.clicked() {
            map_ui.locked = !map_ui.locked;
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(12.0);
            ui.label(egui::RichText::new(map_ui.provider.attribution()).small().color(Color32::from_white_alpha(150)));
        });
    });
}

fn render_legend_overlay(ui: &mut egui::Ui, canvas: Rect, _map_ui: &mut crate::state::MapUi) {
    let legend_rect = Rect::from_min_size(pos2(canvas.right() - 210.0, canvas.bottom() - 170.0), vec2(196.0, 124.0));
    let p = ui.painter();

    p.rect_filled(legend_rect, 6.0, Color32::from_rgba_unmultiplied(20, 24, 28, 230));
    p.rect_stroke(legend_rect, 6.0, Stroke::new(1.0, Color32::from_white_alpha(40)), StrokeKind::Inside);

    let items = [
        (Color32::from_rgb(255, 115, 0), "Unselected photo"),
        (Color32::from_rgb(255, 204, 0), "Selected photo"),
        (Color32::from_rgb(240, 100, 20), "Group / cluster"),
    ];

    for (i, (color, text)) in items.iter().enumerate() {
        let y = legend_rect.top() + 20.0 + i as f32 * 28.0;
        p.circle_filled(pos2(legend_rect.left() + 20.0, y), 5.0, *color);
        p.text(pos2(legend_rect.left() + 34.0, y), Align2::LEFT_CENTER, crate::i18n::tr(text), FontId::proportional(11.0), Color32::WHITE);
    }
}

fn render_loupe_popover(app: &mut LightcraftApp, ui: &mut egui::Ui, pin_pos: Pos2, photo_id: PhotoId, map_ui: &mut crate::state::MapUi) {
    let card_w = 230.0;
    let card_h = 100.0;
    let card_rect = Rect::from_min_size(pos2(pin_pos.x - card_w / 2.0, pin_pos.y - card_h - 18.0), vec2(card_w, card_h));

    let p = ui.painter();
    p.rect_filled(card_rect, 6.0, Color32::from_rgba_unmultiplied(24, 28, 34, 245));
    p.rect_stroke(card_rect, 6.0, Stroke::new(1.2, Color32::from_white_alpha(70)), StrokeKind::Inside);

    let Some(ph) = app.session.catalog.photo(photo_id) else { return };
    let mut card_ui = ui.new_child(egui::UiBuilder::new().max_rect(card_rect));

    card_ui.vertical(|ui| {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(&ph.file_name).strong().color(Color32::WHITE));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                if ui.button("X").clicked() {
                    map_ui.active_pin_photo = None;
                }
            });
        });

        ui.add_space(4.0);
        if let Some(ref cap) = ph.captured {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(cap).small().color(Color32::LIGHT_GRAY));
            });
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if ui.button(crate::i18n::tr("Open in Detail")).clicked() {
                app.ui.view = ViewMode::Detail;
            }
        });
    });
}

fn zoom_from_radius(radius_meters: f64) -> f64 {
    if radius_meters <= 300.0 {
        16.5
    } else if radius_meters <= 800.0 {
        15.5
    } else if radius_meters <= 2000.0 {
        14.0
    } else if radius_meters <= 5000.0 {
        12.5
    } else if radius_meters <= 15000.0 {
        11.0
    } else if radius_meters <= 50000.0 {
        9.5
    } else {
        8.0
    }
}

fn render_left_navigator(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    rects: (Rect, Rect),
    nav_dims: (f32, f32),
    center: &mut LatLon,
    zoom: &mut f64,
    map_ui: &mut crate::state::MapUi,
) {
    let (rect, canvas_rect) = rects;
    let (left_w, nav_h) = nav_dims;
    let t = Tokens::get(ui.ctx());
    let left_rect = Rect::from_min_size(rect.min, vec2(left_w, rect.height()));

    ui.painter().rect_filled(left_rect, 0.0, Color32::from_rgb(26, 30, 36));
    ui.painter()
        .line_segment([pos2(left_rect.right(), left_rect.top()), pos2(left_rect.right(), left_rect.bottom())], Stroke::new(1.0, t.button_border));

    let mut panel_ui = ui.new_child(egui::UiBuilder::new().max_rect(left_rect));
    panel_ui.vertical(|ui| {
        // Navigator mini-map header
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(crate::i18n::tr("Navigator")).strong().color(Color32::WHITE));
        });

        ui.add_space(4.0);
        let mini_w = left_w - 16.0;
        let mini_rect = Rect::from_min_size(pos2(left_rect.left() + 8.0, ui.cursor().top()), vec2(mini_w, nav_h));
        let nav_resp = ui.allocate_rect(mini_rect, Sense::click_and_drag());

        let nav_zoom = map_ui.navigator_zoom.clamp(1.0, (map_ui.provider.max_zoom() - 2) as f64);

        if nav_resp.dragged() {
            let delta = nav_resp.drag_delta();
            let delta_tx = delta.x as f64 / TILE_SIZE as f64;
            let delta_ty = delta.y as f64 / TILE_SIZE as f64;
            let n = 2.0_f64.powf(nav_zoom.clamp(0.0, 24.0));
            let (cur_tx, cur_ty) = center.to_tile_coords(nav_zoom);
            let new_tx = (cur_tx + delta_tx).rem_euclid(n);
            let new_ty = (cur_ty + delta_ty).clamp(0.0, n);
            *center = LatLon::from_tile_coords(new_tx, new_ty, nav_zoom);
            ui.ctx().request_repaint();
        } else if nav_resp.clicked()
            && let Some(pos) = nav_resp.interact_pointer_pos()
        {
            let offset_x = pos.x - mini_rect.center().x;
            let offset_y = pos.y - mini_rect.center().y;
            let delta_tx = offset_x as f64 / TILE_SIZE as f64;
            let delta_ty = offset_y as f64 / TILE_SIZE as f64;
            let n = 2.0_f64.powf(nav_zoom.clamp(0.0, 24.0));
            let (cur_tx, cur_ty) = center.to_tile_coords(nav_zoom);
            let new_tx = (cur_tx + delta_tx).rem_euclid(n);
            let new_ty = (cur_ty + delta_ty).clamp(0.0, n);
            *center = LatLon::from_tile_coords(new_tx, new_ty, nav_zoom);
            ui.ctx().request_repaint();
        }

        if nav_resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.5 {
                let max_z = (map_ui.provider.max_zoom() - 2) as f64;
                map_ui.navigator_zoom = (map_ui.navigator_zoom + (scroll as f64 * 0.005)).clamp(1.0, max_z);
                ui.ctx().request_repaint();
            }
        }

        let nav_painter = ui.painter().with_clip_rect(mini_rect);
        nav_painter.rect_filled(mini_rect, 4.0, Color32::from_rgb(18, 22, 26));

        // Render overview map tiles
        render_tile_grid(&nav_painter, &app.session.activity, mini_rect, mini_rect.center(), *center, nav_zoom, map_ui.provider);

        // Viewport rectangle showing the visible area on the main map
        let scale_diff = 2.0_f64.powf(nav_zoom - *zoom);
        let box_w = (canvas_rect.width() as f64 * scale_diff) as f32;
        let box_h = (canvas_rect.height() as f64 * scale_diff) as f32;
        let box_w_clamped = box_w.clamp(6.0, mini_rect.width() * 1.5);
        let box_h_clamped = box_h.clamp(6.0, mini_rect.height() * 1.5);
        let view_rect = Rect::from_center_size(mini_rect.center(), vec2(box_w_clamped, box_h_clamped));

        nav_painter.rect_filled(view_rect, 1.0, Color32::from_white_alpha(35));
        nav_painter.rect_stroke(view_rect, 1.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Middle);
        nav_painter.circle_filled(mini_rect.center(), 2.0, Color32::from_rgb(255, 204, 0));

        // Frame
        ui.painter().rect_stroke(mini_rect, 4.0, Stroke::new(1.0, t.button_border), StrokeKind::Inside);

        // Saved Locations header
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(crate::i18n::tr("Saved Locations")).strong().color(Color32::WHITE));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                if ui.button("+").on_hover_text(crate::i18n::tr("Save current map location")).clicked() {
                    let _ = app.run(
                        "map.savedLocations.create",
                        json!({
                            "name": format!("Location {}", app.session.saved_locations.len() + 1),
                            "lat": center.lat,
                            "lon": center.lon,
                            "radius": 1500.0,
                        }),
                    );
                }
            });
        });

        ui.add_space(8.0);
        let mut delete_id = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            if app.session.saved_locations.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new(crate::i18n::tr("No saved locations. Click + to save.")).small().italics().color(Color32::GRAY));
                });
            } else {
                for loc in &app.session.saved_locations {
                    let is_active = (loc.lat - center.lat).abs() < 0.0001 && (loc.lon - center.lon).abs() < 0.0001;
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        let resp = ui.selectable_label(is_active, &loc.name);
                        if resp.clicked()
                            && let Some(target) = LatLon::new(loc.lat, loc.lon)
                        {
                            *center = target;
                            *zoom = zoom_from_radius(loc.radius);
                            ui.ctx().request_repaint();
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(8.0);
                            if ui.small_button("✕").on_hover_text(crate::i18n::tr("Delete location")).clicked() {
                                delete_id = Some(loc.id.clone());
                            }
                        });
                    });
                }
            }
        });
        if let Some(id) = delete_id {
            let _ = app.run("map.savedLocations.delete", json!({"id": id}));
            ui.ctx().request_repaint();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use crate::headless::HeadlessView;

    #[test]
    fn cluster_photos_merges_close_pins() {
        let p1 = (PhotoId(1), LatLon::new(45.5700, -73.2000).unwrap(), false);
        let p2 = (PhotoId(2), LatLon::new(45.5701, -73.2001).unwrap(), true);
        let p3 = (PhotoId(3), LatLon::new(51.5074, -0.1278).unwrap(), false);

        let canvas = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 800.0));
        let lat_lon_to_screen = |coord: LatLon| -> Pos2 {
            if coord.lat > 50.0 {
                pos2(800.0, 600.0) // p3
            } else if coord.lat > 45.57005 {
                pos2(205.0, 205.0) // p2 (distance to p1 = 7.07 < 36.0)
            } else {
                pos2(200.0, 200.0) // p1
            }
        };

        let clusters = cluster_photos(&[p1, p2, p3], lat_lon_to_screen, canvas);
        assert_eq!(clusters.len(), 2);
        assert_eq!(clusters[0].photos.len(), 2);
        assert!(clusters[0].has_selected);
        assert_eq!(clusters[1].photos.len(), 1);
        assert!(!clusters[1].has_selected);
    }

    #[test]
    fn headless_map_view_renders_cleanly() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        app.ui.view = ViewMode::Map;

        let mut view = HeadlessView::new();
        let raw = HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        view.run(raw, |ui| {
            show(&mut app, ui);
        });

        assert_eq!(app.ui.view, ViewMode::Map);
        assert_eq!(app.ui.map.provider, MapProvider::OpenStreetMap);
    }

    #[test]
    fn continuous_zoom_and_pan_preserves_coordinates() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        app.ui.view = ViewMode::Map;
        app.ui.map.center_lat = 45.57;
        app.ui.map.center_lon = -73.2;
        app.ui.map.zoom = 10.35;

        let mut view = HeadlessView::new();
        let mut raw = HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        raw.events.push(egui::Event::PointerMoved(pos2(500.0, 350.0)));
        raw.events.push(egui::Event::PointerButton {
            pos: pos2(500.0, 350.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        raw.events.push(egui::Event::PointerMoved(pos2(550.0, 370.0)));

        view.run(raw, |ui| {
            show(&mut app, ui);
        });

        assert!(app.ui.map.center_lat.is_finite());
        assert!(app.ui.map.center_lon.is_finite());
        assert!((app.ui.map.center_lat - 45.57).abs() < 1.0);
        assert!((app.ui.map.center_lon - (-73.2)).abs() < 1.0);
    }

    #[test]
    fn saved_location_selection_updates_map_center() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        app.ui.view = ViewMode::Map;
        app.ui.map.center_lat = 0.0;
        app.ui.map.center_lon = 0.0;
        app.ui.map.zoom = 5.0;

        let _ = app.run(
            "map.savedLocations.create",
            json!({
                "name": "Beloeil",
                "lat": 45.57,
                "lon": -73.2,
                "radius": 1500.0,
            }),
        );

        let mut view = HeadlessView::new();
        let raw = HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        view.run(raw, |ui| {
            show(&mut app, ui);
        });

        assert_eq!(app.session.saved_locations.len(), 1);
        let loc = &app.session.saved_locations[0];
        app.ui.map.center_lat = loc.lat;
        app.ui.map.center_lon = loc.lon;
        app.ui.map.zoom = zoom_from_radius(loc.radius);

        assert_eq!(app.ui.map.center_lat, 45.57);
        assert_eq!(app.ui.map.center_lon, -73.2);
        assert_eq!(app.ui.map.zoom, 14.0);
    }

    #[test]
    fn navigator_zoom_is_independent_from_map_zoom() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
        app.ui.view = ViewMode::Map;
        app.ui.map.zoom = 14.0;
        app.ui.map.navigator_zoom = 3.0;

        let mut view = HeadlessView::new();
        let raw = HeadlessView::raw_input(vec2(1000.0, 700.0), 1.0, 0.0, vec![]);
        view.run(raw, |ui| {
            show(&mut app, ui);
        });

        assert_eq!(app.ui.map.zoom, 14.0);
        assert_eq!(app.ui.map.navigator_zoom, 3.0);
    }

    #[test]
    fn tile_loads_share_one_activity_row_that_goes_with_the_last_load() {
        let activity = lightcraft_engine::activity::Activity::default();
        let mut slot = Weak::new();
        let first = TileLoadEnd(join_tile_batch(&mut slot, &activity));
        let second = TileLoadEnd(join_tile_batch(&mut slot, &activity));

        let rows = activity.list();
        assert_eq!(rows.len(), 1, "every tile load in flight shares one row");
        assert_eq!((rows[0].kind, rows[0].label.as_str()), ("download", "Loading map tiles"));
        assert_eq!((rows[0].done, rows[0].total), (0, 2));
        assert!(!rows[0].cancellable);

        drop(first);
        assert_eq!((activity.list()[0].done, activity.list()[0].total), (1, 2), "a finished load counts");
        drop(second);
        assert!(activity.list().is_empty(), "the row goes with the last load, drawn or not");

        // a load after that starts a new row
        let third = TileLoadEnd(join_tile_batch(&mut slot, &activity));
        assert_eq!(activity.list().len(), 1);
        drop(third);
        assert!(activity.list().is_empty());
    }

    fn key(x: u32) -> TileKey {
        TileKey { provider: MapProvider::OpenStreetMap, z: 10, x, y: 0, layer: 0 }
    }

    /// Loads a 1x1 tile, or fails for odd `x`, while counting how many loads run at once.
    fn counting_load(job: &TileJob) -> Option<ColorImage> {
        let now = RUNNING.fetch_add(1, Ordering::SeqCst) + 1;
        PEAK.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(20));
        RUNNING.fetch_sub(1, Ordering::SeqCst);
        job.key.x.is_multiple_of(2).then(|| ColorImage::new([1, 1], vec![Color32::WHITE]))
    }
    static RUNNING: AtomicU64 = AtomicU64::new(0);
    static PEAK: AtomicU64 = AtomicU64::new(0);

    /// One connection per visible tile made most of them time out; the pool caps them, and
    /// reports every job, failed ones included, so none stays pending.
    #[test]
    fn tile_pool_caps_concurrent_loads_and_reports_every_job() {
        let activity = lightcraft_engine::activity::Activity::default();
        let mut slot = Weak::new();
        let pool = TilePool::new(counting_load, 3);
        let (tx, rx) = channel();
        for x in 0..30 {
            let job = TileJob {
                key: key(x),
                url: String::new(),
                disk_path: std::path::PathBuf::new(),
                tx: tx.clone(),
                ctx: egui::Context::default(),
                retry: false,
                _counted: TileLoadEnd(join_tile_batch(&mut slot, &activity)),
            };
            pool.submit(job);
        }
        let mut results: Vec<TileResult> = (0..30).map(|_| rx.recv_timeout(Duration::from_secs(10)).unwrap()).collect();
        results.sort_by_key(|(k, _)| k.x);
        assert!(PEAK.load(Ordering::SeqCst) <= 3, "peak {}", PEAK.load(Ordering::SeqCst));
        assert!(results.iter().all(|(k, img)| img.is_some() == k.x.is_multiple_of(2)), "failures come back as None");
        // every job dropped: the row is gone
        let deadline = Instant::now() + Duration::from_secs(5);
        while !activity.list().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(activity.list().is_empty());
    }

    /// Retries used to be queued newest-first like any request, so tiles that kept failing could
    /// be taken again and again while visible tiles never got a first attempt.
    #[test]
    fn retries_wait_behind_tiles_never_tried() {
        let activity = lightcraft_engine::activity::Activity::default();
        let mut slot = Weak::new();
        let (tx, _rx) = channel();
        let mut job = |x: u32, retry: bool| TileJob {
            key: key(x),
            url: String::new(),
            disk_path: std::path::PathBuf::new(),
            tx: tx.clone(),
            ctx: egui::Context::default(),
            retry,
            _counted: TileLoadEnd(join_tile_batch(&mut slot, &activity)),
        };
        let mut q = TileQueue { jobs: Vec::new(), workers: 0 };
        q.push(job(1, false));
        q.push(job(2, true));
        q.push(job(3, false));
        q.push(job(4, true));
        let order: Vec<u32> = std::iter::from_fn(|| q.jobs.pop()).map(|j| j.key.x).collect();
        assert_eq!(order, vec![3, 1, 2, 4], "first attempts newest first, then retries oldest first");
    }

    /// A cached file that no longer decodes is deleted, so the next try downloads it again instead of
    /// failing on the same bytes forever. No network: the URL is unusable on purpose.
    #[test]
    fn an_undecodable_cached_tile_is_replaced_not_retried_forever() {
        let dir = std::env::temp_dir().join(format!("lc-tile-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let disk_path = dir.join("bad.png");
        std::fs::write(&disk_path, b"not a png").unwrap();
        let job = TileJob {
            key: key(1),
            url: String::new(),
            disk_path: disk_path.clone(),
            tx: channel().0,
            ctx: egui::Context::default(),
            retry: true,
            _counted: TileLoadEnd(join_tile_batch(&mut Weak::new(), &lightcraft_engine::activity::Activity::default())),
        };
        assert!(load_tile(&job).is_none());
        assert!(!disk_path.exists(), "the undecodable cache file is removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failed tile used to stay pending for good; now it is asked for again after a pause.
    #[test]
    fn a_failed_tile_is_retried_after_a_pause() {
        let mut store = TileStore::new();
        let t0 = Instant::now();
        let k = key(1);
        assert!(store.wants(&k, t0));
        store.pending.insert(k);
        assert!(!store.wants(&k, t0), "in flight");
        store.finish(k, false, t0);
        assert!(!store.pending.contains(&k));
        assert!(!store.wants(&k, t0 + Duration::from_secs(1)), "not on every frame");
        assert!(store.wants(&k, t0 + TILE_RETRY_AFTER), "retried after the pause");
        store.finish(k, true, t0 + TILE_RETRY_AFTER);
        assert!(store.failed.is_empty());
    }
}
