//! Turning what Core ML hands back into plain row-major `f32`: its arrays may be padded (strides larger than the
//! shape needs, which the Neural Engine does) and may hold 16-bit floats. Safe code only; every number that came
//! from the framework is treated as hostile (checked arithmetic, `get()` instead of indexing, capped sizes).

use crate::Error;

/// Most dimensions a tensor may have.
pub const MAX_DIMS: usize = 8;

/// Most elements a tensor may have (a 24-megapixel RGB picture is 72 million).
pub const MAX_ELEMENTS: usize = 1 << 28;

/// The element types Core ML arrays are read as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dtype {
    F16,
    F32,
    F64,
}

impl Dtype {
    /// Bytes per element.
    pub fn size(self) -> usize {
        match self {
            Dtype::F16 => 2,
            Dtype::F32 => 4,
            Dtype::F64 => 8,
        }
    }
}

/// The number of elements of `shape`, or why it is not acceptable.
pub fn element_count(shape: &[usize]) -> Result<usize, Error> {
    if shape.is_empty() || shape.len() > MAX_DIMS {
        return Err(Error::Input(format!("a tensor needs 1 to {MAX_DIMS} dimensions, not {}", shape.len())));
    }
    let mut n = 1usize;
    for &d in shape {
        if d == 0 {
            return Err(Error::Input("a tensor dimension is 0".into()));
        }
        n = n.checked_mul(d).filter(|&n| n <= MAX_ELEMENTS).ok_or_else(|| Error::Input(format!("a tensor of {shape:?} is too large")))?;
    }
    Ok(n)
}

/// The strides (in elements) of a contiguous row-major array of `shape`.
pub fn contiguous_strides(shape: &[usize]) -> Vec<usize> {
    let mut strides = vec![1usize; shape.len()];
    for i in (0..shape.len().saturating_sub(1)).rev() {
        let next = strides.get(i + 1).copied().unwrap_or(1);
        let dim = shape.get(i + 1).copied().unwrap_or(1);
        if let Some(s) = strides.get_mut(i) {
            *s = next.saturating_mul(dim);
        }
    }
    strides
}

/// A 16-bit IEEE float as `f32`.
pub fn f16_to_f32(bits: u16) -> f32 {
    let sign = if bits & 0x8000 != 0 { -1.0f32 } else { 1.0 };
    let exp = i32::from((bits >> 10) & 0x1f);
    let frac = f32::from(bits & 0x3ff);
    match exp {
        0 => sign * frac * 2f32.powi(-24),
        0x1f if frac == 0.0 => sign * f32::INFINITY,
        0x1f => f32::NAN,
        _ => sign * (1.0 + frac / 1024.0) * 2f32.powi(exp - 15),
    }
}

/// One element from exactly `dtype.size()` bytes (anything else reads as 0, which `chunks_exact` and `get` rule out).
fn decode(b: &[u8], dtype: Dtype) -> f32 {
    match (dtype, b) {
        (Dtype::F16, &[a, c]) => f16_to_f32(u16::from_le_bytes([a, c])),
        (Dtype::F32, &[a, b, c, d]) => f32::from_le_bytes([a, b, c, d]),
        (Dtype::F64, &[a, b, c, d, e, f, g, h]) => f64::from_le_bytes([a, b, c, d, e, f, g, h]) as f32,
        _ => 0.0,
    }
}

/// The `element`th number of `bytes`, widened to `f32`; `None` when it lies outside them.
fn read(bytes: &[u8], dtype: Dtype, element: usize) -> Option<f32> {
    let size = dtype.size();
    let at = element.checked_mul(size)?;
    Some(decode(bytes.get(at..at.checked_add(size)?)?, dtype))
}

