//! A small CPU interpreter for the convolutional networks LightCraft bundles (the YuNet face detector).
//!
//! It runs exactly the operators such a network uses (Conv, Relu, Sigmoid, MaxPool, Resize by whole-number
//! nearest-neighbour scales, Add, Transpose, Reshape) on float32 NCHW tensors with a batch of one, and refuses
//! anything else with an error, so an unknown model fails to load rather than computing nonsense. It is pure
//! Rust with no dependencies, and the inner loops are row-wise multiply-adds over slices that the compiler can
//! vectorise. Every shape, stride and size comes from the file, so all of them are checked.
//!
//! This is not a general runtime: models users install run on `tract` (see the plan), not here.

use std::collections::HashMap;
use std::rc::Rc;

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
        if n != Some(data.len()) {
            return shape_err(format!("{} values do not fill the shape {shape:?}", data.len()));
        }
        Ok(Tensor { shape, data })
    }

    fn zeros(shape: Vec<usize>) -> Result<Tensor> {
        match shape.iter().try_fold(1usize, |a, d| a.checked_mul(*d)) {
            Some(n) if n <= MAX_TENSOR => Ok(Tensor { shape, data: vec![0.0; n] }),
            _ => shape_err("a tensor would be too large"),
        }
    }
}

/// A loaded network, ready to run.
pub struct Net {
    graph: Graph,
}

/// Which operators and attribute combinations are supported, checked once when the network is built.
fn check(node: &Node, g: &Graph) -> Result<()> {
    let weight = |i: usize| node.inputs.get(i).and_then(|n| g.weights.get(n));
    match node.op.as_str() {
        "Conv" => {
            let Some(w) = weight(1) else { return unsupported("a Conv without constant weights") };
            if w.dims.len() != 4 || !matches!(w.data, Data::F32(_)) {
                return unsupported("a Conv that is not a 2-D float convolution");
            }
            if node.ints("dilations").is_some_and(|d| d.iter().any(|v| *v != 1)) {
                return unsupported("a dilated Conv");
            }
            if node.string("auto_pad").is_some_and(|s| s != "NOTSET") {
                return unsupported("a Conv with auto_pad");
            }
            if node.inputs.get(2).is_some_and(|b| !b.is_empty() && !g.weights.contains_key(b)) {
                return unsupported("a Conv with a computed bias");
            }
        }
        "Relu" | "Sigmoid" | "Add" => {}
        "MaxPool" => {
            if node.int("ceil_mode").unwrap_or(0) != 0 || node.ints("kernel_shape").is_none_or(|k| k.len() != 2) {
                return unsupported("this MaxPool");
            }
            if node.ints("dilations").is_some_and(|d| d.iter().any(|v| *v != 1)) {
                return unsupported("a dilated MaxPool");
            }
        }
        "Resize" => {
            if node.string("mode").unwrap_or("nearest") != "nearest" {
                return unsupported("a Resize that is not nearest-neighbour");
            }
            if !node.inputs.get(2).is_some_and(|s| g.weights.contains_key(s)) {
                return unsupported("a Resize without constant scales");
            }
        }
        "Transpose" => {
            if node.ints("perm").is_none() {
                return unsupported("a Transpose without a permutation");
            }
        }
        "Reshape" => {
            if !node.inputs.get(1).is_some_and(|s| matches!(g.weights.get(s).map(|w| &w.data), Some(Data::I64(_)))) {
                return unsupported("a Reshape with a computed shape");
            }
        }
        other => return unsupported(format!("the operator `{other}`")),
    }
    Ok(())
}

impl Net {
    pub fn new(graph: Graph) -> Result<Net> {
        for n in &graph.nodes {
            check(n, &graph)?;
        }
        Ok(Net { graph })
    }

    /// The shape the model declares for its input (0 for a dynamic size).
    pub fn input_shape(&self) -> Option<&[usize]> {
        self.graph.inputs.first().map(|(_, s)| s.as_slice())
    }

