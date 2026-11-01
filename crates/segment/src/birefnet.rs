//! BiRefNet (Zheng et al., "Bilateral Reference for High-Resolution Dichotomous Image
//! Segmentation", 2024): subject / foreground matting at 1024 × 1024, on the CPU in plain Rust
//! (candle for the matrix products).
//!
//! The configuration ported is the released one (`BiRefNet`, `BiRefNet-portrait`,
//! `BiRefNet_lite`, …): a Swin v1 backbone run on the image and on its half-size copy with the
//! features concatenated (`mul_scl_ipt='cat'`), the three shallower levels pooled into the
//! deepest (`cxt_num=3`), a squeeze block, and a decoder of `BasicDecBlk`s with
//! deformable-convolution ASPP attention, 1 × 1 lateral blocks, image patches fed to every
//! level (`dec_ipt`, split into a grid), and the gradient-attention gates (`out_ref`) that also
//! act at inference. Training-only heads (multi-scale supervision, gradient predictions) are
//! not run. Eval-mode BatchNorms are folded into their convolutions.
//!
//! Two exact algebraic rewrites save memory and time without changing the result (beyond
//! float rounding): the final 1 × 1 conv is applied before the last bilinear upsampling (both
//! are linear), and it is folded into the last conv of the full-resolution image branch.
//!
//! Modified work (MIT): ported by the LightCraft contributors in 2026 from the BiRefNet code
//! (`models/birefnet.py`, `models/modules/{decoder_blocks,lateral_blocks,aspp,deform_conv}.py`,
//! `config.py`; <https://github.com/ZhengPeng7/BiRefNet>, Copyright (c) 2024 ZhengPeng, MIT
//! licence); translated to Rust and restructured. See NOTICE.

use std::path::Path;
use std::sync::Arc;

use candle_core::Device;
use rayon::prelude::*;

use crate::deform::{Conv, DeformConv};
use crate::feat::{Act, Dense, Map, Params, Profile, add_assign, cat, resize, sigmoid};
use crate::swin::Swin;
use crate::weights::Weights;
use crate::{Error, Result, WEIGHTS_FILE};

/// Side of the square model input and of the predicted logits.
pub const BIREFNET_SIDE: usize = 1024;
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// Whether `dir` holds a BiRefNet checkpoint (`model.safetensors` with the BiRefNet tensors).
pub fn is_birefnet_dir(dir: &Path) -> bool {
    let path = dir.join(WEIGHTS_FILE);
    path.is_file() && Weights::open(&path).is_ok_and(|w| w.contains("bb.patch_embed.proj.weight") && w.contains("decoder.conv_out1.0.weight"))
}

/// Errors of the shared helpers, named for this model.
fn named(e: Error) -> Error {
    match e {
        Error::Model(s) => Error::BiRefNet(s),
        Error::Candle(e) => Error::BiRefNet(e.to_string()),
        e => e,
    }
}

/// `ASPPDeformable`: deformable 1 × 1, 1 × 1, 3 × 3 and 7 × 7 branches and a global-pool
/// branch, concatenated and mixed by a 1 × 1 conv (applied here as a sum of per-branch products).
struct Aspp {
    branches: Vec<DeformConv>,
    gap: Conv,
    /// The 1 × 1 mixing conv split by branch (the last one for the pooled branch).
    mix: Vec<Dense>,
    mix_bias: Vec<f32>,
}

