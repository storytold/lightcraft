//! The heavy part of scanning faces, run off the session thread, on a few threads at once.
//!
//! For one photo: render it (upright, uncropped, default settings), find its faces with the YuNet detector (when installed)
//! so each region can be aligned by landmarks, cut each region out and run the recognition model. A photo that has
//! no face regions at all and has not been searched yet also gets its detections back, with their embeddings, so the
//! scan decodes each photo once. The session thread only prepares the job (which needs the catalog) and later ingests
//! the result, so the window never waits for a model.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use dac_catalog::PhotoId;
use dac_faces::align::{Rgb, align_to_template, crop_box};
use dac_faces::runtime::Embedder;
use dac_faces::yunet::{Detector, Face, Options};
use dac_geom::Rect;

use crate::media::{PreviewLoader, RenderJob};

/// Long edge of the picture faces are cut from.
pub(crate) const EDGE: usize = 2048;
/// A box from an XMP sidecar is cut with this much room around it (there are no landmarks to align by).
pub(crate) const BOX_PAD: f32 = 1.5;
/// How much a detected face must overlap a region to lend it its landmarks.
pub(crate) const MATCH_IOU: f32 = 0.3;
/// A detection must be this sure to become a face region (the bar `faces.detect` uses).
pub(crate) const REGION_SCORE: f32 = 0.6;
/// A raw's embedded preview is used for the scan only when its long edge is at least this (a thumbnail is too small to
/// find faces in; the raw itself is decoded then).
const MIN_PREVIEW_EDGE: usize = 1000;

/// Most photos ever worked on at once.
const MAX_WORKERS: usize = 24;
/// What one photo in progress holds: the file, its decoded picture and the copies the detector and recogniser read.
/// Measured: peak memory grew by about 180 MB for each extra worker.
const WORKER_BYTES: usize = 180 << 20;
/// Photos queued behind the ones running, so a worker that finishes never waits for the next frame to be fed.
pub(crate) const PREFETCH: usize = 2;

/// How hard the background scan may work right now, as the app judges from what the user is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pace {
    /// Start nothing new: the user is dragging, typing or scrolling.
    Pause = 0,
    /// One photo at a time: the user is around, or the window is minimized.
    Light = 1,
    /// Half the machine: the user is idle.
    Normal = 2,
    /// Most of the machine: the user is watching the progress.
    Full = 3,
}

impl Pace {
    fn from_u8(n: u8) -> Pace {
        match n {
            0 => Pace::Pause,
            1 => Pace::Light,
            3 => Pace::Full,
            _ => Pace::Normal,
        }
    }

    /// `pause`, `light`, `normal` or `full`; anything else (or nothing) is `normal`.
    pub fn parse(name: Option<&str>) -> Pace {
        match name {
            Some("pause") => Pace::Pause,
            Some("light") => Pace::Light,
            Some("full") => Pace::Full,
            _ => Pace::Normal,
        }
    }
}

/// How many photos are worked on at once at `pace` on a machine with `cores` threads (as the system reports them, which
/// respects container limits and affinity). `cap` is a limit the user set (`LIGHTCRAFT_FACE_THREADS`). Memory is not
/// decided here: decodes wait at the process-wide memory gate ([`crate::memory::work_gate`]), sized from the RAM.
pub(crate) fn workers_for(cores: usize, pace: Pace, cap: Option<usize>) -> usize {
    let n = match pace {
        Pace::Pause => return 0,
        Pace::Light => 1,
        Pace::Normal => cores / 2,
        Pace::Full => cores * 4 / 5,
    }
    .clamp(1, MAX_WORKERS);
    cap.map_or(n, |c| n.min(c.clamp(1, MAX_WORKERS)))
}

fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

fn cap_from_env() -> Option<usize> {
    std::env::var("LIGHTCRAFT_FACE_THREADS").ok().and_then(|v| v.trim().parse::<usize>().ok())
}

/// The most photos the memory budget can hold in progress at once: half of it (the rest is the window's, the caches',
/// the renders'), a worker at a time. Raising the budget (`LIGHTCRAFT_MEMORY_MB`) raises this.
fn memory_workers() -> usize {
    (crate::memory::budget() / 2 / WORKER_BYTES).max(1)
}

/// Photos to work on at once at `pace` on this machine: by the processor's threads and the pace, and never more than
/// the memory allows. A limit the user sets (`LIGHTCRAFT_FACE_THREADS`) replaces the memory one.
pub(crate) fn target_workers(pace: Pace) -> usize {
    workers_for(cores(), pace, Some(cap_from_env().unwrap_or_else(memory_workers)))
}

