//! The demo library: procedurally generated photos with realistic metadata, albums and a few edits.

use std::sync::Arc;

use lightcraft_catalog::{Album, Flag, Meta, Op, Photo, Source};
use lightcraft_develop::DevelopSettings;

use crate::Session;

pub fn load(s: &mut Session) {
    let scenes = lightcraft_scenes::demo_library();
    let mut ids = Vec::new();
    let mut ops = Vec::new();
    for sc in &scenes {
        let id = s.catalog.alloc_photo_id();
        let file = format!("LC{:05}.JPG", 1200 + sc.id * 7);
        let mut p = Photo::new(id, Source::Demo { scene: sc.id }, &file, "JPEG", sc.width, sc.height, "2026-09-28T09:30:00");
        p.captured = Some(sc.meta.captured.clone());
        p.file_size = (sc.width as u64 * sc.height as u64) / 3;
        p.meta = Meta {
            camera: sc.meta.camera.into(),
            lens: sc.meta.lens.into(),
            focal_mm: Some(sc.meta.focal_mm),
            aperture: Some(sc.meta.aperture),
            shutter: sc.meta.shutter.into(),
            iso: Some(sc.meta.iso),
            location: sc.meta.location.into(),
            title: sc.name.clone(),
            keywords: sc.meta.keywords.iter().map(|k| k.to_string()).collect(),
            creator: "LightCraft Demo".into(),
            ..Default::default()
        };
        // one demo photo carries a camera focus point (as a Sony raw would), on the bloom's centre, so
        // View ▸ Focus Point and Zoom to Focus Point can be tried without a camera file
        if sc.name == "Cosmos bloom" {
            p.meta.focus = Some(lightcraft_meta::FocusPoint {
                rect: lightcraft_geom::Rect { x0: 0.42, y0: 0.433, x1: 0.50, y1: 0.487 },
                mode: Some("AF-S".into()),
                area: Some("Flexible Spot".into()),
                tracking: None,
            });
        }
        p.rating = [0, 3, 4, 5, 2, 0, 4, 3][sc.id as usize % 8];
        p.flag = match sc.id % 6 {
            1 => Flag::Pick,
            4 => Flag::Reject,
            _ => Flag::None,
        };
        // A few photos come pre-edited so the grid shows the "edited" badge and variety.
        let mut d = DevelopSettings::default();
        match sc.id {
            3 => {
                d.light.highlights = -60.0;
                d.light.shadows = 35.0;
                d.color.vibrance = 25.0;
                d.vignette.amount = -20.0;
            }
            4 => {
                d.light.exposure = 0.35;
                d.effects.clarity = 20.0;
                d.effects.dehaze = 15.0;
            }
            9 => {
                d.treatment = lightcraft_develop::Treatment::Bw;
                d.light.contrast = 35.0;
            }
            _ => {}
        }
        p.develop = Arc::new(d);
        ids.push(id);
        ops.push(Op::AddPhoto { photo: Box::new(p) });
    }
    let folder = s.catalog.alloc_album_id();
    ops.push(Op::AddAlbum {
        album: Album {
            id: folder,
            name: "Travel 2026".into(),
            parent: None,
            folder: true,
            photos: vec![],
            cover: None,
            smart: None,
            quick: false,
            order: None,
        },
    });
    let by_kw = |kw: &str| -> Vec<_> { scenes.iter().zip(&ids).filter(|(sc, _)| sc.meta.keywords.contains(&kw)).map(|(_, id)| *id).collect() };
    for (name, parent, photos) in [
        ("Mountains", Some(folder), by_kw("mountains")),
        ("Coastlines", Some(folder), [by_kw("ocean"), by_kw("beach")].concat()),
        ("Deserts", Some(folder), by_kw("desert")),
        ("Night Sky", None, by_kw("night")),
        ("Garden", None, by_kw("flower")),
        ("Portfolio", None, ids.iter().copied().step_by(3).collect()),
    ] {
        let id = s.catalog.alloc_album_id();
        let cover = photos.first().copied();
        ops.push(Op::AddAlbum {
            album: Album { id, name: name.into(), parent, folder: false, photos, cover, smart: None, quick: false, order: None },
        });
    }
    for op in ops {
        let _ = s.catalog.apply(op);
    }
    if let Some(first) = s.visible_cloned().first() {
        s.selection = crate::Selection::single(*first);
    }
}

