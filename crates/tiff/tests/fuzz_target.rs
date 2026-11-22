//! Fuzz target `tiff` (`cargo xtask fuzz tiff`): mutated classic and BigTIFF files in both byte orders,
//! and corpus TIFF-based files, through parsing, every IFD and the image layout. A short run in
//! `cargo test`; time-boxed long runs under `LCC_FUZZ_SECS` (see `lcc-fuzzkit`).

use lcc_tiff::{ByteOrder, IfdBuilder, ParseOptions, Tiff, TiffWriter, Value, tags};

fn seed(order: ByteOrder, big: bool) -> Vec<u8> {
    let mut exif = IfdBuilder::new();
    exif.set(0x829a, Value::Rational(vec![(1, 250)])).set(0x9003, Value::Ascii("2026:11:22 10:00:00".into()));
    let mut ifd = IfdBuilder::new();
    ifd.set(tags::IMAGE_WIDTH, Value::Long(vec![4]))
        .set(tags::IMAGE_LENGTH, Value::Long(vec![2]))
        .set(tags::BITS_PER_SAMPLE, Value::Short(vec![8]))
        .set(259, Value::Short(vec![1]))
        .set(262, Value::Short(vec![1]))
        .set(277, Value::Short(vec![1]))
        .set(1001, Value::Ascii("seed".into()))
        .set(1010, Value::Double(vec![std::f64::consts::PI]))
        .set_child(0x8769, exif);
    TiffWriter::new(order, big).write(&[ifd, IfdBuilder::new().with(1002, Value::Short(vec![7]))]).unwrap()
}

#[test]
fn fuzz_target() {
    let seeds = [seed(ByteOrder::Little, false), seed(ByteOrder::Big, false), seed(ByteOrder::Little, true)];
    lcc_fuzzkit::run_target("tiff", &seeds, &["tif", "tiff", "dng", "nef", "arw", "cr2", "pef"], 2000, |bytes| {
        let _ = Tiff::sniff(bytes);
        for t in [Tiff::parse(bytes), Tiff::parse_with(bytes, &ParseOptions::default())].into_iter().flatten() {
            for ifd in t.all_ifds().into_iter().take(64) {
                let _ = ifd.image();
            }
            let _ = t.exif();
            let _ = t.gps();
            let _ = t.find(tags::IMAGE_WIDTH);
        }
    });
}
