//! Vendor extensions: which maker a camera is, and the little the app does differently for it.
//! Everything here comes from public material (the PTP standard's vendor extension registry, USB
//! vendor ids, and operation codes the makers document in their published SDK / PTP notes). Codes
//! marked *unverified* have not been tried on a body yet; they are only sent when the camera's
//! DeviceInfo lists them as supported, and a refusal falls back to generic PTP.
//!
//! Order of work (PLAN_phase_4.md §4.1): Canon, Nikon, Sony, Fujifilm.

use serde::Serialize;

/// The camera maker, as far as PTP is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Vendor {
    Canon,
    Nikon,
    Sony,
    Fujifilm,
    Other,
}

/// USB vendor ids (from the public USB-IF id list).
pub mod usb_id {
    pub const CANON: u16 = 0x04A9;
    pub const NIKON: u16 = 0x04B0;
    pub const SONY: u16 = 0x054C;
    pub const FUJIFILM: u16 = 0x04CB;
}

/// PTP vendor extension ids (ISO 15740 registry).
pub mod ext_id {
    pub const MICROSOFT: u32 = 0x0000_0006;
    pub const NIKON: u32 = 0x0000_000A;
    pub const CANON: u32 = 0x0000_000B;
    pub const FUJIFILM: u32 = 0x0000_000E;
    pub const SONY: u32 = 0x0000_0011;
}

/// Nikon live view operations (Nikon's published PTP operation list). *Unverified.*
pub mod nikon {
    pub const START_LIVE_VIEW: u16 = 0x9201;
    pub const END_LIVE_VIEW: u16 = 0x9202;
    pub const GET_LIVE_VIEW_IMAGE: u16 = 0x9203;
}

impl Vendor {
    /// From the DeviceInfo's vendor extension id, else the manufacturer string, else the USB
    /// vendor id.
    pub fn detect(extension_id: u32, manufacturer: &str, usb_vendor: Option<u16>) -> Vendor {
        match extension_id {
            ext_id::CANON => return Vendor::Canon,
            ext_id::NIKON => return Vendor::Nikon,
            ext_id::SONY => return Vendor::Sony,
            ext_id::FUJIFILM => return Vendor::Fujifilm,
            _ => {}
        }
        let m = manufacturer.to_lowercase();
        for (k, v) in [("canon", Vendor::Canon), ("nikon", Vendor::Nikon), ("sony", Vendor::Sony), ("fuji", Vendor::Fujifilm)] {
            if m.contains(k) {
                return v;
            }
        }
        match usb_vendor {
            Some(usb_id::CANON) => Vendor::Canon,
            Some(usb_id::NIKON) => Vendor::Nikon,
            Some(usb_id::SONY) => Vendor::Sony,
            Some(usb_id::FUJIFILM) => Vendor::Fujifilm,
            _ => Vendor::Other,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Vendor::Canon => "Canon",
            Vendor::Nikon => "Nikon",
            Vendor::Sony => "Sony",
            Vendor::Fujifilm => "Fujifilm",
            Vendor::Other => "PTP camera",
        }
    }

    /// Live view operations (start, frame, end) for this maker, when known.
    pub fn live_view_ops(self) -> Option<(u16, u16, u16)> {
        match self {
            Vendor::Nikon => Some((nikon::START_LIVE_VIEW, nikon::GET_LIVE_VIEW_IMAGE, nikon::END_LIVE_VIEW)),
            _ => None,
        }
    }

    /// What generic PTP can't do for this maker yet, said plainly (shown in the tether bar).
    pub fn note(self) -> Option<&'static str> {
        match self {
            Vendor::Canon => Some(
                "Canon EOS bodies need Canon's own remote-control extension for capture; if Capture does nothing, use studio capture with EOS Utility.",
            ),
            Vendor::Sony => Some("Sony bodies need PC Remote mode; if Capture does nothing, use studio capture with Imaging Edge Remote."),
            Vendor::Fujifilm => Some("Fujifilm bodies need USB tether mode (PC shoot auto); otherwise use studio capture with the maker's app."),
            Vendor::Nikon | Vendor::Other => None,
        }
    }
}

/// The JPEG inside a vendor live view frame: vendors put a header of their own before it, so
/// the frame starts at the first JPEG start-of-image marker.
pub fn jpeg_in(frame: &[u8]) -> Option<&[u8]> {
    let at = frame.windows(3).position(|w| w == [0xFF, 0xD8, 0xFF])?;
    frame.get(at..)
}
