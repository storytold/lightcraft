//! One CC0 file per uncompressed Olympus ORF and Samsung SRW body from raw.pixls.us (URLs and SHA-256 in `RAW_SAMPLES`,
//! xtask/src/main.rs; downloaded into the gitignored corpus/raw by `cargo xtask corpus --download`).
//!
//! No external decoder was run on these: the FNV-1a hashes over the decoded u16 samples (row-major,
//! little-endian, masked borders included) pin LightCraft's own decode, and the check against the camera is
//! black-box: the green sites of the active area, on a 24 × 16 grid, must follow the camera's own embedded
//! JPEG (gamma-encoded Pearson r; 0.70–1.00 on these files, a misread layout or stream gives r near 0). The E-10 is
//! pinned only (its small JPEG gives r 0.59); its render was inspected by eye.
//! Missing files are reported as skipped.

use lightcraft_raw::{RawData, decode, embedded_preview, probe_info};
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