impl Aspp {
    fn load(p: &Params, name: &str) -> Result<Self> {
        let deform = |b: &str| -> Result<DeformConv> {
            let a = format!("{name}.{b}.atrous_conv");
            let (ow, os) = p.get(&format!("{a}.offset_conv.weight"))?;
            let (mw, ms) = p.get(&format!("{a}.modulator_conv.weight"))?;
            let k = os.get(2).copied().unwrap_or(0);
            let (no, nm) = (os.first().copied().unwrap_or(0), ms.first().copied().unwrap_or(0));
            let ob = p.vec(&format!("{a}.offset_conv.bias"), no)?;
            let mb = p.vec(&format!("{a}.modulator_conv.bias"), nm)?;
            let mut shape = os.clone();
            if let Some(s) = shape.first_mut() {
                *s = no + nm;
            }
            let pad = k / 2;
            let offsets = Conv::new(&[ow, mw].concat(), &shape, Some(&[ob, mb].concat()), None, pad, &p.dev)?;
            let conv = p.conv(&format!("{a}.regular_conv"), pad, Some(&format!("{name}.{b}.bn")))?;
            DeformConv::new(offsets, conv)
        };
        let mut branches = vec![deform("aspp1")?];
        for i in 0..3 {
            branches.push(deform(&format!("aspp_deforms.{i}"))?);
        }
        let gap = p.conv(&format!("{name}.global_avg_pool.1"), 0, Some(&format!("{name}.global_avg_pool.2")))?;
        let (w, shape) = p.get(&format!("{name}.conv1.weight"))?;
        let (out, inp) = (shape.first().copied().unwrap_or(0), shape.get(1).copied().unwrap_or(0));
        let (scale, shift) = p.batch_norm(&format!("{name}.bn1"), out)?;
        let part = gap.cout;
        if part == 0 || inp != 5 * part || w.len() != out * inp {
            return Err(Error::Model(format!("{name}.conv1: shape {shape:?}")));
        }
        let mut mix = Vec::with_capacity(5);
        for b in 0..5 {
            let mut m = vec![0f32; out * part];
            for o in 0..out {
                for i in 0..part {
                    if let (Some(d), Some(s), Some(sc)) = (m.get_mut(o * part + i), w.get(o * inp + b * part + i), scale.get(o)) {
                        *d = s * sc;
                    }
                }
            }
            mix.push(Dense::from_out_in(&m, out, part, None, &p.dev)?);
        }
        Ok(Self { branches, gap, mix, mix_bias: shift })
    }

    fn forward(&self, x: &Map) -> Result<Map> {
        let out = self.mix_bias.len();
        let mut acc = vec![0f32; x.pixels() * out];
        for (b, mix) in self.branches.iter().zip(&self.mix) {
            let y = b.forward(x, Act::Relu)?;
            add_assign(&mut acc, &mix.forward(&y.data, Act::None)?);
        }
        // global average pool → 1 × 1 conv → BN → ReLU, broadcast (an align-corners upsampling
        // of a single pixel)
        let n = x.pixels().max(1) as f64;
        let mean: Vec<f32> = (0..x.c).map(|c| (x.data.iter().skip(c).step_by(x.c.max(1)).map(|v| f64::from(*v)).sum::<f64>() / n) as f32).collect();
        let g = self.gap.forward(&Map::new(1, 1, x.c, mean)?, Act::Relu)?;
        let mix = self.mix.last().ok_or_else(|| Error::Model("ASPP".into()))?;
        let g = mix.forward(&g.data, Act::None)?;
        let bias: Vec<f32> = g.iter().zip(&self.mix_bias).map(|(a, b)| a + b).collect();
        crate::feat::bias_act(&mut acc, out, Some(&bias), Act::Relu);
        Map::new(x.h, x.w, out, acc)
    }
}

/// `BasicDecBlk`: 3 × 3 conv → BN → ReLU → ASPP → 3 × 3 conv → BN.
struct DecBlk {
    conv_in: Conv,
    att: Aspp,
    conv_out: Conv,
}

impl DecBlk {
    fn load(p: &Params, name: &str) -> Result<Self> {
        Ok(Self {
            conv_in: p.conv(&format!("{name}.conv_in"), 1, Some(&format!("{name}.bn_in")))?,
            att: Aspp::load(p, &format!("{name}.dec_att"))?,
            conv_out: p.conv(&format!("{name}.conv_out"), 1, Some(&format!("{name}.bn_out")))?,
        })
    }

