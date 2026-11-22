//! The camera's local tone mapping: a raw's DNG `ProfileGainTableMap`, backed by `craft_develop::local_tone`.

use lightcraft_develop::DevelopSettings;
use lightcraft_geom::Point;
use lightcraft_raster::Rgb32f;
use lightcraft_raw::gaintable::{GainTableMap, SourcePlacement};

use crate::{Plan, SourceInfo};

/// A raw's gain table map and where its developed source sits in the active area.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LocalTone {
    pub map: GainTableMap,
    pub placement: SourcePlacement,
}

/// Whether the render applies the source's local tone mapping.
pub fn enabled(info: &SourceInfo, s: &DevelopSettings) -> bool {
    s.profile.camera_local_tone && info.raw && info.local_tone.is_some()
}

/// Multiply the resampled, not yet white-balanced source `img` (the output frame of `p`) by the
/// local tone gain at each pixel. Blank canvas (outside a lens / perspective warp) is left alone.
pub fn apply(img: &mut Rgb32f, info: &SourceInfo, p: &Plan<'_>) {
    let Some(lt) = info.local_tone.as_deref().filter(|_| enabled(info, &p.settings)) else { return };
    let (w, h) = (img.width, img.height);
    let frame = &p.frame;
    let (ow, oh) = (frame.ow.max(1e-9), frame.oh.max(1e-9));
    let o2t = frame.out_to_oriented(w, h);
    let user = frame.orient.inverse();
    let craft_lt = craft_develop::local_tone::LocalTone {
        map: lt.map.clone(),
        placement: craft_develop::sensor::gaintable::SourcePlacement {
            rect: lt.placement.rect,
            orientation: match lt.placement.orientation {
                lightcraft_geom::Orientation::Normal => craft_geom::Orientation::Normal,
                lightcraft_geom::Orientation::FlipH => craft_geom::Orientation::FlipH,
                lightcraft_geom::Orientation::Rotate180 => craft_geom::Orientation::Rotate180,
                lightcraft_geom::Orientation::FlipV => craft_geom::Orientation::FlipV,
                lightcraft_geom::Orientation::Transpose => craft_geom::Orientation::Transpose,
                lightcraft_geom::Orientation::Rotate90 => craft_geom::Orientation::Rotate90,
                lightcraft_geom::Orientation::Transverse => craft_geom::Orientation::Transverse,
                lightcraft_geom::Orientation::Rotate270 => craft_geom::Orientation::Rotate270,
            },
        },
    };
    let mut craft = crate::bridge::take_craft_rgb32f(img);
    craft_develop::local_tone::apply(&mut craft, &craft_lt, |x, y| {
        let s = match &frame.warp {
            Some(wp) => wp.frame(&o2t, x, y)?.1,
            None => o2t.apply(Point::new(x as f64 + 0.5, y as f64 + 0.5)),
        };
        let (u, v) = user.map(s.x / ow, s.y / oh, 1.0, 1.0);
        Some(lt.placement.active(u, v))
    });
    crate::bridge::put_craft_rgb32f(img, craft);
}
