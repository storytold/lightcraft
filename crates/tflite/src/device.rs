//! Running a model on an accelerator (the GPU, in `lcc-gpu`): the graph lowered to an `lcc-faces`
//! [`Program`], the same flat list of device steps the face recognisers run on, so the GPU's face
//! kernels serve the MediaPipe models too and every activation stays on the device between layers.
//!
//! The interpreter's tensors are NHWC; the device's are planar (NCHW), the layout the face kernels
//! take. The program starts by transposing the input, then lowers the operators in graph order —
//! `CONV_2D` and `DEPTHWISE_CONV_2D` as (grouped) convolutions, `PAD` as an identity convolution
//! with padding, `MAX_POOL_2D`, `ADD`, `MUL`, `PRELU`, `RELU` and `LOGISTIC`, fused activations and
//! folded PRELUs as their own steps — until the first one it cannot express (`RESHAPE` past the
//! NHWC layout, `CONCATENATION`, `RELU6`, …). From there on the CPU runs the rest of the graph
//! (for the face models, the few reshapes and concatenations that gather their outputs) on what the
//! device gives back, transposed to NHWC on the device.
//!
//! The CPU stays the reference: [`Model::accelerate`] keeps an accelerator only after it agreed with
//! the CPU on a test input (and, when asked, was faster), and the first device error drops it.
//!
//! Written in 2026 on the interpreter's own operator definitions ([`crate::ops`]).

use std::borrow::Cow;
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

pub use lcc_faces::net::Accelerator;
use lcc_faces::net::ConvJob;
use lcc_faces::net::program::{Arg, Binary as DevBinary, INPUT, Op, Program, Step as DevStep, Unary as DevUnary};

use crate::ops::{Act, BinOp, Post, Unary};
use crate::{Kind, Model, Result, bad, dims4, numel};

/// Inputs [`Model::accelerate`] times on each side when asked whether the device is faster.
const BENCH_BATCH: usize = 8;

/// A model's accelerator and its lowering (made on first use).
#[derive(Default)]
pub(crate) struct Device {
    accelerator: RwLock<Option<Arc<dyn Accelerator>>>,
    plan: OnceLock<Option<Plan>>,
}

/// A convolution with its weights in the device's layout.
struct PlanConv {
    layer: usize,
    cin: usize,
    h: usize,
    w: usize,
    cout: usize,
    groups: usize,
    kh: usize,
    kw: usize,
    sh: usize,
    sw: usize,
    pt: usize,
    pl: usize,
    oh: usize,
    ow: usize,
    /// `[cout][cin / groups][kh][kw]`.
    weights: Vec<f32>,
    bias: Vec<f32>,
}

enum PlanOp {
    Conv(PlanConv),
    Other(Op<'static>),
}

enum PlanArg {
    Value(usize),
    Const { id: usize, data: Vec<f32> },
}

struct PlanStep {
    op: PlanOp,
    args: Vec<PlanArg>,
    out: usize,
    free: Vec<usize>,
}

/// The model lowered: a device prefix, and where the CPU takes over.
struct Plan {
    lens: Vec<usize>,
    steps: Vec<PlanStep>,
    /// The tensors the device gives back (NHWC, as the interpreter holds them) and the values holding them.
    outputs: Vec<(usize, usize)>,
    /// The first interpreter step the CPU runs.
    tail: usize,
    /// Convolutions on the device (a plan without any is not worth a device).
    convs: usize,
}

impl Plan {
    /// The program, borrowing the weights.
    fn program(&self) -> Program<'_> {
        let steps = self
            .steps
            .iter()
            .map(|s| DevStep {
                op: match &s.op {
                    PlanOp::Conv(c) => Op::Conv(ConvJob {
                        layer: c.layer,
                        cin: c.cin,
                        h: c.h,
                        w: c.w,
                        cout: c.cout,
                        groups: c.groups,
                        kh: c.kh,
                        kw: c.kw,
                        sh: c.sh,
                        sw: c.sw,
                        pt: c.pt,
                        pl: c.pl,
                        oh: c.oh,
                        ow: c.ow,
                        weights: &c.weights,
                        bias: Some(&c.bias),
                        input: &[],
                    }),
                    PlanOp::Other(op) => op.clone(),
                },
                args: s
                    .args
                    .iter()
                    .map(|a| match a {
                        PlanArg::Value(v) => Arg::Value(*v),
                        PlanArg::Const { id, data } => Arg::Const { id: *id, data: Cow::Borrowed(data.as_slice()) },
                    })
                    .collect(),
                out: s.out,
                free: s.free.clone(),
            })
            .collect();
        let outputs = self.outputs.iter().map(|(t, v)| (format!("tensor {t}"), *v, Vec::new())).collect();
        Program { lens: self.lens.clone(), steps, outputs }
    }
}

