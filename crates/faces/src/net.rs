//! A small CPU interpreter for the convolutional networks LightCraft bundles (the YuNet face detector).
//!
//! It runs checked convolution, affine/activation, broadcast, pooling, matrix and shape operators on
//! float32 tensors. Unsupported semantics fail at load time. Dense convolutions use bounded im2col and
//! single-thread GEMM; depthwise convolutions use row-wise Rust loops. Every file-derived shape is checked.
//!
//! The same checked float32 engine runs YuNet and installed SFace/AuraFace recognisers.
//! Unknown operators, attributes and shapes are rejected before recognition starts.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, PoisonError, RwLock};

mod kernels;
pub mod program;
mod shape;

use crate::graph::{Data, Graph, Node};

/// Elements of any one tensor the interpreter will allocate.
const MAX_TENSOR: usize = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum NetError {
    #[error("this model uses something the built-in runtime does not support: {0}")]
    Unsupported(String),
    #[error("the model does not fit together: {0}")]
    Shape(String),
}

type Result<T> = std::result::Result<T, NetError>;

fn shape_err<T>(why: impl Into<String>) -> Result<T> {
    Err(NetError::Shape(why.into()))
}

fn unsupported<T>(why: impl Into<String>) -> Result<T> {
    Err(NetError::Unsupported(why.into()))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tensor {
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

impl Tensor {
    pub fn new(shape: Vec<usize>, data: Vec<f32>) -> Result<Tensor> {
        let n = shape.iter().try_fold(1usize, |a, d| a.checked_mul(*d));
        if shape.len() > 8 || shape.contains(&0) || n.is_none_or(|n| n > MAX_TENSOR) || n != Some(data.len()) || data.iter().any(|v| !v.is_finite()) {
            return shape_err(format!("{} values do not fill the shape {shape:?}", data.len()));
        }
        Ok(Tensor { shape, data })
    }

    fn zeros(shape: Vec<usize>) -> Result<Tensor> {
        match shape.iter().try_fold(1usize, |a, d| a.checked_mul(*d)) {
            Some(n) if n > 0 && n <= MAX_TENSOR && shape.len() <= 8 && !shape.contains(&0) => {
                let mut data = Vec::new();
                data.try_reserve_exact(n).map_err(|_| NetError::Shape("not enough memory for a model tensor".into()))?;
                data.resize(n, 0.0);
                Ok(Tensor { shape, data })
            }
            _ => shape_err("a tensor would be too large"),
        }
    }
}

/// One convolution, as handed to an [`Accelerator`]: batch 1, planar (NCHW) float32, already checked.
///
/// `out[co][oy][ox] = bias[co] + Σ weights[co][ci][ky][kx] · input[g·cg + ci][oy·sh + ky − pt][ox·sw + kx − pl]`
/// with `g = co / (cout / groups)` and zero outside the input.
#[derive(Clone, Copy, Debug)]
pub struct ConvJob<'a> {
    /// The node's index in the graph: the same for every run of a net, so its weights can stay on the device.
    pub layer: usize,
    pub cin: usize,
    pub h: usize,
    pub w: usize,
    pub cout: usize,
    pub groups: usize,
    pub kh: usize,
    pub kw: usize,
    pub sh: usize,
    pub sw: usize,
    pub pt: usize,
    pub pl: usize,
    pub oh: usize,
    pub ow: usize,
    /// `[cout][cin / groups][kh][kw]`.
    pub weights: &'a [f32],
    /// `[cout]`, if the node has one.
    pub bias: Option<&'a [f32]>,
    /// `[cin][h][w]`.
    pub input: &'a [f32],
}

impl ConvJob<'_> {
    /// Multiply-adds the convolution takes.
    pub fn macs(&self) -> usize {
        let per_group = (self.cin / self.groups.max(1)).saturating_mul(self.kh).saturating_mul(self.kw);
        self.cout.saturating_mul(per_group).saturating_mul(self.oh).saturating_mul(self.ow)
    }
}

