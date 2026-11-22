//! Adapters between `lightcraft_*` types and `craft_*` types for delegating to `craft_develop`.

use craft_color_transform::{Mat3 as CraftMat3, Xy as CraftXy};
use craft_develop::local::{Quality as CraftQuality, WbSource};
use craft_develop::raster::{Plane as CraftPlane, Rgb32f as CraftRgb32f};
use craft_develop_model::{
    BwMix, Calibration, ColorAdj, ColorGrading, Detail, DevelopSettings, Effects, Grain, Hsl, Light, LocalAdjustments, Mask, MaskComponent, MaskOp,
    MaskShape, Mixer, PointColor, ProcessVersion, Profile, ToneCurve, Treatment, WbMode, WbScale, Wheel, WhiteBalance,
};
use craft_geom::Point as CraftPoint;
use lightcraft_color::{Mat3, Xy};
use lightcraft_develop as lc;
use lightcraft_raster::{Plane, Rgb32f};

use crate::{Quality, SourceInfo};

pub fn to_craft_xy(xy: Xy) -> CraftXy {
    CraftXy { x: xy.x, y: xy.y }
}

pub fn from_craft_xy(xy: CraftXy) -> Xy {
    Xy { x: xy.x, y: xy.y }
}

pub fn to_craft_mat3(m: Mat3) -> CraftMat3 {
    CraftMat3(m.0)
}

pub fn to_craft_quality(q: Quality) -> CraftQuality {
    match q {
        Quality::Full => CraftQuality::Full,
        Quality::Draft => CraftQuality::Draft,
    }
}

pub fn to_craft_wb_source(info: &SourceInfo) -> WbSource {
    WbSource {
        raw: info.raw,
        as_shot_temp: info.as_shot_temp,
        as_shot_tint: info.as_shot_tint,
        relative_wb: info.relative_wb,
        legacy_relative_wb: info.legacy_relative_wb,
    }
}

pub fn to_craft_rgb32f(img: &Rgb32f) -> CraftRgb32f {
    CraftRgb32f { width: img.width, height: img.height, data: img.data.clone() }
}

pub fn take_craft_rgb32f(img: &mut Rgb32f) -> CraftRgb32f {
    CraftRgb32f { width: img.width, height: img.height, data: std::mem::take(&mut img.data) }
}

pub fn put_craft_rgb32f(img: &mut Rgb32f, craft: CraftRgb32f) {
    img.width = craft.width;
    img.height = craft.height;
    img.data = craft.data;
}

pub fn to_craft_plane(p: &Plane) -> CraftPlane {
    CraftPlane { width: p.width, height: p.height, data: p.data.clone() }
}

pub fn from_craft_plane(p: CraftPlane) -> Plane {
    Plane { width: p.width, height: p.height, data: p.data }
}

pub fn to_craft_treatment(t: lc::Treatment) -> Treatment {
    match t {
        lc::Treatment::Color => Treatment::Color,
        lc::Treatment::Bw => Treatment::Bw,
    }
}

pub fn from_craft_treatment(t: Treatment) -> lc::Treatment {
    match t {
        Treatment::Color => lc::Treatment::Color,
        Treatment::Bw => lc::Treatment::Bw,
    }
}

pub fn to_craft_wb_mode(m: lc::WbMode) -> WbMode {
    match m {
        lc::WbMode::AsShot => WbMode::AsShot,
        lc::WbMode::Auto => WbMode::Auto,
        lc::WbMode::Daylight => WbMode::Daylight,
        lc::WbMode::Cloudy => WbMode::Cloudy,
        lc::WbMode::Shade => WbMode::Shade,
        lc::WbMode::Tungsten => WbMode::Tungsten,
        lc::WbMode::Fluorescent => WbMode::Fluorescent,
        lc::WbMode::Flash => WbMode::Flash,
        lc::WbMode::Custom => WbMode::Custom,
    }
}