/// Strides of a planar `[n, c, h, w]` tensor, 0 along axes of size 1 (so it broadcasts).
fn planar_strides([n, c, h, w]: [usize; 4]) -> [usize; 4] {
    let s = [c * h * w, h * w, w, 1];
    [n, c, h, w].iter().zip(s).map(|(d, s)| if *d == 1 { 0 } else { s }).collect::<Vec<_>>().try_into().unwrap_or([0; 4])
}

/// NHWC dims to the planar order `[n, c, h, w]`.
fn planar([n, h, w, c]: [usize; 4]) -> [usize; 4] {
    [n, c, h, w]
}

/// NHWC values (`d`) in planar order.
fn to_planar(data: &[f32], [n, h, w, c]: [usize; 4]) -> Option<Vec<f32>> {
    let mut out = Vec::with_capacity(data.len());
    for b in 0..n {
        for ch in 0..c {
            for y in 0..h {
                for x in 0..w {
                    out.push(*data.get(((b * h + y) * w + x) * c + ch)?);
                }
            }
        }
    }
    Some(out)
}

/// Builds a [`Plan`].
struct Lower<'m> {
    m: &'m Model,
    lens: Vec<usize>,
    steps: Vec<PlanStep>,
    /// Each tensor's device value (planar), if it has one.
    value: Vec<Option<usize>>,
    convs: usize,
    /// Where this model's layer and constant ids start: unique in the process, so models sharing an accelerator
    /// keep their weights apart.
    base: usize,
}

/// The next model's first id.
static NEXT_ID: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1 << 20);

