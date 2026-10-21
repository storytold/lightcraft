//! A denoise network as plain data: the handful of layer kinds a U-Net denoiser is made of, with their weights.
//!
//! [`onnx`](crate::onnx) reads the file into this description, shared by the pure-Rust CPU and GPU runners.
//! Unsupported operators, groups, dilation or activations are reported as errors when loading the model.
//!
//! The description is checked ([`Net::new`]) because it comes from a stranger's file: channel counts must chain,
//! weights must have the sizes the shapes need and be finite, and the depth must stay small.

/// Most layers a network may have.
pub const MAX_OPS: usize = 512;
/// Most channels a layer may have.
pub const MAX_CHANNELS: usize = 4096;
/// Most weights (numbers) a network may have in all.
pub const MAX_WEIGHTS: usize = 128 * 1024 * 1024;

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum NetError {
    /// The model uses something this denoise network description cannot express.
    #[error("not supported by the denoise runners: {0}")]
    Unsupported(String),
    /// The model is not a well-formed network.
    #[error("not a valid network: {0}")]
    Invalid(String),
}

fn invalid<T>(why: impl Into<String>) -> Result<T, NetError> {
    Err(NetError::Invalid(why.into()))
}

/// A tensor is named by its number: 0 is the network's input, and layer `i` makes tensor `i + 1`.
pub type TensorId = usize;

/// A convolution with stride 1 and "same" padding (`k` is 1 or 3), over one tensor or two joined along the channels
/// (`src` first), then a bias and optionally a leaky ReLU.
#[derive(Clone, Debug, PartialEq)]
pub struct Conv {
    pub src: TensorId,
    /// A second source, whose channels follow `src`'s (a skip connection's concatenation).
    pub src2: Option<TensorId>,
    pub cin: usize,
    pub cout: usize,
    pub k: usize,
    /// `[cout][cin][k][k]`, as ONNX has it.
    pub weight: Vec<f32>,
    pub bias: Vec<f32>,
    /// `Some(alpha)`: a leaky ReLU with this slope follows.
    pub leaky: Option<f32>,
}

/// A transposed convolution with a 2 × 2 kernel and stride 2: every input pixel becomes a 2 × 2 block.
#[derive(Clone, Debug, PartialEq)]
pub struct ConvT2 {
    pub src: TensorId,
    pub cin: usize,
    pub cout: usize,
    /// `[cin][cout][2][2]`, as ONNX has it.
    pub weight: Vec<f32>,
    pub bias: Vec<f32>,
    pub leaky: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Conv(Conv),
    ConvT2(ConvT2),
    /// 2 × 2 maximum, stride 2.
    MaxPool2 {
        src: TensorId,
    },
    /// Depth-to-space with block size 2 in "CRD" order: output channel `c` at `(2y + dy, 2x + dx)` is input channel
    /// `4c + 2dy + dx` at `(y, x)`.
    DepthToSpace2 {
        src: TensorId,
    },
}

/// What a tensor looks like: its channels, and its resolution as a power of two below the input's (a negative
/// level is above it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub channels: usize,
    pub level: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Net {
    pub in_channels: usize,
    ops: Vec<Op>,
    shapes: Vec<Shape>,
}

impl Net {
    /// Check `ops` (layer `i` makes tensor `i + 1`) over an input of `in_channels` and describe every tensor.
    pub fn new(in_channels: usize, ops: Vec<Op>) -> Result<Net, NetError> {
        if in_channels == 0 || in_channels > MAX_CHANNELS {
            return invalid("the input has no channels or too many");
        }
        if ops.is_empty() || ops.len() > MAX_OPS {
            return invalid("the network has no layers or too many");
        }
        let mut shapes = vec![Shape { channels: in_channels, level: 0 }];
        let mut weights = 0usize;
        for (i, op) in ops.iter().enumerate() {
            let shape_of = |t: TensorId| {
                shapes
                    .get(t)
                    .copied()
                    .filter(|_| t <= i)
                    .ok_or_else(|| NetError::Invalid(format!("layer {i} reads tensor {t}, which does not exist yet")))
            };
            let out = match op {
                Op::Conv(c) => {
                    let a = shape_of(c.src)?;
                    let (cin, level) = match c.src2 {
                        Some(s2) => {
                            let b = shape_of(s2)?;
                            if b.level != a.level {
                                return invalid(format!("layer {i} joins tensors of different resolutions"));
                            }
                            (a.channels + b.channels, a.level)
                        }
                        None => (a.channels, a.level),
                    };
                    if c.cin != cin || c.cout == 0 || c.cout > MAX_CHANNELS || !(c.k == 1 || c.k == 3) {
                        return invalid(format!(
                            "layer {i}: a convolution of {} to {} channels with a {}-wide kernel does not fit",
                            c.cin, c.cout, c.k
                        ));
                    }
                    check_weights(
                        i,
                        &c.weight,
                        c.cout.checked_mul(cin).and_then(|n| n.checked_mul(c.k * c.k)),
                        &c.bias,
                        c.cout,
                        c.leaky,
                        &mut weights,
                    )?;
                    Shape { channels: c.cout, level }
                }
                Op::ConvT2(c) => {
                    let a = shape_of(c.src)?;
                    if c.cin != a.channels || c.cout == 0 || c.cout > MAX_CHANNELS {
                        return invalid(format!("layer {i}: a transposed convolution of {} to {} channels does not fit", c.cin, c.cout));
                    }
                    check_weights(i, &c.weight, c.cin.checked_mul(c.cout).and_then(|n| n.checked_mul(4)), &c.bias, c.cout, c.leaky, &mut weights)?;
                    Shape { channels: c.cout, level: a.level - 1 }
                }
                Op::MaxPool2 { src } => {
                    let a = shape_of(*src)?;
                    Shape { channels: a.channels, level: a.level + 1 }
                }
                Op::DepthToSpace2 { src } => {
                    let a = shape_of(*src)?;
                    if !a.channels.is_multiple_of(4) {
                        return invalid(format!("layer {i}: depth-to-space needs a multiple of 4 channels"));
                    }
                    Shape { channels: a.channels / 4, level: a.level - 1 }
                }
            };
            shapes.push(out);
        }
        Ok(Net { in_channels, ops, shapes })
    }

