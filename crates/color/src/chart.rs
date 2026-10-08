//! Least-squares chart calibration and capture-disjoint validation. All reference data is supplied
//! by the photographer; no published chart values, third-party profiles or camera matrices ship here.
use crate::{
    Mat3, Xy,
    camera::{CameraCalibration, CameraModel, Provenance, valid_matrix},
    cct,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub name: String,
    /// Mean linear sensor RGB, after black subtraction and white normalisation, before WB.
    pub camera: [f64; 3],
    /// Measured XYZ under this capture's illuminant (diffuse white Y=1), NOT sRGB or JPEG values.
    pub xyz: [f64; 3],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    /// SHA-256 of the original file, produced by the sampling tool.
    pub id: String,
    pub white: Xy,
    /// Index of an unclipped spectrally neutral reference patch, used for exposure normalisation.
    pub neutral_patch: usize,
    pub patches: Vec<Patch>,
}

impl Capture {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.len() != 64 || !self.id.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err("capture id must be the original's SHA-256".into());
        }
        let w = self.white;
        if !w.x.is_finite() || !w.y.is_finite() || w.x <= 0.0 || w.y <= 0.0 || w.x + w.y >= 1.0 {
            return Err("invalid measured illuminant xy".into());
        }
        let t = cct::xy_to_temp_tint(w).0;
        if !(2000.0..=50000.0).contains(&t) {
            return Err("illuminant outside supported temperature range".into());
        }
        if self.patches.len() < 12 || self.patches.len() > 256 {
            return Err("capture needs 12–256 chart patches".into());
        }
        let mut names = std::collections::HashSet::new();
        for p in &self.patches {
            if p.name.trim().is_empty()
                || p.name.len() > 128
                || !names.insert(&p.name)
                || p.camera.iter().any(|v| !v.is_finite() || *v <= 0.0001 || *v >= 0.98)
                || p.xyz.iter().any(|v| !v.is_finite() || *v < 0.0 || *v > 3.0)
                || p.xyz[1] <= 0.0
            {
                return Err("invalid/repeated patch, clipped/dark sensor value or invalid reference XYZ".into());
            }
        }
        let neutral = self.patches.get(self.neutral_patch).ok_or("neutral patch index outside chart")?;
        let xy = Xy::from_xyz(neutral.xyz);
        if (xy.x - w.x).abs() + (xy.y - w.y).abs() > 0.01 {
            return Err("neutral reference XYZ disagrees with measured illuminant".into());
        }
        Ok(())
    }

    fn exposure(&self) -> Result<f64, String> {
        let p = self.patches.get(self.neutral_patch).ok_or("missing neutral patch")?;
        Ok(p.camera[1] / p.xyz[1])
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dataset {
    pub version: u32,
    pub make: String,
    pub model: String,
    pub provenance: Provenance,
    pub captures: Vec<Capture>,
}

impl Dataset {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.make.is_empty() || self.model.is_empty() || self.make.len() > 256 || self.model.len() > 256 {
            return Err("invalid chart dataset identity/version".into());
        }
        self.provenance.validate()?;
        if self.captures.is_empty() || self.captures.len() > 256 {
            return Err("dataset needs 1–256 captures".into());
        }
        let mut ids = std::collections::HashSet::new();
        for capture in &self.captures {
            capture.validate()?;
            if !ids.insert(capture.id.to_ascii_lowercase()) {
                return Err("duplicate original in dataset".into());
            }
        }
        Ok(())
    }
}