impl Lower<'_> {
    fn dims(&self, t: usize) -> Result<[usize; 4]> {
        dims4(&self.m.tensors.get(t).ok_or_else(|| bad("tensor index"))?.shape)
    }

    fn new_value(&mut self, len: usize) -> usize {
        self.lens.push(len);
        self.lens.len() - 1
    }

    fn push(&mut self, op: PlanOp, args: Vec<PlanArg>, len: usize) -> usize {
        let out = self.new_value(len);
        self.steps.push(PlanStep { op, args, out, free: Vec::new() });
        out
    }

    /// A tensor as an operand: its device value, or a constant (in planar order).
    fn operand(&self, t: usize, id: usize) -> Result<Option<PlanArg>> {
        if let Some(v) = self.value.get(t).copied().flatten() {
            return Ok(Some(PlanArg::Value(v)));
        }
        match self.m.consts.get(t).and_then(|c| c.as_deref()) {
            Some(data) => Ok(to_planar(data, self.dims(t)?).map(|data| PlanArg::Const { id, data })),
            None => Ok(None),
        }
    }

    /// `post` after a step making `x` (`channels` planes of `plane` values), or `None` when the device has no form.
    fn post(&mut self, x: usize, post: &Post, channels: usize, plane: usize, id: usize) -> Option<usize> {
        let len = *self.lens.get(x)?;
        let mut x = x;
        match post.act {
            Act::None => {}
            Act::Relu => x = self.push(PlanOp::Other(Op::Unary(DevUnary::Relu)), vec![PlanArg::Value(x)], len),
            Act::Relu6 | Act::ReluN1To1 => return None,
        }
        if let Some(alpha) = &post.alpha {
            if alpha.len() != channels {
                return None;
            }
            x = self.push(
                PlanOp::Other(Op::ChannelPrelu { channels, plane }),
                vec![PlanArg::Value(x), PlanArg::Const { id, data: alpha.clone() }],
                len,
            );
        }
        Some(x)
    }

    /// Lower interpreter step `si`; `Ok(false)` when the device has no form for it.
    fn step(&mut self, si: usize) -> Result<bool> {
        let s = self.m.steps.get(si).ok_or_else(|| bad("step index"))?;
        let base = self.base;
        let id = |k: usize| base + si * 8 + k;
        let out_dims = self.dims(s.out)?;
        let [on, oh, ow, oc] = out_dims;
        let out_len = numel(&out_dims)?;
        let x_t = *s.ins.first().ok_or_else(|| bad("operator without input"))?;
        let x = self.value.get(x_t).copied().flatten();
        let made = match &s.kind {
            Kind::Conv(_) | Kind::Depthwise(_) | Kind::MaxPool(_) | Kind::Unary(_) | Kind::Pad(_) if x.is_none() || on != 1 => None,
            Kind::Conv(k) => {
                let g = &k.g;
                if g.dh != 1 || g.dw != 1 || k.w.n != oc || k.bias.len() != oc {
                    return Ok(false);
                }
                let mut weights = Vec::with_capacity(oc * g.c * g.kh * g.kw);
                for co in 0..oc {
                    for ci in 0..g.c {
                        for ky in 0..g.kh {
                            for kx in 0..g.kw {
                                weights.push(k.w.at((ky * g.kw + kx) * g.c + ci, co));
                            }
                        }
                    }
                }
                let conv = PlanConv {
                    layer: id(0),
                    cin: g.c,
                    h: g.h,
                    w: g.w,
                    cout: oc,
                    groups: 1,
                    kh: g.kh,
                    kw: g.kw,
                    sh: g.sh,
                    sw: g.sw,
                    pt: g.pt,
                    pl: g.pl,
                    oh,
                    ow,
                    weights,
                    bias: k.bias.clone(),
                };
                self.convs += 1;
                let y = self.push(PlanOp::Conv(conv), x.map(PlanArg::Value).into_iter().collect(), out_len);
                self.post(y, &k.post, oc, oh * ow, id(1))
            }
            Kind::Depthwise(k) => {
                let g = &k.g;
                if g.dh != 1 || g.dw != 1 || g.c * k.mult != oc || k.bias.len() != oc || k.w.len() != g.kh * g.kw * oc {
                    return Ok(false);
                }
                let mut weights = Vec::with_capacity(k.w.len());
                for co in 0..oc {
                    for t in 0..g.kh * g.kw {
                        weights.push(k.w.get(t * oc + co).copied().unwrap_or(0.0));
                    }
                }
                let conv = PlanConv {
                    layer: id(0),
                    cin: g.c,
                    h: g.h,
                    w: g.w,
                    cout: oc,
                    groups: g.c,
                    kh: g.kh,
                    kw: g.kw,
                    sh: g.sh,
                    sw: g.sw,
                    pt: g.pt,
                    pl: g.pl,
                    oh,
                    ow,
                    weights,
                    bias: k.bias.clone(),
                };
                self.convs += 1;
                let y = self.push(PlanOp::Conv(conv), x.map(PlanArg::Value).into_iter().collect(), out_len);
                self.post(y, &k.post, oc, oh * ow, id(1))
            }
            Kind::Pad(p) => {
                let [n, h, w, c] = p.d;
                if n != 1 || p.before[0] != 0 || p.out != out_dims || p.before[3] + c > oc {
                    return Ok(false);
                }
                // an identity convolution with the padding: depthwise when the channels stay, else 1 × 1 dense
                let same = oc == c;
                let groups = if same { c } else { 1 };
                let weights = if same {
                    vec![1.0; c]
                } else {
                    (0..oc).flat_map(|co| (0..c).map(move |ci| if co == ci + p.before[3] { 1.0 } else { 0.0 })).collect()
                };
                let conv = PlanConv {
                    layer: id(0),
                    cin: c,
                    h,
                    w,
                    cout: oc,
                    groups,
                    kh: 1,
                    kw: 1,
                    sh: 1,
                    sw: 1,
                    pt: p.before[1],
                    pl: p.before[2],
                    oh,
                    ow,
                    weights,
                    bias: vec![0.0; oc],
                };
                Some(self.push(PlanOp::Conv(conv), x.map(PlanArg::Value).into_iter().collect(), out_len))
            }
            Kind::MaxPool(k) => {
                let g = &k.g;
                if g.pt != 0 || g.pl != 0 || g.dh != 1 || g.dw != 1 {
                    return Ok(false);
                }
                let op = Op::MaxPool { c: g.c, h: g.h, w: g.w, kh: g.kh, kw: g.kw, sh: g.sh, sw: g.sw, oh, ow };
                let y = self.push(PlanOp::Other(op), x.map(PlanArg::Value).into_iter().collect(), out_len);
                self.post(y, &k.post, oc, oh * ow, id(1))
            }
            Kind::Unary(u) => {
                let Some(x) = x else { return Ok(false) };
                match u {
                    Unary::Act(Act::None) => Some(x),
                    Unary::Act(Act::Relu) => Some(self.push(PlanOp::Other(Op::Unary(DevUnary::Relu)), vec![PlanArg::Value(x)], out_len)),
                    Unary::Logistic => Some(self.push(PlanOp::Other(Op::Unary(DevUnary::Sigmoid)), vec![PlanArg::Value(x)], out_len)),
                    Unary::Act(_) => None,
                }
            }
            Kind::Binary(k) => {
                let y_t = *s.ins.get(1).ok_or_else(|| bad("missing operand"))?;
                if on != 1 || k.out != out_dims {
                    return Ok(false);
                }
                match k.op {
                    BinOp::Prelu => {
                        // per-channel slopes only
                        let (Some(x), Some(alpha)) = (x, self.m.consts.get(y_t).and_then(|c| c.as_deref())) else { return Ok(false) };
                        let alpha: Vec<f32> = match (k.b, alpha) {
                            ([1, 1, 1, c], a) if c == oc && a.len() == oc => a.to_vec(),
                            (_, [one]) => vec![*one; oc],
                            _ => return Ok(false),
                        };
                        let y = self.push(
                            PlanOp::Other(Op::ChannelPrelu { channels: oc, plane: oh * ow }),
                            vec![PlanArg::Value(x), PlanArg::Const { id: id(0), data: alpha }],
                            out_len,
                        );
                        self.post(y, &k.post, oc, oh * ow, id(1))
                    }
                    BinOp::Add | BinOp::Mul => {
                        let (Some(a), Some(b)) = (self.operand(x_t, id(2))?, self.operand(y_t, id(3))?) else { return Ok(false) };
                        if matches!((&a, &b), (PlanArg::Const { .. }, PlanArg::Const { .. })) {
                            return Ok(false);
                        }
                        let op = if k.op == BinOp::Add { DevBinary::Add } else { DevBinary::Mul };
                        let dev = Op::Binary { op, dims: planar(out_dims), a: planar_strides(planar(k.a)), b: planar_strides(planar(k.b)) };
                        let y = self.push(PlanOp::Other(dev), vec![a, b], out_len);
                        self.post(y, &k.post, oc, oh * ow, id(1))
                    }
                }
            }
            // the same values under another shape: still the same planes only when the NHWC dims are the same
            Kind::Copy if x.is_some() && self.dims(x_t)? == out_dims => x,
            _ => None,
        };
        let Some(made) = made else { return Ok(false) };
        if let Some(slot) = self.value.get_mut(s.out) {
            *slot = Some(made);
        }
        Ok(true)
    }

    /// A step making tensor `t`'s NHWC values out of its planar value `v`.
    fn back_to_nhwc(&mut self, t: usize, v: usize) -> Result<usize> {
        let [n, h, w, c] = self.dims(t)?;
        if c == 1 || h * w == 1 {
            return Ok(v);
        }
        let op = Op::Transpose { dims: [n, h, w, c], strides: [c * h * w, w, 1, h * w] };
        Ok(self.push(PlanOp::Other(op), vec![PlanArg::Value(v)], numel(&[n, h, w, c])?))
    }
}

