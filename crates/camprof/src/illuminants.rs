// Ported from RADcamProfiles (github.com/OneSlips/RADcamProfiles, commit dd8003f, crates/engine/src/illuminants.rs),
// Copyright (c) 2026 RAD Films - Roman Alurkoff, MIT; relicensed MIT OR Apache-2.0 by its author.
// Modified by the porting contributors (2026): no panics on bad input, the app's frame type.
//! Illuminants: EXIF light source identifiers, standard spectra and blackbody
//! rendering.
//!
//! Measurement files usually name their light source rather than shipping a
//! measured spectrum. This module turns those names into spectra on the
//! engine's internal 0..100 scale, and provides the white point a named
//! illuminant implies under a given observer.

use crate::color::ColorError;
use crate::data::illuminants as ill_data;
use crate::linalg::Vec3;
use crate::observer::Observer;
use crate::spectrum::Spectrum;

/// EXIF light source tags, as written into DCP files and read from RAW
/// metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ExifLightSource {
    /// Unknown or unrecorded.
    Unknown = 0,
    /// Daylight, roughly D65.
    Daylight = 1,
    /// Fluorescent lamp.
    Fluorescent = 2,
    /// Tungsten filament, roughly Standard Light A.
    Tungsten = 3,
    /// Electronic flash.
    Flash = 4,
    /// Fine weather daylight.
    FineWeather = 9,
    /// Cloudy weather.
    CloudyWeather = 10,
    /// Open shade.
    Shade = 11,
    /// Daylight fluorescent, D 5700-7100K.
    DaylightFluorescent = 12,
    /// Day white fluorescent, N 4600-5500K.
    DayWhiteFluorescent = 13,
    /// Cool white fluorescent, W 3800-4500K.
    CoolWhiteFluorescent = 14,
    /// White fluorescent, WW 3250-3800K.
    WhiteFluorescent = 15,
    /// Warm white fluorescent, L 2600-3250K.
    WarmWhiteFluorescent = 16,
    /// CIE Standard Illuminant A.
    StandardLightA = 17,
    /// CIE Standard Illuminant B.
    StandardLightB = 18,
    /// CIE Standard Illuminant C.
    StandardLightC = 19,
    /// CIE illuminant D55.
    D55 = 20,
    /// CIE illuminant D65.
    D65 = 21,
    /// CIE illuminant D75.
    D75 = 22,
    /// CIE illuminant D50.
    D50 = 23,
    /// ISO studio tungsten.
    IsoStudioTungsten = 24,
    /// Some other light source.
    Other = 255,
}

/// Every EXIF light source the engine recognises, in tag order.
pub const EXIF_LIGHT_SOURCES: [ExifLightSource; 22] = [
    ExifLightSource::Unknown,
    ExifLightSource::Daylight,
    ExifLightSource::Fluorescent,
    ExifLightSource::Tungsten,
    ExifLightSource::Flash,
    ExifLightSource::FineWeather,
    ExifLightSource::CloudyWeather,
    ExifLightSource::Shade,
    ExifLightSource::DaylightFluorescent,
    ExifLightSource::DayWhiteFluorescent,
    ExifLightSource::CoolWhiteFluorescent,
    ExifLightSource::WhiteFluorescent,
    ExifLightSource::WarmWhiteFluorescent,
    ExifLightSource::StandardLightA,
    ExifLightSource::StandardLightB,
    ExifLightSource::StandardLightC,
    ExifLightSource::D55,
    ExifLightSource::D65,
    ExifLightSource::D75,
    ExifLightSource::D50,
    ExifLightSource::IsoStudioTungsten,
    ExifLightSource::Other,
];

impl ExifLightSource {
    /// The tag value.
    pub fn tag(self) -> u8 {
        self as u8
    }

    /// Build from a raw tag value.
    pub fn from_tag(tag: u8) -> Option<Self> {
        EXIF_LIGHT_SOURCES.iter().copied().find(|&l| l.tag() == tag)
    }

