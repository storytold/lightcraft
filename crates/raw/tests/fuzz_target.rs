//! Fuzz target `raw` (`cargo xtask fuzz raw`): mutated DNGs (uncompressed, LJ92, lossy) and corpus raws
//! through probe, preview, decode and a small develop. A short run in `cargo test`; time-boxed long runs
//! under `LCC_FUZZ_SECS` (see `lcc-fuzzkit`). Crash inputs go into `tests/it/robust.rs`'s tables.

use lcc_raw::{BlackLevel, Cfa, ColorData, DngCompression, DngWriteOptions, Method, OpcodeLists, Orientation, RawData, RawFormat, RawImage, Rect};

fn sample(comp: DngCompression) -> Vec<u8> {
    let (w, h) = (24usize, 16usize);
    let raw = RawImage {
        format: RawFormat::Dng,
        width: w,
        height: h,
        cpp: 1,
        data: RawData::U16((0..w * h).map(|i| (i * 97 % 4000) as u16).collect()),
        cfa: Some(Cfa::bayer("RGGB").unwrap()),
        bits: 12,
        black: BlackLevel::uniform(64.0),
        white: vec![4095.0],
        active_area: Rect::new(0, 0, w, h),
        crop: Rect::new(0, 0, w, h),
        orientation: Orientation::Normal,
        color: ColorData::default(),
        wb_multipliers: None,
        linearized: false,
        pixel_aspect: 1.0,
        opcodes: OpcodeLists::default(),
        metadata: Default::default(),
        lens_metadata: Default::default(),
    };
    lcc_raw::write_dng(&raw, &DngWriteOptions { compression: comp, ..Default::default() }).unwrap()
}

#[test]
fn fuzz_target() {
    let seeds = [
        sample(DngCompression::Uncompressed),
        sample(DngCompression::Lj92 { tile: 16 }),
        sample(DngCompression::LossyJpeg { tile: 16, quality: 80, long_edge: None }),
        b"II*\0\x10\0\0\0CR\x02\0".to_vec(),
    ];
    let exts = [
        "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "srw", "3fr", "iiq", "x3f", "erf", "mos", "kdc",
        "dcr", "mrw", "gpr",
    ];
    lcc_fuzzkit::run_target("raw", &seeds, &exts, 300, |bytes| {
        let _ = lcc_raw::probe(bytes);
        let _ = lcc_raw::embedded_preview(bytes);
        if let Ok(img) = lcc_raw::decode(bytes)
            && img.width.saturating_mul(img.height) <= 1 << 16
        {
            let _ = img.develop(Method::Ahd);
            let xy = lcc_raw::color::as_shot_white_xy(&img);
            let _ = lcc_raw::color::camera_transform(&img, xy);
        }
    });
}