impl Model {
    /// This model lowered for an accelerator, or `None` when not worth it (one input; some convolutions).
    fn plan(&self) -> Option<&Plan> {
        self.device
            .plan
            .get_or_init(|| match self.lower() {
                Ok(p) => p,
                Err(e) => {
                    log::debug!("tflite: no device form ({e})");
                    None
                }
            })
            .as_ref()
    }

    fn lower(&self) -> Result<Option<Plan>> {
        let [input] = self.inputs.as_slice() else { return Ok(None) };
        let span = self.steps.len().saturating_add(1).saturating_mul(8);
        let base = NEXT_ID.fetch_add(span, std::sync::atomic::Ordering::Relaxed);
        let mut l = Lower { m: self, lens: Vec::new(), steps: Vec::new(), value: vec![None; self.tensors.len()], convs: 0, base };
        let [n, h, w, c] = l.dims(*input)?;
        if n != 1 {
            return Ok(None);
        }
        let len = numel(&[n, h, w, c])?;
        let raw = l.new_value(len);
        if raw != INPUT {
            return Err(bad("the device input is not value 0"));
        }
        let first = if c == 1 || h * w == 1 {
            raw
        } else {
            l.push(PlanOp::Other(Op::Transpose { dims: [1, c, h, w], strides: [len, 1, w * c, c] }), vec![PlanArg::Value(raw)], len)
        };
        if let Some(slot) = l.value.get_mut(*input) {
            *slot = Some(first);
        }
        let mut tail = self.steps.len();
        for si in 0..self.steps.len() {
            if !l.step(si)? {
                tail = si;
                break;
            }
        }
        if l.convs == 0 {
            return Ok(None);
        }
        // what the CPU needs back: what the rest of the graph reads, and the outputs the device made
        let mut wanted: Vec<usize> = self.steps.iter().skip(tail).flat_map(|s| s.ins.iter().copied()).collect();
        wanted.extend(self.outputs.iter().copied());
        wanted.sort_unstable();
        wanted.dedup();
        let mut outputs = Vec::new();
        for t in wanted {
            // the CPU has the input itself
            if t == *input {
                continue;
            }
            let Some(v) = l.value.get(t).copied().flatten() else { continue };
            let back = l.back_to_nhwc(t, v)?;
            outputs.push((t, back));
        }
        // the last step that reads each value; values nothing reads later are free after it
        let mut last: Vec<Option<usize>> = vec![None; l.lens.len()];
        for (at, step) in l.steps.iter().enumerate() {
            if let Some(x) = last.get_mut(step.out) {
                *x = Some(at);
            }
            for a in &step.args {
                if let PlanArg::Value(v) = a
                    && let Some(x) = last.get_mut(*v)
                {
                    *x = Some(at);
                }
            }
        }
        for (v, at) in last.iter().enumerate() {
            if outputs.iter().any(|(_, o)| *o == v) {
                continue;
            }
            if let Some(step) = at.and_then(|at| l.steps.get_mut(at)) {
                step.free.push(v);
            }
        }
        Ok(Some(Plan { lens: l.lens, steps: l.steps, outputs, tail, convs: l.convs }))
    }

