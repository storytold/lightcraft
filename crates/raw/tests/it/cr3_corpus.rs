//! Exact sensor-pixel regressions, not embedded-JPEG comparisons.
//!
//! Public-domain inputs from raw.pixls.us, kept in the gitignored corpus/raw.
//! Reference pixels were independently obtained by running the DNGLab v0.8.0
//! release executable as a black-box oracle (analyze --raw-pixel). Its source
//! and camera matrices were not used. Canonical FNV-1a hashes below are over
//! decoded u16 samples in row-major, little-endian order, including masked borders.
//! macOS oracle archive SHA256:
//! ee70805cb60f18d5ed62548c4e595f6cee0a33407e15358eb1766a46afc1c16e.

use lightcraft_raw::{RawData, RawFormat, decode, probe_info};
use std::path::{Path, PathBuf};

fn corpus() -> PathBuf {
    std::env::var_os("LIGHTCRAFT_CORPUS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
}

fn check(name: &str, hash: u64, sensor: (usize, usize), developed: (usize, usize)) {
    check_against(name, hash, sensor, Some(developed), "the independent reference");
}

fn check_against(name: &str, hash: u64, sensor: (usize, usize), developed: Option<(usize, usize)>, reference: &str) {
    let path = corpus().join("raw").join(name);
    let Ok(bytes) = std::fs::read(&path) else {
        lightcraft_testkit::skip!(Corpus, "{} absent", path.display());
        return;
    };
    let raw = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(raw.format, RawFormat::Cr3);
    raw.validate().unwrap();
    assert_eq!((raw.width, raw.height, raw.cpp), (sensor.0, sensor.1, 1));
    if let Some(developed) = developed {
        assert_eq!(raw.info().developed_size(), developed);
    }
    assert_eq!(raw.info(), probe_info(&bytes).unwrap());
    let RawData::U16(samples) = &raw.data else { panic!("{name}: integer sensor data expected") };
    let actual =
        samples.iter().flat_map(|v| v.to_le_bytes()).fold(0xcbf2_9ce4_8422_2325u64, |h, byte| (h ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3));
    assert_eq!(actual, hash, "{name}: sensor pixels differ from {reference}");
    assert!(raw.wb_multipliers.is_some(), "{name}: as-shot white balance missing");
    assert!(raw.white_at(0) > raw.black.mean(), "{name}: invalid black/white levels");
}

#[test]
fn canon_m50_lossless_sensor_is_exact() {
    // raw.pixls.us/getfile.php/4657; SHA256
    // 1ba9ad6b51b315b88820eb0fe5fd7f52bc136ba37b1a5ca76825ebfe25b18058
    check("cr3-canon-m50-raw.cr3", 0x6226_1f0b_a81c_fcd2, (6288, 4056), (6000, 4000));
}

#[test]
fn canon_r100_lossless_sensor_is_exact() {
    // raw.pixls.us/getfile.php/7896; SHA256
    // 6d83217d58a5e6d2dabcf470430e91a16676b60531c025b81fa7453fc74b0521
    check("cr3-canon-r100-raw.cr3", 0xcbe5_299a_b9c5_2630, (6288, 4056), (6000, 4000));
}

#[test]
fn canon_m50_craw_sensor_is_exact() {
    // raw.pixls.us/getfile.php/2663; SHA256
    // 15384b775867ec4c42b11882837f1e368cedc0561832ffab271221e6bb80be4c
    // The camera was set to 1:1: the file's AspectInfo is a 4000 x 4000 square at x = 1000 inside the 6000 x 4000
    // image (the archive's "3:2" label is wrong). The sensor samples are unchanged; only the crop is.
    check("cr3-canon-m50-craw.cr3", 0x9aac_bfe6_6f50_5e1b, (6288, 4056), (4000, 4000));
}

#[test]
fn canon_r100_craw_sensor_is_exact() {
    // raw.pixls.us/getfile.php/7897; SHA256
    // 0b66842b2fe00329ebc05fe8ab9357ddbbe1a0a2d2e69a2893102de7fdf5c942
    check("cr3-canon-r100-craw.cr3", 0x341c_706b_37c3_8bbf, (6288, 4056), (6000, 4000));
}

#[test]
fn canon_r8_lossless_sensor_is_exact() {
    // raw.pixls.us/getfile.php/6585; SHA256
    // 7d5c6dbb11ff7e6ee58715d90a20f5d801e8b103f4711ff6ea329ceafcef254c
    check("cr3-canon-r8-raw.cr3", 0x63c1_c6d8_d1eb_2312, (6188, 4120), (6000, 4000));
}

#[test]
fn canon_r8_craw_sensor_is_exact() {
    // raw.pixls.us/getfile.php/6587; SHA256
    // df33cf394573645ce03dea1b2e9f0b5cc2b7734e9e3391ee7114151414cf2812
    check("cr3-canon-r8-craw.cr3", 0xf836_a663_a795_a375, (6188, 4120), (6000, 4000));
}

/// EOS R5 Mark II (raw.pixls.us 7881–7884, CC0). No external decoder was run on these: the hashes pin
/// LightCraft's own decode. Lossless RAW is CRX 0x100, as in the oracle-checked fixtures above, with
/// odd-height (2825- and 1787-row) Bayer planes; every entropy stream decodes to its exact end. These
/// low-ISO C-RAW files need no QP-map escape (most of the camera's files do; that is covered by the unit
/// tests in `crx.rs`, see docs/cr3.md). The developed images match the camera's own previews.
fn check_r5m2(name: &str, hash: u64, sensor: (usize, usize), developed: Option<(usize, usize)>) {
    check_against(name, hash, sensor, developed, "the pinned LightCraft decode");
}

#[test]
fn canon_r5m2_sensor_is_pinned() {
    // raw.pixls.us/getfile.php/7881; SHA256 d6e6aa3b493ea8936dff0efb0cc17c87471437e7317bce37c8c82ba08b9cab7b
    check_r5m2("cr3-canon-r5m2-raw.cr3", 0xff71_6fb3_0248_496f, (8480, 5650), Some((8192, 5464)));
    // raw.pixls.us/getfile.php/7882; SHA256 b3c08a5c7a97299dfd58896a12b939e7b43ec78682761812524825e6e5d087be
    check_r5m2("cr3-canon-r5m2-craw.cr3", 0xaf32_b453_57b5_34e5, (8480, 5650), Some((8192, 5464)));
}

/// Crop mode (1.6×): the sensor samples only. Its developed framing depends on how the file's misplaced valid
/// area is read, which is not part of the CRX decode.
#[test]
fn canon_r5m2_crop_mode_sensor_is_pinned() {
    // raw.pixls.us/getfile.php/7883; SHA256 9f88c0ff012924193b7f4497559d133dfe626766d600b5d67689156721739ead
    check_r5m2("cr3-canon-r5m2-apsc-raw.cr3", 0x31cf_630a_fe86_8958, (5376, 3574), None);
    // raw.pixls.us/getfile.php/7884; SHA256 6a597497776a6abbfab8fcf12d0086ac7a3f8afea2879d61accf6bbf48d357d7
    check_r5m2("cr3-canon-r5m2-apsc-craw.cr3", 0x4721_605b_2033_9332, (5376, 3574), None);
}

#[test]
fn canon_m50_craw_crop_is_the_shot_aspect() {
    // raw.pixls.us/getfile.php/2663: the 1:1 square starts 1000 px right of the recommended crop's origin,
    // which is (12, 12) inside the valid sensor area.
    let path = corpus().join("raw").join("cr3-canon-m50-craw.cr3");
    let Ok(bytes) = std::fs::read(&path) else {
        lightcraft_testkit::skip!(Corpus, "{} absent", path.display());
        return;
    };
    let raw = decode(&bytes).unwrap();
    assert_eq!((raw.crop.x, raw.crop.y, raw.crop.width, raw.crop.height), (1012, 12, 4000, 4000));
}

/// Green of the decoded mosaic against the camera's embedded JPEG, on a `gw × gh` grid of the active area
/// (gamma-encoded, Pearson). A misread CRX stream (wrong tile, plane, band or quantiser) leaves a picture that
/// no longer follows the camera's own rendering; lens-corrected compact JPEGs keep it well above 0.6.
fn green_vs_camera_jpeg(raw: &lightcraft_raw::RawImage, jpeg: &[u8], gw: usize, gh: usize) -> f64 {
    use zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(jpeg), opts);
    let px = d.decode().unwrap();
    let info = d.info().unwrap();
    let (jw, jh) = (info.width as usize, info.height as usize);
    let mean = |sum: Vec<f64>, n: Vec<f64>| sum.iter().zip(&n).map(|(s, n)| s / n.max(1.0)).collect::<Vec<f64>>();
    let (mut js, mut jn) = (vec![0f64; gw * gh], vec![0f64; gw * gh]);
    for y in 0..jh {
        for x in 0..jw {
            let i = (y * gh / jh) * gw + x * gw / jw;
            js[i] += f64::from(px[(y * jw + x) * 3 + 1]);
            jn[i] += 1.0;
        }
    }
    let RawData::U16(samples) = &raw.data else { panic!("integer sensor data expected") };
    let cfa = raw.cfa.clone().expect("Bayer layout");
    let (a, black, white) = (raw.active_area, f64::from(raw.black.mean()), f64::from(raw.white_at(0)));
    let (mut rs, mut rn) = (vec![0f64; gw * gh], vec![0f64; gw * gh]);
    for y in a.y..a.y + a.height {
        for x in a.x..a.x + a.width {
            if cfa.pattern[(y % 2) * 2 + x % 2] == 1 {
                let i = ((y - a.y) * gh / a.height) * gw + (x - a.x) * gw / a.width;
                rs[i] += ((f64::from(samples[y * raw.width + x]) - black).max(0.0) / (white - black)).powf(1.0 / 2.2);
                rn[i] += 1.0;
            }
        }
    }
    let (r, j) = (mean(rs, rn), mean(js, jn));
    let n = r.len() as f64;
    let (mr, mj) = (r.iter().sum::<f64>() / n, j.iter().sum::<f64>() / n);
    let cov: f64 = r.iter().zip(&j).map(|(x, y)| (x - mr) * (y - mj)).sum();
    let vr: f64 = r.iter().map(|x| (x - mr).powi(2)).sum();
    let vj: f64 = j.iter().map(|y| (y - mj).powi(2)).sum();
    cov / (vr * vj).sqrt()
}

