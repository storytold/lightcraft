//! One CC0 file per uncompressed Olympus ORF and Samsung SRW body from raw.pixls.us (URLs and SHA-256 in `RAW_SAMPLES`,
//! xtask/src/main.rs; downloaded into the gitignored corpus/raw by `cargo xtask corpus --download`).
//!
//! No external decoder was run on these: the FNV-1a hashes over the decoded u16 samples (row-major,
//! little-endian, masked borders included) pin LightCraft's own decode, and the check against the camera is
//! black-box: the green sites of the active area, on a 24 × 16 grid, must follow the camera's own embedded
//! JPEG (gamma-encoded Pearson r; 0.70–1.00 on these files, a misread layout or stream gives r near 0). The E-10 is
//! pinned only (its small JPEG gives r 0.59); its render was inspected by eye.
//! Missing files are reported as skipped.

use lightcraft_raw::{RawData, RawFormat, decode, embedded_preview, probe, probe_info};
use std::path::{Path, PathBuf};

fn corpus() -> PathBuf {
    std::env::var_os("LIGHTCRAFT_CORPUS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
}

/// Green of the decoded mosaic against the camera's embedded JPEG on a `gw × gh` grid of the active area.
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

#[test]
fn olympus_and_samsung_bodies_decode_like_the_camera_jpeg() {
    // (file, sensor width × height, pinned FNV-1a of the sensor samples, compare with the camera JPEG)
    let cases: &[(&str, (usize, usize), u64, bool)] = &[
        ("dng-samsung-gx10.dng", (3936, 2624), 0x25d5_1c29_e67f_99e8, true),
        ("dng-samsung-gx20.dng", (4864, 3136), 0x516f_3df8_c879_a2c7, true),
        ("orf-olympus-c5060wz.orf", (2608, 1949), 0x4185_d439_e31d_72bf, true),
        ("orf-olympus-c7070wz.orf", (3088, 2309), 0x016a_f9d1_89e5_4aaf, true),
        ("orf-olympus-e-10.orf", (2256, 1684), 0x1029_c758_4810_f56b, false),
        ("orf-olympus-e-300.orf", (3360, 2504), 0xa9b4_e783_03a4_45c2, true),
        ("orf-olympus-e-330.orf", (3280, 2450), 0xc938_38fa_9822_c81a, true),
        ("orf-olympus-e-500.orf", (3360, 2504), 0x5e22_2ac8_17ed_89d9, true),
        ("orf-olympus-e-m5-mark-ii.orf", (9280, 6932), 0xa222_be38_c767_7782, true),
        ("orf-olympus-pen-f.orf", (10400, 7796), 0x415e_bf27_092e_9897, true),
        ("orf-olympus-sh-2.orf", (4624, 3470), 0xb25b_8d24_4ade_1f3d, true),
        ("orf-olympus-sp510uz.orf", (3088, 2309), 0xa885_b7ca_83ba_02f1, true),
        ("orf-olympus-sp550uz.orf", (3088, 2309), 0x30ac_5e9f_34b9_8645, true),
        ("orf-olympus-sp565uz.orf", (3664, 2741), 0x0962_c436_052d_efa8, true),
        ("orf-olympus-sp570uz.orf", (3664, 2741), 0xdd62_06d6_17d0_8a46, true),
        ("orf-olympus-xz-10.orf", (3984, 2990), 0x6274_1a67_cd42_40f7, true),
        ("srw-samsung-nx1000.srw", (5632, 3720), 0xd99f_f1d0_e3d6_435f, true),
        ("srw-samsung-nx11.srw", (4704, 3124), 0x29b5_58f8_09e4_85cb, true),
        ("srw-samsung-nx1100.srw", (5632, 3720), 0x69cd_048b_c8a1_dd7a, true),
        ("srw-samsung-nx200.srw", (5632, 3720), 0x61ef_6100_b9f6_dabd, true),
        ("srw-samsung-nx210.srw", (5632, 3720), 0x50d5_e55e_6ec7_7faa, true),
        ("srw-samsung-nx5.srw", (4704, 3124), 0xf20d_905b_41b3_32f6, true),
        ("srw-samsung-wb2000.srw", (3728, 2781), 0xe40f_1c19_41ce_9000, true),
    ];
    let mut seen = 0;
    for &(name, sensor, hash, jpeg_check) in cases {
        let path = corpus().join("raw").join(name);
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("skip: {} absent", path.display());
            continue;
        };
        let raw = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        raw.validate().unwrap();
        assert_eq!((raw.width, raw.height, raw.cpp), (sensor.0, sensor.1, 1), "{name}: sensor size");
        assert_eq!(raw.info(), probe_info(&bytes).unwrap(), "{name}: probe and decode disagree");
        let RawData::U16(samples) = &raw.data else { panic!("{name}: integer sensor data expected") };
        let actual = samples
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .fold(0xcbf2_9ce4_8422_2325u64, |h, byte| (h ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3));
        assert_eq!(actual, hash, "{name}: sensor pixels differ from the pinned decode");
        assert!(raw.white_at(0) > raw.black.mean(), "{name}: invalid black/white levels");
        if !jpeg_check {
            seen += 1;
            continue;
        }
        let jpeg = embedded_preview(&bytes).unwrap_or_else(|| panic!("{name}: no camera JPEG"));
        let r = green_vs_camera_jpeg(&raw, &jpeg, 24, 16);
        eprintln!("{name:40} {}x{}: green follows the camera JPEG, r = {r:.3}", raw.width, raw.height);
        assert!(r > 0.6, "{name}: decoded mosaic does not follow the camera JPEG (r = {r:.3})");
        seen += 1;
    }
    eprintln!("Olympus / Samsung bodies checked on {seen} files");
}

