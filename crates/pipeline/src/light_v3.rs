//! Process V4's Contrast and Blacks on the camera-look path, backed by `craft_develop::light_v3`.

use std::ops::Deref;

use lightcraft_develop::DevelopSettings;

use crate::SourceInfo;

pub use craft_develop::light_v3::{FITS, Fit, LUT_N, Mode, Operator, PRO_Y, Unpacked, law, prophoto_matrices};

/// The active operators of a render, in order (Contrast, Blacks), and the conversions to
/// and from the space they work in.
#[derive(Clone, Debug)]
pub struct LightOps(pub craft_develop::light_v3::LightOps);

impl Deref for LightOps {
    type Target = craft_develop::light_v3::LightOps;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Whether `s` renders `info` with the V4 operators.
pub fn applies(s: &DevelopSettings, info: &SourceInfo) -> bool {
    craft_develop::light_v3::applies(&crate::bridge::to_craft_settings(s), info.raw, info.camera_tone.is_some())
}

impl LightOps {
    pub fn new(s: &DevelopSettings, info: &SourceInfo) -> Option<LightOps> {
        craft_develop::light_v3::LightOps::new(&crate::bridge::to_craft_settings(s), info.raw, info.camera_tone.is_some()).map(LightOps)
    }
}

/// CPU preview tables for per-channel operations, built once per render.
pub(crate) struct PreviewOps(craft_develop::light_v3::PreviewOps);

impl Deref for PreviewOps {
    type Target = craft_develop::light_v3::PreviewOps;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PreviewOps {
    pub(crate) fn new(exact: &LightOps, encode: &impl Fn(f32) -> f32) -> Option<Self> {
        craft_develop::light_v3::PreviewOps::new(&exact.0, encode).map(PreviewOps)
    }
}
