//! An optional accelerator for the large matrix products of the models (`activations · weights`):
//! the host (the engine) installs one, e.g. the GPU matrix kernel of `lightcraft-gpu`, and products
//! big enough to gain from it go there. A product the accelerator can't do (`None`) runs on the
//! CPU as before, so installing one never changes what is computed, only where.

use std::sync::{Arc, RwLock};

/// Something that multiplies `a` (`rows × k`, row-major) by the weight matrix `key` (`k × n`,
/// row-major, given by `weights` the first time it is needed, so it can be kept), or declines.
pub trait Gemm: Send + Sync {
    fn gemm(&self, a: &[f32], rows: usize, k: usize, n: usize, key: u64, weights: &dyn Fn() -> Option<Vec<f32>>) -> Option<Vec<f32>>;
}

static GEMM: RwLock<Option<Arc<dyn Gemm>>> = RwLock::new(None);

/// Products smaller than this many multiply-adds stay on the CPU (moving the data costs more).
pub const MIN_WORK: usize = 1 << 26;

/// Install (or, with `None`, remove) the accelerator.
pub fn set_gemm(g: Option<Arc<dyn Gemm>>) {
    *GEMM.write().unwrap_or_else(std::sync::PoisonError::into_inner) = g;
}

/// Whether one is installed.
pub fn installed() -> bool {
    GEMM.read().unwrap_or_else(std::sync::PoisonError::into_inner).is_some()
}

/// The product on the accelerator when one is installed and the product is large enough.
pub(crate) fn gemm(a: &[f32], rows: usize, k: usize, n: usize, key: u64, weights: &dyn Fn() -> Option<Vec<f32>>) -> Option<Vec<f32>> {
    let work = rows.saturating_mul(k).saturating_mul(n);
    if work < MIN_WORK {
        return None;
    }
    let g = GEMM.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()?;
    let out = g.gemm(a, rows, k, n, key, weights)?;
    (out.len() == rows.saturating_mul(n)).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Doubles;
    impl Gemm for Doubles {
        fn gemm(&self, _: &[f32], rows: usize, _: usize, n: usize, _: u64, _: &dyn Fn() -> Option<Vec<f32>>) -> Option<Vec<f32>> {
            Some(vec![2.0; rows * n])
        }
    }

    struct Wrong;
    impl Gemm for Wrong {
        fn gemm(&self, _: &[f32], _: usize, _: usize, _: usize, _: u64, _: &dyn Fn() -> Option<Vec<f32>>) -> Option<Vec<f32>> {
            Some(vec![1.0; 3])
        }
    }

    #[test]
    fn small_products_and_bad_answers_stay_on_the_cpu() {
        let w = || Some(vec![0.0f32; 4]);
        set_gemm(Some(Arc::new(Doubles)));
        assert!(gemm(&[0.0; 4], 2, 2, 2, 1, &w).is_none(), "too small");
        let (rows, k, n) = (1 << 10, 1 << 8, 1 << 8);
        let a = vec![0.0; rows * k];
        assert_eq!(gemm(&a, rows, k, n, 1, &w).map(|c| c.len()), Some(rows * n));
        set_gemm(Some(Arc::new(Wrong)));
        assert!(gemm(&a, rows, k, n, 1, &w).is_none(), "a wrong-sized answer is not used");
        set_gemm(None);
        assert!(!installed() && gemm(&a, rows, k, n, 1, &w).is_none());
    }
}