    /// The canonical EXIF name, matching the spelling used in DCP files.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Daylight => "Daylight",
            Self::Fluorescent => "Fluorescent",
            Self::Tungsten => "Tungsten",
            Self::Flash => "Flash",
            Self::FineWeather => "FineWeather",
            Self::CloudyWeather => "CloudyWeather",
            Self::Shade => "Shade",
            Self::DaylightFluorescent => "DaylightFluorescent",
            Self::DayWhiteFluorescent => "DayWhiteFluorescent",
            Self::CoolWhiteFluorescent => "CoolWhiteFluorescent",
            Self::WhiteFluorescent => "WhiteFluorescent",
            Self::WarmWhiteFluorescent => "WarmWhiteFluorescent",
            Self::StandardLightA => "StdA",
            Self::StandardLightB => "StdB",
            Self::StandardLightC => "StdC",
            Self::D55 => "D55",
            Self::D65 => "D65",
            Self::D75 => "D75",
            Self::D50 => "D50",
            Self::IsoStudioTungsten => "ISOStudioTungsten",
            Self::Other => "Other",
        }
    }

    /// Parse a name as written in a profile or on the command line.
    ///
    /// Accepts the canonical names plus the long forms used by EXIF
    /// documentation, case insensitively.
    pub fn from_name(name: &str) -> Option<Self> {
        let n = name.trim().to_ascii_lowercase().replace([' ', '-', '_'], "");
        let src = match n.as_str() {
            "unknown" => Self::Unknown,
            "daylight" => Self::Daylight,
            "fluorescent" => Self::Fluorescent,
            "tungsten" => Self::Tungsten,
            "flash" => Self::Flash,
            "fineweather" => Self::FineWeather,
            "cloudyweather" => Self::CloudyWeather,
            "shade" => Self::Shade,
            "daylightfluorescent" | "fluorescentdaylight" => Self::DaylightFluorescent,
            "daywhitefluorescent" => Self::DayWhiteFluorescent,
            "coolwhitefluorescent" => Self::CoolWhiteFluorescent,
            "whitefluorescent" => Self::WhiteFluorescent,
            "warmwhitefluorescent" => Self::WarmWhiteFluorescent,
            "stda" | "standardlighta" | "a" => Self::StandardLightA,
            "stdb" | "standardlightb" | "b" => Self::StandardLightB,
            "stdc" | "standardlightc" | "c" => Self::StandardLightC,
            "d55" => Self::D55,
            "d65" => Self::D65,
            "d75" => Self::D75,
            "d50" => Self::D50,
            "isostudiotungsten" => Self::IsoStudioTungsten,
            "other" => Self::Other,
            _ => return None,
        };
        Some(src)
    }
}

impl core::fmt::Display for ExifLightSource {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}

/// The spectral range the engine renders blackbodies over.
pub const BLACKBODY_START: f64 = 300.0;
/// Upper end of the blackbody rendering range.
pub const BLACKBODY_END: f64 = 830.0;
/// Band spacing used when rendering blackbodies and interpolating illuminants.
pub const BLACKBODY_SPACING: f64 = 5.0;

/// Spectral radiance of a blackbody at `temp` kelvin and wavelength `nm`,
/// in the engine's arbitrary but internally consistent units.
fn planck(temp: f64, nm: f64) -> f64 {
    let l = nm / 1.0e9;
    let f = 3.7418e-16 / (l.powi(5) * ((0.014388 / (l * temp)).exp() - 1.0));
    f / 1.0e8
}

/// Render a blackbody radiator normalised to 1.0 at 560 nm.
///
/// Colour temperatures given as `"3200K"` on the command line are rendered
/// this way rather than looked up, so any temperature is expressible.
pub fn render_blackbody(temp: f64) -> Spectrum {
    let band_count = ((BLACKBODY_END - BLACKBODY_START) / BLACKBODY_SPACING).round() as usize + 1;
    let reference = planck(temp, 560.0);
    let amps: Vec<f64> = (0..band_count)
        .map(|i| {
            let nm = BLACKBODY_START + i as f64 * BLACKBODY_SPACING;
            if reference == 0.0 { 0.0 } else { planck(temp, nm) / reference }
        })
        .collect();
    Spectrum::from_uniform(&amps, BLACKBODY_START, BLACKBODY_END, BLACKBODY_SPACING)
}

/// Parse a colour temperature such as `"3200K"` or `"2854k"`.
pub fn parse_temperature(name: &str) -> Option<f64> {
    let trimmed = name.trim();
    let lower = trimmed.to_ascii_lowercase();
    let digits = lower.strip_suffix('k')?;
    let temp: f64 = digits.trim().parse().ok()?;
    if temp > 0.0 { Some(temp) } else { None }
}

/// All spectral illuminant names the engine knows, in listing order.
pub fn known_names() -> Vec<&'static str> {
    ill_data::TABLES.iter().map(|(n, _)| *n).collect()
}

/// Build the spectrum of a named illuminant, or of a blackbody when the name
/// ends in `K`.
///
/// The returned spectrum is normalised so its peak is 1.0, matching the
/// engine's internal 0..100 white scale after multiplication.
pub fn spectrum(name: &str) -> Result<Spectrum, ColorError> {
    if let Some(temp) = parse_temperature(name) {
        return Ok(render_blackbody(temp));
    }
    let table = ill_data::lookup(name).ok_or_else(|| ColorError::UnknownIlluminant(name.trim().to_string()))?;
    build(table.name, &table)
}