pub fn from_craft_wb_mode(m: WbMode) -> lc::WbMode {
    match m {
        WbMode::AsShot => lc::WbMode::AsShot,
        WbMode::Auto => lc::WbMode::Auto,
        WbMode::Daylight => lc::WbMode::Daylight,
        WbMode::Cloudy => lc::WbMode::Cloudy,
        WbMode::Shade => lc::WbMode::Shade,
        WbMode::Tungsten => lc::WbMode::Tungsten,
        WbMode::Fluorescent => lc::WbMode::Fluorescent,
        WbMode::Flash => lc::WbMode::Flash,
        WbMode::Custom => lc::WbMode::Custom,
    }
}

pub fn to_craft_wb_scale(s: lc::WbScale) -> WbScale {
    match s {
        lc::WbScale::Legacy => WbScale::Legacy,
        lc::WbScale::Kelvin => WbScale::Kelvin,
    }
}

pub fn from_craft_wb_scale(s: WbScale) -> lc::WbScale {
    match s {
        WbScale::Legacy => lc::WbScale::Legacy,
        WbScale::Kelvin => lc::WbScale::Kelvin,
    }
}

fn to_craft_points(pts: &[lightcraft_geom::Point]) -> Vec<CraftPoint> {
    pts.iter().map(|p| CraftPoint::new(p.x, p.y)).collect()
}

fn from_craft_points(pts: &[CraftPoint]) -> Vec<lightcraft_geom::Point> {
    pts.iter().map(|p| lightcraft_geom::Point::new(p.x, p.y)).collect()
}

pub fn to_craft_tone_curve(c: &lc::ToneCurve) -> ToneCurve {
    ToneCurve {
        highlights: c.highlights,
        lights: c.lights,
        darks: c.darks,
        shadows: c.shadows,
        split_shadows: c.split_shadows,
        split_mid: c.split_mid,
        split_highlights: c.split_highlights,
        master: to_craft_points(&c.master),
        red: to_craft_points(&c.red),
        green: to_craft_points(&c.green),
        blue: to_craft_points(&c.blue),
        refine_saturation: c.refine_saturation,
    }
}

pub fn from_craft_tone_curve(c: &ToneCurve) -> lc::ToneCurve {
    lc::ToneCurve {
        highlights: c.highlights,
        lights: c.lights,
        darks: c.darks,
        shadows: c.shadows,
        split_shadows: c.split_shadows,
        split_mid: c.split_mid,
        split_highlights: c.split_highlights,
        master: from_craft_points(&c.master),
        red: from_craft_points(&c.red),
        green: from_craft_points(&c.green),
        blue: from_craft_points(&c.blue),
        refine_saturation: c.refine_saturation,
    }
}

fn to_craft_hsl(h: lc::Hsl) -> Hsl {
    Hsl { hue: h.hue, sat: h.sat, lum: h.lum }
}

fn from_craft_hsl(h: Hsl) -> lc::Hsl {
    lc::Hsl { hue: h.hue, sat: h.sat, lum: h.lum }
}

fn to_craft_wheel(w: lc::Wheel) -> Wheel {
    Wheel { hue: w.hue, sat: w.sat, lum: w.lum }
}

fn from_craft_wheel(w: Wheel) -> lc::Wheel {
    lc::Wheel { hue: w.hue, sat: w.sat, lum: w.lum }
}

pub fn to_craft_grain(g: &lc::Grain) -> Grain {
    Grain { amount: g.amount, size: g.size, roughness: g.roughness, seed: g.seed }
}

pub fn to_craft_calibration(c: &lc::Calibration) -> Calibration {
    Calibration {
        shadows_tint: c.shadows_tint,
        red_hue: c.red_hue,
        red_sat: c.red_sat,
        green_hue: c.green_hue,
        green_sat: c.green_sat,
        blue_hue: c.blue_hue,
        blue_sat: c.blue_sat,
    }
}

pub fn from_craft_calibration(c: &Calibration) -> lc::Calibration {
    lc::Calibration {
        shadows_tint: c.shadows_tint,
        red_hue: c.red_hue,
        red_sat: c.red_sat,
        green_hue: c.green_hue,
        green_sat: c.green_sat,
        blue_hue: c.blue_hue,
        blue_sat: c.blue_sat,
    }
}

