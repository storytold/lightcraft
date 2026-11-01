//! WIDER FACE validation: read the ground truth, run a detector on every photo, score the three settings.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use lightcraft_faces::yunet::{Detector, Options};

use super::mat;
use super::metrics::{self, Image};

/// Lowest detection score kept: the protocol wants nearly every detection, the curve then sweeps the threshold.
const MIN_SCORE: f32 = 0.02;
/// Most detections per photo (the densest WIDER crowds hold well over a thousand faces).
const MAX_FACES: usize = 2000;
pub const SETTINGS: [&str; 3] = ["easy", "medium", "hard"];

/// The validation ground truth: per photo, its path and face boxes `[x, y, w, h]`; per setting, the 1-based faces it counts.
pub struct GroundTruth {
    pub images: Vec<PathBuf>,
    pub boxes: Vec<Vec<[f64; 4]>>,
    pub keep: [Vec<Vec<usize>>; 3],
}

fn read_mat(path: &Path) -> Result<Vec<(String, mat::Value)>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e} (run `cargo xtask ai-bench fetch`)", path.display()))?;
    mat::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// Read the ground truth under `dir`: `eval_tools/ground_truth/*.mat` (the evaluation toolkit's) and the photos in
/// `WIDER_val/images/<event>/<name>.jpg`.
pub fn load(dir: &Path) -> Result<GroundTruth, String> {
    let gt = dir.join("eval_tools").join("ground_truth");
    let main = read_mat(&gt.join("wider_face_val.mat"))?;
    let events = mat::var(&main, "event_list")?.cells().ok_or("event_list is not a cell array")?;
    let files = mat::var(&main, "file_list")?.cells().ok_or("file_list is not a cell array")?;
    let boxes = mat::var(&main, "face_bbx_list")?.cells().ok_or("face_bbx_list is not a cell array")?;
    if files.len() != events.len() || boxes.len() != events.len() {
        return Err(format!("event_list has {} events, file_list {} and face_bbx_list {}", events.len(), files.len(), boxes.len()));
    }
    // per event: its name, photo names and box lists, the photo counts checked before anything is paired up
    let mut per_event = Vec::with_capacity(events.len());
    for (e, ((event, files), boxes)) in events.iter().zip(files).zip(boxes).enumerate() {
        let event = event.text().ok_or_else(|| format!("event {e}: event_list entry is not text"))?;
        let files = files.cells().ok_or_else(|| format!("event {e}: file_list entry is not a cell of cells"))?;
        let boxes = boxes.cells().ok_or_else(|| format!("event {e}: face_bbx_list entry is not a cell of cells"))?;
        if files.len() != boxes.len() {
            return Err(format!("event {e}: file_list has {} photos, face_bbx_list {}", files.len(), boxes.len()));
        }
        per_event.push((event, files, boxes));
    }
    let root = dir.join("WIDER_val").join("images");
    let mut out_images = Vec::new();
    let mut out_boxes: Vec<Vec<[f64; 4]>> = Vec::new();
    for (e, (event, files, boxes)) in per_event.iter().enumerate() {
        for (i, (file, face)) in files.iter().zip(*boxes).enumerate() {
            let name = file.text().ok_or_else(|| format!("event {e}: file_list entry is not text"))?;
            out_images.push(root.join(event).join(format!("{name}.jpg")));
            out_boxes.push(face_boxes(face, e, i)?);
        }
    }
    let mut keeps: Vec<Vec<Vec<usize>>> = Vec::new();
    for setting in SETTINGS {
        let vars = read_mat(&gt.join(format!("wider_{setting}_val.mat")))?;
        let lists = mat::var(&vars, "gt_list")?.cells().ok_or("gt_list is not a cell array")?;
        if lists.len() != per_event.len() {
            return Err(format!("the {setting} setting lists {} events, the ground truth {}", lists.len(), per_event.len()));
        }
        let mut flat = Vec::new();
        for (e, (event, (_, files, _))) in lists.iter().zip(&per_event).enumerate() {
            let imgs = event.cells().ok_or_else(|| format!("event {e}: the {setting} setting is not a cell of cells"))?;
            if imgs.len() != files.len() {
                return Err(format!("event {e}: the {setting} setting lists {} photos, the ground truth {}", imgs.len(), files.len()));
            }
            for (i, img) in imgs.iter().enumerate() {
                // `flat` holds every earlier photo of every earlier event, so its length is this photo's place in `out_boxes`
                let faces = out_boxes.get(flat.len()).map_or(0, Vec::len);
                flat.push(face_indices(img, setting, e, i, faces)?);
            }
        }
        keeps.push(flat);
    }
    let [easy, medium, hard]: [Vec<Vec<usize>>; 3] = keeps.try_into().map_err(|_| "three settings expected")?;
    Ok(GroundTruth { images: out_images, boxes: out_boxes, keep: [easy, medium, hard] })
}

