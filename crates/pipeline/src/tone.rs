//! The global tone map: scene luminance → display-linear luminance, backed by `craft_develop::tone`.

pub use craft_develop::tone::{CHROMA_N, CameraTone, GENERIC_KNEE_STOPS, GREY, LUT_MAX_EV, LUT_MIN_EV, LUT_N, ROLLOFF_STOPS, ToneMap};

/// The default raw tone curve ([`ToneMap::new`] at neutral settings) as camera-curve knots, about 1/3 EV apart
/// from 8 EV below grey to just under display white: for a camera look that knows a photo's colour but not its tone.
pub fn default_raw_camera_tone() -> Option<CameraTone> {
    const FROM_EV: f32 = -8.0;
    const TO_EV: f32 = 2.6;
    let map = ToneMap::new(0.0, 0.0, 0.0);
    CameraTone::new(std::array::from_fn(|i| {
        let x = GREY * 2f32.powf(FROM_EV + (TO_EV - FROM_EV) * i as f32 / 31.0);
        [x, map.apply(x)]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_raw_camera_tone_follows_the_default_tone_curve() {
        let (camera, default) = (default_raw_camera_tone().unwrap(), ToneMap::new(0.0, 0.0, 0.0));
        for ev in [-8.0f32, -6.0, -4.0, -2.0, -1.0, 0.0, 1.0, 2.0, 2.5] {
            let y = GREY * 2f32.powf(ev);
            let (a, b) = (camera.apply(y), default.apply(y));
            assert!((a - b).abs() <= 0.01 * b.max(0.01), "{ev} EV: {a} vs {b}");
        }
    }
}
