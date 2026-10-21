//! Bounded, nonrecursive cubic functions on `0..1`. Each piece has its own endpoint values and
//! normalized endpoint tangents, including distinct one-sided slopes at clamped interior knots.
//! Restriction and linear combination preserve the function without a monotonicity refit.

use crate::spline::Lut1;
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

pub const MAX_CURVE_PIECES: usize = 4096;
pub const MAX_CURVE_LUT_ENTRIES: usize = 65_536;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CurveError {
    Empty,
    TooManyPieces,
    InvalidDomain,
    NonFinite,
    Discontinuous,
    InvalidSpline,
    NotRepresentable,
    InvalidLutSize,
}
impl fmt::Display for CurveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "a curve function needs at least one piece",
            Self::TooManyPieces => "the curve function exceeds the 4096-piece limit",
            Self::InvalidDomain => "curve intervals must increase strictly and cover 0 through 1",
            Self::NonFinite => "curve values, tangents and operation parameters must be finite",
            Self::Discontinuous => "adjacent curve pieces have different endpoint values",
            Self::InvalidSpline => "the source spline has invalid or inconsistent knots or tangents",
            Self::NotRepresentable => "the curve result cannot be represented finitely by the renderer",
            Self::InvalidLutSize => "a curve LUT needs between 2 and 65536 entries",
        })
    }
}
impl std::error::Error for CurveError {}
type Result<T> = std::result::Result<T, CurveError>;

/// Wire: `{"end": number, "hermite": [start_value,end_value,start_tangent,end_tangent]}`.
/// Start is the preceding piece's end, or zero. Tangents use the piece's unit interval.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CubicPiece {
    end: f64,
    hermite: [f64; 4],
}
impl CubicPiece {
    /// Validate a piece, including finite f32 output at every interior cubic extremum.
    pub fn new(end: f64, hermite: [f64; 4]) -> Result<Self> {
        if !end.is_finite() || hermite.iter().any(|v| !v.is_finite()) {
            return Err(CurveError::NonFinite);
        }
        if !(0.0 < end && end <= 1.0) {
            return Err(CurveError::InvalidDomain);
        }
        let piece = Self { end, hermite };
        piece.validate_output()?;
        Ok(piece)
    }
    pub fn end(&self) -> f64 {
        self.end
    }
    pub fn hermite(&self) -> [f64; 4] {
        self.hermite
    }
    fn value(&self, t: f64) -> Result<f64> {
        let [y0, y1, m0, m1] = self.hermite;
        // Preserve a constant exactly, including f32::MAX: independently rounded Hermite
        // basis terms can otherwise produce a spurious one-ulp output overflow.
        if y0 == y1 && m0 == 0.0 && m1 == 0.0 {
            return Ok(y0);
        }
        if t == 0.0 {
            return Ok(y0);
        }
        if t == 1.0 {
            return Ok(y1);
        }
        let (t2, t3) = (t * t, t * t * t);
        finite(y0 + (-2.0 * t3 + 3.0 * t2) * (y1 - y0) + (t3 - 2.0 * t2 + t) * m0 + (t3 - t2) * m1)
    }
    fn derivative(&self, t: f64) -> Result<f64> {
        let [y0, y1, m0, m1] = self.hermite;
        if t == 0.0 {
            return Ok(m0);
        }
        if t == 1.0 {
            return Ok(m1);
        }
        let t2 = t * t;
        finite((-6.0 * t2 + 6.0 * t) * (y1 - y0) + (3.0 * t2 - 4.0 * t + 1.0) * m0 + (3.0 * t2 - 2.0 * t) * m1)
    }
    fn validate_output(&self) -> Result<()> {
        let [y0, y1, m0, m1] = self.hermite;
        renderable(y0)?;
        renderable(y1)?;
        let scale = y0.abs().max(y1.abs()).max(m0.abs()).max(m1.abs());
        if scale == 0.0 {
            return Ok(());
        }
        // Normalize before forming the derivative quadratic: hostile finite tangents must not
        // overflow coefficients/discriminant. Endpoints alone would miss cubic overshoot.
        let [a, b, c, d] = self.hermite.map(|v| v / scale);
        let qa = 3.0 * (2.0 * (a - b) + c + d);
        let qb = 2.0 * (3.0 * (b - a) - 2.0 * c - d);
        let qc = c;
        let check = |x: f64| -> Result<()> {
            if x.is_finite() && x > 0.0 && x < 1.0 {
                renderable(self.value(x)?)?;
            }
            Ok(())
        };
        if qa == 0.0 {
            if qb != 0.0 {
                check(-qc / qb)?;
            }
        } else {
            let disc = qb * qb - 4.0 * qa * qc;
            if disc >= 0.0 {
                let q = -0.5 * (qb + disc.sqrt().copysign(qb));
                if q == 0.0 {
                    check(-qb / (2.0 * qa))?;
                } else {
                    check(q / qa)?;
                    check(qc / q)?;
                }
            }
        }
        Ok(())
    }
    pub(crate) fn restricted(&self, lo: f64, hi: f64, end: f64) -> Result<Self> {
        if !lo.is_finite() || !hi.is_finite() || !(0.0 <= lo && lo < hi && hi <= 1.0) {
            return Err(CurveError::InvalidDomain);
        }
        let scale = hi - lo;
        Self::new(end, [self.value(lo)?, self.value(hi)?, finite(self.derivative(lo)? * scale)?, finite(self.derivative(hi)? * scale)?])
    }
}
impl<'de> Deserialize<'de> for CubicPiece {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            end: f64,
            hermite: [f64; 4],
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.end, wire.hermite).map_err(de::Error::custom)
    }
}