/// One photo's face boxes `[x, y, w, h]`; a box matrix with fewer than four columns is an error, never dropped boxes.
fn face_boxes(entry: &mat::Value, event: usize, photo: usize) -> Result<Vec<[f64; 4]>, String> {
    let rows = entry.rows().ok_or_else(|| format!("event {event}, photo {photo}: face_bbx_list entry is not a matrix"))?;
    rows.iter()
        .map(|r| {
            <[f64; 4]>::try_from(r.get(..4).unwrap_or(&[]))
                .map_err(|_| format!("event {event}, photo {photo}: face_bbx_list has {} columns, expected at least 4", r.len()))
        })
        .collect()
}

/// The 1-based face numbers one photo's `gt_list` entry counts; a fractional, non-positive or absurd number, or one
/// beyond the photo's `faces` boxes, is an error.
fn face_indices(entry: &mat::Value, setting: &str, event: usize, photo: usize, faces: usize) -> Result<Vec<usize>, String> {
    entry
        .numbers()
        .unwrap_or(&[])
        .iter()
        .map(|&v| {
            if v.is_finite() && v.fract() == 0.0 && (1.0..=1e9).contains(&v) && v as usize <= faces {
                Ok(v as usize)
            } else {
                Err(format!("event {event}, photo {photo}: the {setting} setting has the face index {v}, expected a whole number from 1 to the photo's {faces} faces"))
            }
        })
        .collect()
}

/// A photo as 8-bit RGB: `(pixels, width, height)`.
fn load_rgb(path: &Path) -> Result<(Vec<u8>, usize, usize), String> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(&bytes), options);
    let pixels = decoder.decode().map_err(|e| format!("{}: {e:?}", path.display()))?;
    let info = decoder.info().ok_or_else(|| format!("{}: no image information", path.display()))?;
    let (w, h) = (usize::from(info.width), usize::from(info.height));
    match pixels.len().checked_div(w.saturating_mul(h)) {
        Some(3) => Ok((pixels, w, h)),
        Some(1) => Ok((pixels.iter().flat_map(|g| [*g; 3]).collect(), w, h)),
        _ => Err(format!("{}: unexpected pixel layout", path.display())),
    }
}

/// The detections of one photo as `[x, y, w, h, score]` in pixels.
fn detect(detector: &Detector, path: &Path) -> Result<Vec<[f64; 5]>, String> {
    let (rgb, w, h) = load_rgb(path)?;
    // the benchmark's own score, overlap and count limits; any other option keeps the detector's default
    #[allow(clippy::needless_update, reason = "options the detector gains later keep their defaults here")]
    let options = Options { score: MIN_SCORE, nms_iou: nms_iou(), max_faces: MAX_FACES, ..Options::default() };
    let faces = detector.detect(&rgb, w, h, &options).map_err(|e| format!("{}: {e}", path.display()))?;
    let (w, h) = (w as f64, h as f64);
    Ok(faces
        .iter()
        .map(|f| [f64::from(f.x0) * w, f64::from(f.y0) * h, f64::from(f.x1 - f.x0) * w, f64::from(f.y1 - f.y0) * h, f64::from(f.score)])
        .collect())
}

/// The suppression overlap LightCraft ships for YuNet.
fn nms_iou() -> f32 {
    lightcraft_faces::known::yunet().thresholds.nms_iou.unwrap_or(0.4)
}

pub struct Run {
    pub photos: usize,
    /// Photos that could not be read or searched (scored as having no detections).
    pub failed: usize,
    pub seconds: f64,
    /// Average precision in percent for easy, medium and hard.
    pub ap: [f64; 3],
}

