//! Reading an `.onnx` file into a [`Net`] with bounded, pure-Rust protobuf decoding.
//!
//! Only what the denoise U-Nets are made of is understood: `Conv` (3 × 3 "same" or 1 × 1, stride 1, no groups, no
//! dilation) optionally followed by `LeakyRelu`, `ConvTranspose` (2 × 2, stride 2) likewise, `MaxPool` (2 × 2, stride 2),
//! a two-input `Concat` on the channels that feeds a convolution, and a final `DepthToSpace` (2, `CRD`). Any other
//! operator or attribute is [`NetError::Unsupported`], which is reported to the user by both runners.

use std::collections::HashMap;
use std::path::Path;

use crate::onnx_proto::{self, AttributeProto, NodeProto, TensorProto, ValueInfoProto};
use std::io::Read;

use crate::net::{Conv, ConvT2, MAX_WEIGHTS, Net, NetError, Op, TensorId};

fn unsupported<T>(why: impl Into<String>) -> Result<T, NetError> {
    Err(NetError::Unsupported(why.into()))
}

fn invalid<T>(why: impl Into<String>) -> Result<T, NetError> {
    Err(NetError::Invalid(why.into()))
}

/// Read the model at `path`.
pub fn read(path: &Path) -> Result<Net, NetError> {
    let file = std::fs::File::open(path).map_err(|e| NetError::Invalid(e.to_string()))?;
    let size = usize::try_from(file.metadata().map_err(|e| NetError::Invalid(e.to_string()))?.len())
        .ok()
        .filter(|&n| n <= onnx_proto::MAX_BYTES)
        .ok_or_else(|| NetError::Invalid("model file too large".into()))?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(size).map_err(|_| NetError::Invalid("not enough memory to load the model".into()))?;
    file.take(onnx_proto::MAX_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|e| NetError::Invalid(e.to_string()))?;
    read_bytes(&bytes)
}