    /// Run the network on `input` (its single input); returns each output by name.
    pub fn run(&self, input: Tensor) -> Result<Vec<(String, Tensor)>> {
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
                values.get(name).cloned().ok_or_else(|| NetError::Shape(format!("`{name}` is read before it is computed")))
            };
            let out = match node.op.as_str() {
                "Conv" => conv(node, &*get(0)?, g)?,
                "Relu" => map(&*get(0)?, |v| v.max(0.0)),
                "Sigmoid" => map(&*get(0)?, |v| 1.0 / (1.0 + (-v).exp())),
                "Add" => add(&*get(0)?, &*get(1)?)?,
                "MaxPool" => max_pool(node, &*get(0)?)?,
                "Resize" => resize(node, &*get(0)?, g)?,
                "Transpose" => transpose(node, &*get(0)?)?,
                "Reshape" => reshape(node, &*get(0)?, g)?,
                other => return unsupported(format!("the operator `{other}`")),
            };
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
                let t = values.get(name).ok_or_else(|| NetError::Shape(format!("output `{name}` was never computed")))?;
                Ok((name.clone(), (**t).clone()))
            })
            .collect()
    }
}

fn map(x: &Tensor, f: impl Fn(f32) -> f32) -> Tensor {
    Tensor { shape: x.shape.clone(), data: x.data.iter().map(|v| f(*v)).collect() }
}

