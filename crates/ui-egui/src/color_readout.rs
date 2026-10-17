//! Colour value readouts: the colour under the pointer in the loupe (shown under the histogram,
//! as Lightroom shows R / G / B %) and the values beside the colour pickers, as RGB, HSL or
//! CIE L*a*b* (D50).
//!
//! The values are those of the sRGB image on screen, the one the histogram counts (a render for
//! a display profile is converted back to sRGB first): RGB and HSL are sRGB-encoded percentages,
//! Lab is that sRGB colour converted to XYZ and Bradford-adapted to D50
//! ([`lcc_color::lab::srgb8_to_lab`]). Lab does not depend on the RGB space it came from, so it
//! reads the same as in an editor that shows Lab from a wider working space.

use serde::{Deserialize, Serialize};

/// The format of the colour readout under the histogram (`view.colorReadout`; persisted).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColorReadout {
    /// R / G / B in percent (Lightroom's default).
    #[default]
    Rgb,
    /// Hue in degrees, saturation and lightness in percent.
    Hsl,
    /// CIE L*a*b*, D50.
    Lab,
}

impl ColorReadout {
    pub const ALL: [ColorReadout; 3] = [ColorReadout::Rgb, ColorReadout::Hsl, ColorReadout::Lab];

    pub fn id(self) -> &'static str {
        match self {
            ColorReadout::Rgb => "rgb",
            ColorReadout::Hsl => "hsl",
            ColorReadout::Lab => "lab",
        }
    }

    pub fn parse(s: &str) -> Option<ColorReadout> {
        Self::ALL.into_iter().find(|f| f.id().eq_ignore_ascii_case(s))
    }

    /// The next format (RGB → HSL → Lab → RGB).
    pub fn next(self) -> ColorReadout {
        match self {
            ColorReadout::Rgb => ColorReadout::Hsl,
            ColorReadout::Hsl => ColorReadout::Lab,
            ColorReadout::Lab => ColorReadout::Rgb,
        }
    }

    /// The command that picks this format.
    pub fn command(self) -> &'static str {
        match self {
            ColorReadout::Rgb => "view.colorReadoutRgb",
            ColorReadout::Hsl => "view.colorReadoutHsl",
            ColorReadout::Lab => "view.colorReadoutLab",
        }
    }

    /// `rgb` (8-bit sRGB) in this format, on one line.
    pub fn format(self, rgb: [u8; 3]) -> String {
        match self {
            ColorReadout::Rgb => {
                let [r, g, b] = rgb.map(|v| f32::from(v) / 2.55);
                format!("R {r:.1}  G {g:.1}  B {b:.1} %")
            }
            ColorReadout::Hsl => {
                let [h, s, l] = lcc_color::perceptual::rgb_to_hsl(rgb.map(|v| f32::from(v) / 255.0));
                let h = if h.is_finite() { h.round().rem_euclid(360.0) } else { 0.0 };
                let pct = |v: f32| if v.is_finite() { (v * 100.0).clamp(0.0, 100.0) } else { 0.0 };
                format!("H {h:.0}°  S {:.1}  L {:.1} %", pct(s), pct(l))
            }
            ColorReadout::Lab => {
                let [l, a, b] = lcc_color::lab::srgb8_to_lab(rgb);
                format!("L {l:.1}  a {a:.1}  b {b:.1}")
            }
        }
    }
}

/// `rgb` in all three formats, one per line (colour pickers' hover text).
pub fn all_formats(rgb: [u8; 3]) -> String {
    ColorReadout::ALL.map(|f| f.format(rgb)).join("\n")
}

/// [`all_formats`] as a hover text on a colour picker button.
pub fn on_picker(resp: egui::Response, rgb: [u8; 3]) -> egui::Response {
    resp.on_hover_text(all_formats(rgb))
}

/// The colour of a loupe pixel as the histogram counts it: `pixels` hold the `Main` render, in
/// the display profile's values when one is in use (then converted back to sRGB).
pub fn srgb_at(pixels: &egui::ColorImage, u: f32, v: f32, display: Option<&lcc_engine::display::Display>) -> Option<[u8; 3]> {
    if !(u.is_finite() && v.is_finite()) || !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
        return None;
    }
    let [w, h] = pixels.size;
    let x = ((u * w as f32) as usize).min(w.checked_sub(1)?);
    let y = ((v * h as f32) as usize).min(h.checked_sub(1)?);
    let c = pixels.pixels.get(y.checked_mul(w)?.checked_add(x)?)?;
    let rgb = [c.r(), c.g(), c.b()];
    match display {
        None => Some(rgb),
        Some(d) => {
            let one = lcc_raster::Rgba8 { width: 1, height: 1, data: vec![[rgb[0], rgb[1], rgb[2], 255]] };
            let s = d.profile.to_srgb(&one).ok()?;
            let p = s.data.first()?;
            Some([p[0], p[1], p[2]])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_format_reads_white_and_round_trips_its_id() {
        assert!(ColorReadout::Rgb.format([255, 255, 255]).contains("100.0"));
        assert!(ColorReadout::Lab.format([255, 255, 255]).starts_with("L 100.0"));
        assert!(ColorReadout::Hsl.format([255, 0, 0]).starts_with("H 0°"));
        for f in ColorReadout::ALL {
            assert_eq!(ColorReadout::parse(f.id()), Some(f));
            assert_ne!(f.next(), f);
        }
        assert_eq!(all_formats([10, 20, 30]).lines().count(), 3);
    }

    #[test]
    fn sampling_outside_or_on_an_empty_image_is_none() {
        let img = egui::ColorImage::new([2, 2], vec![egui::Color32::from_rgb(1, 2, 3); 4]);
        assert_eq!(srgb_at(&img, 0.5, 0.5, None), Some([1, 2, 3]));
        assert_eq!(srgb_at(&img, 1.0, 1.0, None), Some([1, 2, 3]));
        for (u, v) in [(-0.1, 0.5), (0.5, 1.5), (f32::NAN, 0.0), (f32::INFINITY, 0.0)] {
            assert_eq!(srgb_at(&img, u, v, None), None);
        }
        let empty = egui::ColorImage::new([0, 0], vec![]);
        assert_eq!(srgb_at(&empty, 0.5, 0.5, None), None);
    }
}