/// Threads the worker pool starts with: enough for the fullest pace (the idle ones cost nothing).
pub(crate) fn max_workers() -> usize {
    target_workers(Pace::Full)
}

/// Threads for the scan's own parallel work (decoding a picture, developing it) at `pace` on a machine with `cores` threads:
/// two when light, half the machine when normal, four fifths when watched. A photo's parallel parts are most of its cost,
/// so this, more than the number of photos at once, is how much of the processor the scan takes. Measured on a 32-thread
/// desktop (4 photos at once): 8 threads scanned 2.2 photos a second, 16 threads 5.8, 25 threads 6.4.
fn pool_threads_for(cores: usize, pace: Pace, cap: Option<usize>) -> usize {
    let n = match pace {
        Pace::Pause | Pace::Light => 2,
        Pace::Normal => cores / 2,
        Pace::Full => cores * 4 / 5,
    }
    .clamp(2, 32);
    cap.map_or(n, |c| n.min(c.max(1)))
}

/// One photo's faces to embed (and, if `detect`, to find first).
pub(crate) struct Prepared {
    /// `<model id>@<sha256>` the embeddings are for.
    pub tag: String,
    /// The session's face epoch when this was prepared: another library has been opened since if it differs.
    pub epoch: u64,
    pub id: PhotoId,
    pub job: RenderJob,
    /// A raw's embedded preview, tried before the render job (much faster than decoding the raw).
    pub preview: Option<(String, PreviewLoader)>,
    pub todo: Vec<Rect>,
    /// The photo has no face regions and has not been searched: look for faces too.
    pub detect: bool,
    /// The YuNet detector, when the user has installed it: faces are aligned by its landmarks, and it finds the faces of
    /// photos that have none. Without it a scan only embeds the faces the photos already have, cut by their boxes.
    pub detector: Option<Arc<Detector>>,
}

/// The embeddings made for a [`Prepared`]; `None` for a face that could not be embedded.
pub(crate) struct Done {
    pub tag: String,
    pub epoch: u64,
    pub id: PhotoId,
    /// Each face's region, its embedding (`None` when it could not be made) and the box to show it by.
    pub results: Vec<(Rect, Option<Vec<f32>>, Rect)>,
    /// Faces were to be looked for in this photo.
    pub detect_wanted: bool,
    /// What the detector found (their embeddings are in `results`); `None` when the photo could not be searched.
    pub found: Option<Vec<Rect>>,
}

fn overlap(a: &Rect, f: &Face) -> f32 {
    let (w, h) = ((a.x1.min(f64::from(f.x1)) - a.x0.max(f64::from(f.x0))).max(0.0), (a.y1.min(f64::from(f.y1)) - a.y0.max(f64::from(f.y0))).max(0.0));
    let inter = w * h;
    let union = (a.x1 - a.x0) * (a.y1 - a.y0) + f64::from((f.x1 - f.x0) * (f.y1 - f.y0)) - inter;
    if union > 0.0 { (inter / union) as f32 } else { 0.0 }
}

/// A detection as a region box: inside the photo and not a sliver; `None` for anything else (it is only ever data).
fn face_rect(f: &Face) -> Option<Rect> {
    if ![f.x0, f.y0, f.x1, f.y1].iter().all(|v| v.is_finite()) {
        return None;
    }
    let c = |v: f32| f64::from(v.clamp(0.0, 1.0));
    let r = Rect { x0: c(f.x0), y0: c(f.y0), x1: c(f.x1), y1: c(f.y1) };
    (r.x1 - r.x0 > 0.005 && r.y1 - r.y0 > 0.005).then_some(r)
}

