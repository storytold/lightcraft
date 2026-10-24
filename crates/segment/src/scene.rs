//! Scene understanding: COCO panoptic segmentation with EoMT-S ("Your ViT is Secretly an Image
//! Segmentation Model", Kerssies et al., CVPR 2025; checkpoint
//! `tue-mps/coco_panoptic_eomt_small_640_2x`, MIT).
//!
//! One pass over the image gives, for each of the 133 COCO panoptic classes (`person`,
//! `sky-other-merged`, `tree-merged`, …), a probability map, and a list of segment
//! [`Instance`]s (so the UI can offer "person 1, person 2, …").
//!
//! The model is a DINOv2-style ViT-S/16 with 4 register tokens at 640 × 640: 200 learned
//! queries join the token sequence before the last 3 layers; a mask head and a 4× upscaler turn
//! the final tokens into 200 mask logits on a 160 × 160 grid and 134 class logits each
//! (133 classes + "no object"). The checkpoint's attention-mask annealing ends at probability
//! 0, so inference needs no masked attention and only the final prediction.
//!
//! Preprocessing (matching the reference evaluation): resize so the long edge is 640 (Pillow
//! bilinear), ImageNet normalization, zero padding at the bottom/right to 640 × 640. Outputs are
//! cropped to the image part of the 160 × 160 grid.
//!
//! Instances follow Mask2Former's panoptic post-processing, simplified: a query is a candidate
//! when its best real class has probability > [`INSTANCE_THRESHOLD`] (its score); each grid
//! cell goes to the candidate with the highest score × mask probability; a candidate is kept
//! when its mask (probability ≥ 0.5) is not empty and it wins more than 80 % of it (which drops
//! duplicate queries of the same object).
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the LightCraft contributors in 2026 from
//! the Python/PyTorch EoMT code of Hugging Face Transformers (`models/eomt/modeling_eomt.py`,
//! `image_processing_eomt*.py`), Copyright The HuggingFace Team and the EoMT authors (Tommie
//! Kerssies et al.); translated to Rust on candle and restructured. See NOTICE.

use std::path::Path;

use candle_core::{Device, Module, Tensor};
use candle_nn::{Conv2d, Conv2dConfig, LayerNorm, Linear, VarBuilder};

use crate::dino::{self, Block, EOMT_ATTN};
use crate::grid::{Filter, normalize_padded, resize_rgb, round_even};
use crate::nn::{ln, to_vec};
use crate::{Error, Result, best_device};

const WHAT: &str = "EoMT scene";
const MODEL_TYPE: &str = "eomt";
/// Side of the square model input.
pub const INPUT: usize = 640;
const PATCH: usize = 16;
const GRID: usize = INPUT / PATCH;
/// Side of the mask grid over the (padded) input.
pub const MASK_SIDE: usize = GRID * 4;
const HIDDEN: usize = 384;
const HEADS: usize = 6;
const LAYERS: usize = 12;
/// Layers that also see the queries (the last `num_blocks`).
const QUERY_BLOCKS: usize = 3;
const QUERIES: usize = 200;
const REGISTERS: usize = 4;
const PREFIX: usize = 1 + REGISTERS;
const EPS: f64 = 1e-6;
/// Number of classes (without "no object").
pub const CLASSES: usize = 133;
/// A query becomes an instance candidate when its best class probability exceeds this
/// (Mask2Former's panoptic default is 0.8; 0.5 keeps partly hidden people).
pub const INSTANCE_THRESHOLD: f32 = 0.5;
/// A candidate must win more than this fraction of its own mask against the others
/// (Mask2Former's `overlap_mask_area_threshold`).
const OVERLAP_THRESHOLD: f32 = 0.8;

