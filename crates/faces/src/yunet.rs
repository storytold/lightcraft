//! The YuNet face detector (232 KB, MIT, downloaded by the user in Settings ▸ Faces), run by LightCraft's own small
//! interpreter ([`crate::net`]): no extra runtime.
//!
//! A photo is shrunk (never enlarged) until its long edge fits [`Options::long_edge`], padded at the right and bottom
//! to a multiple of 32 (the network is fully convolutional, so the input needn't be its file's 640 × 640 square: this
//! is how OpenCV's `FaceDetectorYN` and the published WIDER FACE figures run it). A model is trusted to do that only
//! after it has run at non-square inputs at load time ([`Detector::new`]); one that has not (a shape-fixed operator
//! in a retrained model) is run in its declared square instead, the photo fitted into it, and says so in
//! [`Detector::input_mode`]. The network gives a score, a box
//! and five landmarks for every cell of three grids (strides 8, 16 and 32), and the cells above the score threshold
//! are decoded, de-duplicated by non-maximum suppression and mapped back to the photo as fractions of its width and
//! height.
//!
//! The pixels come from the caller as plain 8-bit RGB; this module has no image types of its own.

use crate::graph;
use crate::net::{Net, NetError, Tensor};

const STRIDES: [usize; 3] = [8, 16, 32];
/// Largest photo side we accept.
const MAX_SIDE: usize = 65_536;
/// Candidate cells kept before suppression, so a hostile or noisy output cannot make it slow. 5000 is OpenCV's own
/// `top_k` default ("keep top K bboxes before NMS") in `FaceDetectorYN::create` and in the opencv_zoo YuNet demo
/// (`--top_k`, default 5000), so the decoder keeps what the reference keeps.
const MAX_CANDIDATES: usize = 5000;
/// The non-square shapes (height, width) a model must run at, besides its declared square, to be fed photos in their
/// own shape: checked by shape inference at a size scans reach, and by running at small ones.
const PROBE_SHAPES: [(usize, usize); 5] = [(704, 1024), (1024, 704), (32, 1024), (1024, 32), (32, 32)];
/// The (height, width) inputs an accelerator is compared with the CPU at when photos keep their shape: a 3:2
/// landscape (1024 wide, 704 high) and portrait, so a height/width mix-up on the device shows, and a one-row grid for
/// a panorama (1024 wide, 32 high, a shape the CPU is also run at on load). They are representative shapes, not every
/// shape a scan runs: a scan's shapes come from each photo. The device goes live only after it agreed at every one
/// of them.
const ACCELERATOR_PROBES: [(usize, usize); 3] = [(704, 1024), (1024, 704), (32, 1024)];
const PROBE_RUNS: [(usize, usize); 4] = [(96, 160), (160, 96), (32, 32), (32, 192)];
/// Largest network input side: memory for the stride-8 feature maps grows with its square.
pub const MAX_INPUT: usize = 4096;
/// The long edge a scan gives the network. YuNet finds faces down to about 10 px, so the input size decides how small
/// a face it can find: squeezed into its file's 640 × 640 square it missed the back rows of group photos (WIDER FACE
/// hard 59.8 against the published 75.0, which OpenCV measures at each photo's own size, 1024 px wide); at 1024 px it
/// reaches the published figures (hard 76.6) for 2 to 2.5 times the time on a CPU (measured, docs/faces.md).
pub const SCAN_LONG_EDGE: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum DetectError {
    #[error("the detector model is not usable: {0}")]
    Model(String),
    #[error("{0}")]
    Input(String),
}

impl From<NetError> for DetectError {
    fn from(e: NetError) -> Self {
        DetectError::Model(e.to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// Lowest detection score kept (0 to 1).
    pub score: f32,
    /// Boxes overlapping a better one by more than this (intersection over union) are dropped.
    pub nms_iou: f32,
    /// Most faces returned.
    pub max_faces: usize,
    /// The network input's longest side: a larger photo is shrunk to fit, a smaller one is looked at as it is.
    pub long_edge: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { score: 0.6, nms_iou: 0.3, max_faces: 200, long_edge: SCAN_LONG_EDGE }
    }
}

/// A detected face, in fractions of the photo's width and height (0 to 1, y down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Face {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub score: f32,
    /// Right eye, left eye, nose tip, right and left mouth corner, as the detector names them (from the
    /// subject's point of view, so the "right eye" is on the left of the picture). May lie slightly outside 0 to 1.
    pub landmarks: [(f32, f32); 5],
}

