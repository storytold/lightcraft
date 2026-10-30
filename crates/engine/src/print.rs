//! The Print module: page layout, composition of pages and PDF output.
//!
//! [`layout`] is the pure page model (paper, margins, cells); [`compose`] renders a layout and its
//! photos to [`PageRaster`]s; [`template`] holds the saved settings; [`output`] writes the pages
//! as a PDF.
pub mod compose;
pub mod layout;
pub mod output;
pub mod template;

use lightcraft_color::transfer::{linear_to_srgb, srgb_to_linear};
use lightcraft_color::{REC2020, SRGB};
use lightcraft_pipeline::OutputSpace;
use serde::{Deserialize, Serialize};

/// Lowest and highest print resolution, pixels per inch.
pub const MIN_PPI: f32 = 72.0;
pub const MAX_PPI: f32 = 720.0;
/// Pages a single print may have.
pub const MAX_JOB_PAGES: usize = 100;
/// Most pixels one page may have (about 100 megapixels: A3 at 720 ppi is 60).
pub const MAX_PAGE_PIXELS: u64 = 100_000_000;
/// Most pixels a print job may hold across all its pages (6 bytes each in memory).
pub const MAX_JOB_PIXELS: u64 = 200_000_000;

/// One composed page, ready for output: row-major interleaved RGB, 16 bits per sample, encoded in
/// Rec. 2020 ([`lightcraft_pipeline::OutputSpace::Rec2020`]; the transfer curve is that space's).
/// The page covers its paper edge to edge: `width / ppi` by `height / ppi` inches.
#[derive(Clone, Debug, PartialEq)]
pub struct PageRaster {
    pub width: u32,
    pub height: u32,
    pub ppi: f32,
    pub rgb: Vec<u16>,
}

/// How a print job is produced (Lightroom's Print Job panel): resolution, and the colour
/// management, print adjustment, print sharpening and depth of [`crate::print_color`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct PrintJob {
    /// Print resolution in pixels per inch, 72 to 720.
    pub ppi: f32,
    /// Profile, rendering intent, black point compensation, print adjustment, print sharpening.
    pub color: crate::print_color::PrintColorSettings,
    /// Print in grayscale: the composed pages are turned gray ([`gray_rgb16_in_place`]: the gray of
    /// [`gray_u8`], taken from the sRGB rendition of the colours) so the PDF, the raster sent to a
    /// printer and the dialog's preview agree, whatever the driver does with its own monochrome hint.
    pub grayscale: bool,
}

impl Default for PrintJob {
    fn default() -> Self {
        Self { ppi: 240.0, color: crate::print_color::PrintColorSettings::default(), grayscale: false }
    }
}

/// Gray of an 8-bit sRGB colour with Rec. 709 weights applied to the encoded values, as integers that sum
/// to 256 (white stays 255, black 0). It is the gray the Print dialog's preview shows, and the definition
/// [`gray_rgb16_in_place`] follows for the printed pages.
pub fn gray_u8(r: u8, g: u8, b: u8) -> u8 {
    ((u32::from(r) * 54 + u32::from(g) * 183 + u32::from(b) * 19) >> 8) as u8
}

/// Turn a composed page (interleaved 16-bit RGB in the `Rec2020` output encoding) gray, to the gray the
/// preview shows for the same colours ([`gray_u8`]): the colours are taken to sRGB, the Rec. 709 weights
/// are applied to those encoded values, and the result is the neutral of that luma in the page's own
/// encoding. Applying the weights to the page's own samples instead would not agree with the preview: the
/// primaries and the transfer curves differ (a pure red is 53 in the preview and was 100 on the page).
/// Neutral colours, white and black stay as they are.
pub fn gray_rgb16_in_place(rgb: &mut [u16]) {
    let trc = OutputSpace::Rec2020.trc();
    // 16-bit Rec. 2020 code to linear light, once per code value.
    let lut: Vec<f32> = (0..=u16::MAX).map(|v| trc.decode(f32::from(v) / 65535.0)).collect();
    let m = REC2020.to_space(&SRGB).to_f32();
    for px in rgb.as_chunks_mut::<3>().0 {
        let l = px.map(|v| lut.get(usize::from(v)).copied().unwrap_or(0.0));
        let encoded = |r: usize| {
            let linear = m.get(r).map_or(0.0, |m| m[0] * l[0] + m[1] * l[1] + m[2] * l[2]);
            linear_to_srgb(linear.clamp(0.0, 1.0))
        };
        let luma = (54.0 * encoded(0) + 183.0 * encoded(1) + 19.0 * encoded(2)) / 256.0;
        let neutral = trc.encode(srgb_to_linear(luma.clamp(0.0, 1.0))).clamp(0.0, 1.0);
        let y = (neutral * 65535.0 + 0.5) as u16;
        *px = [y, y, y];
    }
}