fn read_bytes(bytes: &[u8]) -> Result<Net, NetError> {
    let graph = onnx_proto::decode(bytes)?;

    let initialisers: HashMap<&str, &TensorProto> = graph.initializer.iter().map(|t| (t.name.as_str(), t)).collect();
    if initialisers.len() != graph.initializer.len() {
        return invalid("duplicate initializer names");
    }
    let inputs: Vec<_> = graph.input.iter().filter(|i| !initialisers.contains_key(i.name.as_str())).collect();
    let ([input], [output]) = (inputs.as_slice(), graph.output.as_slice()) else { return unsupported("not one input and one output") };
    let in_channels = match channel_dim(input) {
        Some(c) => c,
        None => return unsupported("the input's channel count is not written in the file"),
    };

    // how many nodes read each name (a graph output counts as a reader)
    let mut readers: HashMap<&str, usize> = HashMap::new();
    for n in &graph.node {
        for i in &n.input {
            *readers.entry(i.as_str()).or_default() += 1;
        }
    }
    *readers.entry(output.name.as_str()).or_default() += 1;

    let mut ids: HashMap<String, TensorId> = HashMap::new();
    ids.insert(input.name.clone(), 0);
    // a Concat is not a layer of its own: the convolution that reads it takes both sources
    let mut joins: HashMap<String, (TensorId, TensorId)> = HashMap::new();
    let mut ops: Vec<Op> = Vec::new();
    let mut weights_total = 0usize;

    let nodes = &graph.node;
    let mut i = 0;
    while i < nodes.len() {
        let Some(node) = nodes.get(i) else { break };
        i += 1;
        if node.output.len() != 1 || node.output.first().is_none_or(|n| n.is_empty() || ids.contains_key(n) || joins.contains_key(n)) {
            return invalid("a layer has missing, repeated or multiple outputs");
        }
        let allowed: &[&str] = match node.op_type.as_str() {
            "Conv" => &["kernel_shape", "pads", "strides", "dilations", "group", "auto_pad"],
            "ConvTranspose" => &["kernel_shape", "pads", "strides", "dilations", "group", "output_padding", "auto_pad"],
            "MaxPool" => &["kernel_shape", "pads", "strides", "dilations", "ceil_mode", "storage_order", "auto_pad"],
            "Concat" => &["axis"],
            "DepthToSpace" => &["blocksize", "mode"],
            _ => &[],
        };
        if node.attribute.iter().any(|a| !allowed.contains(&a.name.as_str())) {
            return unsupported("an unknown operator attribute");
        }
        let this = ops.len() + 1;
        let lookup = |name: &str| -> Result<TensorId, NetError> {
            ids.get(name).copied().ok_or_else(|| NetError::Invalid(format!("`{}` reads `{name}`, which nothing makes", node.op_type)))
        };
        // a LeakyRelu straight after this layer, which nothing else reads, is part of it
        let mut fuse_leaky = |out: &str| -> Result<(Option<f32>, String), NetError> {
            if let Some(next) = nodes.get(i)
                && next.op_type == "LeakyRelu"
                && next.input.first().map(String::as_str) == Some(out)
                && readers.get(out).copied() == Some(1)
            {
                if next.input.len() != 1 || next.output.len() != 1 || next.attribute.iter().any(|a| a.name != "alpha") {
                    return invalid("malformed LeakyRelu");
                }
                // only an omitted alpha gets the ONNX default; one that is not a single float must not read as 0
                let alpha = match next.attribute.as_slice() {
                    [] => 0.01,
                    [AttributeProto { f: Some(f), kind: 0 | 1, .. }] => *f,
                    _ => return invalid("LeakyRelu alpha is not a single float"),
                };
                i += 1;
                let name = next.output.first().cloned().ok_or_else(|| NetError::Invalid("LeakyRelu without an output".into()))?;
                return Ok((Some(alpha), name));
            }
            Ok((None, out.to_string()))
        };
        match node.op_type.as_str() {
            "Concat" => {
                if attr(node, "axis").map(|a| a.i) != Some(1) {
                    return unsupported("Concat on another axis than the channels");
                }
                let [a, b] = node.input.as_slice() else { return unsupported("Concat of other than two tensors") };
                let out = node.output.first().ok_or_else(|| NetError::Invalid("Concat without an output".into()))?;
                if readers.get(out.as_str()).copied() != Some(1) {
                    return unsupported("a Concat that is read by more than one layer");
                }
                joins.insert(out.clone(), (lookup(a)?, lookup(b)?));
            }
            "Conv" => {
                let (w, b) = weights_of(node, &initialisers, &mut weights_total)?;
                let (cout, cin, kh, kw) = four(&w.0)?;
                let b = if b.is_empty() { vec![0.0; cout] } else { b };
                if kh != kw || !(kh == 1 || kh == 3) {
                    return unsupported(format!("a {kh} × {kw} convolution"));
                }
                if ints_or(node, "strides", &[1, 1]) != [1, 1]
                    || ints_or(node, "dilations", &[1, 1]) != [1, 1]
                    || attr(node, "group").is_some_and(|a| a.i != 1)
                {
                    return unsupported("a convolution with strides, dilation or groups");
                }
                if attr(node, "kernel_shape").is_some_and(|a| a.ints != [kh as i64, kw as i64]) {
                    return invalid("kernel shape does not match the weights");
                }
                let pad = (kh as i64 - 1) / 2;
                if ints_or(node, "pads", &[0; 4]) != [pad; 4] || attr(node, "auto_pad").is_some_and(|a| !a.s.is_empty() && a.s != b"NOTSET") {
                    return unsupported("a convolution that is not padded to keep its size");
                }
                let src_name = node.input.first().ok_or_else(|| NetError::Invalid("Conv without an input".into()))?;
                let (src, src2) = match joins.remove(src_name) {
                    Some((a, b)) => (a, Some(b)),
                    None => (lookup(src_name)?, None),
                };
                let out = node.output.first().ok_or_else(|| NetError::Invalid("Conv without an output".into()))?;
                let (leaky, out_name) = fuse_leaky(out)?;
                ids.insert(out_name, this);
                ops.push(Op::Conv(Conv { src, src2, cin, cout, k: kh, weight: w.1, bias: b, leaky }));
            }
            "ConvTranspose" => {
                let (w, b) = weights_of(node, &initialisers, &mut weights_total)?;
                let (cin, cout, kh, kw) = four(&w.0)?;
                let b = if b.is_empty() { vec![0.0; cout] } else { b };
                if (kh, kw) != (2, 2)
                    || ints_or(node, "strides", &[1, 1]) != [2, 2]
                    || ints_or(node, "pads", &[0; 4]) != [0; 4]
                    || ints_or(node, "dilations", &[1, 1]) != [1, 1]
                    || attr(node, "group").is_some_and(|a| a.i != 1)
                    || attr(node, "output_padding").is_some_and(|a| a.ints.iter().any(|&v| v != 0))
                {
                    return unsupported("a transposed convolution other than 2 × 2 with stride 2");
                }
                if attr(node, "auto_pad").is_some_and(|a| !a.s.is_empty() && a.s != b"NOTSET") {
                    return unsupported("transposed convolution auto padding");
                }
                if attr(node, "kernel_shape").is_some_and(|a| a.ints != [2, 2]) {
                    return invalid("transposed kernel shape does not match the weights");
                }
                let src = lookup(node.input.first().map_or("", String::as_str))?;
                let out = node.output.first().ok_or_else(|| NetError::Invalid("ConvTranspose without an output".into()))?;
                let (leaky, out_name) = fuse_leaky(out)?;
                ids.insert(out_name, this);
                ops.push(Op::ConvT2(ConvT2 { src, cin, cout, weight: w.1, bias: b, leaky }));
            }
            "MaxPool" => {
                if ints_or(node, "kernel_shape", &[]) != [2, 2]
                    || ints_or(node, "strides", &[1, 1]) != [2, 2]
                    || ints_or(node, "pads", &[0; 4]) != [0; 4]
                    || ints_or(node, "dilations", &[1, 1]) != [1, 1]
                    || attr(node, "ceil_mode").is_some_and(|a| a.i != 0)
                    || attr(node, "storage_order").is_some_and(|a| a.i != 0)
                    || attr(node, "auto_pad").is_some_and(|a| !a.s.is_empty() && a.s != b"NOTSET")
                {
                    return unsupported("a max-pool other than 2 × 2 with stride 2");
                }
                let src = lookup(node.input.first().map_or("", String::as_str))?;
                let out = node.output.first().ok_or_else(|| NetError::Invalid("MaxPool without an output".into()))?;
                ids.insert(out.clone(), this);
                ops.push(Op::MaxPool2 { src });
            }
            "DepthToSpace" => {
                if attr(node, "blocksize").map(|a| a.i) != Some(2) || attr(node, "mode").map(|a| a.s.as_slice()) != Some(&b"CRD"[..]) {
                    return unsupported("depth-to-space other than block size 2 in CRD order");
                }
                let src = lookup(node.input.first().map_or("", String::as_str))?;
                let out = node.output.first().ok_or_else(|| NetError::Invalid("DepthToSpace without an output".into()))?;
                ids.insert(out.clone(), this);
                ops.push(Op::DepthToSpace2 { src });
            }
            other => return unsupported(format!("the `{other}` operator")),
        }
    }
    if !joins.is_empty() {
        return unsupported("a Concat whose result no convolution reads");
    }
    if ids.get(output.name.as_str()) != Some(&ops.len()) {
        return unsupported("the output is not the last layer");
    }
    Net::new(in_channels, ops)
}

