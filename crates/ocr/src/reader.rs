//! Reading the text in a photo with a PP-OCRv6 set: PaddleOCR 3's OCR pipeline (text type "general", no document
//! unwarping), step for step.
//!
//! 1. **Detect** ([`detect`]): the photo, BGR, scaled so its sides are multiples of 32 (and at most
//!    [`Options::max_side`]), normalized with ImageNet's mean and deviation, gives a text-probability map; DB
//!    post-processing ([`crate::db`]) turns it into line boxes in reading order.
//! 2. **Cut out** each line along its minimum-area rectangle, straightened ([`Rgb::crop`]); tall lines are turned a
//!    quarter.
//! 3. **Orient**: the text-line classifier (160 × 80) says whether the line is upside down; such lines are turned half way.
//! 4. **Recognise**: the line scaled to 48 pixels high (at least 320 wide, padded), normalized to [-1, 1], gives per-step
//!    character probabilities; greedy CTC decoding ([`Alphabet::decode`]) gives the text and its confidence. As in
//!    PaddleOCR, the lines are read [`REC_BATCH`] at a time in order of their width-to-height ratio, each batch padded
//!    to its widest line (one model run per batch, which is what makes the graphics card worth using).
//!
//! With `LIGHTCRAFT_PROFILE` set, [`Reader::read`] prints the time of each step to stderr.

use std::path::Path;

use candle_core::{Device, Tensor};

use crate::ctc::Alphabet;
use crate::db::{self, Params, Quad};
use crate::exec::Graph;
use crate::image::Rgb;
use crate::known::{self, ModelSet};
use crate::{Error, Result};

/// One line of text found in a photo.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub text: String,
    /// The recogniser's confidence (mean probability of its characters), 0 to 1.
    pub score: f32,
    /// The detector's confidence in the box.
    pub box_score: f32,
    /// Corners clockwise from the top-left, in pixels of the image read.
    pub quad: Quad,
}

/// The confidence below which LightCraft keeps no line it reads (search and the benchmark use it; PaddleOCR's own
/// default keeps everything).
pub const SHIPPED_MIN_SCORE: f32 = 0.5;

/// How the pipeline runs (PaddleOCR 3's defaults).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    pub detection: Params,
    /// The photo is scaled up until its short side is at least this (`limit_side_len`, type "min").
    pub min_side: usize,
    /// …and down until its long side is at most this (`max_side_limit`).
    pub max_side: usize,
    /// Lines read with less confidence are left out.
    pub min_score: f32,
    /// Turn upside-down lines (the orientation classifier).
    pub orientation: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { detection: Params::default(), min_side: 64, max_side: 4000, min_score: 0.0, orientation: true }
    }
}

const DET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const DET_STD: [f32; 3] = [0.229, 0.224, 0.225];
/// The recogniser's input height and narrowest width.
const REC_H: usize = 48;
const REC_MIN_W: usize = 320;
/// The widest line the recogniser takes (PaddleOCR's `max_imgW`): longer ones are squeezed.
const REC_MAX_W: usize = 3200;
const ORI_W: usize = 160;
const ORI_H: usize = 80;

/// A loaded model set.
pub struct Reader {
    det: Graph,
    rec: Graph,
    ori: Graph,
    alphabet: Alphabet,
    set: &'static ModelSet,
}

/// Lines recognised per model run (PaddleOCR's `TextRecognition.batch_size`).
pub const REC_BATCH: usize = 6;
/// Lines whose orientation is classified per model run (each is its own 160 × 80 input, so the batch changes nothing
/// but the speed).
const ORI_BATCH: usize = 32;