/// Turn a published table into a spectrum on the engine's 0..1 white scale.
///
/// The tabulated amplitudes are relative and were published against a fixed
/// divisor, so the result is normalised by that divisor and then scaled so the
/// distribution peaks at one. Doing it here rather than relying on callers
/// means [`ExifLightSource::name`] and the spectrum always describe the same
/// illuminant.
fn build(_name: &str, table: &ill_data::IlluminantTable) -> Result<Spectrum, ColorError> {
    let end = table.start + table.spacing * (table.amps.len() - 1) as f64;
    let mut s = Spectrum::from_uniform(table.amps, table.start, end, table.spacing);
    if table.scale != 0.0 && table.scale != 1.0 {
        s.scale(1.0 / table.scale);
    }
    let peak = s.max();
    if peak > 0.0 {
        s.scale(1.0 / peak);
    }
    Ok(s)
}

/// The spectrum for an EXIF light source tag, if one is known.
pub fn spectrum_for(src: ExifLightSource) -> Option<Spectrum> {
    let table = ill_data::TABLES.iter().map(|(_, t)| t).find(|t| t.exif_tag == src.tag() as i32)?;
    build(table.name, table).ok()
}

/// Resample a spectrum onto the standard 300..830 nm, 5 nm grid.
///
/// Values outside the original range are extrapolated as zero, which is what
/// the integration routines expect.
pub fn to_standard_grid(s: &Spectrum) -> Spectrum {
    let band_count = ((BLACKBODY_END - BLACKBODY_START) / BLACKBODY_SPACING).round() as usize + 1;
    let amps: Vec<f64> = (0..band_count)
        .map(|i| {
            let nm = BLACKBODY_START + i as f64 * BLACKBODY_SPACING;
            s.at(nm)
        })
        .collect();
    Spectrum::from_uniform(&amps, BLACKBODY_START, BLACKBODY_END, BLACKBODY_SPACING)
}

/// The white point a spectrum implies for an observer, normalised to `Y = 1`.
pub fn white_point(observer: &Observer, illuminant: &Spectrum) -> Vec3 {
    let xyz = observer.spec_to_xyz(illuminant);
    let y = xyz[1];
    if y == 0.0 {
        return Vec3::new(0.0, 1.0, 0.0);
    }
    Vec3::new(xyz[0] / y, 1.0, xyz[2] / y)
}

/// The white point of a named illuminant under a named observer.
///
/// Convenience for callers that only have names, e.g. when reading a
/// calibration illuminant out of a measurement file header.
pub fn white_point_by_name(observer_name: &str, illuminant_name: &str) -> Result<Vec3, ColorError> {
    let obs = Observer::by_name(observer_name)?;
    let ill = spectrum(illuminant_name)?;
    Ok(white_point(&obs, &ill))
}