/// The COCO panoptic labels, in class-index order (the checkpoint's `id2label`).
pub const COCO_PANOPTIC_LABELS: [&str; CLASSES] = [
    "person",
    "bicycle",
    "car",
    "motorcycle",
    "airplane",
    "bus",
    "train",
    "truck",
    "boat",
    "traffic light",
    "fire hydrant",
    "stop sign",
    "parking meter",
    "bench",
    "bird",
    "cat",
    "dog",
    "horse",
    "sheep",
    "cow",
    "elephant",
    "bear",
    "zebra",
    "giraffe",
    "backpack",
    "umbrella",
    "handbag",
    "tie",
    "suitcase",
    "frisbee",
    "skis",
    "snowboard",
    "sports ball",
    "kite",
    "baseball bat",
    "baseball glove",
    "skateboard",
    "surfboard",
    "tennis racket",
    "bottle",
    "wine glass",
    "cup",
    "fork",
    "knife",
    "spoon",
    "bowl",
    "banana",
    "apple",
    "sandwich",
    "orange",
    "broccoli",
    "carrot",
    "hot dog",
    "pizza",
    "donut",
    "cake",
    "chair",
    "couch",
    "potted plant",
    "bed",
    "dining table",
    "toilet",
    "tv",
    "laptop",
    "mouse",
    "remote",
    "keyboard",
    "cell phone",
    "microwave",
    "oven",
    "toaster",
    "sink",
    "refrigerator",
    "book",
    "clock",
    "vase",
    "scissors",
    "teddy bear",
    "hair drier",
    "toothbrush",
    "banner",
    "blanket",
    "bridge",
    "cardboard",
    "counter",
    "curtain",
    "door-stuff",
    "floor-wood",
    "flower",
    "fruit",
    "gravel",
    "house",
    "light",
    "mirror-stuff",
    "net",
    "pillow",
    "platform",
    "playingfield",
    "railroad",
    "river",
    "road",
    "roof",
    "sand",
    "sea",
    "shelf",
    "snow",
    "stairs",
    "tent",
    "towel",
    "wall-brick",
    "wall-stone",
    "wall-tile",
    "wall-wood",
    "water-other",
    "window-blind",
    "window-other",
    "tree-merged",
    "fence-merged",
    "ceiling-merged",
    "sky-other-merged",
    "cabinet-merged",
    "table-merged",
    "floor-other-merged",
    "pavement-merged",
    "mountain-merged",
    "grass-merged",
    "dirt-merged",
    "paper-merged",
    "food-other-merged",
    "building-other-merged",
    "rock-merged",
    "wall-other-merged",
    "rug-merged",
];

/// The class indices whose label satisfies `pred` (e.g. `|l| l.contains("sky")`).
pub fn class_ids(pred: impl Fn(&str) -> bool) -> Vec<usize> {
    COCO_PANOPTIC_LABELS.iter().enumerate().filter(|(_, l)| pred(l)).map(|(i, _)| i).collect()
}

/// Whether `dir` holds the EoMT checkpoint (`model.safetensors` and a `config.json` of
/// `model_type` `eomt`).
pub fn is_scene_dir(dir: &Path) -> bool {
    dino::is_model_dir(dir, MODEL_TYPE)
}

/// The segmentation of one image.
///
/// All maps are on the same `width × height` grid, which covers the image (the model's
/// 160 × 160 grid with the padding cropped off: 160 cells along the long edge, e.g. 160 × 107
/// for a 3:2 landscape photo). Cell `(x, y)` covers the image region
/// `[x / width, (x + 1) / width) × [y / height, (y + 1) / height)` in normalized coordinates.
#[derive(Clone, Debug)]
pub struct Scene {
    pub width: usize,
    pub height: usize,
    /// Semantic probabilities, flat `[class][y][x]` (`CLASSES × height × width`): for each
    /// class, the sum over queries of P(class | query) · mask probability (the reference
    /// semantic post-processing); about 0..1, not normalized across classes.
    pub probs: Vec<f32>,
    /// Detected segments (things and stuff), by decreasing score.
    pub instances: Vec<Instance>,
}

/// One detected segment (a query whose best class is confident).
#[derive(Clone, Debug)]
pub struct Instance {
    /// Index into [`COCO_PANOPTIC_LABELS`].
    pub class: usize,
    /// The class probability (0..1).
    pub score: f32,
    /// Sigmoid mask probabilities on the scene's `width × height` grid.
    pub mask: Vec<f32>,
}

