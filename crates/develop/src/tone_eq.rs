//! Tone Equalizer settings (beyond Lightroom): exposure per luminance band, from 8 EV below middle grey
//! up to middle grey's own band (0 EV), applied through a smoothed luminance mask so detail inside a
//! band keeps its local contrast. The pipeline side is `lcc_pipeline::tone_eq`.
//!
//! The model (nine bands one EV apart, a Gaussian radial-basis curve through them, a guided-filter mask)
//! is translated from darktable's `src/iop/toneequal.c` (GPL-3.0-or-later, Aurélien Pierre and the
//! darktable developers); the parameter names and ranges here are our own.

use serde::{Deserialize, Serialize};

/// Number of luminance bands.
pub const BANDS: usize = 9;
/// The darkest band's centre, in EV relative to middle grey; the others follow one EV apart up to 0.
pub const FIRST_BAND_EV: f64 = -8.0;
/// Largest exposure change one band can ask for, in EV.
pub const MAX_BAND_EV: f64 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToneEq {
    /// Exposure change (EV, −2..+2) of the band 8 EV below middle grey.
    pub ev8: f64,
    pub ev7: f64,
    pub ev6: f64,
    pub ev5: f64,
    pub ev4: f64,
    pub ev3: f64,
    pub ev2: f64,
    pub ev1: f64,
    /// Exposure change of middle grey's band.
    pub ev0: f64,
    /// −1..+1: how smoothly the curve passes from one band to the next (0: darktable's default).
    pub smoothing: f64,
    /// 0..100: how much the mask is smoothed (detail preserved inside a band); 0 is a plain per-pixel curve.
    pub detail: f64,
    /// 0..100: the smoothing's size, relative to the image's long edge (100: a tenth of it).
    pub size: f64,
    /// 0..100: how closely the mask follows edges (higher keeps the change from bleeding across them).
    pub edges: f64,
}

impl Default for ToneEq {
    fn default() -> Self {
        Self {
            ev8: 0.0,
            ev7: 0.0,
            ev6: 0.0,
            ev5: 0.0,
            ev4: 0.0,
            ev3: 0.0,
            ev2: 0.0,
            ev1: 0.0,
            ev0: 0.0,
            smoothing: 0.0,
            detail: 100.0,
            size: 50.0,
            edges: 25.0,
        }
    }
}

impl ToneEq {
    pub fn is_default(&self) -> bool {
        *self == ToneEq::default()
    }

    /// The band exposures, darkest first (each in −2..+2; anything else, NaN included, is brought into range).
    pub fn bands(&self) -> [f64; BANDS] {
        [self.ev8, self.ev7, self.ev6, self.ev5, self.ev4, self.ev3, self.ev2, self.ev1, self.ev0]
            .map(|v| if v.is_finite() { v.clamp(-MAX_BAND_EV, MAX_BAND_EV) } else { 0.0 })
    }

    /// Whether it changes the picture: some band is not at 0.
    pub fn active(&self) -> bool {
        self.bands().iter().any(|v| *v != 0.0)
    }

    /// Brought into range: every value finite and within its control's range, `-0` as `0`.
    pub fn sanitized(&self) -> ToneEq {
        let d = ToneEq::default();
        let f = |v: f64, lo: f64, hi: f64, def: f64| if v.is_finite() { v.clamp(lo, hi) + 0.0 } else { def };
        let [ev8, ev7, ev6, ev5, ev4, ev3, ev2, ev1, ev0] = self.bands().map(|v| v + 0.0);
        ToneEq {
            ev8,
            ev7,
            ev6,
            ev5,
            ev4,
            ev3,
            ev2,
            ev1,
            ev0,
            smoothing: f(self.smoothing, -1.0, 1.0, d.smoothing),
            detail: f(self.detail, 0.0, 100.0, d.detail),
            size: f(self.size, 0.0, 100.0, d.size),
            edges: f(self.edges, 0.0, 100.0, d.edges),
        }
    }
}
