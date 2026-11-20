//! People masks from Google's MediaPipe models (Apache-2.0), run by the pure-Rust TFLite
//! interpreter ([`crate::tflite`]) on the files Google publishes, unchanged:
//!
//! - [`PeopleModel`]: Selfie Multiclass — every pixel's probability of background, hair, body skin,
//!   face skin, clothes and other (accessories);
//! - [`FaceModel`]: Face Landmarker — BlazeFace finds the faces (on overlapping windows, as photos
//!   hold smaller faces than the selfies it was made for), FaceMesh-V2 places 478 landmarks on each,
//!   and [`crate::mediapipe`] turns them into eyes, irises, brows, lips and teeth.
//!
//! [`people_mask`] combines them into one mask for a People selection ("Hair" + "Lips" of the
//! second person from the left, …). Own design (OneSlips).

use std::path::Path;

use crate::matte;
use crate::{Error, Result};
use lcc_tflite as tflite;
use lcc_tflite::mediapipe::{self, Detection, FacePart, Grid};

/// The Selfie Multiclass file in its model folder.
pub const PEOPLE_FILE: &str = "selfie_multiclass_256x256.tflite";
/// The Face Landmarker bundle in its model folder.
pub const FACE_FILE: &str = "face_landmarker.task";
/// Faces scoring below this are ignored (BlazeFace's false positives on hands, tattoos and foliage
/// score 0.5–0.6 on wedding photos; real faces, even in profile, above 0.65).
pub const FACE_THRESHOLD: f32 = 0.65;

fn read(dir: &Path, name: &str, max: u64) -> Result<Vec<u8>> {
    let p = dir.join(name);
    let len = std::fs::metadata(&p).map_err(|e| Error::Model(format!("{}: {e}", p.display())))?.len();
    if len > max {
        return Err(Error::Model(format!("{}: unexpectedly large ({len} bytes)", p.display())));
    }
    std::fs::read(&p).map_err(|e| Error::Model(format!("{}: {e}", p.display())))
}

/// Selfie Multiclass.
pub struct PeopleModel {
    net: tflite::Model,
}

/// Per-pixel class probabilities over the image stretched to `side × side` (softmax over
/// [`mediapipe::SELFIE_CLASSES`], interleaved: `[y][x][class]`).
pub struct PeopleMap {
    pub side: usize,
    pub probs: Vec<f32>,
}

impl PeopleMap {
    /// The summed probability of `classes` at normalized point `(u, v)` (bilinear).
    pub fn at(&self, classes: &[usize], u: f32, v: f32) -> f32 {
        let n = mediapipe::SELFIE_CLASSES.len();
        let s = self.side;
        if s == 0 {
            return 0.0;
        }
        let fx = (u * s as f32 - 0.5).clamp(0.0, (s - 1) as f32);
        let fy = (v * s as f32 - 0.5).clamp(0.0, (s - 1) as f32);
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let px = |x: usize, y: usize| -> f32 {
            let base = (y.min(s - 1) * s + x.min(s - 1)) * n;
            classes.iter().filter_map(|c| self.probs.get(base + c)).sum()
        };
        let top = px(x0, y0) * (1.0 - tx) + px(x0 + 1, y0) * tx;
        let bot = px(x0, y0 + 1) * (1.0 - tx) + px(x0 + 1, y0 + 1) * tx;
        (top * (1.0 - ty) + bot * ty).clamp(0.0, 1.0)
    }

    /// The share of the image that is a person (any class but background), 0..1.
    pub fn person_share(&self) -> f32 {
        let n = mediapipe::SELFIE_CLASSES.len();
        let cells = self.probs.len() / n.max(1);
        if cells == 0 {
            return 0.0;
        }
        self.probs.chunks(n).filter(|p| p.first().is_some_and(|bg| *bg < 0.5)).count() as f32 / cells as f32
    }
}

impl PeopleModel {
    pub fn load(dir: &Path) -> Result<Self> {
        let bytes = read(dir, PEOPLE_FILE, 64 << 20)?;
        let net = tflite::Model::from_bytes(&bytes)?;
        let side = mediapipe::SELFIE_INPUT;
        if net.input_shapes() != vec![vec![1, side, side, 3]]
            || net.output_shapes().first() != Some(&vec![1, side, side, mediapipe::SELFIE_CLASSES.len()])
        {
            return Err(Error::Model(format!("{}: not the Selfie Multiclass model", dir.join(PEOPLE_FILE).display())));
        }
        Ok(Self { net })
    }

    /// Class probabilities for an 8-bit RGB image (`w × h`), stretched to the model's input.
    pub fn segment(&self, rgb: &[u8], w: usize, h: usize) -> Result<PeopleMap> {
        let side = mediapipe::SELFIE_INPUT;
        let (sx, sy) = (side as f32 / w.max(1) as f32, side as f32 / h.max(1) as f32);
        let input = mediapipe::warp(rgb, w, h, &[sx, 0.0, 0.0, 0.0, sy, 0.0], side, -1.0, 1.0);
        let out = self.net.run(&[&input])?;
        let logits = out.into_iter().next().ok_or_else(|| Error::Model("Selfie Multiclass gave no output".into()))?;
        if logits.len() != side * side * mediapipe::SELFIE_CLASSES.len() {
            return Err(Error::Model("Selfie Multiclass: unexpected output size".into()));
        }
        Ok(PeopleMap { side, probs: mediapipe::selfie_softmax(&logits) })
    }

    /// Class probabilities for the part `bx` (photo pixels `[x0, y0, x1, y1]`) of `rgb` (`w × h`),
    /// that part stretched to the model's input (supersampled, so a crop larger than the input is
    /// averaged rather than aliased).
    pub fn segment_crop(&self, rgb: &[u8], w: usize, h: usize, bx: [f32; 4]) -> Result<PeopleMap> {
        let side = mediapipe::SELFIE_INPUT;
        let input = crop_input(rgb, w, h, bx, side);
        let out = self.net.run(&[&input])?;
        let logits = out.into_iter().next().ok_or_else(|| Error::Model("Selfie Multiclass gave no output".into()))?;
        if logits.len() != side * side * mediapipe::SELFIE_CLASSES.len() {
            return Err(Error::Model("Selfie Multiclass: unexpected output size".into()));
        }
        Ok(PeopleMap { side, probs: mediapipe::selfie_softmax(&logits) })
    }

    /// The class map of the whole image (`whole`, from [`PeopleModel::segment`]) with the people
    /// in `boxes` (normalized `[x0, y0, x1, y1]`, from [`person_boxes`]) segmented again on their
    /// own crops of `rgb` (`w × h`: the sharpest render at hand, any size) and pasted back, as a
    /// `side × side` map. A group's people are a few cells tall in the whole image's 256-cell map,
    /// so its clothes, skin and hair blur into each other and onto the grass; on a crop each person
    /// fills the model's input. Where crops overlap they blend, fading out towards their inner
    /// edges; outside every crop the whole map stays. The crops run in parallel.
    pub fn segment_people(&self, rgb: &[u8], w: usize, h: usize, whole: &PeopleMap, boxes: &[[f32; 4]], side: usize) -> Result<PeopleMap> {
        let (boxes, crops) = self.segment_crops(rgb, w, h, boxes)?;
        Ok(paste_crops(whole, &boxes, &crops, side))
    }

    /// The class maps of `boxes` (normalized, at most [`MAX_CROPS`], malformed ones dropped) of
    /// `rgb` (`w × h`), each on its own crop, in parallel: `(the boxes kept, their maps)`, for
    /// [`paste_crops`] (kept apart so a photo's crops can be cached and pasted again later).
    pub fn segment_crops(&self, rgb: &[u8], w: usize, h: usize, boxes: &[[f32; 4]]) -> Result<(Vec<[f32; 4]>, Vec<PeopleMap>)> {
        use rayon::prelude::*;
        let boxes: Vec<[f32; 4]> =
            boxes.iter().take(MAX_CROPS).copied().filter(|b| b.iter().all(|v| v.is_finite()) && b[2] > b[0] && b[3] > b[1]).collect();
        let (fw, fh) = (w as f32, h as f32);
        let crops: Vec<PeopleMap> =
            boxes.par_iter().map(|b| self.segment_crop(rgb, w, h, [b[0] * fw, b[1] * fh, b[2] * fw, b[3] * fh])).collect::<Result<Vec<_>>>()?;
        Ok((boxes, crops))
    }
}

/// The class map `whole` with `crops` (each the class map of its box of `boxes`, normalized
/// `[x0, y0, x1, y1]`) pasted back, as a `side × side` map: where crops overlap they blend, each
/// fading out over its inner tenth on the sides the photo goes on past; outside every crop the
/// whole map stays.
pub fn paste_crops(whole: &PeopleMap, boxes: &[[f32; 4]], crops: &[PeopleMap], side: usize) -> PeopleMap {
    let n = mediapipe::SELFIE_CLASSES.len();
    let side = side.clamp(16, 2048);
    let mut acc = vec![0f32; side * side * n];
    let mut weight = vec![0f32; side * side];
    for (b, map) in boxes.iter().zip(crops) {
        let (i0, i1) = (((b[0] * side as f32).floor().max(0.0) as usize).min(side), ((b[2] * side as f32).ceil().max(0.0) as usize).min(side));
        let (j0, j1) = (((b[1] * side as f32).floor().max(0.0) as usize).min(side), ((b[3] * side as f32).ceil().max(0.0) as usize).min(side));
        // fade over the inner tenth of the crop on each side the photo goes on past
        let fade = |t: f32, lo_open: bool, hi_open: bool| -> f32 {
            let lo = if lo_open { (t / 0.1).clamp(0.0, 1.0) } else { 1.0 };
            let hi = if hi_open { ((1.0 - t) / 0.1).clamp(0.0, 1.0) } else { 1.0 };
            lo.min(hi)
        };
        for j in j0..j1 {
            let v = (j as f32 + 0.5) / side as f32;
            let t = (v - b[1]) / (b[3] - b[1]);
            if !(0.0..=1.0).contains(&t) {
                continue;
            }
            let wy = fade(t, b[1] > 0.001, b[3] < 0.999);
            for i in i0..i1 {
                let u = (i as f32 + 0.5) / side as f32;
                let s = (u - b[0]) / (b[2] - b[0]);
                if !(0.0..=1.0).contains(&s) {
                    continue;
                }
                let wt = wy * fade(s, b[0] > 0.001, b[2] < 0.999);
                if wt <= 0.0 {
                    continue;
                }
                let c = j * side + i;
                if let Some(wc) = weight.get_mut(c) {
                    *wc += wt;
                }
                for k in 0..n {
                    if let Some(a) = acc.get_mut(c * n + k) {
                        *a += wt * map.at(&[k], s, t);
                    }
                }
            }
        }
    }
    let mut probs = vec![0f32; side * side * n];
    for c in 0..side * side {
        let (u, v) = (((c % side) as f32 + 0.5) / side as f32, ((c / side) as f32 + 0.5) / side as f32);
        let wc = weight.get(c).copied().unwrap_or(0.0);
        // (a trace of the whole map, so a crop's fading edge blends into it)
        let w0 = if wc > 0.0 { (1.0 - wc).max(1e-3) } else { 1.0 };
        for k in 0..n {
            let a = acc.get(c * n + k).copied().unwrap_or(0.0);
            if let Some(p) = probs.get_mut(c * n + k) {
                *p = (w0 * whole.at(&[k], u, v) + a) / (w0 + wc);
            }
        }
    }
    PeopleMap { side, probs }
}

/// Most people [`PeopleModel::segment_people`] crops (the rest keep the whole image's map).
pub const MAX_CROPS: usize = 160;

/// A person is cropped for the class map when they are shorter than this share of the photo's
/// height (about 9.5 face heights): in the whole image's 256-cell map they'd be under ~150 cells.
/// A bridesmaid seen whole in a ceremony frame (her face a twentieth of the photo's height) is
/// still too small there: her long gown blurs into the floor and reads as background, and her
/// bouquet as clothes; on her own crop the gown is clothes and the flowers aren't.
pub const CROP_BELOW: f32 = 0.6;

/// The bodies of the small people among `faces` (in a `w × h` photo) as normalized boxes for
/// [`PeopleModel::segment_people`]: from a little above the head to the feet (or the photo's
/// edge), wide enough for a gown. Empty when everyone is large enough for the whole-image map
/// (close-ups, couples): no crops, no extra time.
pub fn person_boxes(faces: &[Face], w: usize, h: usize) -> Vec<[f32; 4]> {
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let (fw, fh) = (w as f32, h as f32);
    faces
        .iter()
        .filter_map(|f| {
            let d = &f.detection;
            let s = d.height().max(d.width());
            if !(s.is_finite() && s > 0.0) || s * 9.5 >= fh * CROP_BELOW {
                return None;
            }
            let [cx, cy] = d.center();
            let b = [
                ((cx - 3.5 * s) / fw).clamp(0.0, 1.0),
                ((cy - 1.5 * s) / fh).clamp(0.0, 1.0),
                ((cx + 3.5 * s) / fw).clamp(0.0, 1.0),
                ((cy + 8.5 * s) / fh).clamp(0.0, 1.0),
            ];
            (b[2] > b[0] && b[3] > b[1]).then_some(b)
        })
        .take(MAX_CROPS)
        .collect()
}

/// The model input for the part `bx` (photo pixels) of `rgb` (`w × h`): `side²` RGB in −1..1,
/// each output pixel the mean of up to 4 × 4 bilinear samples over its footprint; black outside
/// the photo.
fn crop_input(rgb: &[u8], w: usize, h: usize, bx: [f32; 4], side: usize) -> Vec<f32> {
    let mut out = vec![-1f32; side * side * 3];
    if w == 0 || h == 0 || rgb.len() < w.saturating_mul(h).saturating_mul(3) || !bx.iter().all(|v| v.is_finite()) {
        return out;
    }
    let (cw, ch) = ((bx[2] - bx[0]).max(1e-3), (bx[3] - bx[1]).max(1e-3));
    let (dx, dy) = (cw / side as f32, ch / side as f32);
    let (kx, ky) = ((dx.ceil() as usize).clamp(1, 4), (dy.ceil() as usize).clamp(1, 4));
    let px = |x: isize, y: isize, c: usize| -> f32 {
        if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
            return 0.0;
        }
        rgb.get((y as usize * w + x as usize) * 3 + c).map_or(0.0, |v| f32::from(*v))
    };
    let bilinear = |x: f32, y: f32, c: usize| -> f32 {
        let (x0, y0) = (x.floor(), y.floor());
        let (tx, ty) = (x - x0, y - y0);
        let (xi, yi) = (x0 as isize, y0 as isize);
        let top = px(xi, yi, c) * (1.0 - tx) + px(xi + 1, yi, c) * tx;
        let bot = px(xi, yi + 1, c) * (1.0 - tx) + px(xi + 1, yi + 1, c) * tx;
        top * (1.0 - ty) + bot * ty
    };
    for oy in 0..side {
        for ox in 0..side {
            let mut acc = [0f32; 3];
            for sy in 0..ky {
                for sx in 0..kx {
                    let x = bx[0] + (ox as f32 + (sx as f32 + 0.5) / kx as f32) * dx - 0.5;
                    let y = bx[1] + (oy as f32 + (sy as f32 + 0.5) / ky as f32) * dy - 0.5;
                    for (c, a) in acc.iter_mut().enumerate() {
                        *a += bilinear(x, y, c);
                    }
                }
            }
            let k = (kx * ky) as f32;
            let o = (oy * side + ox) * 3;
            for (c, a) in acc.iter().enumerate() {
                if let Some(v) = out.get_mut(o + c) {
                    *v = a / k / 127.5 - 1.0;
                }
            }
        }
    }
    out
}

/// Face Landmarker: BlazeFace + FaceMesh-V2.
/// How far a GPU's outputs may be from the CPU's (relative to each output's range) for the face models to use it.
const GPU_TOLERANCE: f32 = 2e-3;