/// One CC0 file per further CR3 body (raw.pixls.us; URLs and SHA-256 in `RAW_SAMPLES`, xtask/src/main.rs).
/// No external decoder was run on these: the sensor hashes pin LightCraft's own decode, every CRX stream
/// decodes, and the mosaic follows the camera's own embedded JPEG (`green_vs_camera_jpeg` on 24 × 16 cells).
/// The renders were also inspected at 100 % for tile, band and quantiser artefacts (none). See docs/cr3.md.
#[test]
fn canon_cr3_bodies_decode_like_the_camera_jpeg() {
    // (file, sensor width × height, pinned FNV-1a of the sensor samples)
    let cases: &[(&str, (usize, usize), u64)] = &[
        ("cr3-canon-m200.cr3", (6288, 4056), 0x4edf_3f62_a6ce_b2ba),
        ("cr3-canon-r10.cr3", (6192, 4060), 0xc140_2394_bf17_c2a2),
        ("cr3-canon-r50v.cr3", (6192, 4060), 0xd60f_f557_0928_8d27),
        ("cr3-canon-r6m3.cr3", (7144, 4760), 0x47a4_cb40_1233_cc80),
        ("cr3-canon-powershot-v1-craw.cr3", (5928, 3944), 0xbc54_037c_de0f_472a),
        ("cr3-canon-1dxm3-craw.cr3", (5568, 3708), 0x3472_dbe7_a666_1a61),
        ("cr3-canon-250d-craw.cr3", (6288, 4056), 0xf4c4_dbf8_1c93_2118),
        ("cr3-canon-850d.cr3", (6288, 4056), 0xedac_1b6a_43cf_4204),
        ("cr3-canon-90d-craw.cr3", (7128, 4732), 0xfaac_50ca_858e_5563),
        ("cr3-canon-m50m2.cr3", (6288, 4056), 0xf36f_717a_9250_ec89),
        ("cr3-canon-m6m2-craw.cr3", (7128, 4732), 0x6dc0_4c24_7f9b_6673),
        ("cr3-canon-r3.cr3", (6192, 4152), 0x4728_af6f_18b1_cba1),
        ("cr3-canon-r50-craw.cr3", (6192, 4060), 0x0354_0075_5fe5_a2a7),
        ("cr3-canon-r5-craw.cr3", (8352, 5586), 0xdb64_0502_1fd7_ba92),
        ("cr3-canon-r6-craw.cr3", (5568, 3708), 0xd3e0_567e_1ffb_07f4),
        ("cr3-canon-r6m2-craw.cr3", (6188, 4120), 0x5b1d_6de8_9562_00d4),
        ("cr3-canon-r7.cr3", (7128, 4732), 0xb725_758b_4410_f4af),
        ("cr3-canon-r-craw.cr3", (6888, 4546), 0xf9cd_81a3_249f_cfc7),
        ("cr3-canon-rp-craw.cr3", (6384, 4224), 0xf48d_f06c_a1d6_7382),
        ("cr3-canon-g5xm2-craw.cr3", (5632, 3710), 0xfed8_f701_f231_442f),
        ("cr3-canon-g7xm3.cr3", (5632, 3710), 0x902e_be92_2fe7_8046),
        ("cr3-canon-sx70.cr3", (5344, 3950), 0x000c_7fe9_fbec_f4c2),
    ];
    let mut seen = 0;
    for &(name, sensor, hash) in cases {
        let path = corpus().join("raw").join(name);
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("skip: {} absent", path.display());
            continue;
        };
        check_against(name, hash, sensor, None, "the pinned LightCraft decode");
        let raw = decode(&bytes).unwrap();
        let jpeg = lightcraft_raw::embedded_preview(&bytes).unwrap_or_else(|| panic!("{name}: no camera JPEG"));
        let r = green_vs_camera_jpeg(&raw, &jpeg, 24, 16);
        eprintln!("{name:34} {}x{}: green follows the camera JPEG, r = {r:.3}", raw.width, raw.height);
        assert!(r > 0.6, "{name}: decoded mosaic does not follow the camera JPEG (r = {r:.3})");
        seen += 1;
    }
    eprintln!("CR3 bodies checked on {seen} files");
}

