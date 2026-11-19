use crate::{Orientation, Rect};
pub use craft_metadata::focus::{HEADER_PREFIX, MAX_HEADER_BYTES, camera_supported, decoder_tag, synthetic_arw};

pub struct HeaderRead {
    pub focus: Option<FocusPoint>,
    pub bytes_read: u64,
}

pub fn read_header(len: u64, read_at: &mut dyn FnMut(u64, &mut [u8]) -> std::io::Result<()>) -> std::io::Result<HeaderRead> {
    craft_metadata::focus::read_header(len, read_at)
        .map(|r| HeaderRead { focus: r.focus.map(crate::convert::focus_from_craft), bytes_read: r.bytes_read })
}
use serde::{Deserialize, Serialize};

/// Where the camera focused, on the upright photo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FocusPoint {
    /// The focus frame, normalized to the photo's full, EXIF-oriented frame (y down, 0..1 on both
    /// axes, clipped to it) — the frame face regions use, so it follows Rotate, flips and crops.
    pub rect: Rect,
    /// Focus mode as the camera names it (`AF-C`, `AF-S`, `DMF`…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// The AF area chosen on the camera (`Wide`, `Zone`, `Flexible Spot`…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<String>,
    /// What the camera followed, when it says (`Animal Eye Tracking`, `Face tracking`, `Lock On AF`…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking: Option<String>,
}

impl FocusPoint {
    /// The box's centre (normalized, upright).
    pub fn center(&self) -> (f64, f64) {
        ((self.rect.x0 + self.rect.x1) / 2.0, (self.rect.y0 + self.rect.y1) / 2.0)
    }
    /// The words that describe it, in order (mode, area, tracking), for "AF-C · Wide · Animal Eye Tracking".
    pub fn words(&self) -> Vec<&str> {
        [&self.mode, &self.area, &self.tracking].into_iter().filter_map(|s| s.as_deref()).filter(|s| !s.is_empty()).collect()
    }
    /// The box clipped to the frame and checked: `None` when it is not a usable box.
    pub fn sanitized(self) -> Option<FocusPoint> {
        let craft_fp =
            craft_metadata::FocusPoint { rect: crate::convert::rect_to_craft(self.rect), mode: self.mode, area: self.area, tracking: self.tracking };
        craft_fp.sanitized().map(crate::convert::focus_from_craft)
    }
    /// Orient a normalized sensor-frame box to the upright photo's frame.
    pub fn orient_box(r: Rect, o: Orientation) -> Rect {
        match o.to_exif() {
            2 => Rect { x0: 1.0 - r.x1, y0: r.y0, x1: 1.0 - r.x0, y1: r.y1 },
            3 => Rect { x0: 1.0 - r.x1, y0: 1.0 - r.y1, x1: 1.0 - r.x0, y1: 1.0 - r.y0 },
            4 => Rect { x0: r.x0, y0: 1.0 - r.y1, x1: r.x1, y1: 1.0 - r.y0 },
            5 => Rect { x0: r.y0, y0: r.x0, x1: r.y1, y1: r.x1 },
            6 => Rect { x0: 1.0 - r.y1, y0: r.x0, x1: 1.0 - r.y0, y1: r.x1 },
            7 => Rect { x0: 1.0 - r.y1, y0: 1.0 - r.x1, x1: 1.0 - r.y0, y1: 1.0 - r.x0 },
            8 => Rect { x0: r.y0, y0: 1.0 - r.x1, x1: r.y1, y1: 1.0 - r.x0 },
            _ => r,
        }
    }
}