/// Correlated colour temperature of a white point, in kelvin.
///
/// Found by golden-section search over rendered blackbodies, comparing
/// chromaticity in `u'v'`, which is the usual Robertson-style approximation.
pub fn correlated_color_temperature(observer: &Observer, xyz: Vec3) -> f64 {
    use crate::color::xyz_to_uvy;
    let target = xyz_to_uvy({
        let y = xyz[1];
        if y == 0.0 { xyz } else { Vec3::new(xyz[0] / y, 1.0, xyz[2] / y) }
    });
    let distance = |temp: f64| -> f64 {
        let bb = render_blackbody(temp);
        let uv = xyz_to_uvy(observer.spec_to_xyz(&bb));
        ((uv[0] - target[0]).powi(2) + (uv[1] - target[1]).powi(2)).sqrt()
    };
    // Coarse scan then refine, which is robust without derivative information.
    let mut best = (2000.0_f64, distance(2000.0));
    let mut t = 2000.0;
    while t <= 20000.0 {
        let d = distance(t);
        if d < best.1 {
            best = (t, d);
        }
        t += 100.0;
    }
    let mut lo = (best.0 - 100.0).max(1000.0);
    let mut hi = best.0 + 100.0;
    for _ in 0..40 {
        let m1 = lo + (hi - lo) / 3.0;
        let m2 = hi - (hi - lo) / 3.0;
        if distance(m1) < distance(m2) {
            hi = m2;
        } else {
            lo = m1;
        }
    }
    (lo + hi) * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_exif_tag_roundtrips() {
        for src in EXIF_LIGHT_SOURCES {
            assert_eq!(ExifLightSource::from_tag(src.tag()), Some(src));
            assert_eq!(ExifLightSource::from_name(src.name()), Some(src));
        }
        // exif names are unique
        {
            let mut tags: Vec<u8> = EXIF_LIGHT_SOURCES.iter().map(|s| s.tag()).collect();
            tags.sort_unstable();
            tags.dedup();
            assert_eq!(tags.len(), EXIF_LIGHT_SOURCES.len());
        }
        // exif tagged spectra exist for standard sources
        {
            for src in [ExifLightSource::D50, ExifLightSource::D65, ExifLightSource::D75, ExifLightSource::StandardLightA] {
                assert!(spectrum_for(src).is_some(), "{src}");
            }
        }
    }

    #[test]
    fn every_known_illuminant_builds() {
        for name in known_names() {
            let s = spectrum(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(s.len() > 1, "{name}");
            assert!(s.max() > 0.0, "{name}");
            assert!(s.amplitudes().iter().all(|a| a.is_finite()), "{name}");
        }
    }

    #[test]
    fn temperature_parsing() {
        assert_eq!(parse_temperature("3200K"), Some(3200.0));
        assert_eq!(parse_temperature(" 2854 k "), Some(2854.0));
        assert_eq!(parse_temperature("D65"), None);
        assert_eq!(parse_temperature("0K"), None);
        // name parsing is forgiving
        {
            assert_eq!(ExifLightSource::from_name("  standard light a "), Some(ExifLightSource::StandardLightA));
            assert_eq!(ExifLightSource::from_name("d-65"), Some(ExifLightSource::D65));
            assert_eq!(ExifLightSource::from_name("nope"), None);
        }
        // unknown illuminant reports its name
        {
            let e = spectrum("NorthLight").unwrap_err();
            assert_eq!(e, ColorError::UnknownIlluminant("NorthLight".into()));
        }
    }

    #[test]
    fn blackbody_peaks_at_560_nm_after_normalisation() {
        let s = render_blackbody(5000.0);
        assert!((s.at(560.0) - 1.0).abs() < 1.0e-12);
        assert!(s.max() >= 1.0);
    }

    #[test]
    fn hotter_blackbody_is_bluer() {
        let obs = Observer::by_name("1931_2").unwrap();
        let warm = white_point(&obs, &render_blackbody(2800.0));
        let cool = white_point(&obs, &render_blackbody(6500.0));
        let xw = warm[0] / warm.sum();
        let xc = cool[0] / cool.sum();
        assert!(xc < xw, "cool x={xc} should be below warm x={xw}");
    }

    #[test]
    fn d50_white_point_is_close_to_the_constant() {
        let obs = Observer::by_name("1931_2").unwrap();
        let wp = white_point(&obs, &spectrum("D50").unwrap());
        let xy = crate::color::xyz_to_xy(wp);
        // Published D50 chromaticity is about (0.3457, 0.3585).
        assert!((xy[0] - 0.3457).abs() < 3.0e-3, "{xy:?}");
        assert!((xy[1] - 0.3585).abs() < 3.0e-3, "{xy:?}");
    }

    #[test]
    fn standard_illuminant_a_is_incandescent() {
        let obs = Observer::by_name("1931_2").unwrap();
        let wp = white_point(&obs, &spectrum("StdA").unwrap());
        let xy = crate::color::xyz_to_xy(wp);
        // Illuminant A chromaticity is about (0.4476, 0.4074).
        assert!((xy[0] - 0.4476).abs() < 5.0e-3, "{xy:?}");
        assert!((xy[1] - 0.4074).abs() < 5.0e-3, "{xy:?}");
    }

    #[test]
    fn cct_of_blackbody_recovers_temperature() {
        let obs = Observer::by_name("1931_2").unwrap();
        for temp in [2856.0, 4000.0, 6500.0] {
            let xyz = obs.spec_to_xyz(&render_blackbody(temp));
            let found = correlated_color_temperature(&obs, xyz);
            assert!((found - temp).abs() / temp < 0.03, "asked {temp}, got {found}");
        }
    }

    #[test]
    fn standard_grid_covers_the_full_range() {
        let s = Spectrum::from_uniform(&[1.0; 11], 400.0, 500.0, 10.0);
        let g = to_standard_grid(&s);
        assert_eq!(g.start(), BLACKBODY_START);
        assert_eq!(g.end(), BLACKBODY_END);
        assert!((g.at(450.0) - 1.0).abs() < 1.0e-12);
        assert_eq!(g.at(300.0), 0.0);
    }
}