/// Detect on every photo (or `limit` of them, spread evenly) on `threads` threads and score the three settings.
pub fn run(dir: &Path, detector: &Detector, limit: Option<usize>, threads: usize) -> Result<Run, String> {
    let truth = load(dir)?;
    let n = truth.images.len();
    let chosen: Vec<usize> = match limit {
        Some(k) if k > 0 && k < n => (0..k).map(|i| i * n / k).collect(),
        _ => (0..n).collect(),
    };
    let next = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);
    let started = Instant::now();
    let step = (chosen.len() / 20).max(1);
    let joined: Vec<Result<Vec<(usize, Result<Vec<[f64; 5]>, String>)>, String>> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads.clamp(1, 64))
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let at = next.fetch_add(1, Ordering::Relaxed);
                        let Some(path) = chosen.get(at).and_then(|&i| truth.images.get(i)) else { break };
                        done.push((at, detect(detector, path)));
                        let count = finished.fetch_add(1, Ordering::Relaxed) + 1;
                        if count.is_multiple_of(step) {
                            eprintln!("  {count}/{} ({:.1}/s)", chosen.len(), count as f64 / started.elapsed().as_secs_f64().max(1e-9));
                        }
                    }
                    done
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().map_err(|_| "a detection worker stopped unexpectedly".to_string())).collect()
    });
    let seconds = started.elapsed().as_secs_f64();
    let mut preds: Vec<Vec<[f64; 5]>> = vec![Vec::new(); chosen.len()];
    let mut failed = 0;
    for worker in joined {
        for (at, result) in worker? {
            match (result, preds.get_mut(at)) {
                (Ok(found), Some(slot)) => *slot = found,
                (Err(e), _) => {
                    if failed < 5 {
                        eprintln!("  warning: {e}");
                    }
                    failed += 1;
                }
                (Ok(_), None) => {}
            }
        }
    }
    let mut ap = [0.0; 3];
    for (s, score) in ap.iter_mut().enumerate() {
        let images: Vec<Image> = chosen
            .iter()
            .zip(&preds)
            .filter_map(|(&i, p)| Some(Image { gt: truth.boxes.get(i)?, keep: truth.keep.get(s)?.get(i)?, preds: p }))
            .collect();
        *score = metrics::wider_ap(&images) * 100.0;
    }
    Ok(Run { photos: chosen.len(), failed, seconds, ap })
}

#[cfg(test)]
mod tests {
    use super::super::mat::tests::{cell, chars, double, file};
    use super::*;

