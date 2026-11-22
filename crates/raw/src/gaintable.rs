//! DNG `ProfileGainTableMap` (tag 52525, DNG 1.6) and `ProfileGainTableMap2` (tag 52544, DNG 1.7),
//! backed by `craft_develop::sensor::gaintable`.

use serde::{Deserialize, Serialize};

pub use craft_develop::sensor::gaintable::{GainEval, GainTableMapApply};
pub use craft_raw::gaintable::GainTableMap;

/// Where the developed picture (the default crop, oriented) sits in the active area.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourcePlacement {
    /// x, y, width, height of the developed crop, relative to the active area.
    pub rect: [f64; 4],
    /// The EXIF orientation applied to the developed crop.
    pub orientation: crate::Orientation,
}

impl SourcePlacement {
    /// Relative active-area position of normalized (0..1) coordinates of the oriented source.
    pub fn active(&self, u: f64, v: f64) -> (f64, f64) {
        let (u, v) = self.orientation.inverse().map(u, v, 1.0, 1.0);
        let [x, y, w, h] = self.rect;
        (x + u * w, y + v * h)
    }
}