/// `imgs` (all `h` high) as one `[n, 3, h, width]` tensor in BGR order: `(v / 255 - mean[c]) / std[c]` for channel c
/// (the mean and deviation applied by index to the BGR array, as PaddleOCR does), each image at the left and zeros to its
/// right up to `width`. Built on the host and copied to the device once.
fn bgr_batch(imgs: &[Rgb], mean: [f32; 3], std: [f32; 3], width: usize, device: &Device) -> Result<Tensor> {
    let h = imgs.first().map_or(0, Rgb::height);
    if imgs.iter().any(|i| i.height() != h || i.width() > width) {
        return Err(Error::Input("the images of a batch must share their height and fit its width".into()));
    }
    let plane = h * width;
    let mut planes = vec![0f32; imgs.len() * 3 * plane];
    for (k, img) in imgs.iter().enumerate() {
        let w = img.width();
        for (i, px) in img.data().as_chunks::<3>().0.iter().enumerate() {
            let (x, y) = (i % w, i / w);
            for (c, v) in [px[2], px[1], px[0]].into_iter().enumerate() {
                if let Some(slot) = planes.get_mut((k * 3 + c) * plane + y * width + x) {
                    *slot = (f32::from(v) / 255.0 - mean[c]) / std[c];
                }
            }
        }
    }
    Ok(Tensor::from_vec(planes, (imgs.len(), 3, h, width), device)?)
}

/// `img` as a `[1, 3, h, w]` tensor ([`bgr_batch`] of one), padded with zeros to `pad_to` columns.
fn bgr_tensor(img: &Rgb, mean: [f32; 3], std: [f32; 3], pad_to: Option<usize>, device: &Device) -> Result<Tensor> {
    bgr_batch(std::slice::from_ref(img), mean, std, pad_to.unwrap_or(0).max(img.width()), device)
}

/// A line scaled for the recogniser (PaddleOCR's `resize_norm_img`): 48 high, as wide as its ratio asks (up to
/// [`REC_MAX_W`], longer lines squeezed), and the width it is padded to on its own (at least 320).
fn rec_input(line: &Rgb) -> Result<(Rgb, usize)> {
    let ratio = line.width() as f64 / line.height() as f64;
    let width = ((REC_H as f64 * ratio.max(REC_MIN_W as f64 / REC_H as f64)) as usize).max(1);
    if width > REC_MAX_W {
        return Ok((line.resize(REC_MAX_W, REC_H)?, REC_MAX_W));
    }
    let w = ((REC_H as f64 * ratio).ceil() as usize).clamp(1, width);
    Ok((line.resize(w, REC_H)?, width))
}

/// Whether `LIGHTCRAFT_PROFILE` is set (step timings to stderr).
fn profiling() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LIGHTCRAFT_PROFILE").is_some())
}

/// The detector's input size for a `w × h` photo (`DetResizeForTest`, type "min"): `None` when the photo can be read
/// as it is.
fn det_size(w: usize, h: usize, o: &Options) -> (usize, usize) {
    let short = w.min(h) as f64;
    let ratio = if short < o.min_side as f64 { o.min_side as f64 / short } else { 1.0 };
    let (mut rw, mut rh) = ((w as f64 * ratio) as usize, (h as f64 * ratio) as usize);
    if rw.max(rh) > o.max_side {
        let r = o.max_side as f64 / rw.max(rh) as f64;
        (rw, rh) = ((rw as f64 * r) as usize, (rh as f64 * r) as usize);
    }
    let to32 = |v: usize| (((v as f64 / 32.0).round() as usize) * 32).max(32);
    (to32(rw), to32(rh))
}

impl Reader {
    /// Load `set` from `dir` onto `device`.
    pub fn load(set: &'static ModelSet, dir: &Path, device: &Device) -> Result<Self> {
        if !set.installed_in(dir) {
            return Err(Error::Model(format!("{} is not completely downloaded in {}", set.name, dir.display())));
        }
        let alphabet = Alphabet::from_preprocessor_config(&dir.join(known::ALPHABET))?;
        Ok(Self {
            det: Graph::load(&dir.join(known::DETECTOR), device)?,
            rec: Graph::load(&dir.join(known::RECOGNISER), device)?,
            ori: Graph::load(&dir.join(known::ORIENTATION), device)?,
            alphabet,
            set,
        })
    }

