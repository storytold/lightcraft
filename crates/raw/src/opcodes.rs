//! DNG OpcodeList1/2/3 execution, backed by `craft_develop::sensor::opcodes`.

use crate::Rgb32f;

pub use craft_develop::sensor::opcodes::{apply_list, is_applied};
pub use craft_raw::opcodes::{Area, Opcode, OpcodeLists, WR2_LEN, applied_ops, parse_list, wr2_factor, write_list};

#[cfg(test)]
use crate::Cfa;
#[cfg(test)]
const MAX_LIST_PASSES: usize = 32;
#[cfg(test)]
const MAX_LIST3_PASSES: usize = 8;

#[cfg(test)]
pub(crate) fn max_radius(cx: f64, cy: f64, w: f64, h: f64) -> f64 {
    let corners = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)];
    corners.iter().map(|&(x, y)| (x - cx).hypot(y - cy)).fold(0.0f64, f64::max).max(1e-9)
}

pub fn apply_list3(list: &[Opcode], img: &mut Rgb32f) {
    let mut craft = crate::bridge::take_craft_rgb32f(img);
    craft_develop::sensor::opcodes::apply_list3(list, &mut craft);
    crate::bridge::put_craft_rgb32f(img, craft);
}

pub fn warp_rectilinear(img: &Rgb32f, planes: &[[f64; 6]], center: [f64; 2]) -> Rgb32f {
    crate::bridge::from_craft_rgb32f(craft_develop::sensor::opcodes::warp_rectilinear(&crate::bridge::to_craft_rgb32f(img), planes, center))
}

pub fn warp_rectilinear2(img: &Rgb32f, planes: &[[f64; WR2_LEN]], center: [f64; 2], reciprocal: bool) -> Rgb32f {
    crate::bridge::from_craft_rgb32f(craft_develop::sensor::opcodes::warp_rectilinear2(
        &crate::bridge::to_craft_rgb32f(img),
        planes,
        center,
        reciprocal,
    ))
}

#[cfg(test)]
pub(crate) mod tests;