/// The elements of an array of `shape` whose element `[i, j, …]` sits at `Σ index × stride` (strides in elements) in
/// `bytes`, as contiguous row-major `f32`.
pub fn gather(bytes: &[u8], dtype: Dtype, shape: &[usize], strides: &[usize]) -> Result<Vec<f32>, Error> {
    let total = element_count(shape)?;
    if strides.len() != shape.len() {
        return Err(Error::Output(format!("{} strides for {} dimensions", strides.len(), shape.len())));
    }
    let short = || Error::Output("the array is smaller than its shape and strides say".into());
    let inner = shape.last().copied().ok_or_else(short)?;
    let inner_stride = strides.last().copied().ok_or_else(short)?;
    let outer = shape.get(..shape.len() - 1).unwrap_or(&[]);
    let outer_strides = strides.get(..strides.len() - 1).unwrap_or(&[]);
    let mut out = Vec::with_capacity(total);
    let mut index = vec![0usize; outer.len()];
    loop {
        let mut base = 0usize;
        for (i, s) in index.iter().zip(outer_strides) {
            base = i.checked_mul(*s).and_then(|v| base.checked_add(v)).ok_or_else(short)?;
        }
        if inner_stride == 1 {
            // a whole row at once
            let size = dtype.size();
            let (start, end) = (base.checked_mul(size), base.checked_add(inner).and_then(|e| e.checked_mul(size)));
            let row = start.zip(end).and_then(|(s, e)| bytes.get(s..e)).ok_or_else(short)?;
            out.extend(row.chunks_exact(size).map(|c| decode(c, dtype)));
        } else {
            for k in 0..inner {
                let at = k.checked_mul(inner_stride).and_then(|v| base.checked_add(v)).ok_or_else(short)?;
                out.push(read(bytes, dtype, at).ok_or_else(short)?);
            }
        }
        // the next row: an odometer over the outer dimensions
        let mut d = outer.len();
        loop {
            if d == 0 {
                return Ok(out);
            }
            d -= 1;
            let (Some(i), Some(&len)) = (index.get_mut(d), outer.get(d)) else { return Err(short()) };
            *i += 1;
            if *i < len {
                break;
            }
            *i = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The little-endian bytes of `values`.
    fn f32_bytes(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    /// Test: half floats decode exactly.
    #[test]
    fn half_floats_decode_exactly() {
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x7bff), 65504.0);
        assert_eq!(f16_to_f32(0x0001), 2f32.powi(-24));
        assert_eq!(f16_to_f32(0x7c00), f32::INFINITY);
        assert_eq!(f16_to_f32(0xfc00), f32::NEG_INFINITY);
        assert!(f16_to_f32(0x7e00).is_nan());
        assert_eq!(f16_to_f32(0x3555), 0.333_251_95);
    }

    /// Test: contiguous arrays are read in order.
    #[test]
    fn contiguous_arrays_are_read_in_order() {
        let v: Vec<f32> = (0..24).map(|i| i as f32).collect();
        let strides = contiguous_strides(&[2, 3, 4]);
        assert_eq!(strides, vec![12, 4, 1]);
        assert_eq!(gather(&f32_bytes(&v), Dtype::F32, &[2, 3, 4], &strides).unwrap(), v);
    }

    /// Test: padded rows are skipped.
    #[test]
    fn padded_rows_are_skipped() {
        // 2 x 3 elements, rows 5 apart: [0 1 2 . .] [5 6 7 . .]
        let buf: Vec<f32> = (0..10).map(|i| i as f32).collect();
        let got = gather(&f32_bytes(&buf), Dtype::F32, &[2, 3], &[5, 1]).unwrap();
        assert_eq!(got, vec![0.0, 1.0, 2.0, 5.0, 6.0, 7.0]);
    }

    /// Test: a transposed view is gathered row major.
    #[test]
    fn a_transposed_view_is_gathered_row_major() {
        // the 2 x 3 view of a 3 x 2 array stored row-major: element [i, j] is at i + 2 j
        let buf: Vec<f32> = (0..6).map(|i| i as f32).collect();
        let got = gather(&f32_bytes(&buf), Dtype::F32, &[2, 3], &[1, 2]).unwrap();
        assert_eq!(got, vec![0.0, 2.0, 4.0, 1.0, 3.0, 5.0]);
    }

    /// Test: half and double arrays are converted.
    #[test]
    fn half_and_double_arrays_are_converted() {
        let h: Vec<u8> = [0x3c00u16, 0xc000, 0x3800].iter().flat_map(|x| x.to_le_bytes()).collect();
        assert_eq!(gather(&h, Dtype::F16, &[3], &[1]).unwrap(), vec![1.0, -2.0, 0.5]);
        let d: Vec<u8> = [1.5f64, -0.25].iter().flat_map(|x| x.to_le_bytes()).collect();
        assert_eq!(gather(&d, Dtype::F64, &[2], &[1]).unwrap(), vec![1.5, -0.25]);
    }

    /// Test: hostile shapes and strides are errors not panics.
    #[test]
    fn hostile_shapes_and_strides_are_errors_not_panics() {
        let bytes = f32_bytes(&[1.0; 4]);
        // too short for what the strides reach
        assert!(gather(&bytes, Dtype::F32, &[2, 3], &[3, 1]).is_err());
        // strides that overflow
        assert!(gather(&bytes, Dtype::F32, &[2, 2], &[usize::MAX, 1]).is_err());
        assert!(gather(&bytes, Dtype::F32, &[2], &[usize::MAX]).is_err());
        // a stride count that does not match, no dimensions, a zero dimension, too many, too large
        assert!(gather(&bytes, Dtype::F32, &[2, 2], &[1]).is_err());
        assert!(gather(&bytes, Dtype::F32, &[], &[]).is_err());
        assert!(gather(&bytes, Dtype::F32, &[0, 2], &[2, 1]).is_err());
        assert!(element_count(&[1; MAX_DIMS + 1]).is_err());
        assert!(element_count(&[usize::MAX, 2]).is_err());
        assert!(element_count(&[MAX_ELEMENTS + 1]).is_err());
        assert_eq!(element_count(&[1, 4, 512, 512]).unwrap(), 4 * 512 * 512);
    }
}