fn add(a: &Tensor, b: &Tensor) -> Result<Tensor> {
    if a.shape != b.shape {
        return unsupported("an Add that broadcasts");
    }
    Ok(Tensor { shape: a.shape.clone(), data: a.data.iter().zip(&b.data).map(|(x, y)| x + y).collect() })
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

fn conv(node: &Node, x: &Tensor, g: &Graph) -> Result<Tensor> {
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
    let (ph, pw) = (h + pt + pb, w + pl + pr);
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
    let perm: Vec<usize> = node
        .ints("perm")
        .unwrap_or(&[])
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
        in_strides[i] = in_strides.get(i + 1).copied().unwrap_or(1) * x.shape.get(i + 1).copied().unwrap_or(1);
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
mod tests {
    use super::*;
    use crate::graph::{Attr, Weight};

    fn node(op: &str, inputs: &[&str], attrs: &[(&str, Attr)]) -> Node {
        Node {
            op: op.into(),
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            outputs: vec!["y".into()],
            attrs: attrs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        }
    }
    fn graph(nodes: Vec<Node>, weights: Vec<(&str, Weight)>) -> Graph {
        Graph {
            nodes,
            weights: weights.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            inputs: vec![("x".into(), vec![1, 1, 3, 3])],
            outputs: vec!["y".into()],
        }
    }
    fn w(dims: &[usize], v: Vec<f32>) -> Weight {
        Weight { dims: dims.to_vec(), data: Data::F32(v) }
    }
    fn input(c: usize, h: usize, wd: usize, f: impl Fn(usize) -> f32) -> Tensor {
        Tensor::new(vec![1, c, h, wd], (0..c * h * wd).map(f).collect()).unwrap()
    }
    fn run(g: Graph, x: Tensor) -> Result<Tensor> {
        Ok(Net::new(g)?.run(x)?.remove(0).1)
    }

    /// A direct, slow reference convolution to check the fast one against.
    fn reference_conv(x: &Tensor, wd: &[f32], dims: [usize; 4], bias: &[f32], s: usize, p: usize, groups: usize) -> Tensor {
        let (c, h, wi) = (x.shape[1], x.shape[2], x.shape[3]);
        let [co_n, cin_g, kh, kw] = dims;
        let (oh, ow) = ((h + 2 * p - kh) / s + 1, (wi + 2 * p - kw) / s + 1);
        let mut out = vec![0.0f32; co_n * oh * ow];
        for co in 0..co_n {
            let g = co / (co_n / groups);
            for oy in 0..oh {
                for ox in 0..ow {
                    let mut acc = bias[co];
                    for cl in 0..cin_g {
                        for ky in 0..kh {
                            for kx in 0..kw {
                                let (iy, ix) = ((oy * s + ky) as isize - p as isize, (ox * s + kx) as isize - p as isize);
                                if iy >= 0 && ix >= 0 && (iy as usize) < h && (ix as usize) < wi {
                                    acc +=
                                        x.data[((g * cin_g + cl) * h + iy as usize) * wi + ix as usize] * wd[((co * cin_g + cl) * kh + ky) * kw + kx];
                                }
                            }
                        }
                    }
                    out[(co * oh + oy) * ow + ox] = acc;
                }
            }
        }
        let _ = c;
        Tensor::new(vec![1, co_n, oh, ow], out).unwrap()
    }

    #[test]
    fn convolutions_match_a_slow_reference() {
        // (cin, cout, groups, kernel, stride, pad, h, w)
        for (cin, cout, groups, k, s, p, h, wi) in [
            (3, 4, 1, 3, 1, 1, 7, 9),
            (3, 4, 1, 3, 2, 1, 8, 8),
            (4, 4, 4, 3, 1, 1, 6, 5),
            (2, 6, 1, 1, 1, 0, 5, 5),
            (4, 8, 2, 3, 1, 1, 5, 6),
            (1, 1, 1, 3, 1, 0, 3, 3),
        ] {
            let cin_g = cin / groups;
            let wd: Vec<f32> = (0..cout * cin_g * k * k).map(|i| ((i * 37 % 11) as f32 - 5.0) * 0.1).collect();
            let bias: Vec<f32> = (0..cout).map(|i| i as f32 * 0.25 - 0.5).collect();
            let x = input(cin, h, wi, |i| ((i * 13 % 17) as f32 - 8.0) * 0.2);
            let attrs = [("group", Attr::Int(groups as i64)), ("strides", Attr::Ints(vec![s as i64; 2])), ("pads", Attr::Ints(vec![p as i64; 4]))];
            let g = graph(
                vec![node("Conv", &["x", "w", "b"], &attrs)],
                vec![("w", w(&[cout, cin_g, k, k], wd.clone())), ("b", w(&[cout], bias.clone()))],
            );
            let got = run(g, x.clone()).unwrap();
            let want = reference_conv(&x, &wd, [cout, cin_g, k, k], &bias, s, p, groups);
            assert_eq!(got.shape, want.shape, "{cin} {cout} {groups} {k} {s} {p}");
            for (a, b) in got.data.iter().zip(&want.data) {
                assert!((a - b).abs() < 1e-4, "{cin} {cout} {groups} {k} {s} {p}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn the_small_operators() {
        let x = input(1, 4, 4, |i| i as f32 - 6.0);
        // Relu, Sigmoid
        let r = run(graph(vec![node("Relu", &["x"], &[])], vec![]), x.clone()).unwrap();
        assert!(r.data.iter().all(|v| *v >= 0.0) && r.data.get(7) == Some(&1.0));
        let s = run(graph(vec![node("Sigmoid", &["x"], &[])], vec![]), x.clone()).unwrap();
        assert!((s.data[6] - 0.5).abs() < 1e-6 && s.data.iter().all(|v| (0.0..=1.0).contains(v)));
        // MaxPool 2x2/2
        let m = run(
            graph(vec![node("MaxPool", &["x"], &[("kernel_shape", Attr::Ints(vec![2, 2])), ("strides", Attr::Ints(vec![2, 2]))])], vec![]),
            x.clone(),
        )
        .unwrap();
        assert_eq!((m.shape.clone(), m.data.clone()), (vec![1, 1, 2, 2], vec![-1.0, 1.0, 7.0, 9.0]));
        // Resize x2 nearest
        let z = run(graph(vec![node("Resize", &["x", "", "s"], &[])], vec![("s", w(&[4], vec![1.0, 1.0, 2.0, 2.0]))]), input(1, 2, 2, |i| i as f32))
            .unwrap();
        assert_eq!(z.shape, vec![1, 1, 4, 4]);
        assert_eq!(&z.data[..8], &[0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0]);
        // Add
        let a = run(graph(vec![node("Add", &["x", "x"], &[])], vec![]), x.clone()).unwrap();
        assert_eq!(a.data[5], 2.0 * x.data[5]);
        // Transpose [0,2,3,1] then Reshape [1,-1,2]
        let t = run(graph(vec![node("Transpose", &["x"], &[("perm", Attr::Ints(vec![0, 2, 3, 1]))])], vec![]), input(2, 2, 3, |i| i as f32)).unwrap();
        assert_eq!(t.shape, vec![1, 2, 3, 2]);
        assert_eq!(t.data, vec![0.0, 6.0, 1.0, 7.0, 2.0, 8.0, 3.0, 9.0, 4.0, 10.0, 5.0, 11.0]);
        let rs = run(
            graph(vec![node("Reshape", &["x", "s"], &[])], vec![("s", Weight { dims: vec![3], data: Data::I64(vec![1, -1, 2]) })]),
            input(2, 2, 3, |i| i as f32),
        )
        .unwrap();
        assert_eq!(rs.shape, vec![1, 6, 2]);
    }

    #[test]
    fn unsupported_and_inconsistent_models_are_errors() {
        let x = input(1, 3, 3, |i| i as f32);
        for (what, g) in [
            ("unknown op", graph(vec![node("Softmax", &["x"], &[])], vec![])),
            (
                "dilated conv",
                graph(vec![node("Conv", &["x", "w"], &[("dilations", Attr::Ints(vec![2, 2]))])], vec![("w", w(&[1, 1, 1, 1], vec![1.0]))]),
            ),
            ("conv without weights", graph(vec![node("Conv", &["x", "w"], &[])], vec![])),
            ("channels do not match", graph(vec![node("Conv", &["x", "w"], &[])], vec![("w", w(&[1, 5, 1, 1], vec![1.0; 5]))])),
            ("kernel too big", graph(vec![node("Conv", &["x", "w"], &[])], vec![("w", w(&[1, 1, 5, 5], vec![1.0; 25]))])),
            ("zero group", graph(vec![node("Conv", &["x", "w"], &[("group", Attr::Int(0))])], vec![("w", w(&[1, 1, 1, 1], vec![1.0]))])),
            ("zero stride", graph(vec![node("Conv", &["x", "w"], &[("strides", Attr::Ints(vec![0, 0]))])], vec![("w", w(&[1, 1, 1, 1], vec![1.0]))])),
            (
                "negative pad",
                graph(vec![node("Conv", &["x", "w"], &[("pads", Attr::Ints(vec![-1, 0, 0, 0]))])], vec![("w", w(&[1, 1, 1, 1], vec![1.0]))]),
            ),
            ("bad perm", graph(vec![node("Transpose", &["x"], &[("perm", Attr::Ints(vec![0, 0, 1, 2]))])], vec![])),
            (
                "huge reshape",
                graph(vec![node("Reshape", &["x", "s"], &[])], vec![("s", Weight { dims: vec![2], data: Data::I64(vec![i64::MAX, 1]) })]),
            ),
            ("reshape mismatch", graph(vec![node("Reshape", &["x", "s"], &[])], vec![("s", Weight { dims: vec![2], data: Data::I64(vec![4, 4]) })])),
            ("non-integer resize", graph(vec![node("Resize", &["x", "", "s"], &[])], vec![("s", w(&[4], vec![1.0, 1.0, 1.5, 1.5]))])),
            ("add that broadcasts", graph(vec![node("Add", &["x", "c"], &[])], vec![])),
        ] {
            assert!(run(g, x.clone()).is_err(), "{what}");
        }
    }
}