pub fn to_craft_point_color(p: &lc::PointColor) -> PointColor {
    PointColor {
        lum: p.lum,
        chroma: p.chroma,
        hue: p.hue,
        hue_shift: p.hue_shift,
        sat_shift: p.sat_shift,
        lum_shift: p.lum_shift,
        range: p.range,
        variance: p.variance,
        hue_range: p.hue_range,
        lum_range: p.lum_range,
        sat_range: p.sat_range,
    }
}

pub fn to_craft_settings(s: &lc::DevelopSettings) -> DevelopSettings {
    DevelopSettings {
        version: s.version,
        process: ProcessVersion(s.process.0),
        profile: Profile { id: s.profile.id.clone(), amount: s.profile.amount, camera_local_tone: s.profile.camera_local_tone },
        treatment: to_craft_treatment(s.treatment),
        wb: WhiteBalance { mode: to_craft_wb_mode(s.wb.mode), temp: s.wb.temp, tint: s.wb.tint, scale: to_craft_wb_scale(s.wb.scale) },
        light: Light {
            exposure: s.light.exposure,
            contrast: s.light.contrast,
            highlights: s.light.highlights,
            shadows: s.light.shadows,
            whites: s.light.whites,
            blacks: s.light.blacks,
        },
        curve: to_craft_tone_curve(&s.curve),
        color: ColorAdj { vibrance: s.color.vibrance, saturation: s.color.saturation },
        mixer: Mixer {
            red: to_craft_hsl(s.mixer.red),
            orange: to_craft_hsl(s.mixer.orange),
            yellow: to_craft_hsl(s.mixer.yellow),
            green: to_craft_hsl(s.mixer.green),
            aqua: to_craft_hsl(s.mixer.aqua),
            blue: to_craft_hsl(s.mixer.blue),
            purple: to_craft_hsl(s.mixer.purple),
            magenta: to_craft_hsl(s.mixer.magenta),
        },
        point_colors: s.point_colors.iter().map(to_craft_point_color).collect(),
        bw_mix: BwMix {
            red: s.bw_mix.red,
            orange: s.bw_mix.orange,
            yellow: s.bw_mix.yellow,
            green: s.bw_mix.green,
            aqua: s.bw_mix.aqua,
            blue: s.bw_mix.blue,
            purple: s.bw_mix.purple,
            magenta: s.bw_mix.magenta,
        },
        grading: ColorGrading {
            shadows: to_craft_wheel(s.grading.shadows),
            midtones: to_craft_wheel(s.grading.midtones),
            highlights: to_craft_wheel(s.grading.highlights),
            global: to_craft_wheel(s.grading.global),
            blending: s.grading.blending,
            balance: s.grading.balance,
        },
        effects: Effects { texture: s.effects.texture, clarity: s.effects.clarity, dehaze: s.effects.dehaze },
        grain: to_craft_grain(&s.grain),
        detail: Detail {
            sharpen_amount: s.detail.sharpen_amount,
            sharpen_radius: s.detail.sharpen_radius,
            sharpen_detail: s.detail.sharpen_detail,
            sharpen_masking: s.detail.sharpen_masking,
            nr_luminance: s.detail.nr_luminance,
            nr_detail: s.detail.nr_detail,
            nr_contrast: s.detail.nr_contrast,
            nr_color: s.detail.nr_color,
            nr_color_detail: s.detail.nr_color_detail,
            nr_color_smoothness: s.detail.nr_color_smoothness,
            nr_outliers: s.detail.nr_outliers,
        },
        calibration: to_craft_calibration(&s.calibration),
        masks: s
            .masks
            .iter()
            .map(|m| Mask {
                id: m.id,
                visible: m.visible,
                invert: m.invert,
                components: if m.components.is_empty() {
                    Vec::new()
                } else {
                    vec![MaskComponent {
                        name: None,
                        op: MaskOp::Add,
                        invert: false,
                        shape: MaskShape::Linear { start: CraftPoint::new(0.0, 0.0), end: CraftPoint::new(1.0, 1.0) },
                    }]
                },
                adjust: LocalAdjustments {
                    temp: m.adjust.temp,
                    tint: m.adjust.tint,
                    exposure: m.adjust.exposure,
                    contrast: m.adjust.contrast,
                    highlights: m.adjust.highlights,
                    shadows: m.adjust.shadows,
                    whites: m.adjust.whites,
                    blacks: m.adjust.blacks,
                    texture: m.adjust.texture,
                    clarity: m.adjust.clarity,
                    dehaze: m.adjust.dehaze,
                    hue: m.adjust.hue,
                    saturation: m.adjust.saturation,
                    sharpness: m.adjust.sharpness,
                    noise: m.adjust.noise,
                    moire: m.adjust.moire,
                    defringe: m.adjust.defringe,
                    lens_blur: m.adjust.lens_blur,
                    lens_blur_off: m.adjust.lens_blur_off,
                    lens_blur_outside: m.adjust.lens_blur_outside,
                    lens_blur_boost: m.adjust.lens_blur_boost,
                    lens_blur_transition: m.adjust.lens_blur_transition,
                    color_hue: m.adjust.color_hue,
                    color_sat: m.adjust.color_sat,
                    amount: m.adjust.amount,
                },
                ..Default::default()
            })
            .collect(),
        disabled_sections: s.disabled_sections.clone(),
        ..Default::default()
    }
}