    pub fn set(&self) -> &'static ModelSet {
        self.set
    }

    pub fn device(&self) -> &Device {
        self.det.device()
    }

    /// The text-line boxes in `img` (step 1), with the detector's scores.
    pub fn detect(&self, img: &Rgb, o: &Options) -> Result<Vec<(Quad, f32)>> {
        let prob = self.probability_map(img, o)?;
        let (h, w) = prob.dims2()?;
        let v: Vec<f32> = prob.flatten_all()?.to_vec1()?;
        Ok(db::boxes(&v, w, h, img.width(), img.height(), &o.detection))
    }

    /// The detector's text-probability map for `img` (`[h, w]` at the detector's input size).
    pub fn probability_map(&self, img: &Rgb, o: &Options) -> Result<Tensor> {
        let (w, h) = det_size(img.width(), img.height(), o);
        let x = bgr_tensor(&img.resize(w, h)?, DET_MEAN, DET_STD, None, self.device())?;
        let out = self.det.run(&x)?;
        let map = out.first().ok_or_else(|| Error::Model("the detector gave no output".into()))?;
        Ok(map.squeeze(0)?.squeeze(0)?)
    }

    /// Whether a cut-out line is upside down (step 3).
    pub fn upside_down(&self, line: &Rgb) -> Result<bool> {
        Ok(self.upside_down_all(std::slice::from_ref(line))?.first().copied().unwrap_or(false))
    }

    /// [`Reader::upside_down`] for each of `lines`, [`ORI_BATCH`] per model run.
    fn upside_down_all(&self, lines: &[Rgb]) -> Result<Vec<bool>> {
        let mut out = Vec::with_capacity(lines.len());
        for chunk in lines.chunks(ORI_BATCH) {
            let small = chunk.iter().map(|l| l.resize(ORI_W, ORI_H)).collect::<Result<Vec<_>>>()?;
            let x = bgr_batch(&small, DET_MEAN, DET_STD, ORI_W, self.device())?;
            let run = self.ori.run(&x)?;
            let p = run.first().ok_or_else(|| Error::Model("the orientation model gave no output".into()))?;
            let p: Vec<Vec<f32>> = p.reshape((chunk.len(), ()))?.to_vec2()?;
            for row in &p {
                // (upright, upside down): any other shape is a wrong or damaged model, not "upright"
                let [up, down] = row[..] else {
                    return Err(Error::Model(format!("the orientation model gave {} values for a line, not 2", row.len())));
                };
                out.push(down > up);
            }
        }
        Ok(out)
    }

    /// The recogniser's per-step probabilities for a cut-out line (step 4): `[steps, classes]`.
    pub fn recognise_probs(&self, line: &Rgb) -> Result<Tensor> {
        let (resized, pad) = rec_input(line)?;
        let x = bgr_tensor(&resized, [0.5; 3], [0.5; 3], Some(pad), self.device())?;
        let out = self.rec.run(&x)?;
        Ok(out.first().ok_or_else(|| Error::Model("the recogniser gave no output".into()))?.squeeze(0)?)
    }

    /// The text of a cut-out line and its confidence.
    pub fn recognise(&self, line: &Rgb) -> Result<(String, f32)> {
        Ok(self.recognise_all(std::slice::from_ref(line), 1)?.pop().unwrap_or_default())
    }

    /// The text and confidence of each of `lines`, in their order: read `batch` at a time in order of their
    /// width-to-height ratio (stable), each batch padded to its widest line, as PaddleOCR's pipeline reads them.
    fn recognise_all(&self, lines: &[Rgb], batch: usize) -> Result<Vec<(String, f32)>> {
        let ratio = |l: &Rgb| l.width() as f64 / l.height() as f64;
        let mut order: Vec<usize> = (0..lines.len()).collect();
        order.sort_by(|a, b| lines.get(*a).map(ratio).unwrap_or(0.0).total_cmp(&lines.get(*b).map(ratio).unwrap_or(0.0)));
        let mut out = vec![(String::new(), 0f32); lines.len()];
        for ids in order.chunks(batch.max(1)) {
            let inputs = ids.iter().filter_map(|i| lines.get(*i)).map(rec_input).collect::<Result<Vec<_>>>()?;
            let width = inputs.iter().map(|(_, pad)| *pad).max().unwrap_or(REC_MIN_W);
            let imgs: Vec<Rgb> = inputs.into_iter().map(|(img, _)| img).collect();
            let x = bgr_batch(&imgs, [0.5; 3], [0.5; 3], width, self.device())?;
            let run = self.rec.run(&x)?;
            let p = run.first().ok_or_else(|| Error::Model("the recogniser gave no output".into()))?;
            let (n, steps, classes) = p.dims3()?;
            if classes != self.alphabet.len() || n != ids.len() {
                return Err(Error::Model(format!(
                    "the recogniser gave {n} × {classes} classes for {} lines and {} symbols",
                    ids.len(),
                    self.alphabet.len()
                )));
            }
            // one copy to the host per batch
            let all: Vec<f32> = p.flatten_all()?.to_vec1()?;
            for (k, i) in ids.iter().enumerate() {
                if let (Some(probs), Some(slot)) = (all.get(k * steps * classes..(k + 1) * steps * classes), out.get_mut(*i)) {
                    *slot = self.alphabet.decode(probs, classes);
                }
            }
        }
        Ok(out)
    }

    /// Every line of text in `img`, in reading order.
    pub fn read(&self, img: &Rgb, o: &Options) -> Result<Vec<Line>> {
        let t0 = std::time::Instant::now();
        let found = self.detect(img, o)?;
        let t1 = std::time::Instant::now();
        let (mut quads, mut cuts) = (Vec::new(), Vec::new());
        for (quad, box_score) in found {
            if let Some(cut) = db::crop_box(&quad).and_then(|r| img.crop(&r)) {
                quads.push((quad, box_score));
                cuts.push(cut);
            }
        }
        let t2 = std::time::Instant::now();
        if o.orientation {
            let flips = self.upside_down_all(&cuts)?;
            for (cut, flip) in cuts.iter_mut().zip(flips) {
                if flip {
                    *cut = cut.rot180();
                }
            }
        }
        let t3 = std::time::Instant::now();
        let texts = self.recognise_all(&cuts, REC_BATCH)?;
        if profiling() {
            let ms = |a: std::time::Instant, b: std::time::Instant| (b - a).as_secs_f64() * 1000.0;
            let t4 = std::time::Instant::now();
            eprintln!(
                "ocr {}x{} on {:?}: detect {:.0} ms, cut {:.0} ms, orient {:.0} ms, recognise {:.0} ms ({} lines)",
                img.width(),
                img.height(),
                self.device().location(),
                ms(t0, t1),
                ms(t1, t2),
                ms(t2, t3),
                ms(t3, t4),
                cuts.len()
            );
        }
        Ok(quads
            .into_iter()
            .zip(texts)
            .filter(|(_, (text, score))| *score >= o.min_score && !text.trim().is_empty())
            .map(|((quad, box_score), (text, score))| Line { text, score, box_score, quad })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{Options, Reader, det_size};
    use crate::{Error, known};
    use candle_core::Device;

    #[test]
    fn detector_sizes_follow_paddleocr() {
        let o = Options::default();
        // multiples of 32, nearest
        assert_eq!(det_size(1000, 750, &o), (992, 736));
        // a tiny image is scaled up to a 64-pixel short side
        assert_eq!(det_size(40, 20, &o), (128, 64));
        // a huge one down to 4000 on its long side
        assert_eq!(det_size(8000, 6000, &o), (4000, 3008));
        assert_eq!(det_size(1, 1, &o), (64, 64));
    }

    #[test]
    fn a_missing_or_damaged_model_folder_is_an_error() {
        let Some(set) = known::recommended() else { return };
        let dir = std::env::temp_dir().join(format!("lc-ocr-reader-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(Reader::load(set, &dir, &Device::Cpu), Err(Error::Model(_))), "no folder");
        // every file present at its pinned size but zeros: the graphs and the alphabet are refused, not run
        assert!(std::fs::create_dir_all(&dir).is_ok());
        for f in set.files {
            let file = std::fs::File::create(dir.join(f.name));
            assert!(file.and_then(|file| file.set_len(f.size)).is_ok());
        }
        assert!(set.installed_in(&dir));
        assert!(matches!(Reader::load(set, &dir, &Device::Cpu), Err(Error::Model(_))), "damaged files");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
