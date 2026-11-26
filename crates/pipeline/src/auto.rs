//! Auto tone and auto white balance, backed by `craft_develop::auto`.

use lightcraft_develop::DevelopSettings;
use lightcraft_raster::Rgb32f;

use crate::SourceInfo;

pub use craft_develop::auto::AutoTone;

/// The scene's log-average luminance under the current white balance, in EV relative to middle grey.
pub fn scene_log_mean(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> Option<f64> {
    craft_develop::auto::scene_log_mean(
        &crate::bridge::to_craft_rgb32f(src),
        &crate::bridge::to_craft_wb_source(info),
        &crate::bridge::to_craft_settings(s),
    )
}

/// Auto tone settings for `src`.
pub fn auto_tone(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> AutoTone {
    craft_develop::auto::auto_tone(
        &crate::bridge::to_craft_rgb32f(src),
        &crate::bridge::to_craft_wb_source(info),
        &crate::bridge::to_craft_settings(s),
    )
}

/// Auto B&W mix for `src`.
pub fn auto_bw_mix(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> [f64; 8] {
    craft_develop::auto::auto_bw_mix(
        &crate::bridge::to_craft_rgb32f(src),
        &crate::bridge::to_craft_wb_source(info),
        &crate::bridge::to_craft_settings(s),
    )
}

/// Auto white balance `(temp, tint)` for `src`.
pub fn auto_wb(src: &Rgb32f, info: &SourceInfo) -> (f64, f64) {
    let craft_src = crate::bridge::to_craft_rgb32f(src);
    let wb_src = crate::bridge::to_craft_wb_source(info);
    let neutral = |rgb: [f64; 3]| {
        let cc = info.camera_color.as_deref()?;
        lightcraft_raw::color::neutral_white(&cc.tags, cc.developed_for, rgb).map(crate::bridge::to_craft_xy)
    };
    let cb: Option<&dyn Fn([f64; 3]) -> Option<craft_color_transform::Xy>> =
        (info.camera_color.is_some() && info.raw && !info.relative_wb).then_some(&neutral);
    craft_develop::auto::auto_wb_with(&craft_src, &wb_src, cb)
}

/// Current white balance `(temp, tint)` for `info` and `s`.
pub fn current_wb(info: &SourceInfo, s: &DevelopSettings) -> (f64, f64) {
    craft_develop::auto::current_wb(&crate::bridge::to_craft_wb_source(info), &crate::bridge::to_craft_settings(s))
}