/// Compressed Olympus ORF and ORI files decode correctly and correlate with the camera embedded JPEG preview.
#[test]
fn compressed_olympus_bodies_decode_like_camera_jpeg() {
    let cases = [
        ("orf-olympus-em1.orf", (4640, 3472)),
        ("orf-olympus-em10iii.orf", (4640, 3472)),
        ("ori-olympus-e-m1markii-16bit-4-3-2.ori", (5240, 3912)),
        ("ori-olympus-e-m5-mark-ii-16bit-16bit-normal-4-3.ori", (4640, 3472)),
    ];
    for &(name, (expected_w, expected_h)) in &cases {
        let mut path = corpus().join("raw").join(name);
        if !path.exists() {
            let alt_name = match name {
                "ori-olympus-e-m1markii-16bit-4-3-2.ori" => "Olympus - E-M1MarkII - 16bit (4_3).ORI",
                "ori-olympus-e-m5-mark-ii-16bit-16bit-normal-4-3.ori" => "Olympus - E-M5 Mark II - 16bit 16bit _normal_ (4_3).ORI",
                _ => name,
            };
            let alt_path = corpus().join("raw").join(alt_name);
            if alt_path.exists() {
                path = alt_path;
            }
        }
        let Ok(bytes) = std::fs::read(&path) else {
            lightcraft_testkit::skip!(Corpus, "{} absent", path.display());
            continue;
        };
        assert_eq!(probe(&bytes), Some(RawFormat::Orf), "{name}");
        let raw = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        raw.validate().unwrap();
        assert_eq!((raw.width, raw.height), (expected_w, expected_h), "{name}: sensor size mismatch");
        assert_eq!(raw.info(), probe_info(&bytes).unwrap(), "{name}: probe and decode disagree");

        let jpeg = embedded_preview(&bytes).unwrap_or_else(|| panic!("{name}: no camera JPEG"));
        let r = green_vs_camera_jpeg(&raw, &jpeg, 24, 16);
        eprintln!("{name:55} {}x{}: green follows the camera JPEG, r = {r:.3}", raw.width, raw.height);
        assert!(r > 0.92, "{name}: decoded mosaic does not follow the camera JPEG (r = {r:.3})");
    }
}