/// Fit exactly two lighting groups. More exposures of either light improve noise averaging.
/// Neutral exposure normalisation is per capture, never per patch/channel. No offsets or tone curves.
pub fn fit(data: &Dataset) -> Result<CameraCalibration, String> {
    data.validate()?;
    let temps: Vec<f64> = data.captures.iter().map(|c| cct::xy_to_temp_tint(c.white).0).collect();
    let lo = temps.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = temps.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if hi - lo < 1000.0 || lo > 4000.0 || hi < 5000.0 {
        return Err("fit needs separate warm (≤4000 K) and daylight (≥5000 K) chart captures".into());
    }
    let mut groups = [Vec::new(), Vec::new()];
    for (capture, t) in data.captures.iter().zip(&temps) {
        let group = if (*t - lo).abs() < (*t - hi).abs() { 0 } else { 1 };
        let anchor = if group == 0 { lo } else { hi };
        if (*t - anchor).abs() > 150.0 {
            return Err("training captures must form two lighting groups within 150 K; reserve intermediate light for validation".into());
        }
        groups[group].push(capture);
    }
    let mut matrices = [Mat3::IDENTITY; 2];
    let mut temperatures = [0.0; 2];
    for (group, captures) in groups.iter().enumerate() {
        if captures.is_empty() {
            return Err("missing lighting group".into());
        }
        temperatures[group] = captures.iter().map(|c| cct::xy_to_temp_tint(c.white).0).sum::<f64>() / captures.len() as f64;
        let mut gram = [[0.0; 3]; 3];
        let mut cross = [[0.0; 3]; 3];
        for c in captures {
            let exposure = c.exposure()?;
            for p in &c.patches {
                for i in 0..3 {
                    for j in 0..3 {
                        gram[i][j] += p.xyz[i] * p.xyz[j];
                        cross[i][j] += p.camera[i] / exposure * p.xyz[j];
                    }
                }
            }
        }
        let count = captures.iter().map(|c| c.patches.len()).sum::<usize>().max(1) as f64;
        gram = gram.map(|r| r.map(|v| v / count));
        cross = cross.map(|r| r.map(|v| v / count));
        if !valid_matrix(Mat3(gram)) {
            return Err("chart cannot constrain a colour matrix (singular or ill-conditioned samples)".into());
        }
        matrices[group] = Mat3(cross).mul(&Mat3(gram).inverse().ok_or("singular chart")?);
    }
    let profile = CameraCalibration {
        version: 1,
        make: data.make.clone(),
        model: data.model.clone(),
        provenance: data.provenance.clone(),
        calibration: CameraModel { temperatures, xyz_to_camera: matrices, reference: None },
        training_captures: data.captures.iter().map(|c| c.id.to_ascii_lowercase()).collect(),
    };
    profile.validate()?;
    Ok(profile)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaptureReport {
    pub id: String,
    pub temperature: f64,
    pub patches: usize,
    pub mean_delta_e76: f64,
    pub p95_delta_e76: f64,
    pub max_delta_e76: f64,
    pub neutral_chroma: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationReport {
    pub make: String,
    pub model: String,
    pub independent_captures: bool,
    pub warm_covered: bool,
    pub daylight_covered: bool,
    pub interpolation_covered: bool,
    pub captures: Vec<CaptureReport>,
}

fn lab(xyz: [f64; 3], white: Xy) -> [f64; 3] {
    let w = white.to_xyz();
    let f = |v: f64| if v > 216.0 / 24389.0 { v.cbrt() } else { (24389.0 / 27.0 * v + 16.0) / 116.0 };
    let [x, y, z] = std::array::from_fn(|i| f(xyz[i] / w[i]));
    [116.0 * y - 16.0, 500.0 * (x - y), 200.0 * (y - z)]
}

/// This is a report, not certification: camera/JPEG fidelity and spectral metamerism are separate.
pub fn validate(profile: &CameraCalibration, data: &Dataset) -> Result<ValidationReport, String> {
    profile.validate()?;
    data.validate()?;
    if profile.make != data.make || profile.model != data.model {
        return Err("validation camera does not match profile".into());
    }
    let mut reports = Vec::new();
    let [warm, day] = profile.calibration.temperatures;
    let (mut wc, mut dc, mut ic) = (false, false, false);
    for c in &data.captures {
        if profile.training_captures.iter().any(|id| id.eq_ignore_ascii_case(&c.id)) {
            return Err("validation reuses a training original; supply independent captures".into());
        }
        let temp = cct::xy_to_temp_tint(c.white).0;
        wc |= (temp - warm).abs() < 300.0;
        dc |= (temp - day).abs() < 500.0;
        ic |= temp > warm + 300.0 && temp < day - 500.0;
        let matrix = profile.calibration.matrix(temp);
        let inv = matrix.inverse().ok_or("singular calibration at validation light")?;
        let green = matrix.apply(c.white.to_xyz())[1];
        let exposure = c.exposure()?;
        let mut errors = Vec::new();
        let mut neutral_chroma = 0.0;
        for (i, p) in c.patches.iter().enumerate() {
            let seen = lab(inv.apply(p.camera.map(|v| v / exposure * green)), c.white);
            let expected = lab(p.xyz, c.white);
            errors.push((0..3).map(|j| (seen[j] - expected[j]).powi(2)).sum::<f64>().sqrt());
            if i == c.neutral_patch {
                neutral_chroma = seen[1].hypot(seen[2]);
            }
        }
        errors.sort_by(f64::total_cmp);
        let n = errors.len();
        reports.push(CaptureReport {
            id: c.id.clone(),
            temperature: temp,
            patches: n,
            mean_delta_e76: errors.iter().sum::<f64>() / n.max(1) as f64,
            p95_delta_e76: errors.get((n.saturating_sub(1) * 95).div_ceil(100)).copied().unwrap_or(0.0),
            max_delta_e76: errors.last().copied().unwrap_or(0.0),
            neutral_chroma,
        });
    }
    Ok(ValidationReport {
        make: data.make.clone(),
        model: data.model.clone(),
        independent_captures: true,
        warm_covered: wc,
        daylight_covered: dc,
        interpolation_covered: ic,
        captures: reports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{D65, SRGB, bradford};
    fn provenance() -> Provenance {
        Provenance {
            author: "LightCraft synthetic test".into(),
            source: "original work".into(),
            license: "MIT OR Apache-2.0".into(),
            created: "2026-10-08".into(),
            chart: "procedural; no camera measurements".into(),
            reference: "analytical XYZ".into(),
        }
    }
    fn camera() -> CameraModel {
        let temps = [2856.0, 6504.0];
        let matrices =
            [Mat3([[0.9, 0.1, 0.05], [-0.1, 1.15, 0.08], [0.03, 0.02, 0.7]]), Mat3([[0.8, 0.2, 0.02], [-0.15, 1.2, 0.1], [0.01, -0.03, 0.9]])];
        CameraModel {
            reference: None,
            temperatures: temps,
            xyz_to_camera: std::array::from_fn(|i| {
                let k = matrices[i].apply(cct::temp_tint_to_xy(temps[i], 0.0).to_xyz())[1];
                Mat3(matrices[i].0.map(|r| r.map(|v| v / k)))
            }),
        }
    }
    fn capture(id: usize, temp: f64, exposure: f64) -> Capture {
        let white = cct::temp_tint_to_xy(temp, 0.0);
        let m = camera().matrix(temp);
        let mut patches = Vec::new();
        for i in 0..24 {
            let rgb = if i == 0 {
                [0.3; 3]
            } else {
                [0.06 + ((i * 7) % 19) as f64 / 30.0, 0.06 + ((i * 11) % 19) as f64 / 30.0, 0.06 + ((i * 13) % 19) as f64 / 30.0]
            };
            let xyz = bradford(D65, white).mul(&SRGB.to_xyz()).apply(rgb);
            patches.push(Patch { name: format!("patch{i}"), xyz, camera: m.apply(xyz).map(|v| v * exposure) });
        }
        Capture { id: format!("{id:064x}"), white, neutral_patch: 0, patches }
    }
    fn dataset(captures: Vec<Capture>) -> Dataset {
        Dataset { version: 1, make: "SYNTHETIC".into(), model: "NOT A CAMERA PROFILE".into(), provenance: provenance(), captures }
    }
    #[test]
    fn chart_fit_independent_exposures_and_intermediate_light() {
        let train = dataset(vec![capture(1, 2856.0, 0.4), capture(2, 6504.0, 0.3)]);
        let p = fit(&train).unwrap();
        for i in 0..2 {
            for r in 0..3 {
                for c in 0..3 {
                    assert!((p.calibration.xyz_to_camera[i].0[r][c] - camera().xyz_to_camera[i].0[r][c]).abs() < 0.001);
                }
            }
        }
        let report = validate(&p, &dataset(vec![capture(3, 2856.0, 0.2), capture(4, 6504.0, 0.55), capture(5, 4500.0, 0.3)])).unwrap();
        assert!(report.warm_covered && report.daylight_covered && report.interpolation_covered);
        for c in report.captures {
            assert!(c.max_delta_e76 < 0.05, "{c:?}");
        }
        assert!(validate(&p, &train).is_err(), "no random patch split can certify its own training original");
    }
    #[test]
    fn rejects_uninformative_or_hostile_chart_inputs() {
        let mut d = dataset(vec![capture(1, 2856.0, 0.4), capture(2, 6504.0, 0.3)]);
        d.captures[0].neutral_patch = usize::MAX;
        assert!(fit(&d).is_err());
        d.captures[0].neutral_patch = 0;
        d.captures[1].id = d.captures[0].id.clone();
        assert!(fit(&d).is_err());
        d.captures[1].id = format!("{:064x}", 2);
        for capture in &mut d.captures {
            let p = capture.patches[0].clone();
            for patch in &mut capture.patches {
                patch.camera = p.camera;
                patch.xyz = p.xyz;
            }
        }
        assert!(fit(&d).is_err());
    }
}
