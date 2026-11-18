// Ported from RADcamProfiles (github.com/OneSlips/RADcamProfiles, commit dd8003f, crates/engine/src/observer.rs),
// Copyright (c) 2026 RAD Films - Roman Alurkoff, MIT; relicensed MIT OR Apache-2.0 by its author.
// Modified by the porting contributors (2026): no panics on bad input, the app's frame type.
//! CIE standard observers and the conversion from spectra to tristimulus.
//!
//! An observer bundles the three colour-matching functions. The engine works
//! with any of the five observers the original tooling supported, because the
//! choice measurably changes the resulting profile for narrow-band sources.

use crate::color::ColorError;
use crate::data::cmf;
use crate::linalg::Vec3;
use crate::spectrum::Spectrum;

/// Canonical observer names, in the order the UI lists them.
pub const OBSERVER_NAMES: [&str; 1] = ["1931_2"];

/// The variables `k` in the tristimulus equations, aligned with an observer.
#[derive(Clone, Debug, PartialEq)]
pub struct Observer {
    name: String,
    xbar: Spectrum,
    ybar: Spectrum,
    zbar: Spectrum,
}

impl Observer {
    /// Build an observer from the standard tables.
    ///
    /// Accepts the canonical names plus the historical aliases
    /// `judd_vos_1978_2` / `juddvos_1978_2`. Names are matched case
    /// insensitively and `-` is accepted where `_` is used.
    pub fn by_name(name: &str) -> Result<Self, ColorError> {
        let normalized = name.trim().to_ascii_lowercase().replace('-', "_");
        let normalized = normalized.strip_prefix("cie_").unwrap_or(&normalized).to_string();
        let canonical = match normalized.as_str() {
            "1931_2" => "1931_2",
            other => {
                return Err(ColorError::UnknownObserver(other.to_string()));
            }
        };
        let table = cmf::lookup(canonical).ok_or_else(|| ColorError::UnknownObserver(canonical.to_string()))?;
        let n = table.rows.len();
        let mut x = Vec::with_capacity(n);
        let mut y = Vec::with_capacity(n);
        let mut z = Vec::with_capacity(n);
        for row in table.rows {
            x.push(row[1]);
            y.push(row[2]);
            z.push(row[3]);
        }
        let end = table.start + table.spacing * (n - 1) as f64;
        Ok(Self {
            name: canonical.to_string(),
            xbar: Spectrum::from_uniform(&x, table.start, end, table.spacing),
            ybar: Spectrum::from_uniform(&y, table.start, end, table.spacing),
            zbar: Spectrum::from_uniform(&z, table.start, end, table.spacing),
        })
    }

    /// The canonical name this observer was built from.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The three colour-matching functions, in order.
    pub fn bars(&self) -> [&Spectrum; 3] {
        [&self.xbar, &self.ybar, &self.zbar]
    }

    /// The luminance function.
    pub fn ybar(&self) -> &Spectrum {
        &self.ybar
    }

    /// Scale factor that normalises a reflecting measurement to `Y = 1`.
    ///
    /// The engine keeps illuminant spectra on a 0..100 scale, so the reciprocal
    /// of the luminous integral is what turns raw integrals into tristimulus
    /// values relative to the illuminant.
    pub fn weight(&self, illuminant: &Spectrum) -> f64 {
        let y = self.ybar.multiply_and_integrate(illuminant, -1.0, -1.0);
        if y == 0.0 { 0.0 } else { 1.0 / y }
    }

    /// Tristimulus of a reflecting sample under `illuminant`.
    ///
    /// `weight` comes from [`Observer::weight`]; the fixed 0.01 factor keeps
    /// the result on the engine's internal scale, where an illuminant of 100
    /// at its peak yields a white with `Y = 1`.
    pub fn spec_to_xyz_ill(&self, sample: &Spectrum, illuminant: &Spectrum, weight: f64) -> Vec3 {
        let reflected = sample.multiplied(illuminant);
        Vec3::new(
            reflected.multiply_and_integrate(&self.xbar, -1.0, -1.0) * weight * 0.01,
            reflected.multiply_and_integrate(&self.ybar, -1.0, -1.0) * weight * 0.01,
            reflected.multiply_and_integrate(&self.zbar, -1.0, -1.0) * weight * 0.01,
        )
    }

