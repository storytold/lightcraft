//! CC0 raw.pixls.us fixtures, downloaded locally as described in docs/nikon-he.md.
//! These hashes cover every sensor sample, including borders, in u16 little-endian order.
//! Reference: redlamp-libraw b9fd8fc63c5e323595c32ae5cefe0098d0c3d178, run as a black-box
//! executable; no decoder source, camera matrices or binary fixtures are part of this change.

use lcc_raw::{RawData, RawFormat};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn check(name: &str, input_sha: &str, sensor_sha: &str) {
    let root = lcc_brand::env_os("CORPUS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"));
    let path = root.join("nikon-he").join(name);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            lcc_testkit::skip!(Corpus, "{} absent (see docs/nikon-he.md)", path.display());
            return;
        }
        Err(e) => panic!("{}: {e}", path.display()),
    };
    assert_eq!(format!("{:x}", Sha256::digest(&bytes)), input_sha, "wrong corpus input: {name}");
    let raw = lcc_raw::decode(&bytes).unwrap();
    raw.validate().unwrap();
    assert_eq!(raw.format, RawFormat::Nef);
    assert_eq!((raw.width, raw.height, raw.cpp, raw.bits), (5600, 3728, 1, 14));
    assert_eq!(raw.black.values, vec![1008.0]);
    assert_eq!(raw.white, vec![16383.0]);
    assert_eq!(raw.cfa.as_ref().unwrap().name(), "RGGB");
    let RawData::U16(samples) = &raw.data else { panic!("expected sensor samples") };
    let mut hash = Sha256::new();
    for chunk in samples.chunks(4096) {
        hash.update(chunk.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>());
    }
    assert_eq!(format!("{:x}", hash.finalize()), sensor_sha, "sensor samples differ: {name}");
}

#[test]
fn z50_ii_high_efficiency_matches_every_reference_sample() {
    check(
        "Z50_2_3x2_HE_71c12d39.NEF",
        "71c12d398e6f5a01e0e32c2211c40cafdb29e85ba1e17c4f65e7a4e958754c54",
        "046f78f95763f43cb9888a40cf2bb2d50c29b8268ca82f000a30516f5da6d846",
    );
}

#[test]
fn z50_ii_high_efficiency_star_matches_every_reference_sample() {
    check(
        "Z50_2_3x2_HEstar_0add1dad.NEF",
        "0add1dad5776669a4896640de31b128032f893e13418f6569da7a4744a05ed42",
        "9c73b6e0be35e9d0951470e35ec3962f7ccd6627d3fba301ea32ca80ef5430f7",
    );
}
