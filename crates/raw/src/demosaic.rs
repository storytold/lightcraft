//! Demosaicing: CFA mosaic → camera RGB, backed by `craft_develop::sensor::demosaic`.

use crate::{Cfa, Normalized, Rgb32f};

pub use craft_develop::sensor::demosaic::Method;

pub fn demosaic(n: &Normalized, method: Method) -> Rgb32f {
    crate::bridge::from_craft_rgb32f(craft_develop::sensor::demosaic::demosaic(n, method))
}

pub fn mosaic_from_rgb(img: &Rgb32f, cfa: &Cfa) -> Normalized {
    craft_develop::sensor::demosaic::mosaic_from_rgb(&crate::bridge::to_craft_rgb32f(img), cfa)
}

pub fn psnr(a: &Rgb32f, b: &Rgb32f, border: usize) -> f64 {
    craft_develop::sensor::demosaic::psnr(&crate::bridge::to_craft_rgb32f(a), &crate::bridge::to_craft_rgb32f(b), border)
}