    /// The accelerator in use, if any.
    pub fn accelerator(&self) -> Option<Arc<dyn Accelerator>> {
        self.device.accelerator.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Stop using the accelerator: every run is on the CPU again.
    pub fn detach(&self) {
        *self.device.accelerator.write().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Whether the model has a device form at all (a one-input model with convolutions the device runs).
    pub fn can_accelerate(&self) -> bool {
        self.plan().is_some_and(|p| p.convs > 0)
    }

    /// Of the interpreter's steps, how many run on the device (the rest run on the CPU after it), and of how many.
    #[doc(hidden)]
    pub fn device_steps(&self) -> (usize, usize) {
        (self.plan().map_or(0, |p| p.tail), self.steps.len())
    }

    /// Run on `accelerator` from now on, but only after it gave the CPU's outputs on `probe` (the model's one input:
    /// every output value within `tolerance` of the CPU's, relative to that output's largest magnitude) and, with
    /// `require_faster`, ran a batch of inputs faster than the CPU ran them one after another. Returns why it was
    /// not used; the model then stays on the CPU.
    pub fn accelerate(
        &self,
        accelerator: Arc<dyn Accelerator>,
        probe: &[f32],
        tolerance: f32,
        require_faster: bool,
    ) -> std::result::Result<(), String> {
        self.detach();
        let Some(plan) = self.plan() else {
            return Err("the model has no form the GPU runs".into());
        };
        if plan.convs == 0 {
            return Err("the model has no form the GPU runs".into());
        }
        let prog_fp = plan.program().fingerprint();
        let cpu = self.run_cpu(&[probe]).map_err(|e| e.to_string())?;
        let acc = accelerator.clone();
        *self.device.accelerator.write().unwrap_or_else(PoisonError::into_inner) = Some(accelerator);
        let eff_tol = if acc.precision() == lcc_faces::net::Precision::Fp16 { tolerance.max(1e-2) } else { tolerance };
        let verdict = (|| {
            // the first run uploads the weights and builds the kernels
            let dev = self.run_on_device(&[probe]).map_err(|e| e.to_string())?.and_then(|mut v| v.pop());
            let Some(dev) = dev else { return Err("the GPU did not take the model".to_string()) };
            agree(&cpu, &dev, eff_tol)?;
            if require_faster && !acc.plan_cached(prog_fp) {
                let batch = vec![probe; BENCH_BATCH];
                let started = web_time::Instant::now();
                let ran = self.run_on_device(&batch).map_err(|e| e.to_string())?;
                let dev_time = started.elapsed();
                if ran.is_none() {
                    return Err("the GPU did not take the model".into());
                }
                let started = web_time::Instant::now();
                for x in &batch {
                    self.run_cpu(&[x]).map_err(|e| e.to_string())?;
                }
                let cpu_time = started.elapsed();
                let dev_ms = dev_time.as_secs_f64() * 1000.0;
                let cpu_ms = cpu_time.as_secs_f64() * 1000.0;
                acc.record_plan(prog_fp, dev_time < cpu_time, dev_ms, cpu_ms);
                if dev_time >= cpu_time {
                    return Err(format!(
                        "the GPU is not faster for this model ({dev_ms:.0} ms against {cpu_ms:.0} ms on the CPU for {BENCH_BATCH} runs)",
                    ));
                }
                log::info!("tflite: {BENCH_BATCH} runs take {dev_ms:.1} ms on the GPU, {cpu_ms:.1} ms on the CPU");
            }
            Ok(())
        })();
        if verdict.is_err() {
            self.detach();
        }
        verdict
    }

    /// Each of `inputs` (the model's one input) on the accelerator, the CPU finishing what the device has no form
    /// for; `Ok(None)` when there is no accelerator or it left the run to the CPU. A device error drops the
    /// accelerator (and the run is left to the CPU).
    pub(crate) fn run_on_device(&self, inputs: &[&[f32]]) -> Result<Option<Vec<Vec<Vec<f32>>>>> {
        let Some(acc) = self.accelerator() else { return Ok(None) };
        let Some(plan) = self.plan() else { return Ok(None) };
        let [input] = self.inputs.as_slice() else { return Ok(None) };
        let want = self.tensors.get(*input).map(|t| numel(&t.shape)).transpose()?.unwrap_or(0);
        if inputs.iter().any(|x| x.len() != want) {
            return Err(bad(&format!("the model takes {want} input values")));
        }
        let program = plan.program();
        let ran = match acc.run_program(&program, inputs) {
            Ok(Some(r)) if r.len() == inputs.len() => r,
            Ok(_) => return Ok(None),
            Err(e) => {
                log::warn!("tflite: the GPU failed ({e}); the model runs on the CPU from now on");
                self.detach();
                return Ok(None);
            }
        };
        let mut all = Vec::with_capacity(inputs.len());
        for (x, outs) in inputs.iter().zip(ran) {
            if outs.len() != plan.outputs.len() || outs.iter().flatten().any(|v| !v.is_finite()) {
                log::warn!("tflite: the GPU gave unusable values; the model runs on the CPU from now on");
                self.detach();
                return Ok(None);
            }
            let preset = plan.outputs.iter().map(|(t, _)| *t).zip(outs).collect();
            let vals = self.execute_from(&[x], plan.tail, preset, false, None)?;
            all.push(self.collect_outputs(vals)?);
        }
        Ok(Some(all))
    }
}

/// Whether `fast` gave `cpu`'s outputs: each value within `tolerance` of the CPU's, relative to that output's
/// largest magnitude.
fn agree(cpu: &[Vec<f32>], fast: &[Vec<f32>], tolerance: f32) -> std::result::Result<(), String> {
    if cpu.len() != fast.len() {
        return Err("the GPU gave a different number of outputs".into());
    }
    for (i, (a, b)) in cpu.iter().zip(fast).enumerate() {
        if a.len() != b.len() {
            return Err(format!("the GPU gave output {i} of another size"));
        }
        let scale = a.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-6);
        let worst = a.iter().zip(b).fold(0f32, |m, (x, y)| m.max((x - y).abs()));
        if b.iter().any(|v| !v.is_finite()) || !worst.is_finite() || worst > tolerance * scale {
            return Err(format!("the GPU's output {i} differs from the CPU's by {:.4} of its range", worst / scale));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planar_layout_reorders_channels_first() {
        // NHWC 1×1×2×3: pixel 0 = [0,1,2], pixel 1 = [3,4,5]
        let x = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(to_planar(&x, [1, 1, 2, 3]), Some(vec![0.0, 3.0, 1.0, 4.0, 2.0, 5.0]));
        assert_eq!(to_planar(&x[..4], [1, 1, 2, 3]), None);
        assert_eq!(planar_strides([1, 3, 1, 2]), [0, 2, 0, 1]);
    }

    /// Runs programs on the CPU by the definitions of their steps (the contract `lcc-gpu` implements), or fails.
    struct Reference {
        fail: bool,
        nan: bool,
    }

    fn run_step(op: &Op, args: &[&[f32]], len: usize) -> Vec<f32> {
        let x = args[0];
        match op {
            Op::Conv(j) => {
                let (m, cg) = (j.cout / j.groups, j.cin / j.groups);
                let mut out = vec![0.0; j.cout * j.oh * j.ow];
                for co in 0..j.cout {
                    let g = co / m;
                    for oy in 0..j.oh {
                        for ox in 0..j.ow {
                            let mut acc = j.bias.map_or(0.0, |b| b[co]);
                            for ci in 0..cg {
                                for ky in 0..j.kh {
                                    for kx in 0..j.kw {
                                        let iy = (oy * j.sh + ky) as isize - j.pt as isize;
                                        let ix = (ox * j.sw + kx) as isize - j.pl as isize;
                                        if iy < 0 || ix < 0 || iy >= j.h as isize || ix >= j.w as isize {
                                            continue;
                                        }
                                        let v = x[((g * cg + ci) * j.h + iy as usize) * j.w + ix as usize];
                                        acc += v * j.weights[((co * cg + ci) * j.kh + ky) * j.kw + kx];
                                    }
                                }
                            }
                            out[(co * j.oh + oy) * j.ow + ox] = acc;
                        }
                    }
                }
                out
            }
            Op::Unary(DevUnary::Relu) => x.iter().map(|v| v.max(0.0)).collect(),
            Op::Unary(DevUnary::Sigmoid) => x.iter().map(|v| 1.0 / (1.0 + (-v).exp())).collect(),
            Op::ChannelPrelu { channels, plane } => {
                (0..len).map(|i| if x[i] < 0.0 { x[i] * args[1][(i / plane) % channels] } else { x[i] }).collect()
            }
            Op::Transpose { dims, strides } | Op::Binary { dims, a: strides, .. } => (0..len)
                .map(|i| {
                    let (mut rest, mut at, mut bt) = (i, 0, 0);
                    for d in (0..4).rev() {
                        at += (rest % dims[d]) * strides[d];
                        if let Op::Binary { b, .. } = op {
                            bt += (rest % dims[d]) * b[d];
                        }
                        rest /= dims[d];
                    }
                    match op {
                        Op::Binary { op: DevBinary::Add, .. } => x[at] + args[1][bt],
                        Op::Binary { op: DevBinary::Mul, .. } => x[at] * args[1][bt],
                        Op::Binary { .. } => x[at] - args[1][bt],
                        _ => x[at],
                    }
                })
                .collect(),
            Op::MaxPool { h, w, kh, kw, sh, sw, oh, ow, .. } => (0..len)
                .map(|i| {
                    let (ox, oy, ch) = (i % ow, (i / ow) % oh, i / (ow * oh));
                    let mut m = f32::MIN;
                    for ky in 0..*kh {
                        for kx in 0..*kw {
                            let (y, xx) = (oy * sh + ky, ox * sw + kx);
                            if y < *h && xx < *w {
                                m = m.max(x[(ch * h + y) * w + xx]);
                            }
                        }
                    }
                    m
                })
                .collect(),
            other => panic!("the lowering made an unexpected step {other:?}"),
        }
    }

    impl Accelerator for Reference {
        fn device(&self) -> String {
            "reference".into()
        }
        fn conv(&self, _: &ConvJob) -> std::result::Result<Option<Vec<f32>>, String> {
            Ok(None)
        }
        fn run_program(&self, p: &Program, inputs: &[&[f32]]) -> std::result::Result<Option<Vec<Vec<Vec<f32>>>>, String> {
            if self.fail {
                return Err("the device was lost".into());
            }
            Ok(Some(
                inputs
                    .iter()
                    .map(|x| {
                        let mut vals: Vec<Option<Vec<f32>>> = vec![None; p.lens.len()];
                        vals[INPUT] = Some(x.to_vec());
                        for s in &p.steps {
                            let args: Vec<Vec<f32>> = s
                                .args
                                .iter()
                                .map(|a| match a {
                                    Arg::Value(v) => vals[*v].clone().expect("read before made"),
                                    Arg::Const { data, .. } => data.to_vec(),
                                })
                                .collect();
                            let refs: Vec<&[f32]> = args.iter().map(Vec::as_slice).collect();
                            let y = run_step(&s.op, &refs, p.lens[s.out]);
                            assert_eq!(y.len(), p.lens[s.out]);
                            vals[s.out] = Some(y);
                        }
                        p.outputs.iter().map(|(_, v, _)| if self.nan { vec![f32::NAN; p.lens[*v]] } else { vals[*v].clone().unwrap() }).collect()
                    })
                    .collect(),
            ))
        }
    }

    #[test]
    fn the_lowered_model_gives_the_interpreters_outputs() {
        let m = Model::from_bytes(&crate::tests::tiny_model(crate::CONV_2D)).unwrap();
        assert!(m.can_accelerate());
        // the convolution on the device, the reshape to [1, 12] on the CPU after it
        assert_eq!(m.device_steps(), (1, 2));
        m.accelerate(Arc::new(Reference { fail: false, nan: false }), &crate::tests::tiny_input(), 1e-5, false).unwrap();
        assert!(m.accelerator().is_some());
        let want = crate::tests::tiny_expected();
        let x = crate::tests::tiny_input();
        let y: Vec<f32> = x.iter().map(|v| -v).collect();
        for out in [m.run(&[&x]).unwrap()].into_iter().chain(m.run_batch(&[&x, &x]).unwrap()) {
            for (a, b) in out[0].iter().zip(&want) {
                assert!((a - b).abs() < 1e-5, "{a} vs {b}");
            }
        }
        let batch = m.run_batch(&[&x, &y]).unwrap();
        assert!(agree(&batch[1], &m.run_cpu(&[&y]).unwrap(), 1e-5).is_ok());
        // a wrong-sized input is an error, not a panic, with an accelerator too
        assert!(m.run_batch(&[&x[..3]]).is_err());
        assert!(m.accelerator().is_some());
    }

    #[test]
    fn a_failing_or_wrong_device_falls_back_to_the_cpu() {
        let m = Model::from_bytes(&crate::tests::tiny_model(crate::CONV_2D)).unwrap();
        let x = crate::tests::tiny_input();
        let want = m.run_cpu(&[&x]).unwrap();
        // garbage on the test input: never attached
        assert!(m.accelerate(Arc::new(Reference { fail: false, nan: true }), &x, 1e-3, false).is_err());
        assert!(m.accelerator().is_none());
        assert!(m.accelerate(Arc::new(Reference { fail: true, nan: false }), &x, 1e-3, false).is_err());
        assert!(m.accelerator().is_none());
        // attached, then lost: that run and every later one on the CPU, with the same answer
        m.accelerate(Arc::new(Reference { fail: false, nan: false }), &x, 1e-3, false).unwrap();
        *m.device.accelerator.write().unwrap() = Some(Arc::new(Reference { fail: true, nan: false }));
        assert_eq!(m.run(&[&x]).unwrap(), want);
        assert!(m.accelerator().is_none());
        assert_eq!(m.run_batch(&[&x, &x]).unwrap(), vec![want.clone(), want]);
    }

    #[test]
    fn agreement_is_relative_and_rejects_garbage() {
        assert!(agree(&[vec![10.0, -10.0]], &[vec![10.01, -9.99]], 0.01).is_ok());
        assert!(agree(&[vec![10.0, -10.0]], &[vec![11.0, -10.0]], 0.01).is_err());
        assert!(agree(&[vec![1.0]], &[vec![f32::NAN]], 0.01).is_err());
        assert!(agree(&[vec![1.0]], &[vec![1.0, 2.0]], 0.01).is_err());
        assert!(agree(&[vec![1.0]], &[], 0.01).is_err());
    }
}