/// How far an accelerator's outputs may stray from the CPU's on the test input, relative to each output's largest value.
pub const ACCELERATOR_TOLERANCE: f32 = 1e-3;

/// A deterministic, photo-like test input (smooth gradients plus texture, in 0..255).
pub fn probe_pixels(n: usize) -> Vec<f32> {
    (0..n).map(|i| ((i % 640) as f32 * 0.3 + ((i * 7919) % 97) as f32 + (i / 640 % 480) as f32 * 0.2) % 255.0).collect()
}

pub struct Detector {
    net: Net,
    /// Side of the square network input the model declares.
    side: usize,
    /// Why the model could not be run at non-square inputs; `None` when it can, and photos keep their shape.
    fixed_because: Option<String>,
}

/// Check the output sizes `shapes` (from shape inference, in `outputs` order) against an `h` × `w` input.
fn check_outputs(shapes: &[Vec<usize>], outputs: &[String], h: usize, w: usize) -> Result<(), DetectError> {
    for stride in STRIDES {
        for (kind, components) in [("cls", 1), ("obj", 1), ("bbox", 4), ("kps", 10)] {
            let name = format!("{kind}_{stride}");
            let index = outputs.iter().position(|n| n == &name).ok_or_else(|| DetectError::Model(format!("the model has no output `{name}`")))?;
            let count = shapes.get(index).and_then(|s| s.iter().try_fold(1usize, |n, d| n.checked_mul(*d)));
            if count != Some((h / stride) * (w / stride) * components) {
                return Err(DetectError::Model(format!("the output `{name}` has the wrong shape at {w} × {h}")));
            }
        }
    }
    Ok(())
}

/// Whether `net` gives YuNet's outputs for non-square inputs: by shape inference, then by running it.
fn probe_non_square(net: &Net, outputs: &[String]) -> Result<(), DetectError> {
    for (h, w) in PROBE_SHAPES {
        check_outputs(&net.validate_input(&[1, 3, h, w])?, outputs, h, w)?;
    }
    for (h, w) in PROBE_RUNS {
        let input = Tensor::new(vec![1, 3, h, w], probe_pixels(3 * h * w))?;
        let result = net.run(input)?;
        for stride in STRIDES {
            for (kind, components) in [("cls", 1), ("obj", 1), ("bbox", 4), ("kps", 10)] {
                let name = format!("{kind}_{stride}");
                let ok = result.iter().any(|(n, t)| *n == name && t.data.len() == (h / stride) * (w / stride) * components);
                if !ok {
                    return Err(DetectError::Model(format!("the output `{name}` has the wrong size at {w} × {h}")));
                }
            }
        }
    }
    Ok(())
}

impl Detector {
    /// A detector from YuNet-architecture ONNX bytes (a retrained model of the same shape works too).
    pub fn new(onnx: &[u8]) -> Result<Detector, DetectError> {
        let g = graph::load(onnx).map_err(|e| DetectError::Model(e.to_string()))?;
        let outputs = g.outputs.clone();
        let net = Net::new(g)?;
        let side = match net.input_shape() {
            Some(&[1, 3, h, w]) if h == w && (64..=4096).contains(&h) && h % 32 == 0 => h,
            other => {
                return Err(DetectError::Model(format!(
                    "the model declares the input {other:?}; it needs a square [1, 3, N, N] with N a multiple of 32 from 64 to 4096 (the side it is checked at, and run at if it cannot take other shapes)"
                )));
            }
        };
        check_outputs(&net.validate_input(&[1, 3, side, side])?, &outputs, side, side)?;
        // A square that works says nothing about other shapes: a literal Reshape target or another shape-fixed operator
        // would fail every photo that is not square. Such a model is run in its declared square instead.
        let fixed_because = probe_non_square(&net, &outputs).err().map(|e| e.to_string());
        Ok(Detector { net, side, fixed_because })
    }

    /// How photos reach the network: in their own shape (the model ran at the non-square probes at load time), or
    /// fitted into the model's declared square (it did not, and why).
    pub fn input_mode(&self) -> String {
        match &self.fixed_because {
            None => "in the photo's own shape, up to 1024 px (the model also ran at non-square inputs at load)".into(),
            Some(why) => format!("fitted into the model's {0} × {0} square (it failed the non-square check at load: {why})", self.side),
        }
    }

    /// Whether photos are looked at in their own shape (see [`Detector::input_mode`]).
    pub fn keeps_photo_shape(&self) -> bool {
        self.fixed_because.is_none()
    }

    /// The network, e.g. to see which device it runs on.
    pub fn net(&self) -> &Net {
        &self.net
    }

