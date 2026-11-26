//! DNG colour model (Adobe DNG Specification 1.7, chapter 6), backed by `craft_develop::sensor::color`.

use lightcraft_color::{Mat3, Xy};
use serde::{Deserialize, Serialize};

use crate::{ColorData, RawImage};

pub use craft_develop::sensor::color::illuminant_temperature;

pub fn illuminant_weight(color: &ColorData, white: Xy) -> f64 {
    craft_develop::sensor::color::illuminant_weight(&crate::bridge::to_craft_color_data(color), crate::bridge::to_craft_xy(white))
}

pub fn has_matrix(color: &ColorData) -> bool {
    craft_develop::sensor::color::has_matrix(&crate::bridge::to_craft_color_data(color))
}

pub fn xyz_to_camera(color: &ColorData, white: Xy) -> Mat3 {
    crate::bridge::from_craft_mat3(craft_develop::sensor::color::xyz_to_camera(
        &crate::bridge::to_craft_color_data(color),
        crate::bridge::to_craft_xy(white),
    ))
}

pub fn camera_neutral(color: &ColorData, white: Xy) -> [f64; 3] {
    craft_develop::sensor::color::camera_neutral(&crate::bridge::to_craft_color_data(color), crate::bridge::to_craft_xy(white))
}

pub fn wb_multipliers(color: &ColorData, white: Xy) -> [f64; 3] {
    craft_develop::sensor::color::wb_multipliers(&crate::bridge::to_craft_color_data(color), crate::bridge::to_craft_xy(white))
}

pub fn neutral_to_xy(color: &ColorData, neutral: [f64; 3]) -> Xy {
    crate::bridge::from_craft_xy(craft_develop::sensor::color::neutral_to_xy(&crate::bridge::to_craft_color_data(color), neutral))
}

pub fn as_shot_white_xy(raw: &RawImage) -> Xy {
    as_shot_white_of(&raw.color, raw.wb_multipliers)
}

pub fn as_shot_white_xy_of(info: &crate::RawInfo) -> Xy {
    as_shot_white_of(&info.color, info.wb_multipliers)
}

pub fn as_shot_white_of(c: &ColorData, wb_multipliers: Option<[f32; 3]>) -> Xy {
    crate::bridge::from_craft_xy(craft_develop::sensor::color::as_shot_white_of(&crate::bridge::to_craft_color_data(c), wb_multipliers))
}

pub fn camera_to_xyz_d50(color: &ColorData, white: Xy) -> Mat3 {
    crate::bridge::from_craft_mat3(craft_develop::sensor::color::camera_to_xyz_d50(
        &crate::bridge::to_craft_color_data(color),
        crate::bridge::to_craft_xy(white),
    ))
}

/// Everything the pipeline needs to go from raw camera RGB to the working space.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraTransform {
    /// White-balanced camera RGB → linear Rec.2020 D65; maps (1, 1, 1) to (1, 1, 1).
    pub matrix: Mat3,
    /// Per-channel white-balance multipliers applied before `matrix` (minimum 1).
    pub wb: [f32; 3],
    pub white_xy: Xy,
    /// No colour matrix was available: a generic camera ≈ sRGB model was used.
    pub matrix_is_fallback: bool,
    pub baseline_exposure: f64,
}

impl From<craft_develop::sensor::color::CameraTransform> for CameraTransform {
    fn from(t: craft_develop::sensor::color::CameraTransform) -> Self {
        Self {
            matrix: crate::bridge::from_craft_mat3(t.matrix),
            wb: t.wb,
            white_xy: crate::bridge::from_craft_xy(t.white_xy),
            matrix_is_fallback: t.matrix_is_fallback,
            baseline_exposure: t.baseline_exposure,
        }
    }
}

pub fn camera_transform(raw: &RawImage, wb_xy: Xy) -> CameraTransform {
    camera_transform_of(&raw.color, wb_xy)
}

pub fn camera_transform_of(color: &ColorData, wb_xy: Xy) -> CameraTransform {
    craft_develop::sensor::color::camera_transform_of(&crate::bridge::to_craft_color_data(color), crate::bridge::to_craft_xy(wb_xy)).into()
}

pub fn rebalance(color: &ColorData, from: Xy, to: Xy) -> Option<Mat3> {
    craft_develop::sensor::color::rebalance(
        &crate::bridge::to_craft_color_data(color),
        crate::bridge::to_craft_xy(from),
        crate::bridge::to_craft_xy(to),
    )
    .map(crate::bridge::from_craft_mat3)
}

pub fn neutral_white(color: &ColorData, developed_for: Xy, rgb: [f64; 3]) -> Option<Xy> {
    craft_develop::sensor::color::neutral_white(&crate::bridge::to_craft_color_data(color), crate::bridge::to_craft_xy(developed_for), rgb)
        .map(crate::bridge::from_craft_xy)
}

pub fn camera_to_rec2020(raw: &RawImage, wb_xy: Xy) -> (Mat3, [f32; 3]) {
    let (m, wb) = craft_develop::sensor::color::camera_to_rec2020(&crate::bridge::to_craft_raw_header(raw), crate::bridge::to_craft_xy(wb_xy));
    (crate::bridge::from_craft_mat3(m), wb)
}

pub fn grey_world(img: &lightcraft_raster::Rgb32f) -> [f32; 3] {
    craft_develop::sensor::color::grey_world(&crate::bridge::to_craft_rgb32f(img))
}
