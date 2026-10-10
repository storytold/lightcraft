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
mod tests {
    use super::*;

    fn small_scene() -> lightcraft_scenes::Scene {
        lightcraft_scenes::demo_library().swap_remove(0)
    }

    #[test]
    fn a_scene_is_generated_once_per_size_and_shared() {
        let s = small_scene();
        let a = scene_pixels(&s, 48).unwrap();
        let b = scene_pixels(&s, 48).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "the second request reuses the first generation");
        assert_eq!(a.width.max(a.height), 48);
        assert_eq!(a.data, s.render_fit(48).data, "the same pixels as generating it directly");
        let c = scene_pixels(&s, 40).unwrap();
        assert_eq!(c.width.max(c.height), 40, "another size is its own generation");
    }

    #[test]
    fn concurrent_requests_wait_for_one_generation() {
        let s = small_scene();
        let edge = 37; // a size no other test asks for
        let got: Vec<_> = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..4).map(|_| sc.spawn(|| scene_pixels(&s, edge).unwrap())).collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(got.windows(2).all(|w| Arc::ptr_eq(&w[0], &w[1])), "every request got the same pixels");
    }

    /// Run `f` on a thread of its own and wait for it at most `secs`: a deadlock fails the test instead of hanging it.
    fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(std::time::Duration::from_secs(secs)).expect("deadlocked")
    }

    /// Wait (at most ten seconds) until `n` callers wait for `slot`'s generation.
    fn until_waiting(slot: &SceneSlot, n: usize) {
        let started = std::time::Instant::now();
        while slot.lock().waiting < n {
            assert!(started.elapsed() < std::time::Duration::from_secs(10), "the callers never got to wait");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn pixels(n: usize) -> lightcraft_raster::Rgb32f {
        lightcraft_raster::Rgb32f::new(n, 1)
    }

    /// Rayon workers asking for a scene while it is generated never deadlock. The render is parallel, and a rayon worker
    /// waiting for rows another worker took runs queued jobs on top of itself: generating on the caller's thread, the
    /// faces scan's pool threads ended up waiting on the generation further down their own stack and hung. Requests
    /// keep arriving on a four-thread pool while the scene is generated, as when several photos are queued; every one
    /// of them must come back, at least four of them must have overlapped the generation, and the test waits with a
    /// timeout, so a deadlock fails it instead of hanging.
    #[test]
    fn rayon_workers_asking_for_a_scene_being_generated_never_deadlock() {
        let scene = Arc::new(small_scene());
        let edge = 601; // a size no other test asks for
        const ASKED: usize = 24;
        let (overlap, sizes) = within(120, move || {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();
            let issued = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            for _ in 0..ASKED {
                let (scene, done, issued) = (scene.clone(), done_tx.clone(), issued.clone());
                issued.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                pool.spawn(move || {
                    let got = scene_pixels(&scene, edge).unwrap();
                    // how many requests had been made when this one came back
                    let _ = done.send((issued.load(std::sync::atomic::Ordering::SeqCst), got.data.len()));
                });
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let replies: Vec<(usize, usize)> = (0..ASKED).map(|_| done_rx.recv().unwrap()).collect();
            (replies.iter().map(|r| r.0).min().unwrap(), replies.iter().map(|r| r.1).collect::<Vec<_>>())
        });
        assert_eq!(sizes.len(), ASKED, "every request came back");
        assert!(sizes.iter().all(|n| *n == sizes[0] && *n > 0));
        assert!(overlap >= 4, "requests overlapped the generation (only {overlap} had been made when the first came back)");
    }

    /// However many requests (from rayon workers and other threads) arrive while a scene is generated, it is rendered
    /// once, and never twice at the same time: duplicates would each hold a whole image (288 MB for a 24 MP original).
    #[test]
    fn a_scene_is_rendered_once_however_many_ask() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (renders, peak) = within(60, || {
            let slot = Arc::new(SceneSlot::default());
            let (running, peak, renders) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
            let render = {
                let (running, peak, renders) = (running.clone(), peak.clone(), renders.clone());
                move |how: Render| {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    renders.fetch_add(1, Ordering::SeqCst);
                    // work like the real render's: in parallel, the rendering thread waits for rows other threads took
                    use rayon::prelude::*;
                    let sum: u64 = match how {
                        Render::Parallel => (0..2_000_000u64).into_par_iter().map(|i| i % 7).sum(),
                        Render::Serial => (0..2_000_000u64).map(|i| i % 7).sum(),
                    };
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    running.fetch_sub(1, Ordering::SeqCst);
                    pixels(sum as usize % 5 + 1)
                }
            };
            let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            for _ in 0..16 {
                let (slot, render, tx) = (slot.clone(), render.clone(), tx.clone());
                pool.spawn(move || {
                    let _ = tx.send(slot.get_or_generate(render).unwrap());
                });
            }
            for _ in 0..4 {
                let (slot, render, tx) = (slot.clone(), render.clone(), tx.clone());
                std::thread::spawn(move || {
                    let _ = tx.send(slot.get_or_generate(render).unwrap());
                });
            }
            let got: Vec<_> = (0..20).map(|_| rx.recv().unwrap()).collect();
            assert!(got.windows(2).all(|w| Arc::ptr_eq(&w[0], &w[1])), "every request got the same pixels");
            (renders.load(Ordering::SeqCst), peak.load(Ordering::SeqCst))
        });
        assert_eq!((renders, peak), (1, 1));
    }

    /// A rayon worker (the faces scan's) renders a scene on its own thread alone, so the work stays within its pool
    /// and that pool's limits; other threads render in parallel on the scene pool.
    #[test]
    fn a_rayon_worker_renders_alone_on_its_own_thread() {
        let ((how, caller, renderer), (other_how, other_name)) = within(30, || {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap();
            let slot = SceneSlot::default();
            let mut seen = None;
            pool.install(|| {
                let caller = std::thread::current().id();
                slot.get_or_generate(|how| {
                    seen = Some((how, caller, std::thread::current().id()));
                    pixels(1)
                })
                .unwrap();
            });
            let other = SceneSlot::default();
            let mut other_seen = None;
            other
                .get_or_generate(|how| {
                    other_seen = Some((how, std::thread::current().name().unwrap_or_default().to_string()));
                    pixels(1)
                })
                .unwrap();
            (seen.unwrap(), other_seen.unwrap())
        });
        assert_eq!((how, renderer), (Render::Serial, caller));
        assert_eq!(other_how, Render::Parallel);
        assert!(other_name.starts_with("lc-demo-scene-"), "{other_name}");
    }

    /// A caller finding a generation running waits for it instead of rendering its own (one 24 MP original costs
    /// minutes of CPU time on a small machine).
    #[test]
    fn callers_wait_for_the_generation_running() {
        let (a, b, renders) = within(30, || {
            let slot = Arc::new(SceneSlot::default());
            let renders = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (started_tx, started) = std::sync::mpsc::channel::<()>();
            let (go, go_rx) = std::sync::mpsc::channel::<()>();
            let (started_tx, go_rx) = (Arc::new(std::sync::Mutex::new(started_tx)), Arc::new(std::sync::Mutex::new(go_rx)));
            let ask = |slot: &Arc<SceneSlot>| {
                let (slot, renders, started_tx, go_rx) = (slot.clone(), renders.clone(), started_tx.clone(), go_rx.clone());
                std::thread::spawn(move || {
                    slot.get_or_generate(move |_| {
                        renders.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let _ = started_tx.lock().unwrap().send(());
                        let _ = go_rx.lock().unwrap().recv();
                        pixels(1)
                    })
                })
            };
            let first = ask(&slot);
            started.recv().unwrap();
            let second = ask(&slot);
            until_waiting(&slot, 1);
            go.send(()).unwrap();
            let (a, b) = (first.join().unwrap().unwrap(), second.join().unwrap().unwrap());
            (a, b, renders.load(std::sync::atomic::Ordering::SeqCst))
        });
        assert!(Arc::ptr_eq(&a, &b) && a.width == 1, "one generation, shared");
        assert_eq!(renders, 1);
    }

    /// A generation that fails (its render panicked) is an error for its callers, never a hang, and the next request
    /// generates the scene again.
    #[test]
    fn a_failed_generation_is_an_error_and_the_next_request_tries_again() {
        let (first, waiters, again) = within(30, || {
            let slot = Arc::new(SceneSlot::default());
            let (started_tx, started) = std::sync::mpsc::channel::<()>();
            let (go, go_rx) = std::sync::mpsc::channel::<()>();
            let failing = {
                let slot = slot.clone();
                std::thread::spawn(move || {
                    slot.get_or_generate(move |_| {
                        let _ = started_tx.send(());
                        let _ = go_rx.recv();
                        panic!("render failed");
                    })
                })
            };
            started.recv().unwrap();
            let waiters: Vec<_> = (0..2)
                .map(|_| {
                    let slot = slot.clone();
                    std::thread::spawn(move || slot.get_or_generate(|_| pixels(2)))
                })
                .collect();
            until_waiting(&slot, 2);
            drop(go);
            let first = failing.join().unwrap();
            let waiters: Vec<_> = waiters.into_iter().map(|w| w.join().unwrap()).collect();
            (first, waiters, slot.get_or_generate(|_| pixels(3)))
        });
        assert!(first.is_err() && waiters.iter().all(Result::is_err), "the callers of the failed generation get its error");
        assert_eq!(again.unwrap().width, 3);
    }

    /// Callers get the outcome of the generation they waited for, even when a retry starts (or finishes) before they
    /// look: a failed generation followed by a retry is still an error for them.
    #[test]
    fn a_retry_never_changes_the_outcome_waited_for() {
        let mut st = SlotState::default();
        let n = st.start();
        assert!(st.outcome(n).is_none(), "running");
        st.generation = Generation::Failed;
        let m = st.start();
        assert!(matches!(st.outcome(n), Some(Err(_))));
        assert!(st.outcome(m).is_none());
        st.generation = Generation::Ready(Arc::new(pixels(1)));
        assert!(matches!(st.outcome(n), Some(Err(_))), "even once the retry succeeded");
        assert!(matches!(st.outcome(m), Some(Ok(_))));
        // and with threads: a retry racing the waiters of a failed generation never hands them its result
        let wrong = within(60, || {
            let mut wrong = 0;
            for _ in 0..20 {
                let slot = Arc::new(SceneSlot::default());
                let (started_tx, started) = std::sync::mpsc::channel::<()>();
                let (go, go_rx) = std::sync::mpsc::channel::<()>();
                let failing = {
                    let slot = slot.clone();
                    std::thread::spawn(move || {
                        slot.get_or_generate(move |_| {
                            let _ = started_tx.send(());
                            let _ = go_rx.recv();
                            panic!("render failed");
                        })
                    })
                };
                started.recv().unwrap();
                let waiters: Vec<_> = (0..3)
                    .map(|_| {
                        let slot = slot.clone();
                        std::thread::spawn(move || slot.get_or_generate(|_| pixels(2)))
                    })
                    .collect();
                until_waiting(&slot, 3);
                let retry = {
                    let slot = slot.clone();
                    std::thread::spawn(move || {
                        loop {
                            if matches!(slot.lock().generation, Generation::Failed) {
                                return slot.get_or_generate(|_| pixels(4));
                            }
                            std::hint::spin_loop();
                        }
                    })
                };
                drop(go);
                let _ = failing.join().unwrap();
                wrong += waiters.into_iter().map(|w| w.join().unwrap()).filter(Result::is_ok).count();
                assert!(retry.join().unwrap().is_ok());
            }
            wrong
        });
        assert_eq!(wrong, 0, "waiters of a failed generation got a retry's pixels");
    }

    /// Generating a scene never touches the global rayon pool, so a process whose global pool couldn't start (asking it
    /// anything then panics) still gets its demo scenes. Run in a process of its own: the global pool is the process's.
    #[test]
    fn scenes_are_generated_when_the_global_pool_is_broken() {
        use std::io::Read;
        use std::process::Stdio;
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "demo::tests::with_a_broken_global_pool", "--ignored", "--test-threads=1"])
            .env("LC_DEMO_BROKEN_GLOBAL_POOL", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Drain both pipes on threads so a chatty child can't block on a full pipe.
        let drain = |mut r: Box<dyn Read + Send>| {
            std::thread::spawn(move || {
                let mut s = String::new();
                let _ = r.read_to_string(&mut s);
                s
            })
        };
        let (stdout, stderr) = (drain(Box::new(child.stdout.take().unwrap())), drain(Box::new(child.stderr.take().unwrap())));
        // A deadlocked child fails the test instead of hanging CI.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the child process did not finish within 120 s");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let (stdout, stderr) = (stdout.join().unwrap(), stderr.join().unwrap());
        assert!(status.success() && stdout.contains("1 passed"), "{stdout}\n{stderr}");
    }

    #[test]
    #[ignore = "run in a process of its own by scenes_are_generated_when_the_global_pool_is_broken"]
    fn with_a_broken_global_pool() {
        if std::env::var_os("LC_DEMO_BROKEN_GLOBAL_POOL").is_none() {
            return;
        }
        let broken = rayon::ThreadPoolBuilder::new().spawn_handler(|_| Err(std::io::Error::other("no threads"))).build_global();
        assert!(broken.is_err());
        assert!(std::panic::catch_unwind(rayon::current_num_threads).is_err(), "the global pool is broken");
        let s = small_scene();
        let got = scene_pixels(&s, 33).unwrap();
        assert_eq!(got.data, s.render_fit_serial(33).data);
    }

    #[test]
    fn the_cache_keeps_the_most_recent_scenes_within_its_limit() {
        let key = |max_edge| SceneKey { kind: lightcraft_scenes::Kind::Dunes, seed: 1, width: 3, height: 2, max_edge };
        let entry = |max_edge, used, px: Option<usize>| {
            let slot = Arc::new(SceneSlot::default());
            if let Some(n) = px {
                slot.lock().generation = Generation::Ready(Arc::new(lightcraft_raster::Rgb32f::new(n, 1)));
            }
            SceneEntry { key: key(max_edge), slot, used }
        };
        let px = std::mem::size_of::<[f32; 3]>();
        let mut c = SceneCache { entries: vec![entry(1, 3, Some(10)), entry(2, 1, Some(10)), entry(3, 0, None), entry(4, 2, Some(10))], tick: 3 };
        trim_scene_cache(&mut c, 25 * px);
        let mut kept: Vec<usize> = c.entries.iter().map(|e| e.key.max_edge).collect();
        kept.sort_unstable();
        assert_eq!(kept, vec![1, 3, 4], "the least recently used finished scene went; the one in progress stays");
        trim_scene_cache(&mut c, 0);
        assert_eq!(c.entries.iter().map(|e| e.key.max_edge).collect::<Vec<_>>(), vec![3], "only the scene in progress is left");
    }
}