    /// The layers: layer `i` makes tensor `i + 1`.
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// The shape of tensor `t` (0 is the input).
    pub fn shape(&self, t: TensorId) -> Option<Shape> {
        self.shapes.get(t).copied()
    }

    /// The tensor the network gives (the last layer's).
    pub fn output(&self) -> TensorId {
        self.ops.len()
    }

    pub fn out_channels(&self) -> usize {
        self.shapes.last().map_or(0, |s| s.channels)
    }

    /// The deepest resolution level: a tile must be a multiple of `2^this` cells wide.
    pub fn depth(&self) -> u32 {
        self.shapes.iter().map(|s| s.level.max(0) as u32).max().unwrap_or(0)
    }

    /// Can the network run on a `tile × tile` input?
    /// For Bayer-to-RGB (`in_channels == 4`), the output must be one level above the input (`level == -1`, giving `2·tile × 2·tile`).
    /// For Linear-RGB (`in_channels == 3`), the output is at the input's level (`level == 0`, giving `tile × tile`).
    /// Pooling must divide the tile.
    pub fn fits_tile(&self, tile: usize) -> bool {
        let Some(multiple) = 1usize.checked_shl(self.depth()) else { return false };
        let wanted_level = match self.in_channels {
            4 => -1,
            3 => 0,
            _ => return false,
        };
        tile > 0 && tile.is_multiple_of(multiple) && self.shapes.last().is_some_and(|s| s.level == wanted_level)
    }

    /// Multiply-adds for one tile, for estimating time.
    pub fn macs(&self, tile: usize) -> u64 {
        let pixels = |level: i32| -> u64 {
            let side = if level >= 0 {
                tile.checked_shr(level as u32).unwrap_or(0)
            } else {
                tile.saturating_mul(1usize.checked_shl(level.unsigned_abs()).unwrap_or(usize::MAX))
            };
            (side as u64).saturating_mul(side as u64)
        };
        self.ops
            .iter()
            .enumerate()
            .map(|(i, op)| {
                let out = self.shapes.get(i + 1).map_or(0, |s| pixels(s.level));
                match op {
                    Op::Conv(c) => out.saturating_mul((c.cin * c.cout * c.k * c.k) as u64),
                    // every input pixel makes a 2 × 2 block: as many multiply-adds as the 2 × 2-wide kernel has weights
                    Op::ConvT2(c) => pixels(self.shapes.get(c.src).map_or(0, |s| s.level)).saturating_mul((c.cin * c.cout * 4) as u64),
                    _ => 0,
                }
            })
            .fold(0u64, u64::saturating_add)
    }
}

