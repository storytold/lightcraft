//! DNG Camera Profiles (`.dcp`): the user's own camera colour profiles, read at run time from
//! files they supply (DNG specification, chapter 6 "Camera Profiles"). A `.dcp` is a TIFF-structured
//! file whose header carries the magic `RC` (0x4352) in place of 42, with the profile tags in IFD 0:
//! `UniqueCameraModel`, `ProfileName`, the colour/forward matrices and calibration illuminants, the
//! hue/saturation map, look table and tone curve. Nothing here ships a profile.

#[cfg(test)]
use craft_tiff::tags as t;

use crate::ColorData;

/// The magic number of a DNG camera profile file (`IIRC` / `MMCR`).
pub use craft_raw::dcp::DCP_MAGIC;

/// A parsed camera profile.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DcpProfile {
    /// `ProfileName` (empty when the file has none).
    pub name: String,
    /// `UniqueCameraModel`: the camera the profile was made for (empty when absent).
    pub camera_model: String,
    /// The colour model: illuminants, matrices and the profile look (hue/sat map, look table, tone curve).
    pub color: ColorData,
}

impl DcpProfile {
    /// `color` (a raw file's own colour facts) with this profile's colour model in place of the
    /// file's: its matrices, illuminants and look. The file's white balance (as-shot neutral or
    /// white, and its white-balance presets), analog balance and baseline exposure stay, plus its
    /// camera calibration when the profile has the same calibration illuminants (DNG 6.3:
    /// calibration belongs to the camera, the profile to the look).
    pub fn applied_to(&self, color: &ColorData) -> ColorData {
        let calibration = if self.color.illuminant == color.illuminant { color.camera_calibration } else { [None, None] };
        ColorData {
            illuminant: self.color.illuminant,
            color_matrix: self.color.color_matrix,
            forward_matrix: self.color.forward_matrix,
            camera_calibration: calibration,
            analog_balance: color.analog_balance,
            as_shot_neutral: color.as_shot_neutral,
            as_shot_white_xy: color.as_shot_white_xy,
            baseline_exposure: color.baseline_exposure + self.color.baseline_exposure,
            baseline_sharpness: color.baseline_sharpness,
            profile: crate::profile::ProfileLook { gain_table_map: color.profile.gain_table_map.clone(), ..self.color.profile.clone() },
            kelvin_presets: color.kelvin_presets.clone(),
            balanced_with: color.balanced_with,
        }
    }
}

pub use craft_raw::dcp::{DcpError, MAX_DCP_BYTES, is_dcp, read_dcp};

/// Parse the shared file model, keeping application color types at the boundary.
pub fn parse_dcp(bytes: &[u8]) -> Result<DcpProfile, DcpError> {
    let p = craft_raw::dcp::parse_dcp(bytes)?;
    Ok(DcpProfile { name: p.name, camera_model: p.camera_model, color: crate::convert::color(p.color) })
}

/// Write profile wire fields through craft-raw; shot/application policy is not serialized.
pub fn write_dcp(p: &DcpProfile) -> Result<Vec<u8>, DcpError> {
    let color = craft_raw::ColorData {
        illuminant: p.color.illuminant,
        color_matrix: p.color.color_matrix.map(|m| m.map(|m| m.0)),
        forward_matrix: p.color.forward_matrix.map(|m| m.map(|m| m.0)),
        baseline_exposure: p.color.baseline_exposure,
        profile: p.color.profile.clone(),
        ..Default::default()
    };
    craft_raw::dcp::write_dcp(&craft_raw::dcp::DcpProfile { name: p.name.clone(), camera_model: p.camera_model.clone(), color })
}