    /// Run the network's convolutions on `accelerator` if it gives the CPU's outputs on a test picture (and, with `require_faster`,
    /// runs it faster than the CPU); otherwise
    /// (or on its first error) detection stays on the CPU. Returns why it was not used.
    pub fn accelerate(&self, accelerator: std::sync::Arc<dyn crate::net::Accelerator>, require_faster: bool) -> Result<(), String> {
        // The shapes detection feeds the network: a landscape, a portrait and a one-row scan input when photos keep
        // their shape (the first also decides "faster than the CPU", as a scan runs it), else the declared square.
        // The net publishes the device only after all of them agreed.
        let side = self.side;
        let shapes: &[(usize, usize)] = if self.keeps_photo_shape() { &ACCELERATOR_PROBES } else { &[(side, side)] };
        let probes = shapes
            .iter()
            .map(|(h, w)| Tensor::new(vec![1, 3, *h, *w], probe_pixels(3 * h * w)).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        self.net.attach_checked(accelerator, &probes, ACCELERATOR_TOLERANCE, require_faster)
    }

    /// The square input side the model file declares, used to check an installed manifest.
    pub fn input_side(&self) -> usize {
        self.side
    }

    /// [`Detector::input_size`] for this model: its own square when it cannot take other shapes.
    fn plan(&self, width: usize, height: usize, long_edge: usize) -> ((usize, usize), (usize, usize)) {
        if self.fixed_because.is_none() {
            return Self::input_size(width, height, long_edge);
        }
        let (fitted, _) = Self::input_size(width, height, long_edge.min(self.side));
        (fitted, (self.side, self.side))
    }

    /// The network input for a `width` × `height` photo with `long_edge`: the photo's size there, and the padded input.
    fn input_size(width: usize, height: usize, long_edge: usize) -> ((usize, usize), (usize, usize)) {
        let long = long_edge.clamp(32, MAX_INPUT);
        let scale = (long as f64 / width.max(height).max(1) as f64).min(1.0);
        let nw = ((width as f64 * scale).round() as usize).clamp(1, long);
        let nh = ((height as f64 * scale).round() as usize).clamp(1, long);
        ((nw, nh), (nw.div_ceil(32) * 32, nh.div_ceil(32) * 32))
    }

    /// Find faces in an upright `width` × `height` photo of 8-bit RGB pixels (`rgb` is `width * height * 3` bytes).
    pub fn detect(&self, rgb: &[u8], width: usize, height: usize, opts: &Options) -> Result<Vec<Face>, DetectError> {
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(DetectError::Input(format!("a {width} × {height} picture cannot be searched")));
        }
        if width.checked_mul(height).and_then(|n| n.checked_mul(3)) != Some(rgb.len()) {
            return Err(DetectError::Input("the pixel data does not match the picture's size".into()));
        }
        let opts = Options {
            score: if opts.score.is_finite() { opts.score.clamp(0.0, 1.0) } else { 0.6 },
            nms_iou: if opts.nms_iou.is_finite() { opts.nms_iou.clamp(0.0, 1.0) } else { 0.3 },
            max_faces: opts.max_faces.min(10_000),
            long_edge: opts.long_edge,
        };
        let ((nw, nh), (iw, ih)) = self.plan(width, height, opts.long_edge);
        // input pixels per photo pixel, along each axis
        let (sx, sy) = (nw as f32 / width as f32, nh as f32 / height as f32);
        let input = letterbox(rgb, width, height, (nw, nh), (iw, ih))?;
        let outputs = self.net.run(input)?;
        let find = |name: String| {
            outputs.iter().find(|(n, _)| *n == name).map(|(_, t)| t).ok_or_else(|| DetectError::Model(format!("the model has no output `{name}`")))
        };
        let mut candidates: Vec<Face> = Vec::new();
        for stride in STRIDES {
            let (cls, obj) = (find(format!("cls_{stride}"))?, find(format!("obj_{stride}"))?);
            let (bbox, kps) = (find(format!("bbox_{stride}"))?, find(format!("kps_{stride}"))?);
            let cols = iw / stride;
            let cells = cols * (ih / stride);
            if cls.data.len() != cells || obj.data.len() != cells || bbox.data.len() != cells * 4 || kps.data.len() != cells * 10 {
                return Err(DetectError::Model(format!("the outputs for stride {stride} have the wrong size")));
            }
            for (i, (c, o)) in cls.data.iter().zip(&obj.data).enumerate() {
                let score = (c.clamp(0.0, 1.0) * o.clamp(0.0, 1.0)).sqrt();
                if score.is_nan() || score < opts.score {
                    continue;
                }
                let (row, col) = ((i / cols) as f32, (i % cols) as f32);
                let (Some(b), Some(k)) = (bbox.data.get(i * 4..i * 4 + 4), kps.data.get(i * 10..i * 10 + 10)) else { continue };
                let s = stride as f32;
                let (cx, cy) = ((col + b[0]) * s, (row + b[1]) * s);
                let (w, h) = (b[2].clamp(-10.0, 10.0).exp() * s, b[3].clamp(-10.0, 10.0).exp() * s);
                let mut landmarks = [(0.0, 0.0); 5];
                for (n, l) in landmarks.iter_mut().enumerate() {
                    let (lx, ly) = (k.get(n * 2).copied().unwrap_or(0.0), k.get(n * 2 + 1).copied().unwrap_or(0.0));
                    *l = (((lx + col) * s) / sx / width as f32, ((ly + row) * s) / sy / height as f32);
                }
                candidates.push(Face {
                    x0: (cx - w / 2.0) / sx / width as f32,
                    y0: (cy - h / 2.0) / sy / height as f32,
                    x1: (cx + w / 2.0) / sx / width as f32,
                    y1: (cy + h / 2.0) / sy / height as f32,
                    score,
                    landmarks,
                });
            }
        }
        candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
        candidates.truncate(MAX_CANDIDATES);
        let mut kept: Vec<Face> = Vec::new();
        for c in candidates {
            if kept.len() >= opts.max_faces {
                break;
            }
            // the box as it lies inside the photo
            let clipped = Face { x0: c.x0.clamp(0.0, 1.0), y0: c.y0.clamp(0.0, 1.0), x1: c.x1.clamp(0.0, 1.0), y1: c.y1.clamp(0.0, 1.0), ..c };
            if !(clipped.x1 - clipped.x0 > 1e-4 && clipped.y1 - clipped.y0 > 1e-4) || kept.iter().any(|k| iou(k, &clipped) > opts.nms_iou) {
                continue;
            }
            kept.push(clipped);
        }
        Ok(kept)
    }
}