/// Something that can run a network's convolutions faster than the CPU kernels (the GPU, in `lightcraft-gpu`).
///
/// The CPU interpreter stays the reference: a net only uses an accelerator that agreed with it on a test input
/// ([`Net::attach_checked`]), and the first error makes the net drop it and run on the CPU again.
pub trait Accelerator: Send + Sync {
    /// What it runs on, for the user ("AMD Radeon RX 7900 (Vulkan)").
    fn device(&self) -> String;
    /// `[cout][oh][ow]`, or `Ok(None)` to leave this one to the CPU (too small to be worth a dispatch, say).
    fn conv(&self, job: &ConvJob) -> std::result::Result<Option<Vec<f32>>, String>;
    /// The whole network on the device, keeping every value there between steps, for each of `inputs` (all of
    /// `program.lens[INPUT]` values) in one go: per input, each of `program.outputs` in order. `Ok(None)` leaves the
    /// run to the CPU path (which may still use [`Accelerator::conv`]).
    fn run_program(&self, _program: &program::Program, _inputs: &[&[f32]]) -> std::result::Result<Option<Vec<Vec<Vec<f32>>>>, String> {
        Ok(None)
    }
}

/// A loaded network, ready to run.
pub struct Net {
    graph: Graph,
    accelerator: RwLock<Option<Arc<dyn Accelerator>>>,
}

/// Validate semantics rather than silently ignoring unknown or mistyped attributes.
fn check(node: &Node, g: &Graph) -> Result<()> {
    shape::check(node, g)
}

impl Net {
    pub fn new(graph: Graph) -> Result<Net> {
        shape::check_graph(&graph)?;
        for n in &graph.nodes {
            check(n, &graph)?;
        }
        Ok(Net { graph, accelerator: RwLock::new(None) })
    }

