//! Safe CPU kernels shared by detection and recognition. No inner thread pool.
use super::{ConvJob, Graph, MAX_TENSOR, NetError, Node, Result, Tensor, chw, conv_direct, pads, pair, program, shape, shape_err};
use ndarray::{ArrayView2, ArrayViewMut2, linalg::general_mat_mul};
const BLOCK: usize = 256;
#[derive(Default)]
pub(super) struct Scratch {
    lower: Vec<f32>,
    product: Vec<f32>,
}
fn storage(v: &mut Vec<f32>, n: usize) -> Result<&mut [f32]> {
    if n > MAX_TENSOR {
        return shape_err("convolution scratch exceeds the limit");
    }
    if v.len() < n {
        v.try_reserve_exact(n - v.len()).map_err(|_| NetError::Shape("not enough memory for convolution scratch".into()))?;
        v.resize(n, 0.0);
    }
    v.get_mut(..n).ok_or_else(|| NetError::Shape("missing convolution scratch".into()))
}
/// The safe matrix views assert lengths, so validate exact slices before creating any view.
fn multiply(w: &[f32], x: &[f32], y: &mut [f32], m: usize, k: usize, n: usize) -> Result<()> {
    if m == 0 || k == 0 || n == 0 || m.checked_mul(k) != Some(w.len()) || k.checked_mul(n) != Some(x.len()) || m.checked_mul(n) != Some(y.len()) {
        return shape_err("invalid GEMM dimensions");
    }
    let a = ArrayView2::from_shape((m, k), w).map_err(|e| NetError::Shape(e.to_string()))?;
    let b = ArrayView2::from_shape((k, n), x).map_err(|e| NetError::Shape(e.to_string()))?;
    let mut c = ArrayViewMut2::from_shape((m, n), y).map_err(|e| NetError::Shape(e.to_string()))?;
    general_mat_mul(1.0, &a, &b, 0.0, &mut c);
    Ok(())
}
/// For equal-width, unit-stride maps a shifted plane is one contiguous copy. Only horizontal
/// border stripes need masking; clipping the flat source range also removes vertical padding.
#[derive(Clone, Copy)]
struct PlaneShift {
    width: usize,
    kernel: (usize, usize),
    padding: (usize, usize),
    start: usize,
}

