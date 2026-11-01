//! The print job's colour: which profile composed pages are converted to and how, print sharpening
//! for the paper, and the print-ready PDF they end up in.
//!
//! - **Profile**: "Managed by Printer" sends the page in sRGB or Adobe RGB with that profile
//!   embedded (the driver converts); a printer / paper ICC profile converts the page to the device's
//!   own values here and names the profile as the document's output intent, so the driver must not
//!   convert again.
//! - **Intent, black point compensation, print adjustment**: [`lightcraft_codecs::print`].
//! - **Print sharpening** (Low / Standard / High for Matte or Glossy paper): each photo is sharpened
//!   at its print size before it goes on the page, with the export sharpener.
//! - **Depth**: 16 bits per component by default (no banding from the profile conversion); 8 bits
//!   for smaller files.
//!
//! Pages arrive as 16-bit RGB in the `Rec2020` output encoding, the composed page of the layout.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lightcraft_codecs::display::MAX_PROFILE_BYTES;
use lightcraft_codecs::print::{PrintColor, PrintTarget, PrintTransform};
use lightcraft_codecs::{NamedSpace, proof::ProofIntent};
use serde::{Deserialize, Serialize};

use crate::export::{DeepImage, DeepSamples, OutputSpace, SharpenAmount, SharpenFor, output_sharpen_deep};
use crate::print_pdf::{ImageData, OutputIntent, PdfImage, PdfInfo, PdfPage, Placement};

/// The colour space a printer that manages colour receives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ManagedSpace {
    #[default]
    Srgb,
    AdobeRgb,
}

/// Where the print's colours go.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum PrintProfile {
    /// Managed by Printer: the page goes in this space and the driver converts it.
    Managed { space: ManagedSpace },
    /// A printer / paper profile (an `.icc` / `.icm` file): converted here.
    Icc { path: PathBuf },
}

impl Default for PrintProfile {
    fn default() -> Self {
        PrintProfile::Managed { space: ManagedSpace::Srgb }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PrintIntent {
    #[default]
    Perceptual,
    Relative,
}

/// Print sharpening strength (Off leaves the photos as rendered).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PrintSharpen {
    Off,
    Low,
    #[default]
    Standard,
    High,
}

/// The paper print sharpening is tuned for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PrintMedia {
    Matte,
    #[default]
    Glossy,
}

/// The print job's colour settings, as saved with print templates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PrintColorSettings {
    pub profile: PrintProfile,
    pub intent: PrintIntent,
    /// Map the page's black to the paper's darkest (relative colorimetric).
    pub black_point_compensation: bool,
    /// Print adjustment, −100..100.
    pub brightness: f32,
    /// Print adjustment, −100..100.
    pub contrast: f32,
    pub sharpen: PrintSharpen,
    pub media: PrintMedia,
    /// 16 bits per component in the PDF (else 8).
    pub sixteen_bit: bool,
}

impl Default for PrintColorSettings {
    fn default() -> Self {
        PrintColorSettings {
            profile: PrintProfile::default(),
            intent: PrintIntent::default(),
            black_point_compensation: true,
            brightness: 0.0,
            contrast: 0.0,
            sharpen: PrintSharpen::default(),
            media: PrintMedia::default(),
            sixteen_bit: true,
        }
    }
}

impl PrintColorSettings {
    /// Refuse settings a command or template shouldn't carry (out-of-range adjustments, an empty
    /// profile path); whether the profile file is usable is checked when the job is built.
    pub fn validate(&self) -> Result<(), String> {
        for (name, v) in [("brightness", self.brightness), ("contrast", self.contrast)] {
            if !(v.is_finite() && (-100.0..=100.0).contains(&v)) {
                return Err(format!("{name} must be a number from -100 to 100"));
            }
        }
        if let PrintProfile::Icc { path } = &self.profile
            && path.as_os_str().is_empty()
        {
            return Err("the printer profile needs a path".into());
        }
        Ok(())
    }

    /// Sharpen one photo for the paper, at the size it prints (before it goes on the page).
    pub fn sharpen_photo(&self, photo: &mut DeepImage) {
        let amount = match self.sharpen {
            PrintSharpen::Off => return,
            PrintSharpen::Low => SharpenAmount::Low,
            PrintSharpen::Standard => SharpenAmount::Standard,
            PrintSharpen::High => SharpenAmount::High,
        };
        let target = match self.media {
            PrintMedia::Matte => SharpenFor::Matte,
            PrintMedia::Glossy => SharpenFor::Glossy,
        };
        output_sharpen_deep(photo, target, amount);
    }
}

