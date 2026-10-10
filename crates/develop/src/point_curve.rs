//! Editable sparse curves and checked cubic functions produced by partial preset amounts.
//! Function edits add a sparse signed correction to an immutable base. Reset replaces both.

use lightcraft_color::spline::{Lut1, MonotoneCurve};
use lightcraft_color::{CurveError, CurveFunction};
use lightcraft_geom::Point;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, sync::Arc};

pub const POINT_CURVE_LUT_SIZE: usize = 1024;
pub const MAX_POINT_CURVE_POINTS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PointCurveError {
    Curve(CurveError),
    TooManyPoints,
    InvalidPoint,
    InvalidAdjustments,
    InvalidIndex,
    FixedEndpoint,
    DuplicateX,
    LegacyOutputRange,
}
impl fmt::Display for PointCurveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Curve(e) => e.fmt(f),
            Self::TooManyPoints => f.write_str("a point curve supports at most 1024 points"),
            Self::InvalidPoint => f.write_str("curve coordinates must be finite and edit inputs must be between zero and one"),
            Self::InvalidAdjustments => f.write_str("curve adjustments must be empty or strictly ordered with endpoints at zero and one"),
            Self::InvalidIndex => f.write_str("the curve point index does not exist"),
            Self::FixedEndpoint => f.write_str("curve endpoints cannot be removed or moved horizontally"),
            Self::DuplicateX => f.write_str("curve points must have distinct input coordinates"),
            Self::LegacyOutputRange => f.write_str("partial Amount for a legacy point curve with outputs outside 0..1 is unsupported; the saved curve and exact 0%/100% remain available"),
        }
    }
}
impl std::error::Error for PointCurveError {}
impl From<CurveError> for PointCurveError {
    fn from(value: CurveError) -> Self {
        Self::Curve(value)
    }
}
type Result<T> = std::result::Result<T, PointCurveError>;

/// The base and signed adjustments are authoritative. Derived caches are checked before this
/// value can be constructed or deserialized, so the renderer never has to discard a curve error.
#[derive(Clone, Debug, PartialEq)]
pub struct FunctionCurve {
    base: CurveFunction,
    adjustments: Vec<Point>,
    effective: CurveFunction,
    lut: Arc<Lut1>,
    editable: Vec<Point>,
}

/// Sparse values preserve the existing point-array wire format and evaluator. Function values
/// serialize as `{kind:"function",version:1,base:{pieces:[...]},adjustments:[{x,y},...]}`.
#[derive(Clone, Debug, PartialEq)]
pub enum PointCurve {
    Sparse(Vec<Point>),
    Function(FunctionCurve),
}
impl Default for PointCurve {
    fn default() -> Self {
        Self::Sparse(Vec::new())
    }
}
/// Compatibility for trusted in-process point vectors. External input should use deserialization
/// or `try_from_points`, which bounds and validates it before accepting the settings.
impl From<Vec<Point>> for PointCurve {
    fn from(value: Vec<Point>) -> Self {
        Self::Sparse(value)
    }
}