    /// Tristimulus of a self-luminous spectrum, i.e. an illuminant.
    pub fn spec_to_xyz(&self, sample: &Spectrum) -> Vec3 {
        Vec3::new(
            sample.multiply_and_integrate(&self.xbar, -1.0, -1.0) * 0.01,
            sample.multiply_and_integrate(&self.ybar, -1.0, -1.0) * 0.01,
            sample.multiply_and_integrate(&self.zbar, -1.0, -1.0) * 0.01,
        )
    }
}

/// A luminous sample measured directly rather than as a reflectance.
///
/// Used for emissive targets and for illuminant spectra that were measured in
/// absolute units.
pub fn spec_to_xyz_normalised(observer: &Observer, sample: &Spectrum) -> Vec3 {
    let xyz = observer.spec_to_xyz(sample);
    if xyz.max() == 0.0 { xyz } else { xyz.normalized() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_canonical_names_resolve() {
        for n in OBSERVER_NAMES {
            let o = Observer::by_name(n).unwrap_or_else(|e| panic!("{n}: {e}"));
            assert_eq!(o.name(), n);
        }
        // aliases and case are accepted
        {
            assert!(Observer::by_name("CIE_1931_2").is_ok());
            assert!(Observer::by_name(" 1931-2 ").is_ok());
            assert!(Observer::by_name("nonsense").is_err());
        }
    }

    #[test]
    fn equal_energy_white_is_neutral_for_every_observer() {
        for n in OBSERVER_NAMES {
            let o = Observer::by_name(n).unwrap();
            let e = Spectrum::from_uniform(&[1.0; 95], 360.0, 830.0, 5.0);
            let xyz = o.spec_to_xyz(&e);
            assert!(xyz.is_finite() && xyz.max() > 0.0, "{n}");
            let xy = crate::color::xyz_to_xy(xyz);
            // Equal-energy white sits at x = y = 1/3 for any reasonable observer.
            assert!((xy[0] - 1.0 / 3.0).abs() < 0.02, "{n}: x={}", xy[0]);
            assert!((xy[1] - 1.0 / 3.0).abs() < 0.02, "{n}: y={}", xy[1]);
        }
    }

    #[test]
    fn ybar_alone_is_the_luminance_response() {
        let o = Observer::by_name("1931_2").unwrap();
        let e = Spectrum::from_uniform(&[1.0; 95], 360.0, 830.0, 5.0);
        let xyz = o.spec_to_xyz(&e);
        // Tristimulus values carry the engine's fixed 0.01 scale.
        let y = e.multiply_and_integrate(o.ybar(), -1.0, -1.0) * 0.01;
        assert!((xyz[1] - y).abs() < 1.0e-12);
    }

    #[test]
    fn observer_dependent_weight_normalises_illuminant() {
        let o = Observer::by_name("1931_2").unwrap();
        // A flat illuminant on the engine's 0..100 scale.
        let e = Spectrum::from_uniform(&[100.0; 95], 360.0, 830.0, 5.0);
        let w = o.weight(&e);
        let sample = Spectrum::from_uniform(&[1.0; 95], 360.0, 830.0, 5.0);
        let xyz = o.spec_to_xyz_ill(&sample, &e, w);
        assert!(xyz.is_finite());
        // A perfect reflector reproduces the illuminant, and the weight leaves
        // it at the engine's 0.01 reference scale.
        assert!((xyz[1] - 0.01).abs() < 1.0e-12, "{xyz:?}");
    }
}
