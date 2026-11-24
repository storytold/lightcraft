//! The parametric tone curve model shared by the pipeline renderer and the UI curve editor,
//! backed by `craft_develop::curve`.

use std::ops::Deref;

use lightcraft_color::spline::Lut1;
use lightcraft_develop::ToneCurve;

pub use craft_develop::curve::{PARAM_AMOUNTS, PARAM_LUT};

/// The parametric (region) curve of a [`ToneCurve`], backed by `craft_develop::curve::ParametricCurve`.
#[derive(Clone, Debug)]
pub struct ParametricCurve(craft_develop::curve::ParametricCurve);

impl Deref for ParametricCurve {
    type Target = craft_develop::curve::ParametricCurve;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ParametricCurve {
    pub fn new(c: &ToneCurve) -> ParametricCurve {
        ParametricCurve(craft_develop::curve::ParametricCurve::new(&crate::bridge::to_craft_tone_curve(c)))
    }

    /// Convert the evaluated parametric curve into a 1024-entry [`Lut1`].
    pub fn to_lut(&self) -> Lut1 {
        Lut1 { v: self.0.to_lut().v }
    }
}