impl Scene {
    /// The probability map of a set of classes (summed, clamped to 0..1), `width × height`.
    pub fn class_probability(&self, classes: &[usize]) -> Vec<f32> {
        let n = self.width.saturating_mul(self.height);
        let mut out = vec![0f32; n];
        for &c in classes {
            let Some(p) = self.probs.get(c.saturating_mul(n)..c.saturating_add(1).saturating_mul(n)) else { continue };
            for (o, v) in out.iter_mut().zip(p) {
                *o += v;
            }
        }
        out.iter_mut().for_each(|v| *v = v.clamp(0.0, 1.0));
        out
    }

    /// The label of a class index (`"?"` when out of range).
    pub fn label(class: usize) -> &'static str {
        COCO_PANOPTIC_LABELS.get(class).copied().unwrap_or("?")
    }
}

/// `EomtScaleLayer`: ConvTranspose2d(k2, s2) → GELU → depthwise 3 × 3 → LayerNorm2d.
struct ScaleLayer {
    up_w: Tensor,
    up_b: Tensor,
    dw: Tensor,
    norm_w: Tensor,
    norm_b: Tensor,
}

impl ScaleLayer {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let x = dino::conv_transpose_patches(x, &self.up_w, &self.up_b, true)?;
        let x = dino::depthwise3x3(&x, &self.dw)?;
        dino::layer_norm2d(&x, &self.norm_w, &self.norm_b, EPS)
    }
}

/// Raw model outputs (tests and tools).
pub struct Raw {
    /// `[1, 200, 160, 160]` mask logits over the padded 640 × 640 input.
    pub mask_logits: Tensor,
    /// `[1, 200, 134]` class logits (the last is "no object").
    pub class_logits: Tensor,
    /// Intermediate tensors by name (`emb`, `layer8`, `final`, `upscaled`).
    pub stages: Vec<(&'static str, Tensor)>,
}

/// A loaded EoMT-S model.
pub struct SceneModel {
    device: Device,
    patch: Conv2d,
    pos: Tensor,
    prefix: Tensor,
    query: Tensor,
    layers: Vec<Block>,
    norm: LayerNorm,
    class: Linear,
    mask_head: [Linear; 3],
    upscale: Vec<ScaleLayer>,
}

impl SceneModel {
    /// Load from a directory holding `model.safetensors` and `config.json` of the
    /// `tue-mps/coco_panoptic_eomt_small_640_2x` checkpoint, on the best device here.
    pub fn load(dir: &Path) -> Result<Self> {
        Self::load_on(dir, best_device())
    }

    pub fn load_on(dir: &Path, device: Device) -> Result<Self> {
        let (vb, config) = dino::open_checkpoint(dir, WHAT, MODEL_TYPE, &device)?;
        for (key, want) in [
            ("hidden_size", HIDDEN),
            ("num_hidden_layers", LAYERS),
            ("num_attention_heads", HEADS),
            ("patch_size", PATCH),
            ("image_size", INPUT),
            ("num_queries", QUERIES),
            ("num_register_tokens", REGISTERS),
            ("num_blocks", QUERY_BLOCKS),
        ] {
            let got = dino::config_usize(&config, &[key], WHAT)?;
            if got != want {
                return Err(Error::Message(format!(
                    "{WHAT} model: config.json has {key} = {got}; only EoMT-S at 640 px ({key} = {want}) is supported"
                )));
            }
        }
        dino::named(WHAT, Self::build(vb, device))
    }

