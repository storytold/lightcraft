//! Crystal-layer film grain, backed by `craft_develop::crystal`.

use std::sync::Arc;

use lightcraft_develop::Grain;

pub use craft_develop::crystal::{CrystalLook, CrystalTile, FILLING, PITCH, REF_LONG, TILE_PER_LONG, TORI, lift, strength_levels};

/// The crystal grain of one render.
#[derive(Clone, Debug)]
pub struct CrystalGrain {
    pub tile: Arc<CrystalTile>,
    /// Encoded-luminance amplitude at unit field and unit tone weight.
    pub k: f32,
    /// An output pixel's side in tile pixels.
    pub foot: f32,
}

impl CrystalGrain {
    fn from_craft(g: craft_develop::crystal::CrystalGrain) -> CrystalGrain {
        CrystalGrain { tile: g.tile, k: g.k, foot: g.foot }
    }

    fn as_craft(&self) -> craft_develop::crystal::CrystalGrain {
        craft_develop::crystal::CrystalGrain { tile: self.tile.clone(), k: self.k, foot: self.foot }
    }

    pub fn new(g: &Grain, px_per_long: f64) -> Option<CrystalGrain> {
        craft_develop::crystal::CrystalGrain::new(&crate::bridge::to_craft_grain(g), px_per_long).map(Self::from_craft)
    }

    pub fn draft(g: &Grain, px_per_long: f64) -> Option<CrystalGrain> {
        craft_develop::crystal::CrystalGrain::draft(&crate::bridge::to_craft_grain(g), px_per_long).map(Self::from_craft)
    }

    #[inline]
    pub fn apply(&self, e: [f32; 3], gx: f32, gy: f32) -> [f32; 3] {
        self.as_craft().apply(e, gx, gy)
    }

    pub fn row(&self, gx: f64, gy: f64, dx: f64, dy: f64, out: &mut [f32]) -> bool {
        self.as_craft().row(gx, gy, dx, dy, out)
    }

    #[inline]
    pub fn apply_field(&self, e: [f32; 3], g: f32) -> [f32; 3] {
        self.as_craft().apply_field(e, g)
    }
}
