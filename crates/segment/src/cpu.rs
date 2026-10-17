//! Fused CPU kernels for the model's elementwise work.
//!
//! On the CPU, candle runs bias adds, activations, broadcasts and layout copies one element at a
//! time on one thread. The kernels do the same arithmetic in the same order (bit-identical
//! results), fused into fewer passes over memory and spread over the rayon pool that candle's
//! matrix products use.

use std::f32::consts::FRAC_1_SQRT_2;

use candle_core::cpu::erf::erf_f32;
use candle_core::{CpuStorage, CustomOp2, D, Layout, Shape, Tensor, bail};
use candle_nn::Linear;
use rayon::prelude::*;

use crate::{Error, Result};

/// The contiguous `f32` values of a CPU tensor.
fn values<'a>(s: &'a CpuStorage, l: &Layout) -> candle_core::Result<&'a [f32]> {
    match (s, l.contiguous_offsets()) {
        (CpuStorage::F32(v), Some((start, end))) => match v.get(start..end) {
            Some(v) => Ok(v),
            None => bail!("layout outside its storage"),
        },
        _ => bail!("fused kernels take contiguous f32 tensors"),
    }
}

/// `x · Wᵀ` over the rows of `x` (`[…, K]`) as `[rows, N]`: `Linear::forward` without the bias.
fn product(x: &Tensor, l: &Linear) -> Result<Tensor> {
    let k = x.dim(D::Minus1)?;
    Ok(x.reshape(((), k))?.matmul(&l.weight().t()?)?)
}

fn bias(l: &Linear) -> Result<&Tensor> {
    l.bias().ok_or_else(|| Error::Model("fused linear without a bias".into()))
}

/// `gelu_erf(x · Wᵀ + b)` (`gelu`) or `relu(x · Wᵀ + b)`: `[rows, N]`.
pub fn linear_act(x: &Tensor, l: &Linear, gelu: bool) -> Result<Tensor> {
    Ok(product(x, l)?.apply_op2_no_bwd(bias(l)?, &BiasAct { gelu })?)
}

/// The detector's box relative-position bias `[1, heads, 1 + queries, side²]` from its row and
/// column terms `ey`, `ex` (`[queries, side, heads]`): `ey[q, y, h] + ex[q, x, h]` at `y·side + x`,
/// after a row of zeros (the presence token).
pub fn box_bias(ey: &Tensor, ex: &Tensor) -> Result<Tensor> {
    Ok(ey.contiguous()?.apply_op2_no_bwd(&ex.contiguous()?, &BoxBias)?)
}

/// Rows of `x` (`[rows, N]`) with a bias `[N]`.
fn rows(x: &[f32], b: &[f32]) -> candle_core::Result<usize> {
    if b.is_empty() || !x.len().is_multiple_of(b.len()) {
        bail!("bias of {} for {} values", b.len(), x.len());
    }
    Ok(b.len())
}

struct BiasAct {
    gelu: bool,
}

impl CustomOp2 for BiasAct {
    fn name(&self) -> &'static str {
        if self.gelu { "bias-gelu-erf" } else { "bias-relu" }
    }

    fn cpu_fwd(&self, s1: &CpuStorage, l1: &Layout, s2: &CpuStorage, l2: &Layout) -> candle_core::Result<(CpuStorage, Shape)> {
        let (x, b) = (values(s1, l1)?, values(s2, l2)?);
        let n = rows(x, b)?;
        let mut out = vec![0f32; x.len()];
        out.par_chunks_mut(n).zip(x.par_chunks(n)).for_each(|(o, x)| {
            for ((o, x), b) in o.iter_mut().zip(x).zip(b) {
                let v = x + b;
                // candle's `gelu_erf` and `relu`
                *o = if self.gelu { (erf_f32(v * FRAC_1_SQRT_2) + 1.) * 0.5 * v } else { v.max(0f32) };
            }
        });
        Ok((CpuStorage::F32(out), l1.shape().clone()))
    }
}

struct BoxBias;

impl CustomOp2 for BoxBias {
    fn name(&self) -> &'static str {
        "box-relative-position-bias"
    }

    fn cpu_fwd(&self, s1: &CpuStorage, l1: &Layout, s2: &CpuStorage, l2: &Layout) -> candle_core::Result<(CpuStorage, Shape)> {
        let (ey, ex) = (values(s1, l1)?, values(s2, l2)?);
        let (q, side, heads) = l1.shape().dims3()?;
        if l2.shape() != l1.shape() {
            bail!("box bias terms {:?} and {:?}", l1.shape(), l2.shape());
        }
        let n = side * side;
        let mut out = vec![0f32; heads * (q + 1) * n];
        // row 0 of each head (the presence token) stays zero
        out.par_chunks_mut(n).enumerate().filter(|(i, _)| i % (q + 1) != 0).for_each(|(i, o)| {
            let (h, q) = (i / (q + 1), i % (q + 1) - 1);
            for (y, o) in o.chunks_exact_mut(side).enumerate() {
                let ey = ey[(q * side + y) * heads + h];
                for (x, o) in o.iter_mut().enumerate() {
                    *o = ey + ex[(q * side + x) * heads + h];
                }
            }
        });
        Ok((CpuStorage::F32(out), Shape::from((1, heads, q + 1, n))))
    }
}

#[cfg(test)]
mod tests {
    use candle_core::Device;
    use candle_nn::Module;

    use super::*;

    fn randn(shape: &[usize]) -> Tensor {
        Tensor::randn(0f32, 1.0, shape, &Device::Cpu).unwrap()
    }

    #[test]
    fn linear_act_matches_the_tensor_ops() {
        let x = randn(&[2, 7, 5]);
        let l = Linear::new(randn(&[6, 5]), Some(randn(&[6])));
        for gelu in [false, true] {
            let want = l.forward(&x).unwrap();
            let want = if gelu { want.gelu_erf() } else { want.relu() }.unwrap().reshape((14, 6)).unwrap();
            let got = linear_act(&x, &l, gelu).unwrap();
            assert_eq!(got.dims(), want.dims());
            assert_eq!(got.flatten_all().unwrap().to_vec1::<f32>().unwrap(), want.flatten_all().unwrap().to_vec1::<f32>().unwrap());
        }
        assert!(linear_act(&x, &Linear::new(randn(&[6, 5]), None), false).is_err());
    }

    #[test]
    fn box_bias_matches_the_tensor_ops() {
        let (ey, ex) = (randn(&[4, 5, 3]), randn(&[4, 5, 3]));
        let want = ey.unsqueeze(2).unwrap().broadcast_add(&ex.unsqueeze(1).unwrap()).unwrap();
        let want = want.reshape((4, 25, 3)).unwrap().permute((2, 0, 1)).unwrap().pad_with_zeros(1, 1, 0).unwrap().unsqueeze(0).unwrap();
        let got = box_bias(&ey, &ex).unwrap();
        assert_eq!(got.dims(), want.dims());
        // the same additions: bit-identical
        assert_eq!(got.flatten_all().unwrap().to_vec1::<f32>().unwrap(), want.flatten_all().unwrap().to_vec1::<f32>().unwrap());
        assert!(box_bias(&ey, &randn(&[4, 6, 3])).is_err());
    }
}
