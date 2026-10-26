//! Running the towers somewhere other than candle: a plain description of a tower ([`TowerPlan`]: its sizes and
//! the names of its tensors) that another runtime turns into a [`TowerRunner`] — LightCraft's GPU runner in
//! `lightcraft-gpu` (wgpu compute kernels), which this crate does not depend on. A model loaded with
//! [`crate::load_with`] runs its towers on such a runner when one can be made, and on candle otherwise.
//!
//! The runner does the transformer (patch or token embeddings in, raw embeddings out); this crate keeps the rest:
//! preprocessing, tokenizing, reading token rows from the weights file, normalising and scoring. Any failure of a
//! runner is an `Err`: the model then runs that batch on the CPU and stops using the runner (and no new runner is
//! made in this process, [`disabled`]), so a GPU problem costs speed, never a result.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::Act;

/// A linear layer as stored (`[out, in]` weight, PyTorch's layout), possibly rows `rows` of a packed tensor (SigLIP's
/// pooling head packs q, k and v in one `in_proj_weight`).
#[derive(Clone, Debug, PartialEq)]
pub struct Linear {
    pub weight: String,
    pub bias: Option<String>,
    /// Rows of the weight (and bias) to use: `(first, count)`; `None` for all.
    pub rows: Option<(usize, usize)>,
    pub inp: usize,
    pub out: usize,
}

/// A layer norm's weight and bias.
#[derive(Clone, Debug, PartialEq)]
pub struct Norm {
    pub weight: String,
    pub bias: String,
}

/// One pre-norm encoder block: `x += o(attention(q, k, v of ln1(x)))`, `x += fc2(act(fc1(ln2(x))))`.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub ln1: Norm,
    pub q: Linear,
    pub k: Linear,
    pub v: Linear,
    pub o: Linear,
    pub ln2: Norm,
    pub fc1: Linear,
    pub fc2: Linear,
}

/// SigLIP's attention-pooling head: one learned query (`probe`) attends over all positions, then `h += fc2(act(fc1(ln(h))))`.
#[derive(Clone, Debug, PartialEq)]
pub struct ProbeHead {
    /// `[1, 1, hidden]`.
    pub probe: String,
    pub q: Linear,
    pub k: Linear,
    pub v: Linear,
    pub o: Linear,
    pub ln: Norm,
    pub fc1: Linear,
    pub fc2: Linear,
}

/// What a tower takes in.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// Image patches `[n · patches, patch_dim]` (each `[channel][y][x]`, as the patch convolution's weight), projected
    /// by `proj` (the convolution as a linear layer).
    Patches { patches: usize, proj: Linear },
    /// Token embeddings `[n · len, hidden]` (rows of the token table, read by this crate).
    Tokens,
}

/// How a tower turns its positions into one vector per input (after the final layer norm).
#[derive(Clone, Debug, PartialEq)]
pub enum Pool {
    /// The position the caller names for each input (CLIP's class token, end-of-text token, SigLIP's last token).
    Pick,
    /// SigLIP's attention-pooling head.
    Probe(Box<ProbeHead>),
}

/// A tower, as plain data: sizes, and the names of the tensors in the weights file.
#[derive(Clone, Debug, PartialEq)]
pub struct TowerPlan {
    /// "image" or "text" (for messages).
    pub name: &'static str,
    pub hidden: usize,
    pub heads: usize,
    pub intermediate: usize,
    pub eps: f32,
    pub act: Act,
    /// Each position sees only itself and those before it (CLIP's text tower).
    pub causal: bool,
    pub input: Input,
    /// A learned class token put in front of the patches (`[hidden]`, CLIP's image tower).
    pub class: Option<String>,
    /// Learned position embeddings, `[positions, hidden]`.
    pub positions: String,
    /// Rows of `positions`: the most positions an input has (with the class token).
    pub max_len: usize,
    /// A layer norm right after the embeddings (CLIP's `pre_layrnorm`).
    pub pre_ln: Option<Norm>,
    pub blocks: Vec<Block>,
    /// The layer norm after the blocks (applied to every position).
    pub final_ln: Norm,
    pub pool: Pool,
    /// The projection into the shared space, if any.
    pub proj: Option<Linear>,
    /// The width of what the tower gives back.
    pub out: usize,
}

impl TowerPlan {
    /// The input width each row of [`TowerRunner::run`]'s input has.
    pub fn input_width(&self) -> usize {
        match &self.input {
            Input::Patches { proj, .. } => proj.inp,
            Input::Tokens => self.hidden,
        }
    }
}