/// The photo resized to `nw` × `nh` into the top left of a zero-filled `iw` × `ih` input, as BGR 0..255 (the model's
/// own input).
fn letterbox(rgb: &[u8], width: usize, height: usize, (nw, nh): (usize, usize), (iw, ih): (usize, usize)) -> Result<Tensor, DetectError> {
    let (xt, yt) = (taps(width, nw), taps(height, nh));
    // horizontal pass: every source row, nw columns, three channels
    let mut mid = vec![0.0f32; height * nw * 3];
    for (src_row, mid_row) in rgb.chunks_exact(width * 3).zip(mid.chunks_exact_mut(nw * 3)) {
        for (x, tap) in xt.iter().enumerate() {
            let mut acc = [0.0f32; 3];
            for (j, wt) in tap.weights.iter().enumerate() {
                if let Some(px) = src_row.get((tap.start + j) * 3..(tap.start + j) * 3 + 3) {
                    acc[0] += wt * f32::from(px[0]);
                    acc[1] += wt * f32::from(px[1]);
                    acc[2] += wt * f32::from(px[2]);
                }
            }
            if let Some(o) = mid_row.get_mut(x * 3..x * 3 + 3) {
                o.copy_from_slice(&acc);
            }
        }
    }
    // vertical pass into the planes B, G, R
    let plane = iw * ih;
    let mut data = vec![0.0f32; 3 * plane];
    for (y, tap) in yt.iter().enumerate() {
        for x in 0..nw {
            let mut acc = [0.0f32; 3];
            for (j, wt) in tap.weights.iter().enumerate() {
                if let Some(px) = mid.get(((tap.start + j) * nw + x) * 3..((tap.start + j) * nw + x) * 3 + 3) {
                    acc[0] += wt * px[0];
                    acc[1] += wt * px[1];
                    acc[2] += wt * px[2];
                }
            }
            let at = y * iw + x;
            for (c, v) in [acc[2], acc[1], acc[0]].into_iter().enumerate() {
                if let Some(o) = data.get_mut(c * plane + at) {
                    *o = v;
                }
            }
        }
    }
    Ok(Tensor::new(vec![1, 3, ih, iw], data)?)
}

struct Tap {
    start: usize,
    weights: Vec<f32>,
}

