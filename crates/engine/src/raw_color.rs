//! Versioned base camera colour. Calibration is separate from a JPEG's display style.
use lightcraft_color::{
    Mat3,
    camera::{CameraModel, CameraWb},
    cct,
};
use lightcraft_develop::{RawColor, RawColorMode};
use lightcraft_pipeline::{RawColorStatus, SourceInfo};
use lightcraft_raster::{
    Rgb32f,
    resample::{Filter, fit},
};
use lightcraft_raw::{RawImage, color};

/// Accept only standard XYZ-to-camera tags with known illuminants and a valid interpolation.
/// Proprietary Nikon maker-note "ColorMatrix" is deliberately not interpreted as this model.
pub fn embedded_model(raw: &RawImage) -> Option<CameraModel> {
    let c = &raw.color;
    let mut pairs = Vec::new();
    for i in 0..2 {
        let Some(cm) = c.color_matrix[i] else { continue };
        let Some(t) = color::illuminant_temperature(c.illuminant[i]) else { continue };
        let cc = c.camera_calibration[i].unwrap_or(Mat3::IDENTITY);
        pairs.push((t, cm, cc, c.forward_matrix[i]));
    }
    let (a, b) = match pairs.as_slice() {
        [a, b] if (a.0 - b.0).abs() >= 1.0 => {
            if a.0 < b.0 {
                (*a, *b)
            } else {
                (*b, *a)
            }
        }
        [a] => ((2856.0, a.1, a.2, a.3), (6504.0, a.1, a.2, a.3)),
        _ => return None,
    };
    let model = CameraModel {
        temperatures: [a.0, b.0],
        xyz_to_camera: [a.1, b.1],
        reference: Some(lightcraft_color::camera::ReferenceCalibration {
            camera_calibration: [a.2, b.2],
            analog_balance: c.analog_balance.unwrap_or([1.0; 3]),
            forward: [a.3, b.3],
        }),
    };
    model.validate().ok()?;
    Some(model)
}

pub fn resolve(raw: &RawImage, selection: &RawColor) -> Result<Option<(CameraModel, RawColorStatus)>, String> {
    if let Some(p) = &selection.calibration {
        p.validate()?;
        if raw.metadata.make.as_deref().map(str::trim) != Some(p.make.trim()) || raw.metadata.model.as_deref().map(str::trim) != Some(p.model.trim())
        {
            return Err("camera calibration make/model does not match this RAW; select a matching profile or clear it".into());
        }
    }
    if let Some(model) = embedded_model(raw) {
        return Ok(Some((model, RawColorStatus::Embedded)));
    }
    Ok(selection.calibration.as_ref().map(|p| (p.calibration, RawColorStatus::Own)))
}