    fn forward(&self, x: &Map) -> Result<Map> {
        let y = self.conv_in.forward(x, Act::Relu)?;
        let y = self.att.forward(&y)?;
        self.conv_out.forward(&y, Act::None)
    }
}

/// `SimpleConvs`: two 3 × 3 convs.
struct Simple {
    conv1: Conv,
    conv_out: Conv,
}

impl Simple {
    fn load(p: &Params, name: &str) -> Result<Self> {
        Ok(Self { conv1: p.conv(&format!("{name}.conv1"), 1, None)?, conv_out: p.conv(&format!("{name}.conv_out"), 1, None)? })
    }

    fn forward(&self, x: &Map) -> Result<Map> {
        self.conv_out.forward(&self.conv1.forward(x, Act::None)?, Act::None)
    }
}

/// The gradient-attention gate: `p · sigmoid(conv1×1(relu(bn(conv3×3(p)))))`.
struct Gate {
    conv: Conv,
    attn: Dense,
}

impl Gate {
    fn load(p: &Params, i: usize) -> Result<Self> {
        let conv = p.conv(&format!("decoder.gdt_convs_{i}.0"), 1, Some(&format!("decoder.gdt_convs_{i}.1")))?;
        let (w, shape) = p.get(&format!("decoder.gdt_convs_attn_{i}.0.weight"))?;
        let b = p.vec(&format!("decoder.gdt_convs_attn_{i}.0.bias"), 1)?;
        if shape.first() != Some(&1) || w.len() != conv.cout {
            return Err(Error::Model(format!("decoder.gdt_convs_attn_{i}: shape {shape:?}")));
        }
        Ok(Self { attn: Dense::from_out_in(&w, 1, conv.cout, Some(b), &p.dev)?, conv })
    }

    fn forward(&self, mut x: Map) -> Result<Map> {
        let g = self.conv.forward(&x, Act::Relu)?;
        let a = self.attn.forward(&g.data, Act::None)?;
        let c = x.c.max(1);
        x.data.par_chunks_mut(c).zip(a.par_iter()).for_each(|(px, a)| {
            let s = sigmoid(*a);
            px.iter_mut().for_each(|v| *v *= s);
        });
        Ok(x)
    }
}

/// Intermediate results kept by [`BiRefNet::trace_pixels`] (name, channels-first values, shape).
pub type Trace = Vec<(String, Vec<f32>, [usize; 3])>;

fn tap(trace: &mut Option<Trace>, name: &str, m: &Map) {
    if let Some(t) = trace {
        t.push((name.to_string(), m.to_chw(), [m.c, m.h, m.w]));
    }
}

/// A loaded BiRefNet model.
pub struct BiRefNet {
    device: Device,
    bb: Swin,
    squeeze: DecBlk,
    /// `ipt_blk5` … `ipt_blk2`.
    ipt: Vec<Simple>,
    /// `ipt_blk1.conv1`, and its `conv_out` folded with the image part of `conv_out1`.
    ipt1_conv1: Conv,
    ipt1_out: Conv,
    /// `decoder_block4` … `decoder_block1`.
    dec: Vec<DecBlk>,
    /// `gdt_convs_*_4` … `_2`.
    gates: Vec<Gate>,
    /// `lateral_block4` … `lateral_block2`.
    lateral: Vec<Conv>,
    /// The decoder part of `conv_out1` (its bias included).
    out: Dense,
}

impl BiRefNet {
    /// Load from a directory holding `model.safetensors` (any BiRefNet checkpoint with a Swin
    /// backbone, e.g. `ZhengPeng7/BiRefNet-portrait` or `BiRefNet_lite`; F32, F16 or BF16).
    pub fn load(dir: &Path) -> Result<Self> {
        Self::load_on(dir, crate::best_device())
    }

    pub fn load_on(dir: &Path, device: Device) -> Result<Self> {
        let path = dir.join(WEIGHTS_FILE);
        if !path.is_file() {
            return Err(Error::BiRefNetMissing(dir.to_path_buf()));
        }
        Self::load_weights(&path, device).map_err(named)
    }