/// All 112 Olympus and OM System ORF and ORI corpus samples decode without panic,
/// match their exact sensor dimensions, agree between probe and decode, and correlate
/// with the embedded camera JPEG preview (Pearson r > 0.60; E-10 r > 0.55).
#[test]
fn all_112_olympus_corpus_samples_decode_like_camera_jpeg() {
    let cases = [
        ("orf-olympus-e1.orf", (2624, 1966)),
        ("orf-olympus-e400.orf", (3768, 2840)),
        ("orf-olympus-em1.orf", (4640, 3472)),
        ("orf-olympus-em10iii.orf", (4640, 3472)),
        ("orf-olympus-xz2.orf", (3984, 2986)),
        ("orf-olympus-sp550uz.orf", (3088, 2309)),
        ("orf-olympus-e-10.orf", (2256, 1684)),
        ("orf-olympus-c5060wz.orf", (2608, 1949)),
        ("orf-olympus-sp565uz.orf", (3664, 2741)),
        ("orf-olympus-e-m5-mark-ii.orf", (9280, 6932)),
        ("orf-olympus-e-300.orf", (3360, 2504)),
        ("orf-olympus-sh-2.orf", (4624, 3470)),
        ("orf-olympus-c7070wz.orf", (3088, 2309)),
        ("orf-olympus-xz-10.orf", (3984, 2990)),
        ("orf-olympus-e-330.orf", (3280, 2450)),
        ("orf-olympus-sp570uz.orf", (3664, 2741)),
        ("orf-olympus-sp510uz.orf", (3088, 2309)),
        ("orf-olympus-pen-f.orf", (10400, 7796)),
        ("orf-olympus-e-500.orf", (3360, 2504)),
        ("orf-olympus-c8080wz-12bit-4-3.orf", (3280, 2453)),
        ("orf-olympus-e-p3-16bit-4-3.orf", (4080, 3040)),
        ("orf-olympus-e-520-16bit-4-3.orf", (3720, 2800)),
        ("orf-olympus-e-30-16bit-4-3.orf", (4100, 3084)),
        ("orf-olympus-sp500uz-12bit-4-3.orf", (2832, 2117)),
        ("orf-olympus-e-600-16bit-4-3.orf", (4100, 3084)),
        ("orf-olympus-c5050z-12bit-4-3.orf", (2576, 1925)),
        ("orf-olympus-sp350.orf", (3280, 2453)),
        ("orf-olympus-e-20.orf", (2576, 1924)),
        ("orf-olympus-e-pm1-16bit-4-3.orf", (4080, 3040)),
        ("orf-olympus-e-m5-16bit-4-3.orf", (4640, 3472)),
        ("orf-olympus-e-p5-16bit-4-3.orf", (4640, 3472)),
        ("orf-olympus-tg-4-16bit-4-3.orf", (4640, 3472)),
        ("orf-olympus-e-410-16bit-4-3.orf", (3720, 2800)),
        ("orf-olympus-e-pm2-16bit-4-3.orf", (4640, 3472)),
        ("orf-olympus-e-pl3-16bit-4-3.orf", (4080, 3040)),
        ("orf-olympus-e-m10-16bit-4-3.orf", (4640, 3472)),
        ("orf-olympus-e-pl6-16bit-4-3.orf", (4640, 3472)),
        ("orf-olympus-e-pl7-16bit-4-3.orf", (4640, 3472)),
        ("orf-olympus-tg-5-16bit-4-3.orf", (4040, 3016)),
        ("orf-olympus-e-pl8-16bit-4-3.orf", (4640, 3472)),
        ("orf-olympus-e-pl1-16bit-4-3.orf", (4100, 3084)),
        ("orf-olympus-xz-1-16bit-4-3.orf", (3680, 2760)),
        ("orf-olympus-e-pl5-16bit-4-3.orf", (4640, 3472)),
        ("orf-olympus-e-pl2-16bit-4-3.orf", (4100, 3084)),
        ("orf-olympus-e-3-16bit-4-3.orf", (3720, 2800)),
        ("orf-olympus-e-620-16bit-4-3.orf", (4100, 3084)),
        ("orf-olympus-e-m1markii-16bit-4-3.orf", (5240, 3912)),
        ("orf-olympus-e-m1markii-16bit-4-3-2.orf", (10400, 7792)),
        ("ori-olympus-e-m1markii-16bit-4-3.ori", (5240, 3912)),
        ("orf-olympus-e-m1markii-16bit-4-3-3.orf", (10400, 7792)),
        ("ori-olympus-e-m1markii-16bit-4-3-2.ori", (5240, 3912)),
        ("orf-olympus-e-m1markii-16bit-4-3-4.orf", (5240, 3912)),
        ("orf-olympus-e-420-16bit-4-3.orf", (3720, 2800)),
        ("orf-olympus-e-5-16bit-4-3.orf", (4100, 3084)),
        ("orf-olympus-e-p2-16bit-4-3.orf", (4100, 3084)),
        ("orf-olympus-e-pl9-16bit-4-3.orf", (4640, 3472)),
        ("orf-olympus-stylus11s-16bit-4-3.orf", (4000, 2992)),
        ("ori-olympus-e-m5-mark-ii-16bit-16bit-normal-4-3.ori", (4640, 3472)),
        ("orf-olympus-pen-f-16bit-4-3-2.orf", (5200, 3904)),
        ("orf-olympus-e-m1x-16bit-4-3.orf", (5240, 3912)),
        ("orf-olympus-tg-6-16bit-4-3.orf", (4040, 3016)),
        ("orf-olympus-stylus1-16bit-4-3.orf", (4000, 2992)),
        ("orf-olympus-e-m5-mark-iii-16bit-4-3.orf", (10400, 7792)),
        ("orf-olympus-e-m5-mark-iii-16bit-4-3-2.orf", (5240, 3912)),
        ("orf-olympus-e-m1markiii-16bit-4-3.orf", (5240, 3912)),
        ("orf-olympus-e-m1markiii-16bit-4-3-2.orf", (8200, 6132)),
        ("ori-olympus-e-m1markiii-16bit-4-3.ori", (5240, 3912)),
        ("orf-olympus-e-m1markiii-16bit-4-3-3.orf", (10400, 7792)),
        ("ori-olympus-e-m1markiii-16bit-4-3-2.ori", (5240, 3912)),
        ("orf-olympus-e-pl10-16bit-4-3.orf", (4640, 3472)),
        ("orf-olympus-e-m10-mark-iv-16bit-4-3.orf", (5200, 3904)),
        ("orf-olympus-e-m10-mark-iv-16bit-4-3-2.orf", (5200, 3904)),
        ("orf-olympus-e-m10-mark-iiis-16bit-4-3.orf", (4640, 3472)),
        ("orf-olympus-e-p1-16bit-4-3.orf", (4100, 3084)),
        ("orf-om-system-om-1-16bit-4-3.orf", (5220, 3912)),
        ("orf-om-system-om-1-16bit-4-3-2.orf", (8180, 6132)),
        ("orf-om-system-om-1-16bit-4-3-3.orf", (10400, 7792)),
        ("ori-om-system-om-1-16bit-4-3.ori", (5220, 3912)),
        ("ori-om-system-om-1-16bit-4-3-2.ori", (5220, 3912)),
        ("orf-olympus-e-510-16bit-4-3.orf", (3720, 2800)),
        ("orf-olympus-e-450-16bit-4-3.orf", (3720, 2800)),
        ("orf-olympus-e-m10-mark-ii-16bit-4-3.orf", (4640, 3472)),
        ("orf-olympus-e-p7-16bit-4-3.orf", (5200, 3904)),
        ("orf-om-system-om-5-16bit-4-3.orf", (5240, 3912)),
        ("orf-om-system-om-5-16bit-4-3-2.orf", (8200, 6132)),
        ("ori-om-system-om-5-16bit-4-3.ori", (5240, 3912)),
        ("orf-om-system-om-5-16bit-4-3-3.orf", (10400, 7792)),
        ("ori-om-system-om-5-16bit-4-3-2.ori", (5240, 3912)),
        ("orf-om-system-tg-7-16bit-4-3.orf", (4040, 3016)),
        ("orf-om-system-om-1-mark-ii-16bit-4-3.orf", (5220, 3912)),
        ("orf-om-system-om-1-mark-ii-16bit-4-3-2.orf", (8180, 6132)),
        ("ori-om-system-om-1-mark-ii-16bit-4-3.ori", (5220, 3912)),
        ("orf-om-system-om-1-mark-ii-16bit-4-3-3.orf", (10400, 7792)),
        ("ori-om-system-om-1-mark-ii-16bit-4-3-2.ori", (5220, 3912)),
        ("orf-om-system-om-1-mark-ii-16bit-4-3-4.orf", (10386, 7792)),
        ("ori-om-system-om-1-mark-ii-16bit-4-3-3.ori", (5220, 3912)),
        ("orf-om-system-om-1-mark-ii-16bit-4-3-5.orf", (8172, 6132)),
        ("ori-om-system-om-1-mark-ii-16bit-4-3-4.ori", (5220, 3912)),
        ("orf-om-system-om-3-16bit-4-3.orf", (5220, 3912)),
        ("orf-om-system-om-3-16bit-4-3-2.orf", (8180, 6132)),
        ("ori-om-system-om-3-16bit-4-3.ori", (5220, 3912)),
        ("orf-om-system-om-3-16bit-4-3-3.orf", (10400, 7792)),
        ("ori-om-system-om-3-16bit-4-3-2.ori", (5220, 3912)),
        ("orf-om-system-om-3-16bit-4-3-4.orf", (10386, 7792)),
        ("ori-om-system-om-3-16bit-4-3-3.ori", (5220, 3912)),
        ("orf-om-system-om-3-16bit-4-3-5.orf", (8172, 6132)),
        ("ori-om-system-om-3-16bit-4-3-4.ori", (5220, 3912)),
        ("orf-om-system-om-5-mark-ii-16bit-4-3.orf", (5240, 3912)),
        ("ori-om-system-om-5-mark-ii-16bit-4-3.ori", (5240, 3912)),
        ("orf-om-system-om-5-mark-ii-16bit-4-3-2.orf", (8200, 6132)),
        ("orf-om-system-om-5-mark-ii-16bit-4-3-3.orf", (10400, 7792)),
        ("ori-om-system-om-5-mark-ii-16bit-4-3-2.ori", (5240, 3912)),
    ];
    let mut tested = 0;
    for &(name, (expected_w, expected_h)) in &cases {
        let mut path = corpus().join("raw").join(name);
        if !path.exists() {
            let alt_name = match name {
                "ori-olympus-e-m1markii-16bit-4-3-2.ori" => "Olympus - E-M1MarkII - 16bit (4_3).ORI",
                "ori-olympus-e-m5-mark-ii-16bit-16bit-normal-4-3.ori" => "Olympus - E-M5 Mark II - 16bit 16bit _normal_ (4_3).ORI",
                _ => name,
            };
            let alt_path = corpus().join("raw").join(alt_name);
            if alt_path.exists() {
                path = alt_path;
            }
        }
        let Ok(bytes) = std::fs::read(&path) else {
            lightcraft_testkit::skip!(Corpus, "{} absent", path.display());
            continue;
        };
        assert_eq!(probe(&bytes), Some(RawFormat::Orf), "{name}");
        let raw = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        raw.validate().unwrap();
        assert_eq!((raw.width, raw.height), (expected_w, expected_h), "{name}: sensor size mismatch");
        assert_eq!(raw.info(), probe_info(&bytes).unwrap(), "{name}: probe and decode disagree");

        let jpeg = embedded_preview(&bytes).unwrap_or_else(|| panic!("{name}: no camera JPEG"));
        let r = green_vs_camera_jpeg(&raw, &jpeg, 24, 16);
        eprintln!("{name:55} {}x{}: green follows camera JPEG, r = {r:.3}", raw.width, raw.height);
        // E-10 has a tiny low-quality thumbnail yielding ~0.59; others are >= 0.60
        let min_r = if name == "orf-olympus-e-10.orf" { 0.55 } else { 0.60 };
        assert!(r > min_r, "{name}: decoded mosaic does not follow camera JPEG (r = {r:.3})");
        tested += 1;
    }
    eprintln!("All Olympus/OM ORF and ORI files checked on {tested} files");

    // Independent assertion on checked-in compressed ORF fixture: ensures compressed decode
    // regression testing runs even when optional corpus files are absent.
    assert_eq!(probe(COMPRESSED_ORF_FIXTURE), Some(RawFormat::Orf));
    let fixture_raw = decode(COMPRESSED_ORF_FIXTURE).unwrap_or_else(|e| panic!("compressed ORF fixture: {e}"));
    fixture_raw.validate().unwrap();
    assert_eq!((fixture_raw.width, fixture_raw.height, fixture_raw.bits), (4, 2, 12));
    assert_eq!(fixture_raw.info(), probe_info(COMPRESSED_ORF_FIXTURE).unwrap());
    let RawData::U16(samples) = &fixture_raw.data else { panic!("compressed ORF fixture: integer sensor data expected") };
    assert_eq!(samples.as_slice(), &[0u16; 8]);
}