    /// A scratch directory that is removed afterwards.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("ai-bench-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            assert!(std::fs::create_dir_all(dir.join("eval_tools").join("ground_truth")).is_ok());
            Scratch(dir)
        }

        fn gt(&self, name: &str, bytes: &[u8]) {
            assert!(std::fs::write(self.0.join("eval_tools").join("ground_truth").join(name), bytes).is_ok());
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// One event with one photo "a" holding two faces; the settings count the faces in `keeps`.
    fn write_truth(s: &Scratch, keeps: [&[f64]; 3]) {
        // the boxes [1 2 3 4] and [10 20 30 40], column-major
        let boxes = double("", 2, 4, &[1.0, 10.0, 2.0, 20.0, 3.0, 30.0, 4.0, 40.0]);
        let main = file(&[
            cell("event_list", &[chars("", "0--Parade")]),
            cell("file_list", &[cell("", &[chars("", "a")])]),
            cell("face_bbx_list", &[cell("", &[boxes])]),
        ]);
        s.gt("wider_face_val.mat", &main);
        for (setting, keep) in SETTINGS.iter().zip(keeps) {
            let list = file(&[cell("gt_list", &[cell("", &[double("", keep.len() as i32, 1, keep)])])]);
            s.gt(&format!("wider_{setting}_val.mat"), &list);
        }
    }

    #[test]
    fn ground_truth_reads_boxes_photos_and_the_faces_each_setting_counts() {
        let s = Scratch::new("reads");
        write_truth(&s, [&[1.0], &[1.0, 2.0], &[2.0]]);
        let truth = load(&s.0).unwrap();
        assert_eq!(truth.images, vec![s.0.join("WIDER_val").join("images").join("0--Parade").join("a.jpg")]);
        assert_eq!(truth.boxes, vec![vec![[1.0, 2.0, 3.0, 4.0], [10.0, 20.0, 30.0, 40.0]]]);
        assert_eq!(truth.keep, [vec![vec![1]], vec![vec![1, 2]], vec![vec![2]]]);
    }

    #[test]
    fn a_missing_annotation_file_is_an_error_that_says_how_to_fetch_it() {
        let s = Scratch::new("missing");
        let e = load(&s.0).err().unwrap();
        assert!(e.contains("wider_face_val.mat") && e.contains("ai-bench fetch"), "{e}");
        // the main file alone is not enough: the settings' files are named too
        write_truth(&s, [&[1.0], &[1.0], &[1.0]]);
        assert!(std::fs::remove_file(s.0.join("eval_tools").join("ground_truth").join("wider_hard_val.mat")).is_ok());
        assert!(load(&s.0).err().unwrap().contains("wider_hard_val.mat"));
    }

    #[test]
    fn malformed_annotation_files_are_errors_not_panics() {
        let s = Scratch::new("malformed");
        write_truth(&s, [&[1.0], &[1.0], &[1.0]]);
        // garbage, a truncated file and a valid file without the variable the loader needs
        s.gt("wider_face_val.mat", b"this is not a MAT-file");
        assert!(load(&s.0).is_err());
        write_truth(&s, [&[1.0], &[1.0], &[1.0]]);
        let good = std::fs::read(s.0.join("eval_tools").join("ground_truth").join("wider_easy_val.mat")).unwrap();
        s.gt("wider_easy_val.mat", &good[..good.len() - 5]);
        assert!(load(&s.0).is_err());
        s.gt("wider_easy_val.mat", &file(&[double("other", 1, 1, &[1.0])]));
        assert!(load(&s.0).err().unwrap().contains("gt_list"));
    }

    #[test]
    fn fractional_zero_and_negative_face_indices_are_errors_not_truncated() {
        for bad in [1.5, 0.0, -1.0, f64::NAN] {
            let s = Scratch::new("index");
            write_truth(&s, [&[1.0], &[1.0], &[1.0]]);
            let list = file(&[cell("gt_list", &[cell("", &[double("", 2, 1, &[1.0, bad])])])]);
            s.gt("wider_medium_val.mat", &list);
            let e = load(&s.0).err().unwrap_or_default();
            assert!(e.contains("medium") && e.contains("face index"), "{bad}: {e:?}");
        }
    }

    #[test]
    fn event_and_photo_counts_are_checked_and_errors_name_the_event() {
        // top level: two events, one file list
        let s = Scratch::new("top");
        write_truth(&s, [&[1.0], &[1.0], &[1.0]]);
        let boxes = double("", 1, 4, &[1.0, 2.0, 3.0, 4.0]);
        let main = file(&[
            cell("event_list", &[chars("", "0--A"), chars("", "1--B")]),
            cell("file_list", &[cell("", &[chars("", "a")])]),
            cell("face_bbx_list", &[cell("", std::slice::from_ref(&boxes)), cell("", std::slice::from_ref(&boxes))]),
        ]);
        s.gt("wider_face_val.mat", &main);
        let e = load(&s.0).err().unwrap_or_default();
        assert!(e.contains("event_list") && e.contains("file_list"), "{e:?}");
        // event 1 has two files but one box list
        let main = file(&[
            cell("event_list", &[chars("", "0--A"), chars("", "1--B")]),
            cell("file_list", &[cell("", &[chars("", "a")]), cell("", &[chars("", "b"), chars("", "c")])]),
            cell("face_bbx_list", &[cell("", std::slice::from_ref(&boxes)), cell("", &[boxes])]),
        ]);
        s.gt("wider_face_val.mat", &main);
        let e = load(&s.0).err().unwrap_or_default();
        assert!(e.contains("event 1"), "{e:?}");
    }

    #[test]
    fn a_setting_with_the_wrong_event_count_is_an_error() {
        let s = Scratch::new("events");
        write_truth(&s, [&[1.0], &[1.0], &[1.0]]);
        let item = cell("", &[double("", 1, 1, &[1.0])]);
        s.gt("wider_hard_val.mat", &file(&[cell("gt_list", &[item.clone(), item])]));
        let e = load(&s.0).err().unwrap_or_default();
        assert!(e.contains("hard") && e.contains("2 events"), "{e:?}");
    }

    #[test]
    fn settings_that_disagree_with_the_ground_truth_on_the_photo_count_are_an_error() {
        let s = Scratch::new("count");
        write_truth(&s, [&[1.0], &[1.0], &[1.0]]);
        let two = file(&[cell("gt_list", &[cell("", &[double("", 1, 1, &[1.0]), double("", 1, 1, &[1.0])])])]);
        s.gt("wider_medium_val.mat", &two);
        assert!(load(&s.0).err().unwrap().contains("event 0"));
    }

    #[test]
    fn a_box_matrix_with_fewer_than_four_columns_is_an_error_not_dropped_boxes() {
        let s = Scratch::new("narrow");
        write_truth(&s, [&[1.0], &[1.0], &[1.0]]);
        let narrow = double("", 2, 3, &[1.0, 10.0, 2.0, 20.0, 3.0, 30.0]);
        let main = file(&[
            cell("event_list", &[chars("", "0--Parade")]),
            cell("file_list", &[cell("", &[chars("", "a")])]),
            cell("face_bbx_list", &[cell("", &[narrow])]),
        ]);
        s.gt("wider_face_val.mat", &main);
        let e = load(&s.0).err().unwrap_or_default();
        assert!(e.contains("event 0") && e.contains("3 columns"), "{e:?}");
    }

    #[test]
    fn a_face_index_beyond_the_photos_box_count_is_an_error() {
        // the photo has two faces; each setting in turn counts a third
        for (at, setting) in SETTINGS.iter().enumerate() {
            let s = Scratch::new("beyond");
            let mut keeps: [&[f64]; 3] = [&[1.0], &[1.0], &[1.0]];
            keeps[at] = &[1.0, 3.0];
            write_truth(&s, keeps);
            let e = load(&s.0).err().unwrap_or_default();
            assert!(e.contains(setting) && e.contains("2 faces"), "{setting}: {e:?}");
        }
    }
}