/// Procedural scenes are deterministic but costly to evaluate: every pixel runs the scene's noise
/// functions, so a 24 MP demo original takes minutes of CPU time on a small machine (a 4-core CI
/// VM). Each scene at each size is therefore generated once per process and shared:
///
/// - one generation per scene and size, which concurrent requests wait for (Before and After at
///   1:1 both load the original; sessions side by side, e.g. parallel tests, open the same photo;
///   how it renders: [`generate`]);
/// - the most recently used results are kept, up to [`scene_cache_limit`] bytes. They are the same
///   allocations the sessions' source caches hold, so they cost extra memory only once no session
///   holds them any more.
///
/// Only demo photos use this; real files are decoded by their loader as before.
pub(crate) fn scene_pixels(scene: &lightcraft_scenes::Scene, max_edge: usize) -> Result<Arc<lightcraft_raster::Rgb32f>, String> {
    let key = SceneKey { kind: scene.kind, seed: scene.seed, width: scene.width, height: scene.height, max_edge };
    let slot = {
        let mut c = scene_cache();
        c.tick += 1;
        let tick = c.tick;
        match c.entries.iter_mut().find(|e| e.key == key) {
            Some(e) => {
                e.used = tick;
                e.slot.clone()
            }
            None => {
                let slot = Arc::new(SceneSlot::default());
                c.entries.push(SceneEntry { key, slot: slot.clone(), used: tick });
                slot
            }
        }
    };
    let pixels = slot.get_or_generate(|how| match how {
        Render::Parallel => scene.render_fit(max_edge),
        Render::Serial => scene.render_fit_serial(max_edge),
    });
    trim_scene_cache(&mut scene_cache(), scene_cache_limit());
    pixels
}

/// How a generation renders (see [`generate`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Render {
    /// On rayon threads that do nothing else.
    Parallel,
    /// On the calling thread alone, without rayon.
    Serial,
}

const FAILED: &str = "the demo scene could not be generated";

/// Render a scene for [`SceneSlot::get_or_generate`], so that it never needs a thread that waits
/// for a generation. A rayon worker renders on its own thread alone: rendering in parallel, it
/// would run queued jobs while it waits for rows other workers took, and one of them could ask for
/// this very scene and wait for the generation further down its own stack (the faces scan's pool
/// threads hung that way). That also keeps the work within the caller's pool and its limits (the
/// scan's pace, `LIGHTCRAFT_FACE_THREADS`). Any other caller (the render workers, which are plain
/// threads) renders in parallel on [`render_pool`], whose threads only ever render, and blocks
/// without running other work meanwhile; where that pool can't be started it renders alone too.
fn generate(render: impl FnOnce(Render) -> lightcraft_raster::Rgb32f + Send) -> lightcraft_raster::Rgb32f {
    if rayon::current_thread_index().is_some() {
        return render(Render::Serial);
    }
    match render_pool() {
        Some(pool) => pool.install(|| render(Render::Parallel)),
        None => render(Render::Serial),
    }
}

/// The threads non-rayon callers' generations render on: rayon's default count (`RAYON_NUM_THREADS`,
/// else the available parallelism), like the global pool they used before, without touching that
/// pool (asking it for its size starts it, which panics where its threads can't start); `None`
/// where this pool's threads can't start.
fn render_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: std::sync::OnceLock<Option<rayon::ThreadPool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| rayon::ThreadPoolBuilder::new().thread_name(|i| format!("lc-demo-scene-{i}")).build().ok()).as_ref()
}

/// One scene at one size: generated once, then shared.
#[derive(Default)]
struct SceneSlot {
    state: std::sync::Mutex<SlotState>,
    /// Signalled when a generation ends.
    ended: std::sync::Condvar,
}

#[derive(Default)]
struct SlotState {
    generation: Generation,
    /// Generations started so far; the one running or last ended is number `started`.
    started: u64,
    /// Callers waiting for the generation running (for the tests).
    waiting: usize,
}