pub struct FaceModel {
    detector: tflite::Model,
    mesh: tflite::Model,
    /// BlazeFace full range ([`FAR_FILE`]), for the small faces of group photos; optional.
    far: Option<tflite::Model>,
}

/// BlazeFace full range, beside the Face Landmarker bundle in its folder (optional: without it,
/// small faces are looked for with the short-range model on finer windows).
pub const FAR_FILE: &str = "face_detection_full_range.tflite";
/// Full-range detections below this are ignored.
pub const FAR_THRESHOLD: f32 = 0.5;
/// A face FaceMesh doesn't take counts only when the detector is at least this sure of it.
pub const FACE_THRESHOLD_NO_MESH: f32 = 0.75;
/// Long edge of the copy the full-range detector looks at in [`FaceModel::detect_all`].
pub const FAR_EDGE: usize = 1600;

/// A face with its landmarks (photo pixels).
#[derive(Clone, Debug)]
pub struct Face {
    pub detection: Detection,
    /// FaceMesh's 478 landmarks, when it accepted the face (it doesn't handle strong profiles).
    pub landmarks: Option<Vec<[f32; 2]>>,
}

impl FaceModel {
    pub fn load(dir: &Path) -> Result<Self> {
        let bundle = read(dir, FACE_FILE, 64 << 20)?;
        let entry = |name: &str| {
            tflite::zip::entry(&bundle, name).ok_or_else(|| Error::Model(format!("{}: no {name} inside", dir.join(FACE_FILE).display())))
        };
        let detector = tflite::Model::from_bytes(entry("face_detector.tflite")?)?;
        let mesh = tflite::Model::from_bytes(entry("face_landmarks_detector.tflite")?)?;
        let (b, m) = (mediapipe::BLAZEFACE_INPUT, mediapipe::MESH_INPUT);
        if detector.input_shapes() != vec![vec![1, b, b, 3]] || mesh.input_shapes() != vec![vec![1, m, m, 3]] {
            return Err(Error::Model(format!("{}: not the Face Landmarker bundle", dir.join(FACE_FILE).display())));
        }
        // the full-range detector is optional: a broken or foreign file is left unused
        let far = read(dir, FAR_FILE, 8 << 20)
            .ok()
            .and_then(|b| tflite::Model::from_bytes(&b).ok())
            .filter(|m| m.input_shapes() == vec![vec![1, mediapipe::FAR_INPUT, mediapipe::FAR_INPUT, 3]]);
        Ok(Self { detector, mesh, far })
    }

    /// Run BlazeFace and FaceMesh on `accelerator` (the GPU) from now on, each only once it gave the CPU's outputs
    /// on a test picture and (with `require_faster`) was faster than the CPU; a model it was not for stays on the
    /// CPU. `Ok` with what runs there when the detector does, else why nothing does.
    pub fn accelerate(
        &self,
        accelerator: std::sync::Arc<dyn tflite::device::Accelerator>,
        require_faster: bool,
    ) -> std::result::Result<String, String> {
        let probe = |m: &tflite::Model| -> Vec<f32> {
            let n: usize = m.input_shapes().first().map(|s| s.iter().product()).unwrap_or(0);
            (0..n).map(|i| (i as f32 * 0.0137).sin() * 0.8 + ((i / 3) % 5) as f32 * 0.04).collect()
        };
        self.detector.accelerate(accelerator.clone(), &probe(&self.detector), GPU_TOLERANCE, require_faster)?;
        let mesh = self.mesh.accelerate(accelerator.clone(), &probe(&self.mesh), GPU_TOLERANCE, require_faster);
        if let Some(far) = &self.far
            && let Err(why) = far.accelerate(accelerator, &probe(far), GPU_TOLERANCE, require_faster)
        {
            log::info!("faces: the full-range detector stays on the CPU: {why}");
        }
        Ok(match mesh {
            Ok(()) => "the detector and the landmarks".into(),
            Err(why) => format!("the detector (the landmarks stay on the CPU: {why})"),
        })
    }

    /// Stop using the GPU: everything runs on the CPU again.
    pub fn detach(&self) {
        self.detector.detach();
        self.mesh.detach();
        if let Some(far) = &self.far {
            far.detach();
        }
    }

    /// Whether the detector runs on an accelerator.
    pub fn accelerated(&self) -> bool {
        self.detector.accelerator().is_some()
    }

    /// Whether the full-range detector is there.
    pub fn has_far(&self) -> bool {
        self.far.is_some()
    }

    /// Small faces: BlazeFace full range on 192-pixel windows of `rgb` (`w × h`) at its own
    /// scale, overlapping by a quarter (it finds faces from about 8 to 100 pixels there: scale the
    /// photo so the faces wanted fall in that range). Pixels of `rgb`; empty without the
    /// full-range model.
    pub fn detect_far(&self, rgb: &[u8], w: usize, h: usize) -> Result<Vec<Detection>> {
        let Some(far) = &self.far else { return Ok(Vec::new()) };
        let n = mediapipe::FAR_INPUT;
        if w == 0 || h == 0 || rgb.len() < w.saturating_mul(h).saturating_mul(3) {
            return Ok(Vec::new());
        }
        let step = n * 3 / 4;
        let starts = |len: usize| {
            let mut v: Vec<usize> = (0..).map(|i| i * step).take_while(|s| s + n < len).collect();
            v.push(len.saturating_sub(n));
            v.dedup();
            v
        };
        use rayon::prelude::*;
        let windows: Vec<(usize, usize)> = starts(h).into_iter().flat_map(|y| starts(w).into_iter().map(move |x| (x, y))).collect();
        if far.accelerator().is_some() && windows.len() > 1 {
            let mut all = Vec::new();
            for chunk in windows.chunks(16) {
                let inputs: Vec<Vec<f32>> = chunk.par_iter().map(|&(x, y)| mediapipe::window(rgb, w, h, (x, y, n), n)).collect();
                let refs: Vec<&[f32]> = inputs.iter().map(Vec::as_slice).collect();
                let outs = far.run_batch(&refs)?;
                for (&(x, y), out) in chunk.iter().zip(&outs) {
                    let (Some(reg), Some(cls)) = (out.first(), out.get(1)) else {
                        return Err(Error::Model("BlazeFace gave no output".into()));
                    };
                    all.extend(mediapipe::decode_far(reg, cls, FAR_THRESHOLD).into_iter().map(|d| d.from_window(x as f32, y as f32, n as f32)));
                }
            }
            return Ok(mediapipe::merge_windows(all, 0.3));
        }
        // the windows are small: one each per thread does better than one at a time on all of them
        let found = windows
            .par_iter()
            .map(|&(x, y)| -> Result<Vec<Detection>> {
                let input = mediapipe::window(rgb, w, h, (x, y, n), n);
                let out = far.run(&[&input])?;
                let (Some(reg), Some(cls)) = (out.first(), out.get(1)) else { return Err(Error::Model("BlazeFace gave no output".into())) };
                Ok(mediapipe::decode_far(reg, cls, FAR_THRESHOLD).into_iter().map(|d| d.from_window(x as f32, y as f32, n as f32)).collect())
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(mediapipe::merge_windows(found.into_iter().flatten().collect(), 0.3))
    }

    /// The faces in an 8-bit RGB image (`w × h`), left to right, scoring at least [`FACE_THRESHOLD`].
    pub fn detect(&self, rgb: &[u8], w: usize, h: usize) -> Result<Vec<Detection>> {
        let b = mediapipe::BLAZEFACE_INPUT;
        let mut all = Vec::new();
        let windows = mediapipe::face_windows(w, h);
        let inputs: Vec<Vec<f32>> = windows.iter().map(|win| mediapipe::window(rgb, w, h, *win, b)).collect();
        let refs: Vec<&[f32]> = inputs.iter().map(Vec::as_slice).collect();
        // all the windows in one go: one submission on the GPU, one after another on the CPU
        let outs = self.detector.run_batch(&refs)?;
        for (win, out) in windows.iter().zip(&outs) {
            let (Some(reg), Some(cls)) = (out.first(), out.get(1)) else { return Err(Error::Model("BlazeFace gave no output".into())) };
            for d in mediapipe::decode_blazeface(reg, cls, FACE_THRESHOLD) {
                all.push(d.from_window(win.0 as f32, win.1 as f32, win.2 as f32));
            }
        }
        let mut faces = mediapipe::merge_windows(all, 0.3);
        faces.sort_by(|a, b| a.center()[0].total_cmp(&b.center()[0]));
        Ok(faces)
    }

    /// FaceMesh's landmarks for `det` (photo pixels); `None` when it doesn't see a face there.
    pub fn landmarks(&self, rgb: &[u8], w: usize, h: usize, det: &Detection) -> Result<Option<Vec<[f32; 2]>>> {
        let mut first = self.landmarks_in(rgb, w, h, &mediapipe::mesh_crop(det))?;
        // a face turned away: other framings before giving up
        for m in mediapipe::mesh_crop_retries(det) {
            if first.is_some() {
                break;
            }
            first = self.landmarks_in(rgb, w, h, &m)?;
        }
        let Some(first) = first else { return Ok(None) };
        // a second pass on the crop the face itself frames (the detector's box can be loose)
        match mediapipe::mesh_crop_from_landmarks(&first) {
            Some(m) => Ok(Some(self.landmarks_in(rgb, w, h, &m)?.unwrap_or(first))),
            None => Ok(Some(first)),
        }
    }

    /// FaceMesh on the crop `m` (photo → input) maps out.
    pub fn landmarks_in(&self, rgb: &[u8], w: usize, h: usize, m: &mediapipe::Affine) -> Result<Option<Vec<[f32; 2]>>> {
        let m = *m;
        let input = mediapipe::warp(rgb, w, h, &m, mediapipe::MESH_INPUT, 0.0, 1.0);
        let out = self.mesh.run(&[&input])?;
        let (Some(raw), Some(presence)) = (out.first(), out.get(1).and_then(|p| p.first())) else {
            return Err(Error::Model("FaceMesh gave no output".into()));
        };
        let p = 1.0 / (1.0 + (-presence.clamp(-80.0, 80.0)).exp());
        if p < 0.5 {
            return Ok(None);
        }
        Ok(mediapipe::landmarks_to_photo(raw, &m))
    }

    /// Every face in an 8-bit RGB image (`w × h`), left to right: [`FaceModel::detect`] for the
    /// people near the camera and, with the full-range detector, [`FaceModel::detect_far`] for
    /// the small faces of a group or a couple seen whole (on a copy at most [`FAR_EDGE`] pixels
    /// long). The same face found by both keeps its more confident detection.
    pub fn detect_all(&self, rgb: &[u8], w: usize, h: usize) -> Result<Vec<Detection>> {
        let mut all = self.detect(rgb, w, h)?;
        if self.far.is_some() && w > 0 && h > 0 {
            let f = (FAR_EDGE as f32 / w.max(h) as f32).min(1.0);
            let far = if f < 1.0 {
                let (sw, sh) = (((w as f32 * f).round() as usize).max(1), ((h as f32 * f).round() as usize).max(1));
                let small = mediapipe::box_resize(rgb, w, h, sw, sh);
                let k = 1.0 / f;
                self.detect_far(&small, sw, sh)?
                    .into_iter()
                    .map(|d| Detection {
                        x0: d.x0 * k,
                        y0: d.y0 * k,
                        x1: d.x1 * k,
                        y1: d.y1 * k,
                        score: d.score,
                        keypoints: d.keypoints.map(|p| [p[0] * k, p[1] * k]),
                    })
                    .collect()
            } else {
                self.detect_far(rgb, w, h)?
            };
            all.extend(far);
        }
        let mut faces = mediapipe::merge_windows(all, 0.3);
        faces.sort_by(|a, b| a.center()[0].total_cmp(&b.center()[0]));
        Ok(faces)
    }

    /// Faces with landmarks, left to right. A face only the full-range detector found, and only
    /// half sure of, that FaceMesh doesn't take either is left out (a pattern, not a face).
    pub fn faces(&self, rgb: &[u8], w: usize, h: usize) -> Result<Vec<Face>> {
        self.faces_confirmed(rgb, w, h, None)
    }

    /// [`FaceModel::faces`], also keeping a face FaceMesh doesn't take (a 3/4 view, a small face)
    /// when the class map `seen` (of the same image) has face skin or hair at its centre.
    pub fn faces_confirmed(&self, rgb: &[u8], w: usize, h: usize, seen: Option<&PeopleMap>) -> Result<Vec<Face>> {
        let faces: Vec<Face> = self
            .detect_all(rgb, w, h)?
            .into_iter()
            .map(|d| Ok(Face { landmarks: self.landmarks(rgb, w, h, &d)?, detection: d }))
            .collect::<Result<_>>()?;
        let (fw, fh) = (w.max(1) as f32, h.max(1) as f32);
        let face_there = |d: &Detection| {
            let [cx, cy] = d.center();
            seen.is_some_and(|m| m.at(&[1, 3], cx / fw, cy / fh) >= 0.5)
        };
        Ok(faces.into_iter().filter(|f| f.landmarks.is_some() || f.detection.score >= FACE_THRESHOLD_NO_MESH || face_there(&f.detection)).collect())
    }
}

/// What a People selection asks for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PeopleRequest {
    /// 0: everyone; n: the n-th person from the left (by their face).
    pub person: u32,
    /// Selfie Multiclass classes (indices into [`mediapipe::SELFIE_CLASSES`]).
    pub classes: Vec<usize>,
    pub face_parts: Vec<FacePart>,
}

impl PeopleRequest {
    /// From Lightroom's part names (`"Entire Person"`, `"Face Skin"`, `"Body Skin"`, `"Hair"`,
    /// `"Clothes"`, `"Eyebrows"`, `"Eye Sclera"`, `"Iris and Pupil"`, `"Lips"`, `"Teeth"`); none,
    /// or "Entire Person", is the whole person. `None` for a name it doesn't know.
    pub fn parse(person: u32, parts: &[String]) -> Option<PeopleRequest> {
        let mut r = PeopleRequest { person, ..Default::default() };
        let whole = [1, 2, 3, 4, 5];
        if parts.is_empty() {
            r.classes = whole.to_vec();
        }
        for p in parts {
            let n = p.to_ascii_lowercase();
            let class = match n.as_str() {
                "person" | "entire person" | "entireperson" => {
                    r.classes.extend(whole);
                    continue;
                }
                "hair" => 1,
                "body skin" | "bodyskin" => 2,
                "face skin" | "faceskin" => 3,
                "skin" => {
                    r.classes.extend([2, 3]);
                    continue;
                }
                "clothes" | "clothing" => 4,
                _ => {
                    r.face_parts.push(FacePart::parse(&n)?);
                    continue;
                }
            };
            r.classes.push(class);
        }
        r.classes.sort_unstable();
        r.classes.dedup();
        r.face_parts.dedup();
        Some(r)
    }

    /// The entire person (every class, or the request with no parts), face parts aside.
    pub fn is_whole_person(&self) -> bool {
        self.classes == [1, 2, 3, 4, 5]
    }

    pub fn needs_people(&self) -> bool {
        !self.classes.is_empty()
    }