    fn load_weights(path: &Path, device: Device) -> Result<Self> {
        let weights: Arc<Weights> = Weights::open(path)?;
        if !weights.contains("decoder.conv_out1.0.weight") {
            return Err(Error::Model(format!("{} is not a BiRefNet checkpoint", path.display())));
        }
        let p = Params { weights: &weights, dev: device.clone() };
        let bb = Swin::load(&p, "bb")?;
        let ipt = (2..=5).rev().map(|i| Simple::load(&p, &format!("decoder.ipt_blk{i}"))).collect::<Result<Vec<_>>>()?;
        let dec = (1..=4).rev().map(|i| DecBlk::load(&p, &format!("decoder.decoder_block{i}"))).collect::<Result<Vec<_>>>()?;
        let gates = (2..=4).rev().map(|i| Gate::load(&p, i)).collect::<Result<Vec<_>>>()?;
        let lateral = (2..=4).rev().map(|i| p.conv(&format!("decoder.lateral_block{i}.conv"), 0, None)).collect::<Result<Vec<_>>>()?;
        // conv_out1 over cat(decoder, ipt_blk1): split into the two parts
        let (w, shape) = p.get("decoder.conv_out1.0.weight")?;
        let b = p.vec("decoder.conv_out1.0.bias", 1)?;
        let dec_c = dec.last().map(|d| d.conv_out.cout).unwrap_or(0);
        let ipt1_conv1 = p.conv("decoder.ipt_blk1.conv1", 1, None)?;
        let (ow, os) = p.get("decoder.ipt_blk1.conv_out.weight")?;
        let &[ipt_c, mid, k, _] = os.as_slice() else { return Err(Error::Model(format!("decoder.ipt_blk1.conv_out: shape {os:?}"))) };
        if shape.first() != Some(&1) || w.len() != dec_c + ipt_c || ow.len() != ipt_c * mid * k * k {
            return Err(Error::Model(format!("decoder.conv_out1: shape {shape:?}")));
        }
        let ob = p.vec("decoder.ipt_blk1.conv_out.bias", ipt_c)?;
        let (wa, wb) = w.split_at(dec_c);
        let mut fused = vec![0f32; mid * k * k];
        for (o, wo) in wb.iter().enumerate() {
            for (i, f) in fused.iter_mut().enumerate() {
                *f += wo * ow.get(o * mid * k * k + i).copied().unwrap_or(0.0);
            }
        }
        let fused_b: f32 = wb.iter().zip(&ob).map(|(a, b)| a * b).sum();
        let ipt1_out = Conv::new(&fused, &[1, mid, k, k], Some(&[fused_b]), None, k / 2, &device)?;
        let out = Dense::from_out_in(wa, 1, dec_c, Some(b), &device)?;
        Ok(Self { squeeze: DecBlk::load(&p, "squeeze_module.0")?, bb, ipt, ipt1_conv1, ipt1_out, dec, gates, lateral, out, device })
    }

    /// The backbone, from the weights: `swin_v1_t` (BiRefNet_lite), `swin_v1_l` (BiRefNet,
    /// BiRefNet-portrait), …
    pub fn backbone_name(&self) -> &'static str {
        self.bb.name
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Segment the subject of an RGB image (8-bit, row-major, `w × h`), stretched to
    /// [`BIREFNET_SIDE`]² like the reference preprocessing. Returns `BIREFNET_SIDE²` logits
    /// (pre-sigmoid) over that stretched square; [`crate::mask::alpha`] maps them back.
    pub fn segment(&self, rgb: &[u8], w: usize, h: usize) -> Result<Vec<f32>> {
        let pixels = crate::mask::preprocess_normalized(rgb, w, h, BIREFNET_SIDE, MEAN, STD).map_err(named)?;
        self.segment_pixels(&pixels)
    }