#[derive(Default)]
enum Generation {
    #[default]
    Empty,
    Running,
    Ready(Arc<lightcraft_raster::Rgb32f>),
    /// The last generation failed (its render panicked); the next request tries again.
    Failed,
}

impl SlotState {
    /// Start a generation; its number.
    fn start(&mut self) -> u64 {
        self.started += 1;
        self.generation = Generation::Running;
        self.started
    }

    /// What came of generation `n`, once it has ended: its pixels, or an error when it failed
    /// (even if a retry has started or finished since: callers get the outcome they waited for).
    fn outcome(&self, n: u64) -> Option<Result<Arc<lightcraft_raster::Rgb32f>, String>> {
        match &self.generation {
            Generation::Running if self.started == n => None,
            // pixels are kept for good once made, so only generation `n` itself can have made them
            Generation::Ready(p) if self.started == n => Some(Ok(p.clone())),
            _ => Some(Err(FAILED.into())),
        }
    }
}

impl SceneSlot {
    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn ready(&self) -> Option<Arc<lightcraft_raster::Rgb32f>> {
        match &self.lock().generation {
            Generation::Ready(p) => Some(p.clone()),
            _ => None,
        }
    }

    /// The pixels `render` makes ([`generate`]), generated once: the first caller renders, with no
    /// lock held, and later ones wait for its outcome. A render that panics is an error for the
    /// callers of its generation, and the next request tries again.
    fn get_or_generate(&self, render: impl FnOnce(Render) -> lightcraft_raster::Rgb32f + Send) -> Result<Arc<lightcraft_raster::Rgb32f>, String> {
        let mut g = self.lock();
        let n = match &g.generation {
            Generation::Ready(p) => return Ok(p.clone()),
            Generation::Running => {
                let n = g.started;
                g.waiting += 1;
                let outcome = loop {
                    if let Some(o) = g.outcome(n) {
                        break o;
                    }
                    g = self.ended.wait(g).unwrap_or_else(std::sync::PoisonError::into_inner);
                };
                g.waiting -= 1;
                return outcome;
            }
            Generation::Empty | Generation::Failed => g.start(),
        };
        drop(g);
        let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| generate(render)));
        let mut g = self.lock();
        g.generation = match made {
            Ok(pixels) => Generation::Ready(Arc::new(pixels)),
            Err(_) => Generation::Failed,
        };
        let outcome = g.outcome(n).unwrap_or_else(|| Err(FAILED.into()));
        drop(g);
        self.ended.notify_all();
        outcome
    }
}

/// Bytes of generated scenes kept for reuse: half the memory budget (768 MB by default; enough
/// for one 24 MP original and the previews around it).
fn scene_cache_limit() -> usize {
    crate::memory::budget() / 2
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct SceneKey {
    kind: lightcraft_scenes::Kind,
    seed: u32,
    width: u32,
    height: u32,
    max_edge: usize,
}

struct SceneEntry {
    key: SceneKey,
    slot: Arc<SceneSlot>,
    used: u64,
}

#[derive(Default)]
struct SceneCache {
    entries: Vec<SceneEntry>,
    tick: u64,
}

fn scene_cache() -> std::sync::MutexGuard<'static, SceneCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<SceneCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn scene_bytes(slot: &SceneSlot) -> usize {
    slot.ready().map_or(0, |img| img.data.len().saturating_mul(std::mem::size_of::<[f32; 3]>()))
}

/// Drop the least recently used generated scenes until the rest fit in `limit` bytes. Scenes still
/// being generated are kept (their waiters hold the slot anyway).
fn trim_scene_cache(c: &mut SceneCache, limit: usize) {
    loop {
        let total: usize = c.entries.iter().map(|e| scene_bytes(&e.slot)).fold(0, usize::saturating_add);
        if total <= limit {
            return;
        }
        let Some(i) = c.entries.iter().enumerate().filter(|(_, e)| e.slot.ready().is_some()).min_by_key(|(_, e)| e.used).map(|(i, _)| i) else {
            return;
        };
        c.entries.swap_remove(i);
    }
}

#[cfg(test)]
mod tests;
