//! Colour tools in OkLCh on display-linear Rec.2020 values, backed by `craft_develop::colorops`.

use std::ops::Deref;

use lightcraft_develop::{Calibration, DevelopSettings, Process};

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
        let mut ops = craft_develop::colorops::ColorOps::new(&crate::bridge::to_craft_settings(s));
        if let Some((wheels, _, _)) = ops.grading.as_mut() {
            wheels[3] = global_wheel(wheels[3], s.process.process());
        }
        ColorOps(ops)
    }
}

/// How much of V6's tint the global colour-grading wheel keeps under V13.
pub const GLOBAL_WHEEL_V13: f32 = 0.45;

/// The global wheel `k`: from V13 its tint is [`GLOBAL_WHEEL_V13`] of the other wheels' at the same saturation. On
/// Lightroom exports of real edits with Global saturation 10 (three photos) and 16 (one), V6 left the photos about
/// +2 b* and up to +2.8 a* warmer than the exports, and 0.4 to 0.6 of the tint cancels it; the shadows and
/// highlights wheels measured right as they are (Highlights 23 got further from the exports when halved). Its
/// luminance is unchanged.
fn global_wheel(k: WheelK, process: Process) -> WheelK {
    if process >= Process::V13 { WheelK { a: k.a * GLOBAL_WHEEL_V13, b: k.b * GLOBAL_WHEEL_V13, ..k } } else { k }
}

/// The calibration panel's primaries as a white-preserving 3×3 matrix on linear Rec.2020.
pub fn calibration_matrix(c: &Calibration) -> Option<[[f32; 3]; 3]> {
    craft_develop::colorops::calibration_matrix(&crate::bridge::to_craft_calibration(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v13_global_wheel_tints_less_and_earlier_processes_keep_it() {
        let tint = |p: Process, global: bool| {
            let mut s = DevelopSettings { process: p.version(), ..Default::default() };
            let w = lightcraft_develop::Wheel { hue: 35.0, sat: 16.0, lum: 10.0 };
            if global {
                s.grading.global = w
            } else {
                s.grading.shadows = w
            }
            let ops = ColorOps::new(&s);
            let (wheels, _, _) = ops.grading.expect("grading");
            let k = wheels[if global { 3 } else { 0 }];
            (k.a, k.b, k.lum)
        };
        let v10 = tint(Process::V10, true);
        for p in Process::ALL.into_iter().filter(|p| *p < Process::V13) {
            assert_eq!(tint(p, true), v10, "{p:?} tints as before");
        }
        let v13 = tint(Process::V13, true);
        assert!(v10.0 != 0.0 && v10.1 != 0.0);
        assert!(v13.0.abs() < v10.0.abs() && v13.1.abs() < v10.1.abs(), "V13's Global wheel tints less: {v13:?} {v10:?}");
        assert_eq!((v13.0.signum(), v13.1.signum(), v13.2), (v10.0.signum(), v10.1.signum(), v10.2), "same hue, same luminance");
        assert_eq!(tint(Process::V10, false), tint(Process::V13, false), "the shadows wheel is unchanged");
    }
}