/// Reads tensors for a runner: `(shape, values as f32)`.
pub trait TensorSource {
    fn read(&self, name: &str) -> Result<(Vec<usize>, Vec<f32>), String>;
}

/// One tower's forward pass on another runtime.
pub trait TowerRunner: Send + Sync {
    /// `n` inputs → `n × out` raw (not normalised) embeddings, row-major. `input` is `[n · len, input width]`
    /// ([`TowerPlan::input_width`]): patches for an image tower (`len` = patches), token embeddings for a text tower.
    /// `pick` names the position pooled for each input when the plan pools by [`Pool::Pick`] (one per input).
    fn run(&self, input: &[f32], n: usize, pick: &[u32]) -> Result<Vec<f32>, String>;
    /// Whether it can be used right now, else why not (e.g. the GPU switched off in Settings): the CPU runs this
    /// call, and the runner is tried again next time.
    fn usable(&self) -> Result<(), String> {
        Ok(())
    }
    /// What it runs on, e.g. "NVIDIA GeForce RTX 4090 (Vulkan)".
    fn device(&self) -> String;
}

/// Makes a [`TowerRunner`] for a tower, or says why it cannot (no GPU, too little memory, …).
pub type Accelerator = dyn Fn(&TowerPlan, &dyn TensorSource) -> Result<Box<dyn TowerRunner>, String> + Send + Sync;

static DISABLED: AtomicBool = AtomicBool::new(false);
static DISABLED_WHY: Mutex<Option<String>> = Mutex::new(None);

/// Stop making runners in this process (a runner failed): models loaded from now on run on the CPU.
pub fn disable(why: &str) {
    let mut w = DISABLED_WHY.lock().unwrap_or_else(|e| e.into_inner());
    if w.is_none() {
        *w = Some(why.to_string());
    }
    DISABLED.store(true, Ordering::SeqCst);
}

/// Why no runners are made any more, if a runner failed in this process.
pub fn disabled() -> Option<String> {
    DISABLED.load(Ordering::SeqCst).then(|| DISABLED_WHY.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default())
}

/// Tests: make runners again.
#[doc(hidden)]
pub fn reset_disabled() {
    *DISABLED_WHY.lock().unwrap_or_else(|e| e.into_inner()) = None;
    DISABLED.store(false, Ordering::SeqCst);
}

/// A tower's runner inside a model, until it fails.
pub(crate) struct Accel {
    runner: Box<dyn TowerRunner>,
    failed: Mutex<Option<String>>,
}

impl Accel {
    pub(crate) fn new(runner: Box<dyn TowerRunner>) -> Self {
        Self { runner, failed: Mutex::new(None) }
    }

    /// Why this runner is not used any more.
    pub(crate) fn failed(&self) -> Option<String> {
        self.failed.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The runner's device while it is in use.
    pub(crate) fn device(&self) -> Option<String> {
        if self.failed().is_some() || self.runner.usable().is_err() {
            return None;
        }
        Some(self.runner.device())
    }

    /// Run on the runner: `None` when it is not usable now or fails (then never again: the caller runs the CPU).
    /// `input` is made only when the runner is tried. Checks the answer's size and that it is finite.
    pub(crate) fn run(&self, input: impl FnOnce() -> crate::Result<Vec<f32>>, n: usize, out: usize, pick: &[u32]) -> Option<Vec<f32>> {
        if self.failed().is_some() || self.runner.usable().is_err() {
            return None;
        }
        // making the input is CPU work (patches, rows of the token table): when it fails the runner is not to blame,
        // and the CPU path, which makes the same input, says what is wrong
        let Ok(input) = input() else { return None };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.runner.run(&input, n, pick)))
            .unwrap_or_else(|_| Err("the runner stopped unexpectedly".into()));
        let r = r.and_then(|v| {
            if v.len() != n.saturating_mul(out) {
                Err(format!("the runner gave {} numbers for {n} × {out}", v.len()))
            } else if v.iter().any(|x| !x.is_finite()) {
                Err("the runner gave a non-finite embedding".into())
            } else {
                Ok(v)
            }
        });
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                let why = format!("{}: {e}", self.runner.device());
                log::warn!("content model: the GPU failed ({why}); the CPU runs it from now on");
                disable(&why);
                *self.failed.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
                None
            }
        }
    }
}

