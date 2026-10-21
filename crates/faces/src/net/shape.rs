//! Bounded, allocation-free (apart from small shape lists) graph checking.
use super::*;
use crate::graph::{Attr, Weight};

pub(super) fn elements(s: &[usize]) -> Result<usize> {
    if s.len() > 8 || s.contains(&0) {
        return shape_err("a tensor has a zero dimension or excessive rank");
    }
    s.iter()
        .try_fold(1usize, |n, &d| n.checked_mul(d))
        .filter(|&n| n <= MAX_TENSOR)
        .ok_or_else(|| NetError::Shape("a tensor exceeds the element limit".into()))
}
pub(super) fn tensor(s: &[usize], n: usize, data: &[f32]) -> Result<()> {
    if elements(s)? != n || data.iter().any(|v| !v.is_finite()) {
        return shape_err("invalid tensor size or non-finite values");
    }
    Ok(())
}
pub(super) fn float(n: &Node, name: &str, default: f32) -> f32 {
    match n.attrs.get(name) {
        Some(Attr::Float(v)) => *v,
        _ => default,
    }
}
pub(super) fn weight<'a>(n: &Node, g: &'a Graph, i: usize) -> Result<&'a Weight> {
    n.inputs.get(i).and_then(|s| g.weights.get(s)).ok_or_else(|| NetError::Unsupported(format!("{} needs constant input {i}", n.op)))
}
pub(super) fn floats<'a>(n: &Node, g: &'a Graph, i: usize) -> Result<&'a [f32]> {
    match &weight(n, g, i)?.data {
        Data::F32(v) => Ok(v),
        _ => unsupported(format!("{} needs float32 weights", n.op)),
    }
}
pub(super) fn check_graph(g: &Graph) -> Result<()> {
    if g.inputs.len() != 1 || g.outputs.is_empty() || g.outputs.len() > 64 || g.nodes.len() > 20_000 || g.weights.len() > 20_000 {
        return unsupported("one input and at most 64 outputs are required");
    }
    let mut known = std::collections::HashSet::new();
    let mut total_weights = 0usize;
    for (name, w) in &g.weights {
        let count = w.dims.iter().try_fold(1usize, |n, &d| n.checked_mul(d));
        let len = match &w.data {
            Data::F32(v) => {
                if v.iter().any(|v| !v.is_finite()) {
                    return shape_err("non-finite weights");
                }
                v.len()
            }
            Data::I64(v) => v.len(),
        };
        if name.is_empty()
            || name.len() > 4096
            || w.dims.len() > 8
            || w.dims.iter().any(|&d| d > MAX_TENSOR)
            || count != Some(len)
            || len > MAX_TENSOR
        {
            return shape_err("invalid weight name, shape or size");
        }
        total_weights = total_weights
            .checked_add(len)
            .filter(|&v| v <= MAX_TENSOR)
            .ok_or_else(|| NetError::Shape("combined weights exceed 64 Mi elements".into()))?;
        known.insert(name.as_str());
    }
    let input = g.inputs.first().ok_or_else(|| NetError::Shape("missing input".into()))?;
    if input.0.is_empty() || !known.insert(&input.0) {
        return shape_err("invalid or repeated input name");
    }
    for n in &g.nodes {
        if n.inputs.len() > 64 || n.outputs.len() != 1 || n.outputs.first().is_none_or(|s| s.is_empty() || s.len() > 4096) {
            return unsupported(format!("{} requires one named output", n.op));
        }
        if n.inputs.iter().any(|s| !s.is_empty() && !known.contains(s.as_str())) {
            return shape_err(format!("{} reads an unknown or forward value", n.op));
        }
        if !known.insert(n.outputs.first().ok_or_else(|| NetError::Shape("missing output".into()))?) {
            return shape_err("a graph value is defined twice");
        }
    }
    if g.outputs.iter().any(|s| g.weights.contains_key(s)) {
        return unsupported("constant graph outputs");
    }
    let mut output_names = std::collections::HashSet::new();
    if g.outputs.iter().any(|s| !known.contains(s.as_str()) || !output_names.insert(s)) {
        return shape_err("unknown or repeated graph output");
    }
    Ok(())
}
pub(super) fn check(n: &Node, g: &Graph) -> Result<()> {
    let (min, max, attrs): (usize, usize, &[(&str, u8)]) = match n.op.as_str() {
        "Conv" => (2, 3, &[("group", 0), ("kernel_shape", 2), ("strides", 2), ("pads", 2), ("dilations", 2), ("auto_pad", 3)]),
        "BatchNormalization" => (5, 5, &[("epsilon", 1), ("momentum", 1), ("training_mode", 0)]),
        "PRelu" | "Add" | "Sub" | "Mul" => (2, 2, &[]),
        "Relu" | "Sigmoid" | "GlobalAveragePool" | "Identity" => (1, 1, &[]),
        "Dropout" => (1, 1, &[("ratio", 1), ("seed", 0)]),
        "Flatten" => (1, 1, &[("axis", 0)]),
        "Gemm" => (2, 3, &[("alpha", 1), ("beta", 1), ("transA", 0), ("transB", 0)]),
        "MaxPool" => {
            (1, 1, &[("kernel_shape", 2), ("strides", 2), ("pads", 2), ("dilations", 2), ("ceil_mode", 0), ("auto_pad", 3), ("storage_order", 0)])
        }
        "Resize" => (
            3,
            4,
            &[
                ("mode", 3),
                ("coordinate_transformation_mode", 3),
                ("nearest_mode", 3),
                ("cubic_coeff_a", 1),
                ("exclude_outside", 0),
                ("extrapolation_value", 1),
            ],
        ),
        "Transpose" => (1, 1, &[("perm", 2)]),
        "Reshape" => (2, 2, &[("allowzero", 0)]),
        op => return unsupported(format!("the operator `{op}`")),
    };
    if !(min..=max).contains(&n.inputs.len()) || n.inputs.first().is_none_or(String::is_empty) {
        return shape_err(format!("{} has invalid inputs", n.op));
    }
    for (name, value) in &n.attrs {
        let actual = match value {
            Attr::Int(_) => 0,
            Attr::Float(v) if v.is_finite() => 1,
            Attr::Ints(_) => 2,
            Attr::Str(_) => 3,
            _ => 255,
        };
        if attrs.iter().find(|(key, _)| *key == name).is_none_or(|(_, kind)| *kind != actual) {
            return unsupported(format!("{} attribute `{name}` (or its type)", n.op));
        }
    }
    for (i, name) in n.inputs.iter().enumerate() {
        if let Some(w) = g.weights.get(name)
            && matches!(w.data, Data::I64(_))
            && !(n.op == "Reshape" && i == 1)
        {
            return unsupported(format!("{} needs float32 input {i}", n.op));
        }
    }
    match n.op.as_str() {
        "Conv" => {
            let w = weight(n, g, 1)?;
            floats(n, g, 1)?;
            if w.dims.len() != 4 || w.dims.contains(&0) {
                return shape_err("Conv needs nonempty 2-D weights");
            }
            if n.ints("kernel_shape").is_some_and(|v| {
                v.iter().map(|&d| usize::try_from(d).ok()).collect::<Option<Vec<_>>>().as_deref() != Some(w.dims.get(2..).unwrap_or(&[]))
            }) {
                return shape_err("Conv kernel_shape differs from its weights");
            }
            if n.ints("dilations").is_some_and(|d| d != [1, 1]) || n.string("auto_pad").is_some_and(|s| s != "NOTSET") {
                return unsupported("Conv dilation or automatic padding");
            }
            let (sh, sw) = pair(n, "strides", 1)?;
            let (pt, pl, pb, pr) = pads(n)?;
            if [sh, sw, pt, pl, pb, pr].iter().any(|&v| v > 65536) {
                return shape_err("Conv stride or padding exceeds 65536");
            }
            if n.int("group").unwrap_or(1) < 1 {
                return shape_err("Conv group must be positive");
            }
            if n.inputs.get(2).is_some_and(|s| !s.is_empty()) {
                floats(n, g, 2)?;
            }
        }
        "BatchNormalization" => {
            if n.int("training_mode").unwrap_or(0) != 0 {
                return unsupported("training BatchNormalization");
            }
            if float(n, "epsilon", 1e-5) <= 0.0 {
                return shape_err("BatchNormalization epsilon must be positive");
            }
            for i in 1..5 {
                floats(n, g, i)?;
            }
        }
        "PRelu" => {
            floats(n, g, 1)?;
        }
        "Gemm" => {
            floats(n, g, 1)?;
            if n.inputs.get(2).is_some_and(|s| !s.is_empty()) {
                floats(n, g, 2)?;
            }
        }
        "Dropout" if !(0.0..1.0).contains(&float(n, "ratio", 0.5)) => return shape_err("Dropout ratio is invalid"),
        "MaxPool" => {
            pair(n, "kernel_shape", 1)?;
            pair(n, "strides", 1)?;
            if n.ints("kernel_shape").is_none()
                || n.int("ceil_mode").unwrap_or(0) != 0
                || n.int("storage_order").unwrap_or(0) != 0
                || n.ints("dilations").is_some_and(|v| v != [1, 1])
                || pads(n)? != (0, 0, 0, 0)
                || n.string("auto_pad").is_some_and(|s| s != "NOTSET")
            {
                return unsupported("this MaxPool padding, dilation or ceil mode");
            }
        }
        "Resize" => {
            floats(n, g, 2)?;
            if n.string("mode").unwrap_or("nearest") != "nearest" || n.inputs.get(3).is_some_and(|s| !s.is_empty()) {
                return unsupported("Resize requires nearest constant scales");
            }
            if n.int("exclude_outside").unwrap_or(0) != 0 || float(n, "extrapolation_value", 0.0) != 0.0 {
                return unsupported("Resize excludes or extrapolates outside values");
            }
            let coord = n.string("coordinate_transformation_mode").unwrap_or("half_pixel");
            let nearest = n.string("nearest_mode").unwrap_or("round_prefer_floor");
            if !matches!((coord, nearest), ("asymmetric", "floor") | ("half_pixel", "round_prefer_floor")) {
                return unsupported("this Resize coordinate/nearest mode");
            }
            if n.inputs.get(1).is_some_and(|s| !s.is_empty()) && !floats(n, g, 1)?.is_empty() {
                return unsupported("Resize with an ROI");
            }
        }
        "Reshape" => {
            if n.int("allowzero").unwrap_or(0) != 0 {
                return unsupported("Reshape allowzero");
            }
            if !matches!(weight(n, g, 1)?.data, Data::I64(_)) {
                return unsupported("Reshape needs a constant int64 shape");
            }
        }
        _ => {}
    }
    Ok(())
}
pub(super) fn broadcast(a: &[usize], b: &[usize]) -> Result<Vec<usize>> {
    elements(a)?;
    elements(b)?;
    let rank = a.len().max(b.len());
    let mut out = vec![1; rank];
    for (i, o) in out.iter_mut().enumerate() {
        let aa = i.checked_sub(rank - a.len()).and_then(|j| a.get(j)).copied().unwrap_or(1);
        let bb = i.checked_sub(rank - b.len()).and_then(|j| b.get(j)).copied().unwrap_or(1);
        if aa != bb && aa != 1 && bb != 1 {
            return shape_err("incompatible broadcast shapes");
        }
        *o = aa.max(bb);
    }
    elements(&out)?;
    Ok(out)
}
pub(super) fn axis(n: &Node, rank: usize) -> Result<usize> {
    let raw = n.int("axis").unwrap_or(1);
    let raw = if raw < 0 { raw.checked_add(rank as i64).ok_or_else(|| NetError::Shape("axis overflow".into()))? } else { raw };
    usize::try_from(raw).ok().filter(|&a| a <= rank).ok_or_else(|| NetError::Shape("Flatten axis is outside its rank".into()))
}
fn image(s: &[usize]) -> Result<(usize, usize, usize)> {
    match s {
        [1, c, h, w] => Ok((*c, *h, *w)),
        _ => unsupported("a spatial operator needs [1,C,H,W]"),
    }
}
pub(super) fn infer(g: &Graph, input: &[usize]) -> Result<Vec<Vec<usize>>> {
    Ok(infer_all(g, input)?.1)
}
/// Each node's output shape, and the graph's outputs' shapes.
pub(super) fn infer_all(g: &Graph, input: &[usize]) -> Result<(Vec<Vec<usize>>, Vec<Vec<usize>>)> {
    elements(input)?;
    let mut per_node = Vec::with_capacity(g.nodes.len());
    let mut shapes: HashMap<&str, Vec<usize>> = g.weights.iter().map(|(n, w)| (n.as_str(), w.dims.clone())).collect();
    shapes.insert(g.inputs.first().ok_or_else(|| NetError::Shape("no input".into()))?.0.as_str(), input.to_vec());
    let mut live = elements(input)?;
    // Scratch grows independently and remains allocated across all subsequent layers.
    let mut scratch_lower = 0;
    let mut scratch_product = 0;
    let mut last = HashMap::new();
    for (i, n) in g.nodes.iter().enumerate() {
        for s in &n.inputs {
            last.insert(s.as_str(), i);
        }
    }
    for s in &g.outputs {
        last.insert(s.as_str(), usize::MAX);
    }
    for (i, n) in g.nodes.iter().enumerate() {
        let a = n.inputs.first().and_then(|s| shapes.get(s.as_str())).ok_or_else(|| NetError::Shape("missing input shape".into()))?;
        elements(a)?;
        let out = match n.op.as_str() {
            "Conv" => {
                let (cin, h, w) = image(a)?;
                let dims = &weight(n, g, 1)?.dims;
                let [co, cg, kh, kw] = dims.as_slice() else {
                    return shape_err("Conv dimensions");
                };
                let groups = usize::try_from(n.int("group").unwrap_or(1)).map_err(|_| NetError::Shape("Conv group".into()))?;
                if groups.checked_mul(*cg) != Some(cin) || !co.is_multiple_of(groups) {
                    return shape_err("Conv channels do not match weights/groups");
                }
                if n.inputs.get(2).is_some_and(|s| !s.is_empty()) && weight(n, g, 2)?.dims != [*co] {
                    return shape_err("Conv bias is not a channel vector");
                }
                let (sh, sw) = pair(n, "strides", 1)?;
                let (pt, pl, pb, pr) = pads(n)?;
                let padded = |size: usize, p: usize, q: usize, k: usize, s: usize| {
                    size.checked_add(p)
                        .and_then(|v| v.checked_add(q))
                        .and_then(|v| v.checked_sub(k))
                        .and_then(|v| (v / s).checked_add(1))
                        .ok_or_else(|| NetError::Shape("Conv padding overflow or oversized kernel".into()))
                };
                vec![1, *co, padded(h, pt, pb, *kh, sh)?, padded(w, pl, pr, *kw, sw)?]
            }
            "BatchNormalization" => {
                let channels = *a.get(1).ok_or_else(|| NetError::Shape("BatchNormalization rank must be at least two".into()))?;
                for j in 1..5 {
                    if weight(n, g, j)?.dims != [channels] {
                        return shape_err("BatchNormalization parameters must be channel vectors");
                    }
                }
                if floats(n, g, 4)?.iter().any(|v| *v < 0.0) {
                    return shape_err("BatchNormalization has negative variance");
                }
                let eps = float(n, "epsilon", 1e-5);
                let (s, b, m, v) = (floats(n, g, 1)?, floats(n, g, 2)?, floats(n, g, 3)?, floats(n, g, 4)?);
                for (((&s, &b), &m), &v) in s.iter().zip(b).zip(m).zip(v) {
                    let variance = v + eps;
                    let coefficient = s / variance.sqrt();
                    let offset = b - m * coefficient;
                    if !variance.is_finite() || !coefficient.is_finite() || !offset.is_finite() {
                        return shape_err("BatchNormalization coefficients overflow");
                    }
                }

                a.clone()
            }
            "PRelu" => {
                if broadcast(a, &weight(n, g, 1)?.dims)? != *a {
                    return shape_err("PRelu slope would enlarge its input");
                }
                a.clone()
            }
            "Add" | "Sub" | "Mul" => {
                let b = n.inputs.get(1).and_then(|s| shapes.get(s.as_str())).ok_or_else(|| NetError::Shape("missing second input".into()))?;
                broadcast(a, b)?
            }
            "Gemm" => {
                let b = &weight(n, g, 1)?.dims;
                let [ar, ac] = a.as_slice() else {
                    return shape_err("Gemm needs matrix A");
                };
                let [br, bc] = b.as_slice() else {
                    return shape_err("Gemm needs matrix B");
                };
                let (m, k) = if n.int("transA").unwrap_or(0) != 0 { (*ac, *ar) } else { (*ar, *ac) };
                let (bk, cols) = if n.int("transB").unwrap_or(0) != 0 { (*bc, *br) } else { (*br, *bc) };
                if k != bk {
                    return shape_err("Gemm inner dimensions differ");
                }
                let s = vec![m, cols];
                if n.inputs.get(2).is_some_and(|s| !s.is_empty()) && broadcast(&s, &weight(n, g, 2)?.dims)? != s {
                    return shape_err("Gemm bias would enlarge the output");
                }
                s
            }
            "Flatten" => {
                let at = axis(n, a.len())?;
                vec![
                    elements(a.get(..at).ok_or_else(|| NetError::Shape("bad axis".into()))?)?,
                    elements(a.get(at..).ok_or_else(|| NetError::Shape("bad axis".into()))?)?,
                ]
            }
            "GlobalAveragePool" => {
                let (c, _, _) = image(a)?;
                vec![1, c, 1, 1]
            }
            "MaxPool" => {
                let (c, h, w) = image(a)?;
                let (kh, kw) = pair(n, "kernel_shape", 1)?;
                let (sh, sw) = pair(n, "strides", 1)?;
                if h < kh || w < kw {
                    return shape_err("MaxPool kernel exceeds input");
                }
                vec![1, c, (h - kh) / sh + 1, (w - kw) / sw + 1]
            }
            "Resize" => {
                let (c, h, w) = image(a)?;
                let s = floats(n, g, 2)?;
                if s.len() != 4
                    || s.first() != Some(&1.0)
                    || s.get(1) != Some(&1.0)
                    || s.iter().any(|&v| !(1.0..=16.0).contains(&v) || v.fract() != 0.0)
                {
                    return unsupported("Resize supports whole spatial scales from 1 to 16");
                }
                vec![
                    1,
                    c,
                    h.checked_mul(s.get(2).copied().unwrap_or(0.0) as usize).ok_or_else(|| NetError::Shape("Resize overflow".into()))?,
                    w.checked_mul(s.get(3).copied().unwrap_or(0.0) as usize).ok_or_else(|| NetError::Shape("Resize overflow".into()))?,
                ]
            }
            "Transpose" => {
                let perm: Vec<i64> = n.ints("perm").map_or_else(|| (0..a.len() as i64).rev().collect(), Vec::from);
                let mut seen = std::collections::HashSet::new();
                let mut s = Vec::new();
                if perm.len() != a.len() {
                    return shape_err("Transpose rank mismatch");
                }
                for d in perm {
                    let d = usize::try_from(d).map_err(|_| NetError::Shape("negative permutation".into()))?;
                    if !seen.insert(d) {
                        return shape_err("repeated permutation axis");
                    }
                    s.push(*a.get(d).ok_or_else(|| NetError::Shape("bad permutation axis".into()))?);
                }
                s
            }
            "Reshape" => {
                let Data::I64(dims) = &weight(n, g, 1)?.data else {
                    return shape_err("Reshape datatype");
                };
                if dims.len() > 8 {
                    return shape_err("Reshape rank exceeds eight");
                }
                let total = elements(a)?;
                let mut s = Vec::new();
                let mut infer = None;
                for (j, &v) in dims.iter().enumerate() {
                    s.push(match v {
                        -1 if infer.is_none() => {
                            infer = Some(j);
                            1
                        }
                        0 => *a.get(j).ok_or_else(|| NetError::Shape("Reshape copies missing dimension".into()))?,
                        v if v > 0 => usize::try_from(v).map_err(|_| NetError::Shape("Reshape dimension overflow".into()))?,
                        _ => return shape_err("invalid Reshape dimension"),
                    });
                }
                let known = elements(&s)?;
                if let Some(j) = infer {
                    if !total.is_multiple_of(known) {
                        return shape_err("Reshape does not divide evenly");
                    }
                    *s.get_mut(j).ok_or_else(|| NetError::Shape("missing inferred dimension".into()))? = total / known;
                }
                if elements(&s)? != total {
                    return shape_err("Reshape changes element count");
                }
                s
            }
            _ => a.clone(),
        };
        let out_count = elements(&out)?;
        let mut copied_constants = 0usize;
        for (j, name) in n.inputs.iter().enumerate() {
            // Execution borrows operator weights, but get(0) and binary get(1) materialize constants.
            if (j == 0 || (j == 1 && matches!(n.op.as_str(), "Add" | "Sub" | "Mul")))
                && let Some(w) = g.weights.get(name)
            {
                copied_constants =
                    copied_constants.checked_add(elements(&w.dims)?).ok_or_else(|| NetError::Shape("constant copies overflow".into()))?;
            }
        }
        if n.op == "Conv" {
            let wd = &weight(n, g, 1)?.dims;
            let [co, cg, kh, kw] = wd.as_slice() else {
                return shape_err("Conv rank");
            };
            let block = 256.min(out.get(2).copied().unwrap_or(1) * out.get(3).copied().unwrap_or(1));
            let lower = cg
                .checked_mul(*kh)
                .and_then(|v| v.checked_mul(*kw))
                .and_then(|v| v.checked_mul(block))
                .filter(|&v| v <= MAX_TENSOR)
                .ok_or_else(|| NetError::Shape("convolution scratch exceeds the limit".into()))?;
            scratch_lower = scratch_lower.max(lower);
            scratch_product = scratch_product.max(co * block);
        }
        let scratch = scratch_lower + scratch_product;
        if live
            .checked_add(out_count)
            .and_then(|v| v.checked_add(scratch))
            .and_then(|v| v.checked_add(copied_constants))
            .is_none_or(|v| v > 128 * 1024 * 1024)
        {
            return shape_err("tensors and scratch exceed the 512 MiB working limit");
        }
        live = live
            .checked_add(out_count)
            .filter(|&n| n <= 128 * 1024 * 1024)
            .ok_or_else(|| NetError::Shape("live tensors exceed the 512 MiB working limit".into()))?;
        per_node.push(out.clone());
        shapes.insert(n.outputs.first().ok_or_else(|| NetError::Shape("no output".into()))?, out);
        let mut removed = std::collections::HashSet::new();
        for name in &n.inputs {
            if last.get(name.as_str()) == Some(&i) && !g.weights.contains_key(name) && removed.insert(name) {
                live = live.saturating_sub(shapes.get(name.as_str()).map_or(0, |s| elements(s).unwrap_or(0)));
            }
        }
    }
    let outputs =
        g.outputs.iter().map(|s| shapes.get(s.as_str()).cloned().ok_or_else(|| NetError::Shape("missing output".into()))).collect::<Result<_>>()?;
    Ok((per_node, outputs))
}
