//! Colour tools in OkLCh on display-linear Rec.2020 values, backed by `craft_develop::colorops`.

use std::ops::Deref;

use lightcraft_develop::{Calibration, DevelopSettings};

pub use craft_develop::colorops::{
    CALIB_HUE, CALIB_SAT, POINT_WORDS, PointK, SHADOW_TINT, WheelK, band_hues, band_weights, calibrate, oklch_hue_of_srgb_hue,
};

/// The colour tools' parameters, resolved once per render (fields are read by the GPU kernel).
#[derive(Clone, Debug)]
pub struct ColorOps(pub craft_develop::colorops::ColorOps);

impl Deref for ColorOps {
    type Target = craft_develop::colorops::ColorOps;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ColorOps {
    pub fn new(s: &DevelopSettings) -> ColorOps {
        ColorOps(craft_develop::colorops::ColorOps::new(&crate::bridge::to_craft_settings(s)))
    }
}

/// The calibration panel's primaries as a white-preserving 3×3 matrix on linear Rec.2020.
pub fn calibration_matrix(c: &Calibration) -> Option<[[f32; 3]; 3]> {
    craft_develop::colorops::calibration_matrix(&crate::bridge::to_craft_calibration(c))
}
