//! Highlight recovery, backed by `craft_develop::sensor::highlight`.

use crate::Rgb32f;

pub use craft_develop::sensor::highlight::{CLIPPED_B, CLIPPED_G, CLIPPED_R};

pub fn clip_neutral(img: &mut Rgb32f, wb: [f32; 3], clip: f32) {
    let mut craft = crate::bridge::take_craft_rgb32f(img);
    craft_develop::sensor::highlight::clip_neutral(&mut craft, wb, clip);
    crate::bridge::put_craft_rgb32f(img, craft);
}

pub fn reconstruct(img: &mut Rgb32f, wb: [f32; 3], clip: f32) -> usize {
    let mut craft = crate::bridge::take_craft_rgb32f(img);
    let n = craft_develop::sensor::highlight::reconstruct(&mut craft, wb, clip);
    crate::bridge::put_craft_rgb32f(img, craft);
    n
}

pub fn reconstruct_with(img: &mut Rgb32f, wb: [f32; 3], clip: f32, white: [f32; 3]) -> usize {
    let mut craft = crate::bridge::take_craft_rgb32f(img);
    let n = craft_develop::sensor::highlight::reconstruct_with(&mut craft, wb, clip, white);
    crate::bridge::put_craft_rgb32f(img, craft);
    n
}

pub fn reconstruct_masked(img: &mut Rgb32f, wb: [f32; 3], clip: f32, white: [f32; 3], mask: &[u8]) -> usize {
    let mut craft = crate::bridge::take_craft_rgb32f(img);
    let n = craft_develop::sensor::highlight::reconstruct_masked(&mut craft, wb, clip, white, mask);
    crate::bridge::put_craft_rgb32f(img, craft);
    n
}
