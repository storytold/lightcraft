//! One CC0 file per Canon CR2, Pentax and Ricoh body from raw.pixls.us (URLs and SHA-256 in `RAW_SAMPLES`,
//! xtask/src/main.rs; downloaded into the gitignored corpus/raw by `cargo xtask corpus --download`).
//!
//! No external decoder was run on these: the FNV-1a hashes over the decoded u16 samples (row-major,
//! little-endian, masked borders included) pin LightCraft's own decode, and the check against the camera is
//! black-box: the green sites of the active area, on a 24 × 16 grid, must follow the camera's own embedded
//! JPEG (gamma-encoded Pearson r; 0.69–1.00 on these files, a misread layout or stream gives r near 0). Four files are
//! pinned only: two have no embedded JPEG (GX200, and the PowerShot S2 IS, a DNG written by CHDK), one is monochrome (K-3 Mark III
//! Monochrome), and the GXR's preview is not oriented like its sensor; their renders were inspected by eye.
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
fn canon_pentax_and_ricoh_bodies_decode_like_the_camera_jpeg() {
    // (file, sensor width × height, pinned FNV-1a of the sensor samples, compare with the camera JPEG)
    let cases: &[(&str, (usize, usize), u64, bool)] = &[
        ("cr2-canon-eos-1000d.cr2", (3948, 2622), 0x7de9_a900_6ae5_ccb6, true),
        ("cr2-canon-eos-1d-mark-ii-n.cr2", (3596, 2360), 0xbfbf_beba_d32c_efdf, true),
        ("cr2-canon-eos-1d-mark-iii.cr2", (3984, 2622), 0x5fbc_a832_828e_c45b, true),
        ("cr2-canon-eos-1d-mark-iv.cr2", (5120, 3318), 0xda97_8f13_a35b_978f, true),
        ("cr2-canon-eos-1d-x-mark-ii.cr2", (5568, 3708), 0xf3cb_7865_8c90_f41f, true),
        ("cr2-canon-eos-2000d.cr2", (6096, 4051), 0xe687_d928_d4e7_821e, true),
        ("cr2-canon-eos-20d.cr2", (3596, 2360), 0x20bf_cfd0_3ec3_7915, true),
        ("cr2-canon-eos-30d.cr2", (3596, 2360), 0x5f49_abf5_3c51_2fc9, true),
        ("cr2-canon-eos-4000d.cr2", (5344, 3516), 0x4f62_9ac5_37ce_9b8e, true),
        ("cr2-canon-eos-400d.cr2", (3948, 2622), 0x3b42_3e9a_e59f_3558, true),
        ("cr2-canon-eos-450d.cr2", (4312, 2876), 0x47ad_c3fe_76a6_bd06, true),
        ("cr2-canon-eos-500d.cr2", (4832, 3204), 0xbb88_0e01_7f8b_8f36, true),
        ("cr2-canon-eos-50d.cr2", (4832, 3228), 0xd4c1_61ba_c2f1_a492, true),
        ("cr2-canon-eos-5d-mark-iv.cr2", (6880, 4544), 0xf62e_e9b9_6976_b2cf, true),
        ("cr2-canon-eos-5d.cr2", (4476, 2954), 0xf0b6_2f3e_3971_7785, true),
        ("cr2-canon-eos-5ds.cr2", (8896, 5920), 0x3e2e_5ff4_4f53_8920, true),
        ("cr2-canon-eos-600d.cr2", (5344, 3516), 0xe962_86a6_f187_091e, true),
        ("cr2-canon-eos-60d.cr2", (5344, 3516), 0x6b47_1bd4_8f4e_3aca, true),
        ("cr2-canon-eos-6d-mark-ii.cr2", (6384, 4224), 0xc9f6_08ad_6cf8_6969, true),
        ("cr2-canon-eos-70d.cr2", (5568, 3708), 0x1dba_52ca_716c_8594, true),
        ("cr2-canon-eos-750d.cr2", (6096, 4056), 0xa8d2_87ba_7428_bfd6, true),
        ("cr2-canon-eos-760d.cr2", (6096, 4056), 0xc9d1_a2a0_4555_77c8, true),
        ("cr2-canon-eos-77d.cr2", (6288, 4056), 0x3bf3_dffd_d202_66e8, true),
        ("cr2-canon-eos-7d-mark-ii.cr2", (5568, 3708), 0x4544_6618_a169_4540, true),
        ("cr2-canon-eos-kiss-digital-n.cr2", (3516, 2328), 0x1cc0_14a6_4e5d_f2a6, true),
        ("cr2-canon-eos-kiss-x80.cr2", (5344, 3516), 0x7d12_8887_a0bc_8544, true),
        ("cr2-canon-eos-kiss-x9.cr2", (6288, 4056), 0x3a83_f960_1d45_854d, true),
        ("cr2-canon-eos-m.cr2", (5280, 3528), 0x845d_bcda_107b_1161, true),
        ("cr2-canon-eos-m10.cr2", (5280, 3528), 0x312a_4709_2c94_f887, true),
        ("cr2-canon-eos-m100.cr2", (6288, 4056), 0x4bde_865a_2037_f121, true),
        ("cr2-canon-eos-m2.cr2", (5280, 3528), 0x2ed7_e8c4_39d7_7f15, true),
        ("cr2-canon-eos-m3.cr2", (6096, 4056), 0x958d_834a_3e39_fc1c, true),
        ("cr2-canon-eos-m5.cr2", (6288, 4056), 0x39af_0b9f_4570_e9f5, true),
        ("cr2-canon-eos-m6.cr2", (6288, 4056), 0xf4c8_36b3_2057_69f3, true),
        ("cr2-canon-eos-rebel-sl1.cr2", (5280, 3528), 0xbcea_0c6b_0df2_4b7d, true),
        ("cr2-canon-eos-rebel-t3.cr2", (4352, 2874), 0xb51e_0a5b_73ce_9ac3, true),
        ("cr2-canon-eos-rebel-t4i.cr2", (5280, 3528), 0xf3e2_4be6_7049_9fac, true),
        ("cr2-canon-eos-rebel-t5.cr2", (5344, 3516), 0xbd11_9ab9_f322_6e55, true),
        ("cr2-canon-eos-rebel-t5i.cr2", (5280, 3528), 0xfb93_e24f_d77a_6db6, true),
        ("cr2-canon-eos-rebel-t7i.cr2", (6288, 4056), 0x7657_7d0a_911d_9c04, true),
        ("cr2-canon-powershot-g1-x-mark-ii.cr2", (4480, 3366), 0xd4b6_ced9_a68d_d520, true),
        ("cr2-canon-powershot-g1-x-mark-iii.cr2", (6288, 4056), 0x7a65_598c_7df5_ef2a, true),
        ("cr2-canon-powershot-g1-x.cr2", (4496, 3366), 0x0d02_3f27_892d_67ae, true),
        ("cr2-canon-powershot-g11.cr2", (3744, 2784), 0x785b_5b63_ad8e_b78a, true),
        ("cr2-canon-powershot-g12.cr2", (3744, 2784), 0xd5e2_266b_d066_5db0, true),
        ("cr2-canon-powershot-g15.cr2", (4160, 3124), 0xf234_7cc5_3ac7_4a42, true),
        ("cr2-canon-powershot-g16.cr2", (4192, 3062), 0xba01_a612_ba7c_bffe, true),
        ("cr2-canon-powershot-g3-x.cr2", (5632, 3710), 0x8a1e_46e0_3d78_1879, true),
        ("cr2-canon-powershot-g5-x.cr2", (5632, 3710), 0xc023_524c_2998_f93d, true),
        ("cr2-canon-powershot-g7-x-mark-ii.cr2", (5632, 3710), 0x9d71_9788_df4e_38d8, true),
        ("cr2-canon-powershot-g7-x.cr2", (5632, 3710), 0xe47b_2d8a_2566_9a60, true),
        ("cr2-canon-powershot-g9-x-mark-ii.cr2", (5632, 3710), 0xfe91_af24_c438_f82a, true),
        ("cr2-canon-powershot-g9-x.cr2", (5632, 3710), 0xc91d_c1ca_ee96_3e2a, true),
        ("cr2-canon-powershot-g9.cr2", (4104, 3048), 0xee20_d244_64ec_8952, true),
        ("cr2-canon-powershot-s100.cr2", (4160, 3124), 0x0f16_9e95_92e6_4275, true),
        ("cr2-canon-powershot-s100v.cr2", (4160, 3124), 0x3973_5bf8_19c5_7ffd, true),
        ("cr2-canon-powershot-s110.cr2", (4160, 3124), 0x8ef0_f13b_7b64_98fb, true),
        ("cr2-canon-powershot-s120.cr2", (4192, 3062), 0x48c5_76b9_6afb_4a60, true),
        ("dng-canon-powershot-s2-is.dng", (2672, 1968), 0x1ba0_5636_0928_545f, false),
        ("cr2-canon-powershot-s95.cr2", (3744, 2784), 0x85a7_3f89_0720_7856, true),
        ("cr2-canon-powershot-sx50-hs.cr2", (4176, 3062), 0x4466_4918_96d5_0162, true),
        ("cr2-canon-powershot-sx60-hs.cr2", (4768, 3516), 0xae5f_336d_4abc_f0a1, true),
        ("dng-pentax-645d.dng", (7424, 5552), 0xbf13_df20_b6fc_e20c, true),
        ("dng-pentax-645z.dng", (8384, 6208), 0x2529_54b5_a64e_1536, true),
        ("dng-pentax-k-01.dng", (4960, 3300), 0x57ee_22b2_2cb5_b2cf, true),
        ("dng-pentax-k-1-mark-ii.dng", (7392, 4950), 0xf2ed_12fa_9824_7028, true),
        ("dng-pentax-k-3-ii.dng", (6080, 4032), 0x921b_d488_3bc3_a6af, true),
        ("dng-pentax-k-3-mark-iii-monochrome.dng", (6304, 4224), 0x3fc2_04b3_4c78_8959, false),
        ("dng-pentax-k-30.dng", (4960, 3300), 0xe665_6a25_81ec_4895, true),
        ("dng-pentax-k-5-ii.dng", (4992, 3284), 0xc6b0_b56b_76ab_74d6, true),
        ("dng-pentax-k-5.dng", (4992, 3284), 0xd020_63f6_a9a5_c566, true),
        ("dng-pentax-k-50.dng", (4960, 3300), 0x7728_7535_4feb_08e3, true),
        ("dng-pentax-k-500.dng", (4960, 3300), 0xbc6c_575a_e188_acbe, true),
        ("dng-pentax-k-7.dng", (4736, 3136), 0xd7c8_ec3d_5bbc_0b22, true),
        ("dng-pentax-k-s1.dng", (5504, 3664), 0xa987_c182_22c8_8fe3, true),
        ("dng-pentax-k-s2.dng", (5504, 3664), 0x056e_63c6_87e4_e504, true),
        ("dng-pentax-k-x.dng", (4352, 2868), 0xad8d_0b39_7a77_bfb7, true),
        ("dng-pentax-k200d.dng", (3936, 2624), 0xecc6_d5f8_d5e5_6cb3, true),
        ("dng-pentax-k20d.dng", (4864, 3136), 0x5bb1_9a98_babd_4bbf, true),
        ("dng-pentax-kp.dng", (6112, 4060), 0xa1a6_c4b4_84c5_d21f, true),
        ("dng-pentax-mx-1.dng", (4016, 3010), 0xb8e1_011c_4b49_6dc3, true),
        ("dng-pentax-q.dng", (4064, 3000), 0xb4e3_dd90_ef7f_874f, true),
        ("dng-pentax-q7.dng", (4064, 3000), 0x8b2b_cf5b_a63b_8811, true),
        ("dng-ricoh-gr-ii.dng", (4960, 3280), 0x9724_b08b_6259_ba49, true),
        ("dng-ricoh-gr-iiix.dng", (6112, 4064), 0x8be6_fa17_2379_4d1a, true),
        ("dng-ricoh-gr.dng", (4960, 3280), 0x3e5e_9ce5_a4ac_0dc6, true),
        ("dng-ricoh-gx200.dng", (4096, 3014), 0xb7b6_ff86_8f38_3ab2, false),
        ("dng-ricoh-gxr.dng", (3672, 2748), 0x6255_34a2_7d07_76aa, false),
        ("pef-pentax-ist-d.pef", (3040, 2024), 0x9528_2820_e0c3_6088, true),
        ("pef-pentax-ist-dl.pef", (3040, 2024), 0xb1b8_7fec_f109_8f66, true),
        ("pef-pentax-ist-ds.pef", (3040, 2024), 0x05ed_70b9_cb1d_a5f8, true),
        ("pef-pentax-k-1.pef", (7392, 4950), 0xdf3a_d5b4_a0a4_a1e1, true),
        ("pef-pentax-k-3-mark-iii.pef", (6304, 4224), 0x4ea0_a41f_fd71_a0b7, true),
        ("pef-pentax-k-70.pef", (6080, 4064), 0xad07_0022_dbfb_4cc6, true),
        ("pef-pentax-k-r.pef", (4352, 2868), 0x8638_b64c_a21e_a94d, true),
        ("pef-pentax-k100d-super.pef", (3040, 2024), 0xc177_9a26_301a_52a4, true),
        ("pef-pentax-k100d.pef", (3040, 2024), 0x2cf8_04e5_757d_1d1c, true),
        ("pef-pentax-k110d.pef", (3040, 2024), 0xc273_870f_da44_d3f8, true),
        ("pef-pentax-k2000.pef", (3936, 2624), 0xadeb_62eb_f697_35ef, true),
        ("pef-pentax-kf.pef", (6080, 4064), 0x0006_b0a3_ad13_45fe, true),
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
    eprintln!("Canon / Pentax / Ricoh bodies checked on {seen} files");
}
