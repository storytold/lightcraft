//! The YuNet face detector (232 KB, MIT, downloaded by the user in Settings ▸ Faces), run by the app's own small
//! interpreter ([`crate::net`]): no extra runtime.
//!
//! A photo is letterboxed (aspect ratio kept, padded at the right and bottom) into the network's square
//! input, the network gives a score, a box and five landmarks for every cell of three grids (strides 8, 16 and
//! 32), and the cells above the score threshold are decoded, de-duplicated by non-maximum suppression and
//! mapped back to the photo as fractions of its width and height.
//!
//! The pixels come from the caller as plain 8-bit RGB; this module has no image types of its own.

use crate::graph;
use crate::net::{Net, NetError, Tensor};

const STRIDES: [usize; 3] = [8, 16, 32];
/// Largest photo side we accept.
const MAX_SIDE: usize = 65_536;
/// Candidate cells kept before suppression, so a hostile or noisy output cannot make it slow.
const MAX_CANDIDATES: usize = 4000;

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
}

impl Default for Options {
    fn default() -> Self {
        Options { score: 0.6, nms_iou: 0.3, max_faces: 200 }
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

pub struct Detector {
    net: Net,
    /// Side of the square network input.
    side: usize,
}

impl Detector {
    /// A detector from YuNet-architecture ONNX bytes (a retrained model of the same shape works too).
    pub fn new(onnx: &[u8]) -> Result<Detector, DetectError> {
        let g = graph::load(onnx).map_err(|e| DetectError::Model(e.to_string()))?;
        let outputs = g.outputs.clone();
        let net = Net::new(g)?;
        let side = match net.input_shape() {
            Some(&[1, 3, h, w]) if h == w && (64..=4096).contains(&h) && h % 32 == 0 => h,
            _ => return Err(DetectError::Model("the model's input is not a fixed square image [1, 3, N, N] with N a multiple of 32".into())),
        };
        let shapes = net.validate_input(&[1, 3, side, side])?;
        for stride in STRIDES {
            for (kind, components) in [("cls", 1), ("obj", 1), ("bbox", 4), ("kps", 10)] {
                let name = format!("{kind}_{stride}");
                let index = outputs.iter().position(|n| n == &name).ok_or_else(|| DetectError::Model(format!("the model has no output `{name}`")))?;
                let count = shapes.get(index).and_then(|s| s.iter().try_fold(1usize, |n, d| n.checked_mul(*d)));
                if count != Some((side / stride) * (side / stride) * components) {
                    return Err(DetectError::Model(format!("the output `{name}` has the wrong shape")));
                }
            }
        }
        Ok(Detector { net, side })
    }

    /// The square input side, used to check an installed manifest.
    pub fn input_side(&self) -> usize {
        self.side
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
        };
        let scale = (self.side as f32 / width as f32).min(self.side as f32 / height as f32);
        let input = self.letterbox(rgb, width, height, scale)?;
        let outputs = self.net.run(input)?;
        let find = |name: String| {
            outputs.iter().find(|(n, _)| *n == name).map(|(_, t)| t).ok_or_else(|| DetectError::Model(format!("the model has no output `{name}`")))
        };
        let mut candidates: Vec<Face> = Vec::new();
        for stride in STRIDES {
            let (cls, obj) = (find(format!("cls_{stride}"))?, find(format!("obj_{stride}"))?);
            let (bbox, kps) = (find(format!("bbox_{stride}"))?, find(format!("kps_{stride}"))?);
            let cols = self.side / stride;
            let cells = cols * cols;
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
                    *l = (((lx + col) * s) / scale / width as f32, ((ly + row) * s) / scale / height as f32);
                }
                candidates.push(Face {
                    x0: (cx - w / 2.0) / scale / width as f32,
                    y0: (cy - h / 2.0) / scale / height as f32,
                    x1: (cx + w / 2.0) / scale / width as f32,
                    y1: (cy + h / 2.0) / scale / height as f32,
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

    /// The photo scaled by `scale` into the top left of a zero-filled square, as BGR 0..255 (the model's own input).
    fn letterbox(&self, rgb: &[u8], width: usize, height: usize, scale: f32) -> Result<Tensor, DetectError> {
        let side = self.side;
        let nw = ((width as f32 * scale).round() as usize).clamp(1, side);
        let nh = ((height as f32 * scale).round() as usize).clamp(1, side);
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
        let plane = side * side;
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
                let at = y * side + x;
                for (c, v) in [acc[2], acc[1], acc[0]].into_iter().enumerate() {
                    if let Some(o) = data.get_mut(c * plane + at) {
                        *o = v;
                    }
                }
            }
        }
        Ok(Tensor::new(vec![1, 3, side, side], data)?)
    }
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
            eprintln!("LC_YUNET_MODEL is not set: skipping");
            return None;
        };
        Some(Detector::new(&bytes).unwrap())
    }

    #[test]
    fn the_model_loads_and_has_a_square_input() {
        let Some(d) = detector() else { return };
        assert_eq!(d.side, 640);
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
        let nan = Options { score: f32::NAN, nms_iou: f32::INFINITY, max_faces: usize::MAX };
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

    /// Opt-in: `LC_YUNET_REF=<folder> cargo test -p dac-faces -- --ignored --nocapture` compares every output
    /// of the network with ONNX Runtime's on the same input (`input.bin`, `out_<name>.bin`, raw little-endian f32).
    #[test]
    #[ignore = "needs reference files: set LC_YUNET_REF"]
    fn the_network_matches_onnx_runtime() {
        let Some(dir) = std::env::var_os("LC_YUNET_REF") else { return };
        let dir = std::path::PathBuf::from(dir);
        let floats =
            |name: &str| -> Vec<f32> { std::fs::read(dir.join(name)).unwrap().as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect() };
        let Some(d) = detector() else { return };
        let started = std::time::Instant::now();
        let outs = d.net.run(Tensor::new(vec![1, 3, 640, 640], floats("input.bin")).unwrap()).unwrap();
        println!("network run: {:?}", started.elapsed());
        assert_eq!(outs.len(), 12);
        for (name, t) in outs {
            let want = floats(&format!("out_{name}.bin"));
            assert_eq!(t.data.len(), want.len(), "{name}");
            let worst = t.data.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            println!("{name}: {} values, worst difference {worst:e}", want.len());
            assert!(worst < 2e-3, "{name} differs by {worst}");
        }
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