/// Do the work for one photo. Never panics: whatever goes wrong, its faces come back as `None`.
pub(crate) fn process(p: Prepared, embedder: &Embedder) -> Done {
    let Prepared { tag, epoch, id, job, preview, todo, detect, detector } = p;
    let fallback = |todo: Vec<Rect>| todo.into_iter().map(|r| (r, None, r)).collect::<Vec<_>>();
    let rects = todo.clone();
    let run = catch_unwind(AssertUnwindSafe(|| {
        let from_preview = preview.and_then(|(path, load)| load(&path, EDGE)).filter(|img| img.width.max(img.height) >= MIN_PREVIEW_EDGE);
        let image = match from_preview {
            Some(img) => img,
            None => match job.run().rendered {
                Ok(rendered) => rendered.image,
                Err(_) => return (fallback(todo), None),
            },
        };
        let (w, h) = (image.width, image.height);
        let rgb: Vec<u8> = image.data.iter().flat_map(|px| [px[0], px[1], px[2]]).collect();
        // the detector's landmarks, where it finds the same face: faces are aligned by them, not cut by a box
        let detected = detector.as_deref().and_then(|d| d.detect(&rgb, w, h, &Options { score: 0.5, ..Options::default() }).ok());
        let found = detected.clone().unwrap_or_default();
        let img = Rgb { data: &rgb, width: w, height: h };
        let (iw, ih) = embedder.input_size();
        let scaled = |f: &Face| f.landmarks.map(|(x, y)| (x * w as f32, y * h as f32));
        let mut results: Vec<(Rect, Option<Vec<f32>>, Rect)> = todo
            .into_iter()
            .map(|rect| {
                let best = found.iter().map(|f| (overlap(&rect, f), f)).filter(|(o, _)| *o >= MATCH_IOU).max_by(|a, b| a.0.total_cmp(&b.0));
                let crop = match best {
                    Some((_, f)) => align_to_template(&img, &scaled(f), iw, ih),
                    None => crop_box(
                        &img,
                        rect.x0 as f32 * w as f32,
                        rect.y0 as f32 * h as f32,
                        rect.x1 as f32 * w as f32,
                        rect.y1 as f32 * h as f32,
                        BOX_PAD,
                        iw,
                        ih,
                    ),
                };
                let v = crop.and_then(|c| embedder.embed(&c).ok());
                // shown by the detector's box where it found this face: every face then has the same tightness
                let view = best.and_then(|(_, f)| face_rect(f)).unwrap_or(rect);
                (rect, v, view)
            })
            .collect();
        // a photo with no face regions: what the detector is sure of becomes its faces, embedded in the same pass
        let new_faces = if detect {
            detected.map(|faces| {
                let mut rects = Vec::new();
                for f in faces.iter().filter(|f| f.score >= REGION_SCORE) {
                    let Some(rect) = face_rect(f) else { continue };
                    let v = align_to_template(&img, &scaled(f), iw, ih).and_then(|c| embedder.embed(&c).ok());
                    results.push((rect, v, rect));
                    rects.push(rect);
                }
                rects
            })
        } else {
            None
        };
        (results, new_faces)
    }));
    let (results, found) = run.unwrap_or_else(|_| (fallback(rects), None));
    Done { tag, epoch, id, results, detect_wanted: detect, found }
}

type Job = (Arc<Embedder>, Prepared);

/// How many photos may be worked on at once, changed at any moment by the pace: a thread that has a job waits here until
/// fewer than that are running. A pace of zero holds every job where it is, with nothing decoded.
struct Limit {
    allowed: AtomicUsize,
    /// The pace the limit was last set for (a [`Pace`] as a number): the pool a photo's parallel work uses.
    pace: AtomicU8,
    running: Mutex<usize>,
    wake: Condvar,
}

impl Limit {
    fn new() -> Limit {
        Limit { allowed: AtomicUsize::new(0), pace: AtomicU8::new(Pace::Normal as u8), running: Mutex::new(0), wake: Condvar::new() }
    }

    fn set(&self, n: usize, pace: Pace) {
        self.pace.store(pace as u8, Ordering::Relaxed);
        self.allowed.store(n, Ordering::Relaxed);
        self.wake.notify_all();
    }

    fn pace(&self) -> Pace {
        Pace::from_u8(self.pace.load(Ordering::Relaxed))
    }

    /// Wait for a place; the limit is looked at again whenever it changes and at least every 100 ms.
    fn enter(&self) {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        while *running >= self.allowed.load(Ordering::Relaxed) {
            running = self.wake.wait_timeout(running, Duration::from_millis(100)).unwrap_or_else(PoisonError::into_inner).0;
        }
        *running += 1;
    }

    fn leave(&self) {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        *running = running.saturating_sub(1);
        drop(running);
        self.wake.notify_one();
    }
}