    /// [`Self::segment`] on preprocessed pixels (`[3, 1024, 1024]`, ImageNet-normalized).
    pub fn segment_pixels(&self, chw: &[f32]) -> Result<Vec<f32>> {
        self.run(chw, &mut None).map_err(named)
    }

    /// [`Self::segment_pixels`], also returning intermediate results (tests).
    #[doc(hidden)]
    pub fn trace_pixels(&self, chw: &[f32]) -> Result<(Vec<f32>, Trace)> {
        let mut trace = Some(Vec::new());
        let logits = self.run(chw, &mut trace).map_err(named)?;
        Ok((logits, trace.unwrap_or_default()))
    }

    /// The ASPP block of `decoder_block{block}` on a channels-first input (tests).
    #[doc(hidden)]
    pub fn probe_aspp(&self, block: usize, chw: &[f32], c: usize, h: usize, w: usize) -> Result<Vec<f32>> {
        let d = 4usize.checked_sub(block).and_then(|i| self.dec.get(i)).ok_or_else(|| Error::BiRefNet(format!("no decoder block {block}")))?;
        let x = Map::from_chw(chw, c, h, w).map_err(named)?;
        Ok(d.att.forward(&x).map_err(named)?.to_chw())
    }

    fn run(&self, chw: &[f32], trace: &mut Option<Trace>) -> Result<Vec<f32>> {
        let side = BIREFNET_SIDE;
        let x = Map::from_chw(chw, 3, side, side)?;
        // encoder: the image and its half-size copy through the backbone, concatenated
        let mut prof = Profile::new();
        let full = self.bb.forward(&x)?;
        prof.step("backbone");
        let half = self.bb.forward(&resize(&x, side / 2, side / 2)?)?;
        prof.step("backbone (half size)");
        let mut xs = Vec::with_capacity(4);
        for (i, (f, h)) in full.into_iter().zip(half).enumerate() {
            tap(trace, &format!("bb_full{}", i + 1), &f);
            tap(trace, &format!("bb_half{}", i + 1), &h);
            let up = resize(&h, f.h, f.w)?;
            drop(h);
            xs.push(cat(&[&f, &up])?);
        }
        let [x1, x2, x3, x4] = xs.as_slice() else { return Err(Error::Model("backbone outputs".into())) };
        // context: the three shallower levels pooled into the deepest
        let (h4, w4) = (x4.h, x4.w);
        let ctx = cat(&[&resize(x1, h4, w4)?, &resize(x2, h4, w4)?, &resize(x3, h4, w4)?, x4])?;
        prof.step("context");
        let x4 = self.squeeze.forward(&ctx)?;
        prof.step("squeeze");
        drop(ctx);
        tap(trace, "squeeze", &x4);

        // decoder
        let levels = [(&x4, None), (x3, Some(x3)), (x2, Some(x2)), (x1, Some(x1))];
        let mut prev: Option<Map> = None;
        let names = ["dec4", "dec3", "dec2", "dec1"];
        for (i, (&(base, lateral_in), name)) in levels.iter().zip(names).enumerate() {
            let (h, w) = (base.h, base.w);
            // the previous level, upsampled, plus the lateral path of this level
            let p = match (prev.take(), lateral_in) {
                (None, _) => base.clone(),
                (Some(pm), Some(lat_in)) => {
                    let mut up = resize(&pm, h, w)?;
                    drop(pm);
                    let lat = self.lateral.get(i - 1).ok_or_else(|| Error::Model("lateral block".into()))?.forward(lat_in, Act::None)?;
                    add_assign(&mut up.data, &lat.data);
                    up
                }
                (Some(_), None) => return Err(Error::Model("decoder level".into())),
            };
            let ipt = self.ipt.get(i).ok_or_else(|| Error::Model("ipt block".into()))?.forward(&patches(&x, h, w)?)?;
            let p = cat(&[&p, &ipt])?;
            drop(ipt);
            let mut d = self.dec.get(i).ok_or_else(|| Error::Model("decoder block".into()))?.forward(&p)?;
            drop(p);
            prof.step(name);
            tap(trace, name, &d);
            if let Some(g) = self.gates.get(i) {
                d = g.forward(d)?;
            }
            prev = Some(d);
        }
        let p1 = prev.ok_or_else(|| Error::Model("decoder output".into()))?;
        // conv_out1(cat(up(p1), ipt_blk1(x))) = up(conv_a(p1)) + conv_b(ipt_blk1(x))
        let s = self.out.map(&p1, Act::None)?;
        drop(p1);
        let mut logits = resize(&s, side, side)?.data;
        let t = self.ipt1_out.forward(&self.ipt1_conv1.forward(&x, Act::None)?, Act::None)?;
        add_assign(&mut logits, &t.data);
        prof.step("output");
        Ok(logits)
    }
}

