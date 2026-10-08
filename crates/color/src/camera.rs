//! Own camera calibration: XYZ → unbalanced, black-subtracted sensor RGB.
//! No camera matrices are bundled. Models are measured from independently sourced chart data.

use crate::{D50, D65, Mat3, REC2020, Xy, bradford, cct};
use serde::{Deserialize, Serialize};

/// Two measured illuminants; matrix interpolation is linear in reciprocal Kelvin.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraModel {
    pub temperatures: [f64; 2],
    pub xyz_to_camera: [Mat3; 2],
    /// Standard file calibration may additionally describe reference-camera calibration and
    /// forward matrices. Own chart profiles normally omit these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<ReferenceCalibration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceCalibration {
    pub camera_calibration: [Mat3; 2],
    pub analog_balance: [f64; 3],
    pub forward: [Option<Mat3>; 2],
}

fn blend(pair: [Mat3; 2], weight: f64) -> Mat3 {
    Mat3(std::array::from_fn(|i| std::array::from_fn(|j| weight * pair[0].0[i][j] + (1.0 - weight) * pair[1].0[i][j])))
}

pub fn valid_matrix(m: Mat3) -> bool {
    let norm = |m: Mat3| m.0.iter().map(|r| r.iter().map(|x| x.abs()).sum::<f64>()).fold(0.0, f64::max);
    m.0.iter().flatten().all(|v| v.is_finite() && v.abs() <= 100.0) && m.inverse().is_some_and(|inv| norm(m) * norm(inv) < 1000.0)
}

impl CameraModel {
    pub fn validate(&self) -> Result<(), String> {
        let [warm, day] = self.temperatures;
        if !(2000.0..=50000.0).contains(&warm) || !(warm..=50000.0).contains(&day) || day - warm < 1.0 {
            return Err("calibration requires distinct ordered illuminants (2000–50000 K)".into());
        }
        if let Some(r) = self.reference {
            if r.analog_balance.iter().any(|v| !v.is_finite() || *v <= 0.0 || *v > 100.0) || r.camera_calibration.iter().any(|m| !valid_matrix(*m)) {
                return Err("invalid reference-camera calibration".into());
            }
            for fm in r.forward.into_iter().flatten() {
                if !valid_matrix(fm) || fm.apply([1.0; 3]).iter().zip(D50.to_xyz()).any(|(a, b)| (a - b).abs() > 0.02) {
                    return Err("forward matrix must map balanced camera white to XYZ D50".into());
                }
            }
        }
        // An invertible pair can interpolate through a singular matrix.
        for i in 0..=64 {
            let t = 1.0 / ((1.0 - i as f64 / 64.0) / warm + (i as f64 / 64.0) / day);
            let m = self.matrix(t);
            let n = m.apply(cct::temp_tint_to_xy(t, 0.0).to_xyz());
            if !valid_matrix(m) || n.iter().any(|x| !x.is_finite() || *x <= 1e-6) {
                return Err("invalid, ill-conditioned or non-positive camera calibration".into());
            }
        }
        Ok(())
    }

    fn weight(&self, temp: f64) -> f64 {
        let [a, b] = self.temperatures;
        let denom = 1.0 / a - 1.0 / b;
        if denom.abs() > 1e-12 { ((1.0 / temp - 1.0 / b) / denom).clamp(0.0, 1.0) } else { 1.0 }
    }

    fn ab_cc(&self, temp: f64) -> Mat3 {
        match self.reference {
            Some(r) => Mat3::diag(r.analog_balance[0], r.analog_balance[1], r.analog_balance[2]).mul(&blend(r.camera_calibration, self.weight(temp))),
            None => Mat3::IDENTITY,
        }
    }

    pub fn matrix(&self, temp: f64) -> Mat3 {
        self.ab_cc(temp).mul(&blend(self.xyz_to_camera, self.weight(temp)))
    }

