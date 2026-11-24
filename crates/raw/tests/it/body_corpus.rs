//! One CC0 file per Nikon and Fujifilm body (and the Sony DSC-F828's SRF) from raw.pixls.us (URLs and SHA-256 in `RAW_SAMPLES`,
//! xtask/src/main.rs; downloaded into the gitignored corpus/raw by `cargo xtask corpus --download`).
//!
//! No external decoder was run on these: the check against the camera is
//! black-box: the green sites of the active area, on a 24 × 16 grid, must follow the camera's own embedded
//! JPEG (gamma-encoded Pearson r; 0.77–1.00 on these files, a misread layout or stream gives r near 0).
//! Missing files are reported as skipped.

use lcc_raw::{RawData, decode, embedded_preview, probe_info};
use std::path::{Path, PathBuf};

fn corpus() -> PathBuf {
    lcc_brand::env_os("CORPUS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
}

/// Green of the decoded mosaic against the camera's embedded JPEG on a `gw × gh` grid of the active area.
fn green_vs_camera_jpeg(raw: &lcc_raw::RawImage, jpeg: &[u8], gw: usize, gh: usize) -> f64 {
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

/// log(red / blue) of the mosaic against the camera JPEG's on a `gw × gh` grid of the crop (Pearson r).
fn red_blue_vs_camera_jpeg(raw: &lcc_raw::RawImage, jpeg: &[u8], gw: usize, gh: usize) -> f64 {
    use zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(jpeg), opts);
    let px = d.decode().unwrap();
    let info = d.info().unwrap();
    let (jw, jh) = (info.width as usize, info.height as usize);
    let mut j = vec![[0f64; 2]; gw * gh];
    for y in 0..jh {
        for x in 0..jw {
            let c = &mut j[(y * gh / jh) * gw + x * gw / jw];
            c[0] += f64::from(px[(y * jw + x) * 3]);
            c[1] += f64::from(px[(y * jw + x) * 3 + 2]);
        }
    }
    let RawData::U16(samples) = &raw.data else { panic!("integer sensor data expected") };
    let cfa = raw.cfa.clone().expect("colour-filter layout");
    let (a, black) = (raw.crop, f64::from(raw.black.mean()));
    let mut r = vec![[0f64; 2]; gw * gh];
    for y in a.y..a.y + a.height {
        for x in a.x..a.x + a.width {
            let c = cfa.color_at(x, y);
            if c != 1 {
                let cell = &mut r[((y - a.y) * gh / a.height) * gw + (x - a.x) * gw / a.width];
                cell[usize::from(c / 2)] += (f64::from(samples[y * raw.width + x]) - black).max(0.0);
            }
        }
    }
    let ratio = |v: &[[f64; 2]]| v.iter().map(|c| ((c[0] + 1.0) / (c[1] + 1.0)).ln()).collect::<Vec<f64>>();
    let (r, j) = (ratio(&r), ratio(&j));
    let n = r.len() as f64;
    let (mr, mj) = (r.iter().sum::<f64>() / n, j.iter().sum::<f64>() / n);
    let cov: f64 = r.iter().zip(&j).map(|(x, y)| (x - mr) * (y - mj)).sum();
    let vr: f64 = r.iter().map(|x| (x - mr).powi(2)).sum();
    let vj: f64 = j.iter().map(|y| (y - mj).powi(2)).sum();
    cov / (vr * vj).sqrt()
}

#[test]
fn nikon_and_fujifilm_bodies_decode_like_the_camera_jpeg() {
    // (file, sensor width × height)
    let cases: &[(&str, (usize, usize))] = &[
        ("dng-nikon-d80.dng", (3900, 2611)),
        ("nef-nikon-1-aw1.nef", (4620, 3084)),
        ("nef-nikon-1-j1.nef", (3904, 2606)),
        ("nef-nikon-1-j2.nef", (3904, 2606)),
        ("nef-nikon-1-j3.nef", (4620, 3084)),
        ("nef-nikon-1-j4.nef", (5248, 3504)),
        ("nef-nikon-1-j5.nef", (5584, 3726)),
        ("nef-nikon-1-s1.nef", (3948, 2604)),
        ("nef-nikon-1-v1.nef", (3904, 2606)),
        ("nef-nikon-1-v2.nef", (4620, 3084)),
        ("nef-nikon-1-v3.nef", (5248, 3504)),
        ("nef-nikon-coolpix-a.nef", (4992, 3280)),
        ("nef-nikon-d1h.nef", (2012, 1324)),
        ("nef-nikon-d200.nef", (3904, 2616)),
        ("nef-nikon-d2h.nef", (2496, 1648)),
        ("nef-nikon-d2hs.nef", (2496, 1648)),
        ("nef-nikon-d2x.nef", (4320, 2868)),
        ("nef-nikon-d2xs.nef", (4320, 2868)),
        ("nef-nikon-d3000.nef", (3904, 2616)),
        ("nef-nikon-d300.nef", (4352, 2868)),
        ("nef-nikon-d300s.nef", (4352, 2868)),
        ("nef-nikon-d3100.nef", (4672, 3084)),
        ("nef-nikon-d3200.nef", (6080, 4012)),
        ("nef-nikon-d3300.nef", (6016, 4016)),
        ("nef-nikon-d3400.nef", (6016, 4016)),
        ("nef-nikon-d3500.nef", (6016, 4016)),
        ("nef-nikon-d3.nef", (4288, 2844)),
        ("nef-nikon-d3s.nef", (4288, 2844)),
        ("nef-nikon-d3x.nef", (6080, 4044)),
        ("nef-nikon-d40.nef", (3040, 2014)),
        ("nef-nikon-d40x.nef", (3904, 2616)),
        ("nef-nikon-d4.nef", (4992, 3292)),
        ("nef-nikon-d4s.nef", (4936, 3288)),
        ("nef-nikon-d5000.nef", (4352, 2868)),
        ("nef-nikon-d500.nef", (5600, 3728)),
        ("nef-nikon-d50.nef", (3040, 2014)),
        ("nef-nikon-d5200.nef", (6036, 4020)),
        ("nef-nikon-d5300.nef", (6016, 4016)),
        ("nef-nikon-d5500.nef", (6016, 4016)),
        ("nef-nikon-d5600.nef", (6016, 4016)),
        ("nef-nikon-d5.nef", (5584, 3728)),
        ("nef-nikon-d600.nef", (6080, 4028)),
        ("nef-nikon-d60.nef", (3904, 2616)),
        ("nef-nikon-d610.nef", (6080, 4028)),
        ("nef-nikon-d700.nef", (4288, 2844)),
        ("nef-nikon-d70.nef", (3040, 2014)),
        ("nef-nikon-d70s.nef", (3040, 2014)),
        ("nef-nikon-d7100.nef", (6036, 4020)),
        ("nef-nikon-d7200.nef", (6016, 4016)),
        ("nef-nikon-d750.nef", (6032, 4032)),
        ("nef-nikon-d780.nef", (6064, 4040)),
        ("nef-nikon-d800e.nef", (7424, 4924)),
        ("nef-nikon-d800.nef", (7424, 4924)),
        ("nef-nikon-d810.nef", (7380, 4928)),
        ("nef-nikon-d850.nef", (8288, 5520)), // the pinned full-size 12-bit file (91a276080), not the earlier M-size one
        ("nef-nikon-d90.nef", (4352, 2868)),
        ("nef-nikon-df.nef", (4992, 3292)),
        ("nef-nikon-z-30.nef", (5600, 3728)),
        ("nef-nikon-z50-2.nef", (5600, 3728)),
        ("nef-nikon-z-50.nef", (5600, 3728)),
        ("nef-nikon-z-5.nef", (6040, 4032)),
        ("nef-nikon-z-6-2.nef", (6064, 4040)),
        ("nef-nikon-z-7-2.nef", (8288, 5520)),
        ("nef-nikon-z-9.nef", (8280, 5520)),
        ("nef-nikon-z-fc.nef", (5600, 3728)),
        ("nrw-nikon-coolpix-a1000.nrw", (4608, 3456)),
        ("nrw-nikon-coolpix-p1000.nrw", (4624, 3470)),
        ("nrw-nikon-coolpix-p1100.nrw", (4624, 3470)),
        ("nrw-nikon-coolpix-p950.nrw", (4624, 3470)),
        ("raf-fujifilm-finepix-f550exr.raf", (3328, 2464)),
        ("raf-fujifilm-finepix-f770exr.raf", (3328, 2464)),
        ("raf-fujifilm-finepix-hs30exr.raf", (3328, 2464)),
        ("raf-fujifilm-finepix-hs33exr.raf", (3328, 2464)),
        ("raf-fujifilm-finepix-hs50exr.raf", (3328, 2464)),
        ("raf-fujifilm-finepix-s1.raf", (4624, 3470)),
        ("raf-fujifilm-finepix-sl1000.raf", (4624, 3470)),
        ("raf-fujifilm-finepix-x100.raf", (4448, 2870)),
        ("raf-fujifilm-finepix-x10.raf", (2944, 2144)),
        ("raf-fujifilm-gfx100-ii.raf", (11808, 8754)),
        ("raf-fujifilm-gfx100s-ii.raf", (11808, 8754)),
        ("raf-fujifilm-gfx-50r.raf", (9216, 6210)),
        ("raf-fujifilm-gfx50s-ii.raf", (8472, 6210)),
        ("raf-fujifilm-x100f.raf", (6048, 4038)),
        ("raf-fujifilm-x100s.raf", (4992, 3296)),
        ("raf-fujifilm-x100t.raf", (4992, 3296)),
        ("raf-fujifilm-x100vi.raf", (7872, 5196)),
        ("raf-fujifilm-x100v.raf", (6384, 4182)),
        ("raf-fujifilm-x20.raf", (4096, 3012)),
        ("raf-fujifilm-x30.raf", (4096, 3012)),
        ("raf-fujifilm-x70.raf", (4992, 3296)),
        ("raf-fujifilm-x-a10.raf", (4912, 3278)),
        ("raf-fujifilm-x-a1.raf", (5120, 3288)),
        ("raf-fujifilm-x-a3.raf", (6016, 4014)),
        ("raf-fujifilm-x-a7.raf", (6016, 4014)),
        ("raf-fujifilm-x-e2.raf", (4992, 3296)),
        ("raf-fujifilm-x-e2s.raf", (4992, 3296)),
        ("raf-fujifilm-x-e3.raf", (6160, 4032)),
        ("raf-fujifilm-x-e4.raf", (6384, 4182)),
        ("raf-fujifilm-xf10.raf", (6016, 4014)),
        ("raf-fujifilm-xf1.raf", (2944, 2144)),
        ("raf-fujifilm-x-h1.raf", (6048, 4038)),
        ("raf-fujifilm-x-m1.raf", (5120, 3288)),
        ("raf-fujifilm-x-pro1.raf", (5120, 3288)),
        ("raf-fujifilm-x-pro2.raf", (6160, 4032)),
        ("raf-fujifilm-x-pro3.raf", (6384, 4182)),
        ("raf-fujifilm-xq1.raf", (4096, 3012)),
        ("raf-fujifilm-xq2.raf", (4096, 3012)),
        ("raf-fujifilm-x-s10.raf", (6384, 4182)),
        ("raf-fujifilm-x-s1.raf", (2944, 2144)),
        ("raf-fujifilm-x-s20.raf", (6336, 4182)),
        ("raf-fujifilm-x-t100.raf", (6016, 4014)),
        ("raf-fujifilm-x-t10.raf", (4992, 3296)),
        ("raf-fujifilm-x-t1.raf", (4992, 3296)),
        ("raf-fujifilm-x-t200.raf", (6016, 4014)),
        ("raf-fujifilm-x-t30-iii.raf", (6336, 4182)),
        ("raf-fujifilm-x-t30-ii.raf", (6384, 4182)),
        ("raf-fujifilm-x-t30.raf", (6384, 4182)),
        ("raf-fujifilm-x-t3.raf", (6384, 4182)),
        ("raf-fuji-finepix-s200exr.raf", (2880, 2128)),
        ("srf-sony-dsc-f828.srf", (3360, 2460)),
    ];
    // bodies whose red / blue phase was set from a decoder reference, not a CFA tag: the red-to-blue balance of
    // the mosaic must follow the camera JPEG's too (a swapped phase gives a negative r)
    let chroma_checked = ["raf-fuji-finepix-s200exr.raf", "srf-sony-dsc-f828.srf"];
    let mut seen = 0;
    for &(name, sensor) in cases {
        let path = corpus().join("raw").join(name);
        let Ok(bytes) = std::fs::read(&path) else {
            lcc_testkit::skip!(Corpus, "{} absent", path.display());
            continue;
        };
        let raw = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        raw.validate().unwrap();
        assert_eq!((raw.width, raw.height, raw.cpp), (sensor.0, sensor.1, 1), "{name}: sensor size");
        assert_eq!(raw.info(), probe_info(&bytes).unwrap(), "{name}: probe and decode disagree");
        assert!(matches!(raw.data, RawData::U16(_)), "{name}: integer sensor data expected");
        assert!(raw.white_at(0) > raw.black.mean(), "{name}: invalid black/white levels");
        let jpeg = embedded_preview(&bytes).unwrap_or_else(|| panic!("{name}: no camera JPEG"));
        let r = green_vs_camera_jpeg(&raw, &jpeg, 24, 16);
        eprintln!("{name:40} {}x{}: green follows the camera JPEG, r = {r:.3}", raw.width, raw.height);
        assert!(r > 0.6, "{name}: decoded mosaic does not follow the camera JPEG (r = {r:.3})");
        if chroma_checked.contains(&name) {
            let rb = red_blue_vs_camera_jpeg(&raw, &jpeg, 16, 12);
            eprintln!("{name:40} red / blue follows the camera JPEG, r = {rb:.3}");
            assert!(rb > 0.5, "{name}: red and blue look swapped against the camera JPEG (r = {rb:.3})");
        }
        seen += 1;
    }
    eprintln!("Nikon / Fujifilm bodies checked on {seen} files");
}

/// A recorded bulk check, run by hand: every file in `<PREFIX>_BULK_DIR` (one raw per body, e.g. fetched from
/// raw.pixls.us) must decode from sensor data and its green sites must follow the camera's embedded JPEG. Prints one
/// line per file (`ok` / `FAIL`) for the evidence table in docs/raw/evidence.tsv.
#[test]
#[ignore = "needs a directory of raw files in <PREFIX>_BULK_DIR"]
fn bulk_dir_decodes_like_the_camera_jpeg() {
    let Some(dir) = lcc_brand::env_os("BULK_DIR").map(PathBuf::from) else {
        lcc_testkit::skip!(Corpus, "BULK_DIR not set");
        return;
    };
    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().filter_map(|e| Some(e.ok()?.path())).collect();
    names.sort();
    let (mut ok, mut failed) = (0, 0);
    for path in names {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let verdict = match (decode(&bytes), embedded_preview(&bytes)) {
            (Ok(raw), Some(jpeg)) if raw.cfa.is_some() && matches!(raw.data, RawData::U16(_)) => {
                let r = green_vs_camera_jpeg(&raw, &jpeg, 24, 16);
                if r > 0.6 { Ok(format!("{}x{} r = {r:.3}", raw.width, raw.height)) } else { Err(format!("r = {r:.3}")) }
            }
            (Ok(_), Some(_)) => Err("not a colour-filter mosaic".to_string()),
            (Ok(_), None) => Err("no camera JPEG".to_string()),
            (Err(e), _) => Err(e.to_string()),
        };
        match verdict {
            Ok(m) => {
                ok += 1;
                eprintln!("ok   {name:50} {m}");
            }
            Err(m) => {
                failed += 1;
                eprintln!("FAIL {name:50} {m}");
            }
        }
    }
    eprintln!("bulk check: {ok} ok, {failed} failed");
}