/// `image2patches(x, patch_ref)` with `'b c (hg h) (wg w) -> b (c hg wg) h w'`: the image cut
/// into a grid of `h × w` tiles stacked along channels.
fn patches(x: &Map, h: usize, w: usize) -> Result<Map> {
    if h == 0 || w == 0 || !x.h.is_multiple_of(h) || !x.w.is_multiple_of(w) || x.h / h != x.w / w {
        return Err(Error::Model(format!("{} × {} image does not split into {h} × {w} tiles", x.w, x.h)));
    }
    let g = x.h / h;
    let (c, gg) = (x.c, g * g);
    let n = c * gg;
    let mut out = vec![0f32; h * w * n];
    out.par_chunks_mut(n.max(1)).enumerate().for_each(|(p, px)| {
        let (y, xx) = (p / w, p % w);
        for gy in 0..g {
            for gx in 0..g {
                let at = ((gy * h + y) * x.w + gx * w + xx) * c;
                for ch in 0..c {
                    if let (Some(d), Some(s)) = (px.get_mut(ch * gg + gy * g + gx), x.data.get(at + ch)) {
                        *d = *s;
                    }
                }
            }
        }
    });
    Map::new(h, w, n, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_stack_tiles_along_channels() {
        // 1 channel, 4 × 4 image of its pixel index, 2 × 2 tiles → 4 channels
        let x = Map::new(4, 4, 1, (0..16).map(|v| v as f32).collect()).unwrap();
        let p = patches(&x, 2, 2).unwrap();
        assert_eq!(p.c, 4);
        // pixel (0, 0) of each tile: tiles (0,0) (0,1) (1,0) (1,1)
        assert_eq!(p.data[..4], [0.0, 2.0, 8.0, 10.0]);
        assert_eq!(patches(&x, 4, 4).unwrap().data, x.data);
        assert!(patches(&x, 3, 3).is_err());
    }

    #[test]
    fn missing_or_garbage_models_are_errors() {
        let dir = std::env::temp_dir().join(format!("lc-birefnet-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_birefnet_dir(&dir));
        let e = BiRefNet::load_on(&dir, Device::Cpu).err().unwrap().to_string();
        assert!(e.contains("model.safetensors"), "{e}");
        std::fs::write(dir.join(WEIGHTS_FILE), b"not a safetensors file").unwrap();
        assert!(!is_birefnet_dir(&dir));
        assert!(BiRefNet::load_on(&dir, Device::Cpu).is_err());
        // a valid file without BiRefNet's tensors
        let header = r#"{"a":{"dtype":"F32","shape":[1],"data_offsets":[0,4]}}"#;
        let mut b = (header.len() as u64).to_le_bytes().to_vec();
        b.extend_from_slice(header.as_bytes());
        b.extend_from_slice(&[0; 4]);
        std::fs::write(dir.join(WEIGHTS_FILE), b).unwrap();
        let e = BiRefNet::load_on(&dir, Device::Cpu).err().unwrap().to_string();
        assert!(e.contains("not a BiRefNet checkpoint"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