    /// Face parts, or one person among several (told apart by their faces).
    pub fn needs_faces(&self) -> bool {
        !self.face_parts.is_empty() || self.person > 0
    }
}

/// A computed mask: `side²` logits over `rect` of the image (normalized; `None` = all of it).
pub struct MaskGrid {
    pub side: usize,
    pub logits: Vec<f32>,
    pub rect: Option<[f64; 4]>,
    /// Faces whose features couldn't be located (no landmarks: profiles), so face skin there
    /// still includes eyes and mouth (only hair is left out).
    pub faces_without_features: usize,
}

/// A class matte made from the 256-pixel class map, refined on the photo: specks the coarse map
/// leaves inside a person (lace, sequins: `fill`, for clothes and the entire person) filled, then
/// a guided filter on the photo's luminance at the grid's resolution, so the edge follows the
/// garment's or the body's own edge instead of the class map's blur.
fn refine(grid: &Grid, cov: &mut Vec<f32>, rgb: &[u8], fill: bool) {
    let side = grid.side;
    if fill {
        // at most 1/2000 of the grid: a 23-cell square at 1024
        matte::fill_small_holes(cov, side, (side * side / 2000).max(4));
    }
    if rgb.len() < grid.w.saturating_mul(grid.h).saturating_mul(3) || grid.w == 0 || grid.h == 0 {
        return;
    }
    let guide: Vec<f32> = (0..side * side)
        .map(|c| {
            let [x, y] = grid.centre(c % side, c / side);
            let (x, y) = ((x.max(0.0) as usize).min(grid.w - 1), (y.max(0.0) as usize).min(grid.h - 1));
            let p = rgb.get((y * grid.w + x) * 3..(y * grid.w + x) * 3 + 3).unwrap_or(&[0, 0, 0]);
            (0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2])) / 255.0
        })
        .collect();
    // a radius of about 1/170 of the grid (6 cells at 1024): wide enough to pull a 256-map's
    // blur onto the edge, narrow enough not to cross to the next person
    let r = (side / 170).max(2);
    *cov = matte::guided_filter(cov, &guide, side, side, r, 2e-3);
}

/// Selfie Multiclass's skin classes (body skin, face skin).
const SKIN: [usize; 2] = [2, 3];
/// What a garment never includes: hair, body skin, face skin.
const NOT_GARMENT: [usize; 3] = [1, 2, 3];
/// The classes that make a head: hair and face skin.
const HEAD: [usize; 2] = [1, 3];
/// A figure shows a head when at least this many of its cells are hair or face skin…
const HEAD_CELLS: usize = 4;
/// …and at least one in this many of them.
const HEAD_SHARE: usize = 40;

/// The photo's colours at the grid's cell centres (0..1, interleaved RGB), the guide of
/// [`matte::guided_filter_rgb`]; `None` when `rgb` isn't the grid's photo.
fn colour_guide(grid: &Grid, rgb: &[u8]) -> Option<Vec<f32>> {
    if grid.w == 0 || grid.h == 0 || rgb.len() < grid.w.saturating_mul(grid.h).saturating_mul(3) {
        return None;
    }
    let side = grid.side;
    Some(
        (0..side * side)
            .flat_map(|c| {
                let [x, y] = grid.centre(c % side, c / side);
                let (x, y) = ((x.max(0.0) as usize).min(grid.w - 1), (y.max(0.0) as usize).min(grid.h - 1));
                let p = rgb.get((y * grid.w + x) * 3..(y * grid.w + x) * 3 + 3).unwrap_or(&[0, 0, 0]);
                [f32::from(p[0]) / 255.0, f32::from(p[1]) / 255.0, f32::from(p[2]) / 255.0]
            })
            .collect(),
    )
}

/// A garment's coverage (`cov`, the grid's cells) cleaned up on the photo: the skin and hair of
/// the person wearing it taken out (the class map's, its edges pulled onto the photo's by a
/// colour guided filter — a neckline, bare arms and hands holding a bouquet aren't the dress),
/// then the garment's own edge pulled onto the photo's and steepened, so the outline is crisp
/// instead of a halo from a coarse model output stretched over the photo. `on_person`: the
/// garment is worn (skin and hair are cut only then).
fn clean_garment(grid: &Grid, cov: &mut Vec<f32>, rgb: &[u8], map: Option<&PeopleMap>, on_person: bool, surfaces: Option<&Surfaces>) {
    let side = grid.side;
    let Some(guide) = colour_guide(grid, rgb) else { return };
    // radius about 1/170 of the grid (6 cells at 1024), as for the class mattes
    let r = (side / 170).max(2);
    let rect = grid.rect;
    let uv = |c: usize| -> (f32, f32) {
        (
            (rect[0] + ((c % side) as f64 + 0.5) / side as f64 * (rect[2] - rect[0])) as f32,
            (rect[1] + ((c / side) as f64 + 0.5) / side as f64 * (rect[3] - rect[1])) as f32,
        )
    };
    // what the garment keeps of each cell: 1 − its wearer's skin and hair, 1 − the floor, wall or
    // table around them
    let mut keep = vec![1f32; side * side];
    if let Some(map) = map.filter(|_| on_person) {
        let mut excl = Vec::with_capacity(side * side);
        let (mut skin, mut cloth) = (Vec::with_capacity(side * side), Vec::with_capacity(side * side));
        for c in 0..side * side {
            let (u, v) = uv(c);
            excl.push(map.at(&NOT_GARMENT, u, v));
            skin.push(map.at(&SKIN, u, v));
            cloth.push(map.at(&[4], u, v));
        }
        // and the skin the class map takes for clothes (a bare arm below a tulle sleeve)
        let grown = skin_in_garment(side, cov, &guide, &skin, &cloth);
        for (e, g) in excl.iter_mut().zip(&grown) {
            *e = e.max(*g);
        }
        let excl = matte::guided_filter_rgb(&excl, &guide, side, side, r, 1e-3);
        for (k, e) in keep.iter_mut().zip(&excl) {
            *k *= 1.0 - matte::smoothstep(0.3, 0.6, *e);
        }
    }
    if let Some(s) = surfaces {
        let ground: Vec<f32> = (0..side * side).map(uv).map(|(u, v)| s.at(u, v)).collect();
        // (the scene model's grid is coarser still: onto the photo's edges first)
        let ground = matte::guided_filter_rgb(&ground, &guide, side, side, r, 1e-3);
        for (k, g) in keep.iter_mut().zip(&ground) {
            *k *= 1.0 - matte::smoothstep(SURFACE_CUT.0, SURFACE_CUT.1, *g);
        }
    }
    for (c, k) in cov.iter_mut().zip(&keep) {
        *c *= k;
    }
    // (a wider window for the garment's own edge, a 64th of the grid — 16 cells, four of SAM's
    // 256 — so it reaches past a halo of that width to the photo's edge)
    let refined = matte::guided_filter_rgb(cov, &guide, side, side, (side / 64).max(r), 1e-3);
    // steeper: the refined edge's soft tail (the halo) goes, the inside stays whole; and the wide
    // window doesn't carry the garment back onto the skin or the floor next to it (pale skin, a
    // pale floor and a white dress look alike to it)
    *cov = refined.into_iter().zip(&keep).map(|(v, k)| matte::smoothstep(0.25, 0.75, v) * k).collect();
}

/// Where a garment gives way to a surface the scene model is sure of (floor, ground, wall, table:
/// [`Surfaces`]): fully kept below the first value, gone above the second.
const SURFACE_CUT: (f32, f32) = (0.45, 0.75);

/// How far the strongest surface class must lead the strongest other class (a person, a boat, a
/// tent…) for the cell to count as a surface: not at all below the first margin, fully above the
/// second.
const SURFACE_WINS: (f32, f32) = (-0.05, 0.10);

/// Floors, grounds, walls, tables… (EoMT's COCO-panoptic "stuff" a garment is never part of), where
/// no person is: the probability, on the scene model's grid over the photo. A dress's hem and a
/// train touch the floor, and the people model's coarse map spills its "clothes" onto a pale floor
/// or a table in front of the bride; the scene model tells floor from tulle (it leaves a train on
/// the grass out of the grass).
pub struct Surfaces {
    width: usize,
    height: usize,
    probs: Vec<f32>,
}

/// The scene model's classes that are surfaces around people, never what they wear: floors,
/// grounds, walls, tables, and what people sit on (a dark chair back above a seated bride is not
/// her clothes: test notes 2026-10-25, note 12).
pub fn is_surface(label: &str) -> bool {
    const PREFIXES: [&str; 12] = ["floor", "pavement", "grass", "dirt", "road", "wall", "table", "counter", "ceiling", "building", "rug", "window"];
    const NAMES: [&str; 18] = [
        "chair",
        "couch",
        "bench",
        "dining table",
        "sand",
        "gravel",
        "snow",
        "playingfield",
        "platform",
        "railroad",
        "stairs",
        "fence-merged",
        "door-stuff",
        "sea",
        "river",
        "water-other",
        "house",
        "roof",
    ];
    PREFIXES.iter().any(|p| label.starts_with(p)) || NAMES.contains(&label)
}

impl Surfaces {
    /// From a scene segmentation: the surfaces' summed probability where the scene isn't a person
    /// and a surface is what the scene model sees there most. A large white gown that EoMT
    /// doesn't take for a person spreads its probability over boat, tent, water, wall… (a
    /// sum of surfaces can then pass the cut while no single one is more than a guess); a floor,
    /// a wall or a table is the clear winner where it is one.
    pub fn from_scene(scene: &crate::scene::Scene) -> Surfaces {
        let surface_ids = crate::scene::class_ids(is_surface);
        let surf = scene.class_probability(&surface_ids);
        let person = scene.class_probability(&crate::scene::class_ids(|l| l == "person"));
        let n = scene.width.saturating_mul(scene.height);
        let classes = scene.probs.len().checked_div(n).unwrap_or(0);
        // per cell: the strongest surface class and the strongest other class
        let mut best_surface = vec![0f32; n];
        let mut best_other = vec![0f32; n];
        for k in 0..classes {
            let Some(plane) = scene.probs.get(k * n..(k + 1) * n) else { break };
            let best = if surface_ids.contains(&k) { &mut best_surface } else { &mut best_other };
            for (b, p) in best.iter_mut().zip(plane) {
                if p.is_finite() {
                    *b = b.max(*p);
                }
            }
        }
        let probs = surf
            .iter()
            .zip(&person)
            .zip(best_surface.iter().zip(&best_other))
            .map(|((s, p), (bs, bo))| {
                let v = s * (1.0 - p) * matte::smoothstep(SURFACE_WINS.0, SURFACE_WINS.1, bs - bo);
                if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
            })
            .collect();
        Surfaces { width: scene.width, height: scene.height, probs }
    }

    /// From a `width × height` probability grid over the photo (tests, and a cached analysis).
    pub fn from_grid(width: usize, height: usize, probs: Vec<f32>) -> Surfaces {
        Surfaces { width, height, probs }
    }

    /// The probability grid: `(width, height, probabilities)` (for keeping it).
    pub fn grid(&self) -> (usize, usize, &[f32]) {
        (self.width, self.height, &self.probs)
    }

    /// The probability at normalized point `(u, v)` (bilinear; 0 for an empty or malformed grid).
    pub fn at(&self, u: f32, v: f32) -> f32 {
        let (w, h) = (self.width, self.height);
        if w == 0 || h == 0 || self.probs.len() < w.saturating_mul(h) || !(u.is_finite() && v.is_finite()) {
            return 0.0;
        }
        let fx = (u * w as f32 - 0.5).clamp(0.0, (w - 1) as f32);
        let fy = (v * h as f32 - 0.5).clamp(0.0, (h - 1) as f32);
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let p = |x: usize, y: usize| self.probs.get(y * w + x).copied().unwrap_or(0.0);
        ((p(x0, y0) * (1.0 - tx) + p(x1, y0) * tx) * (1.0 - ty) + (p(x0, y1) * (1.0 - tx) + p(x1, y1) * tx) * ty).clamp(0.0, 1.0)
    }
}

/// A cell's colour as a feature for telling skin from cloth: its chromaticity (red and green
/// shares) and a little of its log luminance (shading along an arm changes the brightness, not
/// the material).
fn colour_feature(p: &[f32; 3]) -> [f32; 3] {
    let [r, g, b] = *p;
    let s = (r + g + b).max(1e-3);
    [r / s, g / s, 0.03 * (s / 3.0 + 0.03).ln()]
}

fn feature_distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Each channel's median (`None` for no samples).
fn median_feature(samples: &mut [[f32; 3]]) -> Option<[f32; 3]> {
    if samples.is_empty() {
        return None;
    }
    let mid = samples.len() / 2;
    let mut out = [0f32; 3];
    for (k, o) in out.iter_mut().enumerate() {
        samples.select_nth_unstable_by(mid, |a, b| a[k].total_cmp(&b[k]));
        *o = samples.get(mid).map_or(0.0, |s| s[k]);
    }
    Some(out)
}

/// Blocks per side of the grid [`skin_in_garment`] learns the local colours on.
const SKIN_BLOCKS: usize = 32;
/// Skin and the garment around it must differ at least this much (feature distance) for colour to
/// tell them apart (a cream waistcoat next to a hand, an ivory bodice in tungsten light: no).
const SKIN_MIN_SEPARATION: f32 = 0.03;
/// A cell is skin-coloured when its distance to the skin is at most this share of the two
/// distances (to the skin, to the garment).
const SKIN_SHARE: f32 = 0.3;

/// The skin the class map calls clothes, on a `side²` grid (1 there, 0 elsewhere): a bare arm
/// below an off-shoulder tulle sleeve, a shoulder in low light. The skin and the garment's colours
/// are learnt around each part of the photo from the cells the class map is sure of (`skin`,
/// `cloth`: its probabilities; medians, so the misread arm itself doesn't count as the garment's
/// colour), and the class map's skin grows into the garment (`cov`) through the cells much nearer
/// the skin's colour than the garment's. `guide`: the cells' colours (interleaved RGB). Where skin
/// and garment look alike, nothing grows.
fn skin_in_garment(side: usize, cov: &[f32], guide: &[f32], skin: &[f32], cloth: &[f32]) -> Vec<f32> {
    let n = side.saturating_mul(side);
    let mut grown = vec![0f32; n];
    if side < SKIN_BLOCKS || cov.len() < n || guide.len() < n * 3 || skin.len() < n || cloth.len() < n {
        return grown;
    }
    let feats: Vec<[f32; 3]> = guide.as_chunks::<3>().0.iter().map(colour_feature).collect();
    let bs = side.div_ceil(SKIN_BLOCKS);
    // samples of each block: every other cell the class map is sure of
    let mut skin_s: Vec<Vec<[f32; 3]>> = vec![Vec::new(); SKIN_BLOCKS * SKIN_BLOCKS];
    let mut cloth_s: Vec<Vec<[f32; 3]>> = vec![Vec::new(); SKIN_BLOCKS * SKIN_BLOCKS];
    for y in (0..side).step_by(2) {
        for x in (0..side).step_by(2) {
            let c = y * side + x;
            let b = (y / bs).min(SKIN_BLOCKS - 1) * SKIN_BLOCKS + (x / bs).min(SKIN_BLOCKS - 1);
            let (Some(f), Some(sk), Some(cl), Some(cv)) = (feats.get(c), skin.get(c), cloth.get(c), cov.get(c)) else { continue };
            if *sk >= 0.7 {
                if let Some(s) = skin_s.get_mut(b) {
                    s.push(*f);
                }
            } else if *cl >= 0.7
                && *cv >= 0.5
                && let Some(s) = cloth_s.get_mut(b)
            {
                s.push(*f);
            }
        }
    }
    // each block's skin and garment colour: the medians over the 5 × 5 blocks around it (wider
    // where that holds too few of either: the middle of a long arm, far from the hand; at the
    // widest, the whole photo)
    use rayon::prelude::*;
    let models: Vec<Option<([f32; 3], [f32; 3])>> = (0..SKIN_BLOCKS * SKIN_BLOCKS)
        .into_par_iter()
        .map(|b| {
            let (bx, by) = (b % SKIN_BLOCKS, b / SKIN_BLOCKS);
            let gather = |r: usize| {
                let (mut s, mut g) = (Vec::new(), Vec::new());
                for y in by.saturating_sub(r)..(by + r + 1).min(SKIN_BLOCKS) {
                    for x in bx.saturating_sub(r)..(bx + r + 1).min(SKIN_BLOCKS) {
                        s.extend_from_slice(skin_s.get(y * SKIN_BLOCKS + x).map_or(&[][..], Vec::as_slice));
                        g.extend_from_slice(cloth_s.get(y * SKIN_BLOCKS + x).map_or(&[][..], Vec::as_slice));
                    }
                }
                (s, g)
            };
            let model = |(mut s, mut g): (Vec<[f32; 3]>, Vec<[f32; 3]>)| -> Option<([f32; 3], [f32; 3])> {
                // a handful of each at least
                if s.len() < 12 || g.len() < 12 {
                    return None;
                }
                let ms = median_feature(&mut s)?;
                let mut mg = median_feature(&mut g)?;
                // the garment's colour without the skin the class map mistook for it (an arm can
                // be a good part of the "clothes" around it): its samples nearer the garment
                for _ in 0..2 {
                    let mut kept: Vec<[f32; 3]> = g.iter().copied().filter(|f| feature_distance(*f, mg) <= feature_distance(*f, ms)).collect();
                    if kept.len() < 12 {
                        break;
                    }
                    let Some(m) = median_feature(&mut kept) else { break };
                    mg = m;
                }
                (feature_distance(ms, mg) >= SKIN_MIN_SEPARATION).then_some((ms, mg))
            };
            // wider while the skin and the garment can't be told apart there (an arm that is most
            // of the "clothes" near it, the garment itself a little further)
            [2, 4, 8, SKIN_BLOCKS].into_iter().find_map(|r| model(gather(r)))
        })
        .collect();
    let skin_coloured = |c: usize| -> bool {
        let (x, y) = (c % side, c / side);
        let b = (y / bs).min(SKIN_BLOCKS - 1) * SKIN_BLOCKS + (x / bs).min(SKIN_BLOCKS - 1);
        let (Some(Some((ms, mg))), Some(f)) = (models.get(b), feats.get(c)) else { return false };
        let (ds, dg) = (feature_distance(*f, *ms), feature_distance(*f, *mg));
        ds <= SKIN_SHARE * (ds + dg)
    };
    // grow from the class map's skin through skin-coloured garment cells
    let mut seen = vec![false; n];
    let mut q = std::collections::VecDeque::new();
    for c in 0..n {
        if skin.get(c).is_some_and(|s| *s >= 0.5) {
            seen[c] = true;
            q.push_back(c);
        }
    }
    while let Some(c) = q.pop_front() {
        for j in matte::neighbours(c, side) {
            if seen[j] || cov.get(j).is_none_or(|v| *v < 0.05) {
                continue;
            }
            seen[j] = true;
            if skin_coloured(j) {
                grown[j] = 1.0;
                q.push_back(j);
            }
        }
    }
    grown
}