fn attr<'a>(n: &'a NodeProto, name: &str) -> Option<&'a AttributeProto> {
    n.attribute.iter().find(|a| a.name == name)
}

/// The integer list attribute `name`, or `default` when the node does not have it.
fn ints_or(n: &NodeProto, name: &str, default: &[i64]) -> Vec<i64> {
    attr(n, name).map_or_else(|| default.to_vec(), |a| a.ints.clone())
}

fn four(dims: &[i64]) -> Result<(usize, usize, usize, usize), NetError> {
    match dims {
        [a, b, c, d] if dims.iter().all(|&v| v > 0 && v <= 1 << 20) => Ok((*a as usize, *b as usize, *c as usize, *d as usize)),
        _ => invalid("a weight tensor that is not four-dimensional"),
    }
}

type Weights = ((Vec<i64>, Vec<f32>), Vec<f32>);

/// The weight and bias initialisers of a convolution node.
fn weights_of(node: &NodeProto, initialisers: &HashMap<&str, &TensorProto>, total: &mut usize) -> Result<Weights, NetError> {
    let mut get = |idx: usize| -> Result<(Vec<i64>, Vec<f32>), NetError> {
        let name = node.input.get(idx).ok_or_else(|| NetError::Invalid(format!("`{}` without its weights", node.op_type)))?;
        let t = initialisers.get(name.as_str()).ok_or_else(|| NetError::Unsupported(format!("weights that are computed (`{name}`)")))?;
        let values = floats(t)?;
        *total = total.saturating_add(values.len());
        if *total > MAX_WEIGHTS {
            return invalid("the network has too many weights");
        }
        Ok((t.dims.clone(), values))
    };
    let w = get(1)?;
    let b = if node.input.len() > 2 {
        let (dims, values) = get(2)?;
        if dims.len() != 1 || values.is_empty() {
            return invalid("a bias tensor must be a nonempty vector");
        }
        values
    } else {
        Vec::new()
    };
    Ok((w, b))
}

