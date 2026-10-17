//! The Print module: page layout, composition of pages and PDF output.
//!
//! [`layout`] is the pure page model (paper, margins, cells); [`compose`] renders a layout and its
//! photos to [`PageRaster`]s; [`template`] holds the saved settings; [`output`] writes the pages
//! as a PDF.
pub mod compose;
pub mod layout;
pub mod output;
pub mod template;

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
    /// Print in grayscale: the composed pages are turned gray (Rec. 709 weights, [`gray_u8`]) so the
    /// PDF, the raster sent to a printer and the dialog's preview agree, whatever the driver does with
    /// its own monochrome hint.
    pub grayscale: bool,
}

impl Default for PrintJob {
    fn default() -> Self {
        Self { ppi: 240.0, color: crate::print_color::PrintColorSettings::default(), grayscale: false }
    }
}

/// Gray of an 8-bit colour with Rec. 709 weights, as integers that sum to 256 (white stays 255, black 0).
/// The one conversion behind the Print dialog's preview and, through [`gray_u16`], the printed pages.
pub fn gray_u8(r: u8, g: u8, b: u8) -> u8 {
    ((u32::from(r) * 54 + u32::from(g) * 183 + u32::from(b) * 19) >> 8) as u8
}

/// [`gray_u8`] for 16-bit samples.
pub fn gray_u16(r: u16, g: u16, b: u16) -> u16 {
    ((u32::from(r) * 54 + u32::from(g) * 183 + u32::from(b) * 19) >> 8) as u16
}

/// Turn interleaved 16-bit RGB samples gray in place.
pub fn gray_rgb16_in_place(rgb: &mut [u16]) {
    for px in rgb.as_chunks_mut::<3>().0 {
        let y = gray_u16(px[0], px[1], px[2]);
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
        let mut px = [65535u16, 0, 0, 65535, 65535, 65535, 0, 0, 0];
        gray_rgb16_in_place(&mut px);
        assert_eq!(px, [13_823, 13_823, 13_823, 65535, 65535, 65535, 0, 0, 0]);
        assert_eq!(gray_u16(30_000, 30_000, 30_000), 30_000, "gray stays gray");
        // the 16-bit and 8-bit conversions agree on 8-bit values widened by 257
        for c in [[220u8, 30, 40], [0, 200, 0], [10, 20, 250]] {
            let wide = gray_u16(u16::from(c[0]) * 257, u16::from(c[1]) * 257, u16::from(c[2]) * 257);
            assert!((i32::from(wide / 257) - i32::from(gray_u8(c[0], c[1], c[2]))).abs() <= 1);
        }
    }

    #[test]
    fn the_grayscale_flag_is_a_job_setting_off_by_default() {
        assert!(!PrintJob::default().grayscale);
        let j: PrintJob = serde_json::from_str(r#"{"grayscale": true}"#).unwrap();
        assert!(j.grayscale);
    }
}