/// Whether a selection (`cov` over `grid`) is mostly on a person: at least half of what it selects
/// is a person in the class map (a garment worn, a bouquet held), not a dress on a hanger.
fn worn(grid: &Grid, cov: &[f32], map: &PeopleMap) -> bool {
    let side = grid.side;
    let rect = grid.rect;
    let (mut on, mut all) = (0usize, 0usize);
    // every 4th cell is plenty
    for c in (0..side * side).step_by(4) {
        if cov.get(c).is_some_and(|v| *v > 0.5) {
            let u = (rect[0] + ((c % side) as f64 + 0.5) / side as f64 * (rect[2] - rect[0])) as f32;
            let v = (rect[1] + ((c / side) as f64 + 0.5) / side as f64 * (rect[3] - rect[1])) as f32;
            all += 1;
            if map.at(&[0], u, v) < 0.5 {
                on += 1;
            }
        }
    }
    all > 0 && on * 2 >= all
}

/// Side of the grid a garment found by a coarse model (SAM 2.1's 256-cell masks) is refined at.
pub const GARMENT_SIDE: usize = 1024;

/// A remembered garment (or any remembered object) found by SAM 2.1 — `logits`, `lside²` over the
/// stretched photo — refined on the photo (`rgb`, `w × h`, the image SAM saw) at
/// [`GARMENT_SIDE`] cells: specks in lace filled, the wearer's skin and hair cut out when it is
/// worn (`map`: the photo's class map, when the people model is installed), and the edge pulled
/// onto the photo's and made crisp. `None` for inputs of the wrong size or when nothing is left.
pub fn garment_mask(logits: &[f32], lside: usize, rgb: &[u8], w: usize, h: usize, map: Option<&PeopleMap>) -> Option<MaskGrid> {
    if lside == 0 || logits.len() != lside * lside || w == 0 || h == 0 {
        return None;
    }
    let side = GARMENT_SIDE;
    let grid = Grid { side, rect: [0.0, 0.0, 1.0, 1.0], w, h };
    let at = |x: usize, y: usize| logits.get(y.min(lside - 1) * lside + x.min(lside - 1)).copied().unwrap_or(-16.0);
    let mut cov: Vec<f32> = (0..side * side)
        .map(|c| {
            let fx = (((c % side) as f32 + 0.5) / side as f32 * lside as f32 - 0.5).clamp(0.0, (lside - 1) as f32);
            let fy = (((c / side) as f32 + 0.5) / side as f32 * lside as f32 - 0.5).clamp(0.0, (lside - 1) as f32);
            let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
            let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
            let l = (at(x0, y0) * (1.0 - tx) + at(x0 + 1, y0) * tx) * (1.0 - ty) + (at(x0, y0 + 1) * (1.0 - tx) + at(x0 + 1, y0 + 1) * tx) * ty;
            1.0 / (1.0 + (-l.clamp(-80.0, 80.0)).exp())
        })
        .collect();
    matte::fill_small_holes(&mut cov, side, (side * side / 2000).max(4));
    let on_person = map.is_some_and(|m| worn(&grid, &cov, m));
    clean_garment(&grid, &mut cov, rgb, map, on_person, None);
    if cov.iter().all(|c| *c < 0.5) {
        return None;
    }
    Some(MaskGrid { side, logits: mediapipe::coverage_to_logits(&cov), rect: None, faces_without_features: 0 })
}

/// Everyone's silhouettes (one per face, in the faces' order): `side²` logits each over the image
/// stretched to a square (SAM's output), and who owns each cell once cleaned up.
pub struct Instances {
    pub side: usize,
    pub people: Vec<Vec<f32>>,
    /// Each cell's owner ([`matte::NOBODY`] for no one), islands removed ([`matte::clean_owners`]).
    owners: Vec<u16>,
}

impl Instances {
    /// The silhouettes of the faces whose centres are `seeds` (normalized; same order as
    /// `people`). Whose a cell is goes to the strongest claim (logit above −1: a little generous,
    /// so a sleeve's fringe stays its owner's); a claim cut off from its face (a silhouette leaking
    /// onto a neighbour's lace, in square specks of SAM's grid) is dropped and the cell goes to
    /// whoever surrounds it.
    pub fn new(side: usize, people: Vec<Vec<f32>>, seeds: &[[f32; 2]]) -> Instances {
        let s = side.max(1);
        let mut owners: Vec<u16> = (0..side * side)
            .map(|c| {
                people
                    .iter()
                    .enumerate()
                    .filter_map(|(i, l)| l.get(c).map(|v| (i, *v)))
                    .filter(|(_, v)| *v > -1.0)
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .and_then(|(i, _)| u16::try_from(i).ok())
                    .unwrap_or(matte::NOBODY)
            })
            .collect();
        let cell = |p: &[f32; 2]| -> Option<usize> {
            if !p.iter().all(|v| v.is_finite()) {
                return None;
            }
            let x = ((p[0].clamp(0.0, 1.0) * s as f32) as usize).min(s - 1);
            let y = ((p[1].clamp(0.0, 1.0) * s as f32) as usize).min(s - 1);
            Some(y * s + x)
        };
        let seeds: Vec<Option<usize>> = seeds.iter().map(cell).collect();
        matte::clean_owners(&mut owners, side, &seeds);
        Instances { side, people, owners }
    }

    /// Everyone's claims from two sources, either of which may be missing: `silhouettes` (SAM 2.1,
    /// one per face, `side²` logits each) and `figures` (EoMT's people, `side²` logits each, found
    /// whether or not their face was). A figure that holds exactly one of `faces` (in a `w × h`
    /// photo) bounds that face's silhouette (or is their claim without one); a figure holding no
    /// face is a person of its own who claims what is theirs (a groom kissing in profile, a
    /// bystander cut by the frame), so it never goes to the others; a figure holding several faces
    /// (a couple EoMT sees as one) decides nothing. `None` when nothing claims anything.
    pub fn combine(side: usize, silhouettes: Option<Vec<Vec<f32>>>, figures: &[Vec<f32>], faces: &[Face], w: usize, h: usize) -> Option<Instances> {
        let n = side.saturating_mul(side);
        if n == 0 || w == 0 || h == 0 {
            return None;
        }
        let seeds: Vec<[f32; 2]> = faces.iter().map(|f| f.detection.center()).map(|[x, y]| [x / w as f32, y / h as f32]).collect();
        let cell = |p: [f32; 2]| -> Option<usize> {
            if !p.iter().all(|v| v.is_finite()) {
                return None;
            }
            let x = ((p[0].clamp(0.0, 1.0) * side as f32) as usize).min(side - 1);
            let y = ((p[1].clamp(0.0, 1.0) * side as f32) as usize).min(side - 1);
            Some(y * side + x)
        };
        let figures: Vec<&Vec<f32>> = figures.iter().filter(|f| f.len() == n).collect();
        // the faces on each figure (by the face's centre, or just below it: the chin)
        let on: Vec<Vec<usize>> = figures
            .iter()
            .map(|f| {
                faces
                    .iter()
                    .zip(&seeds)
                    .enumerate()
                    .filter(|(_, (face, [x, y]))| {
                        let s = face.detection.height() / h as f32;
                        [0.0, 0.5].iter().filter_map(|t| cell([*x, y + t * s])).any(|c| f.get(c).is_some_and(|l| *l > 0.0))
                    })
                    .map(|(k, _)| k)
                    .collect()
            })
            .collect();
        let floor = -16.0f32;
        let mut people: Vec<Vec<f32>> = match silhouettes {
            Some(s) if s.len() == faces.len() && s.iter().all(|l| l.len() == n) => s,
            _ => vec![vec![floor; n]; faces.len()],
        };
        let mut any = people.iter().any(|p| p.iter().any(|l| *l > -1.0));
        for (f, holds) in figures.iter().zip(&on) {
            match holds.as_slice() {
                [k] => {
                    let Some(p) = people.get_mut(*k) else { continue };
                    if p.iter().all(|l| *l <= floor) {
                        p.clone_from(f);
                    } else {
                        // the silhouette, but not where the figure clearly isn't (a little margin:
                        // the figure's outline is coarser than SAM's)
                        for (a, b) in p.iter_mut().zip(f.iter()) {
                            *a = a.min(*b + 4.0);
                        }
                    }
                    any = true;
                }
                [] => {
                    people.push((*f).clone());
                    any = true;
                }
                _ => {}
            }
        }
        any.then(|| Instances::new(side, people, &seeds))
    }

    /// Whose cell normalized point (u, v) is; `None` where no one's.
    pub fn owner(&self, u: f32, v: f32) -> Option<usize> {
        let s = self.side.max(1);
        let x = ((u.clamp(0.0, 1.0) * s as f32) as usize).min(s - 1);
        let y = ((v.clamp(0.0, 1.0) * s as f32) as usize).min(s - 1);
        self.owners.get(y * s + x).filter(|o| **o != matte::NOBODY).map(|o| usize::from(*o))
    }

    /// How sure person `i`'s silhouette is of the point `(u, v)` (normalized), 0..1.
    pub fn claim(&self, i: usize, u: f32, v: f32) -> f32 {
        let s = self.side.max(1);
        let x = ((u.clamp(0.0, 1.0) * s as f32) as usize).min(s - 1);
        let y = ((v.clamp(0.0, 1.0) * s as f32) as usize).min(s - 1);
        let l = self.people.get(i).and_then(|p| p.get(y * s + x)).copied().unwrap_or(f32::NEG_INFINITY);
        if l.is_finite() { 1.0 / (1.0 + (-l.clamp(-30.0, 30.0)).exp()) } else { 0.0 }
    }
}

/// The people of a class map as connected figures on a grid (cells where the map sees a person,
/// widened by a few cells for the soft fringe), and the faces standing on each.
struct Figures {
    label: Vec<u32>,
    /// The faces on each figure.
    faces_on: Vec<Vec<usize>>,
    /// The figure each face stands on.
    figure_of: Vec<Option<u32>>,
    /// Each figure shows a head (hair or face skin): a person even when their face wasn't found.
    head: Vec<bool>,
    /// Face centres (normalized) and the photo's aspect, for "nearest face" within a figure.
    centres: Vec<[f32; 2]>,
    aspect: f32,
}

impl Figures {
    fn new(grid: &Grid, map: &PeopleMap, faces: &[Face]) -> Option<Figures> {
        let side = grid.side;
        if side == 0 || grid.w == 0 || grid.h == 0 {
            return None;
        }
        let r = grid.rect;
        let norm = |i: usize, j: usize| -> (f32, f32) {
            ((r[0] + (i as f64 + 0.5) / side as f64 * (r[2] - r[0])) as f32, (r[1] + (j as f64 + 0.5) / side as f64 * (r[3] - r[1])) as f32)
        };
        let person: Vec<f32> = (0..side * side)
            .map(|c| {
                let (u, v) = norm(c % side, c / side);
                1.0 - map.at(&[0], u, v)
            })
            .collect();
        let (mut label, count) = matte::regions(&person, side, 0.3);
        // the fringe: unlabelled cells take a neighbour's figure, a few cells deep
        for _ in 0..(side / 256).max(2) {
            let snap = label.clone();
            for c in 0..side * side {
                if snap[c] != u32::MAX {
                    continue;
                }
                let (x, y) = (c % side, c / side);
                let near = [(x > 0).then(|| c - 1), (x + 1 < side).then(|| c + 1), (y > 0).then(|| c - side), (y + 1 < side).then(|| c + side)];
                if let Some(l) = near.into_iter().flatten().map(|n| snap[n]).find(|l| *l != u32::MAX) {
                    label[c] = l;
                }
            }
        }
        let (fw, fh) = (grid.w as f32, grid.h as f32);
        let cell_of = |x: f32, y: f32| -> Option<usize> {
            let (u, v) = (x / fw, y / fh);
            if !(u.is_finite() && v.is_finite()) {
                return None;
            }
            let i = ((u as f64 - r[0]) / (r[2] - r[0]) * side as f64).floor();
            let j = ((v as f64 - r[1]) / (r[3] - r[1]) * side as f64).floor();
            (i >= 0.0 && j >= 0.0 && (i as usize) < side && (j as usize) < side).then(|| j as usize * side + i as usize)
        };
        let mut faces_on = vec![Vec::new(); count];
        let mut figure_of = Vec::with_capacity(faces.len());
        for (k, f) in faces.iter().enumerate() {
            let d = &f.detection;
            let [cx, cy] = d.center();
            let s = d.height().max(d.width());
            // the face, the chin, the neck: whichever the map sees as the person first
            let fig = [0.0, 0.5, 1.2].iter().filter_map(|t| cell_of(cx, cy + t * s)).map(|c| label[c]).find(|l| *l != u32::MAX);
            if let Some(l) = fig
                && let Some(on) = faces_on.get_mut(l as usize)
            {
                on.push(k);
            }
            figure_of.push(fig);
        }
        // a head on each figure: hair or face skin over a share of it (a person turned away, or whose
        // face the detector missed, still has one; a clothes-coloured patch of grass or brick hasn't)
        let (mut cells, mut heads) = (vec![0usize; count], vec![0usize; count]);
        for (c, l) in label.iter().enumerate() {
            let Some(n) = cells.get_mut(*l as usize) else { continue };
            *n += 1;
            let (u, v) = norm(c % side, c / side);
            if map.at(&HEAD, u, v) > 0.5
                && let Some(k) = heads.get_mut(*l as usize)
            {
                *k += 1;
            }
        }
        let head = cells.iter().zip(&heads).map(|(n, k)| *k >= HEAD_CELLS.max(n / HEAD_SHARE)).collect();
        let centres = faces.iter().map(|f| f.detection.center()).map(|[x, y]| [x / fw, y / fh]).collect();
        Some(Figures { label, faces_on, figure_of, head, centres, aspect: fw / fh })
    }