fn validate_points(points: &[Point]) -> Result<()> {
    if points.len() > MAX_POINT_CURVE_POINTS {
        return Err(PointCurveError::TooManyPoints);
    }
    if points.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()) {
        return Err(PointCurveError::InvalidPoint);
    }
    Ok(())
}
fn spline(points: &[Point]) -> MonotoneCurve {
    MonotoneCurve::new(&points.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>())
}
fn visible_input(point: Point) -> Result<()> {
    if !point.x.is_finite() || !point.y.is_finite() || !(0.0..=1.0).contains(&point.x) {
        return Err(PointCurveError::InvalidPoint);
    }
    Ok(())
}
impl PointCurve {
    pub fn try_from_points(points: Vec<Point>) -> Result<Self> {
        validate_points(&points)?;
        spline(&points).to_function()?.to_lut(POINT_CURVE_LUT_SIZE)?;
        Ok(Self::Sparse(points))
    }
    pub fn from_function(base: CurveFunction, adjustments: Vec<Point>) -> Result<Self> {
        validate_points(&adjustments)?;
        if !adjustments.is_empty()
            && (adjustments.len() < 2
                || adjustments.first().is_none_or(|p| p.x != 0.0)
                || adjustments.last().is_none_or(|p| p.x != 1.0)
                || adjustments.windows(2).any(|w| !matches!(w, [a,b] if b.x-a.x >= 1e-9)))
        {
            return Err(PointCurveError::InvalidAdjustments);
        }
        // A missing correction means zero, whereas an empty MonotoneCurve means identity.
        let effective = if adjustments.iter().all(|p| p.y == 0.0) {
            base.clone()
        } else {
            base.linear_combination(&spline(&adjustments).to_function()?, 1.0, 1.0)?
        };
        let lut = Arc::new(effective.to_lut(POINT_CURVE_LUT_SIZE)?);
        let anchors = if adjustments.is_empty() { vec![Point::new(0.0, 0.0), Point::new(1.0, 0.0)] } else { adjustments.clone() };
        let editable = anchors.into_iter().map(|p| Ok(Point::new(p.x, base.eval(p.x)? + p.y))).collect::<Result<Vec<_>>>()?;
        validate_points(&editable)?;
        Ok(Self::Function(FunctionCurve { base, adjustments, effective, lut, editable }))
    }
    pub fn sparse_points(&self) -> Option<&[Point]> {
        match self {
            Self::Sparse(points) => Some(points),
            Self::Function(_) => None,
        }
    }
    pub fn is_sparse(&self) -> bool {
        matches!(self, Self::Sparse(_))
    }
    pub fn is_function(&self) -> bool {
        matches!(self, Self::Function(_))
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn is_identity(&self) -> bool {
        match self {
            Self::Sparse(points) => crate::ToneCurve::point_curve_is_identity(points),
            Self::Function(f) => {
                let mut start = 0.0;
                f.effective.pieces().iter().all(|p| {
                    let [a, b, c, d] = p.hermite();
                    let width = p.end() - start;
                    let identity = (a - start).abs() < 1e-9 && (b - p.end()).abs() < 1e-9 && (c - width).abs() < 1e-9 && (d - width).abs() < 1e-9;
                    start = p.end();
                    identity
                })
            }
        }
    }
    /// Uses the legacy sparse evaluator at exact endpoints, and a validated cache for functions.
    pub fn render_lut(&self) -> Lut1 {
        match self {
            Self::Sparse(points) => spline(points).to_lut(POINT_CURVE_LUT_SIZE),
            Self::Function(f) => f.lut.as_ref().clone(),
        }
    }
    pub fn effective_function(&self) -> Result<CurveFunction> {
        match self {
            Self::Sparse(points) => {
                validate_points(points)?;
                Ok(spline(points).to_function()?)
            }
            Self::Function(f) => Ok(f.effective.clone()),
        }
    }
    pub fn eval(&self, x: f64) -> Result<f64> {
        Ok(self.effective_function()?.eval(x)?)
    }
    pub fn blend(&self, target: &Self, amount: f64) -> Result<Self> {
        if !amount.is_finite() {
            return Err(CurveError::NonFinite.into());
        }
        if amount == 0.0 || self == target {
            return Ok(self.clone());
        }
        if amount == 1.0 {
            return Ok(target.clone());
        }
        // Legacy fade profiles clamp sparse handles before fitting. That operation cannot be
        // reconstructed from an arbitrary function, so refuse this uncommon legacy case rather
        // than introduce a discontinuity beside the exact 0/100% endpoint models.
        if [self, target].iter().any(|curve| curve.sparse_points().is_some_and(|points| points.iter().any(|p| !(0.0..=1.0).contains(&p.y)))) {
            return Err(PointCurveError::LegacyOutputRange);
        }
        Self::from_function(self.effective_function()?.blend(&target.effective_function()?, amount)?, Vec::new())
    }
    pub fn affine(&self, scale: f64, offset: f64) -> Result<Self> {
        if scale == 1.0 && offset == 0.0 {
            return Ok(self.clone());
        }
        Self::from_function(self.effective_function()?.affine(scale, offset)?, Vec::new())
    }
    /// Actual visible outputs, not stored offsets. Empty curves expose two endpoint handles.
    pub fn editable_points(&self) -> Vec<Point> {
        match self {
            Self::Sparse(points) => spline(points).points().into_iter().map(|(x, y)| Point::new(x, y)).collect(),
            Self::Function(f) => f.editable.clone(),
        }
    }
    fn replace_visible(&self, points: Vec<Point>) -> Result<Self> {
        match self {
            Self::Sparse(_) => Self::try_from_points(points),
            Self::Function(f) => {
                let adjustments = points.into_iter().map(|p| Ok(Point::new(p.x, p.y - f.base.eval(p.x)?))).collect::<Result<Vec<_>>>()?;
                Self::from_function(f.base.clone(), adjustments)
            }
        }
    }
    pub fn try_add_point(&mut self, point: Point) -> Result<usize> {
        visible_input(point)?;
        self.effective_function()?;
        let mut points = self.editable_points();
        if points.len() >= MAX_POINT_CURVE_POINTS {
            return Err(PointCurveError::TooManyPoints);
        }
        if points.iter().any(|p| (p.x - point.x).abs() < 1e-9) {
            return Err(PointCurveError::DuplicateX);
        }
        let index = points.partition_point(|p| p.x < point.x);
        points.insert(index, point);
        let replacement = self.replace_visible(points)?;
        *self = replacement;
        Ok(index)
    }
    pub fn try_move_point(&mut self, index: usize, point: Point) -> Result<usize> {
        visible_input(point)?;
        self.effective_function()?;
        let mut points = self.editable_points();
        let old = points.get(index).ok_or(PointCurveError::InvalidIndex)?;
        if *old == point {
            return Ok(index);
        }
        if (index == 0 || index + 1 == points.len()) && point.x != old.x {
            return Err(PointCurveError::FixedEndpoint);
        }
        if points.iter().enumerate().any(|(i, p)| i != index && (p.x - point.x).abs() < 1e-9) {
            return Err(PointCurveError::DuplicateX);
        }
        *points.get_mut(index).ok_or(PointCurveError::InvalidIndex)? = point;
        points.sort_by(|a, b| a.x.total_cmp(&b.x));
        let new_index = points.iter().position(|p| p.x == point.x).ok_or(PointCurveError::InvalidIndex)?;
        let replacement = self.replace_visible(points)?;
        *self = replacement;
        Ok(new_index)
    }
    pub fn try_remove_point(&mut self, index: usize) -> Result<()> {
        self.effective_function()?;
        let mut points = self.editable_points();
        if index >= points.len() {
            return Err(PointCurveError::InvalidIndex);
        }
        if index == 0 || index + 1 == points.len() {
            return Err(PointCurveError::FixedEndpoint);
        }
        points.remove(index);
        let replacement = self.replace_visible(points)?;
        *self = replacement;
        Ok(())
    }
}

impl Serialize for PointCurve {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Sparse(points) => points.serialize(serializer),
            Self::Function(f) => {
                let mut out = serializer.serialize_struct("PointCurveFunction", 4)?;
                out.serialize_field("kind", "function")?;
                out.serialize_field("version", &1u32)?;
                out.serialize_field("base", &f.base)?;
                out.serialize_field("adjustments", &f.adjustments)?;
                out.end()
            }
        }
    }
}
#[derive(Default)]
struct BoundedPoints(Vec<Point>);
impl<'de> Deserialize<'de> for BoundedPoints {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Points;
        impl<'de> Visitor<'de> for Points {
            type Value = BoundedPoints;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("at most 1024 curve points")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error> {
                read_points(&mut seq).map(BoundedPoints)
            }
        }
        deserializer.deserialize_seq(Points)
    }
}
fn read_points<'de, A: SeqAccess<'de>>(seq: &mut A) -> std::result::Result<Vec<Point>, A::Error> {
    let mut points = Vec::new();
    while points.len() < MAX_POINT_CURVE_POINTS {
        match seq.next_element::<Point>()? {
            Some(p) => points.push(p),
            None => return Ok(points),
        }
    }
    struct Excess;
    impl<'de> Deserialize<'de> for Excess {
        fn deserialize<D: Deserializer<'de>>(_: D) -> std::result::Result<Self, D::Error> {
            Err(de::Error::custom(PointCurveError::TooManyPoints))
        }
    }
    let _ = seq.next_element::<Excess>()?;
    Ok(points)
}
impl<'de> Deserialize<'de> for PointCurve {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Curve;
        impl<'de> Visitor<'de> for Curve {
            type Value = PointCurve;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a point array or a version-one curve function")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error> {
                PointCurve::try_from_points(read_points(&mut seq)?).map_err(de::Error::custom)
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> std::result::Result<Self::Value, A::Error> {
                #[derive(Deserialize)]
                #[serde(rename_all = "lowercase")]
                enum Kind {
                    Function,
                }
                struct Version;
                impl<'de> Deserialize<'de> for Version {
                    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
                        if u32::deserialize(deserializer)? == 1 { Ok(Self) } else { Err(de::Error::custom("unsupported curve function version")) }
                    }
                }
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Wire {
                    #[serde(rename = "kind")]
                    _kind: Kind,
                    #[serde(rename = "version")]
                    _version: Version,
                    base: CurveFunction,
                    #[serde(default)]
                    adjustments: BoundedPoints,
                }
                let Wire { base, adjustments, .. } = Wire::deserialize(de::value::MapAccessDeserializer::new(map))?;
                PointCurve::from_function(base, adjustments.0).map_err(de::Error::custom)
            }
        }
        deserializer.deserialize_any(Curve)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn a() -> PointCurve {
        vec![Point::new(0.0, 0.0), Point::new(0.27, 0.1), Point::new(0.68, 0.85), Point::new(1.0, 1.0)].into()
    }
    fn b() -> PointCurve {
        vec![Point::new(0.1, 0.2), Point::new(0.51, 0.74), Point::new(0.9, 0.95)].into()
    }

    #[test]
    fn sparse_wire_and_endpoint_models_preserve_legacy_exactly() {
        let points = vec![Point::new(1.0, 0.95), Point::new(0.0, 0.05), Point::new(0.4, 0.6)];
        let curve = PointCurve::try_from_points(points.clone()).unwrap();
        assert_eq!(serde_json::to_value(&curve).unwrap(), serde_json::to_value(&points).unwrap());
        assert_eq!(curve.render_lut(), spline(&points).to_lut(POINT_CURVE_LUT_SIZE));
        assert_eq!(a().blend(&b(), 0.0).unwrap(), a());
        assert_eq!(a().blend(&b(), 1.0).unwrap(), b());
        let empty = PointCurve::default();
        assert_eq!(serde_json::to_value(&empty).unwrap(), json!([]));
        assert_eq!(empty.editable_points(), vec![Point::new(0.0, 0.0), Point::new(1.0, 1.0)]);
    }

    #[test]
    fn function_blending_is_continuous_at_half_and_at_both_endpoints() {
        let (a, b) = (a(), b());
        for t in [0.000001, 0.499999, 0.5, 0.500001, 0.9999, 1.0001, 2.0] {
            let mixed = a.blend(&b, t).unwrap();
            assert!(mixed.is_function());
            let lut = mixed.render_lut();
            for i in 0..POINT_CURVE_LUT_SIZE {
                let x = i as f64 / (POINT_CURVE_LUT_SIZE - 1) as f64;
                let expected = a.eval(x).unwrap() + t * (b.eval(x).unwrap() - a.eval(x).unwrap());
                assert!((mixed.eval(x).unwrap() - expected).abs() < 2e-12);
                assert!((lut.v[i] as f64 - expected).abs() < 2e-7);
            }
        }
        let left = a.blend(&b, 0.9999).unwrap().render_lut();
        assert!(left.v.iter().zip(b.render_lut().v).all(|(a, b)| (*a - b).abs() < 0.0001));
    }

    #[test]
    fn signed_manual_adjustments_preserve_base_and_zero_is_not_identity() {
        let mut c = a().blend(&b(), 0.37).unwrap();
        let before = c.render_lut();
        let base = serde_json::to_value(&c).unwrap()["base"].clone();
        let y = c.eval(0.4).unwrap();
        let index = c.try_add_point(Point::new(0.4, y)).unwrap();
        assert_eq!(index, 1);
        assert_eq!(c.render_lut(), before);
        assert_eq!(c.editable_points().len(), 3);
        assert_eq!(serde_json::to_value(&c).unwrap()["base"], base);
        c.try_move_point(index, Point::new(0.42, y - 0.1)).unwrap();
        assert!((c.eval(0.42).unwrap() - (y - 0.1)).abs() < 1e-12);
        assert_eq!(serde_json::to_value(&c).unwrap()["base"], base);
        assert_ne!(c.render_lut(), before);
        let wire = serde_json::to_string(&c).unwrap();
        let copy: PointCurve = serde_json::from_str(&wire).unwrap();
        assert_eq!(copy, c);
        assert_eq!(copy.render_lut(), c.render_lut());
        assert_eq!(serde_json::to_value(&copy).unwrap().as_object().unwrap().len(), 4);
        c.try_remove_point(1).unwrap();
        assert_eq!(c.render_lut(), before);
        c.reset();
        assert!(c.is_identity());
        assert!(c.is_sparse());
        assert_eq!(serde_json::to_value(c).unwrap(), json!([]));
    }

    #[test]
    fn sparse_identity_add_and_invalid_edits_are_atomic() {
        let mut c = PointCurve::default();
        assert_eq!(c.try_add_point(Point::new(0.3, 0.4)).unwrap(), 1);
        assert_eq!(c.editable_points().len(), 3);
        let before = c.clone();
        for (index, point) in
            [(usize::MAX, Point::new(0.3, 0.4)), (0, Point::new(0.1, 0.2)), (1, Point::new(1.0, 0.5)), (1, Point::new(0.5, f64::NAN))]
        {
            assert!(c.try_move_point(index, point).is_err());
            assert_eq!(c, before);
        }
        assert!(c.try_remove_point(usize::MAX).is_err());
        assert!(c.try_remove_point(0).is_err());
        assert_eq!(c, before);
    }

    #[test]
    fn bounded_function_serde_rejects_invalid_versions_coordinates_and_payloads() {
        let c = a().blend(&b(), 0.37).unwrap();
        let original = serde_json::to_value(&c).unwrap();
        for (key, value) in
            [("version", json!(2)), ("kind", json!("future")), ("adjustments", json!([{"x":0.4,"y":0.1}])), ("base", json!({"pieces":[]}))]
        {
            let mut wire = original.clone();
            wire[key] = value;
            assert!(serde_json::from_value::<PointCurve>(wire).is_err());
        }
        let wire = serde_json::to_string(&c).unwrap();
        let duplicate = wire.replacen("\"version\":1", "\"version\":999,\"version\":1", 1);
        assert!(serde_json::from_str::<PointCurve>(&duplicate).is_err());
        let mut unknown = original.clone();
        unknown["cache"] = json!([]);
        assert!(serde_json::from_value::<PointCurve>(unknown).is_err());
        let points = (0..=MAX_POINT_CURVE_POINTS).map(|i| Point::new(i as f64 / MAX_POINT_CURVE_POINTS as f64, 0.0)).collect::<Vec<_>>();
        assert!(PointCurve::try_from_points(points.clone()).is_err());
        assert!(serde_json::from_str::<PointCurve>(&serde_json::to_string(&points).unwrap()).is_err());
        let mut too_many = original;
        too_many["adjustments"] = serde_json::to_value(points).unwrap();
        assert!(serde_json::from_value::<PointCurve>(too_many).is_err());
        assert!(PointCurve::try_from_points(vec![Point::new(0.0, f64::MAX), Point::new(1.0, f64::MAX)]).is_err());
        assert!(c.affine(f64::MAX, 0.0).is_err());
    }

    #[test]
    fn repeated_function_blends_stay_flat_and_keep_breakpoints_bounded() {
        let b = b();
        let mut c = a().blend(&b, 0.3).unwrap();
        let count = c.effective_function().unwrap().pieces().len();
        for _ in 0..100 {
            c = c.blend(&b, 0.2).unwrap();
            assert_eq!(c.effective_function().unwrap().pieces().len(), count);
            assert_eq!(c.editable_points().len(), 2);
        }
    }
    #[test]
    fn legacy_signed_sparse_blends_fail_without_changing_exact_endpoints() {
        let signed = PointCurve::try_from_points(vec![Point::new(0.0, -1.0), Point::new(0.5, 0.5), Point::new(1.0, 1.0)]).unwrap();
        let identity = PointCurve::default();
        for amount in [0.000001, 0.5, 0.999999, 1.5] {
            assert_eq!(signed.blend(&identity, amount), Err(PointCurveError::LegacyOutputRange));
            assert_eq!(identity.blend(&signed, amount), Err(PointCurveError::LegacyOutputRange));
        }
        assert_eq!(signed.blend(&identity, 0.0).unwrap(), signed);
        assert_eq!(signed.blend(&identity, 1.0).unwrap(), identity);
        assert_eq!(signed.blend(&signed, 0.5).unwrap(), signed);
        let back: PointCurve = serde_json::from_str(&serde_json::to_string(&signed).unwrap()).unwrap();
        assert_eq!(back.render_lut().v, signed.render_lut().v, "existing signed sparse edits still load and render");
    }
}
