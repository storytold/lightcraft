//! Chart patch sampling in unrotated default-crop coordinates, before demosaic/WB/tone.
use lightcraft_color::{
    Xy,
    camera::Provenance,
    chart::{Capture, Dataset, Patch},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchRegion {
    pub name: String,
    /// [left, top, width, height], fractions of the unrotated default crop. Use patch interiors.
    pub rect: [f64; 4],
    pub xyz: [f64; 3],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    pub provenance: Provenance,
    pub white: Xy,
    pub neutral_patch: usize,
    pub patches: Vec<PatchRegion>,
}

pub fn sample(raw: &crate::RawImage, sha256: String, layout: &Layout) -> Result<Dataset, String> {
    layout.provenance.validate()?;
    if !(12..=256).contains(&layout.patches.len()) {
        return Err("layout needs 12–256 patch regions".into());
    }
    let sensor = raw.normalized().map_err(|e| e.to_string())?;
    let cfa = sensor.cfa.as_ref().filter(|c| c.width == 2 && c.height == 2).ok_or("chart sampler currently requires Bayer RAW")?;
    let crop = raw.crop.clipped(sensor.width, sensor.height);
    let mut patches = Vec::new();
    for p in &layout.patches {
        let [x, y, w, h] = p.rect;
        if p.rect.iter().any(|v| !v.is_finite() || *v < 0.0) || w <= 0.0 || h <= 0.0 || x + w > 1.0 || y + h > 1.0 {
            return Err(format!("{}: patch rectangle must be inside [0,1]", p.name));
        }
        let x0 = crop.x + (x * crop.width as f64).ceil() as usize;
        let y0 = crop.y + (y * crop.height as f64).ceil() as usize;
        let x1 = crop.x + ((x + w) * crop.width as f64).floor() as usize;
        let y1 = crop.y + ((y + h) * crop.height as f64).floor() as usize;
        if x1.saturating_sub(x0) < 8 || y1.saturating_sub(y0) < 8 {
            return Err(format!("{}: patch too small (minimum 8×8 sensor pixels)", p.name));
        }
        let area = x1.saturating_sub(x0).saturating_mul(y1.saturating_sub(y0));
        let step = (((area as f64 / 4096.0).sqrt().ceil() as usize).max(2)).div_ceil(2) * 2;
        let (mut sums, mut counts, mut clipped) = ([0.0; 3], [0usize; 3], 0usize);
        for by in (y0..y1.saturating_sub(1)).step_by(step) {
            for bx in (x0..x1.saturating_sub(1)).step_by(step) {
                for dy in 0..2 {
                    for dx in 0..2 {
                        let (xx, yy) = (bx + dx, by + dy);
                        let i = yy.checked_mul(sensor.width).and_then(|v| v.checked_add(xx)).ok_or("chart offset overflow")?;
                        let v = *sensor.data.get(i).ok_or("chart patch outside sensor")? as f64;
                        let channel = usize::from(cfa.color_at(xx, yy));
                        clipped += usize::from(v >= 0.98 || v <= 0.0001 || !v.is_finite());
                        *sums.get_mut(channel).ok_or("invalid CFA channel")? += v;
                        *counts.get_mut(channel).ok_or("invalid CFA channel")? += 1;
                    }
                }
            }
        }
        if clipped > 0 || counts.iter().any(|n| *n < 16) {
            return Err(format!("{}: clipped/dark patch or insufficient Bayer samples; adjust exposure/region", p.name));
        }
        patches.push(Patch { name: p.name.clone(), xyz: p.xyz, camera: std::array::from_fn(|i| sums[i] / counts[i] as f64) });
    }
    let data = Dataset {
        version: 1,
        make: raw.metadata.make.clone().ok_or("RAW lacks camera make")?,
        model: raw.metadata.model.clone().ok_or("RAW lacks camera model")?,
        provenance: layout.provenance.clone(),
        captures: vec![Capture { id: sha256, white: layout.white, neutral_patch: layout.neutral_patch, patches }],
    };
    data.validate()?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn samples_unbalanced_cfa_and_rejects_hostile_or_clipped_regions() {
        let cfa = crate::Cfa::bayer("RGGB").unwrap();
        let raw = crate::RawImage {
            format: crate::RawFormat::Nef,
            width: 64,
            height: 48,
            cpp: 1,
            data: crate::RawData::U16((0..64 * 48).map(|i| [2000, 4000, 3000][cfa.color_at(i % 64, i / 64) as usize]).collect()),
            cfa: Some(cfa),
            bits: 14,
            black: crate::BlackLevel::uniform(100.0),
            white: vec![10000.0],
            active_area: crate::Rect::new(0, 0, 64, 48),
            crop: crate::Rect::new(1, 1, 62, 46),
            orientation: Default::default(),
            color: Default::default(),
            wb_multipliers: Some([2.0, 1.0, 1.5]),
            linearized: false,
            opcodes: Default::default(),
            metadata: crate::Metadata { make: Some("SYNTHETIC".into()), model: Some("TEST".into()), ..Default::default() },
        };
        let white = lightcraft_color::D65;
        let mut layout = Layout {
            provenance: Provenance {
                author: "test".into(),
                source: "original work".into(),
                license: "MIT".into(),
                created: "2026-10-08".into(),
                chart: "procedural".into(),
                reference: "analytical".into(),
            },
            white,
            neutral_patch: 0,
            patches: (0..12)
                .map(|i| PatchRegion { name: format!("patch{i}"), rect: [0.1, 0.1, 0.8, 0.8], xyz: white.to_xyz().map(|v| v * 0.18) })
                .collect(),
        };
        let d = sample(&raw, format!("{:064x}", 1), &layout).unwrap();
        let p = &d.captures[0].patches[0];
        for (seen, expected) in p.camera.iter().zip([1900.0 / 9900.0, 3900.0 / 9900.0, 2900.0 / 9900.0]) {
            assert!((seen - expected).abs() < 1e-6);
        }
        layout.patches[0].rect[0] = f64::NAN;
        assert!(sample(&raw, format!("{:064x}", 1), &layout).is_err());
        layout.patches[0].rect = [0.1, 0.1, 0.8, 0.8];
        let mut clipped = raw;
        clipped.white = vec![3000.0];
        assert!(sample(&clipped, format!("{:064x}", 1), &layout).is_err());
    }
}