/// The threads a photo's parallel work (decoding, developing a picture) runs on at `pace`: one pool for each of three sizes
/// ([`pool_threads_for`]), made when first needed, whose idle threads cost nothing. rayon's global pool serves the
/// interactive work (the loupe, exports) and has no priorities, so scan work queued there would make a slider drag wait
/// behind it; here a light scan can use only two threads however large the machine is.
fn scan_pool(pace: Pace) -> Option<&'static rayon::ThreadPool> {
    static LIGHT: std::sync::OnceLock<Option<rayon::ThreadPool>> = std::sync::OnceLock::new();
    static NORMAL: std::sync::OnceLock<Option<rayon::ThreadPool>> = std::sync::OnceLock::new();
    static FULL: std::sync::OnceLock<Option<rayon::ThreadPool>> = std::sync::OnceLock::new();
    let (cell, pace) = match pace {
        Pace::Pause | Pace::Light => (&LIGHT, Pace::Light),
        Pace::Normal => (&NORMAL, Pace::Normal),
        Pace::Full => (&FULL, Pace::Full),
    };
    cell.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(pool_threads_for(cores(), pace, cap_from_env()))
            .thread_name(move |i| format!("dac-faces-{}-{i}", pace as u8))
            .build()
            .ok()
    })
    .as_ref()
}

/// Run one photo on the pool for the pace, so its parallel parts stay there.
///
/// It must NOT wait at the memory gate ([`crate::memory::work_gate`], which background decodes do): a job running on a
/// pool thread that waits inside a nested rayon call can pick up another job to run on top of itself, and if that one waits
/// for memory the first holds, neither can finish (seen as a few photos that never come back). The memory a scan may use is
/// instead limited by how many photos run at once ([`memory_workers`]); its decodes count as interactive ones at the gate,
/// which never wait.
fn process_in_background(prepared: Prepared, embedder: &Embedder, pace: Pace) -> Done {
    let work = || process(prepared, embedder);
    match scan_pool(pace) {
        Some(pool) => pool.install(work),
        None => work(),
    }
}

/// Threads that run [`process`] for the jobs they are given, each on one photo at a time.
pub(crate) struct Worker {
    jobs: Sender<Job>,
    done: Receiver<Done>,
    limit: Arc<Limit>,
}

impl Worker {
    pub fn start(threads: usize) -> Option<Worker> {
        let (jobs, job_rx) = channel::<Job>();
        let (done_tx, done) = channel::<Done>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let limit = Arc::new(Limit::new());
        let mut started = 0;
        for i in 0..threads.clamp(1, MAX_WORKERS) {
            let (rx, tx, limit) = (job_rx.clone(), done_tx.clone(), limit.clone());
            let spawned = std::thread::Builder::new().name(format!("dac-faces-{i}")).spawn(move || {
                loop {
                    // the lock is held only while waiting for a job, never while working on one; the threads end when
                    // the session (the only sender) is dropped
                    let next = rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
                    let Ok((embedder, prepared)) = next else { break };
                    limit.enter();
                    let done = process_in_background(prepared, &embedder, limit.pace());
                    limit.leave();
                    if tx.send(done).is_err() {
                        break;
                    }
                }
            });
            if spawned.is_ok() {
                started += 1;
            }
        }
        (started > 0).then_some(Worker { jobs, done, limit })
    }

    /// How many photos may be worked on at once from now on (what is running is not interrupted).
    pub fn allow(&self, n: usize, pace: Pace) {
        self.limit.set(n, pace);
    }

    pub fn submit(&self, embedder: Arc<Embedder>, prepared: Prepared) -> bool {
        self.jobs.send((embedder, prepared)).is_ok()
    }

