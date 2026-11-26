//! Linear colour NR with a fixed, denoised joint guide, backed by `craft_develop::nrc_joint`.

use lightcraft_raster::Rgb32f;

pub use craft_develop::nrc_joint::{
    JointNoise, NoiseSample, SpeckBrake, noise_from_samples, noise_sample_stride, scale_plan, speck_brake, strength, tolerance,
};

/// Tile windows covering `(full_w, full_h)` for [`noise_of_tiles`].
pub fn tiles(full_w: usize, full_h: usize) -> Vec<crate::PixelWindow> {
    craft_develop::nrc_joint::tiles(full_w, full_h).into_iter().map(|w| crate::PixelWindow { x: w.x, y: w.y, w: w.w, h: w.h }).collect()
}

/// Measure joint noise from pre-extracted pixel windows.
pub fn noise_of_tiles(tiles: &[Rgb32f]) -> JointNoise {
    let craft_tiles: Vec<_> = tiles.iter().map(crate::bridge::to_craft_rgb32f).collect();
    craft_develop::nrc_joint::noise_of_tiles(&craft_tiles)
}