/// For each of `dst` output positions, which `src` positions to average and with what weights: exact area
/// averaging when shrinking, linear interpolation when enlarging.
fn taps(src: usize, dst: usize) -> Vec<Tap> {
    let r = src as f32 / dst as f32;
    (0..dst)
        .map(|i| {
            if r >= 1.0 {
                let (a, b) = (i as f32 * r, (i as f32 + 1.0) * r);
                let first = a.floor() as usize;
                let last = ((b.ceil() as usize).min(src)).max(first + 1);
                let weights: Vec<f32> = (first..last).map(|j| ((j as f32 + 1.0).min(b) - (j as f32).max(a)).max(0.0) / r).collect();
                Tap { start: first.min(src - 1), weights }
            } else {
                let c = ((i as f32 + 0.5) * r - 0.5).max(0.0);
                let lo = (c.floor() as usize).min(src - 1);
                let hi = (lo + 1).min(src - 1);
                let f = c - lo as f32;
                if hi == lo { Tap { start: lo, weights: vec![1.0] } } else { Tap { start: lo, weights: vec![1.0 - f, f] } }
            }
        })
        .collect()
}

fn iou(a: &Face, b: &Face) -> f32 {
    let (w, h) = ((a.x1.min(b.x1) - a.x0.max(b.x0)).max(0.0), (a.y1.min(b.y1) - a.y0.max(b.y0)).max(0.0));
    let inter = w * h;
    let union = (a.x1 - a.x0) * (a.y1 - a.y0) + (b.x1 - b.x0) * (b.y1 - b.y0) - inter;
    if union > 0.0 { inter / union } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real model, for the tests that need it: `LC_YUNET_MODEL=<face_detection_yunet_2023mar.onnx>` (the file
    /// Settings ▸ Faces downloads; nothing is committed). Without it those tests say so and pass.
    fn model() -> Option<Vec<u8>> {
        let path = std::env::var_os("LC_YUNET_MODEL")?;
        let bytes = std::fs::read(path).ok()?;
        assert_eq!(crate::hash::sha256_hex(&bytes), crate::known::YUNET_SHA256, "LC_YUNET_MODEL is not YuNet 2023mar");
        Some(bytes)
    }

    fn detector() -> Option<Detector> {
        let Some(bytes) = model() else {
            lightcraft_testkit::skip!(Model, "LC_YUNET_MODEL is not set");
            return None;
        };
        Some(Detector::new(&bytes).unwrap())
    }

    #[test]
    fn the_model_loads_and_has_a_square_input() {
        let Some(d) = detector() else { return };
        assert_eq!(d.side, 640);
        assert!(d.keeps_photo_shape(), "{}", d.input_mode());
    }

    #[test]
    fn a_blank_picture_has_no_faces_and_odd_sizes_are_handled() {
        let Some(d) = detector() else { return };
        for (w, h) in [(640, 480), (100, 700), (1, 1), (37, 41), (1000, 20)] {
            let rgb = vec![128u8; w * h * 3];
            let faces = d.detect(&rgb, w, h, &Options::default()).unwrap();
            assert!(faces.is_empty(), "{w}×{h}: {faces:?}");
        }
    }

    #[test]
    fn bad_input_is_an_error_not_a_panic() {
        let d = Detector::new(&crate::synthetic::tiny_detector_model()).unwrap();
        let o = Options::default();
        assert!(d.detect(&[], 0, 0, &o).is_err());
        assert!(d.detect(&[0; 12], 2, 3, &o).is_err(), "wrong byte count");
        assert!(d.detect(&[0; 3], usize::MAX, 2, &o).is_err(), "overflowing size");
        assert!(d.detect(&[0; 3], MAX_SIDE + 1, 1, &o).is_err());
        let nan = Options { score: f32::NAN, nms_iou: f32::INFINITY, max_faces: usize::MAX, long_edge: usize::MAX };
        assert!(d.detect(&vec![0u8; 12 * 12 * 3], 12, 12, &nan).is_ok(), "non-finite options fall back to defaults");
    }

    #[test]
    fn missing_decoder_outputs_are_refused_at_load_time() {
        let mut bytes = crate::synthetic::tiny_detector_model();
        let mut i = 0;
        while i + 5 <= bytes.len() {
            if &bytes[i..i + 5] == b"cls_8" {
                bytes[i..i + 5].copy_from_slice(b"bad_8");
            }
            i += 1;
        }
        let error = Detector::new(&bytes).err().unwrap().to_string();
        assert!(error.contains("cls_8"), "{error}");
    }

    #[test]
    fn a_model_that_is_not_yunet_shaped_is_refused() {
        let d = Detector::new(&crate::synthetic::tiny_detector_model()).unwrap();
        assert!(d.detect(&[128; 3], 1, 1, &Options::default()).unwrap().is_empty());
        assert!(Detector::new(b"nonsense").is_err());
        assert!(Detector::new(&crate::synthetic::embedder_model(512)).is_err());
        if let Some(bytes) = model() {
            for n in [0, 1, 100, bytes.len() / 2, bytes.len() - 1] {
                assert!(Detector::new(&bytes[..n]).is_err(), "truncated at {n}");
            }
        }
    }

    /// Opt-in: `LC_YUNET_REF=<folder> cargo test -p lightcraft-faces -- --ignored --nocapture` compares every output
    /// of the network with ONNX Runtime's on the same input at five shapes: the declared square (`input.bin`,
    /// `out_<name>.bin`), a non-square 704 high by 1024 wide input (`input_704x1024.bin`, `out_<name>_704x1024.bin`)
    /// and the extremes a scan can feed it: 32 × 1024, 1024 × 32 and 32 × 32 (same naming, height first), all raw
    /// little-endian f32. `tools/yunet_reference.py` writes them.
    #[test]
    #[ignore = "needs reference files: set LC_YUNET_REF"]
    fn the_network_matches_onnx_runtime() {
        let Some(dir) = std::env::var_os("LC_YUNET_REF") else { return };
        let dir = std::path::PathBuf::from(dir);
        let floats =
            |name: &str| -> Vec<f32> { std::fs::read(dir.join(name)).unwrap().as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect() };
        let Some(d) = detector() else { return };
        assert!(d.keeps_photo_shape(), "the real YuNet must pass the non-square check at load: {}", d.input_mode());
        for (suffix, h, w) in [("", 640, 640), ("_704x1024", 704, 1024), ("_32x1024", 32, 1024), ("_1024x32", 1024, 32), ("_32x32", 32, 32)] {
            let started = std::time::Instant::now();
            let outs = d.net.run(Tensor::new(vec![1, 3, h, w], floats(&format!("input{suffix}.bin"))).unwrap()).unwrap();
            println!("network run at {w} × {h}: {:?}", started.elapsed());
            assert_eq!(outs.len(), 12);
            for (name, t) in outs {
                let want = floats(&format!("out_{name}{suffix}.bin"));
                assert_eq!(t.data.len(), want.len(), "{name} at {w} × {h}");
                let worst = t.data.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
                println!("{name} at {w} × {h}: {} values, worst difference {worst:e}", want.len());
                assert!(worst < 2e-3, "{name} at {w} × {h} differs by {worst}");
            }
        }
    }

    #[test]
    fn the_input_keeps_the_photo_shape_and_never_enlarges_it() {
        // a 24 MP photo shrinks to the long edge, padded to a multiple of 32
        assert_eq!(Detector::input_size(6000, 4000, 1280), ((1280, 853), (1280, 864)));
        // a WIDER FACE photo smaller than the long edge is looked at at its own size (the published protocol)
        assert_eq!(Detector::input_size(1024, 683, 1280), ((1024, 683), (1024, 704)));
        assert_eq!(Detector::input_size(683, 1024, 640), ((427, 640), (448, 640)));
        // tiny pictures and silly long edges stay inside the network's limits
        assert_eq!(Detector::input_size(1, 1, 1280), ((1, 1), (32, 32)));
        assert_eq!(Detector::input_size(100, 100, 0), ((32, 32), (32, 32)));
        assert_eq!(Detector::input_size(65_536, 10, usize::MAX), ((MAX_INPUT, 1), (MAX_INPUT, 32)));
        // a scan looks at 1024 px
        assert_eq!(Options::default().long_edge, 1024);
    }

    #[test]
    fn every_input_size_runs_and_a_blank_photo_stays_blank() {
        let d = Detector::new(&crate::synthetic::tiny_detector_model()).unwrap();
        for (w, h, long) in [(900, 600, 320), (900, 600, 640), (900, 600, 1280), (600, 900, 1024), (37, 41, 1024)] {
            let o = Options { long_edge: long, ..Options::default() };
            assert!(d.detect(&vec![128u8; w * h * 3], w, h, &o).unwrap().is_empty(), "{w}×{h} at {long}");
        }
    }

    /// Face boxes of `model` on a black `w` × `h` photo with a white patch `x0..x1` × `y0..y1` (photo pixels).
    fn faces_on_patch(model: &[u8], (w, h): (usize, usize), (x0, y0, x1, y1): (usize, usize, usize, usize)) -> Vec<Face> {
        let mut rgb = vec![0u8; w * h * 3];
        for y in y0..y1 {
            rgb[(y * w + x0) * 3..(y * w + x1) * 3].fill(255);
        }
        Detector::new(model).unwrap().detect(&rgb, w, h, &Options::default()).unwrap()
    }

    #[test]
    fn a_box_lands_on_the_bright_patch_in_a_non_square_photo() {
        // a 1500 × 700 photo (3:2 and wider): the patch is at x 1100..1140, y 390..430 of it: a different column than row index, so a swap shows
        let patch = (1100, 390, 1140, 430);
        let faces = faces_on_patch(&crate::synthetic::bright_patch_detector_model(), (1500, 700), patch);
        assert!(!faces.is_empty(), "the patch was not found");
        for f in &faces {
            // the cell sampled inside the patch, boxed 16 input pixels (about 23 photo pixels) wide and high
            let (cx, cy) = ((f.x0 + f.x1) / 2.0 * 1500.0, (f.y0 + f.y1) / 2.0 * 700.0);
            assert!((1088.0..1152.0).contains(&cx) && (378.0..442.0).contains(&cy), "box centre ({cx}, {cy}) is off the patch: {f:?}");
            let (bw, bh) = ((f.x1 - f.x0) * 1500.0, (f.y1 - f.y0) * 700.0);
            assert!((bw - 23.4).abs() < 1.5 && (bh - 23.4).abs() < 1.5, "box {bw} × {bh} px, expected about 23.4 × 23.4: {f:?}");
            let (lx, ly) = (f.landmarks[0].0 * 1500.0, f.landmarks[0].1 * 700.0);
            assert!((1088.0..1152.0).contains(&lx) && (378.0..442.0).contains(&ly), "landmark ({lx}, {ly}) is off the patch");
        }
        // the same patch in a tall photo (the transpose) is found at the transposed place
        let faces = faces_on_patch(&crate::synthetic::bright_patch_detector_model(), (700, 1500), (390, 1100, 430, 1140));
        assert!(!faces.is_empty());
        for f in &faces {
            let (cx, cy) = ((f.x0 + f.x1) / 2.0 * 700.0, (f.y0 + f.y1) / 2.0 * 1500.0);
            assert!((378.0..442.0).contains(&cx) && (1088.0..1152.0).contains(&cy), "tall photo: box centre ({cx}, {cy}) is off the patch");
        }
    }

    #[test]
    fn the_two_axes_scale_by_their_own_factor_on_a_thin_photo() {
        // 4000 × 40 is shrunk to 1024 × 10: horizontally by 0.256, vertically by 0.25, so a mixed-up pair of
        // factors moves a box by about 2.4% of its distance from the left, 48 px here
        let faces = faces_on_patch(&crate::synthetic::bright_patch_detector_model(), (4000, 40), (1900, 0, 1960, 40));
        assert!(!faces.is_empty());
        for f in &faces {
            let cx = (f.x0 + f.x1) / 2.0 * 4000.0;
            assert!((1900.0..1964.0).contains(&cx), "box centre x {cx} is off the patch (1900..1960): {f:?}");
        }
    }

    #[test]
    fn a_model_that_fails_at_other_shapes_is_run_in_its_declared_square() {
        let flexible = Detector::new(&crate::synthetic::tiny_detector_model()).unwrap();
        assert!(flexible.keeps_photo_shape(), "{}", flexible.input_mode());
        assert!(flexible.input_mode().contains("own shape"));
        // the literal Reshape targets fit 64 × 64 and nothing else
        let fixed = Detector::new(&crate::synthetic::fixed_shape_detector_model()).unwrap();
        assert!(!fixed.keeps_photo_shape());
        assert!(fixed.input_mode().contains("fitted into the model's 64 × 64 square"), "{}", fixed.input_mode());
        assert_eq!(fixed.input_side(), 64);
        // so every photo is fitted into 64 × 64 (never the 1024 a flexible model would take) and searching still works
        assert_eq!(fixed.plan(1500, 700, 1024), ((64, 30), (64, 64)));
        assert_eq!(fixed.plan(30, 700, 1024), ((3, 64), (64, 64)));
        assert_eq!(flexible.plan(1500, 700, 1024), ((1024, 478), (1024, 480)));
        for (w, h) in [(1500, 700), (700, 1500), (64, 64), (5, 9)] {
            assert!(fixed.detect(&vec![0u8; w * h * 3], w, h, &Options::default()).unwrap().is_empty(), "{w} × {h}");
        }
    }

    /// Right on square inputs (it leaves them to the CPU kernels) and finite but wrong on every other shape: what a
    /// height/width mix-up in a device's resize or transpose would give.
    struct ShapeBlind;
    impl crate::net::Accelerator for ShapeBlind {
        fn device(&self) -> String {
            "test".into()
        }
        fn conv(&self, j: &crate::net::ConvJob) -> Result<Option<Vec<f32>>, String> {
            if j.h == j.w { Ok(None) } else { Ok(Some(vec![0.5; j.cout * j.oh * j.ow])) }
        }
    }

    #[test]
    fn an_accelerator_is_checked_at_the_shapes_a_scan_runs() {
        // a model that takes photos in their own shape is checked on non-square inputs: one that is wrong there is dropped
        let d = Detector::new(&crate::synthetic::tiny_detector_model()).unwrap();
        assert!(d.keeps_photo_shape());
        let why = d.accelerate(std::sync::Arc::new(ShapeBlind), false).unwrap_err();
        assert!(why.contains("differs from the CPU's"), "{why}");
        assert!(d.net().accelerator().is_none(), "detection stays on the CPU");
        // a model run in its declared square is only ever fed squares: the square check is the right one for it
        let fixed = Detector::new(&crate::synthetic::fixed_shape_detector_model()).unwrap();
        assert!(!fixed.keeps_photo_shape());
        fixed.accelerate(std::sync::Arc::new(ShapeBlind), false).unwrap();
        assert!(fixed.net().accelerator().is_some());
    }

    /// Right (it leaves the layer to the CPU kernels) except where `wrong(h, w)` holds for a layer's input, where it is
    /// finite but wrong.
    struct WrongWhen {
        wrong: fn(usize, usize) -> bool,
    }
    impl crate::net::Accelerator for WrongWhen {
        fn device(&self) -> String {
            "test".into()
        }
        fn conv(&self, j: &crate::net::ConvJob) -> Result<Option<Vec<f32>>, String> {
            if (self.wrong)(j.h, j.w) { Ok(Some(vec![0.5; j.cout * j.oh * j.ow])) } else { Ok(None) }
        }
    }

    #[test]
    fn each_probe_shape_pins_the_accelerator_check() {
        let d = Detector::new(&crate::synthetic::tiny_detector_model()).unwrap();
        assert!(d.keeps_photo_shape());
        // wrong only on portrait inputs (704 x 1024 is landscape): only the 1024 x 704 probe can catch it
        let why = d.accelerate(std::sync::Arc::new(WrongWhen { wrong: |h, w| h > w }), false).unwrap_err();
        assert!(why.contains("differs from the CPU's"), "portrait: {why}");
        assert!(d.net().accelerator().is_none());
        // wrong only on the one-row grid of a panorama (1024 wide, 32 high): only that probe reaches it
        let why = d.accelerate(std::sync::Arc::new(WrongWhen { wrong: |h, w| w >= 32 * h }), false).unwrap_err();
        assert!(why.contains("differs from the CPU's"), "wide: {why}");
        assert!(d.net().accelerator().is_none());
        // right everywhere
        d.accelerate(std::sync::Arc::new(WrongWhen { wrong: |_, _| false }), false).unwrap();
        assert!(d.net().accelerator().is_some());
    }

    #[test]
    fn taps_cover_the_source_and_sum_to_one() {
        for (src, dst) in [(6000, 640), (640, 640), (37, 640), (3, 5), (1, 1), (1, 640), (641, 640)] {
            let t = taps(src, dst);
            assert_eq!(t.len(), dst);
            for tap in &t {
                assert!(tap.start + tap.weights.len() <= src, "{src}→{dst}");
                let sum: f32 = tap.weights.iter().sum();
                assert!((sum - 1.0).abs() < 1e-3, "{src}→{dst}: {sum}");
            }
        }
    }

    #[test]
    fn overlap_is_symmetric() {
        let f = |x0, y0, x1, y1| Face { x0, y0, x1, y1, score: 1.0, landmarks: [(0.0, 0.0); 5] };
        let (a, b) = (f(0.0, 0.0, 0.5, 0.5), f(0.25, 0.25, 0.75, 0.75));
        assert!((iou(&a, &b) - iou(&b, &a)).abs() < 1e-6 && (iou(&a, &b) - 0.0625 / 0.4375).abs() < 1e-5);
        assert_eq!(iou(&a, &f(0.6, 0.6, 0.9, 0.9)), 0.0);
    }
}
