//! Sensor regressions for the public camera-coverage sweep.
//!
//! `docs/raw/coverage-sweep.tsv` records the file URLs, SHA-256 hashes, decoded dimensions,
//! FNV-1a sensor hashes and camera-image comparisons measured on main 25ef65c8.
//! Sensor hashes pin LightCraft's own output, not an independent decoder's output.
//! Hash input is every row-major sample in little-endian order, including masked borders.
//! Fetch the CC0 files with `cargo xtask corpus --download`. Missing files are reported.

use lightcraft_raw::{RawData, decode, probe_info};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const RECORDS: &str = include_str!("../../../docs/raw/coverage-sweep.tsv");

#[test]
fn coverage_sweep_sensor_decodes_are_pinned() {
    let corpus =
        std::env::var_os("LIGHTCRAFT_CORPUS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    let mut checked = 0;
    for line in RECORDS.lines().filter(|line| !line.starts_with('#')).skip(1) {
        let columns: Vec<_> = line.split('\t').collect();
        assert_eq!(columns.len(), 14, "invalid coverage record: {line}");
        let name = columns[3];
        let path = corpus.join("raw").join(name);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skip: {} absent", path.display());
                continue;
            }
            Err(error) => panic!("{}: {error}", path.display()),
        };
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), columns[5], "{name}: file SHA-256");
        let raw = decode(&bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
        raw.validate().unwrap();
        assert_eq!(raw.info(), probe_info(&bytes).unwrap(), "{name}: probe and decode disagree");
        let dimensions: Vec<usize> = columns[6..9].iter().map(|value| value.parse().unwrap()).collect();
        assert_eq!([raw.width, raw.height, raw.cpp], dimensions.as_slice(), "{name}: sensor dimensions");
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        let mut feed = |byte: u8| hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
        match &raw.data {
            RawData::U16(samples) => {
                assert_eq!(columns[9], "u16", "{name}: sample type");
                for sample in samples {
                    for byte in sample.to_le_bytes() {
                        feed(byte);
                    }
                }
            }
            RawData::F32(samples) => {
                assert_eq!(columns[9], "f32", "{name}: sample type");
                for sample in samples {
                    assert!(sample.is_finite(), "{name}: non-finite sample");
                    for byte in sample.to_le_bytes() {
                        feed(byte);
                    }
                }
            }
        }
        assert_eq!(format!("{hash:016x}"), columns[10], "{name}: sensor samples changed");
        assert!(raw.white_at(0) > raw.black.mean(), "{name}: invalid black/white levels");
        checked += 1;
        eprintln!("checked {name}: {}x{}, {hash:016x}", raw.width, raw.height);
    }
    eprintln!("camera-coverage sweep: {checked} files checked");
}