fn floats(t: &TensorProto) -> Result<Vec<f32>, NetError> {
    if t.data_type != 1 {
        return unsupported("weights that are not 32-bit floats");
    }
    let want: usize = t
        .dims
        .iter()
        .try_fold(1usize, |n, &d| usize::try_from(d).ok().and_then(|d| n.checked_mul(d)))
        .ok_or_else(|| NetError::Invalid("a tensor with a negative or huge size".into()))?;
    if want > MAX_WEIGHTS {
        return invalid("a tensor with too many numbers");
    }
    if !t.raw_data.is_empty() && (!t.float_data.is_empty() || !t.raw_data.len().is_multiple_of(4)) {
        return invalid("ambiguous or truncated tensor data");
    }
    let actual = if t.raw_data.is_empty() { t.float_data.len() } else { t.raw_data.len() / 4 };
    if actual != want {
        return invalid("tensor data does not match its dimensions");
    }
    let mut values = Vec::new();
    values.try_reserve_exact(want).map_err(|_| NetError::Invalid("not enough memory for weights".into()))?;
    if !t.raw_data.is_empty() {
        values.extend(t.raw_data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)));
    } else {
        values.extend_from_slice(&t.float_data);
    }
    if values.len() != want {
        return invalid(format!("tensor `{}` has {} numbers, not the {want} its shape says", t.name, values.len()));
    }
    Ok(values)
}