#[cfg(test)]
#[path = "dcp_legacy_tests.rs"]
mod legacy;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_a_profile_stops_past_the_size_limit() {
        let dir = std::env::temp_dir().join(format!("lc-read-dcp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (fits, big) = (dir.join("fits.dcp"), dir.join("big.dcp"));
        std::fs::File::create(&fits).unwrap().set_len(MAX_DCP_BYTES).unwrap();
        std::fs::File::create(&big).unwrap().set_len(MAX_DCP_BYTES + 1).unwrap();
        assert_eq!(read_dcp(&fits).unwrap().len() as u64, MAX_DCP_BYTES);
        assert_eq!(read_dcp(&big).unwrap_err().kind(), std::io::ErrorKind::InvalidData);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A minimal single-illuminant profile (our own numbers) built in memory: no profile file is
    /// committed to the repository.
    pub(crate) fn synthetic_dcp(little: bool, with_hue_sat: bool) -> Vec<u8> {
        // entries: (tag, type, count, payload bytes)
        let mut entries: Vec<(u16, u16, u32, Vec<u8>)> = Vec::new();
        let u16b = |v: u16| if little { v.to_le_bytes().to_vec() } else { v.to_be_bytes().to_vec() };
        let u32b = |v: u32| if little { v.to_le_bytes().to_vec() } else { v.to_be_bytes().to_vec() };
        let srat = |v: f64| {
            let (n, d) = ((v * 10000.0).round() as i32, 10000i32);
            let mut b = if little { n.to_le_bytes().to_vec() } else { n.to_be_bytes().to_vec() };
            b.extend(if little { d.to_le_bytes() } else { d.to_be_bytes() });
            b
        };
        let ascii = |s: &str| {
            let mut b = s.as_bytes().to_vec();
            b.push(0);
            b
        };
        entries.push((t::UNIQUE_CAMERA_MODEL, 2, 13, ascii("Test Camera1")));
        let m = [0.9, -0.3, -0.05, -0.4, 1.2, 0.2, -0.05, 0.15, 0.7];
        entries.push((t::COLOR_MATRIX_1, 10, 9, m.iter().flat_map(|v| srat(*v)).collect()));
        entries.push((t::CALIBRATION_ILLUMINANT_1, 3, 1, [u16b(21), vec![0, 0]].concat()));
        entries.push((t::PROFILE_NAME, 2, 9, ascii("My Look")));
        if with_hue_sat {
            // 2 hues × 2 sats × 1 value: hue shift, sat scale, value scale per entry (FLOAT)
            entries.push((t::PROFILE_HUE_SAT_MAP_DIMS, 4, 3, [u32b(2), u32b(2), u32b(1)].concat()));
            let f = |v: f32| if little { v.to_le_bytes().to_vec() } else { v.to_be_bytes().to_vec() };
            entries.push((
                t::PROFILE_HUE_SAT_MAP_DATA_1,
                11,
                12,
                [f(0.0), f(1.0), f(1.0), f(5.0), f(1.1), f(1.0), f(0.0), f(1.0), f(1.0), f(-5.0), f(0.9), f(1.0)].concat(),
            ));
        }
        entries.sort_by_key(|e| e.0);
        let mut out = if little { b"IIRC".to_vec() } else { b"MMCR".to_vec() };
        out.extend(u32b(8));
        let n = entries.len();
        let mut data_off = 8 + 2 + n * 12 + 4;
        let mut ifd = u16b(n as u16);
        let mut tail = Vec::new();
        for (tag, ty, count, payload) in &entries {
            ifd.extend(u16b(*tag));
            ifd.extend(u16b(*ty));
            ifd.extend(u32b(*count));
            if payload.len() <= 4 {
                let mut p = payload.clone();
                p.resize(4, 0);
                ifd.extend(p);
            } else {
                ifd.extend(u32b(data_off as u32));
                tail.extend(payload);
                data_off += payload.len();
                if data_off % 2 == 1 {
                    tail.push(0);
                    data_off += 1;
                }
            }
        }
        ifd.extend(u32b(0));
        out.extend(ifd);
        out.extend(tail);
        out
    }

    #[test]
    fn reads_a_single_illuminant_profile_in_both_byte_orders() {
        for little in [true, false] {
            let p = parse_dcp(&synthetic_dcp(little, true)).unwrap();
            assert_eq!(p.name, "My Look");
            assert_eq!(p.camera_model, "Test Camera1");
            assert_eq!(p.color.illuminant, [21, 0]);
            let m = p.color.color_matrix[0].unwrap();
            assert!((m.0[1][1] - 1.2).abs() < 1e-4, "{m:?}");
            assert!(p.color.color_matrix[1].is_none());
            assert!(p.color.profile.hue_sat_map[0].is_some());
            assert!(p.color.as_shot_neutral.is_none());
        }
        assert!(parse_dcp(&synthetic_dcp(true, false)).unwrap().color.profile.hue_sat_map[0].is_none());
    }

    #[test]
    fn rejects_other_files_and_damage_without_panicking() {
        assert_eq!(parse_dcp(b"II*\0\x08\0\0\0"), Err(DcpError::NotDcp));
        assert_eq!(parse_dcp(b""), Err(DcpError::NotDcp));
        let good = synthetic_dcp(true, true);
        for cut in [4, 8, 10, 30, good.len() / 2, good.len() - 1] {
            let _ = parse_dcp(&good[..cut]);
        }
        let mut bad = good.clone();
        bad[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_dcp(&bad).is_err());
    }

    /// The hue/saturation map's header is the user's file too: dimensions far past the data, an entry count
    /// claiming 4 billion values, or a data offset past the end drop the map (the matrix still reads) instead of
    /// allocating for the claim or reading past the file.
    #[test]
    fn a_hostile_hue_sat_map_header_drops_the_map() {
        let good = synthetic_dcp(true, true);
        let at = |needle: &[u8]| good.windows(needle.len()).position(|w| w == needle).unwrap();
        let le = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let dims = at(&le(&[2, 2, 1]));
        // the map data's directory entry: tag, FLOAT, count 12, offset
        let entry = at(&[t::PROFILE_HUE_SAT_MAP_DATA_1.to_le_bytes(), 11u16.to_le_bytes()].concat());
        let patched = |at: usize, bytes: &[u8]| {
            let mut b = good.clone();
            b[at..at + bytes.len()].copy_from_slice(bytes);
            b
        };
        for (what, bytes) in [
            ("4096³ dimensions", patched(dims, &le(&[4096, 4096, 4096]))),
            ("dimensions past u32 products", patched(dims, &le(&[u32::MAX, u32::MAX, u32::MAX]))),
            ("a zero dimension", patched(dims, &le(&[0, 2, 1]))),
            ("4 billion values", patched(entry + 4, &le(&[u32::MAX]))),
            ("data past the end", patched(entry + 8, &le(&[u32::MAX - 16]))),
        ] {
            let p = parse_dcp(&bytes).unwrap_or_else(|e| panic!("{what}: {e:?}"));
            assert!(p.color.profile.hue_sat_map[0].is_none(), "{what}: the map is dropped");
            assert!(p.color.color_matrix[0].is_some(), "{what}: the matrix still reads");
        }
    }

    #[test]
    fn written_profiles_read_back() {
        let p = parse_dcp(&synthetic_dcp(false, true)).unwrap();
        let back = parse_dcp(&write_dcp(&p).unwrap()).unwrap();
        assert_eq!(back.name, p.name);
        assert_eq!(back.camera_model, p.camera_model);
        assert_eq!(back.color.illuminant, p.color.illuminant);
        assert_eq!(back.color.profile.hue_sat_map, p.color.profile.hue_sat_map);
        let (a, b) = (back.color.color_matrix[0].unwrap(), p.color.color_matrix[0].unwrap());
        assert!(a.0.iter().flatten().zip(b.0.iter().flatten()).all(|(x, y)| (x - y).abs() < 1e-6));
    }

    #[test]
    fn applying_keeps_the_shot_and_replaces_the_look() {
        let p = parse_dcp(&synthetic_dcp(true, true)).unwrap();
        let file = ColorData { as_shot_neutral: Some([0.5, 1.0, 0.7]), baseline_exposure: 0.25, ..Default::default() };
        let c = p.applied_to(&file);
        assert_eq!(c.as_shot_neutral, Some([0.5, 1.0, 0.7]));
        assert_eq!(c.color_matrix, p.color.color_matrix);
        assert!(c.profile.hue_sat_map[0].is_some());
        assert!((c.baseline_exposure - 0.25).abs() < 1e-12);
    }
}

#[cfg(test)]
mod shared_parity {
    use super::*;
    fn check(bytes: &[u8]) {
        let old = legacy::parse_dcp(bytes);
        let new = parse_dcp(bytes);
        match (old, new) {
            (Ok(a), Ok(b)) => {
                assert_eq!((a.name.as_str(), a.camera_model.as_str(), &a.color), (b.name.as_str(), b.camera_model.as_str(), &b.color));
                assert_eq!(legacy::write_dcp(&a).map_err(|e| e.to_string()), write_dcp(&b).map_err(|e| e.to_string()));
            }
            (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string()),
            other => panic!("different DCP acceptance: {other:?}"),
        }
    }
    #[test]
    fn shared_reader_and_writer_match_original_on_both_endian_mutations() {
        for little in [false, true] {
            let bytes = super::tests::synthetic_dcp(little, true);
            for n in 0..=bytes.len() {
                check(&bytes[..n]);
            }
            for at in 0..bytes.len() {
                for value in [0, 0xff, 0x80] {
                    let mut mutated = bytes.clone();
                    mutated[at] = value;
                    check(&mutated);
                }
            }
        }
    }
}
