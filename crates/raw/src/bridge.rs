//! Adapters between `lightcraft_*` types and `craft_*` types for delegating to `craft_develop::sensor`.

use craft_color_transform::{Mat3 as CraftMat3, Xy as CraftXy};
use craft_develop::raster::Rgb32f as CraftRgb32f;
use lightcraft_color::{Mat3, Xy};
use lightcraft_raster::Rgb32f;

use crate::{ColorData, RawData, RawImage};

pub fn to_craft_xy(xy: Xy) -> CraftXy {
    CraftXy { x: xy.x, y: xy.y }
}

pub fn from_craft_xy(xy: CraftXy) -> Xy {
    Xy { x: xy.x, y: xy.y }
}

pub fn from_craft_mat3(m: CraftMat3) -> Mat3 {
    Mat3(m.0)
}

pub fn to_craft_rgb32f(img: &Rgb32f) -> CraftRgb32f {
    CraftRgb32f { width: img.width, height: img.height, data: img.data.clone() }
}

pub fn from_craft_rgb32f(craft: CraftRgb32f) -> Rgb32f {
    Rgb32f { width: craft.width, height: craft.height, data: craft.data }
}

pub fn take_craft_rgb32f(img: &mut Rgb32f) -> CraftRgb32f {
    CraftRgb32f { width: img.width, height: img.height, data: std::mem::take(&mut img.data) }
}

pub fn put_craft_rgb32f(img: &mut Rgb32f, craft: CraftRgb32f) {
    img.width = craft.width;
    img.height = craft.height;
    img.data = craft.data;
}

pub fn to_craft_color_data(c: &ColorData) -> craft_raw::ColorData {
    craft_raw::ColorData {
        illuminant: c.illuminant,
        color_matrix: c.color_matrix.map(|m| m.map(|m| m.0)),
        forward_matrix: c.forward_matrix.map(|m| m.map(|m| m.0)),
        camera_calibration: c.camera_calibration.map(|m| m.map(|m| m.0)),
        analog_balance: c.analog_balance,
        as_shot_neutral: c.as_shot_neutral,
        as_shot_white_xy: c.as_shot_white_xy.map(|xy| [xy.x, xy.y]),
        baseline_exposure: c.baseline_exposure,
        baseline_sharpness: c.baseline_sharpness,
        profile: Default::default(),
        kelvin_presets: None,
    }
}

pub fn to_craft_raw_header(raw: &RawImage) -> craft_raw::RawImage {
    craft_raw::RawImage {
        format: raw.format,
        width: raw.width,
        height: raw.height,
        cpp: raw.cpp,
        data: RawData::U16(Vec::new()),
        cfa: raw.cfa.clone(),
        bits: raw.bits,
        black: raw.black.clone(),
        white: raw.white.clone(),
        active_area: raw.active_area,
        crop: raw.crop,
        orientation: craft_raw::Orientation::Normal,
        color: to_craft_color_data(&raw.color),
        wb_multipliers: raw.wb_multipliers,
        linearized: raw.linearized,
        pixel_aspect: raw.pixel_aspect,
        opcodes: raw.opcodes.clone(),
        lens_metadata: raw.lens_metadata.clone(),
        cinema: Default::default(),
        camera: Default::default(),
        linearization_table: None,
        level_sources: Default::default(),
        notices: Default::default(),
    }
}