/// Read a printer profile, refusing files that can't be one (the parse happens later).
fn read_profile(path: &Path) -> Result<Vec<u8>, String> {
    let shown = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
    let len = std::fs::metadata(path).map_err(|e| format!("can't read the printer profile {shown}: {e}"))?.len();
    if len > MAX_PROFILE_BYTES as u64 {
        return Err(format!("the printer profile {shown} is too large"));
    }
    std::fs::read(path).map_err(|e| format!("can't read the printer profile {shown}: {e}"))
}

/// A print job's colour, ready to apply: the conversion built once and what the PDF needs to know.
#[derive(Debug)]
pub struct PrintColorJob {
    pub settings: PrintColorSettings,
    transform: PrintTransform,
    /// The printing condition named in the PDF (only for a printer profile).
    intent: Option<OutputIntent>,
}

impl PrintColorJob {
    /// Build the job's conversion. Errors name the profile and what's wrong with it.
    pub fn new(settings: &PrintColorSettings) -> Result<PrintColorJob, String> {
        let s = settings.clone();
        let color = PrintColor {
            intent: match s.intent {
                PrintIntent::Perceptual => ProofIntent::Perceptual,
                PrintIntent::Relative => ProofIntent::Relative,
            },
            black_point_compensation: s.black_point_compensation,
            brightness: s.brightness,
            contrast: s.contrast,
        };
        let (target, name) = match &s.profile {
            PrintProfile::Managed { space } => {
                (PrintTarget::Managed(if *space == ManagedSpace::AdobeRgb { NamedSpace::AdobeRgb } else { NamedSpace::Srgb }), None)
            }
            PrintProfile::Icc { path } => {
                let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                (PrintTarget::Profile(Arc::new(read_profile(path)?)), Some(name))
            }
        };
        let transform = PrintTransform::new(&target, &color).map_err(|e| match &name {
            Some(n) => format!("the printer profile {n}: {e}"),
            None => e,
        })?;
        let intent = name.map(|file| OutputIntent {
            icc: Arc::clone(transform.icc()),
            channels: transform.channels() as u8,
            condition: transform.description().map(str::to_owned).unwrap_or(file),
        });
        Ok(PrintColorJob { settings: s, transform, intent })
    }

    /// Sharpen one photo for the paper ([`PrintColorSettings::sharpen_photo`]).
    pub fn sharpen_photo(&self, photo: &mut DeepImage) {
        self.settings.sharpen_photo(photo);
    }

    /// Convert a composed page (16-bit RGB, `Rec2020` output encoding) to the PDF image that covers
    /// it, in the target's colours.
    pub fn page_image(&self, width: u32, height: u32, rgb: &[u16]) -> Result<PdfImage, String> {
        let want = u64::from(width).checked_mul(u64::from(height)).and_then(|p| p.checked_mul(3));
        if width == 0 || height == 0 || want != Some(rgb.len() as u64) {
            return Err(format!("a page of {width} × {height} pixels can't be made from {} samples", rgb.len()));
        }
        let device = self.transform.apply(rgb)?;
        let channels = self.transform.channels() as u8;
        let icc = Some(Arc::clone(self.transform.icc()));
        Ok(if self.settings.sixteen_bit {
            PdfImage::from_u16(width, height, channels, &device, icc)
        } else {
            let samples = device.iter().map(|&v| ((u32::from(v) * 255 + 32767) / 65535) as u8).collect();
            PdfImage { width, height, channels, data: ImageData::Raw { bits: 8, samples }, icc }
        })
    }

    /// The print-ready document: each page image covers its page (`size` in points).
    pub fn pdf(&self, pages: Vec<((f64, f64), PdfImage)>, title: &str) -> Result<Vec<u8>, String> {
        let pages: Vec<PdfPage> = pages
            .into_iter()
            .map(|((w, h), image)| PdfPage { width: w, height: h, images: vec![(image, Placement { x: 0.0, y: 0.0, width: w, height: h })] })
            .collect();
        let title = (!title.trim().is_empty()).then(|| title.to_owned());
        crate::print_pdf::write(&pages, &PdfInfo { title, output_intent: self.intent.clone() })
    }