pub fn apply_craft_profile_fields(dst: &mut lc::DevelopSettings, src: &DevelopSettings) {
    dst.treatment = from_craft_treatment(src.treatment);
    dst.light = lc::Light {
        exposure: src.light.exposure,
        contrast: src.light.contrast,
        highlights: src.light.highlights,
        shadows: src.light.shadows,
        whites: src.light.whites,
        blacks: src.light.blacks,
    };
    dst.curve = from_craft_tone_curve(&src.curve);
    dst.color = lc::ColorAdj { vibrance: src.color.vibrance, saturation: src.color.saturation };
    dst.mixer = lc::Mixer {
        red: from_craft_hsl(src.mixer.red),
        orange: from_craft_hsl(src.mixer.orange),
        yellow: from_craft_hsl(src.mixer.yellow),
        green: from_craft_hsl(src.mixer.green),
        aqua: from_craft_hsl(src.mixer.aqua),
        blue: from_craft_hsl(src.mixer.blue),
        purple: from_craft_hsl(src.mixer.purple),
        magenta: from_craft_hsl(src.mixer.magenta),
    };
    dst.bw_mix = lc::BwMix {
        red: src.bw_mix.red,
        orange: src.bw_mix.orange,
        yellow: src.bw_mix.yellow,
        green: src.bw_mix.green,
        aqua: src.bw_mix.aqua,
        blue: src.bw_mix.blue,
        purple: src.bw_mix.purple,
        magenta: src.bw_mix.magenta,
    };
    dst.grading = lc::ColorGrading {
        shadows: from_craft_wheel(src.grading.shadows),
        midtones: from_craft_wheel(src.grading.midtones),
        highlights: from_craft_wheel(src.grading.highlights),
        global: from_craft_wheel(src.grading.global),
        blending: src.grading.blending,
        balance: src.grading.balance,
    };
    dst.effects = lc::Effects { texture: src.effects.texture, clarity: src.effects.clarity, dehaze: src.effects.dehaze };
    dst.detail = lc::Detail {
        sharpen_amount: src.detail.sharpen_amount,
        sharpen_radius: src.detail.sharpen_radius,
        sharpen_detail: src.detail.sharpen_detail,
        sharpen_masking: src.detail.sharpen_masking,
        nr_luminance: src.detail.nr_luminance,
        nr_detail: src.detail.nr_detail,
        nr_contrast: src.detail.nr_contrast,
        nr_color: src.detail.nr_color,
        nr_color_detail: src.detail.nr_color_detail,
        nr_color_smoothness: src.detail.nr_color_smoothness,
        nr_outliers: src.detail.nr_outliers,
    };
    dst.calibration = from_craft_calibration(&src.calibration);
}