    /// Whether every face of `chosen` stands on a figure (else the figures can't be trusted for
    /// them: a face the class map doesn't see as a person).
    fn placed(&self, chosen: &[usize]) -> bool {
        !chosen.is_empty() && chosen.iter().all(|k| self.figure_of.get(*k).is_some_and(Option::is_some))
    }

    fn faces_at(&self, cell: usize) -> &[usize] {
        match self.label.get(cell).filter(|l| **l != u32::MAX) {
            Some(l) => self.faces_on.get(*l as usize).map_or(&[], Vec::as_slice),
            None => &[],
        }
    }

    /// Whether `cell` is on a figure that no face stands on (a clothes-coloured patch of grass,
    /// someone whose face wasn't found): nobody's. Off every figure is not (the class map fades
    /// before a forehead's hairline; the face's own rules decide there).
    fn faceless(&self, cell: usize) -> bool {
        self.label.get(cell).is_some_and(|l| *l != u32::MAX) && self.faces_at(cell).is_empty()
    }

    /// Whether `cell` is on a figure that is nobody at all: no face stands on it and it shows no
    /// head ([`Figures::faceless`] less the people whose face wasn't found).
    fn nobody(&self, cell: usize) -> bool {
        self.faceless(cell) && self.label.get(cell).is_some_and(|l| !self.head.get(*l as usize).copied().unwrap_or(false))
    }

    /// Whether `cell` is on any figure.
    fn on_figure(&self, cell: usize) -> bool {
        self.label.get(cell).is_some_and(|l| *l != u32::MAX)
    }