/// A 266-byte checked-in compressed Olympus ORF fixture (4x2, 12-bit, RGGB CFA) with known sensor samples.
const COMPRESSED_ORF_FIXTURE: &[u8] = &[
    73, 73, 82, 79, 140, 0, 0, 0, 0, 0, 0, 1, 0, 0, 16, 16, 16, 16, 16, 16, 16, 16, 0, 0, 79, 76, 89, 77, 80, 0, 1, 0, 2, 0, 64, 32, 7, 0, 18, 0, 0,
    0, 62, 0, 0, 0, 44, 16, 3, 0, 1, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0, 0, 1, 0, 83, 6, 3, 0, 1, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 2, 0, 1, 1, 2,
    2, 0, 124, 146, 7, 0, 56, 0, 0, 0, 24, 0, 0, 0, 2, 163, 7, 0, 8, 0, 0, 0, 80, 0, 0, 0, 0, 0, 0, 0, 79, 76, 89, 77, 80, 85, 83, 32, 73, 77, 65,
    71, 73, 78, 71, 32, 67, 79, 82, 80, 46, 0, 10, 0, 0, 1, 4, 0, 1, 0, 0, 0, 4, 0, 0, 0, 1, 1, 4, 0, 1, 0, 0, 0, 2, 0, 0, 0, 2, 1, 3, 0, 1, 0, 0, 0,
    16, 0, 0, 0, 3, 1, 3, 0, 1, 0, 0, 0, 1, 0, 0, 0, 6, 1, 3, 0, 1, 0, 0, 0, 1, 0, 0, 0, 15, 1, 2, 0, 22, 0, 0, 0, 118, 0, 0, 0, 17, 1, 4, 0, 1, 0,
    0, 0, 8, 0, 0, 0, 22, 1, 4, 0, 1, 0, 0, 0, 2, 0, 0, 0, 23, 1, 4, 0, 1, 0, 0, 0, 16, 0, 0, 0, 105, 135, 4, 0, 1, 0, 0, 0, 88, 0, 0, 0, 0, 0, 0, 0,
];

#[test]
fn compressed_orf_checked_in_fixture_decodes() {
    assert_eq!(probe(COMPRESSED_ORF_FIXTURE), Some(RawFormat::Orf));
    let raw = decode(COMPRESSED_ORF_FIXTURE).unwrap_or_else(|e| panic!("compressed ORF fixture: {e}"));
    raw.validate().unwrap();
    assert_eq!((raw.width, raw.height, raw.bits), (4, 2, 12));
    assert_eq!(raw.info(), probe_info(COMPRESSED_ORF_FIXTURE).unwrap());
    let RawData::U16(samples) = &raw.data else { panic!("compressed ORF fixture: integer sensor data expected") };
    assert_eq!(samples.as_slice(), &[0u16; 8]);
}