fn check_weights(
    layer: usize,
    weight: &[f32],
    want: Option<usize>,
    bias: &[f32],
    cout: usize,
    leaky: Option<f32>,
    total: &mut usize,
) -> Result<(), NetError> {
    if want != Some(weight.len()) || bias.len() != cout {
        return invalid(format!("layer {layer}: the weights are not the size its shape needs"));
    }
    *total = total.saturating_add(weight.len()).saturating_add(bias.len());
    if *total > MAX_WEIGHTS {
        return invalid("the network has too many weights");
    }
    if weight.iter().chain(bias).any(|v| !v.is_finite()) || leaky.is_some_and(|a| !a.is_finite()) {
        return invalid(format!("layer {layer}: a weight is not a finite number"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(src: TensorId, src2: Option<TensorId>, cin: usize, cout: usize, k: usize) -> Op {
        Op::Conv(Conv { src, src2, cin, cout, k, weight: vec![0.1; cout * cin * k * k], bias: vec![0.0; cout], leaky: Some(0.2) })
    }

    fn convt(src: TensorId, cin: usize, cout: usize) -> Op {
        Op::ConvT2(ConvT2 { src, cin, cout, weight: vec![0.1; cin * cout * 4], bias: vec![0.0; cout], leaky: None })
    }

    /// A two-level network of the shape the denoisers have: 4 → 8 channels, pool, 16 channels, up, join, 12, and the
    /// 1 × 1 convolution to the colours.
    fn unet() -> Vec<Op> {
        vec![
            conv(0, None, 4, 8, 3),       // 1: 8 ch, level 0
            Op::MaxPool2 { src: 1 },      // 2: level 1
            conv(2, None, 8, 16, 3),      // 3: 16 ch, level 1
            convt(3, 16, 8),              // 4: 8 ch, level 0
            conv(4, Some(1), 16, 8, 3),   // 5: join [up, skip]
            conv(5, None, 8, 12, 1),      // 6: 12 ch
            Op::DepthToSpace2 { src: 6 }, // 7: 3 ch, level -1
        ]
    }

    #[test]
    fn deep_networks_and_huge_cost_queries_never_overflow_or_truncate_depth() {
        let n = Net::new(4, unet()).unwrap();
        assert_eq!(n.macs(usize::MAX), u64::MAX);
        assert_eq!(n.macs(0), 0);
        let mut ops = Vec::new();
        for i in 0..21 {
            ops.push(Op::MaxPool2 { src: i });
        }
        for i in 21..42 {
            ops.push(convt(i, 4, 4));
        }
        ops.push(conv(42, None, 4, 12, 1));
        ops.push(Op::DepthToSpace2 { src: 43 });
        let n = Net::new(4, ops).unwrap();
        assert!(!n.fits_tile(1 << 20));
        assert!(n.fits_tile(1 << 21));
        let _ = n.macs(16);
    }

    #[test]
    fn a_u_net_is_described_with_its_shapes_and_costs() {
        let net = Net::new(4, unet()).unwrap();
        assert_eq!(net.out_channels(), 3);
        assert_eq!(net.shape(3), Some(Shape { channels: 16, level: 1 }));
        assert_eq!(net.shape(7), Some(Shape { channels: 3, level: -1 }));
        assert_eq!(net.depth(), 1);
        assert!(net.fits_tile(64) && !net.fits_tile(63) && !net.fits_tile(0));
        // 64 × 64 cells: layer 1 = 4096·4·8·9, and so on; just check it is the sum of what the layers cost
        let t = 64u64;
        let want = t * t * 4 * 8 * 9 + (t / 2) * (t / 2) * 8 * 16 * 9 + (t / 2) * (t / 2) * 16 * 8 * 4 + t * t * 16 * 8 * 9 + t * t * 8 * 12;
        assert_eq!(net.macs(64), want);
    }

    #[test]
    fn a_network_that_does_not_chain_is_refused() {
        // wrong input channel count for the first convolution
        assert!(Net::new(3, unet()).is_err());
        // a layer reading a tensor that does not exist yet
        let mut ops = unet();
        ops[2] = conv(9, None, 8, 16, 3);
        assert!(Net::new(4, ops).is_err());
        // joining tensors of different resolutions
        let mut ops = unet();
        ops[4] = conv(3, Some(1), 24, 8, 3);
        assert!(Net::new(4, ops).is_err());
        // weights of the wrong size, and not finite
        let mut ops = unet();
        if let Op::Conv(c) = &mut ops[0] {
            c.weight.pop();
        }
        assert!(Net::new(4, ops).is_err());
        let mut ops = unet();
        if let Op::Conv(c) = &mut ops[0] {
            c.bias[0] = f32::NAN;
        }
        assert!(Net::new(4, ops).is_err());
        // a kernel that is neither 1 nor 3 wide, no layers, depth-to-space on a channel count it cannot split
        let mut ops = unet();
        if let Op::Conv(c) = &mut ops[0] {
            c.k = 5;
        }
        assert!(Net::new(4, ops).is_err());
        assert!(Net::new(4, vec![]).is_err());
        assert!(Net::new(4, vec![Op::DepthToSpace2 { src: 0 }]).is_ok());
        assert!(Net::new(4, vec![Op::DepthToSpace2 { src: 0 }, Op::DepthToSpace2 { src: 1 }]).is_err());
        assert!(Net::new(2, vec![Op::DepthToSpace2 { src: 0 }]).is_err());
    }

    #[test]
    fn a_network_that_does_not_end_one_level_up_does_not_fit_a_tile() {
        let net = Net::new(4, unet()[..6].to_vec()).unwrap();
        assert!(!net.fits_tile(64), "no depth-to-space: the output is not at twice the resolution");
    }
}
