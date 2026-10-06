//! The heavy part of scanning faces, run off the session thread, on a few threads at once.
//!
//! For one photo: render it (upright, uncropped, default settings), find its faces with the bundled detector
//! so each region can be aligned by landmarks, cut each region out and run the recognition model. A photo that has
//! no face regions at all and has not been searched yet also gets its detections back, with their embeddings, so the
//! scan decodes each photo once. The session thread only prepares the job (which needs the catalog) and later ingests
//! the result, so the window never waits for a model.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};

use lightcraft_catalog::PhotoId;
use lightcraft_faces::align::{Rgb, align_to_template, crop_box};
use lightcraft_faces::runtime::Embedder;
use lightcraft_faces::yunet::{Face, Options};
use lightcraft_geom::Rect;

use crate::cmd::face_detect::detector;
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

/// How many photos are worked on at once: an eighth of the processor's threads, between one and three, so the window and
/// whatever else is running keep their share (each one holds a decoded full-size photo, about 250 MB, while it works).
/// `LIGHTCRAFT_FACE_THREADS` overrides it.
pub(crate) fn scan_threads() -> usize {
    threads_for(std::thread::available_parallelism().map_or(1, |n| n.get()), std::env::var("LIGHTCRAFT_FACE_THREADS").ok().as_deref())
}

fn threads_for(cores: usize, setting: Option<&str>) -> usize {
    match setting.and_then(|v| v.trim().parse::<usize>().ok()) {
        Some(n) => n.clamp(1, 16),
        None => (cores / 8).clamp(1, 3),
    }
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
}

/// The embeddings made for a [`Prepared`]; `None` for a face that could not be embedded.
pub(crate) struct Done {
    pub tag: String,
    pub epoch: u64,
    pub id: PhotoId,
    pub results: Vec<(Rect, Option<Vec<f32>>)>,
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
    let Prepared { tag, epoch, id, job, preview, todo, detect } = p;
    let fallback = |todo: Vec<Rect>| todo.into_iter().map(|r| (r, None)).collect::<Vec<_>>();
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
        let detected = detector().ok().and_then(|d| d.detect(&rgb, w, h, &Options { score: 0.5, ..Options::default() }).ok());
        let found = detected.clone().unwrap_or_default();
        let img = Rgb { data: &rgb, width: w, height: h };
        let (iw, ih) = embedder.input_size();
        let scaled = |f: &Face| f.landmarks.map(|(x, y)| (x * w as f32, y * h as f32));
        let mut results: Vec<(Rect, Option<Vec<f32>>)> = todo
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
                (rect, v)
            })
            .collect();
        // a photo with no face regions: what the detector is sure of becomes its faces, embedded in the same pass
        let new_faces = if detect {
            detected.map(|faces| {
                let mut rects = Vec::new();
                for f in faces.iter().filter(|f| f.score >= REGION_SCORE) {
                    let Some(rect) = face_rect(f) else { continue };
                    let v = align_to_template(&img, &scaled(f), iw, ih).and_then(|c| embedder.embed(&c).ok());
                    results.push((rect, v));
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

/// Threads that run [`process`] for the jobs they are given, each on one photo at a time.
pub(crate) struct Worker {
    jobs: Sender<Job>,
    done: Receiver<Done>,
    threads: usize,
}

impl Worker {
    pub fn start(threads: usize) -> Option<Worker> {
        let (jobs, job_rx) = channel::<Job>();
        let (done_tx, done) = channel::<Done>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let mut started = 0;
        for i in 0..threads.clamp(1, 16) {
            let (rx, tx) = (job_rx.clone(), done_tx.clone());
            let spawned = std::thread::Builder::new().name(format!("lightcraft-faces-{i}")).spawn(move || {
                loop {
                    // the lock is held only while waiting for a job, never while working on one; the threads end when
                    // the session (the only sender) is dropped
                    let next = rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
                    let Ok((embedder, prepared)) = next else { break };
                    if tx.send(process(prepared, &embedder)).is_err() {
                        break;
                    }
                }
            });
            if spawned.is_ok() {
                started += 1;
            }
        }
        (started > 0).then_some(Worker { jobs, done, threads: started })
    }

    /// How many photos can be worked on at once.
    pub fn threads(&self) -> usize {
        self.threads
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
    fn an_eighth_of_the_threads_between_one_and_three_unless_told_otherwise() {
        assert_eq!([1, 2, 4, 8, 12, 16, 32, 128].map(|c| threads_for(c, None)), [1, 1, 1, 1, 1, 2, 3, 3]);
        assert_eq!(threads_for(32, Some("3")), 3);
        assert_eq!(threads_for(32, Some(" 2 ")), 2);
        // an override is kept sane, and rubbish is ignored
        assert_eq!((threads_for(4, Some("0")), threads_for(4, Some("999"))), (1, 16));
        assert_eq!((threads_for(32, Some("many")), threads_for(32, Some("-1")), threads_for(32, Some(""))), (3, 3, 3));
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
