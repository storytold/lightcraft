//! The heavy part of indexing faces, run off the session thread.
//!
//! For one photo: render it (upright, uncropped, default settings), find its faces with the bundled detector
//! so each region can be aligned by landmarks, cut each region out and run the recognition model. The session
//! thread only prepares the job (which needs the catalog) and later ingests the result, so the window never waits
//! for a model.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use lightcraft_catalog::PhotoId;
use lightcraft_faces::align::{Rgb, align_to_template, crop_box};
use lightcraft_faces::runtime::Embedder;
use lightcraft_faces::yunet::{Face, Options};
use lightcraft_geom::Rect;

use crate::cmd::face_detect::detector;
use crate::media::RenderJob;

/// Long edge of the picture faces are cut from.
pub(crate) const EDGE: usize = 2048;
/// A box from an XMP sidecar is cut with this much room around it (there are no landmarks to align by).
pub(crate) const BOX_PAD: f32 = 1.5;
/// How much a detected face must overlap a region to lend it its landmarks.
pub(crate) const MATCH_IOU: f32 = 0.3;

/// One photo's faces to embed.
pub(crate) struct Prepared {
    /// `<model id>@<sha256>` the embeddings are for.
    pub tag: String,
    pub id: PhotoId,
    pub job: RenderJob,
    pub todo: Vec<Rect>,
}

/// The embeddings made for a [`Prepared`]; `None` for a face that could not be embedded.
pub(crate) struct Done {
    pub tag: String,
    pub id: PhotoId,
    pub results: Vec<(Rect, Option<Vec<f32>>)>,
}

fn overlap(a: &Rect, f: &Face) -> f32 {
    let (w, h) = ((a.x1.min(f64::from(f.x1)) - a.x0.max(f64::from(f.x0))).max(0.0), (a.y1.min(f64::from(f.y1)) - a.y0.max(f64::from(f.y0))).max(0.0));
    let inter = w * h;
    let union = (a.x1 - a.x0) * (a.y1 - a.y0) + f64::from((f.x1 - f.x0) * (f.y1 - f.y0)) - inter;
    if union > 0.0 { (inter / union) as f32 } else { 0.0 }
}

/// Do the work for one photo. Never panics: whatever goes wrong, its faces come back as `None`.
pub(crate) fn process(p: Prepared, embedder: &Embedder) -> Done {
    let Prepared { tag, id, job, todo } = p;
    let fallback = |todo: Vec<Rect>| todo.into_iter().map(|r| (r, None)).collect::<Vec<_>>();
    let rects = todo.clone();
    let run = catch_unwind(AssertUnwindSafe(|| {
        let Ok(rendered) = job.run().rendered else { return fallback(todo) };
        let image = rendered.image;
        let (w, h) = (image.width, image.height);
        let rgb: Vec<u8> = image.data.iter().flat_map(|px| [px[0], px[1], px[2]]).collect();
        // the detector's landmarks, where it finds the same face: faces are aligned by them, not cut by a box
        let found = detector().ok().and_then(|d| d.detect(&rgb, w, h, &Options { score: 0.5, ..Options::default() }).ok()).unwrap_or_default();
        let img = Rgb { data: &rgb, width: w, height: h };
        let (iw, ih) = embedder.input_size();
        todo.into_iter()
            .map(|rect| {
                let best = found.iter().map(|f| (overlap(&rect, f), f)).filter(|(o, _)| *o >= MATCH_IOU).max_by(|a, b| a.0.total_cmp(&b.0));
                let crop = match best {
                    Some((_, f)) => align_to_template(&img, &f.landmarks.map(|(x, y)| (x * w as f32, y * h as f32)), iw, ih),
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
            .collect::<Vec<_>>()
    }));
    Done { tag, id, results: run.unwrap_or_else(|_| fallback(rects)) }
}

/// A thread that runs [`process`] for the jobs it is given, one at a time.
pub(crate) struct Worker {
    jobs: Sender<(Arc<Embedder>, Prepared)>,
    done: Receiver<Done>,
}

impl Worker {
    pub fn start() -> Option<Worker> {
        let (jobs, job_rx) = channel::<(Arc<Embedder>, Prepared)>();
        let (done_tx, done) = channel::<Done>();
        std::thread::Builder::new()
            .name("lightcraft-faces".into())
            .spawn(move || {
                // ends when the session (the only sender) is dropped
                while let Ok((embedder, prepared)) = job_rx.recv() {
                    if done_tx.send(process(prepared, &embedder)).is_err() {
                        break;
                    }
                }
            })
            .ok()?;
        Some(Worker { jobs, done })
    }

    pub fn submit(&self, embedder: Arc<Embedder>, prepared: Prepared) -> bool {
        self.jobs.send((embedder, prepared)).is_ok()
    }

    /// Everything finished since the last call.
    pub fn finished(&self) -> Vec<Done> {
        self.done.try_iter().collect()
    }
}