    /// Iterative camera-neutral inversion. A failure is not a measured Kelvin value.
    pub fn neutral_xy(&self, neutral: [f64; 3]) -> Option<Xy> {
        if neutral.iter().any(|v| !v.is_finite() || *v <= 0.0) {
            return None;
        }
        let mut xy = D65;
        for _ in 0..100 {
            let t = cct::xy_to_temp_tint(xy).0;
            let matrix = self.matrix(t);
            if !valid_matrix(matrix) {
                return None;
            }
            let xyz = matrix.inverse()?.apply(neutral);
            if xyz.iter().any(|v| !v.is_finite()) || xyz.iter().sum::<f64>() <= 0.0 {
                return None;
            }
            let next = Xy::from_xyz(xyz);
            if next.x <= 0.0 || next.y <= 0.0 || next.x + next.y >= 1.0 {
                return None;
            }
            if (next.x - xy.x).abs() + (next.y - xy.y).abs() < 1e-8 {
                return Some(next);
            }
            xy = Xy::new((xy.x + next.x) * 0.5, (xy.y + next.y) * 0.5);
        }
        None
    }

    /// Sensor RGB → linear Rec.2020 D65. Green-normalised neutral maps to one.
    /// This fixes the exposure scale across illuminants instead of normalising by a scene maximum.
    pub fn transform(&self, white: Xy) -> Option<Mat3> {
        let temp = cct::xy_to_temp_tint(white).0;
        let cm = self.matrix(temp);
        if !valid_matrix(cm) {
            return None;
        }
        let n = cm.apply(white.to_xyz());
        if n.iter().any(|v| !v.is_finite() || *v <= 1e-8) {
            return None;
        }
        if let Some(r) = self.reference {
            let fm = match r.forward {
                [Some(a), Some(b)] => Some(blend([a, b], self.weight(temp))),
                [Some(a), None] | [None, Some(a)] => Some(a),
                _ => None,
            };
            if let Some(fm) = fm {
                if !valid_matrix(fm) {
                    return None;
                }
                let inv = self.ab_cc(temp).inverse()?;
                let rn = inv.apply(n.map(|v| v / n[1]));
                if rn.iter().any(|v| !v.is_finite() || *v <= 1e-8) {
                    return None;
                }
                return Some(REC2020.from_xyz().mul(&bradford(D50, D65)).mul(&fm).mul(&Mat3::diag(1.0 / rn[0], 1.0 / rn[1], 1.0 / rn[2])).mul(&inv));
            }
        }
        let m = REC2020.from_xyz().mul(&bradford(white, D65)).mul(&cm.inverse()?);
        Some(Mat3(m.0.map(|r| r.map(|v| v * n[1]))))
    }
}

/// Model and invertible as-shot transform travelling with decoded pixels (including smart previews).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraWb {
    pub model: CameraModel,
    pub from_working: Mat3,
}

impl CameraWb {
    pub fn correction(&self, temp: f64, tint: f64) -> Option<Mat3> {
        if !(2000.0..=50000.0).contains(&temp) || !(-150.0..=150.0).contains(&tint) {
            return None;
        }
        let m = self.model.transform(cct::temp_tint_to_xy(temp, tint))?.mul(&self.from_working);
        // WB changes chromaticity, not the luminance of the as-shot neutral.
        let y = REC2020.luma().into_iter().zip(m.apply([1.0; 3])).map(|(a, b)| a * b).sum::<f64>();
        (y.is_finite() && y > 1e-8).then(|| Mat3(m.0.map(|r| r.map(|v| v / y))))
    }
}

/// Source/rights information is part of a profile, not a side effect of its file name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub author: String,
    pub source: String,
    pub license: String,
    pub created: String,
    pub chart: String,
    pub reference: String,
}

impl Provenance {
    pub fn validate(&self) -> Result<(), String> {
        for value in [&self.author, &self.source, &self.license, &self.created, &self.chart, &self.reference] {
            if value.trim().is_empty() || value.len() > 4096 {
                return Err("profile provenance fields must be nonempty and at most 4096 bytes".into());
            }
        }
        Ok(())
    }
}