    /// Everything finished since the last call.
    pub fn finished(&self) -> Vec<Done> {
        self.done.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pace_sets_how_much_of_the_machine_the_scan_may_use() {
        use Pace::*;
        // a 32-thread desktop: nothing while the user works, one photo around them, half when idle, most when watched
        assert_eq!([Pause, Light, Normal, Full].map(|p| workers_for(32, p, None)), [0, 1, 16, 24]);
        assert_eq!([Pause, Light, Normal, Full].map(|p| workers_for(8, p, None)), [0, 1, 4, 6]);
        assert_eq!([Pause, Light, Normal, Full].map(|p| workers_for(4, p, None)), [0, 1, 2, 3]);
        // small machines still get one worker (except while paused)
        assert_eq!([Pause, Light, Normal, Full].map(|p| workers_for(1, p, None)), [0, 1, 1, 1]);
        assert_eq!(workers_for(0, Full, None), 1);
        assert_eq!(workers_for(10_000, Full, None), MAX_WORKERS);
        // a limit set by the user caps every pace, and is itself kept sane
        assert_eq!([Light, Normal, Full].map(|p| workers_for(32, p, Some(3))), [1, 3, 3]);
        assert_eq!((workers_for(32, Full, Some(0)), workers_for(32, Pause, Some(5))), (1, 0));
        assert_eq!(workers_for(32, Full, Some(usize::MAX)), 24);
    }

    #[test]
    fn the_limit_holds_threads_to_the_pace_and_can_change_at_any_moment() {
        let limit = Arc::new(Limit::new());
        let (now, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let started = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..6)
            .map(|_| {
                let (limit, now, peak, started) = (limit.clone(), now.clone(), peak.clone(), started.clone());
                std::thread::spawn(move || {
                    limit.enter();
                    started.fetch_add(1, Ordering::SeqCst);
                    let n = now.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(n, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(40));
                    now.fetch_sub(1, Ordering::SeqCst);
                    limit.leave();
                })
            })
            .collect();
        // paused: nothing starts
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(started.load(Ordering::SeqCst), 0);
        // two at a time, never more
        limit.set(2, Pace::Normal);
        std::thread::sleep(Duration::from_millis(100));
        assert!(started.load(Ordering::SeqCst) >= 2);
        // raised, the rest go; every thread finishes
        limit.set(6, Pace::Full);
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(started.load(Ordering::SeqCst), 6);
        assert!(peak.load(Ordering::SeqCst) >= 2, "it did run in parallel");
        assert_eq!(*limit.running.lock().unwrap(), 0, "every place was given back");
    }

    #[test]
    fn memory_limits_the_workers_and_a_users_limit_replaces_it() {
        // the default budget (a quarter of the RAM, at most 1.5 GiB) holds a few photos in progress, not dozens
        assert!((1..=8).contains(&memory_workers()), "{}", memory_workers());
        assert!(target_workers(Pace::Full) >= 1);
        assert_eq!(target_workers(Pace::Pause), 0);
    }

    #[test]
    fn pace_names_are_forgiving() {
        assert_eq!(["pause", "light", "normal", "full"].map(|n| Pace::parse(Some(n))), [Pace::Pause, Pace::Light, Pace::Normal, Pace::Full]);
        assert_eq!(
            (Pace::parse(None), Pace::parse(Some("")), Pace::parse(Some("FULL")), Pace::parse(Some("turbo"))),
            (Pace::Normal, Pace::Normal, Pace::Normal, Pace::Normal)
        );
    }

    #[test]
    fn the_scans_parallel_work_gets_a_pool_the_size_of_the_pace() {
        use Pace::*;
        assert_eq!([Pause, Light, Normal, Full].map(|p| pool_threads_for(32, p, None)), [2, 2, 16, 25]);
        assert_eq!([Light, Normal, Full].map(|p| pool_threads_for(8, p, None)), [2, 4, 6]);
        assert_eq!([Light, Normal, Full].map(|p| pool_threads_for(2, p, None)), [2, 2, 2]);
        assert_eq!(pool_threads_for(10_000, Full, None), 32);
        assert_eq!([Light, Normal, Full].map(|p| pool_threads_for(32, p, Some(3))), [2, 3, 3]);
        // the pools exist and are the sizes asked for
        for pace in [Light, Normal, Full] {
            let pool = scan_pool(pace).expect("a pool can be built");
            assert_eq!(pool.install(rayon::current_num_threads), pool_threads_for(cores(), pace, cap_from_env()));
        }
        // a paused scan has the light pool; the pace is remembered as a number and read back
        assert_eq!(scan_pool(Pause).map(|p| p as *const _), scan_pool(Light).map(|p| p as *const _));
        assert_eq!([0, 1, 2, 3, 9].map(Pace::from_u8), [Pause, Light, Normal, Full, Normal]);
    }

    #[test]
    fn detections_become_boxes_only_when_they_make_sense() {
        let f = |x0, y0, x1, y1| Face { x0, y0, x1, y1, score: 0.9, landmarks: [(0.0, 0.0); 5] };
        assert!(face_rect(&f(0.2, 0.2, 0.4, 0.5)).is_some());
        // a box that spills over the edge is brought back inside
        let r = face_rect(&f(-0.1, 0.1, 0.3, 1.2)).unwrap();
        assert_eq!((r.x0, r.y1), (0.0, 1.0));
        // slivers, inverted boxes and non-numbers are dropped, never turned into regions
        for bad in [f(0.2, 0.2, 0.2, 0.5), f(0.4, 0.2, 0.2, 0.5), f(f32::NAN, 0.2, 0.4, 0.5), f(0.2, 0.2, f32::INFINITY, 0.5), f(1.5, 0.2, 2.0, 0.5)]
        {
            assert!(face_rect(&bad).is_none(), "{bad:?}");
        }
    }
}