/// None means the existing estimated look must be retained, with an explicit notice.
pub(crate) fn load(bytes: &[u8], max_edge: usize, selection: &RawColor) -> Result<Option<(Rgb32f, SourceInfo)>, String> {
    if !lightcraft_raw::probe(bytes)
        .is_some_and(|f| matches!(f, lightcraft_raw::RawFormat::Nef | lightcraft_raw::RawFormat::Nrw | lightcraft_raw::RawFormat::Dng))
    {
        return Ok(None);
    }
    let mut raw = match lightcraft_raw::decode_base(bytes) {
        Ok(raw) => raw,
        Err(lightcraft_raw::RawError::Unsupported(_)) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let Some((model, status)) = resolve(&raw, selection)? else { return Ok(None) };
    let neutral = raw
        .color
        .as_shot_neutral
        .or_else(|| raw.wb_multipliers.map(|w| w.map(|v| 1.0 / f64::from(v))))
        .or_else(|| raw.color.as_shot_white_xy.map(|xy| model.matrix(cct::xy_to_temp_tint(xy).0).apply(xy.to_xyz())));
    let Some(neutral) = neutral else { return Err("calibrated RAW requires valid as-shot neutral or camera WB coefficients".into()) };
    let xy = model.neutral_xy(neutral).ok_or("camera white balance cannot be interpreted by this calibration")?;
    let matrix = model.transform(xy).ok_or("invalid as-shot camera transform")?;
    // Use the actual camera neutral, not a rounded Kelvin approximation. Row normalisation
    // removes the final numerical inversion residual without changing the green exposure anchor.
    let n = neutral.map(|v| v / neutral[1]);
    let white = matrix.apply(n);
    if white.iter().any(|v| !v.is_finite() || *v <= 1e-8) {
        return Err("invalid calibrated camera neutral".into());
    }
    let matrix = Mat3::diag(1.0 / white[0], 1.0 / white[1], 1.0 / white[2]).mul(&matrix);
    let from_working = matrix.inverse().ok_or("singular camera transform")?;
    let min_n = n.into_iter().fold(f64::INFINITY, f64::min);
    let wb = n.map(|v| (1.0 / v * min_n) as f32);
    // Highlight reconstruction uses gains with minimum one, just as the legacy decoder.
    let wb_min = wb.into_iter().fold(f32::INFINITY, f32::min);
    let wb = wb.map(|v| v / wb_min);
    let transform = lightcraft_raw::color::CameraTransform {
        matrix: matrix.mul(&Mat3::diag(1.0 / wb[0] as f64, 1.0 / wb[1] as f64, 1.0 / wb[2] as f64)),
        wb,
        white_xy: xy,
        matrix_is_fallback: false,
        baseline_exposure: 0.0,
    };
    // The optional fit supplies only a display tone/chroma curve. It cannot replace calibration.
    let camera_tone = if selection.mode == RawColorMode::MatchCamera { crate::camera_preview::fit_style(&raw, bytes, &transform) } else { None };
    let lens = crate::files::embedded_lens(&raw.info());
    raw.opcodes.list3.retain(|op| !matches!(op, lightcraft_raw::Opcode::WarpRectilinear { .. } | lightcraft_raw::Opcode::FixVignetteRadial { .. }));
    let binned = match crate::files::bin_factor(&raw, max_edge) {
        Some(k) => raw.develop_binned(k, 0.99).map_err(|e| e.to_string())?,
        None => None,
    };
    let mut img = match binned {
        Some(img) => img,
        None => raw.develop(lightcraft_raw::Method::Ahd).map_err(|e| e.to_string())?,
    };
    lightcraft_raw::highlight::reconstruct(&mut img, wb, 0.99);
    // Keep signed, scene-linear values until pipeline WB; no JPEG LUT, tone or per-frame gain baked in.
    img.map_in_place(|p| matrix.apply_f32(p));
    let img = fit(&img, max_edge, max_edge, Filter::Box).into_oriented(raw.orientation);
    let (temp, tint) = cct::xy_to_temp_tint(xy);
    Ok(Some((
        img,
        SourceInfo {
            raw: true,
            as_shot_temp: temp,
            as_shot_tint: tint,
            relative_wb: false,
            lens,
            camera_tone,
            camera_wb: Some(CameraWb { model, from_working }),
            raw_color_status: status,
            camera_match: camera_tone.is_some(),
        },
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_develop::{DevelopSettings, WbMode};
    use lightcraft_raw::{ColorData, RawData, RawFormat, Rect};
    use serde_json::json;
    use std::sync::Arc;

    fn synthetic() -> RawImage {
        let cm = Mat3([[0.8, 0.15, 0.04], [-0.2, 1.15, 0.1], [0.01, -0.04, 0.7]]);
        let color = ColorData {
            illuminant: [17, 21],
            color_matrix: [Some(cm), Some(cm)],
            as_shot_neutral: Some(cm.apply(lightcraft_color::D65.to_xyz())),
            ..Default::default()
        };
        let white = cm.apply(lightcraft_color::D65.to_xyz());
        let data = (0..64 * 48)
            .flat_map(|i| {
                let v = 0.02 + 0.5 * (i % 64) as f64 / 63.0;
                white.map(|x| (x / white[1] * v * 65535.0).round() as u16)
            })
            .collect();
        RawImage {
            format: RawFormat::Dng,
            width: 64,
            height: 48,
            cpp: 3,
            data: RawData::U16(data),
            cfa: None,
            bits: 16,
            black: Default::default(),
            white: vec![65535.0],
            active_area: Rect::new(0, 0, 64, 48),
            crop: Rect::new(0, 0, 64, 48),
            orientation: Default::default(),
            color,
            wb_multipliers: None,
            linearized: false,
            opcodes: Default::default(),
            metadata: lightcraft_raw::Metadata { make: Some("SYNTHETIC".into()), model: Some("TEST ONLY".into()), ..Default::default() },
        }
    }
    fn bytes(raw: &RawImage) -> Vec<u8> {
        lightcraft_raw::write_dng(raw, &Default::default()).unwrap()
    }
    fn selection() -> RawColor {
        RawColor { mode: RawColorMode::Base, ..Default::default() }
    }
    #[test]
    fn base_has_neutral_greys_and_preserves_exposure_across_sizes() {
        let b = bytes(&synthetic());
        for edge in [32, 64, 256] {
            let (img, info) = crate::files::load_bytes_with_color(&b, edge, &selection()).unwrap();
            assert_eq!(info.raw_color_status, RawColorStatus::Embedded);
            assert!(info.camera_tone.is_none());
            assert!(!info.relative_wb);
            for p in &img.data {
                assert!((p[0] - p[1]).abs() < 0.0001 && (p[2] - p[1]).abs() < 0.0001, "{p:?}");
            }
            let mut ev = DevelopSettings::default();
            ev.light.exposure = 1.0;
            let mut doubled = img.clone();
            lightcraft_pipeline::local::scene_linear_pre(&mut doubled, &info, &ev);
            for (p, q) in img.data.iter().zip(&doubled.data) {
                assert!((q[1] - 2.0 * p[1]).abs() < 1e-6);
            }
            let req = lightcraft_pipeline::RenderRequest::fit(32, 24);
            let mut s = DevelopSettings::default();
            s.wb.mode = WbMode::Custom;
            s.wb.temp = 5000.0;
            s.wb.tint = 20.0;
            let preview = lightcraft_pipeline::render(&img, &info, &s, &req).image;
            let export = lightcraft_pipeline::render(
                &img,
                &info,
                &s,
                &lightcraft_pipeline::RenderRequest { depth: lightcraft_pipeline::OutputDepth::U16, ..req },
            )
            .image;
            assert_eq!(preview, export);
        }
    }
    #[test]
    fn missing_profile_retains_legacy_and_embedded_data_wins_over_own() {
        let mut raw = synthetic();
        raw.color.camera_calibration[0] = Some(Mat3::diag(1.0, 0.9, 1.1));
        assert_eq!(embedded_model(&raw).unwrap().reference.unwrap().camera_calibration[1], Mat3::IDENTITY);
        raw.color.color_matrix = [None, None];
        let b = bytes(&raw);
        let a = crate::files::load_bytes(&b, 64).unwrap();
        let fallback = crate::files::load_bytes_with_color(&b, 64, &selection()).unwrap();
        assert_eq!(a.0, fallback.0);
        assert_eq!(fallback.1.raw_color_status, RawColorStatus::Estimated);
        let mut profile = lightcraft_color::camera::CameraCalibration {
            version: 1,
            make: "SYNTHETIC".into(),
            model: "TEST ONLY".into(),
            provenance: lightcraft_color::camera::Provenance {
                author: "test".into(),
                source: "original work".into(),
                license: "MIT".into(),
                created: "2026-10-08".into(),
                chart: "synthetic".into(),
                reference: "analytical".into(),
            },
            calibration: CameraModel { temperatures: [2856.0, 6504.0], xyz_to_camera: [lightcraft_color::SRGB.from_xyz(); 2], reference: None },
            training_captures: vec![format!("{:064x}", 1), format!("{:064x}", 2)],
        };
        let selection = RawColor { mode: RawColorMode::Base, calibration: Some(profile.clone()) };
        assert_eq!(resolve(&synthetic(), &selection).unwrap().unwrap().1, RawColorStatus::Embedded);
        assert_eq!(resolve(&raw, &selection).unwrap().unwrap().1, RawColorStatus::Own);
        profile.model = "WRONG CAMERA".into();
        assert!(resolve(&raw, &RawColor { calibration: Some(profile), ..selection }).is_err());
    }
    #[test]
    fn mode_command_undo_old_settings_and_stale_jobs() {
        let b = Arc::new(bytes(&synthetic()));
        let mut s = crate::Session::new();
        s.media.file_loader = Some(Arc::new(move |_, edge, color| crate::files::load_bytes_with_color(&b, edge, color)));
        let id = lightcraft_catalog::PhotoId(1);
        let mut photo =
            lightcraft_catalog::Photo::new(id, lightcraft_catalog::Source::File { path: "test.dng".into() }, "test.dng", "DNG", 64, 48, "");
        let legacy: DevelopSettings = serde_json::from_value(json!({"wb":{"mode":"custom","temp":8000,"tint":-25}})).unwrap();
        assert_eq!(legacy.raw_color.mode, RawColorMode::Legacy);
        photo.kind = lightcraft_catalog::MediaKind::Raw;
        photo.develop = Arc::new(legacy.clone());
        s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        s.execute("library.select", &json!({"ids":[1],"active":1})).unwrap();
        let before = s.render_now(id, 64, 48).unwrap().image;
        let old_job = s.render_job(id, 32, 24, false, true).unwrap();
        s.execute("develop.rawColor", &json!({"mode":"base"})).unwrap();
        let info = s.source_info(id);
        assert_eq!(info.raw_color_status, RawColorStatus::Embedded);
        assert_eq!(s.develop_of(id).unwrap().wb.mode, WbMode::AsShot);
        s.accept(&old_job.run());
        assert_eq!(s.source_info(id).raw_color_status, RawColorStatus::Embedded);
        s.execute("develop.set", &json!({"control":"wb.tint","value":20})).unwrap();
        assert_eq!(s.develop_of(id).unwrap().wb.temp, info.as_shot_temp);
        s.execute("edit.undo", &json!({})).unwrap();
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(*s.develop_of(id).unwrap(), legacy);
        assert_eq!(s.render_now(id, 64, 48).unwrap().image, before);
    }
}

#[cfg(test)]
mod reference_tests {
    use super::*;
    #[test]
    fn file_reference_calibration_and_forward_matrix_keep_their_meaning() {
        let cm = Mat3([[0.8, 0.15, 0.04], [-0.2, 1.15, 0.1], [0.01, -0.04, 0.7]]);
        let ab = [1.0, 1.1, 0.95];
        let cc = Mat3([[1.0, 0.02, 0.0], [0.0, 0.9, 0.0], [0.01, 0.0, 1.03]]);
        let xy = lightcraft_color::D65;
        let n = cm.apply(xy.to_xyz());
        let fm = lightcraft_color::bradford(xy, lightcraft_color::D50).mul(&cm.inverse().unwrap()).mul(&Mat3::diag(n[0], n[1], n[2]));
        let model = CameraModel {
            temperatures: [2856.0, 6504.0],
            xyz_to_camera: [cm; 2],
            reference: Some(lightcraft_color::camera::ReferenceCalibration {
                analog_balance: ab,
                camera_calibration: [cc; 2],
                forward: [Some(fm); 2],
            }),
        };
        model.validate().unwrap();
        let neutral = model.matrix(cct::xy_to_temp_tint(xy).0).apply(xy.to_xyz());
        let m = model.transform(xy).unwrap();
        let out = m.apply(neutral.map(|v| v / neutral[1]));
        for c in out {
            assert!((c - 1.0).abs() < 1e-6);
        }
        let target = [0.2, 0.4, 0.6];
        let camera = model.matrix(cct::xy_to_temp_tint(xy).0).apply(lightcraft_color::REC2020.to_xyz().apply(target));
        let recovered = m.apply(camera.map(|v| v / neutral[1]));
        for (got, want) in recovered.into_iter().zip(target) {
            assert!((got - want).abs() < 1e-6);
        }
    }
}
