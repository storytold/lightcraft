//! Levels (beyond Lightroom, as Capture One's tool of that name): per channel input black and white points, a
//! signed midtone and output levels, applied to the encoded display values.
//!
//! **Where it runs:** in [`crate::finish`], once, right after the encode and before the Tone Curve: after the gamut
//! map (it needs the final in-gamut display values, and the numbers, the histogram and the clip points mean display
//! values), after Exposure (which works in linear light, upstream), before the Tone Curve. The order was measured
//! from Capture One with a synthetic chart. The single call site is `finish_with` (and the kernel's, in
//! `finish.wgsl`).
//!
//! Each colour channel's own levels apply first, then the master's to all three channels. One stage:
//!
//! ```text
//! t = (x - black) / (white - black)       // not limited: the stretch runs on beyond 0 and 1
//! g = midtone curve(t)                    // 0 and 1 stay fixed; beyond them the curve's end slopes continue
//! y = out_black + (out_white - out_black) * g
//! y = max(y, 0)                           // and min(y, 1) in an SDR render
//! ```
//!
//! The midtone curve is [`lcc_develop::levels_curve`] (not a gamma; finite end slopes). Black above white inverts the
//! channel, and so does output black above output white.
//!
//! **Beyond SDR white (HDR renders)** one rule serves the whole range: the stage is the continuous, piecewise
//! function above, so a value above 1 keeps going with the slope the stage has there (the end slope of the curve, scaled
//! by the stretch and the output span: rising for a normal stage, falling for an inverted one) and is limited below at 0
//! like everything else, but *not* above at 1. The stage is monotone (non-decreasing, or non-increasing when it
//! inverts), continuous, and has a continuous slope where the curve ends. An SDR render limits at 1.

use std::sync::Arc;

use lcc_develop::levels_curve::MidtoneTable;
use lcc_develop::{ChannelLevels, Levels};

/// One stage on values in 0..1 (the 0..255 scale divided by 255).
#[derive(Clone, Debug, PartialEq)]
pub struct StageK {
    pub black: f32,
    /// `1 / (white - black)`, negative for an inverted range.
    pub inv_range: f32,
    pub out_black: f32,
    /// `out_white - out_black` (negative for inverted output levels).
    pub span: f32,
    /// The midtone curve; `None` for the neutral midtone (the curve is then the identity, with slope 1 beyond).
    pub mid: Option<Arc<MidtoneTable>>,
}

impl StageK {
    /// The stage for `c` (brought into range first), `None` while it changes nothing.
    pub fn new(c: &ChannelLevels) -> Option<StageK> {
        if c.is_default() {
            return None;
        }
        let c = c.sanitized();
        Some(StageK {
            black: (c.black / 255.0) as f32,
            inv_range: (255.0 / (c.white - c.black)) as f32,
            out_black: (c.out_black / 255.0) as f32,
            span: ((c.out_white - c.out_black) / 255.0) as f32,
            mid: (c.midtone != 0.0).then(|| Arc::new(MidtoneTable::new(c.midtone))),
        })
    }

    /// The stage on `x`; `hdr` lets values above 1 through.
    #[inline]
    pub fn eval(&self, x: f32, hdr: bool) -> f32 {
        let t = (x - self.black) * self.inv_range;
        let g = match &self.mid {
            Some(m) => m.eval(t),
            None => t,
        };
        // (max first: a NaN comes out as 0)
        let y = (self.out_black + self.span * g).max(0.0);
        if hdr { y } else { y.min(1.0) }
    }
}

/// The Levels of a render: the master stage and the three channels' own, each `None` while neutral.
#[derive(Clone, Debug, PartialEq)]
pub struct LevelsK {
    pub master: Option<StageK>,
    /// Red, green, blue.
    pub channels: [Option<StageK>; 3],
}

impl LevelsK {
    /// `None` while every channel is neutral: then the stage does not exist (nothing runs, no parameters are sent
    /// to the GPU, and the render is the one it was without Levels).
    pub fn new(l: &Levels) -> Option<LevelsK> {
        let k = LevelsK { master: StageK::new(&l.master), channels: [StageK::new(&l.red), StageK::new(&l.green), StageK::new(&l.blue)] };
        (k.master.is_some() || k.channels.iter().any(Option::is_some)).then_some(k)
    }

    /// The encoded colour `e` through each channel's own stage, then the master stage.
    #[inline]
    pub fn apply(&self, e: [f32; 3], hdr: bool) -> [f32; 3] {
        std::array::from_fn(|i| {
            let mut v = e.get(i).copied().unwrap_or(0.0);
            if let Some(Some(c)) = self.channels.get(i) {
                v = c.eval(v, hdr);
            }
            if let Some(m) = &self.master {
                v = m.eval(v, hdr);
            }
            v
        })
    }
}

#[cfg(test)]
#[path = "tests_levels.rs"]
mod tests;