    /// Samples per pixel the pages are converted to (3 RGB, 4 CMYK, 1 grey).
    pub fn channels(&self) -> usize {
        self.transform.channels()
    }

    /// The profile's description (or file name) for the job's summary.
    pub fn profile_name(&self) -> Option<&str> {
        self.intent.as_ref().map(|i| i.condition.as_str()).or(self.transform.description())
    }
}

/// A photo rendered for the page as the layout gets it from the export renderer.
pub fn is_print_ready(photo: &DeepImage) -> bool {
    photo.space == OutputSpace::Rec2020 && matches!(photo.samples, DeepSamples::U16(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_codecs::print::print_adjust;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lc-print-color-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A small, flat page of one 16-bit colour.
    fn page(w: u32, h: u32, v: [u16; 3]) -> Vec<u16> {
        (0..w * h).flat_map(|_| v).collect()
    }

    #[test]
    fn settings_default_and_round_trip() {
        let d = PrintColorSettings::default();
        assert_eq!(d.profile, PrintProfile::Managed { space: ManagedSpace::Srgb });
        assert!(d.black_point_compensation && d.sixteen_bit);
        let s = PrintColorSettings {
            profile: PrintProfile::Icc { path: "/Library/ColorSync/Profiles/Paper.icc".into() },
            intent: PrintIntent::Relative,
            brightness: 12.0,
            sharpen: PrintSharpen::High,
            media: PrintMedia::Matte,
            ..d.clone()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["profile"]["kind"], "icc");
        assert_eq!(json["intent"], "relative");
        assert_eq!(serde_json::from_value::<PrintColorSettings>(json).unwrap(), s);
        // partial and unknown-free JSON fills in the defaults
        let partial: PrintColorSettings = serde_json::from_str(r#"{"intent":"relative"}"#).unwrap();
        assert_eq!(partial, PrintColorSettings { intent: PrintIntent::Relative, ..d });
        assert!(serde_json::from_str::<PrintColorSettings>(r#"{"profile":{"kind":"cmyk"}}"#).is_err());
    }

    #[test]
    fn managed_srgb_page_becomes_a_16_bit_srgb_pdf_without_an_output_intent() {
        let job = PrintColorJob::new(&PrintColorSettings::default()).unwrap();
        let img = job.page_image(4, 2, &page(4, 2, [65535, 65535, 65535])).unwrap();
        assert_eq!((img.channels, img.width, img.height), (3, 4, 2));
        let ImageData::Raw { bits: 16, samples } = &img.data else { panic!("16-bit samples") };
        assert!(samples.chunks(2).all(|b| u16::from_be_bytes([b[0], b[1]]) > 65400), "white stays white");
        let pdf = job.pdf(vec![((288.0, 144.0), img)], "Prints").unwrap();
        let s = String::from_utf8_lossy(&pdf);
        assert!(s.contains("/ICCBased") && s.contains("/BitsPerComponent 16") && s.contains("/MediaBox [0 0 288 144]"));
        assert!(!s.contains("/OutputIntents"), "the printer converts: no output intent");
        assert!(s.contains("/Title (Prints)"));
    }

    #[test]
    fn a_printer_profile_converts_and_becomes_the_output_intent() {
        let dir = scratch("icc");
        let path = dir.join("Studio Glossy.icc");
        std::fs::write(&path, lightcraft_codecs::icc::write_named(NamedSpace::AdobeRgb)).unwrap();
        let settings =
            PrintColorSettings { profile: PrintProfile::Icc { path }, intent: PrintIntent::Relative, sixteen_bit: false, ..Default::default() };
        let job = PrintColorJob::new(&settings).unwrap();
        assert_eq!(job.channels(), 3);
        let img = job.page_image(2, 2, &page(2, 2, [0, 65535, 0])).unwrap();
        assert!(matches!(&img.data, ImageData::Raw { bits: 8, .. }));
        let pdf = job.pdf(vec![((100.0, 100.0), img)], "").unwrap();
        let s = String::from_utf8_lossy(&pdf);
        assert!(s.contains("/OutputIntents") && s.contains("/BitsPerComponent 8"), "{}", &s[..s.len().min(600)]);
        assert!(!s.contains("/Title"), "an empty title is left out");
        assert!(job.profile_name().is_some());
    }

    #[test]
    fn profile_errors_name_the_file() {
        let dir = scratch("bad");
        let garbage = dir.join("Broken Paper.icc");
        std::fs::write(&garbage, [0u8; 300]).unwrap();
        let err = PrintColorJob::new(&PrintColorSettings { profile: PrintProfile::Icc { path: garbage }, ..Default::default() }).unwrap_err();
        assert!(err.contains("Broken Paper"), "{err}");
        let missing =
            PrintColorJob::new(&PrintColorSettings { profile: PrintProfile::Icc { path: dir.join("nope.icc") }, ..Default::default() }).unwrap_err();
        assert!(missing.contains("nope.icc"), "{missing}");
    }

    #[test]
    fn print_adjustment_reaches_the_page() {
        let mid = [32768u16; 3];
        let plain = PrintColorJob::new(&PrintColorSettings::default()).unwrap().page_image(1, 1, &mid).unwrap();
        let bright = PrintColorJob::new(&PrintColorSettings { brightness: 60.0, ..Default::default() }).unwrap().page_image(1, 1, &mid).unwrap();
        let first = |i: &PdfImage| match &i.data {
            ImageData::Raw { samples, .. } => u16::from_be_bytes([samples[0], samples[1]]),
            ImageData::Jpeg(_) | ImageData::Flate { .. } => 0,
        };
        assert!(first(&bright) > first(&plain) + 1500, "{} vs {}", first(&bright), first(&plain));
        assert!(print_adjust(0.5, 60.0, 0.0) > 0.5);
    }

    #[test]
    fn print_sharpening_follows_the_setting() {
        // a hard vertical edge in a 16-bit Rec2020 photo
        let (w, h) = (8usize, 4usize);
        let samples: Vec<u16> = (0..w * h).flat_map(|i| if i % w < w / 2 { [20000u16; 3] } else { [40000u16; 3] }).collect();
        let photo = DeepImage { width: w, height: h, space: OutputSpace::Rec2020, samples: DeepSamples::U16(samples) };
        assert!(is_print_ready(&photo));
        let sharpened = |s: PrintSharpen| {
            let job = PrintColorJob::new(&PrintColorSettings { sharpen: s, media: PrintMedia::Matte, ..Default::default() }).unwrap();
            let mut p = photo.clone();
            job.sharpen_photo(&mut p);
            p
        };
        assert_eq!(sharpened(PrintSharpen::Off), photo);
        let edge = |p: &DeepImage| match &p.samples {
            DeepSamples::U16(v) => (v[(w / 2 - 1) * 3], v[(w / 2) * 3]),
            DeepSamples::F32(_) => (0, 0),
        };
        let (dark, light) = edge(&sharpened(PrintSharpen::High));
        assert!(dark < 20000 && light > 40000, "the edge gets more contrast: {dark} {light}");
    }

    #[test]
    fn validation_refuses_out_of_range_settings() {
        assert!(PrintColorSettings::default().validate().is_ok());
        for (b, c) in [(101.0, 0.0), (0.0, -100.5), (f32::NAN, 0.0), (0.0, f32::INFINITY)] {
            assert!(PrintColorSettings { brightness: b, contrast: c, ..Default::default() }.validate().is_err(), "{b} {c}");
        }
        assert!(PrintColorSettings { profile: PrintProfile::Icc { path: PathBuf::new() }, ..Default::default() }.validate().is_err());
        assert!(PrintColorSettings { brightness: -100.0, contrast: 100.0, ..Default::default() }.validate().is_ok());
    }

    #[test]
    fn hostile_pages_are_errors() {
        let job = PrintColorJob::new(&PrintColorSettings::default()).unwrap();
        assert!(job.page_image(0, 1, &[]).is_err());
        assert!(job.page_image(2, 2, &[0; 11]).is_err());
        assert!(job.page_image(u32::MAX, u32::MAX, &[0; 3]).is_err());
        let img = job.page_image(1, 1, &[0; 3]).unwrap();
        assert!(job.pdf(vec![((f64::NAN, 10.0), img)], "x").is_err());
        assert!(job.pdf(vec![], "x").is_err());
    }
}