    /// The accelerator in use, if any.
    pub fn accelerator(&self) -> Option<Arc<dyn Accelerator>> {
        self.accelerator.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Use `accelerator` for this net's convolutions, but only after it gave the CPU's outputs on `probe` (every
    /// output value within `tolerance` of the CPU's, relative to that output's largest magnitude) and, with
    /// `require_faster`, ran it faster than the CPU did (a GPU's per-layer round trip can cost more than a small
    /// model's layers). Returns why it was not used, so the caller can say so; the net then stays on the CPU.
    pub fn attach_checked(
        &self,
        accelerator: Arc<dyn Accelerator>,
        probe: &Tensor,
        tolerance: f32,
        require_faster: bool,
    ) -> std::result::Result<(), String> {
        self.detach();
        let started = web_time::Instant::now();
        let cpu = self.run(probe.clone()).map_err(|e| e.to_string())?;
        let cpu_time = started.elapsed();
        *self.accelerator.write().unwrap_or_else(PoisonError::into_inner) = Some(accelerator);
        // the first run also uploads the weights and builds the kernels: time a second one
        let fast = self.run(probe.clone()).map_err(|e| e.to_string());
        let mut verdict = match fast {
            Err(e) => Err(e),
            Ok(_) if self.accelerator().is_none() => Err("the device failed on the test input".into()),
            Ok(fast) => agree(&cpu, &fast, tolerance),
        };
        if verdict.is_ok() && require_faster {
            let started = web_time::Instant::now();
            let again = self.run(probe.clone()).map_err(|e| e.to_string());
            let fast_time = started.elapsed();
            verdict = match again {
                Err(e) => Err(e),
                Ok(_) if self.accelerator().is_none() => Err("the device failed on the test input".into()),
                Ok(_) if fast_time >= cpu_time => Err(format!(
                    "the device is not faster for this model ({:.0} ms against {:.0} ms on the CPU)",
                    fast_time.as_secs_f64() * 1000.0,
                    cpu_time.as_secs_f64() * 1000.0
                )),
                Ok(_) => Ok(()),
            };
        }
        if verdict.is_err() {
            self.detach();
        }
        verdict
    }

    /// Run on the CPU only from now on.
    pub fn detach(&self) {
        *self.accelerator.write().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// A convolution on the accelerator, if one is attached and takes it. A failing accelerator is dropped.
    fn accelerated_conv(&self, layer: usize, node: &Node, x: &Tensor) -> Result<Option<Tensor>> {
        let Some(accelerator) = self.accelerator() else { return Ok(None) };
        let job = kernels::conv_job(layer, node, &x.shape, &x.data, &self.graph)?;
        match accelerator.conv(&job) {
            Ok(Some(out)) if out.len() == job.cout * job.oh * job.ow => Ok(Some(Tensor::new(vec![1, job.cout, job.oh, job.ow], out)?)),
            Ok(None) => Ok(None),
            Ok(Some(_)) | Err(_) => {
                self.detach();
                Ok(None)
            }
        }
    }

    /// The shape the model declares for its input (0 for a dynamic size).
    pub fn input_shape(&self) -> Option<&[usize]> {
        self.graph.inputs.first().map(|(_, s)| s.as_slice())
    }

    /// Infer and check every intermediate without allocating its pixel data.
    /// Recognition calls this at load time with the manifest's fixed input.
    pub fn validate_input(&self, shape: &[usize]) -> Result<Vec<Vec<usize>>> {
        shape::infer(&self.graph, shape)
    }

    /// Run the network on `input` (its single input); returns each output by name.
    pub fn run(&self, input: Tensor) -> Result<Vec<(String, Tensor)>> {
        self.run_batch(vec![input])?.pop().ok_or_else(|| NetError::Shape("the run gave nothing".into()))
    }

    /// Run the network on each of `inputs`; an accelerator that keeps the values on its device runs inputs of one shape
    /// together (one upload and one read-back for all of them).
    pub fn run_batch(&self, inputs: Vec<Tensor>) -> Result<Vec<Vec<(String, Tensor)>>> {
        for input in &inputs {
            shape::tensor(&input.shape, input.data.len(), &input.data)?;
        }
        if let Some(out) = self.run_on_device(&inputs)? {
            return Ok(out);
        }
        inputs.into_iter().map(|input| self.run_one(input)).collect()
    }

    /// The whole batch on the attached accelerator, or `None` to run it on the CPU path. A failing or misbehaving
    /// accelerator is dropped.
    fn run_on_device(&self, inputs: &[Tensor]) -> Result<Option<Vec<Vec<(String, Tensor)>>>> {
        let (Some(accelerator), Some(first)) = (self.accelerator(), inputs.first()) else { return Ok(None) };
        if inputs.iter().any(|t| t.shape != first.shape) {
            return Ok(None);
        }
        let Some(program) = self.program(&first.shape)? else { return Ok(None) };
        let slices: Vec<&[f32]> = inputs.iter().map(|t| t.data.as_slice()).collect();
        let started = web_time::Instant::now();
        let outs = match accelerator.run_program(&program, &slices) {
            Ok(Some(outs)) => outs,
            Ok(None) => return Ok(None),
            Err(_) => {
                self.detach();
                return Ok(None);
            }
        };
        let gathered: Option<Vec<Vec<(String, Tensor)>>> = (outs.len() == inputs.len())
            .then(|| {
                outs.into_iter()
                    .map(|values| {
                        (values.len() == program.outputs.len())
                            .then(|| {
                                values
                                    .into_iter()
                                    .zip(&program.outputs)
                                    .map(|(data, (name, _, shape))| Tensor::new(shape.clone(), data).ok().map(|t| (name.clone(), t)))
                                    .collect::<Option<Vec<_>>>()
                            })
                            .flatten()
                    })
                    .collect()
            })
            .flatten();
        if std::env::var_os("LIGHTCRAFT_PROFILE").is_some() {
            eprintln!("[faces-gpu] {} steps x {}: {:.3} ms", program.steps.len(), inputs.len(), started.elapsed().as_secs_f64() * 1000.0);
        }
        if gathered.is_none() {
            // wrong sizes or values that are not finite: the device is not trusted any more
            self.detach();
        }
        Ok(gathered)
    }

    fn run_one(&self, input: Tensor) -> Result<Vec<(String, Tensor)>> {
        self.validate_input(&input.shape)?;
        let mut scratch = kernels::Scratch::default();
        let profile = std::env::var_os("LIGHTCRAFT_PROFILE").is_some();
        let g = &self.graph;
        let Some((input_name, _)) = g.inputs.first() else { return shape_err("the model has no input") };
        // when each value is last read, so big intermediates are dropped as soon as nothing needs them
        let mut last_use: HashMap<&str, usize> = HashMap::new();
        for (i, n) in g.nodes.iter().enumerate() {
            for name in &n.inputs {
                last_use.insert(name.as_str(), i);
            }
        }
        for o in &g.outputs {
            last_use.insert(o.as_str(), usize::MAX);
        }
        let mut values: HashMap<String, Rc<Tensor>> = HashMap::new();
        values.insert(input_name.clone(), Rc::new(input));
        for (i, node) in g.nodes.iter().enumerate() {
            let get = |idx: usize| -> Result<Rc<Tensor>> {
                let name = node.inputs.get(idx).ok_or_else(|| NetError::Shape(format!("{} is missing an input", node.op)))?;
                if let Some(value) = values.get(name) {
                    return Ok(value.clone());
                }
                if let Some(crate::graph::Weight { dims, data: Data::F32(data) }) = g.weights.get(name) {
                    return Ok(Rc::new(Tensor::new(dims.clone(), data.clone())?));
                }
                values.get(name).cloned().ok_or_else(|| NetError::Shape(format!("`{name}` is read before it is computed")))
            };
            let started = web_time::Instant::now();
            let out = match node.op.as_str() {
                "Conv" => {
                    let x = get(0)?;
                    match self.accelerated_conv(i, node, &x)? {
                        Some(out) => out,
                        None => kernels::conv(node, &x, g, &mut scratch)?,
                    }
                }
                "Relu" => map(&*get(0)?, |v| v.max(0.0)),
                "Sigmoid" => map(&*get(0)?, |v| 1.0 / (1.0 + (-v).exp())),
                "Add" | "Sub" | "Mul" => kernels::binary(node.op.as_str(), &*get(0)?, &*get(1)?)?,
                "PRelu" => kernels::prelu(node, &*get(0)?, g)?,
                "BatchNormalization" => kernels::batch_norm(node, &*get(0)?, g)?,
                "Gemm" => kernels::gemm(node, &*get(0)?, g)?,
                "Flatten" => kernels::flatten(node, &*get(0)?)?,
                "Dropout" | "Identity" => (*get(0)?).clone(),
                "GlobalAveragePool" => kernels::global_average(&*get(0)?)?,
                "MaxPool" => max_pool(node, &*get(0)?)?,
                "Resize" => resize(node, &*get(0)?, g)?,
                "Transpose" => transpose(node, &*get(0)?)?,
                "Reshape" => reshape(node, &*get(0)?, g)?,
                other => return unsupported(format!("the operator `{other}`")),
            };
            if out.data.iter().any(|v| !v.is_finite()) {
                return shape_err(format!("{} produced non-finite values", node.op));
            }
            if profile {
                eprintln!("[faces-cpu] layer {i} {} {:?}: {:.3} ms", node.op, out.shape, started.elapsed().as_secs_f64() * 1000.0);
            }
            let Some(name) = node.outputs.first() else { return shape_err("a node has no output") };
            values.insert(name.clone(), Rc::new(out));
            for name in &node.inputs {
                if last_use.get(name.as_str()) == Some(&i) {
                    values.remove(name);
                }
            }
        }
        g.outputs
            .iter()
            .map(|name| {
                let t = values.remove(name).ok_or_else(|| NetError::Shape(format!("output `{name}` was never computed")))?;
                let t = Rc::try_unwrap(t).map_err(|_| NetError::Shape(format!("output `{name}` is still shared")))?;
                Ok((name.clone(), t))
            })
            .collect()
    }
}

/// Whether two runs' outputs agree: same names and shapes, values within `tolerance` of each other relative to the
/// output's largest magnitude.
fn agree(a: &[(String, Tensor)], b: &[(String, Tensor)], tolerance: f32) -> std::result::Result<(), String> {
    if a.len() != b.len() {
        return Err("the device gave a different number of outputs".into());
    }
    for ((name, x), (other, y)) in a.iter().zip(b) {
        if name != other || x.shape != y.shape {
            return Err(format!("the device gave `{other}` a different shape"));
        }
        let scale = x.data.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-6);
        let worst = x.data.iter().zip(&y.data).fold(0f32, |m, (p, q)| m.max((p - q).abs()));
        if worst.is_nan() || worst > tolerance * scale {
            return Err(format!("the device's `{name}` differs from the CPU's by {:.2e} (of {scale:.2e})", worst));
        }
    }
    Ok(())
}

fn map(x: &Tensor, f: impl Fn(f32) -> f32) -> Tensor {
    Tensor { shape: x.shape.clone(), data: x.data.iter().map(|v| f(*v)).collect() }
}

/// `[1, c, h, w]` → `(c, h, w)`.
fn chw(x: &Tensor) -> Result<(usize, usize, usize)> {
    match x.shape.as_slice() {
        [1, c, h, w] => Ok((*c, *h, *w)),
        s => unsupported(format!("a tensor of shape {s:?} (only [1, channels, height, width])")),
    }
}

/// `pads` of an ONNX node as (top, left, bottom, right).
fn pads(node: &Node) -> Result<(usize, usize, usize, usize)> {
    match node.ints("pads") {
        None => Ok((0, 0, 0, 0)),
        Some([t, l, b, r]) => {
            let u = |v: &i64| usize::try_from(*v).map_err(|_| NetError::Shape("a negative pad".into()));
            Ok((u(t)?, u(l)?, u(b)?, u(r)?))
        }
        Some(_) => unsupported("pads that are not [top, left, bottom, right]"),
    }
}

fn pair(node: &Node, name: &str, default: usize) -> Result<(usize, usize)> {
    match node.ints(name) {
        None => Ok((default, default)),
        Some([a, b]) => match (usize::try_from(*a), usize::try_from(*b)) {
            (Ok(a), Ok(b)) if a >= 1 && b >= 1 => Ok((a, b)),
            _ => shape_err(format!("`{name}` must be positive")),
        },
        Some(_) => unsupported(format!("`{name}` that is not two numbers")),
    }
}

fn conv_direct(node: &Node, x: &Tensor, g: &Graph) -> Result<Tensor> {
    let (cin, h, w) = chw(x)?;
    let weight = node.inputs.get(1).and_then(|n| g.weights.get(n)).ok_or_else(|| NetError::Shape("a Conv lost its weights".into()))?;
    let Data::F32(wd) = &weight.data else { return unsupported("non-float Conv weights") };
    let [cout, cin_g, kh, kw] = weight.dims.as_slice() else { return unsupported("a Conv that is not 2-D") };
    let (cout, cin_g, kh, kw) = (*cout, *cin_g, *kh, *kw);
    let groups =
        usize::try_from(node.int("group").unwrap_or(1)).ok().filter(|g| *g >= 1).ok_or_else(|| NetError::Shape("a bad Conv group".into()))?;
    if groups.checked_mul(cin_g) != Some(cin) || cout % groups != 0 || kh == 0 || kw == 0 {
        return shape_err("the Conv weights do not match its input");
    }
    let bias: Option<&[f32]> = match node.inputs.get(2).filter(|b| !b.is_empty()).and_then(|b| g.weights.get(b)) {
        Some(b) => match &b.data {
            Data::F32(v) if v.len() == cout => Some(v),
            _ => return shape_err("the Conv bias does not match"),
        },
        None => None,
    };
    let (sh, sw) = pair(node, "strides", 1)?;
    let (pt, pl, pb, pr) = pads(node)?;
    let ph = h.checked_add(pt).and_then(|v| v.checked_add(pb)).ok_or_else(|| NetError::Shape("Conv padding overflow".into()))?;
    let pw = w.checked_add(pl).and_then(|v| v.checked_add(pr)).ok_or_else(|| NetError::Shape("Conv padding overflow".into()))?;
    if ph < kh || pw < kw {
        return shape_err("the Conv kernel is larger than its input");
    }
    let (oh, ow) = ((ph - kh) / sh + 1, (pw - kw) / sw + 1);
    let mut out = Tensor::zeros(vec![1, cout, oh, ow])?;
    let plane = oh * ow;
    let in_planes: Vec<&[f32]> = x.data.chunks_exact((h * w).max(1)).collect();
    let cout_g = cout / groups;
    for (co, out_plane) in out.data.chunks_exact_mut(plane.max(1)).enumerate() {
        if let Some(b) = bias.and_then(|b| b.get(co)) {
            out_plane.iter_mut().for_each(|o| *o = *b);
        }
        let group = co / cout_g;
        for ci_l in 0..cin_g {
            let Some(in_plane) = in_planes.get(group * cin_g + ci_l) else { return shape_err("a Conv input channel is missing") };
            let base = (co * cin_g + ci_l) * kh * kw;
            let Some(kernel) = wd.get(base..base + kh * kw) else { return shape_err("the Conv weights are too short") };
            for (k, wv) in kernel.iter().enumerate() {
                if *wv == 0.0 {
                    continue;
                }
                let (ky, kx) = (k / kw, k % kw);
                for oy in 0..oh {
                    // input row for this output row and kernel row (in padded coordinates minus the pad)
                    let Some(iy) = (oy * sh + ky).checked_sub(pt).filter(|iy| *iy < h) else { continue };
                    let in_row = in_plane.get(iy * w..(iy + 1) * w).unwrap_or(&[]);
                    let out_row = out_plane.get_mut(oy * ow..(oy + 1) * ow).unwrap_or(&mut []);
                    if sw == 1 {
                        // ox runs over outputs whose input column ox + kx - pl lies inside the row
                        let lo = pl.saturating_sub(kx);
                        let hi = ow.min((w + pl).saturating_sub(kx));
                        if lo < hi {
                            let src = in_row.get(lo + kx - pl..hi + kx - pl).unwrap_or(&[]);
                            let dst = out_row.get_mut(lo..hi).unwrap_or(&mut []);
                            dst.iter_mut().zip(src).for_each(|(o, i)| *o += wv * i);
                        }
                    } else {
                        for (ox, o) in out_row.iter_mut().enumerate() {
                            if let Some(ix) = (ox * sw + kx).checked_sub(pl).filter(|ix| *ix < w)
                                && let Some(i) = in_row.get(ix)
                            {
                                *o += wv * i;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(out)
}

fn max_pool(node: &Node, x: &Tensor) -> Result<Tensor> {
    let (c, h, w) = chw(x)?;
    let (kh, kw) = pair(node, "kernel_shape", 1)?;
    let (sh, sw) = pair(node, "strides", 1)?;
    let (pt, pl, pb, pr) = pads(node)?;
    if (pt, pl, pb, pr) != (0, 0, 0, 0) {
        return unsupported("a padded MaxPool");
    }
    if h < kh || w < kw {
        return shape_err("the MaxPool window is larger than its input");
    }
    let (oh, ow) = ((h - kh) / sh + 1, (w - kw) / sw + 1);
    let mut out = Tensor::zeros(vec![1, c, oh, ow])?;
    for (plane_in, plane_out) in x.data.chunks_exact((h * w).max(1)).zip(out.data.chunks_exact_mut((oh * ow).max(1))) {
        for oy in 0..oh {
            for ox in 0..ow {
                let mut m = f32::NEG_INFINITY;
                for ky in 0..kh {
                    let row = plane_in.get((oy * sh + ky) * w..(oy * sh + ky + 1) * w).unwrap_or(&[]);
                    for kx in 0..kw {
                        if let Some(v) = row.get(ox * sw + kx) {
                            m = m.max(*v);
                        }
                    }
                }
                if let Some(o) = plane_out.get_mut(oy * ow + ox) {
                    *o = m;
                }
            }
        }
    }
    Ok(out)
}

/// Nearest-neighbour upscale by whole numbers (YuNet's feature pyramid doubles each map).
fn resize(node: &Node, x: &Tensor, g: &Graph) -> Result<Tensor> {
    let (c, h, w) = chw(x)?;
    let scales = match node.inputs.get(2).and_then(|n| g.weights.get(n)).map(|w| &w.data) {
        Some(Data::F32(s)) if s.len() == 4 => s,
        _ => return unsupported("a Resize whose scales are not four floats"),
    };
    let whole = |v: f32| ((1.0..=16.0).contains(&v) && v.fract() == 0.0).then_some(v as usize);
    let (Some(sh), Some(sw)) = (whole(scales.get(2).copied().unwrap_or(0.0)), whole(scales.get(3).copied().unwrap_or(0.0))) else {
        return unsupported("a Resize by a scale that is not a whole number from 1 to 16");
    };
    if scales.first() != Some(&1.0) || scales.get(1) != Some(&1.0) {
        return unsupported("a Resize that scales channels");
    }
    let (oh, ow) = (h * sh, w * sw);
    let mut out = Tensor::zeros(vec![1, c, oh, ow])?;
    for (plane_in, plane_out) in x.data.chunks_exact((h * w).max(1)).zip(out.data.chunks_exact_mut((oh * ow).max(1))) {
        for (oy, row_out) in plane_out.chunks_exact_mut(ow.max(1)).enumerate() {
            let row_in = plane_in.get((oy / sh) * w..(oy / sh + 1) * w).unwrap_or(&[]);
            for (ox, o) in row_out.iter_mut().enumerate() {
                *o = row_in.get(ox / sw).copied().unwrap_or(0.0);
            }
        }
    }
    Ok(out)
}

fn transpose(node: &Node, x: &Tensor) -> Result<Tensor> {
    let default_perm: Vec<i64> = (0..x.shape.len() as i64).rev().collect();
    let perm: Vec<usize> = node
        .ints("perm")
        .unwrap_or(&default_perm)
        .iter()
        .map(|p| usize::try_from(*p).map_err(|_| NetError::Shape("a negative permutation".into())))
        .collect::<Result<_>>()?;
    let rank = x.shape.len();
    let mut seen = vec![false; rank];
    if perm.len() != rank || perm.iter().any(|p| *p >= rank || std::mem::replace(seen.get_mut(*p).unwrap_or(&mut true), true)) {
        return shape_err("the Transpose permutation does not match the tensor");
    }
    let out_shape: Vec<usize> = perm.iter().filter_map(|p| x.shape.get(*p).copied()).collect();
    // strides of the input, in elements
    let mut in_strides = vec![1usize; rank];
    for i in (0..rank.saturating_sub(1)).rev() {
        let v = in_strides.get(i + 1).copied().unwrap_or(1) * x.shape.get(i + 1).copied().unwrap_or(1);
        *in_strides.get_mut(i).ok_or_else(|| NetError::Shape("missing transpose stride".into()))? = v;
    }
    let strides_for_out: Vec<usize> = perm.iter().filter_map(|p| in_strides.get(*p).copied()).collect();
    let mut out = Tensor::zeros(out_shape.clone())?;
    let mut counter = vec![0usize; rank];
    let mut src = 0usize;
    for o in out.data.iter_mut() {
        *o = x.data.get(src).copied().unwrap_or(0.0);
        // advance the multi-index of the output and the matching input offset
        for d in (0..rank).rev() {
            let (Some(c), Some(len), Some(stride)) = (counter.get_mut(d), out_shape.get(d), strides_for_out.get(d)) else { break };
            *c += 1;
            src += stride;
            if *c < *len {
                break;
            }
            src -= stride * *len;
            *c = 0;
        }
    }
    Ok(out)
}

fn reshape(node: &Node, x: &Tensor, g: &Graph) -> Result<Tensor> {
    let Some(Data::I64(dims)) = node.inputs.get(1).and_then(|n| g.weights.get(n)).map(|w| &w.data) else {
        return unsupported("a Reshape with a computed shape");
    };
    let total = x.data.len();
    let mut shape: Vec<usize> = Vec::with_capacity(dims.len());
    let mut infer = None;
    for (i, d) in dims.iter().enumerate() {
        match *d {
            -1 if infer.is_none() => {
                infer = Some(i);
                shape.push(1);
            }
            0 => shape.push(x.shape.get(i).copied().ok_or_else(|| NetError::Shape("a Reshape copies a missing dimension".into()))?),
            d if d > 0 => shape.push(usize::try_from(d).map_err(|_| NetError::Shape("a Reshape dimension is too large".into()))?),
            _ => return shape_err("a bad Reshape dimension"),
        }
    }
    let known = shape.iter().try_fold(1usize, |a, d| a.checked_mul(*d)).filter(|k| *k > 0);
    if let (Some(i), Some(known)) = (infer, known) {
        if !total.is_multiple_of(known) {
            return shape_err("the Reshape does not divide evenly");
        }
        if let Some(s) = shape.get_mut(i) {
            *s = total / known;
        }
    }
    Tensor::new(shape, x.data.clone())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod recognition_tests;