impl PrintJob {
    /// The resolution to render at: [`Self::ppi`] clamped to 72..=720 (240 for NaN).
    pub fn effective_ppi(&self) -> f32 {
        if self.ppi.is_finite() { self.ppi.clamp(MIN_PPI, MAX_PPI) } else { 240.0 }
    }

    /// Rejects a resolution outside the supported range (commands and templates, which should
    /// say so rather than silently clamp).
    pub fn validate(&self) -> Result<(), String> {
        if !(self.ppi.is_finite() && (MIN_PPI..=MAX_PPI).contains(&self.ppi)) {
            return Err(format!("ppi must be a number from {MIN_PPI} to {MAX_PPI}"));
        }
        self.color.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppi_is_clamped_and_validated() {
        assert_eq!(PrintJob::default().effective_ppi(), 240.0);
        for (v, e) in [(10.0, 72.0), (5000.0, 720.0), (f32::NAN, 240.0), (f32::INFINITY, 240.0), (300.0, 300.0)] {
            assert_eq!(PrintJob { ppi: v, ..Default::default() }.effective_ppi(), e);
        }
        assert!(PrintJob { ppi: 300.0, ..Default::default() }.validate().is_ok());
        for bad in [0.0, 71.0, 721.0, f32::NAN, f32::NEG_INFINITY] {
            assert!(PrintJob { ppi: bad, ..Default::default() }.validate().is_err());
        }
        let j: PrintJob = serde_json::from_str("{}").unwrap();
        assert_eq!(j, PrintJob::default());
        assert!(serde_json::from_str::<PrintJob>(r#"{"dpi": 1}"#).is_err());
        // the colour settings ride along, defaults filled in, and are validated with the job
        let j: PrintJob = serde_json::from_str(r#"{"ppi": 300, "color": {"intent": "relative", "brightness": 20}}"#).unwrap();
        assert_eq!(j.color.intent, crate::print_color::PrintIntent::Relative);
        assert!(j.color.black_point_compensation && j.validate().is_ok());
        let j: PrintJob = serde_json::from_str(r#"{"color": {"contrast": 500}}"#).unwrap();
        assert!(j.validate().is_err());
    }

    #[test]
    fn gray_is_rec709_and_keeps_the_extremes() {
        assert_eq!((gray_u8(255, 0, 0), gray_u8(0, 255, 0), gray_u8(0, 0, 255)), (53, 182, 18));
        assert_eq!((gray_u8(255, 255, 255), gray_u8(0, 0, 0), gray_u8(90, 90, 90)), (255, 0, 90));
        let mut px = [65535u16, 65535, 65535, 0, 0, 0];
        gray_rgb16_in_place(&mut px);
        assert_eq!(px, [65535, 65535, 65535, 0, 0, 0], "white and black stay");
    }

    /// The printed gray is the preview's gray: composing an sRGB colour onto the page and turning the page
    /// gray gives, once the page is shown as sRGB, the value `gray_u8` gives for that colour.
    #[test]
    fn the_printed_gray_is_the_previews_gray() {
        use crate::print::compose::rec2020_from_srgb8;
        let mut colours = vec![[255u8, 0, 0], [0, 255, 0], [0, 0, 255], [220, 30, 40], [10, 20, 250], [128, 128, 128], [90, 90, 90]];
        for r in (0..=255u8).step_by(51) {
            for g in (0..=255u8).step_by(51) {
                colours.push([r, g, 255 - r / 2]);
            }
        }
        for c in colours {
            let mut rgb = rec2020_from_srgb8(c).to_vec();
            gray_rgb16_in_place(&mut rgb);
            assert!(rgb[0] == rgb[1] && rgb[1] == rgb[2], "{c:?} is gray");
            let page = PageRaster { width: 1, height: 1, ppi: 100.0, rgb };
            let shown = output::page_to_srgb8(&page).unwrap().data[0];
            let want = gray_u8(c[0], c[1], c[2]);
            assert!(
                shown[..3].iter().all(|&v| (i32::from(v) - i32::from(want)).abs() <= 2),
                "{c:?}: the page shows {shown:?}, the preview shows {want}"
            );
        }
    }

    #[test]
    fn the_grayscale_flag_is_a_job_setting_off_by_default() {
        assert!(!PrintJob::default().grayscale);
        let j: PrintJob = serde_json::from_str(r#"{"grayscale": true}"#).unwrap();
        assert!(j.grayscale);
    }
}