/// The channel count in an input's declared shape `[batch, channels, …]`.
fn channel_dim(v: &ValueInfoProto) -> Option<usize> {
    v.channels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::{self, unet_onnx};

    fn temp_model(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("lc-denoise-onnx-{name}-{}.onnx", std::process::id()));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn a_u_net_is_read_with_its_layers_fused() {
        let (tile, depth) = (32usize, 2usize);
        let p = temp_model("unet", &unet_onnx(tile as u64, 8, depth, 7));
        let net = read(&p).unwrap();
        // per level two convolutions; a pool per level but the last; going up a transposed convolution and two convolutions
        // per level (the join is part of the first); then the 1 × 1 convolution and depth-to-space
        assert_eq!(net.ops().len(), 2 * (depth + 1) + depth + depth * 3 + 2);
        assert!(matches!(net.ops().last(), Some(crate::net::Op::DepthToSpace2 { .. })));
        assert!(net.ops().iter().any(|o| matches!(o, crate::net::Op::Conv(c) if c.src2.is_some())), "the joins became two-source convolutions");
        assert!(
            net.ops().iter().all(|o| !matches!(o, crate::net::Op::Conv(c) if c.leaky.is_none() && c.k == 3)),
            "every 3 × 3 convolution has its activation"
        );
        assert_eq!((net.in_channels, net.out_channels(), net.depth()), (4, 3, 2));
        assert!(net.fits_tile(tile) && !net.fits_tile(30));

        let _ = std::fs::remove_file(p);
    }

    /// Verifies that a linear RGB U-Net with 3-channel input and output is parsed and passes shape checks.
    #[test]
    fn a_linear_rgb_u_net_is_read_and_fits_tile() {
        let tile = 64usize;
        let p = temp_model("unet-lin-64", &synthetic::unet_linear_rgb_onnx(tile as u64, 8, 2, 99));
        let net = read(&p).unwrap();
        assert!(matches!(net.ops().last(), Some(crate::net::Op::Conv(c)) if c.cout == 3 && c.k == 1));
        assert_eq!((net.in_channels, net.out_channels(), net.depth()), (3, 3, 2));
        assert!(net.fits_tile(tile) && !net.fits_tile(30));

        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn a_leaky_relu_alpha_that_is_not_a_float_is_rejected() {
        let good = unet_onnx(4, 3, 1, 7);
        assert!(read_bytes(&good).is_ok());
        // an alpha attribute: float field 2, then the declared type (field 20) FLOAT
        let mut alpha = vec![0x15];
        alpha.extend_from_slice(&0.2f32.to_le_bytes());
        let at = good.windows(alpha.len()).position(|w| w == alpha.as_slice()).unwrap() + alpha.len();
        assert_eq!(good[at..at + 3], [0xa0, 0x01, 1]);
        // declared INT: the float must not be taken as the slope
        let mut wrong_type = good.clone();
        wrong_type[at + 2] = 2;
        assert!(matches!(read_bytes(&wrong_type), Err(NetError::Invalid(_))));
        // declared FLOAT but no float value (the float field becomes an int field 3 = 0 of the same length): must not read as 0
        let mut no_float = good.clone();
        no_float[at - 5..at].copy_from_slice(&[0x18, 0x80, 0x80, 0x80, 0x00]);
        assert!(matches!(read_bytes(&no_float), Err(NetError::Invalid(_))));
    }

    #[test]
    fn an_explicit_empty_or_nonvector_bias_is_not_treated_as_missing() {
        let w = TensorProto { name: "w".into(), dims: vec![12, 4, 1, 1], data_type: 1, float_data: vec![0.1; 48], ..Default::default() };
        let n = NodeProto { input: vec!["data".into(), "w".into(), "b".into()], ..Default::default() };
        for dims in [vec![0], vec![3, 4]] {
            let b = TensorProto {
                name: "b".into(),
                float_data: vec![0.0; dims.iter().product::<i64>() as usize],
                dims,
                data_type: 1,
                ..Default::default()
            };
            let inits = HashMap::from([("w", &w), ("b", &b)]);
            assert!(weights_of(&n, &inits, &mut 0).is_err());
        }
    }

    #[test]
    fn truncated_and_mutated_protobuf_is_bounded_and_never_panics() {
        let good = unet_onnx(4, 3, 1, 7);
        for n in (0..good.len()).step_by(31) {
            let _ = read_bytes(&good[..n]);
        }
        for at in (0..good.len()).step_by(37) {
            let mut bad = good.clone();
            bad[at] ^= 0xff;
            let _ = read_bytes(&bad);
        }
        assert!(read_bytes(&[0x3a, 0xff, 0xff, 0xff, 0xff, 0x7f]).is_err());
        assert!(read_bytes(&[0x3a, 0, 0x3a, 0]).is_err());
    }
    proptest::proptest! {
        #[test]
        fn arbitrary_model_bytes_never_panic(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096)) {
            let _ = read_bytes(&bytes);
        }
    }

    #[test]
    fn what_the_description_cannot_say_is_unsupported_and_garbage_is_an_error() {
        // not a model
        let p = temp_model("junk", b"this is not an onnx file");
        assert!(matches!(read(&p), Err(NetError::Invalid(_)) | Err(NetError::Unsupported(_))));
        let _ = std::fs::remove_file(p);
        // a missing file
        assert!(read(Path::new("/does/not/exist.onnx")).is_err());
    }
}