    fn build(vb: VarBuilder, device: Device) -> Result<Self> {
        let emb = vb.pp("embeddings");
        let cfg = Conv2dConfig { stride: PATCH, ..Default::default() };
        let patch = candle_nn::conv2d(3, HIDDEN, PATCH, cfg, emb.pp("patch_embeddings.projection"))?;
        let pos = emb.get((GRID * GRID, HIDDEN), "position_embeddings.weight")?.unsqueeze(0)?;
        let cls = emb.get((1, 1, HIDDEN), "cls_token")?;
        let reg = emb.get((1, REGISTERS, HIDDEN), "register_tokens")?;
        let prefix = Tensor::cat(&[cls, reg], 1)?;
        let query = vb.get((QUERIES, HIDDEN), "query.weight")?.unsqueeze(0)?;
        let mut layers = Vec::with_capacity(LAYERS);
        for i in 0..LAYERS {
            layers.push(Block::new(HIDDEN, HEADS, EPS, &EOMT_ATTN, vb.pp(format!("layers.{i}")))?);
        }
        let mh = vb.pp("mask_head");
        let mask_head = [
            candle_nn::linear(HIDDEN, HIDDEN, mh.pp("fc1"))?,
            candle_nn::linear(HIDDEN, HIDDEN, mh.pp("fc2"))?,
            candle_nn::linear(HIDDEN, HIDDEN, mh.pp("fc3"))?,
        ];
        let mut upscale = Vec::new();
        for i in 0..2 {
            let b = vb.pp(format!("upscale_block.block.{i}"));
            upscale.push(ScaleLayer {
                up_w: b.get((HIDDEN, HIDDEN, 2, 2), "conv1.weight")?,
                up_b: b.get(HIDDEN, "conv1.bias")?,
                dw: b.get((HIDDEN, 1, 3, 3), "conv2.weight")?,
                norm_w: b.get(HIDDEN, "layernorm2d.weight")?,
                norm_b: b.get(HIDDEN, "layernorm2d.bias")?,
            });
        }
        Ok(Self {
            patch,
            pos,
            prefix,
            query,
            layers,
            norm: ln(HIDDEN, EPS, vb.pp("layernorm"))?,
            class: candle_nn::linear(HIDDEN, CLASSES + 1, vb.pp("class_predictor"))?,
            mask_head,
            upscale,
            device,
        })
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// The model on preprocessed pixels `[1, 3, 640, 640]` (ImageNet-normalized, padded).
    pub fn raw(&self, pixels: &Tensor) -> Result<Raw> {
        let pixels = pixels.to_device(&self.device)?;
        let mut stages = Vec::new();
        let mut prof = dino::Profile::new("scene");
        let x = self.patch.forward(&pixels)?.flatten_from(2)?.transpose(1, 2)?.broadcast_add(&self.pos)?;
        let mut x = Tensor::cat(&[&self.prefix, &x], 1)?;
        stages.push(("emb", x.clone()));
        prof.mark("embeddings", &x);
        for (i, layer) in self.layers.iter().enumerate() {
            if i == LAYERS - QUERY_BLOCKS {
                x = Tensor::cat(&[&self.query, &x], 1)?;
            }
            x = layer.forward(&x)?;
            if i + 1 == LAYERS - QUERY_BLOCKS {
                stages.push(("layer8", x.clone()));
            }
        }
        let x = self.norm.forward(&x)?;
        stages.push(("final", x.clone()));
        prof.mark("layers", &x);
        // EomtForUniversalSegmentation.predict
        let q = x.narrow(1, 0, QUERIES)?;
        let class_logits = self.class.forward(&q)?;
        let mut p = x.narrow(1, QUERIES + PREFIX, GRID * GRID)?.transpose(1, 2)?.reshape((1, HIDDEN, GRID, GRID))?;
        for l in &self.upscale {
            p = l.forward(&p)?;
        }
        stages.push(("upscaled", p.clone()));
        prof.mark("upscale", &p);
        let [f1, f2, f3] = &self.mask_head;
        let q = f3.forward(&dino::gelu(&f2.forward(&dino::gelu(&f1.forward(&q)?)?)?)?)?;
        let mask_logits = q.squeeze(0)?.matmul(&p.reshape((HIDDEN, MASK_SIDE * MASK_SIDE))?)?.reshape((1, QUERIES, MASK_SIDE, MASK_SIDE))?;
        prof.mark("mask head", &mask_logits);
        Ok(Raw { mask_logits, class_logits, stages })
    }

    /// The model input for an RGB image (8-bit, row-major, `w × h`): `[3, 640, 640]` values and
    /// the size of the image part (`iw × ih`, top-left).
    pub fn preprocess(rgb: &[u8], w: usize, h: usize) -> Result<(Vec<f32>, usize, usize)> {
        let long = w.max(h).max(1) as f64;
        let s = INPUT as f64 / long;
        let iw = round_even(w as f64 * s).clamp(1, INPUT);
        let ih = round_even(h as f64 * s).clamp(1, INPUT);
        let small = resize_rgb(rgb, w, h, iw, ih, Filter::Bilinear)?;
        Ok((normalize_padded(&small, iw, ih, INPUT, INPUT)?, iw, ih))
    }

    /// Segment an RGB image (8-bit, row-major, `w × h`).
    pub fn segment(&self, rgb: &[u8], w: usize, h: usize) -> Result<Scene> {
        let mut prof = dino::Profile::new("scene");
        let (pixels, iw, ih) = Self::preprocess(rgb, w, h)?;
        let pixels = Tensor::from_vec(pixels, (1, 3, INPUT, INPUT), &self.device)?;
        prof.mark("preprocess", &pixels);
        let raw = self.raw(&pixels)?;
        let (cw, ch) = mask_crop(iw, ih);
        let scene = postprocess(&raw, cw, ch)?;
        prof.mark("model + postprocess", &pixels);
        Ok(scene)
    }
}

/// The mask cells (`cw × ch`, top-left) covering an `iw × ih` image part of the input.
pub fn mask_crop(iw: usize, ih: usize) -> (usize, usize) {
    let f = MASK_SIDE as f64 / INPUT as f64;
    (round_even(iw as f64 * f).clamp(1, MASK_SIDE), round_even(ih as f64 * f).clamp(1, MASK_SIDE))
}

/// Semantic maps and instances from the raw outputs, cropped to the top-left `cw × ch` cells.
pub fn postprocess(raw: &Raw, cw: usize, ch: usize) -> Result<Scene> {
    let (cw, ch) = (cw.clamp(1, MASK_SIDE), ch.clamp(1, MASK_SIDE));
    let n = cw * ch;
    let class_p = candle_nn::ops::softmax_last_dim(&raw.class_logits.squeeze(0)?)?; // [Q, 134]
    let masks = raw.mask_logits.squeeze(0)?.narrow(1, 0, ch)?.narrow(2, 0, cw)?;
    let masks = candle_nn::ops::sigmoid(&masks.contiguous()?)?.reshape((QUERIES, n))?;
    let probs = class_p.narrow(1, 0, CLASSES)?.t()?.contiguous()?.matmul(&masks)?; // [133, n]
    let probs = to_vec(&probs)?;
    let class_p = to_vec(&class_p)?;
    let masks = to_vec(&masks)?;

    let mut cands: Vec<(usize, usize, f32)> = Vec::new(); // (query, class, score)
    for q in 0..QUERIES {
        let Some(p) = class_p.get(q * (CLASSES + 1)..q * (CLASSES + 1) + CLASSES) else { continue };
        let (class, score) = p.iter().enumerate().fold((0, f32::MIN), |b, (c, v)| if *v > b.1 { (c, *v) } else { b });
        if score > INSTANCE_THRESHOLD {
            cands.push((q, class, score));
        }
    }
    let mask_of = |q: usize| masks.get(q * n..(q + 1) * n).unwrap_or(&[]);
    // each cell goes to the candidate with the highest score × mask probability
    let mut owner = vec![usize::MAX; n];
    let mut best = vec![0f32; n];
    for (i, (q, _, s)) in cands.iter().enumerate() {
        for ((m, b), o) in mask_of(*q).iter().zip(best.iter_mut()).zip(owner.iter_mut()) {
            if s * m > *b {
                *b = s * m;
                *o = i;
            }
        }
    }
    let mut instances = Vec::new();
    for (i, (q, class, score)) in cands.iter().enumerate() {
        let m = mask_of(*q);
        let area = m.iter().filter(|v| **v >= 0.5).count();
        let won = m.iter().zip(&owner).filter(|(v, o)| **v >= 0.5 && **o == i).count();
        if area > 0 && won as f32 > OVERLAP_THRESHOLD * area as f32 {
            instances.push(Instance { class: *class, score: *score, mask: m.to_vec() });
        }
    }
    instances.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(Scene { width: cw, height: ch, probs, instances })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_ids() {
        assert_eq!(COCO_PANOPTIC_LABELS[0], "person");
        assert_eq!(class_ids(|l| l == "person"), vec![0]);
        assert_eq!(class_ids(|l| l.contains("sky")), vec![119]);
        assert_eq!(Scene::label(119), "sky-other-merged");
        assert_eq!(Scene::label(500), "?");
    }

    #[test]
    fn missing_or_wrong_dirs_are_errors() {
        let dir = std::env::temp_dir().join(format!("lc-scene-missing-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        assert!(!is_scene_dir(&dir));
        let e = SceneModel::load_on(&dir, Device::Cpu).err().unwrap().to_string();
        assert!(e.contains("not found"), "{e}");
        std::fs::write(dir.join("model.safetensors"), b"garbage").unwrap();
        std::fs::write(dir.join("config.json"), br#"{"model_type": "depth_anything"}"#).unwrap();
        assert!(!is_scene_dir(&dir));
        let e = SceneModel::load_on(&dir, Device::Cpu).err().unwrap().to_string();
        assert!(e.contains("not EoMT"), "{e}");
        std::fs::write(dir.join("config.json"), br#"{"model_type": "eomt", "hidden_size": 384}"#).unwrap();
        assert!(is_scene_dir(&dir));
        assert!(SceneModel::load_on(&dir, Device::Cpu).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn preprocess_letterboxes_the_long_edge() {
        let rgb = vec![128u8; 300 * 200 * 3];
        let (p, iw, ih) = SceneModel::preprocess(&rgb, 300, 200).unwrap();
        assert_eq!((iw, ih), (640, 427));
        assert_eq!(mask_crop(iw, ih), (160, 107));
        assert_eq!(p.len(), 3 * INPUT * INPUT);
        assert_eq!(p[(INPUT - 1) * INPUT], 0.0); // padding below the image
        assert!((p[0] - (128.0 / 255.0 - 0.485) / 0.229).abs() < 1e-5);
        assert!(SceneModel::preprocess(&rgb, 0, 200).is_err());
    }

    #[test]
    fn class_probability_sums_and_clamps() {
        let mut probs = vec![0f32; CLASSES * 2];
        probs[0] = 0.7;
        probs[2] = 0.6;
        probs[3] = 0.1;
        let s = Scene { width: 2, height: 1, probs, instances: vec![] };
        assert_eq!(s.class_probability(&[0, 1]), vec![1.0, 0.1]);
        assert_eq!(s.class_probability(&[999]), vec![0.0, 0.0]);
    }

    #[test]
    fn postprocess_keeps_confident_distinct_instances() {
        let dev = Device::Cpu;
        let mut cls = vec![-10f32; QUERIES * (CLASSES + 1)];
        for q in 0..QUERIES {
            cls[q * (CLASSES + 1) + CLASSES] = 10.0; // no object
        }
        // query 0: a confident person on the left half; query 1: a weaker duplicate of it
        cls[CLASSES] = -10.0;
        cls[0] = 10.0;
        cls[(CLASSES + 1) + CLASSES] = 0.0;
        cls[CLASSES + 1] = 1.0;
        let mut masks = vec![-10f32; QUERIES * MASK_SIDE * MASK_SIDE];
        for q in 0..2 {
            for y in 0..MASK_SIDE {
                for x in 0..MASK_SIDE / 2 {
                    masks[(q * MASK_SIDE + y) * MASK_SIDE + x] = 10.0;
                }
            }
        }
        let raw = Raw {
            mask_logits: Tensor::from_vec(masks, (1, QUERIES, MASK_SIDE, MASK_SIDE), &dev).unwrap(),
            class_logits: Tensor::from_vec(cls, (1, QUERIES, CLASSES + 1), &dev).unwrap(),
            stages: vec![],
        };
        let s = postprocess(&raw, 160, 100).unwrap();
        assert_eq!((s.width, s.height), (160, 100));
        assert_eq!(s.instances.len(), 1);
        assert_eq!(s.instances[0].class, 0);
        assert_eq!(s.instances[0].mask.len(), 160 * 100);
        let person = s.class_probability(&[0]);
        assert!(person[0] > 0.99 && person[159] < 0.01);
    }
}