fn shifted_plane(plane: &[f32], row: &mut [f32], shift: PlaneShift) -> Result<()> {
    let PlaneShift { width, kernel: (ky, kx), padding: (pt, pl), start } = shift;
    let end = start + row.len();
    let delta = (ky as i128 - pt as i128) * width as i128 + kx as i128 - pl as i128;
    let lo = (-delta).clamp(start as i128, end as i128);
    let hi = (plane.len() as i128 - delta).clamp(start as i128, end as i128);
    row.fill(0.0);
    if lo < hi {
        let from = usize::try_from(lo + delta).map_err(|_| NetError::Shape("shifted source overflow".into()))?;
        let at = usize::try_from(lo - start as i128).map_err(|_| NetError::Shape("shifted destination overflow".into()))?;
        let len = usize::try_from(hi - lo).map_err(|_| NetError::Shape("shifted length overflow".into()))?;
        row.get_mut(at..at + len)
            .ok_or_else(|| NetError::Shape("short shifted destination".into()))?
            .copy_from_slice(plane.get(from..from + len).ok_or_else(|| NetError::Shape("short shifted source".into()))?);
    }
    let left = pl.saturating_sub(kx).min(width);
    let right = (width + pl).saturating_sub(kx).min(width);
    if left > 0 || right < width {
        for y in start / width..=(end - 1) / width {
            for (a, b) in [(y * width, y * width + left), (y * width + right, (y + 1) * width)] {
                let lo = a.max(start);
                let hi = b.min(end);
                if lo < hi {
                    row.get_mut(lo - start..hi - start).ok_or_else(|| NetError::Shape("short border stripe".into()))?.fill(0.0);
                }
            }
        }
    }
    Ok(())
}
/// The checked geometry of a convolution node, for an accelerator.
pub(super) fn conv_job<'a>(layer: usize, n: &Node, shape: &[usize], input: &'a [f32], g: &'a Graph) -> Result<ConvJob<'a>> {
    let (cin, h, w) = program::chw_of(shape)?;
    let wt = shape::weight(n, g, 1)?;
    let weights = shape::floats(n, g, 1)?;
    let [cout, cg, kh, kw] = wt.dims.as_slice() else {
        return shape_err("invalid convolution weights");
    };
    let (cout, cg, kh, kw) = (*cout, *cg, *kh, *kw);
    let groups = usize::try_from(n.int("group").unwrap_or(1)).map_err(|_| NetError::Shape("invalid group".into()))?;
    let (sh, sw) = pair(n, "strides", 1)?;
    let (pt, pl, pb, pr) = pads(n)?;
    let ph = h.checked_add(pt).and_then(|v| v.checked_add(pb)).ok_or_else(|| NetError::Shape("padding overflow".into()))?;
    let pw = w.checked_add(pl).and_then(|v| v.checked_add(pr)).ok_or_else(|| NetError::Shape("padding overflow".into()))?;
    if ph < kh || pw < kw || groups == 0 || cg == 0 || kh == 0 || kw == 0 || cin != groups * cg || cout % groups != 0 {
        return shape_err("invalid convolution shape");
    }
    if cout.checked_mul(cg).and_then(|v| v.checked_mul(kh * kw)) != Some(weights.len()) {
        return shape_err("short convolution weights");
    }
    let bias = if n.inputs.get(2).is_some_and(|s| !s.is_empty()) { Some(shape::floats(n, g, 2)?) } else { None };
    if bias.is_some_and(|b| b.len() != cout) {
        return shape_err("the convolution bias does not match");
    }
    let (oh, ow) = ((ph - kh) / sh + 1, (pw - kw) / sw + 1);
    Ok(ConvJob { layer, cin, h, w, cout, groups, kh, kw, sh, sw, pt, pl, oh, ow, weights, bias, input })
}
pub(super) fn conv(n: &Node, x: &Tensor, g: &Graph, scratch: &mut Scratch) -> Result<Tensor> {
    let (cin, h, w) = chw(x)?;
    let wt = shape::weight(n, g, 1)?;
    let wd = shape::floats(n, g, 1)?;
    let [cout, cg, kh, kw] = wt.dims.as_slice() else {
        return shape_err("invalid convolution weights");
    };
    let (cout, cg, kh, kw) = (*cout, *cg, *kh, *kw);
    let groups = usize::try_from(n.int("group").unwrap_or(1)).map_err(|_| NetError::Shape("invalid group".into()))?;
    if cg == 1 {
        return conv_direct(n, x, g);
    }
    let (sh, sw) = pair(n, "strides", 1)?;
    let (pt, pl, pb, pr) = pads(n)?;
    let ph = h.checked_add(pt).and_then(|v| v.checked_add(pb)).ok_or_else(|| NetError::Shape("padding overflow".into()))?;
    let pw = w.checked_add(pl).and_then(|v| v.checked_add(pr)).ok_or_else(|| NetError::Shape("padding overflow".into()))?;
    if ph < kh || pw < kw || groups == 0 || cin != groups * cg {
        return shape_err("invalid convolution shape");
    }
    let (oh, ow) = ((ph - kh) / sh + 1, (pw - kw) / sw + 1);
    let mut out = Tensor::zeros(vec![1, cout, oh, ow])?;
    let pixels = oh * ow;
    let k = cg * kh * kw;
    let m = cout / groups;
    let bias = if n.inputs.get(2).is_some_and(|s| !s.is_empty()) { Some(shape::floats(n, g, 2)?) } else { None };
    for group in 0..groups {
        let weights = wd.get(group * m * k..(group + 1) * m * k).ok_or_else(|| NetError::Shape("short convolution weights".into()))?;
        // A pointwise convolution is already a matrix: no lowering, copy or spatial blocking needed.
        if kh == 1 && kw == 1 && sh == 1 && sw == 1 && (pt, pl, pb, pr) == (0, 0, 0, 0) {
            let input = x.data.get(group * cg * pixels..(group + 1) * cg * pixels).ok_or_else(|| NetError::Shape("short pointwise input".into()))?;
            let output =
                out.data.get_mut(group * m * pixels..(group + 1) * m * pixels).ok_or_else(|| NetError::Shape("short pointwise output".into()))?;
            multiply(weights, input, output, m, cg, pixels)?;
            for (co, plane) in output.chunks_exact_mut(pixels).enumerate() {
                let b = bias.and_then(|v| v.get(group * m + co)).copied().unwrap_or(0.0);
                if b != 0.0 {
                    for v in plane {
                        *v += b;
                    }
                }
            }
            continue;
        }
        for start in (0..pixels).step_by(BLOCK) {
            let len = BLOCK.min(pixels - start);
            let lower = storage(&mut scratch.lower, k * len)?;
            for cl in 0..cg {
                let plane = x
                    .data
                    .get((group * cg + cl) * h * w..(group * cg + cl + 1) * h * w)
                    .ok_or_else(|| NetError::Shape("short convolution input".into()))?;
                for ky in 0..kh {
                    for kx in 0..kw {
                        let row = lower
                            .get_mut((cl * kh * kw + ky * kw + kx) * len..(cl * kh * kw + ky * kw + kx + 1) * len)
                            .ok_or_else(|| NetError::Shape("short im2col row".into()))?;
                        if sh == 1 && sw == 1 && ow == w {
                            shifted_plane(
                                plane,
                                row.get_mut(..len).ok_or_else(|| NetError::Shape("short lower panel".into()))?,
                                PlaneShift { width: w, kernel: (ky, kx), padding: (pt, pl), start },
                            )?;
                            continue;
                        }
                        let mut at = 0;
                        while at < len {
                            let p = start + at;
                            let (oy, ox) = (p / ow, p % ow);
                            let span = (ow - ox).min(len - at);
                            if let Some(yy) = (oy * sh + ky).checked_sub(pt).filter(|&v| v < h) {
                                let lo = ox.max(pl.saturating_sub(kx).div_ceil(sw));
                                let hi = (ox + span).min((w + pl).saturating_sub(kx).div_ceil(sw));
                                if lo < hi {
                                    let from = yy * w + lo * sw + kx - pl;
                                    let to = at + lo - ox;
                                    row.get_mut(at..to).ok_or_else(|| NetError::Shape("short im2col padding".into()))?.fill(0.0);
                                    let src = plane
                                        .get(from..from + (hi - lo - 1) * sw + 1)
                                        .ok_or_else(|| NetError::Shape("short im2col source row".into()))?;
                                    let dst = row.get_mut(to..to + hi - lo).ok_or_else(|| NetError::Shape("short im2col row".into()))?;
                                    if sw == 1 {
                                        dst.copy_from_slice(src);
                                    } else {
                                        for (o, &v) in dst.iter_mut().zip(src.iter().step_by(sw)) {
                                            *o = v;
                                        }
                                    }
                                    row.get_mut(to + hi - lo..at + span).ok_or_else(|| NetError::Shape("short im2col padding".into()))?.fill(0.0);
                                } else {
                                    row.get_mut(at..at + span).ok_or_else(|| NetError::Shape("short im2col row".into()))?.fill(0.0);
                                }
                            } else {
                                row.get_mut(at..at + span).ok_or_else(|| NetError::Shape("short im2col row".into()))?.fill(0.0);
                            }
                            at += span;
                        }
                    }
                }
            }
            let product = storage(&mut scratch.product, m * len)?;
            multiply(weights, lower, product, m, k, len)?;
            for (co, row) in product.chunks_exact(len).enumerate() {
                let channel = group * m + co;
                let b = bias.and_then(|v| v.get(channel)).copied().unwrap_or(0.0);
                let dest = out
                    .data
                    .get_mut(channel * pixels + start..channel * pixels + start + len)
                    .ok_or_else(|| NetError::Shape("short convolution output".into()))?;
                for (to, &value) in dest.iter_mut().zip(row) {
                    *to = value + b;
                }
            }
        }
    }
    Ok(out)
}
/// Flat offset under ONNX's trailing-dimension broadcasting; shapes are checked before execution.
pub(super) fn broadcast_offset(mut at: usize, out: &[usize], input: &[usize]) -> usize {
    let mut offset = 0;
    let mut stride = 1;
    for (axis, &size) in out.iter().enumerate().rev() {
        let coordinate = at % size;
        at /= size;
        if let Some(&d) = axis.checked_sub(out.len() - input.len()).and_then(|i| input.get(i)) {
            if d != 1 {
                offset += coordinate * stride;
            }
            stride *= d;
        }
    }
    offset
}
pub(super) fn binary(op: &str, a: &Tensor, b: &Tensor) -> Result<Tensor> {
    let dims = shape::broadcast(&a.shape, &b.shape)?;
    let mut out = Tensor::zeros(dims)?;
    let f = |x: f32, y: f32| match op {
        "Add" => x + y,
        "Sub" => x - y,
        _ => x * y,
    };
    if a.shape == b.shape {
        for ((o, &x), &y) in out.data.iter_mut().zip(&a.data).zip(&b.data) {
            *o = f(x, y);
        }
    } else if b.data.len() == 1 && a.shape == out.shape {
        let y = *b.data.first().ok_or_else(|| NetError::Shape("empty scalar".into()))?;
        for (o, &x) in out.data.iter_mut().zip(&a.data) {
            *o = f(x, y);
        }
    } else {
        for (i, o) in out.data.iter_mut().enumerate() {
            let x = a.data.get(broadcast_offset(i, &out.shape, &a.shape)).ok_or_else(|| NetError::Shape("bad broadcast A".into()))?;
            let y = b.data.get(broadcast_offset(i, &out.shape, &b.shape)).ok_or_else(|| NetError::Shape("bad broadcast B".into()))?;
            *o = f(*x, *y);
        }
    }
    Ok(out)
}
pub(super) fn prelu(n: &Node, x: &Tensor, g: &Graph) -> Result<Tensor> {
    let wt = shape::weight(n, g, 1)?;
    let slope = shape::floats(n, g, 1)?;
    let mut out = Tensor::zeros(x.shape.clone())?;
    let channels = x.shape.get(1).copied().unwrap_or(1);
    let mut expanded = vec![1; x.shape.len().saturating_sub(wt.dims.len())];
    expanded.extend_from_slice(&wt.dims);
    let channel_slope = expanded.iter().enumerate().all(|(i, &d)| d == 1 || i == 1) && slope.len() == channels;
    if channel_slope || slope.len() == 1 {
        let plane = shape::elements(x.shape.get(2..).unwrap_or(&[]))?;
        for (c, (src, dst)) in x.data.chunks_exact(plane).zip(out.data.chunks_exact_mut(plane)).enumerate() {
            let alpha = *slope.get(if slope.len() == 1 { 0 } else { c % channels }).ok_or_else(|| NetError::Shape("short PRelu slope".into()))?;
            for (to, &v) in dst.iter_mut().zip(src) {
                *to = if v < 0.0 { v * alpha } else { v };
            }
        }
    } else {
        for (i, (o, &v)) in out.data.iter_mut().zip(&x.data).enumerate() {
            let alpha = *slope.get(broadcast_offset(i, &x.shape, &wt.dims)).ok_or_else(|| NetError::Shape("bad PRelu broadcast".into()))?;
            *o = if v < 0.0 { v * alpha } else { v };
        }
    }
    Ok(out)
}
pub(super) fn batch_norm(n: &Node, x: &Tensor, g: &Graph) -> Result<Tensor> {
    let channels = *x.shape.get(1).ok_or_else(|| NetError::Shape("BatchNormalization rank".into()))?;
    let (scale, bias, mean, var) = (shape::floats(n, g, 1)?, shape::floats(n, g, 2)?, shape::floats(n, g, 3)?, shape::floats(n, g, 4)?);
    let eps = shape::float(n, "epsilon", 1e-5);
    let plane = shape::elements(x.shape.get(2..).unwrap_or(&[]))?;
    let mut out = Tensor::zeros(x.shape.clone())?;
    for (j, (src, dst)) in x.data.chunks_exact(plane).zip(out.data.chunks_exact_mut(plane)).enumerate() {
        let c = j % channels;
        let coefficient = *scale.get(c).ok_or_else(|| NetError::Shape("short BN scale".into()))?
            / (var.get(c).copied().ok_or_else(|| NetError::Shape("short BN variance".into()))? + eps).sqrt();
        let offset = bias.get(c).copied().ok_or_else(|| NetError::Shape("short BN bias".into()))?
            - mean.get(c).copied().ok_or_else(|| NetError::Shape("short BN mean".into()))? * coefficient;
        for (to, &value) in dst.iter_mut().zip(src) {
            *to = value * coefficient + offset;
        }
    }
    Ok(out)
}
pub(super) fn gemm(n: &Node, a: &Tensor, g: &Graph) -> Result<Tensor> {
    let wt = shape::weight(n, g, 1)?;
    let b = shape::floats(n, g, 1)?;
    let [ar, ac] = a.shape.as_slice() else {
        return shape_err("Gemm A is not a matrix");
    };
    let [br, bc] = wt.dims.as_slice() else {
        return shape_err("Gemm B is not a matrix");
    };
    if ar.checked_mul(*ac) != Some(a.data.len()) || br.checked_mul(*bc) != Some(b.len()) {
        return shape_err("invalid matrix buffers");
    }
    let av = ArrayView2::from_shape((*ar, *ac), a.data.as_slice()).map_err(|e| NetError::Shape(e.to_string()))?;
    let bv = ArrayView2::from_shape((*br, *bc), b).map_err(|e| NetError::Shape(e.to_string()))?;
    let av = if n.int("transA").unwrap_or(0) != 0 { av.reversed_axes() } else { av };
    let bv = if n.int("transB").unwrap_or(0) != 0 { bv.reversed_axes() } else { bv };
    if av.ncols() != bv.nrows() {
        return shape_err("incompatible matrices");
    }
    let mut out = Tensor::zeros(vec![av.nrows(), bv.ncols()])?;
    let mut view = ArrayViewMut2::from_shape((av.nrows(), bv.ncols()), out.data.as_mut_slice()).map_err(|e| NetError::Shape(e.to_string()))?;
    general_mat_mul(shape::float(n, "alpha", 1.0), &av, &bv, 0.0, &mut view);
    if n.inputs.get(2).is_some_and(|s| !s.is_empty()) {
        let wt = shape::weight(n, g, 2)?;
        let bias = shape::floats(n, g, 2)?;
        let beta = shape::float(n, "beta", 1.0);
        for (i, to) in out.data.iter_mut().enumerate() {
            *to += beta * bias.get(broadcast_offset(i, &out.shape, &wt.dims)).copied().ok_or_else(|| NetError::Shape("short matrix bias".into()))?;
        }
    }
    Ok(out)
}
pub(super) fn flatten(n: &Node, x: &Tensor) -> Result<Tensor> {
    let axis = shape::axis(n, x.shape.len())?;
    Tensor::new(vec![shape::elements(x.shape.get(..axis).unwrap_or(&[]))?, shape::elements(x.shape.get(axis..).unwrap_or(&[]))?], x.data.clone())
}
pub(super) fn global_average(x: &Tensor) -> Result<Tensor> {
    let (c, h, w) = chw(x)?;
    Tensor::new(vec![1, c, 1, 1], x.data.chunks_exact(h * w).map(|v| v.iter().sum::<f32>() / (h * w) as f32).collect())
}