/// Own calibration format, deliberately distinct from JPEG look profiles and DCPs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraCalibration {
    pub version: u32,
    pub make: String,
    pub model: String,
    pub provenance: Provenance,
    pub calibration: CameraModel,
    /// SHA-256 identifiers of originals used for training, never validation.
    pub training_captures: Vec<String>,
}

impl CameraCalibration {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.make.trim().is_empty() || self.model.trim().is_empty() || self.make.len() > 256 || self.model.len() > 256 {
            return Err("invalid camera calibration identity/version".into());
        }
        self.provenance.validate()?;
        if self.training_captures.len() < 2
            || self.training_captures.len() > 256
            || self.training_captures.iter().any(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("record 2–256 original SHA-256 training capture identifiers".into());
        }
        let mut ids = std::collections::HashSet::new();
        if self.training_captures.iter().any(|id| !ids.insert(id.to_ascii_lowercase())) {
            return Err("duplicate training original in calibration profile".into());
        }
        self.calibration.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn model() -> CameraModel {
        CameraModel {
            reference: None,
            temperatures: [2856.0, 6504.0],
            xyz_to_camera: [
                Mat3([[0.8, 0.15, 0.04], [-0.2, 1.15, 0.1], [0.01, -0.04, 0.7]]),
                Mat3([[0.75, 0.20, 0.03], [-0.15, 1.1, 0.10], [0.03, -0.05, 0.75]]),
            ],
        }
    }
    #[test]
    fn dual_light_neutral_wb_tint_and_exposure() {
        let model = model();
        model.validate().unwrap();
        for temp in [2856.0, 4000.0, 5000.0, 6504.0] {
            for tint in [-20.0, 0.0, 20.0] {
                let xy = cct::temp_tint_to_xy(temp, tint);
                let n = model.matrix(cct::xy_to_temp_tint(xy).0).apply(xy.to_xyz());
                let back = model.neutral_xy(n).unwrap();
                assert!((xy.x - back.x).abs() + (xy.y - back.y).abs() < 1e-5);
                let m = model.transform(xy).unwrap();
                let grey = m.apply(n.map(|v| v / n[1] * 0.18));
                for v in grey {
                    assert!((v - 0.18).abs() < 1e-7, "{temp}/{tint}: {grey:?}");
                }
                let wb = CameraWb { model, from_working: m.inverse().unwrap() };
                for sign in [-1.0, 1.0] {
                    let c = wb.correction(temp, tint + sign * 20.0).unwrap().apply([0.18; 3]);
                    assert!((c[0] + c[2] - 2.0 * c[1]) * sign > 0.0, "{c:?}");
                    let y = REC2020.luma().iter().zip(c).map(|(a, b)| a * b).sum::<f64>();
                    assert!((y - 0.18).abs() < 1e-7);
                }
                for ev in [-2.0_f64, 0.0, 2.0] {
                    let c = m.apply(n.map(|v| v / n[1] * 0.18 * ev.exp2()));
                    assert!((c[1] - 0.18 * ev.exp2()).abs() < 1e-7);
                }
            }
        }
        let mid = 1.0 / (0.5 / 2856.0 + 0.5 / 6504.0);
        let m = model.matrix(mid);
        assert!((m.0[0][0] - 0.775).abs() < 1e-12);
    }
    #[test]
    fn invalid_models_never_become_calibration() {
        let mut c = model();
        c.xyz_to_camera[0] = Mat3([[0.0; 3]; 3]);
        assert!(c.validate().is_err());
        c = model();
        c.xyz_to_camera[0].0[0][0] = f64::NAN;
        assert!(c.validate().is_err());
        c = model();
        c.temperatures = [6504.0, 2856.0];
        assert!(c.validate().is_err());
        c = model();
        c.xyz_to_camera = [Mat3::IDENTITY, Mat3::diag(-1.0, 1.0, 1.0)];
        assert!(c.validate().is_err());
    }
}