/// Wire: `{"pieces": [CubicPiece, ...]}`. No nested curves or arbitrary settings payload.
/// Continuity tolerates 64 f64 epsilons relative to endpoint magnitude (minimum scale one).
/// Endpoint values are checked, not silently rewritten.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CurveFunction {
    pieces: Vec<CubicPiece>,
}
fn finite(v: f64) -> Result<f64> {
    if v.is_finite() { Ok(v) } else { Err(CurveError::NotRepresentable) }
}
fn renderable(v: f64) -> Result<f64> {
    if v.is_finite() && v.abs() <= f32::MAX as f64 { Ok(v) } else { Err(CurveError::NotRepresentable) }
}
fn continuous(a: f64, b: f64) -> bool {
    (a - b).abs() <= 64.0 * f64::EPSILON * a.abs().max(b.abs()).max(1.0)
}
impl CurveFunction {
    /// Validate coverage and continuity, without allocating from a caller's size hint.
    pub fn from_pieces(pieces: impl IntoIterator<Item = CubicPiece>) -> Result<Self> {
        let mut out = Vec::new();
        let mut end = 0.0;
        let mut previous = None;
        for piece in pieces {
            if out.len() == MAX_CURVE_PIECES {
                return Err(CurveError::TooManyPieces);
            }
            if piece.end <= end {
                return Err(CurveError::InvalidDomain);
            }
            let [a, b, _, _] = piece.hermite;
            if previous.is_some_and(|v| !continuous(v, a)) {
                return Err(CurveError::Discontinuous);
            }
            previous = Some(b);
            end = piece.end;
            out.push(piece);
        }
        if out.is_empty() {
            return Err(CurveError::Empty);
        }
        if end != 1.0 {
            return Err(CurveError::InvalidDomain);
        }
        Ok(Self { pieces: out })
    }
    pub fn pieces(&self) -> &[CubicPiece] {
        &self.pieces
    }
    /// Clamp finite x outside the unit domain; nonfinite inputs return an error.
    pub fn eval(&self, x: f64) -> Result<f64> {
        if !x.is_finite() {
            return Err(CurveError::NonFinite);
        }
        let x = x.clamp(0.0, 1.0);
        let i = self.pieces.partition_point(|p| p.end < x);
        let piece = self.pieces.get(i).ok_or(CurveError::InvalidDomain)?;
        let start = i.checked_sub(1).and_then(|n| self.pieces.get(n)).map_or(0.0, |p| p.end);
        piece.value((x - start) / (piece.end - start))
    }
    /// Restrict `[start,end]` and remap its input interval to `0..1`.
    pub fn restricted(&self, start: f64, end: f64) -> Result<Self> {
        if !start.is_finite() || !end.is_finite() || !(0.0 <= start && start < end && end <= 1.0) {
            return Err(CurveError::InvalidDomain);
        }
        if start == 0.0 && end == 1.0 {
            return Ok(self.clone());
        }
        let mut out = Vec::new();
        let mut lo = 0.0_f64;
        for piece in &self.pieces {
            let (a, b) = (lo.max(start), piece.end.min(end));
            if a < b {
                let width = piece.end - lo;
                let mapped_end = if b == end { 1.0 } else { (b - start) / (end - start) };
                out.push(piece.restricted((a - lo) / width, (b - lo) / width, mapped_end)?);
            }
            lo = piece.end;
        }
        Self::from_pieces(out)
    }
    /// Post-curve output transform `offset + scale * value`, without clipping.
    pub fn affine(&self, scale: f64, offset: f64) -> Result<Self> {
        if !scale.is_finite() || !offset.is_finite() {
            return Err(CurveError::NonFinite);
        }
        if scale == 1.0 && offset == 0.0 {
            return Ok(self.clone());
        }
        let out = self
            .pieces
            .iter()
            .map(|piece| {
                let [a, b, c, d] = piece.hermite;
                CubicPiece::new(piece.end, [finite(offset + scale * a)?, finite(offset + scale * b)?, finite(scale * c)?, finite(scale * d)?])
            })
            .collect::<Result<Vec<_>>>()?;
        Self::from_pieces(out)
    }
    /// `self + amount * (target-self)`. Exact model clones at zero and one; supports extrapolation.
    pub fn blend(&self, target: &Self, amount: f64) -> Result<Self> {
        if !amount.is_finite() {
            return Err(CurveError::NonFinite);
        }
        if amount == 0.0 || self == target {
            return Ok(self.clone());
        }
        if amount == 1.0 {
            return Ok(target.clone());
        }
        self.combine(target, |a, b| finite(a + amount * (b - a)))
    }
    /// `self_weight*self + target_weight*target`; useful for adding a sparse correction curve.
    pub fn linear_combination(&self, target: &Self, self_weight: f64, target_weight: f64) -> Result<Self> {
        if !self_weight.is_finite() || !target_weight.is_finite() {
            return Err(CurveError::NonFinite);
        }
        if self_weight == 1.0 && target_weight == 0.0 {
            return Ok(self.clone());
        }
        if self_weight == 0.0 && target_weight == 1.0 {
            return Ok(target.clone());
        }
        self.combine(target, |a, b| finite(self_weight * a + target_weight * b))
    }
    fn combine(&self, target: &Self, mix: impl Fn(f64, f64) -> Result<f64>) -> Result<Self> {
        let (mut ai, mut bi) = (0usize, 0usize);
        let (mut a_start, mut b_start, mut start) = (0.0, 0.0, 0.0);
        let mut out = Vec::new();
        while start < 1.0 {
            if out.len() == MAX_CURVE_PIECES {
                return Err(CurveError::TooManyPieces);
            }
            let a = self.pieces.get(ai).ok_or(CurveError::InvalidDomain)?;
            let b = target.pieces.get(bi).ok_or(CurveError::InvalidDomain)?;
            let end = a.end.min(b.end);
            let ar = a.restricted((start - a_start) / (a.end - a_start), (end - a_start) / (a.end - a_start), end)?;
            let br = b.restricted((start - b_start) / (b.end - b_start), (end - b_start) / (b.end - b_start), end)?;
            let mut h = [0.0; 4];
            for (slot, (av, bv)) in h.iter_mut().zip(ar.hermite.into_iter().zip(br.hermite)) {
                *slot = mix(av, bv)?;
            }
            out.push(CubicPiece::new(end, h)?);
            if end == a.end {
                a_start = end;
                ai += 1;
            }
            if end == b.end {
                b_start = end;
                bi += 1;
            }
            start = end;
        }
        Self::from_pieces(out)
    }
    /// A bounded render grid; samples and the renderer's adjacent-sample differences are finite.
    pub fn to_lut(&self, entries: usize) -> Result<Lut1> {
        if !(2..=MAX_CURVE_LUT_ENTRIES).contains(&entries) {
            return Err(CurveError::InvalidLutSize);
        }
        let mut v: Vec<f32> = Vec::with_capacity(entries);
        for i in 0..entries {
            let y = renderable(self.eval(i as f64 / (entries - 1) as f64)?)? as f32;
            // Lut1 and the GPU interpolate as a + (b-a)*t. Finite samples alone do not
            // guarantee finite interpolation when consecutive values have opposite signs.
            if !y.is_finite() || v.last().is_some_and(|previous| !(y - *previous).is_finite()) {
                return Err(CurveError::NotRepresentable);
            }
            v.push(y);
        }
        Ok(Lut1 { v })
    }
}
impl<'de> Deserialize<'de> for CurveFunction {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Pieces(Vec<CubicPiece>);
        impl<'de> Deserialize<'de> for Pieces {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
                struct Bounded;
                impl<'de> Visitor<'de> for Bounded {
                    type Value = Pieces;
                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("at most 4096 cubic pieces")
                    }
                    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error> {
                        let mut pieces = Vec::new();
                        while pieces.len() < MAX_CURVE_PIECES {
                            match seq.next_element::<CubicPiece>()? {
                                Some(p) => pieces.push(p),
                                None => return Ok(Pieces(pieces)),
                            }
                        }
                        struct Excess;
                        impl<'de> Deserialize<'de> for Excess {
                            fn deserialize<D: Deserializer<'de>>(_: D) -> std::result::Result<Self, D::Error> {
                                Err(de::Error::custom(CurveError::TooManyPieces))
                            }
                        }
                        let _ = seq.next_element::<Excess>()?;
                        Ok(Pieces(pieces))
                    }
                }
                deserializer.deserialize_seq(Bounded)
            }
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            pieces: Pieces,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::from_pieces(wire.pieces.0).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spline::MonotoneCurve;
    use serde_json::json;

    fn spline(points: &[(f64, f64)]) -> CurveFunction {
        MonotoneCurve::new(points).to_function().unwrap()
    }
    fn base() -> CurveFunction {
        spline(&[(0.0, 0.0), (0.27, 0.08), (0.62, 0.8), (1.0, 1.0)])
    }
    fn target() -> CurveFunction {
        spline(&[(0.0, 0.12), (0.43, 0.68), (1.0, 0.96)])
    }
    fn close(a: f64, b: f64) {
        assert!((a - b).abs() <= 2e-12 * a.abs().max(b.abs()).max(1.0), "{a} != {b}");
    }

    #[test]
    fn conversion_preserves_existing_spline_and_exact_knot_values() {
        let cases = vec![
            vec![],
            vec![(0.2, 0.8)],
            vec![(0.25, 0.25), (0.75, 0.75)],
            vec![(0.0, 0.8), (0.2, 0.1), (0.6, 0.9), (1.0, 0.3)],
            vec![(-0.25, 0.1), (0.4, 0.7), (1.25, 0.9)],
            vec![(0.2, 0.1), (0.20000001, 0.9), (0.7, 0.4)],
            vec![(1.2, 0.4), (1.8, 0.7)],
            vec![(-2.0, 0.2), (-1.0, 0.4)],
        ];
        for points in cases {
            let old = MonotoneCurve::new(&points);
            let exact = old.to_function().unwrap();
            for i in 0..=10000 {
                let x = i as f64 / 10000.0;
                close(exact.eval(x).unwrap(), old.eval(x));
            }
            for (x, y) in old.points().into_iter().filter(|(x, _)| (0.0..=1.0).contains(x)) {
                assert_eq!(exact.eval(x).unwrap(), y);
            }
            assert_eq!(exact.eval(-10.0).unwrap(), exact.eval(0.0).unwrap());
            assert_eq!(exact.eval(10.0).unwrap(), exact.eval(1.0).unwrap());
        }
    }

    #[test]
    fn interior_clamping_keeps_distinct_one_sided_tangents() {
        let c = spline(&[(0.25, 0.25), (0.75, 0.75)]);
        assert_eq!(c.pieces().len(), 3);
        assert_eq!(c.pieces()[0].hermite(), [0.25, 0.25, 0.0, 0.0]);
        assert_eq!(c.pieces()[1].hermite(), [0.25, 0.75, 0.5, 0.5]);
        assert_eq!(c.pieces()[2].hermite(), [0.75, 0.75, 0.0, 0.0]);
        assert_eq!(c.eval(0.1).unwrap(), 0.25);
        assert_eq!(c.eval(0.9).unwrap(), 0.75);
    }

    #[test]
    fn unequal_knot_blends_follow_formula_and_do_not_jump_at_half() {
        let (a, b) = (base(), target());
        assert_eq!(a.blend(&b, 0.0).unwrap(), a);
        assert_eq!(a.blend(&b, 1.0).unwrap(), b);
        for t in [1e-12, 0.25, 0.499999, 0.5, 0.500001, 0.75, 1.0 - 1e-12, 1.5, 2.0, -0.5] {
            let c = a.blend(&b, t).unwrap();
            for i in 0..=4096 {
                let x = i as f64 / 4096.0;
                let av = a.eval(x).unwrap();
                close(c.eval(x).unwrap(), av + t * (b.eval(x).unwrap() - av));
            }
        }
        let (lo, hi) = (a.blend(&b, 0.499999).unwrap(), a.blend(&b, 0.500001).unwrap());
        for i in 0..=1023 {
            let x = i as f64 / 1023.0;
            assert!((lo.eval(x).unwrap() - hi.eval(x).unwrap()).abs() < 2e-6);
        }
    }

    #[test]
    fn extrapolation_retains_signed_output_without_a_new_limiter() {
        let a = spline(&[(0.0, 0.8), (1.0, 0.2)]);
        let b = spline(&[(0.0, 0.1), (1.0, 0.9)]);
        let c = a.blend(&b, 2.0).unwrap();
        close(c.eval(0.0).unwrap(), -0.6);
        close(c.eval(1.0).unwrap(), 1.6);
        assert!(c.to_lut(1024).unwrap().v.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn restriction_affine_and_addition_preserve_functions() {
        let (a, b) = (base(), target());
        let r = a.restricted(0.13, 0.89).unwrap();
        let f = a.affine(-0.7, 0.2).unwrap();
        let sum = a.linear_combination(&b, 1.0, 1.0).unwrap();
        let difference = a.linear_combination(&b, 1.0, -1.0).unwrap();
        for i in 0..=4096 {
            let x = i as f64 / 4096.0;
            close(r.eval(x).unwrap(), a.eval(0.13 + x * 0.76).unwrap());
            close(f.eval(x).unwrap(), 0.2 - 0.7 * a.eval(x).unwrap());
            close(sum.eval(x).unwrap(), a.eval(x).unwrap() + b.eval(x).unwrap());
            close(difference.eval(x).unwrap(), a.eval(x).unwrap() - b.eval(x).unwrap());
        }
        assert_eq!(a.restricted(0.0, 1.0).unwrap(), a);
        assert_eq!(a.affine(1.0, 0.0).unwrap(), a);
    }

    #[test]
    fn repeated_blends_remain_nonrecursive_and_keep_breakpoint_count() {
        let t = target();
        let mut c = base().blend(&t, 0.5).unwrap();
        let count = c.pieces().len();
        for _ in 0..100 {
            c = c.blend(&t, 0.25).unwrap();
            assert_eq!(c.pieces().len(), count);
        }
        let wire = serde_json::to_value(&c).unwrap();
        assert_eq!(wire.as_object().unwrap().len(), 1);
        assert_eq!(wire["pieces"].as_array().unwrap().len(), count);
    }

    #[test]
    fn native_json_roundtrip_preserves_exact_values_and_lut() {
        let c = base().blend(&target(), 0.37).unwrap();
        let text = serde_json::to_string(&c).unwrap();
        let copy: CurveFunction = serde_json::from_str(&text).unwrap();
        assert_eq!(copy, c);
        assert_eq!(copy.to_lut(1024).unwrap(), c.to_lut(1024).unwrap());
        assert!(text.contains("\"hermite\""));
    }

    #[test]
    fn deserialization_rejects_malformed_or_discontinuous_functions() {
        let piece = json!({"end":1.0,"hermite":[0.0,1.0,1.0,1.0]});
        let invalid = vec![
            json!({"pieces":[]}),
            json!({"pieces":[{"end":0.5,"hermite":[0.0,0.5,0.5,0.5]}]}),
            json!({"pieces":[{"end":0.5,"hermite":[0.0,0.5,0.5,0.5]},{"end":1.0,"hermite":[0.6,1.0,0.4,0.4]}]}),
            json!({"pieces":[{"end":0.0,"hermite":[0.0,1.0,1.0,1.0]}]}),
            json!({"pieces":[{"end":1.0,"hermite":[0.0,1.0,1.0]}]}),
            json!({"pieces":[{"end":1.0,"hermite":[0.0,1.0,1.0,1.0,0.0]}]}),
            json!({"pieces":[{"end":1.0,"hermite":[0.0,1.0,"bad",1.0]}]}),
            json!({"pieces":[piece.clone(),piece]}),
            json!({"pieces":"bad"}),
            json!({"pieces":[{"end":1.0,"hermite":[0.0,0.0,1e300,-1e300]}]}),
            json!({"pieces":[{"end":1.0,"hermite":[0.0,1.0,1.0,1.0],"nested":{"pieces":[]}}]}),
        ];
        for v in invalid {
            assert!(serde_json::from_value::<CurveFunction>(v.clone()).is_err(), "{v}");
        }
        assert!(serde_json::from_str::<CurveFunction>(r#"{"pieces":[{"end":1,"hermite":[0,1,1e999,1]}]}"#).is_err());
    }

    fn constant_partition(count: usize, offset: f64) -> CurveFunction {
        CurveFunction::from_pieces((1..=count).map(|i| {
            let end = if i == count { 1.0 } else { (i as f64 - offset) / count as f64 };
            CubicPiece::new(end, [0.5, 0.5, 0.0, 0.0]).unwrap()
        }))
        .unwrap()
    }
    #[test]
    fn limits_bound_construction_streaming_input_union_and_lut() {
        let max = constant_partition(MAX_CURVE_PIECES, 0.0);
        assert_eq!(max.pieces().len(), MAX_CURVE_PIECES);
        let shifted = constant_partition(MAX_CURVE_PIECES, 0.5);
        assert_eq!(max.blend(&shifted, 0.5), Err(CurveError::TooManyPieces));
        let mut wire = serde_json::to_value(&max).unwrap();
        wire["pieces"].as_array_mut().unwrap().push(json!({"payload":["ignored after the cap"]}));
        let error = serde_json::from_str::<CurveFunction>(&wire.to_string()).unwrap_err().to_string();
        assert!(error.contains("4096-piece"), "{error}");
        let extra = (1..=MAX_CURVE_PIECES + 1).map(|i| CubicPiece::new(i as f64 / (MAX_CURVE_PIECES + 1) as f64, [0.5, 0.5, 0.0, 0.0]).unwrap());
        assert_eq!(CurveFunction::from_pieces(extra), Err(CurveError::TooManyPieces));
        for n in [0, 1, MAX_CURVE_LUT_ENTRIES + 1, usize::MAX] {
            assert_eq!(base().to_lut(n), Err(CurveError::InvalidLutSize));
        }
    }

    #[test]
    fn nonfinite_arguments_overflow_and_hidden_overshoot_return_errors() {
        let c = base();
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(c.eval(v), Err(CurveError::NonFinite));
            assert_eq!(c.blend(&target(), v), Err(CurveError::NonFinite));
            assert_eq!(c.affine(v, 0.0), Err(CurveError::NonFinite));
            assert_eq!(c.linear_combination(&target(), 1.0, v), Err(CurveError::NonFinite));
            assert!(CubicPiece::new(1.0, [0.0, 1.0, v, 1.0]).is_err());
        }
        assert!(c.affine(f64::MAX, 0.0).is_err());
        assert!(c.blend(&target(), f64::MAX).is_err());
        assert_eq!(CubicPiece::new(1.0, [0.0, 0.0, 8.0 * f32::MAX as f64, -8.0 * f32::MAX as f64]), Err(CurveError::NotRepresentable));
        for (a, b) in [(0.5, 0.5), (0.8, 0.2), (-0.1, 0.9), (0.1, 1.1)] {
            assert_eq!(c.restricted(a, b), Err(CurveError::InvalidDomain));
        }
    }

    #[test]
    fn finite_lut_values_must_also_have_finite_interpolation_differences() {
        let limit = f32::MAX as f64;
        let c = CurveFunction::from_pieces([CubicPiece::new(1.0, [-limit, limit, 2.0 * limit, 2.0 * limit]).unwrap()]).unwrap();
        assert_eq!(c.to_lut(2), Err(CurveError::NotRepresentable));
        let lut = c.to_lut(1024).unwrap();
        assert!(lut.eval(0.5).is_finite());
    }

    #[test]
    fn extreme_representable_constant_stays_exactly_constant() {
        let limit = f32::MAX as f64;
        let c = CurveFunction::from_pieces([CubicPiece::new(1.0, [limit, limit, 0.0, 0.0]).unwrap()]).unwrap();
        for i in 0..=1023 {
            assert_eq!(c.eval(i as f64 / 1023.0).unwrap(), limit);
        }
        assert!(c.to_lut(1024).unwrap().v.iter().all(|v| *v == f32::MAX));
    }

    #[test]
    fn near_constant_maximum_does_not_spuriously_overflow() {
        let limit = f32::MAX as f64;
        let c = CurveFunction::from_pieces([CubicPiece::new(1.0, [limit, limit * (1.0 - 1e-12), 0.0, 0.0]).unwrap()]).unwrap();
        assert!(c.to_lut(MAX_CURVE_LUT_ENTRIES).unwrap().v.iter().all(|v| v.is_finite()));
        assert!(c.restricted(0.0001, 0.9999).unwrap().to_lut(MAX_CURVE_LUT_ENTRIES).is_ok());
    }

    #[test]
    fn malformed_deserialized_splines_are_rejected_before_evaluation() {
        for wire in [
            json!({"xs":[],"ys":[],"ms":[]}),
            json!({"xs":[0.0,1.0],"ys":[0.0],"ms":[1.0,1.0]}),
            json!({"xs":[0.5,0.5],"ys":[0.0,1.0],"ms":[1.0,1.0]}),
            json!({"xs":[1.0,0.0],"ys":[0.0,1.0],"ms":[1.0,1.0]}),
        ] {
            let old: MonotoneCurve = serde_json::from_value(wire).unwrap();
            assert_eq!(old.to_function(), Err(CurveError::InvalidSpline));
        }
    }

    proptest::proptest! {
        #[test]
        fn arbitrary_small_curves_blend_like_their_functions(ys in proptest::collection::vec(-2.0f64..2.0,2..12), t in -1.0f64..3.0) {
            let n=ys.len();let pts:Vec<_>=ys.into_iter().enumerate().map(|(i,y)|(i as f64/(n-1)as f64,y)).collect();
            let a=spline(&pts);let b=target();let mixed=a.blend(&b,t).unwrap();
            for i in 0..=128 {let x=i as f64/128.0;let av=a.eval(x).unwrap();close(mixed.eval(x).unwrap(),av+t*(b.eval(x).unwrap()-av));}
        }
    }
}