#[test]
fn canon_rp_lossless_sensor_is_exact() {
    // raw.pixls.us/getfile.php/4674; SHA256
    // dcbd0f6f9929de2e2331b0fd10a9d01e7cfd04c5a5e4038456de1647d714cdf2
    check("cr3-canon-rp-raw.cr3", 0x5cb0_f464_04b8_b663, (6384, 4224), (6240, 4160));
}

#[test]
fn canon_rp_craw_sensor_is_exact() {
    // raw.pixls.us/getfile.php/4672; SHA256
    // bd3270610593d37ff8a399908bb9406a4e32f9151d55bf19ac1cf8f10d787b46
    check("cr3-canon-rp-craw.cr3", 0xf48d_f06c_a1d6_7382, (6384, 4224), (6240, 4160));
}

#[test]
fn canon_rp_crop_mode_lossless_sensor_is_exact() {
    // 1.6x crop mode: a single tile. raw.pixls.us/getfile.php/4673; SHA256
    // 7d4ba45dc9264d598336133593c677b09774bcdf13b602ee770e72415a652a16
    check("cr3-canon-rp-raw-crop.cr3", 0x4680_8a9d_e3b3_dcc7, (4032, 2656), (3888, 2592));
}

#[test]
fn canon_rp_crop_mode_craw_sensor_is_exact() {
    // raw.pixls.us/getfile.php/4671; SHA256
    // 4a859ab137f792aedaf3416b0aad1497ebaab02052a49453902e226699154aa7
    check("cr3-canon-rp-craw-crop.cr3", 0x39a6_0c4e_fed4_e1f4, (4032, 2656), (3888, 2592));
}