    /// Whose `cell` (at normalized `(u, v)`) is: the nearest face on its figure; `None` off every
    /// figure or on one without a face.
    fn owner(&self, cell: usize, u: f32, v: f32) -> Option<usize> {
        self.faces_at(cell)
            .iter()
            .filter_map(|k| self.centres.get(*k).map(|c| (*k, c)))
            .map(|(k, c)| {
                let (dx, dy) = ((u - c[0]) * self.aspect, (v - c[1]) * 0.6);
                (k, dx * dx + dy * dy)
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(k, _)| k)
    }
}

/// Take every face out of a garment's coverage (`cov` over `grid`): an ellipse over each face's
/// box (its landmarks' outline when FaceMesh placed them), from the hairline to the chin, with a
/// soft rim. Clothing is never a face.
fn cut_faces(grid: &Grid, cov: &mut [f32], faces: &[Face]) {
    let side = grid.side;
    if side == 0 || grid.w == 0 || grid.h == 0 || cov.len() < side * side {
        return;
    }
    let r = grid.rect;
    let (fw, fh) = (grid.w as f64, grid.h as f64);
    for f in faces {
        let d = &f.detection;
        // the face's own outline when there is one, else the detector's box
        let (x0, y0, x1, y1) = match &f.landmarks {
            Some(lm) => {
                let oval = mediapipe::FACE_OVAL.iter().filter_map(|i| lm.get(*i)).filter(|p| p.iter().all(|v| v.is_finite()));
                oval.fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |(a, b, c, e), p| (a.min(p[0]), b.min(p[1]), c.max(p[0]), e.max(p[1])))
            }
            None => (d.x0, d.y0 - 0.1 * d.height(), d.x1, d.y1),
        };
        if !(x1 > x0 && y1 > y0) {
            continue;
        }
        let (cx, cy) = (f64::from(x0 + x1) / 2.0, f64::from(y0 + y1) / 2.0);
        let (rx, ry) = (f64::from(x1 - x0) * 0.55, f64::from(y1 - y0) * 0.55);
        // the cells the ellipse (and its rim) can touch
        let to_i = |x: f64| ((x / fw - r[0]) / (r[2] - r[0]) * side as f64).floor();
        let to_j = |y: f64| ((y / fh - r[1]) / (r[3] - r[1]) * side as f64).floor();
        let (i0, i1) = (to_i(cx - rx * 1.3).max(0.0) as usize, (to_i(cx + rx * 1.3) + 1.0).clamp(0.0, side as f64) as usize);
        let (j0, j1) = (to_j(cy - ry * 1.3).max(0.0) as usize, (to_j(cy + ry * 1.3) + 1.0).clamp(0.0, side as f64) as usize);
        for j in j0..j1.min(side) {
            for i in i0..i1.min(side) {
                let x = (r[0] + (i as f64 + 0.5) / side as f64 * (r[2] - r[0])) * fw;
                let y = (r[1] + (j as f64 + 0.5) / side as f64 * (r[3] - r[1])) * fh;
                let e = (((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2)).sqrt() as f32;
                let keep = matte::smoothstep(1.0, 1.25, e);
                if let Some(c) = cov.get_mut(j * side + i) {
                    *c *= keep;
                }
            }
        }
    }
}

/// The People selection `req` on an 8-bit RGB image (`w × h`), from the Selfie Multiclass map and
/// the faces (left to right) its parts need. `None`: nothing of it is there (no such person, no
/// face for face parts).
pub fn people_mask(req: &PeopleRequest, rgb: &[u8], w: usize, h: usize, map: Option<&PeopleMap>, faces: &[Face]) -> Option<MaskGrid> {
    people_mask_with(req, rgb, w, h, map, faces, None, None)
}

/// [`people_mask`] with each person's silhouette (`instances`) deciding whose a pixel is, before
/// "nearest face" (which gives an arm around someone, or a hand on them, to them); and, for
/// Clothes, the floors, walls and tables around them (`surfaces`, from the scene model) never
/// part of the garment.
#[allow(clippy::too_many_arguments)]
pub fn people_mask_with(
    req: &PeopleRequest,
    rgb: &[u8],
    w: usize,
    h: usize,
    map: Option<&PeopleMap>,
    faces: &[Face],
    instances: Option<&Instances>,
    surfaces: Option<&Surfaces>,
) -> Option<MaskGrid> {
    if w == 0 || h == 0 {
        return None;
    }
    // the faces it is about: everyone's, or the n-th from the left
    let chosen: Vec<usize> = match req.person {
        0 => (0..faces.len()).collect(),
        n => vec![(n as usize).checked_sub(1).filter(|i| *i < faces.len())?],
    };
    let aspect = w as f32 / h as f32;
    // a pixel belongs to the person whose face is nearest (distances in photo proportions)
    let owner = |u: f32, v: f32| -> Option<usize> {
        faces
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let [cx, cy] = f.detection.center();
                let (dx, dy) = ((u - cx / w as f32) * aspect, (v - cy / h as f32) * 0.6);
                (i, dx * dx + dy * dy)
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };
    let mut rect = None;
    let side;
    // the entire person: everything that isn't background (eyes, mouth, hair, clothes, what they
    // hold), with nothing cut out of the face
    let whole = req.is_whole_person();
    let skin = !whole && req.classes.iter().any(|c| SKIN.contains(c));
    if req.needs_people() {
        // the whole image: classes cover bodies; finer when face features are drawn or cut out
        side = 1024;
    } else {
        // only face parts: a grid over the chosen faces
        let boxes: Vec<&Detection> = chosen.iter().filter_map(|i| faces.get(*i)).map(|f| &f.detection).collect();
        if boxes.is_empty() {
            return None;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for b in boxes {
            let m = 0.3 * b.width().max(b.height());
            (x0, y0, x1, y1) = (x0.min(b.x0 - m), y0.min(b.y0 - m), x1.max(b.x1 + m), y1.max(b.y1 + m));
        }
        let r = [
            f64::from((x0 / w as f32).clamp(0.0, 1.0)),
            f64::from((y0 / h as f32).clamp(0.0, 1.0)),
            f64::from((x1 / w as f32).clamp(0.0, 1.0)),
            f64::from((y1 / h as f32).clamp(0.0, 1.0)),
        ];
        if !(r[2] > r[0] && r[3] > r[1]) {
            return None;
        }
        rect = Some(r);
        side = 768;
    }
    let grid = Grid { side, rect: rect.unwrap_or([0.0, 0.0, 1.0, 1.0]), w, h };
    let mut cov = vec![0f32; side * side];
    // skin never includes the face's features: eyes, brows, mouth, facial hair (of every face,
    // whoever the mask is for)
    let mut holes = vec![0f32; if skin { side * side } else { 0 }];
    let mut faces_without_features = 0;
    if skin {
        for f in faces {
            match &f.landmarks {
                Some(lm) => mediapipe::feature_holes(&grid, &mut holes, lm, rgb),
                None => faces_without_features += 1,
            }
        }
        holes = mediapipe::soften(&holes, side);
    }
    let (skin_classes, other_classes): (Vec<usize>, Vec<usize>) = req.classes.iter().partition(|c| SKIN.contains(c));
    // skin and the other classes apart: the skin is refined on its own, then the features are cut
    // out of it (refining after the cut filled the eye and brow holes back in)
    let mut skin_cov = vec![0f32; if skin { side * side } else { 0 }];
    let mut mine = vec![true; if skin { side * side } else { 0 }];
    // the people as the class map's connected figures, each holding the faces on it: whose a pixel
    // is never crosses to a figure the person isn't part of (a bystander at the frame's edge),
    // and a figure with no face on it (a clothes-coloured patch of grass or brick) is nobody's
    let figures = map.filter(|_| req.needs_people() && !faces.is_empty()).and_then(|m| Figures::new(&grid, m, faces));
    if let Some(map) = map.filter(|_| req.needs_people()) {
        let r = grid.rect;
        // a garment of one person of several: where their silhouette says the pixel is theirs,
        // what the class map splits between clothes and background (tulle, a sheer train showing
        // the grass) counts as clothes
        let sheer = req.person > 0 && other_classes.contains(&4) && instances.is_some();
        for j in 0..side {
            for i in 0..side {
                let u = (r[0] + (i as f64 + 0.5) / side as f64 * (r[2] - r[0])) as f32;
                let v = (r[1] + (j as f64 + 0.5) / side as f64 * (r[3] - r[1])) as f32;
                let cell = j * side + i;
                let silhouette = instances.and_then(|s| s.owner(u, v));
                let theirs = |o: Option<usize>| o.is_some_and(|o| chosen.contains(&o));
                let allowed = if req.person > 0 {
                    match (&figures, silhouette) {
                        (_, Some(o)) => theirs(Some(o)),
                        (Some(f), None) if f.placed(&chosen) && f.on_figure(cell) => theirs(f.owner(cell, u, v)),
                        (Some(f), None) if f.faceless(cell) => false,
                        _ => theirs(owner(u, v)),
                    }
                } else {
                    // everyone: the whole person keeps every figure (a back turned to the camera); the
                    // parts keep every figure with a face or a head on it (someone standing whose face
                    // wasn't found is still someone: test notes 2026-10-25, note 12)
                    whole || figures.as_ref().is_none_or(|f| !f.nobody(cell))
                };
                if !allowed {
                    if let Some(m) = mine.get_mut(cell) {
                        *m = false;
                    }
                    continue;
                }
                let value = if whole {
                    let person = 1.0 - map.at(&[0], u, v);
                    // one person of several: their own silhouette where the class map, seeing the
                    // whole photo at once, is too coarse for a small figure (a group of 29)
                    match silhouette.filter(|o| req.person > 0 && chosen.contains(o)) {
                        Some(o) => person.max(instances.map_or(0.0, |s| s.claim(o, u, v))),
                        None => person,
                    }
                } else {
                    if let Some(s) = skin_cov.get_mut(cell) {
                        *s = map.at(&skin_classes, u, v);
                    }
                    if other_classes.is_empty() {
                        0.0
                    } else {
                        let p = map.at(&other_classes, u, v);
                        let bg = map.at(&[0], u, v);
                        if sheer && theirs(silhouette) && p >= 0.15 && bg < 0.8 { (p / (1.0 - bg).max(0.05)).min(1.0) } else { p }
                    }
                };
                if let Some(c) = cov.get_mut(j * side + i) {
                    *c = value.clamp(0.0, 1.0);
                }
            }
        }
    }
    if map.is_some() && req.needs_people() {
        if whole || !other_classes.is_empty() {
            refine(&grid, &mut cov, rgb, whole || req.classes.contains(&4));
        }
        if !whole && req.classes == [4] {
            // Clothes alone: the class map's blur leaks onto a neckline, arms and hands
            clean_garment(&grid, &mut cov, rgb, map, true, surfaces);
            // and never on a face, whoever's (strong Texture or Clarity there ages a face)
            cut_faces(&grid, &mut cov, faces);
        }
        if skin {
            // the face skin up to the hairline (the class map fades out on the upper forehead)
            for (k, f) in faces.iter().enumerate() {
                if req.person > 0 && !chosen.contains(&k) {
                    continue;
                }
                if let (Some(lm), Some(map)) = (&f.landmarks, map) {
                    mediapipe::forehead(&grid, &mut skin_cov, lm, rgb, |u, v| map.at(&[1], u, v));
                }
            }
            refine(&grid, &mut skin_cov, rgb, false);
            for (k, c) in cov.iter_mut().enumerate() {
                let own = mine.get(k).copied().unwrap_or(true);
                let s = skin_cov.get(k).copied().unwrap_or(0.0) * (1.0 - holes.get(k).copied().unwrap_or(0.0));
                if own {
                    *c = (*c + s).clamp(0.0, 1.0);
                }
            }
        }
    }
    for i in &chosen {
        let Some(lm) = faces.get(*i).and_then(|f| f.landmarks.as_ref()) else { continue };
        for part in &req.face_parts {
            mediapipe::rasterize(&grid, &mut cov, lm, *part, rgb);
        }
    }
    if cov.iter().all(|c| *c < 0.5) {
        return None;
    }
    Some(MaskGrid { side, logits: mediapipe::coverage_to_logits(&cov), rect, faces_without_features })
}

impl From<lcc_tflite::Error> for Error {
    fn from(e: lcc_tflite::Error) -> Self {
        Error::Model(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Clothes mask at photo pixel `(x, y)` of a 200 × 200 photo.
    fn clothes_at(m: &MaskGrid, x: usize, y: usize) -> f32 {
        let c = (y * m.side / 200) * m.side + x * m.side / 200;
        1.0 / (1.0 + (-m.logits[c]).exp())
    }

    #[test]
    fn overlapping_people_go_to_the_strongest_silhouette() {
        // two people on a 4² grid: the left one's silhouette covers the left half and reaches into
        // the right one's, who claims that overlap more strongly
        let side = 4;
        let left: Vec<f32> = (0..16).map(|i| if i % 4 < 3 { 2.0 } else { -5.0 }).collect();
        let right: Vec<f32> = (0..16).map(|i| if i % 4 >= 2 { 6.0 } else { -5.0 }).collect();
        let s = Instances::new(side, vec![left, right], &[]);
        assert_eq!(s.owner(0.1, 0.5), Some(0));
        assert_eq!(s.owner(0.6, 0.5), Some(1), "the overlap is the stronger claim's");
        assert_eq!(s.owner(0.9, 0.5), Some(1));
        // nobody's: nearest face decides (None here); out-of-range and empty are safe
        let none = Instances::new(side, vec![vec![-5.0; 16]], &[]);
        assert_eq!(none.owner(0.5, 0.5), None);
        assert_eq!(Instances::new(0, vec![Vec::new()], &[]).owner(2.0, f32::NAN), None);
    }

    #[test]
    fn requests_parse_lightrooms_part_names() {
        let r = PeopleRequest::parse(0, &[]).unwrap();
        assert_eq!(r.classes, vec![1, 2, 3, 4, 5]);
        assert!(!r.needs_faces());
        let r = PeopleRequest::parse(2, &["Hair".into(), "Lips".into(), "Iris and Pupil".into()]).unwrap();
        assert_eq!(r.classes, vec![1]);
        assert_eq!(r.face_parts, vec![FacePart::Lips, FacePart::Iris]);
        assert!(r.needs_faces() && r.needs_people());
        assert!(PeopleRequest::parse(0, &["Elbows".into()]).is_none());
    }

    fn map_left_half_hair() -> PeopleMap {
        let side = 16;
        let mut probs = vec![0f32; side * side * 6];
        for y in 0..side {
            for x in 0..side {
                let c = if x < side / 2 { 1 } else { 0 };
                probs[(y * side + x) * 6 + c] = 1.0;
            }
        }
        PeopleMap { side, probs }
    }

    #[test]
    fn masks_follow_the_classes_and_the_chosen_person() {
        let map = map_left_half_hair();
        assert!((map.person_share() - 0.5).abs() < 1e-6);
        let req = PeopleRequest::parse(0, &["Hair".into()]).unwrap();
        let m = people_mask(&req, &[], 100, 100, Some(&map), &[]).unwrap();
        assert_eq!(m.side, 1024);
        assert!(m.logits[512 * 1024 + 100] > 4.0 && m.logits[512 * 1024 + 900] < -4.0);
        // two faces: person 2 (the right one) owns the right half, where there is no hair
        let face = |x: f32| Face {
            detection: Detection { x0: x - 5.0, y0: 10.0, x1: x + 5.0, y1: 20.0, score: 1.0, keypoints: [[0.0; 2]; 6] },
            landmarks: None,
        };
        let faces = [face(25.0), face(75.0)];
        assert!(people_mask(&PeopleRequest::parse(2, &["Hair".into()]).unwrap(), &[], 100, 100, Some(&map), &faces).is_none());
        let m = people_mask(&PeopleRequest::parse(1, &["Hair".into()]).unwrap(), &[], 100, 100, Some(&map), &faces).unwrap();
        assert!(m.logits[512 * 1024 + 100] > 4.0);
        // no such person, or face parts without landmarks: nothing
        assert!(people_mask(&PeopleRequest::parse(3, &["Hair".into()]).unwrap(), &[], 100, 100, Some(&map), &faces).is_none());
        assert!(people_mask(&PeopleRequest::parse(0, &["Lips".into()]).unwrap(), &[], 100, 100, None, &faces).is_none());
        assert!(people_mask(&req, &[], 0, 0, Some(&map), &[]).is_none());
    }

    /// A synthetic face filling a 1000 × 1000 photo: oval, eyes, brows and lips as rings of landmarks.
    fn synthetic_face() -> Vec<[f32; 2]> {
        let mut lm = vec![[500.0f32, 500.0]; mediapipe::MESH_POINTS];
        let ring = |idx: &[usize], cx: f32, cy: f32, rx: f32, ry: f32, lm: &mut Vec<[f32; 2]>| {
            for (k, i) in idx.iter().enumerate() {
                let a = k as f32 / idx.len() as f32 * std::f32::consts::TAU;
                lm[*i] = [cx + rx * a.cos(), cy + ry * a.sin()];
            }
        };
        ring(&mediapipe::FACE_OVAL, 500.0, 500.0, 300.0, 400.0, &mut lm);
        ring(&mediapipe::EYES[0], 380.0, 400.0, 50.0, 18.0, &mut lm);
        ring(&mediapipe::EYES[1], 620.0, 400.0, 50.0, 18.0, &mut lm);
        for (e, (c, rim)) in mediapipe::IRISES.iter().enumerate() {
            let cx = if e == 0 { 380.0 } else { 620.0 };
            lm[*c] = [cx, 400.0];
            ring(rim, cx, 400.0, 12.0, 12.0, &mut lm);
        }
        for (b, (upper, lower)) in mediapipe::BROWS.iter().enumerate() {
            let cx = if b == 0 { 380.0 } else { 620.0 };
            for (k, i) in upper.iter().enumerate() {
                lm[*i] = [cx - 60.0 + 30.0 * k as f32, 335.0];
            }
            for (k, i) in lower.iter().enumerate() {
                lm[*i] = [cx - 60.0 + 30.0 * k as f32, 350.0];
            }
        }
        ring(&mediapipe::LIPS_OUTER, 500.0, 680.0, 90.0, 35.0, &mut lm);
        ring(&mediapipe::LIPS_INNER, 500.0, 680.0, 60.0, 12.0, &mut lm);
        lm[2] = [500.0, 590.0];
        lm[205] = [360.0, 560.0];
        lm[425] = [640.0, 560.0];
        lm
    }

    fn face_skin_everywhere(side: usize) -> PeopleMap {
        let mut probs = vec![0f32; side * side * 6];
        for px in probs.chunks_mut(6) {
            px[3] = 1.0;
        }
        PeopleMap { side, probs }
    }

    /// Regression (the user's bug): "Entire person" took face skin's cut-outs and lost the eyes,
    /// brows and mouth; and the 256-cell class map's specks of background in a lace dress showed
    /// as square holes. The entire person includes every feature of the face and has no specks.
    #[test]
    fn entire_person_keeps_the_eyes_and_mouth_and_has_no_square_holes() {
        let lm = synthetic_face();
        let rgb = vec![150u8; 1000 * 1000 * 3];
        let det = Detection { x0: 200.0, y0: 100.0, x1: 800.0, y1: 900.0, score: 1.0, keypoints: [[0.0; 2]; 6] };
        let faces = [Face { detection: det, landmarks: Some(lm) }];
        // face skin above, clothes below with a few one-cell specks of background (lace)
        let side = 64;
        let mut probs = vec![0f32; side * side * 6];
        for y in 0..side {
            for x in 0..side {
                let c = if [(20, 50), (40, 55), (33, 60)].contains(&(x, y)) {
                    0
                } else if y < 45 {
                    3
                } else {
                    4
                };
                probs[(y * side + x) * 6 + c] = 1.0;
            }
        }
        let map = PeopleMap { side, probs };
        for parts in [vec![], vec!["Entire Person".to_string()]] {
            let req = PeopleRequest::parse(1, &parts).unwrap();
            assert!(req.is_whole_person());
            let m = people_mask(&req, &rgb, 1000, 1000, Some(&map), &faces).unwrap();
            let at = |x: usize, y: usize| m.logits[(y * m.side / 1000) * m.side + x * m.side / 1000];
            for (what, x, y) in
                [("eye", 380, 400), ("iris", 620, 400), ("brow", 380, 342), ("lips", 500, 650), ("mouth", 500, 680), ("cheek", 300, 560)]
            {
                assert!(at(x, y) > 2.0, "{what} is part of the entire person: {}", at(x, y));
            }
            // the specks (cells of 1000/64 pixels) are filled
            for (x, y) in [(20, 50), (40, 55), (33, 60)] {
                let (px, py) = ((x * 1000 + 500) / side, (y * 1000 + 500) / side);
                assert!(at(px, py) > 0.0, "no hole at {px},{py}: {}", at(px, py));
            }
        }
        // face skin still leaves the eyes out
        let m = people_mask(&PeopleRequest::parse(1, &["Face Skin".into()]).unwrap(), &rgb, 1000, 1000, Some(&map), &faces).unwrap();
        assert!(m.logits[(400 * m.side / 1000) * m.side + 380 * m.side / 1000] < -2.0);
    }

    /// Face skin has holes where the eyes, brows and mouth are, and keeps the cheeks and forehead;
    /// hair inside the face (the model's hair class) is not skin; body skin loses the features too.
    #[test]
    fn face_skin_leaves_the_features_out() {
        let lm = synthetic_face();
        let rgb = vec![150u8; 1000 * 1000 * 3];
        let det = Detection { x0: 200.0, y0: 100.0, x1: 800.0, y1: 900.0, score: 1.0, keypoints: [[0.0; 2]; 6] };
        let faces = [Face { detection: det, landmarks: Some(lm) }];
        let map = face_skin_everywhere(64);
        let req = PeopleRequest::parse(0, &["Face Skin".into()]).unwrap();
        let m = people_mask(&req, &rgb, 1000, 1000, Some(&map), &faces).unwrap();
        assert_eq!((m.side, m.faces_without_features), (1024, 0));
        let at = |x: usize, y: usize| m.logits[(y * 1024 / 1000) * 1024 + x * 1024 / 1000];
        for (what, x, y) in
            [("eye", 380, 400), ("iris", 620, 400), ("eye corner (lashes)", 425, 405), ("brow", 380, 342), ("lips", 500, 650), ("mouth", 500, 680)]
        {
            assert!(at(x, y) < -2.0, "{what} is not skin: {}", at(x, y));
        }
        for (what, x, y) in [("cheek", 300, 560), ("forehead", 500, 220), ("chin", 500, 800)] {
            assert!(at(x, y) > 2.0, "{what} is skin: {}", at(x, y));
        }
        // soft edges: something between in and out next to the eye
        let edge = (300..400).map(|y| at(380, y)).any(|l| l.abs() < 2.0);
        assert!(edge, "feathered edge");
        // the same holes in body skin
        let body = PeopleMap { side: 64, probs: map.probs.iter().enumerate().map(|(i, _)| if i % 6 == 2 { 1.0 } else { 0.0 }).collect() };
        let req = PeopleRequest::parse(0, &["Body Skin".into()]).unwrap();
        let m2 = people_mask(&req, &rgb, 1000, 1000, Some(&body), &faces).unwrap();
        assert!(m2.logits[(400 * 1024 / 1000) * 1024 + 380 * 1024 / 1000] < -2.0);
        // hair over the face (here its left half) is not face skin
        let mut probs = map.probs.clone();
        for y in 0..64 {
            for x in 0..32 {
                let i = (y * 64 + x) * 6;
                probs[i + 3] = 0.0;
                probs[i + 1] = 1.0;
            }
        }
        let hair = PeopleMap { side: 64, probs };
        let m3 = people_mask(&PeopleRequest::parse(0, &["Face Skin".into()]).unwrap(), &rgb, 1000, 1000, Some(&hair), &faces).unwrap();
        assert!(m3.logits[(560 * 1024 / 1000) * 1024 + 300 * 1024 / 1000] < -2.0, "hair over the cheek is not skin");
        // a face without landmarks (profile): nothing cut, and the result says so
        let profile = [Face { detection: faces[0].detection.clone(), landmarks: None }];
        let m4 = people_mask(&PeopleRequest::parse(0, &["Face Skin".into()]).unwrap(), &rgb, 1000, 1000, Some(&map), &profile).unwrap();
        assert_eq!(m4.faces_without_features, 1);
        // asking for skin and lips: lips come back on top of the skin's hole
        let m5 = people_mask(&PeopleRequest::parse(0, &["Face Skin".into(), "Lips".into()]).unwrap(), &rgb, 1000, 1000, Some(&map), &faces).unwrap();
        assert!(m5.logits[(650 * 1024 / 1000) * 1024 + 500 * 1024 / 1000] > 2.0);
    }

    /// Regression (the user's bug, a man and a woman at a dinner table): on real faces the eyes,
    /// brows and lips were tinted — the edge refinement ran after the features were cut and
    /// filled the small holes back in, and a smiling eye's opening is too thin a sliver to cut
    /// anyway — and the man's upper forehead was left out where the class map fades before the
    /// hairline. A small face (eyes 20 pixels wide, squinting): the eyes, brows and lips are 0,
    /// the forehead up to the hair is skin, the hair isn't.
    #[test]
    fn small_smiling_faces_lose_eyes_and_lips_and_keep_the_forehead() {
        let mut lm = synthetic_face();
        // the oval starting at the top (landmark 10), the chin (152) at the bottom
        for (k, i) in mediapipe::FACE_OVAL.iter().enumerate() {
            let a = k as f32 / mediapipe::FACE_OVAL.len() as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            lm[*i] = [500.0 + 300.0 * a.cos(), 500.0 + 400.0 * a.sin()];
        }
        // squinting: the lids nearly meet
        for (e, cx) in [(0, 380.0f32), (1, 620.0)] {
            for (k, i) in mediapipe::EYES[e].iter().enumerate() {
                let a = k as f32 / 16.0 * std::f32::consts::TAU;
                lm[*i] = [cx + 50.0 * a.cos(), 400.0 + 4.0 * a.sin()];
            }
        }
        // the face 0.2× in a 400-pixel photo, centred at (200, 200): 120 wide, the oval's top at 120
        let lm: Vec<[f32; 2]> = lm.iter().map(|p| [200.0 + (p[0] - 500.0) * 0.2, 200.0 + (p[1] - 500.0) * 0.2]).collect();
        let (w, h) = (400usize, 400usize);
        let in_head = |x: f32, y: f32| ((x - 200.0) / 66.0).powi(2) + ((y - 205.0) / 92.0).powi(2) <= 1.0;
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let c = if y < 118 && in_head(x as f32, y as f32) {
                    [45, 32, 25] // hair
                } else if in_head(x as f32, y as f32) {
                    // skin, with a little texture so the refinement has something to follow
                    let n = ((x * 7 + y * 13) % 5) as u8;
                    [205 - n, 150, 125]
                } else {
                    [70, 90, 120]
                };
                rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&c);
            }
        }
        // the class map: hair on top, face skin only from the brows down (it fades on the forehead)
        let side = 64;
        let mut probs = vec![0f32; side * side * 6];
        for j in 0..side {
            for i in 0..side {
                let (x, y) = ((i as f32 + 0.5) * w as f32 / side as f32, (j as f32 + 0.5) * h as f32 / side as f32);
                let c = if !in_head(x, y) {
                    0
                } else if y < 118.0 {
                    1
                } else if y < 160.0 {
                    0
                } else {
                    3
                };
                probs[(j * side + i) * 6 + c] = 1.0;
            }
        }
        let map = PeopleMap { side, probs };
        let det = Detection { x0: 140.0, y0: 120.0, x1: 260.0, y1: 280.0, score: 0.9, keypoints: [[0.0; 2]; 6] };
        let faces = [Face { detection: det, landmarks: Some(lm) }];
        let m = people_mask(&PeopleRequest::parse(0, &["Face Skin".into()]).unwrap(), &rgb, w, h, Some(&map), &faces).unwrap();
        let at = |x: f32, y: f32| m.logits[((y / h as f32 * m.side as f32) as usize) * m.side + (x / w as f32 * m.side as f32) as usize];
        for (what, x, y) in
            [("eye", 176.0, 180.0), ("eye", 224.0, 180.0), ("lid above the eye", 176.0, 177.0), ("brow", 176.0, 168.5), ("lips", 200.0, 230.0)]
        {
            assert!(at(x, y) < -1.0, "{what} is not skin: {}", at(x, y));
        }
        for (what, x, y) in [("cheek", 160.0, 212.0), ("forehead", 200.0, 145.0), ("upper forehead", 200.0, 128.0)] {
            assert!(at(x, y) > 1.0, "{what} is skin: {}", at(x, y));
        }
        assert!(at(200.0, 105.0) < -1.0, "the hair is not skin: {}", at(200.0, 105.0));
        assert!(at(30.0, 30.0) < -1.0, "the background is not skin");
    }

    /// Regression (the user's dress mask): a remembered dress came back as SAM 2.1's 256-cell mask
    /// stretched over the photo — a soft halo around the dress — with the plunging neckline's skin
    /// and the hands in it. Refined: the skin is out, the edge sits on the dress's own edge, and
    /// nothing a few pixels outside it is selected; a dress on its own (not worn) keeps its insides.
    #[test]
    fn a_found_dress_loses_the_wearers_skin_and_its_halo() {
        let (w, h) = (1024usize, 1024usize);
        // the dress: a white trapezoid from y 200 down; inside it, a skin-coloured neckline wedge
        let dress = |x: f32, y: f32| y >= 200.0 && (x - 256.0).abs() <= 80.0 + (y - 200.0) * 0.3;
        let neckline = |x: f32, y: f32| (200.0..280.0).contains(&y) && (x - 256.0).abs() <= (280.0 - y) * 0.4;
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let c = if neckline(fx, fy) {
                    [222, 170, 140]
                } else if dress(fx, fy) {
                    [240, 236, 228]
                } else {
                    [60, 62, 70]
                };
                rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&c);
            }
        }
        // SAM's coarse logits (256 cells, 4 pixels each): the dress and the neckline, with a soft
        // edge 8 pixels wide around it (the halo: still half selected 4 pixels out)
        let ls = 256;
        let edge = |x: f32, y: f32| -> f32 {
            // the distance outside the dress, along x (its sides are near vertical)
            if y < 200.0 {
                return 200.0 - y;
            }
            ((x - 256.0).abs() - (80.0 + (y - 200.0) * 0.3)).max(0.0)
        };
        let logits: Vec<f32> = (0..ls * ls)
            .map(|k| {
                let (x, y) = (((k % ls) as f32 + 0.5) * 4.0, ((k / ls) as f32 + 0.5) * 4.0);
                if dress(x, y) { 6.0 } else { 6.0 - 1.5 * edge(x, y) }
            })
            .collect();
        // the class map: the neckline is body skin, the dress clothes, the rest background
        let side = 64;
        let mut probs = vec![0f32; side * side * 6];
        for j in 0..side {
            for i in 0..side {
                let (x, y) = ((i as f32 + 0.5) * 16.0, (j as f32 + 0.5) * 16.0);
                let c = if neckline(x, y) {
                    2
                } else if dress(x, y) {
                    4
                } else {
                    0
                };
                probs[(j * side + i) * 6 + c] = 1.0;
            }
        }
        let map = PeopleMap { side, probs };
        let m = garment_mask(&logits, ls, &rgb, w, h, Some(&map)).unwrap();
        let at = |x: f32, y: f32| m.logits[((y / h as f32 * m.side as f32) as usize) * m.side + (x / w as f32 * m.side as f32) as usize];
        assert!(at(256.0, 225.0) < -1.0, "the neckline's skin is out: {}", at(256.0, 225.0));
        assert!(at(256.0, 400.0) > 1.0 && at(200.0, 300.0) > 1.0, "the dress is in");
        // its edge at y = 350 is x = 256 − 125 = 131: 3 pixels outside, nothing (the coarse mask
        // had a logit of 1.5 there, a visible tint), 3 inside, all
        assert!(at(128.0, 350.0) < -2.0, "no halo: {}", at(128.0, 350.0));
        assert!(at(134.0, 350.0) > 2.0, "the edge itself stays: {}", at(134.0, 350.0));
        // not worn (no person in the class map): nothing taken out of it
        let empty = PeopleMap { side, probs: (0..side * side * 6).map(|i| if i % 6 == 0 { 1.0 } else { 0.0 }).collect() };
        let m2 = garment_mask(&logits, ls, &rgb, w, h, Some(&empty)).unwrap();
        assert!(m2.logits[((225 * m2.side) / h) * m2.side + 256 * m2.side / w] > 0.0, "a dress on a hanger keeps what SAM drew");
        // hostile sizes
        assert!(garment_mask(&logits[..10], ls, &rgb, w, h, None).is_none());
        assert!(garment_mask(&logits, ls, &rgb, 0, 0, None).is_none());
    }

    /// Facial hair: dark, textured pixels in the lower face are cut out of skin; dark but smooth
    /// skin (a shadow) and the cheeks stay.
    #[test]
    fn beards_are_not_skin_but_shadows_are() {
        let lm = synthetic_face();
        let mut rgb = vec![150u8; 1000 * 1000 * 3];
        for y in 0..1000 {
            for x in 0..1000 {
                let i = (y * 1000 + x) * 3;
                let v = if (700..880).contains(&y) && (380..620).contains(&x) {
                    // stubble: dark and noisy
                    if (x * 7 + y * 13) % 3 == 0 { 20 } else { 75 }
                } else if (600..700).contains(&y) && (250..330).contains(&x) {
                    // a smooth shadow on the lower cheek
                    70
                } else {
                    150
                };
                rgb[i..i + 3].copy_from_slice(&[v, v, v]);
            }
        }
        let grid = mediapipe::Grid { side: 200, rect: [0.0, 0.0, 1.0, 1.0], w: 1000, h: 1000 };
        let mut holes = vec![0f32; 200 * 200];
        mediapipe::feature_holes(&grid, &mut holes, &lm, &rgb);
        let at = |x: usize, y: usize| holes[(y / 5) * 200 + x / 5];
        assert!(at(500, 800) > 0.9, "stubble is cut");
        assert!(at(290, 650) < 0.1, "a smooth shadow stays skin");
        assert!(at(300, 560) < 0.1 && at(500, 220) < 0.1, "cheek and forehead stay");
    }

    #[test]
    fn missing_or_wrong_model_files_are_errors() {
        let dir = std::env::temp_dir().join(format!("lc-people-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(PeopleModel::load(&dir).is_err());
        assert!(FaceModel::load(&dir).is_err());
        std::fs::write(dir.join(PEOPLE_FILE), b"not a tflite file").unwrap();
        std::fs::write(dir.join(FACE_FILE), b"PK not a zip").unwrap();
        assert!(PeopleModel::load(&dir).is_err());
        assert!(FaceModel::load(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A 200 × 200 scene for the garment tests: each of `people` (face centre x) a face (face skin)
    /// at y 30..50 with a neck, white clothes x ± 20 from y 55 to 190, on grass; plus `patch` (x
    /// range) of clothes-coloured stuff from y 150 to 190 nobody wears. The class map and the photo
    /// agree; `train` (alpha) adds a sheer train beside the first person, part clothes part grass.
    fn garment_scene(people: &[f32], patch: Option<(f32, f32)>, train: Option<f32>) -> (Vec<u8>, PeopleMap, Vec<Face>) {
        let (w, h, side) = (200usize, 200usize, 100usize);
        let class_at = |x: f32, y: f32| -> (usize, f32) {
            for cx in people {
                if (x - cx).abs() < 10.0 && (30.0..50.0).contains(&y) {
                    return (3, 1.0);
                }
                if (x - cx).abs() < 5.0 && (50.0..55.0).contains(&y) {
                    return (2, 1.0);
                }
                if (x - cx).abs() < 20.0 && (55.0..190.0).contains(&y) {
                    return (4, 1.0);
                }
            }
            if let Some((a, b)) = patch
                && (a..b).contains(&x)
                && (150.0..190.0).contains(&y)
            {
                return (4, 1.0);
            }
            if let (Some(t), Some(cx)) = (train, people.first())
                && (20.0..40.0).contains(&(x - cx).abs())
                && (120.0..190.0).contains(&y)
            {
                return (4, t);
            }
            (0, 1.0)
        };
        let mut probs = vec![0f32; side * side * 6];
        for j in 0..side {
            for i in 0..side {
                let (x, y) = ((i as f32 + 0.5) * 2.0, (j as f32 + 0.5) * 2.0);
                let (c, a) = class_at(x, y);
                probs[(j * side + i) * 6 + c] += a;
                probs[(j * side + i) * 6] += 1.0 - a;
            }
        }
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let (c, a) = class_at(x as f32, y as f32);
                let col: [f32; 3] = match c {
                    2 | 3 => [205.0, 150.0, 125.0],
                    4 => [240.0 * a + 60.0 * (1.0 - a), 240.0 * a + 120.0 * (1.0 - a), 235.0 * a + 50.0 * (1.0 - a)],
                    _ => [60.0, 120.0, 50.0],
                };
                for (k, v) in col.iter().enumerate() {
                    rgb[(y * w + x) * 3 + k] = *v as u8;
                }
            }
        }
        let faces = people
            .iter()
            .map(|cx| Face {
                detection: Detection { x0: cx - 10.0, y0: 30.0, x1: cx + 10.0, y1: 50.0, score: 1.0, keypoints: [[0.0; 2]; 6] },
                landmarks: None,
            })
            .collect();
        (rgb, PeopleMap { side, probs }, faces)
    }

    fn logit_at(m: &MaskGrid, x: f32, y: f32) -> f32 {
        let (i, j) = ((x / 200.0 * m.side as f32) as usize, (y / 200.0 * m.side as f32) as usize);
        m.logits[j.min(m.side - 1) * m.side + i.min(m.side - 1)]
    }

    /// Regression (client note 16): Person 1's Clothes took a clothes-coloured patch nobody wears
    /// (the nearest-face rule hands every pixel to someone). Now one person's clothes stay on the
    /// figure their face stands on, and a figure without a face is in no one's mask, Everyone's
    /// included.
    #[test]
    fn one_persons_clothes_stay_inside_their_figure() {
        let (rgb, map, faces) = garment_scene(&[50.0, 150.0], Some((85.0, 115.0)), None);
        let clothes = |n| people_mask(&PeopleRequest::parse(n, &["Clothes".into()]).unwrap(), &rgb, 200, 200, Some(&map), &faces).unwrap();
        let one = clothes(1);
        assert!(logit_at(&one, 50.0, 120.0) > 2.0, "their clothes: {}", logit_at(&one, 50.0, 120.0));
        assert!(logit_at(&one, 150.0, 120.0) < -2.0, "not the other's");
        assert!(logit_at(&one, 95.0, 170.0) < -2.0, "not the patch nearest to them: {}", logit_at(&one, 95.0, 170.0));
        let all = clothes(0);
        assert!(logit_at(&all, 50.0, 120.0) > 2.0 && logit_at(&all, 150.0, 120.0) > 2.0);
        assert!(logit_at(&all, 100.0, 170.0) < -2.0, "nobody wears the patch: {}", logit_at(&all, 100.0, 170.0));
    }

    /// Clothes are never on a face (client note 22), even where the class map calls the face
    /// clothes (a veil, or a group's blur).
    #[test]
    fn clothes_are_never_on_a_face() {
        let (rgb, mut map, faces) = garment_scene(&[100.0], None, None);
        for p in map.probs.chunks_mut(6) {
            if p[3] > 0.5 {
                p[3] = 0.0;
                p[4] = 1.0;
            }
        }
        let m = people_mask(&PeopleRequest::parse(0, &["Clothes".into()]).unwrap(), &rgb, 200, 200, Some(&map), &faces).unwrap();
        assert!(logit_at(&m, 100.0, 40.0) < -2.0, "the face is not clothes: {}", logit_at(&m, 100.0, 40.0));
        assert!(logit_at(&m, 100.0, 120.0) > 2.0, "the body's clothes are");
    }

    /// Regression (client note 16): a tulle train, which the class map splits between clothes and
    /// the grass seen through it, was dropped from its wearer's Clothes. Inside her silhouette it
    /// counts as clothes.
    #[test]
    fn a_sheer_train_inside_her_silhouette_is_clothes() {
        let (rgb, map, faces) = garment_scene(&[60.0, 160.0], None, Some(0.4));
        let side = 50;
        let claim = |x0: f32, x1: f32| -> Vec<f32> {
            (0..side * side).map(|c| if (x0..x1).contains(&((c % side) as f32 * 4.0 + 2.0)) && c / side * 4 >= 28 { 8.0 } else { -8.0 }).collect()
        };
        let seeds = [[0.3, 0.2], [0.8, 0.2]];
        let inst = Instances::new(side, vec![claim(15.0, 105.0), claim(135.0, 185.0)], &seeds);
        let req = PeopleRequest::parse(1, &["Clothes".into()]).unwrap();
        let m = people_mask_with(&req, &rgb, 200, 200, Some(&map), &faces, Some(&inst), None).unwrap();
        assert!(logit_at(&m, 90.0, 160.0) > 0.0, "the train is hers: {}", logit_at(&m, 90.0, 160.0));
        assert!(logit_at(&m, 60.0, 120.0) > 2.0);
        assert!(logit_at(&m, 160.0, 120.0) < -2.0, "the other's clothes are not");
        assert!(logit_at(&m, 110.0, 160.0) < -2.0, "grass beside the train is not");
    }

    #[test]
    fn figures_bound_silhouettes_and_faceless_figures_are_their_own() {
        let side = 8;
        let face = |x: f32| Face {
            detection: Detection { x0: x - 5.0, y0: 5.0, x1: x + 5.0, y1: 15.0, score: 1.0, keypoints: [[0.0; 2]; 6] },
            landmarks: None,
        };
        let faces = [face(20.0), face(40.0)];
        // a figure over the left columns (face 1's), one over the right ones (nobody's face)
        let fig = |c0: usize, c1: usize| -> Vec<f32> { (0..side * side).map(|c| if (c0..c1).contains(&(c % side)) { 6.0 } else { -6.0 }).collect() };
        let inst = Instances::combine(side, None, &[fig(0, 3), fig(6, 8)], &faces, 80, 80).unwrap();
        assert_eq!(inst.owner(0.1, 0.5), Some(0));
        assert_eq!(inst.owner(0.9, 0.5), Some(2), "the faceless figure is someone else");
        // a silhouette reaching past its figure is cut back to it
        let wide: Vec<f32> = (0..side * side).map(|c| if c % side < 6 { 6.0 } else { -6.0 }).collect();
        let none = vec![-6.0; side * side];
        let inst = Instances::combine(side, Some(vec![wide, none]), &[fig(0, 3)], &faces, 80, 80).unwrap();
        assert_eq!(inst.owner(0.1, 0.5), Some(0));
        assert_ne!(inst.owner(0.6, 0.5), Some(0), "past the figure");
        assert!(Instances::combine(side, None, &[], &faces, 80, 80).is_none());
        assert!(Instances::combine(0, None, &[], &faces, 0, 0).is_none());
    }

    #[test]
    fn only_small_people_are_cropped_and_their_crops_paste_back() {
        let face = |x: f32, s: f32| Face {
            detection: Detection { x0: x, y0: 100.0, x1: x + s, y1: 100.0 + s, score: 1.0, keypoints: [[0.0; 2]; 6] },
            landmarks: None,
        };
        // a close-up: no crops; a group: one box each, from above the head down the body
        assert!(person_boxes(&[face(100.0, 200.0)], 1000, 1000).is_empty());
        let b = person_boxes(&[face(100.0, 20.0), face(500.0, 20.0)], 1000, 1000);
        assert_eq!(b.len(), 2);
        assert!(b[0][0] < 0.1 && b[0][2] > 0.12 && b[0][1] < 0.1 && b[0][3] > 0.25, "{:?}", b[0]);
        // a bridesmaid seen whole at a ceremony (OneSlips' test notes 2026-10-24, note 17): cropped, down to
        // where her gown reaches
        let b = person_boxes(&[face(100.0, 50.0)], 1000, 1000);
        assert_eq!(b.len(), 1, "a face a twentieth of the photo's height is cropped");
        assert!(b[0][3] >= 0.55, "the crop reaches the hem: {:?}", b[0]);
        assert!(person_boxes(&[face(f32::NAN, 20.0)], 1000, 1000).iter().all(|b| b.iter().all(|v| v.is_finite())));
        assert!(person_boxes(&[face(1.0, 1.0)], 0, 0).is_empty());
        // the whole map says background; a crop says clothes: inside it clothes, outside background
        let n = mediapipe::SELFIE_CLASSES.len();
        let whole = PeopleMap { side: 16, probs: (0..16 * 16 * n).map(|i| if i % n == 0 { 1.0 } else { 0.0 }).collect() };
        let crop = PeopleMap { side: 16, probs: (0..16 * 16 * n).map(|i| if i % n == 4 { 1.0 } else { 0.0 }).collect() };
        let out = paste_crops(&whole, &[[0.25, 0.25, 0.75, 0.75]], &[crop], 64);
        assert_eq!(out.side, 64);
        assert!(out.at(&[4], 0.5, 0.5) > 0.95, "the crop's class inside");
        assert!(out.at(&[0], 0.1, 0.1) > 0.99, "the whole map outside");
        assert!(out.at(&[4], 0.26, 0.5) < out.at(&[4], 0.5, 0.5), "its edge fades into the whole map");
        // nothing to paste: the whole map
        let same = paste_crops(&whole, &[], &[], 64);
        assert!(same.at(&[0], 0.5, 0.5) > 0.99);
    }

    /// A bride on a floor (`w = h = 200`): a white dress (x 60..140, y 60..170) with a bare arm
    /// beside it (x 40..60, y 60..170, skin-coloured; `arm_rgb`), a pale floor below (y ≥ 170) and
    /// a grey wall above. The class map (64 cells) makes the people model's mistakes: the upper
    /// arm (y < 140) and a band of the floor under the hem (y 170..190) are "clothes"; only the
    /// forearm (y ≥ 140) is skin.
    fn bride_on_a_floor(arm_rgb: [u8; 3]) -> (Vec<u8>, PeopleMap) {
        let (w, h) = (200usize, 200usize);
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let p = if y >= 170 && !(60..140).contains(&x) || y >= 175 {
                    [200, 194, 182]
                } else if (60..140).contains(&x) && y >= 60 {
                    [236, 236, 238]
                } else if (40..60).contains(&x) && y >= 60 {
                    arm_rgb
                } else {
                    [90, 92, 96]
                };
                rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&p);
            }
        }
        let side = 64;
        let mut probs = vec![0f32; side * side * 6];
        for j in 0..side {
            for i in 0..side {
                let (x, y) = ((i * w + w / 2) / side, (j * h + h / 2) / side);
                let class = if (40..60).contains(&x) && (140..170).contains(&y) {
                    2
                } else if (40..140).contains(&x) && (60..170).contains(&y) || (60..140).contains(&x) && (170..190).contains(&y) {
                    4
                } else {
                    0
                };
                probs[(j * side + i) * 6 + class] = 1.0;
            }
        }
        (rgb, PeopleMap { side, probs })
    }

    /// Two women standing apart on a grey wall (200 × 200): each with hair, a face and blue
    /// pyjamas down to y 190, the left one at x 20..70, the right one at x 130..180; and between
    /// them a blue patch (x 88..112, y 150..190) the class map calls clothes, with no head. Only
    /// the right one's face is found (the left one's is turned away). → (rgb, map, faces).
    fn two_standing_one_face_found() -> (Vec<u8>, PeopleMap, Vec<Face>) {
        let (w, h) = (200usize, 200usize);
        // what is at photo pixel (x, y): 0 wall, 1 hair, 3 face skin, 4 clothes
        let at = |x: usize, y: usize| -> usize {
            let person = (20..70).contains(&x) || (130..180).contains(&x);
            let inner = (32..58).contains(&x) || (142..168).contains(&x);
            if person && (20..36).contains(&y) || inner && (36..44).contains(&y) {
                1
            } else if inner && (44..60).contains(&y) {
                3
            } else if person && (60..190).contains(&y) || (88..112).contains(&x) && (150..190).contains(&y) {
                4
            } else {
                0
            }
        };
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let p = match at(x, y) {
                    1 => [70, 48, 30],
                    3 => [222, 170, 142],
                    4 => [60, 90, 200],
                    _ => [128, 128, 130],
                };
                rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&p);
            }
        }
        let side = 64;
        let mut probs = vec![0f32; side * side * 6];
        for j in 0..side {
            for i in 0..side {
                let (x, y) = ((i * w + w / 2) / side, (j * h + h / 2) / side);
                probs[(j * side + i) * 6 + at(x, y)] = 1.0;
            }
        }
        let face = Face { detection: Detection { x0: 142.0, y0: 40.0, x1: 168.0, y1: 60.0, score: 0.9, keypoints: [[0.0; 2]; 6] }, landmarks: None };
        (rgb, PeopleMap { side, probs }, vec![face])
    }

    /// Regression (OneSlips' test notes 2026-10-25, note 12): Everyone ▸ Clothes left out a whole standing
    /// woman whose face wasn't found (her figure counted as nobody's, like a patch of grass). A
    /// figure with a head is someone: her pyjamas are in; the headless blue patch stays out.
    #[test]
    fn everyones_clothes_keep_a_person_whose_face_wasnt_found() {
        let (rgb, map, faces) = two_standing_one_face_found();
        let req = PeopleRequest::parse(0, &["Clothes".into()]).unwrap();
        let m = people_mask_with(&req, &rgb, 200, 200, Some(&map), &faces, None, None).unwrap();
        for (x, y) in [(45, 100), (40, 170), (155, 100), (150, 170)] {
            assert!(clothes_at(&m, x, y) > 0.5, "pyjamas at {x},{y} are clothes: {}", clothes_at(&m, x, y));
        }
        for (x, y) in [(100, 170), (100, 160), (100, 100)] {
            assert!(clothes_at(&m, x, y) < 0.5, "the headless patch / wall at {x},{y} is not: {}", clothes_at(&m, x, y));
        }
        // one person of the two (the found face) still never takes the other's clothes
        let one = people_mask_with(&PeopleRequest::parse(1, &["Clothes".into()]).unwrap(), &rgb, 200, 200, Some(&map), &faces, None, None).unwrap();
        assert!(clothes_at(&one, 155, 100) > 0.5 && clothes_at(&one, 45, 100) < 0.5);
    }

    /// Regression (OneSlips' test notes 2026-10-22, note 6): the people model calls a bare arm below an
    /// off-shoulder tulle sleeve "clothes"; skin is never in Clothes, so the arm, grown from the
    /// skin the model does see (the forearm) through the cells coloured like it, is cut out.
    #[test]
    fn skin_the_class_map_calls_clothes_is_not_clothes() {
        let (rgb, map) = bride_on_a_floor([224, 168, 140]);
        let req = PeopleRequest::parse(0, &["Clothes".into()]).unwrap();
        let m = people_mask_with(&req, &rgb, 200, 200, Some(&map), &[], None, None).unwrap();
        for (x, y) in [(50, 70), (50, 100), (45, 130), (55, 150)] {
            assert!(clothes_at(&m, x, y) < 0.5, "arm skin at {x},{y} is not clothes: {}", clothes_at(&m, x, y));
        }
        for (x, y) in [(70, 100), (100, 120), (130, 160)] {
            assert!(clothes_at(&m, x, y) > 0.5, "the dress at {x},{y} is clothes: {}", clothes_at(&m, x, y));
        }
    }

    /// Where skin and the garment look alike (a cream waistcoat, an ivory bodice in tungsten
    /// light), colour can't tell them apart: nothing of the garment is cut for it.
    #[test]
    fn a_garment_coloured_like_skin_is_kept() {
        let (rgb, map) = bride_on_a_floor([236, 234, 232]);
        let req = PeopleRequest::parse(0, &["Clothes".into()]).unwrap();
        let m = people_mask_with(&req, &rgb, 200, 200, Some(&map), &[], None, None).unwrap();
        for (x, y) in [(50, 80), (50, 110), (100, 120)] {
            assert!(clothes_at(&m, x, y) > 0.5, "the class map's clothes at {x},{y} stay: {}", clothes_at(&m, x, y));
        }
    }

    /// Regression (note 6): the people model spills "clothes" onto a pale floor under the hem; the
    /// floor the scene model sees there is never clothes, while the dress down to its hem stays.
    #[test]
    fn the_floor_below_the_hem_is_not_clothes() {
        let (rgb, map) = bride_on_a_floor([224, 168, 140]);
        // the scene model: floor from y 172 (a little below the hem, as its coarse grid puts it)
        let (sw, sh) = (40, 40);
        let floor: Vec<f32> = (0..sw * sh).map(|c| if (c / sw) * 200 / sh >= 172 { 0.95 } else { 0.0 }).collect();
        let surfaces = Surfaces::from_grid(sw, sh, floor);
        for person in [0, 1] {
            let req = PeopleRequest::parse(person, &["Clothes".into()]).unwrap();
            let faces =
                [Face { detection: Detection { x0: 85.0, y0: 20.0, x1: 115.0, y1: 55.0, score: 1.0, keypoints: [[0.0; 2]; 6] }, landmarks: None }];
            let faces: &[Face] = if person == 0 { &[] } else { &faces };
            let without = people_mask_with(&req, &rgb, 200, 200, Some(&map), faces, None, None).unwrap();
            assert!(clothes_at(&without, 100, 180) > 0.5, "(the class map's spill is there to cut)");
            let m = people_mask_with(&req, &rgb, 200, 200, Some(&map), faces, None, Some(&surfaces)).unwrap();
            for (x, y) in [(70, 180), (100, 185), (130, 178)] {
                assert!(clothes_at(&m, x, y) < 0.5, "floor at {x},{y} is not clothes (person {person}): {}", clothes_at(&m, x, y));
            }
            for (x, y) in [(70, 120), (100, 160), (130, 165)] {
                assert!(clothes_at(&m, x, y) > 0.5, "the dress at {x},{y} is clothes (person {person}): {}", clothes_at(&m, x, y));
            }
        }
        // malformed or empty grids are no surface
        assert_eq!(Surfaces::from_grid(0, 0, Vec::new()).at(0.5, 0.5), 0.0);
        assert_eq!(Surfaces::from_grid(4, 4, vec![1.0; 3]).at(0.5, 0.5), 0.0);
        assert_eq!(Surfaces::from_grid(2, 2, vec![1.0; 4]).at(f32::NAN, 0.5), 0.0);
        assert!(is_surface("floor-wood") && is_surface("wall-brick") && is_surface("grass-merged") && !is_surface("person"));
    }

    /// Regression (test notes 2026-10-25, note 12): the bride seated in an armchair, its dark back
    /// rising above her shoulders; the people model's blurred "clothes" spills onto the chair back
    /// and Clothes tinted it. The scene model sees a chair there: never clothes; her dress stays.
    #[test]
    fn the_chair_back_behind_a_seated_bride_is_not_clothes() {
        use crate::scene::{CLASSES, Scene, class_ids};
        let (w, h) = (200usize, 200usize);
        // 0 the wall, 3 her face (x 88..112, y 44..66), 4 her dress (x 70..130, y 66..190), 5 the
        // chair back (x 50..150, y 30..66, and its sides down to y 150)
        let at = |x: usize, y: usize| -> u8 {
            if (88..112).contains(&x) && (44..66).contains(&y) {
                3
            } else if (70..130).contains(&x) && (66..190).contains(&y) {
                4
            } else if (50..150).contains(&x) && (30..66).contains(&y) || ((50..70).contains(&x) || (130..150).contains(&x)) && (30..150).contains(&y)
            {
                5
            } else {
                0
            }
        };
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let p = match at(x, y) {
                    3 => [222, 170, 142],
                    4 => [236, 236, 238],
                    5 => [38, 30, 28],
                    _ => [150, 146, 140],
                };
                rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&p);
            }
        }
        // the people model: the dress is clothes, and so (its blur) is the chair back
        let side = 64;
        let mut probs = vec![0f32; side * side * 6];
        for j in 0..side {
            for i in 0..side {
                let (x, y) = ((i * w + w / 2) / side, (j * h + h / 2) / side);
                let class = match at(x, y) {
                    3 => 3,
                    4 | 5 => 4,
                    _ => 0,
                };
                probs[(j * side + i) * 6 + class] = 1.0;
            }
        }
        let map = PeopleMap { side, probs };
        // the scene model: the chair where it shows, the person over the dress and face
        let (sw, sh) = (40usize, 40usize);
        let n = sw * sh;
        let id = |name: &str| class_ids(|l| l == name)[0];
        let mut sp = vec![0f32; CLASSES * n];
        for c in 0..n {
            let (x, y) = ((c % sw) * w / sw + 2, (c / sw) * h / sh + 2);
            match at(x, y) {
                5 => sp[id("chair") * n + c] = 0.9,
                3 | 4 => sp[id("person") * n + c] = 0.95,
                _ => sp[id("wall-other-merged") * n + c] = 0.9,
            }
        }
        let surfaces = Surfaces::from_scene(&Scene { width: sw, height: sh, probs: sp, instances: Vec::new() });
        let req = PeopleRequest::parse(0, &["Clothes".into()]).unwrap();
        let m = people_mask_with(&req, &rgb, w, h, Some(&map), &[], None, Some(&surfaces)).unwrap();
        for (x, y) in [(100, 40), (60, 50), (140, 50), (60, 110)] {
            assert!(clothes_at(&m, x, y) < 0.5, "the chair at {x},{y} is not clothes: {}", clothes_at(&m, x, y));
        }
        for (x, y) in [(100, 100), (85, 150), (115, 180)] {
            assert!(clothes_at(&m, x, y) > 0.5, "the dress at {x},{y} is clothes: {}", clothes_at(&m, x, y));
        }
    }

    /// Regression (test notes 2026-10-24, note 17): the scene model doesn't see a big white gown as
    /// a person and spreads it over guesses (boat, water, building, tent); their surfaces summed
    /// passed the cut and the whole gown, train and veil left Clothes. A surface counts only where
    /// one is the scene model's clear winner: the gown stays to the hem, the floor below goes.
    #[test]
    fn a_gown_the_scene_model_only_guesses_at_stays_clothes() {
        use crate::scene::{CLASSES, Scene, class_ids};
        let (rgb, map) = bride_on_a_floor([224, 168, 140]);
        let (sw, sh) = (40usize, 40usize);
        let n = sw * sh;
        let id = |name: &str| class_ids(|l| l == name)[0];
        let mut probs = vec![0f32; CLASSES * n];
        let mut set = |class: usize, c: usize, p: f32| probs[class * n + c] = p;
        for c in 0..n {
            let (x, y) = ((c % sw) * 200 / sw, (c / sw) * 200 / sh);
            if y >= 172 {
                // the deck: floor, sure of it
                set(id("floor-wood"), c, 1.0);
                set(id("boat"), c, 0.35);
            } else if y >= 60 && (40..160).contains(&x) {
                // the gown: guesses, the strongest a thing
                set(id("boat"), c, 0.47);
                set(id("water-other"), c, 0.37);
                set(id("building-other-merged"), c, 0.14);
                set(id("fence-merged"), c, 0.12);
                set(id("tent"), c, 0.12);
            }
        }
        let scene = Scene { width: sw, height: sh, probs, instances: Vec::new() };
        let surfaces = Surfaces::from_scene(&scene);
        assert!(surfaces.at(0.5, 0.95) > 0.9, "the floor is a surface: {}", surfaces.at(0.5, 0.95));
        assert!(surfaces.at(0.5, 0.6) < 0.05, "the gown is not: {}", surfaces.at(0.5, 0.6));
        let req = PeopleRequest::parse(0, &["Clothes".into()]).unwrap();
        let m = people_mask_with(&req, &rgb, 200, 200, Some(&map), &[], None, Some(&surfaces)).unwrap();
        for (x, y) in [(70, 120), (100, 140), (130, 165), (100, 100)] {
            assert!(clothes_at(&m, x, y) > 0.5, "the gown at {x},{y} is clothes: {}", clothes_at(&m, x, y));
        }
        for (x, y) in [(70, 185), (100, 190)] {
            assert!(clothes_at(&m, x, y) < 0.5, "the floor at {x},{y} is not clothes: {}", clothes_at(&m, x, y));
        }
        // a dining table in front of the couple is a surface too, and the chair or sofa they sit
        // on; a malformed scene is none
        assert!(is_surface("chair") && is_surface("couch") && is_surface("bench"));
        assert!(is_surface("dining table") && !is_surface("boat"));
        let empty = Surfaces::from_scene(&Scene { width: 3, height: 3, probs: vec![f32::NAN; 5], instances: Vec::new() });
        assert_eq!(empty.at(0.5, 0.5), 0.0);
    }
}
