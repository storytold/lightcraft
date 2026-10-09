//! Exact sensor-pixel regressions, not embedded-JPEG comparisons.
//!
//! Public-domain inputs from raw.pixls.us, kept in the gitignored corpus/raw.
//! Reference pixels were independently obtained by running the DNGLab v0.8.0
//! release executable as a black-box oracle (analyze --raw-pixel). Its source
//! and camera matrices were not used. Canonical FNV-1a hashes below are over
//! decoded u16 samples in row-major, little-endian order, including masked borders.
//! macOS oracle archive SHA256:
//! ee70805cb60f18d5ed62548c4e595f6cee0a33407e15358eb1766a46afc1c16e.

use dac_raw::{RawData, RawFormat, decode, probe_info};
use std::path::{Path, PathBuf};

fn corpus() -> PathBuf {
    std::env::var_os("LIGHTCRAFT_CORPUS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
}

fn check(name: &str, hash: u64, sensor: (usize, usize), developed: (usize, usize)) {
    let path = corpus().join("raw").join(name);
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!("skip: {} absent", path.display());
        return;
    };
    let raw = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(raw.format, RawFormat::Cr3);
    raw.validate().unwrap();
    assert_eq!((raw.width, raw.height, raw.cpp), (sensor.0, sensor.1, 1));
    assert_eq!(raw.info().developed_size(), developed);
    assert_eq!(raw.info(), probe_info(&bytes).unwrap());
    let RawData::U16(samples) = &raw.data else { panic!("{name}: integer sensor data expected") };
    let actual =
        samples.iter().flat_map(|v| v.to_le_bytes()).fold(0xcbf2_9ce4_8422_2325u64, |h, byte| (h ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3));
    assert_eq!(actual, hash, "{name}: sensor pixels differ from the independent reference");
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

#[test]
fn canon_m50_craw_crop_is_the_shot_aspect() {
    // raw.pixls.us/getfile.php/2663: the 1:1 square starts 1000 px right of the recommended crop's origin,
    // which is (12, 12) inside the valid sensor area.
    let path = corpus().join("raw").join("cr3-canon-m50-craw.cr3");
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!("skip: {} absent", path.display());
        return;
    };
    let raw = decode(&bytes).unwrap();
    assert_eq!((raw.crop.x, raw.crop.y, raw.crop.width, raw.crop.height), (1012, 12, 4000, 4000));
}
