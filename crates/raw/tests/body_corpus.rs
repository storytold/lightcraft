//! One CC0 file per Nikon and Fujifilm body from raw.pixls.us (URLs and SHA-256 in `RAW_SAMPLES`,
//! xtask/src/main.rs; downloaded into the gitignored corpus/raw by `cargo xtask corpus --download`).
//!
//! No external decoder was run on these: the FNV-1a hashes over the decoded u16 samples (row-major,
//! little-endian, masked borders included) pin LightCraft's own decode, and the check against the camera is
//! black-box: the green sites of the active area, on a 24 × 16 grid, must follow the camera's own embedded
//! JPEG (gamma-encoded Pearson r; 0.77–1.00 on these files, a misread layout or stream gives r near 0).
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
fn nikon_and_fujifilm_bodies_decode_like_the_camera_jpeg() {
    // (file, sensor width × height, pinned FNV-1a of the sensor samples)
    let cases: &[(&str, (usize, usize), u64)] = &[
        ("dng-nikon-d80.dng", (3900, 2611), 0x0f79_2821_2660_abae),
        ("nef-nikon-1-aw1.nef", (4620, 3084), 0xbe33_8eec_3c72_fa8f),
        ("nef-nikon-1-j1.nef", (3904, 2606), 0x29e0_7678_537c_7bc8),
        ("nef-nikon-1-j2.nef", (3904, 2606), 0xd7b6_ca84_0c3f_1260),
        ("nef-nikon-1-j3.nef", (4620, 3084), 0x83de_9d80_3aaa_ccf0),
        ("nef-nikon-1-j4.nef", (5248, 3504), 0xf226_1ee3_578c_e841),
        ("nef-nikon-1-j5.nef", (5584, 3726), 0x525e_c8ca_df29_13f1),
        ("nef-nikon-1-s1.nef", (3948, 2604), 0x0464_ed24_fdd0_772d),
        ("nef-nikon-1-v1.nef", (3904, 2606), 0xedfd_ff98_70dd_9e89),
        ("nef-nikon-1-v2.nef", (4620, 3084), 0x99af_e262_e1d0_6ccb),
        ("nef-nikon-1-v3.nef", (5248, 3504), 0x0951_aa90_ad1f_50ae),
        ("nef-nikon-coolpix-a.nef", (4992, 3280), 0x875a_ed86_a3ad_09c6),
        ("nef-nikon-d1h.nef", (2012, 1324), 0x27ce_4372_4d64_8bc5),
        ("nef-nikon-d200.nef", (3904, 2616), 0xd33f_f00a_a90f_0f78),
        ("nef-nikon-d2h.nef", (2496, 1648), 0x0ca6_7d68_03aa_4ae1),
        ("nef-nikon-d2hs.nef", (2496, 1648), 0x9f70_b976_d7d0_a5a8),
        ("nef-nikon-d2x.nef", (4320, 2868), 0xd5b9_d3b6_1efc_6cb8),
        ("nef-nikon-d2xs.nef", (4320, 2868), 0x4f84_d44d_0d9f_c09f),
        ("nef-nikon-d3000.nef", (3904, 2616), 0x9311_0df8_1dca_1e6a),
        ("nef-nikon-d300.nef", (4352, 2868), 0xc7f3_1049_d8f8_17db),
        ("nef-nikon-d300s.nef", (4352, 2868), 0x9c8c_97ba_c369_e6ea),
        ("nef-nikon-d3100.nef", (4672, 3084), 0x916d_09e7_01cf_0314),
        ("nef-nikon-d3200.nef", (6080, 4012), 0xd267_603b_2d71_6fe0),
        ("nef-nikon-d3300.nef", (6016, 4016), 0x93ea_fe7f_2bba_3f5c),
        ("nef-nikon-d3400.nef", (6016, 4016), 0xa55c_8f4a_662f_f9af),
        ("nef-nikon-d3500.nef", (6016, 4016), 0xb1ee_bbd1_346a_3d92),
        ("nef-nikon-d3.nef", (4288, 2844), 0x722b_6aaa_4347_1793),
        ("nef-nikon-d3s.nef", (4288, 2844), 0x2703_65d8_5629_1ea0),
        ("nef-nikon-d3x.nef", (6080, 4044), 0x52c3_e70b_7199_951e),
        ("nef-nikon-d40.nef", (3040, 2014), 0x8536_6f21_fbb0_5eb9),
        ("nef-nikon-d40x.nef", (3904, 2616), 0xffe9_2bb5_14ca_534f),
        ("nef-nikon-d4.nef", (4992, 3292), 0xd8c0_c164_e77e_3d65),
        ("nef-nikon-d4s.nef", (4936, 3288), 0xfb04_e701_dab5_189e),
        ("nef-nikon-d5000.nef", (4352, 2868), 0xb8f6_8efb_0254_6fbc),
        ("nef-nikon-d500.nef", (5600, 3728), 0x9482_f4ef_7856_ad4e),
        ("nef-nikon-d50.nef", (3040, 2014), 0xe23b_8f5e_4b96_b285),
        ("nef-nikon-d5200.nef", (6036, 4020), 0xd148_2f59_a727_a1e2),
        ("nef-nikon-d5300.nef", (6016, 4016), 0xb141_e60b_754f_0ffc),
        ("nef-nikon-d5500.nef", (6016, 4016), 0x2ce0_0d79_4dac_db43),
        ("nef-nikon-d5600.nef", (6016, 4016), 0x2a20_0cfc_45f1_5db0),
        ("nef-nikon-d5.nef", (5584, 3728), 0xfc01_902d_7732_c5ba),
        ("nef-nikon-d600.nef", (6080, 4028), 0x9936_16eb_56f3_7729),
        ("nef-nikon-d60.nef", (3904, 2616), 0x314f_043e_4a1e_0428),
        ("nef-nikon-d610.nef", (6080, 4028), 0x5948_ec20_f2c9_f1ff),
        ("nef-nikon-d700.nef", (4288, 2844), 0x507c_19fc_f74a_ce38),
        ("nef-nikon-d70.nef", (3040, 2014), 0xb148_5e4c_0948_bcf7),
        ("nef-nikon-d70s.nef", (3040, 2014), 0xcd2e_6f87_1fbe_8c5e),
        ("nef-nikon-d7100.nef", (6036, 4020), 0x0cbd_07b1_c8d0_5524),
        ("nef-nikon-d7200.nef", (6016, 4016), 0xe0a0_2fd9_5e26_c48b),
        ("nef-nikon-d750.nef", (6032, 4032), 0xc229_3104_e9c0_8a7f),
        ("nef-nikon-d780.nef", (6064, 4040), 0x9394_1205_66c4_0394),
        ("nef-nikon-d800e.nef", (7424, 4924), 0x2ebe_8c60_c0fd_0c98),
        ("nef-nikon-d800.nef", (7424, 4924), 0x205d_3756_412d_9312),
        ("nef-nikon-d810.nef", (7380, 4928), 0x7065_5115_76ac_80d5),
        ("nef-nikon-d850.nef", (6216, 4136), 0x7735_4e78_daec_87f0),
        ("nef-nikon-d90.nef", (4352, 2868), 0x648c_5bdf_7287_4b88),
        ("nef-nikon-df.nef", (4992, 3292), 0x563f_1897_96a9_31df),
        ("nef-nikon-z-30.nef", (5600, 3728), 0x949f_f3eb_d96e_2444),
        ("nef-nikon-z50-2.nef", (5600, 3728), 0xb896_6b21_ac27_ad07),
        ("nef-nikon-z-50.nef", (5600, 3728), 0x3409_8168_87db_9bee),
        ("nef-nikon-z-5.nef", (6040, 4032), 0x214b_cc7b_5954_7ad2),
        ("nef-nikon-z-6-2.nef", (6064, 4040), 0xd40b_c647_373d_c2f8),
        ("nef-nikon-z-7-2.nef", (8288, 5520), 0xde38_97ec_16cc_c71e),
        ("nef-nikon-z-9.nef", (8280, 5520), 0x7be1_80fd_f125_bc18),
        ("nef-nikon-z-fc.nef", (5600, 3728), 0xa65e_5bf4_6937_3625),
        ("nrw-nikon-coolpix-a1000.nrw", (4608, 3456), 0x2ab8_7617_000c_0a7d),
        ("nrw-nikon-coolpix-p1000.nrw", (4624, 3470), 0xb512_6adb_9e72_6cae),
        ("nrw-nikon-coolpix-p1100.nrw", (4624, 3470), 0x7262_1601_b15b_4bba),
        ("nrw-nikon-coolpix-p950.nrw", (4624, 3470), 0xdc59_8f4d_a414_edfb),
        ("raf-fujifilm-finepix-f550exr.raf", (3328, 2464), 0x4247_157c_9d70_b359),
        ("raf-fujifilm-finepix-f770exr.raf", (3328, 2464), 0x6df4_5810_a581_1e9b),
        ("raf-fujifilm-finepix-hs30exr.raf", (3328, 2464), 0x5d4c_2901_70fa_83c3),
        ("raf-fujifilm-finepix-hs33exr.raf", (3328, 2464), 0x7172_f98c_0034_0dde),
        ("raf-fujifilm-finepix-hs50exr.raf", (3328, 2464), 0xad53_aa23_e481_4fcc),
        ("raf-fujifilm-finepix-s1.raf", (4624, 3470), 0xde37_e148_27f2_4305),
        ("raf-fujifilm-finepix-sl1000.raf", (4624, 3470), 0xcbb1_b0f1_9dce_bdbc),
        ("raf-fujifilm-finepix-x100.raf", (4448, 2870), 0x8c7b_bdea_47c3_01bb),
        ("raf-fujifilm-finepix-x10.raf", (2944, 2144), 0xb1a0_13ee_0889_65ef),
        ("raf-fujifilm-gfx100-ii.raf", (11808, 8754), 0x5677_9820_a59b_43d8),
        ("raf-fujifilm-gfx100s-ii.raf", (11808, 8754), 0x9bb6_d871_b488_1a2b),
        ("raf-fujifilm-gfx-50r.raf", (9216, 6210), 0x2da2_1f02_2188_01e9),
        ("raf-fujifilm-gfx50s-ii.raf", (8472, 6210), 0x0657_0932_cc9b_04a9),
        ("raf-fujifilm-x100f.raf", (6048, 4038), 0x44d7_06fd_e255_f55c),
        ("raf-fujifilm-x100s.raf", (4992, 3296), 0x8bb0_be8f_8e2f_0114),
        ("raf-fujifilm-x100t.raf", (4992, 3296), 0x83a0_fc55_ba4a_8a34),
        ("raf-fujifilm-x100vi.raf", (7872, 5196), 0xe0bd_5ff9_a791_3b1e),
        ("raf-fujifilm-x100v.raf", (6384, 4182), 0x02ee_e2eb_3f29_017e),
        ("raf-fujifilm-x20.raf", (4096, 3012), 0xc13a_34e9_bedb_7b65),
        ("raf-fujifilm-x30.raf", (4096, 3012), 0x9d28_789e_b37a_d23a),
        ("raf-fujifilm-x70.raf", (4992, 3296), 0xb49d_8575_ed65_191c),
        ("raf-fujifilm-x-a10.raf", (4912, 3278), 0xb681_5176_24ae_ea80),
        ("raf-fujifilm-x-a1.raf", (5120, 3288), 0xbfe5_5bc3_818d_aff0),
        ("raf-fujifilm-x-a3.raf", (6016, 4014), 0xab8b_9cf0_25dc_29dd),
        ("raf-fujifilm-x-a7.raf", (6016, 4014), 0x3c75_95af_4bdb_5d1e),
        ("raf-fujifilm-x-e2.raf", (4992, 3296), 0x18b2_ac1b_a7ec_e687),
        ("raf-fujifilm-x-e2s.raf", (4992, 3296), 0x334a_900e_4d2e_6f7a),
        ("raf-fujifilm-x-e3.raf", (6160, 4032), 0x9dae_1182_a6ff_f11a),
        ("raf-fujifilm-x-e4.raf", (6384, 4182), 0x05ea_0600_153c_af50),
        ("raf-fujifilm-xf10.raf", (6016, 4014), 0xc8fb_db43_3419_aee8),
        ("raf-fujifilm-xf1.raf", (2944, 2144), 0x9860_bc05_28ca_7b68),
        ("raf-fujifilm-x-h1.raf", (6048, 4038), 0x235f_cef5_1ece_f7e0),
        ("raf-fujifilm-x-m1.raf", (5120, 3288), 0xe6a2_89ab_2a70_7f59),
        ("raf-fujifilm-x-pro1.raf", (5120, 3288), 0xf5eb_1273_c2d4_0d05),
        ("raf-fujifilm-x-pro2.raf", (6160, 4032), 0xf268_21e1_9eac_3aaf),
        ("raf-fujifilm-x-pro3.raf", (6384, 4182), 0x08d9_6383_c9c5_68da),
        ("raf-fujifilm-xq1.raf", (4096, 3012), 0x5e6f_2478_974a_775f),
        ("raf-fujifilm-xq2.raf", (4096, 3012), 0x755d_a621_b230_a779),
        ("raf-fujifilm-x-s10.raf", (6384, 4182), 0xc987_263e_0cb0_99ee),
        ("raf-fujifilm-x-s1.raf", (2944, 2144), 0xfeb2_062d_7766_3ed6),
        ("raf-fujifilm-x-s20.raf", (6336, 4182), 0x2b15_357d_7db0_1e0a),
        ("raf-fujifilm-x-t100.raf", (6016, 4014), 0xfc92_1c91_2075_1751),
        ("raf-fujifilm-x-t10.raf", (4992, 3296), 0x84cf_2c71_7140_74d0),
        ("raf-fujifilm-x-t1.raf", (4992, 3296), 0x944d_128e_65c7_6375),
        ("raf-fujifilm-x-t200.raf", (6016, 4014), 0xc235_b051_f6e0_8ccc),
        ("raf-fujifilm-x-t30-iii.raf", (6336, 4182), 0x635b_00d6_8d24_d41e),
        ("raf-fujifilm-x-t30-ii.raf", (6384, 4182), 0x88b7_2d0c_92bc_4d9b),
        ("raf-fujifilm-x-t30.raf", (6384, 4182), 0xd589_f85d_366e_6e2a),
        ("raf-fujifilm-x-t3.raf", (6384, 4182), 0x8d50_a172_374f_a578),
    ];
    let mut seen = 0;
    for &(name, sensor, hash) in cases {
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
        let jpeg = embedded_preview(&bytes).unwrap_or_else(|| panic!("{name}: no camera JPEG"));
        let r = green_vs_camera_jpeg(&raw, &jpeg, 24, 16);
        eprintln!("{name:40} {}x{}: green follows the camera JPEG, r = {r:.3}", raw.width, raw.height);
        assert!(r > 0.6, "{name}: decoded mosaic does not follow the camera JPEG (r = {r:.3})");
        seen += 1;
    }
    eprintln!("Nikon / Fujifilm bodies checked on {seen} files");
}
