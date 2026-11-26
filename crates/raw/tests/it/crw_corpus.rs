//! Canon CRW through the shared craft-crw decoder, on the CC0 corpus files (URLs and SHA-256 in
//! `xtask/src/raw_samples.rs`; downloaded into the gitignored corpus/raw by `cargo xtask corpus --download`).
//!
//! The FNV-1a hashes over the decoded u16 samples (row-major, little-endian, masked borders
//! included) were computed from LightCraft's earlier in-tree clean-room CRW decoder (commit
//! dd02f12f, branch `clean-room-crw-format-decode`) before it moved to craft-libs: the shared
//! decoder reproduces it bit for bit on all ten files, six bodies. Black and white levels are not
//! pinned: craft-crw changed both on purpose (border padding no longer lowers the black level; the
//! white level follows these bodies' low-ISO clip near 4000), so they are only checked for sanity.

use lightcraft_raw::{RawData, RawError, decode};
use std::path::{Path, PathBuf};

fn corpus() -> PathBuf {
    std::env::var_os("LIGHTCRAFT_CORPUS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
}

fn fnv1a(samples: &[u16]) -> u64 {
    samples.iter().flat_map(|v| v.to_le_bytes()).fold(0xcbf2_9ce4_8422_2325u64, |h, byte| (h ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3))
}

/// (file, sensor width × height, FNV-1a of the samples from the earlier in-tree decoder)
const CASES: &[(&str, (usize, usize), u64)] = &[
    ("crw-canon-eos-10d-a.crw", (3152, 2068), 0xb8a6_7781_fa37_b1df),
    ("crw-canon-eos-10d-b.crw", (3152, 2068), 0xeeea_3c68_2fb5_62ae),
    ("crw-canon-eos-10d-c.crw", (3152, 2068), 0x060d_d320_55aa_d79e),
    ("crw-canon-eos-300d-a.crw", (3152, 2068), 0xa9a0_5cc0_ca19_01bd),
    ("crw-canon-eos-300d-b.crw", (3152, 2068), 0xf0c3_98e8_be78_58cc),
    ("crw-canon-eos-d30.crw", (2224, 1456), 0x1394_ca5c_1cb8_816d),
    ("crw-canon-eos-d60.crw", (3152, 2068), 0x63e2_4897_3fe4_1cef),
    ("crw-canon-eos-digital-rebel.crw", (3152, 2068), 0x0db1_b751_a769_aa32),
    ("crw-canon-powershot-g1.crw", (2144, 1560), 0x29e5_4008_46b7_a54b),
    ("crw-canon-powershot-g6.crw", (3160, 2344), 0xa7fa_be95_3036_5b64),
];

#[test]
fn crw_samples_match_the_earlier_in_tree_decoder() {
    let mut seen = 0;
    for &(name, sensor, hash) in CASES {
        let path = corpus().join("raw").join(name);
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let raw = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        raw.validate().unwrap();
        assert_eq!((raw.width, raw.height, raw.cpp), (sensor.0, sensor.1, 1), "{name}: sensor size");
        let RawData::U16(samples) = &raw.data else { panic!("{name}: integer sensor data expected") };
        assert_eq!(fnv1a(samples), hash, "{name}: sensor samples differ from the earlier decoder");
        assert!(raw.white_at(0) > raw.black.mean() && raw.black.mean() > 0.0, "{name}: invalid black/white levels");
        seen += 1;
    }
    if seen == 0 {
        lightcraft_testkit::skip!(Corpus, "no CRW files in {}", corpus().join("raw").display());
    }
}

/// Green of the decoded mosaic against the camera's embedded JPEG on a `gw × gh` grid of the active area.
fn green_vs_camera_jpeg(raw: &lightcraft_raw::RawImage, jpeg: &[u8], gw: usize, gh: usize) -> f64 {
    let img = craft_image_codecs::decode_as_with(craft_image_codecs::Format::Jpeg, jpeg, &craft_image_codecs::DecodeOptions::default()).unwrap();
    let (jw, jh) = (img.width() as usize, img.height() as usize);
    let px = img.as_u8().unwrap();
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
    let cfa = raw.cfa.clone().expect("colour-filter layout");
    let (a, black, white) = (raw.active_area, f64::from(raw.black.mean()), f64::from(raw.white_at(0)));
    let (mut rs, mut rn) = (vec![0f64; gw * gh], vec![0f64; gw * gh]);
    for y in a.y..a.y + a.height {
        for x in a.x..a.x + a.width {
            if cfa.color_at(x, y) == 1 {
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

/// More CIFF PowerShots from the CC0 corpus: pinned sensor hashes (from this decoder) and the camera-JPEG
/// check that a misread layout would fail.
#[test]
fn powershot_g2_pro1_and_s70_decode_like_the_camera_jpeg() {
    // (file, sensor width × height, FNV-1a of the sensor samples)
    let cases: &[(&str, (usize, usize), u64)] = &[
        ("crw-canon-powershot-g2.crw", (2376, 1728), 0x53fd_a7c4_2bb0_a935),
        ("crw-canon-powershot-pro1.crw", (3344, 2484), 0x0a42_4ef0_f956_f64b),
        ("crw-canon-powershot-s70.crw", (3160, 2344), 0xea1f_13b8_33fe_ac1f),
    ];
    let mut seen = 0;
    for &(name, sensor, hash) in cases {
        let path = corpus().join("raw").join(name);
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let raw = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        raw.validate().unwrap();
        assert_eq!((raw.width, raw.height, raw.cpp), (sensor.0, sensor.1, 1), "{name}: sensor size");
        let RawData::U16(samples) = &raw.data else { panic!("{name}: integer sensor data expected") };
        assert_eq!(fnv1a(samples), hash, "{name}: sensor pixels differ from the pinned decode");
        assert!(raw.white_at(0) > raw.black.mean() && raw.black.mean() > 0.0, "{name}: invalid black/white levels");
        let jpeg = lightcraft_raw::embedded_preview(&bytes).unwrap_or_else(|| panic!("{name}: no camera JPEG"));
        let r = green_vs_camera_jpeg(&raw, &jpeg, 24, 16);
        eprintln!("{name:40} {}x{}: green follows the camera JPEG, r = {r:.3}", raw.width, raw.height);
        assert!(r > 0.6, "{name}: decoded mosaic does not follow the camera JPEG (r = {r:.3})");
        seen += 1;
    }
    if seen == 0 {
        lightcraft_testkit::skip!(Corpus, "no CRW files in {}", corpus().join("raw").display());
    }
}

/// A real CRW damaged two ways is an error, never a panic or a silently smeared frame.
#[test]
fn a_damaged_real_crw_is_an_error() {
    let path = corpus().join("raw").join("crw-canon-eos-d60.crw");
    let Ok(bytes) = std::fs::read(&path) else {
        lightcraft_testkit::skip!(Corpus, "{} absent", path.display());
        return;
    };
    // cut in half: the CIFF heap's record table sits at the end, so the file no longer parses
    let cut = &bytes[..bytes.len() / 2];
    assert!(matches!(decode(cut), Err(RawError::Corrupt(_) | RawError::NotRaw)), "{:?}", decode(cut).err());
    // the length kept but a run of the sensor data overwritten with an invalid code
    let mut garbled = bytes.clone();
    let mid = garbled.len() / 2;
    garbled[mid..mid + 4096].fill(0xff);
    assert!(matches!(decode(&garbled), Err(RawError::Corrupt(_))), "{:?}", decode(&garbled).err());
}