/// `[n, 3, s, s]` planar pixels → `[n · g², 3 · p · p]` patches (each `[channel][y][x]`), `g = s / p`.
pub(crate) fn patches(pixels: &[f32], n: usize, s: usize, p: usize) -> crate::Result<Vec<f32>> {
    let g = s / p.max(1);
    let (plane, dim) = (s * s, 3 * p * p);
    if p == 0 || g * p != s || pixels.len() != n * 3 * plane {
        return Err(crate::Error::Input(format!("{} pixels for {n} images of {s}² in {p}² patches", pixels.len())));
    }
    let mut out = vec![0f32; n * g * g * dim];
    for (i, row) in out.chunks_exact_mut(dim).enumerate() {
        let (b, cell) = (i / (g * g), i % (g * g));
        let (gy, gx) = (cell / g, cell % g);
        for c in 0..3 {
            for y in 0..p {
                let from = b * 3 * plane + c * plane + (gy * p + y) * s + gx * p;
                if let (Some(dst), Some(src)) = (row.get_mut((c * p + y) * p..(c * p + y + 1) * p), pixels.get(from..from + p)) {
                    dst.copy_from_slice(src);
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Tests that use the process-wide [`disabled`] switch run one at a time.
    pub(crate) static ALONE: Mutex<()> = Mutex::new(());

    struct Fake {
        fail: bool,
        usable: bool,
    }

    impl TowerRunner for Fake {
        fn run(&self, input: &[f32], n: usize, _pick: &[u32]) -> Result<Vec<f32>, String> {
            if self.fail { Err("device lost".into()) } else { Ok(vec![input.first().copied().unwrap_or(0.0); n * 2]) }
        }
        fn usable(&self) -> Result<(), String> {
            if self.usable { Ok(()) } else { Err("switched off".into()) }
        }
        fn device(&self) -> String {
            "Fake GPU".into()
        }
    }

    #[test]
    fn a_failing_runner_is_not_used_again_and_stops_new_ones() {
        let _alone = ALONE.lock().unwrap_or_else(|e| e.into_inner());
        reset_disabled();
        let ok = Accel::new(Box::new(Fake { fail: false, usable: true }));
        assert_eq!(ok.run(|| Ok(vec![3.0]), 2, 2, &[]), Some(vec![3.0; 4]));
        assert_eq!(ok.device().as_deref(), Some("Fake GPU"));
        // a wrong size is a failure too
        assert_eq!(ok.run(|| Ok(vec![3.0]), 3, 4, &[]), None);
        assert!(ok.failed().is_some() && ok.device().is_none());
        assert_eq!(ok.run(|| Ok(vec![3.0]), 2, 2, &[]), None, "never again");
        assert!(disabled().is_some());
        reset_disabled();
        let bad = Accel::new(Box::new(Fake { fail: true, usable: true }));
        assert_eq!(bad.run(|| Ok(vec![1.0]), 1, 2, &[]), None);
        assert!(bad.failed().unwrap().contains("device lost"));
        assert!(disabled().unwrap().contains("Fake GPU"));
        reset_disabled();
        // switched off: not now, but not failed either
        let off = Accel::new(Box::new(Fake { fail: false, usable: false }));
        assert_eq!(off.run(|| Ok(vec![1.0]), 1, 2, &[]), None);
        assert!(off.failed().is_none() && off.device().is_none() && disabled().is_none());
    }

    #[test]
    fn an_input_that_cannot_be_made_is_not_a_gpu_failure() {
        let _alone = ALONE.lock().unwrap_or_else(|e| e.into_inner());
        reset_disabled();
        let ok = Accel::new(Box::new(Fake { fail: false, usable: true }));
        // (a token id outside the table, say: the CPU path reports it)
        assert_eq!(ok.run(|| Err(crate::Error::Input("no such token".into())), 1, 2, &[]), None);
        assert!(ok.failed().is_none() && disabled().is_none(), "the runner stays in use");
        assert_eq!(ok.run(|| Ok(vec![3.0]), 1, 2, &[]), Some(vec![3.0; 2]));
    }

    #[test]
    fn patches_are_cut_channel_then_row_then_column() {
        // 1 image, 4 × 4, patches of 2: 4 patches of 12 numbers
        let px: Vec<f32> = (0..48).map(|v| v as f32).collect();
        let p = patches(&px, 1, 4, 2).unwrap();
        assert_eq!(p.len(), 48);
        // the second patch (top right): channel 0 rows 0..2 columns 2..4, then channel 1, channel 2
        assert_eq!(&p[12..24], &[2.0, 3.0, 6.0, 7.0, 18.0, 19.0, 22.0, 23.0, 34.0, 35.0, 38.0, 39.0]);
        assert!(patches(&px, 1, 4, 3).is_err());
        assert!(patches(&px, 2, 4, 2).is_err());
        assert!(patches(&px, 1, 4, 0).is_err());
    }
}
